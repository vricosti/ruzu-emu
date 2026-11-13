// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
// Encoding reference: Mesa NAK sm70_encode.rs (MIT), Copyright 2022 Collabora, Ltd.
//! Native instruction lowering from `frontend/ampere/lower_to_maxwell.h`.

pub use super::instruction::Instruction;
use super::register_pressure::prepare_register_pressure;
use std::cell::Cell;

#[derive(Debug, Clone)]
pub struct LoweredProgram {
    pub instructions: Vec<u64>,
    pub source_instruction_offsets: Vec<u32>,
    pub source_size: u32,
    pub required_texture_buffer: u32,
    pub high_uniform_registers: u32,
    pub error_offset: u32,
    pub error: &'static str,
}

impl Default for LoweredProgram {
    fn default() -> Self {
        Self {
            instructions: Vec::new(),
            source_instruction_offsets: Vec::new(),
            source_size: 0,
            required_texture_buffer: u32::MAX,
            high_uniform_registers: 0,
            error_offset: 0,
            error: "",
        }
    }
}

impl LoweredProgram {
    /// Upstream explicit operator bool().
    pub fn is_valid(&self) -> bool {
        self.error.is_empty()
    }
}

/// Scheduling/control words are added by the environment adapter, as upstream.
pub fn lower_to_maxwell(code: &[Instruction]) -> LoweredProgram {
    lower_to_maxwell_with_options(code, LoweringOptions::default())
}

/// Ported subset of upstream LoweringOptions. Stage-buffer selection remains
/// on the existing task/mesh entrypoint; unsupported stages are not enabled.
#[derive(Clone, Copy, Debug, Default)]
pub struct LoweringOptions {
    pub tensor: bool,
}

pub fn lower_to_maxwell_with_options(code: &[Instruction], options: LoweringOptions) -> LoweredProgram {
    lower_to_maxwell_with_cbuf(code, options, &mut |_, _| None)
}

/// NX2 extension: resolve native branch tables using the environment owner's
/// cached constant-buffer reads before proving non-adjacent CALL linkage.
pub fn lower_to_maxwell_with_cbuf(code: &[Instruction], options: LoweringOptions,
    read_cbuf: &mut dyn FnMut(u32, u32) -> Option<u32>) -> LoweredProgram {
    lower_with_stage_buffer(code, false, options, read_cbuf)
}

/// Explicit task/mesh path. Callers must supply mesh environment metadata and
/// use collective compute emission; ordinary graphics/compute remains gated.
pub fn lower_task_mesh_to_maxwell(code: &[Instruction]) -> LoweredProgram {
    lower_task_mesh_to_maxwell_with_cbuf(code, &mut |_, _| None)
}

pub fn lower_task_mesh_to_maxwell_with_cbuf(code: &[Instruction],
    read_cbuf: &mut dyn FnMut(u32, u32) -> Option<u32>) -> LoweredProgram {
    lower_with_stage_buffer(code, true, LoweringOptions::default(), read_cbuf)
}

fn lower_with_stage_buffer(code: &[Instruction], stage_buffer: bool, options: LoweringOptions,
    read_cbuf: &mut dyn FnMut(u32, u32) -> Option<u32>) -> LoweredProgram {
    let mut high_uniforms = 0;
    loop {
        let uniforms = UniformRegisters { high_uniforms, ..Default::default() };
        let mut result = lower_to_maxwell_with(code, stage_buffer, options, &uniforms, read_cbuf);
        result.high_uniform_registers = uniforms.requested.get();
        if result.is_valid() || result.high_uniform_registers <= high_uniforms { return result; }
        high_uniforms = result.high_uniform_registers;
    }
}

fn lower_to_maxwell_with(code: &[Instruction], stage_buffer: bool, options: LoweringOptions,
    uniforms: &UniformRegisters, read_cbuf: &mut dyn FnMut(u32, u32) -> Option<u32>) -> LoweredProgram {
    let mut program = ProgramLowering::default();
    let uses_uniform_bank = ProgramLowering::uses_uniform_bank(code);
    if code.is_empty() || code.len() > 65536 {
        return program.fail(0, "invalid native program size");
    }
    let control = BoundedControl::analyze_with_cbuf(code, false, uses_uniform_bank, uniforms.base(), read_cbuf);
    for index in 0..code.len() as u32 {
        let mut insn = code[index as usize];
        program
            .result
            .source_instruction_offsets
            .push(program.result.instructions.len() as u32);
        if control.dead_instructions[index as usize] { continue; }
        let mut opcode = insn.bits(0, 12);
        if insn.bits(12, 4) == 15 {
            continue;
        }
        let mut active_link = None;
        let mut active_entry = 0;
        if program.main_exited {
            for (&target, &link) in program.call_targets.iter().zip(&program.call_links) {
                if target <= index && target >= active_entry {
                    active_entry = target;
                    active_link = Some(link);
                }
            }
            let no_destination = matches!(opcode, 0x950 | 0x947 | 0x918)
                || matches!(opcode & 0x1ff, 0x00c | 0x00b);
            let written = BoundedControl::write_count(&insn).unwrap_or(1);
            if let Some(link) = active_link {
                if !no_destination && insn.bits(16, 8) <= link && link < insn.bits(16, 8) + written {
                    return program.fail(index, "native callee overwrites link register");
                }
            }
        }
        // Pinned operand scanner: ISBERD has Rd/Ra; ISBEWR has Ra/data.
        // Offset bytes and unused fields must not be mistaken for registers.
        let register_offsets: &[u32] = match (stage_buffer, opcode) {
            (true, 0x923) => &[16, 24],
            (true, 0x927) => &[24, 32],
            _ => &[16, 24, 32, 64],
        };
        let mut requires_shadow = false;
        for &bit in register_offsets {
            if opcode == 0x81c { break; }
            if bit == 32
                && insn.bits(9, 3) != 1
                && !matches!(opcode, 0xb60 | 0xb66 | 0x986 | 0x987 | 0xf66 | 0x988 | 0x593 | 0x59f)
                && !(stage_buffer && opcode == 0x927)
            { continue; }
            let reg = insn.bits(bit, 8);
            if reg >= if uses_uniform_bank { uniforms.base() } else { 246 } && reg < 255 {
                requires_shadow = true;
            }
        }
        let pressure = if requires_shadow {
            if !uses_uniform_bank || uniforms.base() & 3 != 0 {
                return program.fail(index, "native program uses lowering scratch registers");
            }
            let pressure = prepare_register_pressure(&insn, uniforms.base());
            if !pressure.valid { return program.fail(index, "native register shadowing mode"); }
            insn = pressure.instruction;
            program.result.instructions.extend_from_slice(&pressure.prefix);
            Some(pressure)
        } else { None };
        let instruction_start = program.result.instructions.len() as u32;
        let relocation_start = program.relocations.len();
        let rewrite = match UniformRewrite::new(&mut insn, &mut opcode, uniforms) {
            Ok(rewrite) => rewrite,
            Err(error) => return program.fail(index, error),
        };
        let native_predicate = rewrite.native_predicate;
        let mut lowering = InstructionLowering {
            uniforms,
            insn,
            predicate: rewrite.predicate,
            dest: u64::from(insn.bits(16, 8)),
            operand_error: "",
            instructions: &mut program.result.instructions,
        };
        rewrite.begin(&mut lowering);
        let result = match opcode {
            0x304 | 0x23e | 0xa3e | 0x232 | 0x832 | 0xa32 | 0x230 | 0x430 | 0x630 | 0x231
            | 0x431 | 0x631 | 0x831 | 0xa31 | 0x240 | 0x840 => lowering.lower_float_and_half(
                opcode,
                index,
                native_predicate,
                &mut program.relocations,
            ),
            0xc24 | 0xe24 | 0x224 | 0x2a5 | 0x225 | 0x227 | 0x625 | 0x825 | 0xa27 | 0x827 | 0xa24 | 0x424 | 0x624 | 0x824 => {
                lowering.lower_integer_multiply(opcode, native_predicate, rewrite.rewritten_uniform)
            }
            0x806 | 0x886 | 0x3c2 | 0x81c | 0x89c | 0x883 | 0x882 | 0xc82 | 0xab9 | 0xabb
            | 0x899 | 0xb82 | 0xf89 | 0x822 => {
                lowering.lower_vote_and_uniform(opcode, native_predicate)
            }
            0xb6c | 0x989 | 0x589 | 0x98e | 0x2bd | 0x2be | 0x2bf | 0x3a1 | 0x3c4 | 0xf8c | 0x38c
            | 0x3a8 | 0x9a8 | 0xb97 | 0xb9b | 0x593 | 0x59f => lowering
                .lower_texture_and_reduction(
                    opcode,
                    native_predicate,
                    &mut program.result.required_texture_buffer,
                ),
            0x301 | 0x219 | 0xa19 | 0xc19 | 0x300 | 0xd00 | 0x309 | 0xd09 | 0x918 | 0x992 | 0x9ab
            | 0xb1d | 0x388 | 0x984 | 0x23a => {
                lowering.lower_bits_and_shared(opcode, native_predicate)
            }
            0x23c => lowering.lower_matrix_multiply(uses_uniform_bank, options.tensor),
            0x83b => lowering.lower_matrix_load(),
            0x919 | 0x9c3 | 0x28c | 0x88c | 0x287 | 0x887 | 0x381 | 0x981 | 0x386 | 0x986
            | 0x95b => lowering.lower_special_and_global(opcode, code, index, &program.relocations),
            0x305 | 0xb05 | 0xd05 | 0xb6f | 0x370 | 0xb66 | 0x91a => lowering
                .lower_conversion_and_fetch(opcode, &mut program.result.required_texture_buffer),
            0xb69 | 0xb63 | 0xf63 | 0xb60 | 0xf60 | 0x361 => {
                lowering.lower_texture_sample(opcode, &mut program.result.required_texture_buffer, code, index, uses_uniform_bank)
            }
            0x816 | 0x307 | 0xb07 | 0x321 | 0x322 | 0x387 | 0x983 | 0x803 | 0x804 | 0xc02 | 0x202
            | 0x802 | 0xa02 => lowering.lower_permutation_and_moves(opcode),
            0x223 | 0x423 | 0x623 | 0x823 | 0xa23 | 0xc23 | 0xe23 | 0x220 | 0x820 | 0xa20
            | 0xc20 | 0x221 | 0x421 | 0x621 | 0x821 | 0xa21 | 0xe21 | 0x209 | 0x809 | 0xa09
            | 0xc09 => lowering.lower_float_arithmetic(opcode),
            0x308 | 0x908 | 0xb08 | 0xd08 | 0x526 | 0x326 | 0x81a | 0x299 | 0x499 | 0x419 | 0x805 | 0x306 | 0xd06
            | 0x245 | 0xa45 | 0xc45 => {
                lowering.lower_special_functions_and_conversion(opcode, native_predicate)
            }
            0x207 | 0x807 | 0xa07 | 0xc07 | 0x208 | 0x808 | 0xa08 | 0x20a | 0x80a | 0xa0a | 0xc0a | 0x234 | 0x434
            | 0x20b | 0x80b | 0xa0b | 0xc0b | 0x20c | 0x80c | 0xa0c | 0xc0c | 0x212 | 0x812 | 0xa12
            | 0xc12 | 0x213 | 0xa13 | 0xc13 | 0x217 | 0x817 | 0xa17 | 0xc17 | 0x819 => {
                lowering.lower_selection_and_logic(opcode)
            }
            0xa11 | 0x211 | 0x811 | 0xc11 => lowering.lower_effective_address(opcode, rewrite.rewritten_uniform),
            0x210 | 0x810 | 0xa10 | 0xc10 => {
                lowering.lower_integer_add(opcode, rewrite.rewritten_uniform, rewrite.guarded_uniform)
            }
            0x949 | 0x958 => {
                if insn.bits(12, 4) != 7
                    || insn.words[2] & !0x3ffff
                        != if opcode == 0x958 {
                            0x0b800000
                        } else {
                            0x03800000
                        }
                {
                    return program.fail(index, "native computed branch mode");
                }
                let raw = u64::from(insn.words[1]) | (u64::from(insn.words[2] & 0x3ffff) << 32);
                let relative = ((raw << 14) as i64) >> 14;
                // NX2 extension to Fritchii's zero-base subset: preserve the native
                // table base. FlowCfg adds it before translating native labels.
                let native_base = i64::from(index) * 16 + 16 + relative;
                if native_base < 0 || native_base > i64::from(u32::MAX) || native_base % 16 != 0 {
                    return program.fail(index, "native computed branch relative base");
                }
                let offset = native_base - i64::from(ProgramLowering::address_of(
                    lowering.instructions.len() as u32
                )) - 8;
                if !(-0x800000..=0x7fffff).contains(&offset) {
                    return program.fail(index, "native computed branch range");
                }
                let source = if opcode == 0x958 {
                    uniforms.register(insn.bits(24, 8))
                } else {
                    insn.bits(24, 8)
                };
                if source == u32::MAX {
                    return program.fail(index, "native uniform computed branch source");
                }
                lowering.emit(
                    0xe25000000000000f
                        | lowering.predicate
                        | (u64::from(source) << 8)
                        | (((offset as u64) & 0xffffff) << 20),
                );
                program.computed_branch = true;
                Ok(())
            }
            0x944 => {
                if BoundedControl::is_loop_transfer(code, index as usize) {
                    program.relocations.push(Relocation {
                        call: false,
                        output_target: None,
                        output_index: lowering.instructions.len() as u32,
                        target: i64::from(index + 2),
                    });
                    lowering.emit(0xe24000000000000f | lowering.predicate);
                } else {
                    let adjacent = index != 0
                        && code[index as usize - 1].words[0] & !0x00ff0000 == 0x00007802
                        && code[index as usize - 1].bits(16, 8) < 250
                        && code[index as usize - 1].words[1] == (index + 1) * 16
                        && code[index as usize - 1].words[2] == 0xf00;
                    if index == 0 || insn.words[0] != 0x7944
                        || insn.words[2] & !0x3ffff != 0x03c00000 || (!adjacent && !control.bounded)
                    { return program.fail(index, "native call linkage mode"); }
                    let raw = u64::from(insn.words[1]) | (u64::from(insn.words[2] & 0x3ffff) << 32);
                    let target = i64::from(index) * 16 + 16 + (((raw << 14) as i64) >> 14);
                    if target < 0 || target % 16 != 0 {
                        return program.fail(index, "native call destination alignment");
                    }
                    let link = if adjacent { code[index as usize - 1].bits(16, 8) } else { control.links[index as usize] };
                    for (&call_target, &call_link) in
                        program.call_targets.iter().zip(&program.call_links)
                    {
                        if i64::from(call_target) == target / 16 && call_link != link {
                            return program.fail(index, "native inconsistent call linkage");
                        }
                    }
                    program.call_targets.push((target / 16) as u32);
                    program.call_links.push(link);
                    program.relocations.push(Relocation {
                        call: true,
                        output_target: None,
                        output_index: lowering.instructions.len() as u32,
                        target: target / 16,
                    });
                    lowering.emit(0xe26000000007000f);
                }
                Ok(())
            }
            0x950 => {
                let return_reg = insn.bits(24, 8);
                let copies_link = |prior: &Instruction| {
                    if prior.bits(16, 8) != return_reg || prior.bits(12, 4) != 7 {
                        return false;
                    }
                    if prior.words[0] & 0xff00ffff == 0x00007202 {
                        return Some(prior.words[1]) == active_link && prior.words[2] == 0xf00;
                    }
                    if prior.words[0] & 0xff00ffff == 0xff007224 {
                        return prior.words[1] == 0xff
                            && active_link
                                .is_some_and(|link| prior.words[2] == (0x078e0000 | link));
                    }
                    false
                };
                let mut linked = active_link == Some(return_reg);
                if active_link.is_some() && !linked {
                    for prior in (active_entry..index).rev() {
                        if copies_link(&code[prior as usize]) {
                            linked = true;
                            break;
                        } else if code[prior as usize].bits(16, 8) == return_reg {
                            break;
                        }
                    }
                }
                if !program.main_exited
                    || !linked
                    || insn.words[0] & 0x00ffffff != 0x00007950
                    || return_reg >= 249
                    || insn.words[2] & !0x3ffff != 0x03c00000
                    || !program.barrier_tokens.is_empty()
                {
                    return program.fail(index, "native return linkage mode");
                }
                let raw = u64::from(insn.words[1]) | (u64::from(insn.words[2] & 0x3ffff) << 32);
                if i64::from(index) * 16 + 16 + (((raw << 14) as i64) >> 14) != 0 {
                    return program.fail(index, "native return relative base");
                }
                lowering.emit(0xe32000000007000f);
                if !program.computed_branch
                    && program.call_targets.iter().all(|&target| target <= index)
                {
                    return program.finish(index);
                }
                Ok(())
            }
            // Upstream reconvergence operations intentionally emit nothing.
            0x945 | 0x941 | 0x942 | 0x948 => Ok(()),
            0x947 => {
                let mode = insn.bits(32, 2);
                if (mode <= 1 && insn.bits(91, 1) != 0)
                    || (mode == 1 && (insn.bits(82, 5) != 0 || insn.bits(92, 13) != 0)) {
                    return program.fail(index, "native branch convergence mode");
                }
                if mode != 2 {
                let mut branch_predicate = lowering.predicate;
                if insn.bits(87, 4) != 7 {
                    if insn.bits(12, 4) != 7 {
                        program.relocations.push(Relocation {
                            call: false,
                            output_target: None,
                            output_index: lowering.instructions.len() as u32,
                            target: i64::from(index + 1),
                        });
                        lowering.emit(0xe24000000000000f | (u64::from(insn.bits(12, 4) ^ 8) << 16));
                    }
                    branch_predicate = u64::from(insn.bits(87, 4)) << 16;
                }
                let raw = u64::from(insn.words[1] & !3) | (u64::from(insn.words[2] & 0x3ffff) << 32);
                let target = i64::from(index) * 16 + 16 + (((raw << 14) as i64) >> 14);
                if target < 0 || target % 16 != 0 {
                    return program.fail(index, "native branch destination alignment");
                }
                program.relocations.push(Relocation {
                    call: false,
                    output_target: None,
                    output_index: lowering.instructions.len() as u32,
                    target: target / 16,
                });
                lowering.emit(0xe24000000000000f | branch_predicate);
                if program.computed_branch
                    && target == i64::from(index) * 16
                    && branch_predicate == 7u64 << 16
                {
                    return program.finish(index);
                }
                }
                Ok(())
            }
            0x923 | 0x927 if stage_buffer => {
                // Eglantine 900ab5a8 non-geometry ISBERD/ISBEWR arm, verbatim
                // bit allocation. Geometry pass-through is a separate path.
                let write = opcode == 0x927;
                let region = insn.bits(76, 2);
                let output = insn.bits(79, 1) != 0;
                if (insn.words[2] & !0xf800) != 0
                    || !matches!(region, 0 | 3)
                    || write != output
                    || (!write && region != 3)
                    || (!write && lowering.dest == 255)
                {
                    Err("native stage buffer access mode")
                } else {
                    lowering.emit(
                        0xefc0000000000000
                            | lowering.predicate
                            | if write {
                                u64::from(insn.bits(32, 8))
                            } else {
                                lowering.dest
                            }
                            | (u64::from(insn.bits(24, 8)) << 8)
                            | (u64::from(insn.bits(40, 24)) << 20)
                            | (u64::from(insn.bits(75, 1)) << 44)
                            | (u64::from(region == 3) << 45)
                            | (u64::from(insn.bits(78, 1)) << 46)
                            | (u64::from(output) << 47)
                            | (u64::from(write) << 48),
                    );
                    Ok(())
                }
            }
            0x94d => {
                if insn.bits(84, 2) != 0 {
                    return program.fail(index, "native conditional/special EXIT");
                }
                if insn.bits(87, 4) != 7 {
                    if native_predicate != 7 {
                        program.relocations.push(Relocation {
                            call: false,
                            output_target: None,
                            output_index: lowering.instructions.len() as u32,
                            target: i64::from(index + 1),
                        });
                        lowering.emit(0xe24000000000000f | (u64::from(native_predicate ^ 8) << 16));
                    }
                    lowering.emit(0xe30000000000000f | (u64::from(insn.bits(87, 4)) << 16));
                } else {
                    lowering.emit(0xe30000000000000f | lowering.predicate);
                    if insn.bits(12, 4) == 7 {
                        if !program.barrier_tokens.is_empty() {
                            return program.fail(index, "native unclosed reconvergence scope");
                        }
                        if program.call_targets.is_empty() {
                            let pending_branch = program
                                .relocations
                                .iter()
                                .any(|r| r.target > i64::from(index));
                            if !pending_branch && !program.computed_branch {
                                return program.finish(index);
                            }
                        } else if !program.main_exited && program.relocations.iter()
                            .any(|r| !r.call && r.target > i64::from(index)) {
                            // This EXIT terminates only one path of the main program.
                        } else if program.main_exited {
                            let complete = program
                                .relocations
                                .iter()
                                .all(|r| r.target <= i64::from(index))
                                && program.call_targets.iter().all(|&target| target <= index);
                            if complete && !program.computed_branch {
                                return program.finish(index);
                            }
                        } else {
                            if program.call_targets.iter().any(|&target| target <= index) {
                                return program
                                    .fail(index, "native call target overlaps main program");
                            }
                            let pending_branch = program.relocations.iter().any(|r|
                                r.target > i64::from(index)
                                    && !program.call_targets.contains(&(r.target as u32)));
                            if !pending_branch { program.main_exited = true; }
                        }
                    }
                }
                Ok(())
            }
            _ => Err("unsupported native opcode"),
        };
        if let Err(error) = result {
            return program.fail(index, error);
        }
        if let Err(error) = rewrite.end(&mut lowering) {
            return program.fail(index, error);
        }
        if let Some(pressure) = pressure {
            if !pressure.suffix.is_empty() {
                for pending in &mut program.relocations[relocation_start..] {
                    if !pending.call && pending.output_index >= instruction_start
                        && pending.target == i64::from(index + 1)
                    { pending.output_target = Some(program.result.instructions.len() as u32); }
                }
                program.result.instructions.extend_from_slice(&pressure.suffix);
            }
        }
    }
    let reason = if program.main_exited {
        "native program lacks captured library return"
    } else {
        "native program has no unconditional EXIT"
    };
    program.fail(code.len() as u32, reason)
}

#[allow(dead_code)]
struct Relocation {
    call: bool,
    output_target: Option<u32>,
    output_index: u32,
    target: i64,
}

// State and lambdas from Fritchii's bounded-control preflight. Kept in the
// lowering owner; integrating the result must also preserve CALL/RET/EXIT ordering.
#[derive(Debug)]
struct BoundedControl {
    bounded: bool,
    links: Vec<u32>,
    direct_targets: Vec<u32>,
    checked_returns: Vec<bool>,
    dead_instructions: Vec<bool>,
}

impl BoundedControl {
    // NX2 extension to the TLD4 arm's constant-offset proof. Only a locally
    // dominating definition is needed; unrelated branches elsewhere are allowed.
    fn gather_constant_offsets(code: &[Instruction], index: usize, source: u32) -> Option<[u32; 2]> {
        let predicate = code[index].bits(12, 4);
        let mut values = [None; 2];
        let mut starts = [index; 2];
        let mut guard_intact = true;
        for pc in (0..index).rev() {
            let insn = code[pc];
            let pred = insn.bits(12, 4);
            if pred == 15 { continue; }
            let opcode = insn.bits(0, 12);
            let dest = insn.bits(16, 8);
            let immediate = (opcode == 0x802 && insn.bits(24, 8) == 0 && insn.words[2] == 0xf00)
                || (opcode == 0x424 && insn.bits(24, 8) == 255 && insn.words[2] == 0x078e00ff);
            if opcode == 0xb63 {
                let count = insn.bits(72, 4).count_ones();
                let high = insn.bits(64, 8);
                // Conservatively cover full-precision writes even for packed F16.
                for component in 0..2 {
                    if values[component].is_some() { continue; }
                    let reg = source + component as u32;
                    if (dest != 255 && dest <= reg && reg < dest + if high == 255 { count } else { count.min(2) })
                        || (high != 255 && count > 2 && high <= reg && reg < high + count - 2) { return None; }
                }
                guard_intact &= insn.bits(81, 3) == 7;
                continue;
            }
            let count = match opcode {
                0x224 | 0x624 | 0x827 | 0xa27 | 0x211 | 0xa11 | 0x831 | 0x832 => 1,
                0x805 => 2,
                0x945 | 0x947 | 0x941 | 0x942 | 0x948 | 0x94d | 0x91a => 0,
                _ => Self::write_count(&insn)?,
            };
            for component in 0..2 {
                if values[component].is_some() { continue; }
                let reg = source + component as u32;
                if dest <= reg && reg < dest + count {
                    if dest != reg || !immediate || !(pred == 7 || (pred == predicate && guard_intact)) { return None; }
                    values[component] = Some(insn.words[1]);
                    starts[component] = pc;
                }
            }
            if values.iter().all(Option::is_some) { break; }
            // Restrict predicated definitions to a provably predicate-preserving
            // interval. Unconditional constants do not depend on this flag.
            guard_intact &= immediate
                || matches!(opcode, 0x202 | 0x802 | 0x918 | 0x805 | 0x819 | 0x91a
                    | 0x945 | 0x947 | 0x941 | 0x942 | 0x948 | 0x94d
                    | 0x220 | 0xa20 | 0x221 | 0x421 | 0x621 | 0x223 | 0x423
                    | 0x623 | 0x823 | 0xa23 | 0x245 | 0x306 | 0x831 | 0x832)
                || (matches!(opcode, 0x212 | 0x812 | 0xa12 | 0xc12)
                    && insn.bits(81, 3) != (predicate & 7))
                || (matches!(opcode, 0x210 | 0x810 | 0xa10 | 0xc10)
                    && insn.bits(81, 6) == 0x3f)
                || (opcode == 0x81c
                    && insn.bits(81, 3) != (predicate & 7)
                    && insn.bits(84, 3) != (predicate & 7))
                || (matches!(opcode, 0x224 | 0x424 | 0x624 | 0x824)
                    && insn.words[2] & !0x2ff == 0x078e0000);
        }
        let values = [values[0]?, values[1]?];
        for value in values {
            for shift in (0..32).step_by(8) {
                let byte = (value >> shift) & 255;
                if byte > 31 && byte < 224 { return None; }
            }
        }
        for (pc, insn) in code.iter().enumerate() {
            let opcode = insn.bits(0, 12);
            if matches!(opcode, 0x945 | 0x947) {
                let raw = u64::from(insn.words[1] & if opcode == 0x947 { !3 } else { u32::MAX })
                    | (u64::from(insn.words[2] & 0x3ffff) << 32);
                let target = pc as i64 * 16 + 16 + (((raw << 14) as i64) >> 14);
                if target < 0 || target % 16 != 0 || target as usize / 16 >= code.len() { return None; }
                // BSSY targets account conservatively for subsequent BSYNC/BREAK.
                for start in starts {
                    if start < target as usize / 16 && target as usize / 16 <= index
                        && (pc < start || pc >= index) { return None; }
                }
            } else if (0x140..=0x15f).contains(&(opcode & 0x1ff))
                && !matches!(opcode, 0x941 | 0x942 | 0x948 | 0x348 | 0x94d | 0x95b) {
                return None;
            }
        }
        Some(values)
    }

    // Native counterpart of Maxwell TrackLDC/TrackSHL/TrackIMNMX. This is used
    // for a linkage proof, so require reaching definitions (not merely matching
    // opcodes), unconditional writes, unsigned MIN and an exact four-byte stride.
    fn branch_table(code: &[Instruction], index: usize) -> Option<(usize, u32, u32, u32, u32)> {
        fn definition(code: &[Instruction], end: usize, reg: u32) -> Option<usize> {
            if reg == 255 { return None; }
            for pc in (0..end).rev() {
                let insn = &code[pc];
                let dest = insn.bits(16, 8);
                if dest == reg { return Some(pc); }
                let count = BoundedControl::write_count(insn)?;
                if dest <= reg && reg < dest + count { return None; }
            }
            None
        }
        let branch = code[index];
        if branch.bits(0, 12) != 0x949 || branch.bits(12, 4) != 7
            || branch.words[2] & !0x3ffff != 0x03800000 { return None; }
        let ldc_pc = definition(code, index, branch.bits(24, 8))?;
        let ldc = code[ldc_pc];
        if ldc.bits(0, 12) != 0xb82 || ldc.bits(12, 4) != 7
            || ldc.words[2] != 0x800 || ldc.words[1] & 0xf800003f != 0 { return None; }
        let bank = match ldc.bits(54, 5) { 24 => 17, value => value };
        let offset = ldc.bits(38, 16);
        if bank >= 18 || offset >= 0x8000 || offset % 4 != 0 { return None; }
        let shift_pc = definition(code, ldc_pc, ldc.bits(24, 8))?;
        let shift = code[shift_pc];
        if shift.bits(0, 12) != 0x819 || shift.bits(12, 4) != 7
            || shift.words[1] != 2 || shift.words[2] != 0x6ff { return None; }
        let clamp_pc = definition(code, shift_pc, shift.bits(24, 8))?;
        let clamp = code[clamp_pc];
        if clamp.bits(0, 12) != 0x817 || clamp.bits(12, 4) != 7
            || clamp.words[2] != 0x03800000 || clamp.words[1] >= 0x4000 { return None; }
        let count = clamp.words[1] + 1;
        if offset + count * 4 > 0x10000 { return None; }
        let raw = u64::from(branch.words[1]) | (u64::from(branch.words[2] & 0x3ffff) << 32);
        let base = index as i64 * 16 + 16 + (((raw << 14) as i64) >> 14);
        if !(0..=i64::from(u32::MAX)).contains(&base) || base % 16 != 0 { return None; }
        Some((clamp_pc, bank, offset, count, base as u32))
    }

    // Same NOINC loop-transfer idiom as the upstream CALL emitter. It has no
    // return-link contract, so preflight must classify it as a branch too.
    fn is_loop_transfer(code: &[Instruction], index: usize) -> bool {
        let insn = code[index];
        insn.bits(0, 12) == 0x944 && insn.bits(12, 4) != 7
            && insn.words[1] == 16 && insn.words[2] == 0x03c00000
            && index + 2 < code.len() && code[index + 1].bits(0, 12) == 0x947
    }

    fn write_count(instruction: &Instruction) -> Option<u32> {
        match instruction.bits(0, 12) {
            0x202 | 0x802 | 0x424 | 0x724 | 0x824 | 0xe24 | 0x227
            | 0x810 | 0x819 | 0x845 | 0x906 | 0x820 | 0x812 | 0x589
            | 0x212 | 0x207 | 0x807 | 0x208 | 0x808 | 0x220 | 0xa20 | 0x221 | 0x421 | 0x621
            | 0x223 | 0x423 | 0x623 | 0x823 | 0xa23 | 0x243 | 0x245 | 0x306
            | 0x919 | 0xc12 | 0x300 | 0x210 | 0x308 => Some(u32::from(instruction.bits(16, 8) != 255)),
            0x225 | 0x625 | 0x825 => Some(2),
            0x305 => Some(if instruction.bits(75, 2) == 3 { 2 } else { 1 }),
            0x83b => (instruction.bits(72, 2) != 3).then(|| 1 << instruction.bits(72, 2)),
            0x381 | 0x981 | 0x984 | 0xb82 => match instruction.bits(73, 3) {
                0..=4 => Some(1), 5 => Some(2), 6 => Some(4), _ => None,
            },
            0x20c | 0x80c | 0x81c | 0x387 | 0x388 | 0xb1d | 0x348 | 0x918 | 0x886 | 0x95b => Some(0),
            _ => None,
        }
    }

    // Called only after the outer size check, like upstream.
    #[cfg(test)]
    fn analyze(code: &[Instruction], geometry_stage: bool, uses_uniform_bank: bool, high_uniform_base: u32) -> Self {
        Self::analyze_with_cbuf(code, geometry_stage, uses_uniform_bank, high_uniform_base, &mut |_, _| None)
    }

    fn analyze_with_cbuf(code: &[Instruction], geometry_stage: bool, uses_uniform_bank: bool, high_uniform_base: u32,
        read_cbuf: &mut dyn FnMut(u32, u32) -> Option<u32>) -> Self {
        let mut result = Self { bounded: !geometry_stage, links: vec![u32::MAX; code.len()],
            direct_targets: vec![u32::MAX; code.len()], checked_returns: vec![false; code.len()],
            dead_instructions: vec![false; code.len()] };
        let mut entries = vec![false; code.len()];
        let mut edges: Vec<(usize, usize)> = Vec::new();
        let mut table_spans = Vec::new();
        entries[0] = true;
        let ordinary_operands = |instruction: &Instruction| {
            for bit in [16, 24, 32, 64] {
                if bit == 32 && instruction.bits(9, 3) != 1 { continue; }
                let reg = instruction.bits(bit, 8);
                if reg >= if uses_uniform_bank { high_uniform_base } else { 246 } && reg != 255 { return false; }
            }
            true
        };
        for (index, instruction) in code.iter().enumerate() {
            let opcode = instruction.bits(0, 12);
            if matches!(opcode, 0x944 | 0x945 | 0x947) {
                if opcode == 0x944 && index == 0 { result.bounded = false; }
                let mode = instruction.words[2] & !0x3ffff;
                // NX2 extension: BRA's secondary predicate occupies bits 87..90.
                // The emitter already handles both predicates. They change edge
                // execution, not its destination: retain the edge conservatively.
                let valid_mode = if opcode == 0x947 {
                    matches!(mode & !0x07800000, 0 | 0x08000000)
                } else {
                    mode == if opcode == 0x944 { 0x03c00000 } else { 0x03800000 }
                };
                if !valid_mode { result.bounded = false; }
                let raw = u64::from(instruction.words[1] & if opcode == 0x947 { !3 } else { u32::MAX })
                    | (u64::from(instruction.words[2] & 0x3ffff) << 32);
                let relative = ((raw << 14) as i64) >> 14;
                let target = index as i64 * 16 + 16 + relative;
                if target < 0 || target % 16 != 0 || target as u64 / 16 >= code.len() as u64 {
                    result.bounded = false;
                } else {
                    result.direct_targets[index] = (target / 16) as u32;
                    entries[target as usize / 16] = true;
                    edges.push((index, target as usize / 16));
                }
                if opcode == 0x944 && index + 1 < code.len() {
                    entries[index + 1] = true; edges.push((index, index + 1));
                }
            } else if opcode == 0x949 {
                if let Some((start, bank, offset, count, base)) = Self::branch_table(code, index) {
                    table_spans.push((start, index));
                    for slot in 0..count {
                        let Some(value) = read_cbuf(bank, offset + slot * 4) else {
                            result.bounded = false; break;
                        };
                        let target = base.wrapping_add(value);
                        if target % 16 != 0 || target as usize / 16 >= code.len() {
                            result.bounded = false; break;
                        }
                        entries[target as usize / 16] = true;
                        edges.push((index, target as usize / 16));
                    }
                } else { result.bounded = false; }
            } else if (0x140..=0x15f).contains(&(opcode & 0x1ff))
                && !matches!(opcode, 0x941 | 0x942 | 0x948 | 0x348 | 0x94d | 0x950 | 0x95b) {
                result.bounded = false;
            }
        }
        // A jump into the definition chain could bypass its clamp/load. Include
        // all resolved indirect edges before deciding whether the proof holds.
        for (start, end) in table_spans {
            if entries[start + 1..=end].iter().any(|&entry| entry) { result.bounded = false; }
        }
        for index in 1..code.len() {
            if !result.bounded { break; }
            if code[index].bits(0, 12) != 0x944 || Self::is_loop_transfer(code, index) { continue; }
            if code[index].words[0] != 0x7944 { result.bounded = false; break; }
            let mut overwritten = [false; 256];
            let mut setup = index;
            for prior in (0..index).rev() {
                let instruction = &code[prior];
                let destination = instruction.bits(16, 8);
                if (instruction.words[0] & !0x00ff0000) == 0x00007802
                    && instruction.words[1] == (index as u32 + 1) * 16 && instruction.words[2] == 0xf00
                    && instruction.words[3] & 0xc00001ff == 0 && ordinary_operands(instruction)
                    && destination < 246 && !overwritten[destination as usize] {
                    result.links[index] = destination; setup = prior; break;
                }
                let Some(count) = Self::write_count(instruction) else { break; };
                if !ordinary_operands(instruction) || destination + count > 255 { break; }
                for component in 0..count { overwritten[(destination + component) as usize] = true; }
            }
            if result.links[index] == u32::MAX { result.bounded = false; break; }
            for entry in setup + 1..=index { if entries[entry] { result.bounded = false; } }
        }
        for index in 0..code.len() {
            if !result.bounded { break; }
            if code[index].bits(0, 12) != 0x944 || Self::is_loop_transfer(code, index) { continue; }
            let link = result.links[index];
            let target = result.direct_targets[index] as usize;
            if target == 0 { result.bounded = false; break; }
            let previous = code[target - 1];
            if previous.bits(12, 4) != 7 || previous.bits(87, 4) != 7
                || !matches!(previous.bits(0, 12), 0x947 | 0x94d | 0x950) {
                result.bounded = false; break;
            }
            for &(source, destination) in &edges {
                if destination == target && code[source].bits(0, 12) != 0x944 { result.bounded = false; }
            }
            let mut aliases = [false; 256]; aliases[link as usize] = true;
            let mut returned = false;
            for pc in target..code.len() {
                let instruction = &code[pc];
                if pc != target && entries[pc] { break; }
                if instruction.bits(0, 12) == 0x950 {
                    let raw = u64::from(instruction.words[1]) | (u64::from(instruction.words[2] & 0x3ffff) << 32);
                    let relative = ((raw << 14) as i64) >> 14;
                    returned = instruction.words[0] & 0x00ffffff == 0x00007950
                        && instruction.words[2] & !0x3ffff == 0x03c00000
                        && instruction.words[3] & 0xc00001ff == 0 && pc as i64 * 16 + 16 + relative == 0
                        && aliases[instruction.bits(24, 8) as usize];
                    if returned { result.checked_returns[pc] = true; }
                    break;
                }
                let Some(count) = Self::write_count(instruction) else { break; };
                let destination = instruction.bits(16, 8);
                if !ordinary_operands(instruction) || destination + count > 255
                    || (count != 0 && destination <= link && link < destination + count) { break; }
                let mut copy = false;
                if instruction.bits(12, 4) == 7 {
                    if instruction.words[0] & 0xff00ffff == 0x00007202
                        && instruction.words[1] < 256 && instruction.words[2] == 0xf00 {
                        copy = aliases[instruction.words[1] as usize];
                    }
                    if instruction.words[0] & 0xff00ffff == 0xff007224
                        && instruction.words[1] == 0xff && instruction.words[2] & !0xff == 0x078e0000 {
                        copy = aliases[instruction.bits(64, 8) as usize];
                    }
                }
                for component in 0..count { aliases[(destination + component) as usize] = component == 0 && copy; }
            }
            if !returned { result.bounded = false; }
        }
        for index in 0..code.len() {
            if !result.bounded { break; }
            if code[index].bits(0, 12) == 0x950 && !result.checked_returns[index] { result.bounded = false; }
        }
        for index in 0..code.len() {
            if !result.bounded { break; }
            let instruction = &code[index];
            let target = result.direct_targets[index];
            if instruction.bits(0, 12) != 0x947 || instruction.bits(12, 4) != 7
                || instruction.bits(32, 2) == 2 || instruction.bits(16, 8) != 0 || instruction.bits(87, 4) != 7
                || target <= index as u32 || target == u32::MAX || instruction.words[3] & 0xc00001ff != 0
                || if instruction.bits(91, 1) != 0 { instruction.bits(24, 8) >= 64 } else { instruction.bits(24, 8) != 0 }
            { continue; }
            if !(index + 1..target as usize).any(|pc| code[pc].bits(0, 12) == 0x944) { continue; }
            let entered = edges.iter().any(|&(source, destination)| index < destination && destination < target as usize
                && !(index < source && source < target as usize));
            if !entered { for pc in index + 1..target as usize { result.dead_instructions[pc] = true; } }
        }
        result
    }
}

// State captured by the upstream fail/finish lambdas and opcode loop.
#[derive(Default)]
#[allow(dead_code)]
struct ProgramLowering {
    result: LoweredProgram,
    relocations: Vec<Relocation>,
    barrier_tokens: Vec<u32>,
    call_targets: Vec<u32>,
    call_links: Vec<u32>,
    main_exited: bool,
    computed_branch: bool,
}

#[allow(dead_code)]
impl ProgramLowering {
    fn address_of(index: u32) -> u32 {
        0x50u32
            .wrapping_add(index.wrapping_mul(8))
            .wrapping_add((index.wrapping_add(1) / 3).wrapping_mul(8))
    }

    fn fail(mut self, index: u32, reason: &'static str) -> LoweredProgram {
        self.result.instructions.clear();
        self.result.source_instruction_offsets.clear();
        self.result.error_offset = index.wrapping_mul(16);
        self.result.error = reason;
        self.result
    }

    fn finish(mut self, index: u32) -> LoweredProgram {
        for relocation in &self.relocations {
            if relocation.target > i64::from(index) {
                return self.fail(index, "native branch or call leaves captured program");
            }
            // Branch/call validation rejects negative targets before recording them.
            let target = relocation.output_target.unwrap_or_else(||
                self.result.source_instruction_offsets[relocation.target as usize]);
            let relative = i64::from(Self::address_of(target))
                - i64::from(Self::address_of(relocation.output_index))
                - 8;
            if !(-0x800000..=0x7fffff).contains(&relative) {
                return self.fail(index, "lowered branch distance");
            }
            self.result.instructions[relocation.output_index as usize] |=
                ((relative as u64) & 0xffffff) << 20;
        }
        self.result.source_size = index.wrapping_add(1).wrapping_mul(16);
        self.result
    }

    fn uses_uniform_bank(code: &[Instruction]) -> bool {
        code.iter().any(|instruction| {
            let op = instruction.bits(0, 12);
            matches!(
                op,
                0xf63 | 0xf60
                    | 0x3c4
                    | 0x2bd
                    | 0xc0b
                    | 0xc0c
                    | 0x986
                    | 0x9a8
                    | 0xc12
                    | 0xc17
                    | 0xe24
                    | 0xc24
                    | 0xd00
                    | 0xd09
                    | 0x9c3
                    | 0xc45
                    | 0x2a4
                    | 0x891
                    | 0x890
                    | 0x290
                    | 0x28c
                    | 0x88c
                    | 0x287
                    | 0x887
                    | 0x958
                    | 0xab9
                    | 0xabb
                    | 0x899
                    | 0xc20
                    | 0xe21
                    | 0xe23
                    | 0xc02
                    | 0x886
                    | 0x3c2
                    | 0xc82
                    | 0x882
                    | 0x292
                    | 0x892
                    | 0x291
                    | 0x981
                    | 0xc13
                    | 0xc10
                    | 0xc23
                    | 0x526
                    | 0x2be
                    | 0x2bf
                    | 0x8a4
                    | 0xd05
                    | 0x89c
                    | 0x883
                    | 0x2a5
                    | 0xd06
                    | 0xd08
                    | 0x8a5
                    | 0x4a4
                    | 0xc11
                    | 0x299
                    | 0x499
                    | 0xc09
                    | 0xc07 | 0xc0a
            ) || (op == 0x984 && instruction.bits(91, 1) != 0)
                || (op == 0x81c && instruction.bits(67, 1) != 0)
        })
    }
}

// Mechanical extraction of the uniform rewrite block, before the opcode switch.
// These flags are also consumed by individual opcode arms and the loop epilogue.
#[allow(dead_code)]
struct UniformRewrite {
    rewritten_uniform: bool,
    native_predicate: u32,
    guarded_uniform: bool,
    guard_mask: u32,
    output_mask: u32,
    predicate: u64,
}

#[allow(dead_code)]
impl UniformRewrite {
    fn new(insn: &mut Instruction, opcode: &mut u32, uniforms: &UniformRegisters) -> Result<Self, &'static str> {
        let mut rewritten_uniform = false;
        let mut uniform_guard = false;
        let mut uniform_output = 7;
        if matches!(
            *opcode,
            0x2a4 | 0x4a4 | 0x8a4 | 0x8a5 | 0x891 | 0x890 | 0x290 | 0x292 | 0x892 | 0x291
        ) {
            if insn.bits(12, 4) != 7 && *opcode != 0x292 && *opcode != 0x892 {
                let guarded_add = matches!(*opcode, 0x290 | 0x890) && insn.bits(74, 1) == 0
                    && insn.bits(81, 6) == 0x3f && insn.bits(77, 4) == 15 && insn.bits(87, 4) == 15;
                let guarded_lea = matches!(*opcode, 0x291 | 0x891) && insn.bits(74, 1) == 0
                    && insn.bits(80, 1) == 0 && insn.bits(81, 3) == 7;
                let guarded_multiply = *opcode == 0x2a4 && (insn.words[2] & !0x080002ff) == 0x078e0000;
                if !guarded_add && !guarded_lea && !guarded_multiply {
                    return Err("native uniform arithmetic predication");
                }
            }
            uniform_guard = insn.bits(12, 4) != 7;
            rewritten_uniform = true;
            let dst = uniforms.register(insn.bits(16, 8));
            let a = uniforms.register(insn.bits(24, 8));
            let c = uniforms.register(insn.bits(64, 8));
            if dst == u32::MAX || a == u32::MAX || c == u32::MAX {
                return Err("native uniform arithmetic register");
            }
            insn.words[0] = (insn.words[0] & 0xffff) | (dst << 16) | (a << 24);
            insn.words[2] = (insn.words[2] & !0x080000ff) | c;
            if matches!(*opcode, 0x2a4 | 0x290 | 0x292 | 0x291) {
                let b = uniforms.register(insn.bits(32, 8));
                if b == u32::MAX {
                    return Err("native uniform arithmetic second register");
                }
                insn.words[1] = (insn.words[1] & !0xff) | b;
            }
            if (*opcode == 0x292 || *opcode == 0x892) && insn.bits(80, 4) != 0xe {
                if insn.bits(80, 1) != 0 {
                    return Err("native uniform logic predicate output");
                }
                uniform_output = insn.bits(81, 3);
                insn.words[2] = (insn.words[2] & !(7 << 17)) | ((6 - uniform_output) << 17);
            }
            *opcode = match *opcode {
                0x2a4 => 0x224,
                0x4a4 => 0x424,
                0x8a4 => 0x824,
                0x8a5 => 0x825,
                0x891 => 0x811,
                0x890 => 0x810,
                0x292 => 0x212,
                0x892 => 0x812,
                0x291 => 0x211,
                _ => 0x210,
            };
        }
        let uniform_predication = matches!(*opcode, 0xabb | 0xab9 | 0xc82 | 0x882 | 0x899 | 0x299 | 0x499);
        let native_predicate = insn.bits(12, 4);
        let guarded_uniform = (uniform_predication || uniform_guard) && native_predicate != 7;
        let guard_slot = 6u32.wrapping_sub(native_predicate & 7);
        let predicate = u64::from(if guarded_uniform {
            (native_predicate & 8) | guard_slot
        } else {
            native_predicate
        }) << 16;
        Ok(Self {
            rewritten_uniform,
            native_predicate,
            guarded_uniform,
            predicate,
            guard_mask: if guard_slot < 7 { 1 << guard_slot } else { 0 },
            output_mask: if uniform_output == 7 {
                0
            } else {
                1 << (6 - uniform_output)
            },
        })
    }

    fn begin(&self, lowering: &mut InstructionLowering<'_>) {
        if self.guarded_uniform {
            lowering.save_predicate(self.guard_mask);
            lowering.emit(0x38f0000000070000 | (239u64 << 8) | (u64::from(self.guard_mask) << 20));
        }
        if self.output_mask != 0 {
            lowering.emit(
                0x38e8000000070000 | 254 | (255u64 << 8) | (u64::from(self.output_mask) << 20),
            );
            lowering.emit(0x38f0000000070000 | (239u64 << 8) | (u64::from(self.output_mask) << 20));
        }
    }

    fn end(&self, lowering: &mut InstructionLowering<'_>) -> Result<(), &'static str> {
        if !lowering.operand_error.is_empty() {
            return Err(lowering.operand_error);
        }
        if self.output_mask != 0 {
            lowering.emit(
                0x38e8000000070000 | 239 | (239u64 << 8) | (u64::from(self.output_mask) << 20),
            );
            lowering.emit(0x38f0000000070000 | (254u64 << 8) | (u64::from(self.output_mask) << 20));
        }
        if self.guarded_uniform {
            lowering.restore_predicate(self.guard_mask);
        }
        Ok(())
    }
}

// Captures of the upstream uniform_register lambda, shared by rewriting and emission.
#[derive(Default)]
struct UniformRegisters {
    high_uniforms: u32,
    requested: Cell<u32>,
}

impl UniformRegisters {
    fn base(&self) -> u32 { 216 - self.high_uniforms }

    fn register(&self, reg: u32) -> u32 {
        if reg < 22 { return 216 + reg; }
        if reg < 28 { return 218 + reg; }
        if reg == 63 { return 255; }
        if reg < 63 && reg - 28 < self.high_uniforms {
            return self.base() + reg - 28;
        }
        if reg < 63 {
            self.requested.set(self.requested.get().max((reg - 26) & !1));
        }
        u32::MAX
    }
}

// Mechanical context for LowerToMaxwell's per-instruction captured lambdas.
// Their state and emitted words stay in this upstream-owned file.
#[allow(dead_code)]
struct InstructionLowering<'a> {
    uniforms: &'a UniformRegisters,
    insn: Instruction,
    predicate: u64,
    dest: u64,
    operand_error: &'static str,
    instructions: &'a mut Vec<u64>,
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
struct HalfSource {
    reg: u32,
    swizzle: u32,
    absolute: bool,
    negate: bool,
}

#[allow(dead_code)]
impl InstructionLowering<'_> {
    fn lower_effective_address(&mut self, opcode: u32, rewritten_uniform: bool) -> Result<(), &'static str> {
        let insn = self.insn;
        if insn.bits(80, 1) != 0 {
            let extended = insn.bits(74, 1) != 0;
            let high = insn.bits(64, 8);
            if insn.bits(72, 1) != 0 || insn.bits(81, 3) != 7
                || (high != 255 && insn.bits(73, 1) != 0)
                || (matches!(opcode, 0x211 | 0xa11) && insn.bits(62, 1) != 0)
                || (opcode == 0xc11 && insn.words[1] & !0x8000003f != 0) {
                return Err("native high effective address mode");
            }
            let mut b = if opcode == 0xc11 { self.uniforms.register(insn.bits(32, 8)) } else { insn.bits(32, 8) };
            if opcode == 0xa11 { b = self.operand(1, false); }
            if b == u32::MAX { return Err("native uniform LEA register"); }
            if opcode == 0x811 {
                b = if insn.words[1] != 0 { 251 } else { 255 };
                if b == 251 { self.mov_imm(251, insn.words[1]); }
            }
            let shift = (u64::from(32 - insn.bits(75, 5)) << 20) | (u64::from(insn.bits(73, 1)) << 48);
            let high_word = |this: &mut Self, target: u64| {
                if high == 255 {
                    this.emit(target | 0x3828000000000000 | (u64::from(insn.bits(24, 8)) << 8) | shift);
                } else {
                    this.emit(target | 0x36f8000000000000 | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(high) << 39) | (u64::from(insn.bits(75, 5)) << 20));
                }
            };
            if b == 255 && !extended {
                high_word(self, self.base(0));
            } else {
                high_word(self, self.predicate | 250);
                if extended {
                    self.mov_imm(252, 1);
                    self.emit(0x5ca0000000000000 | self.predicate | 252 | (252u64 << 8) | (255u64 << 20)
                        | (u64::from(insn.bits(87, 3)) << 39) | (u64::from(insn.bits(90, 1)) << 42));
                    self.emit(0x5c10000000000000 | self.predicate | 250 | (250u64 << 8) | (252u64 << 20));
                }
                // LEA.HI uses the same Rb negation bit as LEA; immediate
                // bit 63 is part of the immediate value, not a modifier.
                self.emit(self.base(0x5c10000000000000) | (250u64 << 8) | (u64::from(b) << 20)
                    | (u64::from(opcode != 0x811 && insn.bits(63, 1) != 0) << 48));
            }
            return Ok(());
        }
        let carry_out = insn.bits(81, 3);
        if insn.bits(72, 1) != 0 || insn.bits(74, 1) != 0 || insn.bits(80, 1) != 0
            || (carry_out != 7 && ((opcode != 0x811 && insn.bits(63, 1) != 0)
                || (rewritten_uniform && (opcode != 0x211 || carry_out > 1))))
            || (matches!(opcode, 0x211 | 0xa11) && insn.bits(62, 1) != 0)
            || (opcode == 0xc11 && insn.words[1] & !0x8000003f != 0) {
            return Err("native LEA carry/high mode");
        }
        let mut b = if opcode == 0xc11 { self.uniforms.register(insn.bits(32, 8)) } else { insn.bits(32, 8) };
        if opcode == 0xa11 { b = self.operand(1, false); }
        if b == u32::MAX { return Err("native uniform LEA register"); }
        if opcode == 0x811 { self.mov_imm(251, insn.words[1]); b = 251; }
        if opcode == 0xa11 && carry_out != 7 {
            self.emit(self.base(0x5c18000000000000) | (u64::from(insn.bits(24, 8)) << 8)
                | (u64::from(b) << 20) | (u64::from(insn.bits(75, 5)) << 39));
            self.emit(0x5b60000000000000 | self.predicate | 7 | (u64::from(carry_out) << 3)
                | (self.dest << 8) | (u64::from(b) << 20) | (7u64 << 39) | (1u64 << 49));
            return Ok(());
        }
        if carry_out != 7 {
            let target = if rewritten_uniform { 6 - carry_out } else { carry_out };
            let mask = u64::from(1u32 << target);
            self.emit(0x5c18000000000000 | self.predicate | 250 | (u64::from(insn.bits(24, 8)) << 8)
                | (u64::from(b) << 20) | (u64::from(insn.bits(75, 5)) << 39));
            if rewritten_uniform {
                self.emit(0x38e8000000070000 | 253 | (255u64 << 8) | (mask << 20));
            }
            self.emit(0x5b60000000000000 | self.predicate | 7 | (u64::from(target) << 3)
                | (250u64 << 8) | (u64::from(b) << 20) | (7u64 << 39) | (1u64 << 49));
            if rewritten_uniform {
                self.emit(0x38e8000000070000 | 239 | (239u64 << 8) | (mask << 20));
                self.emit(0x38f0000000070000 | (253u64 << 8) | (mask << 20));
            }
            self.emit(self.base(0x5c98000000000000) | (15u64 << 39) | (250u64 << 20));
            return Ok(());
        }
        self.emit(self.base(0x5c18000000000000) | (u64::from(insn.bits(24, 8)) << 8)
            | (u64::from(b) << 20) | (u64::from(insn.bits(75, 5)) << 39)
            | (u64::from(if opcode != 0x811 { insn.bits(63, 1) } else { 0 }) << 48));
        Ok(())
    }

    fn lower_integer_add(
        &mut self,
        opcode: u32,
        rewritten_uniform: bool,
        guarded_uniform: bool,
    ) -> Result<(), &'static str> {
        let insn = self.insn;
        assert!(matches!(opcode, 0x210 | 0x810 | 0xa10 | 0xc10));
        if rewritten_uniform && opcode == 0x210 && insn.bits(74, 1) != 0 && insn.bits(81, 6) == 0x3f {
            if guarded_uniform || (insn.words[2] & !0x0781e0ff) != 0x007e0400
                || insn.words[1] & !0xff != 0 {
                return Err("native uniform extended add mode");
            }
            self.emit(0x5c10000000000000 | self.predicate | 250 | (u64::from(insn.bits(24, 8)) << 8)
                | (u64::from(insn.bits(32, 8)) << 20));
            self.emit(0x5c10000000000000 | self.predicate | 250 | (250u64 << 8) | (u64::from(insn.bits(64, 8)) << 20));
            let mut mask = 0;
            for bit in [77, 87] {
                let source = insn.bits(bit, 3);
                if source != 7 { mask |= 1 << (6 - source); }
            }
            self.save_predicate(mask);
            self.emit(0x38f0000000070000 | (239u64 << 8) | (u64::from(mask) << 20));
            self.mov_imm(252, 1);
            for bit in [77, 87] {
                let source = insn.bits(bit, 3);
                self.emit(0x5ca0000000000000 | self.predicate | 251 | (252u64 << 8) | (255u64 << 20)
                    | (u64::from(if source == 7 { 7 } else { 6 - source }) << 39)
                    | (u64::from(insn.bits(bit + 3, 1)) << 42));
                self.emit(0x5c10000000000000 | self.predicate | 250 | (250u64 << 8) | (251u64 << 20));
            }
            self.restore_predicate(mask);
            self.emit(self.base(0x5c98000000000000) | (15u64 << 39) | (250u64 << 20));
            return Ok(());
        }
        if rewritten_uniform && opcode == 0x210 && insn.bits(81, 6) != 0x3f {
            let first = insn.bits(81, 3);
            let second = insn.bits(84, 3);
            if guarded_uniform || (insn.words[2] & !0x007e00ff) != 0x0781e000
                || insn.words[1] & !0xff != 0 || (first != 7 && first == second) {
                return Err("native uniform add carry mode");
            }
            self.emit(0x5c10000000000000 | self.predicate | 250 | (u64::from(insn.bits(24, 8)) << 8)
                | (u64::from(insn.bits(32, 8)) << 20) | (1u64 << 47));
            self.emit(0x5c10000000000000 | self.predicate | 251 | (255u64 << 8) | (255u64 << 20) | (1u64 << 43));
            self.emit(0x5c10000000000000 | self.predicate | 250 | (250u64 << 8) | (u64::from(insn.bits(64, 8)) << 20) | (1u64 << 47));
            self.emit(0x5c10000000000000 | self.predicate | 251 | (251u64 << 8) | (255u64 << 20) | (1u64 << 43));
            let mask = (if first == 7 { 0 } else { 1 << (6 - first) })
                | (if second == 7 { 0 } else { 1 << (6 - second) });
            self.save_predicate(mask);
            self.emit(self.base(0x5c98000000000000) | (15u64 << 39) | (250u64 << 20));
            for output in 0..2 {
                let target = if output != 0 { second } else { first };
                if target == 7 { continue; }
                self.mov_imm(252, output + 1);
                self.emit(0x5b60000000000000 | self.predicate | 7 | (u64::from(6 - target) << 3)
                    | (251u64 << 8) | (252u64 << 20) | (7u64 << 39) | (6u64 << 49));
            }
            self.emit(0x38e8000000070000 | 239 | (239u64 << 8) | (u64::from(mask) << 20));
            self.restore_predicate(mask);
            return Ok(());
        }
        if matches!(opcode, 0x210 | 0x810)
            && insn.bits(64, 8) == 255
            && insn.bits(73, 1) == 0
            && insn.bits(75, 2) == 0
            && insn.bits(84, 3) == 7
            && insn.bits(77, 4) == 15
            && insn.bits(72, 1) == 0
            && (insn.bits(81, 3) != 7 || insn.bits(74, 1) != 0)
            && (opcode == 0x810 || insn.bits(63, 1) == 0)
        {
            let carry_in = insn.bits(74, 1) != 0;
            let carry_out = insn.bits(81, 3);
            let a = insn.bits(24, 8);
            if (carry_in && carry_out != 7)
                || (opcode == 0x210 && insn.words[1] & !0xff != 0)
                || (rewritten_uniform && carry_out != 7 && carry_out > 1)
                || (rewritten_uniform && carry_in && insn.bits(87, 3) > 1 && insn.bits(87, 3) != 7)
            {
                return Err("native integer carry mode");
            }
            if opcode == 0x810 {
                self.emit(
                    0x1c00000000000000
                        | self.predicate
                        | 250
                        | (u64::from(a) << 8)
                        | (u64::from(insn.words[1]) << 20),
                );
            } else {
                self.emit(
                    0x5c10000000000000
                        | self.predicate
                        | 250
                        | (u64::from(a) << 8)
                        | (u64::from(insn.bits(32, 8)) << 20),
                );
            }
            // Upstream's carry helpers use R253 and PT, independently of the outer guard.
            let save = |this: &mut Self, mask: u32| {
                this.emit(0x38e8000000070000 | 253 | (255u64 << 8) | (u64::from(mask) << 20));
            };
            let restore = |this: &mut Self, mask: u32| {
                this.emit(0x38f0000000070000 | (253u64 << 8) | (u64::from(mask) << 20));
            };
            if !carry_in {
                let target = if rewritten_uniform {
                    6 - carry_out
                } else {
                    carry_out
                };
                let mask = 1u32 << target;
                if rewritten_uniform {
                    save(self, mask);
                }
                self.emit(
                    0x5b60000000000000
                        | self.predicate
                        | 7
                        | (u64::from(target) << 3)
                        | (250u64 << 8)
                        | (u64::from(a) << 20)
                        | (7u64 << 39)
                        | (1u64 << 49),
                );
                if rewritten_uniform {
                    self.emit(0x38e8000000070000 | 239 | (239u64 << 8) | (u64::from(mask) << 20));
                    restore(self, mask);
                }
                self.emit(self.base(0x5c98000000000000) | (15u64 << 39) | (250u64 << 20));
                return Ok(());
            }
            let condition = insn.bits(87, 3);
            let source = if rewritten_uniform && condition != 7 {
                6 - condition
            } else {
                condition
            };
            let mask = 1u32 << source;
            self.mov_imm(252, 1);
            if rewritten_uniform && condition != 7 {
                save(self, mask);
                self.emit(0x38f0000000070000 | (239u64 << 8) | (u64::from(mask) << 20));
            }
            self.emit(
                0x5ca0000000000000
                    | self.predicate
                    | 251
                    | (252u64 << 8)
                    | (255u64 << 20)
                    | (u64::from(source) << 39)
                    | (u64::from(insn.bits(90, 1)) << 42),
            );
            if rewritten_uniform && condition != 7 {
                restore(self, mask);
            }
            self.emit(self.base(0x5c10000000000000) | (250u64 << 8) | (251u64 << 20));
            return Ok(());
        }
        if opcode == 0xa10 && insn.bits(81, 3) != 7 {
            if insn.bits(64, 8) != 255
                || insn.bits(73, 4) != 0
                || insn.bits(84, 7) != 0x7f
                || insn.bits(77, 4) != 15
                || insn.bits(63, 1) != 0
                || insn.bits(72, 1) != 0
            {
                return Err("native constant add carry mode");
            }
            let b = self.operand(1, false);
            self.emit(
                self.base(0x5c10000000000000)
                    | (u64::from(insn.bits(24, 8)) << 8)
                    | (u64::from(b) << 20),
            );
            self.emit(
                0x5b60000000000000
                    | self.predicate
                    | 7
                    | (u64::from(insn.bits(81, 3)) << 3)
                    | (self.dest << 8)
                    | (u64::from(b) << 20)
                    | (7u64 << 39)
                    | (1u64 << 49),
            );
            return Ok(());
        }
        if opcode == 0xa10 && insn.bits(74, 1) != 0 {
            if insn.bits(24, 8) != 255 || insn.words[2] & !0x07800000 != 0x007fe4ff {
                return Err("native extended constant add mode");
            }
            let b = self.operand(1, false);
            self.mov_imm(250, 1);
            self.emit(
                0x5ca0000000000000
                    | self.predicate
                    | 250
                    | (250u64 << 8)
                    | (255u64 << 20)
                    | (u64::from(insn.bits(87, 3)) << 39)
                    | (u64::from(insn.bits(90, 1)) << 42),
            );
            self.emit(self.base(0x5c10000000000000) | (u64::from(b) << 8) | (250u64 << 20));
            return Ok(());
        }
        if insn.bits(64, 8) != 255
            && insn.bits(73, 4) & !4 == 0
            && insn.bits(77, 7) == 0x7f
            && insn.bits(84, 7) == 0x7f
        {
            let b = self.operand(1, false);
            self.emit(
                0x5c10000000000000
                    | self.predicate
                    | 250
                    | (u64::from(insn.bits(24, 8)) << 8)
                    | (u64::from(b) << 20)
                    | (u64::from(insn.bits(63, 1)) << 48)
                    | (u64::from(insn.bits(72, 1)) << 49),
            );
            self.emit(
                self.base(0x5c10000000000000)
                    | (250u64 << 8)
                    | (u64::from(insn.bits(64, 8)) << 20)
                    | (u64::from(insn.bits(75, 1)) << 48),
            );
            return Ok(());
        }
        if insn.bits(64, 8) != 255
            || insn.bits(73, 4) != 0
            || insn.bits(77, 7) != 0x7f
            || insn.bits(84, 7) != 0x7f
        {
            return Err("native integer carry/third source");
        }
        if opcode == 0xa10 || opcode == 0xc10 {
            if opcode == 0xc10 && insn.words[1] & !0x8000003f != 0 {
                return Err("native uniform add operand");
            }
            let b = self.operand(1, false);
            self.emit(
                self.base(0x5c10000000000000)
                    | (u64::from(insn.bits(24, 8)) << 8)
                    | (u64::from(b) << 20)
                    | (u64::from(insn.bits(63, 1)) << 48)
                    | (u64::from(insn.bits(72, 1)) << 49),
            );
        } else if opcode == 0x210 {
            if insn.words[1] & !0x800000ff != 0 {
                return Err("native integer add register modifiers");
            }
            self.emit(
                self.base(0x5c10000000000000)
                    | (u64::from(insn.bits(24, 8)) << 8)
                    | (u64::from(insn.bits(32, 8)) << 20)
                    | (u64::from(insn.bits(63, 1)) << 48)
                    | (u64::from(insn.bits(72, 1)) << 49),
            );
        } else {
            self.emit(
                self.base(0x1c00000000000000)
                    | (u64::from(insn.bits(24, 8)) << 8)
                    | (u64::from(insn.words[1]) << 20)
                    | (u64::from(insn.bits(72, 1)) << 56),
            );
        }
        Ok(())
    }

    fn lower_selection_and_logic(&mut self, opcode: u32) -> Result<(), &'static str> {
        let insn = self.insn;
        match opcode {
            0x207 | 0x807 | 0xa07 | 0xc07 | 0x208 | 0x808 | 0xa08 => {
                let floating = opcode & 0x1ff == 8;
                let a = self.operand(0, floating);
                let b = self.operand(1, floating);
                self.emit(
                    self.base(0x5ca0000000000000)
                        | (u64::from(a) << 8)
                        | (u64::from(b) << 20)
                        | (u64::from(insn.bits(87, 3)) << 39)
                        | (u64::from(insn.bits(90, 1)) << 42),
                );
            }
            0x20a | 0x80a | 0xa0a | 0xc0a => {
                if insn.bits(64, 8) != 0
                    || insn.bits(74, 2) == 3
                    || insn.bits(81, 6) != 0
                    || (opcode != 0xc0a && insn.bits(91, 1) != 0)
                {
                    return Err("native floating comparison output mode");
                }
                let a = self.operand(0, true);
                let b = self.operand(1, true);
                self.emit(
                    self.base(0x5800000000000000)
                        | (u64::from(a) << 8)
                        | (u64::from(b) << 20)
                        | (u64::from(insn.bits(87, 4)) << 39)
                        | (u64::from(insn.bits(74, 2)) << 45)
                        | (u64::from(insn.bits(76, 4)) << 48)
                        | (1u64 << 52)
                        | (u64::from(insn.bits(80, 1)) << 55),
                );
            }
            0x234 | 0x434 => {
                let source = if opcode == 0x234 {
                    if insn.words[1] & 0x0fffff00 != 0 || insn.bits(64, 5) != 0 {
                        return Err("native half comparison mode");
                    }
                    (u64::from(insn.bits(32, 8)) << 20) | (u64::from(insn.bits(60, 2)) << 28)
                        | (u64::from(insn.bits(62, 1)) << 30) | (u64::from(insn.bits(63, 1)) << 31)
                } else {
                    self.mov_imm(250, insn.words[1]);
                    250u64 << 20
                };
                self.emit(
                    0x5d20000000000000
                        | self.predicate
                        | u64::from(insn.bits(84, 3))
                        | (u64::from(insn.bits(81, 3)) << 3)
                        | (u64::from(insn.bits(80, 1)) << 6)
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | source
                        | (u64::from(insn.bits(76, 4)) << 35)
                        | (u64::from(insn.bits(87, 3)) << 39)
                        | (u64::from(insn.bits(90, 1)) << 42)
                        | (u64::from(insn.bits(72, 1)) << 43)
                        | (u64::from(insn.bits(73, 1)) << 44)
                        | (u64::from(insn.bits(69, 2)) << 45)
                        | (u64::from(insn.bits(74, 2)) << 47)
                        | (u64::from(insn.bits(71, 1)) << 49),
                );
            }
            0x20b | 0x80b | 0xa0b | 0xc0b | 0x20c | 0x80c | 0xa0c | 0xc0c => {
                let floating = opcode & 0x1ff == 0xb;
                if !floating && insn.bits(72, 1) != 0 {
                    // Fritchii ISETP.EX: combine the high-word comparison with
                    // the predicate produced by the low-word comparison.
                    let compare = insn.bits(76, 3);
                    let destination = insn.bits(81, 3);
                    let destination_b = insn.bits(84, 3);
                    let low = insn.bits(68, 4);
                    let accumulate = insn.bits(87, 4);
                    let combine = insn.bits(74, 2);
                    if compare == 0 || compare == 7 || combine == 3 {
                        return Err("native extended integer comparison mode");
                    }
                    let a = self.operand(0, false);
                    let b = self.operand(1, false);
                    let mut temporary = 0;
                    while temporary == destination || temporary == destination_b
                        || temporary == (low & 7) || temporary == (accumulate & 7)
                        || temporary == insn.bits(12, 3)
                    {
                        temporary += 1;
                    }
                    let mask = 1u64 << temporary;
                    self.emit(0x38e8000000070000 | 253 | (255u64 << 8) | (mask << 20));
                    let isetp = |op: u32, input: u32, combine: u32| {
                        0x5b60000000000000 | self.predicate | 7 | (u64::from(temporary) << 3)
                            | (u64::from(a) << 8) | (u64::from(b) << 20)
                            | (u64::from(input & 7) << 39) | (u64::from(input >> 3) << 42)
                            | (u64::from(combine) << 45) | (u64::from(insn.bits(73, 1)) << 48)
                            | (u64::from(op) << 49)
                    };
                    let first = isetp(if compare == 5 { 5 } else { 2 }, low, u32::from(compare == 5));
                    let second = isetp(if compare <= 3 { 1 } else { 4 }, temporary, 1);
                    self.emit(first);
                    if compare != 2 && compare != 5 {
                        self.emit(second);
                    }
                    self.emit(0x5090000000000000 | self.predicate | u64::from(destination_b)
                        | (u64::from(destination) << 3) | (u64::from(temporary) << 12)
                        | (7u64 << 29) | (u64::from(accumulate & 7) << 39)
                        | (u64::from(accumulate >> 3) << 42) | (u64::from(combine) << 45));
                    self.emit(0x38f0000000070000 | (253u64 << 8) | (mask << 20));
                    return Ok(());
                }
                let a = self.operand(0, floating);
                let b = self.operand(1, floating);
                let combine = insn.bits(74, 2);
                if combine > 2 {
                    return Err("native predicate combination");
                }
                let mut out = (if floating {
                    0x5bb0000000000000
                } else {
                    0x5b60000000000000
                }) | self.predicate
                    | u64::from(insn.bits(84, 3))
                    | (u64::from(insn.bits(81, 3)) << 3)
                    | (u64::from(a) << 8)
                    | (u64::from(b) << 20)
                    | (u64::from(insn.bits(87, 3)) << 39)
                    | (u64::from(insn.bits(90, 1)) << 42)
                    | (u64::from(combine) << 45);
                if floating {
                    out |=
                        (u64::from(insn.bits(80, 1)) << 47) | (u64::from(insn.bits(76, 4)) << 48);
                } else {
                    out |=
                        (u64::from(insn.bits(73, 1)) << 48) | (u64::from(insn.bits(76, 3)) << 49);
                }
                self.emit(out);
            }
            0x212 | 0x812 | 0xa12 | 0xc12 => {
                let lut = insn.bits(72, 8);
                let pred_target = insn.bits(81, 3);
                if insn.bits(80, 1) != 0
                    || insn.bits(87, 4) != 15
                    || (opcode == 0x212 && insn.bits(40, 24) != 0)
                    || (opcode == 0xa12 && insn.bits(62, 2) != 0)
                {
                    return Err("native LUT/predicate mode");
                }
                let b = if opcode == 0x212 {
                    insn.bits(32, 8)
                } else if opcode == 0x812 {
                    self.mov_imm(251, insn.words[1]);
                    251
                } else if opcode == 0xc12 {
                    let b = self.uniforms.register(insn.bits(32, 8));
                    if b == u32::MAX {
                        return Err("native uniform logic source");
                    }
                    b
                } else {
                    self.operand(1, false)
                };
                let a = insn.bits(24, 8);
                let c = insn.bits(64, 8);
                self.emit(
                    0x5be0000000000000
                        | self.predicate
                        | self.dest
                        | (u64::from(a) << 8)
                        | (u64::from(b) << 20)
                        | (u64::from(lut) << 28)
                        | ((if pred_target == 7 { 0u64 } else { 3 }) << 36)
                        | (u64::from(c) << 39)
                        | (u64::from(pred_target) << 48),
                );
            }
            0x213 | 0xa13 | 0xc13 => {
                if insn.words[2] & !(if opcode == 0xc13 { 0x08000000 } else { 0 }) != 0 {
                    return Err("native integer absolute mode");
                }
                let source = self.operand(1, false);
                self.emit(
                    0x5c10000000000000
                        | self.predicate
                        | 250
                        | (255u64 << 8)
                        | (u64::from(source) << 20)
                        | (1u64 << 48),
                );
                self.emit(
                    self.base(0x5c20000000000000)
                        | (u64::from(source) << 8)
                        | (250u64 << 20)
                        | (15u64 << 39)
                        | (1u64 << 48),
                );
            }
            0x217 | 0x817 | 0xa17 | 0xc17 => {
                if insn.words[2] & !(0x04000200 | if opcode == 0xc17 { 0x08000000 } else { 0 })
                    != 0x03800000
                    || (opcode != 0xa17 && opcode != 0x817 && insn.words[1] > 255)
                    || (opcode == 0xa17 && (!self.valid_cbuf() || insn.bits(62, 2) != 0))
                {
                    return Err("native integer min/max mode");
                }
                // Maxwell's immediate carries 19 value bits and a separate sign bit.
                let short_immediate = opcode == 0x817
                    && (insn.words[1] <= 0x7ffff || insn.words[1] >= 0xfff80000);
                if opcode == 0x817 && !short_immediate { self.mov_imm(251, insn.words[1]); }
                let source = if opcode == 0xc17 {
                    self.uniforms.register(insn.words[1])
                } else if opcode == 0xa17 {
                    self.operand(1, false)
                } else if opcode == 0x817 {
                    if short_immediate { insn.words[1] & 0x7ffff } else { 251 }
                } else {
                    insn.words[1]
                };
                if source == u32::MAX {
                    return Err("native uniform min/max operand");
                }
                self.emit(
                    self.base(if short_immediate {
                        0x3820000000000000
                    } else {
                        0x5c20000000000000
                    }) | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(source) << 20)
                        | (u64::from(if short_immediate { insn.words[1] >> 31 } else { 0 }) << 56)
                        | (u64::from(insn.bits(87, 4)) << 39)
                        | (u64::from(insn.bits(73, 1)) << 48),
                );
            }
            0x819 => {
                let is_signed = insn.bits(73, 2) == 2;
                if insn.bits(80, 1) == 0 && insn.bits(76, 1) != 0
                    && insn.bits(73, 2) == 1 && insn.bits(75, 1) == 0
                    && insn.words[1] <= 32
                {
                    // Fritchii: U64 right funnel shifts through 32 need no
                    // clamp bit (it overlaps Maxwell's immediate field).
                    self.emit(self.base(0x38f8000000000000)
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(insn.words[1]) << 20)
                        | (u64::from(insn.bits(64, 8)) << 39));
                    return Ok(());
                }
                if insn.bits(80, 1) == 0
                    && insn.bits(76, 1) == 0
                    && insn.bits(64, 8) == 255
                    && insn.bits(73, 2) == 3
                    && insn.bits(75, 1) == 0
                    && insn.words[1] <= 32
                {
                    self.emit(
                        self.base(0x3848000000000000)
                            | (u64::from(insn.bits(24, 8)) << 8)
                            | (u64::from(insn.words[1]) << 20),
                    );
                    return Ok(());
                }
                if insn.bits(80, 1) == 0
                    && insn.bits(76, 1) != 0
                    && insn.bits(64, 8) == 255
                    && (insn.bits(73, 2) == 3 || is_signed)
                    && insn.bits(75, 1) == 0
                    && insn.words[1] <= 32
                {
                    self.emit(
                        self.base(0x3828000000000000)
                            | (u64::from(insn.bits(24, 8)) << 8)
                            | (u64::from(insn.words[1]) << 20),
                    );
                    return Ok(());
                }
                let u64_left = insn.bits(73, 2) == 1 && insn.bits(76, 1) == 0;
                if (insn.bits(73, 2) != 3 && !is_signed && !u64_left)
                    || insn.bits(75, 1) != 0
                    || insn.bits(80, 1) == 0
                    || insn.words[1] > 32
                {
                    return Err("native funnel-shift mode");
                }
                let low = insn.bits(24, 8);
                let high = insn.bits(64, 8);
                let shift = insn.words[1];
                if insn.bits(76, 1) != 0 {
                    if low != 255 {
                        return Err("native nonzero low right-shift word");
                    }
                    self.emit(
                        self.base(0x3828000000000000)
                            | (u64::from(high) << 8)
                            | (u64::from(shift) << 20)
                            | (u64::from(is_signed) << 48),
                    );
                } else {
                    if is_signed {
                        return Err("native signed left funnel shift");
                    }
                    self.emit(
                        self.base(0x36f8000000000000)
                            | (u64::from(low) << 8)
                            | (u64::from(high) << 39)
                            | (u64::from(shift) << 20),
                    );
                }
            }
            _ => unreachable!("opcode outside upstream SEL through SHF immediate arms"),
        }
        Ok(())
    }

    fn lower_special_functions_and_conversion(
        &mut self,
        opcode: u32,
        _native_predicate: u32,
    ) -> Result<(), &'static str> {
        let insn = self.insn;
        match opcode {
            0x308 | 0x908 | 0xb08 | 0xd08 => {
                let operation = insn.bits(74, 6);
                if opcode == 0x908 && (insn.bits(24, 8) != 0 || (insn.words[2] & !0xfc00) != 0) {
                    return Err("native immediate MUFU mode");
                }
                // TANH (operation 9) still requires its IR/backend prerequisite.
                if operation > 8 || operation == 6 || operation == 7
                    || insn.bits(80, 12) != if opcode == 0xd08 { 0x800 } else { 0 } {
                    return Err("native MUFU mode");
                }
                // SM70+ COS/SIN take turns; Maxwell takes radians, so retain upstream's 2pi scale.
                let turns = operation == 0 || operation == 1;
                let to_radians = |this: &mut Self, source: u32| {
                    this.emit(
                        0x1e00000000000000
                            | this.predicate
                            | 250
                            | (u64::from(source) << 8)
                            | (0x40c90fdbu64 << 20),
                    );
                    250
                };
                if insn.bits(73, 1) != 0 {
                    if opcode != 0x308 || insn.bits(60, 2) > 1 {
                        return Err("native half special-function source");
                    }
                    self.emit(
                        0x5ca8000000000000
                            | self.predicate
                            | 250
                            | (2u64 << 8)
                            | (1u64 << 10)
                            | (u64::from(insn.bits(32, 8)) << 20)
                            | (u64::from(insn.bits(60, 1)) << 41)
                            | (u64::from(insn.bits(63, 1)) << 45)
                            | (u64::from(insn.bits(62, 1)) << 49),
                    );
                    if turns {
                        to_radians(self, 250);
                    }
                    self.emit(
                        0x5080000000000000
                            | self.predicate
                            | 250
                            | (250u64 << 8)
                            | (u64::from(operation) << 20),
                    );
                    self.emit(
                        self.base(0x5ca8000000000000) | (1u64 << 8) | (2u64 << 10) | (250u64 << 20),
                    );
                } else {
                    let mut src = if opcode == 0xd08 {
                        self.uniforms.register(insn.bits(32, 8))
                    } else { self.operand(1, true) };
                    if src == u32::MAX { return Err("native MUFU uniform source"); }
                    if turns {
                        src = to_radians(self, src);
                    }
                    self.emit(
                        self.base(0x5080000000000000)
                            | (u64::from(src) << 8)
                            | (u64::from(operation) << 20),
                    );
                }
            }
            0x526 => {
                let index_register = self.uniforms.register(insn.bits(24, 8));
                if index_register == u32::MAX
                    || insn.bits(32, 8) != 255
                    || (insn.bits(76, 4) != 0 && insn.bits(76, 4) != 4)
                    || insn.bits(81, 3) != 7
                {
                    return Err("native indexed IPA mode");
                }
                if index_register == 255 {
                    self.emit(
                        self.base(0xe000000000000000)
                            | (255u64 << 8)
                            | (255u64 << 20)
                            | (u64::from(insn.bits(64, 8)) << 30)
                            | (u64::from(insn.bits(78, 1)) << 55),
                    );
                    return Ok(());
                }
                // Maxwell IPA.IDX reads the entire byte address from the index register.
                self.emit(
                    0x1c00000000000000
                        | self.predicate
                        | 250
                        | (u64::from(index_register) << 8)
                        | (u64::from(insn.bits(64, 8) * 4) << 20),
                );
                self.emit(
                    self.base(0xe000000000000000)
                        | (250u64 << 8)
                        | (255u64 << 20)
                        | (u64::from(insn.bits(64, 8)) << 30)
                        | (1u64 << 38)
                        | (u64::from(insn.bits(78, 1)) << 55),
                );
            }
            0x326 => {
                if insn.bits(32, 8) != 255
                    || insn.bits(76, 2) > 1
                    || insn.bits(79, 1) != 0
                    || insn.bits(81, 3) != 7
                {
                    return Err("native IPA interpolation mode");
                }
                self.emit(
                    self.base(0xe000000000000000)
                        | (255u64 << 8)
                        | (255u64 << 20)
                        | (u64::from(insn.bits(64, 8)) << 30)
                        | (u64::from(insn.bits(76, 2)) << 52)
                        | (u64::from(insn.bits(78, 1)) << 55),
                );
            }
            0x81a => {
                if insn.words[2] & !0x200 != 0 || insn.words[1] > 32 || self.dest >= 255 {
                    return Err("native sign extend mode");
                }
                self.emit(
                    self.base(0x3800000000000000)
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(insn.words[1] << 8) << 20)
                        | (u64::from(insn.bits(73, 1)) << 48),
                );
            }
            0x299 => {
                let dst = self.uniforms.register(insn.bits(16, 8));
                let low = self.uniforms.register(insn.bits(24, 8));
                let shift = self.uniforms.register(insn.bits(32, 8));
                let high = self.uniforms.register(insn.bits(64, 8));
                let right = insn.bits(76, 1) != 0;
                let high_result = insn.bits(80, 1) != 0;
                if dst == u32::MAX
                    || dst == 255
                    || low == u32::MAX
                    || shift == u32::MAX
                    || high == u32::MAX
                    || insn.bits(73, 2) != 3
                    || insn.words[2] & !0x000118ff != 0x08000600
                {
                    return Err("native uniform register shift mode");
                }
                let source = if high_result && low == 255 {
                    high
                } else if !high_result && high == 255 {
                    low
                } else {
                    return Err("native uniform funnel shift");
                };
                self.emit(
                    (if right {
                        0x5c28000000000000
                    } else {
                        0x5c48000000000000
                    }) | self.predicate
                        | u64::from(dst)
                        | (u64::from(source) << 8)
                        | (u64::from(shift) << 20)
                        | (u64::from(insn.bits(75, 1)) << 39),
                );
            }
            0x499 => {
                let target = self.uniforms.register(self.dest as u32);
                let shift = self.uniforms.register(insn.bits(64, 8));
                if insn.bits(24, 8) != 63 || insn.words[2] & !0x8ff != 0x08010600
                    || insn.words[3] & 0xc00001ff != 0
                    || target == u32::MAX || target == 255 || shift == u32::MAX {
                    return Err("native uniform immediate high-word shift mode");
                }
                self.mov_imm(250, insn.words[1]);
                self.emit(0x5c48000000000000 | self.predicate | u64::from(target) | (250u64 << 8)
                    | (u64::from(shift) << 20) | (u64::from(insn.bits(75, 1)) << 39));
            }
            0x419 => {
                if insn.bits(24, 8) != 255 || insn.words[2] & !0x8ff != 0x00010600
                    || (insn.bits(75, 1) == 0 && insn.words[3] & 0xc00001ff != 0) {
                    return Err("native funnel shift immediate mode");
                }
                self.mov_imm(250, insn.words[1]);
                self.emit(
                    self.base(0x5c48000000000000)
                        | (250u64 << 8)
                        | (u64::from(insn.bits(64, 8)) << 20)
                        | (u64::from(insn.bits(75, 1)) << 39),
                );
            }
            0x805 => {
                if insn.bits(72, 8) != 255 || insn.bits(80, 1) == 0 || self.dest >= 249 {
                    return Err("native special-register read");
                }
                self.mov_imm(self.dest as u32, 0);
                self.mov_imm(self.dest as u32 + 1, 0);
            }
            0x306 | 0xd06 => {
                let source = if opcode == 0xd06 {
                    self.uniforms.register(insn.bits(32, 8))
                } else {
                    insn.bits(32, 8)
                };
                if opcode == 0x306 && matches!(insn.words[2], 0x00200c00 | 0x00209400) {
                    let half = insn.words[2] == 0x00200c00;
                    if insn.bits(24, 8) != 0 || insn.words[1] & !0xff != 0
                        || insn.words[3] & 0xc00001ff != 0 {
                        return Err("native scalar integer-to-float conversion mode");
                    }
                    self.emit(self.base(0x5cb8000000000000)
                        | ((if half { 1u64 } else { 2 }) << 8) | (2u64 << 10)
                        | (1u64 << 13) | (u64::from(source) << 20)
                        | ((if half { 0u64 } else { 2 }) << 39));
                    return Ok(());
                }
                if ((opcode == 0x306 && insn.words[2] & !0x400 == 0x00101000)
                    || (opcode == 0xd06 && insn.words[2] == 0x08101000
                        && insn.bits(61, 1) == 0 && insn.bits(24, 8) == 0
                        && insn.words[3] & 0xc00001ff == 0))
                    && insn.words[1] & 0xcfffff00 == 0 && source != u32::MAX
                {
                    self.emit(
                        self.base(0x5cb8000000000000)
                            | (2u64 << 8)
                            | (1u64 << 10)
                            | (u64::from(insn.bits(74, 1)) << 13)
                            | (u64::from(source) << 20)
                            | (u64::from(insn.bits(60, 2) * 2) << 41),
                    );
                    return Ok(());
                }
                if insn.words[2] & !0x400 != if opcode == 0xd06 { 0x08001000 } else { 0x1000 }
                    || insn.words[1] & if opcode == 0xd06 { 0xffffff00 } else { 0xcfffff00 } != 0
                    || source == u32::MAX
                {
                    return Err("native byte-to-float conversion mode");
                }
                self.emit(self.base(0x5cb8000000000000) | (2u64 << 8)
                    | (u64::from(insn.bits(74, 1)) << 13) | (u64::from(source) << 20)
                    | (u64::from(insn.bits(60, 2)) << 41));
            }
            0x245 | 0xa45 | 0xc45 => {
                if insn.words[2] & !(0x400 | 0xc000 | if opcode == 0xc45 { 0x08000000 } else { 0 })
                    != 0x00201000
                {
                    return Err("native packed integer-to-float format");
                }
                let src = self.operand(1, false);
                self.emit(
                    self.base(0x5cb8000000000000)
                        | (2u64 << 8)
                        | (2u64 << 10)
                        | (u64::from(insn.bits(74, 1)) << 13)
                        | (u64::from(src) << 20)
                        | (u64::from(insn.bits(78, 2)) << 39),
                );
            }
            _ => unreachable!("opcode outside upstream MUFU through I2FP arms"),
        }
        Ok(())
    }

    fn lower_float_arithmetic(&mut self, opcode: u32) -> Result<(), &'static str> {
        let insn = self.insn;
        match opcode {
            0x223 | 0x423 | 0x623 | 0x823 | 0xa23 | 0xc23 | 0xe23 => {
                if insn.bits(76, 1) != 0
                    || insn.bits(81, 11)
                        & !(if matches!(opcode, 0xe23 | 0xc23) {
                            0x400
                        } else {
                            0
                        })
                        != 0
                {
                    return Err("native FFMA denormal/source mode");
                }
                let a = self.operand(0, true);
                let b = self.operand(1, true);
                let c = self.operand(2, true);
                self.emit(
                    self.base(0x5980000000000000)
                        | (u64::from(a) << 8)
                        | (u64::from(b) << 20)
                        | (u64::from(c) << 39)
                        | (u64::from(insn.bits(77, 1)) << 50)
                        | (u64::from(insn.bits(78, 2)) << 51)
                        | (u64::from(insn.bits(80, 1)) << 53),
                );
            }
            0x220 | 0x820 | 0xa20 | 0xc20 => {
                let scale = insn.bits(84, 3);
                if insn.bits(76, 1) != 0
                    || scale == 0
                    || insn.bits(81, 3) != 0
                    || insn.bits(87, 4) != 0
                    || insn.bits(91, 1) != u32::from(opcode == 0xc20)
                    || (scale != 4 && (insn.bits(80, 1) == 0 || insn.bits(78, 2) != 0))
                {
                    return Err("native FMUL scale/mode");
                }
                let a = self.operand(0, true);
                let b = self.operand(1, true);
                self.emit(
                    self.base(0x5c68000000000000)
                        | (u64::from(a) << 8)
                        | (u64::from(b) << 20)
                        | (u64::from(insn.bits(78, 2)) << 39)
                        | (u64::from([0u32, 3, 2, 1, 0, 6, 5, 4][scale as usize]) << 41)
                        | (u64::from(insn.bits(80, 1)) << 44)
                        | (u64::from(insn.bits(77, 1)) << 50),
                );
            }
            0x221 | 0x421 | 0x621 | 0x821 | 0xa21 | 0xe21 => {
                if insn.bits(81, 10) != 0 || insn.bits(91, 1) != u32::from(opcode == 0xe21) {
                    return Err("native FADD mode");
                }
                let a = self.operand(0, true);
                let b = self.operand(
                    if matches!(insn.bits(9, 3), 2 | 3 | 7) {
                        2
                    } else {
                        1
                    },
                    true,
                );
                self.emit(
                    self.base(0x5c58000000000000)
                        | (u64::from(a) << 8)
                        | (u64::from(b) << 20)
                        | (u64::from(insn.bits(78, 2)) << 39)
                        | (u64::from(insn.bits(80, 1)) << 44)
                        | (u64::from(insn.bits(77, 1)) << 50),
                );
            }
            0x209 | 0x809 | 0xa09 | 0xc09 => {
                if insn.bits(74, 6) != 0
                    || insn.bits(81, 6) != 0
                    || (opcode != 0xc09 && insn.bits(91, 1) != 0)
                {
                    return Err("native floating min/max mode");
                }
                let a = self.operand(0, true);
                let b = self.operand(1, true);
                self.emit(
                    self.base(0x5c60000000000000)
                        | (u64::from(a) << 8)
                        | (u64::from(b) << 20)
                        | (u64::from(insn.bits(87, 3)) << 39)
                        | (u64::from(insn.bits(90, 1)) << 42)
                        | (u64::from(insn.bits(80, 1)) << 44),
                );
            }
            _ => unreachable!("opcode outside upstream FFMA through FMNMX arms"),
        }
        Ok(())
    }

    // Contiguous PRMT through MOV arms of upstream LowerToMaxwell.
    fn lower_permutation_and_moves(&mut self, opcode: u32) -> Result<(), &'static str> {
        let insn = self.insn;
        match opcode {
            0x816 => {
                if insn.words[1] > 0xffff || insn.words[2] & !0xff != 0 {
                    return Err("native permutation mode");
                }
                if !matches!(insn.words[1], 0x5410 | 0x5432 | 0x7610 | 0x7632) {
                    let a = insn.bits(24, 8);
                    let c = insn.bits(64, 8);
                    if a >= 250 && a != 255 {
                        return Err("native permutation source");
                    }
                    self.mov_imm(250, 0);
                    for i in 0..4 {
                        let select = (insn.words[1] >> (i * 4)) & 0xf;
                        let source = if select & 4 != 0 { c } else { a };
                        let position = (select & 3) * 8;
                        let extract = if select & 8 != 0 {
                            (position + 7) | (1 << 8)
                        } else {
                            position | (8 << 8)
                        };
                        self.emit(
                            0x3800000000000000
                                | self.predicate
                                | 251
                                | (u64::from(source) << 8)
                                | (u64::from(extract) << 20)
                                | (u64::from((select >> 3) & 1) << 48),
                        );
                        self.emit(
                            0x36f0000000000000
                                | self.predicate
                                | 250
                                | (251u64 << 8)
                                | (u64::from((i * 8) | (8u32 << 8)) << 20)
                                | (250u64 << 39),
                        );
                    }
                    self.emit(self.base(0x5c10000000000000) | (250u64 << 8) | (255u64 << 20));
                    return Ok(());
                }
                let a = insn.bits(24, 8);
                let b = insn.bits(64, 8);
                if insn.words[1] != 0x5432 && insn.words[1] != 0x7632 {
                    self.emit(
                        0x0400000000000000
                            | self.predicate
                            | 250
                            | (u64::from(a) << 8)
                            | (0xffffu64 << 20),
                    );
                } else {
                    self.emit(
                        0x3828000000000000
                            | self.predicate
                            | 250
                            | (u64::from(a) << 8)
                            | (16u64 << 20),
                    );
                }
                if insn.words[1] == 0x7610 || insn.words[1] == 0x7632 {
                    self.emit(
                        0x0400000000000000
                            | self.predicate
                            | 251
                            | (u64::from(b) << 8)
                            | (0xffff0000u64 << 20),
                    );
                    self.emit(self.base(0x5c10000000000000) | (250u64 << 8) | (251u64 << 20));
                } else {
                    self.emit(
                        self.base(0x5c18000000000000)
                            | (u64::from(b) << 8)
                            | (250u64 << 20)
                            | (16u64 << 39),
                    );
                }
            }
            0x307 | 0xb07 => {
                if opcode == 0x307 && matches!(insn.words[2], 0x00100800 | 0x0010c800) {
                    if insn.bits(24, 8) != 0 || insn.words[1] & !0x100000ff != 0
                        || insn.words[3] & 0xc00001ff != 0 {
                        return Err("native half round mode");
                    }
                    self.emit(self.base(0x5ca8000000000000) | (1u64 << 8) | (1u64 << 10)
                        | (u64::from(insn.bits(32, 8)) << 20)
                        | (u64::from(8 + insn.bits(78, 2)) << 39)
                        | (u64::from(insn.bits(60, 1)) << 41));
                    return Ok(());
                }
                if insn.bits(75, 2) != 2
                    || insn.bits(84, 2) != 2
                    || insn.bits(81, 3) != 0
                    || insn.bits(86, 6) != 0
                {
                    return Err("native floating round format");
                }
                if opcode == 0xb07 && !self.valid_cbuf() {
                    return Err("native constant buffer address");
                }
                self.emit(
                    self.base(if opcode == 0xb07 { 0x4ca8000000000000 } else { 0x5ca8000000000000 })
                        | (2u64 << 8)
                        | (2u64 << 10)
                        | (if opcode == 0xb07 { self.cbuf() } else { u64::from(insn.bits(32, 8)) << 20 })
                        | (u64::from(8 + insn.bits(78, 2)) << 39)
                        | (u64::from(insn.bits(80, 1)) << 44)
                        | (u64::from(insn.bits(63, 1)) << 45)
                        | (u64::from(insn.bits(62, 1)) << 49),
                );
            }
            0x321 | 0x322 => {
                if insn.bits(76, 2) != 0 || insn.bits(79, 1) != 0 {
                    return Err("native indexed/patch attribute access");
                }
                let address = insn.bits(40, 10);
                let components = insn.bits(74, 2) + 1;
                if address % 4 != 0 || address + components * 4 > 0x400 || insn.bits(24, 8) != 255 {
                    return Err("native attribute address or indexing");
                }
                // Upstream drops Ampere's per-primitive hint, which has no Maxwell meaning.
                if opcode == 0x322 && address == 0x5c && components == 1 {
                    return Ok(());
                }
                let mut value_reg = if opcode == 0x321 {
                    self.dest as u32
                } else {
                    insn.bits(32, 8)
                };
                if value_reg == 255 {
                    if opcode == 0x321 {
                        return Ok(());
                    }
                    for i in 0..components {
                        self.mov_imm(250 + i, 0);
                    }
                    value_reg = 250;
                }
                if value_reg + components > 255 {
                    return Err("native attribute register range");
                }
                let vertex = if opcode == 0x321 {
                    insn.bits(32, 8)
                } else {
                    insn.bits(64, 8)
                };
                self.emit(
                    (if opcode == 0x321 {
                        0xefd8000000000000
                    } else {
                        0xeff0000000000000
                    }) | self.predicate
                        | u64::from(value_reg)
                        | (255u64 << 8)
                        | (u64::from(address) << 20)
                        | (u64::from(vertex) << 39)
                        | (u64::from(components - 1) << 47),
                );
            }
            0x387 | 0x983 => {
                let store = opcode == 0x387;
                let data = insn.bits(32, 8);
                let offset = insn.bits(40, 24);
                if insn.bits(24, 8) != 255
                    || (store && (insn.bits(16, 8) != 0 || data > 247))
                    || (!store && (data != 0 || self.dest >= 248))
                    || offset & 3 != 0
                    || offset > 0x7fffff
                    || insn.words[2] != if store { 0x00100800 } else { 0x00300800 }
                {
                    return Err("native local scalar spill mode");
                }
                self.emit(
                    (if store {
                        0xef50000000000000
                    } else {
                        0xef40000000000000
                    }) | self.predicate
                        | (if store { u64::from(data) } else { self.dest })
                        | (255u64 << 8)
                        | (u64::from(offset) << 20)
                        | (4u64 << 48),
                );
            }
            0x803 => {
                if insn.words[1] > 0x7f || insn.words[2] & !0x3000 != 0 {
                    return Err("native predicate-to-register mode");
                }
                self.emit(
                    self.base(0x38e8000000000000)
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(insn.words[1]) << 20)
                        | (u64::from(insn.bits(76, 2)) << 41),
                );
            }
            0x804 => {
                if insn.bits(16, 8) != 0 || insn.words[1] > 0x7f || insn.words[2] & !0x3000 != 0 {
                    return Err("native register-to-predicate mode");
                }
                self.emit(
                    0x38f0000000000000
                        | self.predicate
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(insn.words[1]) << 20)
                        | (u64::from(insn.bits(76, 2)) << 41),
                );
            }
            0xc02 | 0x202 | 0x802 | 0xa02 => {
                if insn.bits(72, 4) != 15
                    || insn.bits(76, 16) & !(if opcode == 0xc02 { 0x8000 } else { 0 }) != 0
                {
                    return Err("native MOV modifiers");
                }
                if opcode == 0x802 {
                    self.emit(
                        self.base(0x0100000000000000)
                            | (15u64 << 12)
                            | (u64::from(insn.words[1]) << 20),
                    );
                } else if opcode == 0xc02 {
                    let source = if insn.words[1] < 64 { self.uniforms.register(insn.words[1]) } else { u32::MAX };
                    if source == u32::MAX || insn.words[2] != 0x08000f00 {
                        return Err("native uniform move source");
                    }
                    self.emit(
                        self.base(0x5c98000000000000)
                            | (15u64 << 39)
                            | (u64::from(source) << 20),
                    );
                } else if opcode == 0x202 {
                    if insn.bits(40, 24) != 0 {
                        return Err("native MOV register modifiers");
                    }
                    self.emit(
                        self.base(0x5c98000000000000)
                            | (15u64 << 39)
                            | (u64::from(insn.bits(32, 8)) << 20),
                    );
                } else {
                    if !self.valid_cbuf() || insn.bits(62, 2) != 0 {
                        return Err("native MOV constant buffer");
                    }
                    self.emit(self.base(0x4c98000000000000) | (15u64 << 39) | self.cbuf());
                }
            }
            _ => unreachable!("opcode outside upstream PRMT through MOV arms"),
        }
        Ok(())
    }

    fn lower_texture_sample(
        &mut self,
        opcode: u32,
        required_texture_buffer: &mut u32,
        code: &[Instruction],
        index: u32,
        uses_uniform_bank: bool,
    ) -> Result<(), &'static str> {
        let insn = self.insn;
        match opcode {
            0xb69 => {
                let mask = insn.bits(72, 4);
                let second_dest = insn.bits(64, 8);
                let source_a = insn.bits(24, 8);
                let source_b = insn.bits(32, 8);
                let binding = insn.bits(54, 5);
                if insn.bits(61, 3) != 1
                    || insn.bits(76, 4) != 0
                    || insn.bits(80, 10) != 0
                    || insn.bits(59, 1) != 0
                    || insn.bits(40, 14) > 0x1fff
                    || binding >= 18
                    || mask == 0
                    || source_a >= 246
                    || (*required_texture_buffer != u32::MAX && *required_texture_buffer != binding)
                {
                    return Err("native texture LOD query mode");
                }
                *required_texture_buffer = binding;
                self.emit(
                    0x5c98000000000000
                        | self.predicate
                        | 250
                        | (15u64 << 39)
                        | (u64::from(source_a) << 20),
                );
                self.emit(
                    0x5c98000000000000
                        | self.predicate
                        | 251
                        | (15u64 << 39)
                        | (u64::from(if source_b == 255 {
                            source_a + 1
                        } else {
                            source_b
                        }) << 20),
                );
                self.emit(
                    0xdf58000000000000
                        | self.predicate
                        | 250
                        | (250u64 << 8)
                        | (255u64 << 20)
                        | (2u64 << 28)
                        | (u64::from(mask) << 31)
                        | (u64::from(insn.bits(40, 14)) << 36)
                        | (u64::from(insn.bits(90, 1)) << 49),
                );
                let components = mask.count_ones();
                let split = second_dest != 255;
                for i in 0..components {
                    let target = if !split || i < 2 {
                        if self.dest == 255 {
                            255
                        } else {
                            self.dest as u32 + i
                        }
                    } else {
                        second_dest + (i - 2)
                    };
                    if target != 255 {
                        self.emit(
                            0x5c98000000000000
                                | self.predicate
                                | u64::from(target)
                                | (15u64 << 39)
                                | (u64::from(250 + i) << 20),
                        );
                    }
                }
            }
            0xb63 | 0xf63 => {
                let bindless = opcode == 0xf63;
                let handle = if bindless { self.uniforms.register(insn.bits(40, 6)) } else { 255 };
                let mask = insn.bits(72, 4);
                let second_dest = insn.bits(64, 8);
                let source_a = insn.bits(24, 8);
                let source_b = insn.bits(32, 8);
                let binding = insn.bits(54, 5);
                let offsets = insn.bits(76, 1) != 0;
                let per_tap = insn.bits(76, 2) == 2;
                let half = insn.bits(79, 1) != 0;
                let depth_compare = insn.bits(78, 1) != 0;
                let register_limit = if uses_uniform_bank { self.uniforms.base() } else { 246 };
                let array = insn.bits(61, 3) == 5;
                let dest = self.dest as u32;
                if bindless && array {
                    let count = mask.count_ones();
                    let first_count = if second_dest == 255 { count } else { count.min(2) };
                    let second_count = count - first_count;
                    if insn.bits(60, 1) == 0 || (insn.words[2] & !0x05800fff) != 0x081e0000
                        || source_a + 2 > register_limit || source_b >= register_limit
                        || (dest != 255 && dest + first_count > register_limit)
                        || (second_dest != 255 && second_dest + second_count > register_limit)
                        || (dest != 255 && second_dest != 255 && second_count != 0
                            && second_dest < dest + first_count && dest < second_dest + second_count)
                        || insn.words[3] & 0xc00001ff != 0
                    { return Err("native uniform array texture gather mode"); }
                }
                if depth_compare {
                    // 2D depth gather: XY in Ra/Ra+1, comparison value in Rb.
                    // Maxwell places the comparison value in the metadata group.
                    let count = mask.count_ones();
                    let first = if half { 1 } else if second_dest == 255 { count } else { count.min(2) };
                    let second = if half { 1 } else { count - first };
                    if bindless || array || offsets || per_tap || insn.bits(60, 1) == 0
                        || (source_a != 255 && source_a + 1 >= register_limit)
                        || (source_b != 255 && source_b >= register_limit)
                        || (dest != 255 && dest + first > register_limit)
                        || (second_dest != 255 && second_dest + second > register_limit)
                        || (dest != 255 && second_dest != 255 && second != 0
                            && second_dest < dest + first && dest < second_dest + second)
                    { return Err("native depth gather operands"); }
                }
                if (insn.bits(61, 3) != 1 && !array) || (insn.bits(77, 1) != 0 && !per_tap)
                    || insn.bits(81, 3) != 7 || insn.bits(59, 1) != 0
                    || (array && !bindless && (insn.bits(60, 1) == 0
                        || (insn.words[2] & !0x058000ff) != 0x001e0f00
                        || source_a + 2 > register_limit || source_b >= register_limit
                        || dest + 2 > register_limit || second_dest + 2 > register_limit
                        || (second_dest < dest + 2 && dest < second_dest + 2)
                        || insn.words[3] & 0xc00001ff != 0))
                    || (half && (bindless || offsets || insn.bits(60, 1) == 0
                        || insn.bits(80, 1) != 0 || insn.bits(91, 1) != 0
                        || (source_b != 255 && source_b >= register_limit)
                        || mask != 15 || dest >= register_limit || second_dest >= register_limit))
                    || (offsets && (bindless || source_b == 255 || (source_a != 255 && source_a >= 245)))
                    || (per_tap && (bindless || insn.bits(78, 1) != 0 || insn.bits(80, 1) != 0 || insn.bits(91, 1) != 0))
                    || insn.bits(89, 1) != 0 || mask == 0
                    || if bindless { handle >= 246 || insn.bits(46, 13) != 0 || insn.bits(91, 1) == 0 }
                        else { insn.bits(40, 14) > 0x1fff || binding >= 18
                            || (*required_texture_buffer != u32::MAX && *required_texture_buffer != binding) }
                { return Err("native texture gather mode"); }
                if per_tap {
                    if insn.bits(60, 1) == 0 || mask != 15 || source_a + 1 >= register_limit
                        || source_b + 1 >= register_limit || (dest != 255 && dest + 1 >= register_limit)
                        || (second_dest != 255 && second_dest + 1 >= register_limit)
                    { return Err("native per-tap gather operands"); }
                    // Reserved scratch constants must dominate the predicated
                    // gather. Otherwise SSA merges them with undefined values
                    // and SPIR-V cannot represent the per-tap offsets.
                    let gather_predicate = self.predicate;
                    self.predicate = 7 << 16;
                    if let Some(offsets) = BoundedControl::gather_constant_offsets(code, index as usize, source_b) {
                        for (i, packed) in offsets.into_iter().enumerate() { self.mov_imm(248 + i as u32, packed); }
                    } else {
                    // Only straight-line programs make the backward constant-definition proof sound.
                    let mut terminal = false;
                    for instruction in code {
                        if instruction.bits(12, 4) == 15 { continue; }
                        let op = instruction.bits(0, 12);
                        if op == 0x94d && instruction.bits(12, 4) == 7 { terminal = true; break; }
                        // Upstream's incidental uniform-bank update for 0x83b is unobservable:
                        // this proof always rejects that opcode before any subsequent emission.
                        if !matches!(op, 0x326 | 0x802 | 0x307 | 0x421 | 0xa20 | 0xb63 | 0x221
                            | 0x823 | 0x423 | 0x223 | 0x308 | 0x220 | 0x820)
                        { return Err("native per-tap gather control-flow proof"); }
                    }
                    if !terminal { return Err("native per-tap gather terminal proof"); }
                    let mut packed_offsets: [Option<u32>; 2] = [None; 2];
                    let mut cursor = index as usize;
                    while cursor > 0 && packed_offsets.iter().any(Option::is_none) {
                        cursor -= 1;
                        let definition = code[cursor];
                        if definition.bits(12, 4) == 15 { continue; }
                        let op = definition.bits(0, 12);
                        let target = definition.bits(16, 8);
                        if op == 0xb63 {
                            let count = definition.bits(72, 4).count_ones();
                            let high = definition.bits(64, 8);
                            for i in 0..2 {
                                if packed_offsets[i].is_some() { continue; }
                                let reg = source_b + i as u32;
                                if (target != 255 && reg >= target
                                    && reg < target + if high == 255 { count } else { count.min(2) })
                                    || (high != 255 && count > 2 && reg >= high && reg < high + count - 2)
                                { return Err("native per-tap gather offset clobber"); }
                            }
                            continue;
                        }
                        if !matches!(op, 0x802 | 0x307 | 0x421 | 0xa20) {
                            return Err("native per-tap gather offset proof");
                        }
                        for i in 0..2 {
                            if packed_offsets[i].is_some() || target != source_b + i as u32 { continue; }
                            if op != 0x802 || definition.bits(12, 4) != 7 || definition.bits(24, 8) != 0
                                || definition.words[2] != 0x00000f00
                            { return Err("native per-tap gather nonconstant offsets"); }
                            packed_offsets[i] = Some(definition.words[1]);
                        }
                    }
                    for packed in packed_offsets {
                        let packed = packed.ok_or("native per-tap gather missing offsets")?;
                        for shift in (0..32).step_by(8) {
                            let value = (packed >> shift) & 255;
                            if value > 31 && value < 224 { return Err("native per-tap gather offset range"); }
                        }
                    }
                    for (i, packed) in packed_offsets.into_iter().enumerate() {
                        self.mov_imm(248 + i as u32, packed.unwrap());
                    }
                    }
                    self.predicate = gather_predicate;
                }
                if !bindless { *required_texture_buffer = binding; }
                let y = if !offsets && !per_tap && !depth_compare { source_b } else if source_a == 255 { 255 } else { source_a + 1 };
                self.emit(0x5c98000000000000 | self.predicate | 250 | (15u64 << 39) | (u64::from(source_a) << 20));
                self.emit(0x5c98000000000000 | self.predicate | 251 | (15u64 << 39)
                    | (u64::from(if array { source_a + 1 } else { y }) << 20));
                if array { self.emit(0x5c98000000000000 | self.predicate | 252 | (15u64 << 39) | (u64::from(source_b) << 20)); }
                if offsets || depth_compare { self.emit(0x5c98000000000000 | self.predicate | 246 | (15u64 << 39) | (u64::from(source_b) << 20)); }
                if bindless {
                    self.emit(0x5c98000000000000 | self.predicate | 246 | (15u64 << 39) | (u64::from(handle) << 20));
                    self.emit(0xdec0000000000000 | self.predicate | 250 | (250u64 << 8) | (246u64 << 20)
                        | ((if array { 3u64 } else { 2 }) << 28) | (u64::from(mask) << 31)
                        | (u64::from(insn.bits(87, 2)) << 38) | (u64::from(insn.bits(90, 1)) << 49) | (7u64 << 51));
                } else {
                    self.emit(0xc800000000000000 | self.predicate | 250 | (250u64 << 8)
                        | ((if per_tap { 248u64 } else if offsets || depth_compare { 246 } else { 255 }) << 20)
                        | ((if array { 3u64 } else { 2 }) << 28) | (u64::from(mask) << 31)
                        | (u64::from(insn.bits(40, 14)) << 36) | (u64::from(insn.bits(90, 1)) << 49)
                        | (u64::from(depth_compare) << 50)
                        | (7u64 << 51) | ((if per_tap { 2u64 } else if offsets { 1 } else { 0 }) << 54)
                        | (u64::from(insn.bits(87, 2)) << 56));
                }
                if half {
                    for i in 0u32..2 {
                        self.emit(0x5ca8000000000000 | self.predicate | 246 | (1u64 << 8) | (2u64 << 10)
                            | (u64::from(250 + i * 2) << 20));
                        self.emit(0x5ca8000000000000 | self.predicate | 247 | (1u64 << 8) | (2u64 << 10)
                            | (u64::from(251 + i * 2) << 20));
                        self.emit(0x5c18000000000000 | self.predicate | u64::from(if i == 0 { dest } else { second_dest })
                            | (247u64 << 8) | (246u64 << 20) | (16u64 << 39));
                    }
                    return Ok(());
                }
                let components = mask.count_ones();
                let split = second_dest != 255;
                for i in 0..components {
                    let target = if !split || i < 2 { if dest == 255 { 255 } else { dest + i } }
                        else { second_dest + i - 2 };
                    if target != 255 { self.emit(0x5c98000000000000 | self.predicate | u64::from(target)
                        | (15u64 << 39) | (u64::from(250 + i) << 20)); }
                }
            }
            0xb60 | 0xf60 | 0x361 => {
                // Fritchii TEX: uniform or register handles become Maxwell TEX.B metadata.
                let bindless = opcode != 0xb60;
                let handle = if opcode == 0xf60 { self.uniforms.register(insn.bits(40, 6)) } else { 255 };
                if bindless && ((opcode == 0xf60 && (handle >= 246 || insn.bits(46, 13) != 0))
                    || (opcode == 0x361 && (insn.bits(40, 19) != 0 || insn.bits(87, 3) > 3))) {
                    return Err("native bindless texture mode");
                }
                let mask = insn.bits(72, 4);
                let second_dest = insn.bits(64, 8);
                const RG_MASKS: [u32; 8] = [1, 2, 4, 8, 3, 9, 10, 12];
                const RGBA_MASKS: [u32; 5] = [7, 11, 13, 14, 15];
                let mut swizzle = u32::MAX;
                let split = second_dest != 255;
                let depth_compare = insn.bits(78, 1) != 0;
                let dimension = insn.bits(61, 3);
                let lod = insn.bits(87, 3);
                let source_a = insn.bits(24, 8);
                let source_b = insn.bits(32, 8);
                if !bindless && dimension == 5 && depth_compare && lod == 0 && !split && mask == 1
                    && (*required_texture_buffer == u32::MAX || *required_texture_buffer == insn.bits(54, 5)) {
                    if insn.bits(60, 1) == 0
                        || insn.bits(76, 2) != 0
                        || insn.bits(81, 3) != 7
                        || insn.bits(59, 1) != 0
                        || insn.bits(40, 14) > 0x1fff
                        || insn.bits(54, 5) >= 18
                        || source_a & 1 != 0
                        || source_b & 1 != 0
                        || source_a >= 248
                        || source_b >= 248
                        || self.dest >= 248
                    {
                        return Err("native array depth comparison sample mode");
                    }
                    *required_texture_buffer = insn.bits(54, 5);
                    for i in 0..3u32 {
                        let source = if i < 2 { source_a + i } else { source_b };
                        self.emit(
                            0x5c98000000000000
                                | self.predicate
                                | u64::from(250 + i)
                                | (15u64 << 39)
                                | (u64::from(source) << 20),
                        );
                    }
                    self.emit(
                        0xc000000000000000
                            | self.predicate
                            | self.dest
                            | (250u64 << 8)
                            | (u64::from(source_b + 1) << 20)
                            | (3u64 << 28)
                            | (1u64 << 31)
                            | (u64::from(insn.bits(40, 14)) << 36)
                            | (u64::from(insn.bits(90, 1)) << 49)
                            | (1u64 << 50)
                            | (7u64 << 51),
                    );
                    return Ok(());
                }
                let general_tex = |this: &mut Self, required_texture_buffer: &mut u32| -> bool {
                    const COORDINATE_COUNT: [u32; 8] = [1, 2, 3, 3, 2, 3, 0, 4];
                    const MAXWELL_TYPE: [u32; 8] = [0, 2, 4, 6, 1, 3, 0, 7];
                    let offsets = insn.bits(76, 1) != 0;
                    if dimension == 6
                        || insn.bits(77, 1) != 0
                        || insn.bits(81, 3) != 7
                        || insn.bits(59, 1) != u32::from(opcode == 0x361)
                        || (!bindless && (insn.bits(40, 14) > 0x1fff || insn.bits(54, 5) >= 18))
                        || (lod > 3 && lod != 6 && lod != 7)
                        || (depth_compare && mask != 1)
                        || mask == 0
                    {
                        return false;
                    }
                    let coordinates = COORDINATE_COUNT[dimension as usize];
                    let has_lod = matches!(lod, 2 | 3 | 6 | 7);
                    let handle_slot = u32::from(opcode == 0x361);
                    let count = coordinates + handle_slot
                        + u32::from(has_lod)
                        + u32::from(offsets)
                        + u32::from(depth_compare);
                    let in_a = if opcode == 0x361 && insn.bits(60, 1) == 0 { coordinates }
                        else if count <= 4 { (count + 1) / 2 }
                        else if opcode == 0x361 { coordinates } else { 4 };
                    let value = |i: u32| -> u32 {
                        if i < in_a {
                            if source_a == 255 {
                                255
                            } else {
                                source_a + i
                            }
                        } else if source_b == 255 {
                            255
                        } else {
                            source_b + (i - in_a)
                        }
                    };
                    if (source_a != 255 && source_a + in_a > 246)
                        || (count > in_a && source_b != 255 && source_b + (count - in_a) > 246)
                    {
                        return false;
                    }
                    let texture_handle = if opcode == 0x361 { value(coordinates) } else { handle };
                    if bindless && texture_handle >= 246 { return false; }
                    let binding = insn.bits(54, 5);
                    let second_buffer = !bindless && *required_texture_buffer != u32::MAX
                        && *required_texture_buffer != binding;
                    if !bindless && !second_buffer { *required_texture_buffer = binding; }
                    let tex_b = bindless || second_buffer;
                    let copy = |this: &mut Self, target: u32, source: u32| {
                        this.emit(
                            0x5c98000000000000
                                | this.predicate
                                | u64::from(target)
                                | (15u64 << 39)
                                | (u64::from(source) << 20),
                        );
                    };
                    for i in 0..coordinates {
                        copy(this, 250 + i, value(i));
                    }
                    let mut meta = 246;
                    let mut next = coordinates + handle_slot;
                    if bindless {
                        copy(this, meta, texture_handle);
                        meta += 1;
                    } else if second_buffer {
                        this.emit(0x4c98000000070000 | u64::from(meta) | (15u64 << 39)
                            | (u64::from(insn.bits(40, 14)) << 20) | (u64::from(binding) << 34));
                        meta += 1;
                    }
                    if has_lod {
                        copy(this, meta, value(next));
                        meta += 1;
                        next += 1;
                    }
                    if offsets {
                        copy(this, meta, value(next));
                        meta += 1;
                        next += 1;
                    }
                    if depth_compare {
                        copy(this, meta, value(next));
                        meta += 1;
                    }
                    if tex_b {
                        this.emit(0xde80000000000000 | this.predicate | 250 | (250u64 << 8)
                            | (246u64 << 20) | (u64::from(MAXWELL_TYPE[dimension as usize]) << 28)
                            | (u64::from(mask) << 31) | (u64::from(offsets) << 36)
                            | (u64::from(lod) << 37) | (u64::from(insn.bits(90, 1)) << 49)
                            | (u64::from(depth_compare) << 50) | (7u64 << 51));
                    } else { this.emit(
                        0xc000000000000000
                            | this.predicate
                            | 250
                            | (250u64 << 8)
                            | ((if meta == 246 { 255u64 } else { 246 }) << 20)
                            | (u64::from(MAXWELL_TYPE[dimension as usize]) << 28)
                            | (u64::from(mask) << 31)
                            | (u64::from(insn.bits(40, 14)) << 36)
                            | (u64::from(insn.bits(90, 1)) << 49)
                            | (u64::from(depth_compare) << 50)
                            | (7u64 << 51)
                            | (u64::from(offsets) << 54)
                            | (u64::from(lod) << 55),
                    ); }
                    let components = mask.count_ones();
                    if insn.bits(79, 1) != 0 {
                        let pack = |this: &mut Self, target: u32, low: u32, high: u32| {
                            if target == 255 {
                                return;
                            }
                            this.emit(
                                0x5ca8000000000000
                                    | this.predicate
                                    | 246
                                    | (1u64 << 8)
                                    | (2u64 << 10)
                                    | (u64::from(low) << 20),
                            );
                            this.emit(
                                0x5ca8000000000000
                                    | this.predicate
                                    | 247
                                    | (1u64 << 8)
                                    | (2u64 << 10)
                                    | (u64::from(high) << 20),
                            );
                            this.emit(
                                0x5c18000000000000
                                    | this.predicate
                                    | u64::from(target)
                                    // ISCADD shifts A: retain c0 in the low half,
                                    // and shift c1 into the high half (Fritchii TEX.F16).
                                    | (247u64 << 8)
                                    | (246u64 << 20)
                                    | (16u64 << 39),
                            );
                        };
                        let first_target = this.dest as u32;
                        pack(
                            this,
                            first_target,
                            250,
                            if components > 1 { 251 } else { 255 },
                        );
                        if components > 2 {
                            let target = if split {
                                second_dest
                            } else if this.dest == 255 {
                                255
                            } else {
                                this.dest as u32 + 1
                            };
                            pack(this, target, 252, if components > 3 { 253 } else { 255 });
                        }
                        return true;
                    }
                    for i in 0..components {
                        let target = if !split || i < 2 {
                            if this.dest == 255 {
                                255
                            } else {
                                this.dest as u32 + i
                            }
                        } else {
                            second_dest + (i - 2)
                        };
                        if target != 255 {
                            copy(this, target, 250 + i);
                        }
                    }
                    true
                };
                if split {
                    for (i, &value) in RGBA_MASKS.iter().enumerate() {
                        if value == mask {
                            swizzle = i as u32;
                        }
                    }
                } else {
                    for (i, &value) in RG_MASKS.iter().enumerate() {
                        if value == mask {
                            swizzle = i as u32;
                        }
                    }
                }
                if bindless || insn.bits(60, 1) == 0
                    || (dimension != 1 && dimension != 5)
                    || swizzle == u32::MAX
                    || insn.bits(76, 2) != 0
                    || insn.bits(81, 3) != 7
                    || (lod > 1 && (lod != 3 || dimension != 1 || depth_compare))
                    || (lod == 3 && (source_a & 1 != 0 || source_a >= 249))
                    || insn.bits(59, 1) != 0
                    || insn.bits(40, 14) > 0x1fff
                    || insn.bits(54, 5) != 2
                    || self.dest == 255
                    || (mask & (mask - 1) != 0 && (self.dest & 1 != 0 || self.dest >= 254))
                    || (split && mask == 15 && (second_dest & 1 != 0 || second_dest >= 254))
                    || (depth_compare
                        && (dimension != 1
                            || split
                            || mask != 1
                            || source_a & 1 != 0
                            || source_a >= 249))
                    || (dimension == 5 && (source_a & 1 != 0 || source_a >= 249))
                    || (*required_texture_buffer != u32::MAX && *required_texture_buffer != 2)
                {
                    if !general_tex(self, required_texture_buffer) {
                        return Err("native texture mode or descriptor binding");
                    }
                    return Ok(());
                }
                *required_texture_buffer = 2;
                let texture_type = if dimension == 5 {
                    if lod != 0 {
                        8u64
                    } else {
                        7
                    }
                } else if depth_compare {
                    if lod != 0 {
                        6
                    } else {
                        4
                    }
                } else if lod == 3 {
                    3
                } else if lod != 0 {
                    2
                } else {
                    1
                };
                self.emit(
                    (0xd800000000000000 & !(u64::from(insn.bits(79, 1)) << 59))
                        | self.predicate
                        | self.dest
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(insn.bits(32, 8)) << 20)
                        | (u64::from(insn.bits(64, 8)) << 28)
                        | (u64::from(insn.bits(40, 14)) << 36)
                        | (u64::from(insn.bits(90, 1)) << 49)
                        | (u64::from(swizzle) << 50)
                        | (texture_type << 53),
                );
            }
            _ => unreachable!("caller selected an opcode outside the texture-sample switch arms"),
        }
        Ok(())
    }

    // Contiguous F2I, TXQ, TLD and DEPBAR arms from the upstream switch.
    fn lower_conversion_and_fetch(
        &mut self,
        opcode: u32,
        required_texture_buffer: &mut u32,
    ) -> Result<(), &'static str> {
        let insn = self.insn;
        match opcode {
            0x305 | 0xb05 | 0xd05 => {
                let mode = insn.words[2] & !(if opcode == 0xd05 { 0x08000000 } else { 0 });
                let signed_half = opcode == 0x305 && matches!(mode, 0x00103100 | 0x00107100);
                let half = signed_half || (opcode == 0x305 && (mode & !0x100) == 0x0010f000);
                let small = (mode & !0xc000) == 0x00212800;
                let short_trunc = opcode == 0x305 && (mode & !0x100) == 0x0021e800;
                let absolute = opcode == 0x305 && !half && insn.bits(62, 1) != 0;
                let negate = opcode == 0x305 && !half && insn.bits(63, 1) != 0;
                let byte = (mode & !0x10100) == 0x0020e000;
                let word = (mode & !0x1c100) == 0x00203000;
                let extended_word = word && (insn.bits(78, 2) == 2
                    || (insn.bits(78, 2) == 3 && insn.bits(80, 1) == 0));
                if (!half && !small && !short_trunc && !byte && !word)
                    || (signed_half && (insn.bits(24, 8) != 0 || insn.words[3] & 0xc00001ff != 0))
                    || ((byte || small || short_trunc || extended_word)
                        && (insn.bits(24, 8) != 0 || insn.words[3] & 0xc00001ff != 0
                            || (opcode == 0xb05 && (insn.bits(63, 1) != 0
                                || (!small && insn.bits(62, 1) != 0)
                                || (small && insn.bits(59, 3) != 0)))
                            || insn.bits(91, 1) != u32::from(opcode == 0xd05)))
                    || (opcode != 0xb05 && (insn.words[1] & !(if half { 0x100000ff }
                        else { 0xff | if absolute { 0x40000000 } else { 0 }
                            | if negate { 0x80000000 } else { 0 } })) != 0)
                    || (opcode == 0xd05 && self.uniforms.register(insn.bits(32, 8)) == u32::MAX)
                {
                    return Err("native float-to-integer conversion mode");
                }
                let is_signed = insn.bits(72, 1) != 0;
                let mut source = if opcode == 0xb05 {
                    self.materialize(2, 0, 252, small && insn.bits(62, 1) != 0, false)
                } else if opcode == 0xd05 { self.uniforms.register(insn.bits(32, 8)) }
                else { insn.bits(32, 8) };
                if half {
                    self.extract_half(HalfSource { reg: source,
                        swizzle: if insn.bits(60, 1) != 0 { 3 } else { 2 },
                        absolute: false, negate: false }, 0, 252);
                    source = 252;
                }
                if absolute || negate {
                    source = self.materialize(0, source, 249, absolute, negate);
                }
                if signed_half {
                    self.mov_imm(253, 0x47800000);
                    self.emit(0x5800000000000000 | self.predicate | 250 | (252u64 << 8)
                        | (253u64 << 20) | (7u64 << 39) | (1u64 << 48) | (1u64 << 54));
                    self.emit(0x5800000000000000 | self.predicate | 251 | (252u64 << 8)
                        | (253u64 << 20) | (7u64 << 39) | (4u64 << 48));
                    self.mov_imm(253, 0xc7800000);
                    self.emit(0x5800000000000000 | self.predicate | 253 | (252u64 << 8)
                        | (253u64 << 20) | (7u64 << 39) | (1u64 << 48));
                    self.emit(0x5c40000000000000 | self.predicate | 252 | (252u64 << 8)
                        | (250u64 << 20) | (7u64 << 48));
                    self.emit(self.base(0x5cb0000000000000) | (2u64 << 8) | (2u64 << 10)
                        | (1u64 << 12) | (252u64 << 20) | (u64::from(insn.bits(78, 2)) << 39));
                    self.mov_imm(250, 0x7fffffff);
                    self.emit(0x5c40000000000000 | self.predicate | 251 | (251u64 << 8)
                        | (250u64 << 20) | (7u64 << 48));
                    self.mov_imm(250, 0x80000000);
                    self.emit(0x5c40000000000000 | self.predicate | 253 | (253u64 << 8)
                        | (250u64 << 20) | (7u64 << 48));
                    self.emit(self.base(0x5c40000000000000) | (self.dest << 8) | (251u64 << 20)
                        | (1u64 << 41) | (7u64 << 48));
                    self.emit(self.base(0x5c40000000000000) | (self.dest << 8) | (253u64 << 20)
                        | (1u64 << 41) | (7u64 << 48));
                    return Ok(());
                }
                if byte {
                    self.emit(0x5800000000000000 | self.predicate | 250 | (u64::from(source) << 8)
                        | (u64::from(source) << 20) | (7u64 << 39) | (7u64 << 48));
                    self.emit(0x5c40000000000000 | self.predicate | 250 | (u64::from(source) << 8)
                        | (250u64 << 20) | (7u64 << 48));
                    self.mov_imm(252, if is_signed { 0xc3000000 } else { 0 });
                    self.emit(0x5c60000000000000 | self.predicate | 250 | (250u64 << 8)
                        | (252u64 << 20) | (15u64 << 39));
                    self.mov_imm(252, if is_signed { 0x42fe0000 } else { 0x437f0000 });
                    self.emit(0x5c60000000000000 | self.predicate | 250 | (250u64 << 8)
                        | (252u64 << 20) | (7u64 << 39));
                    source = 250;
                } else if !is_signed {
                    // Clear NaNs without consuming a guest predicate, and retain
                    // the overflow mask when clamping conversion below 2^32.
                    self.emit(0x5800000000000000 | self.predicate | 250 | (u64::from(source) << 8)
                        | (u64::from(source) << 20) | (7u64 << 39) | (7u64 << 48));
                    self.emit(0x5c40000000000000 | self.predicate | 250 | (u64::from(source) << 8)
                        | (250u64 << 20) | (7u64 << 48));
                    if !small && !short_trunc {
                        self.mov_imm(252, 0x4f800000);
                        self.emit(0x5800000000000000 | self.predicate | 251 | (250u64 << 8)
                            | (252u64 << 20) | (7u64 << 39) | (6u64 << 48));
                        self.mov_imm(252, 0x4f7fffff);
                        self.emit(0x5c60000000000000 | self.predicate | 250 | (250u64 << 8)
                            | (252u64 << 20) | (7u64 << 39));
                    }
                    source = 250;
                }
                self.emit(self.base(0x5cb0000000000000) | ((if small || short_trunc { 1u64 } else { 2 }) << 8)
                    | (2u64 << 10) | (u64::from(is_signed) << 12) | (u64::from(source) << 20)
                    | (u64::from(insn.bits(78, 2)) << 39) | (u64::from(insn.bits(80, 1)) << 44));
                if !is_signed && !small && !short_trunc && !byte {
                    self.emit(self.base(0x5c40000000000000) | (self.dest << 8) | (251u64 << 20)
                        | (1u64 << 41) | (7u64 << 48));
                }
            }
            0xb6f | 0x370 => {
                let bindless = opcode == 0x370;
                let binding = insn.bits(54, 5);
                let mask = insn.bits(72, 4);
                let components = mask.count_ones();
                let second_dest = insn.bits(64, 8);
                let direct = second_dest == 255;
                if insn.bits(59, 5) != u32::from(bindless) || mask == 0
                    || (direct && components > 2) || insn.bits(76, 14) != 0
                    || insn.bits(91, 1) != 0 || (!bindless && binding >= 18)
                    || (bindless && (insn.bits(32, 27) != 0 || insn.bits(24, 8) >= 245))
                    || (!direct && (second_dest >= 245 || self.dest >= 245))
                    || insn.bits(40, 14) > 0x1fff
                    || (direct && components == 2 && self.dest & 1 != 0) || self.dest >= 254 {
                    return Err("native texture dimension query mode");
                }
                if !bindless {
                    if *required_texture_buffer != u32::MAX && *required_texture_buffer != binding {
                        return Err("native texture descriptor buffer conflict");
                    }
                    *required_texture_buffer = binding;
                }
                self.emit((if bindless { 0xdf50000000000000 } else { 0xdf48000000000000 })
                    | self.predicate | (if direct { self.dest } else { 250 })
                    | (u64::from(insn.bits(24, 8)) << 8) | (1u64 << 22)
                    | (u64::from(mask) << 31)
                    | (u64::from(if bindless { 0 } else { insn.bits(40, 14) }) << 36)
                    | (u64::from(insn.bits(90, 1)) << 49));
                if !direct {
                    for i in 0..components {
                        let target = if i < 2 {
                            if self.dest == 255 { 255 } else { self.dest as u32 + i }
                        } else { second_dest + i - 2 };
                        if target != 255 {
                            self.emit(0x5c98000000000000 | self.predicate | u64::from(target)
                                | (15u64 << 39) | (u64::from(250 + i) << 20));
                        }
                    }
                }
            }
            0xb66 => {
                let mut mask = insn.bits(72, 4);
                let level = insn.bits(87, 3) == 3;
                let offsets = insn.bits(76, 1) != 0;
                let half = insn.bits(79, 1) != 0;
                let dimension = insn.bits(61, 3);
                let array = dimension == 5;
                let volume = dimension == 2;
                let source_a = insn.bits(24, 8);
                let mut source_b = insn.bits(32, 8);
                let mut second_dest = insn.bits(64, 8);
                // TLDS has no single-pair R+B/G+B swizzle: discard the added alpha in R246.
                if second_dest == 255 && matches!(mask, 5 | 6) {
                    mask |= 8;
                    second_dest = 246;
                }
                let split = second_dest != 255;
                const RG_MASKS: [u32; 8] = [1, 2, 4, 8, 3, 9, 10, 12];
                const RGBA_MASKS: [u32; 5] = [7, 11, 13, 14, 15];
                let mut swizzle = u32::MAX;
                if split {
                    for (i, &value) in RGBA_MASKS.iter().enumerate() {
                        if value == mask {
                            swizzle = i as u32;
                        }
                    }
                } else {
                    for (i, &value) in RG_MASKS.iter().enumerate() {
                        if value == mask {
                            swizzle = i as u32;
                        }
                    }
                }
                if insn.bits(60, 1) == 0
                    || (dimension != 0 && dimension != 1 && !array && !volume)
                    || swizzle == u32::MAX
                    || insn.bits(76, 5) & !0x9 != 0
                    || (half && array && level)
                    || (offsets && (dimension != 1 || source_a & 1 != 0 || source_a >= 254
                        || source_b == 255 || (level && (source_b & 1 != 0 || source_b >= 254))))
                    || insn.bits(81, 3) != 7
                    || (insn.bits(87, 3) != 1 && !level)
                    || (level && volume)
                    || (level && dimension == 1 && (source_a & 1 != 0 || source_a >= 254))
                    || (level && array && (source_b & 1 != 0 || source_b >= 254))
                    || insn.bits(59, 1) != 0
                    || insn.bits(40, 14) > 0x1fff
                    || insn.bits(54, 5) >= 18
                    || self.dest == 255
                    || (!half && mask & (mask - 1) != 0 && (self.dest & 1 != 0 || self.dest >= 254))
                    || (!half && split && mask == 15 && (second_dest & 1 != 0 || second_dest >= 254))
                    || (array && (source_a & 1 != 0 || source_a >= 249))
                    || (volume && (source_a & 1 != 0 || source_a >= 254))
                {
                    return Err("native texel fetch mode or descriptor binding");
                }
                let binding = insn.bits(54, 5);
                if *required_texture_buffer != u32::MAX && *required_texture_buffer != binding {
                    if level || offsets || half { return Err("native texture descriptor buffer conflict"); }
                    // A second descriptor buffer: TLD.B (LZ) with the coordinates in R250.. (array
                    // index or Z after Ra) and the handle, loaded from that buffer, in R253.
                    let copy = |this: &mut Self, target: u32, source: u32| {
                        this.emit(0x5c98000000000000 | this.predicate | u64::from(target) | (15u64 << 39) | (u64::from(source) << 20));
                    };
                    copy(self, 250, source_a);
                    if array || volume { copy(self, 251, source_a + 1); }
                    if dimension != 0 { copy(self, if array || volume { 252 } else { 251 }, source_b); }
                    self.emit(0x4c98000000070000 | 253 | (15u64 << 39) | (u64::from(insn.bits(40, 14)) << 20) |
                         (u64::from(binding) << 34));
                    self.emit(0xdd00000000000000 | self.predicate | 250 | (250u64 << 8) | (253u64 << 20) |
                         (u64::from(if array { 3u32 } else if volume { 4 } else if dimension == 0 { 0 } else { 2 }) << 28) | (u64::from(mask) << 31) |
                         (u64::from(insn.bits(90, 1)) << 49) | (7u64 << 51));
                    let components = mask.count_ones();
                    for i in 0..components {
                        copy(self, if i < 2 || !split { self.dest as u32 + i } else { second_dest + i - 2 }, 250 + i);
                    }
                    return Ok(());
                }
                *required_texture_buffer = binding;
                if array && level {
                    // TLDS has no array form with a level: TLD takes (layer, x, y) from Ra and the level
                    // from Rb (layer, x in Ra, Ra+1; y, level in Rb, Rb+1 here). Results come out packed,
                    // then go where the Ampere destinations (Rd, Rd+1, then the second pair) take them.
                    for i in 0u32..2 {
                        self.emit(0x5c98000000000000 | self.predicate | u64::from(250 + i) | (15u64 << 39) | (u64::from(source_a + i) << 20));
                        self.emit(0x5c98000000000000 | self.predicate | u64::from(252 + i) | (15u64 << 39) | (u64::from(source_b + i) << 20));
                    }
                    self.emit(0xdc00000000000000 | self.predicate | 246 | (250u64 << 8) | (253u64 << 20) |
                         (3u64 << 28) | (u64::from(mask) << 31) | (u64::from(insn.bits(40, 14)) << 36) |
                         (u64::from(insn.bits(90, 1)) << 49) | (7u64 << 51) | (1u64 << 55));
                    let components = mask.count_ones();
                    for i in 0..components {
                        let target = if !split || i < 2 { self.dest as u32 + i } else { second_dest + i - 2 };
                        if target != 255 && (split || i < 2) {
                            self.emit(0x5c98000000000000 | self.predicate | u64::from(target) | (15u64 << 39) | (u64::from(246 + i) << 20));
                        }
                    }
                    return Ok(());
                }
                if array {
                    self.emit(0x5c98000000000000 | self.predicate | 250 | (15u64 << 39) | (u64::from(source_a + 1) << 20));
                    self.emit(0x5c98000000000000 | self.predicate | 251 | (15u64 << 39) | (u64::from(source_b) << 20));
                    source_b = 250;
                }
                self.emit((0xda00000000000000 & !(u64::from(half) << 59)) | self.predicate | self.dest | (u64::from(source_a) << 8) |
                     (u64::from(source_b) << 20) | (u64::from(second_dest) << 28) |
                     (u64::from(insn.bits(40, 14)) << 36) | (u64::from(insn.bits(90, 1)) << 49) |
                     (u64::from(swizzle) << 50) |
                     (u64::from(if offsets { if level { 12u32 } else { 4 } } else if array { 8 } else if volume { 7 } else if dimension == 0 { if level { 1 } else { 0 } } else if level { 5 } else { 2 }) << 53));
            }
            0x91a => {
                if insn.words[0] != 0x791a
                    || (insn.words[1] & !0xfc0) != 0xd000
                    || insn.words[2] != 0
                {
                    return Err("native dependency barrier mode");
                }
            }
            _ => unreachable!("caller selected an opcode outside the conversion/fetch switch arms"),
        }
        Ok(())
    }

    // Contiguous S2R through fragment-discard arms. USEL examines the original
    // preceding native instruction to preserve the upstream clamp idiom.
    fn lower_special_and_global(
        &mut self,
        opcode: u32,
        code: &[Instruction],
        index: u32,
        relocations: &[Relocation],
    ) -> Result<(), &'static str> {
        let insn = self.insn;
        match opcode {
            0x919 | 0x9c3 => {
                let special = insn.bits(72, 8);
                if special == 132 && insn.words[1] == 0 && insn.words[2] == special << 8 {
                    // Fritchii SR_VARIABLE_RATE: without variable-rate shading,
                    // fragment width and height are 1 in bytes 1 and 2.
                    let target = if opcode == 0x9c3 {
                        self.uniforms.register(self.dest as u32)
                    } else {
                        self.dest as u32
                    };
                    if target == u32::MAX {
                        return Err("native uniform special destination");
                    }
                    self.mov_imm(target, 0x00010100);
                    return Ok(());
                }
                if insn.words[1] != 0
                    || insn.words[2] != special << 8
                    || !matches!(
                        special,
                        0 | 33 | 34 | 35 | 37 | 38 | 39 | 56 | 57 | 58 | 59 | 60
                    )
                {
                    return Err("native special register");
                }
                let target = if opcode == 0x9c3 {
                    self.uniforms.register(self.dest as u32)
                } else {
                    self.dest as u32
                };
                if target == u32::MAX {
                    return Err("native uniform special destination");
                }
                self.emit(
                    0xf0c8000000000000
                        | self.predicate
                        | u64::from(target)
                        | (u64::from(special) << 20),
                );
            }
            0x28c | 0x88c => {
                let output = insn.bits(81, 3);
                let a = self.uniforms.register(insn.bits(24, 8));
                let b = if opcode == 0x88c {
                    252
                } else {
                    self.uniforms.register(insn.bits(32, 8))
                };
                let combine = insn.bits(87, 3);
                let combine_op = insn.bits(74, 2);
                if output > 6
                    || insn.bits(84, 3) != 7
                    || insn.bits(72, 1) != 0 || combine_op == 3
                    || a == u32::MAX
                    || b == u32::MAX
                {
                    return Err("native uniform comparison mode");
                }
                let temporary = 6 - output;
                let combine_slot = if combine == 7 { 7 } else { 6 - combine };
                let combine_mask = if combine == 7 { 0 } else { 1u32 << combine_slot };
                let native_predicate = insn.bits(12, 4);
                let guard_slot = 6u32.wrapping_sub(native_predicate & 7);
                let guarded = native_predicate != 7;
                let temporary_mask = 1u32 << temporary;
                // !PT is skipped by the program loop; keep isolated helper calls defined too.
                let guard_mask = 1u32.checked_shl(guard_slot).unwrap_or(0);
                let load_mask = combine_mask | if guarded { temporary_mask | guard_mask } else { 0 };
                let mask = temporary_mask | load_mask;
                if opcode == 0x88c {
                    self.emit(0x0100000000000000 | (7u64 << 16) | 252 | (15u64 << 12)
                        | (u64::from(insn.words[1]) << 20));
                }
                self.emit(0x38e8000000070000 | 253 | (255u64 << 8) | (u64::from(mask) << 20));
                if load_mask != 0 {
                    self.emit(0x38f0000000070000 | (239u64 << 8) | (u64::from(load_mask) << 20));
                }
                self.emit(
                    0x5b60000000000000
                        | (u64::from(if guarded { (native_predicate & 8) | guard_slot } else { 7 }) << 16)
                        | 7
                        | (u64::from(temporary) << 3)
                        | (u64::from(a) << 8)
                        | (u64::from(b) << 20)
                        | (u64::from(combine_slot) << 39)
                        | (u64::from(insn.bits(90, 1)) << 42)
                        | (u64::from(combine_op) << 45)
                        | (u64::from(insn.bits(73, 1)) << 48)
                        | (u64::from(insn.bits(76, 3)) << 49),
                );
                self.emit(0x38e8000000070000 | 239 | (239u64 << 8) | (u64::from(temporary_mask) << 20));
                self.emit(0x38f0000000070000 | (253u64 << 8) | (u64::from(mask) << 20));
            }
            0x287 | 0x887 => {
                let condition = insn.bits(87, 3);
                let target = self.uniforms.register(self.dest as u32);
                let a = self.uniforms.register(insn.bits(24, 8));
                let b = if opcode == 0x887 {
                    252
                } else {
                    self.uniforms.register(insn.bits(32, 8))
                };
                if condition > 6
                    || target == u32::MAX
                    || a == u32::MAX
                    || b == u32::MAX
                    || (insn.words[2] & !0x07800000) != 0x08000000
                {
                    return Err("native uniform select mode");
                }
                let native_predicate = insn.bits(12, 4);
                let guard_slot = 6u32.wrapping_sub(native_predicate & 7);
                let mut compare = index;
                for back in 1..=8.min(index) {
                    let earlier = code[(index - back) as usize].bits(0, 12);
                    let datapath = earlier & 0x1ff;
                    if earlier == 0x88c { compare = index - back; break; }
                    if (0x080..=0x0bf).contains(&datapath) || (0x1c2..=0x1c4).contains(&datapath)
                        || (0x940..=0x95f).contains(&earlier) { break; }
                }
                let target_between = relocations.iter().any(|r| r.target > i64::from(compare) && r.target <= i64::from(index));
                if opcode == 0x887 && native_predicate == 7 && compare < index && !target_between
                    && condition == 0
                    && insn.words[1] <= 0x7ffff
                    && code[compare as usize].bits(12, 4) == 7
                    && code[compare as usize].bits(24, 8) == insn.bits(24, 8)
                    && code[compare as usize].words[1] == insn.words[1]
                    && code[compare as usize].words[2] == 0x0bf01070
                    && insn.words[2] == 0x08000000
                {
                    self.emit(
                        0x3820000000070000
                            | u64::from(target)
                            | (u64::from(a) << 8)
                            | (u64::from(insn.words[1]) << 20)
                            | (7u64 << 39),
                    );
                    return Ok(());
                }
                if opcode == 0x887 {
                    self.emit(0x0100000000000000 | (7u64 << 16) | 252 | (15u64 << 12)
                        | (u64::from(insn.words[1]) << 20));
                }
                let temporary = 6 - condition;
                let mask = (1u32 << temporary) | if native_predicate != 7 && guard_slot < 7 { 1 << guard_slot } else { 0 };
                self.emit(0x38e8000000070000 | 253 | (255u64 << 8) | (u64::from(mask) << 20));
                self.emit(0x38f0000000070000 | (239u64 << 8) | (u64::from(mask) << 20));
                self.emit(
                    0x5ca0000000000000
                        | (u64::from((native_predicate & 8) | if native_predicate != 7 { guard_slot } else { 7 }) << 16)
                        | u64::from(target)
                        | (u64::from(a) << 8)
                        | (u64::from(b) << 20)
                        | (u64::from(temporary) << 39)
                        | (u64::from(insn.bits(90, 1)) << 42),
                );
                self.emit(0x38f0000000070000 | (253u64 << 8) | (u64::from(mask) << 20));
            }
            0x381 | 0x981 => {
                let size = insn.bits(73, 3);
                let mut address = insn.bits(24, 8);
                if size > 6
                    || (opcode == 0x381 && insn.bits(32, 8) != 0)
                    || (size == 5 && self.dest & 1 != 0)
                    || (size == 6 && self.dest & 3 != 0)
                {
                    return Err("native global load mode");
                }
                if opcode == 0x381 {
                    if (insn.words[2] & !0x2e00) != 0x001e8100 || address & 1 != 0 || address >= 246
                    {
                        return Err("native global load mode");
                    }
                } else {
                    let uniform_address = if insn.bits(38, 2) != 0 {
                        u32::MAX
                    } else {
                        self.uniforms.register(insn.bits(32, 6))
                    };
                    if (insn.words[2] & !(0x04000000 | 0x3f | 0xe00 | 0x2000)) != 0x081e8100
                        || uniform_address == u32::MAX
                        || uniform_address & 1 != 0
                    {
                        return Err("native global load uniform addressing mode");
                    }
                    if address == 255 {
                        address = uniform_address;
                    } else {
                        if insn.bits(90, 1) != 0 && (address & 1 != 0 || address >= 246) {
                            return Err("native global load register address pair");
                        }
                        self.emit(
                            0x5c10000000000000
                                | self.predicate
                                | 250
                                | (u64::from(uniform_address) << 8)
                                | (u64::from(address) << 20)
                                | (1u64 << 47),
                        );
                        self.emit(
                            0x5c10000000000000
                                | self.predicate
                                | 251
                                | (u64::from(uniform_address + 1) << 8)
                                | (u64::from(if insn.bits(90, 1) != 0 {
                                    address + 1
                                } else {
                                    255
                                }) << 20)
                                | (1u64 << 43),
                        );
                        address = 250;
                    }
                }
                self.emit(
                    self.base(0xeed0000000000000)
                        | (u64::from(address) << 8)
                        | (u64::from(insn.bits(40, 24)) << 20)
                        | (1u64 << 45)
                        | (u64::from(size) << 48),
                );
            }
            0x386 | 0x986 => {
                let mut data = insn.bits(32, 8);
                let size = insn.bits(73, 3);
                if size > 6
                    || (data != 255
                        && ((size == 5 && data & 1 != 0)
                            || (size == 6 && data & 3 != 0)
                            || data + if size < 4 { 1 } else { 1 << (size - 4) } > 246))
                {
                    return Err("native global store size");
                }
                if data == 255 && size > 4 {
                    data = if size == 6 { 248 } else { 250 };
                    for i in 0..(1u32 << (size - 4)) {
                        self.mov_imm(data + i, 0);
                    }
                }
                let mut address = insn.bits(24, 8);
                let extended = insn.bits(72, 1) != 0;
                if opcode == 0x986 {
                    let uniform_address = self.uniforms.register(insn.bits(64, 6));
                    if (insn.words[2] & !(0x04000000 | 0x3f | 0xe00)) != 0x0810a100
                        || uniform_address == u32::MAX
                        || uniform_address & 1 != 0
                        || !extended
                    {
                        return Err("native global store uniform addressing mode");
                    }
                    if address == 255 {
                        address = uniform_address;
                    } else {
                        if insn.bits(90, 1) != 0 && (address & 1 != 0 || address >= 254) {
                            return Err("native global store register address pair");
                        }
                        self.emit(
                            0x5c10000000000000
                                | self.predicate
                                | 250
                                | (u64::from(uniform_address) << 8)
                                | (u64::from(address) << 20)
                                | (1u64 << 47),
                        );
                        self.emit(
                            0x5c10000000000000
                                | self.predicate
                                | 251
                                | (u64::from(uniform_address + 1) << 8)
                                | (u64::from(if insn.bits(90, 1) != 0 {
                                    address + 1
                                } else {
                                    255
                                }) << 20)
                                | (1u64 << 43),
                        );
                        address = 250;
                    }
                } else if (insn.words[2] & !0xe00) != 0x0010a100 {
                    return Err("native global store register addressing mode");
                }
                self.emit(
                    0xeed8000000000000
                        | self.predicate
                        | u64::from(data)
                        | (u64::from(address) << 8)
                        | (u64::from(insn.bits(40, 24)) << 20)
                        | (u64::from(extended) << 45)
                        | (u64::from(size) << 48),
                );
            }
            0x95b => {
                if insn.words[1] != 0 || insn.words[2] != 0x03800000 {
                    return Err("native fragment discard mode");
                }
                self.emit(0xe33000000000000f | self.predicate);
            }
            _ => unreachable!("caller selected an opcode outside the special/global switch arms"),
        }
        Ok(())
    }

    fn lower_matrix_multiply(&mut self, uses_uniform_bank: bool, compact_matrix: bool) -> Result<(), &'static str> {
        // Fritchii HMMA software warp expansion (compact_matrix=false).
        let insn = self.insn;
        let a = insn.bits(24, 8);
        let b = insn.bits(32, 8);
        let c = insn.bits(64, 8);
        let mode = insn.words[2] & !0xff;
        let depth = if mode == 0 { 8 } else { 16 };
        let a_count = depth / 4;
        let b_count = depth / 8;
        let limit = if uses_uniform_bank { self.uniforms.base() } else { 246 };
        if insn.bits(12, 4) != 7 || (mode != 0 && mode != 0x800)
            || insn.words[1] & !0xff != 0 || insn.words[3] & 0xc00001ff != 0
            || self.dest % 2 != 0 || self.dest + 2 > u64::from(limit)
            || a % a_count != 0 || a + a_count > limit
            || b % b_count != 0 || b + b_count > limit
            || (c != 255 && (c % 2 != 0 || c + 2 > limit)) {
            return Err("native half matrix multiply mode");
        }
        if compact_matrix {
            self.emit(0x5d30000000000000 | self.predicate | self.dest | (u64::from(a) << 8)
                | (u64::from(b) << 20) | (u64::from(c) << 39) | (1u64 << 48)
                | (u64::from(depth == 8) << 49));
            return Ok(());
        }
        // Four accumulators and five temporaries are confined to R246..R254.
        let shuffle = |this: &mut Self, target: u32, source: u32, lane: u32| {
            this.emit(0xef10000000000000 | this.predicate | u64::from(target)
                | (u64::from(source) << 8) | (u64::from(lane) << 20)
                | (1u64 << 29) | (0x1fu64 << 34) | (7u64 << 48));
        };
        let round_half = |this: &mut Self, reg: u32| {
            this.emit(0x5ca8000000000000 | this.predicate | u64::from(reg)
                | (1u64 << 8) | (2u64 << 10) | (u64::from(reg) << 20));
        };
        for row in 0..2 {
            for col in 0..2 {
                self.extract_half(HalfSource { reg: if c == 255 { 255 } else { c + row },
                    swizzle: 0, absolute: false, negate: false }, col, 246 + row * 2 + col);
            }
        }
        for pair in 0..depth / 2 {
            self.emit(0xf0c8000000000000 | self.predicate | 250);
            self.emit(0x0400000000000000 | self.predicate | 251 | (250u64 << 8) | (3u64 << 20));
            self.emit(0x3848000000000000 | self.predicate | 251 | (251u64 << 8) | (3u64 << 20));
            self.emit(0x0400000000000000 | self.predicate | 250 | (250u64 << 8) | (0x1cu64 << 20));
            self.mov_imm(254, pair & 3);
            for reg in [250u64, 251] {
                self.emit(0x5c10000000000000 | self.predicate | reg | (reg << 8) | (254u64 << 20));
            }
            for row in 0..2 {
                for half in 0..2 {
                    shuffle(self, 252, a + if pair >= 4 { 2 } else { 0 } + row, 250);
                    self.extract_half(HalfSource { reg: 252, swizzle: 0, absolute: false, negate: false }, half, 252);
                    for col in 0..2 {
                        self.mov_imm(254, col * 4);
                        self.emit(0x5c10000000000000 | self.predicate | 254 | (251u64 << 8) | (254u64 << 20));
                        shuffle(self, 253, b + pair / 4, 254);
                        self.extract_half(HalfSource { reg: 253, swizzle: 0, absolute: false, negate: false }, half, 253);
                        let acc = u64::from(246 + row * 2 + col);
                        self.emit(0x5980000000000000 | self.predicate | acc | (252u64 << 8)
                            | (253u64 << 20) | (acc << 39));
                    }
                }
            }
            if pair & 3 == 3 {
                for reg in 246..250 {
                    round_half(self, reg);
                    if pair + 1 < depth / 2 {
                        self.extract_half(HalfSource { reg, swizzle: 2, absolute: false, negate: false }, 0, reg);
                    }
                }
            }
        }
        for row in 0..2u64 {
            self.emit(0x5c18000000000000 | self.predicate | (self.dest + row)
                | ((247 + row * 2) << 8) | ((246 + row * 2) << 20) | (16u64 << 39));
        }
        Ok(())
    }

    fn lower_matrix_load(&mut self) -> Result<(), &'static str> {
        let insn = self.insn;
        let count = 1u32 << insn.bits(72, 2);
        let transpose = insn.bits(78, 1) != 0;
        if insn.bits(72, 2) == 3
            || (insn.words[2] & !0x4300) != 0
            || insn.bits(32, 8) != 0
            || self.dest + u64::from(count) > 246
        {
            return Err("native matrix load mode");
        }
        let mut address = insn.bits(24, 8);
        let offset = u64::from(insn.bits(40, 24));
        if address != 255
            && u64::from(address) >= self.dest
            && u64::from(address) < self.dest + u64::from(count)
        {
            self.emit(
                0x5c10000000000000
                    | self.predicate
                    | 253
                    | (u64::from(address) << 8)
                    | (255u64 << 20),
            );
            address = 253;
        }
        if transpose {
            self.emit(0xf0c8000000000000 | self.predicate | 246);
            self.emit(0x0400000000000000 | self.predicate | 247 | (246u64 << 8) | (3u64 << 20));
            self.emit(0x3848000000000000 | self.predicate | 247 | (247u64 << 8) | (1u64 << 20));
            self.emit(0x3828000000000000 | self.predicate | 248 | (246u64 << 8) | (2u64 << 20));
            self.emit(0x3848000000000000 | self.predicate | 248 | (248u64 << 8) | (1u64 << 20));
            for matrix in 0..count {
                for half in 0..2u32 {
                    self.emit(
                        0x1c00000000000000
                            | self.predicate
                            | 249
                            | (247u64 << 8)
                            | (u64::from(matrix * 8 + half) << 20),
                    );
                    self.emit(
                        0xef10000000000000
                            | self.predicate
                            | 250
                            | (u64::from(address) << 8)
                            | (249u64 << 20)
                            | (1u64 << 29)
                            | (31u64 << 34)
                            | (7u64 << 48),
                    );
                    self.emit(
                        0x5c10000000000000 | self.predicate | 250 | (250u64 << 8) | (248u64 << 20),
                    );
                    self.emit(
                        0xef48000000000000
                            | self.predicate
                            | u64::from(251 + half)
                            | (250u64 << 8)
                            | (offset << 20)
                            | (2u64 << 48),
                    );
                }
                self.emit(
                    0x5c18000000000000
                        | self.predicate
                        | (self.dest + u64::from(matrix))
                        | (252u64 << 8)
                        | (251u64 << 20)
                        | (16u64 << 39),
                );
            }
            return Ok(());
        }
        self.emit(0xf0c8000000000000 | self.predicate | 250);
        self.emit(0x3828000000000000 | self.predicate | 251 | (250u64 << 8) | (2u64 << 20));
        self.emit(0x0400000000000000 | self.predicate | 250 | (250u64 << 8) | (3u64 << 20));
        self.emit(0x3848000000000000 | self.predicate | 250 | (250u64 << 8) | (2u64 << 20));
        for matrix in 0..count {
            let mut lane: u32 = 251;
            if matrix != 0 {
                self.emit(
                    0x1c00000000000000
                        | self.predicate
                        | 252
                        | (251u64 << 8)
                        | (u64::from(matrix * 8) << 20),
                );
                lane = 252;
            }
            self.emit(
                0xef10000000000000
                    | self.predicate
                    | 253
                    | (u64::from(address) << 8)
                    | (u64::from(lane) << 20)
                    | (1u64 << 29)
                    | (31u64 << 34)
                    | (7u64 << 48),
            );
            self.emit(0x5c10000000000000 | self.predicate | 253 | (253u64 << 8) | (250u64 << 20));
            self.emit(
                0xef48000000000000
                    | self.predicate
                    | (self.dest + u64::from(matrix))
                    | (253u64 << 8)
                    | (offset << 20)
                    | (4u64 << 48),
            );
        }
        Ok(())
    }

    // Contiguous BREV through MOVM arms, before the opt-in HMMA implementation.
    fn lower_bits_and_shared(
        &mut self,
        opcode: u32,
        native_predicate: u32,
    ) -> Result<(), &'static str> {
        let insn = self.insn;
        match opcode {
            0x301 => {
                if insn.words[2] != 0 || insn.words[1] > 255 {
                    return Err("native bit reverse mode");
                }
                self.emit(
                    self.base(0x3828000000000000)
                        | (u64::from(insn.words[1]) << 8)
                        | (1u64 << 39)
                        | (1u64 << 40),
                );
            }
            0x219 | 0xa19 | 0xc19 => {
                let low = insn.bits(24, 8);
                let high = insn.bits(64, 8);
                let constant = opcode == 0xa19;
                let shift = if opcode == 0xc19 {
                    self.uniforms.register(insn.bits(32, 8))
                } else if constant {
                    251
                } else {
                    insn.bits(32, 8)
                };
                let shift_operand = if constant { self.valid_cbuf() && insn.bits(62, 2) == 0 }
                    else { insn.words[1] <= 255 };
                let right = insn.bits(76, 1) != 0;
                let high_result = insn.bits(80, 1) != 0;
                if opcode == 0x219 && shift_operand && insn.words[2] & !0xff == 0x00001200 {
                    self.right_shift_u64_low(self.dest as u32, low, high, shift);
                    return Ok(());
                }
                if opcode == 0x219 && low == 255 && shift_operand
                    && insn.words[2] & !0x8ff == 0x00011400 && insn.words[3] & 0xc00001ff == 0 {
                    let wrap = insn.bits(75, 1) != 0;
                    if !wrap {
                        self.emit(0x3820000000000000 | self.predicate | 250 | (u64::from(shift) << 8)
                            | (31u64 << 20) | (7u64 << 39));
                    }
                    self.emit(self.base(0x5c28000000000000) | (u64::from(high) << 8)
                        | (u64::from(if wrap { shift } else { 250 }) << 20) | (1u64 << 39) | (1u64 << 48));
                    return Ok(());
                }
                if right
                    && !high_result
                    && high == 255
                    && insn.bits(73, 2) == 3
                    && shift != u32::MAX
                    && shift_operand
                    && (insn.words[2]
                        & !(0x000118ff | if opcode == 0xc19 { 0x08000000 } else { 0 }))
                        == 0x00000600
                {
                    if constant { self.materialize(2, 0, 251, false, false); }
                    self.emit(
                        self.base(0x5c28000000000000)
                            | (u64::from(low) << 8)
                            | (u64::from(shift) << 20),
                    );
                    return Ok(());
                }
                if !shift_operand
                    || insn.bits(73, 2) != 3
                    || shift == u32::MAX
                    || (insn.words[2]
                        & !(0x000118ff | if opcode == 0xc19 { 0x08000000 } else { 0 }))
                        != 0x00000600
                    || if right {
                        !high_result || low != 255
                    } else {
                        if high_result { low != 255 } else { high != 255 }
                    }
                {
                    return Err("native register shift mode");
                }
                if constant { self.materialize(2, 0, 251, false, false); }
                self.emit(
                    self.base(if right {
                        0x5cf8000000000000
                    } else {
                        0x5bf8000000000000
                    }) | (u64::from(if right { high } else { 255 }) << 8)
                        | (u64::from(shift) << 20)
                        | (u64::from(if right { 255 } else if high_result { high } else { low }) << 39)
                        | (u64::from(insn.bits(75, 1)) << 50),
                );
            }
            0x300 | 0xd00 => {
                if (insn.words[2] & !(0x600 | if opcode == 0xd00 { 0x08000000 } else { 0 }))
                    != 0x000e0000
                {
                    return Err("native leading bit mode");
                }
                let source = self.operand(1, false);
                self.emit(
                    self.base(0x5c30000000000000)
                        | (u64::from(source) << 20)
                        | (u64::from(insn.bits(63, 1)) << 40)
                        | (u64::from(insn.bits(74, 1)) << 41)
                        | (u64::from(insn.bits(73, 1)) << 48),
                );
            }
            0x309 | 0xd09 => {
                if insn.words[2] != if opcode == 0xd09 { 0x08000000 } else { 0 } {
                    return Err("native population count mode");
                }
                let source = self.operand(1, false);
                self.emit(
                    self.base(0x5c08000000000000)
                        | (u64::from(source) << 20)
                        | (u64::from(insn.bits(63, 1)) << 40),
                );
            }
            0x918 => {
                if insn.words[1] != 0 || insn.words[2] != 0 {
                    return Err("native NOP mode");
                }
                self.emit(0x50b0000000000f00 | self.predicate);
            }
            0x992 => {
                let scope = insn.bits(76, 3);
                if insn.words[1] != 0
                    || insn.words[2] != scope << 12
                    || !matches!(scope, 0 | 2 | 3 | 5)
                {
                    return Err("native memory barrier scope");
                }
                let legacy_scope: u32 = if scope == 0 {
                    0
                } else if scope == 2 {
                    1
                } else if scope == 3 {
                    2
                } else {
                    3
                };
                self.emit(0xef98000000000000 | self.predicate | (u64::from(legacy_scope) << 8));
            }
            0x9ab => {
                if insn.words[1] != 0 || insn.words[2] != 0 {
                    return Err("native error barrier mode");
                }
                self.emit(0xef98000000000000 | self.predicate | (1u64 << 8));
            }
            0xb1d => {
                if insn.words[1] != 0 || insn.words[2] != 0 || native_predicate != 7 {
                    return Err("native workgroup barrier mode");
                }
                self.emit(
                    0xf0a8000000000000
                        | self.predicate
                        | (1u64 << 43)
                        | (1u64 << 44)
                        | (1u64 << 39)
                        | (7u64 << 39),
                );
            }
            0x388 | 0x984 => {
                let width = insn.bits(73, 3);
                let stride = insn.bits(78, 2);
                let uniform_base = opcode == 0x984 && insn.bits(91, 1) != 0;
                let base_register = if uniform_base {
                    self.uniforms.register(insn.bits(32, 8))
                } else {
                    255
                };
                if width > 6
                    || (insn.words[2] & !(if uniform_base { 0x08000000 } else { 0 }))
                        != ((width << 9) | (stride << 14))
                    || (opcode == 0x984 && !uniform_base && insn.bits(32, 8) != 0)
                    || base_register == u32::MAX
                {
                    return Err("native shared memory addressing mode");
                }
                let mut address = insn.bits(24, 8);
                if stride != 0 && address != 255 {
                    self.emit(
                        0x3848000000000000
                            | self.predicate
                            | 250
                            | (u64::from(address) << 8)
                            | (u64::from(stride + 1) << 20),
                    );
                    address = 250;
                }
                if base_register != 255 {
                    self.emit(
                        0x5c10000000000000
                            | self.predicate
                            | 250
                            | (u64::from(address) << 8)
                            | (u64::from(base_register) << 20),
                    );
                    address = 250;
                }
                let data = if opcode == 0x388 {
                    insn.bits(32, 8)
                } else {
                    self.dest as u32
                };
                self.emit(
                    (if opcode == 0x388 {
                        0xef58000000000000
                    } else {
                        0xef48000000000000
                    }) | self.predicate
                        | u64::from(data)
                        | (u64::from(address) << 8)
                        | (u64::from(insn.bits(40, 24)) << 20)
                        | (u64::from(width) << 48),
                );
            }
            0x23a => {
                let source = insn.bits(24, 8);
                if insn.words[1] != 0 || insn.words[2] != 0 || source >= 246 || self.dest >= 246 {
                    return Err("native matrix move mode");
                }
                self.emit(0xf0c8000000000000 | self.predicate | 246);
                self.emit(0x0400000000000000 | self.predicate | 249 | (246u64 << 8) | (3u64 << 20));
                self.emit(0x3848000000000000 | self.predicate | 249 | (249u64 << 8) | (3u64 << 20));
                self.emit(0x3828000000000000 | self.predicate | 247 | (246u64 << 8) | (3u64 << 20));
                self.emit(
                    0x5c10000000000000 | self.predicate | 249 | (249u64 << 8) | (247u64 << 20),
                );
                self.emit(
                    0xef10000000000000
                        | self.predicate
                        | 247
                        | (u64::from(source) << 8)
                        | (249u64 << 20)
                        | (1u64 << 29)
                        | (31u64 << 34)
                        | (7u64 << 48),
                );
                self.emit(0x1c00000000000000 | self.predicate | 249 | (249u64 << 8) | (4u64 << 20));
                self.emit(
                    0xef10000000000000
                        | self.predicate
                        | 248
                        | (u64::from(source) << 8)
                        | (249u64 << 20)
                        | (1u64 << 29)
                        | (31u64 << 34)
                        | (7u64 << 48),
                );
                self.emit(0x3828000000000000 | self.predicate | 249 | (246u64 << 8) | (2u64 << 20));
                self.emit(0x0400000000000000 | self.predicate | 249 | (249u64 << 8) | (1u64 << 20));
                self.emit(0x3848000000000000 | self.predicate | 249 | (249u64 << 8) | (4u64 << 20));
                self.emit(
                    0x5c28000000000000 | self.predicate | 247 | (247u64 << 8) | (249u64 << 20),
                );
                self.emit(
                    0x0400000000000000 | self.predicate | 247 | (247u64 << 8) | (0xffffu64 << 20),
                );
                self.emit(
                    0x5c28000000000000 | self.predicate | 248 | (248u64 << 8) | (249u64 << 20),
                );
                self.emit(
                    0x5c18000000000000
                        | self.predicate
                        | self.dest
                        | (248u64 << 8)
                        | (247u64 << 20)
                        | (16u64 << 39),
                );
            }
            _ => unreachable!("caller selected an opcode outside the bits/shared switch arms"),
        }
        Ok(())
    }

    // Contiguous TXD through SUST arms. required_texture_buffer refers to the
    // original program result field, separate from its borrowed instruction vector.
    fn lower_texture_and_reduction(
        &mut self,
        opcode: u32,
        native_predicate: u32,
        required_texture_buffer: &mut u32,
    ) -> Result<(), &'static str> {
        let insn = self.insn;
        match opcode {
            0xb6c => {
                let binding = insn.bits(54, 5);
                let offset = insn.bits(40, 14);
                let coordinates = insn.bits(24, 8);
                let derivatives = insn.bits(32, 8);
                let dimension = insn.bits(61, 3);
                let high = insn.bits(64, 8);
                let mask = insn.bits(72, 4);
                let channels = mask.count_ones();
                if (insn.words[2] & !(0xfff | (1 << 26))) != 0x001e0000 || mask == 0
                    || insn.bits(59, 2) != 0
                    || (dimension != 1 && dimension != 5)
                    || binding >= 18
                    || offset > 0x1fff
                    || (channels > 1 && self.dest & 1 != 0)
                    || self.dest >= 248
                    || (channels <= 2 && high != 255)
                    || (high != 255 && ((channels > 3 && high & 1 != 0) || high >= 248))
                    || coordinates >= 248
                    || derivatives >= 247
                {
                    return Err("native gradient texture mode");
                }
                if *required_texture_buffer != u32::MAX && *required_texture_buffer != binding {
                    return Err("native texture descriptor buffer conflict");
                }
                *required_texture_buffer = binding;
                let consecutive = channels <= 2 || u64::from(high) == self.dest + 2;
                let target = if consecutive { self.dest } else { 248 };
                self.emit(
                    0xde00000000000000 | self.predicate | target
                        | (u64::from(coordinates) << 8)
                        | (u64::from(derivatives) << 20)
                        | ((if dimension == 5 { 3u64 } else { 2 }) << 28)
                        | (u64::from(mask) << 31)
                        | (u64::from(offset) << 36)
                        | (u64::from(insn.bits(90, 1)) << 49)
                        | (7u64 << 51),
                );
                if !consecutive {
                    for i in 0..channels {
                        if i >= 2 && high == 255 { break; }
                        let to = if i < 2 { self.dest as u32 + i } else { high + i - 2 };
                        self.emit(0x5c98000000000000 | self.predicate | u64::from(to)
                            | (15u64 << 39) | (u64::from(248 + i) << 20));
                    }
                }
            }
            0x989 => {
                if (insn.words[1] & 0xf01fffff) != 0 || (insn.words[2] & !0x000e00ff) != 0 {
                    return Err("native register-mask shuffle mode");
                }
                self.emit(
                    self.base(0xef10000000000000)
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(insn.bits(53, 5)) << 20)
                        | (1u64 << 28)
                        | (u64::from(insn.bits(58, 2)) << 30)
                        | (u64::from(insn.bits(64, 8)) << 39)
                        | (u64::from(insn.bits(81, 3)) << 48),
                );
            }
            0x589 => {
                if (insn.words[2] & !0x000e0000) != 0 || insn.bits(53, 11) != 0 {
                    return Err("native shuffle mode");
                }
                self.emit(
                    0xef10000000000000
                        | self.predicate
                        | self.dest
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(insn.bits(32, 8)) << 20)
                        | (1u64 << 29)
                        | (u64::from(insn.bits(40, 13)) << 34)
                        | (u64::from(insn.bits(81, 3)) << 48),
                );
            }
            0x98e => {
                // Fritchii RED.E: U32/S32 operations and F32 ADD, with either
                // a register pair or a uniform address and signed byte offset.
                let kind = insn.bits(73, 3);
                let operation = insn.bits(87, 3);
                let uniform_address = insn.bits(24, 8) == 255;
                let variable = 0x03800e00 | if uniform_address { 0x3f } else { 0 };
                let mode = if uniform_address { 0x0c10e100 } else { 0x0010e100 };
                let offset = ((insn.bits(40, 24) << 8) as i32) >> 8;
                if insn.words[2] & !variable != mode
                    || !matches!(kind, 0 | 1 | 3) || (kind == 3 && operation != 0) {
                    return Err("native global reduction mode");
                }
                let address = if uniform_address {
                    self.uniforms.register(insn.bits(64, 6))
                } else {
                    insn.bits(24, 8)
                };
                if address == u32::MAX || address & 1 != 0
                    || !(-0x80000..=0x7ffff).contains(&offset) {
                    return Err("native global reduction address");
                }
                self.emit(0xebf8000000000000 | self.predicate | u64::from(insn.bits(32, 8))
                    | (u64::from(address) << 8) | (u64::from(kind) << 20)
                    | (u64::from(operation) << 23) | (u64::from(offset as u32 & 0xfffff) << 28)
                    | (1u64 << 48));
            }
            0x2bd => {
                let target = self.uniforms.register(self.dest as u32);
                let source = self.uniforms.register(insn.bits(32, 8));
                if self.dest == 63 || target == u32::MAX || source == u32::MAX
                    || insn.words[1] & !0xff != 0
                    || (insn.words[2] & !0x400) != 0x080e0000
                    || native_predicate != 7
                {
                    return Err("native uniform leading bit mode");
                }
                self.emit(
                    0x5c30000000000000
                        | self.predicate
                        | u64::from(target)
                        | (u64::from(source) << 20)
                        | (u64::from(insn.bits(74, 1)) << 41),
                );
            }
            // Pinned Eglantine 900ab5a8: UPOPC reads/writes the uniform mirrors.
            0x2bf => {
                let target = self.uniforms.register(self.dest as u32);
                let source = self.uniforms.register(insn.bits(32, 8));
                if self.dest == 63 || target == u32::MAX || source == u32::MAX
                    || insn.words[1] & !0x800000ff != 0
                    || insn.words[2] != 0x08000000 || native_predicate != 7
                {
                    return Err("native uniform population count mode");
                }
                self.emit(0x5c08000000000000 | self.predicate | u64::from(target)
                    | (u64::from(source) << 20) | (u64::from(insn.bits(63, 1)) << 40));
            }
            0x2be => {
                let target = self.uniforms.register(self.dest as u32);
                let source = self.uniforms.register(insn.bits(32, 8));
                if self.dest == 63 || target == u32::MAX || source == u32::MAX
                    || insn.words[1] & !0xff != 0
                    || insn.words[2] != 0x08000000
                    || native_predicate != 7
                {
                    return Err("native uniform bit reverse mode");
                }
                self.emit(
                    0x3828000000000000
                        | self.predicate
                        | u64::from(target)
                        | (u64::from(source) << 8)
                        | (1u64 << 39)
                        | (1u64 << 40),
                );
            }
            0x3a1 => {
                let source = insn.bits(24, 8);
                let target = insn.bits(81, 3);
                if self.dest != 255
                    || source >= 254
                    || source & 1 != 0
                    || target >= 6
                    || native_predicate != 7
                    || insn.words[1] != 0
                    || (insn.words[2] & !0xe0000) != 0x200
                {
                    return Err("native warp match mode");
                }
                self.save_predicate(1 << 6);
                self.emit(0x50d8000000070000 | 250 | (7u64 << 39) | (7u64 << 45) | (1u64 << 48));
                self.emit(0x5c10000000070000 | 251 | (250u64 << 8) | (255u64 << 20) | (1u64 << 49));
                self.emit(0x5c40000000070000 | 250 | (250u64 << 8) | (251u64 << 20) | (7u64 << 48));
                self.emit(0x5c30000000070000 | 250 | (250u64 << 20));
                for word in 0..2u32 {
                    self.emit(
                        0xef10000000070000
                            | u64::from(251 + word)
                            | (u64::from(source + word) << 8)
                            | (250u64 << 20)
                            | (1u64 << 29)
                            | (31u64 << 34)
                            | (7u64 << 48),
                    );
                }
                self.emit(
                    0x5b60000000070000
                        | 7
                        | (6u64 << 3)
                        | (u64::from(source) << 8)
                        | (251u64 << 20)
                        | (7u64 << 39)
                        | (2u64 << 49),
                );
                self.emit(
                    0x5b60000000070000
                        | 7
                        | (6u64 << 3)
                        | (u64::from(source + 1) << 8)
                        | (252u64 << 20)
                        | (6u64 << 39)
                        | (2u64 << 49),
                );
                self.emit(0x50d8000000070000 | 255 | (6u64 << 39) | (u64::from(target) << 45));
                self.restore_predicate(1 << 6);
            }
            0x3c4 => {
                let source = insn.bits(24, 8);
                let target = self.uniforms.register(self.dest as u32);
                let mode = insn.words[2] & !0x200;
                let maximum = mode == 0x14000;
                let minimum = mode == 0x10000;
                let signed_order = insn.bits(73, 1) != 0;
                let logical = if insn.words[2] == 0x4000 { 1u64 } else if insn.words[2] == 0x8000 { 2 } else { 0 };
                if target == u32::MAX
                    || target == 255
                    || insn.words[1] != 0
                    || (insn.words[2] != 0xc000 && !maximum && !minimum && logical == 0)
                    || (signed_order && !maximum && !minimum)
                    || native_predicate != 7
                {
                    return Err("native warp sum mode");
                }
                self.emit(0x50d8000000070000 | 250 | (7u64 << 39) | (7u64 << 45) | (1u64 << 48));
                self.mov_imm(target, if minimum { u32::MAX } else { 0 });
                for lane in 0..32u32 {
                    self.emit(
                        0xef10000000070000
                            | 251
                            | (u64::from(source) << 8)
                            | (u64::from(lane) << 20)
                            | (1u64 << 28)
                            | (1u64 << 29)
                            | (31u64 << 34)
                            | (7u64 << 48),
                    );
                    if signed_order {
                        self.emit(0x0400000000070000 | 251 | (251u64 << 8) | (0x80000000u64 << 20) | (2u64 << 53));
                    }
                    self.emit(0x3828000000070000 | 252 | (250u64 << 8) | (u64::from(lane) << 20));
                    self.emit(0x0400000000070000 | 252 | (252u64 << 8) | (1u64 << 20));
                    self.emit(
                        0x5c10000000070000 | 252 | (255u64 << 8) | (252u64 << 20) | (1u64 << 48),
                    );
                    self.emit(
                        0x5c40000000070000 | 251 | (251u64 << 8) | (252u64 << 20) | (7u64 << 48)
                            | (u64::from(minimum) << 40) | (u64::from(minimum) << 41),
                    );
                    if minimum {
                        self.emit(0x5c20000000070000 | u64::from(target) | (u64::from(target) << 8)
                            | (251u64 << 20) | (7u64 << 39));
                    } else if maximum {
                        self.emit(
                            0x5c20000000070000
                                | u64::from(target)
                                | (u64::from(target) << 8)
                                | (251u64 << 20)
                                | (15u64 << 39),
                        );
                    } else if logical != 0 {
                        self.emit(0x5c40000000070000 | u64::from(target) | (u64::from(target) << 8)
                            | (251u64 << 20) | (logical << 41) | (7u64 << 48));
                    } else {
                        self.emit(
                            0x5c10000000070000
                                | u64::from(target)
                                | (u64::from(target) << 8)
                                | (251u64 << 20),
                        );
                    }
                }
                if signed_order {
                    self.emit(0x0400000000070000 | u64::from(target) | (u64::from(target) << 8)
                        | (0x80000000u64 << 20) | (2u64 << 53));
                }
            }
            0xf8c => {
                let stride = insn.bits(78, 2);
                if self.dest != 255 || insn.words[1] != 0 || (insn.words[2] & !0xc000) != 0x0d00003f
                {
                    return Err("native shared increment mode");
                }
                let mut address = insn.bits(24, 8);
                if stride != 0 && address != 255 {
                    self.emit(
                        0x3848000000000000
                            | self.predicate
                            | 251
                            | (u64::from(address) << 8)
                            | (u64::from(stride + 1) << 20),
                    );
                    address = 251;
                }
                self.mov_imm(250, 1);
                self.emit(
                    0xec00000000000000
                        | self.predicate
                        | 255
                        | (u64::from(address) << 8)
                        | (250u64 << 20),
                );
            }
            0x38c => {
                let offset = insn.bits(40, 24);
                if insn.words[2] != 0 || offset & 3 != 0 {
                    return Err("native shared atomic mode");
                }
                self.emit(
                    0xec00000000000000
                        | self.predicate
                        | self.dest
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(insn.bits(32, 8)) << 20)
                        | (u64::from(offset >> 2) << 30),
                );
            }
            0x3a8 => {
                let address = insn.bits(24, 8);
                let offset = ((insn.bits(40, 24) << 8) as i32) >> 8;
                if insn.words[2] != 0x001ee100
                    || address & 1 != 0
                    || address >= 246
                    || !(-0x80000..=0x7ffff).contains(&offset)
                {
                    return Err("native global atomic register mode");
                }
                self.emit(
                    0xed00000000000000
                        | self.predicate
                        | self.dest
                        | (u64::from(address) << 8)
                        | (u64::from(insn.bits(32, 8)) << 20)
                        | (u64::from((offset as u32) & 0xfffff) << 28)
                        | (1u64 << 48),
                );
            }
            0x9a8 => {
                let address = self.uniforms.register(insn.bits(64, 6));
                let offset = ((insn.bits(40, 24) << 8) as i32) >> 8;
                if insn.bits(24, 8) != 255
                    || !(-0x80000..=0x7ffff).contains(&offset)
                    || (insn.words[2] & !0x3f) != 0x081ee140
                    || address == u32::MAX
                    || address & 1 != 0
                {
                    return Err("native global atomic mode");
                }
                self.emit(
                    0xed00000000000000
                        | self.predicate
                        | self.dest
                        | (u64::from(address) << 8)
                        | (u64::from(insn.bits(32, 8)) << 20)
                        | (u64::from((offset as u32) & 0xfffff) << 28)
                        | (1u64 << 48),
                );
            }
            0xb97 => {
                let binding = insn.bits(54, 5);
                let offset = insn.bits(40, 14);
                let coordinate = insn.bits(24, 8);
                let channels = insn.bits(72, 4);
                let dimension = insn.bits(61, 3);
                if (insn.words[2] & !0xf00) != 0x001e0000
                    || channels == 0
                    || (dimension != 3 && dimension != 4)
                    || binding >= 18
                    || offset > 0x1fff
                    || insn.bits(59, 2) != 0
                    || self.dest == 255
                    || coordinate > if dimension == 4 { 252 } else { 253 }
                {
                    return Err("native formatted surface load mode");
                }
                if *required_texture_buffer != u32::MAX && *required_texture_buffer != binding {
                    return Err("native surface descriptor buffer conflict");
                }
                *required_texture_buffer = binding;
                self.emit(
                    self.base(0xeb00000000000000)
                        | (u64::from(coordinate) << 8)
                        | (u64::from(channels) << 20)
                        | (u64::from(dimension) << 33)
                        | (u64::from(offset) << 36)
                        | (1u64 << 51),
                );
            }
            0xb9b => {
                let binding = insn.bits(54, 5);
                let offset = insn.bits(40, 14);
                let data = insn.bits(32, 8);
                let coordinate = insn.bits(24, 8);
                let dimension = insn.bits(61, 3);
                if insn.words[2] != 0x00100f00
                    || dimension < 3
                    || dimension > 5
                    || binding >= 18
                    || offset > 0x1fff
                    || insn.bits(59, 2) != 0
                    || data > 252
                    || coordinate > if dimension >= 4 { 252 } else { 253 }
                {
                    return Err("native formatted surface store mode");
                }
                if *required_texture_buffer != u32::MAX && *required_texture_buffer != binding {
                    return Err("native surface descriptor buffer conflict");
                }
                *required_texture_buffer = binding;
                self.emit(
                    0xeb20000000000000
                        | self.predicate
                        | u64::from(data)
                        | (u64::from(coordinate) << 8)
                        | (15u64 << 20)
                        | (u64::from(dimension) << 33)
                        | (u64::from(offset) << 36)
                        | (1u64 << 51),
                );
            }
            // Extension absent from original 50fa86; port of the corresponding
            // arms in Eglantine lower_to_maxwell.h at 900ab5a8251dcdb5eee42dd2837fa936ade15303.
            0x593 | 0x59f => {
                let binding = insn.bits(54, 5);
                let offset = insn.bits(40, 14);
                let data = insn.bits(32, 8);
                let coordinate = insn.bits(24, 8);
                let dimension = insn.bits(61, 3);
                let returns_value = opcode == 0x593;
                let operation = insn.bits(87, 3);
                let mode = if returns_value {
                    0x001ee000
                } else {
                    0x0010e000
                };
                if (insn.words[2] & !0x03800000) != mode
                    || ((returns_value || dimension != 3) && operation != 0 && operation != 6)
                    || (dimension != 3 && dimension != 5)
                    || binding >= 18
                    || offset > 0x1fff
                    || insn.bits(59, 2) != 0
                    || coordinate > if dimension == 5 { 252 } else { 253 }
                {
                    return Err("native surface reduction mode");
                }
                if *required_texture_buffer != u32::MAX && *required_texture_buffer != binding {
                    return Err("native surface descriptor buffer conflict");
                }
                *required_texture_buffer = binding;
                if returns_value {
                    self.emit(
                        self.base(0xea00000000000000)
                            | (u64::from(coordinate) << 8)
                            | (u64::from(data) << 20)
                            | (u64::from(operation) << 29)
                            | (u64::from(dimension) << 33)
                            | (u64::from(offset) << 36),
                    );
                } else {
                    self.emit(
                        0xeb40000000000000
                            | self.predicate
                            | u64::from(data)
                            | (u64::from(coordinate) << 8)
                            | (u64::from(operation) << 24)
                            | (u64::from(dimension) << 33)
                            | (u64::from(offset) << 36)
                            | (1u64 << 51),
                    );
                }
            }
            _ => {
                unreachable!("caller selected an opcode outside the texture/reduction switch arms")
            }
        }
        Ok(())
    }

    // Contiguous VOTE through FSWZADD arms; same captures and constants as upstream.
    fn lower_vote_and_uniform(
        &mut self,
        opcode: u32,
        native_predicate: u32,
    ) -> Result<(), &'static str> {
        let insn = self.insn;
        match opcode {
            0x806 => {
                if insn.words[1] != 0 || insn.bits(72, 2) > 2 || (insn.words[2] & !0x078e0300) != 0
                {
                    return Err("native vote mode");
                }
                self.emit(
                    self.base(0x50d8000000000000)
                        | (u64::from(insn.bits(87, 4)) << 39)
                        | (u64::from(insn.bits(81, 3)) << 45)
                        | (u64::from(insn.bits(72, 2)) << 48),
                );
            }
            0x886 => {
                let output = insn.bits(81, 3);
                let target = insn.bits(16, 8);
                let mapped_target = self.uniforms.register(target);
                if mapped_target == u32::MAX || insn.bits(24, 8) != 0
                    || insn.words[1] != 0
                    || insn.words[2] & !0x078e0300 != 0
                    || insn.bits(72, 2) > 2
                    || native_predicate != 7
                {
                    return Err("native uniform vote mode");
                }
                let temporary = if output == 7 { 7 } else { 6 - output };
                let mask = 1u32 << temporary;
                if output != 7 {
                    self.emit(0x38e8000000070000 | 253 | (255u64 << 8) | (u64::from(mask) << 20));
                }
                self.emit(
                    0x50d8000000070000
                        | u64::from(mapped_target)
                        | (u64::from(insn.bits(87, 4)) << 39)
                        | (u64::from(temporary) << 45)
                        | (u64::from(insn.bits(72, 2)) << 48),
                );
                if output != 7 {
                    self.emit(0x38e8000000070000 | 239 | (239u64 << 8) | (u64::from(mask) << 20));
                    self.emit(0x38f0000000070000 | (253u64 << 8) | (u64::from(mask) << 20));
                }
            }
            0x3c2 => {
                let target = self.uniforms.register(self.dest as u32);
                if target == u32::MAX
                    || target == 255
                    || insn.words[1] != 0
                    || insn.words[2] != 0x000e0000
                {
                    return Err("native register-to-uniform mode");
                }
                self.save_predicate(1 << 6);
                self.emit(
                    0x50d8000000070000
                        | 250
                        | (u64::from(native_predicate) << 39)
                        | (6u64 << 45)
                        | (1u64 << 48),
                );
                self.emit(0x5c10000000070000 | 251 | (250u64 << 8) | (255u64 << 20) | (1u64 << 49));
                self.emit(0x5c40000000070000 | 250 | (250u64 << 8) | (251u64 << 20) | (7u64 << 48));
                self.emit(0x5c30000000070000 | 250 | (250u64 << 20));
                self.emit(
                    0xef10000000070000
                        | 251
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (250u64 << 20)
                        | (1u64 << 29)
                        | (31u64 << 34)
                        | (7u64 << 48),
                );
                self.emit(
                    0x5ca0000000070000
                        | u64::from(target)
                        | (251u64 << 8)
                        | (u64::from(target) << 20)
                        | (6u64 << 39),
                );
                self.restore_predicate(1 << 6);
            }
            0x81c => {
                let target = insn.bits(81, 3);
                let lut = insn.bits(64, 3) | (insn.bits(72, 5) << 3);
                if target == 7
                    || insn.bits(84, 3) != 7
                    || insn.bits(16, 8) != 0
                    || insn.words[1] != 0
                {
                    return Err("native predicate LUT mode");
                }
                if insn.bits(67, 1) == 0 && (lut == 0xfe || lut == 0xa8) {
                    self.emit(
                        0x5090000000000000
                            | self.predicate
                            | (u64::from(target) << 3)
                            | 7
                            | (u64::from(insn.bits(87, 4)) << 12)
                            | (1u64 << 24)
                            | (u64::from(insn.bits(77, 4)) << 29)
                            | (u64::from(insn.bits(68, 4)) << 39)
                            | (u64::from(lut == 0xfe) << 45),
                    );
                } else if insn.bits(67, 1) == 0
                    && (lut == 0x80 || (lut == 8 && insn.bits(68, 4) == 7 && insn.bits(77, 4) == 7))
                {
                    self.emit(
                        0x5090000000000000
                            | self.predicate
                            | (u64::from(target) << 3)
                            | 7
                            | (u64::from(insn.bits(87, 4) ^ if lut == 8 { 8 } else { 0 }) << 12)
                            | (u64::from(insn.bits(77, 4)) << 29)
                            | (u64::from(insn.bits(68, 4)) << 39),
                    );
                } else {
                    self.predicate_lut(
                        lut,
                        insn.bits(87, 4),
                        insn.bits(77, 4),
                        insn.bits(68, 4),
                        target,
                        insn.bits(67, 1) != 0,
                    );
                }
            }
            0x89c => {
                let target = insn.bits(81, 3);
                let lut = insn.bits(64, 3) | (insn.bits(72, 5) << 3);
                if insn.bits(84, 3) != 7
                    || insn.bits(67, 1) != 0
                    || insn.bits(16, 8) != 0
                    || insn.words[1] != 0
                {
                    return Err("native uniform predicate LUT mode");
                }
                if target == 7 {
                    return Ok(());
                }
                let slot = |operand: u32| {
                    if operand & 7 == 7 {
                        operand
                    } else {
                        (operand & 8) | (6 - (operand & 7))
                    }
                };
                self.predicate = u64::from(slot(native_predicate)) << 16;
                self.emit(0x38e8000000070000 | 254 | (255u64 << 8) | (0x7fu64 << 20));
                self.emit(0x38f0000000070000 | (239u64 << 8) | (0x7fu64 << 20));
                self.predicate_lut(
                    lut,
                    slot(insn.bits(87, 4)),
                    slot(insn.bits(77, 4)),
                    slot(insn.bits(68, 4)),
                    6 - target,
                    false,
                );
                self.emit(
                    0x38e8000000070000
                        | 239
                        | (239u64 << 8)
                        | (u64::from(1u32 << (6 - target)) << 20),
                );
                self.emit(0x38f0000000070000 | (254u64 << 8) | (0x7fu64 << 20));
            }
            0x883 => {
                let target = self.uniforms.register(insn.bits(16, 8));
                let mask = insn.words[1];
                if target == u32::MAX
                    || insn.bits(24, 8) != 63
                    || native_predicate != 7
                    || insn.words[2] != 0x08000000
                    || mask > 0x7f
                {
                    return Err("native uniform predicate move mode");
                }
                if target == 255 {
                    return Ok(());
                }
                self.mov_imm(target, 0);
                for k in 0..7u32 {
                    if (mask >> k) & 1 == 0 {
                        continue;
                    }
                    self.emit(
                        0x3800000000070000
                            | 250
                            | (239u64 << 8)
                            | (u64::from((6 - k) | (1 << 8)) << 20),
                    );
                    self.emit(
                        0x36f0000000070000
                            | u64::from(target)
                            | (250u64 << 8)
                            | (u64::from(k | (1 << 8)) << 20)
                            | (u64::from(target) << 39),
                    );
                }
            }
            0x882 => {
                let target = self.uniforms.register(self.dest as u32);
                if target == u32::MAX || insn.words[2] != 0 {
                    return Err("native uniform immediate move mode");
                }
                if target != 255 { self.mov_imm(target, insn.words[1]); }
            }
            0xc82 => {
                let source = if insn.words[1] < 64 {
                    self.uniforms.register(insn.words[1])
                } else {
                    u32::MAX
                };
                let target = self.uniforms.register(self.dest as u32);
                if target == u32::MAX || source == u32::MAX || insn.words[2] != 0x08000000 {
                    return Err("native uniform move mode");
                }
                if target != 255 { self.emit(
                    0x5c98000000000000
                        | self.predicate
                        | u64::from(target)
                        | (u64::from(source) << 20)
                        | (15u64 << 39),
                ); }
            }
            0xab9 | 0xabb => {
                let uniform_dest = insn.bits(16, 8);
                let index_reg = insn.bits(24, 8);
                let width = insn.bits(73, 3);
                let target = self.uniforms.register(uniform_dest);
                let index_target = if opcode == 0xabb {
                    self.uniforms.register(index_reg)
                } else {
                    255
                };
                if target == u32::MAX
                    || target == 255
                    || (width != 4 && width != 5)
                    || (width == 5
                        && (uniform_dest & 1 != 0
                            || self.uniforms.register(uniform_dest + 1) != target + 1))
                    || self.bank() >= 18
                    || insn.bits(38, 16) % 4 != 0
                    || insn.bits(62, 2) != 0
                    || (insn.words[2] & !0xe00) != if opcode == 0xab9 { 0 } else { 0x08000000 }
                    || index_target == u32::MAX
                {
                    return Err("native uniform constant load mode");
                }
                if opcode == 0xab9 && width == 4 {
                    self.emit(
                        0x4c98000000000000
                            | self.predicate
                            | u64::from(target)
                            | (15u64 << 39)
                            | self.cbuf(),
                    );
                } else {
                    self.emit(
                        0xef90000000000000
                            | self.predicate
                            | u64::from(target)
                            | (u64::from(index_target) << 8)
                            | (u64::from(insn.bits(38, 16)) << 20)
                            | (u64::from(self.bank()) << 36)
                            | (u64::from(width) << 48),
                    );
                }
            }
            0x899 => {
                let dst = insn.bits(16, 8);
                let low = insn.bits(24, 8);
                let high = insn.bits(64, 8);
                let dst_reg = self.uniforms.register(dst);
                let low_reg = self.uniforms.register(low);
                let high_reg = self.uniforms.register(high);
                let dst_ok = dst != 63 && dst_reg != u32::MAX;
                let low_ok = low != 63 && low_reg != u32::MAX;
                let high_ok = high != 63 && high_reg != u32::MAX;
                if dst_ok && low_reg != u32::MAX && high_reg != u32::MAX
                    && insn.words[2] == (0x08001200 | high) {
                    self.mov_imm(252, insn.words[1]);
                    self.right_shift_u64_low(dst_reg, low_reg, high_reg, 252);
                    return Ok(());
                }
                if insn.words[2] == (0x08011400 | high) && low == 63 && high_ok
                    && dst_ok && insn.words[1] <= 32 {
                    self.emit(0x3828000000000000 | self.predicate | u64::from(dst_reg)
                        | (u64::from(high_reg) << 8) | (u64::from(insn.words[1]) << 20) | (1u64 << 48));
                    return Ok(());
                }
                let right_low = insn.words[2] == 0x0800163f && low_ok;
                let right_high = insn.words[2] == (0x08011600 | high) && low == 63 && high_ok;
                if (right_low || right_high) && dst_ok && insn.words[1] <= 32 {
                    self.emit(0x3828000000000000 | self.predicate | u64::from(dst_reg)
                        | (u64::from(if right_high { high_reg } else { low_reg }) << 8)
                        | (u64::from(insn.words[1]) << 20));
                    return Ok(());
                }
                let high_result = insn.words[2] == (0x08010600 | high) && low == 63 && high_ok;
                let low_result = insn.words[2] == 0x0800063f && low_ok;
                if !dst_ok || insn.words[1] > 32 || (!high_result && !low_result) {
                    return Err("native uniform shift mode");
                }
                self.emit(0x3848000000000000 | self.predicate | u64::from(dst_reg)
                    | (u64::from(if high_result { high_reg } else { low_reg }) << 8)
                    | (u64::from(insn.words[1]) << 20));
            }
            0xb82 => {
                let width = insn.bits(73, 3);
                let mode = insn.bits(78, 2);
                if width > 5
                    || !matches!(mode, 0 | 2)
                    || insn.words[2] & !0xce00 != 0
                    || insn.words[1] & 0xf800003f != 0
                    || self.bank() >= 18
                    || (width == 5 && (self.dest & 1 != 0 || self.dest >= 254))
                {
                    return Err("native indexed constant load mode");
                }
                self.emit(
                    self.base(0xef90000000000000)
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(insn.bits(38, 16)) << 20)
                        | (u64::from(self.bank()) << 36)
                        | (u64::from(mode) << 44)
                        | (u64::from(width) << 48),
                );
            }
            0xf89 => {
                let lane = insn.bits(53, 5);
                let mask = insn.bits(40, 13);
                let mode = insn.bits(58, 2);
                if insn.words[2] != 0x000e0000 || insn.bits(32, 8) != 0 || insn.bits(60, 4) != 0 {
                    return Err("native immediate shuffle mode");
                }
                self.emit(
                    self.base(0xef10000000000000)
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(lane) << 20)
                        | (1u64 << 28)
                        | (1u64 << 29)
                        | (u64::from(mode) << 30)
                        | (u64::from(mask) << 34)
                        | (7u64 << 48),
                );
            }
            0x822 => {
                let swizzle = insn.words[1];
                if swizzle > 0xff || insn.words[2] > 255 {
                    return Err("native swizzled add mode");
                }
                let maxwell = ((swizzle & 0x55) << 1) | ((swizzle & 0xaa) >> 1);
                self.emit(
                    self.base(0x50f8000000000000)
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(insn.bits(64, 8)) << 20)
                        | (u64::from(maxwell) << 28),
                );
            }
            _ => unreachable!("caller selected an opcode outside the vote/uniform switch arms"),
        }
        Ok(())
    }

    // Contiguous IMAD/UIMAD arms immediately following HMNMX2 upstream.
    fn lower_integer_multiply(
        &mut self,
        opcode: u32,
        native_predicate: u32,
        rewritten_uniform: bool,
    ) -> Result<(), &'static str> {
        let insn = self.insn;
        match opcode {
            0xc24 | 0xe24 => {
                let negate_c = opcode == 0xe24 && insn.bits(63, 1) != 0;
                if (insn.words[2] & !0x080002ff) != 0x078e0000
                    || (insn.words[1] & !(0xff | if negate_c { 0x80000000 } else { 0 })) != 0
                {
                    return Err("native uniform-addend multiply mode");
                }
                let a = self.operand(0, false);
                let b = self.operand(1, false);
                let mut c = self.operand(2, false);
                if negate_c {
                    self.emit(
                        0x5c10000000000000
                            | self.predicate
                            | 252
                            | (255u64 << 8)
                            | (u64::from(c) << 20)
                            | (1u64 << 48),
                    );
                    c = 252;
                }
                self.multiply_low(a, b, c, false);
            }
            0x224 => {
                if insn.bits(24, 8) == 255
                    && insn.words[1] == 255
                    && insn.bits(75, 1) != 0
                    && (insn.words[2] & !0xaff) == 0x078e0000
                {
                    self.emit(
                        self.base(0x5c10000000000000)
                            | (255u64 << 8)
                            | (u64::from(insn.bits(64, 8)) << 20)
                            | (1u64 << 48),
                    );
                } else if insn.bits(24, 8) == 255
                    && insn.words[1] == 255
                    && (insn.words[2] & !0xff) == 0x078e0000
                {
                    self.emit(
                        self.base(0x5c98000000000000)
                            | (15u64 << 39)
                            | (u64::from(insn.bits(64, 8)) << 20),
                    );
                } else if insn.bits(24, 8) == 255 && insn.words[1] == 255
                    && insn.words[2] & !0x07800aff == 0x000e0400
                    && (!rewritten_uniform || insn.bits(87, 3) == 7) {
                    let mut c = insn.bits(64, 8);
                    self.mov_imm(250, 1);
                    self.emit(0x5ca0000000000000 | self.predicate | 250 | (250u64 << 8)
                        | (255u64 << 20) | (u64::from(insn.bits(87, 3)) << 39)
                        | (u64::from(insn.bits(90, 1)) << 42));
                    if insn.bits(75, 1) != 0 {
                        self.emit(0x0400000000000000 | self.predicate | 251 | (u64::from(c) << 8)
                            | (0xffffffffu64 << 20) | (2u64 << 53));
                        c = 251;
                    }
                    self.emit(self.base(0x5c10000000000000) | (u64::from(c) << 8) | (250u64 << 20));
                } else {
                    if (insn.words[2] & !0x2ff) != 0x078e0000 || (insn.words[1] & !0xff) != 0 {
                        return Err("native integer register multiply mode");
                    }
                    self.multiply_low(insn.bits(24, 8), insn.bits(32, 8), insn.bits(64, 8), false);
                }
            }
            0x2a5 => {
                let a = self.uniforms.register(insn.bits(24, 8));
                let b = self.uniforms.register(insn.bits(32, 8));
                let c = insn.bits(64, 8);
                let c_low = self.uniforms.register(c);
                let c_high = if c == 63 {
                    255
                } else {
                    self.uniforms.register(c + 1)
                };
                let d_low = self.uniforms.register(insn.bits(16, 8));
                let d_high = self.uniforms.register(insn.bits(16, 8) + 1);
                if native_predicate != 7
                    || (insn.words[1] & !0xff) != 0
                    || (insn.words[2] & !0xff) != 0x0f8e0000
                    || insn.bits(16, 8) & 1 != 0
                    || (c != 63 && c & 1 != 0)
                    || [a, b, c_low, c_high, d_low, d_high].contains(&u32::MAX)
                    || d_low == 255
                {
                    return Err("native uniform wide multiply mode");
                }
                self.xmad(250, a, b, 255, false, false, 0, false);
                self.xmad(251, a, b, 250, true, false, 2, false);
                self.xmad(250, a, b, 251, false, true, 1, false);
                self.xmad(252, a, b, 251, true, true, 2, false);
                self.emit(0x3828000000070000 | 250 | (250u64 << 8) | (16u64 << 20));
                self.emit(0x5c10000000070000 | 253 | (250u64 << 8) | (252u64 << 20));
                self.xmad(250, a, b, 255, false, false, 0, false);
                self.xmad(251, a, b, 255, true, false, 0, false);
                self.xmad(251, a, b, 251, false, true, 0, false);
                self.emit(
                    0x5c18000000070000 | 250 | (251u64 << 8) | (250u64 << 20) | (16u64 << 39),
                );
                self.emit(
                    0x5c10000000070000
                        | 250
                        | (250u64 << 8)
                        | (u64::from(c_low) << 20)
                        | (1u64 << 47),
                );
                self.emit(
                    0x5c10000000070000
                        | u64::from(d_high)
                        | (253u64 << 8)
                        | (u64::from(c_high) << 20)
                        | (1u64 << 43),
                );
                self.emit(0x5c10000000070000 | u64::from(d_low) | (250u64 << 8) | (255u64 << 20));
            }
            0x225 | 0x227 | 0x625 | 0x825 | 0x827 | 0xa27 => {
                // Fritchii: IMAD.HI / IMAD.WIDE, including signed product and carry output.
                let wide = matches!(opcode, 0x225 | 0x625 | 0x825);
                let immediate = matches!(opcode, 0x825 | 0x827);
                let signed_product = insn.bits(73, 1) != 0;
                let c = if opcode == 0x625 { 255 } else { insn.bits(64, 8) };
                let carry_output = insn.bits(81, 3);
                if (insn.words[2] & !0x000e02ff) != 0x07800000
                    || (matches!(opcode, 0x225 | 0x227) && (insn.words[1] & !0xff) != 0)
                    || (matches!(opcode, 0xa27 | 0x625) && (!self.valid_cbuf() || insn.bits(62, 2) != 0))
                    || (opcode == 0x625 && insn.bits(38, 16) > 0xfff8)
                    || (c != 255 && c & 1 != 0)
                    || (wide && (self.dest == 255 || self.dest & 1 != 0))
                    || (signed_product && carry_output != 7) {
                    return Err("native high multiply mode");
                }
                let a = insn.bits(24, 8);
                let b = if immediate { insn.words[1] } else if opcode == 0xa27 { 254 }
                    else { insn.bits(if opcode == 0x625 { 64 } else { 32 }, 8) };
                if opcode == 0xa27 {
                    self.emit(0x4c98000000000000 | self.predicate | 254 | (15u64 << 39) | self.cbuf());
                }
                if signed_product {
                    let b_register = if immediate { 254 } else { b };
                    if immediate { self.mov_imm(254, b); }
                    self.emit(0x3828000000000000 | self.predicate | 253 | (u64::from(a) << 8) | (31u64 << 20) | (1u64 << 48));
                    self.emit(0x5c40000000000000 | self.predicate | 253 | (253u64 << 8) | (u64::from(b_register) << 20) | (7u64 << 48));
                    self.emit(0x3828000000000000 | self.predicate | 252 | (u64::from(b_register) << 8) | (31u64 << 20) | (1u64 << 48));
                    self.emit(0x5c40000000000000 | self.predicate | 252 | (252u64 << 8) | (u64::from(a) << 20) | (7u64 << 48));
                    self.emit(0x5c10000000000000 | self.predicate | 253 | (253u64 << 8) | (252u64 << 20));
                }
                self.xmad(250, a, b, 255, false, false, 0, immediate);
                self.xmad(251, a, b, 250, true, false, 2, immediate);
                self.xmad(250, a, b, 251, false, true, 1, immediate);
                self.xmad(252, a, b, 251, true, true, 2, immediate);
                self.emit(0x3828000000000000 | self.predicate | 250 | (250u64 << 8) | (16u64 << 20));
                if signed_product {
                    self.emit(0x5c10000000000000 | self.predicate | 250 | (250u64 << 8) | (253u64 << 20) | (1u64 << 48));
                }
                if !wide && c == 255 && carry_output == 7 {
                    self.emit(self.base(0x5c10000000000000) | (250u64 << 8) | (252u64 << 20));
                    return Ok(());
                }
                self.emit(0x5c10000000000000 | self.predicate | 253 | (250u64 << 8) | (252u64 << 20));
                self.xmad(250, a, b, 255, false, false, 0, immediate);
                self.xmad(251, a, b, 255, true, false, 0, immediate);
                self.xmad(251, a, b, 251, false, true, 0, immediate);
                self.emit(0x5c18000000000000 | self.predicate | 250 | (251u64 << 8) | (250u64 << 20) | (16u64 << 39));
                let mut c_low = c;
                let mut c_high = if c == 255 { 255 } else { c + 1 };
                if opcode == 0x625 {
                    self.emit(0x4c98000000000000 | self.predicate | 252 | (15u64 << 39) | self.cbuf());
                    self.emit(0x4c98000000000000 | self.predicate | 254 | (15u64 << 39) | (self.cbuf() + (1u64 << 20)));
                    c_low = 252;
                    c_high = 254;
                }
                let high = if wide { self.dest + 1 } else { self.dest };
                self.emit(0x5c10000000000000 | self.predicate | 250 | (250u64 << 8) | (u64::from(c_low) << 20) | (1u64 << 47));
                if carry_output == 7 {
                    self.emit(0x5c10000000000000 | self.predicate | high | (253u64 << 8) | (u64::from(c_high) << 20) | (1u64 << 43));
                    if wide { self.emit(self.base(0x5c10000000000000) | (250u64 << 8) | (255u64 << 20)); }
                    return Ok(());
                }
                self.emit(0x5c10000000000000 | self.predicate | 251 | (255u64 << 8) | (255u64 << 20) | (1u64 << 43));
                self.emit(0x5c10000000000000 | self.predicate | 253 | (253u64 << 8) | (u64::from(c_high) << 20) | (1u64 << 47));
                self.emit(0x5c10000000000000 | self.predicate | 254 | (255u64 << 8) | (255u64 << 20) | (1u64 << 43));
                self.emit(0x5c10000000000000 | self.predicate | 253 | (253u64 << 8) | (251u64 << 20) | (1u64 << 47));
                self.emit(0x5c10000000000000 | self.predicate | 254 | (254u64 << 8) | (255u64 << 20) | (1u64 << 43));
                self.emit(0x5c98000000000000 | self.predicate | high | (253u64 << 20) | (15u64 << 39));
                if wide { self.emit(self.base(0x5c98000000000000) | (250u64 << 20) | (15u64 << 39)); }
                self.emit(0x5b60000000000000 | self.predicate | 7 | (u64::from(carry_output) << 3) | (254u64 << 8)
                    | (255u64 << 20) | (7u64 << 39) | (5u64 << 49));
            }
            0xa24 => {
                if (insn.words[2] & !0x2ff) != 0x078e0000
                    || !self.valid_cbuf()
                    || insn.bits(62, 2) != 0
                {
                    return Err("native integer constant-buffer multiply mode");
                }
                self.emit(0x4c98000000000000 | self.predicate | 252 | (15u64 << 39) | self.cbuf());
                self.multiply_low(insn.bits(24, 8), 252, insn.bits(64, 8), false);
            }
            0x424 | 0x624 => {
                if opcode == 0x624
                    && insn.bits(24, 8) == 255
                    && (insn.words[2] & !0x07800000) == 0x000e06ff
                {
                    let source = self.operand(2, false);
                    self.mov_imm(250, 1);
                    self.emit(
                        0x5ca0000000000000
                            | self.predicate
                            | 250
                            | (250u64 << 8)
                            | (255u64 << 20)
                            | (u64::from(insn.bits(87, 3)) << 39)
                            | (u64::from(insn.bits(90, 1)) << 42),
                    );
                    self.emit(
                        self.base(0x5c10000000000000) | (u64::from(source) << 8) | (250u64 << 20),
                    );
                    return Ok(());
                }
                if insn.bits(24, 8) != 255 || insn.bits(64, 8) != 255 || insn.words[2] != 0x078e00ff
                {
                    if (insn.words[2] & !0x2ff) != 0x078e0000
                        || (opcode == 0x624 && insn.bits(62, 2) != 0)
                    {
                        return Err("native multiply wide addend mode");
                    }
                    let a = self.operand(0, false);
                    let b = self.operand(1, false);
                    let c = self.operand(2, false);
                    self.multiply_low(a, b, c, false);
                    return Ok(());
                }
                // Retain the duplicate upstream validation for direct comparison.
                if insn.bits(24, 8) != 255 || insn.bits(64, 8) != 255 || insn.words[2] != 0x078e00ff
                {
                    return Err("native integer move mode");
                }
                if opcode == 0x624 {
                    if !self.valid_cbuf() || insn.bits(62, 2) != 0 {
                        return Err("native integer move constant buffer");
                    }
                    self.emit(self.base(0x4c98000000000000) | (15u64 << 39) | self.cbuf());
                } else {
                    self.emit(
                        self.base(0x0100000000000000)
                            | (15u64 << 12)
                            | (u64::from(insn.words[1]) << 20),
                    );
                }
            }
            0x824 => {
                let negate_c = insn.bits(75, 1) != 0;
                if insn.words[1] == 1 && (insn.words[2] & 0xfffff700) == 0x078e0200 {
                    self.emit(
                        self.base(0x5c10000000000000)
                            | (u64::from(insn.bits(24, 8)) << 8)
                            | (u64::from(insn.bits(64, 8)) << 20)
                            | (u64::from(negate_c) << 48),
                    );
                    return Ok(());
                }
                let multiplier = insn.words[1];
                if insn.bits(64, 8) == 255
                    && !negate_c
                    && multiplier > 1
                    && multiplier & (multiplier - 1) == 0
                    && (insn.words[2] & !0x2ff) == 0x078e0000
                {
                    let mut shift: u32 = 0;
                    while (1u32 << shift) != multiplier {
                        shift += 1;
                    }
                    self.emit(
                        self.base(0x3848000000000000)
                            | (u64::from(insn.bits(24, 8)) << 8)
                            | (u64::from(shift) << 20),
                    );
                    return Ok(());
                }
                if insn.words[1] != 48 || insn.words[2] != 0x078e02ff {
                    if (insn.words[2] & !0xaff) != 0x078e0000 {
                        return Err("native integer multiply mode");
                    }
                    let mut c = insn.bits(64, 8);
                    if negate_c && c != 255 {
                        self.emit(
                            0x5c10000000000000
                                | self.predicate
                                | 252
                                | (255u64 << 8)
                                | (u64::from(c) << 20)
                                | (1u64 << 48),
                        );
                        c = 252;
                    }
                    self.multiply_low(insn.bits(24, 8), insn.words[1], c, true);
                    return Ok(());
                }
                let source = u64::from(insn.bits(24, 8));
                self.emit(
                    0x5c18000000000000
                        | self.predicate
                        | 250
                        | (source << 8)
                        | (source << 20)
                        | (1u64 << 39),
                );
                self.emit(self.base(0x3848000000000000) | (250u64 << 8) | (4u64 << 20));
            }
            _ => unreachable!("caller selected an opcode outside the integer multiply switch arms"),
        }
        Ok(())
    }

    // Mechanical extraction of the contiguous F2F/F2FP/HMUL2/HADD2/HFMA2/HMNMX2
    // switch arms. The caller must select one of these opcodes; this is not a
    // partial LowerToMaxwell entry point or a fallback for unported instructions.
    fn lower_float_and_half(
        &mut self,
        opcode: u32,
        index: u32,
        native_predicate: u32,
        relocations: &mut Vec<Relocation>,
    ) -> Result<(), &'static str> {
        let insn = self.insn;
        match opcode {
            0x304 => {
                if insn.words[2] != 0x00200800
                    || insn.bits(24, 8) != 0
                    || (insn.words[1] & !0xc00000ff) != 0
                {
                    return Err("native scalar float conversion mode");
                }
                self.emit(
                    self.base(0x5ca8000000000000)
                        | (1u64 << 8)
                        | (2u64 << 10)
                        | (u64::from(insn.bits(32, 8)) << 20)
                        | (u64::from(insn.bits(62, 1)) << 49)
                        | (u64::from(insn.bits(63, 1)) << 45),
                );
            }
            0x23e | 0xa3e => {
                if insn.bits(62, 2) != 0
                    || insn.bits(73, 5) != 0
                    || insn.bits(79, 2) != 0
                    || insn.bits(81, 10) != 0
                    || insn.bits(91, 1) != 0
                {
                    return Err("native float packing modifiers");
                }
                let merge = insn.bits(78, 1) != 0;
                if insn.bits(72, 1) != 0 {
                    return Err("native float packing selector");
                }
                let a = self.operand(if merge { 2 } else { 0 }, false);
                let b = self.operand(1, false);
                if merge {
                    self.emit(
                        0x5c98000000000000
                            | self.predicate
                            | 250
                            | (u64::from(a) << 20)
                            | (15u64 << 39),
                    );
                } else {
                    self.emit(
                        0x5ca8000000000000
                            | self.predicate
                            | 250
                            | (1u64 << 8)
                            | (2u64 << 10)
                            | (u64::from(a) << 20),
                    );
                }
                self.emit(
                    0x5ca8000000000000
                        | self.predicate
                        | 251
                        | (1u64 << 8)
                        | (2u64 << 10)
                        | (u64::from(b) << 20),
                );
                self.emit(
                    self.base(0x5c18000000000000) | (250u64 << 8) | (251u64 << 20) | (16u64 << 39),
                );
            }
            0x232 | 0x832 | 0xa32 => {
                if insn.bits(78, 2) != 0 || insn.bits(81, 10) != 0 || insn.bits(91, 1) != 0 {
                    return Err("native half multiply mode");
                }
                let a = self.half_source(0);
                let b = self.half_source(1);
                let precision = if insn.bits(76, 1) != 0 {
                    2
                } else {
                    insn.bits(80, 1)
                };
                self.emit(
                    self.base(0x5d08000000000000)
                        | (u64::from(a.reg) << 8)
                        | (u64::from(b.reg) << 20)
                        | (u64::from(b.swizzle) << 28)
                        | (u64::from(b.absolute) << 30)
                        | (u64::from(a.negate != b.negate) << 31)
                        | (u64::from(insn.bits(77, 1)) << 32)
                        | (u64::from(precision) << 39)
                        | (u64::from(a.absolute) << 44)
                        | (u64::from(a.swizzle) << 47),
                );
            }
            0x230 | 0x430 | 0x630 => {
                if insn.bits(76, 1) != 0
                    || insn.bits(79, 1) != 0
                    || insn.bits(81, 10) != 0
                    || insn.bits(91, 1) != 0
                {
                    return Err("native half add mode");
                }
                let a = self.half_source(0);
                let b = self.half_source(if opcode == 0x230 { 1 } else { 2 });
                if insn.bits(78, 1) != 0 {
                    if a.reg != 255
                        || !a.negate
                        || a.absolute
                        || a.swizzle != 0
                        || b.swizzle < 2
                        || insn.bits(77, 1) != 0
                        || insn.bits(80, 1) != 0
                    {
                        return Err("native float-output half add mode");
                    }
                    self.extract_half(b, 0, self.dest as u32);
                } else {
                    self.emit(
                        self.base(0x5d10000000000000)
                            | (u64::from(a.reg) << 8)
                            | (u64::from(b.reg) << 20)
                            | (u64::from(b.swizzle) << 28)
                            | (u64::from(b.absolute) << 30)
                            | (u64::from(b.negate) << 31)
                            | (u64::from(insn.bits(77, 1)) << 32)
                            | (u64::from(insn.bits(80, 1)) << 39)
                            | (u64::from(a.negate) << 43)
                            | (u64::from(a.absolute) << 44)
                            | (u64::from(a.swizzle) << 47),
                    );
                }
            }
            0x231 | 0x431 | 0x631 | 0x831 | 0xa31 => {
                let relu = insn.bits(79, 1) != 0;
                if insn.bits(78, 1) != 0
                    || insn.bits(85, 1) != 0
                    || insn.bits(91, 1) != 0
                    || if relu {
                        insn.bits(87, 4) != 7 || insn.bits(77, 1) != 0
                    } else {
                        insn.bits(87, 4) != 0
                    }
                {
                    return Err("native half fused multiply-add mode");
                }
                if relu && native_predicate != 7 {
                    relocations.push(Relocation {
                        call: false,
                        output_target: None,
                        output_index: self.instructions.len() as u32,
                        target: i64::from(index + 1),
                    });
                    self.emit(0xe24000000000000f | (u64::from(native_predicate ^ 8) << 16));
                    self.predicate = 7u64 << 16;
                }
                let mut a = self.half_source(0);
                let mut b = self.half_source(1);
                let c = self.half_source(2);
                if insn.bits(86, 1) != 0 {
                    if b.swizzle != 0 || b.absolute || b.reg >= 250 {
                        return Err("native half mixed-sign source mode");
                    }
                    self.emit(
                        0x0400000000000000
                            | self.predicate
                            | 251
                            | (u64::from(b.reg) << 8)
                            | (0x80000000u64 << 20)
                            | (2u64 << 53),
                    );
                    b.reg = 251;
                }
                if b.absolute || c.absolute || (a.absolute && a.reg >= 248) {
                    return Err("native half fused multiply-add absolute source");
                }
                if a.absolute {
                    self.emit(
                        0x0400000000000000
                            | self.predicate
                            | 250
                            | (u64::from(a.reg) << 8)
                            | ((if a.swizzle == 1 {
                                0x7fffffffu64
                            } else {
                                0x7fff7fffu64
                            }) << 20),
                    );
                    a.reg = 250;
                    a.absolute = false;
                }
                let precision = if insn.bits(76, 1) != 0 {
                    2
                } else {
                    insn.bits(80, 1)
                };
                self.emit(
                    0x5d00000000000000
                        | self.predicate
                        | (if relu { 254 } else { self.dest })
                        | (u64::from(a.reg) << 8)
                        | (u64::from(b.reg) << 20)
                        | (u64::from(b.swizzle) << 28)
                        | (u64::from(c.negate) << 30)
                        | (u64::from(a.negate != b.negate) << 31)
                        | (u64::from(insn.bits(77, 1)) << 32)
                        | (u64::from(c.swizzle) << 35)
                        | (u64::from(precision) << 37)
                        | (u64::from(c.reg) << 39)
                        | (u64::from(a.swizzle) << 47),
                );
                if relu {
                    // Clamp rounded binary16 lanes; retain +infinity and canonicalize NaNs.
                    self.save_predicate(1 << 6);
                    for lane in 0..2u32 {
                        self.emit(
                            0x3828000000070000 | 251 | (254u64 << 8) | (u64::from(lane * 16) << 20),
                        );
                        self.emit(0x0400000000070000 | 251 | (251u64 << 8) | (0xffffu64 << 20));
                        self.emit(0x0400000000070000 | 252 | (251u64 << 8) | (0x7fffu64 << 20));
                        self.mov_imm(250, 0x7c00);
                        self.emit(
                            0x5b60000000070000
                                | 7
                                | (6u64 << 3)
                                | (250u64 << 8)
                                | (252u64 << 20)
                                | (7u64 << 39)
                                | (1u64 << 49),
                        );
                        self.emit(0x3828000000070000 | 252 | (251u64 << 8) | (15u64 << 20));
                        self.mov_imm(250, 1);
                        self.emit(
                            0x5c10000000070000
                                | 252
                                | (252u64 << 8)
                                | (250u64 << 20)
                                | (1u64 << 48),
                        );
                        self.emit(
                            0x5c40000000070000
                                | 251
                                | (251u64 << 8)
                                | (252u64 << 20)
                                | (7u64 << 48),
                        );
                        self.mov_imm(250, 0x7fff);
                        self.emit(
                            0x5ca0000000070000
                                | 251
                                | (250u64 << 8)
                                | (251u64 << 20)
                                | (6u64 << 39),
                        );
                        if lane == 0 {
                            self.emit(
                                self.base(0x5c98000000000000) | (251u64 << 20) | (15u64 << 39),
                            );
                        } else {
                            self.emit(0x3848000000070000 | 251 | (251u64 << 8) | (16u64 << 20));
                            self.emit(
                                self.base(0x5c40000000000000)
                                    | (self.dest << 8)
                                    | (251u64 << 20)
                                    | (1u64 << 41)
                                    | (7u64 << 48),
                            );
                        }
                    }
                    self.restore_predicate(1 << 6);
                }
            }
            0x240 | 0x840 => {
                if insn.bits(76, 11) != 0 || insn.bits(91, 1) != 0 {
                    return Err("native half min/max mode");
                }
                let a = self.half_source(0);
                let b = if opcode == 0x840 {
                    HalfSource {
                        reg: 255,
                        swizzle: 0,
                        absolute: false,
                        negate: false,
                    }
                } else {
                    self.half_source(1)
                };
                if a.swizzle == 1 || b.swizzle == 1 || (b.reg >= 250 && b.reg != 255) {
                    return Err("native half min/max source mode");
                }
                for lane in 0..2u32 {
                    self.extract_half(a, lane, 250);
                    if opcode == 0x840 {
                        let half = (insn.words[1] >> (lane * 16)) & 0xffff;
                        self.mov_imm(251, half);
                        self.extract_half(
                            HalfSource {
                                reg: 251,
                                swizzle: 2,
                                absolute: false,
                                negate: false,
                            },
                            0,
                            251,
                        );
                    } else {
                        self.extract_half(b, lane, 251);
                    }
                    let target: u32 = if lane == 0 { 252 } else { 250 };
                    self.emit(
                        0x5c60000000000000
                            | self.predicate
                            | u64::from(target)
                            | (250u64 << 8)
                            | (251u64 << 20)
                            | (u64::from(insn.bits(87, 3)) << 39)
                            | (u64::from(insn.bits(90, 1)) << 42),
                    );
                    self.emit(
                        0x5ca8000000000000
                            | self.predicate
                            | u64::from(target)
                            | (1u64 << 8)
                            | (2u64 << 10)
                            | (u64::from(target) << 20),
                    );
                }
                self.emit(
                    self.base(0x5c18000000000000) | (250u64 << 8) | (252u64 << 20) | (16u64 << 39),
                );
            }
            _ => unreachable!("caller selected an opcode outside the floating/half switch arms"),
        }
        Ok(())
    }

    fn base(&self, opcode_bits: u64) -> u64 {
        opcode_bits | self.predicate | self.dest
    }

    fn bank(&self) -> u32 {
        let value = self.insn.bits(54, 5);
        if value == 24 {
            17
        } else {
            value
        }
    }

    fn cbuf(&self) -> u64 {
        (u64::from(self.insn.bits(38, 16) / 4) << 20) | (u64::from(self.bank()) << 34)
    }

    fn valid_cbuf(&self) -> bool {
        self.insn.bits(91, 1) == 0 && self.bank() < 18 && self.insn.bits(38, 16) % 4 == 0
    }

    fn emit(&mut self, word: u64) {
        self.instructions.push(word);
    }

    // The C++ default mask is 1 << 6; callers pass it explicitly in Rust.
    fn save_predicate(&mut self, mask: u32) {
        self.emit(0x38e8000000070000 | 253 | (255u64 << 8) | (u64::from(mask) << 20));
    }

    fn restore_predicate(&mut self, mask: u32) {
        self.emit(0x38f0000000070000 | (253u64 << 8) | (u64::from(mask) << 20));
    }

    fn predicate_lut(
        &mut self,
        mut lut: u32,
        a: u32,
        b: u32,
        c: u32,
        target: u32,
        uniform_c: bool,
    ) {
        for (i, input) in [a, b, c].into_iter().enumerate() {
            if i == 2 && uniform_c {
                if c & 7 == 7 {
                    self.emit(
                        0x0100000000000000
                            | self.predicate
                            | 252
                            | (15u64 << 12)
                            | (u64::from(u32::MAX) << 20),
                    );
                } else {
                    self.emit(
                        0x3800000000000000
                            | self.predicate
                            | 252
                            | (239u64 << 8)
                            | (u64::from((6 - (c & 7)) | (1 << 8)) << 20),
                    );
                    self.emit(
                        0x5c10000000000000
                            | self.predicate
                            | 252
                            | (255u64 << 8)
                            | (252u64 << 20)
                            | (1u64 << 48),
                    );
                }
                if c & 8 != 0 {
                    lut = ((lut & 0x55) << 1) | ((lut & 0xaa) >> 1);
                }
                continue;
            }
            self.emit(
                0x38a0000000000000
                    | self.predicate
                    | (250 + i as u64)
                    | (255u64 << 8)
                    | (0x7ffffu64 << 20)
                    | (1u64 << 56)
                    | (u64::from(input ^ 8) << 39),
            );
        }
        self.emit(
            0x5be0000000000000
                | self.predicate
                | 255
                | (250u64 << 8)
                | (251u64 << 20)
                | (u64::from(lut) << 28)
                | (3u64 << 36)
                | (252u64 << 39)
                | (u64::from(target) << 48),
        );
    }

    fn mov_imm(&mut self, reg: u32, value: u32) {
        self.emit(
            0x0100000000000000
                | self.predicate
                | u64::from(reg)
                | (15u64 << 12)
                | (u64::from(value) << 20),
        );
    }

    fn materialize(
        &mut self,
        kind: u32,
        mut reg: u32,
        scratch: u32,
        absolute: bool,
        negate: bool,
    ) -> u32 {
        if kind == 3 {
            let mapped = self.uniforms.register(reg);
            if mapped == u32::MAX {
                self.operand_error = "native uniform source register";
                return 255;
            }
            reg = mapped;
        } else if kind == 1 {
            self.mov_imm(scratch, self.insn.words[1]);
            reg = scratch;
        } else if kind == 2 {
            if !self.valid_cbuf() {
                self.operand_error = "native constant buffer address";
                return 255;
            }
            self.emit(
                0x4c98000000000000
                    | self.predicate
                    | u64::from(scratch)
                    | (15u64 << 39)
                    | self.cbuf(),
            );
            reg = scratch;
        }
        if absolute || negate {
            if reg != scratch {
                self.emit(
                    0x5c98000000000000
                        | self.predicate
                        | u64::from(scratch)
                        | (u64::from(reg) << 20)
                        | (15u64 << 39),
                );
            }
            if absolute {
                self.emit(
                    0x0400000000000000
                        | self.predicate
                        | u64::from(scratch)
                        | (u64::from(scratch) << 8)
                        | (0x7fffffffu64 << 20),
                );
            }
            if negate {
                self.emit(
                    0x0400000000000000
                        | self.predicate
                        | u64::from(scratch)
                        | (u64::from(scratch) << 8)
                        | (0x80000000u64 << 20)
                        | (2u64 << 53),
                );
            }
            reg = scratch;
        }
        reg
    }

    fn operand(&mut self, which: u32, floating: bool) -> u32 {
        let form = self.insn.bits(9, 3);
        let (mut reg, mut kind, mut abs, mut neg) = (0, 0, 0, 0);
        if which == 0 {
            reg = self.insn.bits(24, 8);
            abs = self.insn.bits(73, 1);
            neg = self.insn.bits(72, 1);
        } else {
            if !(1..=7).contains(&form) {
                self.operand_error = "native ALU source form";
                return 255;
            }
            let wide = if which == 1 {
                matches!(form, 4 | 5 | 6)
            } else {
                matches!(form, 2 | 3 | 7)
            };
            if wide {
                kind = if form == 6 || form == 7 {
                    3
                } else if form == 4 || form == 2 {
                    1
                } else {
                    2
                };
                if kind == 3 {
                    reg = self.insn.bits(32, 8);
                }
                if kind == 2 || kind == 3 {
                    abs = self.insn.bits(62, 1);
                    neg = self.insn.bits(63, 1);
                }
            } else {
                let upper = if which == 1 {
                    matches!(form, 2 | 3 | 7)
                } else {
                    true
                };
                reg = self.insn.bits(if upper { 64 } else { 32 }, 8);
                abs = self.insn.bits(if upper { 74 } else { 62 }, 1);
                neg = self.insn.bits(if upper { 75 } else { 63 }, 1);
            }
        }
        self.materialize(
            kind,
            reg,
            250 + which,
            floating && abs != 0,
            floating && neg != 0,
        )
    }

    fn half_source(&mut self, which: u32) -> HalfSource {
        if which == 0 {
            return HalfSource {
                reg: self.insn.bits(24, 8),
                swizzle: self.insn.bits(74, 2),
                absolute: self.insn.bits(73, 1) != 0,
                negate: self.insn.bits(72, 1) != 0,
            };
        }
        let form = self.insn.bits(9, 3);
        let wide = if which == 1 {
            form == 4 || form == 5
        } else {
            form == 2 || form == 3
        };
        if wide {
            let immediate = form == 2 || form == 4;
            let reg = self.materialize(
                if immediate { 1 } else { 2 },
                255,
                250 + which,
                false,
                false,
            );
            return HalfSource {
                reg,
                swizzle: if immediate { 0 } else { self.insn.bits(60, 2) },
                absolute: !immediate && self.insn.bits(62, 1) != 0,
                negate: !immediate && self.insn.bits(63, 1) != 0,
            };
        }
        let upper = which == 2 || form == 2 || form == 3;
        HalfSource {
            reg: self.insn.bits(if upper { 64 } else { 32 }, 8),
            swizzle: self.insn.bits(if upper { 81 } else { 60 }, 2),
            absolute: self.insn.bits(if upper { 83 } else { 62 }, 1) != 0,
            negate: self.insn.bits(if upper { 84 } else { 63 }, 1) != 0,
        }
    }

    // Fritchii right_shift_u64_low lambda, kept beside the operand helpers.
    fn right_shift_u64_low(&mut self, target: u32, low: u32, high: u32, shift: u32) {
        self.mov_imm(250, 32);
        self.emit(0x5c10000000000000 | self.predicate | 251 | (250u64 << 8)
            | (u64::from(shift) << 20) | (1u64 << 48));
        self.emit(0x5c28000000000000 | self.predicate | 250 | (u64::from(low) << 8) | (u64::from(shift) << 20));
        self.emit(0x5c48000000000000 | self.predicate | 251 | (u64::from(high) << 8) | (251u64 << 20));
        self.emit(0x5c40000000000000 | self.predicate | 250 | (250u64 << 8)
            | (251u64 << 20) | (1u64 << 41) | (7u64 << 48));
        self.emit(0x1c00000000000000 | self.predicate | 251 | (u64::from(shift) << 8) | (0xffffffe0u64 << 20));
        self.emit(0x5c28000000000000 | self.predicate | 251 | (u64::from(high) << 8) | (251u64 << 20));
        self.emit(0x5c40000000000000 | self.predicate | u64::from(target) | (250u64 << 8)
            | (251u64 << 20) | (1u64 << 41) | (7u64 << 48));
    }

    fn extract_half(&mut self, source: HalfSource, lane: u32, target: u32) {
        let high = source.swizzle == 3 || (source.swizzle == 0 && lane == 1);
        self.emit(
            0x5ca8000000000000
                | self.predicate
                | u64::from(target)
                | (2u64 << 8)
                | ((if source.swizzle == 1 { 2u64 } else { 1u64 }) << 10)
                | (u64::from(source.reg) << 20)
                | (u64::from(high) << 41)
                | (u64::from(source.negate) << 45)
                | (u64::from(source.absolute) << 49),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn xmad(
        &mut self,
        target: u32,
        a: u32,
        b: u32,
        c: u32,
        a_high: bool,
        b_high: bool,
        mode: u32,
        immediate: bool,
    ) {
        self.emit(
            (if immediate {
                0x3600000000000000
            } else {
                0x5b00000000000000
            }) | self.predicate
                | u64::from(target)
                | (u64::from(a) << 8)
                | (u64::from(if immediate {
                    (b >> if b_high { 16 } else { 0 }) & 0xffff
                } else {
                    b
                }) << 20)
                | (u64::from(!immediate && b_high) << 35)
                | (u64::from(c) << 39)
                | (u64::from(mode) << 50)
                | (u64::from(a_high) << 53),
        );
    }

    fn multiply_low(&mut self, a: u32, b: u32, c: u32, immediate: bool) {
        self.xmad(250, a, b, c, false, false, 0, immediate);
        self.xmad(251, a, b, 255, true, false, 0, immediate);
        self.xmad(251, a, b, 251, false, true, 0, immediate);
        self.emit(self.base(0x5c18000000000000) | (251u64 << 8) | (250u64 << 20) | (16u64 << 39));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_gather_preserves_xy_comparison_and_split_destinations() {
        use crate::frontend::translate::TranslatorVisitor;
        use crate::ir::{basic_block::Block, opcodes::Opcode, program::Program, types::{ShaderStage, TextureInstInfo}, value::Value};
        use crate::ir_opt::{constant_propagation_pass::constant_propagation_pass, ssa_rewrite_pass::ssa_rewrite_pass};
        fn resolve(program: &Program, mut value: Value) -> Value {
            while let Value::Inst(reference) = value {
                let inst = program.blocks[reference.block as usize].instructions[reference.inst as usize].as_ref().unwrap();
                if inst.opcode != Opcode::Identity { break; }
                value = inst.args[0];
            }
            value
        }
        let exit = Instruction { words: [0x794d, 0, 0x03800000, 0] };
        // Destinations alias the input coordinate pair and comparison value.
        let sample = Instruction { words: [0x18187b63, 0x30800e1c, 0x001e4f1c, 0x000f4200] };
        let lowered = lower_to_maxwell(&[sample, exit]);
        assert!(lowered.is_valid(), "{}", lowered.error);
        assert_eq!(lowered.required_texture_buffer, 2);
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        {
            let mut tv = TranslatorVisitor::new(&mut program, 0);
            tv.set_f(24, Value::ImmF32(0.25));
            tv.set_f(25, Value::ImmF32(0.75));
            tv.set_f(28, Value::ImmF32(0.5));
            for &instruction in &lowered.instructions[..lowered.source_instruction_offsets[1] as usize] {
                tv.translate_instruction(instruction);
            }
        }
        let writes: Vec<_> = program.blocks[0].iter().filter(|i| i.opcode == Opcode::SetRegister)
            .filter_map(|i| if let Value::Reg(reg) = i.args[0] { Some(reg.0) } else { None }).collect();
        assert!(writes.ends_with(&[24, 25, 28, 29]));
        ssa_rewrite_pass(&mut program);
        constant_propagation_pass(&mut program);
        let gather = program.blocks[0].iter().find(|i| i.opcode == Opcode::BoundImageGatherDref).unwrap();
        assert!(TextureInstInfo::from_u32(gather.flags).is_depth);
        assert_eq!(resolve(&program, gather.args[0]), Value::ImmU32(0x38));
        assert_eq!(resolve(&program, gather.args[4]), Value::ImmF32(0.5));
        let Value::Inst(coords) = resolve(&program, gather.args[1]) else { panic!("missing XY"); };
        let coords = program.blocks[coords.block as usize].instructions[coords.inst as usize].as_ref().unwrap();
        assert_eq!(resolve(&program, coords.args[0]), Value::ImmF32(0.25));
        assert_eq!(resolve(&program, coords.args[1]), Value::ImmF32(0.75));
        let mut packed = sample;
        packed.words[2] = (packed.words[2] & !255) | 25 | (1 << 15);
        let packed = lower_to_maxwell(&[packed, exit]);
        assert!(packed.is_valid(), "{}", packed.error);
        let destinations: Vec<_> = packed.instructions.iter()
            .filter(|&&word| word & 0xffff000000000000 == 0x5c18000000000000)
            .map(|word| word & 255).collect();
        assert_eq!(destinations, [24, 25]); // FP16 outputs occupy one register each.
        for (word, mask) in [(2, 1 << 12), (2, 1 << 13), (1, 1 << 28)] {
            let mut unsupported = sample;
            unsupported.words[word] ^= mask;
            assert!(!lower_to_maxwell(&[unsupported, exit]).is_valid());
        }
    }

    #[test]
    fn lea_high_subtraction_preserves_wrapping_arithmetic() {
        use crate::frontend::translate::TranslatorVisitor;
        use crate::ir::{basic_block::Block, program::Program, types::ShaderStage, value::Value};
        use crate::ir_opt::{constant_propagation_pass::constant_propagation_pass, ssa_rewrite_pass::ssa_rewrite_pass};
        let exit = Instruction { words: [0x794d, 0, 0x03800000, 0] };
        for signed in [false, true] {
            for negate in [false, true] {
                for shift in [0u32, 1, 16, 30, 31] {
                    let native = Instruction { words: [0x04037211, 5 | (u32::from(negate) << 31),
                        0x078f00ff | (shift << 11) | (u32::from(signed) << 9), 0] };
                    let lowered = lower_to_maxwell(&[native, exit]);
                    assert!(lowered.is_valid(), "{}", lowered.error);
                    for a in [0u32, 1, 0x7fffffff, 0x80000000, u32::MAX] {
                        for b in [0u32, 1, 0x80000000, u32::MAX] {
                            let mut program = Program::new(ShaderStage::Compute);
                            program.blocks.push(Block::new());
                            let output;
                            {
                                let mut tv = TranslatorVisitor::new(&mut program, 0);
                                tv.set_x(4, Value::ImmU32(a));
                                tv.set_x(5, Value::ImmU32(b));
                                for &instruction in &lowered.instructions[..lowered.source_instruction_offsets[1] as usize] {
                                    tv.translate_instruction(instruction);
                                }
                                output = tv.x(3);
                            }
                            ssa_rewrite_pass(&mut program);
                            constant_propagation_pass(&mut program);
                            let mut value = output;
                            while let Value::Inst(reference) = value {
                                let inst = program.blocks[reference.block as usize].instructions[reference.inst as usize].as_ref().unwrap();
                                assert_eq!(inst.opcode, crate::ir::opcodes::Opcode::Identity);
                                value = inst.args[0];
                            }
                            let high = if signed { ((a as i32 as i64) >> (32 - shift)) as u32 }
                                else { ((a as u64) >> (32 - shift)) as u32 };
                            let expected = if negate { high.wrapping_sub(b) } else { high.wrapping_add(b) };
                            assert_eq!(value, Value::ImmU32(expected), "a={a:x} b={b:x} shift={shift} signed={signed} negate={negate}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn gather_offsets_require_unchanged_execution_predicate() {
        let constant = |reg: u32, value: u32| Instruction {
            words: [0x9802 | (reg << 16), value, 0xf00, 0],
        };
        let gather = Instruction { words: [0x9b63, 0, 0, 0] };
        // !P1 constants remain valid when PLOP3 or LOP3 writes another predicate.
        for opcode in [0x81c, 0x212] {
            for target in 0..7 {
                let middle = Instruction {
                    words: [opcode | 0x7000 | (24 << 16), 0, (target << 17) | (7 << 20), 0],
                };
                let code = [constant(16, 0xffff0001), constant(17, 0x01000100), middle, gather];
                assert_eq!(BoundedControl::gather_constant_offsets(&code, 3, 16),
                    if target == 1 { None } else { Some([0xffff0001, 0x01000100]) });
            }
        }
        let mut code = [constant(16, 0), constant(17, 1), gather];
        code[0].words[0] ^= 1 << 12; // !P0 definition cannot establish a !P1 value.
        assert_eq!(BoundedControl::gather_constant_offsets(&code, 2, 16), None);
    }

    #[test]
    fn gather_offsets_require_dominating_in_range_unclobbered_constants() {
        let constant = |reg: u32, value: u32| Instruction {
            words: [0x7802 | (reg << 16), value, 0xf00, 0],
        };
        let nop = Instruction { words: [0x7918, 0, 0, 0] };
        let gather = Instruction { words: [0x7b63, 0, 0, 0] };
        let branch = |from: usize, to: usize| {
            let offset = ((to as i64 - from as i64 - 1) * 16) as u64;
            Instruction { words: [0x7947, offset as u32, ((offset >> 32) as u32) & 0x3ffff, 0] }
        };
        let base = [nop, constant(16, 0x1fe00001), constant(17, 0xff000100), nop, gather];
        assert_eq!(BoundedControl::gather_constant_offsets(&base, 4, 16), Some([0x1fe00001, 0xff000100]));
        for (from, to) in [(0, 2), (0, 3), (1, 3)] {
            let mut code = base;
            code[from] = branch(from, to);
            assert_eq!(BoundedControl::gather_constant_offsets(&code, 4, 16), None);
        }
        let mut internal = base;
        internal[3] = branch(3, 4);
        assert!(BoundedControl::gather_constant_offsets(&internal, 4, 16).is_some());
        for byte in [32, 127, 128, 223] {
            let mut code = base;
            code[1].words[1] = byte;
            assert_eq!(BoundedControl::gather_constant_offsets(&code, 4, 16), None);
        }
        let mut overwritten = base;
        // Two-register write starting at R15 aliases the first offset register.
        overwritten[3] = Instruction { words: [0x000f7805, 0, 0, 0] };
        assert_eq!(BoundedControl::gather_constant_offsets(&overwritten, 4, 16), None);
        let mut loop_entry = base.to_vec();
        loop_entry.push(branch(5, 3));
        assert_eq!(BoundedControl::gather_constant_offsets(&loop_entry, 4, 16), None);
    }

    #[test]
    fn task_mesh_isbe_native_modes_preserve_pseudo_fields_and_reject_invalid_access() {
        let exit = Instruction {
            words: [0x794d, 0, 0x03800000, 0],
        };
        for write in [false, true] {
            for region in 0..4 {
                for output in [false, true] {
                    for wide in [false, true] {
                        for skew in [false, true] {
                            let insn = Instruction {
                                words: [
                                    (if write { 0x927 } else { 0x923 })
                                        | (3 << 12)
                                        | (17 << 16)
                                        | (19 << 24),
                                    13 | (0xabcdef << 8),
                                    ((wide as u32) << 11)
                                        | (region << 12)
                                        | ((skew as u32) << 14)
                                        | ((output as u32) << 15),
                                    0,
                                ],
                            };
                            let result = lower_task_mesh_to_maxwell(&[insn, exit]);
                            let valid = matches!(region, 0 | 3)
                                && write == output
                                && (write || region == 3);
                            assert_eq!(result.is_valid(), valid);
                            if valid {
                                let pseudo = result.instructions[0];
                                assert_eq!(pseudo & 255, if write { 13 } else { 17 });
                                assert_eq!((pseudo >> 8) & 255, 19);
                                assert_eq!((pseudo >> 16) & 15, 3);
                                assert_eq!((pseudo >> 20) & 0xffffff, 0xabcdef);
                                assert_eq!((pseudo >> 44) & 1, wide as u64);
                                assert_eq!((pseudo >> 45) & 1, (region == 3) as u64);
                                assert_eq!((pseudo >> 46) & 1, skew as u64);
                                assert_eq!((pseudo >> 47) & 1, output as u64);
                                assert_eq!((pseudo >> 48) & 1, write as u64);
                                assert_eq!(
                                    crate::frontend::maxwell_opcodes::decode_opcode(pseudo),
                                    Some(crate::frontend::maxwell_opcodes::MaxwellOpcode::ISBE_nx2)
                                );
                                assert_eq!(result.source_instruction_offsets, [0, 1]);
                                assert!(!lower_to_maxwell(&[insn, exit]).is_valid());
                            } else {
                                assert_eq!(result.error, "native stage buffer access mode");
                            }
                        }
                    }
                }
            }
        }
        let mut read = Instruction {
            words: [0x13ff7923, 0, 0x3800, 0],
        };
        assert_eq!(
            lower_task_mesh_to_maxwell(&[read, exit]).error,
            "native stage buffer access mode"
        );
        read.words[0] = 0x13117923;
        read.words[2] |= 1;
        assert_eq!(
            lower_task_mesh_to_maxwell(&[read, exit]).error,
            "native stage buffer access mode"
        );
        let write = Instruction {
            words: [0x13007927, 246, 0x8000, 0],
        };
        assert_eq!(
            lower_task_mesh_to_maxwell(&[write, exit]).error,
            "native program uses lowering scratch registers"
        );
        // These bytes are not operands in the corresponding instructions.
        let write = Instruction {
            words: [0x13fa7927, 13, 0x8000, 0],
        };
        let read = Instruction {
            words: [0x13117923, 246, 0x3800, 0],
        };
        assert!(lower_task_mesh_to_maxwell(&[write, exit]).is_valid());
        assert!(lower_task_mesh_to_maxwell(&[read, exit]).is_valid());
    }

    #[test]
    fn native_tex_f16_packs_first_component_in_low_half() {
        // TEX RGB.F16: RG in R0, B + zero in R1. The general TEX path
        // must match Fritchii's pack helper and Maxwell TEXS.F16 ordering.
        let sample = Instruction { words: [0x01007b60, 0x30002800, 0x049e8701, 0x000f4200] };
        let exit = Instruction { words: [0x794d, 0, 0x03800000, 0] };
        let result = lower_to_maxwell(&[sample, exit]);
        assert!(result.is_valid(), "{}", result.error);
        let packs: Vec<_> = result.instructions.iter().copied()
            .filter(|word| word & 0xffff000000000000 == 0x5c18000000000000)
            .collect();
        assert_eq!(packs.len(), 2);
        // Distinct half bit patterns make swapped RG and discarded B observable.
        for (word, low, high) in [(packs[0], 0x3400u32, 0x3800u32), (packs[1], 0x3a00, 0)] {
            let mut regs = [0u32; 256];
            regs[246] = low;
            regs[247] = high;
            let a = regs[((word >> 8) & 255) as usize];
            let b = regs[((word >> 20) & 255) as usize];
            let packed = (a << ((word >> 39) & 31)) + b;
            assert_eq!(packed & 0xffff, low);
            assert_eq!(packed >> 16, high);
        }
    }

    #[test]
    fn native_variable_rate_preserves_predication_and_uniform_destination() {
        let exit = Instruction { words: [0x794d, 0, 0x03800000, 0] };
        for opcode in [0x919, 0x9c3] {
            for dest in [0u32, 3, 5, 21, 22, 27] {
                for predicate in 0u32..15 {
                    let insn = Instruction { words: [opcode | predicate << 12 | dest << 16,
                        0, 0x8400, 0x000e2200] };
                    let result = lower_to_maxwell(&[insn, exit]);
                    assert!(result.is_valid(), "{}", result.error);
                    let target = if opcode == 0x919 { dest }
                        else if dest < 22 { 216 + dest } else { 218 + dest };
                    // Upstream mov_imm: predicated MOV32I of packed width=1, height=1.
                    assert_eq!(result.instructions[0], 0x010000101000f000
                        | u64::from(predicate) << 16 | u64::from(target));
                }
            }
        }
        for words in [[0x00037919, 1, 0x8400, 0], [0x00057919, 0, 0x8401, 0]] {
            assert_eq!(lower_to_maxwell(&[Instruction { words }, exit]).error,
                "native special register");
        }
        assert_eq!(lower_to_maxwell(&[Instruction { words: [0x004079c3, 0, 0x8400, 0] }, exit]).error,
            "native uniform special destination");
    }

    #[test]
    fn native_uniform_mufu_and_txq_descriptor_zero_match_captured_encodings() {
        let exit = Instruction { words: [0x794d, 0, 0x03800000, 0] };
        let mufu = Instruction { words: [0x00098d08, 5, 0x08001000, 0x000e2400] };
        assert!(ProgramLowering::uses_uniform_bank(&[mufu]));
        let result = lower_to_maxwell(&[mufu, exit]);
        assert!(result.is_valid(), "{}", result.error);
        assert_eq!(result.instructions[0], 0x5080000000080009 | (221u64 << 8) | (4u64 << 20));
        let mut invalid = mufu;
        invalid.words[1] = 64;
        assert_eq!(lower_to_maxwell(&[invalid, exit]).error, "native MUFU uniform source");

        let query = Instruction { words: [0x02027b6f, 0x1600, 0x3ff, 0x000f4200] };
        let result = lower_to_maxwell(&[query, exit]);
        assert!(result.is_valid(), "{}", result.error);
        assert_eq!(result.required_texture_buffer, 0);
        assert_eq!(result.instructions[0], 0xdf48000000070002 | (2u64 << 8)
            | (1u64 << 22) | (3u64 << 31) | (22u64 << 36));
        let mut conflict = query;
        conflict.words[1] |= 1 << 22;
        assert_eq!(lower_to_maxwell(&[query, conflict, exit]).error, "native texture descriptor buffer conflict");
    }

    #[test]
    fn native_fsetp_uniform_source_preserves_predicates_and_float_modes() {
        let exit = Instruction { words: [0x794d, 0, 0x03800000, 0] };
        // Captured FSETP with a uniform source; the uniform bank must be reserved.
        let insn = Instruction { words: [0x22009c0b, 5, 0x0bf1b000, 0x000fc800] };
        assert!(ProgramLowering::uses_uniform_bank(&[insn]));
        let lowered = lower_to_maxwell(&[insn, exit]);
        assert!(lowered.is_valid(), "{}", lowered.error);
        // Fritchii FSETP emission: UR5 maps to R221, R34 is the other operand.
        let expected = 0x5bb0000000000000u64
            | (u64::from(insn.bits(12, 4)) << 16)
            | u64::from(insn.bits(84, 3))
            | (u64::from(insn.bits(81, 3)) << 3)
            | (34 << 8) | (221 << 20)
            | (u64::from(insn.bits(87, 3)) << 39)
            | (u64::from(insn.bits(90, 1)) << 42)
            | (u64::from(insn.bits(74, 2)) << 45)
            | (u64::from(insn.bits(80, 1)) << 47)
            | (u64::from(insn.bits(76, 4)) << 48);
        assert_eq!(lowered.instructions[0], expected);
    }

    #[test]
    fn native_uniform_population_count_captured_operands_and_inversion() {
        let exit = Instruction { words: [0x794d, 0, 0x03800000, 0] };
        for (dest, source, target, mirror) in [(9, 8, 225, 224), (4, 7, 220, 223), (22, 63, 240, 255), (28, 8, 214, 224), (9, 28, 225, 214)] {
            for inverted in [false, true] {
                let insn = Instruction { words: [0x72bf | (dest << 16), source | ((inverted as u32) << 31), 0x08000000, 0x000fe200] };
                assert!(ProgramLowering::uses_uniform_bank(&[insn]));
                let lowered = lower_to_maxwell(&[insn, exit]);
                assert!(lowered.is_valid(), "{}", lowered.error);
                assert_eq!(lowered.instructions[0], 0x5c08000000070000 | target | (mirror << 20) | ((inverted as u64) << 40));
            }
        }
        for words in [
            [0x003f72bf, 8, 0x08000000, 0], // Uniform zero register is not a destination.
            [0x004072bf, 8, 0x08000000, 0], // Outside uniform register file.
            [0x000972bf, 64, 0x08000000, 0],
            [0x000962bf, 8, 0x08000000, 0], // Predicated uniform form unsupported upstream.
            [0x000972bf, 0x100, 0x08000000, 0],
        ] {
            assert_eq!(lower_to_maxwell(&[Instruction { words }, exit]).error, "native uniform population count mode");
        }
    }

    #[test]
    fn native_surface_reduction_runtime_sample_and_atomic_operations() {
        use crate::frontend::translate::{surface_atomic_operations, TranslatorVisitor};
        use crate::ir::{opcodes::Opcode, program::Program, types::ShaderStage};
        let sample = Instruction {
            words: [0x0400759f, 0x60000c00, 0x0010e000, 0x000fe200],
        };
        let exit = Instruction {
            words: [0x794d, 0, 0x03800000, 0],
        };
        let lowered = lower_to_maxwell(&[sample, exit]);
        assert!(lowered.is_valid(), "{}", lowered.error);
        assert_eq!(lowered.required_texture_buffer, 0);
        assert_eq!(
            lowered.instructions,
            [0xeb4800c600070400, 0xe30000000007000f]
        );

        let ops = [
            Opcode::BoundImageAtomicIAdd32,
            Opcode::BoundImageAtomicUMin32,
            Opcode::BoundImageAtomicUMax32,
            Opcode::BoundImageAtomicInc32,
            Opcode::BoundImageAtomicDec32,
            Opcode::BoundImageAtomicAnd32,
            Opcode::BoundImageAtomicOr32,
            Opcode::BoundImageAtomicXor32,
        ];
        for returns_value in [false, true] {
            for dimension in [3, 5] {
                for (operation, expected) in ops.into_iter().enumerate() {
                    let mut insn = sample;
                    insn.words[0] = (4 << 24)
                        | (2 << 16)
                        | (3 << 12)
                        | if returns_value { 0x593 } else { 0x59f };
                    insn.words[1] = (dimension << 29) | (2 << 22) | (12 << 8) | 8;
                    insn.words[2] |= (operation as u32) << 23;
                    if returns_value {
                        insn.words[2] |= 0xe0000;
                    }
                    let lowered = lower_to_maxwell(&[insn, exit]);
                    if (returns_value || dimension != 3) && operation != 0 && operation != 6 {
                        assert_eq!(lowered.error, "native surface reduction mode");
                        continue;
                    }
                    assert!(lowered.is_valid(), "{}", lowered.error);
                    assert_eq!(lowered.required_texture_buffer, 2);
                    assert_eq!((lowered.instructions[0] >> 16) & 15, 3);
                    let mut program = Program::new(ShaderStage::Compute);
                    let block = program.add_block();
                    let mut tv = TranslatorVisitor::new(&mut program, block);
                    if returns_value {
                        surface_atomic_operations::suatom(&mut tv, lowered.instructions[0]);
                    } else {
                        surface_atomic_operations::sured(&mut tv, lowered.instructions[0]);
                    }
                    let block = &tv.ir.program.blocks[block as usize];
                    let atomic = block.iter().find(|inst| inst.opcode == expected).unwrap();
                    assert_eq!(atomic.args[0], crate::ir::value::Value::ImmU32(48));
                    assert_eq!(
                        block.iter().any(|inst| inst.opcode == Opcode::SetRegister),
                        returns_value
                    );
                }
            }
        }
    }

    #[test]
    fn native_surface_reduction_rejects_unrepresentable_modes_and_binding_conflicts() {
        let sample = Instruction {
            words: [0x0400759f, 0x60000c00, 0x0010e000, 0x000fe200],
        };
        let exit = Instruction {
            words: [0x794d, 0, 0x03800000, 0],
        };
        for (word, mask, value) in [
            (1, 7 << 29, 4 << 29), // Array unsupported by this native arm.
            (1, 31 << 22, 18 << 22),
            (1, 0x3fff << 8, 0x2000 << 8),
            (1, 3 << 27, 1 << 27),
            (2, 0, 1 << 9), // Different memory/type mode.
        ] {
            let mut invalid = sample;
            invalid.words[word] = (invalid.words[word] & !mask) | value;
            assert_eq!(
                lower_to_maxwell(&[invalid, exit]).error,
                "native surface reduction mode"
            );
        }
        let mut second = sample;
        second.words[1] |= 1 << 22;
        assert_eq!(
            lower_to_maxwell(&[sample, second, exit]).error,
            "native surface descriptor buffer conflict"
        );
        for reg in 246..255 {
            let mut reserved = sample;
            reserved.words[1] |= reg;
            assert_eq!(
                lower_to_maxwell(&[reserved, exit]).error,
                "native program uses lowering scratch registers"
            );
        }
    }

    #[test]
    fn complete_programs_match_verbatim_upstream_cpp() {
        check_program_fixture(include_str!("programs.fixture"), 67809);
    }

    #[test]
    fn gameplay_shift_round_and_constant_load_modes_match_upstream_cpp() {
        check_program_fixture(include_str!("gameplay_memory_programs.fixture"), 236);
    }

    #[test]
    fn gameplay_conversion_and_extended_compare_modes_match_upstream_cpp() {
        check_program_fixture(include_str!("gameplay_conversion_programs.fixture"), 450);
    }

    #[test]
    fn high_register_programs_and_restoration_branches_match_upstream_cpp() {
        check_program_fixture(include_str!("register_pressure_programs.fixture"), 806);
    }

    #[test]
    fn float_integer_modes_and_modifiers_match_upstream_cpp() {
        check_program_fixture(include_str!("float_integer_programs.fixture"), 9365);
    }

    #[test]
    fn half_comparison_register_modes_match_upstream_cpp() {
        check_program_fixture(include_str!("half_compare_programs.fixture"), 3073);
    }

    #[test]
    fn texel_fetch_modes_and_descriptor_conflicts_match_upstream_cpp() {
        check_program_fixture(include_str!("texel_fetch_programs.fixture"), 4097);
    }

    #[test]
    fn gather_modes_and_constant_offset_proofs_match_upstream_cpp() {
        check_program_fixture(include_str!("gather_programs.fixture"), 1457);
    }

    #[test]
    fn selection_modifiers_and_uniform_operands_match_upstream_cpp() {
        check_program_fixture(include_str!("selection_programs.fixture"), 5034);
    }

    #[test]
    fn uniform_comparison_predicate_masks_match_upstream_cpp() {
        check_program_fixture(include_str!("uniform_compare_programs.fixture"), 3625);
    }

    #[test]
    fn uniform_moves_and_leading_bits_match_upstream_cpp() {
        check_program_fixture(include_str!("uniform_move_flo_programs.fixture"), 14871);
    }

    #[test]
    fn native_branch_tables_prove_calls_only_with_bounded_reaching_definitions() {
        let code = [
            Instruction { words: [0x00377802, 48, 0xf00, 0] },
            Instruction { words: [0x00017202, 2, 0xf00, 0] },
            Instruction { words: [0x7944, 112, 0x03c00000, 0] },
            Instruction { words: [0x0c0c7817, 1, 0x03800000, 0] },
            Instruction { words: [0x0c0e7819, 2, 0x6ff, 0] },
            Instruction { words: [0x0e0c7b82, 0x00400800, 0x800, 0] },
            Instruction { words: [0xff0d7819, 31, 0x1140c, 0] },
            Instruction { words: [0x0c007949, 128, 0x03800000, 0] },
            Instruction { words: [0x794d, 0, 0x03800000, 0] },
            Instruction { words: [0x794d, 0, 0x03800000, 0] },
            Instruction { words: [0x00017202, 2, 0xf00, 0] },
            Instruction { words: [0x37007950, 0xffffff40, 0x03c3ffff, 0] },
            Instruction { words: [0x7947, 0xfffffff0, 0x0383ffff, 0] },
        ];
        let table = |bank, offset| match (bank, offset) {
            (1, 32) => Some(128u32.wrapping_sub(256)),
            (1, 36) => Some(144u32.wrapping_sub(256)),
            _ => None,
        };
        assert_eq!(BoundedControl::branch_table(&code, 7), Some((3, 1, 32, 2, 256)));
        assert!(!BoundedControl::analyze(&code, false, false, 216).bounded);
        let control = BoundedControl::analyze_with_cbuf(&code, false, false, 216, &mut table.clone());
        assert!(control.bounded);
        assert_eq!(control.links[2], 55);
        assert!(control.checked_returns[11]);
        let lowered = lower_to_maxwell_with_cbuf(&code, LoweringOptions::default(), &mut table.clone());
        assert!(lowered.is_valid(), "{} at {}", lowered.error, lowered.error_offset);
        // Bad targets: unaligned, outside capture, inside the definition chain,
        // or entering a callee without CALL. Signed backward offsets above work.
        for target in [129u32, 208, 64, 160] {
            let control = BoundedControl::analyze_with_cbuf(&code, false, false, 216,
                &mut |_, _| Some(target.wrapping_sub(256)));
            assert!(!control.bounded, "target {target}");
        }
        for (pc, word, value) in [
            (3, 1, u32::MAX), (3, 2, 0x03800200), (3, 2, 0x07800000),
            (4, 1, 1), (5, 0, 0x0e0c0b82), (5, 2, 0xa00),
            (6, 0, 0xff0c7819),
        ] {
            let mut changed = code;
            changed[pc].words[word] = value;
            assert!(BoundedControl::branch_table(&changed, 7).is_none(), "mutation {pc}/{word}");
            assert!(!BoundedControl::analyze_with_cbuf(&changed, false, false, 216, &mut table.clone()).bounded);
        }
    }

    #[test]
    fn secondary_branch_predicates_preserve_call_entry_proofs() {
        for primary in 0..16 {
            for secondary in 0..16 {
                let mut code = [
                    Instruction { words: [0x947 | (primary << 12), 0, secondary << 23, 0] },
                    Instruction { words: [0x00377802, 64, 0xf00, 0] },
                    Instruction { words: [0x00017202, 2, 0xf00, 0] },
                    Instruction { words: [0x7944, 16, 0x03c00000, 0] },
                    Instruction { words: [0x794d, 0, 0x03800000, 0] },
                    Instruction { words: [0x00017202, 2, 0xf00, 0] },
                    Instruction { words: [0x37007950, 0xffffff90, 0x03c3ffff, 0] },
                ];
                let control = BoundedControl::analyze(&code, false, false, 216);
                assert!(control.bounded, "predicates {primary}/{secondary}");
                assert_eq!(control.direct_targets[0], 1);
                assert_eq!(control.links[3], 55);
                assert!(control.checked_returns[6]);
                let lowered = lower_to_maxwell(&code);
                assert!(lowered.is_valid(), "{}", lowered.error);
                // Even a conditional entry between link setup and CALL invalidates
                // the proof. Never discard the edge to accept a wider predicate set.
                code[0].words[1] = 16;
                assert!(!BoundedControl::analyze(&code, false, false, 216).bounded);
                code[0].words[1] = 0;
                for reserved in [1 << 18, 1 << 22, 1 << 28, 1 << 31] {
                    code[0].words[2] = (secondary << 23) | reserved;
                    assert!(!BoundedControl::analyze(&code, false, false, 216).bounded);
                }
            }
        }
    }

    #[test]
    fn conditional_loop_transfer_does_not_require_a_return_link() {
        let code = [
            Instruction { words: [0x00377802, 32, 0xf00, 0] },
            Instruction { words: [0x7944, 48, 0x03c00000, 0] },
            Instruction { words: [0x0944, 16, 0x03c00000, 0] },
            Instruction { words: [0x7947, 0xffffffe0, 0x0383ffff, 0] },
            Instruction { words: [0x794d, 0, 0x03800000, 0] },
            Instruction { words: [0x00017202, 2, 0xf00, 0] },
            Instruction { words: [0x37007950, 0xffffff90, 0x03c3ffff, 0] },
        ];
        let control = BoundedControl::analyze(&code, false, false, 216);
        assert!(control.bounded);
        assert_eq!(control.links[1], 55);
        assert_eq!(control.links[2], u32::MAX);
        assert!(control.checked_returns[6]);
        assert!(lower_to_maxwell(&code).is_valid());
    }

    #[test]
    fn relative_computed_branch_base_checks_alignment_and_encoded_range() {
        for (base, expected) in [(0i64, ""), (0x1f00, ""), (0x7500, ""),
            (0x800050, ""), (0x800060, "native computed branch range"),
            (-16, "native computed branch relative base"), (1, "native computed branch relative base")] {
            let relative = (base - 16) as u64;
            let code = [Instruction { words: [0x0c007949, relative as u32,
                0x03800000 | ((relative >> 32) as u32 & 0x3ffff), 0] },
                Instruction { words: [0x7947, 0xfffffff0, 0x0383ffff, 0] }];
            assert_eq!(lower_to_maxwell(&code).error, expected, "base {base:#x}");
        }
    }

    #[test]
    fn imad_carry_modes_match_upstream_cpp() {
        check_program_fixture(include_str!("imad_carry_programs.fixture"), 1396);
    }

    #[test]
    fn redux_modes_match_upstream_cpp() {
        check_program_fixture(include_str!("redux_programs.fixture"), 512);
    }

    #[test]
    fn uniform_select_predicates_and_clamp_proofs_match_upstream_cpp() {
        check_program_fixture(include_str!("uniform_select_programs.fixture"), 2504);
    }

    #[test]
    fn uniform_high_word_shift_matches_upstream_cpp() {
        check_program_fixture(include_str!("uniform_high_shift_programs.fixture"), 4751);
    }

    #[test]
    fn gradient_texture_masks_and_split_destinations_match_upstream_cpp() {
        check_program_fixture(include_str!("gradient_programs.fixture"), 1792);
    }

    #[test]
    fn register_shift_modes_match_upstream_cpp() {
        check_program_fixture(include_str!("register_shift_programs.fixture"), 3807);
    }

    #[test]
    fn minmax_immediate_boundaries_match_upstream_cpp() {
        check_program_fixture(include_str!("minmax_programs.fixture"), 4096);
    }

    #[test]
    fn bit_modes_and_uniform_register_boundaries_match_upstream_cpp() {
        check_program_fixture(include_str!("bit_modes_programs.fixture"), 2552);
    }

    #[test]
    fn texture_sample_handles_and_descriptor_buffers_match_upstream_cpp() {
        check_program_fixture(include_str!("texture_sample_programs.fixture"), 9501);
    }

    #[test]
    fn bounded_control_preflight_matches_verbatim_cpp() {
        let mut count = 0;
        for (line_number, line) in include_str!("bounded_control.fixture").lines().enumerate() {
            if line.is_empty() || line.starts_with('#') { continue; }
            let fields: Vec<_> = line.split('|').collect();
            let input: Vec<u32> = fields[0].split_whitespace().map(|s| s.parse().unwrap()).collect();
            let code: Vec<_> = input[4..].chunks_exact(4).map(|w| Instruction { words: w.try_into().unwrap() }).collect();
            assert_eq!(code.len(), input[3] as usize);
            let actual = BoundedControl::analyze(&code, input[0] != 0, input[1] != 0, input[2]);
            assert_eq!(actual.bounded, fields[1] == "1", "bounded line {}", line_number + 1);
            for (kind, values, expected) in [
                ("links", actual.links, fields[2]), ("targets", actual.direct_targets, fields[3]),
                ("returns", actual.checked_returns.into_iter().map(u32::from).collect(), fields[4]),
                ("dead", actual.dead_instructions.into_iter().map(u32::from).collect(), fields[5]),
            ] {
                let expected: Vec<u32> = expected.split_whitespace().map(|s| s.parse().unwrap()).collect();
                assert_eq!(values, expected, "{} line {}", kind, line_number + 1);
            }
            count += 1;
        }
        assert_eq!(count, 3528);
    }

    #[test]
    fn bounded_call_branch_and_exit_programs_match_upstream_cpp() {
        check_program_fixture(include_str!("bounded_programs.fixture"), 3528);
    }

    #[test]
    fn captured_runtime_shader_programs_match_upstream_cpp() {
        check_program_fixture(include_str!("runtime_shader_programs.fixture"), 5);
    }

    #[test]
    fn uniform_shift_boundaries_guards_and_imad_addends_match_upstream_cpp() {
        check_program_fixture(include_str!("uniform_shader_programs.fixture"), 386);
    }

    #[test]
    fn address_and_uniform_carry_programs_match_upstream_cpp() {
        check_program_fixture(include_str!("address_carry_programs.fixture"), 388);
    }

    fn check_program_fixture(fixture: &str, expected_count: usize) {
        let mut count = 0;
        for (line_number, line) in fixture.lines().enumerate() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let fields: Vec<_> = line.split('|').collect();
            let input: Vec<u32> = fields[0]
                .split_whitespace()
                .map(|v| v.parse().unwrap())
                .collect();
            let code: Vec<_> = input[1..]
                .chunks_exact(4)
                .map(|w| Instruction {
                    words: w.try_into().unwrap(),
                })
                .collect();
            assert_eq!(code.len(), input[0] as usize);
            let expected: Vec<u32> = fields[1]
                .split_whitespace()
                .map(|v| v.parse().unwrap())
                .collect();
            let instructions: Vec<u64> = fields[3]
                .split_whitespace()
                .map(|v| u64::from_str_radix(v, 16).unwrap())
                .collect();
            let offsets: Vec<u32> = fields[4]
                .split_whitespace()
                .map(|v| v.parse().unwrap())
                .collect();
            let actual = lower_to_maxwell(&code);
            // The frozen original oracle predates these two extensions. Its
            // all-zero operand probes are still rejected, now with the specific
            // mode error. Keep the original fixture and all metadata checks.
            let expected_error = if fields[2] == "unsupported native opcode"
                && matches!(code[0].words, [0x7593 | 0x759f, 0, 0, 0])
            {
                "native surface reduction mode"
            } else if fields[2] == "unsupported native opcode"
                && code[0].words == [0x72bf, 0, 0, 0]
            {
                "native uniform population count mode"
            } else {
                fields[2]
            };
            assert_eq!(
                actual.error,
                expected_error,
                "error on fixture line {}",
                line_number + 1
            );
            assert_eq!(
                [
                    actual.source_size,
                    actual.required_texture_buffer,
                    actual.error_offset
                ],
                expected.as_slice(),
                "metadata on fixture line {}",
                line_number + 1
            );
            assert_eq!(
                actual.instructions,
                instructions,
                "instructions on fixture line {}",
                line_number + 1
            );
            assert_eq!(
                actual.source_instruction_offsets,
                offsets,
                "offsets on fixture line {}",
                line_number + 1
            );
            count += 1;
        }
        assert_eq!(count, expected_count);
    }

    #[test]
    fn complete_program_size_limits_and_never_executed_padding() {
        let padding = Instruction {
            words: [0xffff, 0, 0, 0],
        };
        let mut code = vec![padding; 65536];
        code[65535] = Instruction {
            words: [0x794d, 0, 0x03800000, 0],
        };
        let result = lower_to_maxwell(&code);
        assert!(result.is_valid(), "{}", result.error);
        assert_eq!(result.source_size, 65536 * 16);
        assert_eq!(result.source_instruction_offsets, vec![0; 65536]);
        assert_eq!(result.instructions, [0xe30000000007000f]);
        code.push(padding);
        let result = lower_to_maxwell(&code);
        assert_eq!(result.error, "invalid native program size");
        assert_eq!(result.error_offset, 0);
        assert!(result.instructions.is_empty());
        assert!(result.source_instruction_offsets.is_empty());
    }

    #[test]
    fn operand_helpers_match_verbatim_upstream_cpp() {
        check_fixture(include_str!("operand_helpers.fixture"), 64524);
    }

    #[test]
    fn wide_multiply_signed_unsigned_addends_and_carries_match_upstream_cpp() {
        check_fixture(include_str!("wide_multiply.fixture"), 432);
    }

    #[test]
    fn matrix_software_matches_upstream_without_environment_gate() {
        const CHILD: &str = "RUZU_TEST_AMPERE_TENSOR_MODE";
        let Ok(mode) = std::env::var(CHILD) else {
            for mode in ["missing", "stub", "emulate", "empty", "other"] {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command.args(["--exact", "frontend::ampere::lower_to_maxwell::tests::matrix_software_matches_upstream_without_environment_gate", "--test-threads=1"]);
                command.env(CHILD, mode).env_remove("EDEN_NX2_TENSOR");
                match mode {
                    "missing" => {}
                    "empty" => {
                        command.env("EDEN_NX2_TENSOR", "");
                    }
                    "other" => {
                        command.env("EDEN_NX2_TENSOR", "STUB");
                    }
                    _ => {
                        command.env("EDEN_NX2_TENSOR", mode);
                    }
                }
                let output = command.output().unwrap();
                assert!(
                    output.status.success(),
                    "tensor {mode}: {} {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        };
        let fixture = include_str!("tensor_emulate.fixture");
        check_fixture(fixture, 165);
        // Legacy environment values, including stub, must not replace real
        // matrix arithmetic with a placeholder.
        std::env::set_var(
            "EDEN_NX2_TENSOR",
            if mode == "stub" { "emulate" } else { "stub" },
        );
        check_fixture(fixture, 165);
    }

    fn check_fixture(fixture: &str, expected_count: usize) {
        let mut count = 0;
        for (line_number, line) in fixture.lines().enumerate() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let columns: Vec<_> = line.split('|').collect();
            let input: Vec<u32> = columns[0]
                .split_whitespace()
                .map(|v| v.parse().unwrap())
                .collect();
            let p = &input[1..9];
            let mut emitted = Vec::new();
            let insn = Instruction {
                words: input[9..13].try_into().unwrap(),
            };
            let uniforms = &UniformRegisters::default();
            let mut lowering = InstructionLowering {
                uniforms,
                predicate: u64::from(insn.bits(12, 4)) << 16,
                dest: u64::from(insn.bits(16, 8)),
                insn,
                operand_error: "",
                instructions: &mut emitted,
            };
            let value = match input[0] {
                0 => uniforms.register(p[0]),
                1 => {
                    let valid = u32::from(lowering.valid_cbuf());
                    lowering.emit(lowering.cbuf());
                    valid
                }
                2 => lowering.materialize(p[0], p[1], p[2], p[3] != 0, p[4] != 0),
                3 => lowering.operand(p[0], p[1] != 0),
                4 => {
                    let half = lowering.half_source(p[0]);
                    lowering.extract_half(half, p[1], p[2]);
                    half.reg
                }
                5 => {
                    lowering.predicate_lut(p[0], p[1], p[2], p[3], p[4], p[5] != 0);
                    0
                }
                6 => {
                    lowering.xmad(
                        p[0],
                        p[1],
                        p[2],
                        p[3],
                        p[4] != 0,
                        p[5] != 0,
                        p[6],
                        p[7] != 0,
                    );
                    0
                }
                7 => {
                    lowering.multiply_low(p[0], p[1], p[2], p[3] != 0);
                    0
                }
                8 => {
                    lowering.save_predicate(p[0]);
                    lowering.restore_predicate(p[0]);
                    0
                }
                9 => {
                    let mut opcode = lowering.insn.bits(0, 12);
                    match UniformRewrite::new(&mut lowering.insn, &mut opcode, uniforms) {
                        Err(error) => {
                            lowering.operand_error = error;
                            0
                        }
                        Ok(rewrite) => {
                            lowering.predicate = rewrite.predicate;
                            lowering.dest = u64::from(lowering.insn.bits(16, 8));
                            rewrite.begin(&mut lowering);
                            lowering.emit(u64::from(opcode));
                            for word in lowering.insn.words {
                                lowering.emit(u64::from(word));
                            }
                            lowering.emit(u64::from(rewrite.rewritten_uniform));
                            lowering.emit(u64::from(rewrite.native_predicate));
                            lowering.emit(u64::from(rewrite.guarded_uniform));
                            lowering.emit(u64::from(rewrite.guard_mask));
                            lowering.emit(u64::from(rewrite.output_mask));
                            lowering.emit(rewrite.predicate);
                            rewrite.end(&mut lowering).unwrap();
                            1
                        }
                    }
                }
                10 => u32::from(ProgramLowering::uses_uniform_bank(&[lowering.insn])),
                11 | 12 => {
                    let mut relocations = Vec::new();
                    let mut required_texture_buffer = if input[0] == 12 { p[1] } else { u32::MAX };
                    let opcode = lowering.insn.bits(0, 12);
                    let native_predicate = lowering.insn.bits(12, 4);
                    let result = if matches!(
                        opcode,
                        0xc24 | 0xe24 | 0x224 | 0x2a5 | 0x225 | 0x227 | 0x625 | 0x825 | 0xa27 | 0x827 | 0xa24 | 0x424 | 0x624 | 0x824
                    ) {
                        lowering.lower_integer_multiply(opcode, native_predicate, p[2] != 0)
                    } else if matches!(
                        opcode,
                        0x806
                            | 0x886
                            | 0x3c2
                            | 0x81c
                            | 0x89c
                            | 0x883
                            | 0x882
                            | 0xc82
                            | 0xab9
                            | 0xabb
                            | 0x899
                            | 0xb82
                            | 0xf89
                            | 0x822
                    ) {
                        lowering.lower_vote_and_uniform(opcode, native_predicate)
                    } else if matches!(
                        opcode,
                        0xb6c
                            | 0x989
                            | 0x589
                            | 0x98e
                            | 0x2bd
                            | 0x2be
                            | 0x3a1
                            | 0x3c4
                            | 0xf8c
                            | 0x38c
                            | 0x3a8
                            | 0x9a8
                            | 0xb97
                            | 0xb9b
                    ) {
                        lowering.lower_texture_and_reduction(
                            opcode,
                            native_predicate,
                            &mut required_texture_buffer,
                        )
                    } else if matches!(
                        opcode,
                        0x301
                            | 0x219
                            | 0xa19
                            | 0xc19
                            | 0x300
                            | 0xd00
                            | 0x309
                            | 0xd09
                            | 0x918
                            | 0x992
                            | 0x9ab
                            | 0xb1d
                            | 0x388
                            | 0x984
                            | 0x23a
                    ) {
                        lowering.lower_bits_and_shared(opcode, native_predicate)
                    } else if matches!(opcode, 0x305 | 0xb05 | 0xd05 | 0xb6f | 0x370 | 0xb66 | 0x91a) {
                        lowering.lower_conversion_and_fetch(opcode, &mut required_texture_buffer)
                    } else if matches!(opcode, 0xb69 | 0xb63 | 0xf63 | 0xb60 | 0xf60 | 0x361) {
                        lowering.lower_texture_sample(opcode, &mut required_texture_buffer, &[], p[0], false)
                    } else if matches!(
                        opcode,
                        0x816
                            | 0x307
                            | 0xb07
                            | 0x321
                            | 0x322
                            | 0x387
                            | 0x983
                            | 0x803
                            | 0x804
                            | 0xc02
                            | 0x202
                            | 0x802
                            | 0xa02
                    ) {
                        lowering.lower_permutation_and_moves(opcode)
                    } else if opcode == 0x23c {
                        lowering.lower_matrix_multiply(false, false)
                    } else if matches!(opcode, 0xa11 | 0x211 | 0x811 | 0xc11) {
                        lowering.lower_effective_address(opcode, p[2] != 0)
                    } else if matches!(opcode, 0x210 | 0x810 | 0xa10 | 0xc10) {
                        lowering.lower_integer_add(opcode, p[2] != 0, false)
                    } else if matches!(
                        opcode,
                        0x207
                            | 0x807
                            | 0xa07
                            | 0x208
                            | 0x808
                            | 0xa08
                            | 0x20a
                            | 0x80a
                            | 0xa0a
                            | 0x434
                            | 0x20b
                            | 0x80b
                            | 0xa0b
                            | 0xc0b
                            | 0x20c
                            | 0x80c
                            | 0xa0c
                            | 0xc0c
                            | 0x212
                            | 0x812
                            | 0xa12
                            | 0xc12
                            | 0x213
                            | 0xa13
                            | 0xc13
                            | 0x217
                            | 0x817
                            | 0xa17
                            | 0xc17
                            | 0x819
                    ) {
                        lowering.lower_selection_and_logic(opcode)
                    } else if matches!(
                        opcode,
                        0x308
                            | 0x908
                            | 0xd08
                            | 0xb08
                            | 0x526
                            | 0x326
                            | 0x81a
                            | 0x299
                            | 0x499
                            | 0x419
                            | 0x805
                            | 0x306
                            | 0xd06
                            | 0x245
                            | 0xa45
                            | 0xc45
                    ) {
                        lowering.lower_special_functions_and_conversion(opcode, native_predicate)
                    } else if matches!(
                        opcode,
                        0x223
                            | 0x423
                            | 0x623
                            | 0x823
                            | 0xa23
                            | 0xc23
                            | 0xe23
                            | 0x220
                            | 0x820
                            | 0xa20
                            | 0xc20
                            | 0x221
                            | 0x421
                            | 0x621
                            | 0x821
                            | 0xa21
                            | 0xe21
                            | 0x209
                            | 0x809
                            | 0xa09
                            | 0xc09
                    ) {
                        lowering.lower_float_arithmetic(opcode)
                    } else if opcode == 0x83b {
                        lowering.lower_matrix_load()
                    } else if matches!(
                        opcode,
                        0x919
                            | 0x9c3
                            | 0x28c
                            | 0x88c
                            | 0x287
                            | 0x887
                            | 0x381
                            | 0x981
                            | 0x386
                            | 0x986
                            | 0x95b
                    ) {
                        let mut code = vec![Instruction { words: [0; 4] }; p[0] as usize + 1];
                        if p[0] > 0 {
                            code[p[0] as usize - 1].words.copy_from_slice(&p[4..8]);
                        }
                        lowering.lower_special_and_global(opcode, &code, p[0], &[])
                    } else {
                        lowering.lower_float_and_half(
                            opcode,
                            p[0],
                            native_predicate,
                            &mut relocations,
                        )
                    };
                    let error = result.err().unwrap_or(lowering.operand_error);
                    let value = if !error.is_empty() {
                        lowering.operand_error = error;
                        lowering.instructions.clear();
                        0
                    } else {
                        lowering.emit(lowering.predicate);
                        lowering.emit(relocations.len() as u64);
                        for relocation in relocations {
                            lowering.emit(u64::from(relocation.output_index));
                            lowering.emit(relocation.target as u64);
                        }
                        1
                    };
                    if input[0] == 12 {
                        lowering.emit(u64::from(required_texture_buffer));
                    }
                    value
                }
                action => panic!("unknown oracle action {action}"),
            };
            // UPOPC was absent in the frozen original scanner. Its new arm
            // requires the uniform bank; retain all other original assertions.
            let expected_value = if input[0] == 10 && insn.bits(0, 12) == 0x2bf {
                1
            } else {
                columns[1].parse::<u32>().unwrap()
            };
            assert_eq!(
                value,
                expected_value,
                "return at line {}",
                line_number + 1
            );
            assert_eq!(
                lowering.operand_error,
                columns[2],
                "error at line {}",
                line_number + 1
            );
            let expected: Vec<u64> = columns[3]
                .split_whitespace()
                .map(|word| u64::from_str_radix(word, 16).unwrap())
                .collect();
            assert_eq!(
                *lowering.instructions,
                expected,
                "emitted words at line {}",
                line_number + 1
            );
            count += 1;
        }
        assert_eq!(count, expected_count);
    }

    #[test]
    fn relocation_addresses_include_control_words_and_signed_displacements() {
        let mut program = ProgramLowering::default();
        program.result.instructions = vec![0xe24000000007000f; 4];
        program.result.source_instruction_offsets = vec![0, 2, 3];
        program.relocations = vec![
            Relocation {
                call: false,
                output_target: None,
                output_index: 0,
                target: 1,
            },
            Relocation {
                call: false,
                output_target: None,
                output_index: 2,
                target: 0,
            },
            Relocation {
                call: false,
                output_target: None,
                output_index: 3,
                target: 2,
            },
        ];
        let result = program.finish(2);
        assert!(result.is_valid());
        assert_eq!(result.source_size, 48);
        // Addresses 0x50 -> 0x68 (skip control), 0x68 -> 0x50 and self 0x70.
        assert_eq!(
            result.instructions,
            [
                0xe24000000107000f,
                0xe24000000007000f,
                0xe2400ffffe07000f,
                0xe2400fffff87000f,
            ]
        );
        assert_eq!(ProgramLowering::address_of(u32::MAX), 0x48);
    }

    #[test]
    fn relocation_failure_clears_output_but_preserves_other_result_fields() {
        for (target, source_offset, error) in [
            (1, 0, "native branch or call leaves captured program"),
            (0, 0x100000, "lowered branch distance"),
        ] {
            let mut program = ProgramLowering::default();
            program.result.instructions = vec![0xe24000000007000f];
            program.result.source_instruction_offsets = vec![source_offset];
            program.result.required_texture_buffer = 17;
            program.relocations.push(Relocation {
                call: false,
                output_target: None,
                output_index: 0,
                target,
            });
            let result = program.finish(0);
            assert_eq!(result.error, error);
            assert_eq!(result.error_offset, 0);
            assert_eq!(result.source_size, 0);
            assert_eq!(result.required_texture_buffer, 17);
            assert!(result.instructions.is_empty());
            assert!(result.source_instruction_offsets.is_empty());
        }
        let mut program = ProgramLowering::default();
        program.result.source_size = 123;
        let result = program.fail(65536, "native program has no unconditional EXIT");
        assert_eq!(result.error_offset, 1048576);
        assert_eq!(result.source_size, 123);
    }

    #[test]
    fn operand_error_precedes_uniform_restore_emissions() {
        let mut insn = Instruction {
            words: [0x00000292, 0, 0, 0],
        };
        let mut opcode = 0x292;
        let uniforms = &UniformRegisters::default();
        let rewrite = UniformRewrite::new(&mut insn, &mut opcode, uniforms).unwrap();
        assert!(rewrite.guarded_uniform);
        assert_ne!(rewrite.output_mask, 0);
        let mut words = Vec::new();
        let mut lowering = InstructionLowering {
            uniforms,
            insn,
            predicate: rewrite.predicate,
            dest: 216,
            operand_error: "native constant buffer address",
            instructions: &mut words,
        };
        rewrite.begin(&mut lowering);
        let before = lowering.instructions.clone();
        assert_eq!(
            rewrite.end(&mut lowering),
            Err("native constant buffer address")
        );
        assert_eq!(*lowering.instructions, before);
    }

    #[test]
    fn instruction_fields_cross_words_without_changing_layout() {
        let value = Instruction {
            words: [0x89abcdef, 0x01234567, 0xfedcba98, 0x76543210],
        };
        let all = 0x76543210_fedcba98_01234567_89abcdefu128;
        for offset in 0..128 {
            for count in 0..=32.min(128 - offset) {
                assert_eq!(
                    value.bits(offset, count),
                    ((all >> offset) & ((1u128 << count) - 1)) as u32
                );
            }
        }
        assert_eq!(std::mem::align_of::<Instruction>(), 4);
        let mut program = LoweredProgram::default();
        assert!(program.is_valid());
        assert_eq!(program.required_texture_buffer, u32::MAX);
        program.error = "failure";
        assert!(!program.is_valid());
    }
}

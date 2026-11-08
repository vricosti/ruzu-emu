// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
// Encoding reference: Mesa NAK sm70_encode.rs (MIT), Copyright 2022 Collabora, Ltd.
//! Native instruction lowering from `frontend/ampere/lower_to_maxwell.h`.

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub words: [u32; 4],
}

impl Instruction {
    pub fn bits(&self, offset: u32, count: u32) -> u32 {
        let index = (offset / 32) as usize;
        let mut bits = u64::from(self.words[index]);
        if index + 1 < self.words.len() {
            bits |= u64::from(self.words[index + 1]) << 32;
        }
        ((bits >> (offset % 32)) & ((1u64 << count) - 1)) as u32
    }
}

#[derive(Debug, Clone)]
pub struct LoweredProgram {
    pub instructions: Vec<u64>,
    pub source_instruction_offsets: Vec<u32>,
    pub source_size: u32,
    pub required_texture_buffer: u32,
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

const _: () = assert!(std::mem::size_of::<Instruction>() == 16);

/// Scheduling/control words are added by the environment adapter, as upstream.
pub fn lower_to_maxwell(code: &[Instruction]) -> LoweredProgram {
    lower_with_stage_buffer(code, false)
}

/// Explicit task/mesh path. Callers must supply mesh environment metadata and
/// use collective compute emission; ordinary graphics/compute remains gated.
pub fn lower_task_mesh_to_maxwell(code: &[Instruction]) -> LoweredProgram {
    lower_with_stage_buffer(code, true)
}

fn lower_with_stage_buffer(code: &[Instruction], stage_buffer: bool) -> LoweredProgram {
    let mut program = ProgramLowering::default();
    let uses_uniform_bank = ProgramLowering::uses_uniform_bank(code);
    if code.is_empty() || code.len() > 65536 {
        return program.fail(0, "invalid native program size");
    }
    for index in 0..code.len() as u32 {
        let mut insn = code[index as usize];
        program
            .result
            .source_instruction_offsets
            .push(program.result.instructions.len() as u32);
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
            if active_link == Some(insn.bits(16, 8)) {
                return program.fail(index, "native callee overwrites link register");
            }
        }
        // Pinned operand scanner: ISBERD has Rd/Ra; ISBEWR has Ra/data.
        // Offset bytes and unused fields must not be mistaken for registers.
        let register_offsets: &[u32] = match (stage_buffer, opcode) {
            (true, 0x923) => &[16, 24],
            (true, 0x927) => &[24, 32],
            _ => &[16, 24, 32, 64],
        };
        for &bit in register_offsets {
            if bit == 32
                && insn.bits(9, 3) != 1
                && !matches!(opcode, 0xb60 | 0xb66 | 0x593 | 0x59f)
                && !(stage_buffer && opcode == 0x927)
            {
                continue;
            }
            let reg = insn.bits(bit, 8);
            if reg >= if uses_uniform_bank { 216 } else { 246 } && reg < 255 {
                return program.fail(index, "native program uses lowering scratch registers");
            }
        }
        let rewrite = match UniformRewrite::new(&mut insn, &mut opcode) {
            Ok(rewrite) => rewrite,
            Err(error) => return program.fail(index, error),
        };
        let native_predicate = rewrite.native_predicate;
        let mut lowering = InstructionLowering {
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
            0xc24 | 0xe24 | 0x224 | 0x2a5 | 0x827 | 0xa24 | 0x424 | 0x624 | 0x824 => {
                lowering.lower_integer_multiply(opcode, native_predicate)
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
            0x301 | 0x219 | 0xc19 | 0x300 | 0xd00 | 0x309 | 0xd09 | 0x918 | 0x992 | 0x9ab
            | 0xb1d | 0x388 | 0x984 | 0x23a => {
                lowering.lower_bits_and_shared(opcode, native_predicate)
            }
            0x23c => lowering.lower_matrix_multiply(),
            0x83b => lowering.lower_matrix_load(),
            0x919 | 0x9c3 | 0x28c | 0x88c | 0x287 | 0x887 | 0x381 | 0x981 | 0x386 | 0x986
            | 0x95b => lowering.lower_special_and_global(opcode, code, index),
            0x305 | 0xb05 | 0xd05 | 0xb6f | 0xb66 | 0x91a => lowering
                .lower_conversion_and_fetch(opcode, &mut program.result.required_texture_buffer),
            0xb69 | 0xb63 | 0xb60 => {
                lowering.lower_texture_sample(opcode, &mut program.result.required_texture_buffer)
            }
            0x816 | 0x307 | 0x321 | 0x322 | 0x387 | 0x983 | 0x803 | 0x804 | 0xc02 | 0x202
            | 0x802 | 0xa02 => lowering.lower_permutation_and_moves(opcode),
            0x223 | 0x423 | 0x623 | 0x823 | 0xa23 | 0xc23 | 0xe23 | 0x220 | 0x820 | 0xa20
            | 0xc20 | 0x221 | 0x421 | 0x621 | 0x821 | 0xa21 | 0xe21 | 0x209 | 0x809 | 0xa09
            | 0xc09 => lowering.lower_float_arithmetic(opcode),
            0x308 | 0xb08 | 0x526 | 0x326 | 0x81a | 0x299 | 0x419 | 0x805 | 0x306 | 0xd06
            | 0x245 | 0xa45 | 0xc45 => {
                lowering.lower_special_functions_and_conversion(opcode, native_predicate)
            }
            0x207 | 0x807 | 0xa07 | 0x208 | 0x808 | 0xa08 | 0x20a | 0x80a | 0xa0a | 0x434
            | 0x20b | 0x80b | 0xa0b | 0x20c | 0x80c | 0xa0c | 0xc0c | 0x212 | 0x812 | 0xa12
            | 0xc12 | 0x213 | 0xa13 | 0xc13 | 0x217 | 0x817 | 0xa17 | 0xc17 | 0x819 => {
                lowering.lower_selection_and_logic(opcode)
            }
            0xa11 | 0x211 | 0x811 => lowering.lower_effective_address(opcode),
            0x210 | 0x810 | 0xa10 | 0xc10 => {
                lowering.lower_integer_add(opcode, rewrite.rewritten_uniform)
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
                if i64::from(index) * 16 + 16 + relative != 0 {
                    return program.fail(index, "native computed branch relative base");
                }
                let offset = -i64::from(ProgramLowering::address_of(
                    lowering.instructions.len() as u32
                )) - 8;
                if offset < -0x800000 {
                    return program.fail(index, "native computed branch range");
                }
                let source = if opcode == 0x958 {
                    InstructionLowering::uniform_register(insn.bits(24, 8))
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
                if native_predicate != 7
                    && insn.words[1] == 16
                    && insn.words[2] == 0x03c00000
                    && (index as usize + 2) < code.len()
                    && code[index as usize + 1].bits(0, 12) == 0x947
                {
                    program.relocations.push(Relocation {
                        output_index: lowering.instructions.len() as u32,
                        target: i64::from(index + 2),
                    });
                    lowering.emit(0xe24000000000000f | lowering.predicate);
                } else {
                    if program.main_exited
                        || index == 0
                        || insn.words[0] != 0x7944
                        || insn.words[2] & !0x3ffff != 0x03c00000
                        || code[index as usize - 1].words[0] & !0x00ff0000 != 0x00007802
                        || code[index as usize - 1].bits(16, 8) >= 250
                        || code[index as usize - 1].words[1] != (index + 1) * 16
                        || code[index as usize - 1].words[2] != 0xf00
                    {
                        return program.fail(index, "native call linkage mode");
                    }
                    let raw = u64::from(insn.words[1]) | (u64::from(insn.words[2] & 0x3ffff) << 32);
                    let target = i64::from(index) * 16 + 16 + (((raw << 14) as i64) >> 14);
                    if target < 0 || target % 16 != 0 {
                        return program.fail(index, "native call destination alignment");
                    }
                    let link = code[index as usize - 1].bits(16, 8);
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
                let mut branch_predicate = lowering.predicate;
                if insn.bits(87, 4) != 7 {
                    if insn.bits(12, 4) != 7 {
                        program.relocations.push(Relocation {
                            output_index: lowering.instructions.len() as u32,
                            target: i64::from(index + 1),
                        });
                        lowering.emit(0xe24000000000000f | (u64::from(insn.bits(12, 4) ^ 8) << 16));
                    }
                    branch_predicate = u64::from(insn.bits(87, 4)) << 16;
                }
                let raw = u64::from(insn.words[1]) | (u64::from(insn.words[2] & 0x3ffff) << 32);
                let target = i64::from(index) * 16 + 16 + (((raw << 14) as i64) >> 14);
                if target < 0 || target % 16 != 0 {
                    return program.fail(index, "native branch destination alignment");
                }
                program.relocations.push(Relocation {
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
                            program.main_exited = true;
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
    output_index: u32,
    target: i64,
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
            let target = self.result.source_instruction_offsets[relocation.target as usize];
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
                0x3c4
                    | 0x2bd
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
                    | 0x299
                    | 0xc09
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
    fn new(insn: &mut Instruction, opcode: &mut u32) -> Result<Self, &'static str> {
        let mut rewritten_uniform = false;
        let mut uniform_guard = false;
        let mut uniform_output = 7;
        if matches!(
            *opcode,
            0x2a4 | 0x8a4 | 0x891 | 0x890 | 0x290 | 0x292 | 0x892 | 0x291
        ) {
            if insn.bits(12, 4) != 7 && *opcode != 0x292 && *opcode != 0x892 {
                return Err("native uniform arithmetic predication");
            }
            uniform_guard = insn.bits(12, 4) != 7;
            rewritten_uniform = true;
            let dst = InstructionLowering::uniform_register(insn.bits(16, 8));
            let a = InstructionLowering::uniform_register(insn.bits(24, 8));
            let c = InstructionLowering::uniform_register(insn.bits(64, 8));
            if dst == u32::MAX || a == u32::MAX || c == u32::MAX {
                return Err("native uniform arithmetic register");
            }
            insn.words[0] = (insn.words[0] & 0xffff) | (dst << 16) | (a << 24);
            insn.words[2] = (insn.words[2] & !0x080000ff) | c;
            if matches!(*opcode, 0x2a4 | 0x290 | 0x292 | 0x291) {
                let b = InstructionLowering::uniform_register(insn.bits(32, 8));
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
                0x8a4 => 0x824,
                0x891 => 0x811,
                0x890 => 0x810,
                0x292 => 0x212,
                0x892 => 0x812,
                0x291 => 0x211,
                _ => 0x210,
            };
        }
        let uniform_predication = matches!(*opcode, 0xabb | 0xab9 | 0xc82 | 0x882);
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

// Mechanical context for LowerToMaxwell's per-instruction captured lambdas.
// Their state and emitted words stay in this upstream-owned file.
#[allow(dead_code)]
struct InstructionLowering<'a> {
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
    fn lower_effective_address(&mut self, opcode: u32) -> Result<(), &'static str> {
        let insn = self.insn;
        match opcode {
            0xa11 => {
                if insn.bits(72, 1) != 0
                    || insn.bits(74, 1) != 0
                    || insn.bits(80, 1) != 0
                    || insn.bits(81, 3) == 7
                    || insn.bits(62, 2) != 0
                {
                    return Err("native constant LEA carry mode");
                }
                let b = self.operand(1, false);
                self.emit(
                    self.base(0x5c18000000000000)
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(b) << 20)
                        | (u64::from(insn.bits(75, 5)) << 39),
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
            }
            0x211 | 0x811 => {
                if insn.bits(80, 1) != 0 {
                    if insn.bits(72, 1) != 0
                        || insn.bits(74, 1) != 0
                        || insn.bits(81, 3) != 7
                        || insn.bits(64, 8) != 255
                        || (opcode == 0x211 && insn.bits(62, 2) != 0)
                    {
                        return Err("native high effective address mode");
                    }
                    let mut b = insn.bits(32, 8);
                    if opcode == 0x811 {
                        b = if insn.words[1] != 0 { 251 } else { 255 };
                        if b == 251 {
                            self.mov_imm(251, insn.words[1]);
                        }
                    }
                    let shift = (u64::from(32 - insn.bits(75, 5)) << 20)
                        | (u64::from(insn.bits(73, 1)) << 48);
                    if b == 255 {
                        self.emit(
                            self.base(0x3828000000000000)
                                | (u64::from(insn.bits(24, 8)) << 8)
                                | shift,
                        );
                    } else {
                        self.emit(
                            0x3828000000000000
                                | self.predicate
                                | 250
                                | (u64::from(insn.bits(24, 8)) << 8)
                                | shift,
                        );
                        self.emit(
                            self.base(0x5c10000000000000) | (250u64 << 8) | (u64::from(b) << 20),
                        );
                    }
                    return Ok(());
                }
                if insn.bits(72, 1) != 0
                    || insn.bits(74, 1) != 0
                    || insn.bits(80, 1) != 0
                    || insn.bits(81, 3) != 7
                    || (opcode == 0x211 && insn.bits(62, 1) != 0)
                {
                    return Err("native LEA carry/high mode");
                }
                let mut b = insn.bits(32, 8);
                if opcode == 0x811 {
                    self.mov_imm(251, insn.words[1]);
                    b = 251;
                }
                self.emit(
                    self.base(0x5c18000000000000)
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(b) << 20)
                        | (u64::from(insn.bits(75, 5)) << 39)
                        | (u64::from(if opcode == 0x211 { insn.bits(63, 1) } else { 0 }) << 48),
                );
            }
            _ => unreachable!("opcode outside upstream LEA arms"),
        }
        Ok(())
    }

    fn lower_integer_add(
        &mut self,
        opcode: u32,
        rewritten_uniform: bool,
    ) -> Result<(), &'static str> {
        let insn = self.insn;
        assert!(matches!(opcode, 0x210 | 0x810 | 0xa10 | 0xc10));
        if matches!(opcode, 0x210 | 0x810)
            && insn.bits(64, 8) == 255
            && insn.bits(73, 1) == 0
            && insn.bits(75, 2) == 0
            && insn.bits(84, 3) == 7
            && insn.bits(77, 4) == 15
            && insn.bits(72, 1) == 0
            && (insn.bits(81, 3) != 7 || insn.bits(74, 1) != 0)
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
            0x207 | 0x807 | 0xa07 | 0x208 | 0x808 | 0xa08 => {
                let a = self.operand(0, false);
                let b = self.operand(1, false);
                self.emit(
                    self.base(0x5ca0000000000000)
                        | (u64::from(a) << 8)
                        | (u64::from(b) << 20)
                        | (u64::from(insn.bits(87, 3)) << 39)
                        | (u64::from(insn.bits(90, 1)) << 42),
                );
            }
            0x20a | 0x80a | 0xa0a => {
                if insn.bits(64, 8) != 0
                    || insn.bits(74, 2) == 3
                    || insn.bits(81, 6) != 0
                    || insn.bits(91, 1) != 0
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
            0x434 => {
                self.mov_imm(250, insn.words[1]);
                self.emit(
                    0x5d20000000000000
                        | self.predicate
                        | u64::from(insn.bits(84, 3))
                        | (u64::from(insn.bits(81, 3)) << 3)
                        | (u64::from(insn.bits(80, 1)) << 6)
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (250u64 << 20)
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
            0x20b | 0x80b | 0xa0b | 0x20c | 0x80c | 0xa0c | 0xc0c => {
                let floating = opcode & 0x1ff == 0xb;
                if !floating && insn.bits(72, 1) != 0 {
                    return Err("native extended integer comparison");
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
                    let b = Self::uniform_register(insn.bits(32, 8));
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
                    || (opcode != 0xa17
                        && insn.words[1] > if opcode == 0x817 { 0x7ffff } else { 255 })
                    || (opcode == 0xa17 && (!self.valid_cbuf() || insn.bits(62, 2) != 0))
                {
                    return Err("native integer min/max mode");
                }
                let source = if opcode == 0xc17 {
                    Self::uniform_register(insn.words[1])
                } else if opcode == 0xa17 {
                    self.operand(1, false)
                } else {
                    insn.words[1]
                };
                if source == u32::MAX {
                    return Err("native uniform min/max operand");
                }
                self.emit(
                    self.base(if opcode == 0x817 {
                        0x3820000000000000
                    } else {
                        0x5c20000000000000
                    }) | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(source) << 20)
                        | (u64::from(insn.bits(87, 4)) << 39)
                        | (u64::from(insn.bits(73, 1)) << 48),
                );
            }
            0x819 => {
                let is_signed = insn.bits(73, 2) == 2;
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
                if (insn.bits(73, 2) != 3 && !is_signed)
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
        native_predicate: u32,
    ) -> Result<(), &'static str> {
        let insn = self.insn;
        match opcode {
            0x308 | 0xb08 => {
                let operation = insn.bits(74, 6);
                if operation > 8 || operation == 6 || operation == 7 || insn.bits(80, 12) != 0 {
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
                    let mut src = self.operand(1, true);
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
                let index_register = Self::uniform_register(insn.bits(24, 8));
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
                if insn.words[2] != 0 || insn.words[1] > 32 || self.dest >= 255 {
                    return Err("native sign extend mode");
                }
                self.emit(
                    self.base(0x3800000000000000)
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (u64::from(insn.words[1] << 8) << 20),
                );
            }
            0x299 => {
                let dst = Self::uniform_register(insn.bits(16, 8));
                let low = Self::uniform_register(insn.bits(24, 8));
                let shift = Self::uniform_register(insn.bits(32, 8));
                let high = Self::uniform_register(insn.bits(64, 8));
                let right = insn.bits(76, 1) != 0;
                let high_result = insn.bits(80, 1) != 0;
                if dst == u32::MAX
                    || dst == 255
                    || low == u32::MAX
                    || shift == u32::MAX
                    || high == u32::MAX
                    || native_predicate != 7
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
            0x419 => {
                if insn.bits(24, 8) != 255 || insn.words[2] & !0xff != 0x00010e00 {
                    return Err("native funnel shift immediate mode");
                }
                self.mov_imm(250, insn.words[1]);
                self.emit(
                    self.base(0x5c48000000000000)
                        | (250u64 << 8)
                        | (u64::from(insn.bits(64, 8)) << 20)
                        | (1u64 << 39),
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
                    Self::uniform_register(insn.bits(32, 8))
                } else {
                    insn.bits(32, 8)
                };
                if opcode == 0x306 && insn.words[2] == 0x00101000 && insn.words[1] & 0xcfffff00 == 0
                {
                    self.emit(
                        self.base(0x5cb8000000000000)
                            | (2u64 << 8)
                            | (1u64 << 10)
                            | (u64::from(source) << 20)
                            | (u64::from(insn.bits(60, 2) * 2) << 41),
                    );
                    return Ok(());
                }
                if insn.words[2] != if opcode == 0xd06 { 0x08001000 } else { 0x1000 }
                    || insn.words[1] & 0xffffff00 != 0
                    || source == u32::MAX
                {
                    return Err("native byte-to-float conversion mode");
                }
                self.emit(self.base(0x5cb8000000000000) | (2u64 << 8) | (u64::from(source) << 20));
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
            0x307 => {
                if insn.bits(75, 2) != 2
                    || insn.bits(84, 2) != 2
                    || insn.bits(81, 3) != 0
                    || insn.bits(86, 6) != 0
                {
                    return Err("native floating round format");
                }
                self.emit(
                    self.base(0x5ca8000000000000)
                        | (2u64 << 8)
                        | (2u64 << 10)
                        | (u64::from(insn.bits(32, 8)) << 20)
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
                    if insn.words[1] >= 16 || insn.words[2] != 0x08000f00 {
                        return Err("native uniform move source");
                    }
                    self.emit(
                        self.base(0x5c98000000000000)
                            | (15u64 << 39)
                            | (u64::from(insn.words[1] + 216) << 20),
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
            0xb63 => {
                let mask = insn.bits(72, 4);
                let second_dest = insn.bits(64, 8);
                let source_a = insn.bits(24, 8);
                let source_b = insn.bits(32, 8);
                let binding = insn.bits(54, 5);
                if insn.bits(61, 3) != 1
                    || insn.bits(76, 4) != 0
                    || insn.bits(81, 3) != 7
                    || insn.bits(59, 1) != 0
                    || insn.bits(40, 14) > 0x1fff
                    || binding >= 18
                    || insn.bits(89, 1) != 0
                    || mask == 0
                    || (*required_texture_buffer != u32::MAX && *required_texture_buffer != binding)
                {
                    return Err("native texture gather mode");
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
                        | (u64::from(source_b) << 20),
                );
                self.emit(
                    0xc800000000000000
                        | self.predicate
                        | 250
                        | (250u64 << 8)
                        | (255u64 << 20)
                        | (2u64 << 28)
                        | (u64::from(mask) << 31)
                        | (u64::from(insn.bits(40, 14)) << 36)
                        | (u64::from(insn.bits(90, 1)) << 49)
                        | (7u64 << 51)
                        | (u64::from(insn.bits(87, 2)) << 56),
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
            0xb60 => {
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
                if dimension == 5 && depth_compare && lod == 0 && !split && mask == 1 {
                    if insn.bits(60, 1) == 0
                        || insn.bits(76, 2) != 0
                        || insn.bits(81, 3) != 7
                        || insn.bits(59, 1) != 0
                        || insn.bits(40, 14) > 0x1fff
                        || insn.bits(54, 5) != 2
                        || source_a & 1 != 0
                        || source_b & 1 != 0
                        || source_a >= 248
                        || source_b >= 248
                        || self.dest >= 248
                    {
                        return Err("native array depth comparison sample mode");
                    }
                    *required_texture_buffer = 2;
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
                        || insn.bits(59, 1) != 0
                        || insn.bits(40, 14) > 0x1fff
                        || insn.bits(54, 5) >= 18
                        || (lod > 3 && lod != 6 && lod != 7)
                        || (depth_compare && mask != 1)
                        || mask == 0
                    {
                        return false;
                    }
                    let coordinates = COORDINATE_COUNT[dimension as usize];
                    let has_lod = matches!(lod, 2 | 3 | 6 | 7);
                    let count = coordinates
                        + u32::from(has_lod)
                        + u32::from(offsets)
                        + u32::from(depth_compare);
                    let in_a = if count <= 4 { (count + 1) / 2 } else { 4 };
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
                    let binding = insn.bits(54, 5);
                    if *required_texture_buffer != u32::MAX && *required_texture_buffer != binding {
                        return false;
                    }
                    *required_texture_buffer = binding;
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
                    let mut next = coordinates;
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
                    this.emit(
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
                    );
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
                                    | (246u64 << 8)
                                    | (247u64 << 20)
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
                if insn.bits(60, 1) == 0
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
                let half = opcode == 0x305 && mode == 0x0010f000;
                let small = mode == 0x00212800;
                let floor = (mode & !0x10100) == 0x00207000;
                let nearest = (mode & !0x10100) == 0x00203000;
                if (!half && !small && !floor && !nearest && (mode & !0x100) != 0x0021f000)
                    || (opcode != 0xb05 && (insn.words[1] & 0xffffff00) != 0)
                    || (opcode == 0xd05 && Self::uniform_register(insn.bits(32, 8)) == u32::MAX)
                {
                    return Err("native float-to-integer conversion mode");
                }
                let is_signed = insn.bits(72, 1) != 0;
                let mut source = if opcode == 0xb05 {
                    self.materialize(2, 0, 252, false, false)
                } else if opcode == 0xd05 {
                    Self::uniform_register(insn.bits(32, 8))
                } else {
                    insn.bits(32, 8)
                };
                if half {
                    self.extract_half(
                        HalfSource {
                            reg: source,
                            swizzle: 2,
                            absolute: false,
                            negate: false,
                        },
                        0,
                        252,
                    );
                    source = 252;
                }
                if !is_signed {
                    self.emit(
                        0x5800000000000000
                            | self.predicate
                            | 250
                            | (u64::from(source) << 8)
                            | (u64::from(source) << 20)
                            | (7u64 << 39)
                            | (7u64 << 48),
                    );
                    self.emit(
                        0x5c40000000000000
                            | self.predicate
                            | 250
                            | (u64::from(source) << 8)
                            | (250u64 << 20)
                            | (7u64 << 48),
                    );
                    if !small {
                        self.mov_imm(252, 0x4f800000);
                        self.emit(
                            0x5800000000000000
                                | self.predicate
                                | 251
                                | (250u64 << 8)
                                | (252u64 << 20)
                                | (7u64 << 39)
                                | (6u64 << 48),
                        );
                        self.mov_imm(252, 0x4f7fffff);
                        self.emit(
                            0x5c60000000000000
                                | self.predicate
                                | 250
                                | (250u64 << 8)
                                | (252u64 << 20)
                                | (7u64 << 39),
                        );
                    }
                    source = 250;
                }
                self.emit(
                    self.base(0x5cb0000000000000)
                        | ((if small { 1u64 } else { 2 }) << 8)
                        | (2u64 << 10)
                        | (u64::from(is_signed) << 12)
                        | (u64::from(source) << 20)
                        | ((if small || nearest {
                            0u64
                        } else if floor {
                            1
                        } else {
                            3
                        }) << 39)
                        | (u64::from(insn.bits(80, 1)) << 44),
                );
                if !is_signed && !small {
                    self.emit(
                        self.base(0x5c40000000000000)
                            | (self.dest << 8)
                            | (251u64 << 20)
                            | (1u64 << 41)
                            | (7u64 << 48),
                    );
                }
            }
            0xb6f => {
                if insn.bits(59, 5) != 0
                    || insn.bits(64, 8) != 255
                    || insn.bits(72, 4) == 0
                    || insn.bits(72, 4) > 3
                    || insn.bits(76, 14) != 0
                    || insn.bits(91, 1) != 0
                    || insn.bits(54, 5) != 2
                    || insn.bits(40, 14) > 0x1fff
                    || (insn.bits(72, 4) == 3 && self.dest & 1 != 0)
                    || self.dest >= 254
                {
                    return Err("native texture dimension query mode");
                }
                *required_texture_buffer = 2;
                self.emit(
                    0xdf48000000000000
                        | self.predicate
                        | self.dest
                        | (u64::from(insn.bits(24, 8)) << 8)
                        | (1u64 << 22)
                        | (u64::from(insn.bits(72, 4)) << 31)
                        | (u64::from(insn.bits(40, 14)) << 36)
                        | (u64::from(insn.bits(90, 1)) << 49),
                );
            }
            0xb66 => {
                let mask = insn.bits(72, 4);
                let dimension = insn.bits(61, 3);
                let array = dimension == 5;
                let volume = dimension == 2;
                let source_a = insn.bits(24, 8);
                let mut source_b = insn.bits(32, 8);
                let second_dest = insn.bits(64, 8);
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
                    || insn.bits(76, 5) != 0
                    || insn.bits(81, 3) != 7
                    || insn.bits(87, 3) != 1
                    || insn.bits(59, 1) != 0
                    || insn.bits(40, 14) > 0x1fff
                    || insn.bits(54, 5) >= 18
                    || self.dest == 255
                    || (mask & (mask - 1) != 0 && (self.dest & 1 != 0 || self.dest >= 254))
                    || (split && mask == 15 && (second_dest & 1 != 0 || second_dest >= 254))
                    || (array && (source_a & 1 != 0 || source_a >= 249))
                    || (volume && (source_a & 1 != 0 || source_a >= 254))
                {
                    return Err("native texel fetch mode or descriptor binding");
                }
                let binding = insn.bits(54, 5);
                if *required_texture_buffer != u32::MAX && *required_texture_buffer != binding {
                    return Err("native texture descriptor buffer conflict");
                }
                *required_texture_buffer = binding;
                if array {
                    self.emit(
                        0x5c98000000000000
                            | self.predicate
                            | 250
                            | (15u64 << 39)
                            | (u64::from(source_a + 1) << 20),
                    );
                    self.emit(
                        0x5c98000000000000
                            | self.predicate
                            | 251
                            | (15u64 << 39)
                            | (u64::from(source_b) << 20),
                    );
                    source_b = 250;
                }
                self.emit(
                    0xda00000000000000
                        | self.predicate
                        | self.dest
                        | (u64::from(source_a) << 8)
                        | (u64::from(source_b) << 20)
                        | (u64::from(insn.bits(64, 8)) << 28)
                        | (u64::from(insn.bits(40, 14)) << 36)
                        | (u64::from(insn.bits(90, 1)) << 49)
                        | (u64::from(swizzle) << 50)
                        | ((if array {
                            8u64
                        } else if volume {
                            7
                        } else if dimension == 0 {
                            0
                        } else {
                            2
                        }) << 53),
                );
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
    ) -> Result<(), &'static str> {
        let insn = self.insn;
        match opcode {
            0x919 | 0x9c3 => {
                let special = insn.bits(72, 8);
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
                    Self::uniform_register(self.dest as u32)
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
                let a = Self::uniform_register(insn.bits(24, 8));
                let b = if opcode == 0x88c {
                    252
                } else {
                    Self::uniform_register(insn.bits(32, 8))
                };
                if output > 6
                    || insn.bits(84, 3) != 7
                    || insn.bits(87, 4) != 7
                    || insn.bits(72, 4) & !2 != 0
                    || insn.bits(12, 4) != 7
                    || a == u32::MAX
                    || b == u32::MAX
                {
                    return Err("native uniform comparison mode");
                }
                let temporary = 6 - output;
                let mask = 1u32 << temporary;
                if opcode == 0x88c {
                    self.mov_imm(252, insn.words[1]);
                }
                self.emit(0x38e8000000070000 | 253 | (255u64 << 8) | (u64::from(mask) << 20));
                self.emit(
                    0x5b60000000070000
                        | 7
                        | (u64::from(temporary) << 3)
                        | (u64::from(a) << 8)
                        | (u64::from(b) << 20)
                        | (7u64 << 39)
                        | (u64::from(insn.bits(73, 1)) << 48)
                        | (u64::from(insn.bits(76, 3)) << 49),
                );
                self.emit(0x38e8000000070000 | 239 | (239u64 << 8) | (u64::from(mask) << 20));
                self.emit(0x38f0000000070000 | (253u64 << 8) | (u64::from(mask) << 20));
            }
            0x287 | 0x887 => {
                let condition = insn.bits(87, 3);
                let target = Self::uniform_register(self.dest as u32);
                let a = Self::uniform_register(insn.bits(24, 8));
                let b = if opcode == 0x887 {
                    252
                } else {
                    Self::uniform_register(insn.bits(32, 8))
                };
                if condition > 6
                    || target == u32::MAX
                    || a == u32::MAX
                    || b == u32::MAX
                    || insn.bits(12, 4) != 7
                    || (insn.words[2] & !0x07800000) != 0x08000000
                {
                    return Err("native uniform select mode");
                }
                if opcode == 0x887
                    && index > 0
                    && condition == 0
                    && insn.words[1] <= 0x7ffff
                    && code[index as usize - 1].bits(0, 12) == 0x88c
                    && code[index as usize - 1].bits(24, 8) == insn.bits(24, 8)
                    && code[index as usize - 1].words[1] == insn.words[1]
                    && code[index as usize - 1].words[2] == 0x0bf01070
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
                    self.mov_imm(252, insn.words[1]);
                }
                let temporary = 6 - condition;
                let mask = 1u32 << temporary;
                self.emit(0x38e8000000070000 | 253 | (255u64 << 8) | (u64::from(mask) << 20));
                self.emit(0x38f0000000070000 | (239u64 << 8) | (u64::from(mask) << 20));
                self.emit(
                    0x5ca0000000070000
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
                        Self::uniform_register(insn.bits(32, 6))
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
                    let uniform_address = Self::uniform_register(insn.bits(64, 6));
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

    fn lower_matrix_multiply(&mut self) -> Result<(), &'static str> {
        // Upstream function-local static getenv: snapshot on the first HMMA.
        static MODE: std::sync::OnceLock<Option<std::ffi::OsString>> = std::sync::OnceLock::new();
        let Some(mode) = MODE.get_or_init(|| std::env::var_os("EDEN_NX2_TENSOR")) else {
            return Err("native matrix multiply (set EDEN_NX2_TENSOR to emulate)");
        };
        let insn = self.insn;
        let a = insn.bits(24, 8);
        let b = insn.bits(32, 8);
        let c = insn.bits(64, 8);
        if (insn.words[1] & !0xff) != 0
            || (insn.words[2] & !0xff) != 0x800
            || self.dest + 2 > 246
            || a + 4 > 246
            || b + 2 > 246
            || (c != 255 && c + 2 > 246)
        {
            return Err("native matrix multiply mode");
        }
        if mode == "stub" {
            // Explicit upstream experimental mode, never selected by default.
            for row in 0..2u32 {
                if c == 255 {
                    self.mov_imm(self.dest as u32 + row, 0);
                } else if u64::from(c) != self.dest {
                    self.emit(
                        0x5c98000000000000
                            | self.predicate
                            | (self.dest + u64::from(row))
                            | (15u64 << 39)
                            | (u64::from(c + row) << 20),
                    );
                }
            }
            return Ok(());
        }
        // The explicit context argument preserves the local C++ helper boundaries
        // without simultaneous mutable captures of the emitted instruction vector.
        let shuffle = |this: &mut Self, target: u32, source_reg: u32| {
            this.emit(
                0xef10000000000000
                    | this.predicate
                    | u64::from(target)
                    | (u64::from(source_reg) << 8)
                    | (246u64 << 20)
                    | (1u64 << 29)
                    | (31u64 << 34)
                    | (7u64 << 48),
            );
        };
        let lane_a = |this: &mut Self, i: u32| {
            this.emit(0xf0c8000000000000 | this.predicate | 246);
            this.emit(0x0400000000000000 | this.predicate | 246 | (246u64 << 8) | (0x1cu64 << 20));
            if i != 0 {
                this.emit(
                    0x1c00000000000000
                        | this.predicate
                        | 246
                        | (246u64 << 8)
                        | (u64::from(i) << 20),
                );
            }
        };
        let lane_b = |this: &mut Self, add: u32| {
            this.emit(0xf0c8000000000000 | this.predicate | 246);
            this.emit(0x0400000000000000 | this.predicate | 246 | (246u64 << 8) | (3u64 << 20));
            this.emit(0x3848000000000000 | this.predicate | 246 | (246u64 << 8) | (3u64 << 20));
            if add != 0 {
                this.emit(
                    0x1c00000000000000
                        | this.predicate
                        | 246
                        | (246u64 << 8)
                        | (u64::from(add) << 20),
                );
            }
        };
        let hfma2 = |this: &mut Self, accumulator: u32, swizzle_a: u32, pair: u32| {
            this.emit(
                0x5d00000000000000
                    | this.predicate
                    | u64::from(accumulator)
                    | (251u64 << 8)
                    | (u64::from(pair) << 20)
                    | (u64::from(accumulator) << 39)
                    | (u64::from(swizzle_a) << 47),
            );
        };
        for row in 0..2u32 {
            if c == 255 {
                self.mov_imm(252 + row, 0);
            } else {
                self.emit(
                    0x5c98000000000000
                        | self.predicate
                        | u64::from(252 + row)
                        | (15u64 << 39)
                        | (u64::from(c + row) << 20),
                );
            }
        }
        for i in 0..4u32 {
            for k_block in 0..2u32 {
                lane_b(self, i);
                shuffle(self, 247, b + k_block);
                lane_b(self, 4 + i);
                shuffle(self, 248, b + k_block);
                self.emit(
                    0x0400000000000000 | self.predicate | 249 | (247u64 << 8) | (0xffffu64 << 20),
                );
                self.emit(
                    0x5c18000000000000
                        | self.predicate
                        | 249
                        | (248u64 << 8)
                        | (249u64 << 20)
                        | (16u64 << 39),
                );
                self.emit(
                    0x3828000000000000 | self.predicate | 250 | (247u64 << 8) | (16u64 << 20),
                );
                self.emit(
                    0x0400000000000000
                        | self.predicate
                        | 248
                        | (248u64 << 8)
                        | (0xffff0000u64 << 20),
                );
                self.emit(
                    0x5c10000000000000 | self.predicate | 250 | (250u64 << 8) | (248u64 << 20),
                );
                for row in 0..2u32 {
                    lane_a(self, i);
                    shuffle(self, 251, a + k_block * 2 + row);
                    hfma2(self, 252 + row, 2, 249);
                    hfma2(self, 252 + row, 3, 250);
                }
            }
        }
        for row in 0..2u32 {
            self.emit(
                0x5c98000000000000
                    | self.predicate
                    | (self.dest + u64::from(row))
                    | (15u64 << 39)
                    | (u64::from(252 + row) << 20),
            );
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
            0x219 | 0xc19 => {
                let low = insn.bits(24, 8);
                let high = insn.bits(64, 8);
                let shift = if opcode == 0xc19 {
                    Self::uniform_register(insn.bits(32, 8))
                } else {
                    insn.bits(32, 8)
                };
                let right = insn.bits(76, 1) != 0;
                let high_result = insn.bits(80, 1) != 0;
                if right
                    && !high_result
                    && high == 255
                    && insn.bits(73, 2) == 3
                    && shift != u32::MAX
                    && insn.words[1] <= 255
                    && (insn.words[2]
                        & !(0x000118ff | if opcode == 0xc19 { 0x08000000 } else { 0 }))
                        == 0x00000600
                {
                    self.emit(
                        self.base(0x5c28000000000000)
                            | (u64::from(low) << 8)
                            | (u64::from(shift) << 20),
                    );
                    return Ok(());
                }
                if insn.words[1] > 255
                    || insn.bits(73, 2) != 3
                    || shift == u32::MAX
                    || (insn.words[2]
                        & !(0x000118ff | if opcode == 0xc19 { 0x08000000 } else { 0 }))
                        != 0x00000600
                    || if right {
                        !high_result || low != 255
                    } else {
                        high_result || high != 255
                    }
                {
                    return Err("native register shift mode");
                }
                self.emit(
                    self.base(if right {
                        0x5cf8000000000000
                    } else {
                        0x5bf8000000000000
                    }) | (u64::from(if right { high } else { 255 }) << 8)
                        | (u64::from(shift) << 20)
                        | (u64::from(if right { 255 } else { low }) << 39)
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
                    Self::uniform_register(insn.bits(32, 8))
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
                if (insn.words[2] & !0xff) != 0x001e0f00
                    || insn.bits(59, 2) != 0
                    || insn.bits(61, 3) != 5
                    || binding >= 18
                    || offset > 0x1fff
                    || self.dest & 1 != 0
                    || self.dest >= 248
                    || u64::from(insn.bits(64, 8)) != self.dest + 2
                    || coordinates >= 248
                    || derivatives >= 247
                {
                    return Err("native array gradient texture mode");
                }
                if *required_texture_buffer != u32::MAX && *required_texture_buffer != binding {
                    return Err("native texture descriptor buffer conflict");
                }
                *required_texture_buffer = binding;
                self.emit(
                    self.base(0xde00000000000000)
                        | (u64::from(coordinates) << 8)
                        | (u64::from(derivatives) << 20)
                        | (3u64 << 28)
                        | (15u64 << 31)
                        | (u64::from(offset) << 36)
                        | (u64::from(insn.bits(90, 1)) << 49)
                        | (7u64 << 51),
                );
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
                if (insn.words[2] & !0x0380003f) == 0x0c10e100 && insn.bits(24, 8) == 255 {
                    let address = Self::uniform_register(insn.bits(64, 6));
                    let offset = ((insn.bits(40, 24) << 8) as i32) >> 8;
                    if address == u32::MAX
                        || address & 1 != 0
                        || !(-0x80000..=0x7ffff).contains(&offset)
                    {
                        return Err("native global reduction uniform address");
                    }
                    self.emit(
                        0xebf8000000000000
                            | self.predicate
                            | u64::from(insn.bits(32, 8))
                            | (u64::from(address) << 8)
                            | (u64::from(insn.bits(87, 3)) << 23)
                            | (u64::from((offset as u32) & 0xfffff) << 28)
                            | (1u64 << 48),
                    );
                } else {
                    if (insn.words[2] & !0x03800000) != 0x0010e100
                        || insn.bits(40, 24) != 0
                        || insn.bits(24, 8) & 1 != 0
                    {
                        return Err("native global reduction mode");
                    }
                    self.emit(
                        0xebf8000000000000
                            | self.predicate
                            | u64::from(insn.bits(32, 8))
                            | (u64::from(insn.bits(24, 8)) << 8)
                            | (u64::from(insn.bits(87, 3)) << 23)
                            | (1u64 << 48),
                    );
                }
            }
            0x2bd => {
                if self.dest >= 16
                    || insn.words[1] >= 16
                    || (insn.words[2] & !0x400) != 0x080e0000
                    || native_predicate != 7
                {
                    return Err("native uniform leading bit mode");
                }
                self.emit(
                    0x5c30000000000000
                        | self.predicate
                        | (self.dest + 216)
                        | (u64::from(insn.words[1] + 216) << 20)
                        | (u64::from(insn.bits(74, 1)) << 41),
                );
            }
            // Pinned Eglantine 900ab5a8: UPOPC reads/writes the uniform mirrors.
            0x2bf => {
                let target = Self::uniform_register(self.dest as u32);
                let source = Self::uniform_register(insn.bits(32, 8));
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
                if self.dest >= 16
                    || insn.words[1] >= 16
                    || insn.words[2] != 0x08000000
                    || native_predicate != 7
                {
                    return Err("native uniform bit reverse mode");
                }
                self.emit(
                    0x3828000000000000
                        | self.predicate
                        | (self.dest + 216)
                        | (u64::from(insn.words[1] + 216) << 8)
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
                let target = Self::uniform_register(self.dest as u32);
                let maximum = insn.words[2] == 0x14000;
                if target == u32::MAX
                    || target == 255
                    || insn.words[1] != 0
                    || (insn.words[2] != 0xc000 && !maximum)
                    || native_predicate != 7
                {
                    return Err("native warp sum mode");
                }
                self.emit(0x50d8000000070000 | 250 | (7u64 << 39) | (7u64 << 45) | (1u64 << 48));
                self.mov_imm(target, 0);
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
                    self.emit(0x3828000000070000 | 252 | (250u64 << 8) | (u64::from(lane) << 20));
                    self.emit(0x0400000000070000 | 252 | (252u64 << 8) | (1u64 << 20));
                    self.emit(
                        0x5c10000000070000 | 252 | (255u64 << 8) | (252u64 << 20) | (1u64 << 48),
                    );
                    self.emit(
                        0x5c40000000070000 | 251 | (251u64 << 8) | (252u64 << 20) | (7u64 << 48),
                    );
                    if maximum {
                        self.emit(
                            0x5c20000000070000
                                | u64::from(target)
                                | (u64::from(target) << 8)
                                | (251u64 << 20)
                                | (15u64 << 39),
                        );
                    } else {
                        self.emit(
                            0x5c10000000070000
                                | u64::from(target)
                                | (u64::from(target) << 8)
                                | (251u64 << 20),
                        );
                    }
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
                let address = Self::uniform_register(insn.bits(64, 6));
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
                if (target >= 16 && target != 63)
                    || insn.words[1] != 0
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
                        | u64::from(Self::uniform_register(target))
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
                let target = Self::uniform_register(self.dest as u32);
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
                let target = Self::uniform_register(insn.bits(16, 8));
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
                if self.dest >= 16 || insn.words[2] != 0 {
                    return Err("native uniform immediate move mode");
                }
                self.mov_imm(self.dest as u32 + 216, insn.words[1]);
            }
            0xc82 => {
                let source = if insn.words[1] < 64 {
                    Self::uniform_register(insn.words[1])
                } else {
                    u32::MAX
                };
                if self.dest >= 16 || source == u32::MAX || insn.words[2] != 0x08000000 {
                    return Err("native uniform move mode");
                }
                self.emit(
                    0x5c98000000000000
                        | self.predicate
                        | (self.dest + 216)
                        | (u64::from(source) << 20)
                        | (15u64 << 39),
                );
            }
            0xab9 | 0xabb => {
                let uniform_dest = insn.bits(16, 8);
                let index_reg = insn.bits(24, 8);
                let width = insn.bits(73, 3);
                let target = Self::uniform_register(uniform_dest);
                let index_target = if opcode == 0xabb {
                    Self::uniform_register(index_reg)
                } else {
                    255
                };
                if target == u32::MAX
                    || target == 255
                    || (width != 4 && width != 5)
                    || (width == 5
                        && (uniform_dest & 1 != 0
                            || Self::uniform_register(uniform_dest + 1) != target + 1))
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
                if insn.words[2] == (0x08011400 | high)
                    && low == 63
                    && high < 16
                    && dst < 16
                    && native_predicate == 7
                    && insn.words[1] <= 32
                {
                    self.emit(
                        0x3828000000000000
                            | self.predicate
                            | u64::from(dst + 216)
                            | (u64::from(high + 216) << 8)
                            | (u64::from(insn.words[1]) << 20)
                            | (1u64 << 48),
                    );
                    return Ok(());
                }
                let right_low = insn.words[2] == 0x0800163f && low < 16;
                let right_high = insn.words[2] == (0x08011600 | high) && low == 63 && high < 16;
                if (right_low || right_high)
                    && dst < 16
                    && native_predicate == 7
                    && insn.words[1] <= 32
                {
                    self.emit(
                        0x3828000000000000
                            | self.predicate
                            | u64::from(dst + 216)
                            | (u64::from((if right_high { high } else { low }) + 216) << 8)
                            | (u64::from(insn.words[1]) << 20),
                    );
                    return Ok(());
                }
                let high_result = insn.words[2] == (0x08010600 | high) && low == 63 && high < 16;
                let low_result = insn.words[2] == 0x0800063f && low < 16;
                if dst >= 16
                    || native_predicate != 7
                    || insn.words[1] > 32
                    || (!high_result && !low_result)
                {
                    return Err("native uniform shift mode");
                }
                self.emit(
                    0x3848000000000000
                        | self.predicate
                        | u64::from(dst + 216)
                        | (u64::from((if high_result { high } else { low }) + 216) << 8)
                        | (u64::from(insn.words[1]) << 20),
                );
            }
            0xb82 => {
                let width = insn.bits(73, 3);
                if (width != 4 && width != 5)
                    || insn.bits(78, 2) != 0
                    || insn.bits(91, 1) != 0
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
                } else {
                    if (insn.words[2] & !0x2ff) != 0x078e0000 || (insn.words[1] & !0xff) != 0 {
                        return Err("native integer register multiply mode");
                    }
                    self.multiply_low(insn.bits(24, 8), insn.bits(32, 8), insn.bits(64, 8), false);
                }
            }
            0x2a5 => {
                let a = Self::uniform_register(insn.bits(24, 8));
                let b = Self::uniform_register(insn.bits(32, 8));
                let c = insn.bits(64, 8);
                let c_low = Self::uniform_register(c);
                let c_high = if c == 63 {
                    255
                } else {
                    Self::uniform_register(c + 1)
                };
                let d_low = Self::uniform_register(insn.bits(16, 8));
                let d_high = Self::uniform_register(insn.bits(16, 8) + 1);
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
            0x827 => {
                if insn.words[2] != 0x078e00ff {
                    return Err("native high multiply mode");
                }
                let a = insn.bits(24, 8);
                let b = insn.words[1];
                self.xmad(250, a, b, 255, false, false, 0, true);
                self.xmad(251, a, b, 250, true, false, 2, true);
                self.xmad(250, a, b, 251, false, true, 1, true);
                self.xmad(252, a, b, 251, true, true, 2, true);
                self.emit(
                    0x3828000000000000 | self.predicate | 250 | (250u64 << 8) | (16u64 << 20),
                );
                self.emit(self.base(0x5c10000000000000) | (250u64 << 8) | (252u64 << 20));
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

    fn uniform_register(reg: u32) -> u32 {
        if reg < 22 {
            216 + reg
        } else if reg < 28 {
            218 + reg
        } else if reg == 63 {
            255
        } else {
            u32::MAX
        }
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
            let mapped = Self::uniform_register(reg);
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
    fn native_uniform_population_count_captured_operands_and_inversion() {
        let exit = Instruction { words: [0x794d, 0, 0x03800000, 0] };
        for (dest, source, target, mirror) in [(9, 8, 225, 224), (4, 7, 220, 223), (22, 63, 240, 255)] {
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
            [0x001c72bf, 8, 0x08000000, 0], // No mirror for UR28.
            [0x000972bf, 28, 0x08000000, 0],
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
        let mut count = 0;
        for (line_number, line) in include_str!("programs.fixture").lines().enumerate() {
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
        assert_eq!(count, 67809);
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
        check_fixture(include_str!("operand_helpers.fixture"), 64528);
    }

    #[test]
    fn tensor_modes_match_upstream_and_are_captured_once() {
        const CHILD: &str = "RUZU_TEST_AMPERE_TENSOR_MODE";
        let Ok(mode) = std::env::var(CHILD) else {
            for mode in ["missing", "stub", "emulate", "empty", "other"] {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command.args(["--exact", "frontend::ampere::lower_to_maxwell::tests::tensor_modes_match_upstream_and_are_captured_once", "--test-threads=1"]);
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
        let fixture = match mode.as_str() {
            "missing" => include_str!("tensor_missing.fixture"),
            "stub" => include_str!("tensor_stub.fixture"),
            "empty" => include_str!("tensor_empty.fixture"),
            _ => include_str!("tensor_emulate.fixture"),
        };
        check_fixture(fixture, 165);
        // This child runs only this test. Changing the environment after the
        // first HMMA must not change its function-local static mode.
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
            let mut lowering = InstructionLowering {
                predicate: u64::from(insn.bits(12, 4)) << 16,
                dest: u64::from(insn.bits(16, 8)),
                insn,
                operand_error: "",
                instructions: &mut emitted,
            };
            let value = match input[0] {
                0 => InstructionLowering::uniform_register(p[0]),
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
                    match UniformRewrite::new(&mut lowering.insn, &mut opcode) {
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
                        0xc24 | 0xe24 | 0x224 | 0x2a5 | 0x827 | 0xa24 | 0x424 | 0x624 | 0x824
                    ) {
                        lowering.lower_integer_multiply(opcode, native_predicate)
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
                    } else if matches!(opcode, 0x305 | 0xb05 | 0xd05 | 0xb6f | 0xb66 | 0x91a) {
                        lowering.lower_conversion_and_fetch(opcode, &mut required_texture_buffer)
                    } else if matches!(opcode, 0xb69 | 0xb63 | 0xb60) {
                        lowering.lower_texture_sample(opcode, &mut required_texture_buffer)
                    } else if matches!(
                        opcode,
                        0x816
                            | 0x307
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
                        lowering.lower_matrix_multiply()
                    } else if matches!(opcode, 0xa11 | 0x211 | 0x811) {
                        lowering.lower_effective_address(opcode)
                    } else if matches!(opcode, 0x210 | 0x810 | 0xa10 | 0xc10) {
                        lowering.lower_integer_add(opcode, p[2] != 0)
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
                            | 0xb08
                            | 0x526
                            | 0x326
                            | 0x81a
                            | 0x299
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
                        lowering.lower_special_and_global(opcode, &code, p[0])
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
                output_index: 0,
                target: 1,
            },
            Relocation {
                output_index: 2,
                target: 0,
            },
            Relocation {
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
        let rewrite = UniformRewrite::new(&mut insn, &mut opcode).unwrap();
        assert!(rewrite.guarded_uniform);
        assert_ne!(rewrite.output_mask, 0);
        let mut words = Vec::new();
        let mut lowering = InstructionLowering {
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

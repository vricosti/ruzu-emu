// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `ir_opt/texture_pass.cpp`
//!
//! Resolves bindless and bound texture operations to indexed texture
//! operations. Tracks constant buffer addresses that reference texture
//! descriptors and rewrites texture instructions with concrete descriptor
//! indices.

use crate::environment::{CbufWordKey, ConstBufferAddr, Environment, HandleKey};
use crate::host_translate_info::HostTranslateInfo;
use crate::ir::basic_block::Block;
use crate::ir::instruction::Inst;
use crate::ir::opcodes::Opcode;
use crate::ir::program::{Program, ShaderInfo, SyntaxNode};
use crate::ir::types::TextureInstInfo;
use crate::ir::value::{InstRef, Value};
use crate::shader_info::{
    num_descriptors, HasCount, ImageBufferDescriptor, ImageDescriptor, ImageFormat,
    TextureBufferDescriptor, TextureDescriptor, TexturePixelFormat, TextureType,
};

const DESCRIPTOR_SIZE_SHIFT: u32 = 3;
const DESCRIPTOR_SIZE: u32 = 8;
const DESCRIPTOR_MAX_COUNT: u32 = 1024;

#[derive(Debug, Clone, Copy)]
struct TextureInst {
    block: u32,
    inst: u32,
    cbuf: ConstBufferAddr,
}

// Native indirect-handle prerequisite. These helpers preserve the conditional
// diamond recognition owned by upstream texture_pass.cpp, not a global CFG pass.
fn conditional_header(program: &Program, body: u32) -> Option<u32> {
    let body_block = program.blocks.get(body as usize)?;
    if body_block.imm_predecessors.len() != 1 || body_block.imm_successors.len() != 1 {
        return None;
    }
    let header = body_block.imm_predecessors[0];
    let header_block = program.blocks.get(header as usize)?;
    let targets = &header_block.imm_successors;
    if targets.len() != 2 || targets[0] != body || targets[1] != body_block.imm_successors[0]
        || program.blocks.get(targets[1] as usize)?.imm_predecessors.len() != 2
        || header_block.indexed_rev_iter().next()?.1.opcode != Opcode::ConditionRef
    {
        return None;
    }
    Some(header)
}

fn condition(program: &Program, mut value: Value) -> (Value, bool) {
    let mut negated = false;
    while let Some(inst) = get_inst_recursive(program, value) {
        if inst.opcode == Opcode::LogicalNot {
            negated = !negated;
        } else if inst.opcode != Opcode::ConditionRef {
            break;
        }
        value = inst.args[0];
    }
    (value.resolve(program), negated)
}

fn resolve_conditional_phi(program: &Program, mut value: Value, block: u32) -> Value {
    if !get_inst_recursive(program, value).is_some_and(|inst| inst.opcode == Opcode::Phi) {
        return value;
    }
    let Some(mut block) = conditional_header(program, block) else { return value };
    let header_condition = program.blocks[block as usize].indexed_rev_iter().next().unwrap().1.args[0];
    let (condition_value, negated) = condition(program, header_condition);
    let mut visited = Vec::new();
    while let Some(phi) = get_inst_recursive(program, value) {
        if phi.opcode != Opcode::Phi || visited.contains(&block) { break; }
        visited.push(block);
        let predecessors = &program.blocks[block as usize].imm_predecessors;
        if predecessors.len() == 1 {
            block = predecessors[0];
            continue;
        }
        if predecessors.len() != 2 || phi.phi_args.len() != 2 { break; }
        let mut body = predecessors[0];
        let mut header = conditional_header(program, body);
        if header != Some(predecessors[1]) {
            body = predecessors[1];
            header = conditional_header(program, body);
            if header != Some(predecessors[0]) { break; }
        }
        let header = header.unwrap();
        let other = program.blocks[header as usize].indexed_rev_iter().next().unwrap().1.args[0];
        let (other_condition, other_negated) = condition(program, other);
        if condition_value == other_condition
            && (phi.phi_args[0].0 == header || phi.phi_args[0].0 == body)
        {
            let incoming = if negated == other_negated { body } else { header };
            value = phi.phi_args[usize::from(phi.phi_args[0].0 != incoming)].1;
        }
        block = header;
    }
    value
}

// Upstream's thread-local BuildHandleProgram state is scoped to one build here;
// no state survives translation or is shared between concurrent compilations.
#[derive(Default)]
struct HandleBuildState {
    slots: std::collections::HashMap<InstRef, u32>,
    slot_count: u32,
    failure: Option<Opcode>,
    // Lichii extension (texture_pass.cpp:643-660, g_handle_index_allowed and
    // friends): a handle read from a table at an index each invocation reads
    // for itself (a light index from a per-pixel light list) has no single
    // value for a draw. With `index_allowed`, the build takes that index word
    // as the element index of a descriptor array instead (HandleOp::Index),
    // and `index` is the word, which the shader then indexes the array with.
    // Only a word read at an address no draw-wide value gives (`load_depth`
    // counts the reads whose address is being built), which is scaled to a
    // byte offset (`scaled`) to give the address of another read, qualifies,
    // and one word per program.
    index_allowed: bool,
    index: Option<InstRef>,
    load_depth: i32,
    scaled: i32,
}

fn is_undefined(op: Opcode) -> bool {
    matches!(op, Opcode::UndefU1 | Opcode::UndefU8 | Opcode::UndefU16
        | Opcode::UndefU32 | Opcode::UndefU64)
}

impl HandleBuildState {
    fn build_handle_program(&mut self, program: &Program, value: Value, code: &mut Vec<u32>,
                            budget: &mut i32, block: u32) -> bool {
        use crate::shader_info::HandleOp;
        let value = value.resolve(program);
        if value.is_immediate() {
            return self.build_handle_node(program, value, code, budget, block);
        }
        let Some(reference) = get_inst_ref_recursive(program, value) else { return false };
        if let Some(&slot) = self.slots.get(&reference) {
            if slot == u32::MAX {
                self.failure = Some(Opcode::Phi);
                return false;
            }
            code.extend([HandleOp::Load as u32, slot]);
            return true;
        }
        self.slots.insert(reference, u32::MAX);
        let start = code.len();
        let first_slot = self.slot_count;
        let index_before = self.index;
        let result = self.build_handle_node(program, value, code, budget, block);
        self.slots.remove(&reference);
        if !result {
            // The Tee of every value the dropped code kept goes with it, and
            // so does an index word the dropped code took.
            self.index = index_before;
            code.truncate(start);
            self.slots.retain(|_, slot| *slot == u32::MAX || *slot < first_slot);
            self.slot_count = first_slot;
        }
        if result && get_inst(program, reference).unwrap().use_count > 1 && self.slots.len() < 256 {
            let slot = self.slot_count;
            self.slot_count += 1;
            self.slots.insert(reference, slot);
            code.extend([HandleOp::Tee as u32, slot]);
        }
        result
    }

    fn build_handle_node(&mut self, program: &Program, value: Value, code: &mut Vec<u32>,
                         budget: &mut i32, block: u32) -> bool {
        use crate::shader_info::HandleOp as H;
        let value = value.resolve(program);
        *budget -= 1;
        if *budget < 0 { return false; }
        if value.is_immediate() {
            code.extend([H::Immediate as u32, value.imm_u32()]);
            return true;
        }
        let Some(inst) = get_inst_recursive(program, value) else { return false };
        let build = |state: &mut Self, arg: usize, code: &mut Vec<u32>, budget: &mut i32| {
            state.build_handle_program(program, *inst.arg(arg), code, budget, block)
        };
        let mapped = match inst.opcode {
            Opcode::IAdd32 => Some((H::Add, 2)), Opcode::ISub32 => Some((H::Sub, 2)),
            Opcode::BitwiseAnd32 => Some((H::And, 2)),
            Opcode::BitwiseOr32 => Some((H::Or, 2)), Opcode::BitwiseXor32 => Some((H::Xor, 2)),
            Opcode::ShiftRightLogical32 => Some((H::Shr, 2)),
            Opcode::ShiftRightArithmetic32 => Some((H::Sar, 2)),
            Opcode::INeg32 => Some((H::Neg, 1)), Opcode::BitwiseNot32 => Some((H::Not, 1)),
            Opcode::UMin32 => Some((H::UMin, 2)), Opcode::UMax32 => Some((H::UMax, 2)),
            Opcode::SMin32 => Some((H::SMin, 2)), Opcode::SMax32 => Some((H::SMax, 2)),
            Opcode::SLessThan => Some((H::SLt, 2)), Opcode::SLessThanEqual => Some((H::SLe, 2)),
            Opcode::SGreaterThan => Some((H::SGt, 2)), Opcode::SGreaterThanEqual => Some((H::SGe, 2)),
            Opcode::ULessThan => Some((H::ULt, 2)), Opcode::ULessThanEqual => Some((H::ULe, 2)),
            Opcode::UGreaterThan => Some((H::UGt, 2)), Opcode::UGreaterThanEqual => Some((H::UGe, 2)),
            Opcode::IEqual => Some((H::Eq, 2)), Opcode::INotEqual => Some((H::Ne, 2)),
            Opcode::LogicalAnd => Some((H::LAnd, 2)), Opcode::LogicalOr => Some((H::LOr, 2)),
            Opcode::LogicalXor => Some((H::LXor, 2)), Opcode::LogicalNot => Some((H::LNot, 1)),
            Opcode::SelectU1 | Opcode::SelectU32 => Some((H::Select, 3)),
            _ => None,
        };
        if let Some((op, count)) = mapped {
            for arg in 0..count { if !build(self, arg, code, budget) { return false; } }
            code.push(op as u32);
            return true;
        }
        match inst.opcode {
            Opcode::IMul32 | Opcode::ShiftLeftLogical32 => {
                // By a constant: an index scaled to a byte offset (see
                // build_handle_load_word's index word).
                let is_shift = inst.opcode == Opcode::ShiftLeftLogical32;
                let scaled = inst.arg(1).resolve(program).is_immediate()
                    || (!is_shift && inst.arg(0).resolve(program).is_immediate());
                self.scaled += i32::from(scaled);
                let built = build(self, 0, code, budget) && build(self, 1, code, budget);
                self.scaled -= i32::from(scaled);
                if !built { return false; }
                code.push(if is_shift { H::Shl } else { H::Mul } as u32);
                true
            }
            Opcode::Identity | Opcode::ConditionRef | Opcode::ShuffleIndex
            | Opcode::VoteAny | Opcode::VoteAll | Opcode::BitCastU32F32
            | Opcode::BitCastF32U32 => build(self, 0, code, budget),
            Opcode::GetCbufU32 | Opcode::LoadStorage32 => {
                let node = get_inst_ref_recursive(program, value);
                self.build_handle_load_word(program, inst, node, 0, code, budget, block)
            }
            // Native storage loads can return four words as well as two.
            // Both extraction forms select a word at the same byte offset.
            Opcode::CompositeExtractU32x2 | Opcode::CompositeExtractU32x4 => {
                let element = inst.arg(1).resolve(program);
                if !element.is_immediate() { return false; }
                let Some(source) = get_inst_recursive(program, *inst.arg(0)) else { return false };
                let node = get_inst_ref_recursive(program, value);
                self.build_handle_load_word(program, source, node, element.imm_u32().wrapping_mul(4),
                                            code, budget, block)
            }
            Opcode::Phi => {
                let resolved = resolve_conditional_phi(program, value, block);
                if resolved.is_immediate() || get_inst_ref_recursive(program, resolved)
                    != get_inst_ref_recursive(program, value)
                {
                    return self.build_handle_program(program, resolved, code, budget, block);
                }
                let mut chosen = None;
                for arg in 0..inst.num_args() {
                    let incoming = *inst.arg(arg);
                    let source = get_inst_recursive(program, incoming);
                    if source.is_some_and(|i| is_undefined(i.opcode)) { continue; }
                    if chosen.is_none() || source.is_some_and(|i| i.opcode == Opcode::ShuffleIndex) {
                        chosen = Some(arg);
                    }
                }
                if chosen.is_some_and(|arg| build(self, arg, code, budget)) { return true; }
                for arg in 0..inst.num_args() {
                    if Some(arg) == chosen || get_inst_recursive(program, *inst.arg(arg))
                        .is_some_and(|i| is_undefined(i.opcode)) { continue; }
                    if build(self, arg, code, budget) { return true; }
                }
                false
            }
            Opcode::GetZeroFromOp | Opcode::GetSignFromOp => {
                if !build(self, 0, code, budget) { return false; }
                code.extend([H::Immediate as u32, 0,
                    if inst.opcode == Opcode::GetZeroFromOp { H::Eq } else { H::SLt } as u32]);
                true
            }
            Opcode::GetCarryFromOp => {
                let Some(add) = get_inst_recursive(program, *inst.arg(0)) else {
                    self.failure = Some(inst.opcode); return false;
                };
                if add.opcode != Opcode::IAdd32 {
                    self.failure = Some(inst.opcode); return false;
                }
                if !build(self, 0, code, budget) || !self.build_handle_program(
                    program, *add.arg(0), code, budget, block) { return false; }
                code.push(H::ULt as u32);
                true
            }
            Opcode::UndefU1 | Opcode::UndefU32 => {
                code.extend([H::Immediate as u32, 0]); true
            }
            Opcode::BitFieldUExtract => {
                if !build(self, 0, code, budget) || !build(self, 1, code, budget) { return false; }
                code.extend([H::Shr as u32, H::Immediate as u32, 1]);
                if !build(self, 2, code, budget) { return false; }
                code.extend([H::Shl as u32, H::Immediate as u32, 1, H::Sub as u32, H::And as u32]);
                true
            }
            Opcode::BitFieldSExtract => {
                if !build(self, 0, code, budget) { return false; }
                code.extend([H::Immediate as u32, 32]);
                if !build(self, 1, code, budget) { return false; }
                code.push(H::Sub as u32);
                if !build(self, 2, code, budget) { return false; }
                code.extend([H::Sub as u32, H::Shl as u32, H::Immediate as u32, 32]);
                if !build(self, 2, code, budget) { return false; }
                code.extend([H::Sub as u32, H::Sar as u32]); true
            }
            Opcode::BitFieldInsert => {
                let mask = |state: &mut Self, code: &mut Vec<u32>, budget: &mut i32| {
                    code.extend([H::Immediate as u32, 1]);
                    if !build(state, 3, code, budget) { return false; }
                    code.extend([H::Shl as u32, H::Immediate as u32, 1, H::Sub as u32]);
                    if !build(state, 2, code, budget) { return false; }
                    code.push(H::Shl as u32); true
                };
                if !build(self, 0, code, budget) || !mask(self, code, budget) { return false; }
                code.extend([H::Not as u32, H::And as u32]);
                if !build(self, 1, code, budget) || !build(self, 2, code, budget) { return false; }
                code.push(H::Shl as u32);
                if !mask(self, code, budget) { return false; }
                code.extend([H::And as u32, H::Or as u32]); true
            }
            Opcode::UClamp32 | Opcode::SClamp32 => {
                if !build(self, 0, code, budget) || !build(self, 1, code, budget) { return false; }
                code.push(if inst.opcode == Opcode::SClamp32 { H::SMax } else { H::UMax } as u32);
                if !build(self, 2, code, budget) { return false; }
                code.push(if inst.opcode == Opcode::SClamp32 { H::SMin } else { H::UMin } as u32);
                true
            }
            _ => { self.failure = Some(inst.opcode); false }
        }
    }

    // Mechanical extraction of BuildHandleNode's load_word lambda: same owner.
    // `node` is the instruction BuildHandleNode is building (the word), which
    // the index word records.
    fn build_handle_load_word(&mut self, program: &Program, source: &Inst, node: Option<InstRef>,
                              extra: u32, code: &mut Vec<u32>, budget: &mut i32,
                              block: u32) -> bool {
        use crate::shader_info::HandleOp as H;
        let is_cbuf = matches!(source.opcode, Opcode::GetCbufU32 | Opcode::GetCbufU32x2);
        if !is_cbuf && !matches!(source.opcode, Opcode::LoadStorage32
            | Opcode::LoadStorage64 | Opcode::LoadStorage128) { return false; }
        let index = source.arg(0).resolve(program);
        if !index.is_immediate() { return false; }
        let index = index.imm_u32();
        let offset = source.arg(1).resolve(program);
        if is_cbuf && offset.is_immediate() {
            code.extend([H::Cbuf as u32, index, offset.imm_u32().wrapping_add(extra)]);
            return true;
        }
        if !self.build_handle_address(program, offset, code, budget, block) {
            return self.build_handle_index_word(node, code);
        }
        if extra != 0 { code.extend([H::Immediate as u32, extra, H::Add as u32]); }
        code.extend([if is_cbuf { H::CbufDynamic } else { H::Storage } as u32, index]);
        true
    }

    // Mechanical extraction of load_word's `address` lambda (Lichii
    // texture_pass.cpp:757-768): the read's address. One for a word that gives
    // another read's address must be computable on its own: the word is the
    // index word otherwise.
    fn build_handle_address(&mut self, program: &Program, offset: Value, code: &mut Vec<u32>,
                            budget: &mut i32, block: u32) -> bool {
        let allowed = self.index_allowed;
        let scaled = self.scaled;
        self.index_allowed = allowed && self.load_depth == 0;
        self.scaled = 0;
        self.load_depth += 1;
        let built = self.build_handle_program(program, offset, code, budget, block);
        self.load_depth -= 1;
        self.scaled = scaled;
        self.index_allowed = allowed;
        built
    }

    // Mechanical extraction of load_word's `index_word` lambda (Lichii
    // texture_pass.cpp:770-779): this word as the element index of a
    // descriptor array (`index_allowed`).
    fn build_handle_index_word(&mut self, node: Option<InstRef>, code: &mut Vec<u32>) -> bool {
        use crate::shader_info::HandleOp as H;
        let Some(node) = node else { return false };
        if !self.index_allowed || self.load_depth == 0 || self.scaled == 0
            || self.index.is_some_and(|index| index != node)
        {
            return false;
        }
        self.index = Some(node);
        code.push(H::Index as u32);
        true
    }
}

// Lichii ResolvedTextureType: size queries share a handle program's sample
// type; otherwise inspect the handle, retaining non-buffer ImageFetch types.
fn resolved_texture_type(
    env: &mut Option<&mut dyn Environment>,
    texture_inst: &TextureInst,
    inst: &Inst,
    opcode: Opcode,
    handle_program_types: &std::collections::HashMap<u32, u8>,
) -> TextureType {
    let mut flags = TextureInstInfo::from_u32(inst.flags);
    match opcode {
        Opcode::ImageQueryDimensions => {
            if let Some(&texture_type) = handle_program_types.get(&texture_inst.cbuf.handle_program) {
                flags.texture_type = texture_type;
                texture_type_from_flags(flags)
            } else {
                env.as_deref_mut().map(|env| read_texture_type_cached(env, &texture_inst.cbuf))
                    .unwrap_or_else(|| texture_type_from_flags(flags))
            }
        }
        Opcode::ImageFetch if texture_type_from_flags(flags) == TextureType::Color1D => {
            if env.as_deref_mut().is_some_and(|env|
                read_texture_type_cached(env, &texture_inst.cbuf) == TextureType::Buffer)
            {
                TextureType::Buffer
            } else {
                texture_type_from_flags(flags)
            }
        }
        _ => texture_type_from_flags(flags),
    }
}

/// Resolve bindless/bound texture operations to indexed operations.
pub fn texture_pass(
    env: &mut dyn Environment,
    program: &mut Program,
    host_info: &HostTranslateInfo,
) {
    texture_pass_impl(program, host_info, Some(env), None);
}

/// Bound-texture subset of upstream `TexturePass`.
///
/// Upstream bound texture instructions enter `TexturePass` with Arg(0) as a
/// byte offset into `env.TextureBoundBuffer()`. The pass creates/deduplicates
/// a `TextureDescriptor`, then rewrites Arg(0) to the compact descriptor index
/// used by backends.
pub fn texture_pass_bound_textures(program: &mut Program, texture_bound_buffer: u32) {
    texture_pass_impl(
        program,
        &HostTranslateInfo::default(),
        None,
        Some(texture_bound_buffer),
    );
}

fn texture_pass_impl(
    program: &mut Program,
    host_info: &HostTranslateInfo,
    env: Option<&mut dyn Environment>,
    texture_bound_buffer_override: Option<u32>,
) {
    let mut host_info = host_info.clone();
    host_info.apply_descriptor_limit_policy();
    let mut to_replace = Vec::new();
    let mut env = env;
    if let Some(env) = env.as_deref_mut() {
        env.texture_pass_caches().clear();
    }

    let mut dominance: Option<Dominance> = None;
    let post_order_blocks = program.post_order_blocks.clone();
    for block_index in post_order_blocks {
        if let Some(env) = env.as_deref_mut() {
            env.texture_pass_caches().track_cache.clear();
        }
        let block = &program.blocks[block_index as usize];
        for (inst_index, inst) in block.instructions.iter().enumerate() {
            let Some(inst) = inst.as_ref() else {
                continue;
            };
            if !is_texture_instruction(inst.opcode) {
                continue;
            }
            let Some(handle) = inst.args.first().copied() else {
                continue;
            };
            let cbuf = match handle {
                Value::ImmU32(cbuf_offset) if !is_bindless(inst.opcode) => {
                    let texture_bound_buffer = texture_bound_buffer_override
                        .or_else(|| env.as_ref().map(|env| env.texture_bound_buffer()))
                        .unwrap_or(0);
                    ConstBufferAddr::bound(texture_bound_buffer, cbuf_offset)
                }
                value => {
                    if let Some(addr) = track_cached(value, program, &mut env, &host_info, block_index) {
                        addr
                    } else {
                        let mut code = Vec::new();
                        let mut state = HandleBuildState::default();
                        let mut budget = 4096;
                        let mut built = state.build_handle_program(
                            program, value, &mut code, &mut budget, block_index);
                        let mut index_word = None;
                        // Only sampled textures: an array of storage images would have
                        // every element taken as written (Lichii texture_pass.cpp:1312-1327).
                        let opcode = indexed_instruction(inst.opcode).unwrap_or(inst.opcode);
                        let sampled = !is_storage_image_instruction(opcode)
                            && TextureInstInfo::from_u32(inst.flags).texture_type
                                != TextureType::Buffer as u8;
                        if !built && sampled && host_info.support_handle_table_arrays {
                            // Read from a table at an index each invocation reads for
                            // itself: a descriptor array of the table's entries, which the
                            // shader indexes with that index.
                            code.clear();
                            budget = 4096;
                            state = HandleBuildState { index_allowed: true, ..Default::default() };
                            built = state.build_handle_program(
                                program, value, &mut code, &mut budget, block_index);
                            index_word = if built { state.index } else { None };
                        }
                        if !built {
                            // Temporary, opt-in investigation capture. Remove before delivery.
                            if let Some(directory) = std::env::var_os("RUZU_HANDLE_FAILURE_DUMP_DIR") {
                                use std::sync::atomic::{AtomicUsize, Ordering};
                                static CAPTURES: AtomicUsize = AtomicUsize::new(0);
                                let index = CAPTURES.fetch_add(1, Ordering::Relaxed);
                                if index < 16 {
                                    let path = std::path::PathBuf::from(directory)
                                        .join(format!("handle-failure-{index:02}.txt"));
                                    if let Ok(mut file) = std::fs::OpenOptions::new()
                                        .write(true).create_new(true).open(path) {
                                        use std::io::Write;
                                        let _ = writeln!(file, "root={value:?} block={block_index} budget={budget} failure={:?}\n{program:#?}", state.failure);
                                    }
                                }
                            }
                            // Do not restore the legacy search through memory addresses:
                            // that would bind the address as if it were the loaded handle.
                            std::panic::panic_any(crate::exception::NotImplementedException::new(
                                format!("Failed to compute bindless texture handle: {:?}", state.failure),
                            ));
                        }
                        let index = program.info.handle_programs.iter().position(|p| *p == code)
                            .unwrap_or_else(|| {
                                program.info.handle_programs.push(code);
                                program.info.handle_programs.len() - 1
                            });
                        // The shader indexes the array with the index word, which must
                        // be defined wherever the texture instruction runs; where it
                        // isn't, every invocation takes the first element.
                        let use_ref = InstRef { block: block_index, inst: inst_index as u32 };
                        let count = match index_word {
                            Some(index_word) if dominance
                                .get_or_insert_with(|| Dominance::new(program))
                                .dominates(program, index_word, use_ref) => HANDLE_TABLE_SIZE,
                            _ => 1,
                        };
                        ConstBufferAddr {
                            handle_program: index as u32,
                            dynamic_offset: match index_word {
                                Some(index_word) if count > 1 => Value::Inst(index_word),
                                _ => Value::Void,
                            },
                            count,
                            ..ConstBufferAddr::bound(0, 0)
                        }
                    }
                },
            };
            to_replace.push(TextureInst {
                block: block_index,
                inst: inst_index as u32,
                cbuf,
            });
        }
    }

    to_replace
        .sort_unstable_by_key(|texture_inst| (texture_inst.cbuf.index, texture_inst.cbuf.offset));
    if !to_replace.is_empty() {
        // Upstream `TexturePass` owns the texture/image descriptor vectors.
        // The Rust translators still opportunistically call `register_texture`
        // while lowering instructions; discard those pre-pass placeholders so
        // descriptor_index points at the rebuilt upstream-style arrays.
        program.info.texture_buffer_descriptors.clear();
        program.info.image_buffer_descriptors.clear();
        program.info.texture_descriptors.clear();
        program.info.image_descriptors.clear();
    }
    let mut last_integer_format: Option<(u32, u32, bool)> = None;
    let mut is_texture_pixel_format_integer =
        |env: &mut Option<&mut dyn Environment>, cbuf: &ConstBufferAddr| {
            if cbuf.handle_program != u32::MAX { return false; }
            if let Some((index, offset, is_integer)) = last_integer_format {
                if index == cbuf.index && offset == cbuf.offset {
                    return is_integer;
                }
            }
            let is_integer = env
                .as_deref_mut()
                .map(|env| is_texture_pixel_format_integer_cached(env, cbuf))
                .unwrap_or(false);
            last_integer_format = Some((cbuf.index, cbuf.offset, is_integer));
            is_integer
        };

    let mut handle_program_types = std::collections::HashMap::new();
    for texture_inst in &to_replace {
        let inst = get_inst(program, InstRef { block: texture_inst.block, inst: texture_inst.inst }).unwrap();
        let op = indexed_instruction(inst.opcode).unwrap_or(inst.opcode);
        if texture_inst.cbuf.handle_program != u32::MAX && op != Opcode::ImageQueryDimensions
            && !is_storage_image_instruction(op)
        {
            handle_program_types.entry(texture_inst.cbuf.handle_program)
                .or_insert(TextureInstInfo::from_u32(inst.flags).texture_type);
        }
    }
    let sampled_dynamic_cap = dynamic_sampled_texture_cap(
        &program.info,
        &host_info,
        dynamic_sampled_texture_array_count(&mut env, &to_replace, program, &handle_program_types),
    );
    for texture_inst in to_replace {
        let inst_snapshot = program.blocks[texture_inst.block as usize].instructions
            [texture_inst.inst as usize]
            .as_ref()
            .expect("texture inst collected from live slot")
            .clone();
        let mut flags = TextureInstInfo::from_u32(inst_snapshot.flags);
        let cbuf = texture_inst.cbuf;
        let indexed_opcode =
            indexed_instruction(inst_snapshot.opcode).unwrap_or(inst_snapshot.opcode);
        let mut image_fetch_can_be_multisample = false;
        let mut is_multisample = false;
        let mut is_rect_sample = false;
        match indexed_opcode {
            Opcode::ImageQueryDimensions => {
                flags.texture_type = resolved_texture_type(&mut env, &texture_inst,
                    &inst_snapshot, indexed_opcode, &handle_program_types) as u8;
            }
            Opcode::ImageSampleImplicitLod => {
                if texture_type_from_flags(flags) == TextureType::Color2D {
                    is_rect_sample = env
                        .as_deref_mut()
                        .map(|env| read_texture_type_cached(env, &cbuf) == TextureType::Color2DRect)
                        .unwrap_or(false);
                }
            }
            Opcode::ImageFetch => {
                image_fetch_can_be_multisample = matches!(
                    texture_type_from_flags(flags),
                    TextureType::Color2D | TextureType::Color2DRect | TextureType::ColorArray2D
                );
                is_multisample = image_fetch_can_be_multisample
                    && !matches!(inst_snapshot.args.get(4), None | Some(Value::Void));
                flags.texture_type = resolved_texture_type(&mut env, &texture_inst,
                    &inst_snapshot, indexed_opcode, &handle_program_types) as u8;
            }
            _ => {}
        }

        let size_shift = descriptor_size_shift(&cbuf, program);
        let mut count = cbuf.count;

        let descriptor_index = if is_storage_image_instruction(indexed_opcode) {
            if cbuf.has_secondary {
                std::panic::panic_any(crate::exception::NotImplementedException::new(
                    "Unexpected separate sampler",
                ));
            }
            let is_integer = is_texture_pixel_format_integer(&mut env, &cbuf);
            if texture_type_from_flags(flags) == TextureType::Buffer {
                add_image_buffer_descriptor(
                    &mut program.info.image_buffer_descriptors,
                    ImageBufferDescriptor {
                        handle_program: cbuf.handle_program,
                        format: image_format_from_flags(flags),
                        is_written: indexed_opcode != Opcode::ImageRead,
                        is_read: indexed_opcode != Opcode::ImageWrite,
                        is_integer,
                        cbuf_index: cbuf.index,
                        cbuf_offset: cbuf.offset,
                        count,
                        size_shift,
                    },
                )
            } else {
                add_image_descriptor(
                    &mut program.info.image_descriptors,
                    ImageDescriptor {
                        handle_program: cbuf.handle_program,
                        texture_type: texture_type_from_flags(flags),
                        format: image_format_from_flags(flags),
                        is_written: indexed_opcode != Opcode::ImageRead,
                        is_read: indexed_opcode != Opcode::ImageWrite,
                        is_integer,
                        cbuf_index: cbuf.index,
                        cbuf_offset: cbuf.offset,
                        count,
                        size_shift,
                    },
                )
            }
        } else if flags.texture_type == TextureType::Buffer as u8 {
            add_texture_buffer_descriptor(
                &mut program.info.texture_buffer_descriptors,
                TextureBufferDescriptor {
                    handle_program: cbuf.handle_program,
                    has_secondary: cbuf.has_secondary,
                    cbuf_index: cbuf.index,
                    cbuf_offset: cbuf.offset,
                    shift_left: cbuf.shift_left,
                    secondary_cbuf_index: cbuf.secondary_index,
                    secondary_cbuf_offset: cbuf.secondary_offset,
                    secondary_shift_left: cbuf.secondary_shift_left,
                    count,
                    size_shift,
                },
            )
        } else {
            count = count.min(sampled_dynamic_cap);
            // Depth-compare sampling returns float even if an unknown handle
            // reports an integer format (Lichii TexturePass).
            let is_integer = !flags.is_depth && is_texture_pixel_format_integer(&mut env, &cbuf);
            let descriptor = TextureDescriptor {
                handle_program: cbuf.handle_program,
                texture_type: texture_type_from_flags(flags),
                is_depth: flags.is_depth,
                is_multisample,
                is_integer,
                has_secondary: cbuf.has_secondary,
                cbuf_index: cbuf.index,
                cbuf_offset: cbuf.offset,
                shift_left: cbuf.shift_left,
                secondary_cbuf_index: cbuf.secondary_index,
                secondary_cbuf_offset: cbuf.secondary_offset,
                secondary_shift_left: cbuf.secondary_shift_left,
                count,
                size_shift,
            };
            flags.is_integer = is_integer;
            add_texture_descriptor(&mut program.info.texture_descriptors, descriptor)
        };

        let inst = program.blocks[texture_inst.block as usize].instructions
            [texture_inst.inst as usize]
            .as_mut()
            .expect("texture inst collected from live slot");
        inst.opcode = indexed_opcode;
        flags.descriptor_index = descriptor_index as u16;
        inst.flags = flags.to_u32();
        if indexed_opcode == Opcode::ImageFetch && !image_fetch_can_be_multisample {
            if inst.args.len() > 4 {
                inst.args[4] = Value::Void;
            }
        }
        let dynamic_index = if count > 1 {
            let block = &mut program.blocks[texture_inst.block as usize];
            let shifted = if size_shift != 0 {
                insert_before(
                    block,
                    texture_inst.block,
                    texture_inst.inst,
                    Opcode::ShiftRightLogical32,
                    vec![cbuf.dynamic_offset, Value::ImmU32(size_shift)],
                )
            } else {
                cbuf.dynamic_offset
            };
            insert_before(
                block,
                texture_inst.block,
                texture_inst.inst,
                Opcode::UMin32,
                vec![shifted, Value::ImmU32(count - 1)],
            )
        } else {
            Value::Void
        };
        let inst = program.blocks[texture_inst.block as usize].instructions
            [texture_inst.inst as usize]
            .as_mut()
            .expect("texture inst collected from live slot");
        inst.args[0] = dynamic_index;

        if is_rect_sample {
            patch_image_sample_implicit_lod(
                &mut program.blocks[texture_inst.block as usize],
                texture_inst.block,
                texture_inst.inst,
            );
        }

        if !host_info.support_snorm_render_buffer
            && indexed_opcode == Opcode::ImageFetch
            && texture_type_from_flags(flags) == TextureType::Buffer
        {
            if let Some(env) = env.as_deref_mut() {
                let pixel_format = read_texture_pixel_format_cached(env, &cbuf);
                if is_pixel_format_snorm(pixel_format) {
                    patch_texel_fetch(program, texture_inst.block, texture_inst.inst, pixel_format);
                }
            }
        }
    }
}

/// Elements of the descriptor array of a handle read from a table at an index
/// word (see `HandleBuildState::index_allowed`): the table entries a draw
/// binds, larger indices taking the last one.
///
/// Lichii extension: `texture_pass.cpp:1061-1063` `HANDLE_TABLE_SIZE`.
const HANDLE_TABLE_SIZE: u32 = 64;

/// The immediate dominator of each block of the program, over post order
/// numbers (Cooper, Harvey and Kennedy), built when a handle program first
/// needs it.
///
/// Lichii extension: `texture_pass.cpp:1068-1134` `Dominance`. The Rust IR
/// names an instruction's block in its `InstRef`, so no instruction-to-block
/// map is kept.
struct Dominance {
    numbers: std::collections::HashMap<u32, usize>,
    idom: Vec<usize>,
}

impl Dominance {
    const NONE: usize = usize::MAX;

    fn new(program: &Program) -> Self {
        let order = &program.post_order_blocks;
        let numbers: std::collections::HashMap<u32, usize> =
            order.iter().enumerate().map(|(number, &block)| (block, number)).collect();
        let mut idom = vec![Self::NONE; order.len()];
        if order.is_empty() {
            return Self { numbers, idom };
        }
        let entry = order.len() - 1;
        idom[entry] = entry;
        let intersect = |idom: &[usize], mut lhs: usize, mut rhs: usize| {
            while lhs != rhs {
                while lhs < rhs { lhs = idom[lhs]; }
                while rhs < lhs { rhs = idom[rhs]; }
            }
            lhs
        };
        let mut changed = true;
        while changed {
            changed = false;
            for number in (0..entry).rev() {
                let mut dominator = Self::NONE;
                for predecessor in &program.blocks[order[number] as usize].imm_predecessors {
                    let Some(&pred) = numbers.get(predecessor) else { continue };
                    if idom[pred] == Self::NONE { continue; }
                    dominator = if dominator == Self::NONE {
                        pred
                    } else {
                        intersect(&idom, pred, dominator)
                    };
                }
                if dominator != idom[number] {
                    idom[number] = dominator;
                    changed = true;
                }
            }
        }
        Self { numbers, idom }
    }

    /// Whether `def` runs before `use_ref` on every path to `use_ref`.
    fn dominates(&self, program: &Program, def: InstRef, use_ref: InstRef) -> bool {
        let (Some(&target), Some(&use_number)) =
            (self.numbers.get(&def.block), self.numbers.get(&use_ref.block))
        else {
            return false;
        };
        if def.block == use_ref.block {
            for (index, _) in program.blocks[def.block as usize].indexed_iter() {
                if index == def.inst { return true; }
                if index == use_ref.inst { return false; }
            }
            return false;
        }
        let mut number = use_number;
        while self.idom[number] != Self::NONE {
            if self.idom[number] == number {
                return false; // the entry
            }
            number = self.idom[number];
            if number == target {
                return true;
            }
        }
        false
    }
}

fn dynamic_descriptor_size_shift(dynamic_offset: Value, program: &Program) -> u32 {
    let Some(inst) = get_inst_recursive(program, dynamic_offset) else {
        return DESCRIPTOR_SIZE_SHIFT;
    };
    if inst.opcode != Opcode::ShiftLeftLogical32 {
        return DESCRIPTOR_SIZE_SHIFT;
    }
    let Some(shift) = inst.args.get(1).filter(|shift| shift.is_immediate()) else {
        return DESCRIPTOR_SIZE_SHIFT;
    };
    let size_shift = shift.imm_u32();
    if (DESCRIPTOR_SIZE_SHIFT..31).contains(&size_shift) {
        size_shift
    } else {
        DESCRIPTOR_SIZE_SHIFT
    }
}

fn dynamic_descriptor_count(base_offset: u32, size_shift: u32, max_descriptors: u32) -> u32 {
    let descriptor_limit = max_descriptors.max(1);
    let max_cbuf_bytes = 16 * descriptor_limit;
    if size_shift >= 31 || base_offset >= max_cbuf_bytes {
        return 1;
    }
    let stride = 1 << size_shift;
    let available = max_cbuf_bytes - base_offset;
    if available < DESCRIPTOR_SIZE {
        return 1;
    }
    let available_count = 1 + (available - DESCRIPTOR_SIZE) / stride;
    descriptor_limit.min(available_count)
}

fn static_descriptor_count<T: HasCount>(descriptors: &[T]) -> u32 {
    descriptors
        .iter()
        .map(HasCount::descriptor_count)
        .filter(|&count| count <= 1)
        .sum()
}

fn dynamic_sampled_texture_cap(
    info: &ShaderInfo,
    host_info: &HostTranslateInfo,
    dynamic_arrays: u32,
) -> u32 {
    let sampled_limit = host_info
        .max_per_stage_descriptor_sampled_images
        .min(host_info.max_descriptor_set_sampled_images)
        .max(1);
    let resource_limit = host_info.max_per_stage_resources.max(1);
    if dynamic_arrays > 0 {
        let sampled_static_count = static_descriptor_count(&info.texture_buffer_descriptors)
            + static_descriptor_count(&info.texture_descriptors);
        let resource_static_count = num_descriptors(&info.constant_buffer_descriptors)
            + num_descriptors(&info.storage_buffers_descriptors)
            + sampled_static_count
            + num_descriptors(&info.image_buffer_descriptors)
            + num_descriptors(&info.image_descriptors);
        let sampled_cap = sampled_limit.saturating_sub(sampled_static_count) / dynamic_arrays;
        let resource_cap = resource_limit.saturating_sub(resource_static_count) / dynamic_arrays;
        return sampled_cap.min(resource_cap).max(1);
    }
    DESCRIPTOR_MAX_COUNT.min(sampled_limit).min(resource_limit)
}

// Lichii DescriptorSizeShift: a handle-program array uses an element index.
fn descriptor_size_shift(cbuf: &ConstBufferAddr, program: &Program) -> u32 {
    if cbuf.count <= 1 {
        DESCRIPTOR_SIZE_SHIFT
    } else if cbuf.handle_program != u32::MAX {
        0
    } else {
        dynamic_descriptor_size_shift(cbuf.dynamic_offset, program)
    }
}

fn dynamic_sampled_texture_array_count(
    env: &mut Option<&mut dyn Environment>,
    to_replace: &[TextureInst],
    program: &Program,
    handle_program_types: &std::collections::HashMap<u32, u8>,
) -> u32 {
    // Lichii counts unique descriptor tuples, not instructions using an array.
    // Count itself is excluded because the common cap can make counts equal.
    let mut arrays = Vec::new();
    for texture_inst in to_replace {
        let inst = get_inst(program, InstRef { block: texture_inst.block, inst: texture_inst.inst })
            .expect("texture inst collected from live slot");
        let cbuf = &texture_inst.cbuf;
        let opcode = indexed_instruction(inst.opcode).unwrap_or(inst.opcode);
        if cbuf.count <= 1 || is_storage_image_instruction(opcode) { continue; }
        let flags = TextureInstInfo::from_u32(inst.flags);
        let texture_type = resolved_texture_type(env, texture_inst, inst, opcode, handle_program_types);
        if texture_type != TextureType::Buffer {
            let array = (cbuf.index, cbuf.offset, cbuf.shift_left, cbuf.has_secondary,
                cbuf.secondary_index, cbuf.secondary_offset, cbuf.secondary_shift_left,
                descriptor_size_shift(cbuf, program), cbuf.handle_program,
                texture_type as u32, flags.is_depth);
            if !arrays.contains(&array) { arrays.push(array); }
        }
    }
    arrays.len() as u32
}

fn get_inst<'a>(program: &'a Program, inst_ref: InstRef) -> Option<&'a Inst> {
    program
        .blocks
        .get(inst_ref.block as usize)?
        .instructions
        .get(inst_ref.inst as usize)?
        .as_ref()
}

fn get_inst_recursive(program: &Program, value: Value) -> Option<&Inst> {
    let inst_ref = get_inst_ref_recursive(program, value)?;
    get_inst(program, inst_ref)
}

fn get_inst_ref_recursive(program: &Program, mut value: Value) -> Option<InstRef> {
    loop {
        let Value::Inst(inst_ref) = value else {
            return None;
        };
        let inst = get_inst(program, inst_ref)?;
        if inst.opcode != Opcode::Identity {
            return Some(inst_ref);
        }
        value = *inst.args.first()?;
    }
}

fn track_cached(
    value: Value,
    program: &Program,
    env: &mut Option<&mut dyn Environment>,
    host_info: &HostTranslateInfo,
    block: u32,
) -> Option<ConstBufferAddr> {
    let Some(key) = get_inst_ref_recursive(program, value) else {
        return track(value, program, env, host_info, block);
    };
    if let Some(found) = env
        .as_deref_mut()
        .and_then(|env| env.texture_pass_caches().track_cache.get(&key).copied())
    {
        return Some(found);
    }
    let found = track(value, program, env, host_info, block);
    if let (Some(env), Some(found)) = (env.as_deref_mut(), found) {
        env.texture_pass_caches().track_cache.insert(key, found);
    }
    found
}

fn track(
    value: Value,
    program: &Program,
    env: &mut Option<&mut dyn Environment>,
    host_info: &HostTranslateInfo,
    block: u32,
) -> Option<ConstBufferAddr> {
    let value = resolve_conditional_phi(program, value, block).resolve(program);
    let root = get_inst_ref_recursive(program, value)?;
    let mut queue = std::collections::VecDeque::from([root]);
    let mut visited = std::collections::HashSet::from([root]);
    while let Some(reference) = queue.pop_front() {
        let inst = get_inst(program, reference)?;
        if let Some(addr) = try_get_const_buffer(inst, program, env, host_info, block) { return Some(addr); }
        if matches!(inst.opcode,
            Opcode::LoadGlobalU8 | Opcode::LoadGlobalS8 | Opcode::LoadGlobalU16 | Opcode::LoadGlobalS16
            | Opcode::LoadGlobal32 | Opcode::LoadGlobal64 | Opcode::LoadGlobal128
            | Opcode::LoadStorageU8 | Opcode::LoadStorageS8 | Opcode::LoadStorageU16 | Opcode::LoadStorageS16
            | Opcode::LoadStorage32 | Opcode::LoadStorage64 | Opcode::LoadStorage128 | Opcode::LoadLocal
            | Opcode::LoadSharedU8 | Opcode::LoadSharedS8 | Opcode::LoadSharedU16 | Opcode::LoadSharedS16
            | Opcode::LoadSharedU32 | Opcode::LoadSharedU64 | Opcode::LoadSharedU128) { continue; }
        for arg in (0..inst.num_args()).rev() {
            let value = resolve_conditional_phi(program, *inst.arg(arg), block).resolve(program);
            if let Some(reference) = get_inst_ref_recursive(program, value) {
                if visited.insert(reference) { queue.push_back(reference); }
            }
        }
    }
    None
}

fn try_get_const_buffer(
    inst: &Inst,
    program: &Program,
    env: &mut Option<&mut dyn Environment>,
    host_info: &HostTranslateInfo,
    block: u32,
) -> Option<ConstBufferAddr> {
    match inst.opcode {
        Opcode::BitwiseOr32 => {
            let mut lhs = track_cached(*inst.args.first()?, program, env, host_info, block)?;
            let mut rhs = track_cached(*inst.args.get(1)?, program, env, host_info, block)?;
            if lhs.has_secondary || rhs.has_secondary || (lhs.count > 1 && rhs.count > 1) {
                return None;
            }
            if rhs.count > 1
                || (lhs.count == 1
                    && (lhs.shift_left > 0 || lhs.index > rhs.index || lhs.offset > rhs.offset))
            {
                std::mem::swap(&mut lhs, &mut rhs);
            }
            Some(ConstBufferAddr {
                handle_program: u32::MAX,
                index: lhs.index,
                offset: lhs.offset,
                shift_left: lhs.shift_left,
                secondary_index: rhs.index,
                secondary_offset: rhs.offset,
                secondary_shift_left: rhs.shift_left,
                dynamic_offset: lhs.dynamic_offset,
                count: lhs.count,
                has_secondary: true,
            })
        }
        Opcode::ShiftLeftLogical32 => {
            let shift = inst.args.get(1)?;
            if !shift.is_immediate() {
                return None;
            }
            let mut lhs = track_cached(*inst.args.first()?, program, env, host_info, block)?;
            lhs.shift_left = shift.imm_u32();
            Some(lhs)
        }
        Opcode::BitwiseAnd32 => {
            let mut op1 = *inst.args.first()?;
            let mut op2 = *inst.args.get(1)?;
            if op1.is_immediate() {
                std::mem::swap(&mut op1, &mut op2);
            }
            if !op2.is_immediate() && !op1.is_immediate() {
                if let Some(value) = try_get_constant(op1, program, env) {
                    op1 = op2;
                    op2 = Value::ImmU32(value);
                } else if let Some(value) = try_get_constant(op2, program, env) {
                    op2 = Value::ImmU32(value);
                } else {
                    return None;
                }
            } else if !op2.is_immediate() {
                return None;
            }
            let mask = op2.imm_u32();
            let mut lhs = track_cached(op1, program, env, host_info, block)?;
            lhs.shift_left = mask.trailing_zeros();
            Some(lhs)
        }
        Opcode::GetCbufU32 | Opcode::GetCbufU32x2 => {
            let index = *inst.args.first()?;
            let offset = *inst.args.get(1)?;
            if !index.is_immediate() {
                return None;
            }
            if offset.is_immediate() {
                return Some(ConstBufferAddr::bound(index.imm_u32(), offset.imm_u32()));
            }
            let offset_inst = get_inst_recursive(program, offset)?;
            // Lichii TryGetConstBuffer: a bare scaled index addresses a table
            // at byte zero; Eden only recognized an explicit base addition.
            let mut base_offset = 0;
            let mut dynamic_offset = offset;
            if offset_inst.opcode == Opcode::IAdd32 {
                let lhs = *offset_inst.args.first()?;
                let rhs = *offset_inst.args.get(1)?;
                if lhs.is_immediate() {
                    base_offset = lhs.imm_u32();
                    dynamic_offset = rhs;
                } else if rhs.is_immediate() {
                    base_offset = rhs.imm_u32();
                    dynamic_offset = lhs;
                } else {
                    return None;
                }
            } else if offset_inst.opcode != Opcode::ShiftLeftLogical32 {
                return None;
            }
            let size_shift = dynamic_descriptor_size_shift(dynamic_offset, program);
            let sampled_limit = host_info
                .max_per_stage_descriptor_sampled_images
                .min(host_info.max_descriptor_set_sampled_images)
                .max(1);
            let resource_limit = host_info.max_per_stage_resources.max(1);
            Some(ConstBufferAddr {
                count: dynamic_descriptor_count(
                    base_offset,
                    size_shift,
                    DESCRIPTOR_MAX_COUNT.min(sampled_limit).min(resource_limit),
                ),
                dynamic_offset,
                ..ConstBufferAddr::bound(index.imm_u32(), base_offset)
            })
        }
        _ => None,
    }
}

fn try_get_constant(
    value: Value,
    program: &Program,
    env: &mut Option<&mut dyn Environment>,
) -> Option<u32> {
    let inst = get_inst_recursive(program, value)?;
    if inst.opcode != Opcode::GetCbufU32 {
        return None;
    }
    let index = *inst.args.first()?;
    let offset = *inst.args.get(1)?;
    if !index.is_immediate() || !offset.is_immediate() || index.imm_u32() != 1 {
        return None;
    }
    let env = env.as_deref_mut()?;
    Some(read_cbuf_cached(env, index.imm_u32(), offset.imm_u32()))
}

fn is_texture_instruction(opcode: Opcode) -> bool {
    indexed_instruction(opcode).is_some()
}

fn indexed_instruction(opcode: Opcode) -> Option<Opcode> {
    Some(match opcode {
        Opcode::BindlessImageSampleImplicitLod | Opcode::BoundImageSampleImplicitLod => {
            Opcode::ImageSampleImplicitLod
        }
        Opcode::BindlessImageSampleExplicitLod | Opcode::BoundImageSampleExplicitLod => {
            Opcode::ImageSampleExplicitLod
        }
        Opcode::BindlessImageSampleDrefImplicitLod | Opcode::BoundImageSampleDrefImplicitLod => {
            Opcode::ImageSampleDrefImplicitLod
        }
        Opcode::BindlessImageSampleDrefExplicitLod | Opcode::BoundImageSampleDrefExplicitLod => {
            Opcode::ImageSampleDrefExplicitLod
        }
        Opcode::BindlessImageGather | Opcode::BoundImageGather => Opcode::ImageGather,
        Opcode::BindlessImageGatherDref | Opcode::BoundImageGatherDref => Opcode::ImageGatherDref,
        Opcode::BindlessImageFetch | Opcode::BoundImageFetch => Opcode::ImageFetch,
        Opcode::BindlessImageQueryDimensions | Opcode::BoundImageQueryDimensions => {
            Opcode::ImageQueryDimensions
        }
        Opcode::BindlessImageQueryLod | Opcode::BoundImageQueryLod => Opcode::ImageQueryLod,
        Opcode::BindlessImageGradient | Opcode::BoundImageGradient => Opcode::ImageGradient,
        Opcode::BindlessImageRead | Opcode::BoundImageRead => Opcode::ImageRead,
        Opcode::BindlessImageWrite | Opcode::BoundImageWrite => Opcode::ImageWrite,
        Opcode::BindlessImageAtomicIAdd32 | Opcode::BoundImageAtomicIAdd32 => {
            Opcode::ImageAtomicIAdd32
        }
        Opcode::BindlessImageAtomicSMin32 | Opcode::BoundImageAtomicSMin32 => {
            Opcode::ImageAtomicSMin32
        }
        Opcode::BindlessImageAtomicUMin32 | Opcode::BoundImageAtomicUMin32 => {
            Opcode::ImageAtomicUMin32
        }
        Opcode::BindlessImageAtomicSMax32 | Opcode::BoundImageAtomicSMax32 => {
            Opcode::ImageAtomicSMax32
        }
        Opcode::BindlessImageAtomicUMax32 | Opcode::BoundImageAtomicUMax32 => {
            Opcode::ImageAtomicUMax32
        }
        Opcode::BindlessImageAtomicInc32 | Opcode::BoundImageAtomicInc32 => {
            Opcode::ImageAtomicInc32
        }
        Opcode::BindlessImageAtomicDec32 | Opcode::BoundImageAtomicDec32 => {
            Opcode::ImageAtomicDec32
        }
        Opcode::BindlessImageAtomicAnd32 | Opcode::BoundImageAtomicAnd32 => {
            Opcode::ImageAtomicAnd32
        }
        Opcode::BindlessImageAtomicOr32 | Opcode::BoundImageAtomicOr32 => Opcode::ImageAtomicOr32,
        Opcode::BindlessImageAtomicXor32 | Opcode::BoundImageAtomicXor32 => {
            Opcode::ImageAtomicXor32
        }
        Opcode::BindlessImageAtomicExchange32 | Opcode::BoundImageAtomicExchange32 => {
            Opcode::ImageAtomicExchange32
        }
        Opcode::ImageSampleImplicitLod
        | Opcode::ImageSampleExplicitLod
        | Opcode::ImageSampleDrefImplicitLod
        | Opcode::ImageSampleDrefExplicitLod
        | Opcode::ImageFetch
        | Opcode::ImageQueryDimensions
        | Opcode::ImageGather
        | Opcode::ImageGatherDref
        | Opcode::ImageQueryLod
        | Opcode::ImageGradient
        | Opcode::ImageRead
        | Opcode::ImageWrite
        | Opcode::ImageAtomicIAdd32
        | Opcode::ImageAtomicSMin32
        | Opcode::ImageAtomicUMin32
        | Opcode::ImageAtomicSMax32
        | Opcode::ImageAtomicUMax32
        | Opcode::ImageAtomicInc32
        | Opcode::ImageAtomicDec32
        | Opcode::ImageAtomicAnd32
        | Opcode::ImageAtomicOr32
        | Opcode::ImageAtomicXor32
        | Opcode::ImageAtomicExchange32 => opcode,
        _ => return None,
    })
}

fn is_bindless(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::BindlessImageSampleImplicitLod
            | Opcode::BindlessImageSampleExplicitLod
            | Opcode::BindlessImageSampleDrefImplicitLod
            | Opcode::BindlessImageSampleDrefExplicitLod
            | Opcode::BindlessImageGather
            | Opcode::BindlessImageGatherDref
            | Opcode::BindlessImageFetch
            | Opcode::BindlessImageQueryDimensions
            | Opcode::BindlessImageQueryLod
            | Opcode::BindlessImageGradient
            | Opcode::BindlessImageRead
            | Opcode::BindlessImageWrite
            | Opcode::BindlessImageAtomicIAdd32
            | Opcode::BindlessImageAtomicSMin32
            | Opcode::BindlessImageAtomicUMin32
            | Opcode::BindlessImageAtomicSMax32
            | Opcode::BindlessImageAtomicUMax32
            | Opcode::BindlessImageAtomicInc32
            | Opcode::BindlessImageAtomicDec32
            | Opcode::BindlessImageAtomicAnd32
            | Opcode::BindlessImageAtomicOr32
            | Opcode::BindlessImageAtomicXor32
            | Opcode::BindlessImageAtomicExchange32
    )
}

fn is_storage_image_instruction(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::ImageRead
            | Opcode::ImageWrite
            | Opcode::ImageAtomicIAdd32
            | Opcode::ImageAtomicSMin32
            | Opcode::ImageAtomicUMin32
            | Opcode::ImageAtomicSMax32
            | Opcode::ImageAtomicUMax32
            | Opcode::ImageAtomicInc32
            | Opcode::ImageAtomicDec32
            | Opcode::ImageAtomicAnd32
            | Opcode::ImageAtomicOr32
            | Opcode::ImageAtomicXor32
            | Opcode::ImageAtomicExchange32
    )
}

fn texture_type_from_flags(flags: TextureInstInfo) -> TextureType {
    TextureType::from_u8(flags.texture_type)
}

fn image_format_from_flags(flags: TextureInstInfo) -> ImageFormat {
    ImageFormat::from_u8(flags.image_format)
}

fn read_cbuf_cached(env: &mut dyn Environment, index: u32, offset: u32) -> u32 {
    let key = CbufWordKey { index, offset };
    if let Some(value) = env.texture_pass_caches().cbuf_word_cache.get(&key).copied() {
        return value;
    }
    let value = env.read_cbuf_value(index, offset);
    env.texture_pass_caches().cbuf_word_cache.insert(key, value);
    value
}

fn get_texture_handle_cached(env: &mut dyn Environment, cbuf: &ConstBufferAddr) -> u32 {
    let key = HandleKey {
        index: cbuf.index,
        offset: cbuf.offset,
        shift_left: cbuf.shift_left,
        secondary_index: cbuf.secondary_index,
        secondary_offset: cbuf.secondary_offset,
        secondary_shift_left: cbuf.secondary_shift_left,
        count: cbuf.count,
        has_secondary: cbuf.has_secondary,
    };
    if let Some(handle) = env.texture_pass_caches().handle_cache.get(&key).copied() {
        return handle;
    }
    let secondary_index = if cbuf.has_secondary {
        cbuf.secondary_index
    } else {
        cbuf.index
    };
    let secondary_offset = if cbuf.has_secondary {
        cbuf.secondary_offset
    } else {
        cbuf.offset
    };
    let lhs_raw = read_cbuf_cached(env, cbuf.index, cbuf.offset) << cbuf.shift_left;
    let rhs_raw =
        read_cbuf_cached(env, secondary_index, secondary_offset) << cbuf.secondary_shift_left;
    let handle = lhs_raw | rhs_raw;
    env.texture_pass_caches().handle_cache.insert(key, handle);
    handle
}

fn read_texture_type_cached(env: &mut dyn Environment, cbuf: &ConstBufferAddr) -> TextureType {
    if cbuf.handle_program != u32::MAX { return TextureType::Color2D; }
    let handle = get_texture_handle_cached(env, cbuf);
    env.read_texture_type(handle)
}

fn read_texture_pixel_format_cached(
    env: &mut dyn Environment,
    cbuf: &ConstBufferAddr,
) -> TexturePixelFormat {
    if cbuf.handle_program != u32::MAX { return TexturePixelFormat::A8B8G8R8Unorm; }
    let handle = get_texture_handle_cached(env, cbuf);
    env.read_texture_pixel_format(handle)
}

fn is_texture_pixel_format_integer_cached(
    env: &mut dyn Environment,
    cbuf: &ConstBufferAddr,
) -> bool {
    if cbuf.handle_program != u32::MAX { return false; }
    let handle = get_texture_handle_cached(env, cbuf);
    env.is_texture_pixel_format_integer(handle)
}

fn is_pixel_format_snorm(format: TexturePixelFormat) -> bool {
    matches!(
        format,
        TexturePixelFormat::A8B8G8R8Snorm
            | TexturePixelFormat::R8G8Snorm
            | TexturePixelFormat::R8Snorm
            | TexturePixelFormat::R16G16B16A16Snorm
            | TexturePixelFormat::R16G16Snorm
            | TexturePixelFormat::R16Snorm
    )
}

fn insert_before(
    block: &mut Block,
    block_index: u32,
    before: u32,
    opcode: Opcode,
    args: Vec<Value>,
) -> Value {
    let inst = block.insert_inst_before(before, Inst::new(opcode, args));
    Value::Inst(InstRef {
        block: block_index,
        inst,
    })
}

fn insert_before_with_flags(
    block: &mut Block,
    block_index: u32,
    before: u32,
    opcode: Opcode,
    args: Vec<Value>,
    flags: u32,
) -> Value {
    let inst = block.insert_inst_before(before, Inst::with_flags(opcode, args, flags));
    Value::Inst(InstRef {
        block: block_index,
        inst,
    })
}

fn patch_image_sample_implicit_lod(
    block: &mut Block,
    block_index: u32,
    inst_index: u32,
) {
    // The final descriptor and element must already be assigned to the sample.
    let inst = block.inst(inst_index);
    let info = TextureInstInfo::from_u32(inst.flags);
    let coord = inst.args[1];
    let handle = if inst.args[0] == Value::Void { Value::ImmU32(0) } else { inst.args[0] };
    let texture_size = insert_before_with_flags(
        block,
        block_index,
        inst_index,
        Opcode::ImageQueryDimensions,
        vec![handle, Value::ImmU32(0), Value::ImmU1(true)],
        info.to_u32(),
    );
    let coord_x = insert_before(
        block,
        block_index,
        inst_index,
        Opcode::CompositeExtractF32x2,
        vec![coord, Value::ImmU32(0)],
    );
    let size_x = insert_before(
        block,
        block_index,
        inst_index,
        Opcode::CompositeExtractU32x4,
        vec![texture_size, Value::ImmU32(0)],
    );
    let size_x_f32 = insert_before(
        block,
        block_index,
        inst_index,
        Opcode::ConvertF32U32,
        vec![size_x],
    );
    let recip_x = insert_before(
        block,
        block_index,
        inst_index,
        Opcode::FPRecip32,
        vec![size_x_f32],
    );
    let scaled_x = insert_before(
        block,
        block_index,
        inst_index,
        Opcode::FPMul32,
        vec![coord_x, recip_x],
    );
    let coord_y = insert_before(
        block,
        block_index,
        inst_index,
        Opcode::CompositeExtractF32x2,
        vec![coord, Value::ImmU32(1)],
    );
    let size_y = insert_before(
        block,
        block_index,
        inst_index,
        Opcode::CompositeExtractU32x4,
        vec![texture_size, Value::ImmU32(1)],
    );
    let size_y_f32 = insert_before(
        block,
        block_index,
        inst_index,
        Opcode::ConvertF32U32,
        vec![size_y],
    );
    let recip_y = insert_before(
        block,
        block_index,
        inst_index,
        Opcode::FPRecip32,
        vec![size_y_f32],
    );
    let scaled_y = insert_before(
        block,
        block_index,
        inst_index,
        Opcode::FPMul32,
        vec![coord_y, recip_y],
    );
    let scaled_coord = insert_before(
        block,
        block_index,
        inst_index,
        Opcode::CompositeConstructF32x2,
        vec![scaled_x, scaled_y],
    );
    block.inst_mut(inst_index).args[1] = scaled_coord;
}

fn snorm_max_value(format: TexturePixelFormat) -> f32 {
    match format {
        TexturePixelFormat::A8B8G8R8Snorm
        | TexturePixelFormat::R8G8Snorm
        | TexturePixelFormat::R8Snorm => 1.0 / i8::MAX as f32,
        TexturePixelFormat::R16G16B16A16Snorm
        | TexturePixelFormat::R16G16Snorm
        | TexturePixelFormat::R16Snorm => 1.0 / i16::MAX as f32,
        _ => panic!("Invalid texture pixel format for SNORM patch: {:?}", format),
    }
}

fn replace_uses_with(program: &mut Program, old: InstRef, replacement: Value) {
    let old_value = Value::Inst(old);
    for block in &mut program.blocks {
        for inst in block.iter_mut() {
            for arg in &mut inst.args {
                if *arg == old_value {
                    *arg = replacement;
                }
            }
            for (_, value) in &mut inst.phi_args {
                if *value == old_value {
                    *value = replacement;
                }
            }
        }
    }
    for node in &mut program.syntax_list {
        match node {
            SyntaxNode::If { cond, .. }
            | SyntaxNode::Repeat { cond, .. }
            | SyntaxNode::Break { cond, .. } => {
                if *cond == old_value {
                    *cond = replacement;
                }
            }
            _ => {}
        }
    }
}

fn patch_texel_fetch(
    program: &mut Program,
    block_index: u32,
    inst_index: u32,
    pixel_format: TexturePixelFormat,
) {
    let base_inst = program.block(block_index).inst(inst_index).clone();
    let block = program.block_mut(block_index);
    let fetch = block.insert_inst_before(inst_index, base_inst);
    let fetch_value = Value::Inst(InstRef {
        block: block_index,
        inst: fetch,
    });

    let x = insert_before(
        block,
        block_index,
        inst_index,
        Opcode::CompositeExtractF32x4,
        vec![fetch_value, Value::ImmU32(0)],
    );
    let y = insert_before(
        block,
        block_index,
        inst_index,
        Opcode::CompositeExtractF32x4,
        vec![fetch_value, Value::ImmU32(1)],
    );
    let z = insert_before(
        block,
        block_index,
        inst_index,
        Opcode::CompositeExtractF32x4,
        vec![fetch_value, Value::ImmU32(2)],
    );
    let w = insert_before(
        block,
        block_index,
        inst_index,
        Opcode::CompositeExtractF32x4,
        vec![fetch_value, Value::ImmU32(3)],
    );
    let max_value = Value::ImmF32(snorm_max_value(pixel_format));

    let convert = |block: &mut Block, component: Value| {
        let bits = insert_before(
            block,
            block_index,
            inst_index,
            Opcode::BitCastU32F32,
            vec![component],
        );
        let as_float = insert_before(
            block,
            block_index,
            inst_index,
            Opcode::ConvertF32S32,
            vec![bits],
        );
        insert_before(
            block,
            block_index,
            inst_index,
            Opcode::FPMul32,
            vec![as_float, max_value],
        )
    };

    let converted_x = convert(block, x);
    let converted_y = convert(block, y);
    let converted_z = convert(block, z);
    let converted_w = convert(block, w);
    let converted = insert_before(
        block,
        block_index,
        inst_index,
        Opcode::CompositeConstructF32x4,
        vec![converted_x, converted_y, converted_z, converted_w],
    );

    replace_uses_with(
        program,
        InstRef {
            block: block_index,
            inst: inst_index,
        },
        converted,
    );
}

fn add_texture_descriptor(
    descriptors: &mut Vec<TextureDescriptor>,
    descriptor: TextureDescriptor,
) -> u32 {
    if let Some(index) = descriptors.iter().position(|existing| {
        existing.texture_type == descriptor.texture_type
            && existing.is_depth == descriptor.is_depth
            && existing.has_secondary == descriptor.has_secondary
            && existing.cbuf_index == descriptor.cbuf_index
            && existing.cbuf_offset == descriptor.cbuf_offset
            && existing.shift_left == descriptor.shift_left
            && existing.secondary_cbuf_index == descriptor.secondary_cbuf_index
            && existing.secondary_cbuf_offset == descriptor.secondary_cbuf_offset
            && existing.secondary_shift_left == descriptor.secondary_shift_left
            && existing.count == descriptor.count
            && existing.size_shift == descriptor.size_shift
            && existing.handle_program == descriptor.handle_program
    }) {
        descriptors[index].is_multisample |= descriptor.is_multisample;
        descriptors[index].is_integer |= descriptor.is_integer;
        return index as u32;
    }
    descriptors.push(descriptor);
    (descriptors.len() - 1) as u32
}

fn add_texture_buffer_descriptor(
    descriptors: &mut Vec<TextureBufferDescriptor>,
    descriptor: TextureBufferDescriptor,
) -> u32 {
    if let Some(index) = descriptors
        .iter()
        .position(|existing| *existing == descriptor)
    {
        return index as u32;
    }
    descriptors.push(descriptor);
    (descriptors.len() - 1) as u32
}

fn add_image_descriptor(
    descriptors: &mut Vec<ImageDescriptor>,
    descriptor: ImageDescriptor,
) -> u32 {
    if let Some(index) = descriptors.iter().position(|existing| {
        existing.texture_type == descriptor.texture_type
            && existing.format == descriptor.format
            && existing.cbuf_index == descriptor.cbuf_index
            && existing.cbuf_offset == descriptor.cbuf_offset
            && existing.count == descriptor.count
            && existing.size_shift == descriptor.size_shift
            && existing.handle_program == descriptor.handle_program
    }) {
        let existing = &mut descriptors[index];
        existing.is_written |= descriptor.is_written;
        existing.is_read |= descriptor.is_read;
        existing.is_integer |= descriptor.is_integer;
        return index as u32;
    }
    descriptors.push(descriptor);
    (descriptors.len() - 1) as u32
}

fn add_image_buffer_descriptor(
    descriptors: &mut Vec<ImageBufferDescriptor>,
    descriptor: ImageBufferDescriptor,
) -> u32 {
    if let Some(index) = descriptors.iter().position(|existing| {
        existing.format == descriptor.format
            && existing.cbuf_index == descriptor.cbuf_index
            && existing.cbuf_offset == descriptor.cbuf_offset
            && existing.count == descriptor.count
            && existing.size_shift == descriptor.size_shift
            && existing.handle_program == descriptor.handle_program
    }) {
        let existing = &mut descriptors[index];
        existing.is_written |= descriptor.is_written;
        existing.is_read |= descriptor.is_read;
        existing.is_integer |= descriptor.is_integer;
        return index as u32;
    }
    descriptors.push(descriptor);
    (descriptors.len() - 1) as u32
}

/// Join texture descriptors from `source` into `base`.
///
/// Upstream: `JoinTextureInfo` in `texture_pass.cpp`.
pub fn join_texture_info(base: &mut ShaderInfo, source: &mut ShaderInfo) -> [Vec<u32>; 4] {
    use crate::shader_info::HandleOp as H;
    // Storage descriptors must already be joined. Programs carry source-local
    // binding indices, unlike cbuf indices which are hardware identifiers.
    let mut programs = Vec::with_capacity(source.handle_programs.len());
    for mut code in source.handle_programs.drain(..) {
        let mut pc = 0;
        while pc < code.len() {
            let op = code[pc];
            pc += 1;
            if op == H::Storage as u32 {
                let desc = &source.storage_buffers_descriptors[code[pc] as usize];
                code[pc] = base.storage_buffers_descriptors.iter().position(|d|
                    d.cbuf_index == desc.cbuf_index && d.cbuf_offset == desc.cbuf_offset
                        && d.count == desc.count
                ).expect("storage descriptors must be joined before handle programs") as u32;
            }
            pc += if op == H::Cbuf as u32 { 2 }
                else if [H::Immediate, H::CbufDynamic, H::Storage, H::Tee, H::Load]
                    .iter().any(|h| *h as u32 == op) { 1 } else { 0 };
        }
        let index = base.handle_programs.iter().position(|p| *p == code)
            .unwrap_or_else(|| {
                base.handle_programs.push(code);
                base.handle_programs.len() - 1
            });
        programs.push(index as u32);
    }
    let remap = |program: &mut u32| {
        if *program != u32::MAX { *program = programs[*program as usize]; }
    };
    let mut mapping: [Vec<u32>; 4] = Default::default();
    for mut desc in source.texture_buffer_descriptors.drain(..) {
        remap(&mut desc.handle_program);
        mapping[0].push(add_texture_buffer_descriptor(&mut base.texture_buffer_descriptors, desc));
    }
    for mut desc in source.image_buffer_descriptors.drain(..) {
        remap(&mut desc.handle_program);
        mapping[1].push(add_image_buffer_descriptor(&mut base.image_buffer_descriptors, desc));
    }
    for mut desc in source.texture_descriptors.drain(..) {
        remap(&mut desc.handle_program);
        mapping[2].push(add_texture_descriptor(&mut base.texture_descriptors, desc));
    }
    for mut desc in source.image_descriptors.drain(..) {
        remap(&mut desc.handle_program);
        mapping[3].push(add_image_descriptor(&mut base.image_descriptors, desc));
    }
    mapping
}

/// Apply JoinTextureInfo's source-to-destination mapping to the cloned blocks.
/// The upstream merge omits this when independently numbered stages are joined.
pub fn remap_texture_bindings(blocks: &mut [crate::ir::basic_block::Block], mapping: &[Vec<u32>; 4]) {
    for block in blocks {
        for inst in block.iter_mut() {
            if indexed_instruction(inst.opcode) != Some(inst.opcode) { continue; }
            let mut flags = TextureInstInfo::from_u32(inst.flags);
            let buffer = flags.texture_type == TextureType::Buffer as u8;
            let image = is_storage_image_instruction(inst.opcode);
            let group = match (buffer, image) {
                (true, false) => 0, (true, true) => 1,
                (false, false) => 2, (false, true) => 3,
            };
            flags.descriptor_index = mapping[group][flags.descriptor_index as usize] as u16;
            inst.flags = flags.to_u32();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::{Environment, TexturePassCaches};
    use crate::ir::basic_block::Block;
    use crate::ir::instruction::Inst;
    use crate::ir::types::ShaderStage;
    use crate::program_header::ProgramHeader;
    use crate::shader_info::ReplaceConstant;
    use crate::stage::Stage;

    struct MockEnvironment {
        texture_pass_caches: TexturePassCaches,
        sph: ProgramHeader,
        texture_type: TextureType,
        texture_pixel_format: TexturePixelFormat,
        texture_type_reads: usize,
        cbuf_reads: usize,
        texture_handles: Vec<u32>,
        integer_handles: Vec<u32>,
        integer_format: bool,
    }

    impl Default for MockEnvironment {
        fn default() -> Self {
            Self {
                texture_pass_caches: TexturePassCaches::default(),
                sph: ProgramHeader::default(),
                texture_type: TextureType::Color2D,
                texture_pixel_format: TexturePixelFormat::A8B8G8R8Unorm,
                texture_type_reads: 0,
                cbuf_reads: 0,
                texture_handles: Vec::new(),
                integer_handles: Vec::new(),
                integer_format: false,
            }
        }
    }

    impl Environment for MockEnvironment {
        fn texture_pass_caches(&mut self) -> &mut TexturePassCaches {
            &mut self.texture_pass_caches
        }

        fn read_instruction(&mut self, _address: u32) -> u64 {
            0
        }

        fn read_cbuf_value(&mut self, cbuf_index: u32, cbuf_offset: u32) -> u32 {
            self.cbuf_reads += 1;
            match (cbuf_index, cbuf_offset) {
                // Upstream TryGetConstant resolves only bank 1 constants.
                (1, 0x10) => 0x38,
                // The tracked handle source. Shifted by countr_zero(0x38) = 3.
                (4, 0x40) => 1,
                _ => 0,
            }
        }

        fn read_texture_type(&mut self, raw_handle: u32) -> TextureType {
            self.texture_type_reads += 1;
            self.texture_handles.push(raw_handle);
            self.texture_type
        }

        fn read_texture_pixel_format(&mut self, _raw_handle: u32) -> TexturePixelFormat {
            self.texture_pixel_format
        }

        fn is_texture_pixel_format_integer(&mut self, raw_handle: u32) -> bool {
            self.integer_handles.push(raw_handle);
            self.integer_format
        }

        fn read_viewport_transform_state(&mut self) -> u32 {
            0
        }

        fn texture_bound_buffer(&self) -> u32 {
            0
        }

        fn local_memory_size(&self) -> u32 {
            0
        }

        fn shared_memory_size(&self) -> u32 {
            0
        }

        fn workgroup_size(&self) -> [u32; 3] {
            [1, 1, 1]
        }

        fn has_hle_macro_state(&self) -> bool {
            false
        }

        fn get_replace_const_buffer(
            &mut self,
            _bank: u32,
            _offset: u32,
        ) -> Option<ReplaceConstant> {
            None
        }

        fn dump(&mut self, _pipeline_hash: u64, _shader_hash: u64) {}

        fn sph(&self) -> &ProgramHeader {
            &self.sph
        }

        fn gp_passthrough_mask(&self) -> &[u32; 8] {
            static MASK: [u32; 8] = [0; 8];
            &MASK
        }

        fn shader_stage(&self) -> Stage {
            Stage::Fragment
        }

        fn start_address(&self) -> u32 {
            0
        }

        fn is_proprietary_driver(&self) -> bool {
            false
        }
    }

    fn fragment_program() -> Program {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        program.post_order_blocks.push(0);
        program
    }

    #[test]
    fn bound_texture_pass_rewrites_handle_to_descriptor_index() {
        let mut program = fragment_program();
        let flags = TextureInstInfo {
            texture_type: 2,
            ..Default::default()
        }
        .to_u32();
        program.blocks[0].instructions.push(Some(Inst::with_flags(
            Opcode::ImageSampleImplicitLod,
            vec![Value::ImmU32(0x28), Value::Void],
            flags,
        )));

        texture_pass_bound_textures(&mut program, 3);

        assert_eq!(program.info.texture_descriptors.len(), 1);
        let desc = &program.info.texture_descriptors[0];
        assert_eq!(desc.cbuf_index, 3);
        assert_eq!(desc.cbuf_offset, 0x28);
        assert_eq!(desc.size_shift, DESCRIPTOR_SIZE_SHIFT);
        let inst = program.blocks[0].instructions[0].as_ref().unwrap();
        assert_eq!(inst.args[0], Value::Void);
        let flags = TextureInstInfo::from_u32(inst.flags);
        assert_eq!(flags.descriptor_index, 0);
    }

    #[test]
    fn explicit_lod_does_not_read_texture_type() {
        let mut program = fragment_program();
        let flags = TextureInstInfo {
            texture_type: TextureType::Buffer as u8,
            ..Default::default()
        }
        .to_u32();
        program.blocks[0].instructions.push(Some(Inst::with_flags(
            Opcode::BoundImageSampleExplicitLod,
            vec![Value::ImmU32(0x28), Value::Void],
            flags,
        )));
        let mut env = MockEnvironment::default();

        texture_pass(&mut env, &mut program, &HostTranslateInfo::default());

        assert_eq!(env.texture_type_reads, 0);
        assert_eq!(
            program.blocks[0].instructions[0]
                .as_ref()
                .expect("texture instruction")
                .opcode,
            Opcode::ImageSampleExplicitLod
        );
    }

    #[test]
    fn texture_handle_reads_are_cached_for_each_texture_pass() {
        fn make_program() -> Program {
            let mut program = fragment_program();
            let flags = TextureInstInfo {
                texture_type: TextureType::Color2D as u8,
                ..Default::default()
            }
            .to_u32();
            for _ in 0..2 {
                program.blocks[0].instructions.push(Some(Inst::with_flags(
                    Opcode::BoundImageSampleImplicitLod,
                    vec![Value::ImmU32(0x28), Value::Void],
                    flags,
                )));
            }
            program
        }

        let mut env = MockEnvironment::default();
        let mut first = make_program();
        texture_pass(&mut env, &mut first, &HostTranslateInfo::default());

        // ReadTextureType and IsTexturePixelFormatInteger consume the same
        // handle. Eden resolves its cbuf word only once during the pass.
        assert_eq!(env.cbuf_reads, 1);
        assert_eq!(env.texture_handles, vec![0, 0]);
        assert_eq!(env.integer_handles, vec![0]);

        let mut second = make_program();
        texture_pass(&mut env, &mut second, &HostTranslateInfo::default());

        // TexturePass clears all three Environment caches at entry.
        assert_eq!(env.cbuf_reads, 2);
        assert_eq!(env.texture_handles, vec![0, 0, 0, 0]);
        assert_eq!(env.integer_handles, vec![0, 0]);
    }

    #[test]
    fn texture_pass_only_visits_upstream_post_order_blocks() {
        let mut program = fragment_program();
        program.blocks.push(Block::new());
        let flags = TextureInstInfo {
            texture_type: TextureType::Color2D as u8,
            ..Default::default()
        }
        .to_u32();
        program.blocks[0].append_inst(Inst::with_flags(
            Opcode::BoundImageSampleExplicitLod,
            vec![Value::ImmU32(0x28), Value::Void],
            flags,
        ));
        program.blocks[1].append_inst(Inst::with_flags(
            Opcode::BoundImageSampleExplicitLod,
            vec![Value::ImmU32(0x70), Value::Void],
            flags,
        ));

        texture_pass_bound_textures(&mut program, 3);

        assert_eq!(program.info.texture_descriptors.len(), 1);
        assert_eq!(program.info.texture_descriptors[0].cbuf_offset, 0x28);
        assert_eq!(
            program.blocks[1].inst(0).opcode,
            Opcode::BoundImageSampleExplicitLod
        );
    }

    #[test]
    fn texture_pass_preserves_upstream_untracked_bindless_exception() {
        let mut program = fragment_program();
        let unknown_handle = program.blocks[0].append_inst(Inst::new(
            Opcode::LaneId,
            vec![],
        ));
        program.blocks[0].append_inst(Inst::with_flags(
            Opcode::BindlessImageSampleExplicitLod,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: unknown_handle,
                }),
                Value::Void,
            ],
            TextureInstInfo {
                texture_type: TextureType::Color2D as u8,
                ..Default::default()
            }
            .to_u32(),
        ));
        let mut env = MockEnvironment::default();

        let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            texture_pass(&mut env, &mut program, &HostTranslateInfo::default());
        }))
        .expect_err("upstream rejects an untracked bindless texture handle");

        let exception = payload
            .downcast_ref::<crate::exception::NotImplementedException>()
            .expect("failure must remain a shader NotImplementedException");
        assert_eq!(
            exception.to_string(),
            "Failed to compute bindless texture handle: Some(LaneId) is not implemented"
        );
    }

    /// A handle read from a light table at an index each invocation reads for
    /// itself (a per-tile light list), as Ocarina of Time's clustered light
    /// loop does: Lichii's MakeInst turns it into a descriptor array of the
    /// table's entries, indexed by that index word.
    fn handle_table_program() -> (Program, u32, u32) {
        let mut program = fragment_program();
        let block = &mut program.blocks[0];
        let reference = |inst| Value::Inst(InstRef { block: 0, inst });
        let lane = block.append_inst(Inst::new(Opcode::LaneId, vec![]));
        let word = block.append_new_inst(Opcode::LoadStorage32, vec![Value::ImmU32(0), reference(lane)]);
        let scaled = block.append_new_inst(Opcode::IMul32, vec![reference(word), Value::ImmU32(0x60)]);
        let address = block.append_new_inst(Opcode::IAdd32, vec![reference(scaled), Value::ImmU32(0x10)]);
        let handle = block.append_new_inst(Opcode::LoadStorage32, vec![Value::ImmU32(1), reference(address)]);
        let sample = block.append_inst(Inst::with_flags(
            Opcode::BindlessImageSampleImplicitLod,
            vec![reference(handle), Value::Void],
            TextureInstInfo { texture_type: TextureType::Color2D as u8, ..Default::default() }.to_u32(),
        ));
        (program, word as u32, sample as u32)
    }

    #[test]
    fn handle_read_at_per_invocation_index_becomes_descriptor_array() {
        use crate::shader_info::{evaluate_handle_program, HandleOp as H};
        let (mut program, word, sample) = handle_table_program();
        let mut env = MockEnvironment::default();
        let host_info = HostTranslateInfo { support_handle_table_arrays: true, ..Default::default() };
        texture_pass(&mut env, &mut program, &host_info);

        assert_eq!(program.info.texture_descriptors.len(), 1);
        let desc = &program.info.texture_descriptors[0];
        assert_eq!(desc.count, HANDLE_TABLE_SIZE);
        assert_eq!(desc.size_shift, 0);
        let code = &program.info.handle_programs[desc.handle_program as usize];
        assert_eq!(code, &vec![H::Index as u32, H::Immediate as u32, 0x60, H::Mul as u32,
                               H::Immediate as u32, 0x10, H::Add as u32, H::Storage as u32, 1]);
        for element in [0, 7, 63] {
            assert_eq!(evaluate_handle_program(code, |_, _| panic!("no cbuf"),
                |binding, offset| { assert_eq!(binding, 1); offset }, element),
                Some(element * 0x60 + 0x10));
        }
        // The shader indexes the array with the index word itself (an element
        // index, no byte-offset shift), clamped to the last element.
        let sample_inst = program.blocks[0].instructions[sample as usize].as_ref().unwrap();
        assert_eq!(sample_inst.opcode, Opcode::ImageSampleImplicitLod);
        let Value::Inst(clamp) = sample_inst.args[0] else { panic!("dynamic index expected") };
        let clamp = get_inst(&program, clamp).unwrap();
        assert_eq!(clamp.opcode, Opcode::UMin32);
        assert_eq!(get_inst_ref_recursive(&program, clamp.args[0]),
                   Some(InstRef { block: 0, inst: word }));
        assert_eq!(clamp.args[1], Value::ImmU32(HANDLE_TABLE_SIZE - 1));
    }

    #[test]
    fn handle_table_arrays_need_host_support() {
        let (mut program, _, _) = handle_table_program();
        let mut env = MockEnvironment::default();
        let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            texture_pass(&mut env, &mut program, &HostTranslateInfo::default());
        }))
        .expect_err("without nonuniform indexing the handle stays uncomputable");
        let exception = payload
            .downcast_ref::<crate::exception::NotImplementedException>()
            .expect("failure must remain a shader NotImplementedException");
        assert_eq!(exception.to_string(),
                   "Failed to compute bindless texture handle: Some(LaneId) is not implemented");
    }

    #[test]
    fn index_word_must_be_scaled_into_another_reads_address() {
        // The unscaled word read at an unknown address is the handle itself,
        // not an element index: no program.
        let mut program = fragment_program();
        let block = &mut program.blocks[0];
        let lane = block.append_inst(Inst::new(Opcode::LaneId, vec![]));
        let word = block.append_new_inst(Opcode::LoadStorage32,
            vec![Value::ImmU32(0), Value::Inst(InstRef { block: 0, inst: lane })]);
        let mut state = HandleBuildState { index_allowed: true, ..Default::default() };
        let mut code = Vec::new();
        let mut budget = 4096;
        assert!(!state.build_handle_program(&program, Value::Inst(InstRef { block: 0, inst: word }),
                                            &mut code, &mut budget, 0));
        assert!(state.index.is_none());
        assert!(code.is_empty());
    }

    #[test]
    fn dominance_follows_the_immediate_dominator_chain() {
        // 0 -> {1, 2} -> 3: block 0 dominates 3, neither branch does.
        let mut program = Program::new(ShaderStage::Fragment);
        for _ in 0..4 { program.blocks.push(Block::new()); }
        program.blocks[1].imm_predecessors = vec![0];
        program.blocks[2].imm_predecessors = vec![0];
        program.blocks[3].imm_predecessors = vec![1, 2];
        program.post_order_blocks = vec![3, 1, 2, 0];
        let defs: Vec<u32> = (0..4).map(|block| program.blocks[block]
            .append_inst(Inst::new(Opcode::LaneId, vec![])) as u32).collect();
        let late = program.blocks[3].append_inst(Inst::new(Opcode::LaneId, vec![])) as u32;
        let dominance = Dominance::new(&program);
        let at = |block: u32, inst: u32| InstRef { block, inst };
        let use_ref = at(3, late);
        assert!(dominance.dominates(&program, at(0, defs[0]), use_ref));
        assert!(!dominance.dominates(&program, at(1, defs[1]), use_ref));
        assert!(!dominance.dominates(&program, at(2, defs[2]), use_ref));
        assert!(dominance.dominates(&program, at(3, defs[3]), use_ref));
        assert!(!dominance.dominates(&program, use_ref, at(3, defs[3])));
    }

    #[test]
    fn zero_texture_handle_mask_preserves_upstream_countr_zero() {
        let mut program = fragment_program();
        let cbuf = program.blocks[0].append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(4), Value::ImmU32(0x40)],
        ));
        let masked = program.blocks[0].append_inst(Inst::new(
            Opcode::BitwiseAnd32,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: cbuf,
                }),
                Value::ImmU32(0),
            ],
        ));
        program.blocks[0].append_inst(Inst::with_flags(
            Opcode::BindlessImageSampleExplicitLod,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: masked,
                }),
                Value::Void,
            ],
            TextureInstInfo {
                texture_type: TextureType::Color2D as u8,
                ..Default::default()
            }
            .to_u32(),
        ));

        texture_pass_bound_textures(&mut program, 0);

        assert_eq!(program.info.texture_descriptors[0].shift_left, 32);
    }

    #[test]
    fn texture_contracts_repeated_handle_table_keeps_full_array() {
        let (mut program, _, sample) = handle_table_program();
        let sample_inst = program.blocks[0].inst(sample).clone();
        for _ in 0..3 { program.blocks[0].append_inst(sample_inst.clone()); }
        let host = HostTranslateInfo {
            support_handle_table_arrays: true,
            max_per_stage_descriptor_sampled_images: 64,
            max_descriptor_set_sampled_images: 64,
            max_per_stage_resources: 64,
            ..Default::default()
        };
        texture_pass(&mut MockEnvironment::default(), &mut program, &host);
        assert_eq!(program.info.texture_descriptors.len(), 1);
        assert_eq!(program.info.texture_descriptors[0].count, 64);
        for inst in program.blocks[0].iter().filter(|i| i.opcode == Opcode::ImageSampleImplicitLod) {
            let Value::Inst(index) = inst.args[0] else { panic!("array index") };
            let clamp = program.blocks[0].inst(index.inst);
            assert_eq!(clamp.opcode, Opcode::UMin32);
            assert_eq!(clamp.args[1], Value::ImmU32(63));
            assert_eq!(20u32.min(clamp.args[1].imm_u32()), 20);
        }
    }

    #[test]
    fn texture_contracts_queries_share_resolved_handle_program_type() {
        for texture_type in [TextureType::ColorArray2D, TextureType::Buffer] {
            let (mut program, _, sample) = handle_table_program();
            let handle = program.blocks[0].inst(sample).args[0];
            program.blocks[0].inst_mut(sample).flags = TextureInstInfo {
                texture_type: texture_type as u8, ..Default::default()
            }.to_u32();
            let query = program.blocks[0].append_inst(Inst::with_flags(
                Opcode::BindlessImageQueryDimensions,
                vec![handle, Value::ImmU32(0), Value::ImmU1(true)],
                TextureInstInfo { texture_type: TextureType::Color1D as u8,
                    ..Default::default() }.to_u32()));
            // Buffer handles cannot use per-invocation table arrays. Replace
            // that case with the same draw-wide storage word for both users.
            if texture_type == TextureType::Buffer {
                let word = program.blocks[0].append_inst(Inst::new(Opcode::LoadStorage32,
                    vec![Value::ImmU32(0), Value::ImmU32(0)]));
                let handle = Value::Inst(InstRef { block: 0, inst: word });
                program.blocks[0].inst_mut(sample).args[0] = handle;
                program.blocks[0].inst_mut(query).args[0] = handle;
            }
            let host = HostTranslateInfo { support_handle_table_arrays: true,
                max_per_stage_descriptor_sampled_images: 64,
                max_descriptor_set_sampled_images: 64,
                max_per_stage_resources: 64, ..Default::default() };
            texture_pass(&mut MockEnvironment::default(), &mut program, &host);
            assert_eq!(TextureInstInfo::from_u32(program.blocks[0].inst(query).flags)
                .texture_type, texture_type as u8);
            if texture_type == TextureType::Buffer {
                assert!(program.info.texture_descriptors.is_empty());
                assert_eq!(program.info.texture_buffer_descriptors.len(), 1);
            } else {
                assert_eq!(program.info.texture_descriptors.len(), 1);
                assert_eq!(program.info.texture_descriptors[0].count, 64);
                assert_eq!(TextureInstInfo::from_u32(program.blocks[0].inst(sample).flags)
                    .descriptor_index, TextureInstInfo::from_u32(program.blocks[0].inst(query).flags)
                    .descriptor_index);
            }
        }
    }

    #[test]
    fn texture_contracts_resolved_buffer_queries_do_not_consume_sampled_budget() {
        for query in [true, false] {
            let mut program = fragment_program();
            let block = &mut program.blocks[0];
            let value = |inst| Value::Inst(InstRef { block: 0, inst });
            for bank in [5, 6] {
                let offset = block.append_inst(Inst::new(Opcode::IAdd32,
                    vec![Value::ImmU32(0x80), Value::Reg(crate::ir::value::Reg(6))]));
                let load = block.append_inst(Inst::new(Opcode::GetCbufU32,
                    vec![Value::ImmU32(bank), value(offset)]));
                let (opcode, args, texture_type) = if bank == 5 {
                    (Opcode::BindlessImageSampleExplicitLod, vec![value(load), Value::Void],
                        TextureType::Color2D)
                } else if query {
                    (Opcode::BindlessImageQueryDimensions,
                        vec![value(load), Value::ImmU32(0), Value::ImmU1(true)], TextureType::Color1D)
                } else {
                    (Opcode::BindlessImageFetch,
                        vec![value(load), Value::Void, Value::Void, Value::Void, Value::Void],
                        TextureType::Color1D)
                };
                block.append_inst(Inst::with_flags(opcode, args,
                    TextureInstInfo { texture_type: texture_type as u8, ..Default::default() }.to_u32()));
            }
            let host = HostTranslateInfo { max_per_stage_descriptor_sampled_images: 64,
                max_descriptor_set_sampled_images: 64,
                max_per_stage_resources: 64, support_snorm_render_buffer: true, ..Default::default() };
            let mut env = MockEnvironment { texture_type: TextureType::Buffer, ..Default::default() };
            texture_pass(&mut env, &mut program, &host);
            assert_eq!(program.info.texture_descriptors.len(), 1);
            assert_eq!(program.info.texture_descriptors[0].count, 64);
            assert_eq!(program.info.texture_buffer_descriptors.len(), 1);
        }
    }

    #[test]
    fn texture_contracts_distinct_array_descriptors_keep_separate_budgets() {
        // Vary one descriptor property at a time, while repeating every use.
        for variant in 0..4 {
            let mut program = fragment_program();
            let block = &mut program.blocks[0];
            let value = |inst| Value::Inst(InstRef { block: 0, inst });
            for array in 0..2 {
                let stride = if variant == 0 && array == 1 { 4 } else { 3 };
                let shifted = block.append_inst(Inst::new(Opcode::ShiftLeftLogical32,
                    vec![Value::Reg(crate::ir::value::Reg(6)), Value::ImmU32(stride)]));
                let offset = block.append_inst(Inst::new(Opcode::IAdd32,
                    vec![Value::ImmU32(0x80), value(shifted)]));
                let load = block.append_inst(Inst::new(Opcode::GetCbufU32,
                    vec![Value::ImmU32(5), value(offset)]));
                let handle = if variant == 1 {
                    let sampler = block.append_inst(Inst::new(Opcode::GetCbufU32,
                        vec![Value::ImmU32(2), Value::ImmU32(0x30 + array * 4)]));
                    let shifted = block.append_inst(Inst::new(Opcode::ShiftLeftLogical32,
                        vec![value(sampler), Value::ImmU32(20)]));
                    value(block.append_inst(Inst::new(Opcode::BitwiseOr32,
                        vec![value(load), value(shifted)])))
                } else { value(load) };
                for _ in 0..2 {
                    block.append_inst(Inst::with_flags(Opcode::BindlessImageSampleExplicitLod,
                        vec![handle, Value::Void], TextureInstInfo {
                            texture_type: if variant == 2 && array == 1 {
                                TextureType::ColorArray2D as u8
                            } else { TextureType::Color2D as u8 },
                            is_depth: variant == 3 && array == 1,
                            ..Default::default()
                        }.to_u32()));
                }
            }
            let host = HostTranslateInfo { max_per_stage_descriptor_sampled_images: 64,
                max_descriptor_set_sampled_images: 64,
                max_per_stage_resources: 64, ..Default::default() };
            texture_pass(&mut MockEnvironment::default(), &mut program, &host);
            assert_eq!(program.info.texture_descriptors.len(), 2, "variant={variant}");
            assert!(program.info.texture_descriptors.iter().all(|d| d.count == 32),
                "variant={variant}: {:?}", program.info.texture_descriptors);
        }
    }

    #[test]
    fn texture_contracts_rectangle_query_uses_final_descriptor_and_element() {
        for dynamic in [false, true] {
            let mut program = fragment_program();
            let block = &mut program.blocks[0];
            let value = |inst| Value::Inst(InstRef { block: 0, inst });
            // First descriptor intentionally occupies index zero.
            block.append_inst(Inst::with_flags(Opcode::BoundImageSampleExplicitLod,
                vec![Value::ImmU32(0x10), Value::Void], TextureInstInfo {
                    texture_type: TextureType::Color2D as u8, ..Default::default()
                }.to_u32()));
            let handle = if dynamic {
                let shifted = block.append_inst(Inst::new(Opcode::ShiftLeftLogical32,
                    vec![Value::Reg(crate::ir::value::Reg(6)), Value::ImmU32(3)]));
                let address = block.append_inst(Inst::new(Opcode::IAdd32,
                    vec![Value::ImmU32(0x80), value(shifted)]));
                value(block.append_inst(Inst::new(Opcode::GetCbufU32,
                    vec![Value::ImmU32(0), value(address)])))
            } else { Value::ImmU32(0x80) };
            let coords = block.append_inst(Inst::new(Opcode::CompositeConstructF32x2,
                vec![Value::ImmF32(64.0), Value::ImmF32(32.0)]));
            let sample = block.append_inst(Inst::with_flags(
                if dynamic { Opcode::BindlessImageSampleImplicitLod }
                    else { Opcode::BoundImageSampleImplicitLod },
                vec![handle, value(coords)], TextureInstInfo {
                    texture_type: TextureType::Color2D as u8, ..Default::default()
                }.to_u32()));
            let mut env = MockEnvironment { texture_type: TextureType::Color2DRect,
                ..Default::default() };
            texture_pass(&mut env, &mut program, &HostTranslateInfo::default());
            let sample_inst = program.blocks[0].inst(sample);
            let query = program.blocks[0].iter()
                .find(|i| i.opcode == Opcode::ImageQueryDimensions).unwrap();
            assert_eq!(TextureInstInfo::from_u32(sample_inst.flags).descriptor_index, 1);
            assert_eq!(query.flags, sample_inst.flags);
            assert_eq!(query.args[0], if dynamic { sample_inst.args[0] } else { Value::ImmU32(0) });
            if dynamic { assert!(matches!(query.args[0], Value::Inst(_))); }
            // Synthetic dimensions distinguish descriptor 0 from descriptor 1.
            let widths = [128.0f32, 512.0];
            let width = widths[TextureInstInfo::from_u32(query.flags).descriptor_index as usize];
            assert_eq!(64.0 / width, 0.125);
        }
    }

    #[test]
    fn texture_contracts_depth_compare_stays_float_for_integer_format() {
        for opcode in [Opcode::BoundImageSampleDrefImplicitLod,
            Opcode::BoundImageSampleDrefExplicitLod, Opcode::BoundImageSampleExplicitLod] {
            let depth = opcode != Opcode::BoundImageSampleExplicitLod;
            let mut program = fragment_program();
            let sample = program.blocks[0].append_inst(Inst::with_flags(opcode,
                vec![Value::ImmU32(0x20), Value::Void], TextureInstInfo {
                    texture_type: TextureType::Color2D as u8, is_depth: depth,
                    ..Default::default()
                }.to_u32()));
            let mut env = MockEnvironment { integer_format: true, ..Default::default() };
            texture_pass(&mut env, &mut program, &HostTranslateInfo::default());
            assert_eq!(program.info.texture_descriptors[0].is_integer, !depth);
            assert_eq!(TextureInstInfo::from_u32(program.blocks[0].inst(sample).flags).is_integer, !depth);
            assert_eq!(env.integer_handles.len(), usize::from(!depth));
        }
    }

    #[test]
    fn zero_base_texture_arrays_preserve_table_and_index() {
        // Synthetic IR for Lichii TryGetConstBuffer's zero-base branch.
        // The index itself may come from a different cbuf: it is not a handle.
        for cbuf_index in [false, true] {
            for explicit_zero in [false, true] {
                // None: combined handle; Some: invariant separate sampler,
                // in either OR operand order.
                for reverse in [None, Some(false), Some(true)] {
                    let mut program = fragment_program();
                    let block = &mut program.blocks[0];
                    let value = |inst| Value::Inst(InstRef { block: 0, inst });
                    let index = if cbuf_index {
                        value(block.append_inst(Inst::new(Opcode::GetCbufU32,
                            vec![Value::ImmU32(2), Value::ImmU32(0x10)])))
                    } else {
                        Value::Reg(crate::ir::value::Reg(6))
                    };
                    let dynamic = block.append_inst(Inst::new(Opcode::ShiftLeftLogical32,
                        vec![index, Value::ImmU32(3)]));
                    let address = if explicit_zero {
                        value(block.append_inst(Inst::new(Opcode::IAdd32,
                            vec![Value::ImmU32(0), value(dynamic)])))
                    } else {
                        value(dynamic)
                    };
                    let texture = block.append_inst(Inst::new(Opcode::GetCbufU32,
                        vec![Value::ImmU32(5), address]));
                    let handle = if let Some(reverse) = reverse {
                        let sampler = block.append_inst(Inst::new(Opcode::GetCbufU32,
                            vec![Value::ImmU32(2), Value::ImmU32(0x34)]));
                        let sampler = block.append_inst(Inst::new(Opcode::ShiftLeftLogical32,
                            vec![value(sampler), Value::ImmU32(20)]));
                        let operands = if reverse { vec![value(sampler), value(texture)] }
                            else { vec![value(texture), value(sampler)] };
                        value(block.append_inst(Inst::new(Opcode::BitwiseOr32, operands)))
                    } else {
                        value(texture)
                    };
                    let sample = block.append_inst(Inst::with_flags(
                        Opcode::BindlessImageSampleExplicitLod, vec![handle, Value::Void],
                        TextureInstInfo { texture_type: TextureType::Color2D as u8,
                            ..Default::default() }.to_u32()));

                    texture_pass_bound_textures(&mut program, 0);

                    assert_eq!(program.info.texture_descriptors.len(), 1);
                    let desc = &program.info.texture_descriptors[0];
                    assert_eq!((desc.cbuf_index, desc.cbuf_offset, desc.shift_left), (5, 0, 0));
                    assert_eq!((desc.count, desc.size_shift), (DESCRIPTOR_MAX_COUNT, 3));
                    assert_eq!(desc.handle_program, u32::MAX);
                    assert_eq!(desc.has_secondary, reverse.is_some());
                    if reverse.is_some() {
                        assert_eq!((desc.secondary_cbuf_index, desc.secondary_cbuf_offset,
                            desc.secondary_shift_left), (2, 0x34, 20));
                    }
                    let Value::Inst(index) = program.blocks[0].inst(sample).args[0] else {
                        panic!("zero-base table lost its dynamic index");
                    };
                    let clamp = program.blocks[0].inst(index.inst);
                    assert_eq!(clamp.opcode, Opcode::UMin32);
                    assert_eq!(clamp.args[1], Value::ImmU32(DESCRIPTOR_MAX_COUNT - 1));
                    let Value::Inst(shift) = clamp.args[0] else { panic!("element-index shift") };
                    let shift = program.blocks[0].inst(shift.inst);
                    assert_eq!(shift.opcode, Opcode::ShiftRightLogical32);
                    assert_eq!(shift.args, vec![value(dynamic), Value::ImmU32(3)]);

                    // Check the descriptor's address contract against fresh synthetic
                    // words. This is not a substitute for a runtime backend test.
                    let read_word = |bank, offset| match (bank, offset) {
                        (2, 0x10) => 3u32,
                        (2, 0x34) => 5,
                        (5, 24) if desc.has_secondary => 0x11,
                        (5, 24) => 0x0050_0011,
                        _ => panic!("unexpected descriptor word {bank}:{offset}"),
                    };
                    let element = read_word(2, 0x10);
                    let primary = read_word(desc.cbuf_index,
                        desc.cbuf_offset + (element << desc.size_shift));
                    let handle = if desc.has_secondary {
                        (primary << desc.shift_left) | (read_word(desc.secondary_cbuf_index,
                            desc.secondary_cbuf_offset) << desc.secondary_shift_left)
                    } else { primary };
                    assert_eq!(handle, 0x0050_0011);
                    assert_ne!(handle, element);
                }
            }
        }
    }

    #[test]
    #[ignore = "requires spirv-val; run explicitly for zero-base texture arrays"]
    fn zero_base_texture_arrays_validate_as_spirv() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        use rspirv::{dr::Operand, spirv};

        for cbuf_index in [false, true] {
            for reverse in [None, Some(false), Some(true)] {
                let mut program = fragment_program();
                let block = &mut program.blocks[0];
                let value = |inst| Value::Inst(InstRef { block: 0, inst });
                let index = if cbuf_index {
                    block.append_inst(Inst::new(Opcode::GetCbufU32,
                        vec![Value::ImmU32(2), Value::ImmU32(0x10)]))
                } else {
                    block.append_inst(Inst::new(Opcode::LaneId, vec![]))
                };
                let offset = block.append_inst(Inst::new(Opcode::ShiftLeftLogical32,
                    vec![value(index), Value::ImmU32(3)]));
                let texture = block.append_inst(Inst::new(Opcode::GetCbufU32,
                    vec![Value::ImmU32(5), value(offset)]));
                let handle = if let Some(reverse) = reverse {
                    let sampler = block.append_inst(Inst::new(Opcode::GetCbufU32,
                        vec![Value::ImmU32(2), Value::ImmU32(0x34)]));
                    let sampler = block.append_inst(Inst::new(Opcode::ShiftLeftLogical32,
                        vec![value(sampler), Value::ImmU32(20)]));
                    let operands = if reverse { vec![value(sampler), value(texture)] }
                        else { vec![value(texture), value(sampler)] };
                    value(block.append_inst(Inst::new(Opcode::BitwiseOr32, operands)))
                } else { value(texture) };
                let coords = block.append_inst(Inst::new(Opcode::CompositeConstructF32x2,
                    vec![Value::ImmF32(0.25), Value::ImmF32(0.75)]));
                let sample = block.append_inst(Inst::with_flags(Opcode::BindlessImageGather,
                    vec![handle, value(coords), Value::Void, Value::Void],
                    TextureInstInfo { texture_type: TextureType::Color2D as u8,
                        ..Default::default() }.to_u32()));
                let red = block.append_inst(Inst::new(Opcode::CompositeExtractF32x4,
                    vec![value(sample), Value::ImmU32(0)]));
                block.append_inst(Inst::new(Opcode::SetFragColor,
                    vec![Value::ImmU32(0), Value::ImmU32(0), value(red)]));
                program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
                texture_pass_bound_textures(&mut program, 0);
                assert_eq!(program.info.texture_descriptors[0].count, DESCRIPTOR_MAX_COUNT);
                crate::ir_opt::dead_code_elimination_pass::dead_code_elimination_pass(&mut program);
                crate::ir_opt::collect_shader_info_pass::collect_shader_info_pass(&mut program);
                let profile = crate::Profile {
                    unified_descriptor_binding: true,
                    supported_spirv: 0x0001_0400,
                    support_sampled_image_array_nonuniform_indexing: true,
                    ..Default::default()
                };
                let words = crate::backend::spirv::emit_spirv::emit_spirv(
                    &program, &profile, &crate::runtime_info::RuntimeInfo::default());
                let module = rspirv::dr::load_words(&words).unwrap();
                assert!(module.functions.iter().flat_map(|f| &f.blocks)
                    .flat_map(|b| &b.instructions).any(|i| i.class.opcode == spirv::Op::ImageGather));
                assert!(module.annotations.iter().any(|i| i.operands.iter()
                    .any(|o| *o == Operand::Decoration(spirv::Decoration::NonUniform))));
                let mut validator = Command::new("spirv-val")
                    .args(["--target-env", "vulkan1.2", "-"])
                    .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
                    .spawn().expect("spirv-val is required for explicit array validation");
                let bytes: Vec<_> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
                validator.stdin.take().unwrap().write_all(&bytes).unwrap();
                let result = validator.wait_with_output().unwrap();
                assert!(result.status.success(), "cbuf_index={cbuf_index}, reverse={reverse:?}: {}",
                    String::from_utf8_lossy(&result.stderr));
            }
        }
    }

    #[test]
    fn zero_base_texture_tracking_preserves_offset_validation() {
        let mut program = fragment_program();
        let block = &mut program.blocks[0];
        let reg = Value::Reg(crate::ir::value::Reg(6));
        let other = Value::Reg(crate::ir::value::Reg(7));
        let value = |inst| Value::Inst(InstRef { block: 0, inst });
        let multiply = block.append_inst(Inst::new(Opcode::IMul32,
            vec![reg, Value::ImmU32(8)]));
        let addition = block.append_inst(Inst::new(Opcode::IAdd32, vec![reg, other]));
        let shift = block.append_inst(Inst::new(Opcode::ShiftLeftLogical32,
            vec![reg, Value::ImmU32(3)]));
        let host = HostTranslateInfo::default();
        for (bank, offset) in [(Value::ImmU32(5), value(multiply)),
            (Value::ImmU32(5), value(addition)), (reg, value(shift))] {
            let load = Inst::new(Opcode::GetCbufU32, vec![bank, offset]);
            assert!(try_get_const_buffer(&load, &program, &mut None, &host, 0).is_none());
        }
    }

    #[test]
    fn separate_dynamic_texture_array_preserves_fixed_sampler_operand() {
        // Lichii TryGetConstBuffer(BitwiseOr32), texture_pass.cpp:1178-1198:
        // exactly one dynamic operand remains the primary array; the other
        // operand is the invariant sampler, regardless of OR operand order.
        for reverse in [false, true] {
            let mut program = fragment_program();
            let block = &mut program.blocks[0];
            let value = |inst| Value::Inst(InstRef { block: 0, inst });
            let dynamic = block.append_inst(Inst::new(Opcode::ShiftLeftLogical32,
                vec![Value::Reg(crate::ir::value::Reg(6)), Value::ImmU32(3)]));
            let address = block.append_inst(Inst::new(Opcode::IAdd32,
                vec![Value::ImmU32(0xa0), value(dynamic)]));
            let texture = block.append_inst(Inst::new(Opcode::GetCbufU32,
                vec![Value::ImmU32(5), value(address)]));
            let sampler = block.append_inst(Inst::new(Opcode::GetCbufU32,
                vec![Value::ImmU32(2), Value::ImmU32(0x34)]));
            let sampler = block.append_inst(Inst::new(Opcode::ShiftLeftLogical32,
                vec![value(sampler), Value::ImmU32(20)]));
            let operands = if reverse { vec![value(sampler), value(texture)] }
                else { vec![value(texture), value(sampler)] };
            let handle = block.append_inst(Inst::new(Opcode::BitwiseOr32, operands));
            let sample = block.append_inst(Inst::with_flags(Opcode::BindlessImageSampleExplicitLod,
                vec![value(handle), Value::Void], TextureInstInfo {
                    texture_type: TextureType::Color2D as u8, ..Default::default()
                }.to_u32()));

            texture_pass_bound_textures(&mut program, 0);

            assert_eq!(program.info.texture_descriptors.len(), 1);
            let desc = &program.info.texture_descriptors[0];
            assert!(desc.has_secondary,
                "OR operand lost (reverse={reverse}): {desc:?}");
            assert_eq!((desc.cbuf_index, desc.cbuf_offset, desc.shift_left), (5, 0xa0, 0));
            assert_eq!((desc.secondary_cbuf_index, desc.secondary_cbuf_offset,
                desc.secondary_shift_left), (2, 0x34, 20));
            assert_eq!(desc.count, DESCRIPTOR_MAX_COUNT);
            assert_eq!(desc.size_shift, 3);
            assert_eq!(desc.handle_program, u32::MAX);
            let Value::Inst(index) = program.blocks[0].inst(sample).args[0] else {
                panic!("dynamic descriptor index was lost");
            };
            let clamp = program.blocks[0].inst(index.inst);
            assert_eq!(clamp.opcode, Opcode::UMin32);
            assert_eq!(clamp.args[1], Value::ImmU32(DESCRIPTOR_MAX_COUNT - 1));
            let Value::Inst(shift) = clamp.args[0] else { panic!("element-index shift") };
            let shift = program.blocks[0].inst(shift.inst);
            assert_eq!(shift.opcode, Opcode::ShiftRightLogical32);
            assert_eq!(shift.args, vec![value(dynamic), Value::ImmU32(3)]);
        }
    }

    #[test]
    fn separate_texture_or_does_not_merge_two_dynamic_arrays() {
        let mut program = fragment_program();
        let block = &mut program.blocks[0];
        let mut handles = Vec::new();
        for bank in [2, 5] {
            let address = block.append_inst(Inst::new(Opcode::IAdd32,
                vec![Value::ImmU32(0xa0), Value::Reg(crate::ir::value::Reg(bank as u64))]));
            let handle = block.append_inst(Inst::new(Opcode::GetCbufU32,
                vec![Value::ImmU32(bank), Value::Inst(InstRef { block: 0, inst: address })]));
            handles.push(Value::Inst(InstRef { block: 0, inst: handle }));
        }
        let combine = Inst::new(Opcode::BitwiseOr32, handles);
        let mut host_info = HostTranslateInfo::default();
        host_info.apply_descriptor_limit_policy();
        assert!(try_get_const_buffer(&combine, &program, &mut None,
            &host_info, 0).is_none());
        // This asserts the owner's upstream rejection, not the broader Track
        // descendant fallback; this slice does not redesign that fallback.
    }

    #[test]
    fn texture_pass_tracks_separate_texture_sampler_or_handle() {
        let mut program = fragment_program();
        let block = &mut program.blocks[0];
        block.instructions.push(Some(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(2), Value::ImmU32(0x40)],
        )));
        block.instructions.push(Some(Inst::new(
            Opcode::ShiftLeftLogical32,
            vec![
                Value::Inst(crate::ir::value::InstRef { block: 0, inst: 0 }),
                Value::ImmU32(8),
            ],
        )));
        block.instructions.push(Some(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(3), Value::ImmU32(0x20)],
        )));
        block.instructions.push(Some(Inst::new(
            Opcode::BitwiseOr32,
            vec![
                Value::Inst(crate::ir::value::InstRef { block: 0, inst: 1 }),
                Value::Inst(crate::ir::value::InstRef { block: 0, inst: 2 }),
            ],
        )));
        let flags = TextureInstInfo {
            texture_type: 2,
            ..Default::default()
        }
        .to_u32();
        block.instructions.push(Some(Inst::with_flags(
            Opcode::ImageSampleImplicitLod,
            vec![
                Value::Inst(crate::ir::value::InstRef { block: 0, inst: 3 }),
                Value::Void,
            ],
            flags,
        )));

        texture_pass_bound_textures(&mut program, 0);

        assert_eq!(program.info.texture_descriptors.len(), 1);
        let desc = &program.info.texture_descriptors[0];
        assert!(desc.has_secondary);
        assert_eq!(desc.count, 1);
        assert_eq!(desc.cbuf_index, 3);
        assert_eq!(desc.cbuf_offset, 0x20);
        assert_eq!(desc.shift_left, 0);
        assert_eq!(desc.secondary_cbuf_index, 2);
        assert_eq!(desc.secondary_cbuf_offset, 0x40);
        assert_eq!(desc.secondary_shift_left, 8);
        let inst = program.blocks[0].instructions[4].as_ref().unwrap();
        assert_eq!(inst.args[0], Value::Void);
        assert_eq!(TextureInstInfo::from_u32(inst.flags).descriptor_index, 0);
    }

    #[test]
    fn texture_tracking_must_not_treat_storage_address_as_handle() {
        let mut program = fragment_program();
        let block = &mut program.blocks[0];
        let address = block.append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(0), Value::ImmU32(0x100)],
        ));
        // GlobalMemoryToStorageBufferPass retains the base address in the
        // offset subtraction. The loaded value is not that base address.
        let offset = block.append_inst(Inst::new(
            Opcode::ISub32,
            vec![
                Value::ImmU32(0x20000020),
                Value::Inst(InstRef { block: 0, inst: address }),
            ],
        ));
        let handle = block.append_inst(Inst::new(
            Opcode::LoadStorage32,
            vec![Value::ImmU32(0), Value::Inst(InstRef { block: 0, inst: offset })],
        ));
        let found = track(
            Value::Inst(InstRef { block: 0, inst: handle }),
            &program,
            &mut None,
            &HostTranslateInfo::default(),
            0,
        );
        assert!(found.is_none(), "a memory load must not resolve to its address cbuf: {found:?}");
    }

    #[test]
    fn conditional_handle_phi_selects_the_matching_branch() {
        for negate in [false, true] {
            let mut program = fragment_program();
            program.blocks = (0..5).map(|_| Block::new()).collect();
            program.blocks[0].imm_successors = vec![1, 2];
            program.blocks[1].imm_predecessors = vec![0];
            program.blocks[1].imm_successors = vec![2];
            program.blocks[2].imm_predecessors = vec![0, 1];
            program.blocks[2].imm_successors = vec![3, 4];
            program.blocks[3].imm_predecessors = vec![2];
            program.blocks[3].imm_successors = vec![4];
            program.blocks[4].imm_predecessors = vec![2, 3];
            let condition_inst = program.blocks[0].append_new_inst(
                Opcode::GetCbufU32, vec![Value::ImmU32(0), Value::ImmU32(16)]);
            let cond = Value::Inst(InstRef { block: 0, inst: condition_inst });
            program.blocks[0].append_new_inst(Opcode::ConditionRef, vec![cond]);
            let mut phi = Inst::phi();
            phi.phi_args = vec![(0, Value::ImmU32(11)), (1, Value::ImmU32(22))];
            let phi_index = program.blocks[2].append_inst(phi);
            let phi_value = Value::Inst(InstRef { block: 2, inst: phi_index });
            let cond = if negate {
                let inst = program.blocks[2].append_new_inst(Opcode::LogicalNot, vec![cond]);
                Value::Inst(InstRef { block: 2, inst })
            } else { cond };
            program.blocks[2].append_new_inst(Opcode::ConditionRef, vec![cond]);
            assert_eq!(conditional_header(&program, 3), Some(2));
            assert_eq!(resolve_conditional_phi(&program, phi_value, 3),
                       Value::ImmU32(if negate { 11 } else { 22 }));
            // Outside that conditional body the merge remains unresolved.
            assert_eq!(resolve_conditional_phi(&program, phi_value, 4), phi_value);
            // A different condition cannot select a predecessor of this Phi.
            program.blocks[2].back_mut().args[0] = Value::ImmU32(123);
            assert_eq!(resolve_conditional_phi(&program, phi_value, 3), phi_value);
        }
    }

    #[test]
    fn handle_builder_extracts_every_word_of_storage_vector() {
        use crate::shader_info::evaluate_handle_program;
        for (load_op, extract_op, words) in [
            (Opcode::LoadStorage64, Opcode::CompositeExtractU32x2, 2),
            (Opcode::LoadStorage128, Opcode::CompositeExtractU32x4, 4),
        ] {
            for element in 0..words {
                let mut program = fragment_program();
                let load = program.blocks[0].append_new_inst(load_op,
                    vec![Value::ImmU32(2), Value::ImmU32(32)]);
                let word = program.blocks[0].append_new_inst(extract_op,
                    vec![Value::Inst(InstRef { block: 0, inst: load }), Value::ImmU32(element)]);
                let mut code = Vec::new();
                assert!(HandleBuildState::default().build_handle_program(&program,
                    Value::Inst(InstRef { block: 0, inst: word }), &mut code, &mut 4096, 0));
                assert_eq!(evaluate_handle_program(&code, |_, _| panic!("storage only"),
                    |binding, offset| {
                        assert_eq!((binding, offset), (2, 32 + element * 4)); 123
                    }, 0), Some(123));
            }
        }
    }

    #[test]
    fn handle_builder_reads_storage_at_computed_byte_offset() {
        use crate::shader_info::evaluate_handle_program;
        let mut program = fragment_program();
        let reference = |inst| Value::Inst(InstRef { block: 0, inst });
        let block = &mut program.blocks[0];
        let cbuf = block.append_new_inst(Opcode::GetCbufU32,
            vec![Value::ImmU32(0), Value::ImmU32(16)]);
        let offset = block.append_new_inst(Opcode::ISub32,
            vec![Value::ImmU32(0x1020), reference(cbuf)]);
        let load = block.append_new_inst(Opcode::LoadStorage64,
            vec![Value::ImmU32(2), reference(offset)]);
        let word = block.append_new_inst(Opcode::CompositeExtractU32x2,
            vec![reference(load), Value::ImmU32(1)]);
        let mut code = Vec::new();
        assert!(HandleBuildState::default().build_handle_program(
            &program, reference(word), &mut code, &mut 4096, 0));
        for data in [9, 15] {
            assert_eq!(evaluate_handle_program(&code,
                |index, offset| { assert_eq!((index, offset), (0, 16)); 0x1000 },
                |index, offset| { assert_eq!((index, offset), (2, 36)); data }, 0), Some(data));
        }
    }

    #[test]
    fn texture_pass_keeps_distinct_storage_handles_and_query_types() {
        use crate::shader_info::{evaluate_handle_program, TextureType};
        let mut program = fragment_program();
        for (offset, texture_type) in [(4, TextureType::Color3D), (8, TextureType::ColorCube)] {
            let load = program.blocks[0].append_new_inst(Opcode::LoadStorage32,
                vec![Value::ImmU32(0), Value::ImmU32(offset)]);
            let handle = Value::Inst(InstRef { block: 0, inst: load });
            let flags = TextureInstInfo { texture_type: texture_type as u8, ..Default::default() }.to_u32();
            program.blocks[0].append_inst(Inst::with_flags(Opcode::ImageSampleImplicitLod,
                vec![handle, Value::Void], flags));
            program.blocks[0].append_inst(Inst::with_flags(Opcode::ImageQueryDimensions,
                vec![handle, Value::ImmU32(0)], 0));
        }
        texture_pass_bound_textures(&mut program, 0);
        assert_eq!(program.info.handle_programs.len(), 2);
        assert_eq!(program.info.texture_descriptors.len(), 2);
        for (index, ty) in [TextureType::Color3D, TextureType::ColorCube].into_iter().enumerate() {
            let desc = &program.info.texture_descriptors[index];
            assert_eq!(desc.handle_program, index as u32);
            assert_eq!(desc.texture_type, ty);
            let code = &program.info.handle_programs[index];
            for contents in [100, 200] {
                assert_eq!(evaluate_handle_program(code, |_, _| panic!("not a cbuf handle"),
                    |binding, offset| { assert_eq!(binding, 0); contents + offset }, 0),
                    Some(contents + (index as u32 + 1) * 4));
            }
        }
    }

    #[test]
    fn handle_builder_rolls_back_slots_from_failed_phi_input() {
        use crate::shader_info::evaluate_handle_program;
        let mut program = fragment_program();
        let reference = |inst| Value::Inst(InstRef { block: 0, inst });
        let block = &mut program.blocks[0];
        let shared = block.append_new_inst(Opcode::GetCbufU32,
            vec![Value::ImmU32(1), Value::ImmU32(8)]);
        block.instructions[shared as usize].as_mut().unwrap().use_count = 2;
        let unsupported = block.append_new_inst(Opcode::LaneId, vec![]);
        let bad = block.append_new_inst(Opcode::IAdd32, vec![reference(shared), reference(unsupported)]);
        let good = block.append_new_inst(Opcode::IAdd32, vec![reference(shared), Value::ImmU32(1)]);
        let mut phi = Inst::phi();
        phi.phi_args = vec![(0, reference(bad)), (0, reference(good))];
        let value = reference(block.append_inst(phi));
        let mut code = Vec::new();
        let mut state = HandleBuildState::default();
        assert!(state.build_handle_program(&program, value, &mut code, &mut 4096, 0));
        assert_eq!(evaluate_handle_program(&code, |_, _| 6, |_, _| unreachable!(), 0), Some(7));
        assert_eq!(state.slot_count, 1);
        assert_eq!(state.failure, Some(Opcode::LaneId));
    }

    #[test]
    fn handle_builder_rejects_cycles_and_exhausted_budget() {
        let mut program = fragment_program();
        let value = Value::Inst(InstRef { block: 0, inst: 0 });
        let mut phi = Inst::phi();
        phi.phi_args = vec![(0, value)];
        program.blocks[0].append_inst(phi);
        let mut state = HandleBuildState::default();
        let mut code = Vec::new();
        assert!(!state.build_handle_program(&program, value, &mut code, &mut 4096, 0));
        assert_eq!(state.failure, Some(Opcode::Phi));
        assert!(code.is_empty());
        assert!(state.slots.is_empty());
        assert!(!state.build_handle_program(&program, Value::ImmU32(3), &mut code, &mut 0, 0));
        assert!(code.is_empty());
    }

    #[test]
    fn handle_builder_flags_bitfields_and_clamps() {
        use crate::shader_info::evaluate_handle_program;
        for (op, args, expected) in [
            (Opcode::BitFieldUExtract, vec![0x1234, 4, 8], 0x23),
            (Opcode::BitFieldSExtract, vec![0x800, 8, 4], 0xfffffff8),
            (Opcode::BitFieldInsert, vec![0x1234, 0xab, 4, 8], 0x1ab4),
            (Opcode::UClamp32, vec![12, 2, 8], 8),
            (Opcode::SClamp32, vec![u32::MAX, 0, 8], 0),
            (Opcode::SelectU32, vec![0, 11, 22], 22),
            (Opcode::GetZeroFromOp, vec![0], 1),
            (Opcode::GetSignFromOp, vec![u32::MAX], 1),
        ] {
            let mut program = fragment_program();
            let inst = program.blocks[0].append_new_inst(op, args.into_iter().map(Value::ImmU32).collect());
            let mut code = Vec::new();
            assert!(HandleBuildState::default().build_handle_program(&program,
                Value::Inst(InstRef { block: 0, inst }), &mut code, &mut 4096, 0));
            assert_eq!(evaluate_handle_program(&code, |_, _| 0, |_, _| 0, 0), Some(expected), "{op:?}");
        }
        let mut program = fragment_program();
        let add = program.blocks[0].append_new_inst(Opcode::IAdd32,
            vec![Value::ImmU32(u32::MAX), Value::ImmU32(2)]);
        let carry = program.blocks[0].append_new_inst(Opcode::GetCarryFromOp,
            vec![Value::Inst(InstRef { block: 0, inst: add })]);
        let mut code = Vec::new();
        assert!(HandleBuildState::default().build_handle_program(&program,
            Value::Inst(InstRef { block: 0, inst: carry }), &mut code, &mut 4096, 0));
        assert_eq!(evaluate_handle_program(&code, |_, _| 0, |_, _| 0, 0), Some(1));
        let alias = program.blocks[0].append_new_inst(Opcode::Identity, vec![Value::ImmU1(true)]);
        code.clear();
        assert!(HandleBuildState::default().build_handle_program(&program,
            Value::Inst(InstRef { block: 0, inst: alias }), &mut code, &mut 4096, 0));
        assert_eq!(evaluate_handle_program(&code, |_, _| 0, |_, _| 0, 0), Some(1));
    }

    #[test]
    fn texture_pass_tracks_cbuf_through_upstream_breadth_first_search() {
        let mut program = fragment_program();
        let block = &mut program.blocks[0];
        block.append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(6), Value::ImmU32(0x70)],
        ));
        // IAdd32 is not a texture-handle transform recognized by
        // TryGetConstBuffer. Eden's Track still searches its arguments.
        block.append_inst(Inst::new(
            Opcode::IAdd32,
            vec![Value::Inst(InstRef { block: 0, inst: 0 }), Value::ImmU32(0)],
        ));
        let flags = TextureInstInfo {
            texture_type: TextureType::Color2D as u8,
            ..Default::default()
        }
        .to_u32();
        block.append_inst(Inst::with_flags(
            Opcode::ImageSampleImplicitLod,
            vec![Value::Inst(InstRef { block: 0, inst: 1 }), Value::Void],
            flags,
        ));

        texture_pass_bound_textures(&mut program, 0);

        let descriptor = &program.info.texture_descriptors[0];
        assert_eq!(descriptor.cbuf_index, 6);
        assert_eq!(descriptor.cbuf_offset, 0x70);
    }

    #[test]
    fn texture_pass_tracks_bindless_handle_through_phi_operands() {
        let mut program = fragment_program();
        let block = &mut program.blocks[0];
        let left = block.append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(4), Value::ImmU32(0x20)],
        ));
        let right = block.append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(5), Value::ImmU32(0x40)],
        ));
        let mut phi = Inst::phi();
        phi.add_phi_operand(
            0,
            Value::Inst(InstRef {
                block: 0,
                inst: left,
            }),
        );
        phi.add_phi_operand(
            0,
            Value::Inst(InstRef {
                block: 0,
                inst: right,
            }),
        );
        let phi = block.append_inst(phi);
        block.append_inst(Inst::with_flags(
            Opcode::BindlessImageSampleExplicitLod,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: phi,
                }),
                Value::Void,
            ],
            TextureInstInfo {
                texture_type: TextureType::Color2D as u8,
                ..Default::default()
            }
            .to_u32(),
        ));

        texture_pass_bound_textures(&mut program, 0);

        let descriptor = &program.info.texture_descriptors[0];
        assert_eq!(descriptor.cbuf_index, 5);
        assert_eq!(descriptor.cbuf_offset, 0x40);
    }

    #[test]
    fn texture_pass_tracks_dynamic_cbuf_offset_as_descriptor_array() {
        let mut program = fragment_program();
        let block = &mut program.blocks[0];
        block.append_inst(Inst::new(
            Opcode::IAdd32,
            vec![Value::ImmU32(0x80), Value::Reg(crate::ir::value::Reg(4))],
        ));
        block.append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![
                Value::ImmU32(5),
                Value::Inst(crate::ir::value::InstRef { block: 0, inst: 0 }),
            ],
        ));
        let flags = TextureInstInfo {
            texture_type: 2,
            ..Default::default()
        }
        .to_u32();
        block.append_inst(Inst::with_flags(
            Opcode::ImageSampleImplicitLod,
            vec![
                Value::Inst(crate::ir::value::InstRef { block: 0, inst: 1 }),
                Value::Void,
            ],
            flags,
        ));

        texture_pass_bound_textures(&mut program, 0);

        assert_eq!(program.info.texture_descriptors.len(), 1);
        let desc = &program.info.texture_descriptors[0];
        assert_eq!(desc.cbuf_index, 5);
        assert_eq!(desc.cbuf_offset, 0x80);
        assert_eq!(desc.count, DESCRIPTOR_MAX_COUNT);
        assert_eq!(desc.size_shift, DESCRIPTOR_SIZE_SHIFT);
        let inst = program.blocks[0].instructions[2].as_ref().unwrap();
        let Value::Inst(dynamic_index) = inst.args[0] else {
            panic!("expected dynamic descriptor index instruction");
        };
        assert_eq!(
            program.blocks[0].inst(dynamic_index.inst).opcode,
            Opcode::UMin32
        );
        let ordered_opcodes: Vec<Opcode> = program.blocks[0]
            .indexed_iter()
            .map(|(_, inst)| inst.opcode)
            .collect();
        let shift_pos = ordered_opcodes
            .iter()
            .position(|&opcode| opcode == Opcode::ShiftRightLogical32)
            .unwrap();
        let umin_pos = ordered_opcodes
            .iter()
            .position(|&opcode| opcode == Opcode::UMin32)
            .unwrap();
        let sample_pos = ordered_opcodes
            .iter()
            .position(|&opcode| opcode == Opcode::ImageSampleImplicitLod)
            .unwrap();
        assert!(shift_pos < umin_pos);
        assert!(umin_pos < sample_pos);
    }

    #[test]
    fn dynamic_descriptor_count_matches_upstream_cbuf_budget() {
        assert_eq!(dynamic_descriptor_count(0, 3, 1024), 1024);
        assert_eq!(dynamic_descriptor_count(0x80, 3, 1024), 1024);
        assert_eq!(dynamic_descriptor_count(0x80, 5, 16), 4);
        assert_eq!(dynamic_descriptor_count(0x100, 3, 16), 1);
        assert_eq!(dynamic_descriptor_count(0, 31, 1024), 1);
    }

    #[test]
    fn dynamic_sampled_texture_cap_reserves_static_resources() {
        let mut info = ShaderInfo::default();
        info.texture_descriptors.push(TextureDescriptor {
            handle_program: u32::MAX,
            texture_type: TextureType::Color2D,
            is_depth: false,
            is_multisample: false,
            is_integer: false,
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: 1,
            size_shift: DESCRIPTOR_SIZE_SHIFT,
        });
        let host_info = HostTranslateInfo {
            max_per_stage_descriptor_sampled_images: 9,
            max_descriptor_set_sampled_images: 7,
            max_per_stage_resources: 5,
            ..HostTranslateInfo::default()
        };

        assert_eq!(dynamic_sampled_texture_cap(&info, &host_info, 2), 2);
    }

    #[test]
    fn texture_pass_tracks_mask_constant_loaded_from_cbuf_bank_one() {
        let mut program = fragment_program();
        let block = &mut program.blocks[0];
        block.instructions.push(Some(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(4), Value::ImmU32(0x40)],
        )));
        block.instructions.push(Some(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(1), Value::ImmU32(0x10)],
        )));
        block.instructions.push(Some(Inst::new(
            Opcode::BitwiseAnd32,
            vec![
                Value::Inst(crate::ir::value::InstRef { block: 0, inst: 0 }),
                Value::Inst(crate::ir::value::InstRef { block: 0, inst: 1 }),
            ],
        )));
        let flags = TextureInstInfo {
            texture_type: TextureType::Color2D as u8,
            ..Default::default()
        }
        .to_u32();
        block.instructions.push(Some(Inst::with_flags(
            Opcode::BindlessImageSampleImplicitLod,
            vec![
                Value::Inst(crate::ir::value::InstRef { block: 0, inst: 2 }),
                Value::Void,
            ],
            flags,
        )));

        let mut env = MockEnvironment::default();
        texture_pass(&mut env, &mut program, &HostTranslateInfo::default());

        let inst = program.blocks[0].instructions[3].as_ref().unwrap();
        assert_eq!(inst.opcode, Opcode::ImageSampleImplicitLod);
        assert_eq!(inst.args[0], Value::Void);
        assert_eq!(program.info.texture_descriptors.len(), 1);
        let desc = &program.info.texture_descriptors[0];
        assert_eq!(desc.cbuf_index, 4);
        assert_eq!(desc.cbuf_offset, 0x40);
        assert_eq!(desc.shift_left, 3);
    }

    #[test]
    fn texture_pass_normalizes_bound_opcode_to_indexed_opcode() {
        let mut program = fragment_program();
        let flags = TextureInstInfo {
            texture_type: 2,
            ..Default::default()
        }
        .to_u32();
        program.blocks[0].instructions.push(Some(Inst::with_flags(
            Opcode::BoundImageSampleImplicitLod,
            vec![Value::ImmU32(0x30), Value::Void],
            flags,
        )));

        texture_pass_bound_textures(&mut program, 4);

        let inst = program.blocks[0].instructions[0].as_ref().unwrap();
        assert_eq!(inst.opcode, Opcode::ImageSampleImplicitLod);
        assert_eq!(inst.args[0], Value::Void);
        assert_eq!(program.info.texture_descriptors[0].cbuf_index, 4);
        assert_eq!(program.info.texture_descriptors[0].cbuf_offset, 0x30);
    }

    #[test]
    fn texture_pass_registers_image_atomic_as_image_descriptor() {
        let mut program = fragment_program();
        let flags = TextureInstInfo {
            texture_type: 2,
            ..Default::default()
        }
        .to_u32();
        program.blocks[0].instructions.push(Some(Inst::with_flags(
            Opcode::BoundImageAtomicIAdd32,
            vec![Value::ImmU32(0x38), Value::Void, Value::ImmU32(1)],
            flags,
        )));

        texture_pass_bound_textures(&mut program, 6);

        let inst = program.blocks[0].instructions[0].as_ref().unwrap();
        assert_eq!(inst.opcode, Opcode::ImageAtomicIAdd32);
        assert_eq!(program.info.image_descriptors.len(), 1);
        let desc = &program.info.image_descriptors[0];
        assert_eq!(desc.cbuf_index, 6);
        assert_eq!(desc.cbuf_offset, 0x38);
        assert!(desc.is_read);
        assert!(desc.is_written);
    }

    #[test]
    fn texture_pass_reads_storage_image_format_from_flags() {
        let mut program = fragment_program();
        let flags = TextureInstInfo {
            texture_type: TextureType::Color2D as u8,
            image_format: ImageFormat::R32G32Uint as u8,
            ..Default::default()
        }
        .to_u32();
        program.blocks[0].instructions.push(Some(Inst::with_flags(
            Opcode::BoundImageRead,
            vec![Value::ImmU32(0x3c), Value::Void],
            flags,
        )));

        texture_pass_bound_textures(&mut program, 6);

        assert_eq!(program.info.image_descriptors.len(), 1);
        assert_eq!(
            program.info.image_descriptors[0].format,
            ImageFormat::R32G32Uint
        );
    }

    #[test]
    fn texture_pass_preserves_upstream_texture_type_enum_values() {
        let mut program = fragment_program();
        let flags = TextureInstInfo {
            texture_type: TextureType::ColorArray2D as u8,
            ..Default::default()
        }
        .to_u32();
        program.blocks[0].instructions.push(Some(Inst::with_flags(
            Opcode::BoundImageSampleImplicitLod,
            vec![Value::ImmU32(0x50), Value::Void],
            flags,
        )));

        texture_pass_bound_textures(&mut program, 6);

        assert_eq!(program.info.texture_descriptors.len(), 1);
        assert_eq!(
            program.info.texture_descriptors[0].texture_type,
            TextureType::ColorArray2D
        );
    }

    #[test]
    fn texture_pass_patches_rect_implicit_lod_coords_before_sample() {
        let mut program = fragment_program();
        let coord = program.blocks[0].append_inst(Inst::new(
            Opcode::CompositeConstructF32x2,
            vec![Value::ImmF32(64.0), Value::ImmF32(32.0)],
        ));
        let flags = TextureInstInfo {
            texture_type: TextureType::Color2D as u8,
            ..Default::default()
        }
        .to_u32();
        program.blocks[0].append_inst(Inst::with_flags(
            Opcode::BoundImageSampleImplicitLod,
            vec![
                Value::ImmU32(0x50),
                Value::Inst(InstRef {
                    block: 0,
                    inst: coord,
                }),
            ],
            flags,
        ));

        let mut env = MockEnvironment {
            texture_type: TextureType::Color2DRect,
            ..Default::default()
        };
        texture_pass(&mut env, &mut program, &HostTranslateInfo::default());

        let ordered_opcodes: Vec<Opcode> = program.blocks[0]
            .indexed_iter()
            .map(|(_, inst)| inst.opcode)
            .collect();
        let sample_pos = ordered_opcodes
            .iter()
            .position(|&opcode| opcode == Opcode::ImageSampleImplicitLod)
            .unwrap();
        let query_pos = ordered_opcodes
            .iter()
            .position(|&opcode| opcode == Opcode::ImageQueryDimensions)
            .unwrap();
        assert!(query_pos < sample_pos);

        let sample = program.blocks[0].indexed_iter().last().unwrap().1;
        assert_eq!(sample.opcode, Opcode::ImageSampleImplicitLod);
        assert!(matches!(sample.args[1], Value::Inst(_)));
        assert!(ordered_opcodes.contains(&Opcode::CompositeConstructF32x2));
        assert!(ordered_opcodes.contains(&Opcode::FPRecip32));
    }

    #[test]
    fn texture_pass_patches_snorm_texel_fetch_uses() {
        let mut program = fragment_program();
        let flags = TextureInstInfo {
            texture_type: TextureType::Color1D as u8,
            ..Default::default()
        }
        .to_u32();
        program.blocks[0].append_inst(Inst::with_flags(
            Opcode::BoundImageFetch,
            vec![Value::ImmU32(0x28), Value::Void, Value::ImmU32(0)],
            flags,
        ));
        program.blocks[0].append_inst(Inst::new(
            Opcode::CompositeExtractF32x4,
            vec![Value::Inst(InstRef { block: 0, inst: 0 }), Value::ImmU32(0)],
        ));

        let mut env = MockEnvironment {
            texture_type: TextureType::Buffer,
            texture_pixel_format: TexturePixelFormat::R8Snorm,
            ..Default::default()
        };
        texture_pass(&mut env, &mut program, &HostTranslateInfo::default());

        let consumer = program.blocks[0].inst(1);
        let Value::Inst(converted_ref) = consumer.args[0] else {
            panic!("expected consumer to use converted SNORM value");
        };
        assert_eq!(
            program.blocks[0].inst(converted_ref.inst).opcode,
            Opcode::CompositeConstructF32x4
        );
        let ordered_opcodes: Vec<Opcode> = program.blocks[0]
            .indexed_iter()
            .map(|(_, inst)| inst.opcode)
            .collect();
        let first_fetch = ordered_opcodes
            .iter()
            .position(|&opcode| opcode == Opcode::ImageFetch)
            .unwrap();
        let construct = ordered_opcodes
            .iter()
            .position(|&opcode| opcode == Opcode::CompositeConstructF32x4)
            .unwrap();
        let original_fetch = ordered_opcodes
            .iter()
            .rposition(|&opcode| opcode == Opcode::ImageFetch)
            .unwrap();
        assert!(first_fetch < construct);
        assert!(construct < original_fetch);
        assert!(ordered_opcodes.contains(&Opcode::BitCastU32F32));
        assert!(ordered_opcodes.contains(&Opcode::ConvertF32S32));
        assert!(ordered_opcodes.contains(&Opcode::FPMul32));
    }

    #[test]
    fn replace_uses_with_rewrites_syntax_conditions() {
        let mut program = fragment_program();
        let old = program.blocks[0].append_inst(Inst::new(
            Opcode::ImageFetch,
            vec![Value::ImmU32(0), Value::Void, Value::ImmU32(0)],
        ));
        let old_ref = InstRef {
            block: 0,
            inst: old,
        };
        let replacement = Value::ImmU1(true);
        program.syntax_list.push(SyntaxNode::If {
            cond: Value::Inst(old_ref),
            body: 0,
            merge: 0,
        });

        replace_uses_with(&mut program, old_ref, replacement);

        let SyntaxNode::If { cond, .. } = program.syntax_list[0] else {
            panic!("expected If node");
        };
        assert_eq!(cond, replacement);
    }

    #[test]
    fn computed_program_merge_relocates_storage_and_image_instructions() {
        use crate::shader_info::{HandleOp as H, StorageBufferDescriptor};
        use crate::ir_opt::global_memory_to_storage_buffer_pass::{join_storage_info, remap_storage_bindings};
        let mut base = ShaderInfo::default();
        let mut source = ShaderInfo::default();
        let a = StorageBufferDescriptor { cbuf_index: 2, cbuf_offset: 16, count: 1, is_written: false, ..Default::default() };
        let b = StorageBufferDescriptor { cbuf_offset: 32, ..a.clone() };
        base.storage_buffers_descriptors.push(a.clone());
        source.storage_buffers_descriptors.extend([b, a]);
        let program = vec![H::Immediate as u32, H::Storage as u32, H::Storage as u32, 0];
        base.handle_programs.push(program.clone());
        source.handle_programs.push(program);
        let desc = ImageDescriptor {
            handle_program: 0, texture_type: TextureType::Color2D, format: ImageFormat::R32Uint,
            is_written: true, is_read: false, is_integer: false,
            cbuf_index: 0, cbuf_offset: 0, count: 1, size_shift: DESCRIPTOR_SIZE_SHIFT,
        };
        base.image_descriptors.push(desc.clone());
        source.image_descriptors.push(desc);
        let storage = join_storage_info(&mut base, &mut source);
        assert_eq!(storage, [1, 0]);
        let images = join_texture_info(&mut base, &mut source);
        assert_eq!(images[3], [1]);
        // An immediate whose value equals the Storage opcode must not be patched.
        assert_eq!(base.handle_programs[1], [H::Immediate as u32, H::Storage as u32, H::Storage as u32, 1]);
        let mut blocks = [Block::new()];
        blocks[0].append_inst(Inst::with_flags(Opcode::ImageWrite, vec![], TextureInstInfo {
            descriptor_index: 0, texture_type: TextureType::Color2D as u8, ..Default::default()
        }.to_u32()));
        blocks[0].append_inst(Inst::new(Opcode::WriteStorage32,
            vec![Value::ImmU32(0), Value::ImmU32(0), Value::ImmU32(7)]));
        blocks[0].append_inst(Inst::new(Opcode::StorageAtomicIAdd32,
            vec![Value::ImmU32(1), Value::ImmU32(0), Value::ImmU32(7)]));
        remap_texture_bindings(&mut blocks, &images);
        remap_storage_bindings(&mut blocks, &storage);
        assert_eq!(TextureInstInfo::from_u32(blocks[0].inst(0).flags).descriptor_index, 1);
        assert_eq!(blocks[0].inst(1).args[0], Value::ImmU32(1));
        assert_eq!(blocks[0].inst(2).args[0], Value::ImmU32(0));
    }

    #[test]
    fn join_texture_info_preserves_distinct_computed_programs() {
        use crate::shader_info::HandleOp as H;
        let mut base = ShaderInfo::default();
        let mut source = ShaderInfo::default();
        let descriptor = ImageDescriptor {
            handle_program: 0,
            texture_type: TextureType::Color2D,
            format: ImageFormat::R32Uint,
            is_written: false, is_read: true, is_integer: false,
            cbuf_index: 0, cbuf_offset: 0, count: 1,
            size_shift: DESCRIPTOR_SIZE_SHIFT,
        };
        base.handle_programs.push(vec![H::Immediate as u32, 17]);
        source.handle_programs.push(vec![H::Immediate as u32, 29]);
        base.image_descriptors.push(descriptor.clone());
        source.image_descriptors.push(descriptor);
        join_texture_info(&mut base, &mut source);
        assert_eq!(base.image_descriptors.len(), 2,
            "source-local program zero must not alias destination program zero");
        for (descriptor, expected) in base.image_descriptors.iter().zip([17, 29]) {
            assert_eq!(crate::shader_info::evaluate_handle_program(
                &base.handle_programs[descriptor.handle_program as usize],
                |_, _| panic!("constant program"), |_, _| panic!("constant program"), 0,
            ), Some(expected));
        }
    }

    #[test]
    fn join_texture_info_dedupes_and_merges_image_flags() {
        let mut base = ShaderInfo::default();
        let mut source = ShaderInfo::default();
        base.image_descriptors.push(ImageDescriptor {
            handle_program: u32::MAX,
            texture_type: TextureType::Color2D,
            format: ImageFormat::R32Uint,
            is_written: false,
            is_read: true,
            is_integer: false,
            cbuf_index: 1,
            cbuf_offset: 0x20,
            count: 1,
            size_shift: DESCRIPTOR_SIZE_SHIFT,
        });
        source.image_descriptors.push(ImageDescriptor {
            handle_program: u32::MAX,
            texture_type: TextureType::Color2D,
            format: ImageFormat::R32Uint,
            is_written: true,
            is_read: false,
            is_integer: true,
            cbuf_index: 1,
            cbuf_offset: 0x20,
            count: 1,
            size_shift: DESCRIPTOR_SIZE_SHIFT,
        });

        join_texture_info(&mut base, &mut source);

        assert!(source.image_descriptors.is_empty());
        assert_eq!(base.image_descriptors.len(), 1);
        let desc = &base.image_descriptors[0];
        assert!(desc.is_read);
        assert!(desc.is_written);
        assert!(desc.is_integer);
    }

    #[test]
    fn image_fetch_clears_ms_arg_for_non_2d_fetches() {
        let mut program = fragment_program();
        let flags = TextureInstInfo {
            texture_type: TextureType::Color1D as u8,
            ..Default::default()
        }
        .to_u32();
        program.blocks[0].instructions.push(Some(Inst::with_flags(
            Opcode::BoundImageFetch,
            vec![
                Value::ImmU32(0x48),
                Value::Void,
                Value::Void,
                Value::Void,
                Value::ImmU32(7),
            ],
            flags,
        )));

        texture_pass_bound_textures(&mut program, 2);

        let inst = program.blocks[0].instructions[0].as_ref().unwrap();
        assert_eq!(inst.opcode, Opcode::ImageFetch);
        assert_eq!(inst.args[4], Value::Void);
    }

    #[test]
    fn join_texture_info_dedupes_and_merges_multisample_flag() {
        let mut base = ShaderInfo::default();
        let mut source = ShaderInfo::default();
        base.texture_descriptors.push(TextureDescriptor {
            handle_program: u32::MAX,
            texture_type: TextureType::Color2D,
            is_depth: false,
            is_multisample: false,
            is_integer: false,
            has_secondary: false,
            cbuf_index: 1,
            cbuf_offset: 0x20,
            shift_left: 0,
            secondary_cbuf_index: 1,
            secondary_cbuf_offset: 0x20,
            secondary_shift_left: 0,
            count: 1,
            size_shift: DESCRIPTOR_SIZE_SHIFT,
        });
        source.texture_descriptors.push(TextureDescriptor {
            handle_program: u32::MAX,
            texture_type: TextureType::Color2D,
            is_depth: false,
            is_multisample: true,
            is_integer: true,
            has_secondary: false,
            cbuf_index: 1,
            cbuf_offset: 0x20,
            shift_left: 0,
            secondary_cbuf_index: 1,
            secondary_cbuf_offset: 0x20,
            secondary_shift_left: 0,
            count: 1,
            size_shift: DESCRIPTOR_SIZE_SHIFT,
        });

        join_texture_info(&mut base, &mut source);

        assert!(source.texture_descriptors.is_empty());
        assert_eq!(base.texture_descriptors.len(), 1);
        assert!(base.texture_descriptors[0].is_multisample);
        assert!(base.texture_descriptors[0].is_integer);
    }
}

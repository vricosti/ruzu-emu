// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shader pipeline cache: Maxwell binary → compiled SPIR-V.
//!
//! Caches compiled shaders by hashing the Maxwell binary + runtime state.
//! The cache is device-independent — it stores SPIR-V words that can be
//! loaded into VkShaderModule by the Vulkan backend.
//!
//! Matches zuyu's `vk_pipeline_cache.cpp` concept.

use std::collections::{hash_map::DefaultHasher, HashMap};
use std::hash::{Hash, Hasher};

use super::backend;
use super::environment::Environment;
use super::frontend::control_flow;
use super::frontend::structured_control_flow::{self, Expr, StructuredAction};
use super::frontend::translate::TranslatorVisitor;
use super::frontend::translate_program::{
    add_nvn_storage_buffers, collect_interpolation_info, convert_legacy_to_generic,
    merge_dual_vertex_programs, optimize_program_with_env, optimize_program_without_env,
    remove_unreachable_blocks,
};
use super::ir::basic_block::Block;
use super::ir::emitter::Emitter;
use super::ir::instruction::Inst;
use super::ir::opcodes::Opcode;
use super::ir::post_order::post_order;
use super::ir::program::{Program, ShaderInfo, SyntaxNode};
use super::ir::types::{OutputTopology, ShaderStage};
use super::ir::value::{InstRef, Value};
use super::profile::Profile;
use super::program_header::ProgramHeader;
use super::runtime_info::RuntimeInfo;

/// Key for looking up a cached shader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderKey {
    /// Hash of the Maxwell binary code.
    pub code_hash: u64,
    /// Hash of pipeline runtime state that affects translation/emission.
    pub runtime_hash: u64,
    /// Shader stage.
    pub stage: ShaderStage,
}

/// A compiled shader ready for Vulkan consumption.
#[derive(Debug, Clone)]
pub struct CompiledShader {
    /// SPIR-V words ready for VkShaderModule creation.
    pub spirv_words: Vec<u32>,
    /// Shader resource usage information.
    pub info: ShaderInfo,
    /// Shader stage.
    pub stage: ShaderStage,
}

/// A compiled shader emitted as GLSL source for the OpenGL backend.
#[derive(Debug, Clone)]
pub struct CompiledGlslShader {
    /// GLSL source ready for `glShaderSource` / `glCompileShader`.
    pub source: String,
    /// Shader resource usage information.
    pub info: ShaderInfo,
    /// Shader stage.
    pub stage: ShaderStage,
}

/// Pipeline cache — maps shader keys to compiled SPIR-V.
pub struct PipelineCache {
    /// Cached compiled shaders.
    cache: HashMap<ShaderKey, CompiledShader>,
    /// GPU/driver profile for SPIR-V emission.
    profile: Profile,
}

fn translate_cfg_to_program(
    code: &[u64],
    code_base_offset: u32,
    stage: ShaderStage,
    cfg_blocks: &[control_flow::CfgBlock],
    sph: Option<&ProgramHeader>,
    env: Option<&dyn Environment>,
) -> Program {
    let mut structured = if std::env::var_os("RUZU_SHADER_FORCE_LINEAR_SYNTAX").is_some() {
        linear_structured_syntax(cfg_blocks)
    } else {
        structured_control_flow::structure_cfg_detailed(cfg_blocks)
    };
    remove_pre_end_if_merge_blocks(&mut structured);
    let mut program = Program::new(stage);
    program.syntax_list = structured.syntax;
    program.blocks = (0..structured.block_count).map(|_| Block::new()).collect();
    materialize_entry_prologue(&mut program);
    materialize_return_epilogues(&mut program);

    // Upstream `GenerateBlocks` assigns `Block::order` while traversing the
    // abstract syntax list, not from the block owner's allocation index.
    crate::frontend::translate_program::regenerate_block_order_from_syntax(&mut program);
    materialize_structured_actions(
        &mut program,
        &structured.actions,
        cfg_blocks,
        code,
        code_base_offset,
        sph,
        env,
    );
    rebuild_syntax_successors(&mut program);

    if !program.blocks.is_empty() {
        // Upstream computes `PostOrder(program.syntax_list.front())`. The ASL
        // root can be a synthetic block allocated after the original CFG
        // blocks, so assuming block 0 drops that root and any conditions it
        // defines from SSA construction.
        program.post_order_blocks = post_order_from_syntax_root(&program);
        remove_unreachable_blocks(&mut program);
    }

    program
}

/// Upstream `TranslatePass` inserts `Prologue` at `first_block.begin()` after
/// building the ASL. Rust allocates the program blocks after structuring, so
/// emit it before translating any Maxwell instructions into the entry block.
fn materialize_entry_prologue(program: &mut Program) {
    let Some(SyntaxNode::Block(entry_block)) = program.syntax_list.first() else {
        return;
    };
    Emitter::new(program, *entry_block).prologue();
}

/// Upstream `structured_control_flow.cpp` creates a dedicated return block and
/// immediately emits `IR::IREmitter{*return_block}.Epilogue()`. Ruzu builds the
/// syntax list before allocating the Rust `Program` blocks, so materialize the
/// same IR instruction after the block vector exists.
fn materialize_return_epilogues(program: &mut Program) {
    let return_blocks: Vec<u32> = program
        .syntax_list
        .windows(2)
        .filter_map(|nodes| match (&nodes[0], &nodes[1]) {
            (SyntaxNode::Block(block), SyntaxNode::Return) => Some(*block),
            _ => None,
        })
        .collect();

    for block in return_blocks {
        Emitter::new(program, block).epilogue();
    }
}

fn linear_structured_syntax(
    cfg_blocks: &[control_flow::CfgBlock],
) -> structured_control_flow::StructuredSyntax {
    if cfg_blocks.is_empty() {
        return structured_control_flow::StructuredSyntax {
            syntax: Vec::new(),
            actions: Vec::new(),
            block_count: 0,
        };
    }
    structured_control_flow::StructuredSyntax {
        syntax: vec![SyntaxNode::Block(0), SyntaxNode::Return],
        actions: cfg_blocks
            .iter()
            .enumerate()
            .map(|(cfg_block, _)| StructuredAction::TranslateCode {
                block: 0,
                cfg_block,
            })
            .collect(),
        block_count: 1,
    }
}

fn materialize_condition(
    program: &mut Program,
    block: u32,
    cond: control_flow::Condition,
) -> Value {
    Emitter::new(program, block).condition(cond)
}

fn materialize_structured_actions(
    program: &mut Program,
    actions: &[StructuredAction],
    cfg_blocks: &[control_flow::CfgBlock],
    code: &[u64],
    code_base_offset: u32,
    sph: Option<&ProgramHeader>,
    env: Option<&dyn Environment>,
) {
    for action in actions {
        match action {
            StructuredAction::TranslateCode { block, cfg_block } => {
                let Some(cfg_block) = cfg_blocks.get(*cfg_block) else {
                    continue;
                };
                let mut tv = if let Some(env) = env {
                    TranslatorVisitor::new_with_env(program, *block, env)
                } else {
                    TranslatorVisitor::new_with_sph(program, *block, sph.cloned())
                };
                for i in cfg_block.begin as usize..cfg_block.end as usize {
                    if i >= code.len() {
                        break;
                    }
                    if is_sched_control_word(i, code_base_offset) {
                        continue;
                    }
                    tv.translate_instruction(code[i]);
                }
            }
            StructuredAction::SetVariable { block, id, expr } => {
                let value = materialize_expr(program, *block, expr);
                append_inst(
                    program,
                    *block,
                    Inst::new(Opcode::SetGotoVariable, vec![Value::ImmU32(*id), value]),
                );
            }
            StructuredAction::SetIndirectBranchVariable {
                block,
                branch_reg,
                branch_offset,
            } => {
                let reg = append_inst(
                    program,
                    *block,
                    Inst::new(
                        Opcode::GetRegister,
                        vec![Value::Reg(super::ir::value::Reg(*branch_reg as u8))],
                    ),
                );
                let address = append_inst(
                    program,
                    *block,
                    Inst::new(
                        Opcode::IAdd32,
                        vec![reg, Value::ImmU32(*branch_offset as u32)],
                    ),
                );
                append_inst(
                    program,
                    *block,
                    Inst::new(Opcode::SetIndirectBranchVariable, vec![address]),
                );
            }
            StructuredAction::DemoteToHelperInvocation { block } => {
                Emitter::new(program, *block).demote_to_helper_invocation();
            }
            StructuredAction::Condition {
                syntax_index,
                block,
                expr,
            } => {
                let value = materialize_expr(program, *block, expr);
                let cond_ref = append_inst(
                    program,
                    *block,
                    Inst::new(Opcode::ConditionRef, vec![value]),
                );
                match &mut program.syntax_list[*syntax_index] {
                    SyntaxNode::If { cond, .. }
                    | SyntaxNode::Repeat { cond, .. }
                    | SyntaxNode::Break { cond, .. } => *cond = cond_ref,
                    _ => {}
                }
            }
        }
    }
}

fn remove_pre_end_if_merge_blocks(structured: &mut structured_control_flow::StructuredSyntax) {
    let mut index = 1usize;
    while index < structured.syntax.len() {
        let SyntaxNode::EndIf { merge } = structured.syntax[index] else {
            index += 1;
            continue;
        };
        if matches!(structured.syntax[index - 1], SyntaxNode::Block(block) if block == merge) {
            let removed_index = index - 1;
            structured.syntax.remove(removed_index);
            for action in &mut structured.actions {
                if let StructuredAction::Condition { syntax_index, .. } = action {
                    if *syntax_index > removed_index {
                        *syntax_index -= 1;
                    }
                }
            }
        } else {
            index += 1;
        }
    }
}

fn materialize_expr(program: &mut Program, block: u32, expr: &Expr) -> Value {
    match expr {
        Expr::Identity(cond) => materialize_condition(program, block, *cond),
        Expr::Not(expr) => {
            let value = materialize_expr(program, block, expr);
            append_inst(program, block, Inst::new(Opcode::LogicalNot, vec![value]))
        }
        Expr::Or(lhs, rhs) => {
            let lhs = materialize_expr(program, block, lhs);
            let rhs = materialize_expr(program, block, rhs);
            append_inst(program, block, Inst::new(Opcode::LogicalOr, vec![lhs, rhs]))
        }
        Expr::Variable(id) => append_inst(
            program,
            block,
            Inst::new(Opcode::GetGotoVariable, vec![Value::ImmU32(*id)]),
        ),
        Expr::IndirectBranchCond(location) => {
            let branch = append_inst(
                program,
                block,
                Inst::new(Opcode::GetIndirectBranchVariable, vec![]),
            );
            append_inst(
                program,
                block,
                Inst::new(Opcode::IEqual, vec![branch, Value::ImmU32(*location)]),
            )
        }
    }
}

fn add_syntax_edge(program: &mut Program, from: u32, to: u32) {
    if from as usize >= program.blocks.len() || to as usize >= program.blocks.len() {
        return;
    }
    if !program.block(from).imm_successors.contains(&to) {
        program.block_mut(from).add_successor(to);
    }
    if !program.block(to).imm_predecessors.contains(&from) {
        program.block_mut(to).add_predecessor(from);
    }
}

fn rebuild_syntax_successors(program: &mut Program) {
    for block in &mut program.blocks {
        block.imm_successors.clear();
        block.imm_predecessors.clear();
    }

    let mut current_block = None;
    let syntax = program.syntax_list.clone();
    for node in syntax {
        match node {
            SyntaxNode::Block(block) => {
                if let Some(current) = current_block {
                    if current != block {
                        add_syntax_edge(program, current, block);
                    }
                }
                current_block = Some(block);
            }
            SyntaxNode::If { body, merge, .. }
            | SyntaxNode::Break {
                merge, skip: body, ..
            } => {
                if let Some(current) = current_block {
                    add_syntax_edge(program, current, body);
                    add_syntax_edge(program, current, merge);
                }
                current_block = None;
            }
            SyntaxNode::Loop {
                body,
                continue_block: _,
                merge: _,
            } => {
                if let Some(current) = current_block {
                    add_syntax_edge(program, current, body);
                }
                current_block = None;
            }
            SyntaxNode::Repeat {
                loop_header, merge, ..
            } => {
                if let Some(current) = current_block {
                    add_syntax_edge(program, current, loop_header);
                    add_syntax_edge(program, current, merge);
                }
                current_block = None;
            }
            SyntaxNode::EndIf { merge } => {
                if let Some(current) = current_block {
                    add_syntax_edge(program, current, merge);
                }
                current_block = None;
            }
            SyntaxNode::Return | SyntaxNode::Unreachable => {
                current_block = None;
            }
        }
    }
}

fn post_order_from_syntax_root(program: &Program) -> Vec<u32> {
    let entry = match program.syntax_list.first() {
        Some(SyntaxNode::Block(block)) => *block,
        _ => panic!("First node in abstract syntax list root is not a block"),
    };
    post_order(&program.blocks, entry)
}

fn append_inst(program: &mut Program, block: u32, inst: Inst) -> Value {
    let inst_index = program.block_mut(block).append_inst(inst);
    Value::Inst(InstRef {
        block,
        inst: inst_index,
    })
}

fn is_sched_control_word(word_index: usize, code_base_offset: u32) -> bool {
    (code_base_offset as usize + word_index * std::mem::size_of::<u64>()) % 32 == 0
}

impl PipelineCache {
    /// Create a new pipeline cache with the given GPU profile.
    pub fn new(profile: Profile) -> Self {
        Self {
            cache: HashMap::new(),
            profile,
        }
    }

    /// Create a pipeline cache with default profile.
    pub fn with_default_profile() -> Self {
        Self::new(Profile::default())
    }

    /// Get or compile a shader from Maxwell binary code.
    ///
    /// `code` is a slice of Maxwell instructions (each instruction is 8 bytes / u64).
    /// `stage` is the shader stage (Vertex, Fragment, etc.).
    ///
    /// Returns the compiled shader with SPIR-V words and shader info.
    pub fn get_or_compile(
        &mut self,
        code: &[u64],
        stage: ShaderStage,
        runtime_info: &RuntimeInfo,
    ) -> &CompiledShader {
        let key = ShaderKey {
            code_hash: hash_code(code),
            runtime_hash: hash_runtime_info(runtime_info),
            stage,
        };

        if !self.cache.contains_key(&key) {
            let compiled = compile_shader(code, stage, &self.profile, runtime_info);
            self.cache.insert(key, compiled);
        }

        self.cache.get(&key).unwrap()
    }

    /// Check if a shader is already cached.
    pub fn contains(&self, code: &[u64], stage: ShaderStage) -> bool {
        let key = ShaderKey {
            code_hash: hash_code(code),
            runtime_hash: hash_runtime_info(&RuntimeInfo::default()),
            stage,
        };
        self.cache.contains_key(&key)
    }

    /// Number of cached shaders.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Clear all cached shaders.
    pub fn clear(&mut self) {
        self.cache.clear();
    }
}

/// Compile a Maxwell shader binary to SPIR-V.
///
/// This is the main entry point for the shader recompiler pipeline:
/// Maxwell binary → decode opcodes → build CFG → structured CF
/// → translate to IR → optimize → emit SPIR-V.
pub fn compile_shader(
    code: &[u64],
    stage: ShaderStage,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
) -> CompiledShader {
    log::debug!("Compiling {:?} shader ({} instructions)", stage, code.len());

    // Step 1: Build control flow graph from Maxwell instructions.
    let cfg_blocks = control_flow::build_cfg(code);
    log::trace!("  CFG: {} blocks", cfg_blocks.len());

    // Step 2/3: Convert flat CFG to structured control flow and translate
    // Maxwell instructions into matching IR blocks.
    let mut program = translate_cfg_to_program(code, 0, stage, &cfg_blocks, None, None);
    log::trace!("  Syntax nodes: {}", program.syntax_list.len());

    // Step 4: Run optimization passes.
    optimize_program_without_env(
        &mut program,
        &crate::host_translate_info::HostTranslateInfo::default(),
        None,
        None,
    );

    // Step 5: Emit SPIR-V.
    let spirv_words = backend::emit_spirv(&program, profile, runtime_info);
    log::debug!(
        "  SPIR-V: {} words, {} cbuf descriptors, {} tex descriptors",
        spirv_words.len(),
        program.info.constant_buffer_descriptors.len(),
        program.info.texture_descriptors.len(),
    );

    CompiledShader {
        spirv_words,
        info: program.info,
        stage,
    }
}

/// Translate a Maxwell shader binary into IR and run the currently ported
/// optimization driver.
///
/// This is the Rust counterpart used by `frontend::translate_program` until
/// the full upstream `TranslateProgram(env, cfg, host_info)` signature is
/// ported. It shares the same CFG, structured-control-flow, translation, and
/// host-info-aware pass sequence as the GLSL/Vulkan compile paths instead of
/// returning an empty placeholder program.
pub fn translate_program_with_host_info(
    code: &[u64],
    stage: ShaderStage,
    host_info: &crate::host_translate_info::HostTranslateInfo,
) -> Program {
    translate_and_optimize_with_host_info(code, stage, host_info)
}

/// Translate a Maxwell shader binary into IR using default host capabilities.
pub fn translate_program(code: &[u64], stage: ShaderStage) -> Program {
    translate_program_with_host_info(
        code,
        stage,
        &crate::host_translate_info::HostTranslateInfo::default(),
    )
}

/// Translate a Maxwell shader binary using the upstream-shaped environment
/// owner for stage metadata and environment-dependent optimization passes.
///
/// This is an incremental Rust counterpart of upstream
/// `TranslateProgram(inst_pool, block_pool, env, cfg, host_info)`: the CFG is
/// built through the upstream-owned environment path so instruction locations
/// are byte-addressed Maxwell `Location`s instead of slice-local indices.
pub fn translate_program_from_env_with_host_info(
    code: &[u64],
    base_offset: u32,
    env: &mut dyn Environment,
    host_info: &crate::host_translate_info::HostTranslateInfo,
) -> Program {
    let mut normalized_host_info = host_info.clone();
    normalized_host_info.apply_descriptor_limit_policy();
    let cfg_blocks = control_flow::build_cfg_from_env(env, base_offset, code.len());
    let sph = env.sph().clone();
    let mut program = translate_cfg_to_program(
        code,
        base_offset,
        env.shader_stage(),
        &cfg_blocks,
        Some(&sph),
        Some(&*env),
    );
    apply_environment_program_metadata(&mut program, env, &normalized_host_info);
    optimize_program_with_env(env, &mut program, &normalized_host_info, Some(&sph));
    collect_interpolation_info(&sph, &mut program);
    add_nvn_storage_buffers(&mut program);
    program
}

/// Translate a Maxwell shader binary using default host capabilities.
pub fn translate_program_from_env(
    code: &[u64],
    base_offset: u32,
    env: &mut dyn Environment,
) -> Program {
    translate_program_from_env_with_host_info(
        code,
        base_offset,
        env,
        &crate::host_translate_info::HostTranslateInfo::default(),
    )
}

/// Compile a Maxwell shader binary to GLSL source for the OpenGL backend.
///
/// Mirrors [`compile_shader`] but invokes the GLSL emitter instead of the
/// SPIR-V emitter, returning a [`CompiledGlslShader`] whose `source` field
/// can be fed directly into `glShaderSource` / `glCompileShader`.
pub fn compile_shader_glsl(
    code: &[u64],
    stage: ShaderStage,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
) -> CompiledGlslShader {
    log::debug!(
        "Compiling {:?} shader to GLSL ({} instructions)",
        stage,
        code.len()
    );

    let cfg_blocks = control_flow::build_cfg(code);
    let mut program = translate_cfg_to_program(code, 0, stage, &cfg_blocks, None, None);

    optimize_program_without_env(
        &mut program,
        &crate::host_translate_info::HostTranslateInfo::default(),
        None,
        None,
    );

    let mut bindings = backend::bindings::Bindings::default();
    convert_legacy_to_generic(&mut program, runtime_info);
    let source = backend::glsl::emit_glsl(profile, runtime_info, &mut program, &mut bindings);
    log::debug!("  GLSL: {} bytes", source.len());
    CompiledGlslShader {
        source,
        info: program.info,
        stage,
    }
}

/// Compile a Maxwell shader whose first word corresponds to an absolute
/// shader-program byte offset. This preserves upstream `Location` ownership
/// for sched-control skipping when the cached slice does not start at offset 0.
pub fn compile_shader_glsl_at_offset(
    code: &[u64],
    stage: ShaderStage,
    base_offset: u32,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
) -> CompiledGlslShader {
    let mut bindings = backend::bindings::Bindings::default();
    let host_info = host_info_from_profile(profile);
    emit_glsl_program_at_offset(
        code,
        stage,
        base_offset,
        profile,
        runtime_info,
        &mut bindings,
        None,
        None,
        &host_info,
    )
}

/// Same as [`compile_shader_glsl_at_offset`], but reuses the caller-owned
/// GLSL binding allocator across stages. Upstream `gl_shader_cache.cpp`
/// keeps one `Shader::Backend::Bindings binding` for the whole graphics
/// pipeline so vertex/fragment UBOs and textures receive distinct GL binding
/// points.
pub fn compile_shader_glsl_at_offset_with_bindings(
    code: &[u64],
    stage: ShaderStage,
    base_offset: u32,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    bindings: &mut backend::bindings::Bindings,
) -> CompiledGlslShader {
    let host_info = host_info_from_profile(profile);
    emit_glsl_program_at_offset(
        code,
        stage,
        base_offset,
        profile,
        runtime_info,
        bindings,
        None,
        None,
        &host_info,
    )
}

pub fn compile_shader_glsl_at_offset_with_bindings_and_host_info(
    code: &[u64],
    stage: ShaderStage,
    base_offset: u32,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    bindings: &mut backend::bindings::Bindings,
    host_info: &crate::host_translate_info::HostTranslateInfo,
) -> CompiledGlslShader {
    emit_glsl_program_at_offset(
        code,
        stage,
        base_offset,
        profile,
        runtime_info,
        bindings,
        None,
        None,
        host_info,
    )
}

/// Compile a Maxwell shader to GLSL through the upstream-shaped Environment
/// translation bridge.
pub fn compile_shader_glsl_from_env_with_bindings_and_host_info(
    code: &[u64],
    base_offset: u32,
    env: &mut dyn Environment,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    bindings: &mut backend::bindings::Bindings,
    host_info: &crate::host_translate_info::HostTranslateInfo,
) -> CompiledGlslShader {
    let stage = env.shader_stage();
    let mut program = translate_program_from_env_with_host_info(code, base_offset, env, host_info);
    convert_legacy_to_generic(&mut program, runtime_info);
    let source = backend::glsl::emit_glsl(profile, runtime_info, &mut program, bindings);
    CompiledGlslShader {
        source,
        info: program.info,
        stage,
    }
}

/// Compile a Maxwell shader to SPIR-V through the upstream-shaped Environment
/// translation bridge.
pub fn compile_shader_from_env_with_host_info(
    code: &[u64],
    base_offset: u32,
    env: &mut dyn Environment,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    host_info: &crate::host_translate_info::HostTranslateInfo,
) -> CompiledShader {
    let program =
        translate_shader_from_env_with_host_info(code, base_offset, env, runtime_info, host_info);
    let stage = program.stage;
    let spirv_words = backend::emit_spirv(&program, profile, runtime_info);
    CompiledShader {
        spirv_words,
        info: program.info,
        stage,
    }
}

pub fn compile_shader_from_env_with_bindings_and_host_info(
    code: &[u64],
    base_offset: u32,
    env: &mut dyn Environment,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    bindings: &mut backend::bindings::Bindings,
    host_info: &crate::host_translate_info::HostTranslateInfo,
) -> CompiledShader {
    let program =
        translate_shader_from_env_with_host_info(code, base_offset, env, runtime_info, host_info);
    let stage = program.stage;
    let spirv_words = backend::emit_spirv_with_bindings(&program, profile, runtime_info, bindings);
    CompiledShader {
        spirv_words,
        info: program.info,
        stage,
    }
}

/// Translate and normalize one environment-backed shader into the common IR.
///
/// Backend owners call this function before selecting SPIR-V, GLSL, or MSL
/// emission. Keeping `convert_legacy_to_generic` here guarantees that every
/// backend consumes the same normalized `Program`.
pub fn translate_shader_from_env_with_host_info(
    code: &[u64],
    base_offset: u32,
    env: &mut dyn Environment,
    runtime_info: &RuntimeInfo,
    host_info: &crate::host_translate_info::HostTranslateInfo,
) -> Program {
    let mut program = translate_program_from_env_with_host_info(code, base_offset, env, host_info);
    convert_legacy_to_generic(&mut program, runtime_info);
    program
}

/// OpenGL graphics path variant that mirrors upstream's
/// `TexturePass(env, program, host_info)` for currently ported bound
/// texture instructions.
pub fn compile_shader_glsl_at_offset_with_bindings_and_texture_bound(
    code: &[u64],
    stage: ShaderStage,
    base_offset: u32,
    texture_bound_buffer: u32,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    bindings: &mut backend::bindings::Bindings,
) -> CompiledGlslShader {
    let host_info = host_info_from_profile(profile);
    emit_glsl_program_at_offset(
        code,
        stage,
        base_offset,
        profile,
        runtime_info,
        bindings,
        Some(texture_bound_buffer),
        None,
        &host_info,
    )
}

pub fn compile_shader_glsl_at_offset_with_bindings_and_texture_bound_and_host_info(
    code: &[u64],
    stage: ShaderStage,
    base_offset: u32,
    texture_bound_buffer: u32,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    bindings: &mut backend::bindings::Bindings,
    host_info: &crate::host_translate_info::HostTranslateInfo,
) -> CompiledGlslShader {
    emit_glsl_program_at_offset(
        code,
        stage,
        base_offset,
        profile,
        runtime_info,
        bindings,
        Some(texture_bound_buffer),
        None,
        host_info,
    )
}

/// Same as [`compile_shader_glsl_at_offset_with_bindings_and_texture_bound`],
/// but preserves the upstream environment-owned SPH for fragment interpolation
/// and IPA perspective handling.
pub fn compile_shader_glsl_at_offset_with_bindings_and_texture_bound_and_sph(
    code: &[u64],
    stage: ShaderStage,
    base_offset: u32,
    texture_bound_buffer: u32,
    sph: &ProgramHeader,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    bindings: &mut backend::bindings::Bindings,
) -> CompiledGlslShader {
    let host_info = host_info_from_profile(profile);
    emit_glsl_program_at_offset(
        code,
        stage,
        base_offset,
        profile,
        runtime_info,
        bindings,
        Some(texture_bound_buffer),
        Some(sph),
        &host_info,
    )
}

pub fn compile_shader_glsl_at_offset_with_bindings_and_texture_bound_and_sph_and_host_info(
    code: &[u64],
    stage: ShaderStage,
    base_offset: u32,
    texture_bound_buffer: u32,
    sph: &ProgramHeader,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    bindings: &mut backend::bindings::Bindings,
    host_info: &crate::host_translate_info::HostTranslateInfo,
) -> CompiledGlslShader {
    emit_glsl_program_at_offset(
        code,
        stage,
        base_offset,
        profile,
        runtime_info,
        bindings,
        Some(texture_bound_buffer),
        Some(sph),
        host_info,
    )
}

fn emit_glsl_program_at_offset(
    code: &[u64],
    stage: ShaderStage,
    base_offset: u32,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    bindings: &mut backend::bindings::Bindings,
    texture_bound_buffer: Option<u32>,
    sph: Option<&ProgramHeader>,
    host_info: &crate::host_translate_info::HostTranslateInfo,
) -> CompiledGlslShader {
    log::debug!(
        "Compiling {:?} shader to GLSL ({} instructions, base_offset=0x{:X})",
        stage,
        code.len(),
        base_offset
    );
    let cfg_blocks = control_flow::build_cfg(code);
    let mut program = translate_cfg_to_program(code, base_offset, stage, &cfg_blocks, sph, None);

    optimize_program_without_env(&mut program, host_info, sph, texture_bound_buffer);

    convert_legacy_to_generic(&mut program, runtime_info);
    if let Some(sph) = sph {
        collect_interpolation_info(sph, &mut program);
    }
    add_nvn_storage_buffers(&mut program);
    let source = backend::glsl::emit_glsl(profile, runtime_info, &mut program, bindings);
    CompiledGlslShader {
        source,
        info: program.info,
        stage,
    }
}

fn host_info_from_profile(profile: &Profile) -> crate::host_translate_info::HostTranslateInfo {
    crate::host_translate_info::HostTranslateInfo {
        support_int64: profile.support_int64,
        min_ssbo_alignment: profile.min_ssbo_alignment,
        ..Default::default()
    }
}

/// Compile a Maxwell VertexA + VertexB pair to SPIR-V through the
/// upstream-shaped environment bridge, matching upstream
/// `TranslateProgram(env, cfg, host_info)` followed by
/// `MergeDualVertexPrograms`.
pub fn compile_dual_vertex_shader_from_env_with_bindings_and_host_info(
    vertex_a_code: &[u64],
    vertex_a_base_offset: u32,
    vertex_a_env: &mut dyn Environment,
    vertex_b_code: &[u64],
    vertex_b_base_offset: u32,
    vertex_b_env: &mut dyn Environment,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    bindings: &mut backend::bindings::Bindings,
    host_info: &crate::host_translate_info::HostTranslateInfo,
) -> CompiledShader {
    let program = translate_dual_vertex_shader_from_env_with_host_info(
        vertex_a_code,
        vertex_a_base_offset,
        vertex_a_env,
        vertex_b_code,
        vertex_b_base_offset,
        vertex_b_env,
        runtime_info,
        host_info,
    );
    let spirv_words = backend::emit_spirv_with_bindings(&program, profile, runtime_info, bindings);
    CompiledShader {
        spirv_words,
        info: program.info,
        stage: ShaderStage::VertexB,
    }
}

/// Translate, merge, and normalize a VertexA + VertexB pair into the common
/// IR before choosing a source backend.
pub fn translate_dual_vertex_shader_from_env_with_host_info(
    vertex_a_code: &[u64],
    vertex_a_base_offset: u32,
    vertex_a_env: &mut dyn Environment,
    vertex_b_code: &[u64],
    vertex_b_base_offset: u32,
    vertex_b_env: &mut dyn Environment,
    runtime_info: &RuntimeInfo,
    host_info: &crate::host_translate_info::HostTranslateInfo,
) -> Program {
    let mut vertex_a = translate_program_from_env_with_host_info(
        vertex_a_code,
        vertex_a_base_offset,
        vertex_a_env,
        host_info,
    );
    let mut vertex_b = translate_program_from_env_with_host_info(
        vertex_b_code,
        vertex_b_base_offset,
        vertex_b_env,
        host_info,
    );
    let mut program = merge_dual_vertex_programs(&mut vertex_a, &mut vertex_b, vertex_b_env);

    convert_legacy_to_generic(&mut program, runtime_info);
    program
}

/// Compile a Maxwell VertexA + VertexB pair to GLSL through the
/// upstream-shaped environment bridge.
pub fn compile_dual_vertex_shader_glsl_from_env_with_bindings_and_host_info(
    vertex_a_code: &[u64],
    vertex_a_base_offset: u32,
    vertex_a_env: &mut dyn Environment,
    vertex_b_code: &[u64],
    vertex_b_base_offset: u32,
    vertex_b_env: &mut dyn Environment,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    bindings: &mut backend::bindings::Bindings,
    host_info: &crate::host_translate_info::HostTranslateInfo,
) -> CompiledGlslShader {
    let mut vertex_a = translate_program_from_env_with_host_info(
        vertex_a_code,
        vertex_a_base_offset,
        vertex_a_env,
        host_info,
    );
    let mut vertex_b = translate_program_from_env_with_host_info(
        vertex_b_code,
        vertex_b_base_offset,
        vertex_b_env,
        host_info,
    );
    let mut program = merge_dual_vertex_programs(&mut vertex_a, &mut vertex_b, vertex_b_env);

    convert_legacy_to_generic(&mut program, runtime_info);
    let source = backend::glsl::emit_glsl(profile, runtime_info, &mut program, bindings);
    CompiledGlslShader {
        source,
        info: program.info,
        stage: ShaderStage::VertexB,
    }
}

/// Hash Maxwell instruction code for cache lookup.
fn hash_code(code: &[u64]) -> u64 {
    // FNV-1a hash
    let mut hash: u64 = 0xcbf29ce484222325;
    for &insn in code {
        let bytes = insn.to_le_bytes();
        for &byte in &bytes {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

fn hash_runtime_info(info: &RuntimeInfo) -> u64 {
    let mut hasher = DefaultHasher::new();
    info.generic_input_types.hash(&mut hasher);
    info.previous_stage_stores.mask.hash(&mut hasher);
    info.previous_stage_legacy_stores_mapping.hash(&mut hasher);
    info.convert_depth_mode.hash(&mut hasher);
    info.force_early_z.hash(&mut hasher);
    info.tess_primitive.hash(&mut hasher);
    info.tess_spacing.hash(&mut hasher);
    info.tess_clockwise.hash(&mut hasher);
    info.input_topology.hash(&mut hasher);
    match info.fixed_state_point_size {
        Some(value) => {
            true.hash(&mut hasher);
            value.to_bits().hash(&mut hasher);
        }
        None => false.hash(&mut hasher),
    }
    info.alpha_test_func.hash(&mut hasher);
    info.alpha_test_reference.to_bits().hash(&mut hasher);
    info.y_negate.hash(&mut hasher);
    info.glasm_use_storage_buffers.hash(&mut hasher);
    info.xfb_count.hash(&mut hasher);
    for varying in info.xfb_varyings.iter().take(info.xfb_count as usize) {
        varying.buffer.hash(&mut hasher);
        varying.stream.hash(&mut hasher);
        varying.stride.hash(&mut hasher);
        varying.offset.hash(&mut hasher);
        varying.components.hash(&mut hasher);
    }
    info.frag_color_types.hash(&mut hasher);
    info.dual_source_blend.hash(&mut hasher);
    hasher.finish()
}

fn translate_and_optimize_with_host_info(
    code: &[u64],
    stage: ShaderStage,
    host_info: &crate::host_translate_info::HostTranslateInfo,
) -> Program {
    let mut normalized_host_info = host_info.clone();
    normalized_host_info.apply_descriptor_limit_policy();
    let cfg_blocks = control_flow::build_cfg(code);
    let mut program = translate_cfg_to_program(code, 0, stage, &cfg_blocks, None, None);
    optimize_program_without_env(&mut program, &normalized_host_info, None, None);
    program
}

fn apply_environment_program_metadata(
    program: &mut Program,
    env: &dyn Environment,
    host_info: &crate::host_translate_info::HostTranslateInfo,
) {
    program.stage = env.shader_stage();
    program.local_memory_size = env.local_memory_size();
    match program.stage {
        ShaderStage::TessellationControl => {
            program.invocations = env.sph().threads_per_input_primitive();
        }
        ShaderStage::Geometry => {
            let sph = env.sph();
            program.output_topology = output_topology_from_sph(sph.output_topology());
            program.output_vertices = sph.max_output_vertices();
            program.invocations = sph.threads_per_input_primitive();
            program.is_geometry_passthrough = sph.geometry_passthrough();
            if program.is_geometry_passthrough {
                let mask = env.gp_passthrough_mask();
                for bit in 0..mask.len() * 32 {
                    program
                        .info
                        .passthrough
                        .set(bit, ((mask[bit / 32] >> (bit % 32)) & 1) == 0);
                }
                if !host_info.support_geometry_shader_passthrough {
                    program.output_vertices = output_vertices_for_topology(program.output_topology);
                    // Upstream lowers passthrough here. Ruzu keeps the current
                    // backend passthrough path until `EmitGeometryPassthrough`
                    // is ported.
                }
            }
        }
        ShaderStage::Compute => {
            program.workgroup_size = env.workgroup_size();
            program.shared_memory_size = env.shared_memory_size();
        }
        _ => {}
    }
}

fn output_topology_from_sph(topology: super::program_header::OutputTopology) -> OutputTopology {
    match topology {
        super::program_header::OutputTopology::PointList => OutputTopology::PointList,
        super::program_header::OutputTopology::LineStrip => OutputTopology::LineStrip,
        super::program_header::OutputTopology::TriangleStrip => OutputTopology::TriangleStrip,
    }
}

fn output_vertices_for_topology(topology: OutputTopology) -> u32 {
    match topology {
        OutputTopology::PointList => 1,
        OutputTopology::LineStrip => 2,
        OutputTopology::TriangleStrip => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Environment;
    use crate::frontend::control_flow::{CfgBlock, Condition, EndClass};
    use crate::ir::program::SyntaxNode;
    use crate::program_header::OutputTopology;
    use crate::shader_info::{ReplaceConstant, TexturePixelFormat, TextureType};

    #[test]
    fn test_hash_code_deterministic() {
        let code = vec![0x1234_5678_9abc_def0u64, 0xfedcba9876543210];
        let h1 = hash_code(&code);
        let h2 = hash_code(&code);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_hash_code_different_inputs() {
        let code1 = vec![0x0000_0000_0000_0001u64];
        let code2 = vec![0x0000_0000_0000_0002u64];
        assert_ne!(hash_code(&code1), hash_code(&code2));
    }

    #[test]
    fn test_pipeline_cache_empty() {
        let cache = PipelineCache::with_default_profile();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    #[should_panic]
    fn empty_shader_without_terminator_is_rejected() {
        let profile = Profile::default();
        let code: Vec<u64> = vec![];
        let runtime_info = RuntimeInfo::default();
        let _ = compile_shader(&code, ShaderStage::VertexB, &profile, &runtime_info);
    }

    #[test]
    fn test_compile_shader_glsl_emits_source() {
        let profile = Profile::default();
        let code: Vec<u64> = vec![];
        let runtime_info = RuntimeInfo::default();
        let compiled = compile_shader_glsl(&code, ShaderStage::VertexB, &profile, &runtime_info);
        assert!(
            !compiled.source.is_empty(),
            "GLSL emitter should produce a non-empty source string for an empty shader"
        );
        assert_eq!(compiled.stage, ShaderStage::VertexB);
    }

    #[test]
    fn test_compile_shader_glsl_uses_fragment_color_types() {
        let profile = Profile {
            need_declared_frag_colors: true,
            ..Profile::default()
        };
        let mut runtime_info = RuntimeInfo::default();
        runtime_info.frag_color_types[1] = crate::runtime_info::AttributeType::UnsignedInt;
        runtime_info.frag_color_types[2] = crate::runtime_info::AttributeType::SignedInt;

        let compiled = compile_shader_glsl(&[], ShaderStage::Fragment, &profile, &runtime_info);

        assert!(compiled
            .source
            .contains("layout(location=0)out vec4 frag_color0;"));
        assert!(compiled
            .source
            .contains("layout(location=1)out uvec4 frag_color1;"));
        assert!(compiled
            .source
            .contains("layout(location=2)out ivec4 frag_color2;"));
    }

    #[test]
    fn post_order_starts_at_synthetic_syntax_root() {
        let mut program = Program::new(ShaderStage::VertexB);
        for _ in 0..3 {
            program.add_block();
        }
        program.syntax_list = vec![
            SyntaxNode::Block(2),
            SyntaxNode::Block(0),
            SyntaxNode::Return,
        ];

        rebuild_syntax_successors(&mut program);

        assert_eq!(post_order_from_syntax_root(&program), vec![0, 2]);
        assert_eq!(post_order(&program.blocks, 0), vec![0]);
    }

    #[test]
    fn loop_cfg_routes_continue_through_header_not_body() {
        let mut program = Program::new(ShaderStage::VertexB);
        for _ in 0..4 {
            program.add_block();
        }
        program.syntax_list = vec![
            SyntaxNode::Block(0),
            SyntaxNode::Loop {
                body: 1,
                continue_block: 2,
                merge: 3,
            },
            SyntaxNode::Block(1),
            SyntaxNode::Block(2),
            SyntaxNode::Repeat {
                cond: Value::ImmU1(true),
                loop_header: 0,
                merge: 3,
            },
            SyntaxNode::Block(3),
        ];

        rebuild_syntax_successors(&mut program);

        assert_eq!(program.block(1).imm_predecessors, vec![0]);
        assert!(!program.block(1).imm_predecessors.contains(&2));
        assert!(program.block(0).imm_predecessors.contains(&2));
        assert!(program.block(3).imm_predecessors.contains(&2));
    }

    #[test]
    fn cfg_translation_creates_matching_ir_blocks_and_edges() {
        let cfg_blocks = vec![
            CfgBlock {
                begin: 0,
                end: 1,
                end_class: EndClass::Branch,
                branch_true: Some(1),
                branch_false: None,
                cond: Condition::always(),
                stack_depth: 0,
                branch_reg: 0,
                branch_offset: 0,
                indirect_branches: Vec::new(),
            },
            CfgBlock {
                begin: 1,
                end: 2,
                end_class: EndClass::Branch,
                branch_true: None,
                branch_false: None,
                cond: Condition::always(),
                stack_depth: 0,
                branch_reg: 0,
                branch_offset: 0,
                indirect_branches: Vec::new(),
            },
        ];

        let program = translate_cfg_to_program(
            &[0, 0x50B0_0000_0000_0000],
            0,
            ShaderStage::VertexB,
            cfg_blocks.as_slice(),
            None,
            None,
        );

        // Upstream TranslatePass keeps consecutive Code statements in the
        // current IR block; Flow CFG block identity is not IR block identity.
        assert_eq!(program.blocks.len(), 1);
        assert!(program.block(0).imm_predecessors.is_empty());
        assert!(program.block(0).imm_successors.is_empty());
        assert_eq!(program.post_order_blocks, vec![0]);
        assert!(matches!(
            program.syntax_list.as_slice(),
            [SyntaxNode::Block(0), SyntaxNode::Unreachable]
        ));
    }

    #[test]
    fn cfg_translation_materializes_return_epilogue() {
        let cfg_blocks = vec![CfgBlock {
            begin: 0,
            end: 1,
            end_class: EndClass::Exit,
            branch_true: None,
            branch_false: None,
            cond: Condition::always(),
            stack_depth: 0,
            branch_reg: 0,
            branch_offset: 0,
            indirect_branches: Vec::new(),
        }];

        let program =
            translate_cfg_to_program(&[0], 0, ShaderStage::VertexB, &cfg_blocks, None, None);
        let entry_block = match program.syntax_list.first() {
            Some(SyntaxNode::Block(block)) => *block,
            _ => panic!("translation must start with an entry block"),
        };
        let return_block = program
            .syntax_list
            .windows(2)
            .find_map(|nodes| match (&nodes[0], &nodes[1]) {
                (SyntaxNode::Block(block), SyntaxNode::Return) => Some(*block),
                _ => None,
            })
            .expect("return syntax must have a preceding block");

        assert_eq!(program.block(entry_block).front().opcode, Opcode::Prologue);
        assert_eq!(program.block(return_block).front().opcode, Opcode::Epilogue);
    }

    #[test]
    fn cfg_translation_materializes_kill_as_demote() {
        let cfg_blocks = vec![CfgBlock {
            begin: 0,
            end: 1,
            end_class: EndClass::Kill,
            branch_true: None,
            branch_false: None,
            cond: Condition::always(),
            stack_depth: 0,
            branch_reg: 0,
            branch_offset: 0,
            indirect_branches: Vec::new(),
        }];

        let program =
            translate_cfg_to_program(&[0], 0, ShaderStage::Fragment, &cfg_blocks, None, None);

        assert!(program.blocks.iter().any(|block| {
            block
                .instructions
                .iter()
                .flatten()
                .any(|inst| inst.opcode == Opcode::DemoteToHelperInvocation)
        }));
    }

    #[test]
    fn cfg_translation_materializes_conditional_branch_predicate() {
        use crate::ir::opcodes::Opcode;

        let cfg_blocks = vec![
            CfgBlock {
                begin: 0,
                end: 1,
                end_class: EndClass::Branch,
                branch_true: Some(2),
                branch_false: Some(1),
                cond: Condition::from_pred(crate::ir::condition::IrPred::P2, true),
                stack_depth: 0,
                branch_reg: 0,
                branch_offset: 0,
                indirect_branches: Vec::new(),
            },
            CfgBlock {
                begin: 1,
                end: 2,
                end_class: EndClass::Branch,
                branch_true: Some(2),
                branch_false: None,
                cond: Condition::always(),
                stack_depth: 0,
                branch_reg: 0,
                branch_offset: 0,
                indirect_branches: Vec::new(),
            },
            CfgBlock {
                begin: 2,
                end: 3,
                end_class: EndClass::Return,
                branch_true: None,
                branch_false: None,
                cond: Condition::always(),
                stack_depth: 0,
                branch_reg: 0,
                branch_offset: 0,
                indirect_branches: Vec::new(),
            },
        ];

        let program = translate_cfg_to_program(
            &[0, 0x50B0_0000_0000_0000, 0x50B0_0000_0000_0000],
            0,
            ShaderStage::VertexB,
            cfg_blocks.as_slice(),
            None,
            None,
        );

        let cond = program
            .syntax_list
            .iter()
            .find_map(|node| match node {
                SyntaxNode::If { cond, .. } => Some(*cond),
                _ => None,
            })
            .expect("conditional branch should produce an If syntax node");
        let Value::Inst(cond_ref) = cond else {
            panic!("If condition must be an IR value, got {cond:?}");
        };
        let cond_inst = program.block(cond_ref.block).inst(cond_ref.inst);
        assert_eq!(cond_inst.opcode, Opcode::ConditionRef);
        let condition_ref_count = program
            .blocks
            .iter()
            .flat_map(|block| block.indexed_iter())
            .filter(|(_, inst)| inst.opcode == Opcode::ConditionRef)
            .count();
        assert_eq!(
            condition_ref_count, 1,
            "upstream wraps a structured condition in exactly one ConditionRef"
        );
        let Value::Inst(not_ref) = cond_inst.args[0] else {
            panic!("negated predicate should feed ConditionRef through LogicalNot");
        };
        assert_eq!(
            program.block(not_ref.block).inst(not_ref.inst).opcode,
            Opcode::LogicalNot
        );
    }

    #[test]
    fn sched_control_skip_uses_the_absolute_maxwell_grid() {
        assert!(is_sched_control_word(0, 0));
        assert!(!is_sched_control_word(1, 0));
        assert!(!is_sched_control_word(2, 0));
        assert!(!is_sched_control_word(3, 0));
        assert!(is_sched_control_word(4, 0));

        // A code slice beginning at absolute offset 0x10 reaches the next
        // scheduling word at its relative word index 2, not at index 0.
        assert!(!is_sched_control_word(0, 0x10));
        assert!(!is_sched_control_word(1, 0x10));
        assert!(is_sched_control_word(2, 0x10));
        assert!(is_sched_control_word(6, 0x10));
    }

    const UNCONDITIONAL_EXIT: u64 = 0xE300_0000_0007_000F;

    struct DummyEnvironment {
        texture_pass_caches: crate::environment::TexturePassCaches,
        sph: ProgramHeader,
    }

    impl DummyEnvironment {
        fn compute() -> Self {
            let mut sph = ProgramHeader::default();
            sph.raw[3] = (OutputTopology::TriangleStrip as u32) << 24;
            Self {
                texture_pass_caches: Default::default(),
                sph,
            }
        }
    }

    impl Environment for DummyEnvironment {
        fn texture_pass_caches(&mut self) -> &mut crate::environment::TexturePassCaches {
            &mut self.texture_pass_caches
        }

        fn read_instruction(&mut self, address: u32) -> u64 {
            match address {
                // Location aligns past the sched word at byte offset zero.
                8 => UNCONDITIONAL_EXIT,
                _ => 0,
            }
        }

        fn read_cbuf_value(&mut self, _cbuf_index: u32, _cbuf_offset: u32) -> u32 {
            0
        }

        fn read_texture_type(&mut self, _raw_handle: u32) -> TextureType {
            TextureType::Color2D
        }

        fn read_texture_pixel_format(&mut self, _raw_handle: u32) -> TexturePixelFormat {
            TexturePixelFormat::A8B8G8R8Unorm
        }

        fn is_texture_pixel_format_integer(&mut self, _raw_handle: u32) -> bool {
            false
        }

        fn read_viewport_transform_state(&mut self) -> u32 {
            1
        }

        fn texture_bound_buffer(&self) -> u32 {
            7
        }

        fn local_memory_size(&self) -> u32 {
            0x240
        }

        fn shared_memory_size(&self) -> u32 {
            0x180
        }

        fn workgroup_size(&self) -> [u32; 3] {
            [8, 4, 2]
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

        fn shader_stage(&self) -> ShaderStage {
            ShaderStage::Compute
        }

        fn start_address(&self) -> u32 {
            0
        }

        fn is_proprietary_driver(&self) -> bool {
            false
        }
    }

    #[test]
    fn translate_program_from_env_uses_environment_metadata() {
        let mut env = DummyEnvironment::compute();
        let code = [0, UNCONDITIONAL_EXIT];

        let program = translate_program_from_env(&code, 0, &mut env);

        assert_eq!(program.stage, ShaderStage::Compute);
        assert_eq!(program.local_memory_size, 0x240);
        assert_eq!(program.shared_memory_size, 0x180);
        assert_eq!(program.workgroup_size, [8, 4, 2]);
        assert!(!program.blocks.is_empty());
    }

    #[test]
    fn runtime_translator_uses_compute_environment_local_memory_for_stl_bounds() {
        let env = DummyEnvironment::compute();
        assert_eq!(env.sph.local_memory_size(), 0);
        assert_eq!(env.local_memory_size(), 0x240);

        let mut program = Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        // STL.B32 R2, [RZ + 0x20]. The immediate is within the compute
        // environment allocation, but outside the zero-valued graphics SPH.
        let stl = 0xEF50_0000_0000_0000u64 | (4u64 << 48) | (0x20u64 << 20) | (255u64 << 8) | 2;
        {
            let mut visitor = TranslatorVisitor::new_with_env(&mut program, 0, &env);
            crate::frontend::translate::load_store_local_shared::stl(&mut visitor, stl);
        }

        assert!(program.blocks[0]
            .iter()
            .any(|inst| inst.opcode == Opcode::WriteLocal));
    }

    #[test]
    fn test_pipeline_cache_get_or_compile() {
        let mut cache = PipelineCache::with_default_profile();
        let code: Vec<u64> = vec![0x0000_0000_0000_0000]; // NOP-like instruction
        assert!(!cache.contains(&code, ShaderStage::Fragment));

        let runtime_info = RuntimeInfo::default();
        let _compiled = cache.get_or_compile(&code, ShaderStage::Fragment, &runtime_info);
        assert!(cache.contains(&code, ShaderStage::Fragment));
        assert_eq!(cache.len(), 1);

        // Second lookup should hit cache
        let _compiled2 = cache.get_or_compile(&code, ShaderStage::Fragment, &runtime_info);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn pipeline_cache_keys_runtime_info_that_affects_emission() {
        let mut cache = PipelineCache::with_default_profile();
        let code: Vec<u64> = vec![0x0000_0000_0000_0000];

        let default_runtime = RuntimeInfo::default();
        let mut converted_depth_runtime = RuntimeInfo::default();
        converted_depth_runtime.convert_depth_mode = true;

        let _ = cache.get_or_compile(&code, ShaderStage::VertexB, &default_runtime);
        assert_eq!(cache.len(), 1);

        let _ = cache.get_or_compile(&code, ShaderStage::VertexB, &converted_depth_runtime);
        assert_eq!(cache.len(), 2);
    }
}
#[test]
fn runtime_hash_includes_dual_source_blend_and_xfb_stream() {
    let base = RuntimeInfo::default();
    let mut dual_source = base.clone();
    dual_source.dual_source_blend = true;
    assert_ne!(hash_runtime_info(&base), hash_runtime_info(&dual_source));

    let varying = crate::runtime_info::TransformFeedbackVarying {
        components: 1,
        ..Default::default()
    };
    let mut stream_zero = base.clone();
    stream_zero.xfb_varyings[0] = varying;
    stream_zero.xfb_count = 1;
    let mut stream_one = stream_zero.clone();
    stream_one.xfb_varyings[0].stream = 1;
    assert_ne!(
        hash_runtime_info(&stream_zero),
        hash_runtime_info(&stream_one)
    );
}

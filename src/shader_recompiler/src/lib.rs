// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Maxwell shader recompiler: Maxwell GPU bytecode → IR → SPIR-V.
//!
//! This module implements a shader recompiler matching zuyu's architecture:
//!
//! ```text
//! Maxwell binary
//!   → CFG analysis (control_flow.rs)
//!   → Maxwell decoder (maxwell_opcodes.rs)
//!   → TranslatorVisitor (translate/*.rs) → IR::Program
//!   → Optimization passes (ir_opt/)
//!   → SPIR-V backend (backend/) → Vec<u32>
//!   → VkShaderModule → Vulkan Pipeline → GPU execution
//! ```
//!
//! # Public API
//!
//! - [`compile_shader`] — Compile a single Maxwell shader to SPIR-V.
//! - [`PipelineCache`] — Cache compiled shaders by Maxwell binary hash.
//! - [`Profile`] — GPU/driver capability profile for SPIR-V emission.
//!
//! # Modules
//!
//! - `ir` — Core Intermediate Representation (opcodes, values, instructions, blocks, program)
//! - `frontend` — Maxwell decoder, CFG builder, structured CF, translator
//! - `backend` — SPIR-V emission via rspirv
//! - `ir_opt` — Optimization passes (constant propagation, DCE, etc.)
//! - `pipeline_cache` — Shader compilation and caching

pub mod backend;
pub mod frontend;
pub mod ir;
pub mod ir_opt;
pub mod pipeline_cache;

// Root-level modules matching upstream shader_recompiler/ files
pub mod environment;
pub mod exception;
pub mod host_translate_info;
pub mod object_pool;
pub mod profile;
pub mod program_header;
pub mod runtime_info;
pub mod shader_info;
pub mod stage;
pub mod varying_state;

// Re-export public API
pub use ir::types::ShaderStage;
pub use pipeline_cache::{
    compile_dual_vertex_shader_from_env_with_bindings_and_host_info,
    compile_dual_vertex_shader_glsl_from_env_with_bindings_and_host_info, compile_shader,
    compile_shader_from_env_with_bindings_and_host_info, compile_shader_from_env_with_host_info,
    compile_shader_glsl, compile_shader_glsl_at_offset,
    compile_shader_glsl_at_offset_with_bindings,
    compile_shader_glsl_at_offset_with_bindings_and_host_info,
    compile_shader_glsl_at_offset_with_bindings_and_texture_bound,
    compile_shader_glsl_at_offset_with_bindings_and_texture_bound_and_host_info,
    compile_shader_glsl_at_offset_with_bindings_and_texture_bound_and_sph,
    compile_shader_glsl_at_offset_with_bindings_and_texture_bound_and_sph_and_host_info,
    compile_shader_glsl_from_env_with_bindings_and_host_info,
    translate_dual_vertex_shader_from_env_with_host_info, translate_shader_from_env_with_host_info,
    CompiledGlslShader, CompiledShader, PipelineCache, ShaderKey,
};
pub use profile::Profile;
pub use runtime_info::RuntimeInfo;

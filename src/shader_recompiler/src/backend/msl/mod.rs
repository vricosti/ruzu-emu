// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal Shading Language backend.
//!
//! Eden has no MSL backend. This module mirrors the ownership boundaries of
//! its textual GLSL backend while consuming the same backend-neutral IR used
//! by SPIR-V. It deliberately exposes an explicit Metal binding ABI so the
//! renderer never has to recover resource metadata from generated source.

pub mod emit_msl;
pub mod msl_emit_context;

use std::num::NonZeroU32;

use thiserror::Error;

use crate::stage::Stage;

/// Metal resource namespace consumed by a generated MSL entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MslResourceKind {
    UniformBuffer,
    StorageBuffer,
    StorageImage,
    SampledImage,
    SeparateImage,
    SeparateSampler,
}

/// Mapping from one guest descriptor to Metal's independent namespaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MslResourceBinding {
    pub descriptor_set: u32,
    pub binding: u32,
    pub kind: MslResourceKind,
    pub buffer_index: u32,
    pub texture_index: u32,
    pub sampler_index: u32,
    pub count: Option<NonZeroU32>,
}

/// Complete direct-binding ABI for one MSL entry point.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MslBindingLayout {
    pub resources: Vec<MslResourceBinding>,
    pub push_constant_buffer_index: Option<u32>,
    pub buffer_count: u32,
    pub texture_count: u32,
    pub sampler_count: u32,
}

/// Backend output before native Metal library compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MslShaderSource {
    pub source: String,
    pub stage: Stage,
}

/// MSL source plus the complete ABI required by its native entry point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MslShaderArtifact {
    pub source: MslShaderSource,
    pub bindings: MslBindingLayout,
    pub entry_point: String,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MslError {
    #[error("MSL emission does not accept an unmerged VertexA program")]
    UnmergedVertexA,
    #[error("MSL emission for {0} is not implemented")]
    UnsupportedStage(Stage),
    #[error("MSL emission does not implement program feature {0}")]
    UnsupportedProgramFeature(&'static str),
    #[error("MSL emission does not implement {opcode} at block {block} instruction {inst}")]
    UnsupportedOpcode {
        block: u32,
        inst: u32,
        opcode: crate::ir::opcodes::Opcode,
    },
}

pub use emit_msl::emit_msl;

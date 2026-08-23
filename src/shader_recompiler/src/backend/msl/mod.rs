// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal Shading Language backend.
//!
//! Eden has no MSL backend. This module mirrors the ownership boundaries of
//! its textual GLSL backend while consuming the same backend-neutral IR used
//! by SPIR-V. It deliberately exposes an explicit Metal binding ABI so the
//! renderer never has to recover resource metadata from generated source.

pub mod emit_msl;
mod emit_msl_atomic;
mod emit_msl_barriers;
mod emit_msl_bitwise_conversion;
mod emit_msl_composite;
mod emit_msl_context_get_set;
mod emit_msl_control_flow;
mod emit_msl_convert;
mod emit_msl_floating_point;
mod emit_msl_image;
mod emit_msl_image_atomic;
mod emit_msl_integer;
mod emit_msl_logical;
mod emit_msl_memory;
mod emit_msl_select;
mod emit_msl_shared_memory;
mod emit_msl_special;
mod emit_msl_warp;
pub mod msl_emit_context;

use std::num::NonZeroU32;

use thiserror::Error;

use crate::stage::Stage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MslVersion {
    pub major: u8,
    pub minor: u8,
}

impl MslVersion {
    pub const V2_3: Self = Self { major: 2, minor: 3 };
    pub const V2_4: Self = Self { major: 2, minor: 4 };
    pub const V3_0: Self = Self { major: 3, minor: 0 };
    pub const V3_1: Self = Self { major: 3, minor: 1 };
    pub const V3_2: Self = Self { major: 3, minor: 2 };
    pub const V4_0: Self = Self { major: 4, minor: 0 };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MslOptions {
    pub language_version: MslVersion,
    /// SIMD-group width assumed by subgroup lowering. The active
    /// SPIRV-Cross path fixes this to the Maxwell warp width as well.
    pub fixed_subgroup_size: u32,
    /// Whether the selected Metal device supports the texture LOD query
    /// methods. This is a device capability, independent of the MSL version.
    pub supports_query_texture_lod: bool,
    /// Whether the selected Metal device supports textures declared with
    /// `access::read_write`. This is negotiated from the device's read/write
    /// texture tier rather than inferred from the MSL language version.
    pub supports_read_write_textures: bool,
    /// Whether native texture atomics are available on the selected Metal
    /// device. The renderer derives this from both MSL version and GPU family.
    pub supports_texture_atomics: bool,
}

impl Default for MslOptions {
    fn default() -> Self {
        Self {
            language_version: MslVersion::V2_3,
            fixed_subgroup_size: 32,
            supports_query_texture_lod: false,
            supports_read_write_textures: false,
            supports_texture_atomics: false,
        }
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MslExecutionInfo {
    pub workgroup_size: Option<[u32; 3]>,
    pub fixed_subgroup_size: u32,
}

impl Default for MslExecutionInfo {
    fn default() -> Self {
        Self {
            workgroup_size: None,
            fixed_subgroup_size: 32,
        }
    }
}

/// MSL source plus the complete ABI required by its native entry point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MslShaderArtifact {
    pub source: MslShaderSource,
    pub bindings: MslBindingLayout,
    pub entry_point: String,
    pub language_version: MslVersion,
    pub execution: MslExecutionInfo,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MslError {
    #[error("MSL emission does not accept an unmerged VertexA program")]
    UnmergedVertexA,
    #[error("MSL emission for {0} is not implemented")]
    UnsupportedStage(Stage),
    #[error("MSL emission does not implement program feature {0}")]
    UnsupportedProgramFeature(&'static str),
    #[error("MSL emission does not implement IR type {0}")]
    UnsupportedType(crate::ir::types::Type),
    #[error(
        "MSL emission cannot consume {value} as argument {arg} at block {block} instruction {inst}"
    )]
    UnsupportedValue {
        block: u32,
        inst: u32,
        arg: u32,
        value: &'static str,
    },
    #[error("MSL emission requires an immediate {expected} for {opcode} argument {arg}")]
    ExpectedImmediate {
        opcode: crate::ir::opcodes::Opcode,
        arg: u32,
        expected: &'static str,
    },
    #[error("MSL emission does not implement shader attribute {0}")]
    UnsupportedAttribute(u32),
    #[error("MSL emission references undeclared constant buffer {0}")]
    MissingConstantBuffer(u32),
    #[error("MSL emission references undeclared storage buffer {0}")]
    MissingStorageBuffer(u32),
    #[error("MSL emission references undeclared sampled texture {0}")]
    MissingTexture(u32),
    #[error("MSL emission references undeclared storage image {0}")]
    MissingImage(u32),
    #[error("MSL emission does not implement {opcode} at block {block} instruction {inst}")]
    UnsupportedOpcode {
        block: u32,
        inst: u32,
        opcode: crate::ir::opcodes::Opcode,
    },
}

pub use emit_msl::emit_msl;
pub use emit_msl::emit_msl_with_bindings;
pub use emit_msl::emit_msl_with_options;
pub use emit_msl::emit_msl_with_options_and_bindings;

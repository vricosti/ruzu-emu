// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `shader_recompiler/environment.h`
//!
//! Abstract environment interface providing access to the shader binary
//! and GPU state needed during shader translation.

use super::ir::value::{InstRef, Value};
use super::program_header::ProgramHeader;
use super::shader_info::{ReplaceConstant, TexturePixelFormat, TextureType};
use super::stage::Stage;
use std::collections::HashMap;

/// Guest task/mesh state, kept separate from the host execution stage.
/// Bundles upstream MeshRegisters/MeshPrimitiveHeader with the task-stage test.
#[derive(Debug, Clone)]
pub struct MeshShaderEnvironment {
    pub is_task: bool,
    pub registers: [u32; 3],
    pub primitive_header: ProgramHeader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CbufWordKey {
    pub index: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HandleKey {
    pub index: u32,
    pub offset: u32,
    pub shift_left: u32,
    pub secondary_index: u32,
    pub secondary_offset: u32,
    pub secondary_shift_left: u32,
    pub count: u32,
    pub has_secondary: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ConstBufferAddr {
    pub index: u32,
    pub offset: u32,
    pub shift_left: u32,
    pub secondary_index: u32,
    pub secondary_offset: u32,
    pub secondary_shift_left: u32,
    pub dynamic_offset: Value,
    pub count: u32,
    pub has_secondary: bool,
}

impl ConstBufferAddr {
    pub fn bound(index: u32, offset: u32) -> Self {
        Self {
            index,
            offset,
            shift_left: 0,
            secondary_index: 0,
            secondary_offset: 0,
            secondary_shift_left: 0,
            dynamic_offset: Value::Void,
            count: 1,
            has_secondary: false,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct TexturePassCaches {
    pub cbuf_word_cache: HashMap<CbufWordKey, u32>,
    pub handle_cache: HashMap<HandleKey, u32>,
    pub track_cache: HashMap<InstRef, ConstBufferAddr>,
}

impl TexturePassCaches {
    pub fn clear(&mut self) {
        self.cbuf_word_cache.clear();
        self.handle_cache.clear();
        self.track_cache.clear();
    }
}

/// Abstract environment for shader recompilation.
///
/// Provides access to shader instructions, constant buffer values,
/// texture types, and other GPU state needed during translation.
pub trait Environment {
    /// None denotes an ordinary shader. Some explicitly selects the compute
    /// fallback and requires the task/mesh emission and dispatch path.
    fn mesh_shader(&self) -> Option<&MeshShaderEnvironment> {
        None
    }

    /// Per-environment caches reset and populated by `TexturePass`.
    fn texture_pass_caches(&mut self) -> &mut TexturePassCaches;

    /// Diagnostic discriminator for cache-file shader environments.
    fn is_file_environment(&self) -> bool {
        false
    }

    /// Read a 64-bit instruction at the given byte address.
    fn read_instruction(&mut self, address: u32) -> u64;

    /// Translate a native indirect-branch address into the lowered program.
    /// Upstream Environment::TranslateProgramAddress defaults to identity.
    fn translate_program_address(&self, address: u32) -> u32 {
        address
    }

    /// Read a 32-bit value from a constant buffer.
    fn read_cbuf_value(&mut self, cbuf_index: u32, cbuf_offset: u32) -> u32;

    /// Read the texture type for a raw texture handle.
    fn read_texture_type(&mut self, raw_handle: u32) -> TextureType;

    /// Read the texture pixel format for a raw texture handle.
    fn read_texture_pixel_format(&mut self, raw_handle: u32) -> TexturePixelFormat;

    /// Check if a texture pixel format is integer.
    fn is_texture_pixel_format_integer(&mut self, raw_handle: u32) -> bool;

    /// Read the viewport transform state register.
    fn read_viewport_transform_state(&mut self) -> u32;

    /// Get the texture bound buffer index.
    fn texture_bound_buffer(&self) -> u32;

    /// Get the local memory size.
    fn local_memory_size(&self) -> u32;

    /// Get the shared memory size.
    fn shared_memory_size(&self) -> u32;

    /// Get the workgroup size [x, y, z].
    fn workgroup_size(&self) -> [u32; 3];

    /// Whether the environment has HLE macro state.
    fn has_hle_macro_state(&self) -> bool;

    /// Get a replacement constant for a cbuf access (HLE macro support).
    fn get_replace_const_buffer(&mut self, bank: u32, offset: u32) -> Option<ReplaceConstant>;

    /// Dump shader for debugging.
    fn dump(&mut self, pipeline_hash: u64, shader_hash: u64);

    /// Get the shader program header.
    fn sph(&self) -> &ProgramHeader;

    /// Get the geometry passthrough mask.
    fn gp_passthrough_mask(&self) -> &[u32; 8];

    /// Get the shader stage.
    fn shader_stage(&self) -> Stage;

    /// Get the start address of the shader.
    fn start_address(&self) -> u32;

    /// Whether this is a proprietary driver.
    fn is_proprietary_driver(&self) -> bool;
}

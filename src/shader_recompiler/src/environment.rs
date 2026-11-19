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

/// Compilation dependency for GPU-selected texture/sampler descriptor tables.
/// Unlike a direct handle, a shader-side index needs the live pool bounds.
/// This extends Eden's environment; it is not inferred from sampled handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TexturePoolState {
    pub texture_limit: u32,
    pub sampler_limit: u32,
    pub linked_sampler: bool,
}

impl TexturePoolState {
    /// Pool registers contain inclusive limits. Reject overflow, do not wrap
    /// an unrepresentable array length to zero or silently clamp the pool.
    pub fn descriptor_counts(self) -> Option<(u32, u32)> {
        let textures = self.texture_limit.checked_add(1)?;
        let samplers = if self.linked_sampler {
            textures
        } else {
            self.sampler_limit.checked_add(1)?
        };
        Some((textures, samplers))
    }
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
    pub handle_program: u32,
}

impl ConstBufferAddr {
    pub fn bound(index: u32, offset: u32) -> Self {
        Self {
            handle_program: u32::MAX,
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
    /// Native raw pointers need mapping validation, unlike legacy NVN descriptors.
    /// Matches Lichii Environment::IsCbufStorageBufferPointer.
    fn is_cbuf_storage_buffer_pointer(&mut self, _index: u32, _offset: u32) -> bool {
        true
    }

    fn is_cbuf_indirect_storage_buffer_pointer(&mut self, _index: u32, _offset: u32, _indirect: u32) -> bool {
        true
    }

    /// Capture a table-size dependency only when translation needs dynamic
    /// resource selection. Old cache entries cannot supply this information.
    fn texture_pool_state(&mut self) -> Option<TexturePoolState> {
        None
    }

    /// Fritchii Environment::HasNativeInstructions. Only native lowering may
    /// emit the reserved MOV form that addresses shadow register banks.
    fn has_native_instructions(&self) -> bool {
        false
    }

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

#[cfg(test)]
mod tests {
    use super::TexturePoolState;

    #[test]
    fn texture_pool_counts_preserve_inclusive_limits_and_linked_sampler_mode() {
        let mut state = TexturePoolState {
            texture_limit: 7,
            sampler_limit: 2,
            linked_sampler: false,
        };
        assert_eq!(state.descriptor_counts(), Some((8, 3)));
        state.linked_sampler = true;
        assert_eq!(state.descriptor_counts(), Some((8, 8)));
        state.sampler_limit = u32::MAX;
        assert_eq!(state.descriptor_counts(), Some((8, 8)));
        state.linked_sampler = false;
        assert_eq!(state.descriptor_counts(), None);
        state.texture_limit = u32::MAX;
        state.linked_sampler = true;
        assert_eq!(state.descriptor_counts(), None);
        state.texture_limit = 0;
        assert_eq!(state.descriptor_counts(), Some((1, 1)));
    }
}

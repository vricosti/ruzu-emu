// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! MSL source-emission context.
//!
//! The context owns native MSL source construction and the mapping from the
//! common IR's stable `InstRef` values to MSL SSA locals. It does not own or
//! duplicate Maxwell translation passes.

use std::collections::HashMap;

use crate::backend::bindings::Bindings;
use crate::ir::instruction::Inst;
use crate::ir::types::Type;
use crate::ir::value::{InstRef, Value};
use crate::profile::Profile;
use crate::shader_info::{ImageDescriptor, TextureDescriptor, TextureType};
use crate::stage::Stage;

use super::{
    MslBindingLayout, MslError, MslExecutionInfo, MslOptions, MslResourceBinding, MslResourceKind,
    MslShaderArtifact, MslShaderSource, MslVersion,
};

pub struct MslEmitContext {
    stage: Stage,
    source: String,
    definitions: HashMap<InstRef, String>,
    constant_buffers: HashMap<u32, String>,
    storage_buffers: HashMap<u32, String>,
    textures: Vec<MslTextureDefinition>,
    images: Vec<MslImageDefinition>,
    bindings: MslBindingLayout,
    returns_output: bool,
    uses_no_contraction_add: bool,
    uses_no_contraction_mul: bool,
    uses_no_contraction_fma: bool,
    uses_storage_subword_cas: bool,
    uses_shared_subword_cas: bool,
    uses_atomic_inc_dec_cas: bool,
    uses_texture_cast: bool,
    tracks_helper_invocation: bool,
    language_version: MslVersion,
    supports_query_texture_lod: bool,
    supports_texture_atomics: bool,
    supports_typeless_image_loads: bool,
    need_gather_subpixel_offset: bool,
    execution: MslExecutionInfo,
    has_broken_robust: bool,
}

#[derive(Debug, Clone)]
struct MslTextureDefinition {
    texture_name: String,
    sampler_name: String,
    texture_type: TextureType,
    count: u32,
    is_depth: bool,
    is_integer: bool,
    is_multisample: bool,
}

#[derive(Debug, Clone)]
struct MslImageDefinition {
    image_name: String,
    texture_type: TextureType,
    count: u32,
    is_integer: bool,
}

pub(super) struct MslTextureExpressions {
    pub texture: String,
    pub sampler: String,
    pub texture_type: TextureType,
    pub is_depth: bool,
    pub is_integer: bool,
    pub is_multisample: bool,
}

pub(super) struct MslImageExpressions {
    pub image: String,
    pub texture_type: TextureType,
    pub is_integer: bool,
}

impl MslEmitContext {
    pub fn new(
        program: &crate::ir::Program,
        profile: &Profile,
        options: &MslOptions,
        binding_counters: &mut Bindings,
    ) -> Result<Self, MslError> {
        let stage = program.stage;
        match stage {
            Stage::VertexA => return Err(MslError::UnmergedVertexA),
            Stage::VertexB | Stage::Fragment | Stage::Compute => {}
            Stage::TessellationControl | Stage::TessellationEval | Stage::Geometry => {
                return Err(MslError::UnsupportedStage(stage))
            }
        }

        let mut bindings = MslBindingLayout::default();
        let mut constant_buffers = HashMap::new();
        let mut storage_buffers = HashMap::new();
        let mut textures = Vec::new();
        let mut images = Vec::new();
        let mut parameters = Vec::new();
        let binding_counter = if profile.unified_descriptor_binding {
            &mut binding_counters.unified
        } else {
            &mut binding_counters.uniform_buffer
        };
        for descriptor in &program.info.constant_buffer_descriptors {
            if descriptor.count != 1 {
                return Err(MslError::UnsupportedProgramFeature(
                    "constant buffer descriptor indexing",
                ));
            }
            let descriptor_binding = *binding_counter;
            *binding_counter += descriptor.count;
            let buffer_index = bindings.buffer_count;
            bindings.buffer_count += 1;
            bindings.resources.push(MslResourceBinding {
                descriptor_set: 0,
                binding: descriptor_binding,
                kind: MslResourceKind::UniformBuffer,
                buffer_index,
                texture_index: 0,
                sampler_index: 0,
                count: None,
            });
            let name = format!("c{}", descriptor.index);
            parameters.push(format!("constant uint4* {name} [[buffer({buffer_index})]]"));
            constant_buffers.insert(descriptor.index, name);
        }
        let binding_counter = if profile.unified_descriptor_binding {
            &mut binding_counters.unified
        } else {
            &mut binding_counters.storage_buffer
        };
        let mut storage_index = 0u32;
        for descriptor in &program.info.storage_buffers_descriptors {
            let descriptor_binding = *binding_counter;
            *binding_counter += descriptor.count;
            let buffer_index = bindings.buffer_count;
            bindings.buffer_count += 1;
            bindings.resources.push(MslResourceBinding {
                descriptor_set: 0,
                binding: descriptor_binding,
                kind: MslResourceKind::StorageBuffer,
                buffer_index,
                texture_index: 0,
                sampler_index: 0,
                count: None,
            });
            let name = format!("ssbo{storage_index}");
            parameters.push(format!("device uint* {name} [[buffer({buffer_index})]]"));
            for alias in 0..descriptor.count {
                storage_buffers.insert(storage_index + alias, name.clone());
            }
            storage_index += descriptor.count;
        }
        let binding_counter = if profile.unified_descriptor_binding {
            &mut binding_counters.unified
        } else {
            &mut binding_counters.texture
        };
        for (descriptor_index, descriptor) in program.info.texture_descriptors.iter().enumerate() {
            let definition = Self::define_texture(
                descriptor_index as u32,
                descriptor,
                *binding_counter,
                &mut bindings,
                &mut parameters,
            )?;
            textures.push(definition);
            *binding_counter += 1;
        }
        let binding_counter = if profile.unified_descriptor_binding {
            &mut binding_counters.unified
        } else {
            &mut binding_counters.image
        };
        for (descriptor_index, descriptor) in program.info.image_descriptors.iter().enumerate() {
            let definition = Self::define_image(
                descriptor_index as u32,
                descriptor,
                *binding_counter,
                options.supports_read_write_textures,
                &mut bindings,
                &mut parameters,
            )?;
            images.push(definition);
            *binding_counter += 1;
        }
        if program.info.uses_workgroup_id {
            parameters.push("uint3 workgroup_id [[threadgroup_position_in_grid]]".to_owned());
        }
        if program.info.uses_local_invocation_id {
            parameters
                .push("uint3 local_invocation_id [[thread_position_in_threadgroup]]".to_owned());
        }
        if program.info.uses_sample_id {
            parameters.push("uint sample_id [[sample_id]]".to_owned());
        }
        let parameters = parameters.join(", ");
        let mut source = String::new();
        let returns_output = match stage {
            Stage::VertexB => {
                source.push_str(concat!(
                    "struct MslVertexOut {\n",
                    "    float4 position [[position]];\n",
                    "};\n\n",
                ));
                source.push_str(&format!("vertex MslVertexOut main0({parameters}) {{\n"));
                source.push_str(concat!(
                    "    MslVertexOut output = {};\n",
                    "    output.position = float4(0.0f);\n",
                ));
                true
            }
            Stage::Fragment if program.info.stores_frag_color.iter().any(|store| *store) => {
                source.push_str("struct MslFragmentOut {\n");
                for (index, stored) in program.info.stores_frag_color.iter().enumerate() {
                    if *stored {
                        source.push_str(&format!("    float4 color{index} [[color({index})]];\n"));
                    }
                }
                source.push_str(&format!(
                    "}};\n\nfragment MslFragmentOut main0({parameters}) {{\n"
                ));
                source.push_str("    MslFragmentOut output = {};\n");
                true
            }
            Stage::Fragment => {
                source.push_str(&format!("fragment void main0({parameters}) {{\n"));
                false
            }
            Stage::Compute => {
                source.push_str(&format!("kernel void main0({parameters}) {{\n"));
                if program.shared_memory_size != 0 {
                    let num_words = program.shared_memory_size.div_ceil(4);
                    source.push_str(&format!("    threadgroup uint smem[{num_words}];\n"));
                }
                false
            }
            _ => unreachable!("stage was validated above"),
        };
        if program.local_memory_size != 0 {
            let num_words = program.local_memory_size.div_ceil(4);
            source.push_str(&format!("    thread uint lmem[{num_words}];\n"));
        }
        let tracks_helper_invocation =
            program.info.uses_is_helper_invocation || program.info.uses_demote_to_helper_invocation;
        if tracks_helper_invocation {
            source.push_str("    bool helper_invocation = simd_is_helper_thread();\n");
        }

        Ok(Self {
            stage,
            source,
            definitions: HashMap::new(),
            constant_buffers,
            storage_buffers,
            textures,
            images,
            bindings,
            returns_output,
            uses_no_contraction_add: false,
            uses_no_contraction_mul: false,
            uses_no_contraction_fma: false,
            uses_storage_subword_cas: false,
            uses_shared_subword_cas: false,
            uses_atomic_inc_dec_cas: false,
            uses_texture_cast: false,
            tracks_helper_invocation,
            language_version: options.language_version,
            supports_query_texture_lod: options.supports_query_texture_lod,
            supports_texture_atomics: options.supports_texture_atomics,
            supports_typeless_image_loads: profile.support_typeless_image_loads,
            need_gather_subpixel_offset: profile.need_gather_subpixel_offset,
            execution: MslExecutionInfo {
                workgroup_size: (stage == Stage::Compute).then_some(program.workgroup_size),
            },
            has_broken_robust: profile.has_broken_robust,
        })
    }

    pub(crate) fn type_name(ty: Type) -> Result<&'static str, MslError> {
        match ty {
            Type::U1 => Ok("bool"),
            Type::U32 => Ok("uint"),
            Type::U64 => Ok("ulong"),
            Type::F16 => Ok("half"),
            Type::F32 => Ok("float"),
            Type::U32x2 => Ok("uint2"),
            Type::U32x3 => Ok("uint3"),
            Type::U32x4 => Ok("uint4"),
            Type::F16x2 => Ok("half2"),
            Type::F16x3 => Ok("half3"),
            Type::F16x4 => Ok("half4"),
            Type::F32x2 => Ok("float2"),
            Type::F32x3 => Ok("float3"),
            Type::F32x4 => Ok("float4"),
            _ => Err(MslError::UnsupportedType(ty)),
        }
    }

    fn define_texture(
        descriptor_index: u32,
        descriptor: &TextureDescriptor,
        descriptor_binding: u32,
        bindings: &mut MslBindingLayout,
        parameters: &mut Vec<String>,
    ) -> Result<MslTextureDefinition, MslError> {
        if descriptor.texture_type == TextureType::Buffer {
            return Err(MslError::UnsupportedProgramFeature(
                "texture buffer in sampled texture descriptors",
            ));
        }
        if descriptor.count == 0 {
            return Err(MslError::UnsupportedProgramFeature(
                "zero-sized texture descriptor array",
            ));
        }
        if descriptor.is_depth && descriptor.is_integer {
            return Err(MslError::UnsupportedProgramFeature(
                "integer depth texture descriptor",
            ));
        }

        let texture_index = bindings.texture_count;
        let sampler_index = bindings.sampler_count;
        bindings.texture_count += descriptor.count;
        bindings.sampler_count += descriptor.count;
        bindings.resources.push(MslResourceBinding {
            descriptor_set: 0,
            binding: descriptor_binding,
            kind: MslResourceKind::SampledImage,
            buffer_index: 0,
            texture_index,
            sampler_index,
            count: (descriptor.count > 1)
                .then(|| std::num::NonZeroU32::new(descriptor.count).unwrap()),
        });

        let texture_name = format!("tex{descriptor_index}");
        let sampler_name = format!("samp{descriptor_index}");
        let texture_type = if descriptor.is_multisample {
            let texture_class = match (descriptor.texture_type, descriptor.is_depth) {
                (TextureType::Color2D | TextureType::Color2DRect, false) => "texture2d_ms",
                (TextureType::ColorArray2D, false) => "texture2d_ms_array",
                (TextureType::Color2D | TextureType::Color2DRect, true) => "depth2d_ms",
                (TextureType::ColorArray2D, true) => "depth2d_ms_array",
                _ => {
                    return Err(MslError::UnsupportedProgramFeature(
                        "multisample texture dimension unsupported by Metal",
                    ));
                }
            };
            let component = if descriptor.is_integer {
                "uint"
            } else {
                "float"
            };
            format!("{texture_class}<{component}>")
        } else if descriptor.is_depth {
            let texture_class = match descriptor.texture_type {
                TextureType::Color2D | TextureType::Color2DRect => "depth2d",
                TextureType::ColorArray2D => "depth2d_array",
                TextureType::ColorCube => "depthcube",
                TextureType::ColorArrayCube => "depthcube_array",
                _ => {
                    return Err(MslError::UnsupportedProgramFeature(
                        "depth texture dimension unsupported by Metal",
                    ));
                }
            };
            format!("{texture_class}<float>")
        } else {
            let component = if descriptor.is_integer {
                "uint"
            } else {
                "float"
            };
            let texture_class = match descriptor.texture_type {
                TextureType::Color1D => "texture1d",
                TextureType::ColorArray1D => "texture1d_array",
                TextureType::Color2D | TextureType::Color2DRect => "texture2d",
                TextureType::ColorArray2D => "texture2d_array",
                TextureType::Color3D => "texture3d",
                TextureType::ColorCube => "texturecube",
                TextureType::ColorArrayCube => "texturecube_array",
                TextureType::Buffer => unreachable!("texture buffers were rejected above"),
            };
            format!("{texture_class}<{component}>")
        };
        if descriptor.count > 1 {
            parameters.push(format!(
                "array<{texture_type}, {}> {texture_name} [[texture({texture_index})]]",
                descriptor.count
            ));
            parameters.push(format!(
                "array<sampler, {}> {sampler_name} [[sampler({sampler_index})]]",
                descriptor.count
            ));
        } else {
            parameters.push(format!(
                "{texture_type} {texture_name} [[texture({texture_index})]]"
            ));
            parameters.push(format!(
                "sampler {sampler_name} [[sampler({sampler_index})]]"
            ));
        }
        Ok(MslTextureDefinition {
            texture_name,
            sampler_name,
            texture_type: descriptor.texture_type,
            count: descriptor.count,
            is_depth: descriptor.is_depth,
            is_integer: descriptor.is_integer,
            is_multisample: descriptor.is_multisample,
        })
    }

    fn define_image(
        descriptor_index: u32,
        descriptor: &ImageDescriptor,
        descriptor_binding: u32,
        supports_read_write_textures: bool,
        bindings: &mut MslBindingLayout,
        parameters: &mut Vec<String>,
    ) -> Result<MslImageDefinition, MslError> {
        if descriptor.count == 0 {
            return Err(MslError::UnsupportedProgramFeature(
                "zero-sized storage image descriptor array",
            ));
        }
        let access = match (descriptor.is_read, descriptor.is_written) {
            (true, false) => "read",
            (false, true) => "write",
            (true, true) if supports_read_write_textures => "read_write",
            (true, true) => {
                return Err(MslError::UnsupportedProgramFeature(
                    "read/write storage image on this Metal device",
                ));
            }
            (false, false) => {
                return Err(MslError::UnsupportedProgramFeature(
                    "storage image with no declared access",
                ));
            }
        };
        let texture_class = match descriptor.texture_type {
            TextureType::Color1D => "texture1d",
            TextureType::ColorArray1D => "texture1d_array",
            TextureType::Color2D => "texture2d",
            TextureType::ColorArray2D => "texture2d_array",
            TextureType::Color3D => "texture3d",
            TextureType::Buffer => {
                return Err(MslError::UnsupportedProgramFeature(
                    "image buffer in storage image descriptors",
                ));
            }
            TextureType::ColorCube | TextureType::ColorArrayCube | TextureType::Color2DRect => {
                return Err(MslError::UnsupportedProgramFeature(
                    "invalid storage image texture type",
                ));
            }
        };
        let component = if descriptor.is_integer {
            "uint"
        } else {
            "float"
        };
        let image_type = format!("{texture_class}<{component}, access::{access}>");
        let texture_index = bindings.texture_count;
        bindings.texture_count += descriptor.count;
        bindings.resources.push(MslResourceBinding {
            descriptor_set: 0,
            binding: descriptor_binding,
            kind: MslResourceKind::StorageImage,
            buffer_index: 0,
            texture_index,
            sampler_index: 0,
            count: (descriptor.count > 1)
                .then(|| std::num::NonZeroU32::new(descriptor.count).unwrap()),
        });

        let image_name = format!("img{descriptor_index}");
        if descriptor.count > 1 {
            parameters.push(format!(
                "array<{image_type}, {}> {image_name} [[texture({texture_index})]]",
                descriptor.count
            ));
        } else {
            parameters.push(format!(
                "{image_type} {image_name} [[texture({texture_index})]]"
            ));
        }
        Ok(MslImageDefinition {
            image_name,
            texture_type: descriptor.texture_type,
            count: descriptor.count,
            is_integer: descriptor.is_integer,
        })
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    pub fn supports_query_texture_lod(&self) -> bool {
        self.supports_query_texture_lod
    }

    pub fn need_gather_subpixel_offset(&self) -> bool {
        self.need_gather_subpixel_offset
    }

    pub fn supports_typeless_image_loads(&self) -> bool {
        self.supports_typeless_image_loads
    }

    pub fn supports_texture_atomics(&self) -> bool {
        self.language_version >= MslVersion::V3_1 && self.supports_texture_atomics
    }

    pub fn require_texture_cast(&mut self) {
        self.uses_texture_cast = true;
    }

    pub fn helper_invocation_expression(&self) -> &'static str {
        if self.tracks_helper_invocation {
            "helper_invocation"
        } else {
            "simd_is_helper_thread()"
        }
    }

    pub fn validate_texture(
        &self,
        info: crate::ir::types::TextureInstInfo,
    ) -> Result<(), MslError> {
        let definition = self
            .textures
            .get(info.descriptor_index as usize)
            .ok_or(MslError::MissingTexture(info.descriptor_index.into()))?;
        let instruction_type = TextureType::from_u8(info.texture_type);
        let matches = definition.texture_type == instruction_type
            || (definition.texture_type == TextureType::Color2DRect
                && instruction_type == TextureType::Color2D);
        if !matches {
            return Err(MslError::UnsupportedProgramFeature(
                "texture instruction/descriptor type mismatch",
            ));
        }
        Ok(())
    }

    pub(super) fn texture_expressions(
        &self,
        info: crate::ir::types::TextureInstInfo,
        index: &Value,
        inst_ref: InstRef,
    ) -> Result<MslTextureExpressions, MslError> {
        let definition = self
            .textures
            .get(info.descriptor_index as usize)
            .ok_or(MslError::MissingTexture(info.descriptor_index.into()))?;
        if definition.count == 1 {
            return Ok(MslTextureExpressions {
                texture: definition.texture_name.clone(),
                sampler: definition.sampler_name.clone(),
                texture_type: definition.texture_type,
                is_depth: definition.is_depth,
                is_integer: definition.is_integer,
                is_multisample: definition.is_multisample,
            });
        }
        let index = self.value_expression(index, inst_ref, 0)?;
        Ok(MslTextureExpressions {
            texture: format!("{}[{index}]", definition.texture_name),
            sampler: format!("{}[{index}]", definition.sampler_name),
            texture_type: definition.texture_type,
            is_depth: definition.is_depth,
            is_integer: definition.is_integer,
            is_multisample: definition.is_multisample,
        })
    }

    pub(super) fn image_expressions(
        &self,
        info: crate::ir::types::TextureInstInfo,
        index: &Value,
        inst_ref: InstRef,
    ) -> Result<MslImageExpressions, MslError> {
        let definition = self
            .images
            .get(info.descriptor_index as usize)
            .ok_or(MslError::MissingImage(info.descriptor_index.into()))?;
        let instruction_type = TextureType::from_u8(info.texture_type);
        if definition.texture_type != instruction_type {
            return Err(MslError::UnsupportedProgramFeature(
                "storage image instruction/descriptor type mismatch",
            ));
        }
        let image = if definition.count == 1 {
            definition.image_name.clone()
        } else {
            let index = self.value_expression(index, inst_ref, 0)?;
            format!("{}[{index}]", definition.image_name)
        };
        Ok(MslImageExpressions {
            image,
            texture_type: definition.texture_type,
            is_integer: definition.is_integer,
        })
    }

    pub fn constant_buffer_element_expression(
        &self,
        inst_ref: InstRef,
        binding: u32,
        offset: &Value,
        element_offset: u32,
    ) -> Result<String, MslError> {
        let name = self
            .constant_buffers
            .get(&binding)
            .ok_or(MslError::MissingConstantBuffer(binding))?;
        let offset_expression = self.value_expression(offset, inst_ref, 1)?;
        let vector_index = match offset {
            Value::ImmU32(offset) => format!("{}u", offset / 16),
            _ => format!("(({offset_expression}) >> 4u)"),
        };
        let vector = if self.has_broken_robust && !matches!(offset, Value::ImmU32(_)) {
            format!("(({vector_index}) <= 0x0000FFFFu ? {name}[{vector_index}] : uint4(0u))")
        } else {
            format!("{name}[{vector_index}]")
        };
        let component = match offset {
            Value::ImmU32(offset) => format!("{}u", (offset / 4) % 4 + element_offset),
            _ if element_offset == 0 => {
                format!("((({offset_expression}) >> 2u) & 3u)")
            }
            _ => format!("((((({offset_expression}) >> 2u) & 3u)) + {element_offset}u)"),
        };
        Ok(format!("{vector}[{component}]"))
    }

    pub fn bit_offset_expression(
        &self,
        inst_ref: InstRef,
        offset: &Value,
        width: u32,
    ) -> Result<String, MslError> {
        let expression = self.value_expression(offset, inst_ref, 1)?;
        Ok(match (offset, width) {
            (Value::ImmU32(offset), 8) => format!("{}u", (offset % 4) * 8),
            (Value::ImmU32(offset), 16) => format!("{}u", ((offset / 2) % 2) * 16),
            (_, 8) => format!("((({expression}) << 3u) & 24u)"),
            (_, 16) => format!("((({expression}) << 3u) & 16u)"),
            _ => unreachable!("subword extraction width must be 8 or 16"),
        })
    }

    pub fn storage_buffer_word_expression(
        &self,
        inst_ref: InstRef,
        binding: u32,
        offset: &Value,
        word_offset: u32,
    ) -> Result<String, MslError> {
        let name = self
            .storage_buffers
            .get(&binding)
            .ok_or(MslError::MissingStorageBuffer(binding))?;
        let offset_expression = self.value_expression(offset, inst_ref, 1)?;
        let index = match offset {
            Value::ImmU32(offset) => format!("{}u", offset / 4 + word_offset),
            _ if word_offset == 0 => format!("(({offset_expression}) >> 2u)"),
            _ => format!("((({offset_expression}) >> 2u) + {word_offset}u)"),
        };
        Ok(format!("{name}[{index}]"))
    }

    pub fn emit_statement(&mut self, statement: &str) {
        self.source.push_str("    ");
        self.source.push_str(statement);
        self.source.push('\n');
    }

    pub fn require_storage_subword_cas(&mut self) {
        self.uses_storage_subword_cas = true;
    }

    pub fn require_shared_subword_cas(&mut self) {
        self.uses_shared_subword_cas = true;
    }

    pub fn require_atomic_inc_dec_cas(&mut self) {
        self.uses_atomic_inc_dec_cas = true;
    }

    fn unsupported_value_name(value: &Value) -> &'static str {
        match value {
            Value::Inst(_) => "undefined instruction",
            Value::Reg(_) => "register",
            Value::Pred(_) => "predicate",
            Value::Attribute(_) => "attribute",
            Value::Patch(_) => "patch",
            Value::ImmU1(_) => "u1 immediate",
            Value::ImmU8(_) => "u8 immediate",
            Value::ImmU16(_) => "u16 immediate",
            Value::ImmU32(_) => "u32 immediate",
            Value::ImmU64(_) => "u64 immediate",
            Value::ImmF16(_) => "f16 immediate",
            Value::ImmF32(_) => "f32 immediate",
            Value::ImmF64(_) => "f64 immediate",
            Value::Void => "void",
        }
    }

    pub fn value_expression(
        &self,
        value: &Value,
        inst_ref: InstRef,
        arg: u32,
    ) -> Result<String, MslError> {
        match value {
            Value::Inst(reference) => {
                self.definitions
                    .get(reference)
                    .cloned()
                    .ok_or(MslError::UnsupportedValue {
                        block: inst_ref.block,
                        inst: inst_ref.inst,
                        arg,
                        value: "undefined instruction",
                    })
            }
            Value::ImmU1(value) => Ok(if *value { "true" } else { "false" }.to_owned()),
            Value::ImmU32(value) => Ok(format!("0x{value:08X}u")),
            Value::ImmU64(value) => Ok(format!("0x{value:016X}ul")),
            Value::ImmF16(value) => Ok(format!("as_type<half>(ushort(0x{value:04X}u))")),
            Value::ImmF32(value) => Ok(format!("as_type<float>(0x{:08X}u)", value.to_bits())),
            other => Err(MslError::UnsupportedValue {
                block: inst_ref.block,
                inst: inst_ref.inst,
                arg,
                value: Self::unsupported_value_name(other),
            }),
        }
    }

    pub fn is_defined(&self, inst_ref: InstRef) -> bool {
        self.definitions.contains_key(&inst_ref)
    }

    pub fn define(
        &mut self,
        inst_ref: InstRef,
        ty: Type,
        expression: String,
        precise: bool,
    ) -> Result<(), MslError> {
        let name = format!("v_{}_{}", inst_ref.block, inst_ref.inst);
        debug_assert!(!precise, "precision must be expressed by the MSL operation");
        self.source.push_str(&format!(
            "    {} {name} = {expression};\n",
            Self::type_name(ty)?
        ));
        self.definitions.insert(inst_ref, name);
        Ok(())
    }

    pub fn push_statement(&mut self, statement: String) {
        self.source.push_str("    ");
        self.source.push_str(&statement);
        self.source.push('\n');
    }

    pub fn emit_binary(
        &mut self,
        program: &crate::ir::Program,
        inst_ref: InstRef,
        inst: &Inst,
        ty: Type,
        operator: &'static str,
    ) -> Result<(), MslError> {
        self.emit_binary_with_precision(program, inst_ref, inst, ty, operator, false)
    }

    pub fn emit_binary_with_precision(
        &mut self,
        _program: &crate::ir::Program,
        inst_ref: InstRef,
        inst: &Inst,
        ty: Type,
        operator: &'static str,
        precise: bool,
    ) -> Result<(), MslError> {
        let lhs = self.value_expression(inst.arg(0), inst_ref, 0)?;
        let rhs = self.value_expression(inst.arg(1), inst_ref, 1)?;
        let expression = if precise {
            match operator {
                "+" => {
                    self.uses_no_contraction_add = true;
                    format!("spvFAdd({lhs}, {rhs})")
                }
                "*" => {
                    self.uses_no_contraction_mul = true;
                    format!("spvFMul({lhs}, {rhs})")
                }
                _ => {
                    return Err(MslError::UnsupportedProgramFeature(
                        "NoContraction operation",
                    ))
                }
            }
        } else {
            format!("({lhs}) {operator} ({rhs})")
        };
        self.define(inst_ref, ty, expression, false)
    }

    pub fn emit_fma(&mut self, inst_ref: InstRef, inst: &Inst, ty: Type) -> Result<(), MslError> {
        let a = self.value_expression(inst.arg(0), inst_ref, 0)?;
        let b = self.value_expression(inst.arg(1), inst_ref, 1)?;
        let c = self.value_expression(inst.arg(2), inst_ref, 2)?;
        let control = crate::ir::types::FpControl::from_u32(inst.flags);
        let expression = if control.no_contraction {
            self.uses_no_contraction_fma = true;
            format!("spvFma({a}, {b}, {c})")
        } else {
            format!("fma({a}, {b}, {c})")
        };
        self.define(inst_ref, ty, expression, false)
    }

    pub fn emit_identity(
        &mut self,
        program: &crate::ir::Program,
        inst_ref: InstRef,
        inst: &Inst,
    ) -> Result<(), MslError> {
        let expression = self.value_expression(inst.arg(0), inst_ref, 0)?;
        let ty = match inst.arg(0) {
            Value::Inst(reference) => program
                .block(reference.block)
                .inst(reference.inst)
                .return_type(),
            value => value.ir_type(),
        };
        self.define(inst_ref, ty, expression, false)
    }

    pub fn emit_set_position(
        &mut self,
        inst_ref: InstRef,
        component: u32,
        value: &Value,
    ) -> Result<(), MslError> {
        let expression = self.value_expression(value, inst_ref, 1)?;
        let swizzle = ["x", "y", "z", "w"][component as usize];
        self.source
            .push_str(&format!("    output.position.{swizzle} = {expression};\n"));
        Ok(())
    }

    pub fn emit_set_frag_color(
        &mut self,
        inst_ref: InstRef,
        render_target: u32,
        component: u32,
        value: &Value,
    ) -> Result<(), MslError> {
        let expression = self.value_expression(value, inst_ref, 2)?;
        let swizzle = ["x", "y", "z", "w"][component as usize];
        self.source.push_str(&format!(
            "    output.color{render_target}.{swizzle} = {expression};\n"
        ));
        Ok(())
    }

    pub fn finish(mut self) -> MslShaderArtifact {
        if self.returns_output {
            self.source.push_str("    return output;\n");
        }
        self.source.push_str("}\n");
        let mut source = String::from("#include <metal_stdlib>\nusing namespace metal;\n\n");
        if self.uses_no_contraction_add {
            source.push_str(concat!(
                "template<typename T>\n",
                "[[clang::optnone]] T spvFAdd(T lhs, T rhs) {\n",
                "    return fma(T(1), lhs, rhs);\n",
                "}\n\n",
            ));
        }
        if self.uses_no_contraction_mul {
            source.push_str(concat!(
                "template<typename T>\n",
                "[[clang::optnone]] T spvFMul(T lhs, T rhs) {\n",
                "    return fma(lhs, rhs, T(0));\n",
                "}\n\n",
            ));
        }
        if self.uses_no_contraction_fma {
            source.push_str(concat!(
                "template<typename T>\n",
                "[[clang::optnone]] T spvFma(T a, T b, T c) {\n",
                "    return fma(a, b, c);\n",
                "}\n\n",
            ));
        }
        if self.uses_storage_subword_cas {
            source.push_str(concat!(
                "inline void spvWriteStorageBits(device uint* pointer, uint value, uint bit_offset, uint bit_count) {\n",
                "    device atomic_uint* atomic_pointer = reinterpret_cast<device atomic_uint*>(pointer);\n",
                "    uint expected = atomic_load_explicit(atomic_pointer, memory_order_relaxed);\n",
                "    while (true) {\n",
                "        uint desired = insert_bits(expected, value, bit_offset, bit_count);\n",
                "        if (atomic_compare_exchange_weak_explicit(atomic_pointer, &expected, desired, memory_order_relaxed, memory_order_relaxed)) {\n",
                "            return;\n",
                "        }\n",
                "    }\n",
                "}\n\n",
            ));
        }
        if self.uses_shared_subword_cas {
            source.push_str(concat!(
                "inline void spvWriteSharedBits(threadgroup uint* pointer, uint value, uint bit_offset, uint bit_count) {\n",
                "    threadgroup atomic_uint* atomic_pointer = reinterpret_cast<threadgroup atomic_uint*>(pointer);\n",
                "    uint expected = atomic_load_explicit(atomic_pointer, memory_order_relaxed);\n",
                "    while (true) {\n",
                "        uint desired = insert_bits(expected, value, bit_offset, bit_count);\n",
                "        if (atomic_compare_exchange_weak_explicit(atomic_pointer, &expected, desired, memory_order_relaxed, memory_order_relaxed)) {\n",
                "            return;\n",
                "        }\n",
                "    }\n",
                "}\n\n",
            ));
        }
        if self.uses_atomic_inc_dec_cas {
            source.push_str(concat!(
                "template<typename T>\n",
                "inline uint spvAtomicInc(T pointer, uint limit) {\n",
                "    uint expected = atomic_load_explicit(pointer, memory_order_relaxed);\n",
                "    while (true) {\n",
                "        uint desired = expected >= limit ? 0u : expected + 1u;\n",
                "        if (atomic_compare_exchange_weak_explicit(pointer, &expected, desired, memory_order_relaxed, memory_order_relaxed)) {\n",
                "            return expected;\n",
                "        }\n",
                "    }\n",
                "}\n\n",
                "template<typename T>\n",
                "inline uint spvAtomicDec(T pointer, uint limit) {\n",
                "    uint expected = atomic_load_explicit(pointer, memory_order_relaxed);\n",
                "    while (true) {\n",
                "        uint desired = expected == 0u || expected > limit ? limit : expected - 1u;\n",
                "        if (atomic_compare_exchange_weak_explicit(pointer, &expected, desired, memory_order_relaxed, memory_order_relaxed)) {\n",
                "            return expected;\n",
                "        }\n",
                "    }\n",
                "}\n\n",
            ));
        }
        if self.uses_texture_cast {
            source.push_str(concat!(
                "template<typename T, typename U>\n",
                "T spvTextureCast(U image) {\n",
                "    return reinterpret_cast<thread const T&>(image);\n",
                "}\n\n",
            ));
        }
        source.push_str(&self.source);
        MslShaderArtifact {
            source: MslShaderSource {
                source,
                stage: self.stage,
            },
            bindings: self.bindings,
            entry_point: "main0".to_owned(),
            language_version: self.language_version,
            execution: self.execution,
        }
    }
}

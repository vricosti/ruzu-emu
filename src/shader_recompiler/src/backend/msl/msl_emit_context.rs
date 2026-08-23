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
use crate::runtime_info::{AttributeType, CompareFunction, RuntimeInfo};
use crate::shader_info::{
    ImageBufferDescriptor, ImageDescriptor, Info, TextureBufferDescriptor, TextureDescriptor,
    TextureType,
};
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
    texture_buffers: Vec<MslTextureBufferDefinition>,
    image_buffers: Vec<MslImageBufferDefinition>,
    textures: Vec<MslTextureDefinition>,
    images: Vec<MslImageDefinition>,
    input_generics: [Option<MslInputGenericDefinition>; 32],
    bindings: MslBindingLayout,
    returns_output: bool,
    terminal_return_emitted: bool,
    uses_no_contraction_add: bool,
    uses_no_contraction_mul: bool,
    uses_no_contraction_fma: bool,
    uses_storage_subword_cas: bool,
    uses_shared_subword_cas: bool,
    uses_atomic_inc_dec_cas: bool,
    uses_texture_cast: bool,
    tracks_helper_invocation: bool,
    uses_cbuf_indirect: bool,
    language_version: MslVersion,
    supports_query_texture_lod: bool,
    supports_texture_atomics: bool,
    supports_typeless_image_loads: bool,
    supports_subgroups: bool,
    warp_size_potentially_larger_than_guest: bool,
    fixed_subgroup_size: u32,
    texture_rescaling_index: u32,
    image_rescaling_index: u32,
    uses_rescaling_push_constants: bool,
    need_gather_subpixel_offset: bool,
    execution: MslExecutionInfo,
    has_broken_robust: bool,
    support_vertex_instance_id: bool,
    convert_depth_mode: bool,
    emits_frag_depth: bool,
    emits_point_size: bool,
    clip_distance_count: u32,
    fixed_state_point_size: Option<f32>,
    alpha_test_func: Option<CompareFunction>,
    alpha_test_reference: f32,
    dual_source_blend: bool,
    emits_frag_color: [bool; 8],
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
struct MslTextureBufferDefinition {
    texture_name: String,
    count: u32,
}

#[derive(Debug, Clone)]
struct MslImageBufferDefinition {
    image_name: String,
    count: u32,
    is_integer: bool,
}

#[derive(Debug, Clone)]
struct MslImageDefinition {
    image_name: String,
    texture_type: TextureType,
    count: u32,
    is_integer: bool,
}

#[derive(Debug, Clone, Copy)]
enum MslInputGenericLoadOp {
    None,
    Bitcast,
    SignedToFloat,
    UnsignedToFloat,
}

#[derive(Debug, Clone)]
struct MslInputGenericDefinition {
    name: String,
    load_op: MslInputGenericLoadOp,
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
        runtime_info: &RuntimeInfo,
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
        let mut texture_buffers = Vec::new();
        let mut image_buffers = Vec::new();
        let mut textures = Vec::new();
        let mut images = Vec::new();
        let mut parameters = Vec::new();
        let mut input_generics: [Option<MslInputGenericDefinition>; 32] =
            std::array::from_fn(|_| None);
        let uses_rescaling_push_constants = program.info.uses_rescaling_uniform;
        let push_constant_declaration = if uses_rescaling_push_constants {
            let buffer_index = bindings.buffer_count;
            bindings.buffer_count += 1;
            bindings.push_constant_buffer_index = Some(buffer_index);
            parameters.push(format!(
                "constant MslResolutionInfo& rescaling_push_constants [[buffer({buffer_index})]]"
            ));
            if stage == Stage::Compute {
                concat!(
                    "struct MslResolutionInfo {\n",
                    "    uint4 rescaling_textures;\n",
                    "    uint2 rescaling_images;\n",
                    "};\n\n",
                )
            } else {
                concat!(
                    "struct MslResolutionInfo {\n",
                    "    uint4 rescaling_textures;\n",
                    "    uint2 rescaling_images;\n",
                    "    float down_factor;\n",
                    "};\n\n",
                )
            }
        } else if program.info.uses_render_area {
            let buffer_index = bindings.buffer_count;
            bindings.buffer_count += 1;
            bindings.push_constant_buffer_index = Some(buffer_index);
            parameters.push(format!(
                "constant MslRenderAreaInfo& render_area_push_constants [[buffer({buffer_index})]]"
            ));
            concat!(
                "struct MslRenderAreaInfo {\n",
                "    float4 render_area;\n",
                "};\n\n",
            )
        } else {
            ""
        };
        let texture_rescaling_index = binding_counters.texture_scaling_index;
        let image_rescaling_index = binding_counters.image_scaling_index;
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
        if program.info.uses_cbuf_indirect {
            for index in 0..Info::MAX_INDIRECT_CBUFS as u32 {
                if !constant_buffers.contains_key(&index) {
                    return Err(MslError::MissingConstantBuffer(index));
                }
            }
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
        for (descriptor_index, descriptor) in
            program.info.texture_buffer_descriptors.iter().enumerate()
        {
            let definition = Self::define_texture_buffer(
                descriptor_index as u32,
                descriptor,
                *binding_counter,
                &mut bindings,
                &mut parameters,
            )?;
            texture_buffers.push(definition);
            *binding_counter += 1;
        }
        let binding_counter = if profile.unified_descriptor_binding {
            &mut binding_counters.unified
        } else {
            &mut binding_counters.image
        };
        for (descriptor_index, descriptor) in
            program.info.image_buffer_descriptors.iter().enumerate()
        {
            let definition = Self::define_image_buffer(
                descriptor_index as u32,
                descriptor,
                *binding_counter,
                options.supports_read_write_textures,
                &mut bindings,
                &mut parameters,
            )?;
            image_buffers.push(definition);
            *binding_counter += 1;
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
        binding_counters.texture_scaling_index += program.info.texture_descriptors.len() as u32;
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
        binding_counters.image_scaling_index += program.info.image_descriptors.len() as u32;
        // Normal Metal vertex functions do not expose the SIMD-group lane
        // builtin. Fragment and kernel functions do, and the renderer's
        // profile advertises subgroup support only for those stages.
        let supports_subgroups = profile.supports_subgroup_stage(stage)
            && matches!(stage, Stage::Fragment | Stage::Compute);
        if supports_subgroups && options.fixed_subgroup_size < 32 {
            return Err(MslError::UnsupportedProgramFeature(
                "Metal SIMD group narrower than the guest warp",
            ));
        }
        if supports_subgroups
            && options.fixed_subgroup_size > 32
            && !profile.warp_size_potentially_larger_than_guest
        {
            return Err(MslError::UnsupportedProgramFeature(
                "Metal SIMD group wider than the guest warp",
            ));
        }
        if supports_subgroups && options.fixed_subgroup_size > 64 {
            return Err(MslError::UnsupportedProgramFeature(
                "Metal SIMD group wider than the ballot representation",
            ));
        }
        let needs_subgroup_lane_id = supports_subgroups
            && (program.info.uses_fswzadd
                || program.info.uses_subgroup_invocation_id
                || program.info.uses_subgroup_shuffles
                || program.info.uses_subgroup_mask
                || (profile.warp_size_potentially_larger_than_guest
                    && program.info.uses_subgroup_vote));
        if needs_subgroup_lane_id {
            parameters.push("uint subgroup_lane_id [[thread_index_in_simdgroup]]".to_owned());
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
        let loads = &program.info.loads;
        match stage {
            Stage::VertexB => {
                if loads.get(crate::ir::value::Attribute::INSTANCE_ID.0 as usize) {
                    if profile.support_vertex_instance_id {
                        parameters.push("uint instance_id [[instance_id]]".to_owned());
                        if loads.get(crate::ir::value::Attribute::BASE_INSTANCE.0 as usize) {
                            parameters.push("uint base_instance [[base_instance]]".to_owned());
                        }
                    } else {
                        parameters.push("uint instance_index [[instance_id]]".to_owned());
                        parameters.push("uint base_instance [[base_instance]]".to_owned());
                    }
                } else if loads.get(crate::ir::value::Attribute::BASE_INSTANCE.0 as usize) {
                    parameters.push("uint base_instance [[base_instance]]".to_owned());
                }
                if loads.get(crate::ir::value::Attribute::VERTEX_ID.0 as usize) {
                    if profile.support_vertex_instance_id {
                        parameters.push("uint vertex_id [[vertex_id]]".to_owned());
                        if loads.get(crate::ir::value::Attribute::BASE_VERTEX.0 as usize) {
                            parameters.push("uint base_vertex [[base_vertex]]".to_owned());
                        }
                    } else {
                        parameters.push("uint vertex_index [[vertex_id]]".to_owned());
                        parameters.push("uint base_vertex [[base_vertex]]".to_owned());
                    }
                } else if loads.get(crate::ir::value::Attribute::BASE_VERTEX.0 as usize) {
                    parameters.push("uint base_vertex [[base_vertex]]".to_owned());
                }
            }
            Stage::Fragment => {
                if loads.get(crate::ir::value::Attribute::PRIMITIVE_ID.0 as usize) {
                    parameters.push("uint primitive_id [[primitive_id]]".to_owned());
                }
                if loads.get(crate::ir::value::Attribute::LAYER.0 as usize) {
                    parameters.push("uint layer [[render_target_array_index]]".to_owned());
                }
                if loads.any_component(crate::ir::value::Attribute::POSITION_X.0 as usize) {
                    parameters.push("float4 fragment_position [[position]]".to_owned());
                }
                if loads.get(crate::ir::value::Attribute::FRONT_FACE.0 as usize) {
                    parameters.push("bool front_face [[front_facing]]".to_owned());
                }
                if loads.get(crate::ir::value::Attribute::POINT_SPRITE_S.0 as usize)
                    || loads.get(crate::ir::value::Attribute::POINT_SPRITE_T.0 as usize)
                {
                    parameters.push("float2 point_coord [[point_coord]]".to_owned());
                }
            }
            _ => {}
        }
        let mut stage_input = String::new();
        for index in 0..32 {
            let input_type = runtime_info.generic_input_types[index];
            if !runtime_info.previous_stage_stores.generic_any(index)
                || !program.info.loads.generic_any(index)
                || input_type == AttributeType::Disabled
            {
                continue;
            }
            if stage_input.is_empty() {
                let name = match stage {
                    Stage::VertexB => "MslVertexIn",
                    Stage::Fragment => "MslFragmentIn",
                    _ => {
                        return Err(MslError::UnsupportedProgramFeature(
                            "generic inputs for this stage",
                        ));
                    }
                };
                stage_input.push_str(&format!("struct {name} {{\n"));
            }
            let (type_name, load_op, is_integer) = Self::generic_input_type(profile, input_type);
            let attribute = match stage {
                Stage::VertexB => format!("[[attribute({index})]]"),
                Stage::Fragment => {
                    let interpolation = if is_integer {
                        ""
                    } else {
                        match program.info.interpolation[index] {
                            crate::shader_info::Interpolation::Smooth => "",
                            crate::shader_info::Interpolation::NoPerspective => {
                                ", center_no_perspective"
                            }
                            crate::shader_info::Interpolation::Flat => ", flat",
                        }
                    };
                    format!("[[user(locn{index}){interpolation}]]")
                }
                _ => unreachable!(),
            };
            let name = format!("in_attr{index}");
            stage_input.push_str(&format!("    {type_name} {name} {attribute};\n"));
            input_generics[index] = Some(MslInputGenericDefinition { name, load_op });
        }
        if !stage_input.is_empty() {
            stage_input.push_str("};\n\n");
            let parameter = match stage {
                Stage::VertexB => "MslVertexIn input [[stage_in]]",
                Stage::Fragment => "MslFragmentIn input [[stage_in]]",
                _ => unreachable!(),
            };
            parameters.insert(0, parameter.to_owned());
        }
        let parameters = parameters.join(", ");
        let mut source = String::new();
        source.push_str(push_constant_declaration);
        source.push_str(&stage_input);
        // SPIRV-Cross removes FragDepth when EarlyFragmentTests is active:
        // SPIR-V makes that write ineffective, while Metal rejects the pair.
        let emits_frag_depth = program.info.stores_frag_depth && !runtime_info.force_early_z;
        let emits_point_size = program
            .info
            .stores
            .get(crate::ir::value::Attribute::POINT_SIZE.0 as usize)
            || runtime_info.fixed_state_point_size.is_some();
        if emits_point_size && stage != Stage::VertexB {
            return Err(MslError::UnsupportedProgramFeature(
                "point-size output outside a vertex shader",
            ));
        }
        let clip_distance_count = if program.info.stores.clip_distances() {
            profile.max_user_clip_distances.min(8)
        } else {
            0
        };
        let mut emits_frag_color = program.info.stores_frag_color;
        if runtime_info.dual_source_blend {
            emits_frag_color[0] = true;
            emits_frag_color[1] = true;
        }
        let returns_output = match stage {
            Stage::VertexB => {
                source.push_str("struct MslVertexOut {\n");
                source.push_str("    float4 position [[position]];\n");
                if emits_point_size {
                    source.push_str("    float point_size [[point_size]];\n");
                }
                if clip_distance_count != 0 {
                    source.push_str(&format!(
                        "    float clip_distance [[clip_distance]] [{clip_distance_count}];\n"
                    ));
                }
                for index in 0..32 {
                    if program.info.stores.generic_any(index) {
                        source.push_str(&format!(
                            "    float4 out_attr{index} [[user(locn{index})]];\n"
                        ));
                    }
                }
                source.push_str("};\n\n");
                source.push_str(&format!("vertex MslVertexOut main0({parameters}) {{\n"));
                source.push_str(concat!(
                    "    MslVertexOut output = {};\n",
                    "    output.position = float4(0.0f);\n",
                ));
                true
            }
            Stage::Fragment
                if emits_frag_color.iter().any(|store| *store)
                    || emits_frag_depth
                    || program.info.stores_sample_mask =>
            {
                source.push_str("struct MslFragmentOut {\n");
                for (index, stored) in emits_frag_color.iter().enumerate() {
                    if *stored {
                        let attribute = if runtime_info.dual_source_blend && index <= 1 {
                            format!("[[color(0), index({index})]]")
                        } else {
                            format!("[[color({index})]]")
                        };
                        source.push_str(&format!("    float4 color{index} {attribute};\n"));
                    }
                }
                if emits_frag_depth {
                    source.push_str("    float depth [[depth(any)]];\n");
                }
                if program.info.stores_sample_mask {
                    source.push_str("    uint sample_mask [[sample_mask]];\n");
                }
                let qualifier = if runtime_info.force_early_z {
                    "[[early_fragment_tests]] fragment"
                } else {
                    "fragment"
                };
                source.push_str(&format!(
                    "}};\n\n{qualifier} MslFragmentOut main0({parameters}) {{\n"
                ));
                source.push_str("    MslFragmentOut output = {};\n");
                true
            }
            Stage::Fragment => {
                let qualifier = if runtime_info.force_early_z {
                    "[[early_fragment_tests]] fragment"
                } else {
                    "fragment"
                };
                source.push_str(&format!("{qualifier} void main0({parameters}) {{\n"));
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
            texture_buffers,
            image_buffers,
            textures,
            images,
            input_generics,
            bindings,
            returns_output,
            terminal_return_emitted: false,
            uses_no_contraction_add: false,
            uses_no_contraction_mul: false,
            uses_no_contraction_fma: false,
            uses_storage_subword_cas: false,
            uses_shared_subword_cas: false,
            uses_atomic_inc_dec_cas: false,
            uses_texture_cast: false,
            tracks_helper_invocation,
            uses_cbuf_indirect: program.info.uses_cbuf_indirect,
            language_version: options.language_version,
            supports_query_texture_lod: options.supports_query_texture_lod,
            supports_texture_atomics: options.supports_texture_atomics,
            supports_typeless_image_loads: profile.support_typeless_image_loads,
            supports_subgroups,
            warp_size_potentially_larger_than_guest: profile
                .warp_size_potentially_larger_than_guest,
            fixed_subgroup_size: options.fixed_subgroup_size,
            texture_rescaling_index,
            image_rescaling_index,
            uses_rescaling_push_constants,
            need_gather_subpixel_offset: profile.need_gather_subpixel_offset,
            execution: MslExecutionInfo {
                workgroup_size: (stage == Stage::Compute).then_some(program.workgroup_size),
                fixed_subgroup_size: options.fixed_subgroup_size,
            },
            has_broken_robust: profile.has_broken_robust,
            support_vertex_instance_id: profile.support_vertex_instance_id,
            convert_depth_mode: runtime_info.convert_depth_mode && !profile.support_native_ndc,
            emits_frag_depth,
            emits_point_size,
            clip_distance_count,
            fixed_state_point_size: runtime_info.fixed_state_point_size,
            alpha_test_func: runtime_info.alpha_test_func,
            alpha_test_reference: runtime_info.alpha_test_reference,
            dual_source_blend: runtime_info.dual_source_blend,
            emits_frag_color,
        })
    }

    fn generic_input_type(
        profile: &Profile,
        attribute_type: AttributeType,
    ) -> (&'static str, MslInputGenericLoadOp, bool) {
        match attribute_type {
            AttributeType::Float => ("float4", MslInputGenericLoadOp::None, false),
            AttributeType::SignedInt => ("int4", MslInputGenericLoadOp::Bitcast, true),
            AttributeType::UnsignedInt => ("uint4", MslInputGenericLoadOp::Bitcast, true),
            AttributeType::SignedScaled if profile.support_scaled_attributes => {
                ("float4", MslInputGenericLoadOp::None, false)
            }
            AttributeType::SignedScaled => ("int4", MslInputGenericLoadOp::SignedToFloat, true),
            AttributeType::UnsignedScaled if profile.support_scaled_attributes => {
                ("float4", MslInputGenericLoadOp::None, false)
            }
            AttributeType::UnsignedScaled => {
                ("uint4", MslInputGenericLoadOp::UnsignedToFloat, true)
            }
            AttributeType::Disabled => unreachable!("disabled generic inputs are not declared"),
        }
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

    /// Native-MSL counterpart of Eden `EmitContext::DefineTextureBuffers`.
    fn define_texture_buffer(
        descriptor_index: u32,
        descriptor: &TextureBufferDescriptor,
        descriptor_binding: u32,
        bindings: &mut MslBindingLayout,
        parameters: &mut Vec<String>,
    ) -> Result<MslTextureBufferDefinition, MslError> {
        if descriptor.count != 1 {
            return Err(MslError::UnsupportedProgramFeature(
                "array of texture buffers",
            ));
        }
        let texture_index = bindings.texture_count;
        bindings.texture_count += 1;
        bindings.resources.push(MslResourceBinding {
            descriptor_set: 0,
            binding: descriptor_binding,
            kind: MslResourceKind::SeparateImage,
            buffer_index: 0,
            texture_index,
            sampler_index: 0,
            count: None,
        });
        let texture_name = format!("texbuf{descriptor_index}");
        parameters.push(format!(
            "texture_buffer<float, access::read> {texture_name} [[texture({texture_index})]]"
        ));
        Ok(MslTextureBufferDefinition {
            texture_name,
            count: descriptor.count,
        })
    }

    /// Native-MSL counterpart of Eden `EmitContext::DefineImageBuffers`.
    fn define_image_buffer(
        descriptor_index: u32,
        descriptor: &ImageBufferDescriptor,
        descriptor_binding: u32,
        supports_read_write_textures: bool,
        bindings: &mut MslBindingLayout,
        parameters: &mut Vec<String>,
    ) -> Result<MslImageBufferDefinition, MslError> {
        if descriptor.count == 0 {
            return Err(MslError::UnsupportedProgramFeature(
                "zero-sized image-buffer descriptor array",
            ));
        }
        let access = match (descriptor.is_read, descriptor.is_written) {
            (true, false) => "read",
            (false, true) => "write",
            (true, true) if supports_read_write_textures => "read_write",
            (true, true) => {
                return Err(MslError::UnsupportedProgramFeature(
                    "read/write image buffer on this Metal device",
                ));
            }
            (false, false) => {
                return Err(MslError::UnsupportedProgramFeature(
                    "image buffer with no declared access",
                ));
            }
        };
        let component = if descriptor.is_integer {
            "uint"
        } else {
            "float"
        };
        let image_type = format!("texture_buffer<{component}, access::{access}>");
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
        let image_name = format!("imgbuf{descriptor_index}");
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
        Ok(MslImageBufferDefinition {
            image_name,
            count: descriptor.count,
            is_integer: descriptor.is_integer,
        })
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

    pub(crate) fn converts_depth_mode(&self) -> bool {
        self.convert_depth_mode
    }

    pub(crate) fn emits_point_size(&self) -> bool {
        self.emits_point_size
    }

    pub(crate) fn clip_distance_count(&self) -> u32 {
        self.clip_distance_count
    }

    pub(crate) fn fixed_state_point_size(&self) -> Option<f32> {
        self.fixed_state_point_size
    }

    pub(crate) fn alpha_test(&self) -> Option<(CompareFunction, f32)> {
        self.alpha_test_func
            .map(|function| (function, self.alpha_test_reference))
    }

    pub(crate) fn dual_source_blend(&self) -> bool {
        self.dual_source_blend
    }

    pub(crate) fn emits_frag_color(&self, index: usize) -> bool {
        self.emits_frag_color[index]
    }

    pub fn support_vertex_instance_id(&self) -> bool {
        self.support_vertex_instance_id
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

    pub fn language_version(&self) -> MslVersion {
        self.language_version
    }

    pub fn supports_subgroups(&self) -> bool {
        self.supports_subgroups
    }

    pub fn warp_size_potentially_larger_than_guest(&self) -> bool {
        self.warp_size_potentially_larger_than_guest
    }

    pub fn fixed_subgroup_size(&self) -> u32 {
        self.fixed_subgroup_size
    }

    pub(super) fn texture_rescaling_index(&self) -> u32 {
        self.texture_rescaling_index
    }

    pub(super) fn image_rescaling_index(&self) -> u32 {
        self.image_rescaling_index
    }

    pub(super) fn resolution_down_factor_expression(&self) -> &'static str {
        "rescaling_push_constants.down_factor"
    }

    pub(super) fn render_area_expression(&self) -> &'static str {
        if self.uses_rescaling_push_constants {
            "as_type<float4>(rescaling_push_constants.rescaling_textures)"
        } else {
            "render_area_push_constants.render_area"
        }
    }

    pub fn subgroup_lane_id_expression(&self) -> &'static str {
        if self.supports_subgroups {
            "subgroup_lane_id"
        } else {
            "0u"
        }
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
        let instruction_type = TextureType::from_u8(info.texture_type);
        if instruction_type == TextureType::Buffer {
            self.texture_buffers
                .get(info.descriptor_index as usize)
                .ok_or(MslError::MissingTexture(info.descriptor_index.into()))?;
            return Ok(());
        }
        let definition = self
            .textures
            .get(info.descriptor_index as usize)
            .ok_or(MslError::MissingTexture(info.descriptor_index.into()))?;
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
        if TextureType::from_u8(info.texture_type) == TextureType::Buffer {
            let definition = self
                .texture_buffers
                .get(info.descriptor_index as usize)
                .ok_or(MslError::MissingTexture(info.descriptor_index.into()))?;
            let texture = if definition.count == 1 {
                definition.texture_name.clone()
            } else {
                let index = self.value_expression(index, inst_ref, 0)?;
                format!("{}[{index}]", definition.texture_name)
            };
            return Ok(MslTextureExpressions {
                texture,
                sampler: String::new(),
                texture_type: TextureType::Buffer,
                is_depth: false,
                is_integer: false,
                is_multisample: false,
            });
        }
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
        let instruction_type = TextureType::from_u8(info.texture_type);
        if instruction_type == TextureType::Buffer {
            let definition = self
                .image_buffers
                .get(info.descriptor_index as usize)
                .ok_or(MslError::MissingImage(info.descriptor_index.into()))?;
            let image = if definition.count == 1 {
                definition.image_name.clone()
            } else {
                let index = self.value_expression(index, inst_ref, 0)?;
                format!("{}[{index}]", definition.image_name)
            };
            return Ok(MslImageExpressions {
                image,
                texture_type: TextureType::Buffer,
                is_integer: definition.is_integer,
            });
        }
        let definition = self
            .images
            .get(info.descriptor_index as usize)
            .ok_or(MslError::MissingImage(info.descriptor_index.into()))?;
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
        binding: &Value,
        offset: &Value,
        element_offset: u32,
    ) -> Result<String, MslError> {
        let offset_expression = self.value_expression(offset, inst_ref, 1)?;
        let vector_index = match offset {
            Value::ImmU32(offset) => format!("{}u", offset / 16),
            _ => format!("(({offset_expression}) >> 4u)"),
        };
        let vector = match binding {
            Value::ImmU32(binding) => {
                let name = self
                    .constant_buffers
                    .get(binding)
                    .ok_or(MslError::MissingConstantBuffer(*binding))?;
                format!("{name}[{vector_index}]")
            }
            binding => {
                if !self.uses_cbuf_indirect {
                    return Err(MslError::UnsupportedProgramFeature(
                        "indirect constant-buffer binding was not collected",
                    ));
                }
                let binding_expression = self.value_expression(binding, inst_ref, 0)?;
                let buffers = (0..Info::MAX_INDIRECT_CBUFS)
                    .map(|index| {
                        self.constant_buffers
                            .get(&(index as u32))
                            .cloned()
                            .ok_or(MslError::MissingConstantBuffer(index as u32))
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ");
                format!("spvLoadConstU32x4({binding_expression}, {vector_index}, {buffers})")
            }
        };
        let vector = if self.has_broken_robust && !matches!(offset, Value::ImmU32(_)) {
            format!("(({vector_index}) <= 0x0000FFFFu ? {vector} : uint4(0u))")
        } else {
            vector
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

    /// Native-MSL counterpart of Eden
    /// `EmitContext::DefineConstantBufferIndirectFunctions` for the
    /// non-aliasing `uint4` CBUF representation selected by Metal.
    fn define_constant_buffer_indirect_functions(source: &mut String) {
        let parameters = (0..Info::MAX_INDIRECT_CBUFS)
            .map(|index| format!("constant uint4* c{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        source.push_str(&format!(
            "inline uint4 spvLoadConstU32x4(uint binding, uint offset, {parameters}) {{\n"
        ));
        source.push_str("    switch (binding) {\n");
        for index in 0..Info::MAX_INDIRECT_CBUFS {
            source.push_str(&format!("    case {index}: return c{index}[offset];\n"));
        }
        source.push_str(concat!(
            "    default: return c0[offset];\n",
            "    }\n",
            "}\n\n",
        ));
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

    pub fn declare_phi(&mut self, inst_ref: InstRef, ty: Type) -> Result<(), MslError> {
        let name = format!("v_{}_{}", inst_ref.block, inst_ref.inst);
        let type_name = Self::type_name(ty)?;
        self.source
            .push_str(&format!("    {type_name} {name} = {type_name}(0);\n"));
        self.definitions.insert(inst_ref, name);
        Ok(())
    }

    pub fn declare_loop_safety_counter(&mut self, index: usize) {
        self.source
            .push_str(&format!("    int loop{index} = 0x2000;\n"));
    }

    pub fn emit_return(&mut self) {
        if self.returns_output {
            self.emit_statement("return output;");
        } else {
            self.emit_statement("return;");
        }
    }

    pub fn mark_terminal_return_emitted(&mut self) {
        self.terminal_return_emitted = true;
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
        let ty = match ty {
            Type::U8 | Type::U16 => Type::U32,
            ty => ty,
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

    pub fn emit_set_point_size(
        &mut self,
        inst_ref: InstRef,
        value: &Value,
    ) -> Result<(), MslError> {
        let expression = self.value_expression(value, inst_ref, 1)?;
        self.source
            .push_str(&format!("    output.point_size = {expression};\n"));
        Ok(())
    }

    pub fn emit_set_clip_distance(
        &mut self,
        inst_ref: InstRef,
        index: u32,
        value: &Value,
    ) -> Result<(), MslError> {
        if index >= self.clip_distance_count {
            log::warn!(
                "Ignoring clip distance store {} >= {} supported",
                index,
                self.clip_distance_count
            );
            return Ok(());
        }
        let expression = self.value_expression(value, inst_ref, 1)?;
        self.source.push_str(&format!(
            "    output.clip_distance[{index}] = {expression};\n"
        ));
        Ok(())
    }

    pub fn generic_input_expression(&self, attribute: crate::ir::value::Attribute) -> String {
        let index = attribute.generic_index() as usize;
        let component = attribute.generic_element() as usize;
        let Some(definition) = &self.input_generics[index] else {
            return if component == 3 { "1.0f" } else { "0.0f" }.to_owned();
        };
        let swizzle = ["x", "y", "z", "w"][component];
        let expression = format!("input.{}.{swizzle}", definition.name);
        match definition.load_op {
            MslInputGenericLoadOp::None => expression,
            MslInputGenericLoadOp::Bitcast => format!("as_type<float>({expression})"),
            MslInputGenericLoadOp::SignedToFloat | MslInputGenericLoadOp::UnsignedToFloat => {
                format!("float({expression})")
            }
        }
    }

    pub fn emit_set_generic(
        &mut self,
        inst_ref: InstRef,
        attribute: crate::ir::value::Attribute,
        value: &Value,
    ) -> Result<(), MslError> {
        let expression = self.value_expression(value, inst_ref, 1)?;
        let index = attribute.generic_index();
        let swizzle = ["x", "y", "z", "w"][attribute.generic_element() as usize];
        self.source.push_str(&format!(
            "    output.out_attr{index}.{swizzle} = {expression};\n"
        ));
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

    pub fn emit_set_sample_mask(
        &mut self,
        inst_ref: InstRef,
        value: &Value,
    ) -> Result<(), MslError> {
        let expression = self.value_expression(value, inst_ref, 0)?;
        self.source
            .push_str(&format!("    output.sample_mask = {expression};\n"));
        Ok(())
    }

    pub fn emit_set_frag_depth(
        &mut self,
        inst_ref: InstRef,
        value: &Value,
    ) -> Result<(), MslError> {
        if !self.emits_frag_depth {
            return Ok(());
        }
        let expression = self.value_expression(value, inst_ref, 0)?;
        let expression = if self.convert_depth_mode {
            format!("fma({expression}, 0.5f, 0.5f)")
        } else {
            expression
        };
        self.source
            .push_str(&format!("    output.depth = {expression};\n"));
        Ok(())
    }

    pub fn finish(mut self) -> MslShaderArtifact {
        if self.returns_output && !self.terminal_return_emitted {
            self.source.push_str("    return output;\n");
        }
        self.source.push_str("}\n");
        let mut source = String::from("#include <metal_stdlib>\nusing namespace metal;\n\n");
        if self.uses_cbuf_indirect {
            Self::define_constant_buffer_indirect_functions(&mut source);
        }
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

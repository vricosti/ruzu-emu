// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Metal shader profile and pipeline-cache ownership.
//!
//! This is the native Metal counterpart of Eden's `vk_pipeline_cache.cpp`.
//! Translation capability policy belongs to the pipeline cache, not to the
//! device wrapper or the rasterizer.

use std::collections::HashMap;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLBlendFactor, MTLBlendOperation, MTLColorWriteMask, MTLComputePipelineState, MTLDevice as _,
    MTLPixelFormat, MTLPrimitiveTopologyClass, MTLRenderPipelineDescriptor, MTLRenderPipelineState,
};
use shader_recompiler::host_translate_info::HostTranslateInfo;
use shader_recompiler::profile::Profile;
use spirv_cross2::spirv::ExecutionModel;
use thiserror::Error;

use crate::engines::draw_manager::Maxwell3DDrawView;
use crate::renderer_vulkan::fixed_pipeline_state::DynamicFeatures;
use crate::renderer_vulkan::graphics_pipeline::{GraphicsPipelineCache, GraphicsPipelineKey};
use crate::shader_cache::{GraphicsEnvironments, ShaderCache as SharedShaderCache};

use super::metal_device::{MetalDevice, MetalDeviceProfile};
use super::metal_shader::{
    compile_native_shader, MetalShaderCompileOptions, MetalShaderError, MetalShaderModule,
};

const SPIRV_1_5: u32 = 0x0001_0500;
const METAL_MIN_SSBO_ALIGNMENT: u64 = 4;
const METAL_MAX_USER_CLIP_DISTANCES: u32 = 8;

fn all_shader_stage_bits() -> u32 {
    use shader_recompiler::stage::Stage;

    [
        Stage::VertexA,
        Stage::VertexB,
        Stage::TessellationControl,
        Stage::TessellationEval,
        Stage::Geometry,
        Stage::Fragment,
        Stage::Compute,
    ]
    .into_iter()
    .fold(0, |mask, stage| mask | (1u32 << stage as u32))
}

/// Build the shader-recompiler profile owned by `MetalPipelineCache`.
///
/// These flags describe the complete SPIR-V -> SPIRV-Cross -> MSL 2.3 path,
/// not merely silicon features. A device feature remains disabled until the
/// selected MSL version and the runtime binding ABI can consume it.
pub fn make_shader_profile(device: &MetalDeviceProfile) -> Profile {
    let apple_family = device.highest_apple_family.unwrap_or_default();
    Profile {
        supported_spirv: SPIRV_1_5,
        // A single monotonically increasing SPIR-V binding prevents UBO and
        // SSBO collisions in Metal's shared buffer-index namespace.
        unified_descriptor_binding: true,
        support_descriptor_aliasing: false,
        support_int8: true,
        support_uniform_and_storage_buffer_8bit: true,
        support_storage_buffer_8bit: true,
        support_int16: true,
        support_uniform_and_storage_buffer_16bit: true,
        support_storage_buffer_16bit: true,
        support_int64: apple_family >= 3,
        support_vertex_instance_id: true,
        support_float_controls: false,
        support_vote: apple_family >= 6,
        supported_subgroup_stages: all_shader_stage_bits(),
        support_viewport_index_layer_non_geometry: false,
        support_viewport_mask: false,
        support_typeless_image_loads: false,
        support_demote_to_helper_invocation: true,
        // Apple9 exposes 64-bit atomics, but their MSL language support is
        // newer than the baseline 2.3 compiler selected by metal_shader.rs.
        support_int64_atomics: false,
        support_shared_int64_atomics: false,
        support_derivative_control: true,
        support_geometry_shader_passthrough: false,
        // Metal has fixed [0,w] depth and cannot switch to guest [-w,w].
        support_native_ndc: false,
        support_scaled_attributes: false,
        support_multi_viewport: false,
        support_geometry_streams: false,
        support_sampled_image_array_nonuniform_indexing: false,
        support_storage_image_array_nonuniform_indexing: false,
        support_uniform_texel_buffer_array_nonuniform_indexing: false,
        support_storage_texel_buffer_array_nonuniform_indexing: false,
        warp_size_potentially_larger_than_guest: false,
        lower_left_origin_mode: false,
        need_declared_frag_colors: false,
        need_fastmath_off: true,
        min_ssbo_alignment: METAL_MIN_SSBO_ALIGNMENT,
        max_user_clip_distances: METAL_MAX_USER_CLIP_DISTANCES,
        ..Profile::default()
    }
}

/// Build descriptor/resource limits for translation passes before MSL
/// emission. Direct Metal bindings have separate buffer, texture and sampler
/// namespaces; the aggregate limit is therefore their sum.
pub fn make_host_translate_info(device: &MetalDeviceProfile) -> HostTranslateInfo {
    let mut host_info = HostTranslateInfo {
        min_ssbo_alignment: METAL_MIN_SSBO_ALIGNMENT,
        max_per_stage_descriptor_sampled_images: device.max_texture_bindings_per_stage,
        max_per_stage_resources: device.max_buffer_bindings_per_stage
            + device.max_texture_bindings_per_stage
            + device.max_sampler_bindings_per_stage,
        max_descriptor_set_samplers: device.max_sampler_bindings_per_stage,
        max_descriptor_set_uniform_buffers: device.max_buffer_bindings_per_stage,
        max_descriptor_set_uniform_buffers_dynamic: device.max_buffer_bindings_per_stage,
        max_descriptor_set_storage_buffers: device.max_buffer_bindings_per_stage,
        max_descriptor_set_storage_buffers_dynamic: device.max_buffer_bindings_per_stage,
        max_descriptor_set_sampled_images: device.max_texture_bindings_per_stage,
        max_descriptor_set_storage_images: device.max_texture_bindings_per_stage,
        max_descriptor_set_input_attachements: device.max_color_render_targets,
        support_float64: false,
        support_float16: true,
        support_int64: device
            .highest_apple_family
            .is_some_and(|family| family >= 3),
        needs_demote_reorder: false,
        support_snorm_render_buffer: true,
        support_viewport_index_layer: false,
        support_geometry_shader_passthrough: false,
        support_conditional_barrier: false,
    };
    host_info.apply_descriptor_limit_policy();
    host_info
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MetalColorAttachmentState {
    pub format: MTLPixelFormat,
    pub blending_enabled: bool,
    pub source_rgb: MTLBlendFactor,
    pub destination_rgb: MTLBlendFactor,
    pub rgb_operation: MTLBlendOperation,
    pub source_alpha: MTLBlendFactor,
    pub destination_alpha: MTLBlendFactor,
    pub alpha_operation: MTLBlendOperation,
    pub write_mask: MTLColorWriteMask,
}

impl MetalColorAttachmentState {
    pub const fn disabled() -> Self {
        Self {
            format: MTLPixelFormat::Invalid,
            blending_enabled: false,
            source_rgb: MTLBlendFactor::One,
            destination_rgb: MTLBlendFactor::Zero,
            rgb_operation: MTLBlendOperation::Add,
            source_alpha: MTLBlendFactor::One,
            destination_alpha: MTLBlendFactor::Zero,
            alpha_operation: MTLBlendOperation::Add,
            write_mask: MTLColorWriteMask::All,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MetalRenderPipelineKey {
    pub vertex_shader_hash: u64,
    pub fragment_shader_hash: u64,
    /// Hash of the complete shader runtime variant (`GraphicsPipelineKey`).
    pub shader_variant_hash: u64,
    pub color_attachments: [MetalColorAttachmentState; 8],
    pub depth_format: MTLPixelFormat,
    pub stencil_format: MTLPixelFormat,
    pub sample_count: u32,
    pub topology: MTLPrimitiveTopologyClass,
    pub alpha_to_coverage: bool,
    pub alpha_to_one: bool,
    pub rasterization_enabled: bool,
}

impl MetalRenderPipelineKey {
    pub fn new(vertex_shader_hash: u64, fragment_shader_hash: u64) -> Self {
        Self {
            vertex_shader_hash,
            fragment_shader_hash,
            shader_variant_hash: 0,
            color_attachments: [MetalColorAttachmentState::disabled(); 8],
            depth_format: MTLPixelFormat::Invalid,
            stencil_format: MTLPixelFormat::Invalid,
            sample_count: 1,
            topology: MTLPrimitiveTopologyClass::Triangle,
            alpha_to_coverage: false,
            alpha_to_one: false,
            rasterization_enabled: true,
        }
    }
}

pub struct MetalRenderPipeline {
    key: MetalRenderPipelineKey,
    state: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
}

impl MetalRenderPipeline {
    pub fn key(&self) -> &MetalRenderPipelineKey {
        &self.key
    }

    pub fn state(&self) -> &ProtocolObject<dyn MTLRenderPipelineState> {
        &self.state
    }
}

pub struct MetalComputePipeline {
    shader_hash: u64,
    state: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
}

#[derive(Clone)]
pub struct MetalGraphicsShaderStages {
    key: GraphicsPipelineKey,
    vertex: Arc<MetalShaderModule>,
    fragment: Option<Arc<MetalShaderModule>>,
}

impl MetalGraphicsShaderStages {
    pub fn key(&self) -> &GraphicsPipelineKey {
        &self.key
    }

    pub fn variant_hash(&self) -> u64 {
        self.key.hash_value()
    }

    pub fn vertex(&self) -> &MetalShaderModule {
        &self.vertex
    }

    pub fn fragment(&self) -> Option<&MetalShaderModule> {
        self.fragment.as_deref()
    }
}

impl MetalComputePipeline {
    pub fn shader_hash(&self) -> u64 {
        self.shader_hash
    }

    pub fn state(&self) -> &ProtocolObject<dyn MTLComputePipelineState> {
        &self.state
    }
}

#[derive(Debug, Error)]
pub enum MetalPipelineError {
    #[error("expected {expected} shader, got {actual:?}")]
    InvalidShaderStage {
        expected: &'static str,
        actual: ExecutionModel,
    },
    #[error("sample count {0} is not supported by this Metal device")]
    UnsupportedSampleCount(u32),
    #[error("Metal failed to create render pipeline: {0}")]
    RenderPipeline(String),
    #[error("Metal failed to create compute pipeline: {0}")]
    ComputePipeline(String),
    #[error("graphics shader translation did not produce a vertex stage")]
    MissingVertexStage,
    #[error("native Metal pipeline lowering for {0} is not implemented")]
    UnsupportedGraphicsStage(&'static str),
    #[error(transparent)]
    Shader(#[from] MetalShaderError),
}

pub struct MetalPipelineCache {
    device: MetalDevice,
    profile: Profile,
    host_info: HostTranslateInfo,
    render_pipelines: HashMap<MetalRenderPipelineKey, MetalRenderPipeline>,
    compute_pipelines: HashMap<u64, MetalComputePipeline>,
    graphics_shader_modules: HashMap<GraphicsPipelineKey, MetalGraphicsShaderStages>,
    graphics_key: GraphicsPipelineKey,
    dynamic_features: DynamicFeatures,
}

impl MetalPipelineCache {
    pub fn new(device: MetalDevice) -> Self {
        let profile = make_shader_profile(device.profile());
        let host_info = make_host_translate_info(device.profile());
        Self {
            device,
            profile,
            host_info,
            render_pipelines: HashMap::new(),
            compute_pipelines: HashMap::new(),
            graphics_shader_modules: HashMap::new(),
            graphics_key: GraphicsPipelineKey::default(),
            // Metal state not represented by a dynamic encoder command stays
            // in the fixed key. Start with every Vulkan-only dynamic feature
            // disabled, then opt in only when the Metal rasterizer owns it.
            dynamic_features: DynamicFeatures::default(),
        }
    }

    pub fn device(&self) -> &MetalDevice {
        &self.device
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    pub fn host_info(&self) -> &HostTranslateInfo {
        &self.host_info
    }

    /// Port of Eden `PipelineCache::CurrentGraphicsPipeline` up through
    /// shader discovery, translation, and backend module creation.
    pub fn current_graphics_shaders(
        &mut self,
        draw: &mut Maxwell3DDrawView<'_>,
        shared_cache: &mut SharedShaderCache,
    ) -> Result<Option<MetalGraphicsShaderStages>, MetalPipelineError> {
        if !shared_cache.refresh_stages(&mut self.graphics_key.unique_hashes) {
            return Ok(None);
        }
        self.graphics_key
            .fixed_state
            .refresh(draw, &self.dynamic_features);
        let key = self.graphics_key.clone();
        if !self.graphics_shader_modules.contains_key(&key) {
            let mut environments = GraphicsEnvironments::default();
            shared_cache.get_graphics_environments(&mut environments, &key.unique_hashes);
            let Some(compiled) = GraphicsPipelineCache::compile_graphics_stages_from_environments(
                &self.profile,
                &self.host_info,
                &key,
                &mut environments,
            ) else {
                return Ok(None);
            };
            if compiled[1].is_some() {
                return Err(MetalPipelineError::UnsupportedGraphicsStage(
                    "tessellation control",
                ));
            }
            if compiled[2].is_some() {
                return Err(MetalPipelineError::UnsupportedGraphicsStage(
                    "tessellation evaluation",
                ));
            }
            if compiled[3].is_some() {
                return Err(MetalPipelineError::UnsupportedGraphicsStage("geometry"));
            }
            let vertex = compiled[0]
                .as_ref()
                .ok_or(MetalPipelineError::MissingVertexStage)?;
            let vertex = Arc::new(compile_native_shader(
                self.device.device(),
                self.device.profile(),
                &vertex.spirv_words,
                &MetalShaderCompileOptions::default(),
            )?);
            let fragment = compiled[4]
                .as_ref()
                .map(|fragment| {
                    compile_native_shader(
                        self.device.device(),
                        self.device.profile(),
                        &fragment.spirv_words,
                        &MetalShaderCompileOptions::default(),
                    )
                    .map(Arc::new)
                })
                .transpose()?;
            self.graphics_shader_modules.insert(
                key.clone(),
                MetalGraphicsShaderStages {
                    key: key.clone(),
                    vertex,
                    fragment,
                },
            );
        }
        Ok(self.graphics_shader_modules.get(&key).cloned())
    }

    pub fn get_or_create_render_pipeline(
        &mut self,
        key: MetalRenderPipelineKey,
        vertex: &MetalShaderModule,
        fragment: Option<&MetalShaderModule>,
    ) -> Result<&MetalRenderPipeline, MetalPipelineError> {
        if vertex.source().execution_model != ExecutionModel::Vertex {
            return Err(MetalPipelineError::InvalidShaderStage {
                expected: "vertex",
                actual: vertex.source().execution_model,
            });
        }
        if let Some(fragment) = fragment {
            if fragment.source().execution_model != ExecutionModel::Fragment {
                return Err(MetalPipelineError::InvalidShaderStage {
                    expected: "fragment",
                    actual: fragment.source().execution_model,
                });
            }
        }
        if !self
            .device
            .profile()
            .supports_sample_count(key.sample_count)
        {
            return Err(MetalPipelineError::UnsupportedSampleCount(key.sample_count));
        }
        if !self.render_pipelines.contains_key(&key) {
            let descriptor = MTLRenderPipelineDescriptor::new();
            descriptor.setVertexFunction(Some(vertex.function()));
            descriptor.setFragmentFunction(fragment.map(MetalShaderModule::function));
            descriptor.setRasterSampleCount(key.sample_count as usize);
            descriptor.setAlphaToCoverageEnabled(key.alpha_to_coverage);
            descriptor.setAlphaToOneEnabled(key.alpha_to_one);
            descriptor.setRasterizationEnabled(key.rasterization_enabled);
            unsafe {
                descriptor.setInputPrimitiveTopology(key.topology);
            }
            descriptor.setDepthAttachmentPixelFormat(key.depth_format);
            descriptor.setStencilAttachmentPixelFormat(key.stencil_format);

            let attachments = descriptor.colorAttachments();
            for (index, state) in key.color_attachments.iter().enumerate() {
                let attachment = unsafe { attachments.objectAtIndexedSubscript(index) };
                attachment.setPixelFormat(state.format);
                attachment.setBlendingEnabled(state.blending_enabled);
                attachment.setSourceRGBBlendFactor(state.source_rgb);
                attachment.setDestinationRGBBlendFactor(state.destination_rgb);
                attachment.setRgbBlendOperation(state.rgb_operation);
                attachment.setSourceAlphaBlendFactor(state.source_alpha);
                attachment.setDestinationAlphaBlendFactor(state.destination_alpha);
                attachment.setAlphaBlendOperation(state.alpha_operation);
                attachment.setWriteMask(state.write_mask);
            }

            let state = self
                .device
                .device()
                .newRenderPipelineStateWithDescriptor_error(&descriptor)
                .map_err(|error| {
                    MetalPipelineError::RenderPipeline(error.localizedDescription().to_string())
                })?;
            self.render_pipelines
                .insert(key, MetalRenderPipeline { key, state });
        }
        Ok(self
            .render_pipelines
            .get(&key)
            .expect("pipeline inserted above"))
    }

    pub fn get_or_create_compute_pipeline(
        &mut self,
        shader_hash: u64,
        shader: &MetalShaderModule,
    ) -> Result<&MetalComputePipeline, MetalPipelineError> {
        if shader.source().execution_model != ExecutionModel::GLCompute {
            return Err(MetalPipelineError::InvalidShaderStage {
                expected: "compute",
                actual: shader.source().execution_model,
            });
        }
        if !self.compute_pipelines.contains_key(&shader_hash) {
            let state = self
                .device
                .device()
                .newComputePipelineStateWithFunction_error(shader.function())
                .map_err(|error| {
                    MetalPipelineError::ComputePipeline(error.localizedDescription().to_string())
                })?;
            self.compute_pipelines
                .insert(shader_hash, MetalComputePipeline { shader_hash, state });
        }
        Ok(self
            .compute_pipelines
            .get(&shader_hash)
            .expect("pipeline inserted above"))
    }
}

#[cfg(test)]
mod tests {
    use shader_recompiler::backend::emit_spirv;
    use shader_recompiler::ir::basic_block::Block;
    use shader_recompiler::ir::emitter::Emitter;
    use shader_recompiler::ir::value::{Attribute, Value};
    use shader_recompiler::ir::Program;
    use shader_recompiler::runtime_info::RuntimeInfo;
    use shader_recompiler::stage::Stage;

    use super::*;
    use crate::renderer_metal::metal_shader::{compile_native_shader, MetalShaderCompileOptions};

    #[test]
    fn native_profile_matches_direct_metal_binding_model() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let cache = MetalPipelineCache::new(device);

        assert_eq!(cache.profile().supported_spirv, SPIRV_1_5);
        assert!(cache.profile().unified_descriptor_binding);
        assert!(cache.profile().support_vertex_instance_id);
        assert!(!cache.profile().support_native_ndc);
        assert!(!cache.profile().support_int64_atomics);
        assert_eq!(cache.host_info().max_descriptor_set_samplers, 16);
        assert_eq!(cache.host_info().max_descriptor_set_sampled_images, 128);
        assert_eq!(cache.host_info().max_descriptor_set_uniform_buffers, 31);
        assert_eq!(cache.host_info().min_ssbo_alignment, 4);
    }

    fn compile_test_shader(cache: &MetalPipelineCache, program: &Program) -> MetalShaderModule {
        let words = emit_spirv(program, cache.profile(), &RuntimeInfo::default());
        compile_native_shader(
            cache.device().device(),
            cache.device().profile(),
            &words,
            &MetalShaderCompileOptions::default(),
        )
        .expect("test shader must compile")
    }

    fn vertex_program() -> Program {
        let mut program = Program::new(Stage::VertexB);
        program.blocks.push(Block::new());
        for attribute in [
            Attribute::POSITION_X,
            Attribute::POSITION_Y,
            Attribute::POSITION_Z,
            Attribute::POSITION_W,
        ] {
            program.info.stores.set(attribute.0 as usize, true);
        }
        let mut emitter = Emitter::new(&mut program, 0);
        emitter.prologue();
        emitter.set_attribute(Attribute::POSITION_X, Value::ImmF32(0.0), Value::ImmU32(0));
        emitter.set_attribute(Attribute::POSITION_Y, Value::ImmF32(0.0), Value::ImmU32(0));
        emitter.set_attribute(Attribute::POSITION_Z, Value::ImmF32(0.0), Value::ImmU32(0));
        emitter.set_attribute(Attribute::POSITION_W, Value::ImmF32(1.0), Value::ImmU32(0));
        emitter.epilogue();
        program
    }

    fn fragment_program() -> Program {
        let mut program = Program::new(Stage::Fragment);
        program.blocks.push(Block::new());
        program.info.stores_frag_color[0] = true;
        let mut emitter = Emitter::new(&mut program, 0);
        for (component, value) in [1.0, 0.0, 0.0, 1.0].into_iter().enumerate() {
            emitter.set_frag_color(
                Value::ImmU32(0),
                Value::ImmU32(component as u32),
                Value::ImmF32(value),
            );
        }
        emitter.epilogue();
        program
    }

    #[test]
    fn creates_and_caches_native_render_pipeline_from_recompiler_shaders() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut cache = MetalPipelineCache::new(device);
        let vertex = compile_test_shader(&cache, &vertex_program());
        let fragment = compile_test_shader(&cache, &fragment_program());
        let mut key = MetalRenderPipelineKey::new(0x1111, 0x2222);
        key.color_attachments[0].format = MTLPixelFormat::BGRA8Unorm;

        let first = cache
            .get_or_create_render_pipeline(key, &vertex, Some(&fragment))
            .expect("native render pipeline must compile")
            .state() as *const _;
        let second = cache
            .get_or_create_render_pipeline(key, &vertex, Some(&fragment))
            .expect("native render pipeline must be cached")
            .state() as *const _;

        assert_eq!(first, second);
    }

    #[test]
    fn render_pipeline_key_keeps_shader_runtime_variants_distinct() {
        let base = MetalRenderPipelineKey::new(0x1111, 0x2222);
        let mut variant = base;
        variant.shader_variant_hash = 0x3333;

        assert_ne!(base, variant);
    }

    #[test]
    fn creates_and_caches_native_compute_pipeline() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut cache = MetalPipelineCache::new(device);
        let mut program = Program::new(Stage::Compute);
        program.blocks.push(Block::new());
        Emitter::new(&mut program, 0).epilogue();
        let shader = compile_test_shader(&cache, &program);

        let first = cache
            .get_or_create_compute_pipeline(0x3333, &shader)
            .expect("native compute pipeline must compile")
            .state() as *const _;
        let second = cache
            .get_or_create_compute_pipeline(0x3333, &shader)
            .expect("native compute pipeline must be cached")
            .state() as *const _;

        assert_eq!(first, second);
    }
}

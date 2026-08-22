// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Metal shader profile and pipeline-cache ownership.
//!
//! This is the native Metal counterpart of Eden's `vk_pipeline_cache.cpp`.
//! Translation capability policy belongs to the pipeline cache, not to the
//! device wrapper or the rasterizer.

use std::collections::HashMap;
use std::panic::{catch_unwind, resume_unwind, take_hook, AssertUnwindSafe};
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLBlendFactor, MTLBlendOperation, MTLColorWriteMask, MTLCompareFunction,
    MTLComputePipelineState, MTLDepthStencilDescriptor, MTLDepthStencilState, MTLDevice as _,
    MTLPixelFormat, MTLPrimitiveTopologyClass, MTLRenderPipelineDescriptor, MTLRenderPipelineState,
    MTLStencilDescriptor, MTLStencilOperation, MTLVertexDescriptor, MTLVertexFormat,
    MTLVertexStepFunction,
};
use shader_recompiler::host_translate_info::HostTranslateInfo;
use shader_recompiler::profile::Profile;
use shader_recompiler::shader_info::Info as ShaderInfo;
use shader_recompiler::{backend::bindings::Bindings, RuntimeInfo};
use spirv_cross2::spirv::ExecutionModel;
use thiserror::Error;

use crate::buffer_cache::buffer_cache_base::{
    ComputeUniformBufferSizes, UniformBufferSizes, NUM_STAGES,
};
use crate::engines::draw_manager::Maxwell3DDrawView;
use crate::engines::maxwell_3d::{
    BlendEquation, BlendFactor, ComparisonOp, DepthStencilInfo, PrimitiveTopology, StencilOp,
    VertexAttribSize, VertexAttribType,
};
use crate::renderer_vulkan::fixed_pipeline_state::DynamicFeatures;
use crate::renderer_vulkan::fixed_pipeline_state::FixedPipelineState;
use crate::renderer_vulkan::graphics_pipeline::{
    buffer_cache_metadata, stage_infos_from_compiled, GraphicsPipelineCache, GraphicsPipelineKey,
};
use crate::shader_cache::{GraphicsEnvironments, ShaderCache as SharedShaderCache};
use crate::shader_environment::ComputeEnvironment;

use super::metal_device::{MetalDevice, MetalDeviceProfile};
use super::metal_framebuffer::MetalFramebuffer;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MetalVertexAttributeState {
    pub format: MTLVertexFormat,
    pub offset: u16,
    pub buffer_index: u8,
}

impl MetalVertexAttributeState {
    pub const fn disabled() -> Self {
        Self {
            format: MTLVertexFormat::Invalid,
            offset: 0,
            buffer_index: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MetalVertexBufferLayoutState {
    pub stride: u16,
    pub step_function: MTLVertexStepFunction,
    pub step_rate: u32,
    pub buffer_index: u8,
    pub enabled: bool,
}

impl MetalVertexBufferLayoutState {
    pub const fn disabled() -> Self {
        Self {
            stride: 0,
            step_function: MTLVertexStepFunction::PerVertex,
            step_rate: 1,
            buffer_index: 0,
            enabled: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MetalVertexInputState {
    pub attributes: [MetalVertexAttributeState; 32],
    pub layouts: [MetalVertexBufferLayoutState; 32],
}

impl Default for MetalVertexInputState {
    fn default() -> Self {
        Self {
            attributes: [MetalVertexAttributeState::disabled(); 32],
            layouts: [MetalVertexBufferLayoutState::disabled(); 32],
        }
    }
}

impl MetalVertexInputState {
    fn from_fixed_state(
        fixed_state: &FixedPipelineState,
        vertex_info: &ShaderInfo,
        first_vertex_buffer: u32,
        max_buffer_bindings: u32,
    ) -> Result<Self, MetalPipelineError> {
        let mut result = Self::default();
        let mut source_to_metal = [None; 32];
        let mut next_buffer = first_vertex_buffer;
        for (attribute_index, attribute) in fixed_state.attributes.iter().enumerate() {
            if !attribute.is_enabled() || !vertex_info.loads.generic_any(attribute_index) {
                continue;
            }
            let source_buffer = attribute.buffer() as usize;
            let metal_buffer = if let Some(index) = source_to_metal[source_buffer] {
                index
            } else {
                if next_buffer >= max_buffer_bindings {
                    return Err(MetalPipelineError::VertexBufferBindingLimit {
                        requested: next_buffer + 1,
                        limit: max_buffer_bindings,
                    });
                }
                let index = next_buffer as u8;
                source_to_metal[source_buffer] = Some(index);
                next_buffer += 1;
                index
            };
            let attrib_type = VertexAttribType::from_raw(attribute.attrib_type());
            let attrib_size = VertexAttribSize::from_raw(attribute.attrib_size());
            let format = metal_vertex_format(attrib_type, attrib_size).ok_or(
                MetalPipelineError::UnsupportedVertexFormat {
                    attrib_type,
                    attrib_size,
                },
            )?;
            result.attributes[attribute_index] = MetalVertexAttributeState {
                format,
                offset: attribute.offset() as u16,
                buffer_index: metal_buffer,
            };
        }
        for (source_buffer, metal_buffer) in source_to_metal.into_iter().enumerate() {
            let Some(buffer_index) = metal_buffer else {
                continue;
            };
            let divisor = fixed_state.binding_divisors[source_buffer];
            result.layouts[source_buffer] = MetalVertexBufferLayoutState {
                stride: fixed_state.vertex_strides[source_buffer],
                step_function: if divisor == 0 {
                    MTLVertexStepFunction::PerVertex
                } else {
                    MTLVertexStepFunction::PerInstance
                },
                step_rate: divisor.max(1),
                buffer_index,
                enabled: true,
            };
        }
        Ok(result)
    }
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
    pub vertex_input: MetalVertexInputState,
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
            vertex_input: MetalVertexInputState::default(),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MetalStencilFaceState {
    pub compare: MTLCompareFunction,
    pub stencil_fail: MTLStencilOperation,
    pub depth_fail: MTLStencilOperation,
    pub depth_stencil_pass: MTLStencilOperation,
    pub read_mask: u32,
    pub write_mask: u32,
}

impl Default for MetalStencilFaceState {
    fn default() -> Self {
        Self {
            compare: MTLCompareFunction::Always,
            stencil_fail: MTLStencilOperation::Keep,
            depth_fail: MTLStencilOperation::Keep,
            depth_stencil_pass: MTLStencilOperation::Keep,
            read_mask: u32::MAX,
            write_mask: u32::MAX,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MetalDepthStencilKey {
    pub depth_compare: MTLCompareFunction,
    pub depth_write_enabled: bool,
    pub stencil_enabled: bool,
    pub front: MetalStencilFaceState,
    pub back: MetalStencilFaceState,
}

impl MetalDepthStencilKey {
    fn from_fixed_state(fixed: &FixedPipelineState, live: &DepthStencilInfo) -> Self {
        let dynamic = &fixed.dynamic_state;
        let depth_test_enabled = dynamic.depth_test_enable();
        let front = dynamic.front_stencil();
        let back = dynamic.back_stencil();
        let back_live = if live.stencil_two_side {
            live.back
        } else {
            live.front
        };
        Self {
            // Metal has no separate depth-test enable. `Always` plus disabled
            // writes is the exact disabled-test behavior.
            depth_compare: if depth_test_enabled {
                metal_comparison(dynamic.depth_test_func())
            } else {
                MTLCompareFunction::Always
            },
            depth_write_enabled: depth_test_enabled && dynamic.depth_write_enable(),
            stencil_enabled: dynamic.stencil_enable(),
            front: MetalStencilFaceState {
                compare: metal_comparison(front.test_func(dynamic.raw2)),
                stencil_fail: metal_stencil_operation(front.action_stencil_fail(dynamic.raw2)),
                depth_fail: metal_stencil_operation(front.action_depth_fail(dynamic.raw2)),
                depth_stencil_pass: metal_stencil_operation(front.action_depth_pass(dynamic.raw2)),
                read_mask: live.front.func_mask,
                write_mask: live.front.write_mask,
            },
            back: MetalStencilFaceState {
                compare: metal_comparison(back.test_func(dynamic.raw2)),
                stencil_fail: metal_stencil_operation(back.action_stencil_fail(dynamic.raw2)),
                depth_fail: metal_stencil_operation(back.action_depth_fail(dynamic.raw2)),
                depth_stencil_pass: metal_stencil_operation(back.action_depth_pass(dynamic.raw2)),
                read_mask: back_live.func_mask,
                write_mask: back_live.write_mask,
            },
        }
    }
}

impl MetalRenderPipeline {
    pub fn key(&self) -> &MetalRenderPipelineKey {
        &self.key
    }

    pub fn state(&self) -> &ProtocolObject<dyn MTLRenderPipelineState> {
        &self.state
    }

    pub fn retained_state(&self) -> Retained<ProtocolObject<dyn MTLRenderPipelineState>> {
        self.state.clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MetalComputePipelineKey {
    pub unique_hash: u64,
    pub shared_memory_size: u32,
    pub workgroup_size: [u32; 3],
}

#[derive(Clone)]
pub struct MetalComputePipeline {
    key: MetalComputePipelineKey,
    info: Arc<ShaderInfo>,
    uniform_buffer_sizes: Arc<ComputeUniformBufferSizes>,
    shader: Arc<MetalShaderModule>,
    state: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
}

#[derive(Clone)]
pub struct MetalGraphicsShaderStages {
    key: GraphicsPipelineKey,
    vertex: Arc<MetalShaderModule>,
    fragment: Option<Arc<MetalShaderModule>>,
    stage_infos: Arc<[ShaderInfo; 5]>,
    enabled_uniform_buffer_masks: [u32; NUM_STAGES as usize],
    uniform_buffer_sizes: Arc<UniformBufferSizes>,
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

    pub fn stage_infos(&self) -> &[ShaderInfo; 5] {
        &self.stage_infos
    }

    pub fn enabled_uniform_buffer_masks(&self) -> &[u32; NUM_STAGES as usize] {
        &self.enabled_uniform_buffer_masks
    }

    pub fn uniform_buffer_sizes(&self) -> &UniformBufferSizes {
        &self.uniform_buffer_sizes
    }
}

impl MetalComputePipeline {
    pub fn shader_hash(&self) -> u64 {
        self.key.unique_hash
    }

    pub fn key(&self) -> MetalComputePipelineKey {
        self.key
    }

    pub fn info(&self) -> &ShaderInfo {
        &self.info
    }

    pub fn uniform_buffer_sizes(&self) -> &ComputeUniformBufferSizes {
        &self.uniform_buffer_sizes
    }

    pub fn shader(&self) -> &MetalShaderModule {
        &self.shader
    }

    pub fn state(&self) -> &ProtocolObject<dyn MTLComputePipelineState> {
        &self.state
    }

    pub fn retained_state(&self) -> Retained<ProtocolObject<dyn MTLComputePipelineState>> {
        self.state.clone()
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
    #[error("shader translation failed: {0}")]
    ShaderTranslation(String),
    #[error("Metal failed to create a depth/stencil state")]
    DepthStencilState,
    #[error("graphics shader translation did not produce a vertex stage")]
    MissingVertexStage,
    #[error("native Metal pipeline lowering for {0} is not implemented")]
    UnsupportedGraphicsStage(&'static str),
    #[error("Metal has no native vertex format for {attrib_type:?} {attrib_size:?}")]
    UnsupportedVertexFormat {
        attrib_type: VertexAttribType,
        attrib_size: VertexAttribSize,
    },
    #[error("Metal vertex buffer binding limit exceeded: requested {requested}, limit {limit}")]
    VertexBufferBindingLimit { requested: u32, limit: u32 },
    #[error(transparent)]
    Shader(#[from] MetalShaderError),
}

static SHADER_EXCEPTION_HOOK_INSTALL: std::sync::Once = std::sync::Once::new();

thread_local! {
    static IN_SHADER_EXCEPTION_SCOPE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn shader_exception_message(payload: &(dyn std::any::Any + Send)) -> Option<String> {
    use shader_recompiler::exception::{
        InvalidArgument, LogicError, NotImplementedException, RuntimeError, ShaderException,
    };

    if let Some(error) = payload.downcast_ref::<ShaderException>() {
        Some(error.to_string())
    } else if let Some(error) = payload.downcast_ref::<LogicError>() {
        Some(error.to_string())
    } else if let Some(error) = payload.downcast_ref::<RuntimeError>() {
        Some(error.to_string())
    } else if let Some(error) = payload.downcast_ref::<NotImplementedException>() {
        Some(error.to_string())
    } else {
        payload
            .downcast_ref::<InvalidArgument>()
            .map(ToString::to_string)
    }
}

/// Rust equivalent of Eden's typed `Shader::Exception` catches. Panics that
/// are not shader compiler exceptions remain fatal and are resumed unchanged.
fn catch_shader_exception<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce() -> T,
{
    SHADER_EXCEPTION_HOOK_INSTALL.call_once(|| {
        let previous = take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let is_shader_exception = shader_exception_message(info.payload()).is_some();
            if !IN_SHADER_EXCEPTION_SCOPE.with(std::cell::Cell::get) || !is_shader_exception {
                previous(info);
            }
        }));
    });

    IN_SHADER_EXCEPTION_SCOPE.with(|flag| flag.set(true));
    let result = catch_unwind(AssertUnwindSafe(f));
    IN_SHADER_EXCEPTION_SCOPE.with(|flag| flag.set(false));
    match result {
        Ok(value) => Ok(value),
        Err(payload) => match shader_exception_message(payload.as_ref()) {
            Some(message) => Err(message),
            None => resume_unwind(payload),
        },
    }
}

pub struct MetalPipelineCache {
    device: MetalDevice,
    profile: Profile,
    host_info: HostTranslateInfo,
    render_pipelines: HashMap<MetalRenderPipelineKey, MetalRenderPipeline>,
    depth_stencil_states:
        HashMap<MetalDepthStencilKey, Retained<ProtocolObject<dyn MTLDepthStencilState>>>,
    compute_pipelines: HashMap<MetalComputePipelineKey, MetalComputePipeline>,
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
            depth_stencil_states: HashMap::new(),
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
            let stage_infos = stage_infos_from_compiled(&compiled);
            let (enabled_uniform_buffer_masks, uniform_buffer_sizes) =
                buffer_cache_metadata(&stage_infos);
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
                    stage_infos: Arc::new(stage_infos),
                    enabled_uniform_buffer_masks,
                    uniform_buffer_sizes: Arc::new(uniform_buffer_sizes),
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
            let vertex_descriptor = make_vertex_descriptor(&key.vertex_input);
            descriptor.setVertexDescriptor(Some(&vertex_descriptor));

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

    pub fn make_vertex_input_state(
        &self,
        stages: &MetalGraphicsShaderStages,
    ) -> Result<MetalVertexInputState, MetalPipelineError> {
        MetalVertexInputState::from_fixed_state(
            &stages.key.fixed_state,
            &stages.stage_infos[0],
            stages.vertex.bindings().buffer_count,
            self.device.profile().max_buffer_bindings_per_stage,
        )
    }

    pub fn make_render_pipeline_key(
        &self,
        stages: &MetalGraphicsShaderStages,
        framebuffer: &MetalFramebuffer,
    ) -> Result<MetalRenderPipelineKey, MetalPipelineError> {
        let fixed = &stages.key.fixed_state;
        let mut key =
            MetalRenderPipelineKey::new(stages.key.unique_hashes[1], stages.key.unique_hashes[5]);
        key.shader_variant_hash = stages.variant_hash();
        key.vertex_input = self.make_vertex_input_state(stages)?;
        let color_formats = framebuffer.color_formats();
        key.color_attachments = std::array::from_fn(|index| {
            metal_color_attachment(fixed.attachments[index], color_formats[index])
        });
        key.depth_format = framebuffer.depth_format();
        key.stencil_format = framebuffer.stencil_format();
        key.sample_count = framebuffer.samples();
        key.topology = metal_topology_class(fixed.topology());
        key.alpha_to_coverage = fixed.alpha_to_coverage_enabled();
        key.alpha_to_one = fixed.alpha_to_one_enabled();
        key.rasterization_enabled = fixed.dynamic_state.rasterize_enable();
        Ok(key)
    }

    pub fn make_depth_stencil_key(
        &self,
        stages: &MetalGraphicsShaderStages,
        live: &DepthStencilInfo,
    ) -> MetalDepthStencilKey {
        MetalDepthStencilKey::from_fixed_state(&stages.key.fixed_state, live)
    }

    pub fn get_or_create_depth_stencil_state(
        &mut self,
        key: MetalDepthStencilKey,
    ) -> Result<&ProtocolObject<dyn MTLDepthStencilState>, MetalPipelineError> {
        if !self.depth_stencil_states.contains_key(&key) {
            let descriptor = MTLDepthStencilDescriptor::new();
            descriptor.setDepthCompareFunction(key.depth_compare);
            descriptor.setDepthWriteEnabled(key.depth_write_enabled);
            if key.stencil_enabled {
                let front = make_stencil_descriptor(key.front);
                let back = make_stencil_descriptor(key.back);
                descriptor.setFrontFaceStencil(Some(&front));
                descriptor.setBackFaceStencil(Some(&back));
            }
            let state = self
                .device
                .device()
                .newDepthStencilStateWithDescriptor(&descriptor)
                .ok_or(MetalPipelineError::DepthStencilState)?;
            self.depth_stencil_states.insert(key, state);
        }
        Ok(self
            .depth_stencil_states
            .get(&key)
            .expect("depth/stencil state inserted above"))
    }

    pub fn retained_depth_stencil_state(
        &mut self,
        key: MetalDepthStencilKey,
    ) -> Result<Retained<ProtocolObject<dyn MTLDepthStencilState>>, MetalPipelineError> {
        self.get_or_create_depth_stencil_state(key)?;
        Ok(self
            .depth_stencil_states
            .get(&key)
            .expect("depth/stencil state inserted above")
            .clone())
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
        let key = MetalComputePipelineKey {
            unique_hash: shader_hash,
            shared_memory_size: 0,
            workgroup_size: [1; 3],
        };
        if !self.compute_pipelines.contains_key(&key) {
            let state = self
                .device
                .device()
                .newComputePipelineStateWithFunction_error(shader.function())
                .map_err(|error| {
                    MetalPipelineError::ComputePipeline(error.localizedDescription().to_string())
                })?;
            self.compute_pipelines.insert(
                key,
                MetalComputePipeline {
                    key,
                    info: Arc::new(ShaderInfo::default()),
                    uniform_buffer_sizes: Arc::new([0; 8]),
                    shader: Arc::new(shader.clone()),
                    state,
                },
            );
        }
        Ok(self
            .compute_pipelines
            .get(&key)
            .expect("pipeline inserted above"))
    }

    /// Port of Eden `PipelineCache::CurrentComputePipeline` through native
    /// MSL module and `MTLComputePipelineState` creation.
    pub fn current_compute_pipeline(
        &mut self,
        shared_cache: &mut SharedShaderCache,
    ) -> Result<Option<MetalComputePipeline>, MetalPipelineError> {
        let (unique_hash, shader_size) = {
            let Some(shader) = shared_cache.compute_shader() else {
                return Ok(None);
            };
            (shader.unique_hash, shader.size_bytes)
        };
        let Some(kepler_compute) = shared_cache.current_kepler_compute() else {
            return Ok(None);
        };
        let qmd = kepler_compute.launch_description();
        let key = MetalComputePipelineKey {
            unique_hash,
            shared_memory_size: qmd.shared_alloc,
            workgroup_size: [qmd.block_dim_x, qmd.block_dim_y, qmd.block_dim_z],
        };
        if !self.compute_pipelines.contains_key(&key) {
            let Some(gpu_memory) = shared_cache.current_gpu_memory() else {
                return Ok(None);
            };
            let mut environment =
                ComputeEnvironment::from_kepler_compute(kepler_compute, gpu_memory);
            environment
                .generic_environment_mut()
                .set_cached_size(shader_size);
            let code = environment
                .generic_environment()
                .cached_instruction_slice()
                .to_vec();
            let base_offset = environment.generic_environment().cached_instruction_start();
            if code.is_empty() {
                return Ok(None);
            }

            let (program, spirv_words) = catch_shader_exception(|| {
                let runtime_info = RuntimeInfo::default();
                let mut program =
                    shader_recompiler::pipeline_cache::translate_program_from_env_with_host_info(
                        &code,
                        base_offset,
                        &mut environment,
                        &self.host_info,
                    );
                shader_recompiler::frontend::translate_program::convert_legacy_to_generic(
                    &mut program,
                    &runtime_info,
                );
                let mut bindings = Bindings::default();
                let spirv_words = shader_recompiler::backend::emit_spirv_with_bindings(
                    &program,
                    &self.profile,
                    &runtime_info,
                    &mut bindings,
                );
                (program, spirv_words)
            })
            .map_err(MetalPipelineError::ShaderTranslation)?;
            let info = Arc::new(program.info);
            let shader = Arc::new(compile_native_shader(
                self.device.device(),
                self.device.profile(),
                &spirv_words,
                &MetalShaderCompileOptions::default(),
            )?);
            let state = self
                .device
                .device()
                .newComputePipelineStateWithFunction_error(shader.function())
                .map_err(|error| {
                    MetalPipelineError::ComputePipeline(error.localizedDescription().to_string())
                })?;
            let mut uniform_buffer_sizes = [0; 8];
            uniform_buffer_sizes.copy_from_slice(&info.constant_buffer_used_sizes[..8]);
            self.compute_pipelines.insert(
                key,
                MetalComputePipeline {
                    key,
                    info,
                    uniform_buffer_sizes: Arc::new(uniform_buffer_sizes),
                    shader,
                    state,
                },
            );
        }
        Ok(self.compute_pipelines.get(&key).cloned())
    }
}

fn make_stencil_descriptor(state: MetalStencilFaceState) -> Retained<MTLStencilDescriptor> {
    let descriptor = MTLStencilDescriptor::new();
    descriptor.setStencilCompareFunction(state.compare);
    descriptor.setStencilFailureOperation(state.stencil_fail);
    descriptor.setDepthFailureOperation(state.depth_fail);
    descriptor.setDepthStencilPassOperation(state.depth_stencil_pass);
    descriptor.setReadMask(state.read_mask);
    descriptor.setWriteMask(state.write_mask);
    descriptor
}

fn metal_comparison(comparison: ComparisonOp) -> MTLCompareFunction {
    match comparison {
        ComparisonOp::Never => MTLCompareFunction::Never,
        ComparisonOp::Less => MTLCompareFunction::Less,
        ComparisonOp::Equal => MTLCompareFunction::Equal,
        ComparisonOp::LessEqual => MTLCompareFunction::LessEqual,
        ComparisonOp::Greater => MTLCompareFunction::Greater,
        ComparisonOp::NotEqual => MTLCompareFunction::NotEqual,
        ComparisonOp::GreaterEqual => MTLCompareFunction::GreaterEqual,
        ComparisonOp::Always => MTLCompareFunction::Always,
    }
}

fn metal_stencil_operation(operation: StencilOp) -> MTLStencilOperation {
    match operation {
        StencilOp::Keep => MTLStencilOperation::Keep,
        StencilOp::Zero => MTLStencilOperation::Zero,
        StencilOp::Replace => MTLStencilOperation::Replace,
        StencilOp::IncrSat => MTLStencilOperation::IncrementClamp,
        StencilOp::DecrSat => MTLStencilOperation::DecrementClamp,
        StencilOp::Invert => MTLStencilOperation::Invert,
        StencilOp::Incr => MTLStencilOperation::IncrementWrap,
        StencilOp::Decr => MTLStencilOperation::DecrementWrap,
    }
}

fn make_vertex_descriptor(state: &MetalVertexInputState) -> Retained<MTLVertexDescriptor> {
    let descriptor = MTLVertexDescriptor::vertexDescriptor();
    let attributes = descriptor.attributes();
    for (index, attribute) in state.attributes.iter().enumerate() {
        if attribute.format == MTLVertexFormat::Invalid {
            continue;
        }
        let target = unsafe { attributes.objectAtIndexedSubscript(index) };
        target.setFormat(attribute.format);
        unsafe {
            target.setOffset(attribute.offset as usize);
            target.setBufferIndex(attribute.buffer_index as usize);
        }
    }
    let layouts = descriptor.layouts();
    for layout in state.layouts.iter().filter(|layout| layout.enabled) {
        let target = unsafe { layouts.objectAtIndexedSubscript(layout.buffer_index as usize) };
        unsafe {
            target.setStride(layout.stride as usize);
            target.setStepRate(layout.step_rate as usize);
        }
        target.setStepFunction(layout.step_function);
    }
    descriptor
}

fn metal_color_attachment(
    attachment: crate::renderer_vulkan::fixed_pipeline_state::BlendingAttachment,
    format: MTLPixelFormat,
) -> MetalColorAttachmentState {
    let [red, green, blue, alpha] = attachment.mask();
    let mut write_mask = MTLColorWriteMask::None;
    write_mask.set(MTLColorWriteMask::Red, red);
    write_mask.set(MTLColorWriteMask::Green, green);
    write_mask.set(MTLColorWriteMask::Blue, blue);
    write_mask.set(MTLColorWriteMask::Alpha, alpha);
    MetalColorAttachmentState {
        format,
        blending_enabled: attachment.is_enabled(),
        source_rgb: metal_blend_factor(attachment.source_rgb_factor()),
        destination_rgb: metal_blend_factor(attachment.dest_rgb_factor()),
        rgb_operation: metal_blend_operation(attachment.equation_rgb()),
        source_alpha: metal_blend_factor(attachment.source_alpha_factor()),
        destination_alpha: metal_blend_factor(attachment.dest_alpha_factor()),
        alpha_operation: metal_blend_operation(attachment.equation_alpha()),
        write_mask,
    }
}

fn metal_blend_operation(equation: BlendEquation) -> MTLBlendOperation {
    match equation {
        BlendEquation::Add => MTLBlendOperation::Add,
        BlendEquation::Subtract => MTLBlendOperation::Subtract,
        BlendEquation::ReverseSubtract => MTLBlendOperation::ReverseSubtract,
        BlendEquation::Min => MTLBlendOperation::Min,
        BlendEquation::Max => MTLBlendOperation::Max,
    }
}

fn metal_blend_factor(factor: BlendFactor) -> MTLBlendFactor {
    match factor {
        BlendFactor::Zero => MTLBlendFactor::Zero,
        BlendFactor::One => MTLBlendFactor::One,
        BlendFactor::SrcColor => MTLBlendFactor::SourceColor,
        BlendFactor::OneMinusSrcColor => MTLBlendFactor::OneMinusSourceColor,
        BlendFactor::SrcAlpha => MTLBlendFactor::SourceAlpha,
        BlendFactor::OneMinusSrcAlpha => MTLBlendFactor::OneMinusSourceAlpha,
        BlendFactor::DstAlpha => MTLBlendFactor::DestinationAlpha,
        BlendFactor::OneMinusDstAlpha => MTLBlendFactor::OneMinusDestinationAlpha,
        BlendFactor::DstColor => MTLBlendFactor::DestinationColor,
        BlendFactor::OneMinusDstColor => MTLBlendFactor::OneMinusDestinationColor,
        BlendFactor::SrcAlphaSaturate => MTLBlendFactor::SourceAlphaSaturated,
        BlendFactor::Src1Color => MTLBlendFactor::Source1Color,
        BlendFactor::OneMinusSrc1Color => MTLBlendFactor::OneMinusSource1Color,
        BlendFactor::Src1Alpha => MTLBlendFactor::Source1Alpha,
        BlendFactor::OneMinusSrc1Alpha => MTLBlendFactor::OneMinusSource1Alpha,
        BlendFactor::ConstantColor => MTLBlendFactor::BlendColor,
        BlendFactor::OneMinusConstantColor => MTLBlendFactor::OneMinusBlendColor,
        BlendFactor::ConstantAlpha => MTLBlendFactor::BlendAlpha,
        BlendFactor::OneMinusConstantAlpha => MTLBlendFactor::OneMinusBlendAlpha,
    }
}

fn metal_topology_class(topology: PrimitiveTopology) -> MTLPrimitiveTopologyClass {
    match topology {
        PrimitiveTopology::Points | PrimitiveTopology::Patches => MTLPrimitiveTopologyClass::Point,
        PrimitiveTopology::Lines
        | PrimitiveTopology::LineLoop
        | PrimitiveTopology::LineStrip
        | PrimitiveTopology::LinesAdjacency
        | PrimitiveTopology::LineStripAdjacency => MTLPrimitiveTopologyClass::Line,
        PrimitiveTopology::Triangles
        | PrimitiveTopology::TriangleStrip
        | PrimitiveTopology::TriangleFan
        | PrimitiveTopology::Quads
        | PrimitiveTopology::QuadStrip
        | PrimitiveTopology::Polygon
        | PrimitiveTopology::TrianglesAdjacency
        | PrimitiveTopology::TriangleStripAdjacency => MTLPrimitiveTopologyClass::Triangle,
    }
}

fn metal_vertex_format(
    mut attrib_type: VertexAttribType,
    size: VertexAttribSize,
) -> Option<MTLVertexFormat> {
    if attrib_type == VertexAttribType::UScaled {
        attrib_type = VertexAttribType::UInt;
    } else if attrib_type == VertexAttribType::SScaled {
        attrib_type = VertexAttribType::SInt;
    }
    let format = match (attrib_type, size) {
        (VertexAttribType::UNorm, VertexAttribSize::R8 | VertexAttribSize::A8) => {
            MTLVertexFormat::UCharNormalized
        }
        (VertexAttribType::UNorm, VertexAttribSize::R8G8 | VertexAttribSize::G8R8) => {
            MTLVertexFormat::UChar2Normalized
        }
        (VertexAttribType::UNorm, VertexAttribSize::R8G8B8) => MTLVertexFormat::UChar3Normalized,
        (VertexAttribType::UNorm, VertexAttribSize::R8G8B8A8 | VertexAttribSize::X8B8G8R8) => {
            MTLVertexFormat::UChar4Normalized
        }
        (VertexAttribType::UNorm, VertexAttribSize::R16) => MTLVertexFormat::UShortNormalized,
        (VertexAttribType::UNorm, VertexAttribSize::R16G16) => MTLVertexFormat::UShort2Normalized,
        (VertexAttribType::UNorm, VertexAttribSize::R16G16B16) => {
            MTLVertexFormat::UShort3Normalized
        }
        (VertexAttribType::UNorm, VertexAttribSize::R16G16B16A16) => {
            MTLVertexFormat::UShort4Normalized
        }
        (VertexAttribType::UNorm, VertexAttribSize::A2B10G10R10) => {
            MTLVertexFormat::UInt1010102Normalized
        }
        (VertexAttribType::SNorm, VertexAttribSize::R8 | VertexAttribSize::A8) => {
            MTLVertexFormat::CharNormalized
        }
        (VertexAttribType::SNorm, VertexAttribSize::R8G8 | VertexAttribSize::G8R8) => {
            MTLVertexFormat::Char2Normalized
        }
        (VertexAttribType::SNorm, VertexAttribSize::R8G8B8) => MTLVertexFormat::Char3Normalized,
        (VertexAttribType::SNorm, VertexAttribSize::R8G8B8A8 | VertexAttribSize::X8B8G8R8) => {
            MTLVertexFormat::Char4Normalized
        }
        (VertexAttribType::SNorm, VertexAttribSize::R16) => MTLVertexFormat::ShortNormalized,
        (VertexAttribType::SNorm, VertexAttribSize::R16G16) => MTLVertexFormat::Short2Normalized,
        (VertexAttribType::SNorm, VertexAttribSize::R16G16B16) => MTLVertexFormat::Short3Normalized,
        (VertexAttribType::SNorm, VertexAttribSize::R16G16B16A16) => {
            MTLVertexFormat::Short4Normalized
        }
        (VertexAttribType::SNorm, VertexAttribSize::A2B10G10R10) => {
            MTLVertexFormat::Int1010102Normalized
        }
        (VertexAttribType::UInt, VertexAttribSize::R8 | VertexAttribSize::A8) => {
            MTLVertexFormat::UChar
        }
        (VertexAttribType::UInt, VertexAttribSize::R8G8 | VertexAttribSize::G8R8) => {
            MTLVertexFormat::UChar2
        }
        (VertexAttribType::UInt, VertexAttribSize::R8G8B8) => MTLVertexFormat::UChar3,
        (VertexAttribType::UInt, VertexAttribSize::R8G8B8A8 | VertexAttribSize::X8B8G8R8) => {
            MTLVertexFormat::UChar4
        }
        (VertexAttribType::UInt, VertexAttribSize::R16) => MTLVertexFormat::UShort,
        (VertexAttribType::UInt, VertexAttribSize::R16G16) => MTLVertexFormat::UShort2,
        (VertexAttribType::UInt, VertexAttribSize::R16G16B16) => MTLVertexFormat::UShort3,
        (VertexAttribType::UInt, VertexAttribSize::R16G16B16A16) => MTLVertexFormat::UShort4,
        (VertexAttribType::UInt, VertexAttribSize::R32) => MTLVertexFormat::UInt,
        (VertexAttribType::UInt, VertexAttribSize::R32G32) => MTLVertexFormat::UInt2,
        (VertexAttribType::UInt, VertexAttribSize::R32G32B32) => MTLVertexFormat::UInt3,
        (VertexAttribType::UInt, VertexAttribSize::R32G32B32A32) => MTLVertexFormat::UInt4,
        (VertexAttribType::SInt, VertexAttribSize::R8 | VertexAttribSize::A8) => {
            MTLVertexFormat::Char
        }
        (VertexAttribType::SInt, VertexAttribSize::R8G8 | VertexAttribSize::G8R8) => {
            MTLVertexFormat::Char2
        }
        (VertexAttribType::SInt, VertexAttribSize::R8G8B8) => MTLVertexFormat::Char3,
        (VertexAttribType::SInt, VertexAttribSize::R8G8B8A8 | VertexAttribSize::X8B8G8R8) => {
            MTLVertexFormat::Char4
        }
        (VertexAttribType::SInt, VertexAttribSize::R16) => MTLVertexFormat::Short,
        (VertexAttribType::SInt, VertexAttribSize::R16G16) => MTLVertexFormat::Short2,
        (VertexAttribType::SInt, VertexAttribSize::R16G16B16) => MTLVertexFormat::Short3,
        (VertexAttribType::SInt, VertexAttribSize::R16G16B16A16) => MTLVertexFormat::Short4,
        (VertexAttribType::SInt, VertexAttribSize::R32) => MTLVertexFormat::Int,
        (VertexAttribType::SInt, VertexAttribSize::R32G32) => MTLVertexFormat::Int2,
        (VertexAttribType::SInt, VertexAttribSize::R32G32B32) => MTLVertexFormat::Int3,
        (VertexAttribType::SInt, VertexAttribSize::R32G32B32A32) => MTLVertexFormat::Int4,
        (VertexAttribType::Float, VertexAttribSize::R16) => MTLVertexFormat::Half,
        (VertexAttribType::Float, VertexAttribSize::R16G16) => MTLVertexFormat::Half2,
        (VertexAttribType::Float, VertexAttribSize::R16G16B16) => MTLVertexFormat::Half3,
        (VertexAttribType::Float, VertexAttribSize::R16G16B16A16) => MTLVertexFormat::Half4,
        (VertexAttribType::Float, VertexAttribSize::R32) => MTLVertexFormat::Float,
        (VertexAttribType::Float, VertexAttribSize::R32G32) => MTLVertexFormat::Float2,
        (VertexAttribType::Float, VertexAttribSize::R32G32B32) => MTLVertexFormat::Float3,
        (VertexAttribType::Float, VertexAttribSize::R32G32B32A32) => MTLVertexFormat::Float4,
        (VertexAttribType::Float, VertexAttribSize::B10G11R11) => MTLVertexFormat::FloatRG11B10,
        _ => return None,
    };
    Some(format)
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

    fn enable_vertex_attribute(
        state: &mut FixedPipelineState,
        info: &mut ShaderInfo,
        attribute: usize,
        source_buffer: u32,
        offset: u32,
        attrib_type: VertexAttribType,
        size: VertexAttribSize,
    ) {
        let target = &mut state.attributes[attribute];
        target.set_enabled(true);
        target.set_buffer(source_buffer);
        target.set_offset(offset);
        target.set_type(attrib_type.to_raw());
        target.set_size(size.to_raw());
        info.loads.set(
            shader_recompiler::ir::value::Attribute::generic(attribute as u32, 0).0 as usize,
            true,
        );
    }

    #[test]
    fn vertex_input_compacts_used_streams_after_shader_buffers() {
        let mut state = FixedPipelineState::default();
        let mut info = ShaderInfo::default();
        state.vertex_strides[7] = 24;
        state.binding_divisors[7] = 3;
        enable_vertex_attribute(
            &mut state,
            &mut info,
            3,
            7,
            8,
            VertexAttribType::Float,
            VertexAttribSize::R32G32,
        );

        let vertex = MetalVertexInputState::from_fixed_state(&state, &info, 4, 31).unwrap();

        assert_eq!(vertex.attributes[3].format, MTLVertexFormat::Float2);
        assert_eq!(vertex.attributes[3].offset, 8);
        assert_eq!(vertex.attributes[3].buffer_index, 4);
        assert_eq!(vertex.layouts[7].buffer_index, 4);
        assert_eq!(vertex.layouts[7].stride, 24);
        assert_eq!(
            vertex.layouts[7].step_function,
            MTLVertexStepFunction::PerInstance
        );
        assert_eq!(vertex.layouts[7].step_rate, 3);
    }

    #[test]
    fn vertex_input_reuses_one_metal_slot_for_shared_source_stream() {
        let mut state = FixedPipelineState::default();
        let mut info = ShaderInfo::default();
        enable_vertex_attribute(
            &mut state,
            &mut info,
            1,
            5,
            0,
            VertexAttribType::UNorm,
            VertexAttribSize::R8G8B8A8,
        );
        enable_vertex_attribute(
            &mut state,
            &mut info,
            9,
            5,
            4,
            VertexAttribType::Float,
            VertexAttribSize::R32,
        );

        let vertex = MetalVertexInputState::from_fixed_state(&state, &info, 6, 31).unwrap();

        assert_eq!(vertex.attributes[1].buffer_index, 6);
        assert_eq!(vertex.attributes[9].buffer_index, 6);
        assert_eq!(
            vertex
                .layouts
                .iter()
                .filter(|layout| layout.enabled)
                .count(),
            1
        );
    }

    #[test]
    fn vertex_input_rejects_shader_and_vertex_buffer_namespace_overflow() {
        let mut state = FixedPipelineState::default();
        let mut info = ShaderInfo::default();
        enable_vertex_attribute(
            &mut state,
            &mut info,
            0,
            0,
            0,
            VertexAttribType::Float,
            VertexAttribSize::R32,
        );

        let error = MetalVertexInputState::from_fixed_state(&state, &info, 31, 31)
            .expect_err("vertex buffers must not overlap Metal's binding limit");
        assert!(matches!(
            error,
            MetalPipelineError::VertexBufferBindingLimit {
                requested: 32,
                limit: 31
            }
        ));
    }

    #[test]
    fn color_attachment_preserves_maxwell_blend_and_write_mask() {
        let mut attachment =
            crate::renderer_vulkan::fixed_pipeline_state::BlendingAttachment::default();
        attachment.set_mask(true, false, true, false);
        attachment.set_enabled(true);
        attachment.set_equation_rgb(BlendEquation::ReverseSubtract);
        attachment.set_equation_alpha(BlendEquation::Max);
        attachment.set_source_rgb_factor(BlendFactor::SrcAlpha);
        attachment.set_dest_rgb_factor(BlendFactor::OneMinusSrcAlpha);
        attachment.set_source_alpha_factor(BlendFactor::ConstantAlpha);
        attachment.set_dest_alpha_factor(BlendFactor::OneMinusConstantAlpha);

        let metal = metal_color_attachment(attachment, MTLPixelFormat::RGBA8Unorm);

        assert_eq!(metal.format, MTLPixelFormat::RGBA8Unorm);
        assert!(metal.blending_enabled);
        assert_eq!(metal.rgb_operation, MTLBlendOperation::ReverseSubtract);
        assert_eq!(metal.alpha_operation, MTLBlendOperation::Max);
        assert_eq!(metal.source_rgb, MTLBlendFactor::SourceAlpha);
        assert_eq!(metal.destination_rgb, MTLBlendFactor::OneMinusSourceAlpha);
        assert_eq!(metal.source_alpha, MTLBlendFactor::BlendAlpha);
        assert_eq!(metal.destination_alpha, MTLBlendFactor::OneMinusBlendAlpha);
        assert_eq!(
            metal.write_mask,
            MTLColorWriteMask::Red | MTLColorWriteMask::Blue
        );
    }

    #[test]
    fn depth_stencil_key_preserves_masks_and_two_sided_operations() {
        let mut fixed = FixedPipelineState::default();
        fixed.dynamic_state.set_depth_test_enable(true);
        fixed.dynamic_state.set_depth_write_enable(true);
        fixed
            .dynamic_state
            .set_depth_test_func(ComparisonOp::GreaterEqual);
        fixed.dynamic_state.set_stencil_enable(true);
        fixed.dynamic_state.set_stencil_face(
            0,
            StencilOp::Replace,
            StencilOp::IncrSat,
            StencilOp::Decr,
            ComparisonOp::Less,
        );
        fixed.dynamic_state.set_stencil_face(
            12,
            StencilOp::Zero,
            StencilOp::Invert,
            StencilOp::Incr,
            ComparisonOp::NotEqual,
        );
        let mut live = DepthStencilInfo::default();
        live.stencil_two_side = true;
        live.front.func_mask = 0x12;
        live.front.write_mask = 0x34;
        live.back.func_mask = 0x56;
        live.back.write_mask = 0x78;

        let key = MetalDepthStencilKey::from_fixed_state(&fixed, &live);

        assert_eq!(key.depth_compare, MTLCompareFunction::GreaterEqual);
        assert!(key.depth_write_enabled);
        assert!(key.stencil_enabled);
        assert_eq!(key.front.compare, MTLCompareFunction::Less);
        assert_eq!(key.front.stencil_fail, MTLStencilOperation::Replace);
        assert_eq!(key.front.depth_fail, MTLStencilOperation::IncrementClamp);
        assert_eq!(
            key.front.depth_stencil_pass,
            MTLStencilOperation::DecrementWrap
        );
        assert_eq!((key.front.read_mask, key.front.write_mask), (0x12, 0x34));
        assert_eq!(key.back.compare, MTLCompareFunction::NotEqual);
        assert_eq!(key.back.stencil_fail, MTLStencilOperation::Zero);
        assert_eq!(key.back.depth_fail, MTLStencilOperation::Invert);
        assert_eq!(
            key.back.depth_stencil_pass,
            MTLStencilOperation::IncrementWrap
        );
        assert_eq!((key.back.read_mask, key.back.write_mask), (0x56, 0x78));
    }

    #[test]
    fn disabled_depth_test_cannot_write_depth_on_metal() {
        let mut fixed = FixedPipelineState::default();
        fixed.dynamic_state.set_depth_test_enable(false);
        fixed.dynamic_state.set_depth_write_enable(true);
        fixed.dynamic_state.set_depth_test_func(ComparisonOp::Never);

        let key = MetalDepthStencilKey::from_fixed_state(&fixed, &DepthStencilInfo::default());

        assert_eq!(key.depth_compare, MTLCompareFunction::Always);
        assert!(!key.depth_write_enabled);
    }

    #[test]
    fn topology_class_matches_metal_pipeline_classes() {
        assert_eq!(
            metal_topology_class(PrimitiveTopology::Points),
            MTLPrimitiveTopologyClass::Point
        );
        assert_eq!(
            metal_topology_class(PrimitiveTopology::LineStrip),
            MTLPrimitiveTopologyClass::Line
        );
        assert_eq!(
            metal_topology_class(PrimitiveTopology::Quads),
            MTLPrimitiveTopologyClass::Triangle
        );
        assert_eq!(
            metal_topology_class(PrimitiveTopology::Patches),
            MTLPrimitiveTopologyClass::Point
        );
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

    #[test]
    fn cloned_compute_pipeline_keeps_uniform_sizes_at_a_stable_address() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut cache = MetalPipelineCache::new(device);
        let mut program = Program::new(Stage::Compute);
        program.blocks.push(Block::new());
        Emitter::new(&mut program, 0).epilogue();
        let shader = compile_test_shader(&cache, &program);
        let pipeline = cache
            .get_or_create_compute_pipeline(0x4444, &shader)
            .expect("native compute pipeline must compile")
            .clone();
        let cloned = pipeline.clone();

        assert_eq!(
            pipeline.uniform_buffer_sizes() as *const _,
            cloned.uniform_buffer_sizes() as *const _
        );
    }

    #[test]
    fn compute_pipeline_key_includes_qmd_runtime_dimensions() {
        let base = MetalComputePipelineKey {
            unique_hash: 0x1234,
            shared_memory_size: 0x100,
            workgroup_size: [8, 4, 1],
        };
        assert_ne!(
            base,
            MetalComputePipelineKey {
                shared_memory_size: 0x200,
                ..base
            }
        );
        assert_ne!(
            base,
            MetalComputePipelineKey {
                workgroup_size: [16, 4, 1],
                ..base
            }
        );
    }

    #[test]
    fn shader_exception_scope_catches_only_shader_exceptions() {
        let shader_result = catch_shader_exception(|| {
            std::panic::panic_any(shader_recompiler::exception::NotImplementedException::new(
                "Metal compute test",
            ));
        });
        assert_eq!(
            shader_result.unwrap_err(),
            "Metal compute test is not implemented"
        );

        let ordinary = std::panic::catch_unwind(|| {
            let _: Result<(), String> = catch_shader_exception(|| panic!("ordinary panic"));
        });
        assert!(ordinary.is_err(), "non-shader panics must not be swallowed");
    }
}

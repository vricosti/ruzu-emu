// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's video_core/renderer_opengl/gl_graphics_pipeline.h and gl_graphics_pipeline.cpp
//!
//! OpenGL graphics pipeline management -- compiles and configures vertex/fragment/etc shaders.

use std::ffi::c_void;
use std::hash::{Hash, Hasher};
use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use crate::buffer_cache::buffer_cache_base::UniformBufferSizes;
use crate::engines::const_buffer_info::ConstBufferInfo;
use crate::engines::draw_manager::Maxwell3DDrawView;
use crate::engines::maxwell_3d::SurfaceClipInfo;
use crate::engines::maxwell_3d::{Maxwell3D, MAX_CB_SLOTS};
use crate::memory_manager::MemoryManager;
use crate::renderer_opengl::gl_shader_context::Context as ShaderContext;
use crate::renderer_opengl::gl_shader_manager::ProgramManagerHandle;
use crate::renderer_opengl::gl_shader_util::{
    compile_assembly_program, create_program_from_source, create_program_from_spirv,
    program_local_parameter_4f_arb,
};
use crate::renderer_opengl::gl_state_tracker::StateTracker;
use crate::renderer_opengl::gl_texture_cache::TextureCache as OpenGLTextureCache;
use crate::shader_notify::ShaderNotifyHandle;
use crate::texture_cache::texture_cache_base::{DescriptorSyncRegs, ImageViewInOut};
use crate::texture_cache::types::SamplerId;
use crate::textures::texture::texture_pair;
use crate::transform_feedback::TransformFeedbackState;
use common::thread_worker::StatefulThreadWorker;
use common::{cityhash::city_hash64, settings};
use shader_recompiler::shader_info::{num_descriptors, Info as ShaderInfo};

use super::gl_buffer_cache::BufferCache as OpenGLBufferCache;
use super::gl_resource_manager::{OGLAssemblyProgram, OGLProgram, OGLSync};

/// Maximum number of textures bound to a graphics pipeline.
const MAX_TEXTURES: u32 = 64;

/// Maximum number of images bound to a graphics pipeline.
const MAX_IMAGES: u32 = 8;

/// Number of shader stages (vertex, tess control, tess eval, geometry, fragment).
pub const NUM_STAGES: usize = 5;

/// Number of transform feedback buffers.
const NUM_TRANSFORM_FEEDBACK_BUFFERS: usize = 4;

/// Stride of each XFB attribute entry (token, count, attrib).
const XFB_ENTRY_STRIDE: usize = 3;
const XFB_ATTRIB_COUNT: usize = 128 * XFB_ENTRY_STRIDE * NUM_TRANSFORM_FEEDBACK_BUFFERS;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum GraphicsProgramBackend {
    #[default]
    Glsl,
    Glasm,
    SpirV,
}

/// Runtime representation of Eden's three `ConfigureImpl<Spec>`
/// specializations. Rust does not monomorphize a function pointer selected at
/// pipeline construction, but it preserves the same selection order and the
/// same enabled-stage/descriptor gates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ConfigureSpec {
    SimpleVertex,
    SimpleVertexFragment,
    #[default]
    Default,
}

impl ConfigureSpec {
    fn enabled_stage(self, stage: usize) -> bool {
        match self {
            Self::SimpleVertex => stage == 0,
            Self::SimpleVertexFragment => stage == 0 || stage == 4,
            Self::Default => true,
        }
    }

    fn has_storage_buffers(self) -> bool {
        self == Self::Default
    }

    fn has_texture_buffers(self) -> bool {
        self == Self::Default
    }

    fn has_image_buffers(self) -> bool {
        self == Self::Default
    }

    fn has_images(self) -> bool {
        self == Self::Default
    }

    fn passes(self, infos: &[Option<ShaderInfo>; NUM_STAGES], enabled_stages_mask: u32) -> bool {
        for (stage, info) in infos.iter().enumerate() {
            if !self.enabled_stage(stage) && ((enabled_stages_mask >> stage) & 1) != 0 {
                return false;
            }
            let Some(info) = info.as_ref() else {
                continue;
            };
            if !self.has_storage_buffers() && !info.storage_buffers_descriptors.is_empty() {
                return false;
            }
            if !self.has_texture_buffers() && !info.texture_buffer_descriptors.is_empty() {
                return false;
            }
            if !self.has_image_buffers() && !info.image_buffer_descriptors.is_empty() {
                return false;
            }
            if !self.has_images() && !info.image_descriptors.is_empty() {
                return false;
            }
        }
        true
    }

    fn select(infos: &[Option<ShaderInfo>; NUM_STAGES], enabled_stages_mask: u32) -> Self {
        for spec in [Self::SimpleVertex, Self::SimpleVertexFragment] {
            if spec.passes(infos, enabled_stages_mask) {
                return spec;
            }
        }
        Self::Default
    }
}

type GlTransformFeedbackAttribsNv = unsafe extern "system" fn(
    count: gl::types::GLsizei,
    attribs: *const gl::types::GLint,
    buffer_mode: gl::types::GLenum,
);

static GL_TRANSFORM_FEEDBACK_ATTRIBS_NV: OnceLock<Option<GlTransformFeedbackAttribsNv>> =
    OnceLock::new();

/// Load the NV transform-feedback entry point omitted by the generated bindings.
pub fn load_extra_functions<F>(load_fn: &mut F)
where
    F: FnMut(&'static str) -> *const c_void,
{
    let ptr = load_fn("glTransformFeedbackAttribsNV");
    let function = (!ptr.is_null()).then(|| unsafe {
        std::mem::transmute_copy::<*const c_void, GlTransformFeedbackAttribsNv>(&ptr)
    });
    let _ = GL_TRANSFORM_FEEDBACK_ATTRIBS_NV.set(function);
}

/// Key used to identify a unique graphics pipeline configuration.
///
/// Corresponds to `OpenGL::GraphicsPipelineKey`.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct GraphicsPipelineKey {
    pub unique_hashes: [u64; 6],
    /// Packed bitfield: xfb_enabled(1), early_z(1), gs_input_topology(4),
    /// tessellation_primitive(2), tessellation_spacing(2), tessellation_clockwise(1),
    /// app_stage(3).
    pub raw: u32,
    pub padding: [u32; 3],
    pub xfb_state: TransformFeedbackState,
}

impl PartialEq for GraphicsPipelineKey {
    fn eq(&self, rhs: &Self) -> bool {
        let size = self.size();
        // SAFETY: both values are live `repr(C)` keys and Eden compares this
        // exact prefix with `std::memcmp(this, &rhs, Size())`.
        let lhs_bytes =
            unsafe { std::slice::from_raw_parts(self as *const Self as *const u8, size) };
        let rhs_bytes =
            unsafe { std::slice::from_raw_parts(rhs as *const Self as *const u8, size) };
        lhs_bytes == rhs_bytes
    }
}

impl Eq for GraphicsPipelineKey {}

impl GraphicsPipelineKey {
    const XFB_ENABLED_SHIFT: u32 = 0;
    const EARLY_Z_SHIFT: u32 = 1;
    const GS_INPUT_TOPOLOGY_SHIFT: u32 = 2;
    const TESSELLATION_PRIMITIVE_SHIFT: u32 = 6;
    const TESSELLATION_SPACING_SHIFT: u32 = 8;
    const TESSELLATION_CLOCKWISE_SHIFT: u32 = 10;
    const APP_STAGE_SHIFT: u32 = 11;

    const XFB_ENABLED_MASK: u32 = 0x1 << Self::XFB_ENABLED_SHIFT;
    const EARLY_Z_MASK: u32 = 0x1 << Self::EARLY_Z_SHIFT;
    const GS_INPUT_TOPOLOGY_MASK: u32 = 0xF << Self::GS_INPUT_TOPOLOGY_SHIFT;
    const TESSELLATION_PRIMITIVE_MASK: u32 = 0x3 << Self::TESSELLATION_PRIMITIVE_SHIFT;
    const TESSELLATION_SPACING_MASK: u32 = 0x3 << Self::TESSELLATION_SPACING_SHIFT;
    const TESSELLATION_CLOCKWISE_MASK: u32 = 0x1 << Self::TESSELLATION_CLOCKWISE_SHIFT;
    const APP_STAGE_MASK: u32 = 0x7 << Self::APP_STAGE_SHIFT;

    /// Hash the key, considering only relevant bytes (smaller if xfb not enabled).
    pub fn hash_key(&self) -> u64 {
        let size = self.size();
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(self as *const Self as *const u8, size) };
        city_hash64(bytes)
    }

    /// Returns the xfb_enabled bit.
    pub fn xfb_enabled(&self) -> bool {
        (self.raw & 1) != 0
    }

    /// Returns the early_z bit.
    pub fn early_z(&self) -> bool {
        ((self.raw >> 1) & 1) != 0
    }

    pub fn gs_input_topology(&self) -> u32 {
        (self.raw & Self::GS_INPUT_TOPOLOGY_MASK) >> Self::GS_INPUT_TOPOLOGY_SHIFT
    }

    pub fn tessellation_primitive(&self) -> u32 {
        (self.raw & Self::TESSELLATION_PRIMITIVE_MASK) >> Self::TESSELLATION_PRIMITIVE_SHIFT
    }

    pub fn tessellation_spacing(&self) -> u32 {
        (self.raw & Self::TESSELLATION_SPACING_MASK) >> Self::TESSELLATION_SPACING_SHIFT
    }

    pub fn tessellation_clockwise(&self) -> bool {
        ((self.raw & Self::TESSELLATION_CLOCKWISE_MASK) >> Self::TESSELLATION_CLOCKWISE_SHIFT) != 0
    }

    pub fn set_xfb_enabled(&mut self, enabled: bool) {
        self.raw =
            (self.raw & !Self::XFB_ENABLED_MASK) | ((enabled as u32) << Self::XFB_ENABLED_SHIFT);
    }

    pub fn set_early_z(&mut self, enabled: bool) {
        self.raw = (self.raw & !Self::EARLY_Z_MASK) | ((enabled as u32) << Self::EARLY_Z_SHIFT);
    }

    pub fn set_gs_input_topology(&mut self, topology: u32) {
        self.raw = (self.raw & !Self::GS_INPUT_TOPOLOGY_MASK)
            | ((topology & 0xF) << Self::GS_INPUT_TOPOLOGY_SHIFT);
    }

    pub fn set_tessellation_primitive(&mut self, primitive: u32) {
        self.raw = (self.raw & !Self::TESSELLATION_PRIMITIVE_MASK)
            | ((primitive & 0x3) << Self::TESSELLATION_PRIMITIVE_SHIFT);
    }

    pub fn set_tessellation_spacing(&mut self, spacing: u32) {
        self.raw = (self.raw & !Self::TESSELLATION_SPACING_MASK)
            | ((spacing & 0x3) << Self::TESSELLATION_SPACING_SHIFT);
    }

    pub fn set_tessellation_clockwise(&mut self, clockwise: bool) {
        self.raw = (self.raw & !Self::TESSELLATION_CLOCKWISE_MASK)
            | ((clockwise as u32) << Self::TESSELLATION_CLOCKWISE_SHIFT);
    }

    pub fn set_app_stage(&mut self, app_stage: u32) {
        self.raw =
            (self.raw & !Self::APP_STAGE_MASK) | ((app_stage & 0x7) << Self::APP_STAGE_SHIFT);
    }

    /// Returns the effective size in bytes for hashing/comparison.
    ///
    /// If xfb is enabled, the full key (including xfb_state) is used;
    /// otherwise only up to the padding field.
    pub fn size(&self) -> usize {
        if self.xfb_enabled() {
            std::mem::size_of::<Self>()
        } else {
            // offset of `padding` field
            std::mem::offset_of!(GraphicsPipelineKey, padding)
        }
    }
}

impl Hash for GraphicsPipelineKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash_key());
    }
}

struct ProgramBuild {
    assembly_programs: [OGLAssemblyProgram; NUM_STAGES],
    source_programs: [OGLProgram; NUM_STAGES],
    fence: OGLSync,
}

unsafe impl Send for ProgramBuild {}

impl Drop for ProgramBuild {
    fn drop(&mut self) {
        self.fence.release();
    }
}

type AsyncBuildSlot = Arc<(Mutex<Option<ProgramBuild>>, Condvar)>;

/// OpenGL graphics pipeline.
///
/// Corresponds to `OpenGL::GraphicsPipeline`.
pub struct GraphicsPipeline {
    /// Non-owning references retained by Eden's `GraphicsPipeline`.
    /// Production pipelines always have all four; the optional representation
    /// exists only so GL-free unit tests can exercise key/metadata behavior.
    texture_cache: Option<NonNull<OpenGLTextureCache>>,
    buffer_cache: Option<NonNull<crate::renderer_opengl::gl_buffer_cache::BufferCache>>,
    program_manager: Option<ProgramManagerHandle>,
    state_tracker: Option<NonNull<StateTracker>>,
    maxwell3d: Option<NonNull<Maxwell3D>>,
    gpu_memory: Option<Arc<parking_lot::Mutex<MemoryManager>>>,

    key: GraphicsPipelineKey,

    /// One-shot per-stage GLSL source staging. `start_program_build` moves the
    /// array into the build task, matching Eden's move-captured constructor
    /// lambda. Stages with no shader leave the corresponding entry as `None`.
    glsl_sources: [Option<String>; NUM_STAGES],

    /// One-shot per-stage SPIR-V staging, consumed with `glsl_sources`.
    spirv_sources: [Option<Vec<u32>>; NUM_STAGES],

    program_backend: GraphicsProgramBackend,
    max_glasm_storage_buffer_blocks: u32,

    /// Assembly programs per stage (GLASM).
    assembly_programs: [OGLAssemblyProgram; NUM_STAGES],
    /// Source programs per stage (GLSL or SPIR-V).
    source_programs: [OGLProgram; NUM_STAGES],
    /// Bitmask of enabled stages.
    enabled_stages_mask: u32,
    /// Eden's constructor-selected `ConfigureImpl<Spec>` specialization.
    configure_spec: ConfigureSpec,

    /// Per-stage enabled uniform buffer masks.
    enabled_uniform_buffer_masks: [u32; NUM_STAGES],
    /// Per-stage uniform buffer used sizes.
    uniform_buffer_sizes: UniformBufferSizes,
    /// Per-stage base uniform bindings.
    base_uniform_bindings: [u32; NUM_STAGES],
    /// Per-stage base storage bindings.
    base_storage_bindings: [u32; NUM_STAGES],
    /// Per-stage texture buffer counts.
    num_texture_buffers: [u32; NUM_STAGES],
    /// Per-stage image buffer counts.
    num_image_buffers: [u32; NUM_STAGES],

    use_storage_buffers: bool,
    writes_global_memory: bool,
    uses_local_memory: bool,

    /// Transform feedback attributes array.
    num_xfb_attribs: i32,
    num_xfb_buffers_active: u32,
    xfb_attribs: [i32; XFB_ATTRIB_COUNT],

    // Build synchronization
    shader_notify: Option<ShaderNotifyHandle>,
    pending_build: Option<AsyncBuildSlot>,
    built_fence: OGLSync,
    is_built: bool,

    /// Per-stage shader translation result. Mirrors upstream
    /// `std::array<Shader::Info, NUM_STAGES> stage_infos`. Populated by
    /// `apply_shader_infos` so the rasterizer-side ConfigureImpl
    /// equivalent can iterate `texture_descriptors` /
    /// `texture_buffer_descriptors` / `image_descriptors` per stage to
    /// build `ImageViewInOut[]` for `fill_graphics_image_views`.
    stage_infos: [Option<ShaderInfo>; NUM_STAGES],
}

// SAFETY: pipeline fields stay on the render thread. Worker threads publish a
// complete build through `pending_build` and never access the pipeline itself.
unsafe impl Send for GraphicsPipeline {}
unsafe impl Sync for GraphicsPipeline {}

impl Drop for GraphicsPipeline {
    fn drop(&mut self) {
        // `built_fence` is declared after the program arrays upstream, so its
        // RAII wrapper is destroyed first during reverse member destruction.
        self.built_fence.release();
    }
}

impl GraphicsPipeline {
    /// Synchronize graphics TIC/TSC descriptor tables before configuring the pipeline.
    ///
    /// Corresponds to the first side effect in upstream
    /// `GraphicsPipeline::ConfigureImpl`: `texture_cache.SynchronizeGraphicsDescriptors()`.
    fn synchronize_graphics_descriptors(
        &self,
        texture_cache: &mut OpenGLTextureCache,
        regs: DescriptorSyncRegs,
    ) {
        texture_cache.base.synchronize_graphics_descriptors(regs);
    }

    fn configure_buffer_cache_state(&self, buffer_cache: &mut OpenGLBufferCache) {
        // SAFETY: the pipeline is heap-stable in ShaderCache, matching the
        // non-owning `uniform_buffer_sizes` pointer retained by Eden's cache.
        unsafe {
            buffer_cache.set_uniform_buffers_state(
                &self.enabled_uniform_buffer_masks,
                &self.uniform_buffer_sizes,
            );
        }
        buffer_cache.set_graphics_base_uniform_bindings(&self.base_uniform_bindings);
        buffer_cache.set_graphics_base_storage_bindings(&self.base_storage_bindings);
        buffer_cache.set_enable_storage_buffers(self.use_storage_buffers);
    }

    /// Create a new graphics pipeline.
    ///
    /// Corresponds to `GraphicsPipeline::GraphicsPipeline()`.
    pub(crate) fn new(
        texture_cache: NonNull<OpenGLTextureCache>,
        buffer_cache: NonNull<crate::renderer_opengl::gl_buffer_cache::BufferCache>,
        program_manager: ProgramManagerHandle,
        state_tracker: NonNull<StateTracker>,
        thread_worker: Option<&StatefulThreadWorker<ShaderContext>>,
        shader_notify: Option<ShaderNotifyHandle>,
        glsl_sources: [Option<String>; NUM_STAGES],
        spirv_sources: [Option<Vec<u32>>; NUM_STAGES],
        infos: &[Option<ShaderInfo>; NUM_STAGES],
        key: GraphicsPipelineKey,
        program_backend: GraphicsProgramBackend,
        max_glasm_storage_buffer_blocks: u32,
        use_assembly_shaders: bool,
        force_context_flush: bool,
    ) -> Self {
        if let Some(shader_notify) = shader_notify {
            shader_notify.mark_shader_building();
        }
        let mut pipeline = Self::new_impl(
            key,
            shader_notify,
            Some(texture_cache),
            Some(buffer_cache),
            Some(program_manager),
            Some(state_tracker),
        );
        pipeline.glsl_sources = glsl_sources;
        pipeline.spirv_sources = spirv_sources;
        pipeline.program_backend = program_backend;
        pipeline.max_glasm_storage_buffer_blocks = max_glasm_storage_buffer_blocks;
        pipeline.apply_shader_infos(infos);
        if key.xfb_enabled() && use_assembly_shaders {
            pipeline.generate_transform_feedback_state();
        }
        pipeline.start_program_build(thread_worker, force_context_flush);
        pipeline
    }

    fn new_impl(
        key: GraphicsPipelineKey,
        shader_notify: Option<ShaderNotifyHandle>,
        texture_cache: Option<NonNull<OpenGLTextureCache>>,
        buffer_cache: Option<NonNull<crate::renderer_opengl::gl_buffer_cache::BufferCache>>,
        program_manager: Option<ProgramManagerHandle>,
        state_tracker: Option<NonNull<StateTracker>>,
    ) -> Self {
        Self {
            texture_cache,
            buffer_cache,
            program_manager,
            state_tracker,
            maxwell3d: None,
            gpu_memory: None,
            key,
            glsl_sources: Default::default(),
            spirv_sources: Default::default(),
            program_backend: GraphicsProgramBackend::Glsl,
            max_glasm_storage_buffer_blocks: 0,
            assembly_programs: std::array::from_fn(|_| OGLAssemblyProgram::new()),
            source_programs: std::array::from_fn(|_| OGLProgram::new()),
            enabled_stages_mask: 0,
            configure_spec: ConfigureSpec::Default,
            enabled_uniform_buffer_masks: [0; NUM_STAGES],
            uniform_buffer_sizes: [[0;
                crate::buffer_cache::buffer_cache_base::NUM_GRAPHICS_UNIFORM_BUFFERS as usize];
                NUM_STAGES],
            base_uniform_bindings: [0; NUM_STAGES],
            base_storage_bindings: [0; NUM_STAGES],
            num_texture_buffers: [0; NUM_STAGES],
            num_image_buffers: [0; NUM_STAGES],
            use_storage_buffers: false,
            writes_global_memory: false,
            uses_local_memory: false,
            num_xfb_attribs: 0,
            num_xfb_buffers_active: 0,
            xfb_attribs: [0; XFB_ATTRIB_COUNT],
            shader_notify,
            pending_build: None,
            built_fence: OGLSync::new(),
            is_built: true,
            stage_infos: Default::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        key: GraphicsPipelineKey,
        shader_notify: Option<ShaderNotifyHandle>,
    ) -> Self {
        Self::new_impl(key, shader_notify, None, None, None, None)
    }

    #[cfg(test)]
    pub(crate) fn new_for_test_with_sources(
        key: GraphicsPipelineKey,
        shader_notify: Option<ShaderNotifyHandle>,
        glsl_sources: [Option<String>; NUM_STAGES],
        spirv_sources: [Option<Vec<u32>>; NUM_STAGES],
        infos: &[Option<ShaderInfo>; NUM_STAGES],
        program_backend: GraphicsProgramBackend,
        max_glasm_storage_buffer_blocks: u32,
        use_assembly_shaders: bool,
    ) -> Self {
        let mut pipeline = Self::new_for_test(key, shader_notify);
        pipeline.glsl_sources = glsl_sources;
        pipeline.spirv_sources = spirv_sources;
        pipeline.program_backend = program_backend;
        pipeline.max_glasm_storage_buffer_blocks = max_glasm_storage_buffer_blocks;
        pipeline.apply_shader_infos(infos);
        if key.xfb_enabled() && use_assembly_shaders {
            pipeline.generate_transform_feedback_state();
        }
        pipeline
    }

    #[cfg(test)]
    pub(crate) fn glsl_source_for_test(&self, stage: usize) -> Option<&str> {
        self.glsl_sources[stage].as_deref()
    }

    /// Port of `GraphicsPipeline::SetEngine`.
    pub fn set_engine(
        &mut self,
        maxwell3d: NonNull<Maxwell3D>,
        gpu_memory: Arc<parking_lot::Mutex<MemoryManager>>,
    ) {
        self.maxwell3d = Some(maxwell3d);
        self.gpu_memory = Some(gpu_memory);
    }

    /// Port of `GraphicsPipeline::Configure(bool is_indexed)` and its
    /// `ConfigureImpl` body. The pipeline obtains the cache/manager owners from
    /// its constructor and the live engine/memory owners from `set_engine`, as
    /// Eden does; the rasterizer no longer reconstructs this state per draw.
    pub fn configure(&mut self, is_indexed: bool) -> bool {
        let (
            Some(mut texture_cache_ptr),
            Some(mut buffer_cache_ptr),
            Some(program_manager),
            Some(mut state_tracker_ptr),
            Some(mut maxwell3d_ptr),
            Some(gpu_memory),
        ) = (
            self.texture_cache,
            self.buffer_cache,
            self.program_manager.clone(),
            self.state_tracker,
            self.maxwell3d,
            self.gpu_memory.clone(),
        )
        else {
            return false;
        };

        // SAFETY: all pointers refer to heap-stable renderer/rasterizer owners
        // that outlive the shader cache and its pipelines. Configure runs on
        // the serialized renderer thread while PrepareDraw holds both cache
        // mutexes, matching Eden's non-owning references and raw engine
        // pointers.
        let texture_cache = unsafe { texture_cache_ptr.as_mut() };
        let buffer_cache = unsafe { buffer_cache_ptr.as_mut() };
        let state_tracker = unsafe { state_tracker_ptr.as_mut() };
        let draw_state = unsafe { maxwell3d_ptr.as_ref().draw_manager_state() as *const _ };
        let mut draw_view =
            unsafe { Maxwell3DDrawView::live(&*draw_state, is_indexed, maxwell3d_ptr.as_mut()) };

        let descriptor_sync_regs = draw_view.descriptor_sync_regs();
        self.synchronize_graphics_descriptors(texture_cache, descriptor_sync_regs);
        self.configure_buffer_cache_state(buffer_cache);

        let cb_bindings = draw_view.cb_bindings();
        let via_header_index = descriptor_sync_regs.sampler_binding_via_header;
        let mut views = [ImageViewInOut::default(); (MAX_TEXTURES + MAX_IMAGES) as usize];
        let mut samplers = [SamplerId::default(); MAX_TEXTURES as usize];
        let mut views_index = 0usize;
        let mut samplers_index = 0usize;
        // Eden owns a raw `MemoryManager*` and uses it for both descriptor
        // handles and GetSamplerId. Keep the equivalent Rust guard for the
        // complete descriptor pass and pass that same borrow into the cache,
        // avoiding a recursive lock without changing Eden's call order.
        let gpu_memory_guard = gpu_memory.lock();
        for stage in 0..NUM_STAGES {
            if !self.configure_spec.enabled_stage(stage) {
                continue;
            }
            self.configure_stage(
                stage,
                buffer_cache,
                texture_cache,
                &cb_bindings[stage],
                &gpu_memory_guard,
                via_header_index,
                &mut views,
                &mut views_index,
                &mut samplers,
                &mut samplers_index,
            );
        }
        drop(gpu_memory_guard);
        texture_cache.fill_image_views(
            &mut views[..views_index],
            false,
            self.configure_spec.has_images(),
        );

        let render_targets = draw_view.render_targets();
        let surface_clip = draw_view.surface_clip();
        let (framebuffer, _, _) = texture_cache
            .update_render_targets_and_get_framebuffer_from_snapshot(
                &render_targets,
                &mut draw_view,
                false,
                None,
            );
        state_tracker.bind_framebuffer(framebuffer);

        let mut texture_buffer_it = 0usize;
        for stage in 0..NUM_STAGES {
            if self.configure_spec.enabled_stage(stage) {
                self.bind_stage_info(
                    stage,
                    buffer_cache,
                    texture_cache,
                    &views,
                    &mut texture_buffer_it,
                );
            }
        }
        buffer_cache.update_graphics_buffers(is_indexed);
        buffer_cache.bind_host_geometry_buffers(is_indexed);

        if !self.is_built() {
            self.wait_for_build();
        }
        {
            let mut program_manager = program_manager.lock();
            if self.assembly_programs[0].handle != 0 {
                program_manager
                    .bind_assembly_programs(&self.assembly_programs, self.enabled_stages_mask);
            } else {
                program_manager.bind_source_programs(&self.source_programs);
            }
        }

        let mut textures = [0u32; MAX_TEXTURES as usize];
        let mut images = [0u32; MAX_IMAGES as usize];
        let mut gl_samplers = [0u32; MAX_TEXTURES as usize];
        let mut views_it = 0usize;
        let mut samplers_it = 0usize;
        let mut texture_binding = 0i32;
        let mut image_binding = 0i32;
        let mut sampler_binding = 0i32;
        for stage in 0..NUM_STAGES {
            if self.configure_spec.enabled_stage(stage) {
                self.prepare_stage(
                    stage,
                    buffer_cache,
                    texture_cache,
                    &views,
                    &samplers,
                    surface_clip,
                    &mut views_it,
                    &mut samplers_it,
                    &mut textures,
                    &mut images,
                    &mut gl_samplers,
                    &mut texture_binding,
                    &mut image_binding,
                    &mut sampler_binding,
                );
            }
        }
        if texture_binding != 0 {
            if texture_binding != sampler_binding {
                // Eden's ASSERT is fail-soft and still issues both bindings.
                log::error!(
                    "GraphicsPipeline::Configure texture binding count {} differs from sampler binding count {}",
                    texture_binding,
                    sampler_binding
                );
            }
            unsafe {
                gl::BindTextures(0, texture_binding, textures.as_ptr());
                gl::BindSamplers(0, sampler_binding, gl_samplers.as_ptr());
            }
        }
        if image_binding != 0 {
            unsafe {
                gl::BindImageTextures(0, image_binding, images.as_ptr());
            }
        }
        if buffer_cache.any_buffer_uploaded {
            buffer_cache.runtime.post_copy_barrier();
            buffer_cache.any_buffer_uploaded = false;
        }
        true
    }

    /// Populate per-stage descriptor metadata from translated shader infos.
    ///
    /// Port of the metadata loop in upstream
    /// `GraphicsPipeline::GraphicsPipeline(...)` (`gl_graphics_pipeline.cpp`):
    /// enabled stage mask, per-stage UBO mask/sizes, and cumulative base
    /// bindings are derived from `Shader::Info`.
    fn apply_shader_infos(&mut self, infos: &[Option<ShaderInfo>; NUM_STAGES]) {
        self.enabled_stages_mask = 0;
        self.enabled_uniform_buffer_masks = [0; NUM_STAGES];
        self.uniform_buffer_sizes = [[0;
            crate::buffer_cache::buffer_cache_base::NUM_GRAPHICS_UNIFORM_BUFFERS as usize];
            NUM_STAGES];
        self.base_uniform_bindings = [0; NUM_STAGES];
        self.base_storage_bindings = [0; NUM_STAGES];
        self.num_texture_buffers = [0; NUM_STAGES];
        self.num_image_buffers = [0; NUM_STAGES];
        self.use_storage_buffers = false;
        self.writes_global_memory = false;
        self.uses_local_memory = false;

        // Keep a per-stage `Info` clone so the rasterizer-side ConfigureImpl
        // can iterate `texture_descriptors[]` etc. at draw time. Upstream
        // stores these inside `GraphicsPipeline` as `stage_infos`.
        self.stage_infos = std::array::from_fn(|stage| infos[stage].clone());

        let mut num_textures = 0u32;
        let mut num_images = 0u32;
        let mut num_storage_buffers = 0u32;
        for stage in 0..NUM_STAGES {
            if let Some(info) = infos[stage].as_ref() {
                self.enabled_stages_mask |= 1u32 << stage;
                self.enabled_uniform_buffer_masks[stage] = info.constant_buffer_mask;
                self.uniform_buffer_sizes[stage].copy_from_slice(&info.constant_buffer_used_sizes);
                self.num_texture_buffers[stage] = num_descriptors(&info.texture_buffer_descriptors);
                self.num_image_buffers[stage] = num_descriptors(&info.image_buffer_descriptors);
                num_textures = num_textures.wrapping_add(self.num_texture_buffers[stage]);
                num_images = num_images.wrapping_add(self.num_image_buffers[stage]);
                num_textures =
                    num_textures.wrapping_add(num_descriptors(&info.texture_descriptors));
                num_images = num_images.wrapping_add(num_descriptors(&info.image_descriptors));
                num_storage_buffers = num_storage_buffers
                    .wrapping_add(num_descriptors(&info.storage_buffers_descriptors));
                self.writes_global_memory |= info
                    .storage_buffers_descriptors
                    .iter()
                    .any(|desc| desc.is_written);
                self.uses_local_memory |= info.uses_local_memory;
            }

            if stage < NUM_STAGES - 1 {
                self.base_uniform_bindings[stage + 1] = self.base_uniform_bindings[stage];
                self.base_storage_bindings[stage + 1] = self.base_storage_bindings[stage];
                if let Some(info) = infos[stage].as_ref() {
                    self.base_uniform_bindings[stage + 1] = self.base_uniform_bindings[stage + 1]
                        .wrapping_add(num_descriptors(&info.constant_buffer_descriptors));
                    self.base_storage_bindings[stage + 1] = self.base_storage_bindings[stage + 1]
                        .wrapping_add(num_descriptors(&info.storage_buffers_descriptors));
                }
            }
        }

        if num_textures > MAX_TEXTURES {
            log::error!(
                "GraphicsPipeline texture descriptor count {num_textures} exceeds {MAX_TEXTURES}"
            );
        }
        if num_images > MAX_IMAGES {
            log::error!(
                "GraphicsPipeline image descriptor count {num_images} exceeds {MAX_IMAGES}"
            );
        }

        self.use_storage_buffers = self.program_backend != GraphicsProgramBackend::Glasm
            || num_storage_buffers <= self.max_glasm_storage_buffer_blocks;
        if self.use_storage_buffers {
            self.writes_global_memory = false;
        }
        self.configure_spec = ConfigureSpec::select(infos, self.enabled_stages_mask);
    }

    /// Mechanical Rust form of upstream `ConfigureImpl`'s `config_stage`
    /// lambda. Keeping this helper private preserves the upstream owner while
    /// avoiding the former public combinatorial configure API.
    #[allow(clippy::too_many_arguments)]
    fn configure_stage(
        &self,
        stage: usize,
        buffer_cache: &mut OpenGLBufferCache,
        texture_cache: &mut OpenGLTextureCache,
        cbufs: &[ConstBufferInfo; MAX_CB_SLOTS],
        gpu_memory: &MemoryManager,
        via_header_index: bool,
        views: &mut [ImageViewInOut; (MAX_TEXTURES + MAX_IMAGES) as usize],
        views_index: &mut usize,
        samplers: &mut [SamplerId; MAX_TEXTURES as usize],
        samplers_index: &mut usize,
    ) {
        buffer_cache.unbind_graphics_storage_buffers(stage);
        let Some(info) = self.stage_infos[stage].as_ref() else {
            return;
        };
        if self.configure_spec.has_storage_buffers() {
            for (ssbo_index, desc) in info.storage_buffers_descriptors.iter().enumerate() {
                if desc.count != 1 {
                    // Eden's ASSERT is fail-soft and still binds the descriptor.
                    log::error!(
                        "GraphicsPipeline::Configure storage-buffer descriptor count is {}, expected 1",
                        desc.count
                    );
                }
                buffer_cache.bind_graphics_storage_buffer_with_gpu_reader(
                    stage,
                    ssbo_index,
                    desc.cbuf_index,
                    desc.cbuf_offset,
                    desc.is_written,
                    |gpu_addr| gpu_memory.gpu_to_cpu_address(gpu_addr),
                    |gpu_addr| gpu_memory.get_memory_layout_size(gpu_addr),
                    |gpu_addr, output| gpu_memory.read_block(gpu_addr, output),
                );
            }
        }

        let read_word = |cbuf_index: u32, offset: u32| {
            let cbuf = cbufs[cbuf_index as usize];
            if !cbuf.enabled {
                // Eden's ASSERT is fail-soft and proceeds with the address.
                log::error!(
                    "GraphicsPipeline::Configure reads disabled cbuf {cbuf_index} at offset {offset:#x}"
                );
            }
            let address = cbuf.address.wrapping_add(u64::from(offset));
            gpu_memory.read::<u32>(address)
        };
        macro_rules! read_handle {
            ($desc:expr, $index:expr) => {{
                let index_offset = ($index as u32) << $desc.size_shift;
                let offset = $desc.cbuf_offset.wrapping_add(index_offset);
                if $desc.has_secondary {
                    let second_offset = $desc.secondary_cbuf_offset.wrapping_add(index_offset);
                    let lhs = read_word($desc.cbuf_index, offset) << $desc.shift_left;
                    let rhs = read_word($desc.secondary_cbuf_index, second_offset)
                        << $desc.secondary_shift_left;
                    texture_pair(lhs | rhs, via_header_index)
                } else {
                    texture_pair(read_word($desc.cbuf_index, offset), via_header_index)
                }
            }};
        }
        macro_rules! add_image {
            ($desc:expr, $blacklist:expr) => {{
                for index in 0..$desc.count {
                    let index_offset = index << $desc.size_shift;
                    let offset = $desc.cbuf_offset.wrapping_add(index_offset);
                    let (image_index, _) =
                        texture_pair(read_word($desc.cbuf_index, offset), via_header_index);
                    views[*views_index] = ImageViewInOut {
                        index: image_index,
                        blacklist: $blacklist,
                        id: Default::default(),
                    };
                    *views_index += 1;
                }
            }};
        }

        if self.configure_spec.has_texture_buffers() {
            for desc in &info.texture_buffer_descriptors {
                for index in 0..desc.count {
                    let (image_index, _) = read_handle!(desc, index);
                    views[*views_index] = ImageViewInOut {
                        index: image_index,
                        blacklist: false,
                        id: Default::default(),
                    };
                    *views_index += 1;
                }
            }
        }
        if self.configure_spec.has_image_buffers() {
            for desc in &info.image_buffer_descriptors {
                add_image!(desc, false);
            }
        }
        for desc in &info.texture_descriptors {
            for index in 0..desc.count {
                let (image_index, sampler_index) = read_handle!(desc, index);
                views[*views_index] = ImageViewInOut {
                    index: image_index,
                    blacklist: false,
                    id: Default::default(),
                };
                *views_index += 1;
                samplers[*samplers_index] =
                    texture_cache
                        .base
                        .get_sampler_id_with_memory(sampler_index, false, gpu_memory);
                *samplers_index += 1;
            }
        }
        if self.configure_spec.has_images() {
            for desc in &info.image_descriptors {
                add_image!(desc, desc.is_written);
            }
        }
    }

    /// Mechanical Rust form of upstream `ConfigureImpl`'s `bind_stage_info`
    /// lambda.
    fn bind_stage_info(
        &self,
        stage: usize,
        buffer_cache: &mut OpenGLBufferCache,
        texture_cache: &OpenGLTextureCache,
        views: &[ImageViewInOut],
        views_it: &mut usize,
    ) {
        buffer_cache.unbind_graphics_texture_buffers(stage);
        let Some(info) = self.stage_infos[stage].as_ref() else {
            return;
        };
        let mut index = 0usize;

        macro_rules! add_buffer {
            ($desc:expr, $is_image:expr, $is_written:expr) => {{
                for _ in 0..$desc.count {
                    let view_id = views[*views_it].id;
                    *views_it += 1;
                    let image_view = texture_cache
                        .get_image_view(view_id)
                        .expect("filled texture-buffer view must exist");
                    let gpu_addr = texture_cache.image_view_gpu_addr(view_id);
                    buffer_cache.bind_graphics_texture_buffer(
                        stage,
                        index,
                        gpu_addr,
                        image_view.buffer_size(),
                        image_view.pixel_format(),
                        $is_written,
                        $is_image,
                    );
                    index += 1;
                }
            }};
        }
        if self.configure_spec.has_texture_buffers() {
            for desc in &info.texture_buffer_descriptors {
                add_buffer!(desc, false, false);
            }
        }
        if self.configure_spec.has_image_buffers() {
            for desc in &info.image_buffer_descriptors {
                add_buffer!(desc, true, desc.is_written);
            }
        }
        *views_it += num_descriptors(&info.texture_descriptors) as usize;
        if self.configure_spec.has_images() {
            *views_it += num_descriptors(&info.image_descriptors) as usize;
        }
    }

    /// Mechanical Rust form of upstream `ConfigureImpl`'s `prepare_stage`
    /// lambda.
    #[allow(clippy::too_many_arguments)]
    fn prepare_stage(
        &self,
        stage: usize,
        buffer_cache: &mut OpenGLBufferCache,
        texture_cache: &mut OpenGLTextureCache,
        views: &[ImageViewInOut],
        samplers: &[SamplerId],
        surface_clip: SurfaceClipInfo,
        views_it: &mut usize,
        samplers_it: &mut usize,
        textures: &mut [u32; MAX_TEXTURES as usize],
        images: &mut [u32; MAX_IMAGES as usize],
        gl_samplers: &mut [u32; MAX_TEXTURES as usize],
        texture_binding: &mut i32,
        image_binding: &mut i32,
        sampler_binding: &mut i32,
    ) {
        buffer_cache.set_image_pointers(
            textures[*texture_binding as usize..].as_mut_ptr(),
            images[*image_binding as usize..].as_mut_ptr(),
        );
        buffer_cache.bind_host_stage_buffers(stage);

        *texture_binding =
            (*texture_binding as u32).wrapping_add(self.num_texture_buffers[stage]) as i32;
        *image_binding = (*image_binding as u32).wrapping_add(self.num_image_buffers[stage]) as i32;
        *views_it += self.num_texture_buffers[stage] as usize;
        *views_it += self.num_image_buffers[stage] as usize;

        let Some(info) = self.stage_infos[stage].as_ref() else {
            return;
        };

        let mut texture_scaling_mask = 0u32;
        let mut image_scaling_mask = 0u32;
        let mut stage_texture_binding = 0u32;
        let mut stage_image_binding = 0u32;

        if self.configure_spec.has_texture_buffers() {
            for desc in &info.texture_buffer_descriptors {
                for _ in 0..desc.count {
                    gl_samplers[*sampler_binding as usize] = 0;
                    *sampler_binding += 1;
                }
            }
        }
        for desc in &info.texture_descriptors {
            for _ in 0..desc.count {
                let view_id = views[*views_it].id;
                *views_it += 1;
                let image_view = texture_cache
                    .get_image_view(view_id)
                    .expect("filled sampled-image view must exist");
                textures[*texture_binding as usize] =
                    image_view.handle_for_texture_type(desc.texture_type);
                if texture_cache.image_view_is_rescaling(view_id) {
                    texture_scaling_mask |= 1u32 << stage_texture_binding;
                }
                *texture_binding += 1;
                stage_texture_binding += 1;

                let sampler = texture_cache
                    .get_sampler(samplers[*samplers_it])
                    .expect("filled sampled-image sampler must exist");
                *samplers_it += 1;
                gl_samplers[*sampler_binding as usize] =
                    if sampler.has_added_anisotropy() && !image_view.supports_anisotropy() {
                        sampler.handle_with_default_anisotropy()
                    } else {
                        sampler.handle()
                    };
                *sampler_binding += 1;
            }
        }
        if self.configure_spec.has_images() {
            for desc in &info.image_descriptors {
                for _ in 0..desc.count {
                    let view_id = views[*views_it].id;
                    *views_it += 1;
                    if desc.is_written {
                        let image_id = texture_cache.base.slot_image_views[view_id].image_id;
                        texture_cache.base.mark_modification_by_id(image_id);
                    }
                    images[*image_binding as usize] = texture_cache
                        .get_image_view_mut(view_id)
                        .expect("filled storage-image view must exist")
                        .storage_view(desc.texture_type, desc.format);
                    if texture_cache.image_view_is_rescaling(view_id) {
                        image_scaling_mask |= 1u32 << stage_image_binding;
                    }
                    *image_binding += 1;
                    stage_image_binding += 1;
                }
            }
        }

        let use_assembly = self.assembly_programs[0].handle != 0;
        if info.uses_rescaling_uniform {
            let texture_mask = f32::from_bits(texture_scaling_mask);
            let image_mask = f32::from_bits(image_scaling_mask);
            let down_factor = if texture_cache.is_rescaling_active() {
                settings::values().resolution_info.down_factor
            } else {
                1.0
            };
            if use_assembly {
                program_local_parameter_4f_arb(
                    gl_assembly_stage(stage),
                    0,
                    texture_mask,
                    image_mask,
                    down_factor,
                    0.0,
                );
            } else {
                unsafe {
                    gl::ProgramUniform4f(
                        self.source_programs[stage].handle,
                        0,
                        texture_mask,
                        image_mask,
                        down_factor,
                        0.0,
                    );
                }
            }
        }
        if info.uses_render_area {
            let width = surface_clip.width as f32;
            let height = surface_clip.height as f32;
            if use_assembly {
                program_local_parameter_4f_arb(
                    gl_assembly_stage(stage),
                    1,
                    width,
                    height,
                    0.0,
                    0.0,
                );
            } else {
                unsafe {
                    gl::ProgramUniform4f(
                        self.source_programs[stage].handle,
                        1,
                        width,
                        height,
                        0.0,
                        0.0,
                    );
                }
            }
        }
    }

    /// Configure transform feedback if active.
    ///
    /// Corresponds to `GraphicsPipeline::ConfigureTransformFeedback()`.
    pub fn configure_transform_feedback(&self) {
        if self.num_xfb_attribs != 0 {
            self.configure_transform_feedback_impl();
        }
    }

    /// Return the immutable pipeline cache key.
    ///
    /// Port of `GraphicsPipeline::Key()`.
    pub fn key(&self) -> &GraphicsPipelineKey {
        &self.key
    }

    /// Returns whether any storage buffer is written.
    pub fn writes_global_memory(&self) -> bool {
        self.writes_global_memory
    }

    /// Returns whether local memory is used.
    pub fn uses_local_memory(&self) -> bool {
        self.uses_local_memory
    }

    /// Execute the host-program creation closure owned by Eden's constructor.
    /// Rust publishes a completed build through a slot because moving a
    /// partially-constructed `self` into the worker would not be memory safe.
    fn start_program_build(
        &mut self,
        worker: Option<&StatefulThreadWorker<ShaderContext>>,
        force_context_flush: bool,
    ) {
        self.is_built = false;
        // Eden move-captures both source arrays into the one-shot build task.
        // Do the same here so compiled pipelines do not retain shader source
        // strings and SPIR-V words for their complete cache lifetime.
        let sources = std::mem::take(&mut self.glsl_sources);
        let spirv_sources = std::mem::take(&mut self.spirv_sources);
        let Some(worker) = worker else {
            let build = build_programs(
                &sources,
                &spirv_sources,
                self.program_backend,
                force_context_flush,
            );
            self.accept_program_build(build);
            self.is_built = !force_context_flush;
            if let Some(shader_notify) = self.shader_notify {
                shader_notify.mark_shader_complete();
            }
            return;
        };

        let backend = self.program_backend;
        let shader_notify = self.shader_notify;
        let slot: AsyncBuildSlot = Arc::new((Mutex::new(None), Condvar::new()));
        let worker_slot = Arc::clone(&slot);
        self.pending_build = Some(slot);
        worker.queue_work(move |_context| {
            let build = build_programs(&sources, &spirv_sources, backend, true);
            let (lock, condvar) = &*worker_slot;
            *lock.lock().unwrap() = Some(build);
            condvar.notify_one();
            if let Some(shader_notify) = shader_notify {
                shader_notify.mark_shader_complete();
            }
        });
    }

    fn accept_program_build(&mut self, mut build: ProgramBuild) {
        self.source_programs = std::mem::take(&mut build.source_programs);
        self.assembly_programs = std::mem::take(&mut build.assembly_programs);
        self.built_fence = std::mem::take(&mut build.fence);
    }

    fn receive_pending_build(&mut self, wait: bool) -> bool {
        let Some(slot) = self.pending_build.as_ref().cloned() else {
            return false;
        };
        let (lock, condvar) = &*slot;
        let mut result = lock.lock().unwrap();
        if wait {
            result = condvar
                .wait_while(result, |result| result.is_none())
                .unwrap();
        } else if result.is_none() {
            return false;
        }
        let completed = result.take().expect("pending shader build completed");
        drop(result);
        self.pending_build = None;
        self.accept_program_build(completed);
        true
    }

    /// Returns whether the pipeline has finished building.
    ///
    /// Port of `GraphicsPipeline::IsBuilt()`.
    pub fn is_built(&mut self) -> bool {
        if self.is_built {
            return true;
        }
        self.receive_pending_build(false);
        if self.is_built {
            return true;
        }
        if self.built_fence.handle.is_null() {
            return false;
        }
        self.is_built = self.built_fence.is_signaled();
        self.is_built
    }

    #[cfg(test)]
    pub fn set_built_for_test(&mut self, built: bool) {
        self.is_built = built;
    }

    /// Internal: configure transform feedback attributes.
    ///
    /// Port of `GraphicsPipeline::ConfigureTransformFeedbackImpl()`.
    fn configure_transform_feedback_impl(&self) {
        let buffer_mode = if self.num_xfb_buffers_active == 1 {
            gl::INTERLEAVED_ATTRIBS
        } else {
            gl::SEPARATE_ATTRIBS
        };
        let transform_feedback_attribs = GL_TRANSFORM_FEEDBACK_ATTRIBS_NV
            .get()
            .and_then(|function| *function)
            .expect("glTransformFeedbackAttribsNV is required by the GLASM backend");
        unsafe {
            transform_feedback_attribs(
                self.num_xfb_attribs,
                self.xfb_attribs.as_ptr(),
                buffer_mode,
            );
        }
    }

    /// Generate transform feedback state from the pipeline key.
    ///
    /// Port of `GraphicsPipeline::GenerateTransformFeedbackState()`.
    fn generate_transform_feedback_state(&mut self) {
        let mut cursor = 0usize;
        self.num_xfb_buffers_active = 0;
        for feedback in 0..NUM_TRANSFORM_FEEDBACK_BUFFERS {
            let layout = self.key.xfb_state.layouts[feedback];
            if layout.stride != layout.varying_count.wrapping_mul(4) {
                log::error!(
                    "OpenGL transform feedback stride padding is not implemented: stride={} varying_count={}",
                    layout.stride,
                    layout.varying_count
                );
            }
            if layout.varying_count == 0 {
                continue;
            }
            self.num_xfb_buffers_active += 1;

            let locations = &self.key.xfb_state.varyings[feedback];
            let mut current_index = None;
            for offset in 0..layout.varying_count {
                let location = locations[(offset / 4) as usize];
                let attribute = match offset % 4 {
                    0 => location.attribute0(),
                    1 => location.attribute1(),
                    2 => location.attribute2(),
                    3 => location.attribute3(),
                    _ => unreachable!(),
                };
                let index = attribute / 4;
                if current_index == Some(index) {
                    self.xfb_attribs[cursor - 2] += 1;
                    continue;
                }
                current_index = Some(index);
                let (token, attribute_index) = transform_feedback_enum(attribute);
                self.xfb_attribs[cursor] = token;
                self.xfb_attribs[cursor + 1] = 1;
                self.xfb_attribs[cursor + 2] = attribute_index;
                cursor += XFB_ENTRY_STRIDE;
            }
        }
        self.num_xfb_attribs = (cursor / XFB_ENTRY_STRIDE) as i32;
    }

    /// Wait for the pipeline build to complete.
    ///
    /// Port of `GraphicsPipeline::WaitForBuild()`.
    fn wait_for_build(&mut self) {
        if self.built_fence.handle.is_null() {
            self.receive_pending_build(true);
        }
        let status = unsafe { gl::ClientWaitSync(self.built_fence.handle, 0, gl::TIMEOUT_IGNORED) };
        if status == gl::WAIT_FAILED {
            log::error!("GraphicsPipeline::WaitForBuild: glClientWaitSync returned GL_WAIT_FAILED");
        }
        self.is_built = true;
    }
}

fn build_programs(
    sources: &[Option<String>; NUM_STAGES],
    spirv_sources: &[Option<Vec<u32>>; NUM_STAGES],
    backend: GraphicsProgramBackend,
    create_fence: bool,
) -> ProgramBuild {
    let mut build = ProgramBuild {
        source_programs: std::array::from_fn(|_| OGLProgram::new()),
        assembly_programs: std::array::from_fn(|_| OGLAssemblyProgram::new()),
        fence: OGLSync::new(),
    };
    for stage_index in 0..NUM_STAGES {
        match backend {
            GraphicsProgramBackend::Glsl => {
                let Some(source) = sources[stage_index].as_ref() else {
                    continue;
                };
                if source.is_empty() {
                    continue;
                }
                build.source_programs[stage_index] =
                    create_program_from_source(source, gl_stage(stage_index));
            }
            GraphicsProgramBackend::Glasm => {
                let Some(source) = sources[stage_index].as_ref() else {
                    continue;
                };
                if !source.is_empty() {
                    build.assembly_programs[stage_index] =
                        compile_assembly_program(source, gl_assembly_stage(stage_index));
                }
            }
            GraphicsProgramBackend::SpirV => {
                let Some(source) = spirv_sources[stage_index].as_ref() else {
                    continue;
                };
                if !source.is_empty() {
                    build.source_programs[stage_index] =
                        create_program_from_spirv(source, gl_stage(stage_index));
                }
            }
        }
    }
    if create_fence {
        build.fence.create();
        unsafe { gl::Flush() };
    }
    build
}

/// Helper: map a stage index to the corresponding GL shader stage enum.
///
/// Corresponds to the anonymous `Stage()` function in gl_graphics_pipeline.cpp.
fn gl_stage(stage_index: usize) -> u32 {
    match stage_index {
        0 => gl::VERTEX_SHADER,
        1 => gl::TESS_CONTROL_SHADER,
        2 => gl::TESS_EVALUATION_SHADER,
        3 => gl::GEOMETRY_SHADER,
        4 => gl::FRAGMENT_SHADER,
        _ => {
            // Eden's ASSERT_MSG is fail-soft and returns GL_NONE.
            log::error!("Invalid OpenGL shader stage index: {stage_index}");
            gl::NONE
        }
    }
}

/// Helper: map a stage index to the corresponding NV assembly program enum.
///
/// Corresponds to the anonymous `AssemblyStage()` function in gl_graphics_pipeline.cpp.
fn gl_assembly_stage(stage_index: usize) -> u32 {
    const GL_VERTEX_PROGRAM_NV: u32 = 0x8620;
    const GL_TESS_CONTROL_PROGRAM_NV: u32 = 0x891E;
    const GL_TESS_EVALUATION_PROGRAM_NV: u32 = 0x891F;
    const GL_GEOMETRY_PROGRAM_NV: u32 = 0x8C26;
    const GL_FRAGMENT_PROGRAM_NV: u32 = 0x8870;

    match stage_index {
        0 => GL_VERTEX_PROGRAM_NV,
        1 => GL_TESS_CONTROL_PROGRAM_NV,
        2 => GL_TESS_EVALUATION_PROGRAM_NV,
        3 => GL_GEOMETRY_PROGRAM_NV,
        4 => GL_FRAGMENT_PROGRAM_NV,
        _ => {
            // Eden's ASSERT_MSG is fail-soft and returns GL_NONE.
            log::error!("Invalid OpenGL assembly stage index: {stage_index}");
            gl::NONE
        }
    }
}

/// Translate hardware transform feedback index to ARB_transform_feedback3 tokens.
///
/// Corresponds to `TransformFeedbackEnum()` in gl_graphics_pipeline.cpp.
fn transform_feedback_enum(location: u32) -> (i32, i32) {
    let index = location / 4;
    if (8..=39).contains(&index) {
        return (0x8C7D_i32, (index - 8) as i32); // GL_GENERIC_ATTRIB_NV
    }
    if (48..=55).contains(&index) {
        return (0x8C7A_i32, (index - 48) as i32); // GL_TEXTURE_COORD_NV
    }
    const GL_POSITION: i32 = 0x1203;
    match index {
        7 => (GL_POSITION, 0),
        40 => (0x852C_i32, 0), // GL_PRIMARY_COLOR_NV
        41 => (0x852D_i32, 0), // GL_SECONDARY_COLOR_NV
        42 => (0x8C77_i32, 0), // GL_BACK_PRIMARY_COLOR_NV
        43 => (0x8C78_i32, 0), // GL_BACK_SECONDARY_COLOR_NV
        _ => {
            log::error!("Unimplemented transform feedback index={}", index);
            (GL_POSITION, 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform_feedback::{StreamOutLayout, TransformFeedbackLayout};
    use shader_recompiler::shader_info::StorageBufferDescriptor;
    use std::io::Write;

    #[test]
    fn pipeline_program_arrays_and_fence_use_upstream_raii_owners() {
        let pipeline = GraphicsPipeline::new_for_test(GraphicsPipelineKey::default(), None);
        assert!(pipeline
            .source_programs
            .iter()
            .all(|program| program.handle == 0));
        assert!(pipeline
            .assembly_programs
            .iter()
            .all(|program| program.handle == 0));
        assert!(pipeline.built_fence.handle.is_null());
        let _: &[i32; XFB_ATTRIB_COUNT] = &pipeline.xfb_attribs;
    }

    #[test]
    fn descriptor_staging_uses_upstream_fixed_arrays() {
        let views = [ImageViewInOut::default(); (MAX_TEXTURES + MAX_IMAGES) as usize];
        let samplers = [SamplerId::default(); MAX_TEXTURES as usize];
        assert_eq!(views.len(), 72);
        assert_eq!(samplers.len(), 64);
    }

    #[test]
    fn pipeline_key_xfb_bits() {
        let mut key = GraphicsPipelineKey::default();
        assert!(!key.xfb_enabled());
        assert!(!key.early_z());

        key.raw = 0b11; // xfb_enabled=1, early_z=1
        assert!(key.xfb_enabled());
        assert!(key.early_z());
    }

    #[test]
    fn pipeline_key_equality_uses_upstream_effective_size() {
        let lhs = GraphicsPipelineKey::default();
        let mut rhs = lhs;
        rhs.padding = [0x1111_1111, 0x2222_2222, 0x3333_3333];
        rhs.xfb_state.layouts[0].stride = 64;

        assert_eq!(lhs, rhs);
        assert_eq!(lhs.hash_key(), rhs.hash_key());

        rhs.set_xfb_enabled(true);
        assert_ne!(lhs, rhs);

        let mut enabled_lhs = lhs;
        enabled_lhs.set_xfb_enabled(true);
        let mut enabled_rhs = enabled_lhs;
        enabled_rhs.xfb_state.layouts[0].stride = 64;
        assert_ne!(enabled_lhs, enabled_rhs);
    }

    #[test]
    fn pipeline_key_size_varies_by_xfb() {
        let mut key = GraphicsPipelineKey::default();
        let size_no_xfb = key.size();

        key.raw = 1; // xfb_enabled
        let size_xfb = key.size();

        assert!(size_xfb > size_no_xfb);
        assert_eq!(size_xfb, std::mem::size_of::<GraphicsPipelineKey>());
    }

    #[test]
    fn pipeline_key_cache_layout_round_trips() {
        assert_eq!(std::mem::size_of::<TransformFeedbackLayout>(), 12);
        assert_eq!(std::mem::size_of::<StreamOutLayout>(), 4);
        assert_eq!(std::mem::size_of::<TransformFeedbackState>(), 560);
        assert_eq!(std::mem::size_of::<GraphicsPipelineKey>(), 624);

        let mut key = GraphicsPipelineKey::default();
        key.unique_hashes = [1, 2, 3, 4, 5, 6];
        key.raw = 0x5A5;
        key.xfb_state.layouts[0] = TransformFeedbackLayout {
            stream: 3,
            varying_count: 17,
            stride: 64,
        };
        key.xfb_state.varyings[0][0] = StreamOutLayout::from_raw(0x4433_2211);

        let path = std::env::temp_dir().join(format!(
            "ruzu-gl-graphics-key-{}-{}.bin",
            std::process::id(),
            key.hash_key()
        ));
        let key_bytes = unsafe {
            std::slice::from_raw_parts(
                (&key as *const GraphicsPipelineKey).cast::<u8>(),
                std::mem::size_of::<GraphicsPipelineKey>(),
            )
        };
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(key_bytes).unwrap();
        drop(file);
        assert_eq!(std::fs::read(&path).unwrap(), key_bytes);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn glasm_storage_buffer_selection_matches_upstream_limit() {
        let mut info = ShaderInfo::default();
        info.storage_buffers_descriptors = (0..3)
            .map(|index| StorageBufferDescriptor {
                cbuf_index: index,
                cbuf_offset: 0,
                count: 1,
                is_written: true,
            })
            .collect();
        let infos = [Some(info), None, None, None, None];

        let mut glsl = GraphicsPipeline::new_for_test(GraphicsPipelineKey::default(), None);
        glsl.program_backend = GraphicsProgramBackend::Glsl;
        glsl.max_glasm_storage_buffer_blocks = 0;
        glsl.apply_shader_infos(&infos);
        assert!(glsl.use_storage_buffers);
        assert!(!glsl.writes_global_memory);

        let mut glasm_bindless =
            GraphicsPipeline::new_for_test(GraphicsPipelineKey::default(), None);
        glasm_bindless.program_backend = GraphicsProgramBackend::Glasm;
        glasm_bindless.max_glasm_storage_buffer_blocks = 2;
        glasm_bindless.apply_shader_infos(&infos);
        assert!(!glasm_bindless.use_storage_buffers);
        assert!(glasm_bindless.writes_global_memory);

        let mut glasm_storage =
            GraphicsPipeline::new_for_test(GraphicsPipelineKey::default(), None);
        glasm_storage.program_backend = GraphicsProgramBackend::Glasm;
        glasm_storage.max_glasm_storage_buffer_blocks = 3;
        glasm_storage.apply_shader_infos(&infos);
        assert!(glasm_storage.use_storage_buffers);
        assert!(!glasm_storage.writes_global_memory);
    }

    #[test]
    fn cumulative_descriptor_counts_preserve_upstream_u32_wrapping() {
        let mut first = ShaderInfo::default();
        first
            .storage_buffers_descriptors
            .push(StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: u32::MAX,
                is_written: false,
            });
        let mut second = ShaderInfo::default();
        second
            .storage_buffers_descriptors
            .push(StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 2,
                is_written: false,
            });
        let infos = [Some(first), Some(second), None, None, None];

        let mut pipeline = GraphicsPipeline::new_for_test(GraphicsPipelineKey::default(), None);
        pipeline.program_backend = GraphicsProgramBackend::Glasm;
        pipeline.max_glasm_storage_buffer_blocks = 1;
        pipeline.apply_shader_infos(&infos);

        assert_eq!(pipeline.base_storage_bindings[1], u32::MAX);
        assert_eq!(pipeline.base_storage_bindings[2], 1);
        assert!(pipeline.use_storage_buffers);
    }

    #[test]
    fn program_build_consumes_source_staging_like_upstream_move_capture() {
        let mut pipeline = GraphicsPipeline::new_for_test(GraphicsPipelineKey::default(), None);
        pipeline.glsl_sources[0] = Some(String::new());
        pipeline.spirv_sources[1] = Some(Vec::new());

        pipeline.start_program_build(None, false);

        assert!(pipeline.glsl_sources.iter().all(Option::is_none));
        assert!(pipeline.spirv_sources.iter().all(Option::is_none));
        assert!(pipeline.is_built);
    }

    #[test]
    fn configure_spec_selection_matches_upstream_find_spec_order() {
        let vertex_only: [Option<ShaderInfo>; NUM_STAGES] =
            [Some(ShaderInfo::default()), None, None, None, None];
        assert_eq!(
            ConfigureSpec::select(&vertex_only, 1 << 0),
            ConfigureSpec::SimpleVertex
        );

        let vertex_fragment: [Option<ShaderInfo>; NUM_STAGES] = [
            Some(ShaderInfo::default()),
            None,
            None,
            None,
            Some(ShaderInfo::default()),
        ];
        assert_eq!(
            ConfigureSpec::select(&vertex_fragment, (1 << 0) | (1 << 4)),
            ConfigureSpec::SimpleVertexFragment
        );

        let mut vertex_with_storage = ShaderInfo::default();
        vertex_with_storage
            .storage_buffers_descriptors
            .push(StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: false,
            });
        let complex: [Option<ShaderInfo>; NUM_STAGES] =
            [Some(vertex_with_storage), None, None, None, None];
        let spec = ConfigureSpec::select(&complex, 1 << 0);
        assert_eq!(spec, ConfigureSpec::Default);
        assert!((0..NUM_STAGES).all(|stage| spec.enabled_stage(stage)));
        assert!(spec.has_storage_buffers());
        assert!(spec.has_texture_buffers());
        assert!(spec.has_image_buffers());
        assert!(spec.has_images());
    }

    #[test]
    fn graphics_pipeline_key_hash_matches_cityhash_over_effective_size() {
        let mut key = GraphicsPipelineKey::default();
        key.unique_hashes = [1, 2, 3, 4, 5, 6];
        key.set_early_z(true);
        key.set_gs_input_topology(5);
        key.set_tessellation_primitive(2);
        key.set_tessellation_spacing(1);
        key.set_tessellation_clockwise(true);
        key.set_app_stage(1);

        let size = key.size();
        let bytes = unsafe {
            std::slice::from_raw_parts((&key as *const GraphicsPipelineKey).cast::<u8>(), size)
        };
        assert_eq!(key.hash_key(), city_hash64(bytes));

        key.set_xfb_enabled(true);
        key.xfb_state.layouts[0].stream = 3;
        key.xfb_state.layouts[0].varying_count = 5;
        key.xfb_state.layouts[0].stride = 0x20;
        key.xfb_state.varyings[0][0] =
            crate::transform_feedback::StreamOutLayout::from_raw(0x0403_0201);

        let size = key.size();
        let bytes = unsafe {
            std::slice::from_raw_parts((&key as *const GraphicsPipelineKey).cast::<u8>(), size)
        };
        assert_eq!(key.hash_key(), city_hash64(bytes));
    }

    #[test]
    fn gl_stage_mapping() {
        assert_eq!(gl_stage(0), gl::VERTEX_SHADER);
        assert_eq!(gl_stage(4), gl::FRAGMENT_SHADER);
        assert_eq!(gl_stage(NUM_STAGES), gl::NONE);
        assert_eq!(gl_assembly_stage(NUM_STAGES), gl::NONE);
    }

    #[test]
    fn transform_feedback_generic_attrib() {
        let (token, index) = transform_feedback_enum(8 * 4);
        assert_eq!(token, 0x8C7D); // GL_GENERIC_ATTRIB_NV
        assert_eq!(index, 0);

        let (token, index) = transform_feedback_enum(39 * 4);
        assert_eq!(token, 0x8C7D);
        assert_eq!(index, 31);
    }

    #[test]
    fn generate_transform_feedback_state_groups_components_like_upstream() {
        let mut key = GraphicsPipelineKey::default();
        key.set_xfb_enabled(true);
        key.xfb_state.layouts[0].varying_count = 4;
        key.xfb_state.layouts[0].stride = 16;
        key.xfb_state.varyings[0][0] =
            crate::transform_feedback::StreamOutLayout::from_raw(0x2422_2120);

        let mut pipeline = GraphicsPipeline::new_for_test(key, None);
        pipeline.generate_transform_feedback_state();

        assert_eq!(pipeline.num_xfb_buffers_active, 1);
        assert_eq!(pipeline.num_xfb_attribs, 2);
        assert_eq!(&pipeline.xfb_attribs[..6], &[0x8C7D, 3, 0, 0x8C7D, 1, 1]);
    }

    #[test]
    fn constructor_generates_xfb_state_only_when_device_uses_assembly_shaders() {
        let mut key = GraphicsPipelineKey::default();
        key.set_xfb_enabled(true);
        key.xfb_state.layouts[0].varying_count = 1;
        key.xfb_state.layouts[0].stride = 4;
        key.xfb_state.varyings[0][0] = crate::transform_feedback::StreamOutLayout::from_raw(0);
        let infos: [Option<ShaderInfo>; NUM_STAGES] = Default::default();

        let disabled = GraphicsPipeline::new_for_test_with_sources(
            key,
            None,
            Default::default(),
            Default::default(),
            &infos,
            GraphicsProgramBackend::Glasm,
            0,
            false,
        );
        assert_eq!(disabled.num_xfb_attribs, 0);

        let enabled = GraphicsPipeline::new_for_test_with_sources(
            key,
            None,
            Default::default(),
            Default::default(),
            &infos,
            GraphicsProgramBackend::Glasm,
            0,
            true,
        );
        assert_eq!(enabled.num_xfb_attribs, 1);
    }

    #[test]
    fn transform_feedback_position() {
        let (token, _) = transform_feedback_enum(7 * 4);
        assert_eq!(token, 0x1203); // GL_POSITION
    }
}

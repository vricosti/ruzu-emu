// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/video_core/renderer_opengl/gl_shader_cache.h and gl_shader_cache.cpp
//!
//! OpenGL shader cache -- manages compilation and caching of graphics and compute pipelines.

use std::collections::HashMap;
use std::panic::{catch_unwind, resume_unwind, take_hook, AssertUnwindSafe};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use common::hash::BuildUnorderedDenseHasher;
use common::settings_enums::RendererBackend;
use common::thread_worker::StatefulThreadWorker;
use shader_recompiler::environment::Environment;
use shader_recompiler::frontend::translate_program::{
    convert_legacy_to_generic, generate_geometry_passthrough, merge_dual_vertex_programs,
};
use shader_recompiler::host_translate_info::HostTranslateInfo;
use shader_recompiler::ir::program::Program as ShaderProgram;
use shader_recompiler::ir::types::OutputTopology;
use shader_recompiler::pipeline_cache::translate_program_from_env_with_host_info;
use shader_recompiler::profile::Profile as ShaderProfile;
use shader_recompiler::runtime_info::{InputTopology, RuntimeInfo, TessPrimitive, TessSpacing};
use shader_recompiler::shader_info::Info as ShaderInfo;
use shader_recompiler::ShaderStage;

use crate::engines::kepler_compute::LaunchParams;
use crate::rasterizer_interface::{
    DiskResourceLoadCallback, DiskResourceLoadStop, LoadCallbackStage,
};
use crate::renderer_opengl::gl_graphics_pipeline::GraphicsProgramBackend;
use crate::renderer_opengl::gl_shader_context::{Context as ShaderContext, SharedContextFactory};
use crate::shader_cache::{
    GraphicsEnvironments, ShaderCache as SharedShaderCache, ShaderInfo as SharedShaderInfo,
};
use crate::shader_environment::{
    load_pipelines, serialize_pipeline, ComputeEnvironment, FileEnvironment, GraphicsEnvironment,
};
use crate::shader_notify::ShaderNotifyHandle;
use crate::transform_feedback;

fn compute_pipeline_key_bytes(key: &ComputePipelineKey) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(
            (key as *const ComputePipelineKey).cast::<u8>(),
            std::mem::size_of::<ComputePipelineKey>(),
        )
    }
}

fn graphics_pipeline_key_bytes(key: &GraphicsPipelineKey) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(
            (key as *const GraphicsPipelineKey).cast::<u8>(),
            std::mem::size_of::<GraphicsPipelineKey>(),
        )
    }
}

fn read_compute_pipeline_key(file: &mut std::fs::File) -> std::io::Result<ComputePipelineKey> {
    use std::io::Read;

    let mut key = ComputePipelineKey::default();
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(
            (&mut key as *mut ComputePipelineKey).cast::<u8>(),
            std::mem::size_of::<ComputePipelineKey>(),
        )
    };
    file.read_exact(bytes)?;
    Ok(key)
}

fn read_graphics_pipeline_key(file: &mut std::fs::File) -> std::io::Result<GraphicsPipelineKey> {
    use std::io::Read;

    let mut key = GraphicsPipelineKey::default();
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(
            (&mut key as *mut GraphicsPipelineKey).cast::<u8>(),
            std::mem::size_of::<GraphicsPipelineKey>(),
        )
    };
    file.read_exact(bytes)?;
    Ok(key)
}

/// One-time installation of the OpenGL shader-exception panic-hook filter.
/// Upstream catches `Shader::Exception` at both pipeline creation boundaries;
/// typed Rust shader panics are the direct equivalent.
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

/// Rust equivalent of `catch (Shader::Exception&)` in Eden's OpenGL
/// `CreateGraphicsPipeline` and `CreateComputePipeline`. Non-shader panics
/// remain fatal to the worker and are resumed unchanged.
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

use super::gl_buffer_cache::BufferCache as OpenGLBufferCache;
use super::gl_compute_pipeline::{ComputePipeline, ComputePipelineKey, ComputeProgramBackend};
use super::gl_device::Device;
use super::gl_graphics_pipeline::{
    GraphicsPipeline, GraphicsPipelineKey, NUM_STAGES as NUM_GRAPHICS_STAGES,
};
use super::gl_shader_manager::ProgramManagerHandle;
use super::gl_state_tracker::StateTracker;
use super::gl_texture_cache::TextureCache as OpenGLTextureCache;

/// Cache version for serialized pipeline data.
const CACHE_VERSION: u32 = 15;

/// Port of the OpenGL-specific `Shader::Profile` construction in upstream
/// `gl_shader_cache.cpp`.
fn opengl_shader_profile(device: &Device) -> ShaderProfile {
    ShaderProfile {
        support_int64: device.has_shader_int64(),
        support_vertex_instance_id: true,
        support_vote: true,
        support_viewport_index_layer_non_geometry: device.has_nv_viewport_array2()
            || device.has_vertex_viewport_layer(),
        support_viewport_mask: device.has_nv_viewport_array2(),
        support_typeless_image_loads: device.has_image_load_formatted(),
        support_demote_to_helper_invocation: false,
        support_derivative_control: device.has_derivative_control(),
        support_geometry_shader_passthrough: device.has_geometry_shader_passthrough(),
        support_native_ndc: true,
        support_gl_nv_gpu_shader_5: device.has_nv_gpu_shader5(),
        support_gl_amd_gpu_shader_half_float: device.has_amd_shader_half_float(),
        support_gl_texture_shadow_lod: device.has_texture_shadow_lod(),
        support_gl_warp_intrinsics: false,
        support_gl_variable_aoffi: device.has_variable_aoffi(),
        support_gl_sparse_textures: device.has_sparse_texture2(),
        support_gl_derivative_control: device.has_derivative_control(),
        support_geometry_streams: true,
        warp_size_potentially_larger_than_guest: device
            .is_warp_size_potentially_larger_than_guest(),
        lower_left_origin_mode: true,
        need_declared_frag_colors: true,
        need_fastmath_off: device.needs_fastmath_off(),
        need_gather_subpixel_offset: device.is_amd() || device.is_intel(),
        has_broken_spirv_clamp: true,
        has_broken_unsigned_image_offsets: true,
        has_broken_signed_operations: true,
        has_broken_fp16_float_controls: false,
        has_gl_component_indexing_bug: device.has_component_indexing_bug(),
        has_gl_precise_bug: device.has_precise_bug(),
        has_gl_cbuf_ftou_bug: device.has_cbuf_ftou_bug(),
        has_gl_bool_ref_bug: device.has_bool_ref_bug(),
        ignore_nan_fp_comparisons: true,
        gl_max_compute_smem_size: device.max_compute_shared_memory_size(),
        min_ssbo_alignment: device.shader_storage_buffer_alignment() as u64,
        max_user_clip_distances: device
            .max_user_clip_distances()
            .min(crate::engines::maxwell_3d::NUM_CLIP_DISTANCES),
        ..ShaderProfile::default()
    }
}

fn opengl_host_translate_info(device: &Device) -> HostTranslateInfo {
    let mut host_info = HostTranslateInfo {
        min_ssbo_alignment: device.shader_storage_buffer_alignment() as u64,
        max_per_stage_descriptor_sampled_images: HostTranslateInfo::DEFAULT_DESCRIPTOR_LIMIT,
        max_per_stage_resources: HostTranslateInfo::DEFAULT_DESCRIPTOR_LIMIT,
        max_descriptor_set_samplers: HostTranslateInfo::DEFAULT_DESCRIPTOR_LIMIT,
        max_descriptor_set_uniform_buffers: HostTranslateInfo::DEFAULT_DESCRIPTOR_LIMIT,
        max_descriptor_set_uniform_buffers_dynamic: HostTranslateInfo::DEFAULT_DESCRIPTOR_LIMIT,
        max_descriptor_set_storage_buffers: HostTranslateInfo::DEFAULT_DESCRIPTOR_LIMIT,
        max_descriptor_set_storage_buffers_dynamic: HostTranslateInfo::DEFAULT_DESCRIPTOR_LIMIT,
        max_descriptor_set_sampled_images: HostTranslateInfo::DEFAULT_DESCRIPTOR_LIMIT,
        max_descriptor_set_storage_images: HostTranslateInfo::DEFAULT_DESCRIPTOR_LIMIT,
        max_descriptor_set_input_attachements: HostTranslateInfo::DEFAULT_DESCRIPTOR_LIMIT,
        support_float64: true,
        support_float16: false,
        support_int64: device.has_shader_int64(),
        needs_demote_reorder: device.is_amd(),
        support_snorm_render_buffer: false,
        support_viewport_index_layer: device.has_vertex_viewport_layer(),
        support_geometry_shader_passthrough: device.has_geometry_shader_passthrough(),
        support_conditional_barrier: device.supports_conditional_barriers(),
    };
    host_info.apply_descriptor_limit_policy();
    host_info
}

fn make_compute_runtime_info(
    info: &ShaderInfo,
    max_glasm_storage_buffer_blocks: u32,
) -> RuntimeInfo {
    let mut runtime_info = RuntimeInfo::default();
    let num_storage_buffers =
        shader_recompiler::shader_info::num_descriptors(&info.storage_buffers_descriptors);
    runtime_info.glasm_use_storage_buffers = num_storage_buffers <= max_glasm_storage_buffer_blocks;
    runtime_info
}

/// Mechanical form of the backend `switch` in Eden's compute-pipeline
/// creation path. Capability validation belongs to `Device`; it must not
/// silently replace a requested GLASM pipeline with GLSL here.
fn compute_program_backend(shader_backend: RendererBackend) -> ComputeProgramBackend {
    match shader_backend {
        RendererBackend::OpenGlGlsl => ComputeProgramBackend::Glsl,
        RendererBackend::OpenGlGlasm => ComputeProgramBackend::Glasm,
        RendererBackend::OpenGlSpirV => ComputeProgramBackend::SpirV,
        _ => unreachable!("OpenGL shader cache requires an OpenGL backend"),
    }
}

#[cfg(test)]
fn test_opengl_shader_profile() -> ShaderProfile {
    ShaderProfile {
        support_vertex_instance_id: true,
        support_vote: true,
        support_native_ndc: true,
        support_geometry_streams: true,
        lower_left_origin_mode: true,
        need_declared_frag_colors: true,
        has_broken_spirv_clamp: true,
        has_broken_unsigned_image_offsets: true,
        has_broken_signed_operations: true,
        ignore_nan_fp_comparisons: true,
        max_user_clip_distances: crate::engines::maxwell_3d::NUM_CLIP_DISTANCES,
        ..ShaderProfile::default()
    }
}

/// OpenGL shader cache.
///
/// Corresponds to `OpenGL::ShaderCache`.
pub struct ShaderCache {
    /// Stable non-owning counterparts of Eden's constructor-owned cache and
    /// manager references. `RasterizerOpenGL` owns the boxed caches and the
    /// renderer owns the state tracker for longer than this shader cache.
    texture_cache: Option<NonNull<OpenGLTextureCache>>,
    buffer_cache: Option<NonNull<OpenGLBufferCache>>,
    program_manager: Option<ProgramManagerHandle>,
    state_tracker: Option<NonNull<StateTracker>>,
    /// Whether to use asynchronous shader compilation.
    use_asynchronous_shaders: bool,
    /// Whether a strict GL context is required for compilation.
    strict_context_required: bool,
    profile: ShaderProfile,
    host_info: HostTranslateInfo,
    use_assembly_shaders: bool,
    max_glasm_storage_buffer_blocks: u32,

    /// Current graphics pipeline key.
    graphics_key: GraphicsPipelineKey,
    /// Currently bound graphics pipeline (key lookup).
    current_pipeline: Option<GraphicsPipelineKey>,

    /// Upstream `ShaderWorker`, with one shared OpenGL context per worker.
    ///
    /// This field intentionally precedes the pipeline caches: Rust drops fields
    /// in declaration order, whereas Eden declares `workers` last and destroys
    /// members in reverse declaration order. Both therefore stop and join the
    /// workers before destroying cached pipelines.
    workers: Option<StatefulThreadWorker<ShaderContext>>,
    context_factory: Option<SharedContextFactory>,
    /// Non-owning upstream `VideoCore::ShaderNotify&`.
    shader_notify: Option<ShaderNotifyHandle>,

    /// Cache of compiled graphics pipelines.
    graphics_cache:
        HashMap<GraphicsPipelineKey, Option<Box<GraphicsPipeline>>, BuildUnorderedDenseHasher>,
    /// Cache of compiled compute pipelines.
    compute_cache:
        HashMap<ComputePipelineKey, Option<Box<ComputePipeline>>, BuildUnorderedDenseHasher>,

    /// Path to the on-disk shader cache file.
    shader_cache_filename: PathBuf,
}

#[derive(Clone)]
struct DiskBuildConfig {
    profile: ShaderProfile,
    host_info: HostTranslateInfo,
    use_assembly_shaders: bool,
    max_glasm_storage_buffer_blocks: u32,
    shader_notify: Option<ShaderNotifyHandle>,
    texture_cache: Option<NonNull<OpenGLTextureCache>>,
    buffer_cache: Option<NonNull<OpenGLBufferCache>>,
    program_manager: Option<ProgramManagerHandle>,
    state_tracker: Option<NonNull<StateTracker>>,
}

// The pointers are the same stable, renderer-thread-owned references that
// Eden captures through `this` while shader workers compile pipeline objects.
// Worker code never dereferences the caches; the completed pipeline returns to
// the renderer thread before Configure uses them.
unsafe impl Send for DiskBuildConfig {}

enum DiskPipelineBuildResult {
    Compute(ComputePipelineKey, ComputePipeline),
    Graphics(GraphicsPipelineKey, GraphicsPipeline),
}

#[derive(Default)]
struct DiskResourceLoadState {
    total: usize,
    built: usize,
    has_loaded: bool,
}

impl DiskResourceLoadState {
    fn complete_one(&mut self, callback: &DiskResourceLoadCallback) {
        self.built += 1;
        if self.has_loaded {
            callback(LoadCallbackStage::Build, self.built, self.total);
        }
    }
}

impl ShaderCache {
    fn disk_worker_cache(config: &DiskBuildConfig) -> Self {
        let mut cache = Self::new_with_profile(
            config.profile.clone(),
            config.host_info.clone(),
            false,
            false,
        );
        cache.use_assembly_shaders = config.use_assembly_shaders;
        cache.max_glasm_storage_buffer_blocks = config.max_glasm_storage_buffer_blocks;
        cache.shader_notify = config.shader_notify;
        cache.texture_cache = config.texture_cache;
        cache.buffer_cache = config.buffer_cache;
        cache.program_manager = config.program_manager.clone();
        cache.state_tracker = config.state_tracker;
        cache
    }

    fn graphics_environments_from_files(envs: Vec<FileEnvironment>) -> GraphicsEnvironments {
        let mut environments = GraphicsEnvironments::default();
        for env in envs {
            let slot = match env.shader_stage() {
                ShaderStage::VertexA => 0,
                ShaderStage::VertexB => 1,
                ShaderStage::TessellationControl => 2,
                ShaderStage::TessellationEval => 3,
                ShaderStage::Geometry => 4,
                ShaderStage::Fragment => 5,
                ShaderStage::Compute => continue,
            };
            environments.envs[slot] = GraphicsEnvironment::from_file_environment(env);
            environments.env_ptrs[slot] = Some(slot);
        }
        environments
    }

    /// Create a new shader cache.
    ///
    /// Corresponds to `ShaderCache::ShaderCache()`.
    pub fn new(
        device: &Device,
        texture_cache: &mut OpenGLTextureCache,
        buffer_cache: &mut OpenGLBufferCache,
        program_manager: ProgramManagerHandle,
        state_tracker: &mut StateTracker,
        context_factory: Option<SharedContextFactory>,
        shader_notify: ShaderNotifyHandle,
    ) -> Self {
        let mut cache = Self::new_with_profile(
            opengl_shader_profile(device),
            opengl_host_translate_info(device),
            device.use_asynchronous_shaders(),
            device.strict_context_required(),
        );
        cache.use_assembly_shaders = device.use_assembly_shaders();
        cache.max_glasm_storage_buffer_blocks = device.max_glasm_storage_buffer_blocks();
        cache.shader_notify = Some(shader_notify);
        cache.texture_cache = Some(NonNull::from(texture_cache));
        cache.buffer_cache = Some(NonNull::from(buffer_cache));
        cache.program_manager = Some(program_manager);
        cache.state_tracker = Some(NonNull::from(state_tracker));
        cache.context_factory = context_factory.clone();
        if cache.use_asynchronous_shaders {
            if let Some(factory) = context_factory {
                let worker_count = std::thread::available_parallelism()
                    .map_or(1, usize::from)
                    .max(2)
                    - 1;
                cache.workers = Some(StatefulThreadWorker::new(
                    worker_count,
                    "GlShaderBuilder".to_string(),
                    move || ShaderContext::new(&factory),
                ));
            } else {
                log::warn!(
                    "OpenGL asynchronous shaders requested without a shared-context factory"
                );
                cache.use_asynchronous_shaders = false;
            }
        }
        cache
    }

    fn new_with_profile(
        profile: ShaderProfile,
        host_info: HostTranslateInfo,
        use_asynchronous_shaders: bool,
        strict_context_required: bool,
    ) -> Self {
        Self {
            texture_cache: None,
            buffer_cache: None,
            program_manager: None,
            state_tracker: None,
            use_asynchronous_shaders,
            strict_context_required,
            profile,
            host_info,
            use_assembly_shaders: false,
            max_glasm_storage_buffer_blocks: 0,
            graphics_key: GraphicsPipelineKey::default(),
            current_pipeline: None,
            workers: None,
            context_factory: None,
            shader_notify: None,
            graphics_cache: HashMap::with_hasher(BuildUnorderedDenseHasher),
            compute_cache: HashMap::with_hasher(BuildUnorderedDenseHasher),
            shader_cache_filename: PathBuf::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test() -> Self {
        Self::new_with_profile(
            test_opengl_shader_profile(),
            HostTranslateInfo::default(),
            false,
            false,
        )
    }

    /// Load disk resources for a given title.
    ///
    /// Port of `ShaderCache::LoadDiskResources()`.
    ///
    /// Deserializes the cache, rebuilds pipelines on the main or shared GL
    /// contexts, and reports upstream-compatible progress.
    pub fn load_disk_resources(
        &mut self,
        title_id: u64,
        stop_loading: DiskResourceLoadStop,
        callback: DiskResourceLoadCallback,
    ) {
        if title_id == 0 {
            return;
        }
        let shader_dir =
            common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::ShaderDir);
        let base_dir = shader_dir.join(format!("{:016x}", title_id));
        if let Err(error) = std::fs::create_dir_all(&base_dir) {
            log::error!("Failed to create shader cache directories: {error}");
            return;
        }
        self.shader_cache_filename = base_dir.join("opengl.bin");

        use std::cell::RefCell;
        let compute_entries = RefCell::new(Vec::<(ComputePipelineKey, FileEnvironment)>::new());
        let graphics_entries =
            RefCell::new(Vec::<(GraphicsPipelineKey, Vec<FileEnvironment>)>::new());
        load_pipelines(
            || stop_loading.load(Ordering::Acquire),
            &self.shader_cache_filename,
            CACHE_VERSION,
            Box::new(|file, env| {
                let key = read_compute_pipeline_key(file)?;
                compute_entries.borrow_mut().push((key, env));
                Ok(())
            }),
            Box::new(|file, envs| {
                let key = read_graphics_pipeline_key(file)?;
                graphics_entries.borrow_mut().push((key, envs));
                Ok(())
            }),
        );

        let compute_entries = compute_entries.into_inner();
        let graphics_entries = graphics_entries.into_inner();
        let total = compute_entries.len() + graphics_entries.len();

        if self.workers.is_none() && !self.strict_context_required {
            let factory = self
                .context_factory
                .clone()
                .expect("OpenGL shader loading requires the renderer's shared-context factory");
            let worker_count = std::thread::available_parallelism()
                .map_or(1, usize::from)
                .max(2)
                - 1;
            self.workers = Some(StatefulThreadWorker::new(
                worker_count,
                "GlShaderBuilder".to_string(),
                move || ShaderContext::new(&factory),
            ));
        }

        if self.strict_context_required {
            let factory = self
                .context_factory
                .as_ref()
                .expect("strict OpenGL shader loading requires a shared-context factory");
            let mut strict_context = ShaderContext::new(factory);
            for (key, env) in compute_entries {
                strict_context.pools.release_contents();
                let mut env = ComputeEnvironment::from_file_environment(env);
                if let Some(pipeline) =
                    self.create_compute_pipeline_from_environment(&key, &mut env, true)
                {
                    self.compute_cache
                        .entry(key)
                        .or_insert(Some(Box::new(pipeline)));
                }
            }
            for (key, envs) in graphics_entries {
                strict_context.pools.release_contents();
                let mut environments = Self::graphics_environments_from_files(envs);
                let saved_key = std::mem::replace(&mut self.graphics_key, key);
                let pipeline =
                    self.create_graphics_pipeline_from_environments(&mut environments, false, true);
                self.graphics_key = saved_key;
                if let Some(pipeline) = pipeline {
                    self.graphics_cache
                        .entry(key)
                        .or_insert(Some(Box::new(pipeline)));
                }
            }
            log::info!("Total OpenGL pipeline count: {total}");
            callback(LoadCallbackStage::Build, 0, total);
            return;
        } else {
            let config = DiskBuildConfig {
                profile: self.profile.clone(),
                host_info: self.host_info.clone(),
                use_assembly_shaders: self.use_assembly_shaders,
                max_glasm_storage_buffer_blocks: self.max_glasm_storage_buffer_blocks,
                shader_notify: self.shader_notify,
                texture_cache: self.texture_cache,
                buffer_cache: self.buffer_cache,
                program_manager: self.program_manager.clone(),
                state_tracker: self.state_tracker,
            };
            let results = Arc::new(Mutex::new(Vec::<DiskPipelineBuildResult>::new()));
            let state = Arc::new(Mutex::new(DiskResourceLoadState::default()));
            let workers = self.workers.as_ref().expect("OpenGL shader workers");
            let mut queued_total = 0usize;

            for (key, env) in compute_entries {
                let config = config.clone();
                let results = Arc::clone(&results);
                let state = Arc::clone(&state);
                let callback = Arc::clone(&callback);
                workers.queue_work(move |context| {
                    context.pools.release_contents();
                    let mut cache = Self::disk_worker_cache(&config);
                    let mut env = ComputeEnvironment::from_file_environment(env);
                    if let Some(pipeline) =
                        cache.create_compute_pipeline_from_environment(&key, &mut env, true)
                    {
                        results
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(DiskPipelineBuildResult::Compute(key, pipeline));
                    }
                    state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .complete_one(&callback);
                });
                queued_total += 1;
            }

            for (key, envs) in graphics_entries {
                let config = config.clone();
                let results = Arc::clone(&results);
                let state = Arc::clone(&state);
                let callback = Arc::clone(&callback);
                workers.queue_work(move |context| {
                    context.pools.release_contents();
                    let mut cache = Self::disk_worker_cache(&config);
                    cache.graphics_key = key;
                    let mut environments = Self::graphics_environments_from_files(envs);
                    if let Some(pipeline) = cache.create_graphics_pipeline_from_environments(
                        &mut environments,
                        false,
                        true,
                    ) {
                        results
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(DiskPipelineBuildResult::Graphics(key, pipeline));
                    }
                    state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .complete_one(&callback);
                });
                queued_total += 1;
            }

            log::info!("Total OpenGL pipeline count: {queued_total}");
            {
                let mut state = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.total = queued_total;
                callback(LoadCallbackStage::Build, 0, queued_total);
                state.has_loaded = true;
            }
            workers.wait_for_requests_or_stop(&stop_loading);
            for result in results
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .drain(..)
            {
                match result {
                    DiskPipelineBuildResult::Compute(key, pipeline) => {
                        self.compute_cache
                            .entry(key)
                            .or_insert(Some(Box::new(pipeline)));
                    }
                    DiskPipelineBuildResult::Graphics(key, pipeline) => {
                        self.graphics_cache
                            .entry(key)
                            .or_insert(Some(Box::new(pipeline)));
                    }
                }
            }
        }
        if !self.use_asynchronous_shaders {
            self.workers = None;
        }
    }

    /// Shared-owner runtime path matching upstream `OpenGL::ShaderCache`'s
    /// inherited `VideoCommon::ShaderCache` usage more closely than the local
    /// address-only fallback.
    pub fn current_graphics_pipeline(
        &mut self,
        shared_cache: &mut SharedShaderCache,
    ) -> Option<&mut GraphicsPipeline> {
        if !shared_cache.refresh_stages(&mut self.graphics_key.unique_hashes) {
            self.current_pipeline = None;
            return None;
        }

        let maxwell3d = shared_cache.current_maxwell3d()?;
        self.graphics_key.raw = 0;
        self.graphics_key.set_early_z(maxwell3d.mandated_early_z());
        self.graphics_key
            .set_gs_input_topology(maxwell3d.draw_manager_topology() as u32);
        self.graphics_key
            .set_tessellation_primitive(maxwell3d.tessellation_domain_type());
        self.graphics_key
            .set_tessellation_spacing(maxwell3d.tessellation_spacing());
        self.graphics_key
            .set_tessellation_clockwise(maxwell3d.tessellation_clockwise());
        self.graphics_key
            .set_xfb_enabled(maxwell3d.transform_feedback_enabled());
        self.graphics_key
            .set_app_stage(maxwell3d.engine_state() as u32);
        if self.graphics_key.xfb_enabled() {
            self.graphics_key.xfb_state = maxwell3d.transform_feedback_state();
        }

        let key = self.graphics_key;
        let maxwell3d = shared_cache.current_maxwell3d();

        if self.current_pipeline == Some(key) {
            let pipeline = self
                .graphics_cache
                .get_mut(&key)
                .and_then(Option::as_deref_mut)
                .expect("current OpenGL pipeline must remain owned by graphics_cache");
            return Self::built_pipeline(self.use_asynchronous_shaders, maxwell3d, pipeline);
        }
        self.current_graphics_pipeline_slow_path(shared_cache)
    }

    /// Port of `OpenGL::ShaderCache::CurrentComputePipeline()`.
    pub fn current_compute_pipeline(
        &mut self,
        shared_cache: &mut SharedShaderCache,
    ) -> Option<&mut ComputePipeline> {
        let (shader_hash, shader_size) = {
            let shader = shared_cache.compute_shader()?;
            (shader.unique_hash, shader.size_bytes)
        };
        let qmd = shared_cache.current_kepler_compute()?.launch_description();
        let key = Self::compute_pipeline_key_from_shader_and_qmd(
            SharedShaderInfo {
                unique_hash: shader_hash,
                size_bytes: shader_size,
            },
            qmd,
        );
        if self.compute_cache.contains_key(&key) {
            return self.compute_cache.get_mut(&key)?.as_deref_mut();
        }
        self.compute_cache.insert(key, None);
        let pipeline = self.create_compute_pipeline(shared_cache, &key, shader_size);
        if let Some(pipeline) = pipeline {
            self.compute_cache.insert(key, Some(Box::new(pipeline)));
        }
        self.compute_cache.get_mut(&key)?.as_deref_mut()
    }

    fn current_graphics_pipeline_slow_path(
        &mut self,
        shared_cache: &mut SharedShaderCache,
    ) -> Option<&mut GraphicsPipeline> {
        let key = self.graphics_key;
        let maxwell3d = shared_cache.current_maxwell3d();

        if self.graphics_cache.contains_key(&key) {
            let pipeline = self.graphics_cache.get_mut(&key).unwrap().as_deref_mut()?;
            self.current_pipeline = Some(key);
            return Self::built_pipeline(self.use_asynchronous_shaders, maxwell3d, pipeline);
        }
        self.graphics_cache.insert(key, None);
        let pipeline = self.create_graphics_pipeline(shared_cache);
        if let Some(pipeline) = pipeline {
            self.graphics_cache.insert(key, Some(Box::new(pipeline)));
        }
        let inserted = self.graphics_cache.get_mut(&key)?.as_deref_mut()?;
        self.current_pipeline = Some(key);
        let result = Self::built_pipeline(self.use_asynchronous_shaders, maxwell3d, inserted);
        result
    }

    /// Check if a pipeline is built (or if async shaders should return None).
    fn built_pipeline<'a>(
        use_asynchronous_shaders: bool,
        maxwell3d: Option<&crate::engines::maxwell_3d::Maxwell3D>,
        pipeline: &'a mut GraphicsPipeline,
    ) -> Option<&'a mut GraphicsPipeline> {
        if pipeline.is_built() {
            return Some(pipeline);
        }
        if !use_asynchronous_shaders {
            return Some(pipeline);
        }
        let Some(maxwell3d) = maxwell3d else {
            return None;
        };
        let draw_state = maxwell3d.draw_manager_state();
        if draw_state.index_buffer.count <= 6 || draw_state.vertex_buffer.count <= 6 {
            return Some(pipeline);
        }
        None
    }

    fn create_graphics_pipeline(
        &mut self,
        shared_cache: &SharedShaderCache,
    ) -> Option<GraphicsPipeline> {
        let mut environments = GraphicsEnvironments::default();
        shared_cache.get_graphics_environments(&mut environments, &self.graphics_key.unique_hashes);
        let pipeline = self.create_graphics_pipeline_from_environments(
            &mut environments,
            self.use_asynchronous_shaders,
            false,
        );
        if pipeline.is_some() && !self.shader_cache_filename.as_os_str().is_empty() {
            let envs = environments.span();
            serialize_pipeline(
                graphics_pipeline_key_bytes(&self.graphics_key),
                &envs,
                &self.shader_cache_filename,
                CACHE_VERSION,
            );
        }
        pipeline
    }

    fn create_graphics_pipeline_from_environments(
        &mut self,
        environments: &mut GraphicsEnvironments,
        use_shader_workers: bool,
        force_context_flush: bool,
    ) -> Option<GraphicsPipeline> {
        match catch_shader_exception(|| {
            self.create_graphics_pipeline_from_environments_unchecked(
                environments,
                use_shader_workers,
                force_context_flush,
            )
        }) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                log::error!("{error}");
                None
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn make_graphics_pipeline(
        &self,
        glsl_sources: [Option<String>; NUM_GRAPHICS_STAGES],
        spirv_sources: [Option<Vec<u32>>; NUM_GRAPHICS_STAGES],
        infos: &[Option<ShaderInfo>; NUM_GRAPHICS_STAGES],
        program_backend: GraphicsProgramBackend,
        use_shader_workers: bool,
        force_context_flush: bool,
    ) -> GraphicsPipeline {
        let thread_worker = if use_shader_workers {
            Some(
                self.workers
                    .as_ref()
                    .expect("asynchronous OpenGL pipeline creation requires shader workers"),
            )
        } else {
            None
        };
        match (
            self.texture_cache,
            self.buffer_cache,
            self.program_manager.as_ref(),
            self.state_tracker,
        ) {
            (
                Some(texture_cache),
                Some(buffer_cache),
                Some(program_manager),
                Some(state_tracker),
            ) => GraphicsPipeline::new(
                texture_cache,
                buffer_cache,
                program_manager.clone(),
                state_tracker,
                thread_worker,
                self.shader_notify,
                glsl_sources,
                spirv_sources,
                infos,
                self.graphics_key,
                program_backend,
                self.max_glasm_storage_buffer_blocks,
                self.use_assembly_shaders,
                force_context_flush,
            ),
            #[cfg(test)]
            _ => GraphicsPipeline::new_for_test_with_sources(
                self.graphics_key,
                self.shader_notify,
                glsl_sources,
                spirv_sources,
                infos,
                program_backend,
                self.max_glasm_storage_buffer_blocks,
                self.use_assembly_shaders,
            ),
            #[cfg(not(test))]
            _ => unreachable!("production OpenGL ShaderCache must retain all pipeline owners"),
        }
    }

    fn create_graphics_pipeline_from_environments_unchecked(
        &mut self,
        environments: &mut GraphicsEnvironments,
        use_shader_workers: bool,
        force_context_flush: bool,
    ) -> Option<GraphicsPipeline> {
        let pipeline_hash = self.graphics_key.hash_key();
        log::info!("{:#016x}", pipeline_hash);

        let uses_vertex_a = self.graphics_key.unique_hashes[0] != 0;
        let uses_vertex_b = self.graphics_key.unique_hashes[1] != 0;
        let dump_guest_shaders = *common::settings::values().dump_guest_shaders.get_value();
        let mut programs: [Option<ShaderProgram>; 6] = std::array::from_fn(|_| None);
        let mut total_storage_buffers = 0u32;
        let mut layer_source_program: Option<ShaderProgram> = None;

        for index in 0..6 {
            let is_emulated_stage = layer_source_program.is_some()
                && index == crate::engines::maxwell_3d::ShaderStageType::Geometry as usize;
            if self.graphics_key.unique_hashes[index] == 0 && is_emulated_stage {
                programs[index] = Some(generate_geometry_passthrough(
                    &self.host_info,
                    layer_source_program
                        .as_ref()
                        .expect("layer source checked by is_emulated_stage"),
                    Self::maxwell_to_output_topology(self.graphics_key.gs_input_topology()),
                ));
                continue;
            }
            if self.graphics_key.unique_hashes[index] == 0 {
                continue;
            }

            let env = &mut environments.envs[index];
            if env.generic_environment().cached_code_slice().is_empty()
                && env.generic_environment_mut().analyze().is_none()
            {
                log::error!(
                    "OpenGL shader environment analysis failed for {:?}",
                    env.generic_environment().shader_stage()
                );
                return None;
            }
            if dump_guest_shaders {
                env.dump(pipeline_hash, self.graphics_key.unique_hashes[index]);
            }
            let code = env
                .generic_environment()
                .cached_instruction_slice()
                .to_vec();
            let start = env.generic_environment().cached_instruction_start();

            if !uses_vertex_a || index != 1 {
                let program =
                    translate_program_from_env_with_host_info(&code, start, env, &self.host_info);
                total_storage_buffers += shader_recompiler::shader_info::num_descriptors(
                    &program.info.storage_buffers_descriptors,
                );
                programs[index] = Some(program);
            } else {
                let mut program_va = programs[0]
                    .take()
                    .expect("VertexA must be translated before VertexB");
                let mut program_vb =
                    translate_program_from_env_with_host_info(&code, start, env, &self.host_info);
                total_storage_buffers += shader_recompiler::shader_info::num_descriptors(
                    &program_vb.info.storage_buffers_descriptors,
                );
                programs[index] = Some(merge_dual_vertex_programs(
                    &mut program_va,
                    &mut program_vb,
                    env,
                ));
            }

            if programs[index]
                .as_ref()
                .is_some_and(|program| program.info.requires_layer_emulation)
            {
                layer_source_program = programs[index].clone();
            }
        }

        let glasm_use_storage_buffers =
            total_storage_buffers <= self.max_glasm_storage_buffer_blocks;
        let program_backend = match *common::settings::values().renderer_backend.get_value() {
            RendererBackend::OpenGlGlsl => GraphicsProgramBackend::Glsl,
            RendererBackend::OpenGlGlasm => GraphicsProgramBackend::Glasm,
            RendererBackend::OpenGlSpirV => GraphicsProgramBackend::SpirV,
            _ => unreachable!("OpenGL shader cache requires an OpenGL backend"),
        };
        let mut infos: [Option<ShaderInfo>; NUM_GRAPHICS_STAGES] = Default::default();
        let mut glsl_sources: [Option<String>; NUM_GRAPHICS_STAGES] = Default::default();
        let mut spirv_sources: [Option<Vec<u32>>; NUM_GRAPHICS_STAGES] = Default::default();
        let mut bindings = shader_recompiler::backend::bindings::Bindings::default();
        let mut previous_info: Option<ShaderInfo> = None;
        let first_index = if uses_vertex_a && uses_vertex_b { 1 } else { 0 };

        for index in first_index..6 {
            let is_emulated_stage = layer_source_program.is_some()
                && index == crate::engines::maxwell_3d::ShaderStageType::Geometry as usize;
            if self.graphics_key.unique_hashes[index] == 0 && !is_emulated_stage {
                continue;
            }
            if index == 0 {
                log::error!("OpenGL VertexA without VertexB is not supported upstream");
                return None;
            }

            let program = programs[index]
                .as_mut()
                .expect("translated or generated graphics program must exist");
            let stage_index = index - 1;
            let runtime_info = Self::make_runtime_info(
                &self.graphics_key,
                program.stage,
                previous_info.as_ref(),
                glasm_use_storage_buffers,
                self.use_assembly_shaders,
            );
            match program_backend {
                GraphicsProgramBackend::Glsl => {
                    convert_legacy_to_generic(program, &runtime_info);
                    glsl_sources[stage_index] = Some(shader_recompiler::backend::glsl::emit_glsl(
                        &self.profile,
                        &runtime_info,
                        program,
                        &mut bindings,
                    ));
                }
                GraphicsProgramBackend::Glasm => {
                    glsl_sources[stage_index] =
                        Some(shader_recompiler::backend::glasm::emit_glasm(
                            &self.profile,
                            &runtime_info,
                            program,
                            &mut bindings,
                        ));
                }
                GraphicsProgramBackend::SpirV => {
                    convert_legacy_to_generic(program, &runtime_info);
                    spirv_sources[stage_index] =
                        Some(shader_recompiler::backend::emit_spirv_with_bindings(
                            program,
                            &self.profile,
                            &runtime_info,
                            &mut bindings,
                        ));
                }
            }
            infos[stage_index] = Some(program.info.clone());
            previous_info = infos[stage_index].clone();
        }

        Some(self.make_graphics_pipeline(
            glsl_sources,
            spirv_sources,
            &infos,
            program_backend,
            use_shader_workers,
            force_context_flush,
        ))
    }

    /// Port of upstream `OpenGL::MakeRuntimeInfo(...)` in
    /// `gl_shader_cache.cpp`.
    fn make_runtime_info(
        key: &GraphicsPipelineKey,
        stage: ShaderStage,
        previous_program: Option<&ShaderInfo>,
        glasm_use_storage_buffers: bool,
        use_assembly_shaders: bool,
    ) -> RuntimeInfo {
        let mut info = RuntimeInfo::default();
        if let Some(previous_program) = previous_program {
            info.previous_stage_stores = previous_program.stores.clone();
            info.previous_stage_legacy_stores_mapping =
                previous_program.legacy_stores_mapping.clone();
        } else {
            // Mark all stores as available for vertex shaders.
            info.previous_stage_stores.mask.fill(u64::MAX);
        }

        match stage {
            ShaderStage::VertexB | ShaderStage::Geometry => {
                if !use_assembly_shaders && key.xfb_enabled() {
                    let (varyings, count) =
                        transform_feedback::make_transform_feedback_varyings(&key.xfb_state);
                    info.xfb_varyings = varyings
                        .iter()
                        .map(
                            |varying| shader_recompiler::runtime_info::TransformFeedbackVarying {
                                buffer: varying.buffer,
                                stream: varying.stream,
                                stride: varying.stride,
                                offset: varying.offset,
                                components: varying.components,
                            },
                        )
                        .collect();
                    info.xfb_count = count;
                }
            }
            ShaderStage::TessellationEval => {
                info.tess_clockwise = !key.tessellation_clockwise();
                info.tess_primitive = match key.tessellation_primitive() {
                    0 => TessPrimitive::Isolines,
                    1 => TessPrimitive::Triangles,
                    2 => TessPrimitive::Quads,
                    value => {
                        log::error!("Invalid Maxwell tessellation domain type {value}");
                        TessPrimitive::Triangles
                    }
                };
                info.tess_spacing = match key.tessellation_spacing() {
                    0 => TessSpacing::Equal,
                    1 => TessSpacing::FractionalOdd,
                    2 => TessSpacing::FractionalEven,
                    value => {
                        log::error!("Invalid Maxwell tessellation spacing {value}");
                        TessSpacing::Equal
                    }
                };
            }
            ShaderStage::Fragment => {
                info.force_early_z = key.early_z();
            }
            _ => {}
        }

        info.input_topology = match key.gs_input_topology() {
            0 => InputTopology::Points,
            1 | 2 | 3 => InputTopology::Lines,
            10 | 11 => InputTopology::LinesAdjacency,
            12 | 13 => InputTopology::TrianglesAdjacency,
            4..=9 | 14 => InputTopology::Triangles,
            _ => InputTopology::Points,
        };
        info.glasm_use_storage_buffers = glasm_use_storage_buffers;

        info
    }

    /// Port of upstream `MaxwellToOutputTopology(...)`.
    fn maxwell_to_output_topology(topology: u32) -> OutputTopology {
        match topology {
            0 => OutputTopology::PointList,
            3 => OutputTopology::LineStrip,
            _ => OutputTopology::TriangleStrip,
        }
    }

    fn compute_pipeline_key_from_shader_and_qmd(
        shader: SharedShaderInfo,
        qmd: &LaunchParams,
    ) -> ComputePipelineKey {
        ComputePipelineKey {
            unique_hash: shader.unique_hash,
            shared_memory_size: qmd.shared_alloc,
            workgroup_size: [qmd.block_dim_x, qmd.block_dim_y, qmd.block_dim_z],
        }
    }

    /// Create a new compute pipeline.
    ///
    /// Port of `ShaderCache::CreateComputePipeline(...)`.
    fn create_compute_pipeline(
        &mut self,
        shared_cache: &SharedShaderCache,
        key: &ComputePipelineKey,
        shader_size: usize,
    ) -> Option<ComputePipeline> {
        let kepler_compute = shared_cache.current_kepler_compute()?;
        let gpu_memory = shared_cache.current_gpu_memory()?;
        let mut env = ComputeEnvironment::from_kepler_compute(kepler_compute, gpu_memory);
        env.generic_environment_mut().set_cached_size(shader_size);
        let pipeline = self.create_compute_pipeline_from_environment(key, &mut env, false);
        if pipeline.is_some() && !self.shader_cache_filename.as_os_str().is_empty() {
            serialize_pipeline(
                compute_pipeline_key_bytes(key),
                &[env.generic_environment()],
                &self.shader_cache_filename,
                CACHE_VERSION,
            );
        }
        pipeline
    }

    fn create_compute_pipeline_from_environment(
        &mut self,
        key: &ComputePipelineKey,
        env: &mut ComputeEnvironment,
        force_context_flush: bool,
    ) -> Option<ComputePipeline> {
        match catch_shader_exception(|| {
            self.create_compute_pipeline_from_environment_unchecked(key, env, force_context_flush)
        }) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                log::error!("{error}");
                None
            }
        }
    }

    fn create_compute_pipeline_from_environment_unchecked(
        &mut self,
        key: &ComputePipelineKey,
        env: &mut ComputeEnvironment,
        force_context_flush: bool,
    ) -> Option<ComputePipeline> {
        let hash = key.hash_key();
        log::info!("0x{:016x}", hash);

        if *common::settings::values().dump_guest_shaders.get_value() {
            env.dump(hash, key.unique_hash);
        }

        let code = env
            .generic_environment()
            .cached_instruction_slice()
            .to_vec();
        let base_offset = env.generic_environment().cached_instruction_start();
        let mut program =
            translate_program_from_env_with_host_info(&code, base_offset, env, &self.host_info);
        let glasm_runtime_info =
            make_compute_runtime_info(&program.info, self.max_glasm_storage_buffer_blocks);
        let backend =
            compute_program_backend(*common::settings::values().renderer_backend.get_value());
        let (info, source, spirv_words) = match backend {
            ComputeProgramBackend::Glsl => {
                let mut bindings = shader_recompiler::backend::bindings::Bindings::default();
                let source = shader_recompiler::backend::glsl::emit_glsl(
                    &self.profile,
                    &RuntimeInfo::default(),
                    &mut program,
                    &mut bindings,
                );
                (program.info.clone(), source, Vec::new())
            }
            ComputeProgramBackend::Glasm => {
                let mut bindings = shader_recompiler::backend::bindings::Bindings::default();
                let source = shader_recompiler::backend::glasm::emit_glasm(
                    &self.profile,
                    &glasm_runtime_info,
                    &program,
                    &mut bindings,
                );
                (program.info.clone(), source, Vec::new())
            }
            ComputeProgramBackend::SpirV => {
                let spirv_words = shader_recompiler::backend::emit_spirv(
                    &program,
                    &self.profile,
                    &RuntimeInfo::default(),
                );
                (program.info.clone(), String::new(), spirv_words)
            }
        };
        Some(ComputePipeline::new_with_backend_state(
            self.texture_cache?,
            self.buffer_cache?,
            self.program_manager.clone()?,
            info,
            &source,
            &spirv_words,
            backend,
            self.max_glasm_storage_buffer_blocks,
            force_context_flush,
        ))
    }

    /// Returns the number of cached graphics pipelines.
    #[cfg(test)]
    fn graphics_pipeline_count(&self) -> usize {
        self.graphics_cache.len()
    }

    /// Returns the number of cached compute pipelines.
    #[cfg(test)]
    fn compute_pipeline_count(&self) -> usize {
        self.compute_cache.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::control::channel_state::ChannelState;
    use crate::engines::engine_interface::EngineInterface;
    use crate::engines::maxwell_3d::{EngineHint, Maxwell3D, PrimitiveTopology};
    use crate::memory_manager::MemoryManager;
    use parking_lot::Mutex as ParkingLotMutex;

    static RENDERER_BACKEND_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct RendererBackendGuard {
        previous: RendererBackend,
    }

    impl RendererBackendGuard {
        fn set(backend: RendererBackend) -> Self {
            let mut values = common::settings::values_mut();
            let previous = *values.renderer_backend.get_value();
            values.renderer_backend.set_value(backend);
            Self { previous }
        }
    }

    impl Drop for RendererBackendGuard {
        fn drop(&mut self) {
            common::settings::values_mut()
                .renderer_backend
                .set_value(self.previous);
        }
    }

    fn make_owner_backed_memory_manager(
        gpu_base: u64,
        device_addr: u64,
        backing: &[u8],
    ) -> Arc<ParkingLotMutex<MemoryManager>> {
        let device_memory = Arc::new(
            crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default(),
        );
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            device_addr,
            backing.as_ptr(),
            0x4000_0000,
            backing.len(),
            1,
            true,
        );
        let memory_manager = Arc::new(ParkingLotMutex::new(
            MemoryManager::new_with_geometry_and_device_memory(
                1,
                Arc::clone(&device_memory),
                40,
                0x1_0000_0000,
                16,
                12,
            ),
        ));
        memory_manager
            .lock()
            .map(gpu_base, device_addr, backing.len() as u64, 0, false);
        memory_manager
    }

    fn make_maxwell_for_built_pipeline(
        vertex_count: u32,
        index_count: u32,
        zeta_enable: bool,
    ) -> Maxwell3D {
        let mut maxwell = Maxwell3D::new();
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x583, vertex_count, true);
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x5F8, index_count, true);
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x54E, zeta_enable as u32, true);
        maxwell
    }

    #[test]
    fn shader_cache_creation() {
        let cache = ShaderCache::new_for_test();
        assert_eq!(cache.graphics_pipeline_count(), 0);
        assert_eq!(cache.compute_pipeline_count(), 0);
        assert!(!cache.use_asynchronous_shaders);
    }

    #[test]
    fn null_pipeline_entries_are_retained_as_upstream_negative_cache_entries() {
        let mut cache = ShaderCache::new_for_test();
        cache
            .graphics_cache
            .insert(GraphicsPipelineKey::default(), None);
        cache
            .compute_cache
            .insert(ComputePipelineKey::default(), None);

        assert_eq!(cache.graphics_pipeline_count(), 1);
        assert_eq!(cache.compute_pipeline_count(), 1);
        assert!(cache
            .graphics_cache
            .get(&GraphicsPipelineKey::default())
            .is_some_and(Option::is_none));
        assert!(cache
            .compute_cache
            .get(&ComputePipelineKey::default())
            .is_some_and(Option::is_none));
    }

    #[test]
    fn negative_graphics_entry_does_not_replace_the_current_pipeline() {
        let mut cache = ShaderCache::new_for_test();
        let previous_key = GraphicsPipelineKey {
            unique_hashes: [1, 0, 0, 0, 0, 0],
            ..GraphicsPipelineKey::default()
        };
        let failed_key = GraphicsPipelineKey {
            unique_hashes: [0, 2, 0, 0, 0, 0],
            ..GraphicsPipelineKey::default()
        };
        cache.graphics_cache.insert(
            previous_key,
            Some(Box::new(GraphicsPipeline::new_for_test(previous_key, None))),
        );
        cache.current_pipeline = Some(previous_key);
        cache.graphics_key = failed_key;
        cache.graphics_cache.insert(failed_key, None);

        let mut shared_cache = SharedShaderCache::default();
        assert!(cache
            .current_graphics_pipeline_slow_path(&mut shared_cache)
            .is_none());
        assert_eq!(cache.current_pipeline, Some(previous_key));
    }

    #[test]
    fn cache_version() {
        assert_eq!(CACHE_VERSION, 15);
    }

    #[test]
    fn compute_backend_selection_never_silently_replaces_glasm() {
        assert_eq!(
            compute_program_backend(RendererBackend::OpenGlGlsl),
            ComputeProgramBackend::Glsl
        );
        assert_eq!(
            compute_program_backend(RendererBackend::OpenGlGlasm),
            ComputeProgramBackend::Glasm
        );
        assert_eq!(
            compute_program_backend(RendererBackend::OpenGlSpirV),
            ComputeProgramBackend::SpirV
        );
    }

    #[test]
    fn compute_pipeline_key_matches_upstream_current_compute_pipeline_fields() {
        let mut qmd = LaunchParams::default();
        qmd.shared_alloc = 0x240;
        qmd.block_dim_x = 8;
        qmd.block_dim_y = 4;
        qmd.block_dim_z = 2;
        let shader = SharedShaderInfo {
            unique_hash: 0x1234_5678_9ABC_DEF0,
            size_bytes: 0x180,
        };

        let key = ShaderCache::compute_pipeline_key_from_shader_and_qmd(shader, &qmd);

        assert_eq!(key.unique_hash, 0x1234_5678_9ABC_DEF0);
        assert_eq!(key.shared_memory_size, 0x240);
        assert_eq!(key.workgroup_size, [8, 4, 2]);
    }

    #[test]
    fn compute_glasm_runtime_info_uses_the_same_storage_buffer_limit_as_pipeline() {
        let mut info = ShaderInfo::default();
        info.storage_buffers_descriptors.push(
            shader_recompiler::shader_info::StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 3,
                is_written: false,
            },
        );

        assert!(!make_compute_runtime_info(&info, 2).glasm_use_storage_buffers);
        assert!(make_compute_runtime_info(&info, 3).glasm_use_storage_buffers);
    }

    #[test]
    fn maxwell_to_output_topology_matches_upstream_mapping() {
        assert_eq!(
            ShaderCache::maxwell_to_output_topology(PrimitiveTopology::Points as u32),
            OutputTopology::PointList
        );
        assert_eq!(
            ShaderCache::maxwell_to_output_topology(PrimitiveTopology::LineStrip as u32),
            OutputTopology::LineStrip
        );
        assert_eq!(
            ShaderCache::maxwell_to_output_topology(PrimitiveTopology::TriangleStrip as u32),
            OutputTopology::TriangleStrip
        );
        assert_eq!(
            ShaderCache::maxwell_to_output_topology(PrimitiveTopology::Triangles as u32),
            OutputTopology::TriangleStrip
        );
    }

    #[test]
    fn runtime_info_invalid_fields_use_upstream_fallbacks() {
        let mut key = GraphicsPipelineKey::default();
        key.set_tessellation_primitive(3);
        key.set_tessellation_spacing(3);
        key.set_gs_input_topology(15);

        let info =
            ShaderCache::make_runtime_info(&key, ShaderStage::TessellationEval, None, false, false);

        assert_eq!(info.tess_primitive, TessPrimitive::Triangles);
        assert_eq!(info.tess_spacing, TessSpacing::Equal);
        assert_eq!(info.input_topology, InputTopology::Points);
    }

    #[test]
    fn shared_cache_path_populates_live_graphics_key_fields_from_maxwell() {
        let _settings_lock = RENDERER_BACKEND_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _backend = RendererBackendGuard::set(RendererBackend::OpenGlGlsl);
        let gpu_base = 0x1_0000_0000;
        let device_addr = 0x4000;
        let mut backing = vec![0u8; 0x2000];
        let instruction_offset =
            0x100 + std::mem::size_of::<shader_recompiler::program_header::ProgramHeader>();
        backing[instruction_offset + 8..instruction_offset + 16]
            .copy_from_slice(&0xE300_0000_0007_000Fu64.to_le_bytes());
        backing[0x180..0x188].copy_from_slice(&0xE2400FFFFF87000Fu64.to_le_bytes());
        let memory_manager = make_owner_backed_memory_manager(gpu_base, device_addr, &backing);

        let mut maxwell = Maxwell3D::new();
        maxwell.set_memory_manager(Arc::clone(&memory_manager));
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x582, 1, true);
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x583, 0, true);
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x810, 1 | (1 << 4), true);
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x811, 0x100, true);
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x84, 1, true);
        <Maxwell3D as EngineInterface>::call_method(
            &mut maxwell,
            0xC8,
            0x2 | (1 << 4) | (2 << 8),
            true,
        );
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x1C0, 3, true);
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x1C1, 5, true);
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x1C2, 0x20, true);
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x1D1, 1, true);
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0xA00, 0x0403_0201, true);
        <Maxwell3D as EngineInterface>::call_method(
            &mut maxwell,
            0x586,
            PrimitiveTopology::TriangleStrip as u32,
            true,
        );
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x652, 1, true);
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x65C, 2, true);
        maxwell.set_engine_state(EngineHint::OnHleMacro);

        let mut channel = ChannelState::new(7);
        channel.program_id = 0x1234;
        channel.memory_manager = Some(Arc::clone(&memory_manager));
        channel.maxwell_3d = Some(Box::new(maxwell));
        channel.kepler_compute = Some(Box::default());

        let mut shared_cache = SharedShaderCache::default();
        shared_cache.create_channel(&channel);
        shared_cache.bind_to_channel(7);

        let mut cache = ShaderCache::new_for_test();
        let pipeline = cache
            .current_graphics_pipeline(&mut shared_cache)
            .expect("shared path should build a pipeline");

        assert_eq!(
            pipeline.key().unique_hashes[1],
            shared_cache.shader_info_slots()[1]
                .map(|ptr| unsafe { &*ptr }.unique_hash)
                .unwrap()
        );
        assert!(pipeline.key().early_z());
        assert!(pipeline.key().xfb_enabled());
        assert_eq!(
            (pipeline.key().raw >> 2) & 0xF,
            PrimitiveTopology::TriangleStrip as u32
        );
        assert_eq!((pipeline.key().raw >> 6) & 0x3, 2);
        assert_eq!((pipeline.key().raw >> 8) & 0x3, 1);
        assert_eq!((pipeline.key().raw >> 10) & 0x1, 1);
        assert_eq!(
            (pipeline.key().raw >> 11) & 0x7,
            EngineHint::OnHleMacro as u32
        );
        assert_eq!(pipeline.key().xfb_state.layouts[0].stream, 3);
        assert_eq!(pipeline.key().xfb_state.layouts[0].varying_count, 5);
        assert_eq!(pipeline.key().xfb_state.layouts[0].stride, 0x20);
        assert_eq!(pipeline.key().xfb_state.varyings[0][0].raw(), 0x0403_0201);
        assert!(
            pipeline
                .glsl_source_for_test(0)
                .is_some_and(|source| !source.is_empty()),
            "shared GraphicsEnvironment path must emit VertexB GLSL"
        );
    }

    #[test]
    fn built_pipeline_async_shared_path_allows_small_depth_draws_like_upstream() {
        let maxwell = make_maxwell_for_built_pipeline(4, 64, true);
        let mut pipeline = GraphicsPipeline::new_for_test(GraphicsPipelineKey::default(), None);
        pipeline.set_built_for_test(false);
        assert!(ShaderCache::built_pipeline(true, Some(&maxwell), &mut pipeline).is_some());
    }

    #[test]
    fn built_pipeline_async_shared_path_allows_small_draws() {
        let maxwell = make_maxwell_for_built_pipeline(4, 64, false);
        let mut pipeline = GraphicsPipeline::new_for_test(GraphicsPipelineKey::default(), None);
        pipeline.set_built_for_test(false);
        assert!(ShaderCache::built_pipeline(true, Some(&maxwell), &mut pipeline).is_some());
    }

    #[test]
    fn opengl_fragment_profile_declares_all_frag_colors() {
        let cache = ShaderCache::new_for_test();
        let mut program = ShaderProgram::new(ShaderStage::Fragment);
        let mut bindings = shader_recompiler::backend::bindings::Bindings::default();
        let source = shader_recompiler::backend::glsl::emit_glsl(
            &cache.profile,
            &RuntimeInfo::default(),
            &mut program,
            &mut bindings,
        );

        for index in 0..8 {
            assert!(
                source.contains(&format!(
                    "layout(location={})out vec4 frag_color{};",
                    index, index
                )),
                "OpenGL profile must declare frag_color{} even when the shader does not write it",
                index
            );
        }
    }

    #[test]
    fn shader_cache_uses_stored_opengl_profile() {
        let mut profile = test_opengl_shader_profile();
        profile.need_declared_frag_colors = false;
        let cache =
            ShaderCache::new_with_profile(profile, HostTranslateInfo::default(), false, false);
        let mut program = ShaderProgram::new(ShaderStage::Fragment);
        let mut bindings = shader_recompiler::backend::bindings::Bindings::default();
        let source = shader_recompiler::backend::glsl::emit_glsl(
            &cache.profile,
            &RuntimeInfo::default(),
            &mut program,
            &mut bindings,
        );

        assert!(
            !source.contains("frag_color7"),
            "ShaderCache must pass its stored profile to GLSL compilation"
        );
    }

    #[test]
    fn disk_load_progress_reports_worker_completions_after_loading() {
        let reports = Arc::new(Mutex::new(Vec::new()));
        let callback_reports = Arc::clone(&reports);
        let callback: DiskResourceLoadCallback = Arc::new(move |stage, built, total| {
            callback_reports.lock().unwrap().push((stage, built, total));
        });
        let mut state = DiskResourceLoadState {
            total: 2,
            ..Default::default()
        };

        state.complete_one(&callback);
        assert!(reports.lock().unwrap().is_empty());

        state.has_loaded = true;
        state.complete_one(&callback);
        assert_eq!(
            reports.lock().unwrap().as_slice(),
            &[(LoadCallbackStage::Build, 2, 2)]
        );
    }

    #[test]
    fn file_environment_shader_errors_are_caught_like_upstream() {
        let mut environment =
            GraphicsEnvironment::from_file_environment(FileEnvironment::default());
        let result = catch_shader_exception(|| environment.read_texture_pixel_format(7));

        assert_eq!(
            result,
            Err("Uncached read texture pixel format".to_string())
        );
    }

    #[test]
    fn shader_exception_scope_does_not_swallow_unrelated_panics() {
        let result = std::panic::catch_unwind(|| {
            let _: Result<(), String> = catch_shader_exception(|| panic!("ordinary panic"));
        });

        assert!(result.is_err());
    }
}

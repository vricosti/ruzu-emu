// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `src/video_core/renderer_opengl/gl_compute_pipeline.h` and
//! `gl_compute_pipeline.cpp`.
//!
//! OpenGL compute pipeline management -- compiles and configures compute shaders.

use std::hash::{Hash, Hasher};
use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex};

use common::cityhash::city_hash64;
use smallvec::SmallVec;

use crate::buffer_cache::buffer_cache_base::ComputeUniformBufferSizes;
use crate::engines::kepler_compute::{KeplerCompute, LaunchParams};
use crate::memory_manager::MemoryManager;
use crate::texture_cache::texture_cache_base::{ComputeDescriptorSyncRegs, ImageViewInOut};
use crate::texture_cache::types::SamplerId;
use crate::textures::texture::texture_pair;

use super::gl_buffer_cache::BufferCache as OpenGLBufferCache;
use super::gl_resource_manager::{OGLAssemblyProgram, OGLProgram, OGLSync};
use super::gl_shader_manager::ProgramManagerHandle;
use super::gl_shader_util::{
    compile_assembly_program, create_program_from_source, create_program_from_spirv,
    program_local_parameter_4f_arb,
};
use super::gl_texture_cache::TextureCache;
use shader_recompiler::shader_info::{
    num_descriptors, ImageBufferDescriptor, ImageDescriptor, Info, TextureBufferDescriptor,
    TextureDescriptor,
};

/// Maximum number of textures bound to a compute pipeline.
const MAX_TEXTURES: u32 = 64;

/// Maximum number of images bound to a compute pipeline.
const MAX_IMAGES: u32 = 16;
const GL_COMPUTE_PROGRAM_NV: u32 = 0x90FB;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ComputeProgramBackend {
    Glsl,
    Glasm,
    SpirV,
}

/// Key used to identify a unique compute pipeline configuration.
///
/// Corresponds to `OpenGL::ComputePipelineKey`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct ComputePipelineKey {
    pub unique_hash: u64,
    pub shared_memory_size: u32,
    pub workgroup_size: [u32; 3],
}

impl ComputePipelineKey {
    /// Hash the complete key byte representation, matching upstream.
    pub fn hash_key(&self) -> u64 {
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                self as *const Self as *const u8,
                std::mem::size_of::<Self>(),
            )
        };
        city_hash64(bytes)
    }
}

impl Hash for ComputePipelineKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash_key());
    }
}

#[cfg(test)]
mod key_tests {
    use super::*;
    use std::io::Write;

    fn bytes_of(key: &ComputePipelineKey) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                (key as *const ComputePipelineKey).cast::<u8>(),
                std::mem::size_of::<ComputePipelineKey>(),
            )
        }
    }

    #[test]
    fn pipeline_key_cache_layout_round_trips() {
        assert_eq!(std::mem::size_of::<ComputePipelineKey>(), 24);
        let key = ComputePipelineKey {
            unique_hash: 0x0123_4567_89AB_CDEF,
            shared_memory_size: 0x1122_3344,
            workgroup_size: [7, 11, 13],
        };
        let path = std::env::temp_dir().join(format!(
            "ruzu-gl-compute-key-{}-{}.bin",
            std::process::id(),
            key.unique_hash
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(bytes_of(&key)).unwrap();
        drop(file);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes, bytes_of(&key));
        std::fs::remove_file(path).unwrap();
    }
}

/// Host-side descriptors resolved by the texture/image part of
/// `ComputePipeline::Configure`.
#[derive(Debug, Clone, Default)]
struct ComputeTextureBindings {
    views: SmallVec<[ImageViewInOut; MAX_TEXTURES as usize + MAX_IMAGES as usize]>,
    samplers: SmallVec<[SamplerId; MAX_TEXTURES as usize]>,
}

impl ComputeTextureBindings {
    fn push_view(&mut self, view: ImageViewInOut) {
        assert!(
            self.views.len() < (MAX_TEXTURES + MAX_IMAGES) as usize,
            "ComputePipeline image-view bindings exceed Eden's static_vector capacity"
        );
        self.views.push(view);
    }

    fn push_sampler(&mut self, sampler: SamplerId) {
        assert!(
            self.samplers.len() < MAX_TEXTURES as usize,
            "ComputePipeline sampler bindings exceed Eden's static_vector capacity"
        );
        self.samplers.push(sampler);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ComputePipelineInfoState {
    uniform_buffer_sizes: ComputeUniformBufferSizes,
    num_texture_buffers: u32,
    num_image_buffers: u32,
    use_storage_buffers: bool,
    writes_global_memory: bool,
    uses_local_memory: bool,
}

/// OpenGL compute pipeline.
///
/// Corresponds to `OpenGL::ComputePipeline`.
pub struct ComputePipeline {
    /// Non-owning references retained by Eden's `ComputePipeline`.
    /// Production pipelines always have all three; the optional representation
    /// exists only for GL-free metadata tests.
    texture_cache: Option<NonNull<TextureCache>>,
    buffer_cache: Option<NonNull<OpenGLBufferCache>>,
    program_manager: Option<ProgramManagerHandle>,
    /// Shader resource metadata copied into the pipeline.
    info: Info,
    /// Assembly program (GLASM).
    assembly_program: OGLAssemblyProgram,
    /// Source program (GLSL or SPIR-V).
    source_program: OGLProgram,
    /// Uniform buffer sizes copied from shader info.
    uniform_buffer_sizes: ComputeUniformBufferSizes,

    /// Number of texture buffer descriptors.
    num_texture_buffers: u32,
    /// Number of image buffer descriptors.
    num_image_buffers: u32,

    /// Whether to use storage buffers (vs bindless).
    use_storage_buffers: bool,
    /// Whether any storage buffer descriptor is written.
    writes_global_memory: bool,
    /// Whether local memory is used.
    uses_local_memory: bool,

    /// Live compute engine installed by `SetEngine` before Configure.
    kepler_compute: Option<NonNull<KeplerCompute>>,
    /// Channel GPU memory used by `ComputePipeline::Configure`.
    ///
    /// Upstream stores this as `Tegra::MemoryManager* gpu_memory`.
    gpu_memory: Option<Arc<parking_lot::Mutex<MemoryManager>>>,

    // Build synchronization
    built_mutex: Mutex<()>,
    built_condvar: Condvar,
    built_fence: OGLSync,
    is_built: bool,
}

impl ComputePipeline {
    /// Create a new compute pipeline.
    ///
    /// Corresponds to `ComputePipeline::ComputePipeline()`.
    pub fn new(
        device: &super::gl_device::Device,
        texture_cache: &mut TextureCache,
        buffer_cache: &mut OpenGLBufferCache,
        program_manager: ProgramManagerHandle,
        info: Info,
        code: &str,
        code_v: &[u32],
        force_context_flush: bool,
    ) -> Self {
        Self::new_with_backend_state(
            NonNull::from(texture_cache),
            NonNull::from(buffer_cache),
            program_manager,
            info,
            code,
            code_v,
            match *common::settings::values().renderer_backend.get_value() {
                common::settings_enums::RendererBackend::OpenGlGlsl => ComputeProgramBackend::Glsl,
                common::settings_enums::RendererBackend::OpenGlGlasm => {
                    ComputeProgramBackend::Glasm
                }
                common::settings_enums::RendererBackend::OpenGlSpirV => {
                    ComputeProgramBackend::SpirV
                }
                _ => unreachable!("OpenGL compute pipeline requires an OpenGL backend"),
            },
            device.max_glasm_storage_buffer_blocks(),
            force_context_flush,
        )
    }

    /// Create a pipeline from the cache owners and backend capability snapshot
    /// retained by `ShaderCache`.
    pub(crate) fn new_with_backend_state(
        texture_cache: NonNull<TextureCache>,
        buffer_cache: NonNull<OpenGLBufferCache>,
        program_manager: ProgramManagerHandle,
        info: Info,
        code: &str,
        code_v: &[u32],
        backend: ComputeProgramBackend,
        max_glasm_storage_buffer_blocks: u32,
        force_context_flush: bool,
    ) -> Self {
        Self::new_impl(
            Some(texture_cache),
            Some(buffer_cache),
            Some(program_manager),
            info,
            code,
            code_v,
            backend,
            max_glasm_storage_buffer_blocks,
            force_context_flush,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_impl(
        texture_cache: Option<NonNull<TextureCache>>,
        buffer_cache: Option<NonNull<OpenGLBufferCache>>,
        program_manager: Option<ProgramManagerHandle>,
        info: Info,
        code: &str,
        code_v: &[u32],
        backend: ComputeProgramBackend,
        max_glasm_storage_buffer_blocks: u32,
        force_context_flush: bool,
    ) -> Self {
        let (source_program, assembly_program) = match backend {
            ComputeProgramBackend::Glsl => (
                create_program_from_source(code, gl::COMPUTE_SHADER),
                OGLAssemblyProgram::new(),
            ),
            ComputeProgramBackend::Glasm => (
                OGLProgram::new(),
                compile_assembly_program(code, GL_COMPUTE_PROGRAM_NV),
            ),
            ComputeProgramBackend::SpirV => (
                create_program_from_spirv(code_v, gl::COMPUTE_SHADER),
                OGLAssemblyProgram::new(),
            ),
        };
        let state = Self::info_state(
            &info,
            assembly_program.handle != 0,
            max_glasm_storage_buffer_blocks,
        );

        let built_mutex = Mutex::new(());
        let built_condvar = Condvar::new();
        let mut built_fence = OGLSync::new();
        if force_context_flush {
            let _lock = built_mutex.lock().unwrap();
            built_fence.create();
            unsafe { gl::Flush() };
            built_condvar.notify_one();
        }

        Self {
            texture_cache,
            buffer_cache,
            program_manager,
            info,
            source_program,
            assembly_program,
            uniform_buffer_sizes: state.uniform_buffer_sizes,
            num_texture_buffers: state.num_texture_buffers,
            num_image_buffers: state.num_image_buffers,
            use_storage_buffers: state.use_storage_buffers,
            writes_global_memory: state.writes_global_memory,
            uses_local_memory: state.uses_local_memory,
            kepler_compute: None,
            gpu_memory: None,
            built_mutex,
            built_condvar,
            built_fence,
            is_built: !force_context_flush,
        }
    }

    #[cfg(test)]
    fn new_for_test(info: Info, is_glasm: bool, max_glasm_storage_buffer_blocks: u32) -> Self {
        let state = Self::info_state(&info, is_glasm, max_glasm_storage_buffer_blocks);
        Self {
            texture_cache: None,
            buffer_cache: None,
            program_manager: None,
            info,
            source_program: OGLProgram::new(),
            assembly_program: OGLAssemblyProgram::new(),
            uniform_buffer_sizes: state.uniform_buffer_sizes,
            num_texture_buffers: state.num_texture_buffers,
            num_image_buffers: state.num_image_buffers,
            use_storage_buffers: state.use_storage_buffers,
            writes_global_memory: state.writes_global_memory,
            uses_local_memory: state.uses_local_memory,
            kepler_compute: None,
            gpu_memory: None,
            built_mutex: Mutex::new(()),
            built_condvar: Condvar::new(),
            built_fence: OGLSync::new(),
            is_built: true,
        }
    }

    /// Port of upstream `ComputePipeline::SetEngine`.
    pub fn set_engine(
        &mut self,
        kepler_compute: NonNull<KeplerCompute>,
        gpu_memory: Arc<parking_lot::Mutex<MemoryManager>>,
    ) {
        self.kepler_compute = Some(kepler_compute);
        self.gpu_memory = Some(gpu_memory);
    }

    fn synchronize_texture_descriptors(
        texture_cache: &mut TextureCache,
        kepler_compute: &KeplerCompute,
    ) {
        texture_cache
            .base
            .synchronize_compute_descriptors(Self::descriptor_sync_regs(kepler_compute));
    }

    /// Port of the descriptor-handle collection at the start of upstream
    /// `ComputePipeline::Configure()`.
    ///
    /// This reads compute handles from QMD constant buffers, builds the
    /// `ImageViewInOut` list in the same texture-buffer, image-buffer,
    /// sampled-texture, storage-image order and resolves compute samplers. The
    /// caller releases the memory guard before `FillComputeImageViews` reads
    /// the TIC table, matching the next operation in upstream `Configure`.
    fn prepare_texture_bindings(
        texture_cache: &mut TextureCache,
        info: &Info,
        qmd: &LaunchParams,
        gpu_memory: &MemoryManager,
    ) -> ComputeTextureBindings {
        Self::collect_texture_bindings(
            info,
            qmd,
            |gpu_addr| gpu_memory.read::<u32>(gpu_addr),
            |index| {
                texture_cache
                    .base
                    .get_sampler_id_with_memory(index, true, gpu_memory)
            },
        )
    }

    fn collect_texture_bindings(
        info: &Info,
        qmd: &LaunchParams,
        mut read_u32: impl FnMut(u64) -> u32,
        mut get_sampler_id: impl FnMut(u32) -> SamplerId,
    ) -> ComputeTextureBindings {
        let mut result = ComputeTextureBindings::default();
        let via_header_index = qmd.linked_tsc;

        for desc in &info.texture_buffer_descriptors {
            for index in 0..desc.count {
                let (tic_index, _) =
                    Self::read_handle(qmd, desc, index, via_header_index, &mut read_u32);
                result.push_view(ImageViewInOut {
                    index: tic_index,
                    ..Default::default()
                });
            }
        }
        for desc in &info.image_buffer_descriptors {
            Self::add_image_handles(
                &mut result,
                qmd,
                desc,
                false,
                via_header_index,
                &mut read_u32,
            );
        }

        for desc in &info.texture_descriptors {
            for index in 0..desc.count {
                let (tic_index, tsc_index) =
                    Self::read_handle(qmd, desc, index, via_header_index, &mut read_u32);
                result.push_view(ImageViewInOut {
                    index: tic_index,
                    ..Default::default()
                });
                result.push_sampler(get_sampler_id(tsc_index));
            }
        }

        for desc in &info.image_descriptors {
            Self::add_image_handles(
                &mut result,
                qmd,
                desc,
                desc.is_written,
                via_header_index,
                &mut read_u32,
            );
        }

        result
    }

    fn info_state(
        info: &Info,
        is_glasm: bool,
        max_glasm_storage_buffer_blocks: u32,
    ) -> ComputePipelineInfoState {
        let mut uniform_buffer_sizes = [0; 8];
        uniform_buffer_sizes.copy_from_slice(&info.constant_buffer_used_sizes[..8]);

        let num_texture_buffers = num_descriptors(&info.texture_buffer_descriptors);
        let num_image_buffers = num_descriptors(&info.image_buffer_descriptors);
        let num_textures =
            num_texture_buffers.wrapping_add(num_descriptors(&info.texture_descriptors));
        let num_images = num_image_buffers.wrapping_add(num_descriptors(&info.image_descriptors));
        if num_textures > MAX_TEXTURES {
            // Eden's ASSERT reports this invariant and continues. The fixed
            // binding arrays below preserve the same hard capacity.
            log::error!(
                "ComputePipeline: texture descriptor count {num_textures} exceeds MAX_TEXTURES {MAX_TEXTURES}"
            );
        }
        if num_images > MAX_IMAGES {
            log::error!(
                "ComputePipeline: image descriptor count {num_images} exceeds MAX_IMAGES {MAX_IMAGES}"
            );
        }

        let num_storage_buffers = num_descriptors(&info.storage_buffers_descriptors);
        let use_storage_buffers =
            !is_glasm || num_storage_buffers < max_glasm_storage_buffer_blocks;
        let writes_global_memory = !use_storage_buffers
            && info
                .storage_buffers_descriptors
                .iter()
                .any(|desc| desc.is_written);

        ComputePipelineInfoState {
            uniform_buffer_sizes,
            num_texture_buffers,
            num_image_buffers,
            use_storage_buffers,
            writes_global_memory,
            uses_local_memory: info.uses_local_memory,
        }
    }

    fn descriptor_sync_regs(kepler_compute: &KeplerCompute) -> ComputeDescriptorSyncRegs {
        ComputeDescriptorSyncRegs {
            linked_tsc: kepler_compute.launch_description().linked_tsc,
            tic_addr: kepler_compute.tic_address(),
            tic_limit: kepler_compute.tic_limit(),
            tsc_addr: kepler_compute.tsc_address(),
            tsc_limit: kepler_compute.tsc_limit(),
        }
    }

    /// Port of upstream `ComputePipeline::Configure()`.
    ///
    /// The pipeline obtains cache/manager owners from its constructor and the
    /// live engine/memory owners from `set_engine`, as Eden does.
    pub fn configure(&mut self) {
        let mut texture_cache_ptr = self
            .texture_cache
            .expect("ComputePipeline::Configure requires TextureCache owner");
        let mut buffer_cache_ptr = self
            .buffer_cache
            .expect("ComputePipeline::Configure requires BufferCache owner");
        let program_manager = self
            .program_manager
            .clone()
            .expect("ComputePipeline::Configure requires ProgramManager owner");
        let kepler_compute_ptr = self
            .kepler_compute
            .expect("ComputePipeline::Configure requires SetEngine first");
        let gpu_memory = self
            .gpu_memory
            .as_ref()
            .expect("ComputePipeline::Configure requires GPU memory from SetEngine")
            .clone();

        // SAFETY: ShaderCache owns pipelines and RasterizerOpenGL owns the
        // boxed caches for longer than every pipeline. The bound channel owns
        // KeplerCompute for the duration of this serialized GPU callback.
        let texture_cache = unsafe { texture_cache_ptr.as_mut() };
        let buffer_cache = unsafe { buffer_cache_ptr.as_mut() };
        let kepler_compute = unsafe { kepler_compute_ptr.as_ref() };
        let qmd = kepler_compute.launch_description();

        self.configure_buffer_state(buffer_cache);
        Self::synchronize_texture_descriptors(texture_cache, kepler_compute);
        // Eden uses the same raw MemoryManager pointer for descriptor handles
        // and GetSamplerId. Pass the one guarded Rust borrow through both
        // operations, then release it before FillImageViews accesses the TIC.
        let gpu_memory_guard = gpu_memory.lock();
        let mut bindings =
            Self::prepare_texture_bindings(texture_cache, &self.info, qmd, &gpu_memory_guard);
        drop(gpu_memory_guard);
        texture_cache.fill_image_views(&mut bindings.views, true, true);
        self.configure_backend_bindings(
            buffer_cache,
            texture_cache,
            &mut program_manager.lock(),
            &bindings,
        );
    }

    fn configure_buffer_state(&self, buffer_cache: &mut OpenGLBufferCache) {
        // SAFETY: ShaderCache owns this pipeline through a Box, matching
        // upstream's unique_ptr, so the pointed-to sizes remain stable.
        unsafe {
            buffer_cache.set_compute_uniform_buffer_state(
                self.info.constant_buffer_mask,
                &self.uniform_buffer_sizes,
            );
        }
        buffer_cache.unbind_compute_storage_buffers();
        for (ssbo_index, desc) in self.info.storage_buffers_descriptors.iter().enumerate() {
            if desc.count != 1 {
                // Eden's ASSERT is fail-soft and still binds this descriptor.
                log::error!(
                    "ComputePipeline::Configure storage-buffer descriptor count is {}, expected 1",
                    desc.count
                );
            }
            buffer_cache.bind_compute_storage_buffer(
                ssbo_index,
                desc.cbuf_index,
                desc.cbuf_offset,
                desc.is_written,
            );
        }
    }

    fn configure_backend_bindings(
        &mut self,
        buffer_cache: &mut OpenGLBufferCache,
        texture_cache: &mut TextureCache,
        program_manager: &mut super::gl_shader_manager::ProgramManager,
        bindings: &ComputeTextureBindings,
    ) {
        if !self.is_built {
            self.wait_for_build();
        }
        if self.assembly_program.handle != 0 {
            program_manager.bind_compute_assembly_program(self.assembly_program.handle);
        } else {
            program_manager.bind_compute_program(self.source_program.handle);
        }

        let mut textures = [0u32; MAX_TEXTURES as usize];
        let mut images = [0u32; MAX_IMAGES as usize];
        let mut gl_samplers = [0u32; MAX_TEXTURES as usize];

        buffer_cache.unbind_compute_texture_buffers();
        let mut texbuf_index = 0usize;
        for desc in &self.info.texture_buffer_descriptors {
            for _ in 0..desc.count {
                self.bind_compute_texture_buffer_view(
                    buffer_cache,
                    texture_cache,
                    bindings,
                    texbuf_index,
                    false,
                    false,
                );
                texbuf_index += 1;
            }
        }
        for desc in &self.info.image_buffer_descriptors {
            for _ in 0..desc.count {
                self.bind_compute_texture_buffer_view(
                    buffer_cache,
                    texture_cache,
                    bindings,
                    texbuf_index,
                    desc.is_written,
                    true,
                );
                texbuf_index += 1;
            }
        }

        buffer_cache.update_compute_buffers();
        buffer_cache.set_enable_storage_buffers(self.use_storage_buffers);
        buffer_cache.set_image_pointers(textures.as_mut_ptr(), images.as_mut_ptr());
        buffer_cache.bind_host_compute_buffers();
        // Keep the literal second check present in Eden's ComputePipeline even
        // though BindHostComputeBuffers currently clears the same flag.
        if buffer_cache.any_buffer_uploaded {
            buffer_cache.runtime.post_copy_barrier();
            buffer_cache.any_buffer_uploaded = false;
        }

        let mut views_index =
            (self.num_texture_buffers as usize).wrapping_add(self.num_image_buffers as usize);
        let mut sampler_index = 0usize;
        let mut sampler_binding = 0i32;
        let mut texture_binding = self.num_texture_buffers as i32;
        let mut image_binding = self.num_image_buffers as i32;
        let mut texture_scaling_mask = 0u32;

        for desc in &self.info.texture_buffer_descriptors {
            for _ in 0..desc.count {
                gl_samplers[sampler_binding as usize] = 0;
                sampler_binding += 1;
            }
        }

        for desc in &self.info.texture_descriptors {
            for _ in 0..desc.count {
                let view_id = bindings.views[views_index].id;
                let image_view = texture_cache
                    .get_image_view(view_id)
                    .expect("FillImageViews must publish every compute texture view");
                textures[texture_binding as usize] = image_view.handle(desc.texture_type as usize);
                if texture_cache.image_view_is_rescaling(view_id) {
                    texture_scaling_mask |= 1u32 << texture_binding;
                }
                views_index += 1;
                texture_binding += 1;

                let sampler = texture_cache
                    .get_sampler(bindings.samplers[sampler_index])
                    .expect("GetSamplerId must publish every compute sampler");
                let use_fallback =
                    sampler.has_added_anisotropy() && !image_view.supports_anisotropy();
                gl_samplers[sampler_binding as usize] = if use_fallback {
                    sampler.handle_with_default_anisotropy()
                } else {
                    sampler.handle()
                };
                sampler_binding += 1;
                sampler_index += 1;
            }
        }

        let mut image_scaling_mask = 0u32;
        for desc in &self.info.image_descriptors {
            for _ in 0..desc.count {
                let view_id = bindings.views[views_index].id;
                texture_cache
                    .get_image_view(view_id)
                    .expect("FillImageViews must publish every compute image view");
                if desc.is_written {
                    texture_cache.mark_view_image_modified(view_id);
                }
                let image_view = texture_cache
                    .get_image_view_mut(view_id)
                    .expect("FillImageViews must preserve every compute image view");
                images[image_binding as usize] =
                    image_view.storage_view(desc.texture_type, desc.format);
                if texture_cache.image_view_is_rescaling(view_id) {
                    image_scaling_mask |= 1u32 << image_binding;
                }
                views_index += 1;
                image_binding += 1;
            }
        }

        if self.info.uses_rescaling_uniform {
            let texture_mask = f32::from_bits(texture_scaling_mask);
            let image_mask = f32::from_bits(image_scaling_mask);
            if self.assembly_program.handle != 0 {
                program_local_parameter_4f_arb(
                    GL_COMPUTE_PROGRAM_NV,
                    0,
                    texture_mask,
                    image_mask,
                    0.0,
                    0.0,
                );
            } else {
                unsafe {
                    gl::ProgramUniform4f(
                        self.source_program.handle,
                        0,
                        texture_mask,
                        image_mask,
                        0.0,
                        0.0,
                    );
                }
            }
        }

        unsafe {
            if texture_binding != 0 {
                if texture_binding != sampler_binding {
                    // Eden reports the ASSERT and performs both binds with
                    // their independently accumulated counts.
                    log::error!(
                        "ComputePipeline::Configure texture binding count {texture_binding} differs from sampler binding count {sampler_binding}"
                    );
                }
                gl::BindTextures(0, texture_binding, textures.as_ptr());
                gl::BindSamplers(0, sampler_binding, gl_samplers.as_ptr());
            }
            if image_binding != 0 {
                gl::BindImageTextures(0, image_binding, images.as_ptr());
            }
        }
    }

    fn bind_compute_texture_buffer_view(
        &self,
        buffer_cache: &mut OpenGLBufferCache,
        texture_cache: &TextureCache,
        bindings: &ComputeTextureBindings,
        texbuf_index: usize,
        is_written: bool,
        is_image: bool,
    ) {
        let view_id = bindings.views[texbuf_index].id;
        let image_view = texture_cache
            .get_image_view(view_id)
            .expect("FillImageViews must publish every compute buffer view");
        let gpu_addr = texture_cache.image_view_gpu_addr(view_id);
        buffer_cache.bind_compute_texture_buffer(
            texbuf_index,
            gpu_addr,
            image_view.buffer_size(),
            image_view.pixel_format(),
            is_written,
            is_image,
        );
    }

    fn read_handle(
        qmd: &LaunchParams,
        desc: &impl ComputeHandleDescriptor,
        index: u32,
        via_header_index: bool,
        read_u32: &mut impl FnMut(u64) -> u32,
    ) -> (u32, u32) {
        assert_compute_cbuf_enabled(qmd, desc.cbuf_index());
        let index_offset = index << desc.size_shift();
        let offset = desc.cbuf_offset().wrapping_add(index_offset);
        let addr = qmd.const_buffers[desc.cbuf_index() as usize]
            .address
            .wrapping_add(offset as u64);
        let raw = if desc.has_secondary() {
            assert_compute_cbuf_enabled(qmd, desc.secondary_cbuf_index());
            let secondary_offset = desc.secondary_cbuf_offset().wrapping_add(index_offset);
            let secondary_addr = qmd.const_buffers[desc.secondary_cbuf_index() as usize]
                .address
                .wrapping_add(secondary_offset as u64);
            (read_u32(addr) << desc.shift_left())
                | (read_u32(secondary_addr) << desc.secondary_shift_left())
        } else {
            read_u32(addr)
        };
        texture_pair(raw, via_header_index)
    }

    fn add_image_handles(
        bindings: &mut ComputeTextureBindings,
        qmd: &LaunchParams,
        desc: &impl ComputeHandleDescriptor,
        blacklist: bool,
        via_header_index: bool,
        read_u32: &mut impl FnMut(u64) -> u32,
    ) {
        for index in 0..desc.count() {
            bindings.push_view(ImageViewInOut {
                index: Self::read_handle(qmd, desc, index, via_header_index, read_u32).0,
                blacklist,
                ..Default::default()
            });
        }
    }

    /// Returns whether any storage buffer descriptor is written.
    pub fn writes_global_memory(&self) -> bool {
        self.writes_global_memory
    }

    /// Returns whether local memory is used.
    pub fn uses_local_memory(&self) -> bool {
        self.uses_local_memory
    }

    /// Wait for the pipeline build to complete.
    ///
    /// Port of `ComputePipeline::WaitForBuild()`.
    fn wait_for_build(&mut self) {
        if self.built_fence.handle.is_null() {
            let lock = self.built_mutex.lock().unwrap();
            let _guard = self
                .built_condvar
                .wait_while(lock, |_| self.built_fence.handle.is_null())
                .unwrap();
        }
        unsafe {
            let status = gl::ClientWaitSync(self.built_fence.handle, 0, gl::TIMEOUT_IGNORED);
            if status == gl::WAIT_FAILED {
                log::error!(
                    "ComputePipeline::WaitForBuild: glClientWaitSync returned GL_WAIT_FAILED"
                );
            }
        }
        self.is_built = true;
    }
}

// SAFETY: GL object names and sync handles are transferred from a worker's
// shared context to the render thread. The contexts share one object namespace,
// and the pipeline is not accessed concurrently during that transfer.
unsafe impl Send for ComputePipeline {}

impl Drop for ComputePipeline {
    fn drop(&mut self) {
        self.built_fence.release();
    }
}

fn assert_compute_cbuf_enabled(qmd: &LaunchParams, cbuf_index: u32) {
    if ((qmd.const_buffer_enable_mask >> cbuf_index) & 1) == 0 {
        // Eden's ASSERT is fail-soft; the descriptor access continues.
        log::error!("ComputePipeline::Configure descriptor cbuf {cbuf_index} is disabled");
    }
}

trait ComputeHandleDescriptor {
    fn has_secondary(&self) -> bool {
        false
    }
    fn cbuf_index(&self) -> u32;
    fn cbuf_offset(&self) -> u32;
    fn shift_left(&self) -> u32 {
        0
    }
    fn secondary_cbuf_index(&self) -> u32 {
        0
    }
    fn secondary_cbuf_offset(&self) -> u32 {
        0
    }
    fn secondary_shift_left(&self) -> u32 {
        0
    }
    fn count(&self) -> u32;
    fn size_shift(&self) -> u32;
}

impl ComputeHandleDescriptor for TextureBufferDescriptor {
    fn has_secondary(&self) -> bool {
        self.has_secondary
    }

    fn cbuf_index(&self) -> u32 {
        self.cbuf_index
    }

    fn cbuf_offset(&self) -> u32 {
        self.cbuf_offset
    }

    fn shift_left(&self) -> u32 {
        self.shift_left
    }

    fn secondary_cbuf_index(&self) -> u32 {
        self.secondary_cbuf_index
    }

    fn secondary_cbuf_offset(&self) -> u32 {
        self.secondary_cbuf_offset
    }

    fn secondary_shift_left(&self) -> u32 {
        self.secondary_shift_left
    }

    fn count(&self) -> u32 {
        self.count
    }

    fn size_shift(&self) -> u32 {
        self.size_shift
    }
}

impl ComputeHandleDescriptor for TextureDescriptor {
    fn has_secondary(&self) -> bool {
        self.has_secondary
    }

    fn cbuf_index(&self) -> u32 {
        self.cbuf_index
    }

    fn cbuf_offset(&self) -> u32 {
        self.cbuf_offset
    }

    fn shift_left(&self) -> u32 {
        self.shift_left
    }

    fn secondary_cbuf_index(&self) -> u32 {
        self.secondary_cbuf_index
    }

    fn secondary_cbuf_offset(&self) -> u32 {
        self.secondary_cbuf_offset
    }

    fn secondary_shift_left(&self) -> u32 {
        self.secondary_shift_left
    }

    fn count(&self) -> u32 {
        self.count
    }

    fn size_shift(&self) -> u32 {
        self.size_shift
    }
}

impl ComputeHandleDescriptor for ImageBufferDescriptor {
    fn cbuf_index(&self) -> u32 {
        self.cbuf_index
    }

    fn cbuf_offset(&self) -> u32 {
        self.cbuf_offset
    }

    fn count(&self) -> u32 {
        self.count
    }

    fn size_shift(&self) -> u32 {
        self.size_shift
    }
}

impl ComputeHandleDescriptor for ImageDescriptor {
    fn cbuf_index(&self) -> u32 {
        self.cbuf_index
    }

    fn cbuf_offset(&self) -> u32 {
        self.cbuf_offset
    }

    fn count(&self) -> u32 {
        self.count
    }

    fn size_shift(&self) -> u32 {
        self.size_shift
    }
}

#[cfg(test)]
#[path = "gl_compute_pipeline_test.rs"]
mod tests;

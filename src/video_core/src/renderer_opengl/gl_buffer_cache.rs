// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `video_core/renderer_opengl/gl_buffer_cache.{h,cpp}`.
//!
//! OpenGL buffer cache -- manages GPU buffer objects for vertex, index, uniform, and storage
//! buffer access.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::sync::OnceLock;

use crate::buffer_cache::buffer_base::BufferBase;
use crate::buffer_cache::buffer_cache_base::DEFAULT_SKIP_CACHE_SIZE;
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::surface::PixelFormat;
use shader_recompiler::backend::glasm::PROGRAM_LOCAL_PARAMETER_STORAGE_BUFFER_BASE;

use super::gl_resource_manager::{OGLBuffer, OGLTexture, OGLTransformFeedback};
use super::gl_staging_buffer_pool::{SharedStagingBufferPool, StagingBufferMap, StreamBuffer};
use common::slot_vector::SlotVector;

type GlGetNamedBufferParameterui64vNv = unsafe extern "system" fn(
    buffer: gl::types::GLuint,
    pname: gl::types::GLenum,
    params: *mut u64,
);
type GlMakeNamedBufferResidentNv =
    unsafe extern "system" fn(buffer: gl::types::GLuint, access: gl::types::GLenum);
type GlMakeNamedBufferNonResidentNv = unsafe extern "system" fn(buffer: gl::types::GLuint);
type GlProgramLocalParametersI4uivNv = unsafe extern "system" fn(
    target: gl::types::GLenum,
    index: gl::types::GLuint,
    count: gl::types::GLsizei,
    params: *const gl::types::GLuint,
);
type GlBufferAddressRangeNv = unsafe extern "system" fn(
    pname: gl::types::GLenum,
    index: gl::types::GLuint,
    address: u64,
    length: gl::types::GLsizeiptr,
);
type GlBindBufferRangeNv = unsafe extern "system" fn(
    target: gl::types::GLenum,
    index: gl::types::GLuint,
    buffer: gl::types::GLuint,
    offset: gl::types::GLintptr,
    size: gl::types::GLsizeiptr,
);
type GlProgramBufferParametersIuivNv = unsafe extern "system" fn(
    target: gl::types::GLenum,
    binding_index: gl::types::GLuint,
    word_index: gl::types::GLuint,
    count: gl::types::GLsizei,
    params: *const gl::types::GLuint,
);

static GL_GET_NAMED_BUFFER_PARAMETER_UI64V_NV: OnceLock<Option<GlGetNamedBufferParameterui64vNv>> =
    OnceLock::new();
static GL_MAKE_NAMED_BUFFER_RESIDENT_NV: OnceLock<Option<GlMakeNamedBufferResidentNv>> =
    OnceLock::new();
static GL_MAKE_NAMED_BUFFER_NON_RESIDENT_NV: OnceLock<Option<GlMakeNamedBufferNonResidentNv>> =
    OnceLock::new();
static GL_PROGRAM_LOCAL_PARAMETERS_I4UIV_NV: OnceLock<Option<GlProgramLocalParametersI4uivNv>> =
    OnceLock::new();
static GL_BUFFER_ADDRESS_RANGE_NV: OnceLock<Option<GlBufferAddressRangeNv>> = OnceLock::new();
static GL_BIND_BUFFER_RANGE_NV: OnceLock<Option<GlBindBufferRangeNv>> = OnceLock::new();
static GL_PROGRAM_BUFFER_PARAMETERS_IUIV_NV: OnceLock<Option<GlProgramBufferParametersIuivNv>> =
    OnceLock::new();

const GL_BUFFER_GPU_ADDRESS_NV: u32 = 0x8F1D;
const GL_VERTEX_ATTRIB_ARRAY_ADDRESS_NV: u32 = 0x8F20;

fn load_optional_gl_function<T, F>(load_fn: &mut F, name: &'static str) -> Option<T>
where
    F: FnMut(&'static str) -> *const c_void,
{
    let ptr = load_fn(name);
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { std::mem::transmute_copy::<*const c_void, T>(&ptr) })
    }
}

pub(crate) fn load_extra_functions<F>(load_fn: &mut F)
where
    F: FnMut(&'static str) -> *const c_void,
{
    let _ = GL_GET_NAMED_BUFFER_PARAMETER_UI64V_NV.set(load_optional_gl_function(
        load_fn,
        "glGetNamedBufferParameterui64vNV",
    ));
    let _ = GL_MAKE_NAMED_BUFFER_RESIDENT_NV.set(load_optional_gl_function(
        load_fn,
        "glMakeNamedBufferResidentNV",
    ));
    let _ = GL_MAKE_NAMED_BUFFER_NON_RESIDENT_NV.set(load_optional_gl_function(
        load_fn,
        "glMakeNamedBufferNonResidentNV",
    ));
    let _ = GL_PROGRAM_LOCAL_PARAMETERS_I4UIV_NV.set(load_optional_gl_function(
        load_fn,
        "glProgramLocalParametersI4uivNV",
    ));
    let _ = GL_BUFFER_ADDRESS_RANGE_NV
        .set(load_optional_gl_function(load_fn, "glBufferAddressRangeNV"));
    let _ = GL_BIND_BUFFER_RANGE_NV.set(load_optional_gl_function(load_fn, "glBindBufferRangeNV"));
    let _ = GL_PROGRAM_BUFFER_PARAMETERS_IUIV_NV.set(load_optional_gl_function(
        load_fn,
        "glProgramBufferParametersIuivNV",
    ));
}

/// NV program stage LUT for bindless SSBO.
const PROGRAM_LUT: [u32; 5] = [
    0x8620, // GL_VERTEX_PROGRAM_NV
    0x891E, // GL_TESS_CONTROL_PROGRAM_NV
    0x891F, // GL_TESS_EVALUATION_PROGRAM_NV
    0x8C26, // GL_GEOMETRY_PROGRAM_NV
    0x8870, // GL_FRAGMENT_PROGRAM_NV
];
const GL_COMPUTE_PROGRAM_NV: u32 = 0x90FB;
const GL_COMPUTE_PROGRAM_PARAMETER_BUFFER_NV: u32 = 0x90FC;
const GL_ELEMENT_ARRAY_ADDRESS_NV: u32 = 0x8F29;

/// Port of anonymous `GetTextureBufferFormat` in `gl_buffer_cache.cpp`.
fn get_texture_buffer_format(gl_format: u32) -> u32 {
    match gl_format {
        gl::RGBA8_SNORM => gl::RGBA8I,
        gl::R8_SNORM => gl::R8I,
        gl::RGBA16_SNORM => gl::RGBA16I,
        gl::R16_SNORM => gl::R16I,
        gl::RG16_SNORM => gl::RG16I,
        gl::RG8_SNORM => gl::RG8I,
        _ => gl_format,
    }
}

// Eden owns these constants in VideoCommon. The local aliases only adapt the
// upstream `u32` values to Rust const-generic array lengths.
const NUM_GRAPHICS_UNIFORM_BUFFERS: usize =
    crate::buffer_cache::buffer_cache_base::NUM_GRAPHICS_UNIFORM_BUFFERS as usize;
const NUM_COMPUTE_UNIFORM_BUFFERS: usize =
    crate::buffer_cache::buffer_cache_base::NUM_COMPUTE_UNIFORM_BUFFERS as usize;
const NUM_STAGES: usize = crate::buffer_cache::buffer_cache_base::NUM_STAGES as usize;

/// Bindless SSBO descriptor layout.
///
/// Corresponds to the anonymous `BindlessSSBO` struct in gl_buffer_cache.cpp.
#[repr(C)]
struct BindlessSSBO {
    address: u64,
    length: i32,
    padding: i32,
}

/// A single buffer view used for texture buffer access.
struct BufferView {
    offset: u32,
    size: u32,
    format: PixelFormat,
    texture: OGLTexture,
}

/// An OpenGL buffer object tracked by the buffer cache.
///
/// Corresponds to `OpenGL::Buffer`.
pub struct Buffer {
    // Rust drops fields in declaration order. Eden destroys `views`, then the
    // `OGLBuffer`, then the `BufferBase` base subobject.
    views: Vec<BufferView>,
    buffer: OGLBuffer,
    base: BufferBase,
    address: u64,
    current_residency_access: u32,
}

impl Buffer {
    /// Create a new buffer.
    ///
    /// Port of `Buffer::Buffer(BufferCacheRuntime&, DAddr, u64)`.
    pub fn new(runtime: &mut BufferCacheRuntime, cpu_addr: u64, size_bytes: u64) -> Self {
        // C++ base subobjects are constructed before the derived constructor
        // body creates the OpenGL buffer.
        let base = BufferBase::new(cpu_addr, size_bytes);
        #[cfg(test)]
        if runtime.device.is_none() {
            return Self {
                views: Vec::new(),
                buffer: OGLBuffer::new(),
                base,
                address: 0,
                current_residency_access: gl::NONE,
            };
        }
        let mut buffer = OGLBuffer::new();
        buffer.create();
        unsafe {
            if runtime.device().has_debugging_tool_attached() {
                let name = format!("Buffer {cpu_addr:#x}");
                gl::ObjectLabel(
                    gl::BUFFER,
                    buffer.handle,
                    name.len() as i32,
                    name.as_ptr().cast(),
                );
            }
            gl::NamedBufferData(
                buffer.handle,
                size_bytes as isize,
                std::ptr::null(),
                gl::DYNAMIC_DRAW,
            );
        }

        let mut result = Self {
            views: Vec::new(),
            buffer,
            base,
            address: 0,
            current_residency_access: gl::NONE,
        };
        if runtime.has_unified_vertex_buffers {
            let get_address = GL_GET_NAMED_BUFFER_PARAMETER_UI64V_NV
                .get()
                .and_then(|f| *f)
                .expect("glGetNamedBufferParameterui64vNV must be loaded for unified buffers");
            unsafe {
                get_address(
                    result.buffer.handle,
                    GL_BUFFER_GPU_ADDRESS_NV,
                    &mut result.address,
                );
            }
        }
        result
    }

    /// Create a null buffer.
    pub fn null(_runtime: &mut BufferCacheRuntime) -> Self {
        Self {
            views: Vec::new(),
            buffer: OGLBuffer::new(),
            base: BufferBase::null(crate::buffer_cache::buffer_base::NullBufferParams),
            address: 0,
            current_residency_access: gl::NONE,
        }
    }

    /// Upload data to the buffer immediately.
    ///
    /// Port of `Buffer::ImmediateUpload`.
    pub fn immediate_upload(&self, offset: usize, data: &[u8]) {
        unsafe {
            gl::NamedBufferSubData(
                self.buffer.handle,
                offset as isize,
                data.len() as isize,
                data.as_ptr() as *const _,
            );
        }
    }

    /// Download data from the buffer immediately.
    ///
    /// Port of `Buffer::ImmediateDownload`.
    pub fn immediate_download(&self, offset: usize, data: &mut [u8]) {
        unsafe {
            gl::GetNamedBufferSubData(
                self.buffer.handle,
                offset as isize,
                data.len() as isize,
                data.as_mut_ptr() as *mut _,
            );
        }
    }

    /// Make the buffer resident for NV unified memory.
    ///
    /// Port of `Buffer::MakeResident`.
    pub fn make_resident(&mut self, access: u32) {
        if access <= self.current_residency_access || self.buffer.handle == 0 {
            return;
        }
        let previous_access = std::mem::replace(&mut self.current_residency_access, access);
        if previous_access != gl::NONE {
            let make_non_resident = GL_MAKE_NAMED_BUFFER_NON_RESIDENT_NV
                .get()
                .and_then(|f| *f)
                .expect("glMakeNamedBufferNonResidentNV must be loaded for unified buffers");
            unsafe { make_non_resident(self.buffer.handle) };
        }
        let make_resident = GL_MAKE_NAMED_BUFFER_RESIDENT_NV
            .get()
            .and_then(|f| *f)
            .expect("glMakeNamedBufferResidentNV must be loaded for unified buffers");
        unsafe { make_resident(self.buffer.handle, access) };
    }

    /// Get or create a texture buffer view.
    ///
    /// Port of `Buffer::View`.
    pub fn view(&mut self, offset: u32, size: u32, format: PixelFormat) -> u32 {
        for v in &self.views {
            if v.offset == offset && v.size == size && v.format == format {
                return v.texture.handle;
            }
        }
        let mut texture = OGLTexture::new();
        texture.create(gl::TEXTURE_BUFFER);
        let gl_format = super::maxwell_to_gl::get_format_tuple(format).internal_format;
        let texture_format = get_texture_buffer_format(gl_format);
        let texture_handle = texture.handle;
        unsafe {
            gl::TextureBufferRange(
                texture.handle,
                texture_format,
                self.buffer.handle,
                offset as isize,
                size as isize,
            );
        }
        self.views.push(BufferView {
            offset,
            size,
            format,
            texture,
        });
        texture_handle
    }

    /// Get the host GPU address (NV unified memory).
    pub fn host_gpu_addr(&self) -> u64 {
        self.address
    }

    /// Return the owned OpenGL buffer name.
    pub fn handle(&self) -> u32 {
        self.buffer.handle
    }
}

impl Deref for Buffer {
    type Target = BufferBase;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for Buffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl crate::buffer_cache::buffer_cache_base::BufferCacheBuffer for Buffer {
    type Runtime = BufferCacheRuntime;

    fn null(
        runtime: &mut Self::Runtime,
        _params: crate::buffer_cache::buffer_base::NullBufferParams,
    ) -> Self {
        Buffer::null(runtime)
    }

    fn new(runtime: &mut Self::Runtime, cpu_addr: u64, size_bytes: u64) -> Self {
        Buffer::new(runtime, cpu_addr, size_bytes)
    }

    fn immediate_upload(&self, offset: u64, data: &[u8]) {
        Buffer::immediate_upload(self, offset as usize, data);
    }

    fn immediate_download(&self, offset: u64, data: &mut [u8]) {
        Buffer::immediate_download(self, offset as usize, data);
    }

    fn raw_handle(&self) -> u64 {
        self.handle() as u64
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // C++ destroys vector elements from the back, then the `OGLBuffer`,
        // then the base subobject. Emptying/releasing here leaves the ordinary
        // Rust field drops as no-ops while preserving that order.
        while self.views.pop().is_some() {}
        self.buffer.release();
    }
}

/// Runtime state for the OpenGL buffer cache.
///
/// Corresponds to `OpenGL::BufferCacheRuntime`.
pub struct BufferCacheRuntime {
    // Owning fields are declared in Eden's destruction order. C++ destroys
    // members in reverse declaration order; Rust drops fields in declaration
    // order.
    transform_feedback_objects: HashMap<u64, OGLTransformFeedback>,
    copy_compute_uniforms: [OGLBuffer; NUM_COMPUTE_UNIFORM_BUFFERS],
    copy_uniforms: [[OGLBuffer; NUM_GRAPHICS_UNIFORM_BUFFERS]; NUM_STAGES],
    fast_uniforms: [[OGLBuffer; NUM_GRAPHICS_UNIFORM_BUFFERS]; NUM_STAGES],
    stream_buffer: Option<StreamBuffer>,
    staging_buffer_pool: SharedStagingBufferPool,

    device: Option<NonNull<super::gl_device::Device>>,
    pub has_fast_buffer_sub_data: bool,
    pub use_assembly_shaders: bool,
    pub has_unified_vertex_buffers: bool,
    pub use_storage_buffers: bool,
    pub max_attributes: u32,

    pub graphics_base_uniform_bindings: [u32; NUM_STAGES],
    pub graphics_base_storage_bindings: [u32; NUM_STAGES],

    pub index_buffer_offset: u32,
    pub device_access_memory: u64,
    texture_handles: *mut u32,
    image_handles: *mut u32,
}

impl BufferCacheRuntime {
    pub const INVALID_BINDING: u8 = u8::MAX;

    /// Private class constant `BufferCacheRuntime::PABO_LUT` from Eden.
    const PABO_LUT: [u32; 5] = [
        0x8DA2, // GL_VERTEX_PROGRAM_PARAMETER_BUFFER_NV
        0x8DA3, // GL_TESS_CONTROL_PROGRAM_PARAMETER_BUFFER_NV
        0x8DA4, // GL_TESS_EVALUATION_PROGRAM_PARAMETER_BUFFER_NV
        0x8DA5, // GL_GEOMETRY_PROGRAM_PARAMETER_BUFFER_NV
        0x8DA6, // GL_FRAGMENT_PROGRAM_PARAMETER_BUFFER_NV
    ];

    /// Create a new buffer cache runtime.
    ///
    /// Port of `BufferCacheRuntime::BufferCacheRuntime()`
    /// (gl_buffer_cache.cpp:139-144). `device_access_memory` is the
    /// per-process VRAM budget. When the NVX_gpu_memory_info extension is
    /// present, upstream sets it to `GetCurrentDedicatedVideoMemory() +
    /// 512 MiB` so the subsequent `GetDeviceMemoryUsage` subtraction stays
    /// non-negative. Otherwise it falls back to a hard-coded 2 GiB
    /// minimum.
    pub fn new(
        device: &super::gl_device::Device,
        staging_buffer_pool: SharedStagingBufferPool,
    ) -> Self {
        let has_fast_buffer_sub_data = device.has_fast_buffer_sub_data();
        let use_assembly_shaders = device.use_assembly_shaders();
        let has_unified_vertex_buffers = device.has_vertex_buffer_unified_memory();
        // Eden constructs this optional member before entering the constructor
        // body that queries limits and allocates the uniform buffers.
        let stream_buffer = if has_fast_buffer_sub_data {
            None
        } else {
            Some(StreamBuffer::new())
        };
        let mut fast_uniforms: [[OGLBuffer; NUM_GRAPHICS_UNIFORM_BUFFERS]; NUM_STAGES] =
            std::array::from_fn(|_| std::array::from_fn(|_| OGLBuffer::new()));
        let mut copy_uniforms: [[OGLBuffer; NUM_GRAPHICS_UNIFORM_BUFFERS]; NUM_STAGES] =
            std::array::from_fn(|_| std::array::from_fn(|_| OGLBuffer::new()));
        let mut copy_compute_uniforms: [OGLBuffer; NUM_COMPUTE_UNIFORM_BUFFERS] =
            std::array::from_fn(|_| OGLBuffer::new());
        let transform_feedback_objects = HashMap::new();

        let mut gl_max_attributes = std::mem::MaybeUninit::<i32>::uninit();
        unsafe {
            gl::GetIntegerv(gl::MAX_VERTEX_ATTRIBS, gl_max_attributes.as_mut_ptr());
        }
        // `glGetIntegerv` is required to initialize its output, exactly like
        // Eden's uninitialized local `GLint gl_max_attributes`.
        let max_attributes = unsafe { gl_max_attributes.assume_init() } as u32;
        for stage_uniforms in &mut fast_uniforms {
            for buffer in stage_uniforms {
                buffer.create();
                unsafe {
                    gl::NamedBufferData(
                        buffer.handle,
                        DEFAULT_SKIP_CACHE_SIZE as isize,
                        std::ptr::null(),
                        gl::STREAM_DRAW,
                    );
                }
            }
        }

        if use_assembly_shaders {
            for stage_uniforms in &mut copy_uniforms {
                for buffer in stage_uniforms {
                    buffer.create();
                    unsafe {
                        gl::NamedBufferData(
                            buffer.handle,
                            0x10_000,
                            std::ptr::null(),
                            gl::STREAM_COPY,
                        );
                    }
                }
            }
            for buffer in &mut copy_compute_uniforms {
                buffer.create();
                unsafe {
                    gl::NamedBufferData(buffer.handle, 0x10_000, std::ptr::null(), gl::STREAM_COPY);
                }
            }
        }

        const HALF_GIB: u64 = 512 * 1024 * 1024;
        let device_access_memory = if device.can_report_memory_usage() {
            device
                .get_current_dedicated_video_memory()
                .wrapping_add(HALF_GIB)
        } else {
            2 * 1024 * 1024 * 1024
        };

        Self {
            transform_feedback_objects,
            copy_compute_uniforms,
            copy_uniforms,
            fast_uniforms,
            stream_buffer,
            staging_buffer_pool,
            device: Some(NonNull::from(device)),
            has_fast_buffer_sub_data,
            use_assembly_shaders,
            has_unified_vertex_buffers,
            use_storage_buffers: false,
            max_attributes,
            graphics_base_uniform_bindings: [0; NUM_STAGES],
            graphics_base_storage_bindings: [0; NUM_STAGES],
            index_buffer_offset: 0,
            device_access_memory,
            texture_handles: std::ptr::null_mut(),
            image_handles: std::ptr::null_mut(),
        }
    }

    fn device(&self) -> &super::gl_device::Device {
        // SAFETY: RendererOpenGL owns the boxed Device for longer than its
        // boxed rasterizer and buffer-cache runtime.
        unsafe {
            self.device
                .expect("OpenGL device is unavailable in a context-free unit test")
                .as_ref()
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(staging_buffer_pool: SharedStagingBufferPool) -> Self {
        Self {
            transform_feedback_objects: HashMap::new(),
            copy_compute_uniforms: std::array::from_fn(|_| OGLBuffer::new()),
            copy_uniforms: std::array::from_fn(|_| std::array::from_fn(|_| OGLBuffer::new())),
            fast_uniforms: std::array::from_fn(|_| std::array::from_fn(|_| OGLBuffer::new())),
            stream_buffer: None,
            staging_buffer_pool,
            device: None,
            has_fast_buffer_sub_data: false,
            use_assembly_shaders: false,
            has_unified_vertex_buffers: false,
            use_storage_buffers: false,
            max_attributes: 16,
            graphics_base_uniform_bindings: [0; NUM_STAGES],
            graphics_base_storage_bindings: [0; NUM_STAGES],
            index_buffer_offset: 0,
            device_access_memory: 2 * 1024 * 1024 * 1024,
            texture_handles: std::ptr::null_mut(),
            image_handles: std::ptr::null_mut(),
        }
    }

    /// Set base uniform bindings for graphics stages.
    pub fn set_base_uniform_bindings(&mut self, bindings: &[u32; NUM_STAGES]) {
        self.graphics_base_uniform_bindings = *bindings;
    }

    /// Set base storage bindings for graphics stages.
    pub fn set_base_storage_bindings(&mut self, bindings: &[u32; NUM_STAGES]) {
        self.graphics_base_storage_bindings = *bindings;
    }

    /// Set whether to use storage buffers.
    pub fn set_enable_storage_buffers(&mut self, enable: bool) {
        self.use_storage_buffers = enable;
    }

    /// Set output arrays for texture/image-buffer handles.
    ///
    /// Port of upstream `BufferCacheRuntime::SetImagePointers`.
    pub fn set_image_pointers(&mut self, texture_handles: *mut u32, image_handles: *mut u32) {
        self.texture_handles = texture_handles;
        self.image_handles = image_handles;
    }

    /// Pre-copy memory barrier.
    pub fn pre_copy_barrier(&self) {
        #[cfg(test)]
        if self.device.is_none() {
            return;
        }
        unsafe {
            gl::MemoryBarrier(gl::ALL_BARRIER_BITS);
        }
    }

    /// Post-copy memory barrier.
    pub fn post_copy_barrier(&self) {
        #[cfg(test)]
        if self.device.is_none() {
            return;
        }
        unsafe {
            gl::MemoryBarrier(gl::BUFFER_UPDATE_BARRIER_BIT | gl::CLIENT_MAPPED_BUFFER_BARRIER_BIT);
        }
    }

    fn copy_buffer_handles(
        &mut self,
        dst_buffer: u32,
        src_buffer: u32,
        copies: &[crate::buffer_cache::buffer_cache_base::BufferCopy],
        barrier: bool,
    ) {
        #[cfg(test)]
        if self.device.is_none() {
            return;
        }
        if barrier {
            self.pre_copy_barrier();
        }
        unsafe {
            for copy in copies {
                gl::CopyNamedBufferSubData(
                    src_buffer,
                    dst_buffer,
                    copy.src_offset as isize,
                    copy.dst_offset as isize,
                    copy.size as isize,
                );
            }
        }
        if barrier {
            self.post_copy_barrier();
        }
    }

    /// Finish all pending GL operations.
    pub fn finish(&self) {
        #[cfg(test)]
        if self.device.is_none() {
            return;
        }
        unsafe {
            gl::Finish();
        }
    }

    /// Get device memory usage.
    ///
    /// Port of `BufferCacheRuntime::GetDeviceMemoryUsage` (gl_buffer_cache
    /// .cpp:159-164). Upstream uses
    /// `GL_GPU_MEMORY_INFO_TOTAL_AVAILABLE_MEMORY_NVX = 0x9048` (not the
    /// CURRENT_AVAILABLE variant 0x9049). Since `device_access_memory`
    /// was initialised in the ctor as `total + 512 MiB`, this returns a
    /// roughly-constant 512 MiB headroom value when the NVX extension is
    /// active — same as upstream.
    ///
    /// The 2 GiB fallback (no NVX) matches upstream returning `2_GiB`.
    pub fn get_device_memory_usage(&self) -> u64 {
        if !self
            .device
            .is_some_and(|device| unsafe { device.as_ref() }.can_report_memory_usage())
        {
            return 2 * 1024 * 1024 * 1024;
        }
        self.device_access_memory
            .wrapping_sub(self.device().get_current_dedicated_video_memory())
    }

    /// Get device local memory.
    pub fn get_device_local_memory(&self) -> u64 {
        self.device_access_memory
    }

    /// Whether non-zero uniform offsets are supported.
    pub fn supports_non_zero_uniform_offset(&self) -> bool {
        !self.use_assembly_shaders
    }

    /// Has fast buffer sub data extension.
    pub fn has_fast_buffer_sub_data(&self) -> bool {
        self.has_fast_buffer_sub_data
    }

    /// Index offset for element array draws.
    pub fn index_offset(&self) -> usize {
        self.index_buffer_offset as usize
    }

    /// Port of upstream `BufferCacheRuntime::BindVertexBuffer`.
    pub fn bind_vertex_buffer(
        &mut self,
        index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        stride: u32,
    ) {
        if index >= self.max_attributes {
            return;
        }
        unsafe {
            if self.has_unified_vertex_buffers {
                buffer.make_resident(gl::READ_ONLY);
                gl::BindVertexBuffer(index, 0, 0, stride as i32);
                let buffer_address_range = GL_BUFFER_ADDRESS_RANGE_NV
                    .get()
                    .and_then(|function| *function)
                    .expect("glBufferAddressRangeNV must be loaded for unified vertex buffers");
                buffer_address_range(
                    GL_VERTEX_ATTRIB_ARRAY_ADDRESS_NV,
                    index,
                    buffer.host_gpu_addr().wrapping_add(u64::from(offset)),
                    size as isize,
                );
            } else {
                gl::BindVertexBuffer(index, buffer.handle(), offset as isize, stride as i32);
            }
        }
    }

    /// Port of upstream `BufferCacheRuntime::BindTransformFeedbackBuffer`.
    pub fn bind_transform_feedback_buffer(
        &self,
        index: u32,
        buffer: &Buffer,
        offset: u32,
        size: u32,
    ) {
        unsafe {
            gl::BindBufferRange(
                gl::TRANSFORM_FEEDBACK_BUFFER,
                index,
                buffer.handle(),
                offset as isize,
                size as isize,
            );
        }
    }
}

impl Drop for BufferCacheRuntime {
    fn drop(&mut self) {
        // Reproduce reverse C++ member destruction despite Rust dropping
        // struct fields in declaration order. Every release leaves a zero
        // wrapper, so the wrappers' own Drop remains the final safety net.
        self.transform_feedback_objects.clear();
        for buffer in self.copy_compute_uniforms.iter_mut().rev() {
            buffer.release();
        }
        for stage_uniforms in self.copy_uniforms.iter_mut().rev() {
            for buffer in stage_uniforms.iter_mut().rev() {
                buffer.release();
            }
        }
        for stage_uniforms in self.fast_uniforms.iter_mut().rev() {
            for buffer in stage_uniforms.iter_mut().rev() {
                buffer.release();
            }
        }
        drop(self.stream_buffer.take());
    }
}

use crate::buffer_cache::buffer_cache_base::{self as base, BufferCopy, HostBindings};

impl base::BufferCacheRuntime for BufferCacheRuntime {
    type Buffer = Buffer;
    type AsyncBuffer = StagingBufferMap;

    fn tick_frame(&mut self, _slot_buffers: &mut SlotVector<Buffer>) {}

    fn can_report_memory_usage(&self) -> bool {
        self.device
            .is_some_and(|device| unsafe { device.as_ref() }.can_report_memory_usage())
    }

    fn get_device_local_memory(&self) -> u64 {
        self.device_access_memory
    }

    fn get_device_memory_usage(&self) -> u64 {
        self.get_device_memory_usage()
    }

    fn get_storage_buffer_alignment(&self) -> u32 {
        self.device.map_or(1, |device| {
            unsafe { device.as_ref() }.shader_storage_buffer_alignment() as u32
        })
    }

    fn uniform_buffer_alignment(&self) -> u32 {
        self.device.map_or(1, |device| {
            unsafe { device.as_ref() }.uniform_buffer_alignment() as u32
        })
    }

    fn finish(&mut self) {
        BufferCacheRuntime::finish(self);
    }

    fn upload_staging_buffer(&mut self, size: u64) -> StagingBufferMap {
        self.staging_buffer_pool
            .lock()
            .request_upload_buffer(size as usize)
    }

    fn download_staging_buffer(&mut self, size: u64, deferred: bool) -> StagingBufferMap {
        self.staging_buffer_pool
            .lock()
            .request_download_buffer(size as usize, deferred)
    }

    fn free_deferred_staging_buffer(&mut self, buffer: &mut StagingBufferMap) {
        self.staging_buffer_pool
            .lock()
            .free_deferred_staging_buffer(buffer);
    }

    fn can_reorder_upload(&self, _buffer: &Buffer, _copies: &[BufferCopy]) -> bool {
        false
    }

    fn pre_copy_barrier(&mut self) {
        BufferCacheRuntime::pre_copy_barrier(self);
    }

    fn post_copy_barrier(&mut self) {
        BufferCacheRuntime::post_copy_barrier(self);
    }

    fn copy_buffer(
        &mut self,
        dst: &Buffer,
        src: &Buffer,
        copies: &[BufferCopy],
        _barrier: bool,
        _can_reorder: bool,
    ) {
        // The Buffer-to-Buffer overload ignores its boolean parameter in Eden
        // and always brackets the copy with both barriers.
        self.copy_buffer_handles(dst.handle(), src.handle(), copies, true);
    }

    fn copy_buffer_from_staging(
        &mut self,
        dst: &Buffer,
        src: &StagingBufferMap,
        copies: &[BufferCopy],
        barrier: bool,
        _can_reorder: bool,
    ) {
        self.copy_buffer_handles(dst.handle(), src.buffer, copies, barrier);
    }

    fn copy_buffer_to_staging(
        &mut self,
        dst: &StagingBufferMap,
        src: &Buffer,
        copies: &[BufferCopy],
        barrier: bool,
    ) {
        self.copy_buffer_handles(dst.buffer, src.handle(), copies, barrier);
    }

    fn clear_buffer(&mut self, buffer: &Buffer, offset: u32, size: u64, value: u32) {
        unsafe {
            gl::ClearNamedBufferSubData(
                buffer.handle(),
                gl::R32UI,
                offset as isize,
                size as isize,
                gl::RED,
                gl::UNSIGNED_INT,
                &value as *const u32 as *const _,
            );
        }
    }

    /// Port of upstream `BufferCacheRuntime::BindIndexBuffer`
    /// (`gl_buffer_cache.cpp:215`).
    fn bind_index_buffer(
        &mut self,
        _topology: crate::engines::maxwell_3d::PrimitiveTopology,
        _index_format: crate::engines::maxwell_3d::IndexFormat,
        _base_vertex: u32,
        _num_indices: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
    ) {
        if self.has_unified_vertex_buffers {
            buffer.make_resident(gl::READ_ONLY);
            let buffer_address_range = GL_BUFFER_ADDRESS_RANGE_NV
                .get()
                .and_then(|function| *function)
                .expect("glBufferAddressRangeNV must be loaded for unified index buffers");
            unsafe {
                buffer_address_range(
                    GL_ELEMENT_ARRAY_ADDRESS_NV,
                    0,
                    buffer.host_gpu_addr().wrapping_add(u64::from(offset)),
                    common::alignment::align_up(u64::from(size), 4) as u32 as isize,
                );
            }
        } else {
            unsafe {
                gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, buffer.handle());
            }
            self.index_buffer_offset = offset;
        }
    }

    fn index_offset(&self) -> usize {
        BufferCacheRuntime::index_offset(self)
    }

    fn bind_vertex_buffer(
        &mut self,
        index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        stride: u32,
    ) {
        BufferCacheRuntime::bind_vertex_buffer(self, index, buffer, offset, size, stride);
    }

    /// Port of upstream `BufferCacheRuntime::BindVertexBuffers`
    /// (`gl_buffer_cache.cpp:242`).
    fn bind_vertex_buffers(&mut self, bindings: &HostBindings, buffers: &mut SlotVector<Buffer>) {
        let count = bindings
            .buffer_ids
            .len()
            .min(self.max_attributes.wrapping_sub(bindings.min_index) as usize);
        let mut handles = [0u32; 32];
        let mut strides = [0i32; 32];
        for (index, buffer_id) in bindings.buffer_ids.iter().enumerate() {
            handles[index] = buffers[*buffer_id].handle();
        }
        for (index, stride) in bindings.strides.iter().enumerate() {
            strides[index] = *stride as i32;
        }
        if self.has_unified_vertex_buffers {
            let buffer_address_range = GL_BUFFER_ADDRESS_RANGE_NV
                .get()
                .and_then(|f| *f)
                .expect("glBufferAddressRangeNV must be loaded for unified vertex buffers");
            for index in 0..count {
                let buffer = &mut buffers[bindings.buffer_ids[index]];
                buffer.make_resident(gl::READ_ONLY);
                unsafe {
                    buffer_address_range(
                        GL_VERTEX_ATTRIB_ARRAY_ADDRESS_NV,
                        bindings.min_index.wrapping_add(index as u32),
                        buffer.host_gpu_addr().wrapping_add(bindings.offsets[index]),
                        bindings.sizes[index] as isize,
                    );
                }
            }
            const ZEROS: [usize; 32] = [0; 32];
            unsafe {
                gl::BindVertexBuffers(
                    bindings.min_index,
                    count as i32,
                    ZEROS.as_ptr().cast(),
                    ZEROS.as_ptr().cast(),
                    strides.as_ptr(),
                );
            }
        } else {
            unsafe {
                gl::BindVertexBuffers(
                    bindings.min_index,
                    count as i32,
                    handles.as_ptr(),
                    bindings.offsets.as_ptr().cast(),
                    strides.as_ptr(),
                );
            }
        }
    }

    fn bind_uniform_buffer(
        &mut self,
        stage: usize,
        binding_index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
    ) {
        let gpu_handle = buffer.handle();
        if self.use_assembly_shaders {
            let handle = if offset != 0 {
                let copy = self.copy_uniforms[stage][binding_index as usize].handle;
                unsafe {
                    gl::CopyNamedBufferSubData(gpu_handle, copy, offset as isize, 0, size as isize);
                }
                copy
            } else {
                gpu_handle
            };
            let bind_buffer_range = GL_BIND_BUFFER_RANGE_NV
                .get()
                .and_then(|function| *function)
                .expect("glBindBufferRangeNV must be loaded for GLASM uniform buffers");
            unsafe {
                bind_buffer_range(
                    Self::PABO_LUT[stage],
                    binding_index,
                    handle,
                    0,
                    size as isize,
                );
            }
            return;
        }
        let base_binding = self.graphics_base_uniform_bindings[stage];
        let binding = base_binding.wrapping_add(binding_index);
        unsafe {
            gl::BindBufferRange(
                gl::UNIFORM_BUFFER,
                binding,
                gpu_handle,
                offset as isize,
                size as isize,
            );
        }
    }

    fn set_base_uniform_bindings(&mut self, bindings: &[u32; NUM_STAGES]) {
        BufferCacheRuntime::set_base_uniform_bindings(self, bindings);
    }

    fn set_base_storage_bindings(&mut self, bindings: &[u32; NUM_STAGES]) {
        BufferCacheRuntime::set_base_storage_bindings(self, bindings);
    }

    fn set_enable_storage_buffers(&mut self, enable: bool) {
        BufferCacheRuntime::set_enable_storage_buffers(self, enable);
    }

    fn set_image_pointers(&mut self, texture_handles: *mut u32, image_handles: *mut u32) {
        BufferCacheRuntime::set_image_pointers(self, texture_handles, image_handles);
    }

    fn bind_storage_buffer(
        &mut self,
        stage: usize,
        binding_index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        is_written: bool,
    ) {
        if self.use_storage_buffers {
            let base_binding = self.graphics_base_storage_bindings[stage];
            let binding = base_binding.wrapping_add(binding_index);
            unsafe {
                gl::BindBufferRange(
                    gl::SHADER_STORAGE_BUFFER,
                    binding,
                    buffer.handle(),
                    offset as isize,
                    size as isize,
                );
            }
        } else {
            let ssbo = BindlessSSBO {
                address: buffer.host_gpu_addr().wrapping_add(u64::from(offset)),
                length: size as i32,
                padding: 0,
            };
            buffer.make_resident(if is_written {
                gl::READ_WRITE
            } else {
                gl::READ_ONLY
            });
            let program_local_parameters = GL_PROGRAM_LOCAL_PARAMETERS_I4UIV_NV
                .get()
                .and_then(|function| *function)
                .expect(
                    "glProgramLocalParametersI4uivNV must be loaded for GLASM bindless buffers",
                );
            unsafe {
                program_local_parameters(
                    PROGRAM_LUT[stage],
                    PROGRAM_LOCAL_PARAMETER_STORAGE_BUFFER_BASE.wrapping_add(binding_index),
                    1,
                    (&ssbo as *const BindlessSSBO).cast(),
                );
            }
        }
    }

    fn bind_texture_buffer(
        &mut self,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        format: PixelFormat,
    ) {
        let texture = buffer.view(offset, size, format);
        unsafe {
            *self.texture_handles = texture;
            self.texture_handles = self.texture_handles.add(1);
        }
    }

    fn bind_image_buffer(
        &mut self,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        format: PixelFormat,
    ) {
        let texture = buffer.view(offset, size, format);
        unsafe {
            *self.image_handles = texture;
            self.image_handles = self.image_handles.add(1);
        }
    }

    fn bind_transform_feedback_buffers(
        &mut self,
        bindings: &HostBindings,
        buffers: &mut SlotVector<Buffer>,
    ) {
        let mut buffer_handles = [0u32; 4];
        for (index, buffer_id) in bindings.buffer_ids.iter().enumerate() {
            buffer_handles[index] = buffers[*buffer_id].handle();
        }
        unsafe {
            gl::BindBuffersRange(
                gl::TRANSFORM_FEEDBACK_BUFFER,
                0,
                bindings.buffer_ids.len() as i32,
                buffer_handles.as_ptr(),
                bindings.offsets.as_ptr().cast(),
                bindings.sizes.as_ptr().cast(),
            );
        }
    }

    fn bind_transform_feedback_object(&mut self, tfb_object_addr: u64) {
        let object = self
            .transform_feedback_objects
            .entry(tfb_object_addr)
            .or_default();
        object.create();
        unsafe {
            gl::BindTransformFeedback(gl::TRANSFORM_FEEDBACK, object.handle);
        }
    }

    fn get_transform_feedback_object(&mut self, tfb_object_addr: u64) -> u32 {
        if !self
            .transform_feedback_objects
            .contains_key(&tfb_object_addr)
        {
            // Eden's ASSERT is fail-soft. Its following operator[] then
            // inserts the default zero-name wrapper.
            log::error!(
                "BufferCacheRuntime::GetTransformFeedbackObject: unregistered address {tfb_object_addr:#x}"
            );
        }
        self.transform_feedback_objects
            .entry(tfb_object_addr)
            .or_default()
            .handle
    }

    fn bind_compute_uniform_buffer(
        &mut self,
        binding: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
    ) {
        let gpu_handle = buffer.handle();
        if self.use_assembly_shaders {
            let handle = if offset != 0 {
                let copy = self.copy_compute_uniforms[binding as usize].handle;
                unsafe {
                    gl::CopyNamedBufferSubData(gpu_handle, copy, offset as isize, 0, size as isize);
                }
                copy
            } else {
                gpu_handle
            };
            let bind_buffer_range = GL_BIND_BUFFER_RANGE_NV
                .get()
                .and_then(|function| *function)
                .expect("glBindBufferRangeNV must be loaded for GLASM compute uniform buffers");
            unsafe {
                bind_buffer_range(
                    GL_COMPUTE_PROGRAM_PARAMETER_BUFFER_NV,
                    binding,
                    handle,
                    0,
                    size as isize,
                );
            }
        } else {
            unsafe {
                gl::BindBufferRange(
                    gl::UNIFORM_BUFFER,
                    binding,
                    gpu_handle,
                    offset as isize,
                    size as isize,
                );
            }
        }
    }

    fn bind_compute_storage_buffer(
        &mut self,
        binding: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        is_written: bool,
    ) {
        if self.use_storage_buffers {
            unsafe {
                if size != 0 {
                    gl::BindBufferRange(
                        gl::SHADER_STORAGE_BUFFER,
                        binding,
                        buffer.handle(),
                        offset as isize,
                        size as isize,
                    );
                } else {
                    gl::BindBufferRange(gl::SHADER_STORAGE_BUFFER, binding, 0, 0, 0);
                }
            }
        } else {
            let ssbo = BindlessSSBO {
                address: buffer.host_gpu_addr().wrapping_add(u64::from(offset)),
                length: size as i32,
                padding: 0,
            };
            buffer.make_resident(if is_written {
                gl::READ_WRITE
            } else {
                gl::READ_ONLY
            });
            let program_local_parameters = GL_PROGRAM_LOCAL_PARAMETERS_I4UIV_NV
                .get()
                .and_then(|function| *function)
                .expect(
                    "glProgramLocalParametersI4uivNV must be loaded for GLASM bindless buffers",
                );
            unsafe {
                program_local_parameters(
                    GL_COMPUTE_PROGRAM_NV,
                    PROGRAM_LOCAL_PARAMETER_STORAGE_BUFFER_BASE.wrapping_add(binding),
                    1,
                    (&ssbo as *const BindlessSSBO).cast(),
                );
            }
        }
    }

    fn has_fast_buffer_sub_data(&self) -> bool {
        self.has_fast_buffer_sub_data
    }

    fn supports_non_zero_uniform_offset(&self) -> bool {
        !self.use_assembly_shaders
    }

    fn bind_fast_uniform_buffer(&mut self, stage: usize, binding_index: u32, size: u32) {
        let handle = self.fast_uniforms[stage][binding_index as usize].handle;
        if self.use_assembly_shaders {
            let bind_buffer_range = GL_BIND_BUFFER_RANGE_NV
                .get()
                .and_then(|function| *function)
                .expect("glBindBufferRangeNV must be loaded for GLASM fast uniform buffers");
            unsafe {
                bind_buffer_range(
                    Self::PABO_LUT[stage],
                    binding_index,
                    handle,
                    0,
                    size as isize,
                );
            }
            return;
        }
        let base_binding = self.graphics_base_uniform_bindings[stage];
        let binding = base_binding.wrapping_add(binding_index);
        unsafe {
            gl::BindBufferRange(gl::UNIFORM_BUFFER, binding, handle, 0, size as isize);
        }
    }

    fn push_fast_uniform_buffer(&mut self, stage: usize, binding_index: u32, data: &[u8]) {
        if self.use_assembly_shaders {
            let program_buffer_parameters = GL_PROGRAM_BUFFER_PARAMETERS_IUIV_NV
                .get()
                .and_then(|function| *function)
                .expect("glProgramBufferParametersIuivNV must be loaded for GLASM fast uniforms");
            unsafe {
                program_buffer_parameters(
                    Self::PABO_LUT[stage],
                    binding_index,
                    0,
                    (data.len() / std::mem::size_of::<u32>()) as i32,
                    data.as_ptr().cast(),
                );
            }
            return;
        }
        let handle = self.fast_uniforms[stage][binding_index as usize].handle;
        unsafe {
            gl::NamedBufferSubData(handle, 0, data.len() as isize, data.as_ptr() as *const _);
        }
    }

    fn with_mapped_uniform_buffer(
        &mut self,
        stage: usize,
        binding_index: u32,
        size: u32,
        write: &mut dyn FnMut(&mut [u8]),
    ) -> bool {
        let Some(stream_buffer) = self.stream_buffer.as_mut() else {
            return false;
        };
        let (mapped_ptr, offset) = stream_buffer.request(size as usize);
        unsafe {
            let span = std::slice::from_raw_parts_mut(mapped_ptr, size as usize);
            write(span);
            let base_binding = self.graphics_base_uniform_bindings[stage];
            let binding = base_binding.wrapping_add(binding_index);
            gl::BindBufferRange(
                gl::UNIFORM_BUFFER,
                binding,
                stream_buffer.handle(),
                offset as isize,
                size as isize,
            );
        }
        true
    }
}

/// Buffer cache parameters matching upstream `BufferCacheParams`.
pub struct BufferCacheParams;

impl BufferCacheParams {
    pub const IS_OPENGL: bool = true;
    pub const HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS: bool = true;
    pub const HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT: bool = true;
    pub const NEEDS_BIND_UNIFORM_INDEX: bool = true;
    pub const NEEDS_BIND_STORAGE_INDEX: bool = true;
    pub const USE_MEMORY_MAPS: bool = true;
    pub const SEPARATE_IMAGE_BUFFER_BINDINGS: bool = true;
    pub const USE_MEMORY_MAPS_FOR_UPLOADS: bool = false;
}

impl crate::buffer_cache::buffer_cache_base::BufferCacheParams for BufferCacheParams {
    type Runtime = BufferCacheRuntime;
    type Buffer = Buffer;
    type AsyncBuffer = StagingBufferMap;

    const IS_OPENGL: bool = Self::IS_OPENGL;
    const HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS: bool =
        Self::HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS;
    const HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT: bool = Self::HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT;
    const NEEDS_BIND_UNIFORM_INDEX: bool = Self::NEEDS_BIND_UNIFORM_INDEX;
    const NEEDS_BIND_STORAGE_INDEX: bool = Self::NEEDS_BIND_STORAGE_INDEX;
    const USE_MEMORY_MAPS: bool = Self::USE_MEMORY_MAPS;
    const SEPARATE_IMAGE_BUFFER_BINDINGS: bool = Self::SEPARATE_IMAGE_BUFFER_BINDINGS;
    const USE_MEMORY_MAPS_FOR_UPLOADS: bool = Self::USE_MEMORY_MAPS_FOR_UPLOADS;
}

/// OpenGL specialization matching upstream's `using BufferCache` alias.
pub type BufferCache =
    crate::buffer_cache::buffer_cache::BufferCache<BufferCacheParams, MaxwellDeviceMemoryManager>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pabo_lut_size() {
        assert_eq!(BufferCacheRuntime::PABO_LUT.len(), 5);
        assert_eq!(PROGRAM_LUT.len(), 5);
    }

    #[test]
    fn constants() {
        assert_eq!(NUM_GRAPHICS_UNIFORM_BUFFERS, 18);
        assert_eq!(NUM_COMPUTE_UNIFORM_BUFFERS, 8);
        assert_eq!(NUM_STAGES, 5);
        assert_eq!(GL_COMPUTE_PROGRAM_PARAMETER_BUFFER_NV, 0x90FC);
        assert_eq!(GL_ELEMENT_ARRAY_ADDRESS_NV, 0x8F29);
        assert_eq!(BufferCacheRuntime::INVALID_BINDING, u8::MAX);
    }

    #[test]
    fn buffer_cache_params() {
        assert!(BufferCacheParams::IS_OPENGL);
        assert!(BufferCacheParams::HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS);
        assert!(BufferCacheParams::USE_MEMORY_MAPS);
    }

    #[test]
    fn bindless_ssbo_layout() {
        assert_eq!(std::mem::size_of::<BindlessSSBO>(), 16);
    }

    #[test]
    fn unsigned_address_and_index_size_arithmetic_matches_cpp_widths() {
        assert_eq!(u64::MAX.wrapping_add(1), 0);
        assert_eq!(u32::MAX.wrapping_add(1), 0);
        assert_eq!(
            common::alignment::align_up(u64::from(u32::MAX), 4) as u32,
            0
        );
    }

    #[test]
    fn context_free_runtime_uses_empty_resource_owners() {
        let pool = super::super::gl_staging_buffer_pool::make_shared_staging_buffer_pool();
        let mut runtime = BufferCacheRuntime::new_for_test(pool);
        let buffer = Buffer::null(&mut runtime);

        assert_eq!(buffer.handle(), 0);
        assert!(buffer.views.is_empty());
        assert!(runtime
            .fast_uniforms
            .iter()
            .flatten()
            .all(|buffer| buffer.handle == 0));
        assert!(runtime
            .copy_uniforms
            .iter()
            .flatten()
            .all(|buffer| buffer.handle == 0));
        assert!(runtime
            .copy_compute_uniforms
            .iter()
            .all(|buffer| buffer.handle == 0));
    }

    #[test]
    fn missing_transform_feedback_lookup_is_fail_soft_like_eden() {
        let pool = super::super::gl_staging_buffer_pool::make_shared_staging_buffer_pool();
        let mut runtime = BufferCacheRuntime::new_for_test(pool);

        let handle = base::BufferCacheRuntime::get_transform_feedback_object(&mut runtime, 0x1234);

        assert_eq!(handle, 0);
        assert!(runtime.transform_feedback_objects.contains_key(&0x1234));
    }
}

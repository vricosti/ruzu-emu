// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `src/video_core/renderer_opengl/gl_rasterizer.h` and
//! `gl_rasterizer.cpp`.
//!
//! OpenGL rasterizer — processes Maxwell 3D draw commands using OpenGL and
//! implements [`RasterizerInterface`].

use log::{debug, error};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::OnceLock;

use common::settings;

use super::blit_image::BlitImageHelper;
use super::gl_buffer_cache::BufferCache as OpenGLBufferCache;
use super::gl_device::Device;
use super::gl_fence_manager::FenceManagerOpenGL;
use super::gl_query_cache::QueryCache;
use super::gl_shader_cache::ShaderCache as OpenGLShaderCache;
#[cfg(test)]
use super::gl_shader_manager::ProgramManager;
use super::gl_shader_manager::ProgramManagerHandle;
use super::gl_staging_buffer_pool::{make_shared_staging_buffer_pool, SharedStagingBufferPool};
use super::gl_state_tracker::{dirty as GlDirty, StateTracker};
use super::gl_texture_cache::TextureCache as OpenGLTextureCache;
use crate::buffer_cache::buffer_cache_base::{ObtainBufferOperation, ObtainBufferSynchronize};
use crate::cache_types::CacheType;
use crate::control::channel_state_cache::{ChannelCacheAccessor, ChannelInfo, ChannelSetupCaches};
use crate::engines::draw_manager::{
    DrawState, IndirectParams, Maxwell3DClearView, Maxwell3DDrawTextureView, Maxwell3DDrawView,
    Maxwell3DIndirectView,
};
use crate::engines::kepler_compute::DispatchCall;
use crate::engines::maxwell_3d::{
    BlendEquation, BlendFactor, ComparisonOp, CullFace, DepthMode, FillViaTriangleMode, FrontFace,
    PolygonMode, ShaderStageType, StencilFaceInfo, StencilOp, VertexAttribType,
};
use crate::engines::maxwell_dma::{dma, AccelerateDMAInterface};
use crate::framebuffer_config::FramebufferConfig;
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::host1x::syncpoint_manager::SyncpointManager;
use crate::query_cache::types::QueryPropertiesFlags;
use crate::query_cache_top::QueryType as VideoQueryType;
use crate::rasterizer_interface::{RasterizerDownloadArea, RasterizerInterface};
use crate::renderer_base::GuestMemoryWriter;
use crate::renderer_opengl::gl_blit_screen::FramebufferTextureInfo;
use crate::shader_cache::ShaderCache;
use crate::texture_cache::texture_cache::RenderTargetDirtyFlagAccess;
use crate::texture_cache::types::{Extent3D, Offset2D, Region2D, NULL_IMAGE_ID};

macro_rules! lock_two_reentrant_mutexes {
    ($first_mutex:expr, $second_mutex:expr, $first_guard:ident, $second_guard:ident) => {
        let $first_guard;
        let $second_guard;
        loop {
            let first_candidate = unsafe { (*$first_mutex).lock() };
            if let Some(second_candidate) = unsafe { (*$second_mutex).try_lock() } {
                $first_guard = first_candidate;
                $second_guard = second_candidate;
                break;
            }
            drop(first_candidate);
            std::thread::yield_now();

            let second_candidate = unsafe { (*$second_mutex).lock() };
            if let Some(first_candidate) = unsafe { (*$first_mutex).try_lock() } {
                $first_guard = first_candidate;
                $second_guard = second_candidate;
                break;
            }
            drop(second_candidate);
            std::thread::yield_now();
        }
    };
}

/// OpenGL DMA accelerator owned by `RasterizerOpenGL`.
///
/// Eden stores `BufferCache&` and `TextureCache&` members.  The Rust caches
/// are boxed before this object is created, so these non-owning pointers stay
/// stable for the accelerator's entire lifetime.
struct AccelerateDMA {
    buffer_cache: NonNull<OpenGLBufferCache>,
    texture_cache: NonNull<OpenGLTextureCache>,
}

impl AccelerateDMA {
    fn new(buffer_cache: &mut OpenGLBufferCache, texture_cache: &mut OpenGLTextureCache) -> Self {
        Self {
            buffer_cache: NonNull::from(buffer_cache),
            texture_cache: NonNull::from(texture_cache),
        }
    }

    fn dma_buffer_image_copy(
        &mut self,
        copy_info: &dma::ImageCopy,
        buffer_operand: &dma::BufferOperand,
        image_operand: &dma::ImageOperand,
        is_image_upload: bool,
    ) -> bool {
        let buffer_cache = unsafe { self.buffer_cache.as_mut() };
        let texture_cache = unsafe { self.texture_cache.as_mut() };
        let buffer_mutex: *const _ = &buffer_cache.mutex;
        let texture_mutex: *const _ = &texture_cache.base.mutex;
        lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);

        let image_id = texture_cache
            .base
            .dma_image_id(image_operand, is_image_upload);
        if image_id == NULL_IMAGE_ID {
            return false;
        }
        let buffer_size = buffer_operand.pitch.wrapping_mul(buffer_operand.height);
        let post_op = if is_image_upload {
            ObtainBufferOperation::DoNothing
        } else {
            ObtainBufferOperation::MarkAsWritten
        };
        let (buffer_id, offset) = buffer_cache.obtain_buffer(
            buffer_operand.address,
            buffer_size,
            ObtainBufferSynchronize::FullSynchronize,
            post_op,
        );
        let buffer_handle = buffer_cache.get_buffer_gpu_handle(buffer_id);
        texture_cache.dma_buffer_image_copy(
            copy_info,
            buffer_operand,
            image_operand,
            image_id,
            buffer_handle,
            offset as usize,
            is_image_upload,
        )
    }
}

impl AccelerateDMAInterface for AccelerateDMA {
    fn buffer_copy(&mut self, src_address: u64, dest_address: u64, amount: u64) -> bool {
        unsafe {
            let buffer_cache = self.buffer_cache.as_mut();
            let buffer_mutex: *const _ = &buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            buffer_cache.dma_copy(src_address, dest_address, amount)
        }
    }

    fn buffer_clear(&mut self, dst_address: u64, amount: u64, value: u32) -> bool {
        unsafe {
            let buffer_cache = self.buffer_cache.as_mut();
            let buffer_mutex: *const _ = &buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            buffer_cache.dma_clear(dst_address, amount, value)
        }
    }

    fn image_to_buffer(
        &mut self,
        copy_info: &dma::ImageCopy,
        src: &dma::ImageOperand,
        dst: &dma::BufferOperand,
    ) -> bool {
        self.dma_buffer_image_copy(copy_info, dst, src, false)
    }

    fn buffer_to_image(
        &mut self,
        copy_info: &dma::ImageCopy,
        src: &dma::BufferOperand,
        dst: &dma::ImageOperand,
    ) -> bool {
        self.dma_buffer_image_copy(copy_info, src, dst, true)
    }
}

type GlDepthRangeIndexeddNV = unsafe extern "system" fn(u32, f64, f64);
type GlViewportSwizzleNV = unsafe extern "system" fn(u32, u32, u32, u32, u32);
type GlPolygonOffsetClamp = unsafe extern "system" fn(f32, f32, f32);
type GlAlphaFunc = unsafe extern "system" fn(u32, f32);
type GlMultiDrawArraysIndirectCount =
    unsafe extern "system" fn(u32, *const c_void, isize, i32, i32);
type GlMultiDrawElementsIndirectCount =
    unsafe extern "system" fn(u32, u32, *const c_void, isize, i32, i32);
type GlDrawTextureNV =
    unsafe extern "system" fn(u32, u32, f32, f32, f32, f32, f32, f32, f32, f32, f32);

static GL_DEPTH_RANGE_INDEXEDDNV: OnceLock<Option<GlDepthRangeIndexeddNV>> = OnceLock::new();
static GL_VIEWPORT_SWIZZLE_NV: OnceLock<Option<GlViewportSwizzleNV>> = OnceLock::new();
static GL_POLYGON_OFFSET_CLAMP: OnceLock<Option<GlPolygonOffsetClamp>> = OnceLock::new();
static GL_ALPHA_FUNC: OnceLock<Option<GlAlphaFunc>> = OnceLock::new();
static GL_MULTI_DRAW_ARRAYS_INDIRECT_COUNT: OnceLock<Option<GlMultiDrawArraysIndirectCount>> =
    OnceLock::new();
static GL_MULTI_DRAW_ELEMENTS_INDIRECT_COUNT: OnceLock<Option<GlMultiDrawElementsIndirectCount>> =
    OnceLock::new();
static GL_DRAW_TEXTURE_NV: OnceLock<Option<GlDrawTextureNV>> = OnceLock::new();

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

/// Load optional OpenGL entry points that are not emitted by the generated
/// `gl` bindings but are used by upstream `RasterizerOpenGL::SyncState`.
pub fn load_extra_functions<F>(load_fn: &mut F)
where
    F: FnMut(&'static str) -> *const c_void,
{
    let _ =
        GL_DEPTH_RANGE_INDEXEDDNV.set(load_optional_gl_function(load_fn, "glDepthRangeIndexeddNV"));
    let _ = GL_VIEWPORT_SWIZZLE_NV.set(load_optional_gl_function(load_fn, "glViewportSwizzleNV"));
    let _ = GL_POLYGON_OFFSET_CLAMP.set(load_optional_gl_function(load_fn, "glPolygonOffsetClamp"));
    let _ = GL_ALPHA_FUNC.set(load_optional_gl_function(load_fn, "glAlphaFunc"));
    let arrays_indirect_count =
        load_optional_gl_function(load_fn, "glMultiDrawArraysIndirectCount")
            .or_else(|| load_optional_gl_function(load_fn, "glMultiDrawArraysIndirectCountARB"));
    let elements_indirect_count =
        load_optional_gl_function(load_fn, "glMultiDrawElementsIndirectCount")
            .or_else(|| load_optional_gl_function(load_fn, "glMultiDrawElementsIndirectCountARB"));
    let _ = GL_MULTI_DRAW_ARRAYS_INDIRECT_COUNT.set(arrays_indirect_count);
    let _ = GL_MULTI_DRAW_ELEMENTS_INDIRECT_COUNT.set(elements_indirect_count);
    let _ = GL_DRAW_TEXTURE_NV.set(load_optional_gl_function(load_fn, "glDrawTextureNV"));
}

const GL_PARAMETER_BUFFER: u32 = 0x80EE;

fn maxwell_to_video_core_query(query_type: u32) -> Option<VideoQueryType> {
    match query_type {
        x if x == crate::query_cache::types::QueryType::PrimitivesGenerated as u32
            || x == crate::query_cache::types::QueryType::VtgPrimitivesOut as u32 =>
        {
            Some(VideoQueryType::PrimitivesGenerated)
        }
        x if x == crate::query_cache::types::QueryType::ZPassPixelCount64 as u32 => {
            Some(VideoQueryType::SamplesPassed)
        }
        x if x == crate::query_cache::types::QueryType::StreamingPrimitivesSucceeded as u32 => {
            Some(VideoQueryType::TfbPrimitivesWritten)
        }
        _ => None,
    }
}

/// Adapter that implements `GpuMemoryAccess` for the buffer cache by
/// delegating to the channel's `MemoryManager`.
struct GpuMemoryAccessAdapter {
    mm: Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>,
}

impl crate::buffer_cache::buffer_cache_base::GpuMemoryAccess for GpuMemoryAccessAdapter {
    fn gpu_to_cpu_address(&self, gpu_addr: u64) -> Option<u64> {
        self.mm.lock().gpu_to_cpu_address(gpu_addr)
    }

    fn read_u64(&self, gpu_addr: u64) -> Option<u64> {
        let mut buf = [0u8; 8];
        self.mm.lock().read_block(gpu_addr, &mut buf);
        Some(u64::from_le_bytes(buf))
    }

    fn read_u32(&self, gpu_addr: u64) -> Option<u32> {
        let mut buf = [0u8; 4];
        self.mm.lock().read_block(gpu_addr, &mut buf);
        Some(u32::from_le_bytes(buf))
    }

    fn is_within_gpu_address_range(&self, gpu_addr: u64) -> bool {
        self.mm.lock().is_within_gpu_address_range(gpu_addr)
    }

    fn max_continuous_range(&self, gpu_addr: u64, size: u64) -> u64 {
        self.mm.lock().max_continuous_range(gpu_addr, size)
    }

    fn get_memory_layout_size(&self, gpu_addr: u64) -> u64 {
        self.mm.lock().get_memory_layout_size(gpu_addr)
    }
}

/// Adapter that implements `DeviceMemoryAccess` for the buffer cache by
/// reading/writing through the CPU memory reader/writer callbacks.
struct DeviceMemoryAccessAdapter {
    device_reader: crate::renderer_base::DeviceMemoryReader,
    guest_writer: Option<crate::renderer_base::GuestMemoryWriter>,
}

impl crate::buffer_cache::buffer_cache_base::DeviceMemoryAccess for DeviceMemoryAccessAdapter {
    fn get_pointer(&self, _device_addr: u64) -> Option<*const u8> {
        None
    }

    fn read_block_unsafe(&self, device_addr: u64, dst: &mut [u8]) {
        (self.device_reader)(device_addr, dst);
    }

    fn write_block_unsafe(&self, device_addr: u64, src: &[u8]) {
        if let Some(writer) = self.guest_writer.as_ref() {
            writer(device_addr, src);
        }
    }
}

/// Emit the callback body passed to upstream `PrepareDraw` by
/// `RasterizerOpenGL::DrawIndirect`.
fn emit_indirect_draw(
    buffer_cache: &mut OpenGLBufferCache,
    draw_state: &DrawState,
    params: IndirectParams,
    primitive_mode: u32,
) {
    if params.is_byte_count {
        let tfb_object_base_addr = params.indirect_start_address.wrapping_sub(4);
        let tfb_object = buffer_cache.get_transform_feedback_object(tfb_object_base_addr);
        unsafe {
            gl::DrawTransformFeedback(primitive_mode, tfb_object);
        }
        return;
    }

    let (buffer_id, offset) = buffer_cache.get_draw_indirect_buffer();
    let handle = buffer_cache.get_buffer_gpu_handle(buffer_id);
    unsafe {
        gl::BindBuffer(gl::DRAW_INDIRECT_BUFFER, handle);
    }
    let gl_offset = offset as usize as *const c_void;
    if params.include_count {
        let (count_buffer_id, count_offset) = buffer_cache.get_draw_indirect_count();
        let count_handle = buffer_cache.get_buffer_gpu_handle(count_buffer_id);
        unsafe {
            gl::BindBuffer(GL_PARAMETER_BUFFER, count_handle);
        }
        if params.is_indexed {
            let function = GL_MULTI_DRAW_ELEMENTS_INDIRECT_COUNT
                .get()
                .and_then(|function| *function);
            let function =
                function.expect("OpenGL 4.6 context is missing glMultiDrawElementsIndirectCount");
            unsafe {
                function(
                    primitive_mode,
                    super::maxwell_to_gl::index_format(draw_state.index_buffer.format),
                    gl_offset,
                    count_offset as isize,
                    params.max_draw_counts as i32,
                    params.stride as i32,
                );
            }
        } else {
            let function = GL_MULTI_DRAW_ARRAYS_INDIRECT_COUNT
                .get()
                .and_then(|function| *function);
            let function =
                function.expect("OpenGL 4.6 context is missing glMultiDrawArraysIndirectCount");
            unsafe {
                function(
                    primitive_mode,
                    gl_offset,
                    count_offset as isize,
                    params.max_draw_counts as i32,
                    params.stride as i32,
                );
            }
        }
        return;
    }

    unsafe {
        if params.is_indexed {
            gl::MultiDrawElementsIndirect(
                primitive_mode,
                super::maxwell_to_gl::index_format(draw_state.index_buffer.format),
                gl_offset,
                params.max_draw_counts as i32,
                params.stride as i32,
            );
        } else {
            gl::MultiDrawArraysIndirect(
                primitive_mode,
                gl_offset,
                params.max_draw_counts as i32,
                params.stride as i32,
            );
        }
    }
}

#[derive(Clone, Copy)]
enum PreparedDrawCommand {
    Direct {
        is_indexed: bool,
        instance_count: u32,
    },
    Indirect(IndirectParams),
}

/// Emit the callback body passed to upstream `PrepareDraw` by
/// `RasterizerOpenGL::Draw`.
fn emit_direct_draw(
    buffer_cache: &OpenGLBufferCache,
    draw_state: &DrawState,
    is_indexed: bool,
    instance_count: u32,
    primitive_mode: u32,
) {
    let base_instance = draw_state.base_instance;
    let num_instances = instance_count as i32;
    unsafe {
        if is_indexed {
            let base_vertex = draw_state.base_index as i32;
            let num_vertices = draw_state.index_buffer.count as i32;
            let offset = buffer_cache.index_offset() as *const c_void;
            let format = super::maxwell_to_gl::index_format(draw_state.index_buffer.format);
            if num_instances == 1 && base_instance == 0 && base_vertex == 0 {
                gl::DrawElements(primitive_mode, num_vertices, format, offset);
            } else if num_instances == 1 && base_instance == 0 {
                gl::DrawElementsBaseVertex(
                    primitive_mode,
                    num_vertices,
                    format,
                    offset,
                    base_vertex,
                );
            } else if base_vertex == 0 && base_instance == 0 {
                gl::DrawElementsInstanced(
                    primitive_mode,
                    num_vertices,
                    format,
                    offset,
                    num_instances,
                );
            } else if base_vertex == 0 {
                gl::DrawElementsInstancedBaseInstance(
                    primitive_mode,
                    num_vertices,
                    format,
                    offset,
                    num_instances,
                    base_instance,
                );
            } else if base_instance == 0 {
                gl::DrawElementsInstancedBaseVertex(
                    primitive_mode,
                    num_vertices,
                    format,
                    offset,
                    num_instances,
                    base_vertex,
                );
            } else {
                gl::DrawElementsInstancedBaseVertexBaseInstance(
                    primitive_mode,
                    num_vertices,
                    format,
                    offset,
                    num_instances,
                    base_vertex,
                    base_instance,
                );
            }
        } else {
            let base_vertex = draw_state.vertex_buffer.first as i32;
            let num_vertices = draw_state.vertex_buffer.count as i32;
            if num_instances == 1 && base_instance == 0 {
                gl::DrawArrays(primitive_mode, base_vertex, num_vertices);
            } else if base_instance == 0 {
                gl::DrawArraysInstanced(primitive_mode, base_vertex, num_vertices, num_instances);
            } else {
                gl::DrawArraysInstancedBaseInstance(
                    primitive_mode,
                    base_vertex,
                    num_vertices,
                    num_instances,
                    base_instance,
                );
            }
        }
    }
}

/// Stride in bytes for one index of the given format. Used to compute the
/// IBO byte offset from `IndexBuffer::first` (which counts indices, not bytes).
fn vertex_attrib_type_raw(attrib_type: crate::engines::maxwell_3d::VertexAttribType) -> u32 {
    use crate::engines::maxwell_3d::VertexAttribType::*;
    match attrib_type {
        Invalid => 0,
        SNorm => 1,
        UNorm => 2,
        SInt => 3,
        UInt => 4,
        UScaled => 5,
        SScaled => 6,
        Float => 7,
    }
}

fn vertex_attrib_size_raw(size: crate::engines::maxwell_3d::VertexAttribSize) -> u32 {
    use crate::engines::maxwell_3d::VertexAttribSize::*;
    match size {
        Invalid => 0x00,
        R32G32B32A32 => 0x01,
        R32G32B32 => 0x02,
        R16G16B16A16 => 0x03,
        R32G32 => 0x04,
        R16G16B16 => 0x05,
        R8G8B8A8 => 0x0A,
        R16G16 => 0x0F,
        R32 => 0x12,
        R8G8B8 => 0x13,
        R8G8 => 0x18,
        R16 => 0x1B,
        R8 => 0x1D,
        A2B10G10R10 => 0x30,
        B10G11R11 => 0x31,
        G8R8 => 0x32,
        X8B8G8R8 => 0x33,
        A8 => 0x34,
    }
}

fn vertex_attrib_is_normalized(attrib_type: crate::engines::maxwell_3d::VertexAttribType) -> bool {
    matches!(
        attrib_type,
        crate::engines::maxwell_3d::VertexAttribType::SNorm
            | crate::engines::maxwell_3d::VertexAttribType::UNorm
    )
}

fn vertex_attrib_is_integer(attrib_type: crate::engines::maxwell_3d::VertexAttribType) -> bool {
    matches!(
        attrib_type,
        crate::engines::maxwell_3d::VertexAttribType::SInt
            | crate::engines::maxwell_3d::VertexAttribType::UInt
    )
}

fn comparison_op_to_gl(op: ComparisonOp) -> u32 {
    match op {
        ComparisonOp::Never => gl::NEVER,
        ComparisonOp::Less => gl::LESS,
        ComparisonOp::Equal => gl::EQUAL,
        ComparisonOp::LessEqual => gl::LEQUAL,
        ComparisonOp::Greater => gl::GREATER,
        ComparisonOp::NotEqual => gl::NOTEQUAL,
        ComparisonOp::GreaterEqual => gl::GEQUAL,
        ComparisonOp::Always => gl::ALWAYS,
    }
}

fn stencil_op_to_gl(op: StencilOp) -> u32 {
    match op {
        StencilOp::Keep => gl::KEEP,
        StencilOp::Zero => gl::ZERO,
        StencilOp::Replace => gl::REPLACE,
        StencilOp::IncrSat => gl::INCR,
        StencilOp::DecrSat => gl::DECR,
        StencilOp::Invert => gl::INVERT,
        StencilOp::Incr => gl::INCR_WRAP,
        StencilOp::Decr => gl::DECR_WRAP,
    }
}

fn blend_equation_to_gl(equation: BlendEquation) -> u32 {
    match equation {
        BlendEquation::Add => gl::FUNC_ADD,
        BlendEquation::Subtract => gl::FUNC_SUBTRACT,
        BlendEquation::ReverseSubtract => gl::FUNC_REVERSE_SUBTRACT,
        BlendEquation::Min => gl::MIN,
        BlendEquation::Max => gl::MAX,
    }
}

fn blend_factor_to_gl(factor: BlendFactor) -> u32 {
    match factor {
        BlendFactor::Zero => gl::ZERO,
        BlendFactor::One => gl::ONE,
        BlendFactor::SrcColor => gl::SRC_COLOR,
        BlendFactor::OneMinusSrcColor => gl::ONE_MINUS_SRC_COLOR,
        BlendFactor::SrcAlpha => gl::SRC_ALPHA,
        BlendFactor::OneMinusSrcAlpha => gl::ONE_MINUS_SRC_ALPHA,
        BlendFactor::DstAlpha => gl::DST_ALPHA,
        BlendFactor::OneMinusDstAlpha => gl::ONE_MINUS_DST_ALPHA,
        BlendFactor::DstColor => gl::DST_COLOR,
        BlendFactor::OneMinusDstColor => gl::ONE_MINUS_DST_COLOR,
        BlendFactor::SrcAlphaSaturate => gl::SRC_ALPHA_SATURATE,
        BlendFactor::Src1Color => gl::SRC1_COLOR,
        BlendFactor::OneMinusSrc1Color => gl::ONE_MINUS_SRC1_COLOR,
        BlendFactor::Src1Alpha => gl::SRC1_ALPHA,
        BlendFactor::OneMinusSrc1Alpha => gl::ONE_MINUS_SRC1_ALPHA,
        BlendFactor::ConstantColor => gl::CONSTANT_COLOR,
        BlendFactor::OneMinusConstantColor => gl::ONE_MINUS_CONSTANT_COLOR,
        BlendFactor::ConstantAlpha => gl::CONSTANT_ALPHA,
        BlendFactor::OneMinusConstantAlpha => gl::ONE_MINUS_CONSTANT_ALPHA,
    }
}

fn front_face_to_gl(face: FrontFace) -> u32 {
    match face {
        FrontFace::CW => gl::CW,
        FrontFace::CCW => gl::CCW,
    }
}

fn cull_face_to_gl(face: CullFace) -> u32 {
    match face {
        CullFace::Front => gl::FRONT,
        CullFace::Back => gl::BACK,
        CullFace::FrontAndBack => gl::FRONT_AND_BACK,
    }
}

fn polygon_mode_to_gl(mode: PolygonMode) -> u32 {
    match mode {
        PolygonMode::Point => gl::POINT,
        PolygonMode::Line => gl::LINE,
        PolygonMode::Fill => gl::FILL,
    }
}

impl RenderTargetDirtyFlagAccess for Maxwell3DDrawView<'_> {
    fn render_target_dirty_flag(&self, flag: u8) -> bool {
        self.dirty_flag(flag)
    }

    fn clear_render_target_dirty_flag(&mut self, flag: u8) {
        self.clear_dirty_flag(flag);
    }

    fn set_render_target_dirty_flag(&mut self, flag: u8) {
        self.set_dirty_flag(flag);
    }
}

impl RenderTargetDirtyFlagAccess for Maxwell3DClearView<'_> {
    fn render_target_dirty_flag(&self, flag: u8) -> bool {
        self.dirty_flags()[flag as usize]
    }

    fn clear_render_target_dirty_flag(&mut self, flag: u8) {
        self.clear_dirty_flag(flag);
    }

    fn set_render_target_dirty_flag(&mut self, flag: u8) {
        self.set_dirty_flag(flag);
    }
}

impl RenderTargetDirtyFlagAccess for Maxwell3DDrawTextureView<'_> {
    fn render_target_dirty_flag(&self, flag: u8) -> bool {
        self.dirty_flags()[flag as usize]
    }

    fn clear_render_target_dirty_flag(&mut self, flag: u8) {
        self.clear_dirty_flag(flag);
    }

    fn set_render_target_dirty_flag(&mut self, flag: u8) {
        self.draw_view_mut().set_dirty_flag(flag);
    }
}

fn sync_stencil_face(face: u32, info: StencilFaceInfo) {
    unsafe {
        gl::StencilFuncSeparate(
            face,
            comparison_op_to_gl(info.func),
            info.ref_value as i32,
            info.func_mask,
        );
        gl::StencilOpSeparate(
            face,
            stencil_op_to_gl(info.fail_op),
            stencil_op_to_gl(info.zfail_op),
            stencil_op_to_gl(info.zpass_op),
        );
        gl::StencilMaskSeparate(face, info.write_mask);
    }
}

fn sync_depth_test_state(
    draw_view: &mut Maxwell3DDrawView<'_>,
    state_tracker: &mut Option<&mut StateTracker>,
) {
    let flags = draw_view.dirty_flags();
    let tracker_dirty_depth_mask = exchange_tracker_dirty(state_tracker, GlDirty::DEPTH_MASK);
    unsafe {
        if flags[GlDirty::DEPTH_MASK as usize] || tracker_dirty_depth_mask {
            draw_view.clear_dirty_flag(GlDirty::DEPTH_MASK);
            gl::DepthMask(if draw_view.depth_stencil().depth_write_enable {
                gl::TRUE
            } else {
                gl::FALSE
            });
        }
    }

    let flags = draw_view.dirty_flags();
    let tracker_dirty_depth_test = exchange_tracker_dirty(state_tracker, GlDirty::DEPTH_TEST);
    if !flags[GlDirty::DEPTH_TEST as usize] && !tracker_dirty_depth_test {
        return;
    }
    draw_view.clear_dirty_flag(GlDirty::DEPTH_TEST);

    let depth_stencil = draw_view.depth_stencil();
    unsafe {
        if depth_stencil.depth_test_enable {
            gl::Enable(gl::DEPTH_TEST);
            gl::DepthFunc(comparison_op_to_gl(depth_stencil.depth_func));
        } else {
            gl::Disable(gl::DEPTH_TEST);
        }
    }
}

trait ClearSyncStateView {
    fn dirty_flags(&self) -> &[bool; 256];
    fn clear_dirty_flag(&mut self, index: u8);
    fn depth_stencil(&self) -> crate::engines::maxwell_3d::DepthStencilInfo;
    fn rasterize_enable(&self) -> bool;
    fn frag_color_clamp_any_enabled(&self) -> bool;
    fn framebuffer_srgb(&self) -> bool;
}

impl ClearSyncStateView for Maxwell3DDrawView<'_> {
    fn dirty_flags(&self) -> &[bool; 256] {
        self.dirty_flags()
    }

    fn clear_dirty_flag(&mut self, index: u8) {
        self.clear_dirty_flag(index);
    }

    fn depth_stencil(&self) -> crate::engines::maxwell_3d::DepthStencilInfo {
        self.depth_stencil()
    }

    fn rasterize_enable(&self) -> bool {
        Maxwell3DDrawView::rasterize_enable(self)
    }

    fn frag_color_clamp_any_enabled(&self) -> bool {
        Maxwell3DDrawView::frag_color_clamp_any_enabled(self)
    }

    fn framebuffer_srgb(&self) -> bool {
        Maxwell3DDrawView::framebuffer_srgb(self)
    }
}

impl ClearSyncStateView for Maxwell3DClearView<'_> {
    fn dirty_flags(&self) -> &[bool; 256] {
        self.dirty_flags()
    }

    fn clear_dirty_flag(&mut self, index: u8) {
        self.clear_dirty_flag(index);
    }

    fn depth_stencil(&self) -> crate::engines::maxwell_3d::DepthStencilInfo {
        self.depth_stencil()
    }

    fn rasterize_enable(&self) -> bool {
        Maxwell3DClearView::rasterize_enable(self)
    }

    fn frag_color_clamp_any_enabled(&self) -> bool {
        Maxwell3DClearView::frag_color_clamp_any_enabled(self)
    }

    fn framebuffer_srgb(&self) -> bool {
        Maxwell3DClearView::framebuffer_srgb(self)
    }
}

trait ViewportSyncStateView {
    fn dirty_flags(&self) -> &[bool; 256];
    fn clear_dirty_flag(&mut self, index: u8);
    fn rasterizer(&self) -> crate::engines::maxwell_3d::RasterizerInfo;
    fn window_origin_flip_y(&self) -> bool;
    fn window_origin_lower_left(&self) -> bool;
    fn viewport0_scale_y(&self) -> f32;
    fn depth_mode(&self) -> DepthMode;
    fn viewport_scale_offset_enabled(&self) -> bool;
    fn surface_clip(&self) -> crate::engines::maxwell_3d::SurfaceClipInfo;
    fn viewport_transforms(
        &self,
    ) -> [crate::engines::maxwell_3d::ViewportTransformInfo;
           crate::engines::maxwell_3d::NUM_VIEWPORTS];
}

macro_rules! impl_viewport_sync_state_view {
    ($type:ty) => {
        impl ViewportSyncStateView for $type {
            fn dirty_flags(&self) -> &[bool; 256] {
                <$type>::dirty_flags(self)
            }

            fn clear_dirty_flag(&mut self, index: u8) {
                <$type>::clear_dirty_flag(self, index);
            }

            fn rasterizer(&self) -> crate::engines::maxwell_3d::RasterizerInfo {
                <$type>::rasterizer(self)
            }

            fn window_origin_flip_y(&self) -> bool {
                <$type>::window_origin_flip_y(self)
            }

            fn window_origin_lower_left(&self) -> bool {
                <$type>::window_origin_lower_left(self)
            }

            fn viewport0_scale_y(&self) -> f32 {
                <$type>::viewport0_scale_y(self)
            }

            fn depth_mode(&self) -> DepthMode {
                <$type>::depth_mode(self)
            }

            fn viewport_scale_offset_enabled(&self) -> bool {
                <$type>::viewport_scale_offset_enabled(self)
            }

            fn surface_clip(&self) -> crate::engines::maxwell_3d::SurfaceClipInfo {
                <$type>::surface_clip(self)
            }

            fn viewport_transforms(
                &self,
            ) -> [crate::engines::maxwell_3d::ViewportTransformInfo;
                   crate::engines::maxwell_3d::NUM_VIEWPORTS] {
                <$type>::viewport_transforms(self)
            }
        }
    };
}

impl_viewport_sync_state_view!(Maxwell3DDrawView<'_>);
impl_viewport_sync_state_view!(Maxwell3DClearView<'_>);

trait ScissorSyncStateView {
    fn dirty_flags(&self) -> &[bool; 256];
    fn clear_dirty_flag(&mut self, index: u8);
    fn scissors(
        &self,
    ) -> [crate::engines::maxwell_3d::ScissorInfo; crate::engines::maxwell_3d::NUM_VIEWPORTS];
}

impl ScissorSyncStateView for Maxwell3DDrawView<'_> {
    fn dirty_flags(&self) -> &[bool; 256] {
        Maxwell3DDrawView::dirty_flags(self)
    }

    fn clear_dirty_flag(&mut self, index: u8) {
        Maxwell3DDrawView::clear_dirty_flag(self, index);
    }

    fn scissors(
        &self,
    ) -> [crate::engines::maxwell_3d::ScissorInfo; crate::engines::maxwell_3d::NUM_VIEWPORTS] {
        Maxwell3DDrawView::scissors(self)
    }
}

impl ScissorSyncStateView for Maxwell3DClearView<'_> {
    fn dirty_flags(&self) -> &[bool; 256] {
        Maxwell3DClearView::dirty_flags(self)
    }

    fn clear_dirty_flag(&mut self, index: u8) {
        Maxwell3DClearView::clear_dirty_flag(self, index);
    }

    fn scissors(
        &self,
    ) -> [crate::engines::maxwell_3d::ScissorInfo; crate::engines::maxwell_3d::NUM_VIEWPORTS] {
        Maxwell3DClearView::scissors(self)
    }
}

fn sync_stencil_test_state<V: ClearSyncStateView>(
    draw_view: &mut V,
    state_tracker: &mut Option<&mut StateTracker>,
) {
    let flags = draw_view.dirty_flags();
    let tracker_dirty = exchange_tracker_dirty(state_tracker, GlDirty::STENCIL_TEST);
    if !flags[GlDirty::STENCIL_TEST as usize] && !tracker_dirty {
        return;
    }
    draw_view.clear_dirty_flag(GlDirty::STENCIL_TEST);

    let depth_stencil = draw_view.depth_stencil();
    unsafe {
        if depth_stencil.stencil_enable {
            gl::Enable(gl::STENCIL_TEST);
        } else {
            gl::Disable(gl::STENCIL_TEST);
        }
        sync_stencil_face(gl::FRONT, depth_stencil.front);
        if depth_stencil.stencil_two_side {
            sync_stencil_face(gl::BACK, depth_stencil.back);
        } else {
            gl::StencilFuncSeparate(gl::BACK, gl::ALWAYS, 0, 0xFFFF_FFFF);
            gl::StencilOpSeparate(gl::BACK, gl::KEEP, gl::KEEP, gl::KEEP);
            gl::StencilMaskSeparate(gl::BACK, 0xFFFF_FFFF);
        }
    }
}

fn sync_depth_clamp(
    draw_view: &mut Maxwell3DDrawView<'_>,
    state_tracker: &mut Option<&mut StateTracker>,
) {
    let flags = draw_view.dirty_flags();
    let tracker_dirty = exchange_tracker_dirty(state_tracker, GlDirty::DEPTH_CLAMP_ENABLED);
    if !flags[GlDirty::DEPTH_CLAMP_ENABLED as usize] && !tracker_dirty {
        return;
    }
    draw_view.clear_dirty_flag(GlDirty::DEPTH_CLAMP_ENABLED);

    unsafe {
        if draw_view.depth_clamp_enabled() {
            gl::Enable(gl::DEPTH_CLAMP);
        } else {
            gl::Disable(gl::DEPTH_CLAMP);
        }
    }
}

fn sync_framebuffer_srgb<V: ClearSyncStateView>(
    draw_view: &mut V,
    state_tracker: &mut Option<&mut StateTracker>,
) {
    let flags = draw_view.dirty_flags();
    let tracker_dirty = exchange_tracker_dirty(state_tracker, GlDirty::FRAMEBUFFER_SRGB);
    if !flags[GlDirty::FRAMEBUFFER_SRGB as usize] && !tracker_dirty {
        return;
    }
    draw_view.clear_dirty_flag(GlDirty::FRAMEBUFFER_SRGB);
    unsafe {
        if draw_view.framebuffer_srgb() {
            gl::Enable(gl::FRAMEBUFFER_SRGB);
        } else {
            gl::Disable(gl::FRAMEBUFFER_SRGB);
        }
    }
}

fn viewport_front_face_to_gl(
    front_face: FrontFace,
    window_origin_flip_y: bool,
    viewport0_scale_y: f32,
) -> u32 {
    let mode = front_face_to_gl(front_face);
    let mut flip_faces = true;
    if window_origin_flip_y {
        flip_faces = !flip_faces;
    }
    if viewport0_scale_y < 0.0 {
        flip_faces = !flip_faces;
    }
    if !flip_faces {
        return mode;
    }
    match mode {
        gl::CW => gl::CCW,
        gl::CCW => gl::CW,
        _ => mode,
    }
}

fn clip_control_depth(depth_mode: DepthMode) -> u32 {
    match depth_mode {
        DepthMode::ZeroToOne => gl::ZERO_TO_ONE,
        DepthMode::MinusOneToOne => gl::NEGATIVE_ONE_TO_ONE,
    }
}

fn clip_control_origin(window_origin_lower_left: bool, viewport0_scale_y: f32) -> u32 {
    let mut flip_y = false;
    if viewport0_scale_y < 0.0 {
        flip_y = !flip_y;
    }
    if window_origin_lower_left {
        flip_y = !flip_y;
    }
    if flip_y {
        gl::UPPER_LEFT
    } else {
        gl::LOWER_LEFT
    }
}

fn viewport_swizzle_components(swizzle: u32) -> [u32; 4] {
    [
        crate::renderer_opengl::maxwell_to_gl::viewport_swizzle(swizzle & 0x7),
        crate::renderer_opengl::maxwell_to_gl::viewport_swizzle((swizzle >> 4) & 0x7),
        crate::renderer_opengl::maxwell_to_gl::viewport_swizzle((swizzle >> 8) & 0x7),
        crate::renderer_opengl::maxwell_to_gl::viewport_swizzle((swizzle >> 12) & 0x7),
    ]
}

fn scale_viewport_value(value: f32, scale: f32) -> f32 {
    let mut new_value = value * scale;
    if scale < 1.0 {
        new_value = new_value.abs().round().copysign(value);
    }
    new_value
}

fn nonzero_viewport_extent(value: f32) -> f32 {
    if value != 0.0 {
        value
    } else {
        1.0
    }
}

fn scale_scissor_value(value: u32, up_scale: u32, down_shift: u32) -> u32 {
    if value == 0 {
        return 0;
    }
    let upset = value.wrapping_mul(up_scale);
    let accumulator = if (up_scale >> down_shift) == 0 {
        upset % 2
    } else {
        0
    };
    ((upset >> down_shift).wrapping_add(accumulator)).max(1)
}

fn exchange_tracker_dirty(state_tracker: &mut Option<&mut StateTracker>, flag: u8) -> bool {
    state_tracker
        .as_deref_mut()
        .is_some_and(|tracker| tracker.exchange(flag))
}

fn sync_rasterize_enable<V: ClearSyncStateView>(
    draw_view: &mut V,
    state_tracker: &mut Option<&mut StateTracker>,
) {
    let flags = draw_view.dirty_flags();
    let tracker_dirty = exchange_tracker_dirty(state_tracker, GlDirty::RASTERIZE_ENABLE);
    if !flags[GlDirty::RASTERIZE_ENABLE as usize] && !tracker_dirty {
        return;
    }
    draw_view.clear_dirty_flag(GlDirty::RASTERIZE_ENABLE);
    unsafe {
        if draw_view.rasterize_enable() {
            gl::Disable(gl::RASTERIZER_DISCARD);
        } else {
            gl::Enable(gl::RASTERIZER_DISCARD);
        }
    }
}

fn sync_scissor_test<V: ScissorSyncStateView>(
    draw_view: &mut V,
    state_tracker: &mut Option<&mut StateTracker>,
    is_rescaling: bool,
) {
    let flags = *draw_view.dirty_flags();
    let tracker_dirty = exchange_tracker_dirty(state_tracker, GlDirty::SCISSORS);
    let dirty_scissors = flags[GlDirty::SCISSORS as usize]
        || flags[GlDirty::RESCALE_SCISSORS as usize]
        || tracker_dirty;
    if !dirty_scissors {
        return;
    }

    let force = flags[GlDirty::RESCALE_SCISSORS as usize] || tracker_dirty;
    draw_view.clear_dirty_flag(GlDirty::SCISSORS);
    draw_view.clear_dirty_flag(GlDirty::RESCALE_SCISSORS);
    let resolution = common::settings::values().resolution_info.clone();
    let (up_scale, down_shift) = if is_rescaling {
        (resolution.up_scale, resolution.down_shift)
    } else {
        (1, 0)
    };
    unsafe {
        for (index, scissor) in draw_view.scissors().iter().enumerate() {
            if !force && !flags[(GlDirty::SCISSOR_0 as usize) + index] {
                continue;
            }
            draw_view.clear_dirty_flag(GlDirty::SCISSOR_0 + index as u8);
            if scissor.enabled {
                gl::Enablei(gl::SCISSOR_TEST, index as u32);
                gl::ScissorIndexed(
                    index as u32,
                    scale_scissor_value(scissor.min_x, up_scale, down_shift) as i32,
                    scale_scissor_value(scissor.min_y, up_scale, down_shift) as i32,
                    scale_scissor_value(
                        scissor.max_x.wrapping_sub(scissor.min_x),
                        up_scale,
                        down_shift,
                    ) as i32,
                    scale_scissor_value(
                        scissor.max_y.wrapping_sub(scissor.min_y),
                        up_scale,
                        down_shift,
                    ) as i32,
                );
            } else {
                gl::Disablei(gl::SCISSOR_TEST, index as u32);
            }
        }
    }
}

fn sync_color_mask(
    draw_view: &mut Maxwell3DDrawView<'_>,
    state_tracker: &mut Option<&mut StateTracker>,
) {
    let flags = *draw_view.dirty_flags();
    let tracker_dirty = exchange_tracker_dirty(state_tracker, GlDirty::COLOR_MASKS);
    if !flags[GlDirty::COLOR_MASKS as usize] && !tracker_dirty {
        return;
    }

    draw_view.clear_dirty_flag(GlDirty::COLOR_MASKS);
    let force = flags[GlDirty::COLOR_MASK_COMMON as usize] || tracker_dirty;
    draw_view.clear_dirty_flag(GlDirty::COLOR_MASK_COMMON);
    let color_masks = draw_view.color_masks();
    unsafe {
        if draw_view.color_mask_common() {
            if force || flags[GlDirty::COLOR_MASK_0 as usize] {
                draw_view.clear_dirty_flag(GlDirty::COLOR_MASK_0);
                let mask = color_masks[0];
                gl::ColorMask(
                    if mask.r { gl::TRUE } else { gl::FALSE },
                    if mask.g { gl::TRUE } else { gl::FALSE },
                    if mask.b { gl::TRUE } else { gl::FALSE },
                    if mask.a { gl::TRUE } else { gl::FALSE },
                );
            }
        } else {
            for (rt, mask) in color_masks.iter().enumerate() {
                if !force && !flags[(GlDirty::COLOR_MASK_0 as usize) + rt] {
                    continue;
                }
                draw_view.clear_dirty_flag(GlDirty::COLOR_MASK_0 + rt as u8);
                gl::ColorMaski(
                    rt as u32,
                    if mask.r { gl::TRUE } else { gl::FALSE },
                    if mask.g { gl::TRUE } else { gl::FALSE },
                    if mask.b { gl::TRUE } else { gl::FALSE },
                    if mask.a { gl::TRUE } else { gl::FALSE },
                );
            }
        }
    }
}

fn sync_blend_state(
    draw_view: &mut Maxwell3DDrawView<'_>,
    state_tracker: &mut Option<&mut StateTracker>,
) {
    let flags = draw_view.dirty_flags();
    let tracker_dirty_blend_color = exchange_tracker_dirty(state_tracker, GlDirty::BLEND_COLOR);
    unsafe {
        if flags[GlDirty::BLEND_COLOR as usize] || tracker_dirty_blend_color {
            draw_view.clear_dirty_flag(GlDirty::BLEND_COLOR);
            gl::BlendColor(
                draw_view.blend_color().r,
                draw_view.blend_color().g,
                draw_view.blend_color().b,
                draw_view.blend_color().a,
            );
        }
    }

    let flags = *draw_view.dirty_flags();
    let tracker_dirty_blend_states = exchange_tracker_dirty(state_tracker, GlDirty::BLEND_STATES);
    if !flags[GlDirty::BLEND_STATES as usize] && !tracker_dirty_blend_states {
        return;
    }

    draw_view.clear_dirty_flag(GlDirty::BLEND_STATES);
    unsafe {
        if !draw_view.blend_per_target_enabled() {
            let blend = draw_view.global_blend();
            if !blend.enabled {
                gl::Disable(gl::BLEND);
            } else {
                gl::Enable(gl::BLEND);
                if draw_view.iterated_blend_enabled()
                    && common::settings::values().use_squashed_iterated_blend
                {
                    gl::BlendFuncSeparate(gl::ONE, gl::ONE, gl::ONE_MINUS_SRC_COLOR, gl::ZERO);
                    gl::BlendEquationSeparate(gl::FUNC_ADD, gl::FUNC_ADD);
                    return;
                }
                gl::BlendFuncSeparate(
                    blend_factor_to_gl(blend.color_src),
                    blend_factor_to_gl(blend.color_dst),
                    blend_factor_to_gl(blend.alpha_src),
                    blend_factor_to_gl(blend.alpha_dst),
                );
                gl::BlendEquationSeparate(
                    blend_equation_to_gl(blend.color_op),
                    blend_equation_to_gl(blend.alpha_op),
                );
            }
        } else {
            let force =
                flags[GlDirty::BLEND_INDEPENDENT_ENABLED as usize] || tracker_dirty_blend_states;
            draw_view.clear_dirty_flag(GlDirty::BLEND_INDEPENDENT_ENABLED);
            for (rt, blend) in draw_view.blend().iter().enumerate() {
                if !force && !flags[(GlDirty::BLEND_STATE_0 as usize) + rt] {
                    continue;
                }
                draw_view.clear_dirty_flag(GlDirty::BLEND_STATE_0 + rt as u8);
                if blend.enabled {
                    gl::Enablei(gl::BLEND, rt as u32);
                    gl::BlendFuncSeparatei(
                        rt as u32,
                        blend_factor_to_gl(blend.color_src),
                        blend_factor_to_gl(blend.color_dst),
                        blend_factor_to_gl(blend.alpha_src),
                        blend_factor_to_gl(blend.alpha_dst),
                    );
                    gl::BlendEquationSeparatei(
                        rt as u32,
                        blend_equation_to_gl(blend.color_op),
                        blend_equation_to_gl(blend.alpha_op),
                    );
                } else {
                    gl::Disablei(gl::BLEND, rt as u32);
                }
            }
        }
    }
}

fn sync_logic_op_state(
    draw_view: &mut Maxwell3DDrawView<'_>,
    state_tracker: &mut Option<&mut StateTracker>,
    is_amd: bool,
) {
    let flags = draw_view.dirty_flags();
    let tracker_dirty = exchange_tracker_dirty(state_tracker, GlDirty::LOGIC_OP);
    if !flags[GlDirty::LOGIC_OP as usize] && !tracker_dirty {
        return;
    }
    draw_view.clear_dirty_flag(GlDirty::LOGIC_OP);

    let logic_op = apply_amd_logic_op_workaround(draw_view, is_amd);
    unsafe {
        if logic_op.enabled {
            gl::Enable(gl::COLOR_LOGIC_OP);
            gl::LogicOp(crate::renderer_opengl::maxwell_to_gl::logic_op(logic_op.op));
        } else {
            gl::Disable(gl::COLOR_LOGIC_OP);
        }
    }
}

fn apply_amd_logic_op_workaround(
    draw_view: &mut Maxwell3DDrawView<'_>,
    is_amd: bool,
) -> crate::engines::maxwell_3d::LogicOpInfo {
    let logic_op = draw_view.logic_op();
    if !is_amd {
        return logic_op;
    }
    let enabled = effective_logic_op_enabled(true, true, &draw_view.vertex_attribs());
    // Upstream mutates `regs.logic_op.enable`, so the adjusted value persists
    // into subsequent fixed-pipeline keys as well as the current GL call.
    draw_view.set_logic_op_enabled(enabled);
    draw_view.logic_op()
}

fn effective_logic_op_enabled(
    guest_enabled: bool,
    is_amd: bool,
    vertex_attribs: &[crate::engines::maxwell_3d::VertexAttribInfo],
) -> bool {
    if is_amd {
        !vertex_attribs
            .iter()
            .any(|attrib| attrib.attrib_type == VertexAttribType::Float)
    } else {
        guest_enabled
    }
}

fn sync_cull_mode(
    draw_view: &mut Maxwell3DDrawView<'_>,
    state_tracker: &mut Option<&mut StateTracker>,
) {
    let flags = draw_view.dirty_flags();
    let tracker_dirty = exchange_tracker_dirty(state_tracker, GlDirty::CULL_TEST);
    if !flags[GlDirty::CULL_TEST as usize] && !tracker_dirty {
        return;
    }
    draw_view.clear_dirty_flag(GlDirty::CULL_TEST);

    let rasterizer = draw_view.rasterizer();
    unsafe {
        if rasterizer.cull_enable {
            gl::Enable(gl::CULL_FACE);
            gl::CullFace(cull_face_to_gl(rasterizer.cull_face));
        } else {
            gl::Disable(gl::CULL_FACE);
        }
    }
}

fn sync_polygon_modes(
    draw_view: &mut Maxwell3DDrawView<'_>,
    state_tracker: &mut Option<&mut StateTracker>,
    has_fill_rectangle: bool,
) {
    let flags = *draw_view.dirty_flags();
    let tracker_dirty = exchange_tracker_dirty(state_tracker, GlDirty::POLYGON_MODES);
    if !flags[GlDirty::POLYGON_MODES as usize] && !tracker_dirty {
        return;
    }
    draw_view.clear_dirty_flag(GlDirty::POLYGON_MODES);

    let rasterizer = draw_view.rasterizer();
    unsafe {
        if rasterizer.fill_via_triangle_mode != FillViaTriangleMode::Disabled {
            if !has_fill_rectangle {
                error!("GL_NV_fill_rectangle used and not supported");
                gl::PolygonMode(gl::FRONT_AND_BACK, gl::FILL);
                return;
            }

            const GL_FILL_RECTANGLE_NV: u32 = 0x933C;
            draw_view.set_dirty_flag(GlDirty::POLYGON_MODE_FRONT);
            draw_view.set_dirty_flag(GlDirty::POLYGON_MODE_BACK);
            gl::PolygonMode(gl::FRONT_AND_BACK, GL_FILL_RECTANGLE_NV);
            return;
        }

        if rasterizer.polygon_mode_front == rasterizer.polygon_mode_back {
            draw_view.clear_dirty_flag(GlDirty::POLYGON_MODE_FRONT);
            draw_view.clear_dirty_flag(GlDirty::POLYGON_MODE_BACK);
            gl::PolygonMode(
                gl::FRONT_AND_BACK,
                polygon_mode_to_gl(rasterizer.polygon_mode_front),
            );
            return;
        }

        if flags[GlDirty::POLYGON_MODE_FRONT as usize] || tracker_dirty {
            draw_view.clear_dirty_flag(GlDirty::POLYGON_MODE_FRONT);
            gl::PolygonMode(gl::FRONT, polygon_mode_to_gl(rasterizer.polygon_mode_front));
        }

        if flags[GlDirty::POLYGON_MODE_BACK as usize] || tracker_dirty {
            draw_view.clear_dirty_flag(GlDirty::POLYGON_MODE_BACK);
            gl::PolygonMode(gl::BACK, polygon_mode_to_gl(rasterizer.polygon_mode_back));
        }
    }
}

fn sync_fragment_color_clamp_state<V: ClearSyncStateView>(
    draw_view: &mut V,
    state_tracker: &mut Option<&mut StateTracker>,
) {
    let flags = draw_view.dirty_flags();
    let tracker_dirty = exchange_tracker_dirty(state_tracker, GlDirty::FRAGMENT_CLAMP_COLOR);
    if !flags[GlDirty::FRAGMENT_CLAMP_COLOR as usize] && !tracker_dirty {
        return;
    }
    draw_view.clear_dirty_flag(GlDirty::FRAGMENT_CLAMP_COLOR);

    const GL_CLAMP_FRAGMENT_COLOR: u32 = 0x891B;
    unsafe {
        gl::ClampColor(
            GL_CLAMP_FRAGMENT_COLOR,
            if draw_view.frag_color_clamp_any_enabled() {
                gl::TRUE as u32
            } else {
                gl::FALSE as u32
            },
        );
    }
}

fn sync_multi_sample_state(
    draw_view: &mut Maxwell3DDrawView<'_>,
    state_tracker: &mut Option<&mut StateTracker>,
) {
    let flags = draw_view.dirty_flags();
    let tracker_dirty = exchange_tracker_dirty(state_tracker, GlDirty::MULTISAMPLE_CONTROL);
    if !flags[GlDirty::MULTISAMPLE_CONTROL as usize] && !tracker_dirty {
        return;
    }
    draw_view.clear_dirty_flag(GlDirty::MULTISAMPLE_CONTROL);

    let control = draw_view.anti_alias_alpha_control();
    unsafe {
        if control.alpha_to_coverage {
            gl::Enable(gl::SAMPLE_ALPHA_TO_COVERAGE);
        } else {
            gl::Disable(gl::SAMPLE_ALPHA_TO_COVERAGE);
        }
        if control.alpha_to_one {
            gl::Enable(gl::SAMPLE_ALPHA_TO_ONE);
        } else {
            gl::Disable(gl::SAMPLE_ALPHA_TO_ONE);
        }
    }
}

fn sync_point_state(
    draw_view: &mut Maxwell3DDrawView<'_>,
    state_tracker: &mut Option<&mut StateTracker>,
    viewport_scale: f32,
) {
    let flags = draw_view.dirty_flags();
    let tracker_dirty = exchange_tracker_dirty(state_tracker, GlDirty::POINT_SIZE);
    if !flags[GlDirty::POINT_SIZE as usize] && !tracker_dirty {
        return;
    }
    draw_view.clear_dirty_flag(GlDirty::POINT_SIZE);

    const GL_POINT_SPRITE: u32 = 0x8861;
    let point = draw_view.point_state();
    unsafe {
        if point.point_sprite_enable {
            gl::Enable(GL_POINT_SPRITE);
        } else {
            gl::Disable(GL_POINT_SPRITE);
        }
        if point.point_size_attribute_enabled {
            gl::Enable(gl::PROGRAM_POINT_SIZE);
        } else {
            gl::Disable(gl::PROGRAM_POINT_SIZE);
        }
        gl::PointSize((point.point_size * viewport_scale).max(1.0));
    }
}

fn sync_line_state(
    draw_view: &mut Maxwell3DDrawView<'_>,
    state_tracker: &mut Option<&mut StateTracker>,
) {
    let flags = draw_view.dirty_flags();
    let tracker_dirty = exchange_tracker_dirty(state_tracker, GlDirty::LINE_WIDTH);
    if !flags[GlDirty::LINE_WIDTH as usize] && !tracker_dirty {
        return;
    }
    draw_view.clear_dirty_flag(GlDirty::LINE_WIDTH);

    let line = draw_view.line_state();
    unsafe {
        if line.line_anti_alias_enable {
            gl::Enable(gl::LINE_SMOOTH);
        } else {
            gl::Disable(gl::LINE_SMOOTH);
        }
        gl::LineWidth(if line.line_anti_alias_enable {
            line.line_width_smooth
        } else {
            line.line_width_aliased
        });
    }
}

fn sync_polygon_offset(
    draw_view: &mut Maxwell3DDrawView<'_>,
    state_tracker: &mut Option<&mut StateTracker>,
) {
    let flags = draw_view.dirty_flags();
    let tracker_dirty = exchange_tracker_dirty(state_tracker, GlDirty::POLYGON_OFFSET);
    if !flags[GlDirty::POLYGON_OFFSET as usize] && !tracker_dirty {
        return;
    }
    draw_view.clear_dirty_flag(GlDirty::POLYGON_OFFSET);

    let rasterizer = draw_view.rasterizer();
    unsafe {
        if rasterizer.polygon_offset_fill_enable {
            gl::Enable(gl::POLYGON_OFFSET_FILL);
        } else {
            gl::Disable(gl::POLYGON_OFFSET_FILL);
        }
        if rasterizer.polygon_offset_line_enable {
            gl::Enable(gl::POLYGON_OFFSET_LINE);
        } else {
            gl::Disable(gl::POLYGON_OFFSET_LINE);
        }
        if rasterizer.polygon_offset_point_enable {
            gl::Enable(gl::POLYGON_OFFSET_POINT);
        } else {
            gl::Disable(gl::POLYGON_OFFSET_POINT);
        }

        if rasterizer.polygon_offset_fill_enable
            || rasterizer.polygon_offset_line_enable
            || rasterizer.polygon_offset_point_enable
        {
            let units = rasterizer.depth_bias / 2.0;
            GL_POLYGON_OFFSET_CLAMP
                .get()
                .and_then(|function| *function)
                .expect("OpenGL 4.6 context is missing glPolygonOffsetClamp")(
                rasterizer.slope_scale_depth_bias,
                units,
                rasterizer.depth_bias_clamp,
            );
        }
    }
}

fn sync_alpha_test(
    draw_view: &mut Maxwell3DDrawView<'_>,
    state_tracker: &mut Option<&mut StateTracker>,
) {
    let flags = draw_view.dirty_flags();
    let tracker_dirty = exchange_tracker_dirty(state_tracker, GlDirty::ALPHA_TEST);
    if !flags[GlDirty::ALPHA_TEST as usize] && !tracker_dirty {
        return;
    }
    draw_view.clear_dirty_flag(GlDirty::ALPHA_TEST);

    const GL_ALPHA_TEST_COMPAT: u32 = 0x0BC0;
    unsafe {
        if draw_view.alpha_test_enabled() {
            gl::Enable(GL_ALPHA_TEST_COMPAT);
            GL_ALPHA_FUNC
                .get()
                .and_then(|function| *function)
                .expect("OpenGL compatibility context is missing glAlphaFunc")(
                comparison_op_to_gl(draw_view.alpha_test_func()),
                draw_view.alpha_test_ref(),
            );
        } else {
            gl::Disable(GL_ALPHA_TEST_COMPAT);
        }
    }
}

fn sync_primitive_restart(
    draw_view: &mut Maxwell3DDrawView<'_>,
    state_tracker: &mut Option<&mut StateTracker>,
) {
    let flags = draw_view.dirty_flags();
    let tracker_dirty = exchange_tracker_dirty(state_tracker, GlDirty::PRIMITIVE_RESTART);
    if !flags[GlDirty::PRIMITIVE_RESTART as usize] && !tracker_dirty {
        return;
    }
    draw_view.clear_dirty_flag(GlDirty::PRIMITIVE_RESTART);

    let primitive_restart = draw_view.primitive_restart();
    unsafe {
        if primitive_restart.enabled {
            gl::Enable(gl::PRIMITIVE_RESTART);
            gl::PrimitiveRestartIndex(primitive_restart.index);
        } else {
            gl::Disable(gl::PRIMITIVE_RESTART);
        }
    }
}

fn sync_viewport<V: ViewportSyncStateView>(
    draw_view: &mut V,
    state_tracker: &mut Option<&mut StateTracker>,
    has_depth_buffer_float: bool,
    has_viewport_swizzle: bool,
    viewport_scale: f32,
) {
    let flags = *draw_view.dirty_flags();
    let rescale_viewports = flags[crate::dirty_flags::flags::RESCALE_VIEWPORTS as usize];
    let mut tracker_dirty_viewport = false;
    let mut tracker_dirty_clip_control = false;
    let mut tracker_dirty_front_face = false;
    let mut tracker_dirty_viewport_transform = false;
    if let Some(tracker) = state_tracker.as_deref_mut() {
        tracker_dirty_viewport =
            tracker.exchange(GlDirty::VIEWPORTS) || tracker.exchange(GlDirty::RESCALE_VIEWPORTS);
        tracker_dirty_clip_control = tracker.exchange(GlDirty::CLIP_CONTROL);
        tracker_dirty_front_face = tracker.exchange(GlDirty::FRONT_FACE);
        tracker_dirty_viewport_transform = tracker.exchange(GlDirty::VIEWPORT_TRANSFORM);
    }
    let dirty_viewport =
        flags[GlDirty::VIEWPORTS as usize] || rescale_viewports || tracker_dirty_viewport;
    let dirty_clip_control = flags[GlDirty::CLIP_CONTROL as usize] || tracker_dirty_clip_control;

    unsafe {
        if dirty_viewport
            || dirty_clip_control
            || flags[GlDirty::FRONT_FACE as usize]
            || tracker_dirty_front_face
        {
            draw_view.clear_dirty_flag(GlDirty::FRONT_FACE);
            gl::FrontFace(viewport_front_face_to_gl(
                draw_view.rasterizer().front_face,
                draw_view.window_origin_flip_y(),
                draw_view.viewport0_scale_y(),
            ));
        }

        if dirty_viewport || dirty_clip_control {
            draw_view.clear_dirty_flag(GlDirty::CLIP_CONTROL);
            let clip_origin = clip_control_origin(
                draw_view.window_origin_lower_left(),
                draw_view.viewport0_scale_y(),
            );
            let clip_depth = clip_control_depth(draw_view.depth_mode());
            if let Some(tracker) = state_tracker.as_deref_mut() {
                tracker.clip_control(clip_origin, clip_depth);
                let y_negate = draw_view.window_origin_lower_left();
                tracker.set_y_negate(y_negate);
            } else {
                gl::ClipControl(clip_origin, clip_depth);
            }
        }

        if dirty_viewport {
            draw_view.clear_dirty_flag(GlDirty::VIEWPORTS);
            draw_view.clear_dirty_flag(GlDirty::VIEWPORT_TRANSFORM);
            draw_view.clear_dirty_flag(GlDirty::RESCALE_VIEWPORTS);
            let force = flags[GlDirty::VIEWPORT_TRANSFORM as usize]
                || rescale_viewports
                || tracker_dirty_viewport_transform
                || tracker_dirty_viewport;
            if !draw_view.viewport_scale_offset_enabled() {
                let surface_clip = draw_view.surface_clip();
                for index in 0..draw_view.viewport_transforms().len() {
                    if !force && !flags[(GlDirty::VIEWPORT_0 as usize) + index] {
                        continue;
                    }
                    draw_view.clear_dirty_flag(GlDirty::VIEWPORT_0 + index as u8);
                    gl::ViewportIndexedf(
                        index as u32,
                        surface_clip.x as f32,
                        surface_clip.y as f32,
                        nonzero_viewport_extent(surface_clip.width as f32),
                        nonzero_viewport_extent(surface_clip.height as f32),
                    );
                }
            } else {
                let reduce_z = if draw_view.depth_mode() == DepthMode::MinusOneToOne {
                    1.0
                } else {
                    0.0
                };
                for (index, viewport) in draw_view.viewport_transforms().iter().enumerate() {
                    if !force && !flags[(GlDirty::VIEWPORT_0 as usize) + index] {
                        continue;
                    }
                    draw_view.clear_dirty_flag(GlDirty::VIEWPORT_0 + index as u8);
                    let x = scale_viewport_value(
                        viewport.translate_x - viewport.scale_x,
                        viewport_scale,
                    );
                    let mut y = scale_viewport_value(
                        viewport.translate_y - viewport.scale_y,
                        viewport_scale,
                    );
                    let width = scale_viewport_value(viewport.scale_x * 2.0, viewport_scale);
                    let mut height = scale_viewport_value(viewport.scale_y * 2.0, viewport_scale);
                    if height < 0.0 {
                        y += height;
                        height = -height;
                    }
                    gl::ViewportIndexedf(
                        index as u32,
                        x,
                        y,
                        nonzero_viewport_extent(width),
                        nonzero_viewport_extent(height),
                    );
                    let near_depth =
                        viewport.translate_z as f64 - viewport.scale_z as f64 * reduce_z;
                    let far_depth = viewport.translate_z as f64 + viewport.scale_z as f64;
                    if has_depth_buffer_float {
                        GL_DEPTH_RANGE_INDEXEDDNV
                            .get()
                            .and_then(|function| *function)
                            .expect(
                                "GL_NV_depth_buffer_float advertised without \
                                 glDepthRangeIndexeddNV",
                            )(index as u32, near_depth, far_depth);
                    } else {
                        gl::DepthRangeIndexed(index as u32, near_depth, far_depth);
                    }
                    if has_viewport_swizzle {
                        let viewport_swizzle = GL_VIEWPORT_SWIZZLE_NV
                            .get()
                            .and_then(|function| *function)
                            .expect(
                                "GL_NV_viewport_swizzle advertised without glViewportSwizzleNV",
                            );
                        let swizzle = viewport_swizzle_components(viewport.swizzle);
                        viewport_swizzle(
                            index as u32,
                            swizzle[0],
                            swizzle[1],
                            swizzle[2],
                            swizzle[3],
                        );
                    }
                    let _ = viewport.snap_grid_precision;
                }
            }
        }
    }
}

fn sync_vertex_formats(
    draw_view: &mut Maxwell3DDrawView<'_>,
    state_tracker: &mut Option<&mut StateTracker>,
) {
    let flags = *draw_view.dirty_flags();
    let tracker_dirty_group = exchange_tracker_dirty(state_tracker, GlDirty::VERTEX_FORMATS);
    let mut tracker_dirty_formats = [false; 16];
    for (index, dirty) in tracker_dirty_formats.iter_mut().enumerate() {
        *dirty = exchange_tracker_dirty(state_tracker, GlDirty::VERTEX_FORMAT_0 + index as u8);
    }
    if !flags[GlDirty::VERTEX_FORMATS as usize] && !tracker_dirty_group {
        if !tracker_dirty_formats.iter().any(|dirty| *dirty) {
            return;
        }
    }
    draw_view.clear_dirty_flag(GlDirty::VERTEX_FORMATS);

    // Upstream caps this at 16 to avoid OpenGL errors even though Maxwell
    // exposes 32 vertex attributes.
    for (index, attrib) in draw_view.vertex_attribs().iter().take(16).enumerate() {
        if !flags[(GlDirty::VERTEX_FORMAT_0 as usize) + index] && !tracker_dirty_formats[index] {
            continue;
        }
        draw_view.clear_dirty_flag(GlDirty::VERTEX_FORMAT_0 + index as u8);

        let gl_index = index as u32;
        unsafe {
            // Upstream disables only constant attributes. An attribute whose
            // size decodes to `Invalid` is still enabled and formatted there —
            // `ComponentCount` returns 1 and `VertexFormat` returns `GL_NONE`.
            if attrib.constant {
                gl::DisableVertexAttribArray(gl_index);
                continue;
            }

            gl::EnableVertexAttribArray(gl_index);
            let component_count = attrib.size.component_count() as i32;
            let gl_format = crate::renderer_opengl::maxwell_to_gl::vertex_format(
                vertex_attrib_type_raw(attrib.attrib_type),
                vertex_attrib_size_raw(attrib.size),
            );
            if vertex_attrib_is_integer(attrib.attrib_type) {
                gl::VertexAttribIFormat(gl_index, component_count, gl_format, attrib.offset);
            } else {
                gl::VertexAttribFormat(
                    gl_index,
                    component_count,
                    gl_format,
                    if vertex_attrib_is_normalized(attrib.attrib_type) {
                        gl::TRUE
                    } else {
                        gl::FALSE
                    },
                    attrib.offset,
                );
            }
            gl::VertexAttribBinding(gl_index, attrib.buffer_index);
        }
    }
}

fn sync_vertex_instances(
    draw_view: &mut Maxwell3DDrawView<'_>,
    state_tracker: &mut Option<&mut StateTracker>,
) {
    let flags = *draw_view.dirty_flags();
    let tracker_dirty_group = exchange_tracker_dirty(state_tracker, GlDirty::VERTEX_INSTANCES);
    let mut tracker_dirty_instances = [false; 16];
    for (index, dirty) in tracker_dirty_instances.iter_mut().enumerate() {
        *dirty = exchange_tracker_dirty(state_tracker, GlDirty::VERTEX_INSTANCE_0 + index as u8);
    }
    if !flags[GlDirty::VERTEX_INSTANCES as usize] && !tracker_dirty_group {
        if !tracker_dirty_instances.iter().any(|dirty| *dirty) {
            return;
        }
    }
    draw_view.clear_dirty_flag(GlDirty::VERTEX_INSTANCES);

    for index in 0..16 {
        if !flags[(GlDirty::VERTEX_INSTANCE_0 as usize) + index] && !tracker_dirty_instances[index]
        {
            continue;
        }
        draw_view.clear_dirty_flag(GlDirty::VERTEX_INSTANCE_0 + index as u8);

        let stream = draw_view.vertex_streams()[index];
        let instancing_enabled = draw_view.vertex_stream_instances()[index] != 0;
        let divisor = if instancing_enabled {
            stream.frequency
        } else {
            0
        };
        unsafe {
            gl::VertexBindingDivisor(index as u32, divisor);
        }
    }
}

/// OpenGL rasterizer matching zuyu's `RasterizerOpenGL`.
///
/// Processes draw calls from the Maxwell 3D engine using OpenGL.
pub struct RasterizerOpenGL {
    syncpoints: Arc<SyncpointManager>,
    channel_caches: ChannelSetupCaches<ChannelInfo>,
    fence_manager: FenceManagerOpenGL,
    num_queued_commands: usize,
    has_written_global_memory: bool,
    last_clip_distance_mask: u32,
    /// Upstream `staging_buffer_pool` owner. Rust cache runtimes retain `Arc`
    /// clones, so ownership itself is the only direct use of this field.
    #[allow(dead_code)]
    staging_buffer_pool: SharedStagingBufferPool,
    // Rust drops fields in declaration order.  Declare the borrower before
    // both boxed cache owners so it is destroyed first, matching C++ reverse
    // destruction of upstream's later-declared `accelerate_dma` member.
    accelerate_dma: AccelerateDMA,
    buffer_cache: Box<OpenGLBufferCache>,
    /// Shared owner for the device tracker retained by `buffer_cache`.
    /// Eden stores the same manager as `Tegra::MaxwellDeviceMemoryManager&`.
    #[allow(dead_code)]
    device_memory: Arc<MaxwellDeviceMemoryManager>,
    /// Shared OpenGL program manager reference.
    ///
    /// Upstream `RasterizerOpenGL` stores `ProgramManager&`, with the concrete
    /// manager owned by `RendererOpenGL`.
    #[allow(dead_code)]
    program_manager: ProgramManagerHandle,
    texture_cache: Box<OpenGLTextureCache>,
    /// Generic region-tracking shader cache (region invalidation, guest address bookkeeping).
    /// Upstream inherits `GL::ShaderCache` from `VideoCommon::ShaderCache`; we keep them
    /// as two separate composed fields so each can evolve independently.
    shader_cache: ShaderCache,
    /// OpenGL-specific shader cache — owns compiled `GraphicsPipeline` / `ComputePipeline`
    /// objects and is the entry point for the draw hot path.
    gl_shader_cache: OpenGLShaderCache,
    query_cache: QueryCache,
    /// Non-owning equivalent of upstream `StateTracker& state_tracker`.
    /// `RendererOpenGL` owns the heap-stable tracker and outlives the
    /// rasterizer. OpenGL renderer operations serialize mutable access on the
    /// renderer thread, as upstream does without a tracker mutex.
    state_tracker: NonNull<StateTracker>,
    /// Unit tests construct a rasterizer without its renderer owner.
    #[cfg(test)]
    #[allow(dead_code)]
    owned_state_tracker: Option<Box<StateTracker>>,
    /// Non-owning equivalent of upstream `const Device& device`. The renderer
    /// owns the heap-stable device and drops this rasterizer before it.
    device: Option<NonNull<Device>>,
    has_viewport_swizzle: bool,
    has_fill_rectangle: bool,
    blit_image: Option<BlitImageHelper>,
    invalidate_gpu_cache_callback: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Per-channel GPU memory manager, extracted from `ChannelState` in
    /// `bind_channel`. Used to build the `GpuMemoryAccess` adapter for the
    /// buffer cache.
    channel_memory_manager: Option<Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>>,
    /// Raw guest/device memory reader installed through RendererBase.
    device_memory_reader: Option<crate::renderer_base::DeviceMemoryReader>,
    guest_memory_writer: Option<crate::renderer_base::GuestMemoryWriter>,
    /// GPU tick getter used for timestamped query writes.
    gpu_ticks_getter: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
    /// Callback to process pending GPU sync work from draw paths.
    ///
    /// Upstream `RasterizerOpenGL` stores a `Tegra::GPU&` and calls
    /// `gpu.TickWork()` directly in `PrepareDraw` / `DrawTexture`.
    /// Rust keeps the owner boundary explicit by receiving the same operation
    /// as a renderer-installed callback from `Gpu::bind_renderer`.
    gpu_tick_callback: Option<Arc<dyn Fn() + Send + Sync>>,
}

// The OpenGL rasterizer is owned and used from the renderer thread. The newly restored
// cache owners still contain backend trait-object slots that are not marked `Send`, but
// this slice does not populate them yet. Matching the existing renderer ownership model,
// we keep the type movable to the renderer thread.
unsafe impl Send for RasterizerOpenGL {}

struct GpuTickGuard(Option<Arc<dyn Fn() + Send + Sync>>);

impl Drop for GpuTickGuard {
    fn drop(&mut self) {
        if let Some(callback) = self.0.as_ref() {
            callback();
        }
    }
}

impl RasterizerOpenGL {
    fn device(&self) -> &Device {
        unsafe {
            self.device
                .expect("production OpenGL rasterizer requires its Device owner")
                .as_ref()
        }
    }

    fn must_flush_region_with(
        gpu_level_high: bool,
        is_buffer_modified: impl FnOnce() -> bool,
        is_texture_modified: impl FnOnce() -> bool,
    ) -> bool {
        if is_buffer_modified() {
            return true;
        }
        gpu_level_high && is_texture_modified()
    }

    /// Port of upstream `RasterizerOpenGL::AnyCommandQueued`.
    pub fn any_command_queued(&self) -> bool {
        self.num_queued_commands != 0
    }

    /// Port of Eden's currently uncalled `RasterizerOpenGL::SyncClipEnabled`.
    #[allow(dead_code)]
    fn sync_clip_enabled(&mut self, draw_view: &mut Maxwell3DDrawView<'_>, mut clip_mask: u32) {
        if !draw_view.dirty_flag(GlDirty::CLIP_DISTANCES)
            && !draw_view.dirty_flag(crate::dirty_flags::flags::SHADERS)
        {
            return;
        }
        draw_view.clear_dirty_flag(GlDirty::CLIP_DISTANCES);

        clip_mask &= draw_view.user_clip_enable_raw();
        if clip_mask == self.last_clip_distance_mask {
            return;
        }
        self.last_clip_distance_mask = clip_mask;

        for index in 0..crate::engines::maxwell_3d::NUM_CLIP_DISTANCES {
            unsafe {
                if clip_mask & (1 << index) != 0 {
                    gl::Enable(gl::CLIP_DISTANCE0 + index);
                } else {
                    gl::Disable(gl::CLIP_DISTANCE0 + index);
                }
            }
        }
    }

    /// Eden keeps this private method as an `UNIMPLEMENTED()` placeholder.
    #[allow(dead_code)]
    fn sync_clip_coef(&self) {
        error!("RasterizerOpenGL::SyncClipCoef is unimplemented");
    }

    /// Port of `RasterizerOpenGL::BeginTransformFeedback` using the draw snapshot
    /// that replaces Eden's persistent `Maxwell3D*` member.
    fn begin_transform_feedback(
        pipeline: &crate::renderer_opengl::gl_graphics_pipeline::GraphicsPipeline,
        transform_feedback_enabled: bool,
        tessellation_init_enabled: bool,
        tessellation_enabled: bool,
        primitive_mode: u32,
    ) {
        if !transform_feedback_enabled {
            return;
        }
        pipeline.configure_transform_feedback();
        if tessellation_init_enabled || tessellation_enabled {
            error!("OpenGL transform feedback with tessellation is unimplemented");
        }
        unsafe {
            gl::BeginTransformFeedback(primitive_mode);
        }
    }

    /// Port of `RasterizerOpenGL::EndTransformFeedback`.
    fn end_transform_feedback(transform_feedback_enabled: bool) {
        if transform_feedback_enabled {
            unsafe {
                gl::EndTransformFeedback();
            }
        }
    }

    fn sync_state(
        draw_view: &mut Maxwell3DDrawView<'_>,
        state_tracker: &mut StateTracker,
        has_depth_buffer_float: bool,
        has_viewport_swizzle: bool,
        has_fill_rectangle: bool,
        is_amd: bool,
        viewport_scale: f32,
        is_rescaling: bool,
    ) {
        let mut state_tracker = Some(state_tracker);
        sync_viewport(
            draw_view,
            &mut state_tracker,
            has_depth_buffer_float,
            has_viewport_swizzle,
            viewport_scale,
        );
        sync_rasterize_enable(draw_view, &mut state_tracker);
        sync_polygon_modes(draw_view, &mut state_tracker, has_fill_rectangle);
        sync_color_mask(draw_view, &mut state_tracker);
        sync_fragment_color_clamp_state(draw_view, &mut state_tracker);
        sync_multi_sample_state(draw_view, &mut state_tracker);
        sync_depth_test_state(draw_view, &mut state_tracker);
        sync_depth_clamp(draw_view, &mut state_tracker);
        sync_stencil_test_state(draw_view, &mut state_tracker);
        sync_blend_state(draw_view, &mut state_tracker);
        sync_logic_op_state(draw_view, &mut state_tracker, is_amd);
        sync_cull_mode(draw_view, &mut state_tracker);
        sync_primitive_restart(draw_view, &mut state_tracker);
        sync_scissor_test(draw_view, &mut state_tracker, is_rescaling);
        sync_point_state(draw_view, &mut state_tracker, viewport_scale);
        sync_line_state(draw_view, &mut state_tracker);
        sync_polygon_offset(draw_view, &mut state_tracker);
        sync_alpha_test(draw_view, &mut state_tracker);
        sync_framebuffer_srgb(draw_view, &mut state_tracker);
        sync_vertex_formats(draw_view, &mut state_tracker);
        sync_vertex_instances(draw_view, &mut state_tracker);
    }

    /// Port of Eden's template `RasterizerOpenGL::PrepareDraw`.
    fn prepare_draw(&mut self, mut draw_view: Maxwell3DDrawView<'_>, command: PreparedDrawCommand) {
        let _gpu_tick_guard = GpuTickGuard(self.gpu_tick_callback.clone());
        self.channel_memory_manager
            .as_ref()
            .cloned()
            .expect("OpenGL draw requires the bound GPU memory manager")
            .lock()
            .flush_caching();

        let is_indexed = match command {
            PreparedDrawCommand::Direct { is_indexed, .. } => is_indexed,
            PreparedDrawCommand::Indirect(params) => params.is_indexed,
        };
        let live_maxwell3d = draw_view.live_maxwell3d_ptr();
        let pipeline_gpu_memory = self.channel_memory_manager.as_ref().cloned();
        let gpu_tick_callback = self.gpu_tick_callback.as_ref().cloned();
        let has_depth_buffer_float = self.device().has_depth_buffer_float();
        let is_amd = self.device().is_amd();

        let Some(pipeline) = self
            .gl_shader_cache
            .current_graphics_pipeline(&mut self.shader_cache)
        else {
            debug!("RasterizerOpenGL::PrepareDraw skipped — no graphics pipeline available");
            return;
        };

        if let Some(callback) = gpu_tick_callback.as_ref() {
            callback();
        }

        let buffer_mutex: *const _ = &self.buffer_cache.mutex;
        let texture_mutex: *const _ = &self.texture_cache.base.mutex;
        lock_two_reentrant_mutexes!(
            buffer_mutex,
            texture_mutex,
            _buffer_mutex_guard,
            _texture_mutex_guard
        );
        if pipeline.uses_local_memory() {
            self.program_manager.lock().local_memory_warmup();
        }
        let maxwell3d = live_maxwell3d.expect("OpenGL PrepareDraw requires the bound Maxwell3D");
        let gpu_memory =
            pipeline_gpu_memory.expect("OpenGL PrepareDraw requires the bound GPU memory manager");
        pipeline.set_engine(maxwell3d, gpu_memory);
        if !pipeline.configure(is_indexed) {
            return;
        }

        let is_rescaling = self.texture_cache.is_rescaling_active();
        let viewport_scale = if is_rescaling {
            settings::values().resolution_info.up_factor
        } else {
            1.0
        };
        Self::sync_state(
            &mut draw_view,
            unsafe { self.state_tracker.as_mut() },
            has_depth_buffer_float,
            self.has_viewport_swizzle,
            self.has_fill_rectangle,
            is_amd,
            viewport_scale,
            is_rescaling,
        );

        let draw_state = draw_view.draw_state();
        let primitive_mode = super::maxwell_to_gl::primitive_topology(draw_state.topology);
        let transform_feedback_active = draw_view.transform_feedback_enabled();
        Self::begin_transform_feedback(
            pipeline,
            transform_feedback_active,
            draw_view.shader_config_enabled(ShaderStageType::TessInit),
            draw_view.shader_config_enabled(ShaderStageType::Tessellation),
            primitive_mode,
        );

        match command {
            PreparedDrawCommand::Direct {
                is_indexed,
                instance_count,
            } => emit_direct_draw(
                &self.buffer_cache,
                draw_state,
                is_indexed,
                instance_count,
                primitive_mode,
            ),
            PreparedDrawCommand::Indirect(params) => {
                emit_indirect_draw(&mut self.buffer_cache, draw_state, params, primitive_mode)
            }
        }

        Self::end_transform_feedback(transform_feedback_active);
        self.num_queued_commands = self.num_queued_commands.wrapping_add(1);
        self.has_written_global_memory |= pipeline.writes_global_memory();
    }

    /// Create a new rasterizer. Must be called with a current GL context.
    ///
    /// `device_memory` is the single shared `MaxwellDeviceMemoryManager`
    /// from `Host1x::memory_manager()`. Upstream:
    /// `RasterizerOpenGL::RasterizerOpenGL(emu_window, gpu, device_memory, ...)`.
    pub fn new(
        device: &Device,
        syncpoints: Arc<SyncpointManager>,
        device_memory: Arc<crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager>,
        program_manager: ProgramManagerHandle,
        state_tracker: &mut StateTracker,
        shared_context_factory: Option<super::gl_shader_context::SharedContextFactory>,
        shader_notify: crate::shader_notify::ShaderNotifyHandle,
    ) -> Self {
        let staging_buffer_pool = make_shared_staging_buffer_pool();
        let gl_runtime = super::gl_buffer_cache::BufferCacheRuntime::new(
            device,
            Arc::clone(&staging_buffer_pool),
        );
        let mut buffer_cache = Box::new(OpenGLBufferCache::new(device_memory.as_ref(), gl_runtime));
        let blit_image = Some(BlitImageHelper::new(program_manager.clone()));
        let mut texture_cache = Box::new(OpenGLTextureCache::new(
            device_memory.clone(),
            device,
            program_manager.clone(),
            state_tracker,
            Arc::clone(&staging_buffer_pool),
        ));
        let gl_shader_cache = OpenGLShaderCache::new(
            device,
            texture_cache.as_mut(),
            buffer_cache.as_mut(),
            program_manager.clone(),
            state_tracker,
            shared_context_factory,
            shader_notify,
        );
        let accelerate_dma = AccelerateDMA::new(buffer_cache.as_mut(), texture_cache.as_mut());
        Self {
            syncpoints,
            channel_caches: ChannelSetupCaches::new(),
            fence_manager: FenceManagerOpenGL::new(),
            num_queued_commands: 0,
            has_written_global_memory: false,
            last_clip_distance_mask: 0,
            staging_buffer_pool: Arc::clone(&staging_buffer_pool),
            buffer_cache,
            device_memory: Arc::clone(&device_memory),
            program_manager: program_manager.clone(),
            texture_cache,
            shader_cache: ShaderCache::new(device_memory),
            gl_shader_cache,
            query_cache: QueryCache::new(),
            accelerate_dma,
            state_tracker: NonNull::from(&mut *state_tracker),
            #[cfg(test)]
            owned_state_tracker: None,
            device: Some(NonNull::from(device)),
            has_viewport_swizzle: super::gl_device::has_extension("GL_NV_viewport_swizzle"),
            has_fill_rectangle: super::gl_device::has_extension("GL_NV_fill_rectangle"),
            blit_image,
            invalidate_gpu_cache_callback: None,
            channel_memory_manager: None,
            device_memory_reader: None,
            guest_memory_writer: None,
            gpu_ticks_getter: None,
            gpu_tick_callback: None,
        }
    }

    #[cfg(test)]
    fn new_for_test(syncpoints: Arc<SyncpointManager>) -> Self {
        let test_device_memory = Arc::new(
            crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default(),
        );
        let program_manager = ProgramManager::new_shared_for_test();
        let mut state_tracker = Box::new(StateTracker::new());
        let state_tracker_ptr = NonNull::from(state_tracker.as_mut());
        let staging_buffer_pool = make_shared_staging_buffer_pool();
        let buffer_runtime = super::gl_buffer_cache::BufferCacheRuntime::new_for_test(Arc::clone(
            &staging_buffer_pool,
        ));
        let mut buffer_cache = Box::new(OpenGLBufferCache::new(
            test_device_memory.as_ref(),
            buffer_runtime,
        ));
        let mut texture_cache = Box::new(OpenGLTextureCache::new_with_caps(
            Arc::clone(&test_device_memory),
            true,
            false,
            false,
            false,
            program_manager.clone(),
            state_tracker.as_mut(),
            Arc::clone(&staging_buffer_pool),
        ));
        let accelerate_dma = AccelerateDMA::new(buffer_cache.as_mut(), texture_cache.as_mut());
        Self {
            syncpoints,
            channel_caches: ChannelSetupCaches::new(),
            fence_manager: FenceManagerOpenGL::new_for_test(),
            num_queued_commands: 0,
            has_written_global_memory: false,
            last_clip_distance_mask: 0,
            staging_buffer_pool: Arc::clone(&staging_buffer_pool),
            buffer_cache,
            device_memory: Arc::clone(&test_device_memory),
            program_manager: program_manager.clone(),
            texture_cache,
            shader_cache: ShaderCache::default(),
            gl_shader_cache: OpenGLShaderCache::new_for_test(),
            query_cache: QueryCache::new_for_test(),
            accelerate_dma,
            state_tracker: state_tracker_ptr,
            owned_state_tracker: Some(state_tracker),
            device: None,
            has_viewport_swizzle: false,
            has_fill_rectangle: false,
            blit_image: None,
            invalidate_gpu_cache_callback: None,
            channel_memory_manager: None,
            device_memory_reader: None,
            guest_memory_writer: None,
            gpu_ticks_getter: None,
            gpu_tick_callback: None,
        }
    }

    /// Rust adaptation for upstream `RasterizerOpenGL::InvalidateGPUCache()`,
    /// which delegates to the owning `GPU`.
    pub fn set_invalidate_gpu_cache_callback(&mut self, callback: Arc<dyn Fn() + Send + Sync>) {
        self.invalidate_gpu_cache_callback = Some(callback);
    }

    pub fn set_gpu_memory_reader(&mut self, _reader: crate::shader_environment::GpuMemoryReader) {
        if let Some(mm) = self.channel_memory_manager.as_ref() {
            self.buffer_cache
                .set_gpu_memory(Box::new(GpuMemoryAccessAdapter { mm: Arc::clone(mm) }));
        }
    }

    pub fn set_device_memory_reader(&mut self, reader: crate::renderer_base::DeviceMemoryReader) {
        self.device_memory_reader = Some(Arc::clone(&reader));
        self.buffer_cache
            .set_device_memory(Box::new(DeviceMemoryAccessAdapter {
                device_reader: reader,
                guest_writer: self.guest_memory_writer.clone(),
            }));
    }

    pub fn set_gpu_ticks_getter(&mut self, getter: Arc<dyn Fn() -> u64 + Send + Sync>) {
        self.gpu_ticks_getter = Some(getter);
    }

    pub fn set_gpu_tick_callback(&mut self, callback: Arc<dyn Fn() + Send + Sync>) {
        self.gpu_tick_callback = Some(callback);
    }

    fn make_query_fallback_operation(
        mm: Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>,
        gpu_addr: u64,
        has_timeout: bool,
        payload: u32,
        gpu_ticks_getter: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
    ) -> Box<dyn FnOnce() + Send> {
        Box::new(move || {
            let mut mm = mm.lock();
            if has_timeout {
                let gpu_ticks = gpu_ticks_getter
                    .as_ref()
                    .expect("timestamped OpenGL queries require the GPU tick getter")(
                );
                mm.write::<u64>(gpu_addr + 8, gpu_ticks);
                mm.write::<u64>(gpu_addr, payload as u64);
            } else {
                mm.write::<u32>(gpu_addr, payload);
            }
        })
    }

    fn query_fallback(
        &mut self,
        gpu_addr: u64,
        query_type: u32,
        flags: QueryPropertiesFlags,
        mut payload: u32,
        _subreport: u32,
    ) {
        if query_type != crate::query_cache::types::QueryType::Payload as u32 {
            payload = 1;
        }
        let mm = self
            .channel_memory_manager
            .as_ref()
            .cloned()
            .expect("OpenGL query fallback requires the bound GPU memory manager");
        let has_timeout = flags.contains(QueryPropertiesFlags::HAS_TIMEOUT);
        let is_fence = flags.contains(QueryPropertiesFlags::IS_A_FENCE);
        let gpu_ticks_getter = self.gpu_ticks_getter.as_ref().cloned();
        let operation = Self::make_query_fallback_operation(
            mm,
            gpu_addr,
            has_timeout,
            payload,
            gpu_ticks_getter,
        );
        if is_fence {
            RasterizerInterface::signal_fence(self, operation);
        } else {
            operation();
        }
    }

    pub fn set_guest_memory_writer(&mut self, writer: GuestMemoryWriter) {
        self.guest_memory_writer = Some(Arc::clone(&writer));
        self.texture_cache.set_guest_memory_writer(writer);
        if let Some(device_reader) = self.device_memory_reader.as_ref().cloned() {
            self.buffer_cache
                .set_device_memory(Box::new(DeviceMemoryAccessAdapter {
                    device_reader,
                    guest_writer: self.guest_memory_writer.clone(),
                }));
        }
    }

    /// Port of `RasterizerOpenGL::AccelerateDisplay`.
    pub fn accelerate_display(
        &mut self,
        config: &FramebufferConfig,
        framebuffer_addr: u64,
        _pixel_stride: u32,
    ) -> Option<FramebufferTextureInfo> {
        if framebuffer_addr == 0 {
            return None;
        }

        let texture_cache: *mut OpenGLTextureCache = &mut *self.texture_cache;
        let _texture_lock = unsafe { (*texture_cache).base.mutex.lock() };
        let framebuffer_view =
            unsafe { (*texture_cache).try_find_framebuffer_image_view(config, framebuffer_addr) }?;
        let resolution = settings::values().resolution_info.clone();
        let scaled_width = if framebuffer_view.scaled {
            resolution.scale_up_u32(framebuffer_view.width)
        } else {
            framebuffer_view.width
        };
        let scaled_height = if framebuffer_view.scaled {
            resolution.scale_up_u32(framebuffer_view.height)
        } else {
            framebuffer_view.height
        };
        Some(FramebufferTextureInfo {
            display_texture: framebuffer_view.display_texture,
            width: framebuffer_view.width,
            height: framebuffer_view.height,
            scaled_width,
            scaled_height,
        })
    }

    fn should_wait_async_flushes(&mut self) -> bool {
        let buffer_cache: *mut OpenGLBufferCache = &mut *self.buffer_cache;
        let texture_cache: *mut OpenGLTextureCache = &mut *self.texture_cache;
        let buffer_mutex: *const _ = unsafe { &(*buffer_cache).mutex };
        let texture_mutex: *const _ = unsafe { &(*texture_cache).base.mutex };
        lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_lock, _texture_lock);
        unsafe {
            (*texture_cache).should_wait_async_flushes()
                || (*buffer_cache).should_wait_async_flushes()
                || self.query_cache.should_wait_async_flushes()
        }
    }

    fn should_flush_async(&mut self) -> bool {
        let buffer_cache: *mut OpenGLBufferCache = &mut *self.buffer_cache;
        let texture_cache: *mut OpenGLTextureCache = &mut *self.texture_cache;
        let buffer_mutex: *const _ = unsafe { &(*buffer_cache).mutex };
        let texture_mutex: *const _ = unsafe { &(*texture_cache).base.mutex };
        lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_lock, _texture_lock);
        unsafe {
            (*texture_cache).has_uncommitted_flushes()
                || (*buffer_cache).has_uncommitted_flushes()
                || self.query_cache.has_uncommitted_flushes()
        }
    }

    fn pop_async_flushes(&mut self) {
        let buffer_cache: *mut OpenGLBufferCache = &mut *self.buffer_cache;
        let texture_cache: *mut OpenGLTextureCache = &mut *self.texture_cache;
        let buffer_mutex: *const _ = unsafe { &(*buffer_cache).mutex };
        let texture_mutex: *const _ = unsafe { &(*texture_cache).base.mutex };
        lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_lock, _texture_lock);
        unsafe {
            (*texture_cache).pop_async_flushes();
            (*buffer_cache).pop_async_flushes();
        }
        let any_command_queued = self.any_command_queued();
        self.query_cache.pop_async_flushes(any_command_queued);
    }

    fn commit_async_flushes(&mut self) {
        let buffer_cache: *mut OpenGLBufferCache = &mut *self.buffer_cache;
        let texture_cache: *mut OpenGLTextureCache = &mut *self.texture_cache;
        let buffer_mutex: *const _ = unsafe { &(*buffer_cache).mutex };
        let texture_mutex: *const _ = unsafe { &(*texture_cache).base.mutex };
        lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_lock, _texture_lock);
        unsafe {
            (*texture_cache).commit_async_flushes();
            (*buffer_cache).commit_async_flushes();
        }
        self.query_cache.commit_async_flushes();
    }
}

impl RasterizerInterface for RasterizerOpenGL {
    fn load_disk_resources(
        &mut self,
        title_id: u64,
        stop_loading: crate::rasterizer_interface::DiskResourceLoadStop,
        callback: crate::rasterizer_interface::DiskResourceLoadCallback,
    ) {
        self.gl_shader_cache
            .load_disk_resources(title_id, stop_loading, callback);
    }

    /// Port of `RasterizerOpenGL::Draw(bool is_indexed, u32 instance_count)`
    /// (cpp:267) + `RasterizerOpenGL::PrepareDraw` (cpp:230).
    ///
    /// Upstream call chain (abridged):
    ///   `RasterizerOpenGL::Draw`
    ///   → `PrepareDraw(is_indexed, draw_func)`
    ///   → `ShaderCache::CurrentGraphicsPipeline()` (compiles pipeline on miss)
    ///   → `GraphicsPipeline::Configure(is_indexed)` (bind program, UBOs, textures, XFB)
    ///   → `SyncState()` (global GL state sync)
    ///   → read `maxwell3d->draw_manager->GetDrawState()` for topology and
    ///     vertex/index buffer bindings
    ///   → `glDrawElements{Instanced,BaseVertex,...}` or `glDrawArrays{Instanced,BaseInstance,...}`
    ///
    /// The `MaxwellToGL::PrimitiveTopology` mapping is intentionally kept
    /// here (not on the pipeline) because upstream re-reads topology on
    /// every `Draw` — a single pipeline key may be drawn with multiple
    /// topologies in successive calls.
    fn draw(&mut self, draw_view: Maxwell3DDrawView<'_>, instance_count: u32) {
        let is_indexed = draw_view.is_indexed();
        self.prepare_draw(
            draw_view,
            PreparedDrawCommand::Direct {
                is_indexed,
                instance_count,
            },
        );
    }

    /// Port of `RasterizerOpenGL::DrawIndirect`.
    fn draw_indirect(&mut self, indirect_view: Maxwell3DIndirectView<'_>) {
        let params = *indirect_view.params();
        let cache_params = crate::buffer_cache::buffer_cache_base::DrawIndirectParams {
            indirect_start_address: params.indirect_start_address,
            count_start_address: params.count_start_address,
            buffer_size: params.buffer_size as u64,
            max_draw_counts: params.max_draw_counts as u32,
            stride: params.stride as u32,
            include_count: params.include_count,
        };

        self.buffer_cache.set_draw_indirect(Some(cache_params));
        self.prepare_draw(
            indirect_view.into_draw_view(),
            PreparedDrawCommand::Indirect(params),
        );
        self.buffer_cache.set_draw_indirect(None);
    }

    fn draw_texture(
        &mut self,
        mut draw_texture_view: crate::engines::draw_manager::Maxwell3DDrawTextureView<'_>,
    ) {
        let _gpu_tick_guard = GpuTickGuard(self.gpu_tick_callback.clone());
        let draw_texture_state = draw_texture_view.draw_texture_state();
        let render_targets = draw_texture_view.render_targets();
        let descriptor_sync_regs = draw_texture_view.descriptor_sync_regs();

        let texture_cache: *mut OpenGLTextureCache = &mut *self.texture_cache;
        let framebuffer = unsafe {
            (*texture_cache)
                .base
                .synchronize_graphics_descriptors(descriptor_sync_regs);
            (*texture_cache).update_render_targets_and_get_framebuffer_from_snapshot(
                &render_targets,
                &mut draw_texture_view,
                false,
                None,
            )
        };
        let (framebuffer, _, _) = framebuffer;

        let is_rescaling = unsafe { (*texture_cache).base.is_rescaling };
        let has_depth_buffer_float = self.device().has_depth_buffer_float();
        let has_draw_texture = self.device().has_draw_texture();
        let is_amd = self.device().is_amd();
        let viewport_scale = if is_rescaling {
            settings::values().resolution_info.up_factor
        } else {
            1.0
        };
        Self::sync_state(
            draw_texture_view.draw_view_mut(),
            unsafe { self.state_tracker.as_mut() },
            has_depth_buffer_float,
            self.has_viewport_swizzle,
            self.has_fill_rectangle,
            is_amd,
            viewport_scale,
            is_rescaling,
        );

        let sampler_id = unsafe {
            (*texture_cache)
                .base
                .get_sampler_id(draw_texture_state.src_sampler, true)
        };
        let sampler = unsafe { (*texture_cache).get_sampler(sampler_id) }
            .expect("OpenGL DrawTexture sampler slot must exist")
            .handle();
        let (texture, source_size) =
            unsafe { (*texture_cache).draw_texture_source(draw_texture_state.src_texture) }
                .expect("OpenGL DrawTexture image-view slot must exist");

        let resolution = settings::values().resolution_info.clone();
        let scale = |value: f32| resolution.scale_up_i32(value as i32);
        let dst_region = Region2D {
            start: Offset2D {
                x: scale(draw_texture_state.dst_x0),
                y: scale(draw_texture_state.dst_y0),
            },
            end: Offset2D {
                x: scale(draw_texture_state.dst_x1),
                y: scale(draw_texture_state.dst_y1),
            },
        };
        let src_region = Region2D {
            start: Offset2D {
                x: scale(draw_texture_state.src_x0),
                y: scale(draw_texture_state.src_y0),
            },
            end: Offset2D {
                x: scale(draw_texture_state.src_x1),
                y: scale(draw_texture_state.src_y1),
            },
        };
        let src_size = Extent3D {
            width: resolution.scale_up_u32(source_size.width),
            height: resolution.scale_up_u32(source_size.height),
            depth: source_size.depth,
        };

        if has_draw_texture {
            let draw_texture = GL_DRAW_TEXTURE_NV
                .get()
                .and_then(|entry| *entry)
                .expect("GL_NV_draw_texture advertised without glDrawTextureNV");
            unsafe { self.state_tracker.as_mut() }.bind_framebuffer(framebuffer);
            unsafe {
                draw_texture(
                    texture,
                    sampler,
                    dst_region.start.x as f32,
                    dst_region.start.y as f32,
                    dst_region.end.x as f32,
                    dst_region.end.y as f32,
                    0.0,
                    draw_texture_state.src_x0 / source_size.width as f32,
                    draw_texture_state.src_y0 / source_size.height as f32,
                    draw_texture_state.src_x1 / source_size.width as f32,
                    draw_texture_state.src_y1 / source_size.height as f32,
                );
            }
        }
        if !has_draw_texture {
            let blit_image = self
                .blit_image
                .as_ref()
                .expect("production OpenGL rasterizer owns BlitImageHelper");
            blit_image.blit_color(
                framebuffer,
                texture,
                sampler,
                &dst_region,
                &src_region,
                &src_size,
            );
            unsafe { self.state_tracker.as_mut() }.invalidate_state();
        }
        self.num_queued_commands = self.num_queued_commands.wrapping_add(1);
    }

    fn clear(&mut self, mut clear_view: Maxwell3DClearView<'_>, _layer_count: u32) {
        // Upstream `RasterizerOpenGL::Clear` starts with
        // `gpu_memory->FlushCaching()`.
        self.channel_memory_manager
            .as_ref()
            .cloned()
            .expect("OpenGL clear requires the bound GPU memory manager")
            .lock()
            .flush_caching();
        let clear_state = clear_view.clear_state();
        let render_targets = clear_view.render_targets();
        let flags = clear_state.flags;
        let clear_z = flags & (1 << 0) != 0;
        let clear_s = flags & (1 << 1) != 0;
        let clear_r = flags & (1 << 2) != 0;
        let clear_g = flags & (1 << 3) != 0;
        let clear_b = flags & (1 << 4) != 0;
        let clear_a = flags & (1 << 5) != 0;
        let use_color = clear_r || clear_g || clear_b || clear_a;
        let use_depth = clear_z;
        let use_stencil = clear_s;

        if !use_color && !use_depth && !use_stencil {
            return;
        }

        let rt_index = ((flags >> 6) & 0xF) as usize;
        {
            let mut state_tracker = Some(unsafe { self.state_tracker.as_mut() });
            if use_color {
                if let Some(tracker) = state_tracker.as_deref_mut() {
                    tracker.notify_color_mask(rt_index);
                }
                unsafe {
                    gl::ColorMaski(
                        rt_index as u32,
                        if clear_r { gl::TRUE } else { gl::FALSE },
                        if clear_g { gl::TRUE } else { gl::FALSE },
                        if clear_b { gl::TRUE } else { gl::FALSE },
                        if clear_a { gl::TRUE } else { gl::FALSE },
                    );
                }
                sync_fragment_color_clamp_state(&mut clear_view, &mut state_tracker);
                sync_framebuffer_srgb(&mut clear_view, &mut state_tracker);
            }
            if use_depth {
                if render_targets.zeta.enabled {
                    debug!("Tried to clear Z but buffer is not enabled!");
                }
                if let Some(tracker) = state_tracker.as_deref_mut() {
                    tracker.notify_depth_mask();
                }
                unsafe { gl::DepthMask(gl::TRUE) };
            }
            if use_stencil && render_targets.zeta.enabled {
                debug!("Tried to clear stencil but buffer is not enabled!");
            }
            sync_rasterize_enable(&mut clear_view, &mut state_tracker);
            sync_stencil_test_state(&mut clear_view, &mut state_tracker);
        }

        let texture_cache: *mut OpenGLTextureCache = &mut *self.texture_cache;
        let is_rescaling = unsafe { (*texture_cache).base.is_rescaling };
        let has_depth_buffer_float = self.device().has_depth_buffer_float();
        let viewport_scale = if is_rescaling {
            settings::values().resolution_info.up_factor
        } else {
            1.0
        };
        let texture_mutex: *const _ = unsafe { &(*texture_cache).base.mutex };
        let _texture_lock = unsafe { (*texture_mutex).lock() };
        let framebuffer = unsafe {
            let clear_scissor = clear_view.use_scissor().then(|| clear_view.scissor(0));
            (*texture_cache).update_render_targets_and_get_framebuffer_from_snapshot(
                &render_targets,
                &mut clear_view,
                true,
                clear_scissor,
            )
        };
        let (framebuffer, _, _) = framebuffer;
        unsafe { self.state_tracker.as_mut() }.bind_framebuffer(framebuffer);
        {
            let mut state_tracker = Some(unsafe { self.state_tracker.as_mut() });
            sync_viewport(
                &mut clear_view,
                &mut state_tracker,
                has_depth_buffer_float,
                self.has_viewport_swizzle,
                viewport_scale,
            );
            if clear_view.use_scissor() {
                sync_scissor_test(&mut clear_view, &mut state_tracker, is_rescaling);
            } else if let Some(tracker) = state_tracker.as_deref_mut() {
                tracker.notify_scissor0();
                unsafe { gl::Disablei(gl::SCISSOR_TEST, 0) };
            }
        }
        if clear_view.use_viewport_clip0() {
            error!("Clear with use_viewport_clip0 is unimplemented");
        }
        unsafe {
            if use_color {
                gl::ClearBufferfv(gl::COLOR, rt_index as i32, clear_state.color.as_ptr());
            }
            if use_depth && use_stencil {
                gl::ClearBufferfi(gl::DEPTH_STENCIL, 0, clear_state.depth, clear_state.stencil);
            } else if use_depth {
                gl::ClearBufferfv(gl::DEPTH, 0, &clear_state.depth);
            } else if use_stencil {
                gl::ClearBufferiv(gl::STENCIL, 0, &clear_state.stencil);
            }
        }
        self.num_queued_commands = self.num_queued_commands.wrapping_add(1);
    }

    fn dispatch_compute(&mut self, _dispatch: &DispatchCall) {
        // Upstream `RasterizerOpenGL::DispatchCompute` starts with
        // `gpu_memory->FlushCaching()`, then obtains the current compute
        // pipeline whose `Configure()` synchronizes compute TIC/TSC descriptors.
        self.channel_memory_manager
            .as_ref()
            .cloned()
            .expect("OpenGL compute dispatch requires the bound GPU memory manager")
            .lock()
            .flush_caching();
        let (kepler_compute, indirect_compute_address, grid) = {
            let kepler_compute = self
                .shader_cache
                .current_kepler_compute()
                .expect("OpenGL compute dispatch requires the bound KeplerCompute engine");
            let qmd = kepler_compute.launch_description();
            (
                NonNull::from(kepler_compute),
                kepler_compute.get_indirect_compute_address(),
                [qmd.grid_dim_x, qmd.grid_dim_y, qmd.grid_dim_z],
            )
        };
        let gpu_memory = self
            .shader_cache
            .current_gpu_memory()
            .expect("OpenGL compute dispatch requires the bound GPU memory manager");
        let Some(pipeline) = self
            .gl_shader_cache
            .current_compute_pipeline(&mut self.shader_cache)
        else {
            return;
        };
        if pipeline.uses_local_memory() {
            self.program_manager.lock().local_memory_warmup();
        }
        pipeline.set_engine(kepler_compute, gpu_memory);
        pipeline.configure();
        if let Some(indirect_address) = indirect_compute_address {
            let (buffer_id, offset) = self.buffer_cache.obtain_buffer(
                indirect_address,
                12,
                ObtainBufferSynchronize::FullSynchronize,
                ObtainBufferOperation::DiscardWrite,
            );
            let handle = self.buffer_cache.get_buffer_gpu_handle(buffer_id);
            unsafe {
                gl::BindBuffer(gl::DISPATCH_INDIRECT_BUFFER, handle);
                gl::DispatchComputeIndirect(offset as isize);
            }
            return;
        }
        unsafe {
            gl::DispatchCompute(grid[0], grid[1], grid[2]);
        }
        self.num_queued_commands = self.num_queued_commands.wrapping_add(1);
        self.has_written_global_memory |= pipeline.writes_global_memory();
    }

    fn reset_counter(&mut self, query_type: u32) {
        let Some(mapped_query_type) = maxwell_to_video_core_query(query_type) else {
            if query_type != crate::query_cache::types::QueryType::Payload as u32 {
                error!("Reset query type: {query_type}");
            }
            return;
        };
        let any_command_queued = self.any_command_queued();
        self.query_cache
            .reset_counter(mapped_query_type, any_command_queued);
    }

    fn query(
        &mut self,
        gpu_addr: u64,
        query_type: u32,
        flags: QueryPropertiesFlags,
        payload: u32,
        _subreport: u32,
    ) {
        let Some(mapped_query_type) = maxwell_to_video_core_query(query_type) else {
            self.query_fallback(gpu_addr, query_type, flags, payload, _subreport);
            return;
        };

        let this = self as *mut Self;
        let this_for_invalidate = this as usize;
        let timestamp = flags.contains(QueryPropertiesFlags::HAS_TIMEOUT).then(|| {
            self.gpu_ticks_getter
                .as_ref()
                .expect("timestamped OpenGL queries require the GPU tick getter")()
        });
        let any_command_queued = self.any_command_queued();
        self.query_cache.query(
            gpu_addr,
            mapped_query_type,
            timestamp,
            any_command_queued,
            move |func| unsafe { (*this).sync_operation(func) },
            move |addr, size| unsafe {
                (*(this_for_invalidate as *mut Self)).invalidate_region(
                    addr,
                    size,
                    CacheType::NO_QUERY_CACHE,
                )
            },
        );
    }

    fn bind_graphics_uniform_buffer(&mut self, stage: usize, index: u32, gpu_addr: u64, size: u32) {
        let buffer_cache: *mut OpenGLBufferCache = &mut *self.buffer_cache;
        let _buffer_guard = unsafe { (*buffer_cache).mutex.lock() };
        unsafe {
            (*buffer_cache).bind_graphics_uniform_buffer(stage, index, gpu_addr, size);
        }
    }

    fn disable_graphics_uniform_buffer(&mut self, stage: usize, index: u32) {
        self.buffer_cache
            .disable_graphics_uniform_buffer(stage, index);
    }

    fn signal_fence(&mut self, func: Box<dyn FnOnce() + Send>) {
        let this = self as *mut Self;
        let this_for_pop = this as usize;
        self.fence_manager.signal_fence(
            func,
            move || unsafe { (*this).should_wait_async_flushes() },
            move || unsafe { (*(this_for_pop as *mut Self)).pop_async_flushes() },
            move || unsafe { (*this).num_queued_commands != 0 || (*this).should_flush_async() },
            move || unsafe { (*this).commit_async_flushes() },
            move || unsafe { (*this).flush_commands() },
            move || unsafe { (*this).invalidate_gpu_cache() },
        );
    }

    fn sync_operation(&mut self, func: Box<dyn FnOnce() + Send>) {
        self.fence_manager.sync_operation(func);
    }

    fn signal_sync_point(&mut self, id: u32) {
        let this = self as *mut Self;
        let this_for_pop = this as usize;
        let syncpoints = Arc::clone(&self.syncpoints);
        self.fence_manager.signal_sync_point(
            id,
            {
                let syncpoints = Arc::clone(&syncpoints);
                move |value| syncpoints.increment_guest(value)
            },
            move |value| syncpoints.increment_host(value),
            move || unsafe { (*this).should_wait_async_flushes() },
            move || unsafe { (*(this_for_pop as *mut Self)).pop_async_flushes() },
            move || unsafe { (*this).num_queued_commands != 0 || (*this).should_flush_async() },
            move || unsafe { (*this).commit_async_flushes() },
            move || unsafe { (*this).flush_commands() },
            move || unsafe { (*this).invalidate_gpu_cache() },
        );
    }

    fn signal_reference(&mut self) {
        let this = self as *mut Self;
        let this_for_pop = this as usize;
        self.fence_manager.signal_ordering(
            move || unsafe { (*this).should_wait_async_flushes() },
            move || unsafe { (*(this_for_pop as *mut Self)).pop_async_flushes() },
            move || unsafe {
                let buffer_cache: *mut OpenGLBufferCache = &mut *(*this).buffer_cache;
                let _buffer_guard = (*buffer_cache).mutex.lock();
                (*buffer_cache).accumulate_flushes();
            },
        );
    }

    fn release_fences(&mut self, force: bool) {
        let this = self as *mut Self;
        let this_for_pop = this as usize;
        self.fence_manager.wait_pending_fences(
            force,
            move || unsafe { (*this).should_wait_async_flushes() },
            move || unsafe { (*(this_for_pop as *mut Self)).pop_async_flushes() },
            move || unsafe { (*this).num_queued_commands != 0 || (*this).should_flush_async() },
            move || unsafe { (*this).commit_async_flushes() },
            move || unsafe { (*this).flush_commands() },
            move || unsafe { (*this).invalidate_gpu_cache() },
        );
    }

    fn flush_all(&mut self) {}

    fn flush_region(&mut self, addr: u64, size: u64, which: CacheType) {
        if addr == 0 || size == 0 {
            return;
        }
        if which.contains(CacheType::TEXTURE_CACHE) {
            unsafe {
                let texture_mutex: *const _ = &self.texture_cache.base.mutex;
                let _texture_guard = (*texture_mutex).lock();
                self.texture_cache.download_memory(addr, size as usize);
            }
        }
        if which.contains(CacheType::BUFFER_CACHE) {
            unsafe {
                let buffer_mutex: *const _ = &self.buffer_cache.mutex;
                let _buffer_guard = (*buffer_mutex).lock();
                self.buffer_cache.download_memory(addr, size);
            }
        }
        if which.contains(CacheType::QUERY_CACHE) {
            let any_command_queued = self.any_command_queued();
            self.query_cache
                .flush_region(addr, size as usize, any_command_queued);
        }
    }

    fn must_flush_region(&self, addr: u64, size: u64, which: CacheType) -> bool {
        Self::must_flush_region_with(
            common::settings::is_gpu_level_high(&common::settings::values()),
            || {
                if !which.contains(CacheType::BUFFER_CACHE) {
                    return false;
                }
                let _buffer_guard = self.buffer_cache.mutex.lock();
                self.buffer_cache
                    .is_region_gpu_modified(addr, size as usize)
            },
            || {
                if !which.contains(CacheType::TEXTURE_CACHE) {
                    return false;
                }
                let _texture_guard = self.texture_cache.base.mutex.lock();
                self.texture_cache
                    .base
                    .is_region_gpu_modified(addr, size as usize)
            },
        )
    }

    fn get_flush_area(&self, addr: u64, size: u64) -> RasterizerDownloadArea {
        unsafe {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let _texture_guard = (*texture_mutex).lock();
            let texture_cache: *mut OpenGLTextureCache =
                &*self.texture_cache as *const OpenGLTextureCache as *mut OpenGLTextureCache;
            if let Some(area) = (*texture_cache).get_flush_area(addr, size) {
                return area;
            }
        }

        unsafe {
            let buffer_mutex: *const _ = &self.buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            let buffer_cache: *mut OpenGLBufferCache =
                &*self.buffer_cache as *const OpenGLBufferCache as *mut OpenGLBufferCache;
            if let Some(area) = (*buffer_cache).get_flush_area(addr, size) {
                return RasterizerDownloadArea {
                    start_address: area.start_address,
                    end_address: area.end_address,
                    preemptive: area.preemtive,
                };
            }
        }

        const PAGE: u64 = ruzu_core::device_memory_manager::DEVICE_PAGESIZE as u64;
        RasterizerDownloadArea {
            start_address: addr & !(PAGE - 1),
            end_address: (addr + size + PAGE - 1) & !(PAGE - 1),
            preemptive: true,
        }
    }

    fn invalidate_region(&mut self, addr: u64, size: u64, which: CacheType) {
        if addr == 0 || size == 0 {
            return;
        }
        if which.contains(CacheType::TEXTURE_CACHE) {
            unsafe {
                let texture_mutex: *const _ = &self.texture_cache.base.mutex;
                let _texture_guard = (*texture_mutex).lock();
                self.texture_cache.write_memory(addr, size as usize);
            }
        }
        if which.contains(CacheType::BUFFER_CACHE) {
            unsafe {
                let buffer_mutex: *const _ = &self.buffer_cache.mutex;
                let _buffer_guard = (*buffer_mutex).lock();
                self.buffer_cache.write_memory(addr, size);
            }
        }
        if which.contains(CacheType::SHADER_CACHE) {
            self.shader_cache.invalidate_region(addr, size as usize);
        }
        if which.contains(CacheType::QUERY_CACHE) {
            let any_command_queued = self.any_command_queued();
            self.query_cache
                .invalidate_region(addr, size as usize, any_command_queued);
        }
    }

    fn on_cache_invalidation(&mut self, addr: u64, size: u64) {
        if addr == 0 || size == 0 {
            return;
        }
        // Mirrors upstream `RasterizerOpenGL::OnCacheInvalidation`
        // (gl_rasterizer.cpp:693-707): take per-cache mutexes in order
        // (texture_cache, buffer_cache) before mutating cache state.
        //
        // The sentinel `Mutex<()>` lives INSIDE the cache it protects,
        // so we acquire it through a raw pointer to avoid Rust's borrow
        // checker rejecting `&mut self.texture_cache` while a guard
        // borrows `&self.texture_cache.base.mutex` immutably. Upstream
        // C++ does this trivially (`std::scoped_lock lock{cache.mutex}`)
        // — the unsafe block matches that semantics.
        unsafe {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let _texture_guard = (*texture_mutex).lock();
            self.texture_cache.write_memory(addr, size as usize);
        }
        unsafe {
            let buffer_mutex: *const _ = &self.buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            self.buffer_cache.write_memory(addr, size);
        }
        self.shader_cache.invalidate_region(addr, size as usize);
    }

    fn on_cpu_write(&mut self, addr: u64, size: u64) -> bool {
        debug_assert!(addr != 0 || size != 0);
        // Mirrors upstream `RasterizerOpenGL::OnCPUWrite`
        // (gl_rasterizer.cpp:671-691): take per-cache mutexes before
        // mutating cache state. Without these locks, CPU emulation
        // threads invoking on_cpu_write via the JIT memory-write
        // trampoline race with the GPU thread using the same caches
        // for rendering — observed as a `hashbrown::Tag::full` SIGSEGV
        //
        // See on_cache_invalidation above for why the locks are taken
        // through raw pointers.
        let buffer_handled = unsafe {
            let buffer_mutex: *const _ = &self.buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            self.buffer_cache.on_cpu_write(addr, size)
        };
        if buffer_handled {
            return true;
        }
        unsafe {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let _texture_guard = (*texture_mutex).lock();
            self.texture_cache.write_memory(addr, size as usize);
        }
        self.shader_cache.invalidate_region(addr, size as usize);
        false
    }

    fn invalidate_gpu_cache(&mut self) {
        if let Some(callback) = &self.invalidate_gpu_cache_callback {
            callback();
        }
    }

    fn unmap_memory(&mut self, addr: u64, size: u64) {
        unsafe {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let _texture_guard = (*texture_mutex).lock();
            self.texture_cache.unmap_memory(addr, size as usize);
        }
        unsafe {
            let buffer_mutex: *const _ = &self.buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            self.buffer_cache.write_memory(addr, size);
        }
        self.shader_cache.on_cache_invalidation(addr, size as usize);
    }

    fn modify_gpu_memory(&mut self, as_id: usize, addr: u64, size: u64) {
        let texture_cache: *mut OpenGLTextureCache = &mut *self.texture_cache;
        let _texture_guard = unsafe { (*texture_cache).base.mutex.lock() };
        unsafe {
            (*texture_cache)
                .base
                .unmap_gpu_memory(as_id, addr, size as usize);
        }
    }

    fn flush_and_invalidate_region(&mut self, addr: u64, size: u64, which: CacheType) {
        if settings::is_gpu_level_high(&settings::values()) {
            self.flush_region(addr, size, which);
        }
        self.invalidate_region(addr, size, which);
    }

    fn wait_for_idle(&mut self) {
        unsafe { gl::MemoryBarrier(gl::ALL_BARRIER_BITS) };
        self.signal_reference();
    }

    fn fragment_barrier(&mut self) {
        unsafe {
            gl::TextureBarrier();
            gl::MemoryBarrier(gl::FRAMEBUFFER_BARRIER_BIT | gl::TEXTURE_FETCH_BARRIER_BIT);
        }
    }

    fn tiled_cache_barrier(&mut self) {
        unsafe { gl::TextureBarrier() }
    }

    fn flush_commands(&mut self) {
        if self.num_queued_commands == 0 {
            return;
        }
        self.num_queued_commands = 0;
        if self.has_written_global_memory {
            self.has_written_global_memory = false;
            unsafe { gl::MemoryBarrier(gl::BUFFER_UPDATE_BARRIER_BIT) };
        }
        unsafe {
            gl::Flush();
        }
    }

    fn tick_frame(&mut self) {
        self.num_queued_commands = 0;
        self.fence_manager.tick_frame();
        unsafe {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let _texture_guard = (*texture_mutex).lock();
            self.texture_cache.tick_frame();
        }
        unsafe {
            let buffer_mutex: *const _ = &self.buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            self.buffer_cache.tick_frame();
        }
    }

    fn accelerate_surface_copy(
        &mut self,
        src: &crate::engines::fermi_2d::Surface,
        dst: &crate::engines::fermi_2d::Surface,
        copy_config: &crate::engines::fermi_2d::Config,
    ) -> bool {
        let Some(mm) = self.channel_memory_manager.as_ref().cloned() else {
            return false;
        };
        let texture_cache: *mut OpenGLTextureCache = &mut *self.texture_cache;
        let accelerated = unsafe {
            let _texture_lock = (*texture_cache).base.mutex.lock();
            (*texture_cache).blit_image(
                dst,
                src,
                copy_config,
                |gpu_addr| mm.lock().gpu_to_cpu_address(gpu_addr),
                |gpu_addr, out| {
                    let guard = mm.lock();
                    guard.read_block(gpu_addr, out);
                    true
                },
            )
        };
        accelerated
    }

    fn accelerate_conditional_rendering_with_address(
        &mut self,
        condition_address: u64,
        compare_size: u64,
    ) -> bool {
        let Some(memory_manager) = self.channel_memory_manager.as_ref().cloned() else {
            return false;
        };
        let mut memory_manager = memory_manager.lock();
        memory_manager.flush_caching();
        if settings::is_gpu_level_high(&settings::values()) {
            return false;
        }
        memory_manager.is_memory_dirty_with_cache_type(
            condition_address,
            compare_size,
            CacheType::BUFFER_CACHE,
        )
    }

    fn access_accelerate_dma(&mut self) -> &mut dyn AccelerateDMAInterface {
        &mut self.accelerate_dma
    }

    fn accelerate_inline_to_memory(&mut self, address: u64, copy_size: usize, memory: &[u8]) {
        let mm = self
            .channel_memory_manager
            .as_ref()
            .cloned()
            .expect("OpenGL inline upload requires the bound GPU memory manager");
        debug_assert!(copy_size <= memory.len());
        // Upstream forwards `copy_size` and the raw span pointer without a
        // release-mode bounds check. The engine guarantees that the span has
        // at least that many bytes.
        let memory = unsafe { std::slice::from_raw_parts(memory.as_ptr(), copy_size) };
        let cpu_addr = {
            let mm = mm.lock();
            let Some(cpu_addr) = mm.gpu_to_cpu_address(address) else {
                mm.write_block(address, memory);
                return;
            };
            mm.write_block_unsafe(address, memory);
            cpu_addr
        };
        {
            let buffer_cache: *mut OpenGLBufferCache = &mut *self.buffer_cache;
            let _buffer_lock = unsafe { (*buffer_cache).mutex.lock() };
            unsafe {
                if !(*buffer_cache).inline_memory(cpu_addr, copy_size, memory) {
                    (*buffer_cache).write_memory(cpu_addr, copy_size as u64);
                }
            }
        }
        {
            let texture_cache: *mut OpenGLTextureCache = &mut *self.texture_cache;
            let _texture_lock = unsafe { (*texture_cache).base.mutex.lock() };
            unsafe {
                (*texture_cache).write_memory(cpu_addr, copy_size);
            }
        }
        self.shader_cache.invalidate_region(cpu_addr, copy_size);
        let any_command_queued = self.any_command_queued();
        self.query_cache
            .invalidate_region(cpu_addr, copy_size, any_command_queued);
    }

    fn initialize_channel(&mut self, channel: &mut crate::control::channel_state::ChannelState) {
        self.channel_caches.create_channel(channel);
        {
            let buffer_mutex: *const _ = &self.buffer_cache.mutex;
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);
            self.texture_cache.base.create_channel(channel);
            self.buffer_cache.create_channel(channel);
        }
        self.shader_cache.create_channel(channel);
        self.query_cache.create_channel(channel);
        unsafe { self.state_tracker.as_mut() }.setup_tables(channel);
    }

    fn bind_channel(&mut self, channel: &mut crate::control::channel_state::ChannelState) {
        self.channel_caches.bind_to_channel(channel.bind_id);
        {
            let buffer_mutex: *const _ = &self.buffer_cache.mutex;
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);
            self.texture_cache.base.bind_to_channel(channel.bind_id);
            self.buffer_cache.bind_to_channel(channel.bind_id);
        }
        self.shader_cache.bind_to_channel(channel.bind_id);
        self.query_cache.bind_to_channel(channel.bind_id);
        unsafe { self.state_tracker.as_mut() }.change_channel(channel);
        unsafe { self.state_tracker.as_mut() }.invalidate_state();
        self.channel_memory_manager = self
            .channel_caches
            .current_channel_state()
            .and_then(ChannelCacheAccessor::gpu_memory_arc);
        if let Some(mm) = self.channel_memory_manager.as_ref() {
            self.buffer_cache
                .set_gpu_memory(Box::new(GpuMemoryAccessAdapter { mm: Arc::clone(mm) }));
            if let Some(ref device_reader) = self.device_memory_reader {
                self.buffer_cache
                    .set_device_memory(Box::new(DeviceMemoryAccessAdapter {
                        device_reader: Arc::clone(device_reader),
                        guest_writer: self.guest_memory_writer.clone(),
                    }));
            }
        }
    }

    fn release_channel(&mut self, channel_id: i32) {
        unsafe { self.state_tracker.as_mut() }.release_channel(channel_id);
        self.channel_caches.erase_channel(channel_id);
        {
            let buffer_mutex: *const _ = &self.buffer_cache.mutex;
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);
            self.texture_cache.base.erase_channel(channel_id);
            self.buffer_cache.erase_channel(channel_id);
        }
        self.shader_cache.erase_channel(channel_id);
        self.query_cache.erase_channel(channel_id);
        self.channel_memory_manager = self
            .channel_caches
            .current_channel_state()
            .and_then(ChannelCacheAccessor::gpu_memory_arc);
        if let Some(mm) = self.channel_memory_manager.as_ref() {
            self.buffer_cache
                .set_gpu_memory(Box::new(GpuMemoryAccessAdapter { mm: Arc::clone(mm) }));
        } else {
            self.buffer_cache.clear_gpu_memory();
        }
    }

    fn register_transform_feedback(&mut self, tfb_object_addr: u64) {
        self.buffer_cache
            .bind_transform_feedback_object(tfb_object_addr);
    }

    fn has_draw_transform_feedback(&self) -> bool {
        true
    }
}

#[cfg(test)]
#[path = "gl_rasterizer_test.rs"]
mod tests;

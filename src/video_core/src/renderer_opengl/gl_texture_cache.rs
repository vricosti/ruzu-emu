// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `src/video_core/renderer_opengl/gl_texture_cache.h` and
//! `gl_texture_cache.cpp`.
//!
//! OpenGL texture cache -- manages GPU texture and image objects, framebuffers, and samplers.

use std::collections::HashMap;
use std::ptr::NonNull;

use common::settings;
use smallvec::SmallVec;

use super::gl_resource_manager::{
    OGLBuffer, OGLFramebuffer, OGLSampler, OGLTexture, OGLTextureView,
};
use super::gl_shader_manager::ProgramManagerHandle;
use super::gl_staging_buffer_pool::{SharedStagingBufferPool, StagingBufferMap};
use super::gl_state_tracker::StateTracker;
use super::util_shaders::UtilShaders;
use crate::engines::draw_manager::Maxwell3DRenderTargets;
use crate::engines::maxwell_3d::{RenderTargetInfo, ScissorInfo};
use crate::engines::maxwell_dma::dma;
use crate::framebuffer_config::FramebufferConfig;
use crate::renderer_base::GuestMemoryWriter;
use crate::shader_environment::TextureType;
use crate::surface::{PixelFormat, SurfaceType};
use crate::texture_cache::image_base::{ImageBase, ImageFlagBits};
use crate::texture_cache::image_info::ImageInfo;
use crate::texture_cache::image_view_base::{ImageViewBase, ImageViewFlagBits};
use crate::texture_cache::image_view_info::{ImageViewInfo, SwizzleSource};
use crate::texture_cache::render_targets::RenderTargets;
use crate::texture_cache::texture_cache::RenderTargetDirtyFlagAccess;
use crate::texture_cache::texture_cache_base::{
    BufferDownload, FramebufferImageView, PendingDownload, TextureCacheBase as CommonTextureCache,
};
#[cfg(test)]
use crate::texture_cache::types::SubresourceExtent;
use crate::texture_cache::types::{BufferImageCopy, ImageCopy};
use crate::texture_cache::types::{
    Extent2D, Extent3D, FramebufferId, ImageId, ImageType, ImageViewId, ImageViewType, Offset2D,
    Region2D, RelaxedOptions, SubresourceRange, NULL_IMAGE_ID, NULL_IMAGE_VIEW_ID, NULL_SAMPLER_ID,
};
use crate::texture_cache::util::{full_download_copies, map_size_bytes};

/// Number of render targets.
pub const NUM_RT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FramebufferAttachmentMode {
    Texture,
    TextureLayer(i32),
}

fn framebuffer_attachment_mode(view_base: &ImageViewBase) -> FramebufferAttachmentMode {
    if view_base.flags.contains(ImageViewFlagBits::SLICE) && view_base.range.extent.layers == 1 {
        FramebufferAttachmentMode::TextureLayer(view_base.range.base.layer)
    } else {
        FramebufferAttachmentMode::Texture
    }
}

fn framebuffer_attachment_texture(view_base: &ImageViewBase, backend_view: &ImageView) -> u32 {
    if view_base.flags.contains(ImageViewFlagBits::SLICE) {
        backend_view.handle_for_texture_type(TextureType::Color3D)
    } else {
        backend_view.default_handle()
    }
}

unsafe fn attach_framebuffer_texture(
    framebuffer: u32,
    attachment: u32,
    texture: u32,
    view_base: &ImageViewBase,
) {
    match framebuffer_attachment_mode(view_base) {
        FramebufferAttachmentMode::Texture => {
            gl::NamedFramebufferTexture(framebuffer, attachment, texture, 0);
        }
        FramebufferAttachmentMode::TextureLayer(layer) => {
            gl::NamedFramebufferTextureLayer(framebuffer, attachment, texture, 0, layer);
        }
    }
}

fn scale_up_image_copies(copies: &[ImageCopy], both_2d: bool) -> Vec<ImageCopy> {
    let resolution = settings::values().resolution_info.clone();
    copies
        .iter()
        .copied()
        .map(|mut copy| {
            copy.src_offset.x = resolution.scale_up_i32(copy.src_offset.x);
            copy.dst_offset.x = resolution.scale_up_i32(copy.dst_offset.x);
            copy.extent.width = resolution.scale_up_u32(copy.extent.width);
            if both_2d {
                copy.src_offset.y = resolution.scale_up_i32(copy.src_offset.y);
                copy.dst_offset.y = resolution.scale_up_i32(copy.dst_offset.y);
                copy.extent.height = resolution.scale_up_u32(copy.extent.height);
            }
            copy
        })
        .collect()
}

fn is_pixel_format_bgr(format: PixelFormat) -> bool {
    matches!(
        format,
        PixelFormat::B5G6R5Unorm | PixelFormat::B8G8R8A8Unorm | PixelFormat::B8G8R8A8Srgb
    )
}

fn framebuffer_attachment_type(format: PixelFormat) -> u32 {
    match crate::surface::get_format_type(format) {
        SurfaceType::Depth => gl::DEPTH_ATTACHMENT,
        SurfaceType::Stencil => gl::STENCIL_ATTACHMENT,
        SurfaceType::DepthStencil => gl::DEPTH_STENCIL_ATTACHMENT,
        _ => {
            log::error!(
                "OpenGL::AttachmentType received non-depth/stencil format {:?}",
                format
            );
            gl::NONE
        }
    }
}

fn rescale_attachment_type(format_type: SurfaceType) -> u32 {
    match format_type {
        SurfaceType::ColorTexture => gl::COLOR_ATTACHMENT0,
        SurfaceType::Depth => gl::DEPTH_ATTACHMENT,
        SurfaceType::Stencil => gl::STENCIL_ATTACHMENT,
        SurfaceType::DepthStencil => gl::DEPTH_STENCIL_ATTACHMENT,
        _ => {
            // Eden's ASSERT is fail-soft and returns the colour attachment.
            log::error!("OpenGL::Image::Scale invalid surface type {format_type:?}");
            gl::COLOR_ATTACHMENT0
        }
    }
}

fn rescale_buffer_mask(format_type: SurfaceType) -> u32 {
    match format_type {
        SurfaceType::ColorTexture => gl::COLOR_BUFFER_BIT,
        SurfaceType::Depth => gl::DEPTH_BUFFER_BIT,
        SurfaceType::Stencil => gl::STENCIL_BUFFER_BIT,
        SurfaceType::DepthStencil => gl::DEPTH_BUFFER_BIT | gl::STENCIL_BUFFER_BIT,
        _ => {
            // Eden's ASSERT is fail-soft and returns the colour mask.
            log::error!("OpenGL::Image::Scale invalid surface type {format_type:?}");
            gl::COLOR_BUFFER_BIT
        }
    }
}

fn rescale_fbo_index(format_type: SurfaceType) -> usize {
    match format_type {
        SurfaceType::ColorTexture => 0,
        SurfaceType::Depth => 1,
        SurfaceType::Stencil => 2,
        SurfaceType::DepthStencil => 3,
        _ => {
            // Eden's ASSERT is fail-soft and returns the colour FBO index.
            log::error!("OpenGL::Image::Scale invalid surface type {format_type:?}");
            0
        }
    }
}

/// Port of upstream `IsConverted(const Device&, PixelFormat, ImageType)`.
fn is_converted_image(has_native_astc: bool, format: PixelFormat, image_type: ImageType) -> bool {
    if !has_native_astc && crate::surface::is_pixel_format_astc(format) {
        return true;
    }
    matches!(format, PixelFormat::Bc4Unorm | PixelFormat::Bc5Unorm) && image_type == ImageType::E3D
}

/// Port of upstream `CanBeAccelerated(const TextureCacheRuntime&, const ImageInfo&)`.
fn can_be_accelerated(has_native_astc: bool, info: &ImageInfo) -> bool {
    if crate::surface::is_pixel_format_astc(info.format) && info.size.depth == 1 && !has_native_astc
    {
        return *common::settings::values().accelerate_astc.get_value()
            == common::settings_enums::AstcDecodeMode::Gpu
            && *common::settings::values().astc_recompression.get_value()
                == common::settings_enums::AstcRecompression::Uncompressed;
    }
    false
}

/// Port of upstream `CanBeDecodedAsync(const TextureCacheRuntime&, const ImageInfo&)`.
fn can_be_decoded_async(has_native_astc: bool, info: &ImageInfo) -> bool {
    crate::surface::is_pixel_format_astc(info.format)
        && !has_native_astc
        && *common::settings::values().accelerate_astc.get_value()
            == common::settings_enums::AstcDecodeMode::CpuAsynchronous
}

const GL_COMPRESSED_RGBA_S3TC_DXT1_EXT: u32 = 0x83F1;
const GL_COMPRESSED_RGBA_S3TC_DXT5_EXT: u32 = 0x83F3;
const GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT1_EXT: u32 = 0x8C4D;
const GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT5_EXT: u32 = 0x8C4F;

/// Port of upstream `IsAstcRecompressionEnabled()`.
fn is_astc_recompression_enabled() -> bool {
    *common::settings::values().astc_recompression.get_value()
        != common::settings_enums::AstcRecompression::Uncompressed
}

/// Port of upstream `SelectAstcFormat(PixelFormat format, bool is_srgb)`.
fn select_astc_format(_format: PixelFormat, is_srgb: bool) -> u32 {
    match *common::settings::values().astc_recompression.get_value() {
        common::settings_enums::AstcRecompression::Bc1 => {
            if is_srgb {
                GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT1_EXT
            } else {
                GL_COMPRESSED_RGBA_S3TC_DXT1_EXT
            }
        }
        common::settings_enums::AstcRecompression::Bc3 => {
            if is_srgb {
                GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT5_EXT
            } else {
                GL_COMPRESSED_RGBA_S3TC_DXT5_EXT
            }
        }
        common::settings_enums::AstcRecompression::Uncompressed => {
            if is_srgb {
                gl::SRGB8_ALPHA8
            } else {
                gl::RGBA8
            }
        }
    }
}

/// Number of texture types (1D, 2D, 2DRect, 3D, Cube, 1DArray, 2DArray, Buffer, CubeArray).
const NUM_TEXTURE_TYPES: usize = 9;

/// Format properties for a given GL internal format.
///
/// Corresponds to `OpenGL::FormatProperties`.
#[derive(Clone, Debug, Default)]
pub struct FormatProperties {
    pub compatibility_class: u32,
    pub compatibility_by_size: bool,
    pub is_compressed: bool,
}

/// Format conversion pass using compute shaders.
///
/// Corresponds to `OpenGL::FormatConversionPass`.
pub struct FormatConversionPass {
    /// Upstream: `UtilShaders& util_shaders`.
    util_shaders: NonNull<UtilShaders>,
    intermediate_pbo: OGLBuffer,
    pbo_size: usize,
}

impl FormatConversionPass {
    pub fn new(util_shaders: &mut UtilShaders) -> Self {
        Self {
            util_shaders: NonNull::from(util_shaders),
            intermediate_pbo: OGLBuffer::new(),
            pbo_size: 0,
        }
    }

    /// Port of `FormatConversionPass::ConvertImage`: reinterpret through a
    /// reusable PBO using the source and destination GL formats.
    pub fn convert_image(
        &mut self,
        dst_image: &mut Image,
        src_image: &mut Image,
        copies: &[ImageCopy],
    ) {
        let dst_target = image_target(&dst_image.base().info);
        let src_target = image_target(&src_image.base().info);
        let src_pixel_format = src_image.base().info.format;
        let dst_pixel_format = dst_image.base().info.format;
        let dst_texture = dst_image.handle();
        let src_texture = src_image.handle();
        let dst_format = dst_image.gl_format;
        let dst_type = dst_image.gl_type;
        let src_format = src_image.gl_format;
        let src_type = src_image.gl_type;
        unsafe {
            let img_bpp = crate::surface::bytes_per_block(src_pixel_format);
            for copy in copies {
                let src_origin =
                    make_copy_origin(copy.src_offset, copy.src_subresource, src_target);
                let dst_origin =
                    make_copy_origin(copy.dst_offset, copy.dst_subresource, dst_target);
                let region = make_copy_region(copy.extent, copy.dst_subresource, dst_target);
                let copy_size = (region.width as u32)
                    .wrapping_mul(region.height as u32)
                    .wrapping_mul(region.depth as u32)
                    .wrapping_mul(img_bpp) as usize;
                if self.pbo_size < copy_size {
                    self.intermediate_pbo.create();
                    self.pbo_size = common::bit_util::next_pow2_u32(copy_size as u32) as usize;
                    gl::NamedBufferData(
                        self.intermediate_pbo.handle,
                        self.pbo_size as isize,
                        std::ptr::null(),
                        gl::STREAM_COPY,
                    );
                }

                gl::PixelStorei(gl::PACK_ALIGNMENT, 1);
                gl::PixelStorei(gl::PACK_ROW_LENGTH, copy.extent.width as i32);
                gl::BindBuffer(gl::PIXEL_PACK_BUFFER, self.intermediate_pbo.handle);
                gl::GetTextureSubImage(
                    src_texture,
                    src_origin.level,
                    src_origin.x,
                    src_origin.y,
                    src_origin.z,
                    region.width,
                    region.height,
                    region.depth,
                    src_format,
                    src_type,
                    self.pbo_size as i32,
                    std::ptr::null_mut(),
                );

                gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
                gl::PixelStorei(gl::UNPACK_ROW_LENGTH, copy.extent.width as i32);
                gl::BindBuffer(gl::PIXEL_UNPACK_BUFFER, self.intermediate_pbo.handle);
                gl::TextureSubImage3D(
                    dst_texture,
                    dst_origin.level,
                    dst_origin.x,
                    dst_origin.y,
                    dst_origin.z,
                    region.width,
                    region.height,
                    region.depth,
                    dst_format,
                    dst_type,
                    std::ptr::null(),
                );
            }
        }

        if src_pixel_format == PixelFormat::D24UnormS8Uint
            && dst_pixel_format == PixelFormat::A8B8G8R8Unorm
        {
            // SAFETY: the runtime owns the boxed UtilShaders for longer than
            // this pass, matching upstream's retained reference.
            unsafe { self.util_shaders.as_mut() }.convert_s8d24(dst_image, copies);
        }
    }
}

/// Runtime state for the OpenGL texture cache.
///
/// Corresponds to `OpenGL::TextureCacheRuntime`.
pub struct TextureCacheRuntime {
    // Owning members are declared in effective reverse C++ destruction
    // order. The custom Drop below also releases array elements in reverse.
    rescale_read_fbos: [OGLFramebuffer; 4],
    rescale_draw_fbos: [OGLFramebuffer; 4],
    null_image_view_cube: OGLTextureView,
    null_image_view_2d_array: OGLTextureView,
    null_image_view_2d: OGLTextureView,
    null_image_view_1d: OGLTextureView,
    null_image_3d: OGLTexture,
    null_image_cube_array: OGLTexture,
    null_image_1d_array: OGLTexture,
    format_conversion_pass: FormatConversionPass,
    util_shaders: Box<UtilShaders>,

    pub has_broken_texture_view_formats: bool,
    pub device_access_memory: u64,
    /// Upstream: `const Device& device`.
    /// Production always stores `Some`; context-free unit tests use the
    /// dedicated test capability fields below.
    device: Option<NonNull<super::gl_device::Device>>,
    #[cfg(test)]
    test_can_report_memory_usage: bool,
    #[cfg(test)]
    test_has_native_astc: bool,
    // Upstream stores `StateTracker& state_tracker`. `RasterizerOpenGL` owns
    // the tracker allocation in Rust, and this non-null pointer has the same
    // lifetime as the texture cache runtime field.
    state_tracker: NonNull<StateTracker>,
    staging_buffer_pool: SharedStagingBufferPool,
    format_properties: [HashMap<u32, FormatProperties>; 3],
    null_image_views: [u32; NUM_TEXTURE_TYPES],
    resolution: common::settings::ResolutionScalingInfo,
    #[cfg(test)]
    is_test_stub: bool,
}

impl TextureCacheRuntime {
    /// Create a new texture cache runtime.
    ///
    /// `device_access_memory` mirrors the upstream
    /// `TextureCacheRuntime::TextureCacheRuntime` budget: NVX total +
    /// 512 MiB when the extension is present, else a 2 GiB minimum. Kept
    /// in sync with the buffer-cache runtime (gl_buffer_cache.cpp:139).
    pub fn new(
        device: &super::gl_device::Device,
        program_manager: ProgramManagerHandle,
        state_tracker: &mut StateTracker,
        staging_buffer_pool: SharedStagingBufferPool,
    ) -> Self {
        // Eden initializes these members before entering the constructor body.
        let mut util_shaders = Box::new(UtilShaders::new(program_manager));
        let format_conversion_pass = FormatConversionPass::new(util_shaders.as_mut());
        let resolution = settings::values().resolution_info.clone();
        let format_properties = create_format_properties();
        let has_broken_texture_view_formats = device.has_broken_texture_view_formats();

        let mut null_image_1d_array = OGLTexture::new();
        let mut null_image_cube_array = OGLTexture::new();
        let mut null_image_3d = OGLTexture::new();
        null_image_1d_array.create(gl::TEXTURE_1D_ARRAY);
        null_image_cube_array.create(gl::TEXTURE_CUBE_MAP_ARRAY);
        null_image_3d.create(gl::TEXTURE_3D);
        unsafe {
            gl::TextureStorage2D(null_image_1d_array.handle, 1, gl::R8, 1, 1);
            gl::TextureStorage3D(null_image_cube_array.handle, 1, gl::R8, 1, 1, 6);
            gl::TextureStorage3D(null_image_3d.handle, 1, gl::R8, 1, 1, 1);
        }

        let mut null_image_view_1d = OGLTextureView::new();
        let mut null_image_view_2d = OGLTextureView::new();
        let mut null_image_view_2d_array = OGLTextureView::new();
        let mut null_image_view_cube = OGLTextureView::new();
        let mut new_handles = [0u32; 4];
        unsafe {
            gl::GenTextures(4, new_handles.as_mut_ptr());
        }
        null_image_view_1d.handle = new_handles[0];
        null_image_view_2d.handle = new_handles[1];
        null_image_view_2d_array.handle = new_handles[2];
        null_image_view_cube.handle = new_handles[3];
        unsafe {
            gl::TextureView(
                null_image_view_1d.handle,
                gl::TEXTURE_1D,
                null_image_1d_array.handle,
                gl::R8,
                0,
                1,
                0,
                1,
            );
            gl::TextureView(
                null_image_view_2d.handle,
                gl::TEXTURE_2D,
                null_image_cube_array.handle,
                gl::R8,
                0,
                1,
                0,
                1,
            );
            gl::TextureView(
                null_image_view_2d_array.handle,
                gl::TEXTURE_2D_ARRAY,
                null_image_cube_array.handle,
                gl::R8,
                0,
                1,
                0,
                1,
            );
            gl::TextureView(
                null_image_view_cube.handle,
                gl::TEXTURE_CUBE_MAP,
                null_image_cube_array.handle,
                gl::R8,
                0,
                1,
                0,
                6,
            );
            const NULL_SWIZZLE: [i32; 4] = [gl::ZERO as i32; 4];
            for handle in [
                null_image_1d_array.handle,
                null_image_cube_array.handle,
                null_image_3d.handle,
                null_image_view_1d.handle,
                null_image_view_2d.handle,
                null_image_view_2d_array.handle,
                null_image_view_cube.handle,
            ] {
                gl::TextureParameteriv(handle, gl::TEXTURE_SWIZZLE_RGBA, NULL_SWIZZLE.as_ptr());
            }
        }

        let mut null_image_views = [0u32; NUM_TEXTURE_TYPES];
        let mut set_view = |texture_type: TextureType, handle: u32| {
            if device.has_debugging_tool_attached() {
                let name = format!("NullImage {texture_type:?}");
                unsafe {
                    gl::ObjectLabel(gl::TEXTURE, handle, name.len() as i32, name.as_ptr().cast());
                }
            }
            null_image_views[texture_type as usize] = handle;
        };
        set_view(TextureType::Color1D, null_image_view_1d.handle);
        set_view(TextureType::Color2D, null_image_view_2d.handle);
        set_view(TextureType::ColorCube, null_image_view_cube.handle);
        set_view(TextureType::Color3D, null_image_3d.handle);
        set_view(TextureType::ColorArray1D, null_image_1d_array.handle);
        set_view(TextureType::ColorArray2D, null_image_view_2d_array.handle);
        set_view(TextureType::ColorArrayCube, null_image_cube_array.handle);
        set_view(TextureType::Color2DRect, null_image_view_2d.handle);

        let mut rescale_draw_fbos = std::array::from_fn(|_| OGLFramebuffer::new());
        let mut rescale_read_fbos = std::array::from_fn(|_| OGLFramebuffer::new());
        if resolution.active {
            for index in 0..rescale_draw_fbos.len() {
                rescale_draw_fbos[index].create();
                rescale_read_fbos[index].create();
            }
        }

        const HALF_GIB: u64 = 512 * 1024 * 1024;
        let device_access_memory = if device.can_report_memory_usage() {
            device.get_current_dedicated_video_memory() + HALF_GIB
        } else {
            2 * 1024 * 1024 * 1024
        };

        Self {
            rescale_read_fbos,
            rescale_draw_fbos,
            null_image_view_cube,
            null_image_view_2d_array,
            null_image_view_2d,
            null_image_view_1d,
            null_image_3d,
            null_image_cube_array,
            null_image_1d_array,
            format_conversion_pass,
            util_shaders,
            has_broken_texture_view_formats,
            device_access_memory,
            device: Some(NonNull::from(device)),
            #[cfg(test)]
            test_can_report_memory_usage: device.can_report_memory_usage(),
            #[cfg(test)]
            test_has_native_astc: device.has_astc(),
            state_tracker: NonNull::from(state_tracker),
            staging_buffer_pool,
            format_properties,
            null_image_views,
            resolution,
            #[cfg(test)]
            is_test_stub: false,
        }
    }

    #[cfg(test)]
    fn new_for_test(
        has_broken_texture_view_formats: bool,
        has_native_astc: bool,
        program_manager: ProgramManagerHandle,
        state_tracker: &mut StateTracker,
        staging_buffer_pool: SharedStagingBufferPool,
    ) -> Self {
        let mut util_shaders = Box::new(UtilShaders::new_for_test(program_manager));
        let format_conversion_pass = FormatConversionPass::new(util_shaders.as_mut());
        Self {
            rescale_read_fbos: std::array::from_fn(|_| OGLFramebuffer::new()),
            rescale_draw_fbos: std::array::from_fn(|_| OGLFramebuffer::new()),
            null_image_view_cube: OGLTextureView::new(),
            null_image_view_2d_array: OGLTextureView::new(),
            null_image_view_2d: OGLTextureView::new(),
            null_image_view_1d: OGLTextureView::new(),
            null_image_3d: OGLTexture::new(),
            null_image_cube_array: OGLTexture::new(),
            null_image_1d_array: OGLTexture::new(),
            format_conversion_pass,
            util_shaders,
            has_broken_texture_view_formats,
            device_access_memory: 2 * 1024 * 1024 * 1024,
            device: None,
            test_can_report_memory_usage: false,
            test_has_native_astc: has_native_astc,
            state_tracker: NonNull::from(state_tracker),
            staging_buffer_pool,
            format_properties: std::array::from_fn(|_| HashMap::new()),
            null_image_views: [0; NUM_TEXTURE_TYPES],
            resolution: common::settings::ResolutionScalingInfo::default(),
            is_test_stub: true,
        }
    }

    fn device(&self) -> &super::gl_device::Device {
        // SAFETY: production `TextureCacheRuntime` is owned by the
        // rasterizer while the renderer-owned Device outlives it, matching
        // upstream's retained `const Device&`.
        unsafe {
            self.device
                .expect("production OpenGL TextureCacheRuntime requires Device")
                .as_ref()
        }
    }

    pub fn finish(&self) {
        unsafe {
            gl::Finish();
        }
    }

    pub fn upload_staging_buffer(&mut self, size: usize, _deferred: bool) -> StagingBufferMap {
        self.staging_buffer_pool.lock().request_upload_buffer(size)
    }

    pub fn download_staging_buffer(&mut self, size: usize, deferred: bool) -> StagingBufferMap {
        self.staging_buffer_pool
            .lock()
            .request_download_buffer(size, deferred)
    }

    pub fn free_deferred_staging_buffer(&mut self, buffer: &StagingBufferMap) {
        self.staging_buffer_pool
            .lock()
            .free_deferred_staging_buffer(buffer);
    }

    pub fn blit_framebuffer(
        &mut self,
        dst_framebuffer: u32,
        src_framebuffer: u32,
        dst_buffer_bits: u32,
        src_buffer_bits: u32,
        dst_region: Region2D,
        src_region: Region2D,
        filter: crate::engines::fermi_2d::Filter,
        _operation: crate::engines::fermi_2d::Operation,
    ) {
        let state_tracker = unsafe { self.state_tracker.as_mut() };
        state_tracker.notify_scissor0();
        state_tracker.notify_rasterize_enable();
        state_tracker.notify_framebuffer_srgb();

        if dst_buffer_bits != src_buffer_bits {
            // Eden's ASSERT is fail-soft in production.
            log::error!(
                "OpenGL::TextureCacheRuntime::BlitFramebuffer buffer bits differ: dst=0x{dst_buffer_bits:x}, src=0x{src_buffer_bits:x}"
            );
        }
        let buffer_bits = dst_buffer_bits;
        let has_depth = (buffer_bits & !gl::COLOR_BUFFER_BIT) != 0;
        let is_linear = !has_depth && filter == crate::engines::fermi_2d::Filter::Bilinear;
        let gl_filter = if is_linear { gl::LINEAR } else { gl::NEAREST };
        unsafe {
            gl::Enable(gl::FRAMEBUFFER_SRGB);
            gl::Disable(gl::RASTERIZER_DISCARD);
            gl::Disablei(gl::SCISSOR_TEST, 0);
            gl::BlitNamedFramebuffer(
                src_framebuffer,
                dst_framebuffer,
                src_region.start.x,
                src_region.start.y,
                src_region.end.x,
                src_region.end.y,
                dst_region.start.x,
                dst_region.start.y,
                dst_region.end.x,
                dst_region.end.y,
                buffer_bits,
                gl_filter,
            );
        }
    }

    pub fn copy_image(&self, dst_image: &Image, src_image: &Image, copies: &[ImageCopy]) {
        let dst_name = dst_image.handle();
        let src_name = src_image.handle();
        let dst_target = image_target(&dst_image.base().info);
        let src_target = image_target(&src_image.base().info);
        unsafe {
            for copy in copies {
                let src_origin =
                    make_copy_origin(copy.src_offset, copy.src_subresource, src_target);
                let dst_origin =
                    make_copy_origin(copy.dst_offset, copy.dst_subresource, dst_target);
                let region = make_copy_region(copy.extent, copy.dst_subresource, dst_target);
                gl::CopyImageSubData(
                    src_name,
                    src_target,
                    src_origin.level,
                    src_origin.x,
                    src_origin.y,
                    src_origin.z,
                    dst_name,
                    dst_target,
                    dst_origin.level,
                    dst_origin.x,
                    dst_origin.y,
                    dst_origin.z,
                    region.width,
                    region.height,
                    region.depth,
                );
            }
        }
    }

    pub fn copy_image_msaa(
        &mut self,
        dst_image: &mut Image,
        src_image: &mut Image,
        copies: &[ImageCopy],
    ) {
        log::debug!(
            "Copying from {} samples to {} samples",
            src_image.base().info.num_samples,
            dst_image.base().info.num_samples,
        );
        self.util_shaders.copy_msaa(dst_image, src_image, copies);
    }

    pub fn can_image_be_copied(&self, dst: &Image, src: &Image) -> bool {
        if dst.base().info.image_type == ImageType::E3D
            && dst.base().info.format == crate::surface::PixelFormat::Bc4Unorm
        {
            return false;
        }
        if is_pixel_format_bgr(dst.base().info.format)
            != is_pixel_format_bgr(src.base().info.format)
        {
            return false;
        }
        true
    }

    pub fn reinterpret_image(
        &mut self,
        dst_image: &mut Image,
        src_image: &mut Image,
        copies: &[ImageCopy],
    ) {
        log::debug!(
            "Converting {:?} to {:?}",
            src_image.base().info.format,
            dst_image.base().info.format,
        );
        self.format_conversion_pass
            .convert_image(dst_image, src_image, copies);
    }

    /// Port of Eden's inline `TextureCacheRuntime::ConvertImage` overload.
    /// That API is intentionally unimplemented upstream and reports through
    /// the fail-soft assertion path.
    pub fn convert_image(
        &mut self,
        _dst: Option<&mut TextureCacheFramebuffer>,
        _dst_view: &mut ImageView,
        _src_view: &mut ImageView,
    ) {
        log::error!("OpenGL::TextureCacheRuntime::ConvertImage is unimplemented");
    }

    pub fn emulate_copy_image(
        &mut self,
        dst_image: &mut Image,
        src_image: &mut Image,
        copies: &[ImageCopy],
    ) {
        if dst_image.base().info.image_type == ImageType::E3D
            && dst_image.base().info.format == crate::surface::PixelFormat::Bc4Unorm
        {
            if src_image.base().info.image_type != ImageType::E3D {
                log::error!(
                    "TextureCacheRuntime::EmulateCopyImage expected a 3D source, got {:?}",
                    src_image.base().info.image_type,
                );
            }
            self.util_shaders.copy_bc4(dst_image, src_image, copies);
            return;
        }
        if is_pixel_format_bgr(dst_image.base().info.format)
            || is_pixel_format_bgr(src_image.base().info.format)
        {
            self.reinterpret_image(dst_image, src_image, copies);
            return;
        }
        log::error!(
            "TextureCacheRuntime::EmulateCopyImage has no implementation for {:?} -> {:?}",
            src_image.base().info.format,
            dst_image.base().info.format,
        );
    }

    pub fn get_device_local_memory(&self) -> u64 {
        self.device_access_memory
    }

    /// Port of `TextureCacheRuntime::GetDeviceMemoryUsage`. Mirrors the
    /// buffer-cache port: queries `TOTAL_AVAILABLE_MEMORY_NVX` (0x9048),
    /// the same constant upstream uses; subtraction stays non-negative
    /// because the ctor sized `device_access_memory` to `total + 512MiB`.
    pub fn get_device_memory_usage(&self) -> u64 {
        if !self.can_report_memory_usage() {
            return 2 * 1024 * 1024 * 1024;
        }
        self.device_access_memory
            .wrapping_sub(self.device().get_current_dedicated_video_memory())
    }

    pub fn can_report_memory_usage(&self) -> bool {
        if let Some(device) = self.device {
            // SAFETY: see `device()`.
            return unsafe { device.as_ref() }.can_report_memory_usage();
        }
        #[cfg(test)]
        {
            return self.test_can_report_memory_usage;
        }
        #[cfg(not(test))]
        unreachable!("production OpenGL TextureCacheRuntime requires Device")
    }

    pub fn should_reinterpret(&self, _dst: &Image, _src: &Image) -> bool {
        true
    }

    pub fn can_upload_msaa(&self) -> bool {
        true
    }

    pub fn format_info(&self, image_type: ImageType, internal_format: u32) -> FormatProperties {
        let table_index = match image_type {
            ImageType::E1D => 0,
            ImageType::E2D | ImageType::Linear => 1,
            ImageType::E3D => 2,
            _ => {
                // Eden's ASSERT is fail-soft in production.
                log::error!(
                    "OpenGL::TextureCacheRuntime::FormatInfo unsupported image type {image_type:?}"
                );
                return FormatProperties::default();
            }
        };
        self.format_properties[table_index]
            .get(&internal_format)
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "OpenGL format-properties table has no entry for internal format 0x{internal_format:x}"
                )
            })
    }

    pub fn has_native_bgr(&self) -> bool {
        false
    }

    pub fn has_broken_texture_view_formats(&self) -> bool {
        self.has_broken_texture_view_formats
    }

    /// Port of `TextureCacheRuntime::HasNativeASTC`.
    pub fn has_native_astc(&self) -> bool {
        if let Some(device) = self.device {
            // SAFETY: see `device()`.
            return unsafe { device.as_ref() }.has_astc();
        }
        #[cfg(test)]
        {
            return self.test_has_native_astc;
        }
        #[cfg(not(test))]
        unreachable!("production OpenGL TextureCacheRuntime requires Device")
    }

    fn has_debugging_tool_attached(&self) -> bool {
        self.device
            .map(|device| {
                // SAFETY: see `device()`.
                unsafe { device.as_ref() }.has_debugging_tool_attached()
            })
            .unwrap_or(false)
    }

    pub fn insert_upload_memory_barrier(&self) {
        unsafe {
            gl::MemoryBarrier(gl::TEXTURE_FETCH_BARRIER_BIT | gl::SHADER_IMAGE_ACCESS_BARRIER_BIT);
        }
    }

    pub fn transition_image_layout(&self, _image: &ImageBase) {}

    pub fn tick_frame(&self) {}

    pub fn barrier_feedback_loop(&self) {}

    pub fn get_state_tracker(&mut self) -> &mut StateTracker {
        // SAFETY: the renderer-owned tracker outlives this runtime, exactly
        // like Eden's retained `StateTracker&`.
        unsafe { self.state_tracker.as_mut() }
    }

    pub fn accelerate_image_upload(
        &mut self,
        image: &mut Image,
        map: &StagingBufferMap,
        swizzles: &[crate::texture_cache::types::SwizzleParameters],
        _z_start: u32,
        _z_count: u32,
    ) {
        let info = image.base().info.clone();
        match info.image_type {
            ImageType::E2D => {
                if crate::surface::is_pixel_format_astc(info.format) {
                    self.util_shaders.astc_decode(image, map, swizzles);
                } else {
                    self.util_shaders
                        .block_linear_upload_2d(image, map, swizzles);
                }
            }
            ImageType::E3D => self
                .util_shaders
                .block_linear_upload_3d(image, map, swizzles),
            ImageType::Linear => self.util_shaders.pitch_upload(image, map, swizzles),
            _ => log::error!(
                "TextureCacheRuntime::accelerate_image_upload unsupported image type {:?}",
                info.image_type
            ),
        }
    }
}

impl Drop for TextureCacheRuntime {
    fn drop(&mut self) {
        for framebuffer in self.rescale_read_fbos.iter_mut().rev() {
            framebuffer.release();
        }
        for framebuffer in self.rescale_draw_fbos.iter_mut().rev() {
            framebuffer.release();
        }
        self.null_image_view_cube.release();
        self.null_image_view_2d_array.release();
        self.null_image_view_2d.release();
        self.null_image_view_1d.release();
        self.null_image_3d.release();
        self.null_image_cube_array.release();
        self.null_image_1d_array.release();
    }
}

/// Port of upstream `OpenGL::ImageTarget(const VideoCommon::ImageInfo& info)`
/// (gl_texture_cache.cpp:70-88). Maps ruzu `ImageType` to the GL target
/// that the corresponding texture object lives under. Note that 1D and 2D
/// non-multisampled images map to the *_ARRAY targets — upstream uses
/// `GL_TEXTURE_1D_ARRAY` / `GL_TEXTURE_2D_ARRAY` even for layer-count=1
/// images so the same target works for arrays without re-allocation.
fn image_target(info: &ImageInfo) -> u32 {
    match info.image_type {
        ImageType::E1D => gl::TEXTURE_1D_ARRAY,
        ImageType::E2D => {
            if info.num_samples > 1 {
                gl::TEXTURE_2D_MULTISAMPLE_ARRAY
            } else {
                gl::TEXTURE_2D_ARRAY
            }
        }
        ImageType::E3D => gl::TEXTURE_3D,
        // Upstream: `case ImageType::Linear: return GL_TEXTURE_2D_ARRAY;`
        ImageType::Linear => gl::TEXTURE_2D_ARRAY,
        ImageType::Buffer => gl::TEXTURE_BUFFER,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CopyOrigin {
    level: i32,
    x: i32,
    y: i32,
    z: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CopyRegion {
    width: i32,
    height: i32,
    depth: i32,
}

/// Port of upstream `MakeCopyOrigin` (gl_texture_cache.cpp:278).
fn make_copy_origin(
    offset: crate::texture_cache::types::Offset3D,
    subresource: crate::texture_cache::types::SubresourceLayers,
    target: u32,
) -> CopyOrigin {
    match target {
        gl::TEXTURE_1D => CopyOrigin {
            level: subresource.base_level,
            x: offset.x,
            y: 0,
            z: 0,
        },
        gl::TEXTURE_1D_ARRAY => CopyOrigin {
            level: subresource.base_level,
            x: offset.x,
            y: 0,
            z: subresource.base_layer,
        },
        gl::TEXTURE_2D_ARRAY | gl::TEXTURE_2D_MULTISAMPLE_ARRAY => CopyOrigin {
            level: subresource.base_level,
            x: offset.x,
            y: offset.y,
            z: subresource.base_layer,
        },
        gl::TEXTURE_3D => CopyOrigin {
            level: subresource.base_level,
            x: offset.x,
            y: offset.y,
            z: offset.z,
        },
        _ => {
            // Eden's `UNIMPLEMENTED_MSG` reports and then returns the
            // zeroed origin from the following statement.
            log::error!("gl_texture_cache::make_copy_origin: unhandled target=0x{target:x}");
            CopyOrigin {
                level: 0,
                x: 0,
                y: 0,
                z: 0,
            }
        }
    }
}

/// Port of upstream `MakeCopyRegion` (gl_texture_cache.cpp:313).
fn make_copy_region(
    extent: crate::texture_cache::types::Extent3D,
    dst_subresource: crate::texture_cache::types::SubresourceLayers,
    target: u32,
) -> CopyRegion {
    match target {
        gl::TEXTURE_1D => CopyRegion {
            width: extent.width as i32,
            height: 1,
            depth: 1,
        },
        gl::TEXTURE_1D_ARRAY => CopyRegion {
            width: extent.width as i32,
            height: 1,
            depth: dst_subresource.num_layers,
        },
        gl::TEXTURE_2D_ARRAY | gl::TEXTURE_2D_MULTISAMPLE_ARRAY => CopyRegion {
            width: extent.width as i32,
            height: extent.height as i32,
            depth: dst_subresource.num_layers,
        },
        gl::TEXTURE_3D => CopyRegion {
            width: extent.width as i32,
            height: extent.height as i32,
            depth: extent.depth as i32,
        },
        _ => {
            // Eden's `UNIMPLEMENTED_MSG` reports and then returns the
            // zeroed region from the following statement.
            log::error!("gl_texture_cache::make_copy_region: unhandled target=0x{target:x}");
            CopyRegion {
                width: 0,
                height: 0,
                depth: 0,
            }
        }
    }
}

/// Port of upstream `OpenGL::ImageTarget(Shader::TextureType, int)`
/// (gl_texture_cache.cpp:90-113). Maps a shader texture view type to the
/// GL target used for `glTextureView`, including multisample targets.
fn image_view_target(view_type: TextureType, num_samples: i32) -> u32 {
    let is_multisampled = num_samples > 1;
    match view_type {
        TextureType::Color1D => gl::TEXTURE_1D,
        TextureType::Color2D | TextureType::Color2DRect => {
            if is_multisampled {
                gl::TEXTURE_2D_MULTISAMPLE
            } else {
                gl::TEXTURE_2D
            }
        }
        TextureType::ColorCube => gl::TEXTURE_CUBE_MAP,
        TextureType::Color3D => gl::TEXTURE_3D,
        TextureType::ColorArray1D => gl::TEXTURE_1D_ARRAY,
        TextureType::ColorArray2D => {
            if is_multisampled {
                gl::TEXTURE_2D_MULTISAMPLE_ARRAY
            } else {
                gl::TEXTURE_2D_ARRAY
            }
        }
        TextureType::ColorArrayCube => gl::TEXTURE_CUBE_MAP_ARRAY,
        TextureType::Buffer => gl::TEXTURE_BUFFER,
    }
}

fn swizzle(source: SwizzleSource) -> i32 {
    match source {
        SwizzleSource::Zero => gl::ZERO as i32,
        SwizzleSource::R => gl::RED as i32,
        SwizzleSource::G => gl::GREEN as i32,
        SwizzleSource::B => gl::BLUE as i32,
        SwizzleSource::A => gl::ALPHA as i32,
        SwizzleSource::OneInt | SwizzleSource::OneFloat => gl::ONE as i32,
    }
}

fn convert_green_red(value: SwizzleSource) -> SwizzleSource {
    match value {
        SwizzleSource::G => SwizzleSource::R,
        _ => value,
    }
}

fn convert_a5b5g5r1_unorm(source: SwizzleSource) -> i32 {
    match source {
        SwizzleSource::Zero => gl::ZERO as i32,
        SwizzleSource::R => gl::ALPHA as i32,
        SwizzleSource::G => gl::BLUE as i32,
        SwizzleSource::B => gl::GREEN as i32,
        SwizzleSource::A => gl::RED as i32,
        SwizzleSource::OneInt | SwizzleSource::OneFloat => gl::ONE as i32,
    }
}

fn depth_stencil_texture_mode(format: PixelFormat, swizzle: [SwizzleSource; 4]) -> u32 {
    if !matches!(swizzle[0], SwizzleSource::R | SwizzleSource::G) {
        // Eden's UNIMPLEMENTED_IF reports and continues.
        log::error!(
            "ApplySwizzle depth/stencil swizzle[0] is unimplemented: {:?}",
            swizzle[0]
        );
    }
    let any_r = swizzle.iter().any(|source| *source == SwizzleSource::R);
    match format {
        PixelFormat::D24UnormS8Uint | PixelFormat::D32FloatS8Uint => {
            if any_r {
                gl::DEPTH_COMPONENT
            } else {
                gl::STENCIL_INDEX
            }
        }
        PixelFormat::S8UintD24Unorm => {
            if any_r {
                gl::STENCIL_INDEX
            } else {
                gl::DEPTH_COMPONENT
            }
        }
        _ => {
            // Eden's ASSERT returns GL_DEPTH_COMPONENT after reporting.
            log::error!("ApplySwizzle invalid depth/stencil format: {format:?}");
            gl::DEPTH_COMPONENT
        }
    }
}

fn apply_swizzle(handle: u32, format: PixelFormat, mut source_swizzle: [SwizzleSource; 4]) {
    unsafe {
        match format {
            PixelFormat::D24UnormS8Uint
            | PixelFormat::D32FloatS8Uint
            | PixelFormat::S8UintD24Unorm => {
                gl::TextureParameteri(
                    handle,
                    gl::DEPTH_STENCIL_TEXTURE_MODE,
                    depth_stencil_texture_mode(format, source_swizzle) as i32,
                );
                source_swizzle = source_swizzle.map(convert_green_red);
            }
            PixelFormat::A5B5G5R1Unorm => {
                let gl_swizzle = source_swizzle.map(convert_a5b5g5r1_unorm);
                gl::TextureParameteriv(handle, gl::TEXTURE_SWIZZLE_RGBA, gl_swizzle.as_ptr());
                return;
            }
            _ => {}
        }
        let gl_swizzle = source_swizzle.map(swizzle);
        gl::TextureParameteriv(handle, gl::TEXTURE_SWIZZLE_RGBA, gl_swizzle.as_ptr());
    }
}

fn decode_swizzle(raw: [u8; 4]) -> Option<[SwizzleSource; 4]> {
    fn decode(value: u8) -> Option<SwizzleSource> {
        match value {
            0 => Some(SwizzleSource::Zero),
            2 => Some(SwizzleSource::R),
            3 => Some(SwizzleSource::G),
            4 => Some(SwizzleSource::B),
            5 => Some(SwizzleSource::A),
            6 => Some(SwizzleSource::OneInt),
            7 => Some(SwizzleSource::OneFloat),
            _ => None,
        }
    }
    Some([
        decode(raw[0])?,
        decode(raw[1])?,
        decode(raw[2])?,
        decode(raw[3])?,
    ])
}

/// Port of upstream `OpenGL::MakeImage` (gl_texture_cache.cpp:363-406).
/// Allocates a GL texture sized to `info` via `OGLTexture::create` +
/// `glTextureStorage{2,3}D(...)`.
///
/// MULTISAMPLE_ARRAY uses `glTextureStorage3DMultisample` with the
/// effective per-sample width/height (`width >> samples_x` etc.) via the
/// shared `texture_cache::samples_helper::samples_log2` port.
fn make_image(info: &ImageInfo, gl_internal_format: u32, gl_num_levels: i32) -> OGLTexture {
    // Upstream uses GLsizei (i32) here; ruzu stores extents as u32. Casts
    // are lossless for realistic texture sizes (<= 16384). No defensive
    // `.max(1)` — upstream lets the GL driver reject invalid extents.
    let width = info.size.width as i32;
    let height = info.size.height as i32;
    let depth = info.size.depth as i32;
    let num_layers = info.resources.layers;
    let num_samples = info.num_samples as i32;

    let target = image_target(info);
    let mut texture = OGLTexture::new();
    let mut handle = 0;
    if target != gl::TEXTURE_BUFFER {
        texture.create(target);
        handle = texture.handle;
    }
    unsafe {
        match target {
            gl::TEXTURE_1D_ARRAY => {
                gl::TextureStorage2D(handle, gl_num_levels, gl_internal_format, width, num_layers);
            }
            gl::TEXTURE_2D_ARRAY => {
                gl::TextureStorage3D(
                    handle,
                    gl_num_levels,
                    gl_internal_format,
                    width,
                    height,
                    num_layers,
                );
            }
            gl::TEXTURE_2D_MULTISAMPLE_ARRAY => {
                // Upstream calls `SamplesLog2(info.num_samples)` from
                // video_core/texture_cache/samples_helper.h. The Rust port
                // lives at `texture_cache::samples_helper::samples_log2`.
                let (samples_x, samples_y) =
                    crate::texture_cache::samples_helper::samples_log2(num_samples);
                gl::TextureStorage3DMultisample(
                    handle,
                    num_samples,
                    gl_internal_format,
                    width >> samples_x,
                    height >> samples_y,
                    num_layers,
                    gl::FALSE,
                );
            }
            gl::TEXTURE_RECTANGLE => {
                gl::TextureStorage2D(handle, gl_num_levels, gl_internal_format, width, height);
            }
            gl::TEXTURE_3D => {
                gl::TextureStorage3D(
                    handle,
                    gl_num_levels,
                    gl_internal_format,
                    width,
                    height,
                    depth,
                );
            }
            gl::TEXTURE_BUFFER => {
                // Eden's ASSERT is fail-soft in production and returns the
                // zero-handle OGLTexture constructed above.
                log::error!("OpenGL::MakeImage called for GL_TEXTURE_BUFFER");
            }
            _ => {
                log::error!("OpenGL::MakeImage invalid target=0x{target:x}");
            }
        }
    }
    texture
}

/// An OpenGL texture/image.
///
/// Corresponds to `OpenGL::Image`.
pub struct Image {
    // C++ destroys derived members in reverse declaration order. Rust drops
    // struct fields in declaration order, so owning GL fields are listed in
    // the effective C++ destruction order: store view, backup, texture.
    pub store_view: OGLTextureView,
    pub upscaled_backup: OGLTexture,
    pub texture: OGLTexture,
    /// Stable address of the `ImageBase` subobject owned by the typed slot.
    /// Upstream stores this state through C++ inheritance; the boxed slot base
    /// gives the Rust payload the same single source of truth.
    base: NonNull<ImageBase>,
    /// Upstream: `TextureCacheRuntime* runtime`.
    /// Production images always retain the runtime that constructed them;
    /// context-free unit-test images leave it empty and never execute GL
    /// operations requiring the runtime.
    runtime: Option<NonNull<TextureCacheRuntime>>,
    pub gl_internal_format: u32,
    pub gl_format: u32,
    pub gl_type: u32,
    pub gl_num_levels: i32,
    pub current_texture: u32,
    pub allocation_tick: u64,
}

impl Image {
    #[cfg(test)]
    pub fn new(base: &mut ImageBase) -> Self {
        Self {
            store_view: OGLTextureView::new(),
            upscaled_backup: OGLTexture::new(),
            texture: OGLTexture::new(),
            base: NonNull::from(base),
            runtime: None,
            gl_internal_format: gl::NONE,
            gl_format: gl::NONE,
            gl_type: gl::NONE,
            gl_num_levels: 0,
            current_texture: 0,
            allocation_tick: 0,
        }
    }

    pub(super) fn base(&self) -> &ImageBase {
        // SAFETY: the backend payload is dropped before its containing slot;
        // the base lives in a Box, so SlotVector/ring moves cannot relocate it.
        unsafe { self.base.as_ref() }
    }

    pub fn handle(&self) -> u32 {
        self.current_texture
    }

    #[cfg(test)]
    fn matches_base(&self, base: &ImageBase) -> bool {
        std::ptr::eq(self.base(), base)
    }

    /// Port of `OpenGL::Image::Image(TextureCacheRuntime&, const VideoCommon::ImageInfo&,
    /// GPUVAddr, VAddr)` (gl_texture_cache.cpp:692-728).
    ///
    /// Allocates the backing GL texture via `MakeImage` (`image_target` +
    /// `glTextureStorage*D` per dimensionality), including upstream's
    /// converted-format retargeting for ASTC-without-native-support and
    /// BC4/BC5 3D images.
    pub fn from_base(base: NonNull<ImageBase>, runtime: &mut TextureCacheRuntime) -> Self {
        // SAFETY: construction is performed from the boxed base of the slot
        // that will immediately own this backend payload.
        let base_ref = unsafe { base.as_ref() };
        let converted = is_converted_image(
            runtime.has_native_astc(),
            base_ref.info.format,
            base_ref.info.image_type,
        );
        let (gl_internal_format, gl_format, gl_type) = if converted {
            let is_srgb = crate::surface::is_pixel_format_srgb(base_ref.info.format);
            let mut internal_format = if is_srgb { gl::SRGB8_ALPHA8 } else { gl::RGBA8 };
            let mut format = gl::RGBA;
            if crate::surface::is_pixel_format_astc(base_ref.info.format)
                && is_astc_recompression_enabled()
            {
                internal_format = select_astc_format(base_ref.info.format, is_srgb);
                format = gl::NONE;
            }
            (internal_format, format, gl::UNSIGNED_INT_8_8_8_8_REV)
        } else {
            let tuple = super::maxwell_to_gl::get_format_tuple(base_ref.info.format);
            (tuple.internal_format, tuple.format, tuple.gl_type)
        };
        // Upstream: `gl_num_levels = std::min(info.resources.levels,
        //                                     std::bit_width(info.size.width));`
        // bit_width(x) = floor(log2(x)) + 1 = 32 - x.leading_zeros() for u32.
        let max_host_mip_levels = if base_ref.info.size.width == 0 {
            0
        } else {
            32 - base_ref.info.size.width.leading_zeros() as i32
        };
        let gl_num_levels = base_ref.info.resources.levels.min(max_host_mip_levels);

        let target = image_target(&base_ref.info);
        let texture = make_image(&base_ref.info, gl_internal_format, gl_num_levels);
        let current_texture = texture.handle;

        if runtime.device.is_some() && runtime.device().has_debugging_tool_attached() {
            let name = crate::texture_cache::formatter::image_name(base_ref);
            unsafe {
                gl::ObjectLabel(
                    if target == gl::TEXTURE_BUFFER {
                        gl::BUFFER
                    } else {
                        gl::TEXTURE
                    },
                    texture.handle,
                    name.len() as i32,
                    name.as_ptr().cast(),
                );
            }
        }

        Self {
            store_view: OGLTextureView::new(),
            upscaled_backup: OGLTexture::new(),
            texture,
            base,
            runtime: Some(NonNull::from(runtime)),
            gl_internal_format,
            gl_format,
            gl_type,
            gl_num_levels,
            current_texture,
            allocation_tick: 0,
        }
    }

    /// Port of `Image::StorageHandle`.
    ///
    /// OpenGL image load/store cannot bind an sRGB image with `GL_RGBA8` as
    /// the access format directly. Upstream therefore creates a linear RGBA8
    /// view of the same storage and caches it for accelerated uploads.
    pub fn storage_handle(&mut self) -> u32 {
        match self.base().info.format {
            PixelFormat::A8B8G8R8Srgb
            | PixelFormat::B8G8R8A8Srgb
            | PixelFormat::Bc1RgbaSrgb
            | PixelFormat::Bc2Srgb
            | PixelFormat::Bc3Srgb
            | PixelFormat::Bc7Srgb
            | PixelFormat::Astc2d4x4Srgb
            | PixelFormat::Astc2d8x8Srgb
            | PixelFormat::Astc2d8x5Srgb
            | PixelFormat::Astc2d5x4Srgb
            | PixelFormat::Astc2d5x5Srgb
            | PixelFormat::Astc2d10x5Srgb
            | PixelFormat::Astc2d10x6Srgb
            | PixelFormat::Astc2d10x8Srgb
            | PixelFormat::Astc2d6x6Srgb
            | PixelFormat::Astc2d10x10Srgb
            | PixelFormat::Astc2d12x10Srgb
            | PixelFormat::Astc2d12x12Srgb
            | PixelFormat::Astc2d8x6Srgb
            | PixelFormat::Astc2d6x5Srgb => {
                if self.store_view.handle != 0 {
                    return self.store_view.handle;
                }
                unsafe {
                    self.store_view.create();
                    gl::TextureView(
                        self.store_view.handle,
                        image_target(&self.base().info),
                        self.current_texture,
                        gl::RGBA8,
                        0,
                        self.gl_num_levels as u32,
                        0,
                        self.base().info.resources.layers as u32,
                    );
                }
                self.store_view.handle
            }
            _ => self.current_texture,
        }
    }

    /// Port of `Image::UploadMemory(GLuint buffer_handle, size_t
    /// buffer_offset, std::span<const BufferImageCopy>)`
    /// (gl_texture_cache.cpp:734-765).
    ///
    /// Binds the supplied PBO as the unpack source, flushes the mapped
    /// range so the driver sees the staging writes, then walks `copies`
    /// applying per-slice `glTextureSubImage{2,3}D` (or compressed
    /// variant when `gl_format == GL_NONE`). The pixel-store state for
    /// `GL_UNPACK_ROW_LENGTH` / `GL_UNPACK_IMAGE_HEIGHT` is updated
    /// only when it changes, mirroring upstream's cached comparison.
    ///
    pub fn upload_memory(
        &mut self,
        base: &mut ImageBase,
        buffer_handle: u32,
        buffer_offset: usize,
        copies: &[crate::texture_cache::types::BufferImageCopy],
    ) {
        let is_rescaled = base.flags.contains(ImageFlagBits::RESCALED);
        if is_rescaled {
            self.scale_down(base, true);
        }
        unsafe {
            gl::BindBuffer(gl::PIXEL_UNPACK_BUFFER, buffer_handle);
            gl::FlushMappedBufferRange(
                gl::PIXEL_UNPACK_BUFFER,
                buffer_offset as isize,
                base.unswizzled_size_bytes as isize,
            );
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
        }
        let mut current_row_length: u32 = u32::MAX;
        let mut current_image_height: u32 = u32::MAX;
        for copy in copies {
            if copy.image_subresource.base_level >= self.gl_num_levels {
                continue;
            }
            if current_row_length != copy.buffer_row_length {
                current_row_length = copy.buffer_row_length;
                unsafe {
                    gl::PixelStorei(gl::UNPACK_ROW_LENGTH, current_row_length as i32);
                }
            }
            if current_image_height != copy.buffer_image_height {
                current_image_height = copy.buffer_image_height;
                unsafe {
                    gl::PixelStorei(gl::UNPACK_IMAGE_HEIGHT, current_image_height as i32);
                }
            }
            self.copy_buffer_to_image(copy, buffer_offset);
        }
        if is_rescaled {
            self.scale_up(base, false);
        }
    }

    /// Port of `Image::CopyBufferToImage` (gl_texture_cache.cpp:853-907).
    /// Per `image_type` dispatch to the right `glTextureSubImage*` call.
    /// Compressed formats (`gl_format == GL_NONE`) use the
    /// `glCompressedTextureSubImage*` variants. 1D and 2D/Linear share
    /// the 2D/3D APIs respectively (upstream uses 2D-array semantics for
    /// 1D and 2D-array for 2D).
    fn copy_buffer_to_image(
        &self,
        copy: &crate::texture_cache::types::BufferImageCopy,
        buffer_offset: usize,
    ) {
        let is_compressed = self.gl_format == gl::NONE;
        let offset = (copy.buffer_offset + buffer_offset) as *const std::ffi::c_void;
        let level = copy.image_subresource.base_level;
        let base_layer = copy.image_subresource.base_layer;
        let num_layers = copy.image_subresource.num_layers;
        let width = copy.image_extent.width as i32;
        let height = copy.image_extent.height as i32;
        let depth = copy.image_extent.depth as i32;
        let buf_size = copy.buffer_size as i32;
        let ox = copy.image_offset.x;
        let oy = copy.image_offset.y;
        let oz = copy.image_offset.z;
        unsafe {
            match self.base().info.image_type {
                ImageType::E1D => {
                    if is_compressed {
                        gl::CompressedTextureSubImage2D(
                            self.texture.handle,
                            level,
                            ox,
                            base_layer,
                            width,
                            num_layers,
                            self.gl_internal_format,
                            buf_size,
                            offset,
                        );
                    } else {
                        gl::TextureSubImage2D(
                            self.texture.handle,
                            level,
                            ox,
                            base_layer,
                            width,
                            num_layers,
                            self.gl_format,
                            self.gl_type,
                            offset,
                        );
                    }
                }
                ImageType::E2D | ImageType::Linear => {
                    if is_compressed {
                        gl::CompressedTextureSubImage3D(
                            self.texture.handle,
                            level,
                            ox,
                            oy,
                            base_layer,
                            width,
                            height,
                            num_layers,
                            self.gl_internal_format,
                            buf_size,
                            offset,
                        );
                    } else {
                        gl::TextureSubImage3D(
                            self.texture.handle,
                            level,
                            ox,
                            oy,
                            base_layer,
                            width,
                            height,
                            num_layers,
                            self.gl_format,
                            self.gl_type,
                            offset,
                        );
                    }
                }
                ImageType::E3D => {
                    if is_compressed {
                        gl::CompressedTextureSubImage3D(
                            self.texture.handle,
                            level,
                            ox,
                            oy,
                            oz,
                            width,
                            height,
                            depth,
                            self.gl_internal_format,
                            buf_size,
                            offset,
                        );
                    } else {
                        gl::TextureSubImage3D(
                            self.texture.handle,
                            level,
                            ox,
                            oy,
                            oz,
                            width,
                            height,
                            depth,
                            self.gl_format,
                            self.gl_type,
                            offset,
                        );
                    }
                }
                ImageType::Buffer => {
                    // Upstream `ASSERT(false)` — buffer images don't go
                    // through this path; they bind via the buffer cache.
                    log::error!(
                        "Image::copy_buffer_to_image: called on Buffer-type image — should never happen"
                    );
                }
            }
        }
    }

    /// Port of `Image::DownloadMemory(GLuint, size_t, span<BufferImageCopy>)`.
    pub fn download_memory_to_buffer(
        &mut self,
        base: &mut ImageBase,
        buffer_handle: u32,
        buffer_offset: usize,
        copies: &[BufferImageCopy],
    ) {
        self.download_memory_to_buffers(base, &[buffer_handle], &[buffer_offset], copies);
    }

    /// Port of `Image::DownloadMemory(span<GLuint>, span<size_t>, span<BufferImageCopy>)`.
    pub fn download_memory_to_buffers(
        &mut self,
        base: &mut ImageBase,
        buffer_handles: &[u32],
        buffer_offsets: &[usize],
        copies: &[BufferImageCopy],
    ) {
        let is_rescaled = base.flags.contains(ImageFlagBits::RESCALED);
        if is_rescaled {
            self.scale_down(base, false);
        }
        unsafe {
            gl::MemoryBarrier(gl::PIXEL_BUFFER_BARRIER_BIT);
        }
        for (index, &buffer_handle) in buffer_handles.iter().enumerate() {
            // Eden indexes the offsets span unconditionally for every buffer.
            let buffer_offset = buffer_offsets[index];
            unsafe {
                gl::BindBuffer(gl::PIXEL_PACK_BUFFER, buffer_handle);
                gl::PixelStorei(gl::PACK_ALIGNMENT, 1);
            }
            let mut current_row_length = u32::MAX;
            let mut current_image_height = u32::MAX;
            for copy in copies {
                if copy.image_subresource.base_level >= self.gl_num_levels {
                    continue;
                }
                if current_row_length != copy.buffer_row_length {
                    current_row_length = copy.buffer_row_length;
                    unsafe {
                        gl::PixelStorei(gl::PACK_ROW_LENGTH, current_row_length as i32);
                    }
                }
                if current_image_height != copy.buffer_image_height {
                    current_image_height = copy.buffer_image_height;
                    unsafe {
                        gl::PixelStorei(gl::PACK_IMAGE_HEIGHT, current_image_height as i32);
                    }
                }
                self.copy_image_to_buffer(copy, buffer_offset);
            }
        }
        if is_rescaled {
            self.scale_up(base, true);
        }
    }

    /// Port of `Image::DownloadMemory(StagingBufferMap&, span<BufferImageCopy>)`.
    pub fn download_memory_to_staging(
        &mut self,
        base: &mut ImageBase,
        map: &mut StagingBufferMap,
        copies: &[BufferImageCopy],
    ) {
        self.download_memory_to_buffer(base, map.buffer, map.offset, copies);
    }

    /// Port of `Image::CopyImageToBuffer`.
    fn copy_image_to_buffer(&self, copy: &BufferImageCopy, buffer_offset: usize) {
        let level = copy.image_subresource.base_level;
        let width = copy.image_extent.width as i32;
        let ox = copy.image_offset.x;
        let offset = (copy.buffer_offset + buffer_offset) as *mut std::ffi::c_void;
        let buf_size = copy.buffer_size as i32;
        let mut y_offset = 0;
        let mut z_offset = 0;
        let mut height = 1;
        let mut depth = 1;

        match self.base().info.image_type {
            ImageType::E1D => {
                y_offset = copy.image_subresource.base_layer;
                height = copy.image_subresource.num_layers;
            }
            ImageType::E2D | ImageType::Linear => {
                y_offset = copy.image_offset.y;
                z_offset = copy.image_subresource.base_layer;
                height = copy.image_extent.height as i32;
                depth = copy.image_subresource.num_layers;
            }
            ImageType::E3D => {
                y_offset = copy.image_offset.y;
                z_offset = copy.image_offset.z;
                height = copy.image_extent.height as i32;
                depth = copy.image_extent.depth as i32;
            }
            ImageType::Buffer => {
                // Eden's ASSERT is fail-soft; execution continues into the
                // common glGet*TextureSubImage call with the defaults above.
                log::error!(
                    "Image::copy_image_to_buffer: called on Buffer-type image — should never happen"
                );
            }
        }

        unsafe {
            if self.gl_format == gl::NONE {
                gl::GetCompressedTextureSubImage(
                    self.texture.handle,
                    level,
                    ox,
                    y_offset,
                    z_offset,
                    width,
                    height,
                    depth,
                    buf_size,
                    offset,
                );
            } else {
                gl::GetTextureSubImage(
                    self.texture.handle,
                    level,
                    ox,
                    y_offset,
                    z_offset,
                    width,
                    height,
                    depth,
                    self.gl_format,
                    self.gl_type,
                    buf_size,
                    offset,
                );
            }
        }
    }

    /// Port of `Image::ScaleUp`.
    /// Scales the image up by the resolution scaling factor.
    pub fn scale_up(&mut self, base: &mut ImageBase, ignore: bool) -> bool {
        let resolution_active = self.runtime.is_some_and(|runtime| {
            // SAFETY: production images retain the runtime that constructed
            // them, matching Eden's TextureCacheRuntime pointer lifetime.
            unsafe { runtime.as_ref() }.resolution.active
        });
        if !resolution_active {
            return false;
        }
        if base.flags.contains(ImageFlagBits::RESCALED) {
            return false;
        }
        if self.gl_format == gl::NONE && self.gl_type == gl::NONE {
            return false;
        }
        if base.info.image_type == ImageType::Linear {
            // Eden's ASSERT is fail-soft in production.
            log::error!("OpenGL::Image::ScaleUp called for a linear image");
            return false;
        }
        base.flags.insert(ImageFlagBits::RESCALED);
        base.has_scaled = true;
        if ignore {
            self.current_texture = self.upscaled_backup.handle;
            return true;
        }
        self.scale(base, true);
        true
    }

    /// Port of `Image::ScaleDown`.
    /// Scales the image down from the resolution scaling factor.
    pub fn scale_down(&mut self, base: &mut ImageBase, ignore: bool) -> bool {
        let resolution_active = self.runtime.is_some_and(|runtime| {
            // SAFETY: see `scale_up`.
            unsafe { runtime.as_ref() }.resolution.active
        });
        if !resolution_active {
            return false;
        }
        if !base.flags.contains(ImageFlagBits::RESCALED) {
            return false;
        }
        base.flags.remove(ImageFlagBits::RESCALED);
        if ignore {
            self.current_texture = self.texture.handle;
            return true;
        }
        self.scale(base, false);
        true
    }

    fn scale(&mut self, base: &ImageBase, up_scale: bool) {
        let format_type = crate::surface::get_format_type(base.info.format);
        let attachment = rescale_attachment_type(format_type);
        let mask = rescale_buffer_mask(format_type);
        let fbo_index = rescale_fbo_index(format_type);
        let is_2d = base.info.image_type == ImageType::E2D;
        let is_color = (mask & gl::COLOR_BUFFER_BIT) != 0;
        let linear_color_format =
            is_color && !crate::surface::is_pixel_format_integer(base.info.format);
        let filter = if linear_color_format {
            gl::LINEAR
        } else {
            gl::NEAREST
        };
        let mut runtime_ptr = self
            .runtime
            .expect("production OpenGL Image requires TextureCacheRuntime");
        // SAFETY: the runtime is boxed by the texture cache and outlives all
        // image slots, matching Eden's retained runtime pointer.
        let resolution = unsafe { runtime_ptr.as_ref() }.resolution.clone();
        let scaled_width = resolution.scale_up_u32(base.info.size.width);
        let scaled_height = if is_2d {
            resolution.scale_up_u32(base.info.size.height)
        } else {
            base.info.size.height
        };
        let original_width = base.info.size.width;
        let original_height = base.info.size.height;

        if self.upscaled_backup.handle == 0 {
            let mut dst_info = base.info.clone();
            dst_info.size.width = scaled_width;
            dst_info.size.height = scaled_height;
            self.upscaled_backup =
                make_image(&dst_info, self.gl_internal_format, self.gl_num_levels);
        }
        let src_width = if up_scale {
            original_width
        } else {
            scaled_width
        };
        let src_height = if up_scale {
            original_height
        } else {
            scaled_height
        };
        let dst_width = if up_scale {
            scaled_width
        } else {
            original_width
        };
        let dst_height = if up_scale {
            scaled_height
        } else {
            original_height
        };
        let src_handle = if up_scale {
            self.texture.handle
        } else {
            self.upscaled_backup.handle
        };
        let dst_handle = if up_scale {
            self.upscaled_backup.handle
        } else {
            self.texture.handle
        };
        // SAFETY: see the lifetime argument above.
        let runtime = unsafe { runtime_ptr.as_mut() };
        let read_fbo = runtime.rescale_read_fbos[fbo_index].handle;
        let draw_fbo = runtime.rescale_draw_fbos[fbo_index].handle;

        unsafe {
            gl::Disablei(gl::SCISSOR_TEST, 0);
            gl::ViewportIndexedf(0, 0.0, 0.0, dst_width as f32, dst_height as f32);
            for layer in 0..base.info.resources.layers {
                for level in 0..base.info.resources.levels {
                    let src_level_width = std::cmp::max(1, src_width >> level);
                    let src_level_height = std::cmp::max(1, src_height >> level);
                    let dst_level_width = std::cmp::max(1, dst_width >> level);
                    let dst_level_height = std::cmp::max(1, dst_height >> level);
                    gl::NamedFramebufferTextureLayer(
                        read_fbo, attachment, src_handle, level, layer,
                    );
                    gl::NamedFramebufferTextureLayer(
                        draw_fbo, attachment, dst_handle, level, layer,
                    );
                    gl::BlitNamedFramebuffer(
                        read_fbo,
                        draw_fbo,
                        0,
                        0,
                        src_level_width as i32,
                        src_level_height as i32,
                        0,
                        0,
                        dst_level_width as i32,
                        dst_level_height as i32,
                        mask,
                        filter,
                    );
                }
            }
        }
        self.current_texture = dst_handle;
        let state_tracker = runtime.get_state_tracker();
        state_tracker.notify_viewport0();
        state_tracker.notify_scissor0();
    }
}

/// Port of upstream `ImageView::StorageViews` (gl_texture_cache.h ~~line 130).
/// Per-texture-type cache for storage views, split by signed/unsigned
/// channel interpretation (signed views need different GL internal
/// formats than unsigned, even at the same bit width).
#[derive(Default)]
struct StorageViews {
    signeds: [u32; NUM_TEXTURE_TYPES],
    unsigneds: [u32; NUM_TEXTURE_TYPES],
}

/// An OpenGL image view.
///
/// Corresponds to `OpenGL::ImageView`.
pub struct ImageView {
    /// Stable address of the `ImageViewBase` subobject owned by the typed
    /// slot, matching upstream's inheritance rather than duplicating it.
    base: NonNull<ImageViewBase>,
    pub views: [u32; NUM_TEXTURE_TYPES],
    pub default_handle: u32,
    pub internal_format: u32,
    pub buffer_size: u32,
    original_texture: u32,
    num_samples: i32,
    flat_range: SubresourceRange,
    full_range: SubresourceRange,
    swizzle: [SwizzleSource; 4],
    set_object_label: bool,
    is_render_target: bool,
    /// Lazily-allocated storage-view cache. Upstream uses
    /// `std::unique_ptr<StorageViews>` — same lazy-alloc pattern.
    storage_views: Option<StorageViews>,
    /// Owns every name published through `views` and `storage_views`, as
    /// upstream's `std::vector<OGLTextureView> stored_views` does.
    stored_views: Vec<OGLTextureView>,
}

impl ImageView {
    #[cfg(test)]
    pub fn new(base: &mut ImageViewBase) -> Self {
        Self::with_null_views(NonNull::from(base), [0; NUM_TEXTURE_TYPES])
    }

    fn with_null_views(
        base: NonNull<ImageViewBase>,
        null_image_views: [u32; NUM_TEXTURE_TYPES],
    ) -> Self {
        Self {
            base,
            views: null_image_views,
            default_handle: 0,
            internal_format: gl::NONE,
            buffer_size: 0,
            original_texture: 0,
            num_samples: 0,
            flat_range: SubresourceRange::default(),
            full_range: SubresourceRange::default(),
            swizzle: [SwizzleSource::Zero; 4],
            set_object_label: false,
            is_render_target: false,
            storage_views: None,
            stored_views: Vec::new(),
        }
    }

    /// Port of `ImageView(TextureCacheRuntime&, NullImageViewParams)`.
    fn from_null_base(
        base: NonNull<ImageViewBase>,
        null_image_views: [u32; NUM_TEXTURE_TYPES],
    ) -> Self {
        Self::with_null_views(base, null_image_views)
    }

    fn base(&self) -> &ImageViewBase {
        // SAFETY: the backend view is dropped before the slot and the boxed
        // base remains at a stable address while the slot is moved/recycled.
        unsafe { self.base.as_ref() }
    }

    pub fn supports_anisotropy(&self) -> bool {
        self.base().supports_anisotropy()
    }

    pub fn handle(&self, handle_type: usize) -> u32 {
        self.views[handle_type]
    }

    pub fn default_handle(&self) -> u32 {
        self.default_handle
    }

    pub fn format(&self) -> u32 {
        self.internal_format
    }

    /// PixelFormat stored on upstream `ImageViewBase::format`.
    ///
    /// Texture-buffer bindings pass this value to `BufferCache`, which then
    /// performs the same PixelFormat -> GL internal-format conversion as
    /// upstream `OpenGL::Buffer::View(...)`.
    pub fn pixel_format(&self) -> PixelFormat {
        self.base().format
    }

    /// Port of upstream `ImageView::BufferSize()`.
    pub fn buffer_size(&self) -> u32 {
        self.buffer_size
    }

    /// Port of upstream `ImageViewBase::image_id` access used by compute
    /// image-store binding.
    pub fn image_id(&self) -> ImageId {
        self.base().image_id
    }

    pub fn size(&self) -> Extent3D {
        self.base().size
    }

    /// Port of
    /// `ImageView::ImageView(TextureCacheRuntime&, const ImageViewInfo&, ImageId, Image&, ...)`
    /// for the currently ported Color2D render-target path.
    pub fn new_color_2d(
        base: NonNull<ImageViewBase>,
        image: &Image,
        null_image_views: [u32; NUM_TEXTURE_TYPES],
        set_object_label: bool,
    ) -> Self {
        // SAFETY: `base` points into the boxed base of the owning slot.
        let base_ref = unsafe { base.as_ref() };
        let mut view = Self::with_null_views(base, null_image_views);
        view.internal_format = present_internal_format(base_ref.format);
        view.original_texture = image.handle();
        view.num_samples = image.base().info.num_samples as i32;
        view.flat_range = base_ref.range;
        view.full_range = base_ref.range;
        view.set_object_label = set_object_label;
        view.is_render_target = true;

        if base_ref.flags.contains(ImageViewFlagBits::SLICE) {
            view.full_range = Self::effective_full_range(base_ref);
            view.setup_view(TextureType::Color3D);
        } else {
            if base_ref.view_type == ImageViewType::E2DArray {
                view.flat_range.extent.layers = 1;
            }
            view.setup_view(TextureType::Color2D);
            view.setup_view(TextureType::ColorArray2D);
        }
        view.default_handle = match base_ref.view_type {
            ImageViewType::E2DArray => view.handle_for_texture_type(TextureType::ColorArray2D),
            _ => view.handle_for_texture_type(TextureType::Color2D),
        };
        view
    }

    /// Port of upstream `ImageView::ImageView(TextureCacheRuntime&,
    /// const ImageInfo&, const ImageViewInfo&, GPUVAddr)` for texture buffers.
    ///
    /// Upstream stores `buffer_size = CalculateGuestSizeInBytes(info)`. For
    /// buffer image views that expression is `BytesPerBlock(format) * width`,
    /// and `ImageViewBase::new_buffer` already copied those two fields.
    pub fn from_buffer_base(
        base: NonNull<ImageViewBase>,
        null_image_views: [u32; NUM_TEXTURE_TYPES],
    ) -> Self {
        // SAFETY: `base` points into the boxed base of the owning slot.
        let base_ref = unsafe { base.as_ref() };
        let mut view = Self::with_null_views(base, null_image_views);
        view.flat_range = base_ref.range;
        view.full_range = base_ref.range;
        view.buffer_size =
            crate::surface::bytes_per_block(base_ref.format).wrapping_mul(base_ref.size.width);
        view
    }

    pub fn handle_for_texture_type(&self, handle_type: TextureType) -> u32 {
        self.handle(handle_type as usize)
    }

    fn matches_base_image(&self, base: &ImageViewBase, image: &Image) -> bool {
        std::ptr::eq(self.base(), base) && self.original_texture == image.handle()
    }

    #[cfg(test)]
    fn matches_buffer_base(&self, base: &ImageViewBase) -> bool {
        std::ptr::eq(self.base(), base)
    }

    fn effective_full_range(base: &ImageViewBase) -> SubresourceRange {
        if base.flags.contains(ImageViewFlagBits::SLICE) {
            SubresourceRange {
                base: crate::texture_cache::types::SubresourceBase {
                    level: base.range.base.level,
                    layer: 0,
                },
                extent: crate::texture_cache::types::SubresourceExtent {
                    levels: 1,
                    layers: 1,
                },
            }
        } else {
            base.range
        }
    }

    /// Port of upstream `ImageView::ImageView(TextureCacheRuntime& runtime,
    /// const ImageViewInfo& info, ImageId image_id_, Image& image,
    /// const SlotVector<Image>&)` (gl_texture_cache.cpp:1101-1196).
    ///
    /// Selects the per-`ImageViewType` `SetupView` calls and `default_handle`
    /// mapping. Buffer views are constructed via a separate path upstream
    /// (`ImageView::ImageView(..., const ImageInfo&, const ImageViewInfo&,
    /// GPUVAddr)`) and are not handled here — those are produced from the
    /// buffer cache, not the descriptor-fill path.
    ///
    /// The upstream `Converted` / ASTC / SRGB re-targeting branch keys off
    /// `image.flags & ImageFlagBits::Converted`; the backend reads that flag
    /// from the same boxed `ImageBase` subobject as the generic cache.
    pub fn from_image_view_info(
        base: NonNull<ImageViewBase>,
        image: &Image,
        null_image_views: [u32; NUM_TEXTURE_TYPES],
        set_object_label: bool,
    ) -> Self {
        use crate::texture_cache::types::ImageViewType;

        // SAFETY: `base` points into the boxed base of the owning slot.
        let base_ref = unsafe { base.as_ref() };
        let mut view = Self::with_null_views(base, null_image_views);
        view.internal_format = if image.base().flags.contains(ImageFlagBits::CONVERTED) {
            let is_srgb = crate::surface::is_pixel_format_srgb(base_ref.format);
            let mut internal_format = if is_srgb { gl::SRGB8_ALPHA8 } else { gl::RGBA8 };
            if crate::surface::is_pixel_format_astc(base_ref.format)
                && is_astc_recompression_enabled()
            {
                internal_format = select_astc_format(base_ref.format, is_srgb);
            }
            internal_format
        } else {
            present_internal_format(base_ref.format)
        };
        view.original_texture = image.handle();
        view.num_samples = image.base().info.num_samples as i32;
        view.full_range = base_ref.range;
        view.flat_range = base_ref.range;
        view.set_object_label = set_object_label;
        view.is_render_target = base_ref.is_render_target();
        if !view.is_render_target {
            view.swizzle = decode_swizzle(base_ref.swizzle).unwrap_or_else(|| {
                log::error!("OpenGL::ImageView received an invalid component swizzle");
                [SwizzleSource::Zero; 4]
            });
        }

        // First switch: per-type SetupView calls.
        match base_ref.view_type {
            ImageViewType::E1DArray => {
                view.flat_range.extent.layers = 1;
                view.setup_view(TextureType::Color1D);
                view.setup_view(TextureType::ColorArray1D);
            }
            ImageViewType::E1D => {
                view.setup_view(TextureType::Color1D);
                view.setup_view(TextureType::ColorArray1D);
            }
            ImageViewType::E2DArray | ImageViewType::E2D | ImageViewType::Rect => {
                if base_ref.view_type == ImageViewType::E2DArray {
                    view.flat_range.extent.layers = 1;
                }
                if base_ref.flags.contains(ImageViewFlagBits::SLICE) {
                    if base_ref.range.extent.levels != 1 {
                        // Eden's ASSERT is fail-soft in production.
                        log::error!(
                            "OpenGL::ImageView slice has {} mip levels instead of one",
                            base_ref.range.extent.levels
                        );
                    }
                    view.full_range = Self::effective_full_range(base_ref);
                    view.setup_view(TextureType::Color3D);
                } else {
                    view.setup_view(TextureType::Color2D);
                    view.setup_view(TextureType::ColorArray2D);
                }
            }
            ImageViewType::E3D => {
                view.setup_view(TextureType::Color3D);
            }
            ImageViewType::CubeArray => {
                view.flat_range.extent.layers = 6;
                view.setup_view(TextureType::ColorCube);
                view.setup_view(TextureType::ColorArrayCube);
            }
            ImageViewType::Cube => {
                view.setup_view(TextureType::ColorCube);
                view.setup_view(TextureType::ColorArrayCube);
            }
            ImageViewType::Buffer => {
                // Upstream ASSERT(false): buffer views go through a separate
                // constructor.
                log::error!("OpenGL::ImageView image constructor received a buffer view");
            }
        }

        // Second switch: default_handle selection.
        view.default_handle = match base_ref.view_type {
            ImageViewType::E1D => view.handle_for_texture_type(TextureType::Color1D),
            ImageViewType::E1DArray => view.handle_for_texture_type(TextureType::ColorArray1D),
            ImageViewType::E2D | ImageViewType::Rect => {
                view.handle_for_texture_type(TextureType::Color2D)
            }
            ImageViewType::E2DArray => view.handle_for_texture_type(TextureType::ColorArray2D),
            ImageViewType::E3D => view.handle_for_texture_type(TextureType::Color3D),
            ImageViewType::Cube => view.handle_for_texture_type(TextureType::ColorCube),
            ImageViewType::CubeArray => view.handle_for_texture_type(TextureType::ColorArrayCube),
            ImageViewType::Buffer => 0,
        };
        view
    }

    fn setup_view(&mut self, view_type: TextureType) {
        let view = self.make_view(view_type, self.internal_format);
        self.views[view_type as usize] = view;
    }

    fn make_view(&mut self, view_type: TextureType, view_format: u32) -> u32 {
        let view_range = match view_type {
            TextureType::Color1D
            | TextureType::Color2D
            | TextureType::ColorCube
            | TextureType::Color2DRect => self.flat_range,
            TextureType::ColorArray1D
            | TextureType::ColorArray2D
            | TextureType::Color3D
            | TextureType::ColorArrayCube => self.full_range,
            _ => unreachable!("OpenGL::ImageView::MakeView invalid type {view_type:?}"),
        };
        let mut view = OGLTextureView::new();
        view.create();
        let handle = view.handle;
        let target = image_view_target(view_type, self.num_samples);
        unsafe {
            gl::TextureView(
                handle,
                target,
                self.original_texture,
                view_format,
                view_range.base.level as u32,
                view_range.extent.levels as u32,
                view_range.base.layer as u32,
                view_range.extent.layers as u32,
            );
            if !self.is_render_target {
                apply_swizzle(handle, self.base().format, self.swizzle);
            }
            if self.set_object_label {
                let name = crate::texture_cache::formatter::image_view_name(
                    self.base(),
                    self.base().gpu_addr,
                );
                gl::ObjectLabel(gl::TEXTURE, handle, name.len() as i32, name.as_ptr().cast());
            }
        }
        self.stored_views.push(view);
        handle
    }

    /// Port of `ImageView::StorageView`
    /// (gl_texture_cache.cpp:1212-1227). Returns a GL texture handle
    /// usable as an image-load/store target with the requested
    /// `image_format`. A `Typeless` request just returns the sampling
    /// handle for the texture type. Otherwise the result is a separate
    /// `glTextureView` cached by (signed/unsigned, texture_type) so
    /// signed/unsigned formats with the same width get distinct names.
    ///
    /// Rust's `Option<StorageViews>` preserves upstream's lazy
    /// `std::unique_ptr<StorageViews>` allocation.
    pub fn storage_view(
        &mut self,
        texture_type: TextureType,
        image_format: shader_recompiler::shader_info::ImageFormat,
    ) -> u32 {
        use shader_recompiler::shader_info::ImageFormat;
        if image_format == ImageFormat::Typeless {
            return self.handle(texture_type as usize);
        }
        let is_signed = matches!(image_format, ImageFormat::R8Sint | ImageFormat::R16Sint);
        let idx = texture_type as usize;
        // Probe the cache first (immutable read), then create + insert
        // if missing. The make_view borrow + storage_views borrow can't
        // overlap, so split into two stages.
        let cached = self.storage_views.as_ref().map(|sv| {
            if is_signed {
                sv.signeds[idx]
            } else {
                sv.unsigneds[idx]
            }
        });
        if let Some(h) = cached {
            if h != 0 {
                return h;
            }
        }
        let new_view = self.make_view(texture_type, shader_format(image_format));
        let storage = self.storage_views.get_or_insert_with(StorageViews::default);
        if is_signed {
            storage.signeds[idx] = new_view;
        } else {
            storage.unsigneds[idx] = new_view;
        }
        new_view
    }
}

/// Port of upstream `OpenGL::ShaderFormat(Shader::ImageFormat)`
/// (gl_texture_cache.cpp:419-440). Maps a single-channel /
/// integer-channel storage-image format to its GL internal format.
/// `Typeless` is not legal here — caller short-circuits before this.
pub fn shader_format(format: shader_recompiler::shader_info::ImageFormat) -> u32 {
    use shader_recompiler::shader_info::ImageFormat;
    match format {
        ImageFormat::Typeless => {
            // Eden's `ASSERT_MSG` is fail-soft and returns GL_R32UI.
            log::error!("gl_texture_cache::shader_format: called with Typeless");
            gl::R32UI
        }
        ImageFormat::R8Sint => gl::R8I,
        ImageFormat::R8Uint => gl::R8UI,
        ImageFormat::R16Uint => gl::R16UI,
        ImageFormat::R16Sint => gl::R16I,
        ImageFormat::R32Uint => gl::R32UI,
        ImageFormat::R32G32Uint => gl::RG32UI,
        ImageFormat::R32G32B32A32Uint => gl::RGBA32UI,
    }
}

/// Returns the OpenGL internal format for an image view.
///
/// Port of the `MaxwellToGL::GetFormatTuple(format).internal_format` branch
/// used by upstream `ImageView::ImageView(...)`.
pub fn present_internal_format(format: PixelFormat) -> u32 {
    super::maxwell_to_gl::get_format_tuple(format).internal_format
}

/// An OpenGL sampler.
///
/// Corresponds to `OpenGL::Sampler`.
pub struct Sampler {
    // Rust field-drop order reproduces C++ reverse member destruction.
    sampler_default_anisotropy: OGLSampler,
    sampler: OGLSampler,
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            sampler_default_anisotropy: OGLSampler::new(),
            sampler: OGLSampler::new(),
        }
    }

    /// Port of `OpenGL::Sampler::Sampler(TextureCacheRuntime&, const TSCEntry&)`
    /// (gl_texture_cache.cpp:1271-1324).
    ///
    /// Builds a `glCreateSamplers`-allocated sampler object configured per
    /// the TSC descriptor: wrap modes, compare op, min/mag filters, LOD
    /// range/bias, border colour, and (when extensions are present)
    /// anisotropy + reduction-filter mode + seamless-cubemap toggle.
    ///
    /// The dual-sampler "fallback anisotropy" path that upstream uses
    /// when `MaxAnisotropy() > (1 << config.max_anisotropy)` is wired
    /// up identically — `sampler_default_anisotropy` is allocated only
    /// when needed.
    pub fn from_tsc_entry(
        _runtime: &mut TextureCacheRuntime,
        config: &crate::textures::texture::TscEntry,
    ) -> Self {
        let compare_mode = if config.depth_compare_enabled() != 0 {
            gl::COMPARE_REF_TO_TEXTURE as i32
        } else {
            gl::NONE as i32
        };
        let compare_func =
            super::maxwell_to_gl::depth_compare_func(config.depth_compare_func()) as i32;
        // Upstream calls `TextureFilterMode(config.mag_filter,
        // TextureMipmapFilter::None)`; the raw enum value of `None` is 1.
        let mag = super::maxwell_to_gl::texture_filter_mode(config.mag_filter(), 1) as i32;
        let min_mipmap_filter = config.mipmap_filter();
        let min = super::maxwell_to_gl::texture_filter_mode(config.min_filter(), min_mipmap_filter)
            as i32;
        let reduction = super::maxwell_to_gl::reduction_filter(config.reduction_filter()) as i32;
        let seamless = if config.cubemap_interface_filtering() != 0 {
            gl::TRUE as i32
        } else {
            gl::FALSE as i32
        };

        if config.cubemap_anisotropy() != 1 {
            log::error!(
                "OpenGL::Sampler cubemap_anisotropy={} is not implemented",
                config.cubemap_anisotropy()
            );
        }

        let max_anisotropy = config.computed_max_anisotropy().clamp(1.0, 16.0);
        let border = config.border_color();
        let has_anisotropy = super::gl_device::has_extension("GL_ARB_texture_filter_anisotropic")
            || super::gl_device::has_extension("GL_EXT_texture_filter_anisotropic");
        let has_minmax = super::gl_device::has_extension("GL_ARB_texture_filter_minmax")
            || super::gl_device::has_extension("GL_EXT_texture_filter_minmax");
        let has_seamless = super::gl_device::has_extension("GL_ARB_seamless_cubemap_per_texture")
            || super::gl_device::has_extension("GL_AMD_seamless_cubemap_per_texture");

        let create_one = |anisotropy: f32| -> OGLSampler {
            let mut sampler = OGLSampler::new();
            sampler.create();
            let handle = sampler.handle;
            unsafe {
                gl::SamplerParameteri(
                    handle,
                    gl::TEXTURE_WRAP_S,
                    super::maxwell_to_gl::wrap_mode(config.wrap_u()) as i32,
                );
                gl::SamplerParameteri(
                    handle,
                    gl::TEXTURE_WRAP_T,
                    super::maxwell_to_gl::wrap_mode(config.wrap_v()) as i32,
                );
                gl::SamplerParameteri(
                    handle,
                    gl::TEXTURE_WRAP_R,
                    super::maxwell_to_gl::wrap_mode(config.wrap_p()) as i32,
                );
                gl::SamplerParameteri(handle, gl::TEXTURE_COMPARE_MODE, compare_mode);
                gl::SamplerParameteri(handle, gl::TEXTURE_COMPARE_FUNC, compare_func);
                gl::SamplerParameteri(handle, gl::TEXTURE_MAG_FILTER, mag);
                gl::SamplerParameteri(handle, gl::TEXTURE_MIN_FILTER, min);
                gl::SamplerParameterf(handle, gl::TEXTURE_LOD_BIAS, config.lod_bias());
                gl::SamplerParameterf(handle, gl::TEXTURE_MIN_LOD, config.min_lod());
                gl::SamplerParameterf(handle, gl::TEXTURE_MAX_LOD, config.max_lod());
                gl::SamplerParameterfv(handle, gl::TEXTURE_BORDER_COLOR, border.as_ptr());
                // The `gl` crate doesn't expose either pname symbolically.
                // GL_TEXTURE_MAX_ANISOTROPY = 0x84FE (ARB-promoted from EXT).
                // GL_TEXTURE_REDUCTION_MODE_ARB = 0x9366.
                const GL_TEXTURE_MAX_ANISOTROPY: u32 = 0x84FE;
                const GL_TEXTURE_REDUCTION_MODE_ARB: u32 = 0x9366;
                const GL_WEIGHTED_AVERAGE_ARB: i32 = 0x9367;
                if has_anisotropy {
                    gl::SamplerParameterf(handle, GL_TEXTURE_MAX_ANISOTROPY, anisotropy);
                } else {
                    log::warn!("GL_ARB_texture_filter_anisotropic is required");
                }
                if has_minmax {
                    gl::SamplerParameteri(handle, GL_TEXTURE_REDUCTION_MODE_ARB, reduction);
                } else if reduction != GL_WEIGHTED_AVERAGE_ARB {
                    log::warn!("GL_ARB_texture_filter_minmax is required");
                }
                if has_seamless {
                    gl::SamplerParameteri(handle, gl::TEXTURE_CUBE_MAP_SEAMLESS, seamless);
                } else if seamless == gl::FALSE as i32 {
                    log::warn!("GL_ARB_seamless_cubemap_per_texture is required");
                }
            }
            sampler
        };

        let sampler = create_one(max_anisotropy);
        // Upstream's dual-sampler trick: if the requested anisotropy
        // exceeds the descriptor's `max_anisotropy` bit field (rare —
        // happens when Settings forces higher than the game asked for),
        // build a second sampler with the descriptor's value so render
        // passes that fall outside the override can use the original.
        let max_anisotropy_default = (1u32 << config.max_anisotropy_raw()) as f32;
        let sampler_default_anisotropy = if max_anisotropy > max_anisotropy_default {
            create_one(max_anisotropy_default)
        } else {
            OGLSampler::new()
        };

        Self {
            sampler,
            sampler_default_anisotropy,
        }
    }

    pub fn handle(&self) -> u32 {
        self.sampler.handle
    }

    pub fn handle_with_default_anisotropy(&self) -> u32 {
        self.sampler_default_anisotropy.handle
    }

    pub fn has_added_anisotropy(&self) -> bool {
        self.sampler_default_anisotropy.handle != 0
    }
}

/// An OpenGL framebuffer.
///
/// Corresponds to `OpenGL::Framebuffer` (texture cache version).
pub struct TextureCacheFramebuffer {
    pub framebuffer: OGLFramebuffer,
    pub buffer_bits: u32,
}

impl TextureCacheFramebuffer {
    /// Port of `OpenGL::Framebuffer::Framebuffer(TextureCacheRuntime&, ...)`.
    fn new(
        runtime: &mut TextureCacheRuntime,
        color_buffers: [Option<NonNull<ImageView>>; NUM_RT],
        depth_buffer: Option<NonNull<ImageView>>,
        key: &RenderTargets,
    ) -> Self {
        let mut framebuffer = OGLFramebuffer::new();
        framebuffer.create();
        let handle = framebuffer.handle;
        let mut buffer_bits = gl::NONE;
        unsafe {
            let mut num_buffers = 0i32;
            let mut gl_draw_buffers = [gl::NONE; NUM_RT];
            for (index, image_view) in color_buffers.iter().enumerate() {
                let Some(image_view) = image_view else {
                    continue;
                };
                let image_view = image_view.as_ref();
                buffer_bits |= gl::COLOR_BUFFER_BIT;
                gl_draw_buffers[index] = gl::COLOR_ATTACHMENT0 + key.draw_buffers[index] as u32;
                num_buffers = index as i32 + 1;

                attach_framebuffer_texture(
                    handle,
                    gl::COLOR_ATTACHMENT0 + index as u32,
                    framebuffer_attachment_texture(image_view.base(), image_view),
                    image_view.base(),
                );
            }

            if let Some(depth_buffer) = depth_buffer {
                let image_view = depth_buffer.as_ref();
                match crate::surface::get_format_type(image_view.base().format) {
                    SurfaceType::Depth => buffer_bits |= gl::DEPTH_BUFFER_BIT,
                    SurfaceType::Stencil => buffer_bits |= gl::STENCIL_BUFFER_BIT,
                    SurfaceType::DepthStencil => {
                        buffer_bits |= gl::DEPTH_BUFFER_BIT | gl::STENCIL_BUFFER_BIT;
                    }
                    _ => {
                        // Eden's ASSERT is fail-soft in production.
                        log::error!("OpenGL framebuffer depth attachment has non-depth format");
                        buffer_bits |= gl::DEPTH_BUFFER_BIT;
                    }
                }
                attach_framebuffer_texture(
                    handle,
                    framebuffer_attachment_type(image_view.base().format),
                    framebuffer_attachment_texture(image_view.base(), image_view),
                    image_view.base(),
                );
            }

            if num_buffers > 1 {
                gl::NamedFramebufferDrawBuffers(handle, num_buffers, gl_draw_buffers.as_ptr());
            } else if num_buffers > 0 {
                gl::NamedFramebufferDrawBuffer(handle, gl_draw_buffers[0]);
            } else {
                gl::NamedFramebufferDrawBuffer(handle, gl::NONE);
            }

            gl::NamedFramebufferParameteri(
                handle,
                gl::FRAMEBUFFER_DEFAULT_WIDTH,
                key.size.width as i32,
            );
            gl::NamedFramebufferParameteri(
                handle,
                gl::FRAMEBUFFER_DEFAULT_HEIGHT,
                key.size.height as i32,
            );

            if runtime.has_debugging_tool_attached() {
                let name = crate::texture_cache::formatter::render_targets_name(key);
                gl::ObjectLabel(
                    gl::FRAMEBUFFER,
                    handle,
                    name.len() as i32,
                    name.as_ptr().cast(),
                );
            }
        }
        Self {
            framebuffer,
            buffer_bits,
        }
    }

    pub fn handle(&self) -> u32 {
        self.framebuffer.handle
    }

    pub fn buffer_bits(&self) -> u32 {
        self.buffer_bits
    }
}

/// Texture cache parameters matching upstream `TextureCacheParams`.
pub struct TextureCacheParams;

impl TextureCacheParams {
    pub const ENABLE_VALIDATION: bool = true;
    pub const FRAMEBUFFER_BLITS: bool = true;
    pub const HAS_EMULATED_COPIES: bool = true;
    pub const HAS_DEVICE_MEMORY_INFO: bool = true;
    pub const IMPLEMENTS_ASYNC_DOWNLOADS: bool = true;
}

impl crate::texture_cache::texture_cache_base::TextureCacheParams for TextureCacheParams {
    type Runtime = TextureCacheRuntime;
    type Image = Image;
    type ImageAlloc = ();
    type ImageView = ImageView;
    type Sampler = Sampler;
    type Framebuffer = TextureCacheFramebuffer;
    type FramebufferError = std::convert::Infallible;
    type AsyncBuffer = StagingBufferMap;
    type BufferType = u32;

    const ENABLE_VALIDATION: bool = Self::ENABLE_VALIDATION;
    const FRAMEBUFFER_BLITS: bool = Self::FRAMEBUFFER_BLITS;
    const HAS_EMULATED_COPIES: bool = Self::HAS_EMULATED_COPIES;
    const HAS_DEVICE_MEMORY_INFO: bool = Self::HAS_DEVICE_MEMORY_INFO;
    const IMPLEMENTS_ASYNC_DOWNLOADS: bool = Self::IMPLEMENTS_ASYNC_DOWNLOADS;

    fn create_image(
        runtime: Option<&mut TextureCacheRuntime>,
        _image_id: ImageId,
        mut base: NonNull<ImageBase>,
    ) -> Image {
        let runtime = runtime.expect("OpenGL TextureCache runtime must be bound");
        // SAFETY: the generic cache exclusively owns the just-inserted slot.
        TextureCache::apply_backend_image_flags(
            unsafe { base.as_mut() },
            runtime.has_native_astc(),
        );
        Image::from_base(base, runtime)
    }

    fn set_image_allocation_tick(image: &mut Image, allocation_tick: u64) {
        image.allocation_tick = allocation_tick;
    }

    fn create_image_view(
        runtime: Option<&mut TextureCacheRuntime>,
        _view_id: ImageViewId,
        base: NonNull<ImageViewBase>,
        image: Option<&Image>,
    ) -> ImageView {
        let runtime = runtime.expect("OpenGL TextureCache runtime must be bound");
        // SAFETY: the base belongs to the slot receiving this payload.
        if unsafe { base.as_ref() }.is_buffer() {
            ImageView::from_buffer_base(base, runtime.null_image_views)
        } else {
            ImageView::from_image_view_info(
                base,
                image.expect("non-buffer OpenGL image view requires its parent image"),
                runtime.null_image_views,
                runtime.has_debugging_tool_attached(),
            )
        }
    }

    fn create_sampler(
        runtime: Option<&mut TextureCacheRuntime>,
        config: &crate::textures::texture::TscEntry,
    ) -> Sampler {
        Sampler::from_tsc_entry(
            runtime.expect("OpenGL TextureCache runtime must be bound"),
            config,
        )
    }

    fn create_framebuffer(
        runtime: Option<&mut TextureCacheRuntime>,
        color_buffers: [Option<NonNull<ImageView>>; NUM_RT],
        depth_buffer: Option<NonNull<ImageView>>,
        key: &RenderTargets,
    ) -> Result<TextureCacheFramebuffer, std::convert::Infallible> {
        Ok(TextureCacheFramebuffer::new(
            runtime.expect("OpenGL TextureCache runtime must be bound"),
            color_buffers,
            depth_buffer,
            key,
        ))
    }

    fn prepare_image_view(
        cache: &mut CommonTextureCache<Self>,
        image_view_id: ImageViewId,
        is_modification: bool,
        invalidate: bool,
    ) {
        if !image_view_id.is_valid()
            || image_view_id == NULL_IMAGE_VIEW_ID
            || !cache.slot_image_views.contains(image_view_id)
        {
            return;
        }
        let view_base = cache.slot_image_views.get(image_view_id);
        if view_base.is_buffer() {
            return;
        }
        let image_id = view_base.image_id;
        cache.prepare_image(image_id, is_modification, invalidate);
    }

    fn scale_up_image(
        cache: &mut CommonTextureCache<Self>,
        image_id: ImageId,
        ignore: bool,
    ) -> bool {
        let slot = &mut cache.slot_images[image_id];
        slot.backend
            .as_mut()
            .expect("OpenGL image backend must be materialized")
            .scale_up(slot.base.as_mut(), ignore)
    }

    fn scale_down_image(
        cache: &mut CommonTextureCache<Self>,
        image_id: ImageId,
        ignore: bool,
    ) -> bool {
        let slot = &mut cache.slot_images[image_id];
        slot.backend
            .as_mut()
            .expect("OpenGL image backend must be materialized")
            .scale_down(slot.base.as_mut(), ignore)
    }

    fn upload_staging_buffer(
        cache: &mut CommonTextureCache<Self>,
        size: usize,
        deferred: bool,
    ) -> StagingBufferMap {
        cache.runtime_mut().upload_staging_buffer(size, deferred)
    }

    fn staging_mapped_span(buffer: &mut StagingBufferMap) -> &mut [u8] {
        buffer.mapped_span_mut()
    }

    fn free_deferred_staging_buffer(
        cache: &mut CommonTextureCache<Self>,
        buffer: &mut StagingBufferMap,
    ) {
        cache.runtime_mut().free_deferred_staging_buffer(buffer);
    }

    fn can_upload_msaa(cache: &CommonTextureCache<Self>) -> bool {
        cache.runtime().can_upload_msaa()
    }

    fn transition_image_layout(cache: &mut CommonTextureCache<Self>, image_id: ImageId) {
        let image = cache.slot_images[image_id].base.as_ref();
        cache.runtime().transition_image_layout(image);
    }

    fn upload_image(
        cache: &mut CommonTextureCache<Self>,
        image_id: ImageId,
        staging: &StagingBufferMap,
        copies: &[BufferImageCopy],
    ) {
        let slot = &mut cache.slot_images[image_id];
        slot.backend
            .as_mut()
            .expect("OpenGL image backend must be materialized")
            .upload_memory(&mut slot.base, staging.buffer, staging.offset, copies);
    }

    fn accelerate_image_upload(
        cache: &mut CommonTextureCache<Self>,
        image_id: ImageId,
        staging: &StagingBufferMap,
        swizzles: &[crate::texture_cache::types::SwizzleParameters],
        z_start: u32,
        z_count: u32,
    ) {
        let runtime = cache.runtime_mut() as *mut TextureCacheRuntime;
        let image = cache.slot_images[image_id]
            .backend
            .as_mut()
            .expect("OpenGL image backend must be materialized");
        // SAFETY: the runtime and image slot are disjoint owners in the
        // cache, matching Eden's `runtime` and `slot_images` members.
        unsafe { &mut *runtime }
            .accelerate_image_upload(image, staging, swizzles, z_start, z_count);
    }

    fn insert_upload_memory_barrier(cache: &mut CommonTextureCache<Self>) {
        cache.runtime().insert_upload_memory_barrier();
    }

    fn copy_image(
        cache: &mut CommonTextureCache<Self>,
        dst_id: ImageId,
        src_id: ImageId,
        copies: &[ImageCopy],
    ) {
        if cache.runtime.is_none() {
            return;
        }
        texture_cache_from_base(cache).copy_image(dst_id, src_id, copies);
    }

    fn copy_image_msaa(
        cache: &mut CommonTextureCache<Self>,
        dst_id: ImageId,
        src_id: ImageId,
        copies: &[ImageCopy],
    ) {
        if cache.runtime.is_none() {
            return;
        }
        texture_cache_from_base(cache).copy_image_msaa(dst_id, src_id, copies);
    }
}

/// OpenGL texture cache policy instance.
///
/// Corresponds to upstream
/// `using TextureCache = VideoCommon::TextureCache<TextureCacheParams>`.
/// The generic cache owns the concrete OpenGL slot payloads and the common
/// asynchronous download/decode state, matching upstream `TextureCache<P>`.
#[repr(transparent)]
pub struct TextureCache {
    pub base: CommonTextureCache<TextureCacheParams>,
}

fn texture_cache_from_base(base: &mut CommonTextureCache<TextureCacheParams>) -> &mut TextureCache {
    // SAFETY: `TextureCache` is a transparent newtype whose only field is
    // this generic owner. Policy callbacks are invoked through that owner.
    unsafe { &mut *(base as *mut _ as *mut TextureCache) }
}

/// OpenGL backend result of `TextureCache<P>::TryFindFramebufferImageView`.
pub struct FramebufferImageViewOpenGL {
    pub view_id: ImageViewId,
    pub display_texture: u32,
    pub width: u32,
    pub height: u32,
    pub scaled: bool,
}

fn create_format_properties() -> [HashMap<u32, FormatProperties>; 3] {
    const GL_IMAGE_COMPATIBILITY_CLASS: u32 = 0x82A8;
    const GL_IMAGE_FORMAT_COMPATIBILITY_TYPE: u32 = 0x90C7;
    const GL_IMAGE_FORMAT_COMPATIBILITY_BY_SIZE: i32 = 0x90C8;
    const GL_TEXTURE_COMPRESSED: u32 = 0x86A1;

    let targets = [gl::TEXTURE_1D_ARRAY, gl::TEXTURE_2D_ARRAY, gl::TEXTURE_3D];
    let mut format_properties: [HashMap<u32, FormatProperties>; 3] =
        std::array::from_fn(|_| HashMap::new());
    for (index, target) in targets.into_iter().enumerate() {
        for tuple in super::maxwell_to_gl::FORMAT_TABLE {
            let format = tuple.internal_format;
            let mut compatibility_class = 0;
            let mut compatibility_type = 0;
            let mut is_compressed = 0;
            unsafe {
                gl::GetInternalformativ(
                    target,
                    format,
                    GL_IMAGE_COMPATIBILITY_CLASS,
                    1,
                    &mut compatibility_class,
                );
                gl::GetInternalformativ(
                    target,
                    format,
                    GL_IMAGE_FORMAT_COMPATIBILITY_TYPE,
                    1,
                    &mut compatibility_type,
                );
                gl::GetInternalformativ(
                    target,
                    format,
                    GL_TEXTURE_COMPRESSED,
                    1,
                    &mut is_compressed,
                );
            }
            format_properties[index].insert(
                format,
                FormatProperties {
                    compatibility_class: compatibility_class as u32,
                    compatibility_by_size: compatibility_type
                        == GL_IMAGE_FORMAT_COMPATIBILITY_BY_SIZE,
                    is_compressed: is_compressed == gl::TRUE as i32,
                },
            );
        }
    }
    format_properties
}

impl TextureCache {
    /// Port of `OpenGL::TextureCache::TextureCache(Runtime&, MaxwellDeviceMemoryManager&)`.
    /// `device_memory` is the shared `Arc` from `Host1x::memory_manager()`.
    pub fn new(
        device_memory: std::sync::Arc<
            crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager,
        >,
        device: &super::gl_device::Device,
        program_manager: ProgramManagerHandle,
        state_tracker: &mut StateTracker,
        staging_buffer_pool: SharedStagingBufferPool,
    ) -> Self {
        Self::new_with_runtime(
            device_memory,
            device.has_broken_texture_view_formats(),
            false,
            TextureCacheRuntime::new(device, program_manager, state_tracker, staging_buffer_pool),
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_caps(
        device_memory: std::sync::Arc<
            crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager,
        >,
        has_native_astc: bool,
        has_broken_texture_view_formats: bool,
        has_native_bgr: bool,
        _has_debugging_tool_attached: bool,
        program_manager: ProgramManagerHandle,
        state_tracker: &mut StateTracker,
        staging_buffer_pool: SharedStagingBufferPool,
    ) -> Self {
        Self::new_with_runtime(
            device_memory,
            has_broken_texture_view_formats,
            has_native_bgr,
            TextureCacheRuntime::new_for_test(
                has_broken_texture_view_formats,
                has_native_astc,
                program_manager,
                state_tracker,
                staging_buffer_pool,
            ),
        )
    }

    fn new_with_runtime(
        device_memory: std::sync::Arc<
            crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager,
        >,
        has_broken_texture_view_formats: bool,
        has_native_bgr: bool,
        runtime: TextureCacheRuntime,
    ) -> Self {
        let mut runtime = Box::new(runtime);
        let mut base = CommonTextureCache::<TextureCacheParams>::new_with_caps_for_backend(
            device_memory,
            has_broken_texture_view_formats,
            has_native_bgr,
        );
        base.configure_device_memory_budget(runtime.get_device_local_memory());
        let null_view_base = NonNull::from(base.slot_image_views[NULL_IMAGE_VIEW_ID].base.as_mut());
        base.slot_image_views[NULL_IMAGE_VIEW_ID].backend = Some(ImageView::from_null_base(
            null_view_base,
            runtime.null_image_views,
        ));
        #[cfg(test)]
        let materialize_null_sampler = !runtime.is_test_stub;
        #[cfg(not(test))]
        let materialize_null_sampler = true;
        if materialize_null_sampler {
            let null_sampler = **base.slot_samplers.get(NULL_SAMPLER_ID);
            base.slot_samplers[NULL_SAMPLER_ID].backend =
                Some(Sampler::from_tsc_entry(runtime.as_mut(), &null_sampler));
        }
        base.bind_runtime(runtime);
        Self { base }
    }

    /// Port of `TextureCache<P>::IsRescaling`.
    pub fn is_rescaling_active(&self) -> bool {
        self.base.is_rescaling_active()
    }

    /// OpenGL-backed port of `TextureCache<P>::DmaBufferImageCopy`.
    ///
    /// The common base owns `DmaImageId`, `FindDMAImage`, and the
    /// `BufferImageCopy` field construction. This wrapper performs the
    /// backend `PrepareImage` and `Image::{UploadMemory,DownloadMemory}` work
    /// that upstream keeps in the `TextureCache<P>` specialization.
    pub fn dma_buffer_image_copy(
        &mut self,
        copy_info: &dma::ImageCopy,
        buffer_operand: &dma::BufferOperand,
        image_operand: &dma::ImageOperand,
        image_id: ImageId,
        buffer_handle: u32,
        buffer_offset: usize,
        is_upload: bool,
    ) -> bool {
        if buffer_handle == 0 || image_id == NULL_IMAGE_ID || !image_id.is_valid() {
            return false;
        }
        let Some(copy) = self
            .base
            .dma_buffer_image_copy_descriptor(copy_info, buffer_operand, image_operand, image_id)
            .map(|result| result.copy)
        else {
            return false;
        };

        if is_upload {
            self.base.prepare_image(image_id, true, false);
        } else {
            self.base.prepare_image(image_id, false, false);
            let bpp = crate::surface::bytes_per_block(self.base.slot_images[image_id].info.format);
            if buffer_offset % bpp as usize != 0 {
                return false;
            }
        }

        let copies = [copy];
        if is_upload {
            if !self.backend_image_is_ready(image_id) {
                return false;
            }
            let slot = &mut self.base.slot_images[image_id];
            let Some(image) = slot.backend.as_mut() else {
                return false;
            };
            image.upload_memory(&mut slot.base, buffer_handle, buffer_offset, &copies);
        } else {
            let size = buffer_operand.pitch.wrapping_mul(buffer_operand.height) as usize;
            if !self.download_image_into_buffer(
                image_id,
                buffer_handle,
                buffer_offset,
                &copies,
                buffer_operand.address,
                size,
            ) {
                return false;
            }
        }
        true
    }

    /// OpenGL-backed port of `TextureCache<P>::DownloadImageIntoBuffer`.
    fn download_image_into_buffer(
        &mut self,
        image_id: ImageId,
        buffer_handle: u32,
        buffer_offset: usize,
        copies: &[BufferImageCopy],
        address: u64,
        size: usize,
    ) -> bool {
        if size == 0 || buffer_handle == 0 || !self.backend_image_is_ready(image_id) {
            return false;
        }
        let slot = self
            .base
            .slot_buffer_downloads
            .insert(BufferDownload { address, size });
        self.base.uncommitted_downloads.push(PendingDownload {
            is_swizzle: false,
            async_buffer_id: self.base.uncommitted_async_buffers.len(),
            object_id: slot,
        });

        let download_map = self.base.runtime_mut().download_staging_buffer(size, true);
        let slot_image = &mut self.base.slot_images[image_id];
        let image = slot_image
            .backend
            .as_mut()
            .expect("backend_image_is_ready checked above");
        image.download_memory_to_buffers(
            &mut slot_image.base,
            &[buffer_handle, download_map.buffer],
            &[buffer_offset, download_map.offset],
            copies,
        );
        self.base.uncommitted_async_buffers.push(download_map);
        true
    }

    fn backend_image_is_ready(&self, image_id: ImageId) -> bool {
        if !image_id.is_valid() || image_id == NULL_IMAGE_ID {
            return false;
        }
        self.base.slot_images[image_id]
            .backend
            .as_ref()
            .is_some_and(|image| image.handle() != 0)
    }

    fn ready_backend_image_mut(&mut self, image_id: ImageId) -> Option<&mut Image> {
        self.base.slot_images[image_id]
            .backend
            .as_mut()
            .filter(|image| image.handle() != 0)
    }

    fn backend_image(&self, image_id: ImageId) -> Option<&Image> {
        self.base
            .slot_images
            .contains(image_id)
            .then(|| self.base.slot_images[image_id].backend.as_ref())
            .flatten()
    }

    fn backend_image_pair_mut(
        &mut self,
        dst_id: ImageId,
        src_id: ImageId,
    ) -> (&mut Image, &mut Image) {
        assert_ne!(
            dst_id, src_id,
            "mutable OpenGL image copy paths require distinct source and destination images"
        );
        let dst = self.base.slot_images[dst_id]
            .backend
            .as_mut()
            .expect("destination backend image must be materialized")
            as *mut Image;
        let src = self.base.slot_images[src_id]
            .backend
            .as_mut()
            .expect("source backend image must be materialized") as *mut Image;
        // SAFETY: IDs are distinct and each typed slot owns exactly one
        // backend payload. The returned borrows are tied to `&mut self`.
        unsafe { (&mut *dst, &mut *src) }
    }

    fn take_backend_image(&mut self, image_id: ImageId) -> Option<Image> {
        if !self.base.slot_images.contains(image_id) {
            return None;
        }
        self.base.slot_images[image_id].backend.take()
    }

    fn backend_image_view(&self, view_id: ImageViewId) -> Option<&ImageView> {
        self.base
            .slot_image_views
            .contains(view_id)
            .then(|| self.base.slot_image_views[view_id].backend.as_ref())
            .flatten()
    }

    fn backend_image_view_mut(&mut self, view_id: ImageViewId) -> Option<&mut ImageView> {
        if !self.base.slot_image_views.contains(view_id) {
            return None;
        }
        self.base.slot_image_views[view_id].backend.as_mut()
    }

    fn take_backend_image_view(&mut self, view_id: ImageViewId) -> Option<ImageView> {
        if !self.base.slot_image_views.contains(view_id) {
            return None;
        }
        self.base.slot_image_views[view_id].backend.take()
    }

    fn apply_backend_image_flags(image: &mut ImageBase, has_native_astc: bool) {
        // Upstream sets ASYNCHRONOUS_DECODE / ACCELERATED_UPLOAD in
        // OpenGL::Image::Image with this priority.
        if can_be_decoded_async(has_native_astc, &image.info) {
            image.flags.insert(ImageFlagBits::ASYNCHRONOUS_DECODE);
        } else if can_be_accelerated(has_native_astc, &image.info) {
            image.flags.insert(ImageFlagBits::ACCELERATED_UPLOAD);
        }
        if is_converted_image(has_native_astc, image.info.format, image.info.image_type) {
            image
                .flags
                .insert(ImageFlagBits::CONVERTED | ImageFlagBits::COSTLY_LOAD);
        }
    }

    #[cfg(test)]
    fn apply_backend_image_flags_for_test(image: &mut ImageBase, has_native_astc: bool) {
        Self::apply_backend_image_flags(image, has_native_astc);
    }

    pub fn set_guest_memory_writer(&mut self, writer: GuestMemoryWriter) {
        self.base.set_guest_memory_writer(writer);
    }

    fn base_image_exists(&self, image_id: ImageId) -> bool {
        Self::base_image_exists_in(&self.base, image_id)
    }

    fn base_image_exists_in(
        base: &CommonTextureCache<TextureCacheParams>,
        image_id: ImageId,
    ) -> bool {
        base.slot_images.contains(image_id)
    }

    fn find_or_insert_image_from_info_with_options_and_finish(
        &mut self,
        info: &ImageInfo,
        gpu_addr: u64,
        cpu_addr: u64,
        options: RelaxedOptions,
        _read_gpu: &mut dyn FnMut(u64, &mut [u8]) -> bool,
    ) -> ImageId {
        self.base
            .find_or_insert_image_from_info_with_options(info, gpu_addr, cpu_addr, options)
    }

    fn copy_image(&mut self, dst_id: ImageId, src_id: ImageId, copies: &[ImageCopy]) {
        if !dst_id.is_valid() || !src_id.is_valid() || copies.is_empty() {
            return;
        }

        let dst_info = self.base.slot_images[dst_id].info.clone();
        let src_info = self.base.slot_images[src_id].info.clone();
        let src_flags = self.base.slot_images[src_id].flags;
        let src_rescaled = src_flags.contains(ImageFlagBits::RESCALED);
        let scaled_copies = if src_rescaled {
            if !self.base.slot_images[dst_id]
                .flags
                .contains(ImageFlagBits::RESCALED)
            {
                // Eden's ASSERT is fail-soft: it reports the invariant but
                // continues through the rescaled-copy path.
                log::error!(
                    "TextureCache::CopyImage source is rescaled but destination is not: src={} dst={}",
                    src_id.index,
                    dst_id.index,
                );
            }
            let both_2d =
                src_info.image_type == ImageType::E2D && dst_info.image_type == ImageType::E2D;
            Some(scale_up_image_copies(copies, both_2d))
        } else {
            None
        };
        let copies = scaled_copies.as_deref().unwrap_or(copies);

        let dst_format_type = crate::surface::get_format_type(dst_info.format);
        let src_format_type = crate::surface::get_format_type(src_info.format);

        if !self.backend_image_is_ready(dst_id) || !self.backend_image_is_ready(src_id) {
            return;
        }

        if self.backend_image(dst_id).map(Image::handle).unwrap_or(0) == 0
            || self.backend_image(src_id).map(Image::handle).unwrap_or(0) == 0
        {
            return;
        }

        let can_copy = self
            .backend_image(dst_id)
            .zip(self.backend_image(src_id))
            .is_some_and(|(dst, src)| self.base.runtime().can_image_be_copied(dst, src));
        if src_format_type == dst_format_type && can_copy {
            let runtime = self.base.runtime() as *const TextureCacheRuntime;
            let dst = self
                .backend_image(dst_id)
                .expect("backend image was checked above");
            let src = self
                .backend_image(src_id)
                .expect("backend image was checked above");
            // SAFETY: runtime and image slots are disjoint cache owners.
            unsafe { &*runtime }.copy_image(dst, src, copies);
            return;
        }

        if src_format_type == dst_format_type {
            self.emulate_copy_image(dst_id, src_id, copies);
            return;
        }

        if dst_info.image_type != ImageType::E2D || src_info.image_type != ImageType::E2D {
            // These are upstream UNIMPLEMENTED_IF checks. Eden reports them
            // and then continues into ShouldReinterpret.
            log::error!(
                "TextureCache::copy_image: reinterpret path only implemented for 2D images dst={:?} src={:?}",
                dst_info.image_type,
                src_info.image_type
            );
        }
        let should_reinterpret = self
            .backend_image(dst_id)
            .zip(self.backend_image(src_id))
            .is_some_and(|(dst, src)| self.base.runtime().should_reinterpret(dst, src));
        if should_reinterpret {
            self.reinterpret_image(dst_id, src_id, copies);
        }
    }

    fn copy_image_msaa(&mut self, dst_id: ImageId, src_id: ImageId, copies: &[ImageCopy]) {
        let dst_num_samples = self.base.slot_images[dst_id].info.num_samples;
        let src_num_samples = self.base.slot_images[src_id].info.num_samples;

        if !self.backend_image_is_ready(dst_id) || !self.backend_image_is_ready(src_id) {
            return;
        }

        if self.backend_image(dst_id).map(Image::handle).unwrap_or(0) == 0
            || self.backend_image(src_id).map(Image::handle).unwrap_or(0) == 0
        {
            return;
        }

        let src_msaa = src_num_samples > 1;
        let dst_msaa = dst_num_samples > 1;
        if src_msaa == dst_msaa {
            log::warn!(
                "TextureCache::copy_image_msaa: unsupported sample transition src_samples={} dst_samples={}",
                src_num_samples,
                dst_num_samples
            );
            return;
        }
        let runtime = self.base.runtime_mut() as *mut TextureCacheRuntime;
        let (dst_image, src_image) = self.backend_image_pair_mut(dst_id, src_id);
        // SAFETY: runtime and the two distinct image slots are disjoint.
        unsafe { &mut *runtime }.copy_image_msaa(dst_image, src_image, copies);
    }

    fn emulate_copy_image(&mut self, dst_id: ImageId, src_id: ImageId, copies: &[ImageCopy]) {
        if self.backend_image(dst_id).map(Image::handle).unwrap_or(0) == 0
            || self.backend_image(src_id).map(Image::handle).unwrap_or(0) == 0
        {
            return;
        }
        let runtime = self.base.runtime_mut() as *mut TextureCacheRuntime;
        let (dst_image, src_image) = self.backend_image_pair_mut(dst_id, src_id);
        // SAFETY: runtime and the two distinct image slots are disjoint.
        unsafe { &mut *runtime }.emulate_copy_image(dst_image, src_image, copies);
    }

    fn reinterpret_image(&mut self, dst_id: ImageId, src_id: ImageId, copies: &[ImageCopy]) {
        if self.backend_image(dst_id).map(Image::handle).unwrap_or(0) == 0
            || self.backend_image(src_id).map(Image::handle).unwrap_or(0) == 0
        {
            return;
        }
        let runtime = self.base.runtime_mut() as *mut TextureCacheRuntime;
        let (dst_image, src_image) = self.backend_image_pair_mut(dst_id, src_id);
        // SAFETY: runtime and the two distinct image slots are disjoint.
        unsafe { &mut *runtime }.reinterpret_image(dst_image, src_image, copies);
    }

    /// Port of `TextureCache<P>::DownloadMemory` for `TextureCacheParams =
    /// OpenGL`.
    ///
    /// The OpenGL wrapper owns the backend image table, so it performs the
    /// upstream runtime/image download sequence directly, then uses the common
    /// swizzle/writeback helper for guest memory.
    pub fn download_memory(&mut self, cpu_addr: u64, size: usize) {
        if self.base.channel_gpu_memory.is_none() && self.base.guest_memory_writer.is_none() {
            return;
        }

        let mut images = SmallVec::<[ImageId; 16]>::new();
        self.base
            .for_each_image_in_region(cpu_addr, size, |image_id, image| {
                if !image.is_safe_download() {
                    return false;
                }
                image.flags.remove(ImageFlagBits::GPU_MODIFIED);
                images.push(image_id);
                false
            });
        if images.is_empty() {
            return;
        }
        images.sort_by_key(|&id| self.base.slot_images[id].modification_tick);

        for image_id in images {
            let Some((base_image, staging)) = self.download_image_to_host_staging(image_id) else {
                continue;
            };

            let copies = full_download_copies(&base_image.info);
            let _ = self
                .base
                .write_downloaded_image(&base_image, &copies, &staging);
        }
    }

    fn download_image_to_host_staging(
        &mut self,
        image_id: ImageId,
    ) -> Option<(ImageBase, Vec<u8>)> {
        let base_image = self.base.slot_images[image_id].base.as_ref().clone();
        let buffer_size = base_image.unswizzled_size_bytes as usize;
        if buffer_size == 0 {
            return None;
        }
        let copies = full_download_copies(&base_image.info);
        let mut map = self
            .base
            .runtime_mut()
            .download_staging_buffer(buffer_size, false);
        if self.ready_backend_image_mut(image_id).is_none() {
            return None;
        }
        let slot = &mut self.base.slot_images[image_id];
        slot.backend
            .as_mut()
            .expect("backend image was materialized above")
            .download_memory_to_staging(&mut slot.base, &mut map, &copies);
        self.base.runtime_mut().finish();
        Some((base_image, map.mapped_span().to_vec()))
    }

    /// Port of `TextureCache<P>::GetFlushArea`.
    pub fn get_flush_area(
        &mut self,
        cpu_addr: u64,
        size: u64,
    ) -> Option<crate::rasterizer_interface::RasterizerDownloadArea> {
        self.base.get_flush_area(cpu_addr, size as usize)
    }

    /// OpenGL specialization of upstream `TextureCache<P>::FillImageViews`.
    pub fn fill_image_views(
        &mut self,
        views: &mut [crate::texture_cache::texture_cache_base::ImageViewInOut],
        compute: bool,
        blacklist: bool,
    ) {
        self.base.fill_image_views(views, compute, blacklist);
    }

    /// Look up the GL `Sampler` constructed synchronously with its typed slot.
    pub fn get_sampler(&self, id: crate::texture_cache::types::SamplerId) -> Option<&Sampler> {
        if !id.is_valid() {
            return None;
        }
        self.base.slot_samplers[id].backend.as_ref()
    }

    /// Look up the GL `ImageView` constructed synchronously with its typed slot.
    pub fn get_image_view(&self, view_id: ImageViewId) -> Option<&ImageView> {
        if !view_id.is_valid() {
            return None;
        }
        self.backend_image_view(view_id)
    }

    /// Mutable variant of `get_image_view` — used by the storage-image
    /// binding path (Slice 14) since `ImageView::storage_view` caches
    /// per-format views via `glTextureView` and needs `&mut self`.
    pub fn get_image_view_mut(&mut self, view_id: ImageViewId) -> Option<&mut ImageView> {
        if !view_id.is_valid() {
            return None;
        }
        self.backend_image_view_mut(view_id)
    }

    /// Resolve the TIC index consumed by upstream `DrawTexture` into its
    /// prepared OpenGL image view.
    pub fn draw_texture_source(&mut self, index: u32) -> Option<(u32, Extent3D)> {
        let mut selected = [crate::texture_cache::texture_cache_base::ImageViewInOut {
            index,
            blacklist: false,
            id: NULL_IMAGE_VIEW_ID,
        }];
        self.fill_image_views(&mut selected, false, false);
        let view_id = selected[0].id;
        if !view_id.is_valid() || view_id == NULL_IMAGE_VIEW_ID {
            return None;
        }
        let view = self.get_image_view(view_id)?;
        Some((view.default_handle(), view.size()))
    }

    /// Port of upstream `ImageView::GpuAddr()` through the slot's inherited
    /// `ImageViewBase` representation.
    pub fn image_view_gpu_addr(&self, view_id: ImageViewId) -> u64 {
        self.base.slot_image_views[view_id].gpu_addr
    }

    /// Mark an image modified from an image-view owner, matching
    /// `texture_cache.MarkModification(image_view.image_id)`.
    pub fn mark_view_image_modified(&mut self, view_id: ImageViewId) {
        let image_id = self.base.slot_image_views[view_id].image_id;
        self.base.mark_modification_by_id(image_id);
    }

    /// Port of upstream `TextureCache<P>::IsRescaling(ImageView&)`.
    pub fn image_view_is_rescaling(&self, view_id: ImageViewId) -> bool {
        let image_id = self.base.slot_image_views[view_id].image_id;
        self.base.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::RESCALED)
    }

    pub fn write_memory(&mut self, cpu_addr: u64, size: usize) {
        self.base.write_memory(cpu_addr, size);
    }

    pub fn unmap_memory(&mut self, cpu_addr: u64, size: usize) {
        let end = cpu_addr.saturating_add(size as u64);
        let deleted_images: Vec<(ImageId, Vec<ImageViewId>)> = self
            .base
            .slot_images
            .iter()
            .filter_map(|(image_id, image)| {
                if image_id == crate::texture_cache::types::NULL_IMAGE_ID {
                    return None;
                }
                let image_end = image
                    .cpu_addr_end
                    .max(image.cpu_addr.saturating_add(map_size_bytes(image) as u64));
                (image.cpu_addr < end && cpu_addr < image_end)
                    .then(|| (image_id, image.image_view_ids.clone()))
            })
            .collect();
        for (image_id, view_ids) in deleted_images {
            self.take_backend_image(image_id);
            for view_id in view_ids {
                self.take_backend_image_view(view_id);
                self.remove_framebuffers_for_view(view_id);
            }
        }
        self.base.unmap_memory(cpu_addr, size);
    }

    pub fn tick_frame(&mut self) {
        if self.base.runtime().can_report_memory_usage() {
            let used_memory = self.base.runtime().get_device_memory_usage();
            self.base.update_total_used_memory_from_runtime(used_memory);
        }
        if self.base.total_used_memory > self.base.minimum_memory {
            let runtime = self.base.runtime_mut() as *mut TextureCacheRuntime;
            self.base.run_garbage_collector_with_downloader(
                |_image_id, base_image, image, staging| {
                    // SAFETY: texture-cache work is serialized on the GPU
                    // thread and the runtime is stored in a stable Box.
                    let runtime = unsafe { &mut *runtime };
                    if staging.is_empty() {
                        return false;
                    }
                    let copies = full_download_copies(&base_image.info);
                    if image.is_none() {
                        *image = Some(Image::from_base(NonNull::from(&mut *base_image), runtime));
                    }
                    let Some(backend_image) = image.as_mut() else {
                        return false;
                    };
                    let mut map = runtime.download_staging_buffer(staging.len(), false);
                    backend_image.download_memory_to_staging(base_image, &mut map, &copies);
                    runtime.finish();
                    staging.copy_from_slice(map.mapped_span());
                    true
                },
            );
        }
        self.base.tick_delayed_destruction_rings();
        self.base.tick_async_decode();
        self.base.tick_async_unswizzle();
        self.base.runtime_mut().tick_frame();
        self.base.tick_frame();
        let expired_buffers = std::mem::take(&mut self.base.async_buffers_death_ring);
        for buffer in &expired_buffers {
            self.base.runtime_mut().free_deferred_staging_buffer(buffer);
        }
    }

    pub fn should_wait_async_flushes(&self) -> bool {
        self.base.should_wait_async_flushes()
    }

    pub fn has_uncommitted_flushes(&self) -> bool {
        self.base.has_uncommitted_flushes()
    }

    pub fn pop_async_flushes(&mut self) {
        let Some(download_ids) = self.base.committed_downloads.pop_front() else {
            return;
        };
        let mut download_map = self.base.async_buffers.pop_front().unwrap_or_default();
        if download_ids.is_empty() {
            return;
        }
        for download_info in download_ids.iter().rev() {
            let Some(download_buffer) = download_map.get_mut(download_info.async_buffer_id) else {
                log::warn!(
                    "TextureCache::pop_async_flushes missing async buffer {}",
                    download_info.async_buffer_id
                );
                continue;
            };
            if download_info.is_swizzle {
                let image = self.base.slot_images[download_info.object_id]
                    .base
                    .as_ref()
                    .clone();
                let aligned_size =
                    common::alignment::align_up(image.unswizzled_size_bytes as u64, 64) as usize;
                download_buffer.offset = download_buffer.offset.saturating_sub(aligned_size);
                let start = download_buffer.offset;
                let end = start.saturating_add(image.unswizzled_size_bytes as usize);
                let span = download_buffer.mapped_span();
                if end <= span.len() {
                    let copies = full_download_copies(&image.info);
                    let _ = self
                        .base
                        .write_downloaded_image(&image, &copies, &span[start..end]);
                } else {
                    log::warn!(
                        "TextureCache::pop_async_flushes swizzle range out of bounds start={} end={} len={}",
                        start,
                        end,
                        span.len()
                    );
                }
            } else {
                let buffer_info = self
                    .base
                    .slot_buffer_downloads
                    .take(download_info.object_id);
                let start = download_buffer.offset;
                let end = start.saturating_add(buffer_info.size);
                let span = download_buffer.mapped_span();
                if end <= span.len() {
                    if let Some(gpu_memory) = self.base.channel_gpu_memory.as_ref().cloned() {
                        let _ = gpu_memory
                            .lock()
                            .write_block_unsafe(buffer_info.address, &span[start..end]);
                    } else {
                        log::warn!(
                            "TextureCache::pop_async_flushes missing channel GPU memory for DMA download"
                        );
                    }
                } else {
                    log::warn!(
                        "TextureCache::pop_async_flushes DMA range out of bounds start={} end={} len={}",
                        start,
                        end,
                        span.len()
                    );
                }
            }
        }
        self.base.async_buffers_death_ring.extend(download_map);
    }

    pub fn commit_async_flushes(&mut self) {
        let mut download_ids = std::mem::take(&mut self.base.uncommitted_downloads);
        if download_ids.is_empty() {
            self.base.committed_downloads.push_back(download_ids);
            self.base
                .async_buffers
                .push_back(std::mem::take(&mut self.base.uncommitted_async_buffers));
            return;
        }

        let mut total_size_bytes = 0usize;
        let last_async_buffer_id = self.base.uncommitted_async_buffers.len();
        let mut any_non_dma = false;
        for download_info in &mut download_ids {
            if download_info.is_swizzle {
                total_size_bytes += common::alignment::align_up(
                    self.base.slot_images[download_info.object_id].unswizzled_size_bytes as u64,
                    64,
                ) as usize;
                any_non_dma = true;
                download_info.async_buffer_id = last_async_buffer_id;
            }
        }

        if any_non_dma {
            let mut download_map = self
                .base
                .runtime_mut()
                .download_staging_buffer(total_size_bytes, true);
            for download_info in &download_ids {
                if !download_info.is_swizzle {
                    continue;
                }
                let image_id = download_info.object_id;
                if !self.backend_image_is_ready(image_id) {
                    continue;
                }
                let image_base = self.base.slot_images[image_id].base.as_ref().clone();
                let copies = full_download_copies(&image_base.info);
                let slot = &mut self.base.slot_images[image_id];
                if let Some(image) = slot.backend.as_mut() {
                    image.download_memory_to_staging(&mut slot.base, &mut download_map, &copies);
                    download_map.offset +=
                        common::alignment::align_up(image_base.unswizzled_size_bytes as u64, 64)
                            as usize;
                }
            }
            self.base.uncommitted_async_buffers.push(download_map);
        }

        self.base
            .async_buffers
            .push_back(std::mem::take(&mut self.base.uncommitted_async_buffers));
        self.base.committed_downloads.push_back(download_ids);
    }

    fn remove_framebuffers_for_view(&mut self, view_id: ImageViewId) {
        let removed_ids = [view_id];
        let last_framebuffer_id = self.base.last_framebuffer_id;
        let mut removed_last = false;
        self.base.framebuffers.retain(|key, framebuffer_id| {
            if key.contains(&removed_ids) {
                removed_last |= *framebuffer_id == last_framebuffer_id;
                let framebuffer = self.base.slot_framebuffers.take(*framebuffer_id);
                self.base.sentenced_framebuffers.push(framebuffer);
                false
            } else {
                true
            }
        });
        if removed_last {
            self.base.last_framebuffer_id = FramebufferId::default();
            self.base.last_framebuffer_serial = 0;
        }
    }

    /// OpenGL-backed bridge for upstream `TextureCache<P>::UpdateRenderTargets`.
    ///
    /// Rust still passes a register snapshot because this cache does not yet
    /// own `Maxwell3D*`. Keep the dirty update and always-run
    /// `PrepareImageView` phase together so the false-dirty path still
    /// prepares existing views like upstream.
    pub fn update_and_prepare_render_targets_from_snapshot(
        &mut self,
        render_targets: &Maxwell3DRenderTargets,
        dirty_access: &mut impl RenderTargetDirtyFlagAccess,
        is_clear: bool,
        clear_scissor: Option<ScissorInfo>,
    ) {
        let gpu_memory =
            self.base.channel_gpu_memory.as_ref().cloned().expect(
                "TextureCache must have bound channel GPU memory before render-target update",
            );
        self.base.update_render_targets_with_snapshot(
            render_targets,
            dirty_access,
            |gpu_addr, guest_size| {
                let gpu_memory = gpu_memory.lock();
                gpu_memory
                    .gpu_to_cpu_address(gpu_addr)
                    .or_else(|| gpu_memory.gpu_to_cpu_address_range(gpu_addr, guest_size))
            },
            is_clear,
            clear_scissor
                .map(|scissor| (scissor.min_x, scissor.min_y, scissor.max_x, scissor.max_y)),
        );
    }

    /// OpenGL-backed bridge for upstream `TextureCache<P>::UpdateRenderTargets`
    /// followed by `TextureCache<P>::GetFramebuffer`.
    pub fn update_render_targets_and_get_framebuffer_from_snapshot(
        &mut self,
        render_targets: &Maxwell3DRenderTargets,
        dirty_access: &mut impl RenderTargetDirtyFlagAccess,
        is_clear: bool,
        clear_scissor: Option<ScissorInfo>,
    ) -> (u32, u32, u32) {
        self.update_and_prepare_render_targets_from_snapshot(
            render_targets,
            dirty_access,
            is_clear,
            clear_scissor,
        );
        self.framebuffer_for_render_targets()
    }

    /// OpenGL counterpart of upstream `TextureCache<P>::GetFramebuffer()`.
    ///
    /// Upstream keys framebuffers by the complete `RenderTargets` object and
    /// attaches every color target before selecting draw buffers. The previous
    /// Rust path bound only the first mapped render target, which is not
    pub fn framebuffer_for_render_targets(&mut self) -> (u32, u32, u32) {
        let size = self.base.render_targets.size;
        let framebuffer = match self.base.get_framebuffer() {
            Ok(framebuffer) => framebuffer,
            Err(never) => match never {},
        };
        (framebuffer.handle(), size.width, size.height)
    }
    /// OpenGL-backed port of `TextureCache<P>::BlitImage` for the currently
    /// implemented framebuffer-blit path (`TextureCacheParams::FRAMEBUFFER_BLITS`).
    pub fn blit_image(
        &mut self,
        dst: &crate::engines::fermi_2d::Surface,
        src: &crate::engines::fermi_2d::Surface,
        copy: &crate::engines::fermi_2d::Config,
        mut gpu_to_cpu: impl FnMut(u64) -> Option<u64>,
        mut read_gpu: impl FnMut(u64, &mut [u8]) -> bool,
    ) -> bool {
        let dst_addr = dst.address();
        let src_addr = src.address();
        let mut dst_info = ImageInfo::from_fermi2d_surface(dst);
        let mut src_info = ImageInfo::from_fermi2d_surface(src);
        let can_be_depth_blit = dst_info.format == src_info.format
            && copy.filter == crate::engines::fermi_2d::Filter::Point;
        let try_options = if can_be_depth_blit {
            RelaxedOptions::SAMPLES | RelaxedOptions::FORMAT
        } else {
            RelaxedOptions::SAMPLES
        };

        let Some(src_cpu_addr) = gpu_to_cpu(src_addr) else {
            return false;
        };
        let Some(dst_cpu_addr) = gpu_to_cpu(dst_addr) else {
            return false;
        };

        let mut src_id;
        let mut dst_id;
        loop {
            self.base.has_deleted_images = false;
            src_id = self.base.find_image_in_cpu_region_with_caps(
                &src_info,
                src_addr,
                src_cpu_addr,
                try_options,
                self.base.has_broken_texture_view_formats,
                self.base.has_native_bgr,
            );
            dst_id = self.base.find_image_in_cpu_region_with_caps(
                &dst_info,
                dst_addr,
                dst_cpu_addr,
                try_options,
                self.base.has_broken_texture_view_formats,
                self.base.has_native_bgr,
            );
            if !copy.must_accelerate {
                let src_gpu_modified = src_id
                    .map(|id| {
                        self.base.slot_images[id]
                            .flags
                            .contains(ImageFlagBits::GPU_MODIFIED)
                    })
                    .unwrap_or(false);
                let dst_gpu_modified = dst_id
                    .map(|id| {
                        self.base.slot_images[id]
                            .flags
                            .contains(ImageFlagBits::GPU_MODIFIED)
                    })
                    .unwrap_or(false);
                if src_id.is_none() && dst_id.is_none() {
                    return false;
                }
                if !src_gpu_modified && !dst_gpu_modified {
                    return false;
                }
            }

            let src_image = src_id.map(|id| &self.base.slot_images[id]);
            if src_image.is_some_and(|image| image.info.num_samples > 1) {
                let msaa_options = RelaxedOptions::SAMPLES | RelaxedOptions::FORCE_BROKEN_VIEWS;
                src_id = Some(self.find_or_insert_image_from_info_with_options_and_finish(
                    &src_info,
                    src_addr,
                    src_cpu_addr,
                    msaa_options,
                    &mut read_gpu,
                ));
                dst_id = Some(self.find_or_insert_image_from_info_with_options_and_finish(
                    &dst_info,
                    dst_addr,
                    dst_cpu_addr,
                    msaa_options,
                    &mut read_gpu,
                ));
                if self.base.has_deleted_images {
                    continue;
                }
                break;
            }

            if can_be_depth_blit {
                let src_image = src_id.map(|id| &*self.base.slot_images[id]);
                let dst_image = dst_id.map(|id| &*self.base.slot_images[id]);
                crate::texture_cache::util::deduce_blit_images(
                    &mut dst_info,
                    &mut src_info,
                    dst_image,
                    src_image,
                );
                if crate::surface::get_format_type(dst_info.format)
                    != crate::surface::get_format_type(src_info.format)
                {
                    continue;
                }
            }

            if src_id.is_none() {
                src_id = Some(self.find_or_insert_image_from_info_with_options_and_finish(
                    &src_info,
                    src_addr,
                    src_cpu_addr,
                    RelaxedOptions::empty(),
                    &mut read_gpu,
                ));
            }
            if dst_id.is_none() {
                dst_id = Some(self.find_or_insert_image_from_info_with_options_and_finish(
                    &dst_info,
                    dst_addr,
                    dst_cpu_addr,
                    RelaxedOptions::empty(),
                    &mut read_gpu,
                ));
            }
            if !self.base.has_deleted_images {
                break;
            }
        }

        let mut src_id = src_id.unwrap_or(NULL_IMAGE_ID);
        let mut dst_id = dst_id.unwrap_or(NULL_IMAGE_ID);
        if !src_id.is_valid() || !dst_id.is_valid() {
            return false;
        }

        if !self.base_image_exists(src_id) || !self.base_image_exists(dst_id) {
            return false;
        }

        let native_bgr = self.base.has_native_bgr;
        if crate::surface::get_format_type(dst_info.format)
            != crate::surface::get_format_type(self.base.slot_images[dst_id].info.format)
            || crate::surface::get_format_type(src_info.format)
                != crate::surface::get_format_type(self.base.slot_images[src_id].info.format)
            || !crate::compatible_formats::is_view_compatible(
                dst_info.format,
                self.base.slot_images[dst_id].info.format,
                false,
                native_bgr,
            )
            || !crate::compatible_formats::is_view_compatible(
                src_info.format,
                self.base.slot_images[src_id].info.format,
                false,
                native_bgr,
            )
        {
            loop {
                self.base.has_deleted_images = false;
                src_id = self.find_or_insert_image_from_info_with_options_and_finish(
                    &src_info,
                    src_addr,
                    src_cpu_addr,
                    RelaxedOptions::empty(),
                    &mut read_gpu,
                );
                dst_id = self.find_or_insert_image_from_info_with_options_and_finish(
                    &dst_info,
                    dst_addr,
                    dst_cpu_addr,
                    RelaxedOptions::empty(),
                    &mut read_gpu,
                );
                if !self.base.has_deleted_images {
                    break;
                }
            }
            if !self.base_image_exists(src_id) || !self.base_image_exists(dst_id) {
                return false;
            }
        }

        self.base.prepare_image(src_id, false, false);
        self.base.prepare_image(dst_id, true, false);

        if !self.backend_image_is_ready(dst_id) || !self.backend_image_is_ready(src_id) {
            return false;
        }

        let mut is_src_rescaled = self.base.slot_images[src_id]
            .flags
            .contains(ImageFlagBits::RESCALED);
        let mut is_dst_rescaled = self.base.slot_images[dst_id]
            .flags
            .contains(ImageFlagBits::RESCALED);
        let is_resolve = self.base.slot_images[src_id].info.num_samples != 1
            && self.base.slot_images[dst_id].info.num_samples == 1;
        if is_src_rescaled != is_dst_rescaled {
            if self.base.image_can_rescale(src_id) {
                self.base.scale_up(src_id);
                is_src_rescaled = self.base.slot_images[src_id]
                    .flags
                    .contains(ImageFlagBits::RESCALED);
                if is_resolve {
                    self.base.slot_images[dst_id].info.rescaleable = true;
                    let aliases = self.base.slot_images[dst_id].aliased_images.clone();
                    for alias in aliases {
                        self.base.slot_images[alias.id].info.rescaleable = true;
                    }
                }
            }
            if self.base.image_can_rescale(dst_id) {
                self.base.scale_up(dst_id);
                is_dst_rescaled = self.base.slot_images[dst_id]
                    .flags
                    .contains(ImageFlagBits::RESCALED);
            }
        }
        if is_resolve && is_src_rescaled != is_dst_rescaled {
            self.base.scale_down(src_id);
            self.base.scale_down(dst_id);
            is_src_rescaled = self.base.slot_images[src_id]
                .flags
                .contains(ImageFlagBits::RESCALED);
            is_dst_rescaled = self.base.slot_images[dst_id]
                .flags
                .contains(ImageFlagBits::RESCALED);
        }
        let resolution = settings::values().resolution_info.clone();
        let scale_region = |region: &mut Region2D| {
            region.start.x = resolution.scale_up_i32(region.start.x);
            region.start.y = resolution.scale_up_i32(region.start.y);
            region.end.x = resolution.scale_up_i32(region.end.x);
            region.end.y = resolution.scale_up_i32(region.end.y);
        };

        let Some(src_base) = self.base.slot_images[src_id].try_find_base(src_addr) else {
            return false;
        };
        let Some(dst_base) = self.base.slot_images[dst_id].try_find_base(dst_addr) else {
            return false;
        };
        let src_range = SubresourceRange {
            base: src_base,
            ..Default::default()
        };
        let dst_range = SubresourceRange {
            base: dst_base,
            ..Default::default()
        };
        let src_view_id = self.ensure_color_view_for_range(src_id, src_info.format, src_range);
        let dst_view_id = self.ensure_color_view_for_range(dst_id, dst_info.format, dst_range);
        let Some((src_fbo, _, _)) = self.framebuffer_for_image_view(src_view_id) else {
            return false;
        };
        let Some((dst_fbo, _, _)) = self.framebuffer_for_image_view(dst_view_id) else {
            return false;
        };

        let (src_samples_x, src_samples_y) = crate::texture_cache::samples_helper::samples_log2(
            self.base.slot_images[src_id].info.num_samples as i32,
        );
        let (dst_samples_x, dst_samples_y) = crate::texture_cache::samples_helper::samples_log2(
            self.base.slot_images[dst_id].info.num_samples as i32,
        );

        let mut src_region = Region2D {
            start: Offset2D {
                x: copy.src_x0 >> src_samples_x,
                y: copy.src_y0 >> src_samples_y,
            },
            end: Offset2D {
                x: copy.src_x1 >> src_samples_x,
                y: copy.src_y1 >> src_samples_y,
            },
        };
        if is_src_rescaled {
            scale_region(&mut src_region);
        }
        let mut dst_region = Region2D {
            start: Offset2D {
                x: copy.dst_x0 >> dst_samples_x,
                y: copy.dst_y0 >> dst_samples_y,
            },
            end: Offset2D {
                x: copy.dst_x1 >> dst_samples_x,
                y: copy.dst_y1 >> dst_samples_y,
            },
        };
        if is_dst_rescaled {
            scale_region(&mut dst_region);
        }
        self.base.runtime_mut().blit_framebuffer(
            dst_fbo,
            src_fbo,
            gl::COLOR_BUFFER_BIT,
            gl::COLOR_BUFFER_BIT,
            dst_region,
            src_region,
            copy.filter,
            copy.operation,
        );
        self.base.mark_modification_by_id(dst_id);

        true
    }

    fn ensure_color_view_for_range(
        &mut self,
        image_id: ImageId,
        view_format: crate::surface::PixelFormat,
        range: SubresourceRange,
    ) -> ImageViewId {
        let view_info = ImageViewInfo::for_render_target(ImageViewType::E2D, view_format, range);
        let existing = self.base.slot_images[image_id].find_view(&view_info);
        if existing.is_valid() {
            return existing;
        }
        let gpu_addr = self.base.slot_images[image_id].gpu_addr;
        self.base
            .find_or_emplace_image_view(image_id, view_info, gpu_addr)
    }

    fn framebuffer_for_image_view(&mut self, view_id: ImageViewId) -> Option<(u32, u32, u32)> {
        let view_base = self
            .base
            .slot_image_views
            .get(view_id)
            .base
            .as_ref()
            .clone();
        let image_id = view_base.image_id;
        if !image_id.is_valid() {
            return None;
        }
        self.ready_backend_image_mut(image_id)?;
        let view_mismatch = {
            let backend_image = self
                .backend_image(image_id)
                .expect("image inserted above must be present");
            self.base.slot_image_views[view_id]
                .backend
                .as_ref()
                .is_some_and(|view| !view.matches_base_image(&view_base, backend_image))
        };
        if view_mismatch {
            self.take_backend_image_view(view_id);
            self.remove_framebuffers_for_view(view_id);
        }
        if self.base.slot_image_views[view_id].backend.is_none() {
            let base = NonNull::from(self.base.slot_image_views[view_id].base.as_mut());
            let backend_image = self.base.slot_images[image_id]
                .backend
                .as_ref()
                .expect("image inserted above must be present");
            let view = ImageView::new_color_2d(
                base,
                backend_image,
                self.base.runtime().null_image_views,
                self.base.runtime().has_debugging_tool_attached(),
            );
            self.base.slot_image_views[view_id].backend = Some(view);
        }
        let backend_view = self.base.slot_image_views[view_id]
            .backend
            .as_ref()
            .expect("image view was materialized above");
        let attachment_texture = framebuffer_attachment_texture(&view_base, backend_view);
        if attachment_texture == 0 {
            return None;
        }
        let key = self.render_targets_key_for_image_view(view_id, &view_base);
        let framebuffer_id = if let Some(&framebuffer_id) = self.base.framebuffers.get(&key) {
            framebuffer_id
        } else {
            let framebuffer =
                self.create_color_framebuffer_for_view(key, &view_base, attachment_texture);
            let framebuffer_id = self.base.slot_framebuffers.insert(framebuffer);
            self.base.framebuffers.insert(key, framebuffer_id);
            framebuffer_id
        };
        let framebuffer = &self.base.slot_framebuffers[framebuffer_id];
        let handle = framebuffer.handle();
        (handle != 0).then_some((handle, key.size.width, key.size.height))
    }

    fn render_targets_key_for_image_view(
        &self,
        view_id: ImageViewId,
        view_base: &ImageViewBase,
    ) -> RenderTargets {
        let image = &self.base.slot_images[view_base.image_id];
        let is_rescaled = image.flags.contains(ImageFlagBits::RESCALED);
        let resolution = settings::values().resolution_info.clone();
        let mut width = view_base.size.width;
        let mut height = view_base.size.height;
        if is_rescaled {
            width = resolution.scale_up_u32(width);
            if image.info.image_type == ImageType::E2D {
                height = resolution.scale_up_u32(height);
            }
        }
        let (samples_x, samples_y) =
            crate::texture_cache::samples_helper::samples_log2(image.info.num_samples as i32);
        let mut color_buffer_ids = [ImageViewId::default(); NUM_RT];
        color_buffer_ids[0] = view_id;
        RenderTargets {
            color_buffer_ids,
            depth_buffer_id: ImageViewId::default(),
            draw_buffers: [0; NUM_RT],
            size: Extent2D {
                width: (width >> samples_x).max(1),
                height: (height >> samples_y).max(1),
            },
            is_rescaled,
        }
    }

    fn create_color_framebuffer_for_view(
        &self,
        key: RenderTargets,
        view_base: &ImageViewBase,
        attachment_texture: u32,
    ) -> TextureCacheFramebuffer {
        let mut framebuffer = OGLFramebuffer::new();
        framebuffer.create();
        let handle = framebuffer.handle;
        unsafe {
            if handle != 0 {
                attach_framebuffer_texture(
                    handle,
                    gl::COLOR_ATTACHMENT0,
                    attachment_texture,
                    view_base,
                );
                gl::NamedFramebufferDrawBuffer(handle, gl::COLOR_ATTACHMENT0);
                gl::NamedFramebufferParameteri(
                    handle,
                    gl::FRAMEBUFFER_DEFAULT_WIDTH,
                    key.size.width as i32,
                );
                gl::NamedFramebufferParameteri(
                    handle,
                    gl::FRAMEBUFFER_DEFAULT_HEIGHT,
                    key.size.height as i32,
                );
            }
        }
        TextureCacheFramebuffer {
            framebuffer,
            buffer_bits: gl::COLOR_BUFFER_BIT,
        }
    }

    /// Port of `TextureCache<P>::TryFindFramebufferImageView` for
    /// `TextureCacheParams = OpenGL`.
    pub fn try_find_framebuffer_image_view(
        &mut self,
        config: &FramebufferConfig,
        cpu_addr: u64,
    ) -> Option<FramebufferImageViewOpenGL> {
        let framebuffer_view = self
            .base
            .try_find_framebuffer_image_view(config, cpu_addr)?;
        let FramebufferImageView {
            view_id,
            view,
            scaled,
        } = framebuffer_view;
        let image_id = view.image_id;
        self.ready_backend_image_mut(image_id)?;
        let backend_image = self.base.slot_images[image_id]
            .backend
            .as_ref()
            .expect("image inserted above must be present");
        let view_mismatch = self.base.slot_image_views[view_id]
            .backend
            .as_ref()
            .is_some_and(|backend_view| !backend_view.matches_base_image(&view, backend_image));
        if view_mismatch {
            self.take_backend_image_view(view_id);
            self.remove_framebuffers_for_view(view_id);
        }
        if self.base.slot_image_views[view_id].backend.is_none() {
            let base = NonNull::from(self.base.slot_image_views[view_id].base.as_mut());
            let backend_image = self.base.slot_images[image_id]
                .backend
                .as_ref()
                .expect("image inserted above must be present");
            let backend_view = ImageView::from_image_view_info(
                base,
                backend_image,
                self.base.runtime().null_image_views,
                self.base.runtime().has_debugging_tool_attached(),
            );
            self.base.slot_image_views[view_id].backend = Some(backend_view);
        }
        let backend_view = self.base.slot_image_views[view_id]
            .backend
            .as_ref()
            .expect("image view was materialized above");
        let display_texture = backend_view.handle_for_texture_type(TextureType::Color2D);
        if display_texture == 0 {
            return None;
        }
        Some(FramebufferImageViewOpenGL {
            view_id,
            display_texture,
            width: view.size.width,
            height: view.size.height,
            scaled,
        })
    }

    /// OpenGL counterpart of upstream `TextureCache<P>::GetFramebuffer()` for
    /// the currently ported single-color-target clear/present path.
    pub fn framebuffer_for_render_target(
        &mut self,
        rt: &RenderTargetInfo,
    ) -> Option<(u32, u32, u32)> {
        if rt.address == 0 || rt.width == 0 || rt.height == 0 {
            return None;
        }

        let lookup_addr = rt.address;
        let (image_id, _) = self.base.slot_images.iter().find(|(_, image)| {
            image.gpu_addr == lookup_addr
                && image.info.size.width == rt.width
                && image.info.size.height == rt.height
                && !image.image_view_ids.is_empty()
        })?;
        let image_id = image_id;
        let view_id = self
            .base
            .find_render_target_view_from_image(image_id, rt, 0, lookup_addr);
        if !view_id.is_valid() {
            return None;
        }
        let view_base = self.base.slot_image_views[view_id].base.as_ref().clone();

        self.ready_backend_image_mut(image_id)?;
        let view_mismatch = {
            let backend_image = self
                .backend_image(image_id)
                .expect("image inserted above must be present");
            self.base.slot_image_views[view_id]
                .backend
                .as_ref()
                .is_some_and(|view| !view.matches_base_image(&view_base, backend_image))
        };
        if view_mismatch {
            self.take_backend_image_view(view_id);
            self.remove_framebuffers_for_view(view_id);
        }
        if self.base.slot_image_views[view_id].backend.is_none() {
            let base = NonNull::from(self.base.slot_image_views[view_id].base.as_mut());
            let backend_image = self.base.slot_images[image_id]
                .backend
                .as_ref()
                .expect("image inserted above must be present");
            let view = ImageView::new_color_2d(
                base,
                backend_image,
                self.base.runtime().null_image_views,
                self.base.runtime().has_debugging_tool_attached(),
            );
            self.base.slot_image_views[view_id].backend = Some(view);
        }
        let backend_view = self.base.slot_image_views[view_id]
            .backend
            .as_ref()
            .expect("image view was materialized above");
        let attachment_texture = framebuffer_attachment_texture(&view_base, backend_view);
        if attachment_texture == 0 {
            return None;
        }

        let key = self.render_targets_key_for_image_view(view_id, &view_base);
        let framebuffer_id = if let Some(&framebuffer_id) = self.base.framebuffers.get(&key) {
            framebuffer_id
        } else {
            let framebuffer =
                self.create_color_framebuffer_for_view(key, &view_base, attachment_texture);
            let framebuffer_id = self.base.slot_framebuffers.insert(framebuffer);
            self.base.framebuffers.insert(key, framebuffer_id);
            framebuffer_id
        };
        let framebuffer = &self.base.slot_framebuffers[framebuffer_id];
        let handle = framebuffer.handle();
        (handle != 0).then_some((handle, key.size.width, key.size.height))
    }
}

#[cfg(test)]
#[path = "gl_texture_cache_test.rs"]
mod tests;

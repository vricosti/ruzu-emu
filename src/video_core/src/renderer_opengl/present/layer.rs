// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden `video_core/renderer_opengl/present/layer.{h,cpp}`.
//!
//! Presentation layer -- manages a single framebuffer texture, applies anti-aliasing
//! and upscaling, and prepares draw data for the window adapt pass.

use super::fsr::FSR;
use super::fxaa::FXAA;
use super::present_uniforms::{make_orthographic_matrix, ScreenRectVertex};
use super::smaa::SMAA;

use common::math_util::Rectangle;
use ruzu_core::hle::service::nvnflinger::pixel_format::PixelFormat as AndroidPixelFormat;

use crate::framebuffer_config::{self, FramebufferConfig};
use crate::present::{AntiAliasing, PresentFilters, ScalingFilter};
use crate::renderer_base::DeviceMemoryReader;
use crate::renderer_opengl::gl_blit_screen::FramebufferTextureInfo;
use crate::renderer_opengl::gl_resource_manager::OGLTexture;
use crate::renderer_opengl::gl_shader_manager::ProgramManager;
use crate::renderer_opengl::RasterizerOpenGL;
use ruzu_core::frontend::framebuffer_layout::FramebufferLayout;

/// Information about the Switch screen texture.
///
/// Corresponds to `OpenGL::TextureInfo`.
pub struct TextureInfo {
    pub resource: OGLTexture,
    pub width: i32,
    pub height: i32,
    pub gl_format: u32,
    pub gl_type: u32,
    pub pixel_format: AndroidPixelFormat,
}

enum AntiAlias {
    None,
    Fxaa(FXAA),
    Smaa(SMAA),
}

/// A presentation layer.
///
/// Corresponds to `OpenGL::Layer`.
pub struct Layer {
    // Rust drops fields in declaration order. Eden destroys the C++ members in
    // reverse declaration order: anti_alias, fsr, framebuffer_texture, then
    // gl_framebuffer_data.
    anti_alias: AntiAlias,
    fsr: Option<FSR>,
    framebuffer_texture: TextureInfo,
    gl_framebuffer_data: Vec<u8>,
    /// Upstream stores `RasterizerOpenGL& rasterizer` on `OpenGL::Layer`.
    ///
    /// Rust keeps the non-owning reference as a raw pointer because `RendererOpenGL`
    /// owns both the boxed rasterizer and the present pipeline. The rasterizer is
    /// heap-stable for the renderer lifetime, matching the C++ member reference.
    rasterizer: *mut RasterizerOpenGL,
    /// Upstream stores `Tegra::MaxwellDeviceMemoryManager& device_memory` on `OpenGL::Layer`.
    device_memory: DeviceMemoryReader,
    /// Presentation filter selector.
    ///
    /// Upstream stores `const PresentFilters& filters` on `OpenGL::Layer`.
    filters: &'static PresentFilters,
}

// Safety: `Layer` is owned by the OpenGL renderer/present pipeline. Its raw
// rasterizer pointer is non-owning and points to the boxed rasterizer owned by
// the same `RendererOpenGL`; access remains on the renderer thread.
unsafe impl Send for Layer {}

impl Layer {
    /// Create a new presentation layer.
    ///
    /// Port of `Layer::Layer()`.
    /// Creates an initial 1x1 RGBA8 texture, cleared to black.
    pub fn new(
        rasterizer: *mut RasterizerOpenGL,
        device_memory: DeviceMemoryReader,
        filters: &'static PresentFilters,
    ) -> Self {
        let mut texture = OGLTexture::new();
        texture.create(gl::TEXTURE_2D);
        unsafe {
            gl::TextureStorage2D(texture.handle, 1, gl::RGBA8, 1, 1);
            let black: [u8; 4] = [0, 0, 0, 0];
            gl::ClearTexImage(
                texture.handle,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                black.as_ptr() as *const _,
            );
        }

        Self {
            anti_alias: AntiAlias::None,
            fsr: None,
            framebuffer_texture: TextureInfo {
                resource: texture,
                width: 1,
                height: 1,
                gl_format: gl::RGBA,
                gl_type: gl::UNSIGNED_BYTE,
                pixel_format: AndroidPixelFormat::default(),
            },
            gl_framebuffer_data: Vec::new(),
            rasterizer,
            device_memory,
            filters,
        }
    }

    /// Configure the layer for drawing and return the display texture handle.
    ///
    /// Port of `Layer::ConfigureDraw()`.
    ///
    /// 1. Loads the framebuffer from emulated memory into the screen texture
    /// 2. Applies anti-aliasing (FXAA or SMAA) if enabled
    /// 3. Applies FSR upscaling if enabled
    /// 4. Fills the output matrix and vertex data
    /// 5. Returns the final texture handle to draw
    pub fn configure_draw(
        &mut self,
        out_matrix: &mut [f32; 6],
        out_vertices: &mut [ScreenRectVertex; 4],
        framebuffer: &FramebufferConfig,
        layout: &FramebufferLayout,
        invert_y: bool,
        program_manager: &mut ProgramManager,
    ) -> u32 {
        let info = self.prepare_render_target(framebuffer);
        let mut crop = framebuffer_config::normalize_crop(framebuffer, info.width, info.height);
        let mut texture = info.display_texture;

        let anti_aliasing = (self.filters.get_anti_aliasing)();
        if anti_aliasing != AntiAliasing::None {
            let resolution = common::settings::values().resolution_info.clone();
            let viewport_width = resolution.scale_up_i32(self.framebuffer_texture.width);
            let viewport_height = resolution.scale_up_i32(self.framebuffer_texture.height);
            unsafe {
                gl::Enablei(gl::SCISSOR_TEST, 0);
                gl::ScissorIndexed(0, 0, 0, viewport_width, viewport_height);
                gl::ViewportIndexedf(0, 0.0, 0.0, viewport_width as f32, viewport_height as f32);
            }

            match anti_aliasing {
                AntiAliasing::Fxaa => {
                    self.create_fxaa();
                    if let AntiAlias::Fxaa(fxaa) = &self.anti_alias {
                        texture = fxaa.draw(program_manager, info.display_texture);
                    }
                }
                AntiAliasing::Smaa => {
                    self.create_smaa();
                    if let AntiAlias::Smaa(smaa) = &self.anti_alias {
                        texture = smaa.draw(program_manager, info.display_texture);
                    }
                }
                AntiAliasing::None => {}
            }
        }

        unsafe {
            gl::Disablei(gl::SCISSOR_TEST, 0);
        }

        if (self.filters.get_scaling_filter)() == ScalingFilter::Fsr {
            let screen_width = layout.screen.get_width();
            let screen_height = layout.screen.get_height();
            if self.fsr.as_ref().map_or(true, |fsr| {
                fsr.needs_recreation(screen_width, screen_height)
            }) {
                self.fsr = Some(FSR::new(screen_width, screen_height));
            }
            if let Some(fsr) = &self.fsr {
                let fsr_texture = fsr.draw(
                    program_manager,
                    texture,
                    info.scaled_width,
                    info.scaled_height,
                    crop,
                );
                texture = fsr_texture;
                crop = Rectangle {
                    left: 0.0,
                    top: 0.0,
                    right: 1.0,
                    bottom: 1.0,
                };
            }
        }

        // Build orthographic projection matrix.
        *out_matrix = make_orthographic_matrix(layout.width as f32, layout.height as f32);

        // Map the coordinates to the screen.
        let x = layout.screen.left;
        let y = layout.screen.top;
        let w = layout.screen.right - layout.screen.left;
        let h = layout.screen.bottom - layout.screen.top;

        let left = crop.left;
        let right = crop.right;
        let top = if invert_y { crop.bottom } else { crop.top };
        let bottom = if invert_y { crop.top } else { crop.bottom };

        out_vertices[0] = ScreenRectVertex::new(x, y, left, top);
        out_vertices[1] = ScreenRectVertex::new(x + w, y, right, top);
        out_vertices[2] = ScreenRectVertex::new(x, y + h, left, bottom);
        out_vertices[3] = ScreenRectVertex::new(x + w, y + h, right, bottom);

        texture
    }

    /// Prepare the render target: reallocate texture if dimensions/format changed,
    /// then load framebuffer data.
    ///
    /// Port of `Layer::PrepareRenderTarget()`.
    fn prepare_render_target(&mut self, framebuffer: &FramebufferConfig) -> FramebufferTextureInfo {
        if self.framebuffer_texture.width != framebuffer.width as i32
            || self.framebuffer_texture.height != framebuffer.height as i32
            || self.framebuffer_texture.pixel_format != framebuffer.pixel_format
            || self.gl_framebuffer_data.is_empty()
        {
            self.configure_framebuffer_texture(framebuffer);
        }

        self.load_fb_to_screen_info(framebuffer)
    }

    /// Load the framebuffer from emulated memory into the screen texture.
    ///
    /// Port of `Layer::LoadFBToScreenInfo()`.
    ///
    /// Upstream flow:
    /// 1. Try rasterizer.AccelerateDisplay() — GPU-cached texture fast path
    /// 2. If miss: read from device_memory, unswizzle, upload via glTextureSubImage2D
    fn load_fb_to_screen_info(
        &mut self,
        framebuffer: &FramebufferConfig,
    ) -> FramebufferTextureInfo {
        let framebuffer_addr = framebuffer.address + framebuffer.offset as u64;

        let rasterizer = unsafe {
            self.rasterizer
                .as_mut()
                .expect("OpenGL Layer rasterizer pointer must remain valid")
        };
        if let Some(info) =
            rasterizer.accelerate_display(framebuffer, framebuffer_addr, framebuffer.stride)
        {
            return info;
        }

        let info = FramebufferTextureInfo {
            display_texture: self.framebuffer_texture.resource.handle,
            width: framebuffer.width,
            height: framebuffer.height,
            scaled_width: framebuffer.width,
            scaled_height: framebuffer.height,
        };

        let pixel_format =
            crate::surface::pixel_format_from_gpu_pixel_format(framebuffer.pixel_format);
        let bytes_per_pixel = crate::surface::bytes_per_block(pixel_format);

        // Calculate tiled buffer size.
        // Upstream hardcodes block_height_log2 = 4 (TODO: read from HLE).
        let block_height_log2: u32 = 4;
        let size_in_bytes = crate::textures::decoders::calculate_size(
            true, // tiled (Tegra block-linear)
            bytes_per_pixel,
            framebuffer.stride, // width in pixels
            framebuffer.height,
            1, // depth
            block_height_log2,
            0, // block_depth
        );

        // Read tiled framebuffer data from GPU memory.
        let mut tiled_data = vec![0u8; size_in_bytes];
        let host_ptr_available = (self.device_memory)(framebuffer_addr, &mut tiled_data);

        if host_ptr_available {
            // Unswizzle from Tegra block-linear to linear layout.
            let linear_size = (framebuffer.width * framebuffer.height * bytes_per_pixel) as usize;
            if self.gl_framebuffer_data.len() < linear_size {
                self.gl_framebuffer_data.resize(linear_size, 0);
            }
            crate::textures::decoders::unswizzle_texture(
                &mut self.gl_framebuffer_data[..linear_size],
                &tiled_data,
                bytes_per_pixel,
                framebuffer.width,
                framebuffer.height,
                1, // depth
                block_height_log2,
                0, // block_depth
                1, // stride_alignment
            );
        }

        // Upload to GL texture.
        unsafe {
            gl::BindBuffer(gl::PIXEL_UNPACK_BUFFER, 0);
            gl::PixelStorei(gl::UNPACK_ROW_LENGTH, framebuffer.stride as i32);
            gl::TextureSubImage2D(
                self.framebuffer_texture.resource.handle,
                0, // level
                0,
                0, // x, y offset
                framebuffer.width as i32,
                framebuffer.height as i32,
                self.framebuffer_texture.gl_format,
                self.framebuffer_texture.gl_type,
                self.gl_framebuffer_data.as_ptr() as *const _,
            );
            gl::PixelStorei(gl::UNPACK_ROW_LENGTH, 0);
        }

        info
    }

    /// Reconfigure the framebuffer texture for new dimensions/format.
    ///
    /// Port of `Layer::ConfigureFramebufferTexture()`.
    fn configure_framebuffer_texture(&mut self, framebuffer: &FramebufferConfig) {
        self.framebuffer_texture.width = framebuffer.width as i32;
        self.framebuffer_texture.height = framebuffer.height as i32;
        self.framebuffer_texture.pixel_format = framebuffer.pixel_format;

        let pixel_format =
            crate::surface::pixel_format_from_gpu_pixel_format(framebuffer.pixel_format);
        let bytes_per_pixel = crate::surface::bytes_per_block(pixel_format);

        let (internal_format, gl_format, gl_type, bytes_per_pixel) = match framebuffer.pixel_format
        {
            AndroidPixelFormat::Rgba8888 => (
                gl::RGBA8,
                gl::RGBA,
                gl::UNSIGNED_INT_8_8_8_8_REV,
                bytes_per_pixel,
            ),
            AndroidPixelFormat::Rgb565 => (
                gl::RGB565,
                gl::RGB,
                gl::UNSIGNED_SHORT_5_6_5,
                bytes_per_pixel,
            ),
            _ => (
                gl::RGBA8,
                gl::RGBA,
                gl::UNSIGNED_INT_8_8_8_8_REV,
                bytes_per_pixel,
            ),
        };

        self.framebuffer_texture.gl_format = gl_format;
        self.framebuffer_texture.gl_type = gl_type;

        // Allocate CPU buffer for unswizzled data.
        self.gl_framebuffer_data.resize(
            (framebuffer.width * framebuffer.height * bytes_per_pixel) as usize,
            0,
        );

        // Recreate the owned GL texture exactly as Eden's OGLTexture
        // Release/Create sequence.
        self.framebuffer_texture.resource.release();
        self.framebuffer_texture.resource.create(gl::TEXTURE_2D);
        unsafe {
            gl::TextureStorage2D(
                self.framebuffer_texture.resource.handle,
                1,
                internal_format,
                framebuffer.width as i32,
                framebuffer.height as i32,
            );
        }

        // Invalidate post-processors since dimensions changed.
        self.anti_alias = AntiAlias::None;
    }

    /// Create or recreate the FXAA pass.
    fn create_fxaa(&mut self) {
        if !matches!(self.anti_alias, AntiAlias::Fxaa(_)) {
            let resolution = common::settings::values().resolution_info.clone();
            self.anti_alias = AntiAlias::Fxaa(FXAA::new(
                resolution.scale_up_u32(self.framebuffer_texture.width as u32),
                resolution.scale_up_u32(self.framebuffer_texture.height as u32),
            ));
        }
    }

    /// Create or recreate the SMAA pass.
    fn create_smaa(&mut self) {
        if !matches!(self.anti_alias, AntiAlias::Smaa(_)) {
            let resolution = common::settings::values().resolution_info.clone();
            self.anti_alias = AntiAlias::Smaa(SMAA::new(
                resolution.scale_up_u32(self.framebuffer_texture.width as u32),
                resolution.scale_up_u32(self.framebuffer_texture.height as u32),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_info_owns_the_upstream_raii_texture_and_typed_format() {
        let info = TextureInfo {
            resource: OGLTexture::new(),
            width: 0,
            height: 0,
            gl_format: gl::NONE,
            gl_type: gl::NONE,
            pixel_format: AndroidPixelFormat::default(),
        };

        let _: &OGLTexture = &info.resource;
        let _: AndroidPixelFormat = info.pixel_format;
        assert!(std::mem::needs_drop::<TextureInfo>());
    }
}

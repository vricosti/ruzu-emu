// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden `video_core/renderer_opengl/present/fsr.{h,cpp}`.
//!
//! AMD FidelityFX Super Resolution (FSR) upscaling pass for OpenGL.

use super::util::{create_bilinear_sampler, replace_include};
use crate::fsr::{fsr_easu_con_offset, fsr_rcas_con};
use crate::host_shaders::fragment_shaders::{
    OPENGL_FIDELITYFX_FSR_EASU_FRAG, OPENGL_FIDELITYFX_FSR_FRAG, OPENGL_FIDELITYFX_FSR_RCAS_FRAG,
};
use crate::host_shaders::glsl_includes::{FFX_A_H, FFX_FSR1_H};
use crate::host_shaders::vertex_shaders::FULL_SCREEN_TRIANGLE_VERT;
use crate::renderer_opengl::gl_resource_manager::{
    OGLFramebuffer, OGLProgram, OGLSampler, OGLTexture,
};
use crate::renderer_opengl::gl_shader_manager::ProgramManager;
use crate::renderer_opengl::gl_shader_util::create_program_from_source;
use common::math_util::Rectangle;

type FsrConstants = [u32; 4 * 4];

/// FSR upscaling pass.
///
/// Corresponds to `OpenGL::FSR`.
pub struct FSR {
    // Rust drops fields in declaration order; resources are declared in
    // Eden's reverse-member destruction order.
    rcas_tex: OGLTexture,
    easu_tex: OGLTexture,
    rcas_frag: OGLProgram,
    easu_frag: OGLProgram,
    vert: OGLProgram,
    sampler: OGLSampler,
    framebuffer: OGLFramebuffer,
    width: u32,
    height: u32,
}

impl FSR {
    /// Create a new FSR pass with the given output dimensions.
    ///
    /// Corresponds to `FSR::FSR()`.
    ///
    /// Compiles the EASU/RCAS programs with their upstream includes and creates
    /// the sampler, framebuffer, and two RGBA16F targets.
    pub fn new(output_width: u32, output_height: u32) -> Self {
        let mut fsr_source = OPENGL_FIDELITYFX_FSR_FRAG.to_string();
        replace_include(&mut fsr_source, "ffx_a.h", FFX_A_H);
        replace_include(&mut fsr_source, "ffx_fsr1.h", FFX_FSR1_H);

        let mut fsr_easu_source = OPENGL_FIDELITYFX_FSR_EASU_FRAG.to_string();
        let mut fsr_rcas_source = OPENGL_FIDELITYFX_FSR_RCAS_FRAG.to_string();
        replace_include(
            &mut fsr_easu_source,
            "opengl_fidelityfx_fsr.frag",
            &fsr_source,
        );
        replace_include(
            &mut fsr_rcas_source,
            "opengl_fidelityfx_fsr.frag",
            &fsr_source,
        );

        let vert = create_program_from_source(FULL_SCREEN_TRIANGLE_VERT, gl::VERTEX_SHADER);
        let easu_frag = create_program_from_source(&fsr_easu_source, gl::FRAGMENT_SHADER);
        let rcas_frag = create_program_from_source(&fsr_rcas_source, gl::FRAGMENT_SHADER);

        unsafe {
            gl::ProgramUniform2f(vert.handle, 0, 1.0, -1.0);
            gl::ProgramUniform2f(vert.handle, 1, 0.0, 1.0);
        }
        let sampler = create_bilinear_sampler();
        let mut framebuffer = OGLFramebuffer::new();
        framebuffer.create();
        let mut easu_tex = OGLTexture::new();
        easu_tex.create(gl::TEXTURE_2D);
        unsafe {
            gl::TextureStorage2D(
                easu_tex.handle,
                1,
                gl::RGBA16F,
                output_width as i32,
                output_height as i32,
            );
        }
        let mut rcas_tex = OGLTexture::new();
        rcas_tex.create(gl::TEXTURE_2D);
        unsafe {
            gl::TextureStorage2D(
                rcas_tex.handle,
                1,
                gl::RGBA16F,
                output_width as i32,
                output_height as i32,
            );
        }

        Self {
            rcas_tex,
            easu_tex,
            rcas_frag,
            easu_frag,
            vert,
            sampler,
            framebuffer,
            width: output_width,
            height: output_height,
        }
    }

    /// Execute the FSR pass and return the output texture handle.
    ///
    /// Corresponds to `FSR::Draw()`.
    ///
    /// The two passes are:
    /// 1. EASU (Edge Adaptive Spatial Upsampling) — scales the input to output resolution
    /// 2. RCAS (Robust Contrast Adaptive Sharpening) — sharpens the upscaled result
    pub fn draw(
        &self,
        program_manager: &mut ProgramManager,
        input_texture: u32,
        input_image_width: u32,
        input_image_height: u32,
        crop_rect: Rectangle<f32>,
    ) -> u32 {
        let input_width = input_image_width as f32;
        let input_height = input_image_height as f32;
        let output_width = self.width as f32;
        let output_height = self.height as f32;
        let viewport_width = (crop_rect.right - crop_rect.left) * input_width;
        let viewport_x = crop_rect.left * input_width;
        let viewport_height = (crop_rect.bottom - crop_rect.top) * input_height;
        let viewport_y = crop_rect.top * input_height;

        let mut easu_con: FsrConstants = [0; 4 * 4];
        let mut rcas_con: FsrConstants = [0; 4 * 4];
        {
            let (con0, rest) = easu_con.split_at_mut(4);
            let (con1, rest) = rest.split_at_mut(4);
            let (con2, con3) = rest.split_at_mut(4);
            fsr_easu_con_offset(
                con0.try_into().unwrap(),
                con1.try_into().unwrap(),
                con2.try_into().unwrap(),
                con3.try_into().unwrap(),
                viewport_width,
                viewport_height,
                input_width,
                input_height,
                output_width,
                output_height,
                viewport_x,
                viewport_y,
            );
        }

        let sharpening =
            *common::settings::values().fsr_sharpening_slider.get_value() as f32 / 100.0;
        fsr_rcas_con((&mut rcas_con[..4]).try_into().unwrap(), sharpening);

        unsafe {
            gl::ProgramUniform4uiv(
                self.easu_frag.handle,
                0,
                std::mem::size_of_val(&easu_con) as i32,
                easu_con.as_ptr(),
            );
            gl::ProgramUniform4uiv(
                self.rcas_frag.handle,
                0,
                std::mem::size_of_val(&rcas_con) as i32,
                rcas_con.as_ptr(),
            );
            gl::FrontFace(gl::CW);
            gl::BindFramebuffer(gl::DRAW_FRAMEBUFFER, self.framebuffer.handle);

            // Pass 1: EASU upscaling
            gl::NamedFramebufferTexture(
                self.framebuffer.handle,
                gl::COLOR_ATTACHMENT0,
                self.easu_tex.handle,
                0,
            );
            gl::ViewportIndexedf(0, 0.0, 0.0, output_width, output_height);
            program_manager.bind_present_programs(self.vert.handle, self.easu_frag.handle);
            gl::BindTextureUnit(0, input_texture);
            gl::BindSampler(0, self.sampler.handle);
            gl::DrawArrays(gl::TRIANGLES, 0, 3);

            // Pass 2: RCAS sharpening
            gl::NamedFramebufferTexture(
                self.framebuffer.handle,
                gl::COLOR_ATTACHMENT0,
                self.rcas_tex.handle,
                0,
            );
            program_manager.bind_present_programs(self.vert.handle, self.rcas_frag.handle);
            gl::BindTextureUnit(0, self.easu_tex.handle);
            gl::DrawArrays(gl::TRIANGLES, 0, 3);
        }

        self.rcas_tex.handle
    }

    /// Check if the FSR pass needs to be recreated for new screen dimensions.
    ///
    /// Corresponds to `FSR::NeedsRecreation()`.
    pub fn needs_recreation(&self, screen_width: u32, screen_height: u32) -> bool {
        screen_width != self.width || screen_height != self.height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_count_matches_upstream_sizeof_expression() {
        assert_eq!(std::mem::size_of::<FsrConstants>(), 64);
    }
}

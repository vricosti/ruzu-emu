// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `video_core/renderer_opengl/gl_resource_manager.{h,cpp}`.

macro_rules! ogl_resource {
    ($name:ident, $create:ident, $delete:ident) => {
        #[derive(Default)]
        pub struct $name {
            pub handle: gl::types::GLuint,
        }

        impl $name {
            pub fn new() -> Self {
                Self::default()
            }

            pub fn create(&mut self) {
                if self.handle != 0 {
                    return;
                }
                unsafe {
                    gl::$create(1, &mut self.handle);
                }
            }

            pub fn release(&mut self) {
                if self.handle == 0 {
                    return;
                }
                unsafe {
                    gl::$delete(1, &self.handle);
                }
                self.handle = 0;
            }
        }

        impl Drop for $name {
            fn drop(&mut self) {
                self.release();
            }
        }
    };
}

ogl_resource!(OGLRenderbuffer, CreateRenderbuffers, DeleteRenderbuffers);

#[derive(Default)]
pub struct OGLTexture {
    pub handle: gl::types::GLuint,
}

impl OGLTexture {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(&mut self, target: gl::types::GLenum) {
        if self.handle != 0 {
            return;
        }
        unsafe {
            gl::CreateTextures(target, 1, &mut self.handle);
        }
    }

    pub fn release(&mut self) {
        if self.handle == 0 {
            return;
        }
        unsafe {
            gl::DeleteTextures(1, &self.handle);
        }
        self.handle = 0;
    }
}

impl Drop for OGLTexture {
    fn drop(&mut self) {
        self.release();
    }
}

ogl_resource!(OGLTextureView, GenTextures, DeleteTextures);
ogl_resource!(OGLSampler, CreateSamplers, DeleteSamplers);

#[derive(Default)]
pub struct OGLShader {
    pub handle: gl::types::GLuint,
}

impl OGLShader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn release(&mut self) {
        if self.handle == 0 {
            return;
        }
        unsafe {
            gl::DeleteShader(self.handle);
        }
        self.handle = 0;
    }
}

impl Drop for OGLShader {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Default)]
pub struct OGLProgram {
    pub handle: gl::types::GLuint,
}

impl OGLProgram {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn release(&mut self) {
        if self.handle == 0 {
            return;
        }
        unsafe {
            gl::DeleteProgram(self.handle);
        }
        self.handle = 0;
    }
}

impl Drop for OGLProgram {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Default)]
pub struct OGLAssemblyProgram {
    pub handle: gl::types::GLuint,
}

impl OGLAssemblyProgram {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn release(&mut self) {
        if self.handle == 0 {
            return;
        }
        super::gl_shader_util::delete_assembly_program(self.handle);
        self.handle = 0;
    }
}

impl Drop for OGLAssemblyProgram {
    fn drop(&mut self) {
        self.release();
    }
}

ogl_resource!(OGLPipeline, GenProgramPipelines, DeleteProgramPipelines);
ogl_resource!(OGLBuffer, CreateBuffers, DeleteBuffers);

pub struct OGLSync {
    pub handle: gl::types::GLsync,
}

impl Default for OGLSync {
    fn default() -> Self {
        Self {
            handle: std::ptr::null(),
        }
    }
}

impl OGLSync {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(&mut self) {
        if !self.handle.is_null() {
            return;
        }
        unsafe {
            self.handle = gl::FenceSync(gl::SYNC_GPU_COMMANDS_COMPLETE, 0);
        }
    }

    pub fn release(&mut self) {
        if self.handle.is_null() {
            return;
        }
        unsafe {
            gl::DeleteSync(self.handle);
        }
        self.handle = std::ptr::null();
    }

    pub fn is_signaled(&self) -> bool {
        let sync_status = unsafe { gl::ClientWaitSync(self.handle, 0, 0) };
        sync_status_is_signaled(
            sync_status,
            *common::settings::values().use_debug_asserts.get_value(),
        )
    }
}

/// Testable body of Eden's fail-soft assertion and completion check in
/// `OGLSync::IsSignaled`.
fn sync_status_is_signaled(sync_status: u32, debug_asserts_enabled: bool) -> bool {
    if sync_status == gl::WAIT_FAILED {
        log::error!("OGLSync::IsSignaled: glClientWaitSync returned GL_WAIT_FAILED");
        if debug_asserts_enabled {
            panic!("OGLSync::IsSignaled: glClientWaitSync returned GL_WAIT_FAILED");
        }
    }
    sync_status != gl::TIMEOUT_EXPIRED
}

impl Drop for OGLSync {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Default)]
pub struct OGLFramebuffer {
    pub handle: gl::types::GLuint,
}

impl OGLFramebuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(&mut self) {
        if self.handle != 0 {
            return;
        }
        unsafe {
            // Binding here forces Nvidia to create a core framebuffer instead of an EXT one.
            gl::GenFramebuffers(1, &mut self.handle);
            gl::BindFramebuffer(gl::READ_FRAMEBUFFER, self.handle);
        }
    }

    pub fn release(&mut self) {
        if self.handle == 0 {
            return;
        }
        unsafe {
            gl::DeleteFramebuffers(1, &self.handle);
        }
        self.handle = 0;
    }
}

impl Drop for OGLFramebuffer {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Default)]
pub struct OGLQuery {
    pub handle: gl::types::GLuint,
}

impl OGLQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(&mut self, target: gl::types::GLenum) {
        if self.handle != 0 {
            return;
        }
        unsafe {
            gl::CreateQueries(target, 1, &mut self.handle);
        }
    }

    pub fn release(&mut self) {
        if self.handle == 0 {
            return;
        }
        unsafe {
            gl::DeleteQueries(1, &self.handle);
        }
        self.handle = 0;
    }
}

impl Drop for OGLQuery {
    fn drop(&mut self) {
        self.release();
    }
}

ogl_resource!(
    OGLTransformFeedback,
    CreateTransformFeedbacks,
    DeleteTransformFeedbacks
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_wrappers_start_without_a_gl_handle() {
        assert_eq!(OGLRenderbuffer::new().handle, 0);
        assert_eq!(OGLTexture::new().handle, 0);
        assert_eq!(OGLTextureView::new().handle, 0);
        assert_eq!(OGLSampler::new().handle, 0);
        assert_eq!(OGLShader::new().handle, 0);
        assert_eq!(OGLProgram::new().handle, 0);
        assert_eq!(OGLAssemblyProgram::new().handle, 0);
        assert_eq!(OGLPipeline::new().handle, 0);
        assert_eq!(OGLBuffer::new().handle, 0);
        assert!(OGLSync::new().handle.is_null());
        assert_eq!(OGLFramebuffer::new().handle, 0);
        assert_eq!(OGLQuery::new().handle, 0);
        assert_eq!(OGLTransformFeedback::new().handle, 0);
    }

    #[test]
    fn sync_status_matches_upstream_timeout_and_fail_soft_behavior() {
        assert!(!sync_status_is_signaled(gl::TIMEOUT_EXPIRED, false));
        assert!(sync_status_is_signaled(gl::ALREADY_SIGNALED, false));
        assert!(sync_status_is_signaled(gl::WAIT_FAILED, false));

        let fatal_wait_failure = std::panic::catch_unwind(|| {
            sync_status_is_signaled(gl::WAIT_FAILED, true);
        });
        assert!(fatal_wait_failure.is_err());
    }
}

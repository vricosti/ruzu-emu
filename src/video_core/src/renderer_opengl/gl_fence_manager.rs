// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `video_core/renderer_opengl/gl_fence_manager.{h,cpp}`.
//!
//! OpenGL fence manager — manages GPU synchronization fences.

use std::sync::Arc;

use super::gl_resource_manager::OGLSync;
use crate::fence_manager::{FenceBase, FenceManager};

/// An OpenGL sync fence.
///
/// Corresponds to `OpenGL::GLInnerFence`.
pub struct GLInnerFence {
    /// Whether this fence is stubbed (no actual GL sync).
    is_stubbed: bool,
    /// GL sync object wrapper.
    sync_object: OGLSync,
}

impl GLInnerFence {
    /// Create a new fence.
    ///
    /// Corresponds to `GLInnerFence::GLInnerFence()`.
    pub fn new(is_stubbed: bool) -> Self {
        Self {
            is_stubbed,
            sync_object: OGLSync::new(),
        }
    }

    /// Queue the fence into the GL command stream.
    ///
    /// Corresponds to `GLInnerFence::Queue()`.
    pub fn queue(&mut self) {
        if self.is_stubbed {
            return;
        }
        if !self.sync_object.handle.is_null() {
            log::error!("GLInnerFence::Queue assertion failed: sync object is already queued");
        }
        self.sync_object.create();
    }

    /// Check if the fence has been signaled.
    ///
    /// Corresponds to `GLInnerFence::IsSignaled()`.
    pub fn is_signaled(&self) -> bool {
        if self.is_stubbed {
            return true;
        }
        if self.sync_object.handle.is_null() {
            log::error!("GLInnerFence::IsSignaled assertion failed: sync object is not queued");
        }
        self.sync_object.is_signaled()
    }

    /// Wait for the fence to be signaled.
    ///
    /// Corresponds to `GLInnerFence::Wait()`.
    pub fn wait(&self) {
        if self.is_stubbed {
            return;
        }
        if self.sync_object.handle.is_null() {
            log::error!("GLInnerFence::Wait assertion failed: sync object is not queued");
        }
        unsafe {
            gl::ClientWaitSync(self.sync_object.handle, 0, gl::TIMEOUT_IGNORED);
        }
    }
}

/// Shared fence type.
///
/// Corresponds to `OpenGL::Fence = std::shared_ptr<GLInnerFence>`.
pub type Fence = Arc<std::sync::Mutex<GLInnerFence>>;

impl FenceBase for Fence {
    fn is_stubbed(&self) -> bool {
        self.lock().unwrap().is_stubbed
    }

    fn wait_for_fence(&self) {
        self.lock().unwrap().wait();
    }
}

unsafe impl Send for GLInnerFence {}
unsafe impl Sync for GLInnerFence {}

/// OpenGL fence manager.
///
/// Corresponds to `OpenGL::FenceManagerOpenGL`.
pub struct FenceManagerOpenGL {
    generic: FenceManager<Fence>,
    #[cfg(test)]
    force_stubbed_fences: bool,
}

impl FenceManagerOpenGL {
    /// Create a new fence manager.
    pub fn new() -> Self {
        Self {
            generic: FenceManager::new(false),
            #[cfg(test)]
            force_stubbed_fences: false,
        }
    }

    #[cfg(test)]
    pub fn new_for_test() -> Self {
        Self {
            generic: FenceManager::new(false),
            force_stubbed_fences: true,
        }
    }

    #[cfg(test)]
    fn force_stubbed_for_test(&self) -> bool {
        self.force_stubbed_fences
    }

    #[cfg(not(test))]
    fn force_stubbed_for_test(&self) -> bool {
        false
    }

    /// Corresponds to `FenceManagerOpenGL::CreateFence()`.
    fn create_fence(is_stubbed: bool) -> Fence {
        Arc::new(std::sync::Mutex::new(GLInnerFence::new(is_stubbed)))
    }

    /// Corresponds to `FenceManagerOpenGL::QueueFence()`.
    fn queue_fence(fence: &mut Fence) {
        fence.lock().unwrap().queue();
    }

    /// Corresponds to `FenceManagerOpenGL::IsFenceSignaled()`.
    fn is_fence_signaled(fence: &Fence) -> bool {
        fence.lock().unwrap().is_signaled()
    }

    /// Corresponds to `FenceManagerOpenGL::WaitFence()`.
    fn wait_fence(fence: &Fence) {
        fence.lock().unwrap().wait();
    }

    pub fn tick_frame(&mut self) {
        self.generic.tick_frame();
    }

    pub fn sync_operation(&mut self, func: Box<dyn FnOnce() + Send>) {
        self.generic.sync_operation(func);
    }

    pub fn signal_ordering<FSW, FPF, FAF>(
        &mut self,
        should_wait_async_flushes: FSW,
        pop_async_flushes: FPF,
        accumulate_flushes: FAF,
    ) where
        FSW: FnMut() -> bool,
        FPF: FnMut() + Send + 'static,
        FAF: FnMut(),
    {
        self.generic.signal_ordering(
            should_wait_async_flushes,
            Self::is_fence_signaled,
            pop_async_flushes,
            accumulate_flushes,
        );
    }

    pub fn signal_reference<FSW, FPF, FSHF, FCAF, FFL, FINV>(
        &mut self,
        should_wait_async_flushes: FSW,
        pop_async_flushes: FPF,
        should_flush: FSHF,
        commit_async_flushes: FCAF,
        flush_commands: FFL,
        invalidate_gpu_cache: FINV,
    ) where
        FSW: FnMut() -> bool,
        FPF: FnMut() + Send + 'static,
        FSHF: FnMut() -> bool,
        FCAF: FnMut(),
        FFL: FnMut(),
        FINV: FnMut(),
    {
        let force_stubbed = self.force_stubbed_for_test();
        self.generic.signal_reference(
            move |is_stubbed| Self::create_fence(is_stubbed || force_stubbed),
            Self::queue_fence,
            should_wait_async_flushes,
            Self::is_fence_signaled,
            pop_async_flushes,
            should_flush,
            commit_async_flushes,
            flush_commands,
            invalidate_gpu_cache,
        );
    }

    pub fn signal_fence<FSW, FPF, FSHF, FCAF, FFL, FINV>(
        &mut self,
        func: Box<dyn FnOnce() + Send>,
        should_wait_async_flushes: FSW,
        pop_async_flushes: FPF,
        should_flush: FSHF,
        commit_async_flushes: FCAF,
        flush_commands: FFL,
        invalidate_gpu_cache: FINV,
    ) where
        FSW: FnMut() -> bool,
        FPF: FnMut() + Send + 'static,
        FSHF: FnMut() -> bool,
        FCAF: FnMut(),
        FFL: FnMut(),
        FINV: FnMut(),
    {
        let force_stubbed = self.force_stubbed_for_test();
        self.generic.signal_fence(
            func,
            move |is_stubbed| Self::create_fence(is_stubbed || force_stubbed),
            Self::queue_fence,
            should_wait_async_flushes,
            Self::is_fence_signaled,
            pop_async_flushes,
            should_flush,
            commit_async_flushes,
            flush_commands,
            invalidate_gpu_cache,
        );
    }

    pub fn signal_sync_point<FG, FH, FSW, FPF, FSHF, FCAF, FFL, FINV>(
        &mut self,
        value: u32,
        increment_guest: FG,
        increment_host: FH,
        should_wait_async_flushes: FSW,
        pop_async_flushes: FPF,
        should_flush: FSHF,
        commit_async_flushes: FCAF,
        flush_commands: FFL,
        invalidate_gpu_cache: FINV,
    ) where
        FG: FnMut(u32),
        FH: FnMut(u32) + Send + 'static,
        FSW: FnMut() -> bool,
        FPF: FnMut() + Send + 'static,
        FSHF: FnMut() -> bool,
        FCAF: FnMut(),
        FFL: FnMut(),
        FINV: FnMut(),
    {
        let force_stubbed = self.force_stubbed_for_test();
        self.generic.signal_sync_point(
            value,
            increment_guest,
            increment_host,
            move |is_stubbed| Self::create_fence(is_stubbed || force_stubbed),
            Self::queue_fence,
            should_wait_async_flushes,
            Self::is_fence_signaled,
            pop_async_flushes,
            should_flush,
            commit_async_flushes,
            flush_commands,
            invalidate_gpu_cache,
        );
    }

    pub fn wait_pending_fences<FSW, FPF, FSHF, FCAF, FFL, FINV>(
        &mut self,
        force: bool,
        should_wait_async_flushes: FSW,
        pop_async_flushes: FPF,
        should_flush: FSHF,
        commit_async_flushes: FCAF,
        flush_commands: FFL,
        invalidate_gpu_cache: FINV,
    ) where
        FSW: FnMut() -> bool,
        FPF: FnMut() + Send + 'static,
        FSHF: FnMut() -> bool,
        FCAF: FnMut(),
        FFL: FnMut(),
        FINV: FnMut(),
    {
        let force_stubbed = self.force_stubbed_for_test();
        self.generic.wait_pending_fences(
            force,
            move |is_stubbed| Self::create_fence(is_stubbed || force_stubbed),
            Self::queue_fence,
            should_wait_async_flushes,
            Self::is_fence_signaled,
            Self::wait_fence,
            pop_async_flushes,
            should_flush,
            commit_async_flushes,
            flush_commands,
            invalidate_gpu_cache,
        );
    }

    #[cfg(test)]
    pub(crate) fn queued_fence_count(&self) -> usize {
        self.generic.queued_fence_count()
    }
}

#[cfg(test)]
mod tests {
    use super::GLInnerFence;

    #[test]
    fn stubbed_fence_is_immediately_signaled_and_noop() {
        let mut fence = GLInnerFence::new(true);
        fence.queue();
        assert!(fence.is_signaled());
        fence.wait();
    }
}

// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden `video_core/renderer_opengl/gl_staging_buffer_pool.{h,cpp}`.
//!
//! OpenGL staging buffer pool -- manages persistent mapped buffers for CPU-GPU transfers.

use std::sync::Arc;

use parking_lot::Mutex;

use super::gl_resource_manager::{OGLBuffer, OGLSync};

pub(crate) type SharedStagingBufferPool = Arc<Mutex<StagingBufferPool>>;

pub(crate) fn make_shared_staging_buffer_pool() -> SharedStagingBufferPool {
    Arc::new(Mutex::new(StagingBufferPool::new()))
}

/// A mapped region from a staging buffer.
///
/// Corresponds to `OpenGL::StagingBufferMap`.
pub struct StagingBufferMap {
    /// Pointer to the mapped memory.
    pub mapped_ptr: *mut u8,
    /// Size of the mapped region.
    pub mapped_size: usize,
    /// Offset within the staging buffer.
    pub offset: usize,
    pub sync: *mut OGLSync,
    /// GL buffer handle.
    pub buffer: u32,
    /// Index in the alloc array (for freeing deferred buffers).
    pub index: usize,
}

impl StagingBufferMap {
    pub fn mapped_span_mut(&mut self) -> &mut [u8] {
        if self.mapped_size == 0 {
            return &mut [];
        }
        assert!(!self.mapped_ptr.is_null());
        unsafe { std::slice::from_raw_parts_mut(self.mapped_ptr, self.mapped_size) }
    }

    pub fn mapped_span(&self) -> &[u8] {
        if self.mapped_size == 0 {
            return &[];
        }
        assert!(!self.mapped_ptr.is_null());
        unsafe { std::slice::from_raw_parts(self.mapped_ptr, self.mapped_size) }
    }
}

impl crate::buffer_cache::buffer_cache_base::BufferCacheAsyncBuffer for StagingBufferMap {
    fn offset(&self) -> u64 {
        self.offset as u64
    }

    fn mapped_span(&self) -> &[u8] {
        StagingBufferMap::mapped_span(self)
    }

    fn mapped_span_mut(&mut self) -> &mut [u8] {
        StagingBufferMap::mapped_span_mut(self)
    }

    #[cfg(test)]
    fn empty_for_test() -> Self {
        Self {
            mapped_ptr: std::ptr::null_mut(),
            mapped_size: 0,
            offset: 0,
            buffer: 0,
            index: usize::MAX,
            sync: std::ptr::null_mut(),
        }
    }
}

impl Drop for StagingBufferMap {
    fn drop(&mut self) {
        if self.sync.is_null() {
            return;
        }
        unsafe {
            (*self.sync).create();
        }
    }
}

/// A single staging buffer allocation.
pub struct StagingBufferAlloc {
    // Rust drops fields in declaration order. Eden destroys `buffer` before
    // `sync` because C++ members are destroyed in reverse declaration order.
    pub buffer: OGLBuffer,
    pub sync: OGLSync,
    pub map: *mut u8,
    pub size: usize,
    pub sync_index: usize,
    pub deferred: bool,
}

/// A collection of staging buffers for a given access pattern.
///
/// Corresponds to `OpenGL::StagingBuffers`.
pub struct StagingBuffers {
    pub allocs: Vec<StagingBufferAlloc>,
    pub storage_flags: u32,
    pub map_flags: u32,
    pub current_sync_index: usize,
}

impl StagingBuffers {
    /// Create a new staging buffer collection.
    pub fn new(storage_flags: u32, map_flags: u32) -> Self {
        Self {
            allocs: Vec::new(),
            storage_flags,
            map_flags,
            current_sync_index: 0,
        }
    }

    /// Request a mapped staging buffer of the given size.
    ///
    /// Port of `StagingBuffers::RequestMap`.
    pub fn request_map(
        &mut self,
        requested_size: usize,
        insert_fence: bool,
        deferred: bool,
    ) -> StagingBufferMap {
        let index = self.request_buffer(requested_size);
        let alloc = &mut self.allocs[index];

        if insert_fence {
            self.current_sync_index = self.current_sync_index.wrapping_add(1);
            alloc.sync_index = self.current_sync_index;
        } else {
            alloc.sync_index = 0;
        }
        alloc.deferred = deferred;

        StagingBufferMap {
            mapped_ptr: alloc.map,
            mapped_size: requested_size,
            offset: 0,
            buffer: alloc.buffer.handle,
            index,
            sync: if insert_fence {
                &mut alloc.sync as *mut _
            } else {
                std::ptr::null_mut()
            },
        }
    }

    /// Free a deferred staging buffer.
    ///
    /// Port of `StagingBuffers::FreeDeferredStagingBuffer`.
    pub fn free_deferred_staging_buffer(&mut self, index: usize) {
        if !self.allocs[index].deferred {
            log::error!(
                "StagingBuffers::FreeDeferredStagingBuffer: allocation {index} is not deferred"
            );
        }
        self.allocs[index].deferred = false;
    }

    /// Request or allocate a buffer of the given size.
    ///
    /// Port of `StagingBuffers::RequestBuffer`.
    pub fn request_buffer(&mut self, requested_size: usize) -> usize {
        if let Some(index) = self.find_buffer(requested_size) {
            return index;
        }

        let mut alloc = StagingBufferAlloc {
            buffer: OGLBuffer::new(),
            sync: OGLSync::new(),
            map: std::ptr::null_mut(),
            size: 0,
            sync_index: 0,
            deferred: false,
        };
        alloc.buffer.create();
        let next_pow2_size = common::bit_util::next_pow2_u64(requested_size as u64) as usize;
        let persistent_flags = self.storage_flags | gl::MAP_PERSISTENT_BIT;
        let persistent_map_flags = self.map_flags | gl::MAP_PERSISTENT_BIT;

        unsafe {
            gl::NamedBufferStorage(
                alloc.buffer.handle,
                next_pow2_size as isize,
                std::ptr::null(),
                persistent_flags,
            );
            alloc.map = gl::MapNamedBufferRange(
                alloc.buffer.handle,
                0,
                next_pow2_size as isize,
                persistent_map_flags,
            ) as *mut u8;
        }
        debug_assert!(!alloc.map.is_null());
        alloc.size = next_pow2_size;

        self.allocs.push(alloc);
        self.allocs.len() - 1
    }

    /// Find an existing free buffer that fits the requested size.
    ///
    /// Port of `StagingBuffers::FindBuffer`.
    pub fn find_buffer(&mut self, requested_size: usize) -> Option<usize> {
        let mut known_unsignaled_index = self.current_sync_index.wrapping_add(1);
        let mut smallest_buffer = usize::MAX;
        let mut found: Option<usize> = None;

        for index in 0..self.allocs.len() {
            let buffer_size = self.allocs[index].size;
            if buffer_size < requested_size || buffer_size >= smallest_buffer {
                continue;
            }
            if self.allocs[index].deferred {
                continue;
            }
            if !self.allocs[index].sync.handle.is_null() {
                let sync_index = self.allocs[index].sync_index;
                if sync_index >= known_unsignaled_index {
                    continue;
                }
                if !self.allocs[index].sync.is_signaled() {
                    known_unsignaled_index = known_unsignaled_index.min(sync_index);
                    continue;
                }
                self.allocs[index].sync.release();
            }
            smallest_buffer = buffer_size;
            found = Some(index);
        }
        found
    }
}

/// A persistent mapped stream buffer for uniform data.
///
/// Corresponds to `OpenGL::StreamBuffer`.
pub struct StreamBuffer {
    // Rust drops owning fields in declaration order, matching Eden's reverse
    // C++ member destruction: fences before buffer.
    fences: [OGLSync; StreamBuffer::NUM_SYNCS],
    buffer: OGLBuffer,
    iterator: usize,
    used_iterator: usize,
    free_iterator: usize,
    mapped_pointer: *mut u8,
}

// SAFETY: The GL handles and mapped pointers are only accessed on the GL thread.
unsafe impl Send for StreamBuffer {}

impl StreamBuffer {
    /// Stream buffer size (64 MiB).
    const STREAM_BUFFER_SIZE: usize = 64 * 1024 * 1024;

    /// Number of sync regions in the stream buffer.
    const NUM_SYNCS: usize = 16;

    /// Size of each sync region.
    const REGION_SIZE: usize = Self::STREAM_BUFFER_SIZE / Self::NUM_SYNCS;

    /// Maximum alignment for stream buffer requests.
    const MAX_ALIGNMENT: usize = 256;

    /// Create a new stream buffer.
    ///
    /// Port of `StreamBuffer::StreamBuffer`.
    pub fn new() -> Self {
        let mut buffer = OGLBuffer::new();
        let mut fences = std::array::from_fn(|_| OGLSync::new());
        buffer.create();
        let mapped_pointer: *mut u8;
        let flags = gl::MAP_WRITE_BIT | gl::MAP_PERSISTENT_BIT | gl::MAP_COHERENT_BIT;

        unsafe {
            gl::ObjectLabel(gl::BUFFER, buffer.handle, -1, c"Stream Buffer".as_ptr());
            gl::NamedBufferStorage(
                buffer.handle,
                Self::STREAM_BUFFER_SIZE as isize,
                std::ptr::null(),
                flags,
            );
            mapped_pointer =
                gl::MapNamedBufferRange(buffer.handle, 0, Self::STREAM_BUFFER_SIZE as isize, flags)
                    as *mut u8;
        }

        for fence in &mut fences {
            fence.create();
        }

        Self {
            fences,
            buffer,
            iterator: 0,
            used_iterator: 0,
            free_iterator: 0,
            mapped_pointer,
        }
    }

    /// Request a region of the stream buffer.
    ///
    /// Returns (mapped slice pointer, offset).
    ///
    /// Port of `StreamBuffer::Request`.
    pub fn request(&mut self, size: usize) -> (*mut u8, usize) {
        if size >= Self::REGION_SIZE {
            log::error!(
                "StreamBuffer::Request size {size} must be smaller than region size {}",
                Self::REGION_SIZE
            );
        }

        // Create fences for used regions
        let region_start = Self::region(self.used_iterator);
        let region_end = Self::region(self.iterator);
        for region in region_start..region_end {
            self.fences[region].create();
        }
        self.used_iterator = self.iterator;

        // Wait for regions we're about to overwrite
        let wait_start = Self::region(self.free_iterator).wrapping_add(1);
        let request_end = self.iterator.wrapping_add(size);
        let wait_end = Self::region(request_end)
            .wrapping_add(1)
            .min(Self::NUM_SYNCS);
        for region in wait_start..wait_end {
            unsafe {
                gl::ClientWaitSync(self.fences[region].handle, 0, gl::TIMEOUT_IGNORED);
            }
            self.fences[region].release();
        }
        if request_end >= self.free_iterator {
            self.free_iterator = request_end;
        }

        // Wrap around if needed
        if request_end > Self::STREAM_BUFFER_SIZE {
            for region in Self::region(self.used_iterator)..Self::NUM_SYNCS {
                self.fences[region].create();
            }
            self.used_iterator = 0;
            self.iterator = 0;
            self.free_iterator = size;

            for region in 0..=Self::region(size) {
                unsafe {
                    gl::ClientWaitSync(self.fences[region].handle, 0, gl::TIMEOUT_IGNORED);
                }
                self.fences[region].release();
            }
        }

        let offset = self.iterator;
        // Align up to MAX_ALIGNMENT
        self.iterator = self
            .iterator
            .wrapping_add(size)
            .wrapping_add(Self::MAX_ALIGNMENT - 1)
            & !(Self::MAX_ALIGNMENT - 1);

        let ptr = unsafe { self.mapped_pointer.add(offset) };
        (ptr, offset)
    }

    /// Get the GL buffer handle.
    pub fn handle(&self) -> u32 {
        self.buffer.handle
    }

    fn region(offset: usize) -> usize {
        offset / Self::REGION_SIZE
    }
}

const _: () = assert!(StreamBuffer::STREAM_BUFFER_SIZE % StreamBuffer::MAX_ALIGNMENT == 0);
const _: () = assert!(StreamBuffer::STREAM_BUFFER_SIZE % StreamBuffer::NUM_SYNCS == 0);
const _: () = assert!(StreamBuffer::REGION_SIZE % StreamBuffer::MAX_ALIGNMENT == 0);

/// Top-level staging buffer pool.
///
/// Corresponds to `OpenGL::StagingBufferPool`.
pub struct StagingBufferPool {
    // Eden's C++ members are destroyed in reverse declaration order.
    download_buffers: StagingBuffers,
    upload_buffers: StagingBuffers,
}

// The rasterizer transfers this GL-thread-owned pool with the renderer. Access
// by the texture and buffer runtimes is serialized by SharedStagingBufferPool.
unsafe impl Send for StagingBufferPool {}

impl StagingBufferPool {
    /// Create a new staging buffer pool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request an upload staging buffer.
    ///
    /// Port of `StagingBufferPool::RequestUploadBuffer`.
    pub fn request_upload_buffer(&mut self, size: usize) -> StagingBufferMap {
        self.upload_buffers.request_map(size, true, false)
    }

    /// Request a download staging buffer.
    ///
    /// Port of `StagingBufferPool::RequestDownloadBuffer`.
    pub fn request_download_buffer(&mut self, size: usize, deferred: bool) -> StagingBufferMap {
        self.download_buffers.request_map(size, false, deferred)
    }

    /// Free a deferred staging buffer.
    ///
    /// Port of `StagingBufferPool::FreeDeferredStagingBuffer`.
    pub fn free_deferred_staging_buffer(&mut self, buffer: &StagingBufferMap) {
        self.download_buffers
            .free_deferred_staging_buffer(buffer.index);
    }
}

impl Default for StagingBufferPool {
    fn default() -> Self {
        Self {
            download_buffers: StagingBuffers::new(
                gl::MAP_READ_BIT | gl::CLIENT_STORAGE_BIT,
                gl::MAP_READ_BIT,
            ),
            upload_buffers: StagingBuffers::new(
                gl::MAP_WRITE_BIT,
                gl::MAP_WRITE_BIT | gl::MAP_FLUSH_EXPLICIT_BIT,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants() {
        assert_eq!(StreamBuffer::STREAM_BUFFER_SIZE, 64 * 1024 * 1024);
        assert_eq!(StreamBuffer::NUM_SYNCS, 16);
        assert_eq!(
            StreamBuffer::REGION_SIZE,
            StreamBuffer::STREAM_BUFFER_SIZE / StreamBuffer::NUM_SYNCS
        );
        assert_eq!(StreamBuffer::MAX_ALIGNMENT, 256);
    }

    #[test]
    fn stream_buffer_region() {
        assert_eq!(StreamBuffer::region(0), 0);
        assert_eq!(StreamBuffer::region(StreamBuffer::REGION_SIZE - 1), 0);
        assert_eq!(StreamBuffer::region(StreamBuffer::REGION_SIZE), 1);
        assert_eq!(
            StreamBuffer::region(StreamBuffer::STREAM_BUFFER_SIZE - 1),
            StreamBuffer::NUM_SYNCS - 1
        );
    }

    #[test]
    fn free_deferred_staging_buffer_preserves_edens_fail_soft_assertion() {
        let mut buffers = StagingBuffers::new(0, 0);
        buffers.allocs.push(StagingBufferAlloc {
            buffer: OGLBuffer::new(),
            sync: OGLSync::new(),
            map: std::ptr::null_mut(),
            size: 1,
            sync_index: 0,
            deferred: false,
        });

        buffers.free_deferred_staging_buffer(0);
        assert!(!buffers.allocs[0].deferred);
    }

    #[test]
    fn shared_pool_clones_retain_one_rasterizer_owner() {
        let pool = make_shared_staging_buffer_pool();
        let texture_runtime_pool = Arc::clone(&pool);
        let buffer_runtime_pool = Arc::clone(&pool);

        assert!(Arc::ptr_eq(&pool, &texture_runtime_pool));
        assert!(Arc::ptr_eq(&pool, &buffer_runtime_pool));
    }
}

// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/texture_cache/texture_cache.h and texture_cache.cpp
//!
//! This file corresponds to the ~3 000-line template-method implementation
//! of `TextureCache<P>` from the upstream header.
//!
//! texture_cache.cpp itself is tiny (just explicit template instantiation
//! of `TextureCacheChannelInfo` and `ChannelSetupCaches`).  The real code
//! lives in the .h because it is template code in C++.
//!
//! In Rust the template is replaced by generic methods on
//! `TextureCacheBase<P>` (defined in texture_cache_base.rs). Backend-specific
//! construction and runtime operations are supplied by `TextureCacheParams`,
//! while the common control flow remains in this upstream-owned module.

use crate::cache_types::CacheType;
use crate::engines::draw_manager::Maxwell3DRenderTargets;
use crate::engines::maxwell_3d::RenderTargetInfo;
use crate::engines::maxwell_dma::dma;
use crate::framebuffer_config::{BlendMode, FramebufferConfig};
use crate::memory_manager::MemoryManager;
use crate::rasterizer_interface::RasterizerDownloadArea;
use crate::surface;
use common::hash::BuildUnorderedDenseHasher;
use parking_lot::Mutex as ParkingMutex;
use smallvec::SmallVec;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::format_lookup_table::PixelFormat;
use super::image_base::{
    add_image_alias, GPUVAddr, ImageAllocBase, ImageBase, ImageFlagBits, ImageMapView,
};
use super::image_info::ImageInfo;
#[cfg(test)]
use super::image_info::TilingMode;
use super::image_view_base::{ImageViewBase, ImageViewFlagBits};
use super::image_view_info::{ImageViewInfo, SwizzleSource};
use super::render_targets::RenderTargets;
use super::texture_cache_base::*;
use super::types::*;
use super::util::{convert_image, full_upload_swizzles, map_size_bytes, unswizzle_image};

// All method implementations live on TextureCacheBase.

#[derive(Debug, Clone, Copy)]
pub struct DmaBufferImageCopyResult {
    pub image_id: ImageId,
    pub copy: BufferImageCopy,
}

/// Rust access adapter for the persistent Maxwell3D dirty flags consumed by
/// upstream `TextureCache<P>::RescaleRenderTargets` and
/// `TextureCache<P>::UpdateRenderTargets`.
///
/// The renderer passes a draw-time register snapshot, but dirty-flag writes
/// still target the channel-owned Maxwell3D state just like upstream.
pub trait RenderTargetDirtyFlagAccess {
    fn render_target_dirty_flag(&self, flag: u8) -> bool;
    fn clear_render_target_dirty_flag(&mut self, flag: u8);
    fn set_render_target_dirty_flag(&mut self, flag: u8);
}

impl RenderTargetDirtyFlagAccess for [bool; 256] {
    fn render_target_dirty_flag(&self, flag: u8) -> bool {
        self[flag as usize]
    }

    fn clear_render_target_dirty_flag(&mut self, flag: u8) {
        self[flag as usize] = false;
    }

    fn set_render_target_dirty_flag(&mut self, flag: u8) {
        self[flag as usize] = true;
    }
}

impl<P: TextureCacheParams> TextureCacheBase<P> {
    /// Port of upstream `TextureCache<P>::RefreshContents`.
    pub fn refresh_contents(&mut self, image_id: ImageId) {
        if !self.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::CPU_MODIFIED)
        {
            return;
        }

        self.slot_images[image_id]
            .flags
            .remove(ImageFlagBits::CPU_MODIFIED);
        self.track_image(image_id);

        if self.slot_images[image_id].info.num_samples > 1 && !P::can_upload_msaa(self) {
            log::warn!("MSAA image uploads are not implemented");
            P::transition_image_layout(self, image_id);
            return;
        }
        if self.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::ASYNCHRONOUS_DECODE)
        {
            self.queue_async_decode(image_id);
            return;
        }
        let image = &self.slot_images[image_id];
        if *common::settings::values().gpu_unswizzle_enabled.get_value()
            && surface::is_pixel_format_bcn(image.info.format)
            && image.info.image_type == ImageType::E3D
            && image.info.resources.levels == 1
            && image.info.resources.layers == 1
            && map_size_bytes(image) as usize >= self.gpu_unswizzle_maxsize
            && !image.flags.contains(ImageFlagBits::GPU_MODIFIED)
        {
            self.queue_async_unswizzle(image_id);
            return;
        }

        let mut staging = P::upload_staging_buffer(
            self,
            map_size_bytes(&self.slot_images[image_id]) as usize,
            false,
        );
        self.upload_image_contents(image_id, &mut staging);
        P::insert_upload_memory_barrier(self);
    }

    /// Port of upstream `TextureCache<P>::UploadImageContents`.
    fn upload_image_contents(&mut self, image_id: ImageId, staging: &mut P::AsyncBuffer) {
        let (gpu_addr, guest_size_bytes, unswizzled_size_bytes, info, flags) = {
            let image = &self.slot_images[image_id];
            (
                image.gpu_addr,
                image.guest_size_bytes as usize,
                image.unswizzled_size_bytes as usize,
                image.info.clone(),
                image.flags,
            )
        };

        if flags.contains(ImageFlagBits::ACCELERATED_UPLOAD) {
            let gpu_memory = self
                .channel_gpu_memory
                .as_ref()
                .cloned()
                .expect("TextureCache::UploadImageContents requires bound channel GPU memory");
            gpu_memory.lock().read_block_with_cache_type(
                gpu_addr,
                P::staging_mapped_span(staging),
                CacheType::NO_TEXTURE_CACHE,
            );
            let uploads = full_upload_swizzles(&info);
            P::accelerate_image_upload(self, image_id, staging, &uploads, 0, 0);
            return;
        }

        let gpu_memory = self
            .channel_gpu_memory
            .as_ref()
            .cloned()
            .expect("TextureCache::UploadImageContents requires bound channel GPU memory");
        self.swizzle_data_buffer
            .resize_destructive(guest_size_bytes);
        gpu_memory
            .lock()
            .read_block_unsafe(gpu_addr, &mut self.swizzle_data_buffer);

        let copies = if flags.contains(ImageFlagBits::CONVERTED) {
            self.unswizzle_data_buffer
                .resize_destructive(unswizzled_size_bytes);
            let mut copies = unswizzle_image(
                &(),
                gpu_addr,
                &info,
                &self.swizzle_data_buffer,
                &mut self.unswizzle_data_buffer,
            );
            convert_image(
                &self.unswizzle_data_buffer,
                &info,
                P::staging_mapped_span(staging),
                &mut copies,
            );
            copies
        } else {
            unswizzle_image(
                &(),
                gpu_addr,
                &info,
                &self.swizzle_data_buffer,
                P::staging_mapped_span(staging),
            )
        };
        P::upload_image(self, image_id, staging, &copies);
    }

    /// Port of upstream `TextureCache<P>::QueueAsyncDecode`.
    fn queue_async_decode(&mut self, image_id: ImageId) {
        let image = &self.slot_images[image_id];
        if !image.flags.contains(ImageFlagBits::CONVERTED) {
            log::error!("QueueAsyncDecode called for a non-converted image");
        }
        log::info!("Queuing async texture decode");

        let (gpu_addr, guest_size_bytes, unswizzled_size_bytes, info) = (
            image.gpu_addr,
            image.guest_size_bytes as usize,
            image.unswizzled_size_bytes as usize,
            image.info.clone(),
        );
        self.slot_images[image_id]
            .flags
            .insert(ImageFlagBits::IS_DECODING);
        let decode = Arc::new(AsyncDecodeContext::new(image_id));
        self.async_decodes.push(Arc::clone(&decode));

        let gpu_memory = self
            .channel_gpu_memory
            .as_ref()
            .cloned()
            .expect("TextureCache::QueueAsyncDecode requires bound channel GPU memory");
        self.swizzle_data_buffer
            .resize_destructive(guest_size_bytes);
        gpu_memory
            .lock()
            .read_block_unsafe(gpu_addr, &mut self.swizzle_data_buffer);
        let mut local_unswizzle_data_buffer = vec![0; unswizzled_size_bytes];
        let mut copies: SmallVec<[BufferImageCopy; 16]> = unswizzle_image(
            &(),
            gpu_addr,
            &info,
            &self.swizzle_data_buffer,
            &mut local_unswizzle_data_buffer,
        )
        .into_iter()
        .collect();
        let out_size = map_size_bytes(&self.slot_images[image_id]) as usize;
        self.texture_decode_worker.queue_stateless_work(move || {
            let mut decoded_data = common::scratch_buffer::ScratchBuffer::<u8>::new();
            decoded_data.resize_destructive(out_size);
            convert_image(
                &local_unswizzle_data_buffer,
                &info,
                &mut decoded_data,
                &mut copies,
            );
            let mut output = decode.output.lock().unwrap();
            output.decoded_data = decoded_data;
            output.copies = copies;
            decode.complete.store(true, Ordering::Release);
        });
    }

    /// Port of upstream `TextureCache<P>::QueueAsyncUnswizzle`.
    fn queue_async_unswizzle(&mut self, image_id: ImageId) {
        if self.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::IS_DECODING)
        {
            return;
        }

        self.slot_images[image_id]
            .flags
            .insert(ImageFlagBits::IS_DECODING);
        let info = self.slot_images[image_id].info.clone();
        self.unswizzle_queue
            .push_back(PendingUnswizzle::new(image_id, info));
    }

    /// Port of upstream `TextureCache<P>::TickAsyncDecode`.
    pub fn tick_async_decode(&mut self) {
        let mut has_uploads = false;
        let mut index = 0;
        while index < self.async_decodes.len() {
            let decode = Arc::clone(&self.async_decodes[index]);
            if !decode.complete.load(Ordering::Acquire) {
                index += 1;
                continue;
            }
            let mut output = decode.output.lock().unwrap();
            let decoded_data = std::mem::take(&mut output.decoded_data);
            let copies = std::mem::take(&mut output.copies);
            drop(output);

            let mut staging = P::upload_staging_buffer(
                self,
                map_size_bytes(&self.slot_images[decode.image_id]) as usize,
                false,
            );
            P::staging_mapped_span(&mut staging).copy_from_slice(&decoded_data);
            P::upload_image(self, decode.image_id, &staging, &copies);
            self.slot_images[decode.image_id]
                .flags
                .remove(ImageFlagBits::IS_DECODING);
            has_uploads = true;
            self.async_decodes.remove(index);
        }
        if has_uploads {
            P::insert_upload_memory_barrier(self);
        }
    }

    /// Port of upstream `TextureCache<P>::TickAsyncUnswizzle`.
    pub fn tick_async_unswizzle(&mut self) {
        if self.unswizzle_queue.is_empty() {
            return;
        }
        if self.current_unswizzle_frame > 0 {
            self.current_unswizzle_frame -= 1;
            return;
        }

        let image_id = self.unswizzle_queue.front().unwrap().image_id;
        if !self.unswizzle_queue.front().unwrap().initialized {
            let (total_size, info) = {
                let image = &self.slot_images[image_id];
                (map_size_bytes(image) as usize, image.info.clone())
            };
            let staging = P::upload_staging_buffer(self, total_size, true);
            let bytes_per_block = crate::surface::bytes_per_block(info.format) as usize;
            let width_blocks = info.size.width.div_ceil(4) as usize;
            let height_blocks = info.size.height.div_ceil(4) as usize;
            let task = self.unswizzle_queue.front_mut().unwrap();
            task.total_size = total_size;
            task.staging_buffer = Some(staging);
            task.bytes_per_slice = width_blocks * bytes_per_block * height_blocks;
            task.last_submitted_offset = 0;
            task.initialized = true;
        }

        let (gpu_addr, current_offset, copy_amount) = {
            let task = self.unswizzle_queue.front().unwrap();
            let image = &self.slot_images[image_id];
            if task.current_offset < task.total_size {
                let remaining = task.total_size - task.current_offset;
                let mut copy_amount = self.swizzle_chunk_size.min(remaining);
                if remaining > self.swizzle_chunk_size {
                    copy_amount = (copy_amount / task.bytes_per_slice) * task.bytes_per_slice;
                    if copy_amount == 0 {
                        copy_amount = task.bytes_per_slice;
                    }
                }
                (image.gpu_addr, task.current_offset, copy_amount)
            } else {
                (image.gpu_addr, task.current_offset, 0)
            }
        };
        if copy_amount != 0 {
            let gpu_memory = self
                .channel_gpu_memory
                .as_ref()
                .cloned()
                .expect("TextureCache::TickAsyncUnswizzle requires bound channel GPU memory");
            let task = self.unswizzle_queue.front_mut().unwrap();
            let staging = task.staging_buffer.as_mut().unwrap();
            let end = current_offset + copy_amount;
            gpu_memory.lock().read_block(
                gpu_addr.wrapping_add(current_offset as u64),
                &mut P::staging_mapped_span(staging)[current_offset..end],
            );
            task.current_offset = end;
        }

        let (is_final_batch, complete_slices, bytes_per_slice, last_submitted_offset, info) = {
            let task = self.unswizzle_queue.front().unwrap();
            let bytes_ready = task.current_offset - task.last_submitted_offset;
            (
                task.current_offset >= task.total_size,
                (bytes_ready / task.bytes_per_slice) as u32,
                task.bytes_per_slice,
                task.last_submitted_offset,
                task.info.clone(),
            )
        };
        if complete_slices >= self.swizzle_slices_per_batch
            || (is_final_batch && complete_slices > 0)
        {
            let z_start = (last_submitted_offset / bytes_per_slice) as u32;
            let slices_to_process = complete_slices.min(self.swizzle_slices_per_batch);
            let z_count = slices_to_process.min(
                self.slot_images[image_id]
                    .info
                    .size
                    .depth
                    .wrapping_sub(z_start),
            );
            if z_count > 0 {
                let uploads = full_upload_swizzles(&info);
                let staging = self
                    .unswizzle_queue
                    .front_mut()
                    .unwrap()
                    .staging_buffer
                    .take()
                    .unwrap();
                P::accelerate_image_upload(self, image_id, &staging, &uploads, z_start, z_count);
                let task = self.unswizzle_queue.front_mut().unwrap();
                task.staging_buffer = Some(staging);
                task.last_submitted_offset += z_count as usize * bytes_per_slice;
            }
        }

        let complete = {
            let task = self.unswizzle_queue.front().unwrap();
            let slices_submitted = (task.last_submitted_offset / task.bytes_per_slice) as u32;
            is_final_batch && slices_submitted >= self.slot_images[image_id].info.size.depth
        };
        if complete {
            let mut staging = self
                .unswizzle_queue
                .front_mut()
                .unwrap()
                .staging_buffer
                .take()
                .unwrap();
            P::free_deferred_staging_buffer(self, &mut staging);
            self.slot_images[image_id]
                .flags
                .remove(ImageFlagBits::IS_DECODING);
            self.unswizzle_queue.pop_front();
            self.current_unswizzle_frame = 4;
        }
    }

    /// Port of upstream `TextureCache<P>::VisitImageView`.
    pub(crate) fn visit_image_view(&mut self, index: u32, compute: bool) -> ImageViewId {
        let (descriptor, is_new) = {
            let channel_state = self
                .channel_caches
                .current_channel_state_mut()
                .unwrap_or(&mut self.channel_state);
            let table = if compute {
                &mut channel_state.compute_image_table
            } else {
                &mut channel_state.graphics_image_table
            };
            if index > table.current_limit {
                log::debug!("Invalid image view index={}", index);
                return NULL_IMAGE_VIEW_ID;
            }
            if let Some(gpu_memory) = self.channel_gpu_memory.as_ref() {
                table.read_with(index, |gpu_addr, out| {
                    gpu_memory.lock().read_block_unsafe(gpu_addr, out)
                })
            } else {
                table.read(self.device_memory.as_ref(), index)
            }
        };
        let map_index = index
            | if compute {
                common::slot_vector::SlotId::TAGGED_VALUE
            } else {
                0
            };
        let image_view_id = if is_new {
            let image_view_id = self.find_image_view(&descriptor);
            if image_view_id != NULL_IMAGE_VIEW_ID {
                P::prepare_image_view(self, image_view_id, false, false);
            }
            self.current_channel_state_mut()
                .image_view_ids
                .insert(map_index, image_view_id);
            image_view_id
        } else {
            let image_view_id = *self
                .current_channel_state()
                .image_view_ids
                .get(&map_index)
                .expect("an unchanged descriptor must have a cached image-view id");
            if image_view_id != NULL_IMAGE_VIEW_ID {
                P::prepare_image_view(self, image_view_id, false, false);
            }
            image_view_id
        };
        image_view_id
    }

    /// Port of upstream `TextureCache<P>::FillImageViews`.
    pub(crate) fn fill_image_views(
        &mut self,
        views: &mut [ImageViewInOut],
        compute: bool,
        blacklist: bool,
    ) {
        loop {
            self.has_deleted_images = false;
            let mut has_blacklisted = false;
            for view in views.iter_mut() {
                view.id = self.visit_image_view(view.index, compute);
                if blacklist && view.blacklist && view.id != NULL_IMAGE_VIEW_ID {
                    let image_id = self.slot_image_views[view.id].image_id;
                    has_blacklisted |= self.scale_down(image_id);
                    self.slot_images[image_id].scale_rating = 0;
                }
            }
            if !self.has_deleted_images && !(blacklist && has_blacklisted) {
                break;
            }
        }
    }

    fn insert_typed_image(&mut self, base: ImageBase) -> ImageId {
        let image_id = self.slot_images.insert(ImageSlot::pending(base));
        let base = std::ptr::NonNull::from(self.slot_images[image_id].base.as_mut());
        let runtime = self.runtime.as_deref_mut();
        let backend = P::create_image(runtime, image_id, base);
        self.slot_images[image_id].backend = Some(backend);
        P::set_image_allocation_tick(
            self.slot_images[image_id]
                .backend
                .as_mut()
                .expect("typed image payload was just constructed"),
            self.frame_tick,
        );
        image_id
    }

    fn insert_typed_image_view(
        &mut self,
        info: ImageViewInfo,
        base: ImageViewBase,
        image_id: Option<ImageId>,
    ) -> ImageViewId {
        let view_id = self
            .slot_image_views
            .insert(ImageViewSlot::pending(info, base));
        let base = std::ptr::NonNull::from(self.slot_image_views[view_id].base.as_mut());
        let image = image_id.and_then(|id| {
            self.slot_images[id]
                .backend
                .as_ref()
                .map(|image| image as *const P::Image)
        });
        let runtime = self.runtime.as_deref_mut();
        let image = image.map(|image| {
            // SAFETY: the parent image slot remains alive while its view is
            // constructed and registered.
            unsafe { &*image }
        });
        let backend = P::create_image_view(runtime, view_id, &info, base, image);
        self.slot_image_views[view_id].backend = Some(backend);
        view_id
    }

    fn insert_typed_sampler(&mut self, config: crate::textures::texture::TscEntry) -> SamplerId {
        let runtime = self.runtime.as_deref_mut();
        let backend = P::create_sampler(runtime, &config);
        self.slot_samplers.insert(SamplerSlot {
            config,
            backend: Some(backend),
        })
    }

    /// Port of upstream `TextureCache<P>::ForEachImageInRegion`.
    ///
    /// The callback returns true to stop traversal, matching the upstream
    /// `BOOL_BREAK` specialization used by `FindImage` and `FindDMAImage`.
    pub(crate) fn for_each_image_in_region(
        &mut self,
        cpu_addr: u64,
        size: usize,
        mut func: impl FnMut(ImageId, &mut ImageBase) -> bool,
    ) -> bool {
        Self::for_each_image_in_region_parts(
            &self.page_table,
            &mut self.slot_map_views,
            &mut self.slot_images,
            cpu_addr,
            size,
            |image_id, image, _| func(image_id, image),
        )
    }

    /// Borrow-split body of upstream `TextureCache<P>::ForEachImageInRegion`.
    ///
    /// C++ can invoke the member callback while it mutates other members of
    /// `TextureCache`. Rust passes the three fields owned by the traversal
    /// explicitly so callers such as `JoinImages` can borrow their unrelated
    /// join state at the same time; traversal and callback ordering are
    /// unchanged.
    fn for_each_image_in_region_parts(
        page_table: &HashMap<u64, Vec<ImageMapId>, BuildUnorderedDenseHasher>,
        slot_map_views: &mut common::slot_vector::SlotVector<ImageMapView>,
        slot_images: &mut common::slot_vector::SlotVector<ImageSlot<P::Image>>,
        cpu_addr: u64,
        size: usize,
        mut func: impl FnMut(
            ImageId,
            &mut ImageBase,
            &common::slot_vector::SlotVector<ImageMapView>,
        ) -> bool,
    ) -> bool {
        let mut images = SmallVec::<[ImageId; 32]>::new();
        let mut maps = SmallVec::<[ImageMapId; 32]>::new();
        let stop = Self::for_each_cpu_page_until(cpu_addr, size, |page| {
            if let Some(page_map_ids) = page_table.get(&page) {
                for &map_id in page_map_ids {
                    let image_id = {
                        let map = &mut slot_map_views[map_id];
                        if map.picked || !map.overlaps(cpu_addr, size) {
                            continue;
                        }
                        map.picked = true;
                        maps.push(map_id);
                        map.image_id
                    };

                    let image = &mut slot_images[image_id];
                    if image.flags.contains(ImageFlagBits::PICKED) {
                        continue;
                    }
                    image.flags.insert(ImageFlagBits::PICKED);
                    images.push(image_id);
                    if func(image_id, image, slot_map_views) {
                        return true;
                    }
                }
            }
            false
        });

        for image_id in images {
            slot_images[image_id].flags.remove(ImageFlagBits::PICKED);
        }
        for map_id in maps {
            slot_map_views[map_id].picked = false;
        }
        stop
    }

    /// Borrow-split body shared by upstream `ForEachImageInRegionGPU` and
    /// `ForEachSparseImageInRegion`.
    fn for_each_image_in_gpu_region_parts(
        page_table: &TextureCacheGPUMap,
        slot_images: &mut common::slot_vector::SlotVector<ImageSlot<P::Image>>,
        gpu_addr: GPUVAddr,
        size: usize,
        mut func: impl FnMut(ImageId, &mut ImageBase) -> bool,
    ) -> bool {
        let mut images = SmallVec::<[ImageId; 8]>::new();
        let stop = Self::for_each_gpu_page_until(gpu_addr, size, |page| {
            if let Some(page_image_ids) = page_table.get(&page) {
                for &image_id in page_image_ids {
                    let image = &mut slot_images[image_id];
                    if image.flags.contains(ImageFlagBits::PICKED)
                        || !image.overlaps_gpu(gpu_addr, size)
                    {
                        continue;
                    }
                    image.flags.insert(ImageFlagBits::PICKED);
                    images.push(image_id);
                    if func(image_id, image) {
                        return true;
                    }
                }
            }
            false
        });

        for image_id in images {
            slot_images[image_id].flags.remove(ImageFlagBits::PICKED);
        }
        stop
    }

    pub fn set_channel_gpu_memory(&mut self, gpu_memory: Arc<ParkingMutex<MemoryManager>>) {
        self.channel_gpu_memory = Some(gpu_memory);
        self.rebase_virtual_invalid_images();
    }

    pub fn clear_channel_gpu_memory(&mut self) {
        self.channel_gpu_memory = None;
    }

    fn translated_cpu_addr(&self, gpu_addr: GPUVAddr, size: u64) -> Option<u64> {
        self.channel_gpu_memory.as_ref().and_then(|gpu_memory| {
            let gpu_memory = gpu_memory.lock();
            gpu_memory
                .gpu_to_cpu_address(gpu_addr)
                .or_else(|| gpu_memory.gpu_to_cpu_address_range(gpu_addr, size))
        })
    }

    pub(crate) fn resolve_or_allocate_cpu_addr(&mut self, gpu_addr: GPUVAddr, size: u64) -> u64 {
        if let Some(cpu_addr) = self.channel_gpu_memory.as_ref().and_then(|gpu_memory| {
            let gpu_memory = gpu_memory.lock();
            gpu_memory
                .gpu_to_cpu_address(gpu_addr)
                .or_else(|| gpu_memory.gpu_to_cpu_address_range(gpu_addr, size))
        }) {
            return cpu_addr;
        }
        let fake_addr = !(1u64 << 40) + self.virtual_invalid_space;
        self.virtual_invalid_space += common::alignment::align_up(size, 32);
        fake_addr
    }

    fn sparse_segments_for_image(
        &self,
        image_id: ImageId,
        context: &'static str,
    ) -> Vec<(GPUVAddr, u64, usize)> {
        let image = &self.slot_images[image_id];
        let gpu_memory = self
            .channel_gpu_memory
            .as_ref()
            .unwrap_or_else(|| panic!("{context} sparse image requires channel GPU memory"))
            .lock();
        gpu_memory
            .get_submapped_range(image.gpu_addr, image.guest_size_bytes as u64)
            .into_iter()
            .map(|(segment_gpu_addr, segment_size)| {
                let cpu_addr = gpu_memory
                    .gpu_to_cpu_address(segment_gpu_addr)
                    .unwrap_or_else(|| panic!("{context} sparse segment must have CPU address"));
                (segment_gpu_addr, cpu_addr, segment_size as usize)
            })
            .collect()
    }

    pub(crate) fn rebase_virtual_invalid_images(&mut self) {
        let Some(gpu_memory) = self.channel_gpu_memory.as_ref().cloned() else {
            return;
        };
        let sentinel = !(1u64 << 40);
        let image_ids = self
            .slot_images
            .iter()
            .filter_map(|(id, image)| {
                (id != NULL_IMAGE_ID
                    && image.cpu_addr >= sentinel
                    && !image.flags.contains(ImageFlagBits::SPARSE))
                .then_some(id)
            })
            .collect::<Vec<_>>();

        for image_id in image_ids {
            let (gpu_addr, size, was_registered, was_tracked) = {
                let image = &self.slot_images[image_id];
                (
                    image.gpu_addr,
                    image.guest_size_bytes as u64,
                    image.flags.contains(ImageFlagBits::REGISTERED),
                    image.flags.contains(ImageFlagBits::TRACKED),
                )
            };
            let Some(cpu_addr) = ({
                let gpu_memory = gpu_memory.lock();
                gpu_memory
                    .gpu_to_cpu_address(gpu_addr)
                    .or_else(|| gpu_memory.gpu_to_cpu_address_range(gpu_addr, size))
            }) else {
                continue;
            };
            if was_tracked {
                self.untrack_image(image_id);
            }
            if was_registered {
                self.unregister_image(image_id);
            }
            {
                let image = &mut self.slot_images[image_id];
                image.cpu_addr = cpu_addr;
                image.cpu_addr_end = cpu_addr + image.guest_size_bytes as u64;
            }
            if was_registered {
                self.register_image(image_id);
            }
            if was_tracked {
                self.track_image(image_id);
            }
        }
    }

    // ── Garbage collection ─────────────────────────────────────────────

    /// Port of `TextureCache<P>::RunGarbageCollector`.
    pub fn run_garbage_collector(&mut self) {
        let downloader = self.image_downloader.as_ref().cloned();
        self.run_garbage_collector_with_downloader(|_image_id, image, _backend, staging| {
            let Some(downloader) = downloader.as_ref() else {
                return false;
            };
            downloader(_image_id, image, staging)
        });
    }

    /// Port of `TextureCache<P>::RunGarbageCollector`, with the backend
    /// `Runtime::DownloadStagingBuffer` + `Image::DownloadMemory` operation
    /// supplied by the concrete renderer wrapper.
    pub fn run_garbage_collector_with_downloader(
        &mut self,
        mut download_image: impl FnMut(
            ImageId,
            &mut ImageBase,
            &mut Option<P::Image>,
            &mut [u8],
        ) -> bool,
    ) {
        let mut high_priority_mode = false;
        let mut aggressive_mode = false;
        let mut ticks_to_destroy = 0u64;
        let mut num_iterations = 0usize;

        let configure = |cache: &Self,
                         allow_aggressive: bool,
                         high_priority_mode: &mut bool,
                         aggressive_mode: &mut bool,
                         ticks_to_destroy: &mut u64,
                         num_iterations: &mut usize| {
            *high_priority_mode = cache.total_used_memory >= cache.expected_memory;
            *aggressive_mode = allow_aggressive && cache.total_used_memory >= cache.critical_memory;
            *ticks_to_destroy = if *aggressive_mode {
                10
            } else if *high_priority_mode {
                25
            } else {
                50
            };
            *num_iterations = if *aggressive_mode {
                40
            } else if *high_priority_mode {
                20
            } else {
                10
            };
        };

        configure(
            self,
            false,
            &mut high_priority_mode,
            &mut aggressive_mode,
            &mut ticks_to_destroy,
            &mut num_iterations,
        );
        self.cleanup_lru_images(
            self.frame_tick.wrapping_sub(ticks_to_destroy),
            &mut num_iterations,
            &mut high_priority_mode,
            &mut aggressive_mode,
            &mut download_image,
        );

        if self.total_used_memory >= self.critical_memory {
            configure(
                self,
                true,
                &mut high_priority_mode,
                &mut aggressive_mode,
                &mut ticks_to_destroy,
                &mut num_iterations,
            );
            self.cleanup_lru_images(
                self.frame_tick.wrapping_sub(ticks_to_destroy),
                &mut num_iterations,
                &mut high_priority_mode,
                &mut aggressive_mode,
                &mut download_image,
            );
        }
    }

    fn cleanup_lru_images(
        &mut self,
        tick_threshold: u64,
        num_iterations: &mut usize,
        high_priority_mode: &mut bool,
        aggressive_mode: &mut bool,
        download_image: &mut impl FnMut(
            ImageId,
            &mut ImageBase,
            &mut Option<P::Image>,
            &mut [u8],
        ) -> bool,
    ) {
        let mut candidates = Vec::new();
        self.lru_cache
            .for_each_item_below(tick_threshold, |image_id| {
                candidates.push(image_id);
                false
            });

        for image_id in candidates {
            if *num_iterations == 0 {
                break;
            }
            if !image_id.is_valid() || image_id == NULL_IMAGE_ID {
                continue;
            }

            *num_iterations -= 1;
            let image = &self.slot_images[image_id];
            if image.flags.contains(ImageFlagBits::IS_DECODING) {
                continue;
            }
            if !*aggressive_mode && image.flags.contains(ImageFlagBits::COSTLY_LOAD) {
                continue;
            }
            let must_download =
                image.is_safe_download() && !image.flags.contains(ImageFlagBits::BAD_OVERLAP);
            if !*high_priority_mode && must_download {
                continue;
            }
            if must_download && !self.download_image_for_gc(image_id, download_image) {
                continue;
            }
            if self.slot_images[image_id]
                .flags
                .contains(ImageFlagBits::TRACKED)
            {
                self.untrack_image(image_id);
            }
            self.unregister_image(image_id);
            let immediate_delete = self.slot_images[image_id].scale_tick > self.frame_tick + 5;
            self.delete_image(image_id, immediate_delete);

            if self.total_used_memory < self.critical_memory {
                if *aggressive_mode {
                    *num_iterations >>= 2;
                    *aggressive_mode = false;
                    break;
                }
                if *high_priority_mode && self.total_used_memory < self.expected_memory {
                    *num_iterations >>= 1;
                    *high_priority_mode = false;
                }
            }
        }
    }

    fn download_image_for_gc(
        &mut self,
        image_id: ImageId,
        download_image: &mut impl FnMut(
            ImageId,
            &mut ImageBase,
            &mut Option<P::Image>,
            &mut [u8],
        ) -> bool,
    ) -> bool {
        let mut staging = vec![0u8; self.slot_images[image_id].unswizzled_size_bytes as usize];
        let downloaded = {
            let slot = &mut self.slot_images[image_id];
            let mut backend = slot.backend.take();
            let downloaded = download_image(image_id, &mut slot.base, &mut backend, &mut staging);
            slot.backend = backend;
            downloaded
        };
        if !downloaded {
            return false;
        }
        let image = self.slot_images[image_id].base.clone();
        let copies = super::util::full_download_copies(&image.info);
        if !self.write_downloaded_image(&image, &copies, &staging) {
            return false;
        }
        self.slot_images[image_id]
            .flags
            .remove(ImageFlagBits::GPU_MODIFIED);
        true
    }

    /// Common writeback half of upstream `SwizzleImage(*gpu_memory, image.gpu_addr, ...)`.
    pub fn write_downloaded_image(
        &mut self,
        image: &ImageBase,
        copies: &[BufferImageCopy],
        staging: &[u8],
    ) -> bool {
        if let Some(gpu_memory) = self.channel_gpu_memory.as_ref().cloned() {
            if let Some(gpu_memory) = gpu_memory.try_lock() {
                super::util::swizzle_image(
                    &|gpu_addr, output| {
                        let _ = gpu_memory.read_block_unsafe(gpu_addr, output);
                    },
                    &|gpu_addr, data| {
                        let _ = gpu_memory.write_block_unsafe(gpu_addr, data);
                    },
                    image.gpu_addr,
                    &image.info,
                    copies,
                    staging,
                    &mut self.swizzle_data_buffer,
                );
                return true;
            }
        }

        let Some(writer) = self.guest_memory_writer.as_ref().cloned() else {
            return false;
        };
        let device_memory = Arc::clone(&self.device_memory);
        super::util::swizzle_image(
            &move |device_addr, output| {
                let _ = device_memory.smmu_read_block_unsafe(device_addr, output);
            },
            writer.as_ref(),
            image.cpu_addr,
            &image.info,
            copies,
            staging,
            &mut self.swizzle_data_buffer,
        );
        true
    }

    /// Common writeback half of upstream `gpu_memory->WriteBlockUnsafe`
    /// in `TextureCache<P>::PopAsyncFlushes` for DMA buffer downloads.
    pub fn write_downloaded_buffer(&mut self, gpu_addr: GPUVAddr, staging: &[u8]) -> bool {
        if let Some(gpu_memory) = self.channel_gpu_memory.as_ref().cloned() {
            let gpu_memory = gpu_memory.lock();
            let _ = gpu_memory.write_block_unsafe(gpu_addr, staging);
            return true;
        }
        self.device_memory
            .smmu_write_block_unsafe(gpu_addr, staging)
    }

    /// Port of `TextureCache<P>::WriteMemory`.
    pub fn write_memory(&mut self, cpu_addr: u64, size: usize) {
        let device_memory = &self.device_memory;
        let sparse_views = &self.sparse_views;
        Self::for_each_image_in_region_parts(
            &self.page_table,
            &mut self.slot_map_views,
            &mut self.slot_images,
            cpu_addr,
            size,
            |image_id, image, slot_map_views| {
                if image.flags.contains(ImageFlagBits::CPU_MODIFIED) {
                    return false;
                }
                image.flags.insert(ImageFlagBits::CPU_MODIFIED);
                if image.flags.contains(ImageFlagBits::TRACKED) {
                    Self::untrack_image_parts(
                        device_memory,
                        sparse_views,
                        slot_map_views,
                        image_id,
                        image,
                    );
                }
                false
            },
        );
    }

    /// Port of `TextureCache<P>::DownloadMemory` for the common Rust fallback.
    pub fn download_memory(&mut self, cpu_addr: u64, size: usize) {
        let Some(downloader) = self.image_downloader.as_ref().cloned() else {
            return;
        };
        let Some(writer) = self.guest_memory_writer.as_ref().cloned() else {
            return;
        };

        let mut images = SmallVec::<[ImageId; 16]>::new();
        self.for_each_image_in_region(cpu_addr, size, |image_id, image| {
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
        images.sort_by_key(|&image_id| self.slot_images[image_id].modification_tick);

        for image_id in images {
            let image = self.slot_images[image_id].base.as_ref().clone();
            let mut staging = vec![0u8; image.unswizzled_size_bytes as usize];
            if !downloader(image_id, &image, &mut staging) {
                continue;
            }
            let copies = super::util::full_download_copies(&image.info);
            let device_memory = std::sync::Arc::clone(&self.device_memory);
            super::util::swizzle_image(
                &move |device_addr, output| {
                    let _ = device_memory.smmu_read_block_unsafe(device_addr, output);
                },
                writer.as_ref(),
                image.cpu_addr,
                &image.info,
                &copies,
                &staging,
                &mut self.swizzle_data_buffer,
            );
        }
    }

    /// Port of `TextureCache<P>::GetFlushArea`.
    pub fn get_flush_area(&mut self, cpu_addr: u64, size: usize) -> Option<RasterizerDownloadArea> {
        let mut area: Option<RasterizerDownloadArea> = None;
        let slot_image_views = &mut self.slot_image_views;
        Self::for_each_image_in_region_parts(
            &self.page_table,
            &mut self.slot_map_views,
            &mut self.slot_images,
            cpu_addr,
            size,
            |_, image, _| {
                if !image.flags.contains(ImageFlagBits::GPU_MODIFIED) {
                    return false;
                }
                let current = area.get_or_insert(RasterizerDownloadArea {
                    start_address: cpu_addr,
                    end_address: cpu_addr.wrapping_add(size as u64),
                    preemtive: true,
                });
                current.start_address = current.start_address.min(image.cpu_addr);
                current.end_address = current.end_address.max(image.cpu_addr_end);
                for &image_view_id in &image.image_view_ids {
                    slot_image_views[image_view_id]
                        .flags
                        .insert(ImageViewFlagBits::PREEMTIVE_DOWNLOAD);
                }
                current.preemtive &= image.info.forced_flushed;
                image.info.forced_flushed = true;
                false
            },
        );
        area
    }

    /// Port of `TextureCache<P>::UnmapMemory`.
    pub fn unmap_memory(&mut self, cpu_addr: u64, size: usize) {
        let mut deleted_images = SmallVec::<[ImageId; 16]>::new();
        self.for_each_image_in_region(cpu_addr, size, |image_id, _| {
            deleted_images.push(image_id);
            false
        });
        for image_id in deleted_images {
            if self.slot_images[image_id]
                .flags
                .contains(ImageFlagBits::TRACKED)
            {
                self.untrack_image(image_id);
            }
            self.unregister_image(image_id);
            self.delete_image(image_id, false);
        }
    }

    /// Port of `TextureCache<P>::TryFindFramebufferImageView`.
    pub fn try_find_framebuffer_image_view(
        &mut self,
        config: &FramebufferConfig,
        cpu_addr: u64,
    ) -> Option<FramebufferImageView> {
        let image_map_ids = self.page_table.get(&(cpu_addr >> YUZU_PAGEBITS))?;
        let mut valid_image_ids = SmallVec::<[ImageId; 4]>::new();
        for &map_id in image_map_ids {
            let map = &self.slot_map_views[map_id];
            let image = &self.slot_images[map.image_id];
            if image.cpu_addr != cpu_addr || image.image_view_ids.is_empty() {
                continue;
            }
            valid_image_ids.push(map.image_id);
        }

        let Some(&first_id) = valid_image_ids.first() else {
            return None;
        };
        let mut image_id = first_id;
        for &candidate_id in &valid_image_ids[1..] {
            if self.slot_images[image_id].modification_tick
                < self.slot_images[candidate_id].modification_tick
            {
                image_id = candidate_id;
            }
        }

        let view_format = match config.pixel_format {
            ruzu_core::hle::service::nvnflinger::pixel_format::PixelFormat::Rgb565 => {
                PixelFormat::R5G6B5Unorm
            }
            ruzu_core::hle::service::nvnflinger::pixel_format::PixelFormat::Bgra8888 => {
                PixelFormat::B8G8R8A8Unorm
            }
            _ => PixelFormat::A8B8G8R8Unorm,
        };
        let mut info = ImageViewInfo::for_render_target(
            ImageViewType::E2D,
            view_format,
            SubresourceRange::default(),
        );
        if config.blending == BlendMode::Opaque {
            info.x_source = SwizzleSource::R as u8;
            info.y_source = SwizzleSource::G as u8;
            info.z_source = SwizzleSource::B as u8;
            info.w_source = SwizzleSource::OneFloat as u8;
        }

        let existing_view_id = self.slot_images[image_id].find_view(&info);
        let view_id = if existing_view_id.is_valid() {
            existing_view_id
        } else {
            let image = &self.slot_images[image_id];
            let view = ImageViewBase::new(&info, &image.info, image_id, image.gpu_addr);
            let view_id = self.insert_typed_image_view(info, view, Some(image_id));
            self.slot_images[image_id].insert_view(info, view_id);
            view_id
        };
        let image = &self.slot_images[image_id];
        let view = (**self.slot_image_views.get(view_id)).clone();
        Some(FramebufferImageView {
            view_id,
            view,
            scaled: image.flags.contains(ImageFlagBits::RESCALED),
        })
    }

    fn registered_image_memory_size(image: &ImageBase) -> u64 {
        let mut tentative_size = u64::from(image.guest_size_bytes.max(image.unswizzled_size_bytes));
        if (surface::is_pixel_format_astc(image.info.format)
            && image.flags.contains(ImageFlagBits::ACCELERATED_UPLOAD))
            || image.flags.contains(ImageFlagBits::CONVERTED)
        {
            tentative_size = surface::transcoded_astc_size(tentative_size, image.info.format);
        }
        common::alignment::align_up(tentative_size, 1024)
    }

    pub(crate) fn scaled_image_memory_size(image: &ImageBase) -> u64 {
        let resolution = common::settings::values().resolution_info.clone();
        let scale_up = (resolution.up_scale * resolution.up_scale) as u64;
        let down_shift = (resolution.down_shift + resolution.down_shift) as u64;
        let image_size_bytes = u64::from(image.guest_size_bytes.max(image.unswizzled_size_bytes));
        let tentative_size = (image_size_bytes * scale_up) >> down_shift;
        common::alignment::align_up(tentative_size, 1024)
    }

    /// Port of `TextureCache<P>::ImageCanRescale`.
    pub(crate) fn image_can_rescale(&mut self, image_id: ImageId) -> bool {
        if !self.slot_images[image_id].info.rescaleable {
            return false;
        }
        let resolution = common::settings::values().resolution_info.clone();
        if resolution.downscale && !self.slot_images[image_id].info.downscaleable {
            return false;
        }
        if self.slot_images[image_id]
            .flags
            .intersects(ImageFlagBits::RESCALED | ImageFlagBits::CHECKING_RESCALABLE)
        {
            return true;
        }
        if self.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::IS_RESCALABLE)
        {
            return true;
        }
        self.slot_images[image_id]
            .flags
            .insert(ImageFlagBits::CHECKING_RESCALABLE);
        let aliases = self.slot_images[image_id]
            .aliased_images
            .iter()
            .map(|alias| alias.id)
            .collect::<SmallVec<[ImageId; 8]>>();
        for alias_id in aliases {
            if !self.image_can_rescale(alias_id) {
                self.slot_images[image_id]
                    .flags
                    .remove(ImageFlagBits::CHECKING_RESCALABLE);
                return false;
            }
        }
        self.slot_images[image_id]
            .flags
            .remove(ImageFlagBits::CHECKING_RESCALABLE);
        self.slot_images[image_id]
            .flags
            .insert(ImageFlagBits::IS_RESCALABLE);
        true
    }

    /// Port of `TextureCache<P>::ScaleUp`.
    pub(crate) fn scale_up(&mut self, image_id: ImageId) -> bool {
        let has_copy = self.slot_images[image_id].has_scaled;
        if !P::scale_up_image(self, image_id, false) {
            return false;
        }
        if !has_copy {
            self.total_used_memory = self
                .total_used_memory
                .wrapping_add(Self::scaled_image_memory_size(&self.slot_images[image_id]));
        }
        self.invalidate_scale(image_id);
        true
    }

    /// Port of `TextureCache<P>::ScaleDown`.
    pub(crate) fn scale_down(&mut self, image_id: ImageId) -> bool {
        if !P::scale_down_image(self, image_id, false) {
            return false;
        }
        self.invalidate_scale(image_id);
        true
    }

    // ── Image view resolution ──────────────────────────────────────────

    /// Port of `TextureCache<P>::FindImageView` (texture_cache.h:1103-1113).
    /// Guards on `IsValidEntry`, then does a HashMap try_emplace against the
    /// descriptor; on cache miss, calls `create_image_view`.
    fn find_image_view(&mut self, descriptor: &crate::textures::texture::TicEntry) -> ImageViewId {
        if let Some(gpu_memory) = self.channel_gpu_memory.as_ref().cloned() {
            return self.find_image_view_with_gpu_to_cpu(descriptor, &mut |gpu_addr, size| {
                let gpu_memory = gpu_memory.lock();
                gpu_memory
                    .gpu_to_cpu_address(gpu_addr)
                    .or_else(|| gpu_memory.gpu_to_cpu_address_range(gpu_addr, size))
            });
        }
        let gpu_memory = self.device_memory.clone();
        if !super::util::is_valid_entry(&*gpu_memory, descriptor) {
            return NULL_IMAGE_VIEW_ID;
        }
        if let Some(&id) = self.current_channel_state_mut().image_views.get(descriptor) {
            return id;
        }
        let new_id = self.create_image_view(descriptor);
        self.current_channel_state_mut()
            .image_views
            .insert(*descriptor, new_id);
        new_id
    }

    fn find_image_view_with_gpu_to_cpu(
        &mut self,
        descriptor: &crate::textures::texture::TicEntry,
        gpu_to_cpu: &mut dyn FnMut(GPUVAddr, u64) -> Option<u64>,
    ) -> ImageViewId {
        if !super::util::is_valid_entry_with_range_valid(descriptor, |gpu_addr, size| {
            gpu_to_cpu(gpu_addr, size).is_some()
        }) {
            return NULL_IMAGE_VIEW_ID;
        }
        if let Some(&id) = self.current_channel_state_mut().image_views.get(descriptor) {
            return id;
        }
        let new_id = self.create_image_view_with_gpu_to_cpu(descriptor, gpu_to_cpu);
        self.current_channel_state_mut()
            .image_views
            .insert(*descriptor, new_id);
        new_id
    }

    fn create_image_view_with_gpu_to_cpu(
        &mut self,
        descriptor: &crate::textures::texture::TicEntry,
        gpu_to_cpu: &mut dyn FnMut(GPUVAddr, u64) -> Option<u64>,
    ) -> ImageViewId {
        let info = super::image_info::ImageInfo::from_tic_entry(descriptor);
        if info.image_type == ImageType::Buffer {
            let view_info = super::image_view_info::ImageViewInfo::from_tic_entry(descriptor, 0);
            let view = ImageViewBase::new_buffer(&info, &view_info, descriptor.address());
            return self.insert_typed_image_view(view_info, view, None);
        }
        let layer_offset = descriptor.base_layer() as u64 * info.layer_stride as u64;
        let image_gpu_addr = descriptor.address().wrapping_sub(layer_offset);
        let image_size = super::util::calculate_guest_size_in_bytes(&info);
        let cpu_addr = gpu_to_cpu(image_gpu_addr, image_size as u64).unwrap_or_else(|| {
            self.resolve_or_allocate_cpu_addr(image_gpu_addr, image_size as u64)
        });
        let image_id = self.find_or_insert_image_from_info_with_options(
            &info,
            image_gpu_addr,
            cpu_addr,
            RelaxedOptions::empty(),
        );
        if image_id == NULL_IMAGE_ID {
            return NULL_IMAGE_VIEW_ID;
        }
        let base = Self::create_image_view_base(self.slot_images.get(image_id), descriptor);
        let view_info =
            super::image_view_info::ImageViewInfo::from_tic_entry(descriptor, base.layer);
        let view_id = self.find_or_emplace_image_view(image_id, view_info, descriptor.address());
        self.slot_image_views.get_mut(view_id).flags |=
            super::image_view_base::ImageViewFlagBits::STRONG;
        self.slot_images.get_mut(image_id).flags |= ImageFlagBits::STRONG;
        view_id
    }

    /// Port of `TextureCache<P>::CreateImageView` (texture_cache.h:1115-1137).
    /// Now wired through to the real slot pools — returns the inserted view's
    /// `ImageViewId` (not a NULL stub). The created `ImageViewBase` carries
    /// the format, dimensions, range and parent `ImageId`, with the
    /// `Strong` flag set on both the view and its backing image. The backend
    /// concrete backend view is constructed in the same slot before the ID is
    /// returned, matching upstream's typed `SlotVector<ImageView>`.
    fn create_image_view(
        &mut self,
        descriptor: &crate::textures::texture::TicEntry,
    ) -> ImageViewId {
        let info = super::image_info::ImageInfo::from_tic_entry(descriptor);
        if info.image_type == ImageType::Buffer {
            let view_info = super::image_view_info::ImageViewInfo::from_tic_entry(descriptor, 0);
            let view = ImageViewBase::new_buffer(&info, &view_info, descriptor.address());
            return self.insert_typed_image_view(view_info, view, None);
        }
        let layer_offset = descriptor.base_layer() as u64 * info.layer_stride as u64;
        let image_gpu_addr = descriptor.address().wrapping_sub(layer_offset);
        let image_size = super::util::calculate_guest_size_in_bytes(&info) as u64;
        let cpu_addr = self.resolve_or_allocate_cpu_addr(image_gpu_addr, image_size);
        let image_id = self.find_or_insert_image_from_info_with_options(
            &info,
            image_gpu_addr,
            cpu_addr,
            RelaxedOptions::empty(),
        );
        if image_id == NULL_IMAGE_ID {
            return NULL_IMAGE_VIEW_ID;
        }
        let base = Self::create_image_view_base(self.slot_images.get(image_id), descriptor);
        let view_info =
            super::image_view_info::ImageViewInfo::from_tic_entry(descriptor, base.layer);
        let view_id = self.find_or_emplace_image_view(image_id, view_info, descriptor.address());
        // Upstream tags both the view and its image as `Strong`. Bitflags
        // already supports `|=` on the existing `flags` fields.
        self.slot_image_views.get_mut(view_id).flags |=
            super::image_view_base::ImageViewFlagBits::STRONG;
        self.slot_images.get_mut(image_id).flags |= ImageFlagBits::STRONG;
        view_id
    }

    fn create_image_view_base(
        image: &ImageBase,
        descriptor: &crate::textures::texture::TicEntry,
    ) -> SubresourceBase {
        let base = image
            .try_find_base(descriptor.address())
            .expect("TextureCache::CreateImageView TryFindBase failed");
        assert_eq!(
            base.level, 0,
            "TextureCache::CreateImageView base level must be zero"
        );
        base
    }

    /// Port of `TextureCache<P>::FindOrInsertImage` (texture_cache.h:1140-1146).
    /// Looks up an existing image that can satisfy `info` at `gpu_addr`;
    /// on miss, inserts a fresh `ImageBase` keyed by that address.
    #[cfg(test)]
    pub(crate) fn find_or_insert_image(
        &mut self,
        info: &super::image_info::ImageInfo,
        gpu_addr: GPUVAddr,
    ) -> ImageId {
        self.find_or_insert_image_with_caps(
            info,
            gpu_addr,
            RelaxedOptions::empty(),
            self.has_broken_texture_view_formats,
            self.has_native_bgr,
        )
    }

    fn find_or_insert_image_with_caps(
        &mut self,
        info: &super::image_info::ImageInfo,
        gpu_addr: GPUVAddr,
        options: RelaxedOptions,
        broken_views: bool,
        native_bgr: bool,
    ) -> ImageId {
        if let Some(id) =
            self.find_image_with_caps(info, gpu_addr, options, broken_views, native_bgr)
        {
            return id;
        }
        self.insert_image(info, gpu_addr)
    }

    /// Port of `TextureCache<P>::FindImage` (texture_cache.h:1149-1202).
    ///
    /// Upstream first translates the requested GPU range to a CPU/device
    /// address and returns the null image on translation failure. Candidate
    /// reuse is then restricted to images registered in that CPU region.
    #[cfg(test)]
    pub(crate) fn find_image(
        &mut self,
        info: &super::image_info::ImageInfo,
        gpu_addr: GPUVAddr,
    ) -> Option<ImageId> {
        self.find_image_with_caps(
            info,
            gpu_addr,
            RelaxedOptions::empty(),
            self.has_broken_texture_view_formats,
            self.has_native_bgr,
        )
    }

    /// Same compatibility predicate as `find_image`, with backend runtime
    /// flags supplied by the caller. Upstream `TextureCache<P>::FindImage`
    /// derives these from `runtime.HasBrokenTextureViewFormats()` and
    /// `runtime.HasNativeBgr()` before calling `IsSubresource`.
    pub(crate) fn find_image_with_caps(
        &mut self,
        info: &super::image_info::ImageInfo,
        gpu_addr: GPUVAddr,
        options: RelaxedOptions,
        broken_views: bool,
        native_bgr: bool,
    ) -> Option<ImageId> {
        let size = super::util::calculate_guest_size_in_bytes(info) as u64;
        let cpu_addr = self.translated_cpu_addr(gpu_addr, size)?;
        self.find_image_in_cpu_region_with_caps(
            info,
            gpu_addr,
            cpu_addr,
            options,
            broken_views,
            native_bgr,
        )
    }

    /// CPU-region bounded variant of upstream `TextureCache<P>::FindImage`.
    ///
    /// Upstream translates the candidate GPU address to a CPU/device address
    /// and scans only images registered in that backing region. It accepts
    /// compatible texture views, not just exact format matches, and chooses
    /// the most recently modified candidate when multiple images overlap.
    pub(crate) fn find_image_in_cpu_region_with_caps(
        &mut self,
        info: &super::image_info::ImageInfo,
        gpu_addr: GPUVAddr,
        cpu_addr: u64,
        options: RelaxedOptions,
        broken_views: bool,
        native_bgr: bool,
    ) -> Option<ImageId> {
        let broken_views = broken_views || options.contains(RelaxedOptions::FORCE_BROKEN_VIEWS);
        let flexible_formats = options.contains(RelaxedOptions::FORMAT);
        let size_bytes = super::util::calculate_guest_size_in_bytes(info) as usize;
        let mut image_id = None;
        let mut image_ids = SmallVec::<[ImageId; 8]>::new();

        self.for_each_image_in_region(cpu_addr, size_bytes, |existing_image_id, existing_image| {
            if existing_image.flags.contains(ImageFlagBits::REMAPPED) {
                return false;
            }

            let matched = if info.image_type == ImageType::Linear
                || existing_image.info.image_type == ImageType::Linear
            {
                let strict_size = !options.contains(RelaxedOptions::SIZE)
                    && existing_image.flags.contains(ImageFlagBits::STRONG);
                let existing = &existing_image.info;
                existing_image.gpu_addr == gpu_addr
                    && existing.image_type == info.image_type
                    && existing.pitch() == info.pitch()
                    && super::util::is_pitch_linear_same_size(existing, info, strict_size)
                    && crate::compatible_formats::is_view_compatible(
                        existing.format,
                        info.format,
                        broken_views,
                        native_bgr,
                    )
            } else {
                super::util::is_subresource(
                    info,
                    existing_image,
                    gpu_addr,
                    options,
                    broken_views,
                    native_bgr,
                )
            };

            if !matched {
                return false;
            }
            image_id = Some(existing_image_id);
            image_ids.push(existing_image_id);
            !flexible_formats && existing_image.info.format == info.format
        });

        if image_ids.len() <= 1 {
            return image_id;
        }
        image_ids
            .into_iter()
            .max_by_key(|&id| self.slot_images[id].modification_tick)
    }

    /// Port of `InsertImage` minus the backend `slot_images.insert(runtime,
    /// info, ...)` upload glue. Resolves the CPU/device address in the same
    /// order as upstream: direct GPU translation, range translation, then a
    /// virtual-invalid fake CPU range.
    pub(crate) fn insert_image(
        &mut self,
        info: &super::image_info::ImageInfo,
        gpu_addr: GPUVAddr,
    ) -> ImageId {
        assert!(
            !info.is_sparse || self.channel_gpu_memory.is_some(),
            "TextureCache::insert_image sparse image requires channel GPU memory"
        );
        let size = super::util::calculate_guest_size_in_bytes(info) as u64;
        let cpu_addr = self.resolve_or_allocate_cpu_addr(gpu_addr, size);
        let image_id = self.join_images(info, gpu_addr, cpu_addr);
        self.register_image_alloc(image_id);
        image_id
    }

    /// Port of `TextureCache<P>::CheckFeedbackLoop`.
    ///
    /// Exact colour-target views and aliases are skipped; only a distinct view
    /// aliasing the active depth image requires the feedback-loop barrier.
    pub fn check_feedback_loop(
        &mut self,
        views: &[ImageViewInOut],
        mut barrier_feedback_loop: impl FnMut(),
    ) {
        if !*common::settings::values()
            .barrier_feedback_loops
            .get_value()
        {
            return;
        }

        if self.render_targets_serial == self.last_feedback_loop_serial
            && self.texture_bindings_serial == self.last_feedback_texture_serial
        {
            if self.last_feedback_loop_result {
                barrier_feedback_loop();
            }
            return;
        }
        if self.rt_active_mask == 0 {
            self.last_feedback_loop_serial = self.render_targets_serial;
            self.last_feedback_texture_serial = self.texture_bindings_serial;
            self.last_feedback_loop_result = false;
            return;
        }
        let depth_active = (self.rt_active_mask & (1 << NUM_RT)) != 0;
        let requires_barrier = views.iter().any(|view| {
            if !view.id.is_valid() || view.id == NULL_IMAGE_VIEW_ID {
                return false;
            }
            if (0..NUM_RT).any(|index| {
                (self.rt_active_mask & (1 << index)) != 0
                    && view.id == self.render_targets.color_buffer_ids[index]
            }) {
                return false;
            }
            if depth_active && view.id == self.render_targets.depth_buffer_id {
                return false;
            }
            let view_image_id = self.slot_image_views[view.id].image_id;
            if (0..NUM_RT).any(|index| {
                (self.rt_active_mask & (1 << index)) != 0
                    && view_image_id == self.rt_image_id[index]
            }) {
                return false;
            }
            depth_active && view_image_id == self.rt_depth_image_id
        });
        self.last_feedback_loop_serial = self.render_targets_serial;
        self.last_feedback_texture_serial = self.texture_bindings_serial;
        self.last_feedback_loop_result = requires_barrier;
        if requires_barrier {
            barrier_feedback_loop();
        }
    }

    // ── Descriptor synchronisation ─────────────────────────────────────

    /// Port of `TextureCache<P>::SynchronizeGraphicsDescriptors`
    /// (texture_cache.h:294-307).
    ///
    /// Upstream reads `maxwell3d->regs` directly; ruzu can't borrow Maxwell3D
    /// from the texture cache so the caller hands in a `DescriptorSyncRegs`
    /// snapshot captured at draw-time. The body otherwise mirrors upstream
    /// step-for-step: pick TIC/TSC table limits (collapsing TSC limit onto
    /// TIC's when `sampler_binding == ViaHeaderBinding`) and call
    /// `DescriptorTable::synchronize` on each.
    pub fn synchronize_graphics_descriptors(&mut self, regs: DescriptorSyncRegs) {
        let tic_limit = regs.tex_header_limit;
        let tsc_limit = if regs.sampler_binding_via_header {
            tic_limit
        } else {
            regs.tex_sampler_limit
        };
        let channel = self.current_channel_state_mut();
        let mut bindings_changed = false;
        if channel
            .graphics_sampler_table
            .synchronize(regs.tex_sampler_addr, tsc_limit)
        {
            bindings_changed = true;
        }
        if channel
            .graphics_image_table
            .synchronize(regs.tex_header_addr, tic_limit)
        {
            bindings_changed = true;
        }
        if bindings_changed {
            self.texture_bindings_serial = self.texture_bindings_serial.wrapping_add(1);
        }
    }

    /// Port of `TextureCache<P>::SynchronizeComputeDescriptors`
    /// (texture_cache.h:310-322).
    ///
    /// Upstream reads `kepler_compute->regs` and
    /// `kepler_compute->launch_description.linked_tsc` directly. Ruzu passes
    /// those values as a snapshot, matching the graphics descriptor sync
    /// pattern.
    pub fn synchronize_compute_descriptors(
        &mut self,
        regs: crate::texture_cache::texture_cache_base::ComputeDescriptorSyncRegs,
    ) {
        let tic_limit = regs.tic_limit;
        let tsc_limit = if regs.linked_tsc {
            tic_limit
        } else {
            regs.tsc_limit
        };

        let channel = self.current_channel_state_mut();
        let mut bindings_changed = false;
        if channel
            .compute_sampler_table
            .synchronize(regs.tsc_addr, tsc_limit)
        {
            bindings_changed = true;
        }
        if channel
            .compute_image_table
            .synchronize(regs.tic_addr, tic_limit)
        {
            bindings_changed = true;
        }
        if bindings_changed {
            self.texture_bindings_serial = self.texture_bindings_serial.wrapping_add(1);
        }
    }

    // ── Sampler resolution ─────────────────────────────────────────────

    /// Resolve a stage sampler index to a `SamplerId`.
    ///
    /// Port of `TextureCache<P>::GetSamplerId`.
    /// Reads the TSC table at `index`, dedupes via `channel_state.samplers`,
    /// and caches the result in `channel_state.sampler_ids[index]`
    /// so subsequent draws skip the lookup when the descriptor hasn't
    /// changed.
    ///
    /// Returns `NULL_SAMPLER_ID` for out-of-range indices — upstream logs
    /// `LOG_DEBUG("Invalid sampler index={}")` and does the same.
    pub fn get_sampler_id(&mut self, index: u32, compute: bool) -> SamplerId {
        self.get_sampler_id_impl(index, compute, None)
    }

    /// Rust ownership adapter for an upstream caller that already borrows the
    /// channel `MemoryManager*`. This is still `GetSamplerId`; passing the
    /// existing borrow prevents recursively locking Reden's mutex wrapper.
    pub(crate) fn get_sampler_id_with_memory(
        &mut self,
        index: u32,
        compute: bool,
        gpu_memory: &MemoryManager,
    ) -> SamplerId {
        self.get_sampler_id_impl(index, compute, Some(gpu_memory))
    }

    fn get_sampler_id_impl(
        &mut self,
        index: u32,
        compute: bool,
        borrowed_gpu_memory: Option<&MemoryManager>,
    ) -> SamplerId {
        use crate::texture_cache::types::NULL_SAMPLER_ID;
        let (descriptor, is_new) = {
            let Self {
                channel_gpu_memory,
                device_memory,
                channel_caches,
                channel_state,
                ..
            } = self;
            let channel_state = channel_caches
                .current_channel_state_mut()
                .unwrap_or(channel_state);
            let table = if compute {
                &mut channel_state.compute_sampler_table
            } else {
                &mut channel_state.graphics_sampler_table
            };
            if index > table.current_limit {
                log::debug!("Invalid sampler index={}", index);
                return NULL_SAMPLER_ID;
            }
            if let Some(gpu_memory) = borrowed_gpu_memory {
                table.read_with(index, |gpu_addr, out| {
                    gpu_memory.read_block_unsafe(gpu_addr, out)
                })
            } else if let Some(gpu_memory) = channel_gpu_memory.as_ref() {
                table.read_with(index, |gpu_addr, out| {
                    gpu_memory.lock().read_block_unsafe(gpu_addr, out)
                })
            } else {
                table.read(device_memory.as_ref(), index)
            }
        };
        let map_index = index
            | if compute {
                common::slot_vector::SlotId::TAGGED_VALUE
            } else {
                0
            };
        if is_new {
            let id = self.find_sampler(&descriptor, compute);
            self.current_channel_state_mut()
                .sampler_ids
                .insert(map_index, id);
            return id;
        }
        *self
            .current_channel_state()
            .sampler_ids
            .get(&map_index)
            .expect("an unchanged descriptor must have a cached sampler id")
    }

    /// Look up or insert a sampler by its TSC descriptor.
    ///
    /// Port of `TextureCache<P>::FindSampler` (texture_cache.h:1873-1883):
    /// all-zero TSC → `NULL_SAMPLER_ID`; otherwise `try_emplace` into the
    /// `channel_state.samplers` HashMap and on first occurrence allocate
    /// a fresh slot in `slot_samplers`.
    pub fn find_sampler(
        &mut self,
        config: &crate::textures::texture::TscEntry,
        _compute: bool,
    ) -> SamplerId {
        use crate::texture_cache::types::NULL_SAMPLER_ID;
        // Upstream `std::ranges::all_of(config.raw, [](u64 v){ return v == 0; })`.
        if config.raw.iter().all(|&w| w == 0) {
            return NULL_SAMPLER_ID;
        }
        if let Some(&id) = self.current_channel_state_mut().samplers.get(config) {
            return id;
        }
        let id = self.insert_typed_sampler(*config);
        self.current_channel_state_mut()
            .samplers
            .insert(*config, id);
        self.enforce_sampler_budget();
        id
    }

    /// Port of `TextureCache<P>::EnforceSamplerBudget`.
    fn enforce_sampler_budget(&mut self) {
        let Some(budget) = self.sampler_heap_budget else {
            return;
        };
        if self.slot_samplers.size() < budget
            || !self.channel_caches.has_current_channel_state()
            || self.last_sampler_gc_frame == self.frame_tick
        {
            return;
        }
        self.last_sampler_gc_frame = self.frame_tick;
        self.trim_inactive_samplers(budget);
    }

    /// Port of `TextureCache<P>::TrimInactiveSamplers`.
    fn trim_inactive_samplers(&mut self, budget: usize) {
        const SAMPLER_GC_SLACK: usize = 1024;
        let active_sampler_ids: HashSet<SamplerId, BuildUnorderedDenseHasher> = {
            let channel = self.current_channel_state();
            channel.sampler_ids.values().copied().collect()
        };
        let cached_samplers: Vec<_> = self
            .current_channel_state()
            .samplers
            .iter()
            .map(|(config, id)| (*config, *id))
            .collect();
        let mut removed_configs = Vec::new();
        let mut removed = 0usize;
        for (config, sampler_id) in cached_samplers {
            if !sampler_id.is_valid() || sampler_id == CORRUPT_ID {
                removed_configs.push(config);
                continue;
            }
            if active_sampler_ids.contains(&sampler_id) {
                continue;
            }
            self.slot_samplers.erase(sampler_id);
            removed_configs.push(config);
            removed += 1;
            if self.slot_samplers.size().wrapping_add(SAMPLER_GC_SLACK) <= budget {
                break;
            }
        }
        if !removed_configs.is_empty() {
            let channel = self.current_channel_state_mut();
            for config in removed_configs {
                channel.samplers.remove(&config);
            }
        }
        if removed != 0 {
            log::warn!(
                "Sampler cache exceeded {} entries on this driver; reclaimed {} inactive samplers",
                budget,
                removed,
            );
        }
    }

    // ── Render targets ─────────────────────────────────────────────────

    /// Port of `TextureCache<P>::RescaleRenderTargets`.
    ///
    /// The register values are carried by the draw-time snapshot because the
    /// Rust command path cannot retain a borrow of Maxwell3D across the
    /// rasterizer call. Backend operations remain policy callbacks, matching
    /// the upstream `TextureCache<P>` specialization boundary.
    fn rescale_render_targets(
        &mut self,
        regs: &Maxwell3DRenderTargets,
        dirty_access: &mut impl RenderTargetDirtyFlagAccess,
        gpu_to_cpu: &mut impl FnMut(GPUVAddr, u64) -> Option<u64>,
    ) -> bool {
        let mut scale_rating: u32;
        let mut rescaled;
        let mut color_images = [None; NUM_RT];
        let mut depth_image = None;

        loop {
            dirty_access.clear_render_target_dirty_flag(crate::dirty_flags::flags::RENDER_TARGETS);
            self.has_deleted_images = false;
            let force = dirty_access
                .render_target_dirty_flag(crate::dirty_flags::flags::RENDER_TARGET_CONTROL);
            dirty_access
                .clear_render_target_dirty_flag(crate::dirty_flags::flags::RENDER_TARGET_CONTROL);

            for index in 0..NUM_RT {
                let color_flag = crate::dirty_flags::flags::COLOR_BUFFER0 + index as u8;
                if !force && !dirty_access.render_target_dirty_flag(color_flag) {
                    continue;
                }
                dirty_access.clear_render_target_dirty_flag(color_flag);
                let view_id = self.find_color_buffer_from_snapshot(index, regs, gpu_to_cpu);
                self.bind_color_render_target(index, view_id);
            }
            if force
                || dirty_access.render_target_dirty_flag(crate::dirty_flags::flags::ZETA_BUFFER)
            {
                dirty_access.clear_render_target_dirty_flag(crate::dirty_flags::flags::ZETA_BUFFER);
                let view_id = self.find_depth_buffer_from_snapshot(regs, gpu_to_cpu);
                self.bind_depth_render_target(view_id);
            }

            scale_rating = 0;
            let mut any_rescaled = false;
            let mut can_rescale = true;
            for (index, saved) in color_images.iter_mut().enumerate() {
                self.check_render_target_rescale(
                    self.render_targets.color_buffer_ids[index],
                    saved,
                    &mut can_rescale,
                    &mut any_rescaled,
                    &mut scale_rating,
                );
            }
            self.check_render_target_rescale(
                self.render_targets.depth_buffer_id,
                &mut depth_image,
                &mut can_rescale,
                &mut any_rescaled,
                &mut scale_rating,
            );

            if can_rescale {
                rescaled = any_rescaled || scale_rating >= 2;
                if rescaled {
                    for image_id in color_images.iter().flatten().copied().chain(depth_image) {
                        self.scale_up(image_id);
                    }
                    scale_rating = 2;
                }
            } else {
                rescaled = false;
                for image_id in color_images.iter().flatten().copied().chain(depth_image) {
                    self.scale_down(image_id);
                }
                scale_rating = 1;
            }

            // Upstream `InvalidateScale` writes the same live Maxwell dirty
            // array consumed by this loop. Rust can receive a draw snapshot
            // adapter instead, so mirror those writes into the adapter before
            // repeating the loop after image-view deletion.
            if self.has_deleted_images {
                dirty_access
                    .set_render_target_dirty_flag(crate::dirty_flags::flags::RENDER_TARGETS);
                dirty_access.set_render_target_dirty_flag(crate::dirty_flags::flags::ZETA_BUFFER);
                for index in 0..NUM_RT {
                    dirty_access.set_render_target_dirty_flag(
                        crate::dirty_flags::flags::COLOR_BUFFER0 + index as u8,
                    );
                }
            }

            if !self.has_deleted_images {
                break;
            }
        }

        for image_id in color_images.iter().flatten().copied().chain(depth_image) {
            let image = &mut self.slot_images[image_id];
            image.scale_rating = scale_rating;
            if image.scale_tick <= self.frame_tick {
                image.scale_tick = self.frame_tick.wrapping_add(1);
            }
        }
        rescaled
    }

    fn check_render_target_rescale(
        &mut self,
        view_id: ImageViewId,
        saved: &mut Option<ImageId>,
        can_rescale: &mut bool,
        any_rescaled: &mut bool,
        scale_rating: &mut u32,
    ) {
        if !view_id.is_valid() || view_id == NULL_IMAGE_VIEW_ID {
            *saved = None;
            return;
        }
        let image_id = self.slot_image_views[view_id].image_id;
        *saved = Some(image_id);
        *can_rescale &= self.image_can_rescale(image_id);
        let image = &self.slot_images[image_id];
        *any_rescaled |= image.flags.contains(ImageFlagBits::RESCALED)
            || crate::surface::get_format_type(image.info.format)
                != crate::surface::SurfaceType::ColorTexture;
        *scale_rating = (*scale_rating).max(if image.scale_tick <= self.frame_tick {
            image.scale_rating.wrapping_add(1)
        } else {
            image.scale_rating
        });
    }

    fn find_color_buffer_from_snapshot(
        &mut self,
        index: usize,
        regs: &Maxwell3DRenderTargets,
        gpu_to_cpu: &mut impl FnMut(GPUVAddr, u64) -> Option<u64>,
    ) -> ImageViewId {
        if index >= regs.rt_control.count as usize {
            return ImageViewId::default();
        }
        let rt = regs.render_targets[index];
        if rt.address == 0 || rt.format == 0 {
            return ImageViewId::default();
        }
        let info = ImageInfo::from_render_target_info(&rt, regs.anti_alias_samples_mode);
        self.find_render_target_view_from_snapshot(&info, rt.address, gpu_to_cpu)
    }

    fn find_depth_buffer_from_snapshot(
        &mut self,
        regs: &Maxwell3DRenderTargets,
        gpu_to_cpu: &mut impl FnMut(GPUVAddr, u64) -> Option<u64>,
    ) -> ImageViewId {
        let zeta = regs.zeta;
        if !zeta.enabled || zeta.address == 0 {
            return ImageViewId::default();
        }
        let info = ImageInfo::from_zeta_info(&zeta, regs.anti_alias_samples_mode);
        self.find_render_target_view_from_snapshot(&info, zeta.address, gpu_to_cpu)
    }

    fn find_render_target_view_from_snapshot(
        &mut self,
        info: &ImageInfo,
        gpu_addr: GPUVAddr,
        gpu_to_cpu: &mut impl FnMut(GPUVAddr, u64) -> Option<u64>,
    ) -> ImageViewId {
        let guest_size = super::util::calculate_guest_size_in_bytes(info) as u64;
        let cpu_addr = gpu_to_cpu(gpu_addr, guest_size)
            .unwrap_or_else(|| self.resolve_or_allocate_cpu_addr(gpu_addr, guest_size));
        let mut delete_state = self.has_deleted_images;
        let image_id = loop {
            self.has_deleted_images = false;
            let image_id = self.find_or_insert_image_from_info_with_options(
                info,
                gpu_addr,
                cpu_addr,
                RelaxedOptions::empty(),
            );
            delete_state |= self.has_deleted_images;
            if !self.has_deleted_images {
                break image_id;
            }
        };
        self.has_deleted_images = delete_state;
        if !image_id.is_valid() || image_id == NULL_IMAGE_ID {
            return NULL_IMAGE_VIEW_ID;
        }
        self.find_image_view_from_image_info(image_id, info, gpu_addr)
    }

    fn is_full_clear_from_snapshot(
        &self,
        view_id: ImageViewId,
        clear_scissor: Option<(u32, u32, u32, u32)>,
    ) -> bool {
        if !view_id.is_valid() || view_id == NULL_IMAGE_VIEW_ID {
            return true;
        }
        let view = &self.slot_image_views[view_id];
        let image = &self.slot_images[view.image_id];
        if image.info.resources.levels > 1 || image.info.resources.layers > 1 {
            return false;
        }
        let Some((min_x, min_y, max_x, max_y)) = clear_scissor else {
            return true;
        };
        min_x == 0 && min_y == 0 && max_x >= view.size.width && max_y >= view.size.height
    }

    /// Port of `TextureCache<P>::UpdateRenderTargets`.
    pub fn update_render_targets_with_snapshot(
        &mut self,
        regs: &Maxwell3DRenderTargets,
        dirty_access: &mut impl RenderTargetDirtyFlagAccess,
        mut gpu_to_cpu: impl FnMut(GPUVAddr, u64) -> Option<u64>,
        is_clear: bool,
        clear_scissor: Option<(u32, u32, u32, u32)>,
    ) {
        if !dirty_access.render_target_dirty_flag(crate::dirty_flags::flags::RENDER_TARGETS) {
            for view_id in self.render_targets.color_buffer_ids {
                let invalidate =
                    is_clear && self.is_full_clear_from_snapshot(view_id, clear_scissor);
                P::prepare_image_view(self, view_id, true, invalidate);
            }
            let depth_id = self.render_targets.depth_buffer_id;
            let invalidate = is_clear && self.is_full_clear_from_snapshot(depth_id, clear_scissor);
            P::prepare_image_view(self, depth_id, true, invalidate);
            return;
        }

        let previous_render_targets = self.render_targets;
        let rescaled = self.rescale_render_targets(regs, dirty_access, &mut gpu_to_cpu);
        if self.is_rescaling != rescaled {
            dirty_access.set_render_target_dirty_flag(crate::dirty_flags::flags::RESCALE_VIEWPORTS);
            dirty_access.set_render_target_dirty_flag(crate::dirty_flags::flags::RESCALE_SCISSORS);
            self.is_rescaling = rescaled;
        }

        for view_id in self.render_targets.color_buffer_ids {
            let invalidate = is_clear && self.is_full_clear_from_snapshot(view_id, clear_scissor);
            P::prepare_image_view(self, view_id, true, invalidate);
        }
        let depth_id = self.render_targets.depth_buffer_id;
        let invalidate = is_clear && self.is_full_clear_from_snapshot(depth_id, clear_scissor);
        P::prepare_image_view(self, depth_id, true, invalidate);

        self.rt_active_mask = 0;
        self.rt_image_id = [ImageId::default(); NUM_RT];
        for index in 0..NUM_RT {
            let view_id = self.render_targets.color_buffer_ids[index];
            if view_id.is_valid() && view_id != NULL_IMAGE_VIEW_ID {
                self.rt_active_mask |= 1 << index;
                self.rt_image_id[index] = self.slot_image_views[view_id].image_id;
            }
        }
        if depth_id.is_valid() && depth_id != NULL_IMAGE_VIEW_ID {
            self.rt_active_mask |= 1 << NUM_RT;
            self.rt_depth_image_id = self.slot_image_views[depth_id].image_id;
        } else {
            self.rt_depth_image_id = ImageId::default();
        }

        for index in 0..NUM_RT {
            self.render_targets.draw_buffers[index] = regs.rt_control.map[index] as u8;
        }
        let resolution = common::settings::values().resolution_info.clone();
        let (up_scale, down_shift) = if self.is_rescaling {
            (resolution.up_scale, resolution.down_shift)
        } else {
            (1, 0)
        };
        self.render_targets.size = Extent2D {
            width: regs.surface_clip.width.wrapping_mul(up_scale) >> down_shift,
            height: regs.surface_clip.height.wrapping_mul(up_scale) >> down_shift,
        };
        self.render_targets.is_rescaled = self.is_rescaling;
        if self.render_targets != previous_render_targets {
            self.render_targets_serial = self.render_targets_serial.wrapping_add(1);
        }
        dirty_access.set_render_target_dirty_flag(crate::dirty_flags::flags::DEPTH_BIAS_GLOBAL);
    }

    /// Port of `TextureCache<P>::GetFramebuffer`.
    pub fn get_framebuffer(&mut self) -> Result<&mut P::Framebuffer, P::FramebufferError> {
        if self.last_framebuffer_id.is_valid()
            && self.last_framebuffer_serial == self.render_targets_serial
        {
            return Ok(&mut self.slot_framebuffers[self.last_framebuffer_id]);
        }
        let key = self.render_targets;
        let framebuffer_id = self.get_framebuffer_id(&key)?;
        self.last_framebuffer_id = framebuffer_id;
        self.last_framebuffer_serial = self.render_targets_serial;
        Ok(&mut self.slot_framebuffers[framebuffer_id])
    }

    /// Port of `TextureCache<P>::GetFramebufferId`.
    pub(crate) fn get_framebuffer_id(
        &mut self,
        key: &RenderTargets,
    ) -> Result<FramebufferId, P::FramebufferError> {
        if let Some(&framebuffer_id) = self.framebuffers.get(key) {
            return Ok(framebuffer_id);
        }

        let mut color_buffers = [None; NUM_RT];
        for (index, &view_id) in key.color_buffer_ids.iter().enumerate() {
            if !view_id.is_valid() || view_id == NULL_IMAGE_VIEW_ID {
                continue;
            }
            let backend_view = self.slot_image_views[view_id]
                .backend
                .as_ref()
                .expect("render-target image view must be prepared before GetFramebuffer");
            color_buffers[index] = Some(std::ptr::NonNull::from(backend_view));
        }
        let depth_buffer =
            if key.depth_buffer_id.is_valid() && key.depth_buffer_id != NULL_IMAGE_VIEW_ID {
                let backend_view = self.slot_image_views[key.depth_buffer_id]
                    .backend
                    .as_ref()
                    .expect("depth image view must be prepared before GetFramebuffer");
                Some(std::ptr::NonNull::from(backend_view))
            } else {
                None
            };

        let framebuffer = P::create_framebuffer(
            self.runtime.as_deref_mut(),
            color_buffers,
            depth_buffer,
            key,
        )?;
        let framebuffer_id = self.slot_framebuffers.insert(framebuffer);
        self.framebuffers.insert(*key, framebuffer_id);
        Ok(framebuffer_id)
    }

    /// Rust bridge for `TextureCache<P>::UpdateRenderTargets` while the cache
    /// does not own a `Maxwell3D*`.
    ///
    /// Upstream reads `maxwell3d->regs.rt_control` and `maxwell3d->regs.rt[]`
    /// directly from this owner. The Rust draw path snapshots those registers
    /// into `Maxwell3DRenderTargets` and provides the channel GPU->CPU translator here.
    pub fn update_render_targets_from_snapshot(
        &mut self,
        render_targets: &Maxwell3DRenderTargets,
        gpu_to_cpu: impl FnMut(GPUVAddr, u64) -> Option<u64>,
    ) {
        let mut dirty_flags = [false; 256];
        dirty_flags[crate::dirty_flags::flags::RENDER_TARGETS as usize] = true;
        dirty_flags[crate::dirty_flags::flags::RENDER_TARGET_CONTROL as usize] = true;
        let mut dirty_access = dirty_flags;
        self.update_render_targets_with_snapshot(
            render_targets,
            &mut dirty_access,
            gpu_to_cpu,
            false,
            None,
        );
    }

    pub fn update_render_targets_from_snapshot_with_dirty_flags(
        &mut self,
        render_targets: &Maxwell3DRenderTargets,
        dirty_flags: &[bool; 256],
        gpu_to_cpu: impl FnMut(GPUVAddr, u64) -> Option<u64>,
    ) {
        let mut dirty_access = *dirty_flags;
        self.update_render_targets_with_snapshot(
            render_targets,
            &mut dirty_access,
            gpu_to_cpu,
            false,
            None,
        );
    }

    fn queue_preemptive_render_target_download(&mut self, new_id: ImageViewId) {
        if !new_id.is_valid() || new_id == NULL_IMAGE_VIEW_ID {
            return;
        }
        let new_view = &self.slot_image_views[new_id];
        if new_view
            .flags
            .contains(ImageViewFlagBits::PREEMTIVE_DOWNLOAD)
        {
            self.uncommitted_downloads.push(PendingDownload {
                is_swizzle: true,
                async_buffer_id: 0,
                object_id: new_view.image_id,
            });
        }
    }

    pub(crate) fn bind_color_render_target(&mut self, index: usize, new_id: ImageViewId) {
        if self.render_targets.color_buffer_ids[index] == new_id {
            return;
        }
        self.queue_preemptive_render_target_download(new_id);
        self.render_targets.color_buffer_ids[index] = new_id;
    }

    pub(crate) fn bind_depth_render_target(&mut self, new_id: ImageViewId) {
        if self.render_targets.depth_buffer_id == new_id {
            return;
        }
        self.queue_preemptive_render_target_download(new_id);
        self.render_targets.depth_buffer_id = new_id;
    }

    pub fn find_or_insert_image_from_info(
        &mut self,
        info: &ImageInfo,
        gpu_addr: GPUVAddr,
        cpu_addr: u64,
    ) -> ImageId {
        self.find_or_insert_image_from_info_with_options(
            info,
            gpu_addr,
            cpu_addr,
            RelaxedOptions::empty(),
        )
    }

    /// CPU-address-aware counterpart of upstream
    /// `TextureCache<P>::FindOrInsertImage(info, gpu_addr, options)`.
    pub fn find_or_insert_image_from_info_with_options(
        &mut self,
        info: &ImageInfo,
        gpu_addr: GPUVAddr,
        cpu_addr: u64,
        options: RelaxedOptions,
    ) -> ImageId {
        if let Some(image_id) = self.find_image_in_cpu_region_with_caps(
            info,
            gpu_addr,
            cpu_addr,
            options,
            self.has_broken_texture_view_formats,
            self.has_native_bgr,
        ) {
            return image_id;
        }
        self.insert_image_from_info_with_options(info, gpu_addr, cpu_addr, options)
    }

    /// CPU-address-aware counterpart of upstream `TextureCache<P>::InsertImage`.
    fn insert_image_from_info_with_options(
        &mut self,
        info: &ImageInfo,
        gpu_addr: GPUVAddr,
        cpu_addr: u64,
        _options: RelaxedOptions,
    ) -> ImageId {
        let image_id = self.join_images(info, gpu_addr, cpu_addr);
        // Upstream `InsertImage` registers the new ImageId in image_allocs_table
        // immediately after the synchronous `JoinImages` lifecycle. Delaying
        // this allocation-table update makes a second lookup in the same
        // region create a duplicate ImageId for the same surface.
        self.register_image_alloc(image_id);
        image_id
    }

    pub(crate) fn register_image_alloc(&mut self, image_id: ImageId) {
        if !image_id.is_valid() {
            return;
        }
        let gpu_addr = self.slot_images[image_id].gpu_addr;
        let alloc_id = if let Some(&alloc_id) = self.image_allocs_table.get(&gpu_addr) {
            alloc_id
        } else {
            let alloc_id = self
                .slot_image_allocs
                .insert(ImageAllocBase::default().into());
            self.image_allocs_table.insert(gpu_addr, alloc_id);
            alloc_id
        };
        let alloc_images = &mut self.slot_image_allocs[alloc_id].images;
        if !alloc_images.contains(&image_id) {
            alloc_images.push(image_id);
        }
    }

    /// Port of `TextureCache<P>::FindRenderTargetView` after the target image
    /// has been found or inserted.
    pub fn find_render_target_view_from_image(
        &mut self,
        image_id: ImageId,
        rt: &RenderTargetInfo,
        anti_alias_samples_mode: u32,
        gpu_addr: GPUVAddr,
    ) -> ImageViewId {
        let rt_info = ImageInfo::from_render_target_info(rt, anti_alias_samples_mode);
        self.find_image_view_from_image_info(image_id, &rt_info, gpu_addr)
    }

    pub fn find_image_view_from_image_info(
        &mut self,
        image_id: ImageId,
        rt_info: &ImageInfo,
        gpu_addr: GPUVAddr,
    ) -> ImageViewId {
        let image = &self.slot_images[image_id];
        let view_type = super::util::render_target_image_view_type(&rt_info);
        let base = if image.info.image_type == ImageType::Linear {
            SubresourceBase { level: 0, layer: 0 }
        } else {
            image
                .try_find_base(gpu_addr)
                .expect("TextureCache::FindRenderTargetView TryFindBase failed")
        };
        let layers = if image.info.image_type == ImageType::E3D {
            rt_info.size.depth as i32
        } else {
            rt_info.resources.layers
        };
        let info = ImageViewInfo::for_render_target(
            view_type,
            rt_info.format,
            SubresourceRange {
                base,
                extent: SubresourceExtent { levels: 1, layers },
            },
        );
        self.find_or_emplace_image_view(image_id, info, gpu_addr)
    }

    /// Port of `TextureCache<P>::FindOrEmplaceImageView`.
    pub fn find_or_emplace_image_view(
        &mut self,
        image_id: ImageId,
        info: ImageViewInfo,
        gpu_addr: GPUVAddr,
    ) -> ImageViewId {
        let existing = self.slot_images[image_id].find_view(&info);
        if existing.is_valid() {
            return existing;
        }

        let image_info = self.slot_images[image_id].info.clone();
        let view = ImageViewBase::new(&info, &image_info, image_id, gpu_addr);
        let view_id = self.insert_typed_image_view(info, view, Some(image_id));
        self.slot_images[image_id].insert_view(info, view_id);
        view_id
    }

    /// Port of `TextureCache<P>::JoinImages`.
    ///
    /// Resolves overlapping images by computing aliases and copies to do.
    pub fn join_images(
        &mut self,
        info: &ImageInfo,
        mut gpu_addr: GPUVAddr,
        mut cpu_addr: u64,
    ) -> ImageId {
        let mut new_info = info.clone();
        let size_bytes = super::util::calculate_guest_size_in_bytes(&new_info) as usize;
        let broken_views = self.has_broken_texture_view_formats;
        let native_bgr = self.has_native_bgr;

        self.join_overlap_ids.clear();
        self.join_overlaps_found.clear();
        self.join_left_aliased_ids.clear();
        self.join_right_aliased_ids.clear();
        self.join_ignore_textures.clear();
        self.join_bad_overlap_ids.clear();
        self.join_copies_to_do.clear();
        self.join_alias_indices.clear();

        let this_is_linear = info.image_type == ImageType::Linear;
        let page_table = &self.page_table;
        let slot_map_views = &mut self.slot_map_views;
        let slot_images = &mut self.slot_images;
        let join_ignore_textures = &mut self.join_ignore_textures;
        let join_left_aliased_ids = &mut self.join_left_aliased_ids;
        let join_overlaps_found = &mut self.join_overlaps_found;
        let join_overlap_ids = &mut self.join_overlap_ids;
        let join_copies_to_do = &mut self.join_copies_to_do;
        let join_right_aliased_ids = &mut self.join_right_aliased_ids;
        let join_bad_overlap_ids = &mut self.join_bad_overlap_ids;
        Self::for_each_image_in_region_parts(
            page_table,
            slot_map_views,
            slot_images,
            cpu_addr,
            size_bytes,
            |overlap_id, overlap, _| {
                if overlap.flags.contains(ImageFlagBits::REMAPPED) {
                    join_ignore_textures.insert(overlap_id);
                    return false;
                }
                let overlap_is_linear = overlap.info.image_type == ImageType::Linear;
                if this_is_linear != overlap_is_linear {
                    return false;
                }
                if this_is_linear && overlap_is_linear {
                    if info.pitch() == overlap.info.pitch() && gpu_addr == overlap.gpu_addr {
                        join_left_aliased_ids.push(overlap_id);
                    }
                    return false;
                }

                join_overlaps_found.insert(overlap_id);
                if let Some(solution) = super::util::resolve_overlap(
                    &new_info,
                    gpu_addr,
                    cpu_addr,
                    overlap,
                    true,
                    broken_views,
                    native_bgr,
                ) {
                    gpu_addr = solution.gpu_addr;
                    cpu_addr = solution.cpu_addr;
                    new_info.resources = solution.resources;
                    join_overlap_ids.push(overlap_id);
                    join_copies_to_do.push(JoinCopy {
                        is_alias: false,
                        id: overlap_id,
                    });
                    return false;
                }

                let options = RelaxedOptions::SIZE | RelaxedOptions::FORMAT;
                let new_image_base = ImageBase::new(new_info.clone(), gpu_addr, cpu_addr);
                if super::util::is_subresource(
                    &new_info,
                    overlap,
                    gpu_addr,
                    options,
                    broken_views,
                    native_bgr,
                ) {
                    join_left_aliased_ids.push(overlap_id);
                    overlap.flags.insert(ImageFlagBits::ALIAS);
                    join_copies_to_do.push(JoinCopy {
                        is_alias: true,
                        id: overlap_id,
                    });
                } else if super::util::is_subresource(
                    &overlap.info,
                    &new_image_base,
                    overlap.gpu_addr,
                    options,
                    broken_views,
                    native_bgr,
                ) {
                    join_right_aliased_ids.push(overlap_id);
                    overlap.flags.insert(ImageFlagBits::ALIAS);
                    join_copies_to_do.push(JoinCopy {
                        is_alias: true,
                        id: overlap_id,
                    });
                } else {
                    join_bad_overlap_ids.push(overlap_id);
                }
                false
            },
        );
        if let Some(table_index) = self.current_gpu_page_table_index(true) {
            let sparse_page_table = &self.gpu_page_table_storage[table_index];
            let slot_images = &mut self.slot_images;
            let join_overlaps_found = &self.join_overlaps_found;
            let join_ignore_textures = &mut self.join_ignore_textures;
            Self::for_each_image_in_gpu_region_parts(
                sparse_page_table,
                slot_images,
                gpu_addr,
                size_bytes,
                |overlap_id, overlap| {
                    if join_overlaps_found.contains(&overlap_id) {
                        return false;
                    }
                    if overlap.flags.contains(ImageFlagBits::REMAPPED)
                        || (overlap.gpu_addr == gpu_addr
                            && overlap.guest_size_bytes as usize == size_bytes)
                    {
                        join_ignore_textures.insert(overlap_id);
                    }
                    false
                },
            );
        }

        let join_copies = self.join_copies_to_do.clone();
        let mut can_rescale = info.rescaleable;
        let mut any_rescaled = false;
        for copy in &join_copies {
            if !can_rescale {
                break;
            }
            can_rescale &= self.image_can_rescale(copy.id);
            any_rescaled |= self.slot_images[copy.id]
                .flags
                .contains(ImageFlagBits::RESCALED);
        }
        can_rescale &= any_rescaled;

        for copy in &join_copies {
            if can_rescale {
                self.scale_up(copy.id);
            } else {
                self.scale_down(copy.id);
            }
        }

        let new_image_id =
            self.insert_typed_image(ImageBase::new(new_info.clone(), gpu_addr, cpu_addr));
        if new_info.is_sparse {
            let gpu_memory = self
                .channel_gpu_memory
                .as_ref()
                .expect("TextureCache::join_images sparse image requires channel GPU memory")
                .lock();
            if !gpu_memory.is_continuous_range(gpu_addr, size_bytes as u64) {
                self.slot_images[new_image_id]
                    .flags
                    .insert(ImageFlagBits::SPARSE);
            }
        }

        for overlap_id in self.join_ignore_textures.clone() {
            if !overlap_id.is_valid() || overlap_id == NULL_IMAGE_ID {
                continue;
            }
            if self.slot_images[overlap_id]
                .flags
                .contains(ImageFlagBits::GPU_MODIFIED)
            {
                log::error!(
                    "TextureCache::JoinImages ignored GPU-modified overlap id={} gpu=0x{:X}",
                    overlap_id.index,
                    self.slot_images[overlap_id].gpu_addr,
                );
            }
            if self.slot_images[overlap_id]
                .flags
                .contains(ImageFlagBits::TRACKED)
            {
                self.untrack_image(overlap_id);
            }
            self.unregister_image(overlap_id);
            self.delete_image(overlap_id, false);
        }

        self.refresh_contents(new_image_id);
        if can_rescale {
            self.scale_up(new_image_id);
        } else {
            self.scale_down(new_image_id);
        }

        self.join_copies_to_do
            .sort_by_key(|copy| self.slot_images[copy.id].modification_tick);
        self.join_alias_indices = self.apply_join_relations(
            new_image_id,
            &self.join_right_aliased_ids.clone(),
            &self.join_left_aliased_ids.clone(),
            &self.join_bad_overlap_ids.clone(),
        );

        for copy_object in self.join_copies_to_do.clone() {
            if copy_object.is_alias {
                if !self.slot_images[copy_object.id].is_safe_download() {
                    continue;
                }
                let Some(&alias_index) = self.join_alias_indices.get(&copy_object.id) else {
                    continue;
                };
                let Some(alias) = self.slot_images[new_image_id]
                    .aliased_images
                    .get(alias_index)
                    .cloned()
                else {
                    continue;
                };
                self.copy_image(new_image_id, alias.id, &alias.copies);
                self.slot_images[new_image_id].modification_tick =
                    self.slot_images[copy_object.id].modification_tick;
                continue;
            }

            let overlap_gpu_modified = self.slot_images[copy_object.id]
                .flags
                .contains(ImageFlagBits::GPU_MODIFIED);
            if overlap_gpu_modified {
                self.slot_images[new_image_id]
                    .flags
                    .insert(ImageFlagBits::GPU_MODIFIED);
                let overlap_gpu_addr = self.slot_images[copy_object.id].gpu_addr;
                let base = self.slot_images[new_image_id]
                    .try_find_base(overlap_gpu_addr)
                    .expect("TextureCache::JoinImages overlap base must exist");
                let resolution = common::settings::values().resolution_info.clone();
                let up_scale = if can_rescale { resolution.up_scale } else { 1 };
                let down_shift = if can_rescale {
                    resolution.down_shift
                } else {
                    0
                };
                let overlap_info = self.slot_images[copy_object.id].info.clone();
                let copies = super::util::make_shrink_image_copies(
                    &new_info,
                    &overlap_info,
                    base,
                    up_scale,
                    down_shift,
                );
                if overlap_info.num_samples != new_info.num_samples {
                    P::copy_image_msaa(self, new_image_id, copy_object.id, &copies);
                } else {
                    P::copy_image(self, new_image_id, copy_object.id, &copies);
                }
                self.slot_images[new_image_id].modification_tick =
                    self.slot_images[copy_object.id].modification_tick;
            }
            if self.slot_images[copy_object.id]
                .flags
                .contains(ImageFlagBits::TRACKED)
            {
                self.untrack_image(copy_object.id);
            }
            self.unregister_image(copy_object.id);
            self.delete_image(copy_object.id, false);
        }
        self.register_image(new_image_id);
        new_image_id
    }

    pub(crate) fn apply_join_relations(
        &mut self,
        new_image_id: ImageId,
        right_aliased_ids: &[ImageId],
        left_aliased_ids: &[ImageId],
        bad_overlap_ids: &[ImageId],
    ) -> std::collections::HashMap<ImageId, usize, BuildUnorderedDenseHasher> {
        let mut alias_indices = std::collections::HashMap::default();
        let new_image = self.slot_images[new_image_id].base.as_mut() as *mut ImageBase;
        for &aliased_id in right_aliased_ids {
            assert_ne!(aliased_id, new_image_id);
            let aliased = self.slot_images[aliased_id].base.as_mut() as *mut ImageBase;
            // SAFETY: `new_image_id` and `aliased_id` identify distinct boxed
            // bases. Neither allocation moves or is replaced while backend
            // payloads retain their inheritance pointers.
            let (new_image, aliased) = unsafe { (&mut *new_image, &mut *aliased) };
            let alias_index = new_image.aliased_images.len();
            if !add_image_alias(new_image, aliased, new_image_id, aliased_id) {
                continue;
            }
            alias_indices.insert(aliased_id, alias_index);
            new_image.flags.insert(ImageFlagBits::ALIAS);
        }
        for &aliased_id in left_aliased_ids {
            assert_ne!(aliased_id, new_image_id);
            let aliased = self.slot_images[aliased_id].base.as_mut() as *mut ImageBase;
            // SAFETY: same distinct stable allocations as above.
            let (new_image, aliased) = unsafe { (&mut *new_image, &mut *aliased) };
            let alias_index = new_image.aliased_images.len();
            if !add_image_alias(aliased, new_image, aliased_id, new_image_id) {
                continue;
            }
            alias_indices.insert(aliased_id, alias_index);
            new_image.flags.insert(ImageFlagBits::ALIAS);
        }

        for &aliased_id in bad_overlap_ids {
            self.slot_images[aliased_id]
                .overlapping_images
                .push(new_image_id);
            self.slot_images[new_image_id]
                .overlapping_images
                .push(aliased_id);
            let aliased_bad = {
                let aliased = &self.slot_images[aliased_id];
                aliased.info.resources.levels == 1
                    && aliased.info.block().depth == 0
                    && aliased.overlapping_images.len() > 1
            };
            if aliased_bad {
                self.slot_images[aliased_id]
                    .flags
                    .insert(ImageFlagBits::BAD_OVERLAP);
            }
            let new_bad = {
                let image = &self.slot_images[new_image_id];
                image.info.resources.levels == 1
                    && image.info.block().depth == 0
                    && image.overlapping_images.len() > 1
            };
            if new_bad {
                self.slot_images[new_image_id]
                    .flags
                    .insert(ImageFlagBits::BAD_OVERLAP);
            }
        }

        alias_indices
    }

    // ── Registration / tracking ────────────────────────────────────────

    /// Port of `TextureCache<P>::RegisterImage`.
    ///
    /// Inserts the image into page tables and marks it for CPU write-tracking.
    pub fn register_image(&mut self, image_id: ImageId) {
        debug_assert!(
            !self.slot_images[image_id]
                .flags
                .contains(ImageFlagBits::REGISTERED),
            "TextureCache::register_image: image already registered"
        );
        self.slot_images[image_id]
            .flags
            .insert(ImageFlagBits::REGISTERED);
        let memory_size = Self::registered_image_memory_size(&self.slot_images[image_id]);
        self.total_used_memory = self.total_used_memory.wrapping_add(memory_size);
        let lru_index = self.lru_cache.insert(image_id, self.frame_tick);
        self.slot_images[image_id].lru_index = lru_index;

        let (gpu_addr, guest_size_bytes, is_sparse) = {
            let image = &self.slot_images[image_id];
            (
                image.gpu_addr,
                image.guest_size_bytes as usize,
                image.flags.contains(ImageFlagBits::SPARSE),
            )
        };
        let table_index = self
            .current_gpu_page_table_index(false)
            .expect("TextureCache::register_image requires a bound GPU page table");
        let previous_owner = self
            .image_gpu_page_table_indices
            .insert(image_id, table_index);
        debug_assert!(
            previous_owner.is_none(),
            "TextureCache::register_image: image already has a GPU page-table owner"
        );
        let table = &mut self.gpu_page_table_storage[table_index];
        Self::for_each_gpu_page(gpu_addr, guest_size_bytes, |page| {
            table.entry(page).or_default().push(image_id);
        });

        if is_sparse {
            let segments = self.sparse_segments_for_image(image_id, "TextureCache::register_image");
            let mut sparse_maps = SmallVec::<[ImageMapId; 16]>::new();
            for (segment_gpu_addr, cpu_addr, segment_size) in segments {
                let map_id = self.slot_map_views.insert(ImageMapView::new(
                    segment_gpu_addr,
                    cpu_addr,
                    segment_size,
                    image_id,
                ));
                Self::for_each_cpu_page(cpu_addr, segment_size, |page| {
                    self.page_table.entry(page).or_default().push(map_id);
                });
                sparse_maps.push(map_id);
            }
            self.sparse_views.insert(image_id, sparse_maps);
            let table = &mut self.gpu_page_table_storage[table_index + 1];
            Self::for_each_gpu_page(gpu_addr, guest_size_bytes, |page| {
                table.entry(page).or_default().push(image_id);
            });
            return;
        }

        let image = &self.slot_images[image_id];
        let map_id = self.slot_map_views.insert(ImageMapView::new(
            image.gpu_addr,
            image.cpu_addr,
            image.guest_size_bytes as usize,
            image_id,
        ));
        self.slot_images[image_id].map_view_id = map_id;
        let (cpu_addr, size) = {
            let map = &self.slot_map_views[map_id];
            (map.cpu_addr, map.size)
        };
        Self::for_each_cpu_page(cpu_addr, size, |page| {
            self.page_table.entry(page).or_default().push(map_id);
        });
    }

    /// Port of `lru_cache.Touch(image.lru_index, frame_tick)` in
    /// `TextureCache<P>::PrepareImage`.
    pub fn touch_image(&mut self, image_id: ImageId) {
        if !image_id.is_valid() || image_id == NULL_IMAGE_ID {
            return;
        }
        let lru_index = self.slot_images[image_id].lru_index;
        if lru_index != usize::MAX {
            self.lru_cache.touch(lru_index, self.frame_tick);
        }
    }

    /// Port of `TextureCache<P>::UnregisterImage`.
    ///
    /// Removes the image from CPU page tables and clears registration state.
    pub fn unregister_image(&mut self, image_id: ImageId) {
        let (is_sparse, gpu_addr, guest_size_bytes, map_view_id, lru_index) = {
            let image = &mut self.slot_images[image_id];
            debug_assert!(
                image.flags.contains(ImageFlagBits::REGISTERED),
                "TextureCache::unregister_image: image not registered"
            );
            image.flags.remove(ImageFlagBits::REGISTERED);
            image.flags.remove(ImageFlagBits::BAD_OVERLAP);
            let lru_index = image.lru_index;
            image.lru_index = usize::MAX;
            (
                image.flags.contains(ImageFlagBits::SPARSE),
                image.gpu_addr,
                image.guest_size_bytes as usize,
                image.map_view_id,
                lru_index,
            )
        };
        if lru_index != usize::MAX {
            self.lru_cache.free(lru_index);
        }
        let table_index = self
            .image_gpu_page_table_indices
            .remove(&image_id)
            .expect("TextureCache::unregister_image missing GPU page-table owner");
        let table = &mut self.gpu_page_table_storage[table_index];
        Self::for_each_gpu_page(gpu_addr, guest_size_bytes, |page| {
            if let Some(image_ids) = table.get_mut(&page) {
                image_ids.retain(|&id| id != image_id);
                if image_ids.is_empty() {
                    table.remove(&page);
                }
            }
        });
        if is_sparse {
            let table = &mut self.gpu_page_table_storage[table_index + 1];
            Self::for_each_gpu_page(gpu_addr, guest_size_bytes, |page| {
                if let Some(image_ids) = table.get_mut(&page) {
                    image_ids.retain(|&id| id != image_id);
                    if image_ids.is_empty() {
                        table.remove(&page);
                    }
                }
            });
        }
        let map_ids = if is_sparse {
            self.sparse_views
                .remove(&image_id)
                .unwrap_or_default()
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            if map_view_id.is_valid() {
                vec![map_view_id]
            } else {
                Vec::new()
            }
        };

        for map_id in &map_ids {
            if !map_id.is_valid() {
                continue;
            }
            let map = &self.slot_map_views[*map_id];
            Self::for_each_cpu_page(map.cpu_addr, map.size, |page| {
                if let Some(image_map_ids) = self.page_table.get_mut(&page) {
                    image_map_ids.retain(|&id| id != *map_id);
                    if image_map_ids.is_empty() {
                        self.page_table.remove(&page);
                    }
                }
            });
        }
        for map_id in map_ids {
            if map_id.is_valid() {
                self.slot_map_views.erase(map_id);
            }
        }

        if !is_sparse {
            self.slot_images[image_id].map_view_id = ImageMapId::default();
        }
    }

    /// Port of `TextureCache<P>::TrackImage` (texture_cache.h:2113).
    ///
    /// Marks the image as `Tracked` and bumps the per-page cached count
    /// on the shared `MaxwellDeviceMemoryManager` so guest CPU writes to
    /// the image's backing range trigger cache invalidation. Handles both
    /// dense images (single contiguous range) and sparse images (multiple
    /// map views), matching upstream's branch on `ImageFlagBits::Sparse`.
    pub fn track_image(&mut self, image_id: ImageId) {
        let image = &mut self.slot_images[image_id];
        debug_assert!(
            !image.flags.contains(ImageFlagBits::TRACKED),
            "TextureCache::track_image: image already tracked"
        );
        image.flags.insert(ImageFlagBits::TRACKED);
        let is_sparse = image.flags.contains(ImageFlagBits::SPARSE);
        let registered = image.flags.contains(ImageFlagBits::REGISTERED);
        let cpu_addr = image.cpu_addr;
        let guest_size_bytes = image.guest_size_bytes;
        if !is_sparse {
            // Upstream guard: skip the "kernel" sentinel range
            // (`cpu_addr >= ~(1ULL << 40)`).
            if cpu_addr < !(1u64 << 40) {
                self.device_memory.update_pages_cached_count(
                    cpu_addr,
                    guest_size_bytes as usize,
                    1,
                );
            }
            return;
        }
        if registered {
            let sparse_maps = self
                .sparse_views
                .get(&image_id)
                .expect("sparse image missing from sparse_views")
                .clone();
            for map_view_id in sparse_maps {
                let map = &self.slot_map_views[map_view_id];
                self.device_memory
                    .update_pages_cached_count(map.cpu_addr, map.size, 1);
            }
            return;
        }
        for (_, cpu_addr, size) in
            self.sparse_segments_for_image(image_id, "TextureCache::track_image")
        {
            self.device_memory
                .update_pages_cached_count(cpu_addr, size, 1);
        }
    }

    /// Port of `TextureCache<P>::UntrackImage` (texture_cache.h:2141).
    ///
    /// Inverse of `track_image`: clears the `Tracked` flag and decrements
    /// the per-page cached count for the image's backing pages.
    pub fn untrack_image(&mut self, image_id: ImageId) {
        Self::untrack_image_parts(
            &self.device_memory,
            &self.sparse_views,
            &self.slot_map_views,
            image_id,
            &mut self.slot_images[image_id],
        );
    }

    /// Field-split body of upstream `TextureCache<P>::UntrackImage`.
    fn untrack_image_parts(
        device_memory: &crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager,
        sparse_views: &HashMap<ImageId, SmallVec<[ImageMapId; 16]>, BuildUnorderedDenseHasher>,
        slot_map_views: &common::slot_vector::SlotVector<ImageMapView>,
        image_id: ImageId,
        image: &mut ImageBase,
    ) {
        debug_assert!(
            image.flags.contains(ImageFlagBits::TRACKED),
            "TextureCache::untrack_image: image not tracked"
        );
        image.flags.remove(ImageFlagBits::TRACKED);
        let is_sparse = image.flags.contains(ImageFlagBits::SPARSE);
        let registered = image.flags.contains(ImageFlagBits::REGISTERED);
        let cpu_addr = image.cpu_addr;
        let guest_size_bytes = image.guest_size_bytes;
        if !is_sparse {
            if cpu_addr < !(1u64 << 40) {
                device_memory.update_pages_cached_count(cpu_addr, guest_size_bytes as usize, -1);
            }
            return;
        }
        debug_assert!(
            registered,
            "TextureCache::untrack_image: sparse image must be registered first"
        );
        let sparse_maps = sparse_views
            .get(&image_id)
            .expect("sparse image missing from sparse_views");
        for &map_view_id in sparse_maps {
            let map = &slot_map_views[map_view_id];
            device_memory.update_pages_cached_count(map.cpu_addr, map.size, -1);
        }
    }

    /// Port of `TextureCache<P>::DeleteImage`.
    ///
    /// Destroys the backend image and removes it from all data structures.
    /// `immediate_delete` corresponds to the upstream `bool immediate` parameter
    /// that determines whether the image is placed in the delayed-destruction
    /// ring or freed immediately.
    pub fn delete_image(&mut self, image_id: ImageId, immediate_delete: bool) {
        self.delete_image_impl(image_id, immediate_delete);
    }

    fn delete_image_impl(&mut self, image_id: ImageId, immediate_delete: bool) {
        let image_view_ids = self.slot_images[image_id].image_view_ids.clone();
        let aliased_images = self.slot_images[image_id].aliased_images.clone();
        let overlapping_images = self.slot_images[image_id].overlapping_images.clone();
        let gpu_addr = self.slot_images[image_id].gpu_addr;
        let registered_size = Self::registered_image_memory_size(&self.slot_images[image_id]);
        let scaled_size = if self.slot_images[image_id].has_scaled {
            Self::scaled_image_memory_size(&self.slot_images[image_id])
        } else {
            0
        };

        debug_assert!(
            !self.slot_images[image_id]
                .flags
                .contains(ImageFlagBits::TRACKED),
            "TextureCache::delete_image: image was not untracked"
        );
        debug_assert!(
            !self.slot_images[image_id]
                .flags
                .contains(ImageFlagBits::REGISTERED),
            "TextureCache::delete_image: image was not unregistered"
        );

        self.mark_render_targets_dirty();
        for view_id in &image_view_ids {
            for color_buffer_id in &mut self.render_targets.color_buffer_ids {
                if *color_buffer_id == *view_id {
                    *color_buffer_id = ImageViewId::default();
                }
            }
            if self.render_targets.depth_buffer_id == *view_id {
                self.render_targets.depth_buffer_id = ImageViewId::default();
            }
        }
        self.remove_image_view_references(&image_view_ids);
        self.remove_framebuffers(&image_view_ids);

        for alias in aliased_images {
            if alias.id == image_id {
                continue;
            }
            let other_image = &mut self.slot_images[alias.id];
            other_image
                .aliased_images
                .retain(|other_alias| other_alias.id != image_id);
            other_image.check_alias_state();
        }
        for overlap_id in overlapping_images {
            if overlap_id == image_id {
                continue;
            }
            let other_image = &mut self.slot_images[overlap_id];
            other_image
                .overlapping_images
                .retain(|&other_overlap_id| other_overlap_id != image_id);
            other_image.check_bad_overlap_state();
        }

        for image_view_id in image_view_ids {
            if image_view_id != NULL_IMAGE_VIEW_ID && image_view_id.is_valid() {
                if immediate_delete {
                    self.slot_image_views.erase(image_view_id);
                } else {
                    let image_view = self.slot_image_views.take(image_view_id);
                    self.sentenced_image_view.push(image_view);
                }
            }
        }

        if immediate_delete {
            self.slot_images.erase(image_id);
        } else {
            let image = self.slot_images.take(image_id);
            self.sentenced_images.push(image);
        }
        self.total_used_memory = self
            .total_used_memory
            .wrapping_sub(registered_size.wrapping_add(scaled_size));

        if let Some(alloc_id) = self.image_allocs_table.get(&gpu_addr).copied() {
            let alloc_images = &mut self.slot_image_allocs[alloc_id].images;
            alloc_images.retain(|&id| id != image_id);
            if alloc_images.is_empty() {
                self.slot_image_allocs.erase(alloc_id);
                self.image_allocs_table.remove(&gpu_addr);
            }
        }

        self.invalidate_channel_image_views();
        self.has_deleted_images = true;
    }

    pub(crate) fn invalidate_channel_image_views(&mut self) {
        self.for_each_active_channel_state_mut(|channel| {
            channel.graphics_image_table.invalidate();
            channel.compute_image_table.invalidate();
            for image_view_id in channel.image_view_ids.values_mut() {
                *image_view_id = CORRUPT_ID;
            }
        });
    }

    /// Port of `TextureCache<P>::InvalidateScale`.
    ///
    /// Invalidates image views and framebuffers that were created against the
    /// image's previous scale state.
    pub(crate) fn invalidate_scale(&mut self, image_id: ImageId) {
        if self.slot_images[image_id].scale_tick <= self.frame_tick {
            self.slot_images[image_id].scale_tick = self.frame_tick.wrapping_add(1);
        }

        let image_view_ids = self.slot_images[image_id].image_view_ids.clone();
        self.mark_render_targets_dirty();
        for image_view_id in &image_view_ids {
            for color_buffer_id in &mut self.render_targets.color_buffer_ids {
                if *color_buffer_id == *image_view_id {
                    *color_buffer_id = ImageViewId::default();
                }
            }
            if self.render_targets.depth_buffer_id == *image_view_id {
                self.render_targets.depth_buffer_id = ImageViewId::default();
            }
        }
        self.remove_image_view_references(&image_view_ids);
        self.remove_framebuffers(&image_view_ids);
        for image_view_id in &image_view_ids {
            if *image_view_id != NULL_IMAGE_VIEW_ID && image_view_id.is_valid() {
                let image_view = self.slot_image_views.take(*image_view_id);
                self.sentenced_image_view.push(image_view);
            }
        }
        self.slot_images[image_id].image_view_ids.clear();
        self.slot_images[image_id].image_view_infos.clear();
        self.invalidate_channel_image_views();
        self.has_deleted_images = true;
    }

    /// Port of `TextureCache<P>::RemoveImageViewReferences`.
    pub(crate) fn remove_image_view_references(&mut self, removed_views: &[ImageViewId]) {
        self.for_each_active_channel_state_mut(|channel| {
            channel
                .image_views
                .retain(|_, id| !removed_views.contains(id));
        });
    }

    /// Port of `TextureCache<P>::RemoveFramebuffers`.
    fn remove_framebuffers(&mut self, removed_views: &[ImageViewId]) {
        let last_framebuffer_id = self.last_framebuffer_id;
        let mut removed_framebuffers = Vec::new();
        self.framebuffers.retain(|key, framebuffer_id| {
            if !key.contains(removed_views) {
                return true;
            }
            if framebuffer_id.is_valid() {
                removed_framebuffers.push(*framebuffer_id);
            }
            false
        });
        if removed_framebuffers.contains(&last_framebuffer_id) {
            self.last_framebuffer_id = FramebufferId::default();
            self.last_framebuffer_serial = 0;
        }
        for framebuffer_id in removed_framebuffers {
            if self.slot_framebuffers.contains(framebuffer_id) {
                let framebuffer = self.slot_framebuffers.take(framebuffer_id);
                self.sentenced_framebuffers.push(framebuffer);
            }
        }
    }

    // ── Blit ───────────────────────────────────────────────────────────

    /// Port of upstream `TextureCache<P>::GetBlitImages`.
    ///
    /// Image lookup, insertion, format deduction and retry ordering remain
    /// owned here, matching `texture_cache.h`. The bound channel memory
    /// manager supplies translation and `InsertImage` allocates upstream's
    /// virtual-invalid CPU range when translation fails.
    pub fn get_blit_images(
        &mut self,
        dst: &crate::engines::fermi_2d::Surface,
        src: &crate::engines::fermi_2d::Surface,
        copy: &crate::engines::fermi_2d::Config,
    ) -> Option<BlitImages> {
        const FIND_OPTIONS: RelaxedOptions = RelaxedOptions::SAMPLES;

        let dst_addr = dst.address();
        let src_addr = src.address();
        let mut dst_info = ImageInfo::from_fermi2d_surface(dst);
        let mut src_info = ImageInfo::from_fermi2d_surface(src);
        let can_be_depth_blit = dst_info.format == src_info.format
            && copy.filter == crate::engines::fermi_2d::Filter::Point;
        let try_options = if can_be_depth_blit {
            FIND_OPTIONS | RelaxedOptions::FORMAT
        } else {
            FIND_OPTIONS
        };

        let mut src_id;
        let mut dst_id;
        loop {
            self.has_deleted_images = false;
            src_id = self.find_image_with_caps(
                &src_info,
                src_addr,
                try_options,
                self.has_broken_texture_view_formats,
                self.has_native_bgr,
            );
            dst_id = self.find_image_with_caps(
                &dst_info,
                dst_addr,
                try_options,
                self.has_broken_texture_view_formats,
                self.has_native_bgr,
            );
            if !copy.must_accelerate {
                if src_id.is_none() && dst_id.is_none() {
                    return None;
                }
                let src_gpu_modified = src_id.is_some_and(|id| {
                    self.slot_images[id]
                        .flags
                        .contains(ImageFlagBits::GPU_MODIFIED)
                });
                let dst_gpu_modified = dst_id.is_some_and(|id| {
                    self.slot_images[id]
                        .flags
                        .contains(ImageFlagBits::GPU_MODIFIED)
                });
                if !src_gpu_modified && !dst_gpu_modified {
                    return None;
                }
            }

            let src_image = src_id.map(|id| &*self.slot_images[id]);
            if src_image.is_some_and(|image| image.info.num_samples > 1) {
                let find_options = FIND_OPTIONS | RelaxedOptions::FORCE_BROKEN_VIEWS;
                src_id = Some(self.find_or_insert_image_with_caps(
                    &src_info,
                    src_addr,
                    find_options,
                    self.has_broken_texture_view_formats,
                    self.has_native_bgr,
                ));
                dst_id = Some(self.find_or_insert_image_with_caps(
                    &dst_info,
                    dst_addr,
                    find_options,
                    self.has_broken_texture_view_formats,
                    self.has_native_bgr,
                ));
                if self.has_deleted_images {
                    continue;
                }
                break;
            }

            if can_be_depth_blit {
                let src_image = src_id.map(|id| &*self.slot_images[id]);
                let dst_image = dst_id.map(|id| &*self.slot_images[id]);
                super::util::deduce_blit_images(&mut dst_info, &mut src_info, dst_image, src_image);
                if surface::get_format_type(dst_info.format)
                    != surface::get_format_type(src_info.format)
                {
                    continue;
                }
            }

            if src_id.is_none() {
                src_id = Some(self.insert_image(&src_info, src_addr));
            }
            if dst_id.is_none() {
                dst_id = Some(self.insert_image(&dst_info, dst_addr));
            }
            if !self.has_deleted_images {
                break;
            }
        }

        let mut src_id = src_id?;
        let mut dst_id = dst_id?;
        let src_image = &self.slot_images[src_id];
        let dst_image = &self.slot_images[dst_id];
        if surface::get_format_type(dst_info.format)
            != surface::get_format_type(dst_image.info.format)
            || surface::get_format_type(src_info.format)
                != surface::get_format_type(src_image.info.format)
            || !crate::compatible_formats::is_view_compatible(
                dst_info.format,
                dst_image.info.format,
                false,
                self.has_native_bgr,
            )
            || !crate::compatible_formats::is_view_compatible(
                src_info.format,
                src_image.info.format,
                false,
                self.has_native_bgr,
            )
        {
            loop {
                self.has_deleted_images = false;
                src_id = self.find_or_insert_image_with_caps(
                    &src_info,
                    src_addr,
                    RelaxedOptions::empty(),
                    self.has_broken_texture_view_formats,
                    self.has_native_bgr,
                );
                dst_id = self.find_or_insert_image_with_caps(
                    &dst_info,
                    dst_addr,
                    RelaxedOptions::empty(),
                    self.has_broken_texture_view_formats,
                    self.has_native_bgr,
                );
                if !self.has_deleted_images {
                    break;
                }
            }
        }

        Some(BlitImages {
            dst_id,
            src_id,
            dst_format: dst_info.format,
            src_format: src_info.format,
        })
    }

    /// Port of upstream `TextureCache<P>::BlitImage`.
    pub fn blit_image(
        &mut self,
        dst: &crate::engines::fermi_2d::Surface,
        src: &crate::engines::fermi_2d::Surface,
        copy: &crate::engines::fermi_2d::Config,
    ) -> bool {
        let Some(images) = self.get_blit_images(dst, src, copy) else {
            return false;
        };
        let dst_id = images.dst_id;
        let src_id = images.src_id;

        self.prepare_image(src_id, false, false);
        self.prepare_image(dst_id, true, false);

        let mut is_src_rescaled = self.slot_images[src_id]
            .flags
            .contains(ImageFlagBits::RESCALED);
        let mut is_dst_rescaled = self.slot_images[dst_id]
            .flags
            .contains(ImageFlagBits::RESCALED);
        let is_resolve = self.slot_images[src_id].info.num_samples != 1
            && self.slot_images[dst_id].info.num_samples == 1;
        if is_src_rescaled != is_dst_rescaled {
            if self.image_can_rescale(src_id) {
                self.scale_up(src_id);
                is_src_rescaled = self.slot_images[src_id]
                    .flags
                    .contains(ImageFlagBits::RESCALED);
                if is_resolve {
                    self.slot_images[dst_id].info.rescaleable = true;
                    let aliases = self.slot_images[dst_id].aliased_images.clone();
                    for alias in aliases {
                        self.slot_images[alias.id].info.rescaleable = true;
                    }
                }
            }
            if self.image_can_rescale(dst_id) {
                self.scale_up(dst_id);
                is_dst_rescaled = self.slot_images[dst_id]
                    .flags
                    .contains(ImageFlagBits::RESCALED);
            }
        }
        if is_resolve && is_src_rescaled != is_dst_rescaled {
            self.scale_down(src_id);
            self.scale_down(dst_id);
            is_src_rescaled = self.slot_images[src_id]
                .flags
                .contains(ImageFlagBits::RESCALED);
            is_dst_rescaled = self.slot_images[dst_id]
                .flags
                .contains(ImageFlagBits::RESCALED);
        }

        let resolution = common::settings::values().resolution_info.clone();
        let scale_region = |region: &mut Region2D| {
            region.start.x = resolution.scale_up_i32(region.start.x);
            region.start.y = resolution.scale_up_i32(region.start.y);
            region.end.x = resolution.scale_up_i32(region.end.x);
            region.end.y = resolution.scale_up_i32(region.end.y);
        };

        let src_base = self.slot_images[src_id]
            .try_find_base(src.address())
            .expect("TextureCache::BlitImage source must belong to its image");
        let src_view_info = ImageViewInfo::for_render_target(
            ImageViewType::E2D,
            images.src_format,
            SubresourceRange {
                base: src_base,
                extent: SubresourceExtent {
                    levels: 1,
                    layers: 1,
                },
            },
        );
        let Ok((src_framebuffer_id, src_view_id)) =
            self.render_target_from_image(src_id, src_view_info, src.address())
        else {
            return false;
        };
        let (src_samples_x, src_samples_y) =
            super::samples_helper::samples_log2(self.slot_images[src_id].info.num_samples as i32);
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

        let dst_base = self.slot_images[dst_id]
            .try_find_base(dst.address())
            .expect("TextureCache::BlitImage destination must belong to its image");
        let dst_view_info = ImageViewInfo::for_render_target(
            ImageViewType::E2D,
            images.dst_format,
            SubresourceRange {
                base: dst_base,
                extent: SubresourceExtent {
                    levels: 1,
                    layers: 1,
                },
            },
        );
        let Ok((dst_framebuffer_id, dst_view_id)) =
            self.render_target_from_image(dst_id, dst_view_info, dst.address())
        else {
            return false;
        };
        let (dst_samples_x, dst_samples_y) =
            super::samples_helper::samples_log2(self.slot_images[dst_id].info.num_samples as i32);
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

        P::blit_image(
            self,
            dst_framebuffer_id,
            src_framebuffer_id,
            dst_view_id,
            src_view_id,
            dst_region,
            src_region,
            copy.filter,
            copy.operation,
        );
        true
    }

    /// Port of upstream `TextureCache<P>::RenderTargetFromImage`.
    fn render_target_from_image(
        &mut self,
        image_id: ImageId,
        view_info: ImageViewInfo,
        gpu_addr: GPUVAddr,
    ) -> Result<(FramebufferId, ImageViewId), P::FramebufferError> {
        let view_id = self.find_or_emplace_image_view(image_id, view_info, gpu_addr);
        let image = &self.slot_images[image_id];
        let is_rescaled = image.flags.contains(ImageFlagBits::RESCALED);
        let is_color =
            surface::get_format_type(image.info.format) == surface::SurfaceType::ColorTexture;
        let color_view_id = if is_color {
            view_id
        } else {
            ImageViewId::default()
        };
        let depth_view_id = if is_color {
            ImageViewId::default()
        } else {
            view_id
        };
        let mut extent = super::util::mip_size(image.info.size, view_info.range.base.level as u32);
        if is_rescaled {
            let resolution = common::settings::values().resolution_info.clone();
            extent.width = resolution.scale_up_u32(extent.width);
            if image.info.image_type == ImageType::E2D {
                extent.height = resolution.scale_up_u32(extent.height);
            }
        }
        let (samples_x, samples_y) =
            super::samples_helper::samples_log2(image.info.num_samples as i32);
        let mut color_buffer_ids = [ImageViewId::default(); NUM_RT];
        color_buffer_ids[0] = color_view_id;
        let key = RenderTargets {
            color_buffer_ids,
            depth_buffer_id: depth_view_id,
            size: Extent2D {
                width: extent.width >> samples_x,
                height: extent.height >> samples_y,
            },
            is_rescaled,
            ..RenderTargets::default()
        };
        let framebuffer_id = self.get_framebuffer_id(&key)?;
        Ok((framebuffer_id, view_id))
    }

    // ── Maxwell DMA image/buffer copies ────────────────────────────────

    /// Port of `TextureCache<P>::DmaImageId`.
    pub fn dma_image_id(&mut self, operand: &dma::ImageOperand, is_upload: bool) -> ImageId {
        let dst_info = ImageInfo::from_dma_operand(operand);
        let dst_id = self.find_dma_image(&dst_info, operand.address);
        if !dst_id.is_valid() {
            return NULL_IMAGE_ID;
        }

        let image = &mut self.slot_images[dst_id];
        if !image.flags.contains(ImageFlagBits::GPU_MODIFIED) {
            return NULL_IMAGE_ID;
        }
        if image.info.image_type == ImageType::E3D {
            return NULL_IMAGE_ID;
        }
        if !is_upload && !image.info.dma_downloaded {
            image.info.dma_downloaded = true;
            return NULL_IMAGE_ID;
        }
        if image.try_find_base(operand.address).is_none() {
            return NULL_IMAGE_ID;
        }
        dst_id
    }

    /// Port of `TextureCache<P>::FindDMAImage`.
    pub fn find_dma_image(&mut self, info: &ImageInfo, gpu_addr: GPUVAddr) -> ImageId {
        let size = super::util::calculate_guest_size_in_bytes(info) as u64;
        let Some(cpu_addr) = self.translated_cpu_addr(gpu_addr, size) else {
            return NULL_IMAGE_ID;
        };

        let mut image_id = NULL_IMAGE_ID;
        let mut image_ids = SmallVec::<[ImageId; 8]>::new();
        self.for_each_image_in_region(
            cpu_addr,
            size as usize,
            |existing_image_id, existing_image| {
                if existing_image.flags.contains(ImageFlagBits::REMAPPED) {
                    return false;
                }

                let matched = if info.image_type == ImageType::Linear
                    || existing_image.info.image_type == ImageType::Linear
                {
                    let strict_size = existing_image.flags.contains(ImageFlagBits::STRONG);
                    let existing = &existing_image.info;
                    existing_image.gpu_addr == gpu_addr
                        && existing.image_type == info.image_type
                        && existing.pitch() == info.pitch()
                        && super::util::is_pitch_linear_same_size(existing, info, strict_size)
                        && crate::compatible_formats::is_view_compatible(
                            existing.format,
                            info.format,
                            false,
                            true,
                        )
                } else {
                    super::util::is_sub_copy(info, existing_image, gpu_addr)
                };

                if matched {
                    image_id = existing_image_id;
                    image_ids.push(existing_image_id);
                    return true;
                }
                false
            },
        );

        if image_ids.len() <= 1 {
            return image_id;
        }
        image_ids
            .into_iter()
            .max_by_key(|&id| self.slot_images[id].modification_tick)
            .unwrap_or(NULL_IMAGE_ID)
    }

    /// Backend-independent copy descriptor half of
    /// `TextureCache<P>::DmaBufferImageCopy`.
    ///
    /// Upstream also calls `PrepareDmaImage` here; the Rust base cache cannot
    /// invoke backend `PrepareImage`, so the OpenGL wrapper performs that
    /// preparation and then uses this exact descriptor construction.
    pub fn dma_buffer_image_copy_descriptor(
        &self,
        copy_info: &dma::ImageCopy,
        buffer_operand: &dma::BufferOperand,
        image_operand: &dma::ImageOperand,
        image_id: ImageId,
    ) -> Option<DmaBufferImageCopyResult> {
        if !image_id.is_valid() {
            return None;
        }
        let image = &self.slot_images[image_id];
        let base = image.try_find_base(image_operand.address)?;
        let buffer_size = buffer_operand.pitch.wrapping_mul(buffer_operand.height);
        let bpp = surface::bytes_per_block(image.info.format);
        let convert = |value: u32| (image_operand.bytes_per_pixel.wrapping_mul(value)) / bpp;
        let copy = BufferImageCopy {
            buffer_offset: 0,
            buffer_size: buffer_size as usize,
            buffer_row_length: convert(buffer_operand.pitch),
            buffer_image_height: buffer_operand.height,
            image_subresource: SubresourceLayers {
                base_level: base.level,
                base_layer: base.layer,
                num_layers: 1,
            },
            image_offset: Offset3D {
                x: convert(image_operand.params.origin.x()) as i32,
                y: image_operand.params.origin.y() as i32,
                z: 0,
            },
            image_extent: Extent3D {
                width: convert(copy_info.length_x),
                height: copy_info.length_y,
                depth: 1,
            },
        };
        Some(DmaBufferImageCopyResult { image_id, copy })
    }

    // ── Rescaling ──────────────────────────────────────────────────────

    /// Port of `TextureCache<P>::IsRescaling`.
    pub fn is_rescaling_active(&self) -> bool {
        self.is_rescaling
    }

    /// Port of `TextureCache<P>::IsRescaling(const ImageViewBase&)`.
    pub fn is_rescaling_image_view(&self, image_view_id: ImageViewId) -> bool {
        if !image_view_id.is_valid() || image_view_id == NULL_IMAGE_VIEW_ID {
            return false;
        }
        let image_view = &self.slot_image_views[image_view_id];
        if image_view.view_type == ImageViewType::Buffer {
            return false;
        }
        image_view.image_id.is_valid()
            && image_view.image_id != NULL_IMAGE_ID
            && self.slot_images[image_view.image_id]
                .flags
                .contains(ImageFlagBits::RESCALED)
    }

    // ── Prepare / refresh ──────────────────────────────────────────────

    /// Port of `TextureCache<P>::SynchronizeAliases`.
    fn synchronize_aliases(&mut self, image_id: ImageId) {
        let image_tick = self.slot_images[image_id].modification_tick;
        let image_flags = self.slot_images[image_id].flags;
        let mut aliases = SmallVec::<[(ImageId, Vec<ImageCopy>); 8]>::new();
        let mut most_recent_tick = image_tick;
        let mut any_rescaled = image_flags.contains(ImageFlagBits::RESCALED);
        let mut any_modified = image_flags.contains(ImageFlagBits::GPU_MODIFIED);

        for alias in &self.slot_images[image_id].aliased_images {
            let alias_image = &self.slot_images[alias.id];
            if image_tick < alias_image.modification_tick {
                most_recent_tick = most_recent_tick.max(alias_image.modification_tick);
                any_rescaled |= alias_image.flags.contains(ImageFlagBits::RESCALED);
                any_modified |= alias_image.flags.contains(ImageFlagBits::GPU_MODIFIED);
                aliases.push((alias.id, alias.copies.clone()));
            }
        }
        if aliases.is_empty() {
            return;
        }

        let can_rescale = self.image_can_rescale(image_id);
        if any_rescaled {
            if can_rescale {
                self.scale_up(image_id);
            } else {
                self.scale_down(image_id);
            }
        }
        let image = &mut self.slot_images[image_id];
        image.modification_tick = most_recent_tick;
        if any_modified {
            image.flags.insert(ImageFlagBits::GPU_MODIFIED);
        }

        aliases.sort_unstable_by_key(|(alias_id, _)| self.slot_images[*alias_id].modification_tick);
        let resolution_active = common::settings::values().resolution_info.active;
        for (alias_id, copies) in aliases {
            if resolution_active && any_rescaled {
                if can_rescale {
                    self.scale_up(alias_id);
                } else {
                    self.scale_down(alias_id);
                }
            }
            self.copy_image(image_id, alias_id, &copies);
        }
    }

    /// Port of `TextureCache<P>::PrepareImage`.
    pub fn prepare_image(&mut self, image_id: ImageId, is_modification: bool, invalidate: bool) {
        if invalidate {
            self.slot_images[image_id]
                .flags
                .remove(ImageFlagBits::CPU_MODIFIED | ImageFlagBits::GPU_MODIFIED);
            if !self.slot_images[image_id]
                .flags
                .contains(ImageFlagBits::TRACKED)
            {
                self.track_image(image_id);
            }
        } else {
            self.refresh_contents(image_id);
            if !self.slot_images[image_id].aliased_images.is_empty() {
                self.synchronize_aliases(image_id);
            }
        }
        if is_modification {
            self.mark_modification_by_id(image_id);
        }
        let lru_index = self.slot_images[image_id].lru_index;
        self.lru_cache.touch(lru_index, self.frame_tick);
    }

    /// Port of upstream `TextureCache<P>::CopyImage`.
    fn copy_image(&mut self, dst_id: ImageId, src_id: ImageId, copies: &[ImageCopy]) {
        let mut copies = copies.to_vec();
        let is_rescaled = self.slot_images[src_id]
            .flags
            .contains(ImageFlagBits::RESCALED);
        if is_rescaled {
            if !self.slot_images[dst_id]
                .flags
                .contains(ImageFlagBits::RESCALED)
            {
                log::error!(
                    "TextureCache::CopyImage source is rescaled but destination is not: src={} dst={}",
                    src_id.index,
                    dst_id.index,
                );
            }
            let both_2d = self.slot_images[src_id].info.image_type == ImageType::E2D
                && self.slot_images[dst_id].info.image_type == ImageType::E2D;
            let resolution = common::settings::values().resolution_info.clone();
            for copy in &mut copies {
                copy.src_offset.x = resolution.scale_up_i32(copy.src_offset.x);
                copy.dst_offset.x = resolution.scale_up_i32(copy.dst_offset.x);
                copy.extent.width = resolution.scale_up_u32(copy.extent.width);
                if both_2d {
                    copy.src_offset.y = resolution.scale_up_i32(copy.src_offset.y);
                    copy.dst_offset.y = resolution.scale_up_i32(copy.dst_offset.y);
                    copy.extent.height = resolution.scale_up_u32(copy.extent.height);
                }
            }
        }

        let dst_format_type = surface::get_format_type(self.slot_images[dst_id].info.format);
        let src_format_type = surface::get_format_type(self.slot_images[src_id].info.format);
        if src_format_type == dst_format_type {
            if P::HAS_EMULATED_COPIES && !P::can_image_be_copied(self, dst_id, src_id) {
                P::emulate_copy_image(self, dst_id, src_id, &copies);
                return;
            }
            P::copy_image(self, dst_id, src_id, &copies);
            return;
        }

        let dst_info = self.slot_images[dst_id].info.clone();
        let src_info = self.slot_images[src_id].info.clone();
        if dst_info.image_type != ImageType::E2D {
            log::error!(
                "TextureCache::CopyImage destination reinterpret type is not 2D: {:?}",
                dst_info.image_type,
            );
        }
        if src_info.image_type != ImageType::E2D {
            log::error!(
                "TextureCache::CopyImage source reinterpret type is not 2D: {:?}",
                src_info.image_type,
            );
        }
        if P::should_reinterpret(self, dst_id, src_id) {
            P::reinterpret_image(self, dst_id, src_id, &copies);
            return;
        }

        let dst_gpu_addr = self.slot_images[dst_id].gpu_addr;
        let src_gpu_addr = self.slot_images[src_id].gpu_addr;
        for copy in &copies {
            if copy.dst_subresource.num_layers != 1 {
                log::error!("TextureCache::CopyImage destination layer count is not one");
            }
            if copy.src_subresource.num_layers != 1 {
                log::error!("TextureCache::CopyImage source layer count is not one");
            }
            if copy.src_offset != Offset3D::default() {
                log::error!("TextureCache::CopyImage source offset is not zero");
            }
            if copy.dst_offset != Offset3D::default() {
                log::error!("TextureCache::CopyImage destination offset is not zero");
            }

            let dst_range = SubresourceRange {
                base: SubresourceBase {
                    level: copy.dst_subresource.base_level,
                    layer: copy.dst_subresource.base_layer,
                },
                extent: SubresourceExtent {
                    levels: 1,
                    layers: 1,
                },
            };
            let src_range = SubresourceRange {
                base: SubresourceBase {
                    level: copy.src_subresource.base_level,
                    layer: copy.src_subresource.base_layer,
                },
                extent: SubresourceExtent {
                    levels: 1,
                    layers: 1,
                },
            };
            let mut dst_format = dst_info.format;
            if src_format_type == surface::SurfaceType::DepthStencil
                && dst_format_type == surface::SurfaceType::ColorTexture
                && surface::bytes_per_block(dst_format) == 4
            {
                dst_format = PixelFormat::A8B8G8R8Unorm;
            }
            let dst_view_info =
                ImageViewInfo::for_render_target(ImageViewType::E2D, dst_format, dst_range);
            let src_view_info =
                ImageViewInfo::for_render_target(ImageViewType::E2D, src_info.format, src_range);
            let Ok((dst_framebuffer_id, dst_view_id)) =
                self.render_target_from_image(dst_id, dst_view_info, dst_gpu_addr)
            else {
                return;
            };
            let src_view_id = self.find_or_emplace_image_view(src_id, src_view_info, src_gpu_addr);
            let dst_view = &self.slot_image_views[dst_view_id];
            let src_view = &self.slot_image_views[src_view_id];
            let expected_size = Extent3D {
                width: dst_view.size.width.min(src_view.size.width),
                height: dst_view.size.height.min(src_view.size.height),
                depth: dst_view.size.depth.min(src_view.size.depth),
            };
            let scaled_extent = if is_rescaled {
                let resolution = common::settings::values().resolution_info.clone();
                Extent3D {
                    width: resolution.scale_up_u32(expected_size.width),
                    height: resolution.scale_up_u32(expected_size.height),
                    depth: expected_size.depth,
                }
            } else {
                expected_size
            };
            if copy.extent != scaled_extent {
                log::error!(
                    "TextureCache::CopyImage extent differs from the conversion views: copy={:?} expected={:?}",
                    copy.extent,
                    scaled_extent,
                );
            }
            P::convert_image(self, dst_framebuffer_id, dst_view_id, src_view_id);
        }
    }

    // ── Modification marks ─────────────────────────────────────────────

    /// Port of `TextureCache<P>::MarkModification(ImageId)`.
    ///
    /// Sets the `GpuModified` flag on the image and updates
    /// `modification_tick`.
    pub fn mark_modification_by_id(&mut self, id: ImageId) {
        self.modification_tick = self.modification_tick.wrapping_add(1);
        let image = &mut self.slot_images[id];
        image.flags.insert(ImageFlagBits::GPU_MODIFIED);
        image.modification_tick = self.modification_tick;
    }

    // ── GPU memory queries ─────────────────────────────────────────────

    /// Port of `TextureCache<P>::IsRegionGpuModified`.
    ///
    /// Returns true if any image overlapping the given CPU address range has
    /// been modified from the GPU and not yet downloaded to guest memory.
    pub fn is_region_gpu_modified(&self, addr: u64, size: usize) -> bool {
        let mut is_modified = false;
        Self::for_each_cpu_page(addr, size, |page| {
            if is_modified {
                return;
            }
            let Some(map_ids) = self.page_table.get(&page) else {
                return;
            };
            is_modified = map_ids.iter().any(|&map_id| {
                let map = &self.slot_map_views[map_id];
                map.overlaps(addr, size)
                    && self.slot_images[map.image_id]
                        .flags
                        .contains(ImageFlagBits::GPU_MODIFIED)
            });
        });
        is_modified
    }

    /// Port of `TextureCache<P>::UnmapGPUMemory`.
    pub fn unmap_gpu_memory(&mut self, as_id: usize, gpu_addr: u64, size: usize) {
        let Some(storage_id) = self.channel_caches.get_storage_id(as_id) else {
            return;
        };
        let table_index = storage_id * 2;
        let Some(table) = self.gpu_page_table_storage.get(table_index) else {
            return;
        };
        let mut image_ids = Vec::new();
        let slot_images = &mut self.slot_images;
        Self::for_each_gpu_page(gpu_addr, size, |page| {
            let Some(ids) = table.get(&page) else {
                return;
            };
            for &image_id in ids {
                let image = &mut slot_images[image_id];
                if image.flags.contains(ImageFlagBits::PICKED)
                    || !image.overlaps_gpu(gpu_addr, size)
                {
                    continue;
                }
                image.flags.insert(ImageFlagBits::PICKED);
                image_ids.push(image_id);
            }
        });
        for &image_id in &image_ids {
            slot_images[image_id].flags.remove(ImageFlagBits::PICKED);
        }

        for image_id in image_ids {
            if !self.slot_images[image_id]
                .flags
                .contains(ImageFlagBits::CPU_MODIFIED)
            {
                self.slot_images[image_id]
                    .flags
                    .insert(ImageFlagBits::CPU_MODIFIED);
                if self.slot_images[image_id]
                    .flags
                    .contains(ImageFlagBits::TRACKED)
                {
                    self.untrack_image(image_id);
                }
            }
            if self.slot_images[image_id]
                .flags
                .contains(ImageFlagBits::REMAPPED)
            {
                continue;
            }
            self.slot_images[image_id]
                .flags
                .insert(ImageFlagBits::REMAPPED);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::maxwell_3d::{RenderTargetInfo, RtControlInfo};
    use crate::framebuffer_config::FramebufferConfig;
    use crate::texture_cache::render_targets::RenderTargets;
    use crate::textures::texture::{ComponentType, TextureFormat, TextureType, TicEntry, TscEntry};
    use ruzu_core::hle::service::nvnflinger::pixel_format::PixelFormat as AndroidPixelFormat;

    fn collect_images_in_region_for_test(
        cache: &mut TextureCacheBase,
        cpu_addr: u64,
        size: usize,
    ) -> SmallVec<[ImageId; 32]> {
        let mut image_ids = SmallVec::new();
        cache.for_each_image_in_region(cpu_addr, size, |image_id, _| {
            image_ids.push(image_id);
            false
        });
        image_ids
    }

    fn color_2d_tic(address: u64, base_layer: u32) -> TicEntry {
        let word0 = (TextureFormat::A8B8G8R8 as u32)
            | ((ComponentType::Unorm as u32) << 7)
            | ((ComponentType::Unorm as u32) << 10)
            | ((ComponentType::Unorm as u32) << 13)
            | ((ComponentType::Unorm as u32) << 16);
        let word1 = address as u32;
        let word2 = (((address >> 32) as u32) & 0xFFFF) | (3 << 21);
        let word3 = 0;
        let word4 = 63 | ((base_layer & 0x7) << 16) | ((TextureType::Texture2D as u32) << 23);
        let word5 = 31 | (1 << 31);

        TicEntry {
            raw: [
                word0 as u64 | ((word1 as u64) << 32),
                word2 as u64 | ((word3 as u64) << 32),
                word4 as u64 | ((word5 as u64) << 32),
                0,
            ],
        }
    }

    fn descriptor_bytes(raw: [u64; 4]) -> Vec<u8> {
        raw.into_iter()
            .flat_map(u64::to_le_bytes)
            .collect::<Vec<_>>()
    }

    #[test]
    fn remove_framebuffers_invalidates_last_and_sentences_backend_object() {
        let mut cache = test_cache();
        let removed_view = ImageViewId { index: 7 };
        let kept_view = ImageViewId { index: 8 };
        let removed_framebuffer = cache.slot_framebuffers.insert(());
        let kept_framebuffer = cache.slot_framebuffers.insert(());
        let mut removed_key = RenderTargets::default();
        removed_key.color_buffer_ids[0] = removed_view;
        let mut kept_key = RenderTargets::default();
        kept_key.depth_buffer_id = kept_view;
        cache.framebuffers.insert(removed_key, removed_framebuffer);
        cache.framebuffers.insert(kept_key, kept_framebuffer);
        cache.last_framebuffer_id = removed_framebuffer;
        cache.last_framebuffer_serial = 19;

        cache.remove_framebuffers(&[removed_view]);

        assert!(!cache.framebuffers.contains_key(&removed_key));
        assert_eq!(cache.framebuffers.get(&kept_key), Some(&kept_framebuffer));
        assert_eq!(cache.last_framebuffer_id, FramebufferId::default());
        assert_eq!(cache.last_framebuffer_serial, 0);
        assert!(!cache.slot_framebuffers.contains(removed_framebuffer));
        assert!(cache.slot_framebuffers.contains(kept_framebuffer));
    }

    fn unbound_test_cache() -> TextureCacheBase {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use std::sync::Arc;
        TextureCacheBase::new(Arc::new(MaxwellDeviceMemoryManager::default()))
    }

    fn test_cache() -> TextureCacheBase {
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex;
        use std::sync::Arc;

        let mut cache = unbound_test_cache();
        cache.set_channel_gpu_memory(Arc::new(Mutex::new(MemoryManager::new(17))));
        cache
    }

    #[test]
    fn get_blit_images_matches_upstream_lookup_insertion_and_formats() {
        use crate::engines::fermi_2d::{Config, Filter, MemoryLayout, Operation, Surface};
        use crate::gpu::RenderTargetFormat;

        let surface = |gpu_addr: u64| Surface {
            format: RenderTargetFormat::A8B8G8R8Unorm as u32,
            linear: MemoryLayout::Pitch as u32,
            block_dimensions: 0,
            depth: 1,
            layer: 0,
            pitch: 16,
            width: 4,
            height: 2,
            addr_upper: (gpu_addr >> 32) as u32,
            addr_lower: gpu_addr as u32,
        };
        let src = surface(0x1_0000);
        let dst = surface(0x2_0000);
        let mut copy = Config {
            operation: Operation::SrcCopy,
            filter: Filter::Point,
            must_accelerate: false,
            dst_x0: 0,
            dst_y0: 0,
            dst_x1: 4,
            dst_y1: 2,
            src_x0: 0,
            src_y0: 0,
            src_x1: 4,
            src_y1: 2,
        };
        let mut cache = test_cache();
        let gpu_memory = cache
            .channel_gpu_memory
            .as_ref()
            .cloned()
            .expect("test cache binds channel GPU memory");
        gpu_memory
            .lock()
            .map(0x1_0000, 0x10_0000, 0x1_0000, 0, true);
        gpu_memory
            .lock()
            .map(0x2_0000, 0x20_0000, 0x1_0000, 0, true);
        cache.set_channel_gpu_memory(gpu_memory);
        assert!(cache.get_blit_images(&dst, &src, &copy).is_none());

        copy.must_accelerate = true;
        let images = cache
            .get_blit_images(&dst, &src, &copy)
            .expect("accelerated blit inserts both upstream image owners");
        assert!(images.src_id.is_valid());
        assert!(images.dst_id.is_valid());
        assert_ne!(images.src_id, images.dst_id);
        assert_eq!(images.src_format, PixelFormat::A8B8G8R8Unorm);
        assert_eq!(images.dst_format, PixelFormat::A8B8G8R8Unorm);
        assert_eq!(cache.slot_images[images.src_id].gpu_addr, 0x1_0000);
        assert_eq!(cache.slot_images[images.dst_id].gpu_addr, 0x2_0000);
    }

    #[test]
    fn get_blit_images_uses_distinct_virtual_invalid_addresses_for_unmapped_images() {
        use crate::engines::fermi_2d::{Config, Filter, MemoryLayout, Operation, Surface};
        use crate::gpu::RenderTargetFormat;

        let surface = |gpu_addr: u64| Surface {
            format: RenderTargetFormat::A8B8G8R8Unorm as u32,
            linear: MemoryLayout::Pitch as u32,
            block_dimensions: 0,
            depth: 1,
            layer: 0,
            pitch: 16,
            width: 4,
            height: 2,
            addr_upper: (gpu_addr >> 32) as u32,
            addr_lower: gpu_addr as u32,
        };
        let src = surface(0x3000);
        let dst = surface(0x4000);
        let copy = Config {
            operation: Operation::SrcCopy,
            filter: Filter::Point,
            must_accelerate: true,
            dst_x0: 0,
            dst_y0: 0,
            dst_x1: 4,
            dst_y1: 2,
            src_x0: 0,
            src_y0: 0,
            src_x1: 4,
            src_y1: 2,
        };

        let mut cache = test_cache();
        let images = cache
            .get_blit_images(&dst, &src, &copy)
            .expect("upstream inserts accelerated blit images without a GPU mapping");

        let src_image = &cache.slot_images[images.src_id];
        let dst_image = &cache.slot_images[images.dst_id];
        assert_eq!(src_image.gpu_addr, 0x3000);
        assert_eq!(dst_image.gpu_addr, 0x4000);
        assert!(src_image.cpu_addr >= !(1u64 << 40));
        assert!(dst_image.cpu_addr >= !(1u64 << 40));
        assert_ne!(src_image.cpu_addr, dst_image.cpu_addr);
    }

    struct TestImageViewBackend {
        base: TextureCacheBase<TestImageViewParams>,
    }

    struct TestImageViewParams;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct TestFramebuffer {
        key: RenderTargets,
        generation: u32,
    }

    thread_local! {
        static IMAGE_VIEW_PREPARE_PUBLICATION: std::cell::RefCell<Vec<bool>> = const {
            std::cell::RefCell::new(Vec::new())
        };
        static RENDER_TARGET_PREPARE_CALLS: std::cell::RefCell<Vec<(ImageViewId, bool, bool)>> = const {
            std::cell::RefCell::new(Vec::new())
        };
        static JOIN_COPY_DISPATCH: std::cell::Cell<(u32, u32)> = const {
            std::cell::Cell::new((0, 0))
        };
        static JOIN_COPY_COPIES: std::cell::RefCell<Vec<ImageCopy>> = const {
            std::cell::RefCell::new(Vec::new())
        };
        static FRAMEBUFFER_CONSTRUCTIONS: std::cell::Cell<u32> = const {
            std::cell::Cell::new(0)
        };
        static TEST_IMAGE_SCALE_SUCCEEDS: std::cell::Cell<bool> = const {
            std::cell::Cell::new(false)
        };
        static PREPARE_REFRESH_CALLS: std::cell::Cell<u32> = const {
            std::cell::Cell::new(0)
        };
        static UPLOAD_BARRIER_CALLS: std::cell::Cell<u32> = const {
            std::cell::Cell::new(0)
        };
        static BLIT_DISPATCH: std::cell::RefCell<Option<(
            FramebufferId,
            FramebufferId,
            ImageViewId,
            ImageViewId,
            Region2D,
            Region2D,
            crate::engines::fermi_2d::Filter,
            crate::engines::fermi_2d::Operation,
        )>> = const {
            std::cell::RefCell::new(None)
        };
    }

    impl TextureCacheParams for TestImageViewParams {
        type Runtime = ();
        type Image = ();
        type ImageAlloc = ();
        type ImageView = ();
        type Sampler = ();
        type Framebuffer = TestFramebuffer;
        type FramebufferError = std::convert::Infallible;
        type AsyncBuffer = Vec<u8>;
        type BufferType = ();

        const ENABLE_VALIDATION: bool = true;
        const FRAMEBUFFER_BLITS: bool = false;
        const HAS_EMULATED_COPIES: bool = false;
        const HAS_DEVICE_MEMORY_INFO: bool = false;
        const IMPLEMENTS_ASYNC_DOWNLOADS: bool = false;

        fn create_image(_: Option<&mut ()>, _: ImageId, _: std::ptr::NonNull<ImageBase>) {}

        fn set_image_allocation_tick(_: &mut (), _: u64) {}

        fn create_image_view(
            _: Option<&mut ()>,
            _: ImageViewId,
            _: &ImageViewInfo,
            _: std::ptr::NonNull<ImageViewBase>,
            _: Option<&()>,
        ) {
        }

        fn create_sampler(_: Option<&mut ()>, _: &crate::textures::texture::TscEntry) {}

        fn create_framebuffer(
            _: Option<&mut ()>,
            _: [Option<std::ptr::NonNull<()>>; NUM_RT],
            _: Option<std::ptr::NonNull<()>>,
            key: &RenderTargets,
        ) -> Result<TestFramebuffer, std::convert::Infallible> {
            let generation = FRAMEBUFFER_CONSTRUCTIONS.with(|constructions| {
                let generation = constructions.get().wrapping_add(1);
                constructions.set(generation);
                generation
            });
            Ok(TestFramebuffer {
                key: *key,
                generation,
            })
        }

        fn prepare_image_view(
            cache: &mut TextureCacheBase<Self>,
            image_view_id: ImageViewId,
            is_modification: bool,
            invalidate: bool,
        ) {
            let already_published = cache
                .current_channel_state()
                .image_view_ids
                .values()
                .any(|&cached_id| cached_id == image_view_id);
            IMAGE_VIEW_PREPARE_PUBLICATION
                .with(|observations| observations.borrow_mut().push(already_published));
            RENDER_TARGET_PREPARE_CALLS.with(|calls| {
                calls
                    .borrow_mut()
                    .push((image_view_id, is_modification, invalidate));
            });
        }

        fn scale_up_image(cache: &mut TextureCacheBase<Self>, image_id: ImageId, _: bool) -> bool {
            if !TEST_IMAGE_SCALE_SUCCEEDS.with(std::cell::Cell::get) {
                return false;
            }
            if cache.slot_images[image_id]
                .flags
                .contains(ImageFlagBits::RESCALED)
            {
                return false;
            }
            cache.slot_images[image_id]
                .flags
                .insert(ImageFlagBits::RESCALED);
            cache.slot_images[image_id].has_scaled = true;
            true
        }

        fn scale_down_image(
            cache: &mut TextureCacheBase<Self>,
            image_id: ImageId,
            _: bool,
        ) -> bool {
            if !TEST_IMAGE_SCALE_SUCCEEDS.with(std::cell::Cell::get) {
                return false;
            }
            if !cache.slot_images[image_id]
                .flags
                .contains(ImageFlagBits::RESCALED)
            {
                return false;
            }
            cache.slot_images[image_id]
                .flags
                .remove(ImageFlagBits::RESCALED);
            true
        }

        fn upload_staging_buffer(_: &mut TextureCacheBase<Self>, size: usize, _: bool) -> Vec<u8> {
            PREPARE_REFRESH_CALLS.with(|calls| calls.set(calls.get().wrapping_add(1)));
            vec![0; size]
        }

        fn staging_mapped_span(buffer: &mut Vec<u8>) -> &mut [u8] {
            buffer
        }

        fn free_deferred_staging_buffer(_: &mut TextureCacheBase<Self>, _: &mut Vec<u8>) {}

        fn can_upload_msaa(_: &TextureCacheBase<Self>) -> bool {
            true
        }

        fn transition_image_layout(_: &mut TextureCacheBase<Self>, _: ImageId) {}

        fn upload_image(
            _: &mut TextureCacheBase<Self>,
            _: ImageId,
            _: &Vec<u8>,
            _: &[BufferImageCopy],
        ) {
        }

        fn accelerate_image_upload(
            _: &mut TextureCacheBase<Self>,
            _: ImageId,
            _: &Vec<u8>,
            _: &[SwizzleParameters],
            _: u32,
            _: u32,
        ) {
        }

        fn insert_upload_memory_barrier(_: &mut TextureCacheBase<Self>) {
            UPLOAD_BARRIER_CALLS.with(|calls| calls.set(calls.get().wrapping_add(1)));
        }

        fn copy_image(
            _: &mut TextureCacheBase<Self>,
            _: ImageId,
            _: ImageId,
            copies: &[ImageCopy],
        ) {
            JOIN_COPY_DISPATCH.with(|dispatch| {
                let (copy, msaa) = dispatch.get();
                dispatch.set((copy + 1, msaa));
            });
            JOIN_COPY_COPIES.with(|observed| observed.borrow_mut().extend_from_slice(copies));
        }

        fn copy_image_msaa(
            _: &mut TextureCacheBase<Self>,
            _: ImageId,
            _: ImageId,
            _: &[ImageCopy],
        ) {
            JOIN_COPY_DISPATCH.with(|dispatch| {
                let (copy, msaa) = dispatch.get();
                dispatch.set((copy, msaa + 1));
            });
        }

        fn blit_image(
            _: &mut TextureCacheBase<Self>,
            dst_framebuffer_id: FramebufferId,
            src_framebuffer_id: FramebufferId,
            dst_view_id: ImageViewId,
            src_view_id: ImageViewId,
            dst_region: Region2D,
            src_region: Region2D,
            filter: crate::engines::fermi_2d::Filter,
            operation: crate::engines::fermi_2d::Operation,
        ) {
            BLIT_DISPATCH.with(|dispatch| {
                *dispatch.borrow_mut() = Some((
                    dst_framebuffer_id,
                    src_framebuffer_id,
                    dst_view_id,
                    src_view_id,
                    dst_region,
                    src_region,
                    filter,
                    operation,
                ));
            });
        }
    }

    impl TestImageViewBackend {
        fn new(base: TextureCacheBase<TestImageViewParams>) -> Self {
            IMAGE_VIEW_PREPARE_PUBLICATION.with(|observations| observations.borrow_mut().clear());
            RENDER_TARGET_PREPARE_CALLS.with(|calls| calls.borrow_mut().clear());
            TEST_IMAGE_SCALE_SUCCEEDS.with(|enabled| enabled.set(false));
            Self { base }
        }

        fn fill_compute_image_views(&mut self, views: &mut [ImageViewInOut]) {
            self.base.fill_image_views(views, true, true);
        }
    }

    #[test]
    fn get_framebuffer_reuses_complete_key_and_tracks_render_target_serial() {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use std::sync::Arc;

        FRAMEBUFFER_CONSTRUCTIONS.with(|constructions| constructions.set(0));
        let mut cache = TextureCacheBase::<TestImageViewParams>::new_for_backend(Arc::new(
            MaxwellDeviceMemoryManager::default(),
        ));
        let image_info = test_color_info(1280, 720);
        let image_id = cache.slot_images.insert(ImageSlot {
            backend: Some(()),
            base: Box::new(ImageBase::new(image_info.clone(), 0x5000_0000, 0x9000_0000)),
        });
        let view_info = ImageViewInfo {
            view_type: ImageViewType::E2D,
            format: image_info.format,
            range: SubresourceRange {
                base: SubresourceBase { level: 0, layer: 0 },
                extent: SubresourceExtent {
                    levels: 1,
                    layers: 1,
                },
            },
            ..ImageViewInfo::default()
        };
        let view_id = cache.slot_image_views.insert(ImageViewSlot {
            backend: Some(()),
            info: view_info,
            base: Box::new(ImageViewBase::new(
                &view_info,
                &image_info,
                image_id,
                0x5000_0000,
            )),
        });
        cache.render_targets = RenderTargets {
            color_buffer_ids: std::array::from_fn(|index| {
                if index == 0 {
                    view_id
                } else {
                    NULL_IMAGE_VIEW_ID
                }
            }),
            draw_buffers: [0, 1, 2, 3, 4, 5, 6, 7],
            size: Extent2D {
                width: 1280,
                height: 720,
            },
            ..RenderTargets::default()
        };
        cache.render_targets_serial = 1;

        let first = *cache.get_framebuffer().unwrap();
        let first_id = cache.last_framebuffer_id;
        assert_eq!(first.generation, 1);
        assert_eq!(first.key, cache.render_targets);

        let cached = *cache.get_framebuffer().unwrap();
        assert_eq!(cached, first);
        assert_eq!(cache.last_framebuffer_id, first_id);
        assert_eq!(FRAMEBUFFER_CONSTRUCTIONS.with(std::cell::Cell::get), 1);

        cache.render_targets_serial = 2;
        let same_key = *cache.get_framebuffer().unwrap();
        assert_eq!(same_key, first);
        assert_eq!(cache.last_framebuffer_id, first_id);
        assert_eq!(cache.last_framebuffer_serial, 2);
        assert_eq!(FRAMEBUFFER_CONSTRUCTIONS.with(std::cell::Cell::get), 1);

        cache.render_targets.draw_buffers[0] = 3;
        cache.render_targets_serial = 3;
        let changed = *cache.get_framebuffer().unwrap();
        assert_eq!(changed.generation, 2);
        assert_ne!(cache.last_framebuffer_id, first_id);
        assert_eq!(FRAMEBUFFER_CONSTRUCTIONS.with(std::cell::Cell::get), 2);
    }

    fn test_color_info(width: u32, height: u32) -> ImageInfo {
        ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width,
                height,
                depth: 1,
            },
            ..ImageInfo::default()
        }
    }

    #[test]
    fn update_render_targets_clean_path_prepares_every_slot_with_upstream_arguments() {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use std::sync::Arc;

        RENDER_TARGET_PREPARE_CALLS.with(|calls| calls.borrow_mut().clear());
        let mut cache = TextureCacheBase::<TestImageViewParams>::new_for_backend(Arc::new(
            MaxwellDeviceMemoryManager::default(),
        ));
        let image_info = test_color_info(64, 32);
        let image_id = cache.slot_images.insert(ImageSlot {
            backend: Some(()),
            base: Box::new(ImageBase::new(image_info.clone(), 0x1000, 0x2000)),
        });
        let view_info = ImageViewInfo {
            view_type: ImageViewType::E2D,
            format: image_info.format,
            range: SubresourceRange {
                base: SubresourceBase { level: 0, layer: 0 },
                extent: SubresourceExtent {
                    levels: 1,
                    layers: 1,
                },
            },
            ..ImageViewInfo::default()
        };
        let view_id = cache.slot_image_views.insert(ImageViewSlot {
            backend: Some(()),
            info: view_info,
            base: Box::new(ImageViewBase::new(
                &view_info,
                &image_info,
                image_id,
                0x1000,
            )),
        });
        cache.render_targets.color_buffer_ids[0] = view_id;

        let dirty_flags = [false; 256];
        let mut dirty_access = dirty_flags;
        cache.update_render_targets_with_snapshot(
            &Maxwell3DRenderTargets::default(),
            &mut dirty_access,
            |_, _| None,
            true,
            Some((1, 0, 64, 32)),
        );

        RENDER_TARGET_PREPARE_CALLS.with(|calls| {
            let calls = calls.borrow();
            assert_eq!(calls.len(), NUM_RT + 1);
            assert_eq!(calls[0], (view_id, true, false));
            assert!(calls[1..]
                .iter()
                .all(|&(_, is_modification, invalidate)| is_modification && invalidate));
        });
        assert_eq!(dirty_access, dirty_flags);
    }

    #[test]
    fn update_render_targets_force_lookup_consumes_flags_and_sets_depth_bias_dirty() {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use std::sync::Arc;

        RENDER_TARGET_PREPARE_CALLS.with(|calls| calls.borrow_mut().clear());
        let mut cache = TextureCacheBase::<TestImageViewParams>::new_for_backend(Arc::new(
            MaxwellDeviceMemoryManager::default(),
        ));
        let mut dirty_flags = [false; 256];
        dirty_flags[crate::dirty_flags::flags::RENDER_TARGETS as usize] = true;
        dirty_flags[crate::dirty_flags::flags::RENDER_TARGET_CONTROL as usize] = true;
        let mut dirty_access = dirty_flags;

        cache.update_render_targets_with_snapshot(
            &Maxwell3DRenderTargets::default(),
            &mut dirty_access,
            |_, _| None,
            false,
            None,
        );

        assert!(!dirty_access[crate::dirty_flags::flags::RENDER_TARGETS as usize]);
        assert!(!dirty_access[crate::dirty_flags::flags::RENDER_TARGET_CONTROL as usize]);
        for index in 0..NUM_RT {
            assert!(
                !dirty_access[(crate::dirty_flags::flags::COLOR_BUFFER0 + index as u8) as usize]
            );
        }
        assert!(!dirty_access[crate::dirty_flags::flags::ZETA_BUFFER as usize]);
        assert!(dirty_access[crate::dirty_flags::flags::DEPTH_BIAS_GLOBAL as usize]);
        assert!(!dirty_access[crate::dirty_flags::flags::RESCALE_VIEWPORTS as usize]);
        assert!(!dirty_access[crate::dirty_flags::flags::RESCALE_SCISSORS as usize]);
        RENDER_TARGET_PREPARE_CALLS.with(|calls| {
            assert_eq!(calls.borrow().len(), NUM_RT + 1);
        });
    }

    fn dma_operand(
        address: u64,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
    ) -> dma::ImageOperand {
        dma::ImageOperand {
            bytes_per_pixel,
            params: dma::Parameters {
                width,
                height,
                depth: 1,
                ..dma::Parameters::default()
            },
            address,
        }
    }

    fn map_dma_operand(cache: &TextureCacheBase, gpu_addr: u64, cpu_addr: u64) {
        cache
            .channel_gpu_memory
            .as_ref()
            .expect("test cache binds channel GPU memory")
            .lock()
            .map(gpu_addr, cpu_addr, 0x1_0000, 0, true);
    }

    #[test]
    fn dma_image_id_requires_gpu_modified_image() {
        let mut cache = test_cache();
        let operand = dma_operand(0x5000_0000, 64, 64, 4);
        map_dma_operand(&cache, operand.address, 0x1000_0000);
        let info = ImageInfo::from_dma_operand(&operand);
        let image_id = cache.insert_image(&info, operand.address);

        assert_eq!(cache.dma_image_id(&operand, true), NULL_IMAGE_ID);

        cache.mark_modification_by_id(image_id);

        assert_eq!(cache.dma_image_id(&operand, true), image_id);
    }

    #[test]
    fn dma_image_id_download_first_marks_dma_downloaded_and_returns_null() {
        let mut cache = test_cache();
        let operand = dma_operand(0x5100_0000, 64, 64, 4);
        map_dma_operand(&cache, operand.address, 0x1100_0000);
        let info = ImageInfo::from_dma_operand(&operand);
        let image_id = cache.insert_image(&info, operand.address);
        cache.mark_modification_by_id(image_id);

        assert!(!cache.slot_images[image_id].info.dma_downloaded);
        assert_eq!(cache.dma_image_id(&operand, false), NULL_IMAGE_ID);
        assert!(cache.slot_images[image_id].info.dma_downloaded);
        assert_eq!(cache.dma_image_id(&operand, false), image_id);
    }

    #[test]
    fn dma_image_id_refuses_3d_images() {
        let mut cache = test_cache();
        let mut operand = dma_operand(0x5200_0000, 32, 32, 4);
        operand.params.block_size.raw = 1 << 8;
        operand.params.depth = 4;
        map_dma_operand(&cache, operand.address, 0x1200_0000);
        let info = ImageInfo::from_dma_operand(&operand);
        let image_id = cache.insert_image(&info, operand.address);
        cache.mark_modification_by_id(image_id);

        assert_eq!(cache.dma_image_id(&operand, true), NULL_IMAGE_ID);
    }

    #[test]
    fn dma_buffer_image_copy_descriptor_matches_upstream_fields() {
        let mut cache = test_cache();
        let info = test_color_info(64, 64);
        let image_id = cache.slot_images.insert(
            crate::texture_cache::image_base::ImageBase::new(info, 0x5300_0000, 0x9300_0000).into(),
        );
        let image_operand = dma::ImageOperand {
            bytes_per_pixel: 8,
            params: dma::Parameters {
                origin: dma::Origin { raw: 3 | (7 << 16) },
                ..dma::Parameters::default()
            },
            address: 0x5300_0000,
        };
        let buffer_operand = dma::BufferOperand {
            pitch: 64,
            height: 9,
            address: 0x6000_0000,
            ..dma::BufferOperand::default()
        };
        let copy_info = dma::ImageCopy {
            length_x: 5,
            length_y: 6,
        };

        let result = cache
            .dma_buffer_image_copy_descriptor(&copy_info, &buffer_operand, &image_operand, image_id)
            .expect("valid descriptor");

        assert_eq!(result.image_id, image_id);
        assert_eq!(result.copy.buffer_offset, 0);
        assert_eq!(result.copy.buffer_size, 64 * 9);
        assert_eq!(result.copy.buffer_row_length, 128);
        assert_eq!(result.copy.buffer_image_height, 9);
        assert_eq!(result.copy.image_subresource.base_level, 0);
        assert_eq!(result.copy.image_subresource.base_layer, 0);
        assert_eq!(result.copy.image_subresource.num_layers, 1);
        assert_eq!(result.copy.image_offset, Offset3D { x: 6, y: 7, z: 0 });
        assert_eq!(
            result.copy.image_extent,
            Extent3D {
                width: 10,
                height: 6,
                depth: 1,
            }
        );
    }

    #[test]
    fn texture_cache_reserves_zero_for_null_resources() {
        let mut cache = test_cache();
        let mut descriptor = crate::textures::texture::TscEntry::default();
        descriptor.raw[0] = 0x0000_03A2_0002_6080;

        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let image_id = cache
            .slot_images
            .insert(crate::texture_cache::image_base::ImageBase::new(info, 0x1000, 0x2000).into());

        let sampler_id = cache.find_sampler(&descriptor, false);

        assert_ne!(image_id, crate::texture_cache::types::NULL_IMAGE_ID);
        assert_eq!(image_id.index, 1);
        assert_ne!(sampler_id, crate::texture_cache::types::NULL_SAMPLER_ID);
        assert_eq!(sampler_id.index, 1);
    }

    #[test]
    fn sampler_budget_reclaims_only_inactive_sampler_slots_once_per_frame() {
        use crate::control::channel_state::ChannelState;

        let mut cache = test_cache();
        let channel = ChannelState::new(10);
        cache.create_channel(&channel);
        cache.bind_to_channel(10);
        let mut first = crate::textures::texture::TscEntry::default();
        first.raw[0] = 1;
        let mut second = crate::textures::texture::TscEntry::default();
        second.raw[0] = 2;
        let first_id = cache.find_sampler(&first, false);
        let second_id = cache.find_sampler(&second, false);
        cache
            .current_channel_state_mut()
            .sampler_ids
            .insert(0, first_id);
        cache.set_sampler_heap_budget(Some(cache.slot_samplers.size()));

        cache.enforce_sampler_budget();

        assert!(cache.slot_samplers.contains(first_id));
        assert!(!cache.slot_samplers.contains(second_id));
        assert!(cache.current_channel_state().samplers.contains_key(&first));
        assert!(!cache.current_channel_state().samplers.contains_key(&second));

        cache.enforce_sampler_budget();
        assert!(!cache.slot_samplers.contains(second_id));
    }

    #[test]
    fn fill_compute_image_views_uses_channel_gpu_memory_for_tic_reads() {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let mut cache =
            TestImageViewBackend::new(TextureCacheBase::<TestImageViewParams>::new_for_backend(
                Arc::new(MaxwellDeviceMemoryManager::default()),
            ));
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut backing = vec![0u8; 0x6000];
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x9000_0000,
            backing.as_mut_ptr(),
            0x5000_0000,
            backing.len(),
            3,
            true,
        );
        let gpu_memory = Arc::new(ParkingMutex::new(
            MemoryManager::new_with_geometry_and_device_memory(
                7,
                Arc::clone(&device_memory),
                32,
                0x1_0000_0000,
                16,
                12,
            ),
        ));
        {
            let mut gpu_memory = gpu_memory.lock();
            gpu_memory.map(0x1000, 0x9000_0000, 0x1000, 0, false);
            gpu_memory.map(0x8000, 0x9000_1000, 0x4000, 0, false);
            let tic = color_2d_tic(0x8000, 0);
            assert!(gpu_memory.write_block(0x1000, &descriptor_bytes(tic.raw)));
        }
        cache.base.set_channel_gpu_memory(Arc::clone(&gpu_memory));
        assert!(cache
            .base
            .channel_state
            .compute_image_table
            .synchronize(0x1000, 0));

        let mut views = [ImageViewInOut {
            index: 0,
            blacklist: false,
            id: NULL_IMAGE_VIEW_ID,
        }];
        cache.fill_compute_image_views(&mut views);

        assert!(views[0].id.is_valid());
        assert_ne!(views[0].id, NULL_IMAGE_VIEW_ID);
        let view = &cache.base.slot_image_views[views[0].id];
        assert_eq!(view.gpu_addr, 0x8000);
    }

    #[test]
    fn visit_image_view_prepares_before_publication_and_on_cached_reads() {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let mut cache =
            TestImageViewBackend::new(TextureCacheBase::<TestImageViewParams>::new_for_backend(
                Arc::new(MaxwellDeviceMemoryManager::default()),
            ));
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut backing = vec![0u8; 0x6000];
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x9000_0000,
            backing.as_mut_ptr(),
            0x5000_0000,
            backing.len(),
            3,
            true,
        );
        let gpu_memory = Arc::new(ParkingMutex::new(
            MemoryManager::new_with_geometry_and_device_memory(
                7,
                Arc::clone(&device_memory),
                32,
                0x1_0000_0000,
                16,
                12,
            ),
        ));
        {
            let mut gpu_memory = gpu_memory.lock();
            gpu_memory.map(0x1000, 0x9000_0000, 0x1000, 0, false);
            gpu_memory.map(0x8000, 0x9000_1000, 0x4000, 0, false);
            let tic = color_2d_tic(0x8000, 0);
            assert!(gpu_memory.write_block(0x1000, &descriptor_bytes(tic.raw)));
        }
        cache.base.set_channel_gpu_memory(Arc::clone(&gpu_memory));
        assert!(cache
            .base
            .channel_state
            .compute_image_table
            .synchronize(0x1000, 0));
        let mut views = [ImageViewInOut {
            index: 0,
            blacklist: false,
            id: NULL_IMAGE_VIEW_ID,
        }];
        cache.fill_compute_image_views(&mut views);

        assert!(views[0].id.is_valid());
        assert_ne!(views[0].id, NULL_IMAGE_VIEW_ID);
        assert_ne!(views[0].id, CORRUPT_ID);
        let first_id = views[0].id;
        assert_eq!(
            cache
                .base
                .channel_state
                .image_view_ids
                .get(&(common::slot_vector::SlotId::TAGGED_VALUE)),
            Some(&first_id)
        );
        IMAGE_VIEW_PREPARE_PUBLICATION.with(|observations| {
            assert_eq!(*observations.borrow(), vec![false]);
        });
        cache.fill_compute_image_views(&mut views);
        assert_eq!(views[0].id, first_id);
        assert_eq!(cache.base.slot_image_views[views[0].id].gpu_addr, 0x8000);
        IMAGE_VIEW_PREPARE_PUBLICATION.with(|observations| {
            assert_eq!(*observations.borrow(), vec![false, true]);
        });
    }

    #[test]
    fn fill_image_views_constructs_typed_payload_before_publishing_view() {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let mut cache =
            TestImageViewBackend::new(TextureCacheBase::<TestImageViewParams>::new_for_backend(
                Arc::new(MaxwellDeviceMemoryManager::default()),
            ));

        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut backing = vec![0u8; 0x6000];
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x9000_0000,
            backing.as_mut_ptr(),
            0x5000_0000,
            backing.len(),
            3,
            true,
        );
        let gpu_memory = Arc::new(ParkingMutex::new(
            MemoryManager::new_with_geometry_and_device_memory(
                7,
                Arc::clone(&device_memory),
                32,
                0x1_0000_0000,
                16,
                12,
            ),
        ));
        {
            let mut gpu_memory = gpu_memory.lock();
            gpu_memory.map(0x1000, 0x9000_0000, 0x1000, 0, false);
            gpu_memory.map(0x8000, 0x9000_1000, 0x4000, 0, false);
            let tic = color_2d_tic(0x8000, 0);
            assert!(gpu_memory.write_block(0x1000, &descriptor_bytes(tic.raw)));
        }
        cache.base.set_channel_gpu_memory(Arc::clone(&gpu_memory));
        assert!(cache
            .base
            .channel_state
            .compute_image_table
            .synchronize(0x1000, 0));

        let mut views = [ImageViewInOut {
            index: 0,
            blacklist: false,
            id: NULL_IMAGE_VIEW_ID,
        }];
        cache.fill_compute_image_views(&mut views);

        let image_id = cache.base.slot_image_views[views[0].id].image_id;
        assert!(cache.base.slot_images[image_id].backend.is_some());
        assert!(cache.base.slot_image_views[views[0].id].backend.is_some());
        assert!(cache.base.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::REGISTERED));
    }

    #[test]
    fn get_sampler_id_accepts_existing_channel_memory_borrow_for_compute_tsc_reads() {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let mut cache = test_cache();
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut backing = vec![0u8; 0x1000];
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x9000_0000,
            backing.as_mut_ptr(),
            0x5000_0000,
            backing.len(),
            3,
            true,
        );
        let gpu_memory = Arc::new(ParkingMutex::new(
            MemoryManager::new_with_geometry_and_device_memory(
                7,
                Arc::clone(&device_memory),
                32,
                0x1_0000_0000,
                16,
                12,
            ),
        ));
        let mut tsc = TscEntry::default();
        tsc.raw[0] = 0x0000_03A2_0002_6080;
        {
            let mut gpu_memory = gpu_memory.lock();
            gpu_memory.map(0x2000, 0x9000_0000, 0x1000, 0, false);
            assert!(gpu_memory.write_block(0x2000, &descriptor_bytes(tsc.raw)));
        }
        cache.set_channel_gpu_memory(Arc::clone(&gpu_memory));
        assert!(cache
            .channel_state
            .compute_sampler_table
            .synchronize(0x2000, 0));

        let sampler_id = {
            let gpu_memory = gpu_memory.lock();
            cache.get_sampler_id_with_memory(0, true, &gpu_memory)
        };

        assert!(sampler_id.is_valid());
        assert_ne!(sampler_id, NULL_SAMPLER_ID);
        assert_eq!(cache.slot_samplers[sampler_id].raw, tsc.raw);
        assert!(cache.slot_samplers[sampler_id].backend.is_some());
    }

    #[test]
    fn synchronize_compute_descriptors_updates_tables_without_eager_id_entries() {
        let mut cache = test_cache();

        cache.synchronize_compute_descriptors(
            crate::texture_cache::texture_cache_base::ComputeDescriptorSyncRegs {
                linked_tsc: false,
                tic_addr: 0x3000,
                tic_limit: 4,
                tsc_addr: 0x2000,
                tsc_limit: 2,
            },
        );

        assert_eq!(cache.channel_state.compute_image_table.current_limit, 4);
        assert_eq!(cache.channel_state.compute_sampler_table.current_limit, 2);
        assert!(cache.channel_state.image_view_ids.is_empty());
        assert!(cache.channel_state.sampler_ids.is_empty());
    }

    #[test]
    fn synchronize_compute_descriptors_uses_tic_limit_when_tsc_is_linked() {
        let mut cache = test_cache();

        cache.synchronize_compute_descriptors(
            crate::texture_cache::texture_cache_base::ComputeDescriptorSyncRegs {
                linked_tsc: true,
                tic_addr: 0x7000,
                tic_limit: 6,
                tsc_addr: 0x6000,
                tsc_limit: 1,
            },
        );

        assert_eq!(cache.channel_state.compute_image_table.current_limit, 6);
        assert_eq!(cache.channel_state.compute_sampler_table.current_limit, 6);
        assert!(cache.channel_state.image_view_ids.is_empty());
        assert!(cache.channel_state.sampler_ids.is_empty());
    }

    #[test]
    fn synchronize_graphics_descriptors_updates_bound_channel_state() {
        use crate::control::channel_state::ChannelState;
        use crate::texture_cache::texture_cache_base::DescriptorSyncRegs;

        let mut cache = test_cache();
        let channel = ChannelState::new(10);
        cache.create_channel(&channel);
        cache.bind_to_channel(10);

        cache.synchronize_graphics_descriptors(DescriptorSyncRegs {
            sampler_binding_via_header: false,
            tex_header_addr: 0x5000,
            tex_header_limit: 808,
            tex_sampler_addr: 0x3000,
            tex_sampler_limit: 64,
        });

        let bound = cache
            .channel_caches
            .channel_state_by_bind_id(10)
            .expect("bound texture-cache channel exists");
        assert_eq!(bound.graphics_image_table.current_limit, 808);
        assert_eq!(bound.graphics_sampler_table.current_limit, 64);
        assert!(bound.image_view_ids.is_empty());
        assert!(bound.sampler_ids.is_empty());
        assert_eq!(cache.channel_state.graphics_image_table.current_limit, 0);
        assert_eq!(cache.channel_state.graphics_sampler_table.current_limit, 0);
    }

    #[test]
    fn synchronize_compute_descriptors_updates_bound_channel_state() {
        use crate::control::channel_state::ChannelState;
        use crate::texture_cache::texture_cache_base::ComputeDescriptorSyncRegs;

        let mut cache = test_cache();
        let channel = ChannelState::new(10);
        cache.create_channel(&channel);
        cache.bind_to_channel(10);

        cache.synchronize_compute_descriptors(ComputeDescriptorSyncRegs {
            linked_tsc: true,
            tic_addr: 0x7000,
            tic_limit: 12,
            tsc_addr: 0x6000,
            tsc_limit: 1,
        });

        let bound = cache
            .channel_caches
            .channel_state_by_bind_id(10)
            .expect("bound texture-cache channel exists");
        assert_eq!(bound.compute_image_table.current_limit, 12);
        assert_eq!(bound.compute_sampler_table.current_limit, 12);
        assert!(bound.image_view_ids.is_empty());
        assert!(bound.sampler_ids.is_empty());
        assert_eq!(cache.channel_state.compute_image_table.current_limit, 0);
        assert_eq!(cache.channel_state.compute_sampler_table.current_limit, 0);
    }

    #[test]
    fn update_render_targets_from_snapshot_registers_presentable_view() {
        let mut cache = test_cache();
        let mut render_targets = Maxwell3DRenderTargets::default();
        render_targets.surface_clip.width = 1280;
        render_targets.surface_clip.height = 720;
        render_targets.rt_control = RtControlInfo {
            count: 1,
            map: [0, 0, 0, 0, 0, 0, 0, 0],
        };
        render_targets.render_targets[0] = RenderTargetInfo {
            address: 0x4000_0000,
            width: 64,
            height: 32,
            format: 0xD5,
            tile_mode: 2 | (1 << 4),
            array_pitch: 32 * 4,
            depth: 1,
            base_layer: 0,
        };
        render_targets.zeta = crate::engines::maxwell_3d::ZetaInfo {
            enabled: true,
            address: 0x5000_0000,
            width: 64,
            height: 32,
            format: 0xA,
            tile_mode: 2 | (1 << 4),
            array_pitch: 32,
            depth: 1,
        };

        cache.update_render_targets_from_snapshot(&render_targets, |gpu_addr, _guest_size| {
            match gpu_addr {
                0x4000_0000 => Some(0x535B_5000),
                0x5000_0000 => Some(0x535C_0000),
                _ => None,
            }
        });

        let config = FramebufferConfig {
            address: 0x535B_5000,
            width: 64,
            height: 32,
            stride: 64,
            ..Default::default()
        };
        let view = cache.try_find_framebuffer_image_view(&config, 0x535B_5000);
        assert!(view.is_some());
        assert!(cache.render_targets.color_buffer_ids[0].is_valid());
        assert!(cache.render_targets.depth_buffer_id.is_valid());
        assert_eq!(cache.render_targets.draw_buffers[0], 0);
        assert_eq!(cache.render_targets.size.width, 1280);
        assert_eq!(cache.render_targets.size.height, 720);
        assert_eq!(cache.slot_images.size(), 3);
        assert_eq!(cache.slot_image_views.size(), 4);
    }

    #[test]
    fn update_render_targets_from_snapshot_passes_guest_size_for_range_translation() {
        let mut cache = test_cache();
        let mut render_targets = Maxwell3DRenderTargets::default();
        render_targets.rt_control = RtControlInfo {
            count: 1,
            map: [0, 0, 0, 0, 0, 0, 0, 0],
        };
        render_targets.render_targets[0] = RenderTargetInfo {
            address: 0x4000_0000,
            width: 64,
            height: 32,
            format: 0xD5,
            tile_mode: 1 << 12,
            array_pitch: 32 * 4,
            depth: 1,
            base_layer: 0,
        };
        let info = ImageInfo::from_render_target_info(
            &render_targets.render_targets[0],
            render_targets.anti_alias_samples_mode,
        );
        let expected_guest_size =
            crate::texture_cache::util::calculate_guest_size_in_bytes(&info) as u64;

        cache.update_render_targets_from_snapshot(&render_targets, |gpu_addr, guest_size| {
            if gpu_addr == 0x4000_0000 && guest_size == expected_guest_size {
                Some(0x535B_5000)
            } else {
                None
            }
        });

        assert!(cache.render_targets.color_buffer_ids[0].is_valid());
    }

    #[test]
    fn update_render_targets_from_snapshot_forwards_raw_msaa_mode_to_image_info() {
        let mut cache = test_cache();
        let mut render_targets = Maxwell3DRenderTargets::default();
        render_targets.anti_alias_samples_mode = 3;
        render_targets.rt_control = RtControlInfo {
            count: 1,
            map: [0, 0, 0, 0, 0, 0, 0, 0],
        };
        render_targets.render_targets[0] = RenderTargetInfo {
            address: 0x4000_0000,
            width: 64,
            height: 32,
            format: 0xD5,
            tile_mode: 2 | (1 << 4),
            array_pitch: 32 * 4,
            depth: 1,
            base_layer: 0,
        };
        render_targets.zeta = crate::engines::maxwell_3d::ZetaInfo {
            enabled: true,
            address: 0x5000_0000,
            width: 64,
            height: 32,
            format: 0xA,
            tile_mode: 2 | (1 << 4),
            array_pitch: 32,
            depth: 1,
        };

        cache.update_render_targets_from_snapshot(&render_targets, |gpu_addr, _guest_size| {
            match gpu_addr {
                0x4000_0000 => Some(0x535B_5000),
                0x5000_0000 => Some(0x535C_0000),
                _ => None,
            }
        });

        let color_id = cache.render_targets.color_buffer_ids[0];
        let zeta_id = cache.render_targets.depth_buffer_id;

        assert_eq!(cache.slot_images[color_id].info.num_samples, 8);
        assert_eq!(cache.slot_images[zeta_id].info.num_samples, 8);
    }

    fn rescaled_render_target_size(surface_width: u32, surface_height: u32) -> Extent2D {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use std::sync::Arc;

        TEST_IMAGE_SCALE_SUCCEEDS.with(|enabled| enabled.set(true));
        let mut cache = TextureCacheBase::<TestImageViewParams>::new_for_backend(Arc::new(
            MaxwellDeviceMemoryManager::default(),
        ));
        cache.set_channel_gpu_memory(Arc::new(ParkingMutex::new(MemoryManager::new(17))));
        let mut render_targets = Maxwell3DRenderTargets::default();
        render_targets.surface_clip.width = surface_width;
        render_targets.surface_clip.height = surface_height;
        render_targets.rt_control = RtControlInfo {
            count: 1,
            map: [0; NUM_RT],
        };
        render_targets.render_targets[0] = RenderTargetInfo {
            address: 0x4000_0000,
            width: 64,
            height: 512,
            format: 0xD5,
            tile_mode: 2 | (1 << 4),
            array_pitch: 32 * 4,
            depth: 1,
            base_layer: 0,
        };

        cache.update_render_targets_from_snapshot(&render_targets, |_, _| Some(0x535B_5000));
        let image_id = cache.slot_image_views[cache.render_targets.color_buffer_ids[0]].image_id;
        assert!(cache.slot_images[image_id].info.rescaleable);
        assert_eq!(cache.slot_images[image_id].scale_rating, 1);
        cache.frame_tick = cache.frame_tick.wrapping_add(1);
        cache.update_render_targets_from_snapshot(&render_targets, |_, _| Some(0x535B_5000));
        assert!(cache.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::RESCALED));
        let rebound_image_id =
            cache.slot_image_views[cache.render_targets.color_buffer_ids[0]].image_id;
        assert_eq!(rebound_image_id, image_id);
        assert!(cache.slot_images[rebound_image_id]
            .flags
            .contains(ImageFlagBits::RESCALED));
        TEST_IMAGE_SCALE_SUCCEEDS.with(|enabled| enabled.set(false));
        assert!(cache.render_targets.is_rescaled);
        cache.render_targets.size
    }

    #[test]
    fn update_render_targets_from_snapshot_scales_render_target_size_when_rescaling() {
        use common::settings;

        let _settings_guard = crate::test_support::RESOLUTION_SETTINGS_MUTEX
            .lock()
            .unwrap();
        let previous_resolution = settings::values().resolution_info.clone();
        {
            let mut values = settings::values_mut();
            values.resolution_info.up_scale = 3;
            values.resolution_info.down_shift = 1;
            values.resolution_info.active = true;
        }

        let size = rescaled_render_target_size(1280, 720);

        settings::values_mut().resolution_info = previous_resolution;

        assert_eq!(size.width, 1920);
        assert_eq!(size.height, 1080);
    }

    #[test]
    fn update_render_targets_size_uses_wrapping_scale_multiply_like_upstream() {
        use common::settings;

        let _settings_guard = crate::test_support::RESOLUTION_SETTINGS_MUTEX
            .lock()
            .unwrap();
        let previous_resolution = settings::values().resolution_info.clone();
        {
            let mut values = settings::values_mut();
            values.resolution_info.up_scale = 3;
            values.resolution_info.down_shift = 1;
            values.resolution_info.active = true;
        }

        let size = rescaled_render_target_size(u32::MAX, u32::MAX - 1);

        settings::values_mut().resolution_info = previous_resolution;

        assert_eq!(size.width, u32::MAX.wrapping_mul(3) >> 1);
        assert_eq!(size.height, (u32::MAX - 1).wrapping_mul(3) >> 1);
    }

    #[test]
    fn update_render_targets_from_snapshot_with_clean_render_targets_preserves_state_like_upstream()
    {
        let mut cache = test_cache();
        cache.render_targets.draw_buffers = [7, 6, 5, 4, 3, 2, 1, 0];
        cache.render_targets.size = Extent2D {
            width: 320,
            height: 180,
        };
        cache.render_targets.is_rescaled = true;
        let initial_image_count = cache.slot_images.size();
        let initial_image_view_count = cache.slot_image_views.size();

        let mut render_targets = Maxwell3DRenderTargets::default();
        render_targets.surface_clip.width = 1280;
        render_targets.surface_clip.height = 720;
        render_targets.rt_control = RtControlInfo {
            count: 1,
            map: [0, 1, 2, 3, 4, 5, 6, 7],
        };
        render_targets.render_targets[0] = RenderTargetInfo {
            address: 0x4000_0000,
            width: 64,
            height: 32,
            format: 0xD5,
            tile_mode: 1 << 12,
            array_pitch: 32 * 4,
            depth: 1,
            base_layer: 0,
        };
        let mut dirty_flags = [false; 256];
        dirty_flags[crate::dirty_flags::flags::RENDER_TARGET_CONTROL as usize] = true;
        dirty_flags[crate::dirty_flags::flags::COLOR_BUFFER0 as usize] = true;

        cache.update_render_targets_from_snapshot_with_dirty_flags(
            &render_targets,
            &dirty_flags,
            |_, _| panic!("clean RenderTargets must not re-resolve views"),
        );

        assert_eq!(cache.render_targets.draw_buffers, [7, 6, 5, 4, 3, 2, 1, 0]);
        assert_eq!(cache.render_targets.size.width, 320);
        assert_eq!(cache.render_targets.size.height, 180);
        assert!(cache.render_targets.is_rescaled);
        assert_eq!(cache.slot_images.size(), initial_image_count);
        assert_eq!(cache.slot_image_views.size(), initial_image_view_count);
    }

    #[test]
    fn update_render_targets_from_snapshot_uses_virtual_invalid_fallback_for_color_translation_miss(
    ) {
        let mut cache = test_cache();
        let mut render_targets = Maxwell3DRenderTargets::default();
        render_targets.surface_clip.width = 1280;
        render_targets.surface_clip.height = 720;
        render_targets.rt_control = RtControlInfo {
            count: 1,
            map: [0, 0, 0, 0, 0, 0, 0, 0],
        };
        render_targets.render_targets[0] = RenderTargetInfo {
            address: 0x4000_0000,
            width: 64,
            height: 32,
            format: 0xD5,
            tile_mode: 1 << 12,
            array_pitch: 32 * 4,
            depth: 1,
            base_layer: 0,
        };

        cache.update_render_targets_from_snapshot(&render_targets, |_, _| None);

        let view_id = cache.render_targets.color_buffer_ids[0];
        assert!(view_id.is_valid());
        let image_id = cache.slot_image_views[view_id].image_id;
        assert_eq!(cache.slot_images[image_id].gpu_addr, 0x4000_0000);
        assert!(cache.slot_images[image_id].cpu_addr >= !(1u64 << 40));
    }

    #[test]
    fn update_render_targets_from_snapshot_uses_virtual_invalid_fallback_for_zeta_translation_miss()
    {
        let mut cache = test_cache();
        let mut render_targets = Maxwell3DRenderTargets::default();
        render_targets.surface_clip.width = 1280;
        render_targets.surface_clip.height = 720;
        render_targets.rt_control = RtControlInfo {
            count: 1,
            map: [0, 0, 0, 0, 0, 0, 0, 0],
        };
        render_targets.render_targets[0] = RenderTargetInfo {
            address: 0x4000_0000,
            width: 64,
            height: 32,
            format: 0xD5,
            tile_mode: 1 << 12,
            array_pitch: 32 * 4,
            depth: 1,
            base_layer: 0,
        };
        render_targets.zeta = crate::engines::maxwell_3d::ZetaInfo {
            enabled: true,
            address: 0x5000_0000,
            width: 64,
            height: 32,
            format: 0xA,
            tile_mode: 1 << 12,
            array_pitch: 32,
            depth: 1,
        };

        cache.update_render_targets_from_snapshot(&render_targets, |gpu_addr, _guest_size| {
            match gpu_addr {
                0x4000_0000 => Some(0x535B_5000),
                0x5000_0000 => Some(0x535C_0000),
                _ => None,
            }
        });

        let original_depth = cache.render_targets.depth_buffer_id;
        assert!(original_depth.is_valid());

        render_targets.zeta.address = 0x5100_0000;
        let mut dirty_flags = [false; 256];
        dirty_flags[crate::dirty_flags::flags::RENDER_TARGETS as usize] = true;
        dirty_flags[crate::dirty_flags::flags::ZETA_BUFFER as usize] = true;
        cache.update_render_targets_from_snapshot_with_dirty_flags(
            &render_targets,
            &dirty_flags,
            |_, _| None,
        );

        assert!(cache.render_targets.depth_buffer_id.is_valid());
        assert_ne!(cache.render_targets.depth_buffer_id, original_depth);
        let image_id = cache.slot_image_views[cache.render_targets.depth_buffer_id].image_id;
        assert_eq!(cache.slot_images[image_id].gpu_addr, 0x5100_0000);
        assert!(cache.slot_images[image_id].cpu_addr >= !(1u64 << 40));
    }

    #[test]
    fn update_render_targets_from_snapshot_ignores_slots_past_rt_control_count() {
        let mut cache = test_cache();
        let mut render_targets = Maxwell3DRenderTargets::default();
        render_targets.surface_clip.width = 1280;
        render_targets.surface_clip.height = 720;
        render_targets.rt_control = RtControlInfo {
            count: 1,
            map: [0, 1, 0, 0, 0, 0, 0, 0],
        };
        render_targets.render_targets[0] = RenderTargetInfo {
            address: 0x4000_0000,
            width: 64,
            height: 32,
            format: 0xD5,
            tile_mode: 1 << 12,
            array_pitch: 32 * 4,
            depth: 1,
            base_layer: 0,
        };
        render_targets.render_targets[1] = RenderTargetInfo {
            address: 0x4100_0000,
            width: 64,
            height: 32,
            format: 0xD5,
            tile_mode: 1 << 12,
            array_pitch: 32 * 4,
            depth: 1,
            base_layer: 0,
        };

        cache.update_render_targets_from_snapshot(&render_targets, |gpu_addr, _guest_size| {
            match gpu_addr {
                0x4000_0000 => Some(0x535B_5000),
                0x4100_0000 => Some(0x535C_5000),
                _ => None,
            }
        });

        assert!(cache.render_targets.color_buffer_ids[0].is_valid());
        assert_eq!(
            cache.render_targets.color_buffer_ids[1],
            ImageViewId::default()
        );
        assert_eq!(cache.slot_images.size(), 2);
        assert_eq!(cache.slot_image_views.size(), 2);
    }

    #[test]
    fn update_render_targets_from_snapshot_dirty_flags_preserve_clean_color_slot() {
        let mut cache = test_cache();
        let mut render_targets = Maxwell3DRenderTargets::default();
        render_targets.surface_clip.width = 1280;
        render_targets.surface_clip.height = 720;
        render_targets.rt_control = RtControlInfo {
            count: 1,
            map: [0, 0, 0, 0, 0, 0, 0, 0],
        };
        render_targets.render_targets[0] = RenderTargetInfo {
            address: 0x4000_0000,
            width: 64,
            height: 32,
            format: 0xD5,
            tile_mode: 1 << 12,
            array_pitch: 32 * 4,
            depth: 1,
            base_layer: 0,
        };

        cache.update_render_targets_from_snapshot(&render_targets, |gpu_addr, _guest_size| {
            (gpu_addr == 0x4000_0000).then_some(0x535B_5000)
        });
        let original_view = cache.render_targets.color_buffer_ids[0];
        let original_images = cache.slot_images.size();

        render_targets.render_targets[0].address = 0x4100_0000;
        let mut dirty_flags = [false; 256];
        dirty_flags[crate::dirty_flags::flags::RENDER_TARGETS as usize] = true;
        cache.update_render_targets_from_snapshot_with_dirty_flags(
            &render_targets,
            &dirty_flags,
            |gpu_addr, _guest_size| (gpu_addr == 0x4100_0000).then_some(0x535C_5000),
        );

        assert_eq!(cache.render_targets.color_buffer_ids[0], original_view);
        assert_eq!(cache.slot_images.size(), original_images);

        dirty_flags[crate::dirty_flags::flags::RENDER_TARGET_CONTROL as usize] = true;
        cache.update_render_targets_from_snapshot_with_dirty_flags(
            &render_targets,
            &dirty_flags,
            |gpu_addr, _guest_size| (gpu_addr == 0x4100_0000).then_some(0x535C_5000),
        );

        assert_ne!(cache.render_targets.color_buffer_ids[0], original_view);
        assert!(cache.slot_images.size() > original_images);
    }

    #[test]
    fn bind_same_preemptive_render_target_view_has_no_download() {
        let mut cache = test_cache();
        let mut render_targets = Maxwell3DRenderTargets::default();
        render_targets.rt_control = RtControlInfo {
            count: 1,
            map: [0, 0, 0, 0, 0, 0, 0, 0],
        };
        render_targets.render_targets[0] = RenderTargetInfo {
            address: 0x4000_0000,
            width: 64,
            height: 32,
            format: 0xD5,
            tile_mode: 1 << 12,
            array_pitch: 32 * 4,
            depth: 1,
            base_layer: 0,
        };

        cache.update_render_targets_from_snapshot(&render_targets, |gpu_addr, _guest_size| {
            (gpu_addr == 0x4000_0000).then_some(0x535B_5000)
        });
        let view_id = cache.render_targets.color_buffer_ids[0];
        cache.slot_image_views[view_id]
            .flags
            .insert(ImageViewFlagBits::PREEMTIVE_DOWNLOAD);
        cache.uncommitted_downloads.clear();

        cache.bind_color_render_target(0, view_id);

        assert_eq!(cache.render_targets.color_buffer_ids[0], view_id);
        assert!(cache.uncommitted_downloads.is_empty());
    }

    #[test]
    fn try_find_framebuffer_image_view_emplaces_display_specific_view() {
        use crate::texture_cache::image_view_info::SwizzleSource;

        let mut cache = test_cache();
        let mut render_targets = Maxwell3DRenderTargets::default();
        render_targets.rt_control = RtControlInfo {
            count: 1,
            map: [0, 0, 0, 0, 0, 0, 0, 0],
        };
        render_targets.render_targets[0] = RenderTargetInfo {
            address: 0x4000_0000,
            width: 64,
            height: 32,
            format: 0xD5,
            tile_mode: 1 << 12,
            array_pitch: 32 * 4,
            depth: 1,
            base_layer: 0,
        };

        cache.update_render_targets_from_snapshot(&render_targets, |gpu_addr, _guest_size| {
            (gpu_addr == 0x4000_0000).then_some(0x535B_5000)
        });
        let initial_view_count = cache.slot_image_views.size();

        let config = FramebufferConfig {
            address: 0x535B_5000,
            width: 64,
            height: 32,
            stride: 64,
            pixel_format: AndroidPixelFormat::Bgra8888,
            ..Default::default()
        };
        let view = cache
            .try_find_framebuffer_image_view(&config, 0x535B_5000)
            .expect("framebuffer view");

        assert_eq!(cache.slot_image_views.size(), initial_view_count + 1);
        assert_eq!(
            cache.slot_image_views[view.view_id].format,
            surface::PixelFormat::B8G8R8A8Unorm
        );
        let image = &cache.slot_images[view.view.image_id];
        assert_eq!(
            image
                .image_view_infos
                .last()
                .expect("new view info")
                .w_source,
            SwizzleSource::OneFloat as u8
        );
    }

    #[test]
    fn try_find_framebuffer_image_view_unknown_format_uses_upstream_default() {
        let mut cache = test_cache();
        let mut render_targets = Maxwell3DRenderTargets::default();
        render_targets.rt_control = RtControlInfo {
            count: 1,
            map: [0, 0, 0, 0, 0, 0, 0, 0],
        };
        render_targets.render_targets[0] = RenderTargetInfo {
            address: 0x4000_0000,
            width: 64,
            height: 32,
            format: 0xD5,
            tile_mode: 1 << 12,
            array_pitch: 32 * 4,
            depth: 1,
            base_layer: 0,
        };

        cache.update_render_targets_from_snapshot(&render_targets, |gpu_addr, _guest_size| {
            (gpu_addr == 0x4000_0000).then_some(0x535B_5000)
        });

        let config = FramebufferConfig {
            address: 0x535B_5000,
            width: 64,
            height: 32,
            stride: 64,
            pixel_format: AndroidPixelFormat::NoFormat,
            ..Default::default()
        };
        let view = cache
            .try_find_framebuffer_image_view(&config, 0x535B_5000)
            .expect("framebuffer view");

        assert_eq!(
            cache.slot_image_views[view.view_id].format,
            surface::PixelFormat::A8B8G8R8Unorm
        );
    }

    #[test]
    fn try_find_framebuffer_image_view_keeps_first_candidate_on_equal_tick() {
        let mut cache = test_cache();
        let info = test_color_info(64, 32);
        let first_id = cache
            .slot_images
            .insert(ImageBase::new(info.clone(), 0x4000_0000, 0x535B_5000).into());
        let second_id = cache
            .slot_images
            .insert(ImageBase::new(info, 0x5000_0000, 0x535B_5000).into());
        for image_id in [first_id, second_id] {
            cache.register_image(image_id);
            cache.slot_images[image_id]
                .image_view_ids
                .push(ImageViewId { index: 99 });
            cache.slot_images[image_id].modification_tick = 7;
        }

        let view = cache
            .try_find_framebuffer_image_view(&FramebufferConfig::default(), 0x535B_5000)
            .expect("framebuffer view");

        assert_eq!(view.view.image_id, first_id);
    }

    #[test]
    fn get_flush_area_marks_gpu_modified_image_views_preemptive() {
        use crate::texture_cache::image_view_base::ImageViewFlagBits;

        let mut cache = test_cache();
        let mut render_targets = Maxwell3DRenderTargets::default();
        render_targets.rt_control = RtControlInfo {
            count: 1,
            map: [0, 0, 0, 0, 0, 0, 0, 0],
        };
        render_targets.render_targets[0] = RenderTargetInfo {
            address: 0x4000_0000,
            width: 64,
            height: 32,
            format: 0xD5,
            tile_mode: 1 << 12,
            array_pitch: 32 * 4,
            depth: 1,
            base_layer: 0,
        };

        cache.update_render_targets_from_snapshot(&render_targets, |gpu_addr, _guest_size| {
            (gpu_addr == 0x4000_0000).then_some(0x535B_5000)
        });
        let view_id = cache.render_targets.color_buffer_ids[0];
        let image_id = cache.slot_image_views[view_id].image_id;
        cache.slot_images[image_id].info.forced_flushed = false;
        cache.slot_image_views[view_id]
            .flags
            .remove(ImageViewFlagBits::PREEMTIVE_DOWNLOAD);
        cache.mark_modification_by_id(image_id);
        assert!(!cache.slot_images[image_id].info.forced_flushed);

        let area = cache
            .get_flush_area(0x535B_5008, 0x10)
            .expect("GPU-modified image should return a flush area");
        assert_eq!(area.start_address, cache.slot_images[image_id].cpu_addr);
        assert_eq!(area.end_address, cache.slot_images[image_id].cpu_addr_end);
        assert!(!area.preemtive);
        assert!(cache.slot_images[image_id].info.forced_flushed);
        assert!(cache.slot_image_views[view_id]
            .flags
            .contains(ImageViewFlagBits::PREEMTIVE_DOWNLOAD));

        let second = cache
            .get_flush_area(0x535B_5010, 0x10)
            .expect("forced-flushed image should still return a flush area");
        assert!(second.preemtive);

        cache.slot_images[image_id]
            .flags
            .remove(ImageFlagBits::GPU_MODIFIED);
        assert!(cache.get_flush_area(0x535B_5008, 0x10).is_none());
    }

    #[test]
    fn is_region_gpu_modified_checks_registered_overlapping_images() {
        let mut cache = test_cache();
        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let image_id = cache.insert_image(&info, 0x4000);
        let cpu_addr = cache.slot_images[image_id].cpu_addr;

        assert!(!cache.is_region_gpu_modified(cpu_addr, 0x10));

        cache.mark_modification_by_id(image_id);

        assert!(cache.is_region_gpu_modified(cpu_addr + 4, 0x10));
        assert!(
            !cache.is_region_gpu_modified(cache.slot_images[image_id].cpu_addr_end + 0x1000, 0x10)
        );
    }

    #[test]
    fn mark_modification_preserves_cpu_modified_like_upstream() {
        let mut cache = test_cache();
        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let image_id = cache.insert_image(&info, 0x4000);
        cache.slot_images[image_id]
            .flags
            .insert(ImageFlagBits::CPU_MODIFIED);

        cache.mark_modification_by_id(image_id);

        let image = &cache.slot_images[image_id];
        assert!(image.flags.contains(ImageFlagBits::CPU_MODIFIED));
        assert!(image.flags.contains(ImageFlagBits::GPU_MODIFIED));
        assert_eq!(image.modification_tick, cache.modification_tick);
    }

    #[test]
    fn write_memory_marks_registered_image_cpu_modified_and_untracks() {
        let mut cache = test_cache();
        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let image_id = cache.insert_image(&info, 0x4000);
        let cpu_addr = cache.slot_images[image_id].cpu_addr;
        assert!(cache.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::TRACKED));

        cache.write_memory(cpu_addr, 4);

        let image = &cache.slot_images[image_id];
        assert!(image.flags.contains(ImageFlagBits::CPU_MODIFIED));
        assert!(!image.flags.contains(ImageFlagBits::TRACKED));
    }

    #[test]
    fn download_memory_selects_only_upstream_safe_images_and_clears_gpu_modified() {
        use std::sync::{Arc, Mutex};

        let mut cache = test_cache();
        let info = test_color_info(1, 1);
        let safe_id = cache
            .slot_images
            .insert(ImageBase::new(info.clone(), 0x8000, 0x8000).into());
        let cpu_modified_id = cache
            .slot_images
            .insert(ImageBase::new(info, 0x9000, 0x9000).into());
        cache.register_image(safe_id);
        cache.register_image(cpu_modified_id);
        cache.slot_images[safe_id]
            .flags
            .remove(ImageFlagBits::CPU_MODIFIED);
        cache.slot_images[safe_id]
            .flags
            .insert(ImageFlagBits::GPU_MODIFIED);
        cache.slot_images[cpu_modified_id]
            .flags
            .insert(ImageFlagBits::GPU_MODIFIED | ImageFlagBits::CPU_MODIFIED);

        let downloaded = Arc::new(Mutex::new(Vec::new()));
        let downloaded_for_callback = Arc::clone(&downloaded);
        cache.set_image_downloader(Arc::new(move |image_id, _, staging| {
            downloaded_for_callback.lock().unwrap().push(image_id);
            staging.fill(0);
            true
        }));
        cache.set_guest_memory_writer(Arc::new(|_, _| {}));

        cache.download_memory(0x8000, 0x2000);

        assert_eq!(*downloaded.lock().unwrap(), vec![safe_id]);
        assert!(!cache.slot_images[safe_id]
            .flags
            .contains(ImageFlagBits::GPU_MODIFIED));
        assert!(cache.slot_images[cpu_modified_id]
            .flags
            .contains(ImageFlagBits::GPU_MODIFIED));
    }

    #[test]
    fn insert_image_uses_virtual_invalid_cpu_space_when_untranslated() {
        let mut cache = test_cache();
        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            ..ImageInfo::default()
        };

        let first_id = cache.insert_image(&info, 0x4000);
        let second_id = cache.insert_image(&info, 0x8000);

        let first_cpu_addr = cache.slot_images[first_id].cpu_addr;
        let first_gpu_addr = cache.slot_images[first_id].gpu_addr;
        let first_guest_size_bytes = cache.slot_images[first_id].guest_size_bytes;
        let second_cpu_addr = cache.slot_images[second_id].cpu_addr;
        assert_eq!(first_cpu_addr, !(1u64 << 40));
        assert_ne!(first_cpu_addr, first_gpu_addr);
        assert_eq!(
            second_cpu_addr,
            !(1u64 << 40) + common::alignment::align_up(first_guest_size_bytes as u64, 32)
        );
        assert!(collect_images_in_region_for_test(&mut cache, 0x4000, 4).is_empty());
        assert_eq!(
            collect_images_in_region_for_test(&mut cache, first_cpu_addr, 4).as_slice(),
            &[first_id]
        );
    }

    #[test]
    fn set_channel_gpu_memory_rebases_virtual_invalid_images() {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let gpu_memory = Arc::new(ParkingMutex::new(
            MemoryManager::new_with_geometry_and_device_memory(
                7,
                Arc::clone(&device_memory),
                32,
                0x1_0000_0000,
                16,
                12,
            ),
        ));
        let mut cache = TextureCacheBase::new(Arc::clone(&device_memory));
        cache.set_channel_gpu_memory(Arc::clone(&gpu_memory));
        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            ..ImageInfo::default()
        };

        let image_id = cache.insert_image(&info, 0x4000);
        assert_eq!(cache.slot_images[image_id].cpu_addr, !(1u64 << 40));

        gpu_memory.lock().map(0x4000, 0x9000_0000, 0x1000, 0, false);

        cache.set_channel_gpu_memory(gpu_memory);

        assert_eq!(cache.slot_images[image_id].cpu_addr, 0x9000_0000);
        assert_eq!(
            collect_images_in_region_for_test(&mut cache, 0x9000_0000, 4).as_slice(),
            &[image_id]
        );
        assert!(collect_images_in_region_for_test(&mut cache, !(1u64 << 40), 4).is_empty());
    }

    #[test]
    fn find_image_uses_translated_cpu_region_and_rejects_unmapped_gpu_addr() {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut cache = TextureCacheBase::new(Arc::clone(&device_memory));
        let gpu_memory = Arc::new(ParkingMutex::new(
            MemoryManager::new_with_geometry_and_device_memory(
                7,
                Arc::clone(&device_memory),
                32,
                0x1_0000_0000,
                16,
                12,
            ),
        ));
        gpu_memory.lock().map(0x4000, 0x9000_0000, 0x1000, 0, false);
        cache.set_channel_gpu_memory(gpu_memory);

        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let image_id = cache.insert_image(&info, 0x4000);

        assert_eq!(cache.find_image(&info, 0x4000), Some(image_id));
        assert_eq!(cache.find_image(&info, 0x8000), None);
    }

    #[test]
    fn find_image_force_broken_views_disables_compatible_format_reuse() {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut cache = TextureCacheBase::new(Arc::clone(&device_memory));
        let gpu_memory = Arc::new(ParkingMutex::new(
            MemoryManager::new_with_geometry_and_device_memory(
                7,
                Arc::clone(&device_memory),
                32,
                0x1_0000_0000,
                16,
                12,
            ),
        ));
        gpu_memory.lock().map(0x4000, 0x9000_0000, 0x1000, 0, false);
        cache.set_channel_gpu_memory(gpu_memory);

        let existing_info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let existing_id = cache.insert_image(&existing_info, 0x4000);
        let view_compatible_info = ImageInfo {
            format: surface::PixelFormat::B8G8R8A8Unorm,
            ..existing_info
        };

        assert!(crate::compatible_formats::is_view_compatible(
            existing_info.format,
            view_compatible_info.format,
            false,
            true
        ));
        assert_eq!(
            cache.find_image_with_caps(
                &view_compatible_info,
                0x4000,
                RelaxedOptions::empty(),
                false,
                true,
            ),
            Some(existing_id)
        );
        assert_eq!(
            cache.find_image_with_caps(
                &view_compatible_info,
                0x4000,
                RelaxedOptions::FORCE_BROKEN_VIEWS,
                false,
                true,
            ),
            None
        );
        assert_eq!(
            cache.find_image_in_cpu_region_with_caps(
                &view_compatible_info,
                0x4000,
                0x9000_0000,
                RelaxedOptions::FORCE_BROKEN_VIEWS,
                false,
                true,
            ),
            None
        );
    }

    #[test]
    fn find_or_insert_allocates_a_new_virtual_invalid_range_after_each_lookup_miss() {
        let mut cache = test_cache();
        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            ..ImageInfo::default()
        };

        let first_id = cache.find_or_insert_image(&info, 0x4000);
        let second_id = cache.find_or_insert_image(&info, 0x4000);
        let other_id = cache.find_or_insert_image(&info, 0x8000);

        assert_ne!(first_id, second_id);
        assert_ne!(first_id, other_id);
        assert_ne!(second_id, other_id);
        assert_eq!(cache.slot_images.size(), 4);
        assert_ne!(
            cache.slot_images[first_id].cpu_addr,
            cache.slot_images[second_id].cpu_addr
        );
    }

    #[test]
    fn sparse_insert_without_channel_memory_does_not_allocate_virtual_state() {
        use std::panic::{catch_unwind, AssertUnwindSafe};

        let mut cache = unbound_test_cache();
        let mut info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: Extent3D {
                width: 512,
                height: 1,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        info.is_sparse = true;

        let slot_count = cache.slot_images.size();
        let virtual_invalid_space = cache.virtual_invalid_space;

        let result = catch_unwind(AssertUnwindSafe(|| {
            cache.insert_image(&info, 0x8000);
        }));

        assert!(result.is_err());
        assert_eq!(cache.slot_images.size(), slot_count);
        assert_eq!(cache.virtual_invalid_space, virtual_invalid_space);
    }

    #[test]
    fn track_sparse_unregistered_image_uses_submapped_segments_like_upstream() {
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let mut cache = test_cache();
        let gpu_memory = Arc::new(ParkingMutex::new(MemoryManager::new_with_geometry(
            11,
            22,
            1 << 22,
            16,
            12,
        )));
        {
            let mut gpu_memory = gpu_memory.lock();
            gpu_memory.map(0x8000, 0xA000, 0x1000, 0, false);
            gpu_memory.map(0x9000, 0xC000, 0x1000, 0, false);
        }
        cache.set_channel_gpu_memory(Arc::clone(&gpu_memory));

        let mut info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: Extent3D {
                width: 2048,
                height: 1,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        info.is_sparse = true;
        let image_id = cache
            .slot_images
            .insert(ImageBase::new(info, 0x8000, 0xA000).into());
        cache.slot_images[image_id]
            .flags
            .insert(ImageFlagBits::SPARSE);

        cache.track_image(image_id);

        assert!(cache.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::TRACKED));
        assert!(!cache.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::REGISTERED));
        assert!(!cache.sparse_views.contains_key(&image_id));
    }

    #[test]
    fn run_garbage_collector_deletes_old_lru_images_only() {
        let mut cache = test_cache();
        let info = test_color_info(16, 16);
        let old_id = cache.insert_image(&info, 0x4000);
        let touched_id = cache.insert_image(&info, 0x8000);
        let initial_memory = cache.total_used_memory;
        assert!(initial_memory > 0);

        cache.frame_tick = 100;
        cache.touch_image(touched_id);
        cache.run_garbage_collector();

        assert!(cache
            .collect_images_in_gpu_region(0x4000, 4, false)
            .is_empty());
        assert!(!cache.slot_images.contains(old_id));
        assert_eq!(
            cache
                .collect_images_in_gpu_region(0x8000, 4, false)
                .as_slice(),
            &[touched_id]
        );
        assert!(cache.total_used_memory < initial_memory);
    }

    #[test]
    fn run_garbage_collector_preserves_gpu_modified_image_without_download_path() {
        let mut cache = test_cache();
        let info = test_color_info(16, 16);
        let image_id = cache.insert_image(&info, 0x4000);
        cache.slot_images[image_id]
            .flags
            .remove(ImageFlagBits::CPU_MODIFIED);
        cache.mark_modification_by_id(image_id);
        cache.frame_tick = 100;
        cache.expected_memory = 0;
        cache.critical_memory = u64::MAX;

        cache.run_garbage_collector();

        assert_eq!(
            cache
                .collect_images_in_gpu_region(0x4000, 4, false)
                .as_slice(),
            &[image_id]
        );
        assert!(cache.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::GPU_MODIFIED));
    }

    #[test]
    fn gc_backend_download_mutates_the_owned_image_base() {
        let mut cache = test_cache();
        let info = test_color_info(16, 16);
        let image_id = cache.insert_image(&info, 0x4000);
        cache.slot_images[image_id]
            .flags
            .remove(ImageFlagBits::CPU_MODIFIED);
        cache.slot_images[image_id]
            .flags
            .insert(ImageFlagBits::GPU_MODIFIED);
        cache.set_guest_memory_writer(std::sync::Arc::new(|_, _| {}));

        let downloaded =
            cache.download_image_for_gc(image_id, &mut |_image_id, image, _backend, staging| {
                image.flags.insert(ImageFlagBits::REMAPPED);
                staging.fill(0);
                true
            });

        assert!(downloaded);
        assert!(cache.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::REMAPPED));
        assert!(!cache.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::GPU_MODIFIED));
    }

    #[test]
    fn unmap_gpu_memory_marks_overlapping_images_cpu_modified_and_remapped() {
        use crate::control::channel_state::ChannelState;
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let mut cache = test_cache();
        let gpu_memory = Arc::new(ParkingMutex::new(MemoryManager::new(17)));
        let other_gpu_memory = Arc::new(ParkingMutex::new(MemoryManager::new(23)));
        let mut channel = ChannelState::new(10);
        channel.memory_manager = Some(gpu_memory);
        let mut other_channel = ChannelState::new(11);
        other_channel.memory_manager = Some(other_gpu_memory);
        cache.create_channel(&channel);
        cache.create_channel(&other_channel);
        cache.bind_to_channel(10);

        let info = test_color_info(16, 16);
        let image_id = cache.insert_image(&info, 0x4000);
        assert!(cache.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::TRACKED));

        cache.unmap_gpu_memory(23, 0x4000, 0x100);
        assert!(!cache.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::REMAPPED));

        cache.unmap_gpu_memory(17, 0x4000, 0x100);

        let flags = cache.slot_images[image_id].flags;
        assert!(flags.contains(ImageFlagBits::CPU_MODIFIED));
        assert!(flags.contains(ImageFlagBits::REMAPPED));
        assert!(!flags.contains(ImageFlagBits::TRACKED));
    }

    #[test]
    fn write_downloaded_image_prefers_channel_gpu_memory() {
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let mut cache = test_cache();
        let gpu_memory = Arc::new(ParkingMutex::new(MemoryManager::new_with_geometry(
            7,
            22,
            1 << 22,
            16,
            12,
        )));
        {
            let mut gpu_memory = gpu_memory.lock();
            gpu_memory.map(0, 0, 0x1_0000, 0, false);
        }
        cache.set_channel_gpu_memory(Arc::clone(&gpu_memory));
        cache.set_guest_memory_writer(Arc::new(|_, _| {
            panic!("channel gpu_memory should own texture download writeback")
        }));

        let mut info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::Linear,
            size: crate::texture_cache::types::Extent3D {
                width: 2,
                height: 2,
                depth: 1,
            },
            tiling: TilingMode::PitchLinear(16),
            ..ImageInfo::default()
        };
        info.resources.levels = 1;
        info.resources.layers = 1;
        let image = ImageBase::new(info.clone(), 0x4000, 0x8000);
        let copy = BufferImageCopy {
            buffer_offset: 0,
            buffer_size: 16,
            buffer_row_length: 4,
            buffer_image_height: 2,
            image_subresource: SubresourceLayers::default(),
            image_offset: Offset3D { x: 0, y: 0, z: 0 },
            image_extent: Extent3D {
                width: 2,
                height: 2,
                depth: 1,
            },
        };
        let staging = [
            1, 2, 3, 4, 5, 6, 7, 8, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 9, 10, 11, 12,
            13, 14, 15, 16,
        ];

        assert!(cache.write_downloaded_image(&image, &[copy], &staging));
    }

    #[test]
    fn write_downloaded_image_uses_guest_writer_when_channel_memory_is_locked() {
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::{Arc, Mutex};

        let mut cache = test_cache();
        let gpu_memory = Arc::new(ParkingMutex::new(MemoryManager::new_with_geometry(
            7,
            22,
            1 << 22,
            16,
            12,
        )));
        cache.set_channel_gpu_memory(Arc::clone(&gpu_memory));

        let writes = Arc::new(Mutex::new(Vec::<(u64, Vec<u8>)>::new()));
        let writes_for_callback = Arc::clone(&writes);
        cache.set_guest_memory_writer(Arc::new(move |addr, data| {
            writes_for_callback
                .lock()
                .unwrap()
                .push((addr, data.to_vec()));
        }));

        let _held_channel_lock = gpu_memory.lock();
        let mut info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::Linear,
            size: crate::texture_cache::types::Extent3D {
                width: 1,
                height: 1,
                depth: 1,
            },
            tiling: TilingMode::PitchLinear(4),
            ..ImageInfo::default()
        };
        info.resources.levels = 1;
        info.resources.layers = 1;
        let image = ImageBase::new(info, 0x4000, 0x8000);
        let copy = BufferImageCopy {
            buffer_offset: 0,
            buffer_size: 4,
            buffer_row_length: 1,
            buffer_image_height: 1,
            image_subresource: SubresourceLayers::default(),
            image_offset: Offset3D { x: 0, y: 0, z: 0 },
            image_extent: Extent3D {
                width: 1,
                height: 1,
                depth: 1,
            },
        };

        assert!(cache.write_downloaded_image(&image, &[copy], &[1, 2, 3, 4]));
        let writes = writes.lock().unwrap();
        assert!(!writes.is_empty());
        assert_eq!(writes[0].0, 0x8000);
    }

    #[test]
    fn register_image_updates_gpu_page_table_and_unregister_clears_it() {
        let mut cache = test_cache();
        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            ..ImageInfo::default()
        };

        let image_id = cache.insert_image(&info, 0x4000);

        assert_eq!(
            cache
                .collect_images_in_gpu_region(0x4000, 4, false)
                .as_slice(),
            &[image_id]
        );

        cache.unregister_image(image_id);

        assert!(cache
            .collect_images_in_gpu_region(0x4000, 4, false)
            .is_empty());
    }

    #[test]
    fn unregister_image_uses_registration_address_space_after_channel_switch() {
        use crate::control::channel_state::ChannelState;
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let mut cache = unbound_test_cache();
        let memory_a = Arc::new(ParkingMutex::new(MemoryManager::new(17)));
        let memory_b = Arc::new(ParkingMutex::new(MemoryManager::new(18)));
        let mut channel_a = ChannelState::new(10);
        let mut channel_b = ChannelState::new(11);
        channel_a.memory_manager = Some(memory_a);
        channel_b.memory_manager = Some(memory_b);
        cache.create_channel(&channel_a);
        cache.create_channel(&channel_b);
        cache.bind_to_channel(10);

        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let image_id = cache.insert_image(&info, 0x4000);
        let owner_table = cache
            .image_gpu_page_table_indices
            .get(&image_id)
            .copied()
            .expect("registered image must retain its GPU page-table owner");
        assert!(cache.gpu_page_table_storage[owner_table]
            .values()
            .any(|image_ids| image_ids.contains(&image_id)));

        cache.bind_to_channel(11);
        assert_ne!(cache.current_gpu_page_table_index(false), Some(owner_table));
        cache.unregister_image(image_id);

        assert!(cache.gpu_page_table_storage[owner_table]
            .values()
            .all(|image_ids| !image_ids.contains(&image_id)));
        assert!(!cache.image_gpu_page_table_indices.contains_key(&image_id));
    }

    #[test]
    fn region_collection_deduplicates_and_clears_temporary_picked_flags() {
        let mut cache = test_cache();
        let info = test_color_info(1024, 512);
        let image_id = cache
            .slot_images
            .insert(ImageBase::new(info, 0x80000, 0x80000).into());
        cache.register_image(image_id);
        let map_id = cache.slot_images[image_id].map_view_id;

        assert!(
            cache
                .page_table
                .values()
                .filter(|map_ids| map_ids.contains(&map_id))
                .count()
                > 1
        );
        assert_eq!(
            collect_images_in_region_for_test(&mut cache, 0x80000, 0x20_0000).as_slice(),
            &[image_id]
        );
        assert!(!cache.slot_map_views[map_id].picked);
        assert!(!cache.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::PICKED));

        assert_eq!(
            cache
                .collect_images_in_gpu_region(0x80000, 0x20_0000, false)
                .as_slice(),
            &[image_id]
        );
        assert!(!cache.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::PICKED));
    }

    #[test]
    fn region_callback_stops_early_and_clears_temporary_picked_state() {
        let mut cache = test_cache();
        let info = test_color_info(1024, 512);
        let first_id = cache
            .slot_images
            .insert(ImageBase::new(info.clone(), 0x80000, 0x80000).into());
        cache.register_image(first_id);
        let second_id = cache
            .slot_images
            .insert(ImageBase::new(info, 0x90000, 0x90000).into());
        cache.register_image(second_id);

        let mut stopped_visits = 0;
        assert!(cache.for_each_image_in_region(0x80000, 0x20_0000, |_, _| {
            stopped_visits += 1;
            true
        }));
        assert_eq!(stopped_visits, 1);

        for image_id in [first_id, second_id] {
            let map_id = cache.slot_images[image_id].map_view_id;
            assert!(!cache.slot_map_views[map_id].picked);
            assert!(!cache.slot_images[image_id]
                .flags
                .contains(ImageFlagBits::PICKED));
        }

        let mut complete_visits = 0;
        assert!(!cache.for_each_image_in_region(0x80000, 0x20_0000, |_, _| {
            complete_visits += 1;
            false
        }));
        assert_eq!(complete_visits, 2);
    }

    #[test]
    fn delete_image_marks_maxwell_render_targets_dirty() {
        use crate::control::channel_state::ChannelState;
        use crate::dirty_flags;
        use crate::engines::draw_manager::Maxwell3DAccess;
        use crate::engines::maxwell_3d::Maxwell3D;

        let mut cache = test_cache();
        let mut channel = ChannelState::new(10);
        channel.maxwell_3d = Some(Box::new(Maxwell3D::new()));
        channel.memory_manager = cache.channel_gpu_memory.clone();
        cache.create_channel(&channel);
        cache.bind_to_channel(10);

        let maxwell3d = channel.maxwell_3d.as_mut().unwrap();
        maxwell3d.clear_dirty_flag(dirty_flags::flags::RENDER_TARGETS);
        maxwell3d.clear_dirty_flag(dirty_flags::flags::ZETA_BUFFER);
        for rt in 0..8 {
            maxwell3d.clear_dirty_flag(dirty_flags::flags::COLOR_BUFFER0 + rt);
        }

        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            ..ImageInfo::default()
        };

        let image_id = cache.insert_image(&info, 0x4000);
        cache.untrack_image(image_id);
        cache.unregister_image(image_id);
        cache.delete_image(image_id, false);

        let flags = maxwell3d.dirty_flags();
        assert!(flags[dirty_flags::flags::RENDER_TARGETS as usize]);
        assert!(flags[dirty_flags::flags::ZETA_BUFFER as usize]);
        for rt in 0..8 {
            assert!(flags[(dirty_flags::flags::COLOR_BUFFER0 + rt) as usize]);
        }
    }

    #[test]
    fn gpu_page_table_storage_is_shared_per_address_space() {
        use crate::control::channel_state::ChannelState;
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let mut cache = test_cache();
        let shared_gpu_memory = Arc::new(ParkingMutex::new(MemoryManager::new(17)));
        let separate_gpu_memory = Arc::new(ParkingMutex::new(MemoryManager::new(23)));

        let mut channel_a = ChannelState::new(10);
        channel_a.memory_manager = Some(Arc::clone(&shared_gpu_memory));
        let mut channel_b = ChannelState::new(11);
        channel_b.memory_manager = Some(Arc::clone(&shared_gpu_memory));
        let mut channel_c = ChannelState::new(12);
        channel_c.memory_manager = Some(Arc::clone(&separate_gpu_memory));

        cache.create_channel(&channel_a);
        cache.create_channel(&channel_b);
        cache.create_channel(&channel_c);

        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            ..ImageInfo::default()
        };

        cache.bind_to_channel(10);
        let image_id = cache.insert_image(&info, 0x4000);

        cache.bind_to_channel(11);
        assert_eq!(
            cache
                .collect_images_in_gpu_region(0x4000, 4, false)
                .as_slice(),
            &[image_id]
        );

        cache.bind_to_channel(12);
        assert!(cache
            .collect_images_in_gpu_region(0x4000, 4, false)
            .is_empty());
    }

    #[test]
    fn join_images_deletes_exact_sparse_gpu_overlap() {
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let mut cache = test_cache();
        let gpu_memory = Arc::new(ParkingMutex::new(MemoryManager::new_with_geometry(
            7,
            22,
            1 << 22,
            16,
            12,
        )));
        {
            let mut gpu_memory = gpu_memory.lock();
            gpu_memory.map(0x8000, 0xA000, 0x1000, 0, false);
            gpu_memory.map(0x9000, 0xC000, 0x1000, 0, false);
        }
        cache.set_channel_gpu_memory(Arc::clone(&gpu_memory));
        let mut info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width: 512,
                height: 1,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        info.is_sparse = true;
        let old_id = cache.join_images(&info, 0x8000, 0xA000);
        assert!(cache.slot_images[old_id]
            .flags
            .contains(ImageFlagBits::SPARSE));
        assert_eq!(
            cache.sparse_views.get(&old_id).expect("sparse maps").len(),
            2
        );

        let new_id = cache.join_images(&info, 0x8000, 0xD000);

        assert_ne!(new_id, old_id);
        assert_eq!(cache.slot_images[new_id].cpu_addr, 0xD000);
        assert_eq!(
            cache.sparse_views.get(&new_id).expect("sparse maps").len(),
            2
        );
        assert_eq!(
            cache
                .collect_images_in_gpu_region(0x8000, 4, false)
                .as_slice(),
            &[new_id]
        );
    }

    #[test]
    fn join_images_inserts_replacement_before_deleting_ignored_overlap_like_upstream() {
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let mut cache = test_cache();
        let gpu_memory = Arc::new(ParkingMutex::new(MemoryManager::new_with_geometry(
            7,
            22,
            1 << 22,
            16,
            12,
        )));
        {
            let mut gpu_memory = gpu_memory.lock();
            gpu_memory.map(0x8000, 0xA000, 0x1000, 0, false);
            gpu_memory.map(0x9000, 0xC000, 0x1000, 0, false);
        }
        cache.set_channel_gpu_memory(Arc::clone(&gpu_memory));
        let mut info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width: 512,
                height: 1,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        info.is_sparse = true;
        let old_id = cache.join_images(&info, 0x8000, 0xA000);

        let new_id = cache.join_images(&info, 0x8000, 0xD000);

        assert_ne!(
            new_id, old_id,
            "upstream inserts the replacement before deleting ignored overlaps"
        );
        assert_eq!(cache.slot_images[new_id].cpu_addr, 0xD000);
    }

    #[test]
    fn join_images_deletes_ignored_overlap_from_typed_slot() {
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let mut cache = test_cache();
        let gpu_memory = Arc::new(ParkingMutex::new(MemoryManager::new_with_geometry(
            7,
            22,
            1 << 22,
            16,
            12,
        )));
        {
            let mut gpu_memory = gpu_memory.lock();
            gpu_memory.map(0x8000, 0xA000, 0x1000, 0, false);
            gpu_memory.map(0x9000, 0xC000, 0x1000, 0, false);
        }
        cache.set_channel_gpu_memory(Arc::clone(&gpu_memory));
        let mut info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width: 512,
                height: 1,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        info.is_sparse = true;
        let old_id = cache.join_images(&info, 0x8000, 0xA000);

        let new_id = cache.join_images(&info, 0x8000, 0xD000);

        assert_ne!(new_id, old_id);
        assert!(!cache.slot_images.contains(old_id));
    }

    #[test]
    fn join_images_continues_after_gpu_modified_ignored_overlap_fail_soft() {
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let mut cache = test_cache();
        let gpu_memory = Arc::new(ParkingMutex::new(MemoryManager::new_with_geometry(
            7,
            22,
            1 << 22,
            16,
            12,
        )));
        {
            let mut gpu_memory = gpu_memory.lock();
            gpu_memory.map(0x8000, 0xA000, 0x1000, 0, false);
            gpu_memory.map(0x9000, 0xC000, 0x1000, 0, false);
        }
        cache.set_channel_gpu_memory(Arc::clone(&gpu_memory));
        let mut info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width: 512,
                height: 1,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        info.is_sparse = true;
        let old_id = cache.join_images(&info, 0x8000, 0xA000);
        cache.slot_images[old_id]
            .flags
            .insert(ImageFlagBits::GPU_MODIFIED);

        let new_id = cache.join_images(&info, 0x8000, 0xD000);

        assert_ne!(new_id, old_id);
        assert!(cache.slot_images.contains(new_id));
        assert!(!cache.slot_images.contains(old_id));
    }

    #[test]
    fn backend_join_continues_after_gpu_modified_ignored_overlap_fail_soft() {
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let mut cache = test_cache();
        let gpu_memory = Arc::new(ParkingMutex::new(MemoryManager::new_with_geometry(
            7,
            22,
            1 << 22,
            16,
            12,
        )));
        {
            let mut gpu_memory = gpu_memory.lock();
            gpu_memory.map(0x8000, 0xA000, 0x1000, 0, false);
            gpu_memory.map(0x9000, 0xC000, 0x1000, 0, false);
        }
        cache.set_channel_gpu_memory(Arc::clone(&gpu_memory));
        let mut info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width: 512,
                height: 1,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        info.is_sparse = true;
        let old_id = cache.join_images(&info, 0x8000, 0xA000);
        cache.slot_images[old_id]
            .flags
            .insert(ImageFlagBits::GPU_MODIFIED);

        let new_id = cache.join_images(&info, 0x8000, 0xD000);

        assert_ne!(new_id, old_id);
        assert!(!cache.slot_images.contains(old_id));
    }

    #[test]
    fn unmap_memory_unregisters_untracks_and_deletes_image() {
        let mut cache = test_cache();
        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: crate::texture_cache::types::Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let image_id = cache.insert_image(&info, 0x6000);
        let cpu_addr = cache.slot_images[image_id].cpu_addr;
        let map_id = cache.slot_images[image_id].map_view_id;
        assert!(map_id.is_valid());
        assert_eq!(
            collect_images_in_region_for_test(&mut cache, cpu_addr, 4).as_slice(),
            &[image_id]
        );

        cache.unmap_memory(cpu_addr, 4);

        assert!(collect_images_in_region_for_test(&mut cache, cpu_addr, 4).is_empty());
        assert_eq!(cache.slot_images.size(), 1);
        assert_eq!(cache.slot_map_views.size(), 0);
        assert!(cache.page_table.is_empty());
        assert!(cache.has_deleted_images);
    }

    #[test]
    fn find_or_insert_reuses_cpu_region_view_compatible_image() {
        let mut cache = test_cache();
        let first = ImageInfo {
            format: surface::PixelFormat::A2B10G10R10Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 1280,
                height: 720,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let second = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            ..first.clone()
        };

        let first_id = cache.find_or_insert_image_from_info(&first, 0x51FC_90000, 0x2AE9_E000);
        cache.mark_modification_by_id(first_id);
        let second_id = cache.find_or_insert_image_from_info(&second, 0x51FC_90000, 0x2AE9_E000);

        assert_eq!(first_id, second_id);
    }

    #[test]
    fn find_or_insert_reuses_existing_image_like_upstream() {
        let mut cache = test_cache();
        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            rescaleable: true,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            ..ImageInfo::default()
        };

        let first = cache.find_or_insert_image_from_info_with_options(
            &info,
            0x8000,
            0x1000,
            RelaxedOptions::empty(),
        );
        let second = cache.find_or_insert_image_from_info_with_options(
            &info,
            0x8000,
            0x1000,
            RelaxedOptions::empty(),
        );
        assert_eq!(second, first);
    }

    #[test]
    fn join_images_registers_common_cache_immediately_like_upstream() {
        let mut cache = test_cache();
        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            rescaleable: true,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            ..ImageInfo::default()
        };

        let inserted = cache.find_or_insert_image_from_info_with_options(
            &info,
            0x9000,
            0x2000,
            RelaxedOptions::empty(),
        );
        assert!(cache.slot_images[inserted]
            .flags
            .contains(ImageFlagBits::REGISTERED));
        assert_eq!(
            collect_images_in_region_for_test(&mut cache, 0x2000, 4).as_slice(),
            &[inserted]
        );
        assert!(!cache.image_allocs_table.is_empty());

        let reused = cache.find_or_insert_image_from_info_with_options(
            &info,
            0x9000,
            0x2000,
            RelaxedOptions::empty(),
        );
        assert_eq!(reused, inserted);
    }

    #[test]
    fn direct_find_or_insert_returns_fully_registered_image() {
        let mut cache = test_cache();
        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            ..ImageInfo::default()
        };

        let image_id = cache.find_or_insert_image_from_info(&info, 0x9000, 0x2000);
        assert!(cache.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::REGISTERED));
    }

    #[test]
    fn explicit_find_or_insert_result_path_returns_registered_image() {
        let mut cache = test_cache();
        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            ..ImageInfo::default()
        };

        let image_id = cache.find_or_insert_image_from_info_with_options(
            &info,
            0x9000,
            0x2000,
            RelaxedOptions::empty(),
        );

        assert!(cache.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::REGISTERED));
    }

    #[test]
    fn prepare_image_runs_common_refresh_and_mark_ordering() {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use std::sync::Arc;

        PREPARE_REFRESH_CALLS.with(|calls| calls.set(0));
        UPLOAD_BARRIER_CALLS.with(|calls| calls.set(0));
        let mut cache = TextureCacheBase::<TestImageViewParams>::new_for_backend(Arc::new(
            MaxwellDeviceMemoryManager::default(),
        ));
        cache.bind_runtime(Box::new(()));
        cache.channel_gpu_memory = Some(Arc::new(ParkingMutex::new(MemoryManager::new(17))));
        let image_id = cache.insert_typed_image(ImageBase::new(
            ImageInfo {
                format: surface::PixelFormat::A8B8G8R8Unorm,
                image_type: ImageType::E2D,
                resources: SubresourceExtent {
                    levels: 1,
                    layers: 1,
                },
                size: Extent3D {
                    width: 16,
                    height: 16,
                    depth: 1,
                },
                ..ImageInfo::default()
            },
            0x1000,
            0x2000,
        ));
        cache.slot_images[image_id].lru_index = cache.lru_cache.insert(image_id, 0);
        cache.prepare_image(image_id, true, false);

        PREPARE_REFRESH_CALLS.with(|calls| assert_eq!(calls.get(), 1));
        UPLOAD_BARRIER_CALLS.with(|calls| assert_eq!(calls.get(), 1));
        assert!(cache.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::GPU_MODIFIED));
        assert_eq!(cache.slot_images[image_id].modification_tick, 1);
    }

    #[test]
    fn refresh_contents_uploads_cpu_modified_image_once_and_tracks_it() {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use std::sync::Arc;

        PREPARE_REFRESH_CALLS.with(|calls| calls.set(0));
        UPLOAD_BARRIER_CALLS.with(|calls| calls.set(0));
        let mut cache = TextureCacheBase::<TestImageViewParams>::new_for_backend(Arc::new(
            MaxwellDeviceMemoryManager::default(),
        ));
        cache.set_channel_gpu_memory(Arc::new(ParkingMutex::new(MemoryManager::new(17))));
        let image_id = cache.insert_typed_image(ImageBase::new(
            ImageInfo {
                format: surface::PixelFormat::A8B8G8R8Unorm,
                image_type: ImageType::E2D,
                size: Extent3D {
                    width: 16,
                    height: 16,
                    depth: 1,
                },
                ..ImageInfo::default()
            },
            0x1000,
            0x2000,
        ));

        cache.refresh_contents(image_id);

        assert!(!cache.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::CPU_MODIFIED));
        assert!(cache.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::TRACKED));
        PREPARE_REFRESH_CALLS.with(|calls| assert_eq!(calls.get(), 1));
        UPLOAD_BARRIER_CALLS.with(|calls| assert_eq!(calls.get(), 1));

        cache.refresh_contents(image_id);

        PREPARE_REFRESH_CALLS.with(|calls| assert_eq!(calls.get(), 1));
        UPLOAD_BARRIER_CALLS.with(|calls| assert_eq!(calls.get(), 1));
    }

    #[test]
    fn synchronize_aliases_uses_common_tick_scale_and_copy_ordering() {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use crate::texture_cache::image_base::AliasedImage;
        use std::sync::Arc;

        JOIN_COPY_DISPATCH.with(|dispatch| dispatch.set((0, 0)));
        TEST_IMAGE_SCALE_SUCCEEDS.with(|enabled| enabled.set(true));
        let mut cache = TextureCacheBase::<TestImageViewParams>::new_for_backend(Arc::new(
            MaxwellDeviceMemoryManager::default(),
        ));
        cache.bind_runtime(Box::new(()));
        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            rescaleable: true,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let dst_id = cache.insert_typed_image(ImageBase::new(info.clone(), 0x1000, 0x2000));
        let src_id = cache.insert_typed_image(ImageBase::new(info, 0x2000, 0x3000));
        cache.slot_images[dst_id].modification_tick = 1;
        cache.slot_images[src_id].modification_tick = 2;
        cache.slot_images[src_id]
            .flags
            .insert(ImageFlagBits::GPU_MODIFIED | ImageFlagBits::RESCALED);
        cache.slot_images[dst_id].aliased_images.push(AliasedImage {
            id: src_id,
            copies: vec![ImageCopy::default()],
        });

        cache.synchronize_aliases(dst_id);

        assert_eq!(cache.slot_images[dst_id].modification_tick, 2);
        assert!(cache.slot_images[dst_id]
            .flags
            .contains(ImageFlagBits::GPU_MODIFIED | ImageFlagBits::RESCALED));
        JOIN_COPY_DISPATCH.with(|dispatch| assert_eq!(dispatch.get(), (1, 0)));
    }

    #[test]
    fn copy_image_owns_upstream_same_type_and_rescaling_policy() {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use std::sync::Arc;

        let _settings_guard = crate::test_support::RESOLUTION_SETTINGS_MUTEX
            .lock()
            .unwrap();
        let previous_resolution = common::settings::values().resolution_info.clone();
        struct ResolutionRestore(common::settings::ResolutionScalingInfo);
        impl Drop for ResolutionRestore {
            fn drop(&mut self) {
                common::settings::values_mut().resolution_info = self.0.clone();
            }
        }
        let _restore = ResolutionRestore(previous_resolution);
        {
            let mut values = common::settings::values_mut();
            values.resolution_info.up_scale = 3;
            values.resolution_info.down_shift = 1;
            values.resolution_info.active = true;
        }

        JOIN_COPY_DISPATCH.with(|dispatch| dispatch.set((0, 0)));
        JOIN_COPY_COPIES.with(|copies| copies.borrow_mut().clear());
        let mut cache = TextureCacheBase::<TestImageViewParams>::new_for_backend(Arc::new(
            MaxwellDeviceMemoryManager::default(),
        ));
        let image_info = |format| ImageInfo {
            format,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 64,
                height: 32,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let dst_id = cache.insert_typed_image(ImageBase::new(
            image_info(PixelFormat::A8B8G8R8Unorm),
            0x1000,
            0x2000,
        ));
        let src_id = cache.insert_typed_image(ImageBase::new(
            image_info(PixelFormat::R8Unorm),
            0x3000,
            0x4000,
        ));
        cache.slot_images[dst_id]
            .flags
            .insert(ImageFlagBits::RESCALED);
        cache.slot_images[src_id]
            .flags
            .insert(ImageFlagBits::RESCALED);
        let copy = ImageCopy {
            src_offset: Offset3D { x: 2, y: 4, z: 1 },
            dst_offset: Offset3D { x: 6, y: 8, z: 1 },
            extent: Extent3D {
                width: 10,
                height: 12,
                depth: 2,
            },
            ..ImageCopy::default()
        };

        cache.copy_image(dst_id, src_id, &[copy]);

        assert_eq!(
            surface::get_format_type(cache.slot_images[dst_id].info.format),
            surface::get_format_type(cache.slot_images[src_id].info.format)
        );
        assert_ne!(
            surface::bytes_per_block(cache.slot_images[dst_id].info.format),
            surface::bytes_per_block(cache.slot_images[src_id].info.format)
        );
        JOIN_COPY_DISPATCH.with(|dispatch| assert_eq!(dispatch.get(), (1, 0)));
        JOIN_COPY_COPIES.with(|copies| {
            let copies = copies.borrow();
            assert_eq!(copies.len(), 1);
            let observed = copies[0];
            assert_eq!(observed.src_offset, Offset3D { x: 3, y: 6, z: 1 });
            assert_eq!(observed.dst_offset, Offset3D { x: 9, y: 12, z: 1 });
            assert_eq!(
                observed.extent,
                Extent3D {
                    width: 15,
                    height: 18,
                    depth: 2,
                }
            );
        });
    }

    #[test]
    fn common_blit_image_owns_upstream_view_framebuffer_and_region_flow() {
        use crate::engines::fermi_2d::{Config, Filter, MemoryLayout, Operation, Surface};
        use crate::gpu::RenderTargetFormat;
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;

        BLIT_DISPATCH.with(|dispatch| *dispatch.borrow_mut() = None);
        let mut cache = TextureCacheBase::<TestImageViewParams>::new_for_backend(Arc::new(
            MaxwellDeviceMemoryManager::default(),
        ));
        cache.set_channel_gpu_memory(Arc::new(ParkingMutex::new(MemoryManager::new(17))));
        let surface = |gpu_addr: u64| Surface {
            format: RenderTargetFormat::A8B8G8R8Unorm as u32,
            linear: MemoryLayout::Pitch as u32,
            block_dimensions: 0,
            depth: 1,
            layer: 0,
            pitch: 32,
            width: 8,
            height: 8,
            addr_upper: (gpu_addr >> 32) as u32,
            addr_lower: gpu_addr as u32,
        };
        let src = surface(0x5000);
        let dst = surface(0x6000);
        let copy = Config {
            operation: Operation::SrcCopy,
            filter: Filter::Point,
            must_accelerate: true,
            dst_x0: 1,
            dst_y0: 2,
            dst_x1: 7,
            dst_y1: 6,
            src_x0: 2,
            src_y0: 3,
            src_x1: 8,
            src_y1: 7,
        };

        assert!(cache.blit_image(&dst, &src, &copy));
        BLIT_DISPATCH.with(|dispatch| {
            let (
                dst_framebuffer_id,
                src_framebuffer_id,
                dst_view_id,
                src_view_id,
                dst_region,
                src_region,
                filter,
                operation,
            ) = dispatch
                .borrow()
                .expect("common BlitImage must dispatch exactly once to its backend policy");
            assert!(dst_framebuffer_id.is_valid());
            assert!(src_framebuffer_id.is_valid());
            assert!(dst_view_id.is_valid());
            assert!(src_view_id.is_valid());
            assert_eq!(
                dst_region,
                Region2D {
                    start: Offset2D { x: 1, y: 2 },
                    end: Offset2D { x: 7, y: 6 },
                }
            );
            assert_eq!(
                src_region,
                Region2D {
                    start: Offset2D { x: 2, y: 3 },
                    end: Offset2D { x: 8, y: 7 },
                }
            );
            assert_eq!(filter, Filter::Point);
            assert_eq!(operation, Operation::SrcCopy);
        });
    }

    #[test]
    fn find_or_insert_continues_after_gpu_modified_ignored_overlap_fail_soft() {
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex as ParkingMutex;
        use std::sync::Arc;

        let mut cache = test_cache();
        let gpu_memory = Arc::new(ParkingMutex::new(MemoryManager::new_with_geometry(
            7,
            22,
            1 << 22,
            16,
            12,
        )));
        {
            let mut gpu_memory = gpu_memory.lock();
            gpu_memory.map(0x8000, 0xA000, 0x1000, 0, false);
            gpu_memory.map(0x9000, 0xC000, 0x1000, 0, false);
        }
        cache.set_channel_gpu_memory(Arc::clone(&gpu_memory));
        let mut info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            size: Extent3D {
                width: 512,
                height: 1,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        info.is_sparse = true;

        let old = cache.find_or_insert_image_from_info_with_options(
            &info,
            0x8000,
            0xA000,
            RelaxedOptions::empty(),
        );
        cache.slot_images[old]
            .flags
            .insert(ImageFlagBits::GPU_MODIFIED);

        let replacement = cache.find_or_insert_image_from_info_with_options(
            &info,
            0x8000,
            0xD000,
            RelaxedOptions::empty(),
        );

        assert!(cache.slot_images.contains(replacement));
        assert!(!cache.slot_images.contains(old));
    }

    #[test]
    fn find_or_insert_does_not_reuse_cpu_region_incompatible_samples_or_layers() {
        let mut cache = test_cache();
        let existing = ImageInfo {
            format: surface::PixelFormat::B10G11R11Float,
            image_type: ImageType::E2D,
            num_samples: 4,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 32,
                height: 32,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let requested_cube = ImageInfo {
            num_samples: 1,
            resources: SubresourceExtent {
                levels: 1,
                layers: 6,
            },
            layer_stride: 32 * 32 * 4,
            maybe_unaligned_layer_stride: 32 * 32 * 4,
            ..existing.clone()
        };

        let first_id = cache.find_or_insert_image_from_info(&existing, 0x55C_BB0000, 0x5065_6000);
        let second_id =
            cache.find_or_insert_image_from_info(&requested_cube, 0x55C_BB0000, 0x5065_6000);

        assert_ne!(first_id, second_id);
        assert!(!cache.slot_images.contains(first_id));
        assert_eq!(cache.slot_images[second_id].info.num_samples, 1);
        assert_eq!(cache.slot_images[second_id].info.resources.layers, 6);
    }

    #[test]
    fn delete_image_removes_view_references_and_framebuffers() {
        use crate::texture_cache::render_targets::RenderTargets;
        use common::slot_vector::SlotId;

        let mut cache = test_cache();
        let descriptor = color_2d_tic(0x5000_0000, 0);
        let view_id = cache.create_image_view(&descriptor);
        assert!(view_id.is_valid());
        let image_id = cache.slot_image_views[view_id].image_id;
        assert!(image_id.is_valid());

        cache.channel_state.image_views.insert(descriptor, view_id);
        cache.channel_state.image_view_ids.insert(0, view_id);
        cache
            .channel_state
            .image_view_ids
            .insert(common::slot_vector::SlotId::TAGGED_VALUE, view_id);
        let mut framebuffer_key = RenderTargets::default();
        framebuffer_key.color_buffer_ids[0] = view_id;
        cache
            .framebuffers
            .insert(framebuffer_key, SlotId { index: 0x1234 });

        cache.untrack_image(image_id);
        cache.unregister_image(image_id);
        cache.delete_image(image_id, false);

        assert!(!cache.channel_state.image_views.contains_key(&descriptor));
        assert!(cache
            .channel_state
            .image_view_ids
            .values()
            .all(|&id| id == CORRUPT_ID));
        assert!(cache.framebuffers.is_empty());
        assert!(cache.has_deleted_images);
        assert_eq!(cache.sentenced_images.retained_len(), 1);
        assert_eq!(cache.sentenced_image_view.retained_len(), 1);

        for _ in 0..TICKS_TO_DESTROY {
            cache.tick_delayed_destruction_rings();
        }

        assert_eq!(cache.sentenced_images.retained_len(), 0);
        assert_eq!(cache.sentenced_image_view.retained_len(), 0);
    }

    #[test]
    fn delete_image_invalidates_all_active_channel_image_views() {
        use crate::control::channel_state::ChannelState;

        let mut cache = test_cache();
        let mut channel_a = ChannelState::new(10);
        let mut channel_b = ChannelState::new(11);
        channel_a.bind_id = 10;
        channel_b.bind_id = 11;
        channel_a.memory_manager = cache.channel_gpu_memory.clone();
        channel_b.memory_manager = cache.channel_gpu_memory.clone();
        cache.create_channel(&channel_a);
        cache.create_channel(&channel_b);
        cache.bind_to_channel(10);

        let descriptor = color_2d_tic(0x5100_0000, 0);
        let view_id = cache.create_image_view(&descriptor);
        let image_id = cache.slot_image_views[view_id].image_id;

        {
            let channel = cache
                .channel_caches
                .channel_state_by_bind_id_mut(10)
                .expect("channel 10 exists");
            channel.image_views.insert(descriptor, view_id);
            channel.image_view_ids.insert(0, view_id);
            channel
                .image_view_ids
                .insert(common::slot_vector::SlotId::TAGGED_VALUE, view_id);
        }
        {
            let channel = cache
                .channel_caches
                .channel_state_by_bind_id_mut(11)
                .expect("channel 11 exists");
            channel.image_views.insert(descriptor, view_id);
            channel.image_view_ids.insert(0, view_id);
            channel
                .image_view_ids
                .insert(common::slot_vector::SlotId::TAGGED_VALUE, view_id);
        }

        cache.untrack_image(image_id);
        cache.unregister_image(image_id);
        cache.delete_image(image_id, false);

        for bind_id in [10, 11] {
            let channel = cache
                .channel_caches
                .channel_state_by_bind_id(bind_id)
                .expect("channel exists after delete");
            assert!(!channel.image_views.contains_key(&descriptor));
            assert!(channel.image_view_ids.values().all(|&id| id == CORRUPT_ID));
        }
    }

    #[test]
    fn invalidate_scale_shared_tail_invalidates_all_active_channel_image_views() {
        use crate::control::channel_state::ChannelState;

        let mut cache = test_cache();
        let mut channel_a = ChannelState::new(10);
        let mut channel_b = ChannelState::new(11);
        channel_a.bind_id = 10;
        channel_b.bind_id = 11;
        channel_a.memory_manager = cache.channel_gpu_memory.clone();
        channel_b.memory_manager = cache.channel_gpu_memory.clone();
        cache.create_channel(&channel_a);
        cache.create_channel(&channel_b);
        cache.bind_to_channel(10);

        let descriptor = color_2d_tic(0x5100_0000, 0);
        let view_id = cache.create_image_view(&descriptor);

        for bind_id in [10, 11] {
            let channel = cache
                .channel_caches
                .channel_state_by_bind_id_mut(bind_id)
                .expect("channel exists");
            channel.image_views.insert(descriptor, view_id);
            channel.image_view_ids.insert(0, view_id);
            channel
                .image_view_ids
                .insert(common::slot_vector::SlotId::TAGGED_VALUE, view_id);
        }

        cache.remove_image_view_references(&[view_id]);
        cache.invalidate_channel_image_views();

        for bind_id in [10, 11] {
            let channel = cache
                .channel_caches
                .channel_state_by_bind_id(bind_id)
                .expect("channel exists after invalidate");
            assert!(!channel.image_views.contains_key(&descriptor));
            assert!(channel.image_view_ids.values().all(|&id| id == CORRUPT_ID));
        }
    }

    #[test]
    fn invalidate_scale_removes_views_framebuffers_and_marks_deleted() {
        use crate::texture_cache::render_targets::RenderTargets;
        use common::slot_vector::SlotId;

        let mut cache = test_cache();
        cache.frame_tick = 7;
        let descriptor = color_2d_tic(0x5300_0000, 0);
        let view_id = cache.create_image_view(&descriptor);
        let image_id = cache.slot_image_views[view_id].image_id;

        cache.channel_state.image_views.insert(descriptor, view_id);
        cache.channel_state.image_view_ids.insert(0, view_id);
        cache
            .channel_state
            .image_view_ids
            .insert(common::slot_vector::SlotId::TAGGED_VALUE, view_id);
        cache.render_targets.color_buffer_ids[0] = view_id;
        cache.render_targets.depth_buffer_id = view_id;
        let mut framebuffer_key = RenderTargets::default();
        framebuffer_key.color_buffer_ids[0] = view_id;
        cache
            .framebuffers
            .insert(framebuffer_key, SlotId { index: 0x4321 });

        cache.invalidate_scale(image_id);

        assert_eq!(cache.slot_images[image_id].scale_tick, 8);
        assert!(cache.slot_images[image_id].image_view_ids.is_empty());
        assert!(cache.slot_images[image_id].image_view_infos.is_empty());
        assert_eq!(
            cache.render_targets.color_buffer_ids[0],
            ImageViewId::default()
        );
        assert_eq!(cache.render_targets.depth_buffer_id, ImageViewId::default());
        assert!(cache.framebuffers.is_empty());
        assert!(!cache.channel_state.image_views.contains_key(&descriptor));
        assert!(cache
            .channel_state
            .image_view_ids
            .values()
            .all(|&id| id == CORRUPT_ID));
        assert!(cache.has_deleted_images);
        assert_eq!(cache.sentenced_image_view.retained_len(), 1);
        assert_eq!(cache.sentenced_images.retained_len(), 0);
    }

    #[test]
    fn scale_up_accounts_scaled_memory_once() {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use std::sync::Arc;

        TEST_IMAGE_SCALE_SUCCEEDS.with(|enabled| enabled.set(true));
        let mut cache = TextureCacheBase::<TestImageViewParams>::new_for_backend(Arc::new(
            MaxwellDeviceMemoryManager::default(),
        ));
        cache.bind_runtime(Box::new(()));
        let image_id = cache.insert_typed_image(ImageBase::new(
            ImageInfo {
                format: surface::PixelFormat::A8B8G8R8Unorm,
                image_type: ImageType::E2D,
                rescaleable: true,
                size: Extent3D {
                    width: 16,
                    height: 16,
                    depth: 1,
                },
                ..ImageInfo::default()
            },
            0x5400_0000,
            0x6400_0000,
        ));
        let initial_memory = cache.total_used_memory;
        let expected_scaled_size =
            TextureCacheBase::<TestImageViewParams>::scaled_image_memory_size(
                &cache.slot_images[image_id],
            );

        assert!(cache.scale_up(image_id));

        assert_eq!(
            cache.total_used_memory,
            initial_memory + expected_scaled_size
        );

        assert!(!cache.scale_up(image_id));

        assert_eq!(
            cache.total_used_memory,
            initial_memory + expected_scaled_size
        );
    }

    #[test]
    fn insert_image_registers_and_delete_removes_image_alloc_entry() {
        let mut cache = test_cache();
        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let image_id = cache.insert_image(&info, 0x5200_0000);
        let gpu_addr = cache.slot_images[image_id].gpu_addr;
        let alloc_id = cache.image_allocs_table[&gpu_addr];

        assert_eq!(cache.slot_image_allocs[alloc_id].images, vec![image_id]);

        cache.untrack_image(image_id);
        cache.unregister_image(image_id);
        cache.delete_image(image_id, false);

        assert!(!cache.image_allocs_table.contains_key(&gpu_addr));
    }

    #[test]
    fn join_images_records_incompatible_overlaps() {
        let mut cache = test_cache();
        let first = ImageInfo {
            format: surface::PixelFormat::B10G11R11Float,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 480,
                height: 272,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let second = ImageInfo {
            format: surface::PixelFormat::A2B10G10R10Unorm,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 1920,
                height: 1080,
                depth: 1,
            },
            ..ImageInfo::default()
        };

        let first_id = cache.join_images(&first, 0x5219_F0000, 0x2CBF_E000);
        let second_id = cache.join_images(&second, 0x5219_F0000, 0x2CBF_E000);

        assert_ne!(first_id, second_id);
        assert!(cache.slot_images[first_id]
            .overlapping_images
            .contains(&second_id));
        assert!(cache.slot_images[second_id]
            .overlapping_images
            .contains(&first_id));
    }

    #[test]
    fn join_images_applies_bad_overlap_relations_before_return() {
        let mut cache = test_cache();
        let first = ImageInfo {
            format: surface::PixelFormat::B10G11R11Float,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 480,
                height: 272,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let second = ImageInfo {
            format: surface::PixelFormat::A2B10G10R10Unorm,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 1920,
                height: 1080,
                depth: 1,
            },
            ..ImageInfo::default()
        };

        let first_id = cache.join_images(&first, 0x5219_F0000, 0x2CBF_E000);
        let second_id = cache.join_images(&second, 0x5219_F0000, 0x2CBF_E000);

        assert_ne!(first_id, second_id);
        assert!(cache.slot_images[first_id]
            .overlapping_images
            .contains(&second_id));
        assert!(cache.slot_images[second_id]
            .overlapping_images
            .contains(&first_id));
        assert!(cache.slot_images[second_id]
            .flags
            .contains(ImageFlagBits::REGISTERED));
    }

    #[test]
    fn join_images_applies_alias_relations_before_return() {
        let mut cache = test_cache();
        let mut full = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 2,
                layers: 1,
            },
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        full.layer_stride = crate::texture_cache::util::calculate_layer_stride(&full);
        full.maybe_unaligned_layer_stride = crate::texture_cache::util::calculate_layer_size(&full);
        let sub = ImageInfo {
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 32,
                height: 32,
                depth: 1,
            },
            ..full.clone()
        };

        let full_base = ImageBase::new(full.clone(), 0x5000, 0x9000);
        let mip_offset = full_base.mip_level_offsets[1] as u64;
        let full_id = cache.join_images(&full, 0x5000, 0x9000);

        let sub_id = cache.join_images(&sub, 0x5000 + mip_offset, 0x9000 + mip_offset);

        assert!(!cache.slot_images[sub_id].aliased_images.is_empty());
        assert!(!cache.slot_images[full_id].aliased_images.is_empty());
        assert!(cache.slot_images[sub_id]
            .flags
            .contains(ImageFlagBits::ALIAS));
        assert!(cache.slot_images[sub_id]
            .flags
            .contains(ImageFlagBits::REGISTERED));
    }

    #[test]
    fn join_images_preserves_gpu_modified_alias_source_and_registers_result() {
        let mut cache = test_cache();
        let mut full = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 2,
                layers: 1,
            },
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        full.layer_stride = crate::texture_cache::util::calculate_layer_stride(&full);
        full.maybe_unaligned_layer_stride = crate::texture_cache::util::calculate_layer_size(&full);
        let sub = ImageInfo {
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 32,
                height: 32,
                depth: 1,
            },
            ..full.clone()
        };

        let full_base = ImageBase::new(full.clone(), 0x5000, 0x9000);
        let mip_offset = full_base.mip_level_offsets[1] as u64;
        let full_id = cache.join_images(&full, 0x5000, 0x9000);
        cache.mark_modification_by_id(full_id);

        let sub_id = cache.join_images(&sub, 0x5000 + mip_offset, 0x9000 + mip_offset);

        assert!(cache.slot_images[sub_id]
            .flags
            .contains(ImageFlagBits::REGISTERED));
        assert!(cache.slot_images[sub_id]
            .flags
            .contains(ImageFlagBits::ALIAS));
        assert!(cache.slot_images[full_id]
            .flags
            .contains(ImageFlagBits::GPU_MODIFIED));
    }

    #[test]
    fn join_images_uses_runtime_msaa_copy_when_sample_counts_differ() {
        use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
        use std::sync::Arc;

        JOIN_COPY_DISPATCH.with(|dispatch| dispatch.set((0, 0)));
        let mut cache = TextureCacheBase::<TestImageViewParams>::new_for_backend(Arc::new(
            MaxwellDeviceMemoryManager::default(),
        ));
        cache.set_channel_gpu_memory(Arc::new(ParkingMutex::new(MemoryManager::new(17))));
        let multisampled = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            num_samples: 4,
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let single_sampled = ImageInfo {
            num_samples: 1,
            ..multisampled.clone()
        };

        let source_id = cache.join_images(&multisampled, 0x6000, 0xA000);
        cache.mark_modification_by_id(source_id);
        let _destination_id = cache.join_images(&single_sampled, 0x6000, 0xA000);

        assert_eq!(
            JOIN_COPY_DISPATCH.with(std::cell::Cell::get),
            (0, 1),
            "upstream JoinImages dispatches sample-count mismatches to CopyImageMSAA"
        );
    }

    #[test]
    fn join_images_registers_result_after_gpu_modified_overlap_classification() {
        let mut cache = test_cache();
        let mut full = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 2,
                layers: 1,
            },
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        full.layer_stride = crate::texture_cache::util::calculate_layer_stride(&full);
        full.maybe_unaligned_layer_stride = crate::texture_cache::util::calculate_layer_size(&full);
        let sub = ImageInfo {
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 32,
                height: 32,
                depth: 1,
            },
            ..full.clone()
        };

        let full_base = ImageBase::new(full.clone(), 0x5000, 0x9000);
        let mip_offset = full_base.mip_level_offsets[1] as u64;
        let sub_id = cache.join_images(&sub, 0x5000 + mip_offset, 0x9000 + mip_offset);
        cache.mark_modification_by_id(sub_id);

        let full_id = cache.join_images(&full, 0x5000, 0x9000);

        assert!(cache.slot_images[full_id]
            .flags
            .contains(ImageFlagBits::REGISTERED));
        assert!(cache.slot_images[full_id]
            .flags
            .contains(ImageFlagBits::GPU_MODIFIED));
        assert!(!cache.slot_images.contains(sub_id));
    }

    #[test]
    fn create_image_view_uses_try_find_base_layer() {
        let mut cache = test_cache();
        let mut descriptor = color_2d_tic(0, 2);
        let layer_stride = ImageInfo::from_tic_entry(&descriptor).layer_stride as u64;
        descriptor = color_2d_tic(0x5000_0000 + 2 * layer_stride, 2);

        let view_id = cache.create_image_view(&descriptor);

        assert!(view_id.is_valid());
        let view = &cache.slot_image_views[view_id];
        assert_eq!(view.range.base.layer, 2);
        assert_eq!(cache.slot_images[view.image_id].gpu_addr, 0x5000_0000);
    }

    #[test]
    fn create_image_view_with_gpu_to_cpu_uses_virtual_invalid_fallback_like_upstream_insert() {
        let mut cache = test_cache();
        let mut descriptor = color_2d_tic(0, 2);
        let layer_stride = ImageInfo::from_tic_entry(&descriptor).layer_stride as u64;
        descriptor = color_2d_tic(0x5000_0000 + 2 * layer_stride, 2);

        let view_id = cache.create_image_view_with_gpu_to_cpu(&descriptor, &mut |_gpu, _size| None);

        assert!(view_id.is_valid());
        let image_id = cache.slot_image_views[view_id].image_id;
        assert_eq!(cache.slot_images[image_id].gpu_addr, 0x5000_0000);
        assert!(cache.slot_images[image_id].cpu_addr >= !(1u64 << 40));
    }

    #[test]
    fn typed_create_image_view_uses_virtual_invalid_fallback_like_upstream_insert() {
        let mut cache = test_cache();
        let mut descriptor = color_2d_tic(0, 2);
        let layer_stride = ImageInfo::from_tic_entry(&descriptor).layer_stride as u64;
        descriptor = color_2d_tic(0x5100_0000 + 2 * layer_stride, 2);

        let view_id = cache.create_image_view(&descriptor);

        assert!(view_id.is_valid());
        let image_id = cache.slot_image_views[view_id].image_id;
        assert_eq!(cache.slot_images[image_id].gpu_addr, 0x5100_0000);
        assert!(cache.slot_images[image_id].cpu_addr >= !(1u64 << 40));
        assert!(cache.slot_images[image_id].backend.is_some());
        assert!(cache.slot_image_views[view_id].backend.is_some());
    }

    #[test]
    #[should_panic(expected = "TextureCache::CreateImageView TryFindBase failed")]
    fn create_image_view_panics_when_try_find_base_fails_like_upstream() {
        let descriptor = color_2d_tic(0x5001, 0);
        let image = ImageBase::new(ImageInfo::from_tic_entry(&descriptor), 0x5000, 0x9000);

        let _ = TextureCacheBase::<CommonTextureCacheParams>::create_image_view_base(
            &image,
            &descriptor,
        );
    }

    #[test]
    #[should_panic(expected = "TextureCache::CreateImageView base level must be zero")]
    fn create_image_view_panics_when_base_level_is_not_zero_like_upstream() {
        let mut image = ImageBase::new(
            ImageInfo {
                format: surface::PixelFormat::A8B8G8R8Unorm,
                image_type: ImageType::E2D,
                resources: SubresourceExtent {
                    levels: 2,
                    layers: 1,
                },
                size: Extent3D {
                    width: 64,
                    height: 64,
                    depth: 1,
                },
                tiling: TilingMode::BlockLinear(Extent3D {
                    width: 0,
                    height: 0,
                    depth: 0,
                }),
                layer_stride: 0x1000,
                maybe_unaligned_layer_stride: 0x1000,
                num_samples: 1,
                tile_width_spacing: 0,
                rescaleable: false,
                downscaleable: false,
                forced_flushed: false,
                dma_downloaded: false,
                is_sparse: false,
            },
            0x5000,
            0x9000,
        );
        image.mip_level_offsets[1] = 0x100;
        let mip_descriptor = color_2d_tic(0x5100, 0);

        let _ = TextureCacheBase::<CommonTextureCacheParams>::create_image_view_base(
            &image,
            &mip_descriptor,
        );
    }

    #[test]
    #[should_panic(expected = "TextureCache::FindRenderTargetView TryFindBase failed")]
    fn find_render_target_view_panics_when_try_find_base_fails_like_upstream() {
        let mut cache = test_cache();
        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let image_id = cache
            .slot_images
            .insert(ImageBase::new(info.clone(), 0x5000, 0x9000).into());

        let _ = cache.find_image_view_from_image_info(image_id, &info, 0x5001);
    }

    fn insert_test_image(cache: &mut TextureCacheBase, gpu_addr: u64) -> ImageId {
        let info = ImageInfo {
            format: surface::PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        cache.slot_images.insert(
            crate::texture_cache::image_base::ImageBase::new(info, gpu_addr, gpu_addr).into(),
        )
    }

    fn insert_test_view(cache: &mut TextureCacheBase, image_id: ImageId) -> ImageViewId {
        let mut view =
            ImageViewBase::null(crate::texture_cache::image_view_base::NullImageViewParams);
        view.image_id = image_id;
        cache
            .slot_image_views
            .insert(ImageViewSlot::pending(ImageViewInfo::default(), view))
    }

    #[test]
    fn check_feedback_loop_skips_color_target_alias_like_upstream() {
        let mut cache = test_cache();
        let image_id = insert_test_image(&mut cache, 0x6000_0000);
        let sampled_view_id = insert_test_view(&mut cache, image_id);
        let color_view_id = insert_test_view(&mut cache, image_id);
        cache.render_targets.color_buffer_ids[0] = color_view_id;
        cache.rt_active_mask = 1;
        cache.rt_image_id[0] = image_id;
        cache.render_targets_serial = 1;

        let mut barriers = 0;
        cache.check_feedback_loop(
            &[ImageViewInOut {
                id: sampled_view_id,
                ..ImageViewInOut::default()
            }],
            || barriers += 1,
        );
        assert_eq!(barriers, 0);
    }

    #[test]
    fn check_feedback_loop_detects_depth_target_alias() {
        let mut cache = test_cache();
        let image_id = insert_test_image(&mut cache, 0x6100_0000);
        let sampled_view_id = insert_test_view(&mut cache, image_id);
        cache.render_targets.depth_buffer_id = insert_test_view(&mut cache, image_id);
        cache.rt_active_mask = 1 << NUM_RT;
        cache.rt_depth_image_id = image_id;
        cache.render_targets_serial = 1;

        let mut barriers = 0;
        cache.check_feedback_loop(
            &[ImageViewInOut {
                id: sampled_view_id,
                ..ImageViewInOut::default()
            }],
            || barriers += 1,
        );
        assert_eq!(barriers, 1);
    }

    #[test]
    fn check_feedback_loop_ignores_unrelated_and_null_views() {
        let mut cache = test_cache();
        let sampled_image_id = insert_test_image(&mut cache, 0x6200_0000);
        let target_image_id = insert_test_image(&mut cache, 0x6300_0000);
        let sampled_view_id = insert_test_view(&mut cache, sampled_image_id);
        cache.render_targets.color_buffer_ids[0] = insert_test_view(&mut cache, target_image_id);
        cache.rt_active_mask = 1;
        cache.rt_image_id[0] = target_image_id;
        cache.render_targets_serial = 1;

        let mut barriers = 0;
        cache.check_feedback_loop(
            &[
                ImageViewInOut {
                    id: NULL_IMAGE_VIEW_ID,
                    ..ImageViewInOut::default()
                },
                ImageViewInOut {
                    id: sampled_view_id,
                    ..ImageViewInOut::default()
                },
            ],
            || barriers += 1,
        );
        assert_eq!(barriers, 0);
    }

    #[test]
    fn check_feedback_loop_cache_is_invalidated_by_texture_binding_serial() {
        let mut cache = test_cache();
        let depth_image_id = insert_test_image(&mut cache, 0x6400_0000);
        let unrelated_image_id = insert_test_image(&mut cache, 0x6500_0000);
        let depth_alias = insert_test_view(&mut cache, depth_image_id);
        let unrelated = insert_test_view(&mut cache, unrelated_image_id);
        cache.render_targets.depth_buffer_id = insert_test_view(&mut cache, depth_image_id);
        cache.rt_active_mask = 1 << NUM_RT;
        cache.rt_depth_image_id = depth_image_id;
        cache.render_targets_serial = 1;

        let mut barriers = 0;
        cache.check_feedback_loop(
            &[ImageViewInOut {
                id: unrelated,
                ..ImageViewInOut::default()
            }],
            || barriers += 1,
        );
        cache.check_feedback_loop(
            &[ImageViewInOut {
                id: depth_alias,
                ..ImageViewInOut::default()
            }],
            || barriers += 1,
        );
        assert_eq!(barriers, 0);

        cache.texture_bindings_serial = 1;
        cache.check_feedback_loop(
            &[ImageViewInOut {
                id: depth_alias,
                ..ImageViewInOut::default()
            }],
            || barriers += 1,
        );
        cache.check_feedback_loop(
            &[ImageViewInOut {
                id: depth_alias,
                ..ImageViewInOut::default()
            }],
            || barriers += 1,
        );
        assert_eq!(barriers, 2);
    }
}

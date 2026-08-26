// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::texture_cache::image_view_info::SwizzleSource;
use std::sync::{Mutex, MutexGuard};

static SETTINGS_LOCK: Mutex<()> = Mutex::new(());

fn lock_astc_settings() -> MutexGuard<'static, ()> {
    SETTINGS_LOCK.lock().unwrap()
}

#[test]
fn constants() {
    assert_eq!(NUM_RT, 8);
    assert_eq!(NUM_TEXTURE_TYPES, 9);
}

#[test]
fn texture_cache_gl_owners_start_empty_without_a_context() {
    let mut image_base = ImageBase::new(
        ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 1,
                height: 1,
                depth: 1,
            },
            ..ImageInfo::default()
        },
        0,
        0,
    );
    let image = Image::new(&mut image_base);
    assert_eq!(image.texture.handle, 0);
    assert_eq!(image.upscaled_backup.handle, 0);
    assert_eq!(image.store_view.handle, 0);

    let sampler = Sampler::new();
    assert_eq!(sampler.handle(), 0);
    assert_eq!(sampler.handle_with_default_anisotropy(), 0);
    assert!(!sampler.has_added_anisotropy());

    let framebuffer = TextureCacheFramebuffer {
        framebuffer: OGLFramebuffer::new(),
        buffer_bits: gl::NONE,
    };
    assert_eq!(framebuffer.handle(), 0);
    assert_eq!(framebuffer.buffer_bits(), gl::NONE);
}

#[test]
fn image_scaling_reads_live_resolution_state() {
    use crate::renderer_opengl::gl_shader_manager::ProgramManager;
    use crate::renderer_opengl::gl_staging_buffer_pool::make_shared_staging_buffer_pool;

    let _lock = SETTINGS_LOCK.lock().unwrap();
    let previous = common::settings::values().resolution_info.clone();
    struct ResolutionRestore(common::settings::ResolutionScalingInfo);
    impl Drop for ResolutionRestore {
        fn drop(&mut self) {
            common::settings::values_mut().resolution_info = self.0.clone();
        }
    }
    let _restore = ResolutionRestore(previous);

    let program_manager = ProgramManager::new_shared_for_test();
    let mut state_tracker = StateTracker::new();
    let mut runtime = Box::new(TextureCacheRuntime::new_for_test(
        false,
        false,
        program_manager,
        &mut state_tracker,
        make_shared_staging_buffer_pool(),
    ));
    let mut base = ImageBase::new(
        ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 1,
                height: 1,
                depth: 1,
            },
            ..ImageInfo::default()
        },
        0,
        0,
    );
    let mut image = Image::new(&mut base);
    image.runtime = Some(NonNull::from(runtime.as_mut()));
    image.gl_format = gl::RGBA;
    image.gl_type = gl::UNSIGNED_BYTE;

    common::settings::values_mut().resolution_info.active = false;
    assert!(!image.scale_up(&mut base, true));

    common::settings::values_mut().resolution_info.active = true;
    assert!(image.scale_up(&mut base, true));
    assert!(base.flags.contains(ImageFlagBits::RESCALED));
}

#[test]
fn texture_cache_materializes_upstream_null_image_view_slot() {
    use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
    use crate::renderer_opengl::gl_shader_manager::ProgramManager;
    use crate::renderer_opengl::gl_staging_buffer_pool::make_shared_staging_buffer_pool;
    use std::sync::Arc;

    let program_manager = ProgramManager::new_shared_for_test();
    let mut state_tracker = StateTracker::new();
    let cache = TextureCache::new_with_caps(
        Arc::new(MaxwellDeviceMemoryManager::default()),
        false,
        false,
        false,
        false,
        program_manager,
        &mut state_tracker,
        make_shared_staging_buffer_pool(),
    );

    let null_view = cache
        .get_image_view(NULL_IMAGE_VIEW_ID)
        .expect("upstream reserves a constructed OpenGL null image view at slot zero");
    assert_eq!(null_view.views, [0; NUM_TEXTURE_TYPES as usize]);
    assert_eq!(null_view.default_handle(), 0);
}

#[test]
fn backend_image_identity_rejects_reused_slot_gpu_addr() {
    use crate::texture_cache::format_lookup_table::PixelFormat;
    use crate::texture_cache::image_info::ImageInfo;

    let mut base = ImageBase::new(
        ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            ..ImageInfo::default()
        },
        0x1000,
        0x2000,
    );
    let backend = Image::new(&mut base);

    assert!(backend.matches_base(&base));
    base.gpu_addr = 0x3000;
    assert!(backend.matches_base(&base));
    assert_eq!(backend.base().gpu_addr, 0x3000);
}

#[test]
fn framebuffer_attachment_mode_matches_upstream_attach_texture() {
    use common::slot_vector::SlotId;

    let image_id = SlotId { index: 1 };
    let mut image_info = ImageInfo {
        image_type: ImageType::E2D,
        ..ImageInfo::default()
    };
    let mut view_info = ImageViewInfo::default();
    let non_slice = ImageViewBase::new(&view_info, &image_info, image_id, 0x1000);
    assert_eq!(
        framebuffer_attachment_mode(&non_slice),
        FramebufferAttachmentMode::Texture
    );

    image_info.image_type = ImageType::E3D;
    view_info.view_type = ImageViewType::E2D;
    view_info.range.base.layer = 3;
    view_info.range.extent.layers = 1;
    let single_slice = ImageViewBase::new(&view_info, &image_info, image_id, 0x1000);
    assert_eq!(
        framebuffer_attachment_mode(&single_slice),
        FramebufferAttachmentMode::TextureLayer(3)
    );

    view_info.range.extent.layers = 2;
    let layered_slice = ImageViewBase::new(&view_info, &image_info, image_id, 0x1000);
    assert_eq!(
        framebuffer_attachment_mode(&layered_slice),
        FramebufferAttachmentMode::Texture
    );
}

#[test]
fn join_relations_keep_backend_base_addresses_stable() {
    use crate::texture_cache::format_lookup_table::PixelFormat;
    use crate::texture_cache::image_info::ImageInfo;
    use crate::texture_cache::texture_cache_base::ImageSlot;

    let mut cache = CommonTextureCache::<TextureCacheParams>::new_for_backend(std::sync::Arc::new(
        crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default(),
    ));
    let mut full = ImageInfo {
        format: PixelFormat::A8B8G8R8Unorm,
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
    let full_base = ImageBase::new(full.clone(), 0x5000, 0x9000);
    let mip_offset = full_base.mip_level_offsets[1] as u64;
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
        ..full
    };

    let full_id = cache.slot_images.insert(ImageSlot::pending(full_base));
    let sub_id = cache.slot_images.insert(ImageSlot::pending(ImageBase::new(
        sub,
        0x5000 + mip_offset,
        0x9000 + mip_offset,
    )));
    for image_id in [full_id, sub_id] {
        let backend = Image::new(cache.slot_images[image_id].base.as_mut());
        cache.slot_images[image_id].backend = Some(backend);
    }
    let full_ptr = cache.slot_images[full_id].base.as_ref() as *const ImageBase;
    let sub_ptr = cache.slot_images[sub_id].base.as_ref() as *const ImageBase;

    cache.apply_join_relations(sub_id, &[], &[full_id], &[]);

    assert_eq!(
        cache.slot_images[full_id].backend.as_ref().unwrap().base() as *const ImageBase,
        full_ptr,
    );
    assert_eq!(
        cache.slot_images[sub_id].backend.as_ref().unwrap().base() as *const ImageBase,
        sub_ptr,
    );
    assert!(!cache.slot_images[sub_id].aliased_images.is_empty());
}

#[test]
#[should_panic]
fn join_relations_reject_self_alias_before_creating_mutable_references() {
    use crate::texture_cache::image_info::ImageInfo;
    use crate::texture_cache::texture_cache_base::ImageSlot;

    let mut cache = CommonTextureCache::<TextureCacheParams>::new_for_backend(std::sync::Arc::new(
        crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default(),
    ));
    let image_id = cache.slot_images.insert(ImageSlot::pending(ImageBase::new(
        ImageInfo::default(),
        0x5000,
        0x9000,
    )));

    cache.apply_join_relations(image_id, &[image_id], &[], &[]);
}

#[test]
fn typed_image_view_keeps_inherited_base_address_across_slot_growth() {
    use crate::texture_cache::image_view_info::ImageViewInfo;
    use crate::texture_cache::texture_cache_base::ImageViewSlot;

    let mut cache = CommonTextureCache::<TextureCacheParams>::new_for_backend(std::sync::Arc::new(
        crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default(),
    ));
    let info = ImageInfo {
        format: PixelFormat::A8B8G8R8Unorm,
        image_type: ImageType::Buffer,
        size: Extent3D {
            width: 16,
            height: 1,
            depth: 1,
        },
        ..ImageInfo::default()
    };
    let view_info = ImageViewInfo {
        view_type: ImageViewType::Buffer,
        format: info.format,
        ..ImageViewInfo::default()
    };
    let view_id = cache.slot_image_views.insert(ImageViewSlot::pending(
        view_info,
        ImageViewBase::new_buffer(&info, &view_info, 0x1234_0000),
    ));
    let base = std::ptr::NonNull::from(cache.slot_image_views[view_id].base.as_mut());
    cache.slot_image_views[view_id].backend = Some(ImageView::from_buffer_base(
        base,
        &view_info,
        [0; NUM_TEXTURE_TYPES as usize],
    ));
    let expected = cache.slot_image_views[view_id].base.as_ref() as *const ImageViewBase;

    for index in 0..256u64 {
        cache.slot_image_views.insert(ImageViewSlot::pending(
            view_info,
            ImageViewBase::new_buffer(&info, &view_info, 0x2000_0000 + index * 0x1000),
        ));
    }

    assert_eq!(
        cache.slot_image_views[view_id]
            .backend
            .as_ref()
            .unwrap()
            .base() as *const ImageViewBase,
        expected,
    );
}

#[test]
fn texture_cache_params() {
    assert!(TextureCacheParams::ENABLE_VALIDATION);
    assert!(TextureCacheParams::FRAMEBUFFER_BLITS);
    assert!(TextureCacheParams::HAS_EMULATED_COPIES);
}

#[test]
fn buffer_image_view_materializes_upstream_buffer_size() {
    let info = ImageInfo {
        format: PixelFormat::A8B8G8R8Unorm,
        image_type: ImageType::Buffer,
        size: Extent3D {
            width: 17,
            height: 1,
            depth: 1,
        },
        ..ImageInfo::default()
    };
    let view_info = ImageViewInfo {
        view_type: ImageViewType::Buffer,
        format: PixelFormat::A8B8G8R8Unorm,
        ..ImageViewInfo::default()
    };
    let mut base = ImageViewBase::new_buffer(&info, &view_info, 0x1234_0000);
    let base_ptr = std::ptr::NonNull::from(&mut base);
    let view = ImageView::from_buffer_base(base_ptr, &view_info, [0; NUM_TEXTURE_TYPES as usize]);

    assert_eq!(view.pixel_format(), PixelFormat::A8B8G8R8Unorm);
    assert_eq!(
        view.buffer_size(),
        crate::texture_cache::util::calculate_guest_size_in_bytes(&info)
    );
    assert!(view.matches_buffer_base(&base));
}

#[test]
fn present_internal_format_matches_basic_surface_formats() {
    assert_eq!(
        present_internal_format(PixelFormat::A8B8G8R8Unorm),
        gl::RGBA8
    );
    assert_eq!(
        present_internal_format(PixelFormat::B8G8R8A8Unorm),
        gl::RGBA8
    );
    assert_eq!(
        present_internal_format(PixelFormat::A8B8G8R8Srgb),
        gl::SRGB8_ALPHA8
    );
    assert_eq!(
        present_internal_format(PixelFormat::R5G6B5Unorm),
        gl::RGB565
    );
}

#[test]
fn select_astc_format_follows_recompression_setting() {
    use common::settings_enums::AstcRecompression;

    let _lock = lock_astc_settings();

    struct AstcRecompressionRestore(AstcRecompression);

    impl Drop for AstcRecompressionRestore {
        fn drop(&mut self) {
            common::settings::values_mut()
                .astc_recompression
                .set_value(self.0);
        }
    }

    let previous = *common::settings::values().astc_recompression.get_value();
    let _restore = AstcRecompressionRestore(previous);

    common::settings::values_mut()
        .astc_recompression
        .set_value(AstcRecompression::Uncompressed);
    assert_eq!(
        select_astc_format(PixelFormat::Astc2d4x4Unorm, false),
        gl::RGBA8
    );
    assert_eq!(
        select_astc_format(PixelFormat::Astc2d4x4Srgb, true),
        gl::SRGB8_ALPHA8
    );

    common::settings::values_mut()
        .astc_recompression
        .set_value(AstcRecompression::Bc1);
    assert_eq!(
        select_astc_format(PixelFormat::Astc2d4x4Unorm, false),
        GL_COMPRESSED_RGBA_S3TC_DXT1_EXT
    );
    assert_eq!(
        select_astc_format(PixelFormat::Astc2d4x4Srgb, true),
        GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT1_EXT
    );

    common::settings::values_mut()
        .astc_recompression
        .set_value(AstcRecompression::Bc3);
    assert_eq!(
        select_astc_format(PixelFormat::Astc2d4x4Unorm, false),
        GL_COMPRESSED_RGBA_S3TC_DXT5_EXT
    );
    assert_eq!(
        select_astc_format(PixelFormat::Astc2d4x4Srgb, true),
        GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT5_EXT
    );
}

#[test]
fn astc_upload_flags_follow_upstream_policy() {
    use crate::renderer_opengl::gl_shader_manager::ProgramManager;
    use crate::renderer_opengl::gl_staging_buffer_pool::make_shared_staging_buffer_pool;
    use common::settings_enums::{AstcDecodeMode, AstcRecompression};

    let _lock = lock_astc_settings();

    let mut state_tracker = StateTracker::new();
    let runtime_without_astc = TextureCacheRuntime::new_for_test(
        false,
        false,
        ProgramManager::new_shared_for_test(),
        &mut state_tracker,
        make_shared_staging_buffer_pool(),
    );
    let runtime_with_astc = TextureCacheRuntime::new_for_test(
        false,
        true,
        ProgramManager::new_shared_for_test(),
        &mut state_tracker,
        make_shared_staging_buffer_pool(),
    );

    struct AstcSettingsRestore {
        decode: AstcDecodeMode,
        recompression: AstcRecompression,
    }

    impl Drop for AstcSettingsRestore {
        fn drop(&mut self) {
            let mut values = common::settings::values_mut();
            values.accelerate_astc.set_value(self.decode);
            values.astc_recompression.set_value(self.recompression);
        }
    }

    let _restore = AstcSettingsRestore {
        decode: *common::settings::values().accelerate_astc.get_value(),
        recompression: *common::settings::values().astc_recompression.get_value(),
    };
    let info = ImageInfo {
        format: PixelFormat::Astc2d4x4Unorm,
        image_type: ImageType::E2D,
        size: crate::texture_cache::types::Extent3D {
            width: 64,
            height: 64,
            depth: 1,
        },
        ..ImageInfo::default()
    };

    {
        let mut values = common::settings::values_mut();
        values
            .accelerate_astc
            .set_value(AstcDecodeMode::CpuAsynchronous);
        values
            .astc_recompression
            .set_value(AstcRecompression::Uncompressed);
    }
    assert!(can_be_decoded_async(&runtime_without_astc, &info));
    assert!(!can_be_accelerated(&runtime_without_astc, &info));
    let mut async_image = ImageBase::new(info.clone(), 0, 0);
    TextureCache::apply_backend_image_flags_for_test(&mut async_image, &runtime_without_astc);
    assert!(async_image
        .flags
        .contains(ImageFlagBits::ASYNCHRONOUS_DECODE));
    assert!(!async_image
        .flags
        .contains(ImageFlagBits::ACCELERATED_UPLOAD));

    {
        let mut values = common::settings::values_mut();
        values.accelerate_astc.set_value(AstcDecodeMode::Gpu);
        values
            .astc_recompression
            .set_value(AstcRecompression::Uncompressed);
    }
    assert!(!can_be_decoded_async(&runtime_without_astc, &info));
    assert!(can_be_accelerated(&runtime_without_astc, &info));
    let mut accelerated_image = ImageBase::new(info.clone(), 0, 0);
    TextureCache::apply_backend_image_flags_for_test(&mut accelerated_image, &runtime_without_astc);
    assert!(!accelerated_image
        .flags
        .contains(ImageFlagBits::ASYNCHRONOUS_DECODE));
    assert!(accelerated_image
        .flags
        .contains(ImageFlagBits::ACCELERATED_UPLOAD));

    common::settings::values_mut()
        .astc_recompression
        .set_value(AstcRecompression::Bc1);
    assert!(!can_be_accelerated(&runtime_without_astc, &info));
    assert!(!can_be_decoded_async(&runtime_with_astc, &info));
    assert!(!can_be_accelerated(&runtime_with_astc, &info));
    let mut native_image = ImageBase::new(info, 0, 0);
    TextureCache::apply_backend_image_flags_for_test(&mut native_image, &runtime_with_astc);
    assert!(!native_image
        .flags
        .contains(ImageFlagBits::ASYNCHRONOUS_DECODE));
    assert!(!native_image
        .flags
        .contains(ImageFlagBits::ACCELERATED_UPLOAD));
}

#[test]
fn decode_swizzle_matches_upstream_sources() {
    assert_eq!(
        decode_swizzle([
            SwizzleSource::R as u8,
            SwizzleSource::R as u8,
            SwizzleSource::R as u8,
            SwizzleSource::OneFloat as u8,
        ]),
        Some([
            SwizzleSource::R,
            SwizzleSource::R,
            SwizzleSource::R,
            SwizzleSource::OneFloat,
        ])
    );
    assert_eq!(
        decode_swizzle([1, 2, 3, 4]),
        Some([
            SwizzleSource::Invalid,
            SwizzleSource::R,
            SwizzleSource::G,
            SwizzleSource::B,
        ])
    );
    assert_eq!(decode_swizzle([u8::MAX; 4]), None);
}

#[test]
fn depth_stencil_texture_mode_matches_upstream_r_g_selection() {
    assert_eq!(
        depth_stencil_texture_mode(
            PixelFormat::D24UnormS8Uint,
            [
                SwizzleSource::R,
                SwizzleSource::G,
                SwizzleSource::B,
                SwizzleSource::A,
            ],
        ),
        gl::DEPTH_COMPONENT
    );
    assert_eq!(
        depth_stencil_texture_mode(
            PixelFormat::S8UintD24Unorm,
            [
                SwizzleSource::G,
                SwizzleSource::G,
                SwizzleSource::B,
                SwizzleSource::A,
            ],
        ),
        gl::DEPTH_COMPONENT
    );
}

#[test]
fn depth_stencil_texture_mode_continues_after_upstream_fail_soft_report() {
    assert_eq!(
        depth_stencil_texture_mode(
            PixelFormat::D24UnormS8Uint,
            [
                SwizzleSource::B,
                SwizzleSource::G,
                SwizzleSource::R,
                SwizzleSource::A,
            ],
        ),
        gl::DEPTH_COMPONENT
    );
}

#[test]
fn image_view_parent_guard_rejects_stale_backend_view() {
    use crate::texture_cache::format_lookup_table::PixelFormat;
    use crate::texture_cache::image_info::ImageInfo;
    use crate::texture_cache::image_view_base::ImageViewBase;
    use crate::texture_cache::image_view_info::ImageViewInfo;
    use crate::texture_cache::types::ImageType;
    use common::slot_vector::SlotId;

    let image_id = SlotId { index: 42 };
    let image_info = ImageInfo {
        format: PixelFormat::A8B8G8R8Unorm,
        image_type: ImageType::E2D,
        ..ImageInfo::default()
    };
    let view_info = ImageViewInfo {
        format: PixelFormat::A8B8G8R8Unorm,
        ..ImageViewInfo::default()
    };
    let mut base = ImageViewBase::new(&view_info, &image_info, image_id, 0x1000);
    let mut image_base = ImageBase::new(image_info.clone(), 0x1000, 0x2000);
    let mut image = Image::new(&mut image_base);
    image.current_texture = 7;

    let mut view = ImageView::new(&mut base);
    view.original_texture = 7;
    view.full_range = base.range;
    assert!(view.matches_base_image(&base, &image));

    let mut different_view_info = view_info;
    different_view_info.range.base.level = 1;
    let different_base = ImageViewBase::new(&different_view_info, &image_info, image_id, 0x1000);
    assert!(!view.matches_base_image(&different_base, &image));

    let mut different_view_info = view_info;
    different_view_info.x_source = SwizzleSource::B as u8;
    let different_base = ImageViewBase::new(&different_view_info, &image_info, image_id, 0x1000);
    assert!(!view.matches_base_image(&different_base, &image));

    image.current_texture = 8;
    assert!(!view.matches_base_image(&base, &image));
}

#[test]
fn image_view_parent_guard_accepts_slice_effective_full_range() {
    use crate::texture_cache::format_lookup_table::PixelFormat;
    use crate::texture_cache::image_info::ImageInfo;
    use crate::texture_cache::image_view_base::{ImageViewBase, ImageViewFlagBits};
    use crate::texture_cache::image_view_info::ImageViewInfo;
    use crate::texture_cache::types::{ImageType, ImageViewType};
    use common::slot_vector::SlotId;

    let image_id = SlotId { index: 42 };
    let image_info = ImageInfo {
        format: PixelFormat::A8B8G8R8Unorm,
        image_type: ImageType::E3D,
        size: Extent3D {
            width: 64,
            height: 64,
            depth: 4,
        },
        ..ImageInfo::default()
    };
    let view_info = ImageViewInfo {
        format: PixelFormat::A8B8G8R8Unorm,
        view_type: ImageViewType::E2D,
        range: SubresourceRange {
            base: crate::texture_cache::types::SubresourceBase { level: 0, layer: 2 },
            extent: crate::texture_cache::types::SubresourceExtent {
                levels: 1,
                layers: 1,
            },
        },
        ..ImageViewInfo::default()
    };
    let mut base = ImageViewBase::new(&view_info, &image_info, image_id, 0x1000);
    assert!(base.flags.contains(ImageViewFlagBits::SLICE));

    let mut image_base = ImageBase::new(image_info.clone(), 0x1000, 0x2000);
    let mut image = Image::new(&mut image_base);
    image.current_texture = 7;

    let mut view = ImageView::new(&mut base);
    view.original_texture = 7;
    view.full_range = ImageView::effective_full_range(&base);
    assert!(view.matches_base_image(&base, &image));
    assert_eq!(view.full_range.base.layer, 0);
    assert_eq!(view.full_range.extent.layers, 1);
}

#[test]
fn image_view_info_render_target_sentinel_is_preserved_outside_image_view_base() {
    use crate::texture_cache::format_lookup_table::PixelFormat;
    use crate::texture_cache::image_info::ImageInfo;
    use crate::texture_cache::image_view_base::ImageViewBase;
    use crate::texture_cache::image_view_info::ImageViewInfo;
    use crate::texture_cache::types::{ImageType, ImageViewType, SubresourceRange};
    use common::slot_vector::SlotId;

    let image_id = SlotId { index: 42 };
    let image_info = ImageInfo {
        format: PixelFormat::A8B8G8R8Unorm,
        image_type: ImageType::E2D,
        size: Extent3D {
            width: 64,
            height: 64,
            depth: 1,
        },
        ..ImageInfo::default()
    };
    let view_info = ImageViewInfo::for_render_target(
        ImageViewType::E2D,
        PixelFormat::A8B8G8R8Unorm,
        SubresourceRange::default(),
    );
    let base = ImageViewBase::new(&view_info, &image_info, image_id, 0x1000);
    let slot =
        crate::texture_cache::texture_cache_base::ImageViewSlot::<()>::pending(view_info, base);
    assert!(slot.info.is_render_target());
}

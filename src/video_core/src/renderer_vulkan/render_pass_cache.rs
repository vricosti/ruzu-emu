// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! VkRenderPass cache keyed by render target format configuration.
//!
//! Ref: zuyu `vk_render_pass_cache.h` — caches VkRenderPass objects to avoid
//! redundant creation for identical render target configurations.

use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::Mutex;

use ash::vk;
use log::debug;

use super::maxwell_to_vk;
use crate::surface::{PixelFormat, SurfaceType};
use crate::vulkan_common::vulkan_device::{Device, FormatType};

/// Port of the anonymous-namespace `GetSurfaceType` in
/// `vk_render_pass_cache.cpp`.
const fn get_surface_type(format: PixelFormat) -> SurfaceType {
    match format {
        PixelFormat::D16Unorm | PixelFormat::D32Float | PixelFormat::X8D24Unorm => {
            SurfaceType::Depth
        }
        PixelFormat::S8Uint => SurfaceType::Stencil,
        PixelFormat::D24UnormS8Uint | PixelFormat::S8UintD24Unorm | PixelFormat::D32FloatS8Uint => {
            SurfaceType::DepthStencil
        }
        _ => SurfaceType::ColorTexture,
    }
}

fn attachment_stencil_ops(
    pixel_format: PixelFormat,
    load_op: vk::AttachmentLoadOp,
    store_op: vk::AttachmentStoreOp,
) -> (vk::AttachmentLoadOp, vk::AttachmentStoreOp) {
    if matches!(
        get_surface_type(pixel_format),
        SurfaceType::Stencil | SurfaceType::DepthStencil
    ) {
        (load_op, store_op)
    } else {
        (
            vk::AttachmentLoadOp::DONT_CARE,
            vk::AttachmentStoreOp::DONT_CARE,
        )
    }
}

fn color_attachment_ops(
    key: &RenderPassKey,
    index: usize,
) -> (vk::AttachmentLoadOp, vk::AttachmentStoreOp) {
    let load_op = if key.color_clear_mask & (1 << index) != 0 {
        vk::AttachmentLoadOp::CLEAR
    } else {
        vk::AttachmentLoadOp::LOAD
    };
    let store_op = if key.color_discard_mask & (1 << index) != 0 {
        vk::AttachmentStoreOp::DONT_CARE
    } else {
        vk::AttachmentStoreOp::STORE
    };
    (load_op, store_op)
}

/// Port of upstream `RenderPassKey`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderPassKey {
    pub color_formats: [PixelFormat; 8],
    pub depth_format: PixelFormat,
    pub samples: vk::SampleCountFlags,
    pub resolve_color: bool,
    pub color_clear_mask: u32,
    pub depth_stencil_clear: bool,
    pub color_discard_mask: u32,
}

impl Default for RenderPassKey {
    fn default() -> Self {
        Self {
            color_formats: [PixelFormat::Invalid; 8],
            depth_format: PixelFormat::Invalid,
            samples: vk::SampleCountFlags::TYPE_1,
            resolve_color: false,
            color_clear_mask: 0,
            depth_stencil_clear: false,
            color_discard_mask: 0,
        }
    }
}

/// Caches VkRenderPass objects by render target configuration.
///
/// Ref: zuyu RenderPassCache — avoids re-creating VkRenderPass objects when
/// the render target format configuration hasn't changed.
pub struct RenderPassCache {
    device: NonNull<Device>,
    cache: Mutex<HashMap<RenderPassKey, Option<vk::RenderPass>>>,
}

// SAFETY: the pointed-to `Device` is boxed by `RendererVulkan` and outlives
// the rasterizer and this cache. Vulkan device operations are externally
// synchronized where required; the render-pass map itself is mutex-protected.
unsafe impl Send for RenderPassCache {}
unsafe impl Sync for RenderPassCache {}

impl RenderPassCache {
    pub fn new(device: &Device) -> Self {
        Self {
            device: NonNull::from(device),
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn device(&self) -> &Device {
        // SAFETY: `RendererVulkan` owns stable boxed storage for `Device` and
        // drops the rasterizer (and this cache) before that owner.
        unsafe { self.device.as_ref() }
    }

    /// Get or create a VkRenderPass for the given key.
    pub fn get(&self, key: &RenderPassKey) -> Result<vk::RenderPass, vk::Result> {
        let mut cache = self.cache.lock().expect("render-pass cache mutex poisoned");
        if let Some(render_pass) = cache.get(key) {
            return Ok(render_pass.unwrap_or(vk::RenderPass::null()));
        }

        cache.insert(key.clone(), None);
        let render_pass = self.create_render_pass(key)?;
        cache.insert(key.clone(), Some(render_pass));
        debug!(
            "RenderPassCache: created new render pass (depth={:?})",
            key.depth_format,
        );
        Ok(render_pass)
    }

    /// Port of the anonymous-namespace `AttachmentDescription` helper in
    /// `vk_render_pass_cache.cpp`.
    fn attachment_description(
        &self,
        pixel_format: PixelFormat,
        samples: vk::SampleCountFlags,
        load_op: vk::AttachmentLoadOp,
        store_op: vk::AttachmentStoreOp,
    ) -> vk::AttachmentDescription {
        let (stencil_load_op, stencil_store_op) =
            attachment_stencil_ops(pixel_format, load_op, store_op);
        vk::AttachmentDescription::builder()
            .format(
                maxwell_to_vk::surface_format(
                    self.device(),
                    FormatType::Optimal,
                    true,
                    pixel_format,
                )
                .format,
            )
            .samples(samples)
            .load_op(load_op)
            .store_op(store_op)
            .stencil_load_op(stencil_load_op)
            .stencil_store_op(stencil_store_op)
            .initial_layout(vk::ImageLayout::GENERAL)
            .final_layout(vk::ImageLayout::GENERAL)
            .build()
    }

    fn create_render_pass(&self, key: &RenderPassKey) -> Result<vk::RenderPass, vk::Result> {
        let mut attachments = Vec::new();
        let mut color_refs = Vec::new();
        let mut num_attachments = 0usize;
        let mut num_colors = 0u32;

        // Color attachments. Upstream keeps the original RT slot indices in
        // pColorAttachments and uses VK_ATTACHMENT_UNUSED for holes; only the
        // VkFramebuffer attachment array is compacted to the actually-bound
        // views. Do not compact these references or Location(N) fragment
        // outputs target the wrong attachment.
        for i in 0..key.color_formats.len() {
            let pixel_format = key.color_formats[i];
            if pixel_format == PixelFormat::Invalid {
                color_refs.push(vk::AttachmentReference {
                    attachment: vk::ATTACHMENT_UNUSED,
                    layout: vk::ImageLayout::GENERAL,
                });
                continue;
            }
            color_refs.push(vk::AttachmentReference {
                attachment: num_colors,
                layout: vk::ImageLayout::GENERAL,
            });
            num_attachments = i + 1;
            num_colors += 1;
            let (load_op, store_op) = color_attachment_ops(key, i);
            attachments.push(self.attachment_description(
                pixel_format,
                key.samples,
                load_op,
                store_op,
            ));
        }

        // Depth attachment
        let depth_ref;
        let has_depth = key.depth_format != PixelFormat::Invalid;
        if has_depth {
            depth_ref = Some(vk::AttachmentReference {
                attachment: num_colors,
                layout: vk::ImageLayout::GENERAL,
            });
            let load_op = if key.depth_stencil_clear {
                vk::AttachmentLoadOp::CLEAR
            } else {
                vk::AttachmentLoadOp::LOAD
            };
            attachments.push(self.attachment_description(
                key.depth_format,
                key.samples,
                load_op,
                vk::AttachmentStoreOp::STORE,
            ));
        } else {
            depth_ref = None;
        }

        let do_resolve_color =
            key.resolve_color && key.samples != vk::SampleCountFlags::TYPE_1 && num_colors > 0;
        let mut resolve_refs = Vec::new();
        if do_resolve_color {
            for &pixel_format in &key.color_formats {
                if pixel_format == PixelFormat::Invalid {
                    resolve_refs.push(vk::AttachmentReference {
                        attachment: vk::ATTACHMENT_UNUSED,
                        layout: vk::ImageLayout::GENERAL,
                    });
                    continue;
                }
                resolve_refs.push(vk::AttachmentReference {
                    attachment: attachments.len() as u32,
                    layout: vk::ImageLayout::GENERAL,
                });
                let mut description = self.attachment_description(
                    pixel_format,
                    vk::SampleCountFlags::TYPE_1,
                    vk::AttachmentLoadOp::DONT_CARE,
                    vk::AttachmentStoreOp::STORE,
                );
                description.initial_layout = vk::ImageLayout::UNDEFINED;
                attachments.push(description);
            }
        }

        let mut subpass = vk::SubpassDescription::builder()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_refs[..num_attachments]);
        if do_resolve_color {
            subpass = subpass.resolve_attachments(&resolve_refs[..num_attachments]);
        }
        if let Some(ref dr) = depth_ref {
            subpass = subpass.depth_stencil_attachment(dr);
        }
        let subpass = subpass.build();

        // Upstream permits attachment writes to become fragment-shader reads
        // within the same render pass (feedback-loop handling). Keep the
        // dependency by-region so synchronization is limited to overlapping
        // framebuffer regions.
        let dependency = vk::SubpassDependency::builder()
            .src_subpass(0)
            .dst_subpass(0)
            .src_stage_mask(
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                    | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
                    | vk::PipelineStageFlags::LATE_FRAGMENT_TESTS,
            )
            .dst_stage_mask(vk::PipelineStageFlags::FRAGMENT_SHADER)
            .src_access_mask(
                vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                    | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
            )
            .dst_access_mask(vk::AccessFlags::SHADER_READ)
            .dependency_flags(vk::DependencyFlags::BY_REGION)
            .build();

        let render_pass_info = vk::RenderPassCreateInfo::builder()
            .attachments(&attachments)
            .subpasses(std::slice::from_ref(&subpass))
            .dependencies(std::slice::from_ref(&dependency))
            .build();

        unsafe {
            self.device()
                .get_logical()
                .create_render_pass(&render_pass_info, None)
        }
    }
}

impl Drop for RenderPassCache {
    fn drop(&mut self) {
        let device = self.device().get_logical().clone();
        let cache = self
            .cache
            .get_mut()
            .expect("render-pass cache mutex poisoned");
        for (_, render_pass) in cache.drain() {
            if let Some(render_pass) = render_pass {
                unsafe {
                    device.destroy_render_pass(render_pass, None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::mem::ManuallyDrop;

    use super::*;

    #[test]
    fn test_render_pass_key_default() {
        let key = RenderPassKey::default();
        assert!(key
            .color_formats
            .iter()
            .all(|&format| format == PixelFormat::Invalid));
        assert_eq!(key.depth_format, PixelFormat::Invalid);
        assert_eq!(key.samples, vk::SampleCountFlags::TYPE_1);
        assert!(!key.resolve_color);
        assert_eq!(key.color_clear_mask, 0);
        assert!(!key.depth_stencil_clear);
        assert_eq!(key.color_discard_mask, 0);
    }

    #[test]
    fn failed_render_pass_entry_is_returned_as_null_without_retrying_creation() {
        let key = RenderPassKey::default();
        let cache = ManuallyDrop::new(RenderPassCache {
            device: NonNull::dangling(),
            cache: Mutex::new(HashMap::from([(key.clone(), None)])),
        });

        assert_eq!(cache.get(&key), Ok(vk::RenderPass::null()));
    }

    #[test]
    fn attachment_stencil_ops_match_surface_type() {
        assert_eq!(
            attachment_stencil_ops(
                PixelFormat::A8B8G8R8Unorm,
                vk::AttachmentLoadOp::LOAD,
                vk::AttachmentStoreOp::STORE,
            ),
            (
                vk::AttachmentLoadOp::DONT_CARE,
                vk::AttachmentStoreOp::DONT_CARE,
            )
        );
        assert_eq!(
            attachment_stencil_ops(
                PixelFormat::D24UnormS8Uint,
                vk::AttachmentLoadOp::CLEAR,
                vk::AttachmentStoreOp::STORE,
            ),
            (vk::AttachmentLoadOp::CLEAR, vk::AttachmentStoreOp::STORE,)
        );
    }

    #[test]
    fn test_render_pass_key_equality() {
        let mut a = RenderPassKey::default();
        let mut b = RenderPassKey::default();
        a.color_formats[0] = PixelFormat::A8B8G8R8Unorm;
        b.color_formats[0] = PixelFormat::A8B8G8R8Unorm;
        assert_eq!(a, b);
    }

    #[test]
    fn test_render_pass_key_different_format() {
        let mut a = RenderPassKey::default();
        let mut b = RenderPassKey::default();
        a.color_formats[0] = PixelFormat::A8B8G8R8Unorm;
        b.color_formats[0] = PixelFormat::B8G8R8A8Unorm;
        assert_ne!(a, b);
    }

    #[test]
    fn invalid_surface_type_follows_render_pass_local_color_fallback() {
        assert_eq!(
            get_surface_type(PixelFormat::Invalid),
            SurfaceType::ColorTexture
        );
    }

    #[test]
    fn render_pass_variants_select_clear_and_discard_ops_per_rt_slot() {
        let mut key = RenderPassKey::default();
        key.color_clear_mask = 1 << 3;
        key.color_discard_mask = 1 << 5;

        assert_eq!(
            color_attachment_ops(&key, 3),
            (vk::AttachmentLoadOp::CLEAR, vk::AttachmentStoreOp::STORE)
        );
        assert_eq!(
            color_attachment_ops(&key, 5),
            (vk::AttachmentLoadOp::LOAD, vk::AttachmentStoreOp::DONT_CARE)
        );
        assert_eq!(
            color_attachment_ops(&key, 0),
            (vk::AttachmentLoadOp::LOAD, vk::AttachmentStoreOp::STORE)
        );
    }
}

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
use smallvec::SmallVec;

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

/// Upstream `ResolveAspects` (anonymous namespace).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolveAspects {
    depth: bool,
    stencil: bool,
}

/// Upstream `ResolveModes` (anonymous namespace).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolveModes {
    depth: vk::ResolveModeFlags,
    stencil: vk::ResolveModeFlags,
}

/// Port of the anonymous-namespace `GetResolveAspects`.
const fn get_resolve_aspects(format: PixelFormat) -> ResolveAspects {
    let surface_type = get_surface_type(format);
    ResolveAspects {
        depth: matches!(surface_type, SurfaceType::Depth | SurfaceType::DepthStencil),
        stencil: matches!(
            surface_type,
            SurfaceType::Stencil | SurfaceType::DepthStencil
        ),
    }
}

/// Device-independent core of the anonymous-namespace `PickResolveModes`.
fn pick_resolve_modes_with(
    depth_resolve_modes: vk::ResolveModeFlags,
    stencil_resolve_modes: vk::ResolveModeFlags,
    independent_resolve_none: bool,
    format: PixelFormat,
) -> ResolveModes {
    const MODE: vk::ResolveModeFlags = vk::ResolveModeFlags::SAMPLE_ZERO;

    let aspects = get_resolve_aspects(format);
    let depth_mode_supported = depth_resolve_modes.contains(MODE);
    let stencil_mode_supported = stencil_resolve_modes.contains(MODE);

    let mut modes = ResolveModes {
        depth: vk::ResolveModeFlags::NONE,
        stencil: vk::ResolveModeFlags::NONE,
    };
    if aspects.depth && depth_mode_supported {
        modes.depth = MODE;
    }
    if aspects.stencil && stencil_mode_supported {
        modes.stencil = MODE;
    }
    if modes.depth == modes.stencil || independent_resolve_none {
        return modes;
    }
    if modes.depth != vk::ResolveModeFlags::NONE && stencil_mode_supported {
        modes.stencil = MODE;
    } else if modes.stencil != vk::ResolveModeFlags::NONE && depth_mode_supported {
        modes.depth = MODE;
    }
    modes
}

/// Port of the anonymous-namespace `PickResolveModes`.
fn pick_resolve_modes(device: &Device, format: PixelFormat) -> ResolveModes {
    pick_resolve_modes_with(
        device.get_depth_resolve_modes(),
        device.get_stencil_resolve_modes(),
        device.supports_independent_resolve_none(),
        format,
    )
}

/// Device-independent core of `SupportsDepthStencilResolve`.
fn supports_depth_stencil_resolve_with(
    khr_depth_stencil_resolve_supported: bool,
    depth_resolve_modes: vk::ResolveModeFlags,
    stencil_resolve_modes: vk::ResolveModeFlags,
    independent_resolve_none: bool,
    depth_format: PixelFormat,
) -> bool {
    if depth_format == PixelFormat::Invalid || !khr_depth_stencil_resolve_supported {
        return false;
    }
    let aspects = get_resolve_aspects(depth_format);
    if !aspects.depth && !aspects.stencil {
        return false;
    }
    let modes = pick_resolve_modes_with(
        depth_resolve_modes,
        stencil_resolve_modes,
        independent_resolve_none,
        depth_format,
    );
    if (aspects.depth && modes.depth == vk::ResolveModeFlags::NONE)
        || (aspects.stencil && modes.stencil == vk::ResolveModeFlags::NONE)
    {
        return false;
    }
    modes.depth == modes.stencil || independent_resolve_none
}

/// Port of `Vulkan::SupportsDepthStencilResolve`: whether a render pass can
/// resolve `depth_format` through `VK_KHR_depth_stencil_resolve`.
pub fn supports_depth_stencil_resolve(device: &Device, depth_format: PixelFormat) -> bool {
    supports_depth_stencil_resolve_with(
        device.is_khr_depth_stencil_resolve_supported(),
        device.get_depth_resolve_modes(),
        device.get_stencil_resolve_modes(),
        device.supports_independent_resolve_none(),
        depth_format,
    )
}

/// Port of upstream `RenderPassKey`.
///
/// Upstream hashes the key manually (`std::hash<RenderPassKey>`); the Rust
/// map derives `Hash` over the same fields.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderPassKey {
    pub color_formats: [PixelFormat; 8],
    pub depth_format: PixelFormat,
    pub samples: vk::SampleCountFlags,
    pub resolve_color: bool,
    pub resolve_depth_stencil: bool,
    pub color_clear_mask: u32,
    pub depth_stencil_clear: bool,
    pub color_discard_mask: u32,
    pub depth_stencil_discard: bool,
}

impl Default for RenderPassKey {
    fn default() -> Self {
        Self {
            color_formats: [PixelFormat::Invalid; 8],
            depth_format: PixelFormat::Invalid,
            samples: vk::SampleCountFlags::TYPE_1,
            resolve_color: false,
            resolve_depth_stencil: false,
            color_clear_mask: 0,
            depth_stencil_clear: false,
            color_discard_mask: 0,
            depth_stencil_discard: false,
        }
    }
}

/// Upstream `MAX_ATTACHMENTS`: colors + color resolves + depth + depth resolve.
const MAX_ATTACHMENTS: usize = 2 * 8 + 2;

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
        let mut attachments = SmallVec::<[vk::AttachmentDescription; MAX_ATTACHMENTS]>::new();
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
            let store_op = if key.depth_stencil_discard {
                vk::AttachmentStoreOp::DONT_CARE
            } else {
                vk::AttachmentStoreOp::STORE
            };
            attachments.push(self.attachment_description(
                key.depth_format,
                key.samples,
                load_op,
                store_op,
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

        let do_resolve_depth_stencil = key.resolve_depth_stencil
            && has_depth
            && key.samples != vk::SampleCountFlags::TYPE_1
            && supports_depth_stencil_resolve(self.device(), key.depth_format);
        let mut depth_resolve_reference = vk::AttachmentReference::default();
        if do_resolve_depth_stencil {
            depth_resolve_reference = vk::AttachmentReference {
                attachment: attachments.len() as u32,
                layout: vk::ImageLayout::GENERAL,
            };
            let mut resolve_desc = self.attachment_description(
                key.depth_format,
                vk::SampleCountFlags::TYPE_1,
                vk::AttachmentLoadOp::DONT_CARE,
                vk::AttachmentStoreOp::STORE,
            );
            resolve_desc.initial_layout = vk::ImageLayout::UNDEFINED;
            attachments.push(resolve_desc);
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

        if self.device().is_khr_create_render_pass2_supported() {
            let descriptions2: SmallVec<[vk::AttachmentDescription2; MAX_ATTACHMENTS]> =
                attachments
                    .iter()
                    .map(|description| {
                        vk::AttachmentDescription2::builder()
                            .flags(description.flags)
                            .format(description.format)
                            .samples(description.samples)
                            .load_op(description.load_op)
                            .store_op(description.store_op)
                            .stencil_load_op(description.stencil_load_op)
                            .stencil_store_op(description.stencil_store_op)
                            .initial_layout(description.initial_layout)
                            .final_layout(description.final_layout)
                            .build()
                    })
                    .collect();
            let promote = |reference: &vk::AttachmentReference| {
                vk::AttachmentReference2::builder()
                    .attachment(reference.attachment)
                    .layout(reference.layout)
                    .aspect_mask(vk::ImageAspectFlags::empty())
                    .build()
            };
            let mut references2 = [vk::AttachmentReference2::default(); 8];
            let mut resolve_references2 = [vk::AttachmentReference2::default(); 8];
            for index in 0..8 {
                references2[index] =
                    promote(color_refs.get(index).unwrap_or(&vk::AttachmentReference {
                        attachment: vk::ATTACHMENT_UNUSED,
                        layout: vk::ImageLayout::GENERAL,
                    }));
                resolve_references2[index] =
                    promote(resolve_refs.get(index).unwrap_or(&vk::AttachmentReference {
                        attachment: vk::ATTACHMENT_UNUSED,
                        layout: vk::ImageLayout::GENERAL,
                    }));
            }
            let depth_reference2 =
                promote(depth_ref.as_ref().unwrap_or(&vk::AttachmentReference {
                    attachment: vk::ATTACHMENT_UNUSED,
                    layout: vk::ImageLayout::GENERAL,
                }));
            let depth_resolve_reference2 = promote(&depth_resolve_reference);
            let resolve_modes = pick_resolve_modes(self.device(), key.depth_format);
            let mut depth_stencil_resolve = vk::SubpassDescriptionDepthStencilResolve::builder()
                .depth_resolve_mode(resolve_modes.depth)
                .stencil_resolve_mode(resolve_modes.stencil)
                .depth_stencil_resolve_attachment(&depth_resolve_reference2)
                .build();
            let mut subpass2 = vk::SubpassDescription2::builder()
                .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
                .view_mask(0)
                .color_attachments(&references2[..num_attachments]);
            if do_resolve_color {
                subpass2 = subpass2.resolve_attachments(&resolve_references2[..num_attachments]);
            }
            if has_depth {
                subpass2 = subpass2.depth_stencil_attachment(&depth_reference2);
            }
            if do_resolve_depth_stencil {
                subpass2 = subpass2.push_next(&mut depth_stencil_resolve);
            }
            let subpass2 = subpass2.build();
            let dependency2 = vk::SubpassDependency2::builder()
                .src_subpass(dependency.src_subpass)
                .dst_subpass(dependency.dst_subpass)
                .src_stage_mask(dependency.src_stage_mask)
                .dst_stage_mask(dependency.dst_stage_mask)
                .src_access_mask(dependency.src_access_mask)
                .dst_access_mask(dependency.dst_access_mask)
                .dependency_flags(dependency.dependency_flags)
                .view_offset(0)
                .build();
            let render_pass_info = vk::RenderPassCreateInfo2::builder()
                .attachments(&descriptions2)
                .subpasses(std::slice::from_ref(&subpass2))
                .dependencies(std::slice::from_ref(&dependency2))
                .build();
            return self.device().create_render_pass2(&render_pass_info);
        }

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
        assert!(!key.resolve_depth_stencil);
        assert_eq!(key.color_clear_mask, 0);
        assert!(!key.depth_stencil_clear);
        assert_eq!(key.color_discard_mask, 0);
        assert!(!key.depth_stencil_discard);
    }

    #[test]
    fn resolve_aspects_follow_render_pass_surface_types() {
        assert_eq!(
            get_resolve_aspects(PixelFormat::D32Float),
            ResolveAspects {
                depth: true,
                stencil: false
            }
        );
        assert_eq!(
            get_resolve_aspects(PixelFormat::S8Uint),
            ResolveAspects {
                depth: false,
                stencil: true
            }
        );
        assert_eq!(
            get_resolve_aspects(PixelFormat::D24UnormS8Uint),
            ResolveAspects {
                depth: true,
                stencil: true
            }
        );
        assert_eq!(
            get_resolve_aspects(PixelFormat::A8B8G8R8Unorm),
            ResolveAspects {
                depth: false,
                stencil: false
            }
        );
    }

    #[test]
    fn pick_resolve_modes_uses_sample_zero_and_pairs_aspects_without_independent_none() {
        let zero = vk::ResolveModeFlags::SAMPLE_ZERO;
        let none = vk::ResolveModeFlags::NONE;
        // Depth/stencil with both modes supported.
        let modes = pick_resolve_modes_with(zero, zero, false, PixelFormat::D24UnormS8Uint);
        assert_eq!((modes.depth, modes.stencil), (zero, zero));
        // Depth only: stencil stays NONE when the device allows independent NONE.
        let modes = pick_resolve_modes_with(zero, zero, true, PixelFormat::D32Float);
        assert_eq!((modes.depth, modes.stencil), (zero, none));
        // Depth only without independent NONE: stencil is forced to the same mode.
        let modes = pick_resolve_modes_with(zero, zero, false, PixelFormat::D32Float);
        assert_eq!((modes.depth, modes.stencil), (zero, zero));
        // Stencil only without independent NONE and depth supported: depth follows.
        let modes = pick_resolve_modes_with(zero, zero, false, PixelFormat::S8Uint);
        assert_eq!((modes.depth, modes.stencil), (zero, zero));
        // Depth mode unsupported: nothing can resolve.
        let modes = pick_resolve_modes_with(none, zero, false, PixelFormat::D24UnormS8Uint);
        assert_eq!((modes.depth, modes.stencil), (none, zero));
    }

    #[test]
    fn supports_depth_stencil_resolve_requires_extension_aspects_and_modes() {
        let zero = vk::ResolveModeFlags::SAMPLE_ZERO;
        let none = vk::ResolveModeFlags::NONE;
        assert!(!supports_depth_stencil_resolve_with(
            false,
            zero,
            zero,
            true,
            PixelFormat::D32Float
        ));
        assert!(!supports_depth_stencil_resolve_with(
            true,
            zero,
            zero,
            true,
            PixelFormat::Invalid
        ));
        assert!(!supports_depth_stencil_resolve_with(
            true,
            zero,
            zero,
            true,
            PixelFormat::A8B8G8R8Unorm
        ));
        assert!(supports_depth_stencil_resolve_with(
            true,
            zero,
            zero,
            false,
            PixelFormat::D24UnormS8Uint
        ));
        // Depth-only format with a stencil mode forced on and no independent NONE
        // is still fine (modes are equal).
        assert!(supports_depth_stencil_resolve_with(
            true,
            zero,
            zero,
            false,
            PixelFormat::D32Float
        ));
        // Stencil resolve unsupported by the device: depth/stencil formats fail.
        assert!(!supports_depth_stencil_resolve_with(
            true,
            zero,
            none,
            true,
            PixelFormat::D24UnormS8Uint
        ));
        assert!(supports_depth_stencil_resolve_with(
            true,
            zero,
            none,
            true,
            PixelFormat::D32Float
        ));
        // Depth-only format, no independent NONE, stencil unsupported: depth mode
        // stays SAMPLE_ZERO while stencil is NONE, so the pair is rejected.
        assert!(!supports_depth_stencil_resolve_with(
            true,
            zero,
            none,
            false,
            PixelFormat::D32Float
        ));
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

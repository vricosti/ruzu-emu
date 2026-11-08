// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Vulkan rasterizer port matching zuyu `vk_rasterizer.h/.cpp`.
//!
//! Ref: zuyu `vk_rasterizer.h/.cpp` â€” central orchestrator that coordinates
//! shader compilation, pipeline caching, buffer/texture management, command
//! batching, and GPU state tracking for efficient Vulkan rendering.
//!
//! # Components
//!
//! - [`Scheduler`] â€” command chunk batching + submission
//! - [`StateTracker`] â€” dirty flags for selective state updates
//! - [`FixedPipelineState`] â€” hashable pipeline key
//! - [`GraphicsPipelineCache`] â€” compiled VkPipeline caching
//! - [`RenderPassCache`] â€” format-keyed VkRenderPass caching
//! - [`StagingBufferPool`] â€” CPUâ†”GPU transfer buffer pooling
//! - [`DescriptorPool`] â€” banked descriptor set allocation
//! - [`UpdateDescriptorQueue`] â€” ring-buffered descriptor updates
//! - [`BufferCache`] â€” vertex/index/uniform buffer management
//! - [`TextureCache`] â€” image/view/sampler/framebuffer management

use crate::query_cache::types::{QueryPropertiesFlags, QueryType};
use crate::nx2_profile::{Scope as ProfileScope, Zone as ProfileZone};

use std::ptr::NonNull;
use std::sync::{Arc, Once};

use ash::vk;
use ash::vk::Handle;

fn mesh_dispatch_mapping_fits(group_count: u32, max_groups: u32, mapping_bytes: u64) -> bool {
    let record_bytes = u64::from(shader_recompiler::backend::spirv::spirv_emit_context::MeshDispatchRecord::WORDS) * 4;
    group_count <= max_groups && u64::from(group_count) * record_bytes <= mapping_bytes
}

/// Mechanical batching of the compute transport's indirect records. FirstVertex
/// still selects the original output record, including holes from culled tasks.
fn mesh_indirect_draw_batches(count: u32, stride: u64, max_draws: u32)
    -> impl Iterator<Item = (u64, u32)> {
    assert_ne!(max_draws, 0, "Vulkan indirect draw limit must be nonzero");
    (0..count).step_by(max_draws as usize)
        .map(move |first| (u64::from(first) * stride, (count - first).min(max_draws)))
}

/// The output allocation stays deferred until its generated vertex/indirect
/// consumer has been recorded. The caller then returns it with FreeDeferred.
struct MeshComputeOutput {
    buffer: super::staging_buffer_pool::StagingBufferRef,
    descriptor: vk::DescriptorBufferInfo,
    record_count: u32,
    stride_bytes: u64,
    image_views: Vec<crate::texture_cache::texture_cache_base::ImageViewInOut>,
    task_count: Option<MeshTaskIndirectCount>,
}

/// Exact TaskEpilogue counts stay GPU-owned through the graphics consumer.
#[derive(Clone, Copy)]
struct MeshTaskIndirectCount {
    buffer: super::staging_buffer_pool::StagingBufferRef,
    parent_count: u32,
    payload_words: u32,
    child_capacity: u32,
}

impl MeshTaskIndirectCount {
    fn draw_offsets(&self, output_stride: u64) -> impl Iterator<Item = (u64, u64)> + '_ {
        (0..self.parent_count).map(move |parent| (
            u64::from(parent) * u64::from(self.child_capacity) * output_stride,
            self.buffer.offset + (u64::from(parent) * u64::from(self.payload_words) + 1) * 4,
        ))
    }
}

impl MeshComputeOutput {
    fn retains(&self, buffer: vk::Buffer) -> bool {
        self.buffer.buffer == buffer || self.task_count.is_some_and(|count| count.buffer.buffer == buffer)
    }
}

fn mesh_output_record_capacity(
    layout: shader_recompiler::backend::spirv::spirv_emit_context::MeshOutputLayout,
    max_storage_range: u64,
) -> Result<u32, &'static str> {
    if layout.stride_words == 0 || layout.vertex_span == 0 {
        return Err("empty mesh output layout");
    }
    Ok((max_storage_range / (u64::from(layout.stride_words) * 4))
        .min(u64::from(u32::MAX / layout.stride_words))
        .min(u64::from(u32::MAX / layout.vertex_span)) as u32)
}

/// Compute adaptation of DrawMeshTasksEXT's task-to-mesh expansion. Upstream
/// TaskEpilogue emits payload[1] child groups, each inheriting that parent's
/// payload. The caller must finish task writes and make them host-visible before
/// supplying this readback. Each range is uploaded as its own mapping-buffer
/// slice so physical WorkgroupId.x starts at zero for every dispatch.
#[derive(Debug)]
pub struct MeshTaskDispatchPlan {
    pub records: Vec<shader_recompiler::backend::spirv::spirv_emit_context::MeshDispatchRecord>,
    pub batches: Vec<std::ops::Range<usize>>,
}

impl MeshTaskDispatchPlan {
    /// Reserve a proven maximum per parent; a uniform shader guard rejects
    /// children beyond the actual payload count before any guest side effects.
    pub fn bounded(parent_count: u32, payload_words: u32, bound: u32,
        max_groups: u32, capacity: u32) -> Result<Self, &'static str> {
        use shader_recompiler::backend::spirv::spirv_emit_context::MeshDispatchRecord;
        let total = parent_count.checked_mul(bound).ok_or("mesh reservation overflow")?;
        if payload_words < 2 || parent_count.checked_mul(payload_words).is_none()
            || max_groups == 0 || total > capacity {
            return Err("mesh reservation exceeds transport capacity");
        }
        let mut records = Vec::new();
        records.try_reserve_exact(total as usize).map_err(|_| "mesh reservation allocation failed")?;
        for parent in 0..parent_count {
            for child in 0..bound {
                records.push(MeshDispatchRecord { guest_workgroup_x: child,
                    payload_base_words: parent * payload_words, output_record: records.len() as u32 });
            }
        }
        let batches = (0..records.len()).step_by(max_groups as usize)
            .map(|first| first..(first + max_groups as usize).min(records.len())).collect();
        Ok(Self { records, batches })
    }

    /// With no task stage, guest workgroup IDs directly identify mesh groups.
    /// No parent payload exists; all groups use the same initialized transport.
    pub fn direct(group_count: u32, max_groups: u32, capacity: u32) -> Result<Self, &'static str> {
        use shader_recompiler::backend::spirv::spirv_emit_context::MeshDispatchRecord;
        if max_groups == 0 || group_count > capacity {
            return Err("direct mesh dispatch exceeds transport capacity");
        }
        let mut records = Vec::new();
        records.try_reserve_exact(group_count as usize).map_err(|_| "mesh mapping allocation failed")?;
        for group in 0..group_count {
            records.push(MeshDispatchRecord { guest_workgroup_x: group, payload_base_words: 0, output_record: group });
        }
        let mut batches = Vec::new();
        batches.try_reserve_exact(u64::from(group_count).div_ceil(u64::from(max_groups)) as usize)
            .map_err(|_| "mesh dispatch allocation failed")?;
        for start in (0..records.len()).step_by(max_groups as usize) {
            batches.push(start..(start + max_groups as usize).min(records.len()));
        }
        Ok(Self { records, batches })
    }

    pub fn from_task_payloads(
        payload: &[u32],
        payload_words: u32,
        max_groups_per_dispatch: u32,
        output_record_capacity: u32,
    ) -> Result<Self, &'static str> {
        use shader_recompiler::backend::spirv::spirv_emit_context::MeshDispatchRecord;
        if payload_words < 2 || max_groups_per_dispatch == 0
            || payload.len() % payload_words as usize != 0 {
            return Err("invalid task payload or dispatch dimensions");
        }
        // The shader transport uses u32 word addresses. Reject an unaddressable
        // allocation before producing any records; never clamp a guest count.
        if payload.len() > u32::MAX as usize {
            return Err("task payload exceeds shader word addressing");
        }
        let mut total = 0u32;
        for parent in payload.chunks_exact(payload_words as usize) {
            total = total.checked_add(parent[1]).ok_or("mesh child count overflows")?;
        }
        if total > output_record_capacity {
            return Err("mesh output allocation is too small");
        }
        let mut records = Vec::new();
        records.try_reserve_exact(total as usize).map_err(|_| "mesh mapping allocation failed")?;
        for (parent_index, parent) in payload.chunks_exact(payload_words as usize).enumerate() {
            let payload_base_words = (parent_index * payload_words as usize) as u32;
            for guest_workgroup_x in 0..parent[1] {
                records.push(MeshDispatchRecord {
                    guest_workgroup_x,
                    payload_base_words,
                    output_record: records.len() as u32,
                });
            }
        }
        let mut batches = Vec::new();
        batches.try_reserve_exact((total as u64).div_ceil(max_groups_per_dispatch as u64) as usize)
            .map_err(|_| "mesh dispatch allocation failed")?;
        for start in (0..records.len()).step_by(max_groups_per_dispatch as usize) {
            batches.push(start..(start + max_groups_per_dispatch as usize).min(records.len()));
        }
        Ok(Self { records, batches })
    }
}

#[cfg(test)]
mod mesh_task_dispatch_tests {
    use super::MeshTaskDispatchPlan;

    #[test]
    fn task_indirect_count_uses_each_parent_word_one_and_retains_payload() {
        use ash::vk::{self, Handle};
        use crate::vulkan_common::vulkan_memory_allocator::MemoryUsage;
        let payload = super::super::staging_buffer_pool::StagingBufferRef {
            buffer: vk::Buffer::from_raw(11), device_address: 0, offset: 256,
            mapped: std::ptr::null_mut(), size: 3 * 64 * 4, usage: MemoryUsage::DeviceLocal,
            log2_level: 10, index: 1,
        };
        let count = super::MeshTaskIndirectCount {
            buffer: payload, parent_count: 3, payload_words: 64, child_capacity: 32,
        };
        assert_eq!(count.draw_offsets(1120).collect::<Vec<_>>(),
            [(0, 260), (35840, 516), (71680, 772)]);
        // Descriptor rollover may submit the compute work before graphics is
        // recorded: neither consumer buffer may be returned to the pool yet.
        let output = super::MeshComputeOutput {
            buffer: super::super::staging_buffer_pool::StagingBufferRef {
                buffer: vk::Buffer::from_raw(12), index: 2, ..payload
            },
            descriptor: vk::DescriptorBufferInfo::default(), record_count: 96,
            stride_bytes: 1120, image_views: Vec::new(), task_count: Some(count),
        };
        assert!(output.retains(vk::Buffer::from_raw(11)));
        assert!(output.retains(vk::Buffer::from_raw(12)));
        assert!(!output.retains(vk::Buffer::from_raw(13)));
        let direct = super::MeshComputeOutput { task_count: None, ..output };
        assert!(!direct.retains(vk::Buffer::from_raw(11)));
        assert!(direct.retains(vk::Buffer::from_raw(12)));
    }

    #[test]
    fn indirect_batches_keep_padded_record_offsets_and_device_limits() {
        let batches: Vec<_> = super::mesh_indirect_draw_batches(5, 1120, 2).collect();
        assert_eq!(batches, [(0, 2), (2240, 2), (4480, 1)]);
        assert_eq!(super::mesh_indirect_draw_batches(0, 1120, 2).count(), 0);
        assert_eq!(super::mesh_indirect_draw_batches(5, 1120, u32::MAX).collect::<Vec<_>>(), [(0, 5)]);
        assert_eq!(super::mesh_indirect_draw_batches(2, u64::from(u32::MAX), 1)
            .collect::<Vec<_>>(), [(0, 1), (u64::from(u32::MAX), 1)]);
    }

    #[test]
    fn reserved_children_preserve_parent_identity_and_output_holes() {
        let plan = MeshTaskDispatchPlan::bounded(3, 64, 3, 2, 9).unwrap();
        let actual_counts = [0, 3, 1];
        let active: Vec<_> = plan.records.iter().filter(|record|
            record.guest_workgroup_x < actual_counts[(record.payload_base_words / 64) as usize])
            .map(|record| record.words()).collect();
        assert_eq!(active, [[0, 64, 3], [1, 64, 4], [2, 64, 5], [0, 128, 6]]);
        assert_eq!(plan.batches, [0..2, 2..4, 4..6, 6..8, 8..9]);
        assert!(MeshTaskDispatchPlan::bounded(3, 64, 3, 2, 8).is_err());
        assert!(MeshTaskDispatchPlan::bounded(u32::MAX, 64, 3, 2, u32::MAX).is_err());
        assert!(MeshTaskDispatchPlan::bounded(3, 64, 0, 2, 0).unwrap().records.is_empty());
    }

    #[test]
    fn direct_mesh_groups_keep_guest_identity_across_host_batches() {
        let plan = MeshTaskDispatchPlan::direct(5, 2, 5).unwrap();
        assert_eq!(plan.batches, [0..2, 2..4, 4..5]);
        let words: Vec<_> = plan.records.iter().map(|record| record.words()).collect();
        assert_eq!(words, [[0, 0, 0], [1, 0, 1], [2, 0, 2], [3, 0, 3], [4, 0, 4]]);
        assert!(MeshTaskDispatchPlan::direct(5, 2, 4).is_err());
        assert!(MeshTaskDispatchPlan::direct(1, 0, 1).is_err());
        let empty = MeshTaskDispatchPlan::direct(0, 2, 0).unwrap();
        assert!(empty.records.is_empty() && empty.batches.is_empty());
    }

    #[test]
    fn mesh_output_capacity_obeys_storage_word_and_vertex_address_limits() {
        use shader_recompiler::backend::spirv::spirv_emit_context::MeshOutputLayout;
        let mut layout = MeshOutputLayout { stride_words: 280, vertex_span: 48, ..Default::default() };
        assert_eq!(super::mesh_output_record_capacity(layout, 2240), Ok(2));
        assert_eq!(super::mesh_output_record_capacity(layout, 2239), Ok(1));
        assert_eq!(super::mesh_output_record_capacity(layout, u64::MAX), Ok(u32::MAX / 280));
        layout.vertex_span = 1024;
        assert_eq!(super::mesh_output_record_capacity(layout, u64::MAX), Ok(u32::MAX / 1024));
        layout.vertex_span = 0;
        assert!(super::mesh_output_record_capacity(layout, 2240).is_err());
    }

    #[test]
    fn task_counts_preserve_parent_payload_and_child_ids_across_dispatch_limits() {
        // Word0 is not the count; unreferenced payload words must not affect it.
        let payload = [99, 0, 77, 88, 66, 3, 55, 44, 33, 2, 22, 11];
        let plan = MeshTaskDispatchPlan::from_task_payloads(&payload, 4, 2, 5).unwrap();
        let words: Vec<_> = plan.records.iter().map(|record| record.words()).collect();
        assert_eq!(words, [[0, 4, 0], [1, 4, 1], [2, 4, 2], [0, 8, 3], [1, 8, 4]]);
        assert_eq!(plan.batches, [0..2, 2..4, 4..5]);
        // Dispatch boundaries do not reset child identity or output record.
        assert_eq!(plan.records[plan.batches[1].start].guest_workgroup_x, 2);
        assert_eq!(plan.records[plan.batches[2].start].output_record, 4);
    }

    #[test]
    fn task_counts_reject_unrepresentable_work_without_truncating() {
        assert!(MeshTaskDispatchPlan::from_task_payloads(&[0, u32::MAX, 0, 1], 2, 32, u32::MAX).is_err());
        assert!(MeshTaskDispatchPlan::from_task_payloads(&[0, 3], 2, 32, 2).is_err());
        assert!(MeshTaskDispatchPlan::from_task_payloads(&[0, 1, 2], 2, 32, 1).is_err());
        assert!(MeshTaskDispatchPlan::from_task_payloads(&[0, 1], 2, 0, 1).is_err());
        assert!(MeshTaskDispatchPlan::from_task_payloads(&[], 0, 32, 1).is_err());
        let empty = MeshTaskDispatchPlan::from_task_payloads(&[55, 0], 2, 32, 0).unwrap();
        assert!(empty.records.is_empty());
        assert!(empty.batches.is_empty());
    }
}
use common::alignment::{align_down, align_up};
use common::types::PAGE_SIZE_U64;
use log::{debug, info, warn};
use smallvec::SmallVec;
use thiserror::Error;

use super::descriptor_buffer::DescriptorBufferRing;
use super::{blit_image, blit_screen, maxwell_to_vk};
use crate::buffer_cache::buffer_cache_base::{
    DeviceMemoryAccess, GpuMemoryAccess, ObtainBufferOperation, ObtainBufferSynchronize,
};
use crate::cache_types::CacheType;
use crate::control::channel_state_cache::{ChannelCacheAccessor, ChannelInfo, ChannelSetupCaches};
use crate::engines::draw_manager::Maxwell3DDrawView;
#[cfg(test)]
use crate::engines::draw_manager::{DrawState, Maxwell3DDrawRegisters};
use crate::engines::kepler_compute::DispatchCall;
#[cfg(test)]
use crate::engines::maxwell_3d::{BlendEquation, BlendFactor, ComparisonOp, CullFace, FrontFace};
use crate::engines::maxwell_3d::{PrimitiveTopology, VertexAttribType, NUM_VIEWPORTS};
use crate::engines::maxwell_dma::{dma, AccelerateDMAInterface};
use crate::fence_manager::FenceManager as GenericFenceManager;
use crate::framebuffer_config::FramebufferConfig;
use crate::gpu_logging::{get_instance, is_active};
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::host1x::syncpoint_manager::SyncpointManager;
use crate::memory_manager::MemoryManagerHandle;
use crate::rasterizer_interface::{RasterizerDownloadArea, RasterizerInterface};
use crate::texture_cache::types::NULL_IMAGE_ID;
use crate::vulkan_common::vulkan_device::{Device, DeviceReference};
use crate::vulkan_common::vulkan_memory_allocator::MemoryAllocator;

type VertexInputBindings = SmallVec<[vk::VertexInputBindingDescription2EXT<'static>; 32]>;
type VertexInputAttributes = SmallVec<[vk::VertexInputAttributeDescription2EXT<'static>; 32]>;

struct VertexInputDescriptions {
    bindings: VertexInputBindings,
    attributes: VertexInputAttributes,
}

impl VertexInputDescriptions {
    unsafe fn set(&self, extension: &ash::ext::vertex_input_dynamic_state::DeviceFn, cmdbuf: vk::CommandBuffer) {
        (extension.cmd_set_vertex_input_ext)(
            cmdbuf,
            self.bindings.len() as u32,
            self.bindings.as_ptr(),
            self.attributes.len() as u32,
            self.attributes.as_ptr(),
        );
    }
}

// SAFETY: every description is constructed by ash's builder with `p_next == null`.
// The remaining fields are owned scalar Vulkan values, and the scheduler moves
// the payload to its worker before reading and dropping it there.
unsafe impl Send for VertexInputDescriptions {}

// Rust counterpart of upstream `std::scoped_lock{buffer_cache.mutex,
// texture_cache.mutex}`. `parking_lot::ReentrantMutex` does not provide a
// multi-lock scoped helper, so retry both orders to avoid ABBA deadlocks.
macro_rules! lock_two_reentrant_mutexes {
    ($first_mutex:expr, $second_mutex:expr, $first_guard:ident, $second_guard:ident) => {
        let $first_guard;
        let $second_guard;
        loop {
            let first_candidate = unsafe { (*$first_mutex).lock() };
            if let Some(second_candidate) = unsafe { (*$second_mutex).try_lock() } {
                $first_guard = first_candidate;
                $second_guard = second_candidate;
                break;
            }
            drop(first_candidate);
            std::thread::yield_now();

            let second_candidate = unsafe { (*$second_mutex).lock() };
            if let Some(first_candidate) = unsafe { (*$first_mutex).try_lock() } {
                $first_guard = first_candidate;
                $second_guard = second_candidate;
                break;
            }
            drop(second_candidate);
            std::thread::yield_now();
        }
    };
}

struct GpuTickGuard(Option<NonNull<dyn Fn() + Send + Sync>>);

impl GpuTickGuard {
    fn new(callback: Option<&crate::renderer_base::GpuTickCallback>) -> Self {
        Self(callback.map(|callback| NonNull::from(callback.as_ref())))
    }
}

impl Drop for GpuTickGuard {
    fn drop(&mut self) {
        if let Some(callback) = self.0 {
            // SAFETY: the rasterizer owns the callback Arc for the complete
            // draw call. The guard is dropped before that field can be
            // replaced, matching Eden's scope-exit reference to `gpu`.
            unsafe { callback.as_ref()() };
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct DrawParams {
    base_instance: u32,
    num_instances: u32,
    base_vertex: i32,
    num_vertices: u32,
    first_index: u32,
    is_indexed: bool,
}

fn direct_draw_log_info(draw_params: DrawParams) -> (&'static str, String) {
    if draw_params.is_indexed {
        (
            "vkCmdDrawIndexed",
            format!(
                "vertices={}, instances={}, firstIndex={}, baseVertex={}, baseInstance={}",
                draw_params.num_vertices,
                draw_params.num_instances,
                draw_params.first_index,
                draw_params.base_vertex,
                draw_params.base_instance
            ),
        )
    } else {
        (
            "vkCmdDraw",
            format!(
                "vertices={}, instances={}, firstVertex={}, firstInstance={}",
                draw_params.num_vertices,
                draw_params.num_instances,
                draw_params.base_vertex,
                draw_params.base_instance
            ),
        )
    }
}

fn indirect_draw_log_info(
    is_indexed: bool,
    max_draw_counts: usize,
    stride: usize,
) -> (&'static str, String) {
    (
        if is_indexed {
            "vkCmdDrawIndexedIndirect"
        } else {
            "vkCmdDrawIndirect"
        },
        format!("drawCount={}, stride={}", max_draw_counts, stride),
    )
}

fn dispatch_log_info(dim: [u32; 3]) -> String {
    format!(
        "groupCountX={}, groupCountY={}, groupCountZ={}",
        dim[0], dim[1], dim[2]
    )
}

fn make_draw_params(draw: &Maxwell3DDrawView<'_>, instance_count: u32) -> DrawParams {
    let draw_state = draw.draw_state();
    let is_indexed = draw.is_indexed();
    let mut params = DrawParams {
        base_instance: draw_state.base_instance,
        num_instances: instance_count,
        base_vertex: if is_indexed {
            draw_state.base_index as i32
        } else {
            draw_state.vertex_buffer.first as i32
        },
        num_vertices: if is_indexed {
            draw_state.index_buffer.count
        } else {
            draw_state.vertex_buffer.count
        },
        first_index: if is_indexed {
            draw_state.index_buffer.first
        } else {
            0
        },
        is_indexed,
    };

    match draw_state.topology {
        PrimitiveTopology::Quads => {
            params.num_vertices = (params.num_vertices / 4) * 6;
            params.base_vertex = 0;
            params.is_indexed = true;
        }
        PrimitiveTopology::QuadStrip => {
            params.num_vertices = params.num_vertices.wrapping_sub(2) / 2 * 6;
            params.base_vertex = 0;
            params.is_indexed = true;
        }
        _ => {}
    }

    params
}

/// Port of the color clear-value conversion in `RasterizerVulkan::Clear`.
fn make_color_clear_value(format: crate::surface::PixelFormat, color: [f32; 4]) -> vk::ClearValue {
    if !crate::surface::is_pixel_format_integer(format) {
        return vk::ClearValue {
            color: vk::ClearColorValue { float32: color },
        };
    }
    let int_size = crate::surface::pixel_component_size_bits_integer(format);
    if !crate::surface::is_pixel_format_signed_integer(format) {
        let scale = ((int_size as u64) << 1) as f32;
        return vk::ClearValue {
            color: vk::ClearColorValue {
                uint32: color.map(|component| (scale * component) as u32),
            },
        };
    }
    let scale = (((int_size - 1) as i64) << 1) as f32;
    vk::ClearValue {
        color: vk::ClearColorValue {
            int32: color.map(|component| (scale * (component - 0.5)) as i32),
        },
    }
}

use super::fence_manager::{Fence as VkFence, FenceManager as VkFenceBackend};

/// Port of `GetViewportState` from the anonymous namespace in
/// `vk_rasterizer.cpp`.
fn get_viewport_state(
    translate_x: f32,
    scale_x: f32,
    translate_y: f32,
    scale_y: f32,
    translate_z: f32,
    scale_z: f32,
    scale: f32,
    depth_minus_one_to_one: bool,
    lower_left: bool,
    y_negate: bool,
    surface_clip_height: f32,
    clamp_depth: bool,
) -> vk::Viewport {
    let conv = |value: f32| -> f32 {
        let new_value = value * scale;
        if scale < 1.0 {
            new_value.abs().round().copysign(value)
        } else {
            new_value
        }
    };

    let x = conv(translate_x - scale_x);
    let width = conv(scale_x * 2.0);
    let mut y = conv(translate_y - scale_y);
    let mut height = conv(scale_y * 2.0);
    if lower_left {
        y += conv(surface_clip_height);
        height = -height;
    }
    if y_negate {
        y += height;
        height = -height;
    }

    let reduce_z = if depth_minus_one_to_one { 1.0 } else { 0.0 };
    let mut min_depth = translate_z - scale_z * reduce_z;
    let mut max_depth = translate_z + scale_z;
    if clamp_depth {
        min_depth = min_depth.clamp(0.0, 1.0);
        max_depth = max_depth.clamp(0.0, 1.0);
    }
    vk::Viewport {
        x,
        y,
        width: if width != 0.0 { width } else { 1.0 },
        height: if height != 0.0 { height } else { 1.0 },
        min_depth,
        max_depth,
    }
}

fn viewport_state(
    viewport_transforms: &[crate::engines::maxwell_3d::ViewportTransformInfo; NUM_VIEWPORTS],
    depth_mode: crate::engines::maxwell_3d::DepthMode,
    window_origin_lower_left: bool,
    surface_clip_height: u32,
    index: usize,
    scale: f32,
    depth_range_unrestricted: bool,
    nv_viewport_swizzle: bool,
) -> vk::Viewport {
    let src = viewport_transforms[index];
    get_viewport_state(
        src.translate_x,
        src.scale_x,
        src.translate_y,
        src.scale_y,
        src.translate_z,
        src.scale_z,
        scale,
        depth_mode == crate::engines::maxwell_3d::DepthMode::MinusOneToOne,
        window_origin_lower_left,
        !nv_viewport_swizzle && ((src.swizzle >> 4) & 0x7) == 3,
        surface_clip_height as f32,
        !depth_range_unrestricted,
    )
}

fn scissor_state(
    src: crate::engines::maxwell_3d::ScissorInfo,
    window_origin_lower_left: bool,
    surface_clip_height: u32,
    up_scale: u32,
    down_shift: u32,
) -> vk::Rect2D {
    // Port of upstream `GetScissorState::scale_up`. Keep the signed rounding
    // behavior literal: fractional downscales must still cover at least one
    // pixel, including for negative offsets.
    let scale_up = |value: i32| -> i32 {
        if value == 0 {
            return 0;
        }
        let upset = value.wrapping_mul(up_scale as i32);
        let accumulation = if (up_scale >> down_shift) == 0 {
            upset % 2
        } else {
            0
        };
        let converted = upset >> down_shift;
        if value < 0 {
            (converted - accumulation).min(-1)
        } else {
            (converted + accumulation).max(1)
        }
    };
    let clip_height = surface_clip_height as i32;
    let mut min_y = if window_origin_lower_left {
        clip_height - src.max_y as i32
    } else {
        src.min_y as i32
    };
    let mut max_y = if window_origin_lower_left {
        clip_height - src.min_y as i32
    } else {
        src.max_y as i32
    };
    min_y = min_y.max(0);
    max_y = max_y.max(0);

    if src.enabled {
        vk::Rect2D {
            offset: vk::Offset2D {
                x: scale_up(src.min_x as i32),
                y: scale_up(min_y),
            },
            extent: vk::Extent2D {
                width: scale_up(src.max_x.wrapping_sub(src.min_x) as i32) as u32,
                height: scale_up(max_y.wrapping_sub(min_y)) as u32,
            },
        }
    } else {
        vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D {
                width: i32::MAX as u32,
                height: i32::MAX as u32,
            },
        }
    }
}
use super::blit_image::BlitImageHelper;
use super::buffer_cache::{BufferCacheRuntime, VulkanCommonBufferCache};
use super::descriptor_pool::DescriptorPool;
use super::pipeline_cache::PipelineCache as VulkanPipelineCache;
use super::query_cache::QueryCache as VulkanQueryCache;
use super::render_pass_cache::RenderPassCache;
use super::scheduler::Scheduler;
use super::staging_buffer_pool::StagingBufferPool;
use super::state_tracker::StateTracker;
use super::texture_cache::TextureCache;
use super::update_descriptor::{
    UpdateDescriptorQueue, COMPUTE_FRAME_PAYLOAD_SIZE, GUEST_FRAME_PAYLOAD_SIZE,
};

const NEEDS_D24: [u64; 3] = [
    0x0100_6A80_0016_E000,
    0x0100_E950_0403_8000,
    0x0100_A630_1214_E000,
];

fn supports_primitive_restart(topology: vk::PrimitiveTopology) -> bool {
    !matches!(
        topology,
        vk::PrimitiveTopology::POINT_LIST
            | vk::PrimitiveTopology::LINE_LIST
            | vk::PrimitiveTopology::TRIANGLE_LIST
            | vk::PrimitiveTopology::LINE_LIST_WITH_ADJACENCY
            | vk::PrimitiveTopology::TRIANGLE_LIST_WITH_ADJACENCY
            | vk::PrimitiveTopology::PATCH_LIST
    )
}

fn depth_bias_constant(
    depth_bias: f32,
    zeta_format: u32,
    supports_d24_depth: bool,
    program_id: u64,
) -> f32 {
    let mut units = depth_bias / 2.0;
    let is_d24 = matches!(zeta_format, 0x14 | 0x15 | 0x16 | 0x18);
    if is_d24 && !supports_d24_depth && NEEDS_D24.contains(&program_id) {
        let rescale_factor = (1u64 << (32 - 24)) as f64 / f32::MAX as f64;
        units = (units as f64 * rescale_factor) as f32;
    }
    units
}

fn supports_counter_reset(query_type: u32) -> bool {
    matches!(
        query_type,
        x if x == QueryType::ZPassPixelCount64 as u32
            || x == QueryType::StreamingByteCount as u32
            || x == QueryType::StreamingPrimitivesSucceeded as u32
            || x == QueryType::VtgPrimitivesOut as u32
    )
}

#[derive(Debug, Error)]
pub enum RendererError {
    #[error("Vulkan initialization failed: {0}")]
    InitFailed(String),
    #[error("No suitable GPU found")]
    NoSuitableDevice,
    #[error("Surface creation failed: {0}")]
    SurfaceFailed(String),
    #[error("Shader compilation failed: {0}")]
    ShaderCompilationFailed(String),
    #[error("Pipeline creation failed: {0}")]
    PipelineCreationFailed(String),
    #[error("Vulkan error: {0}")]
    VulkanError(vk::Result),
}

impl From<vk::Result> for RendererError {
    fn from(e: vk::Result) -> Self {
        RendererError::VulkanError(e)
    }
}

struct GpuMemoryAccessAdapter {
    mm: MemoryManagerHandle,
}

impl GpuMemoryAccess for GpuMemoryAccessAdapter {
    fn gpu_to_cpu_address(&self, gpu_addr: u64) -> Option<u64> {
        // SAFETY: the rasterizer retains the channel's owning Arc while this
        // adapter is installed, and BufferCache access is externally
        // serialized by its recursive mutex, matching upstream's stored raw
        // `Tegra::MemoryManager*`.
        unsafe { self.mm.as_ref().gpu_to_cpu_address(gpu_addr) }
    }

    fn read_u64(&self, gpu_addr: u64) -> Option<u64> {
        let mut buf = [0u8; 8];
        unsafe {
            self.mm.as_ref().read_block(gpu_addr, &mut buf);
        }
        Some(u64::from_le_bytes(buf))
    }

    fn read_u32(&self, gpu_addr: u64) -> Option<u32> {
        let mut buf = [0u8; 4];
        unsafe {
            self.mm.as_ref().read_block(gpu_addr, &mut buf);
        }
        Some(u32::from_le_bytes(buf))
    }

    fn is_within_gpu_address_range(&self, gpu_addr: u64) -> bool {
        unsafe { self.mm.as_ref().is_within_gpu_address_range(gpu_addr) }
    }

    fn max_continuous_range(&self, gpu_addr: u64, size: u64) -> u64 {
        unsafe { self.mm.as_ref().max_continuous_range(gpu_addr, size) }
    }

    fn get_memory_layout_size(&self, gpu_addr: u64) -> u64 {
        unsafe { self.mm.as_ref().get_memory_layout_size(gpu_addr) }
    }

    fn get_id(&self) -> usize {
        unsafe { self.mm.as_ref().get_id() }
    }

    fn get_submapped_range(&self, gpu_addr: u64, size: u64) -> Vec<(u64, u64)> {
        unsafe { self.mm.as_ref().get_submapped_range(gpu_addr, size) }
    }
}

struct DeviceMemoryAccessAdapter {
    device_memory: Arc<MaxwellDeviceMemoryManager>,
}

impl DeviceMemoryAccess for DeviceMemoryAccessAdapter {
    fn get_pointer(&self, device_addr: u64) -> Option<*const u8> {
        let pointer = self.device_memory.get_pointer(device_addr);
        (!pointer.is_null()).then_some(pointer)
    }

    fn read_block_unsafe(&self, device_addr: u64, dst: &mut [u8]) {
        self.device_memory.smmu_read_block_unsafe(device_addr, dst);
    }

    fn write_block_unsafe(&self, device_addr: u64, src: &[u8]) {
        self.device_memory.smmu_write_block_unsafe(device_addr, src);
    }
}

/// Central Vulkan rendering orchestrator.
///
/// Ref: zuyu RasterizerVulkan â€” coordinates all rendering sub-components:
/// shader compilation, pipeline caching, buffer management, dynamic state
/// tracking, and command batching for efficient GPU rendering.
pub struct RasterizerVulkan {
    // Rust drops fields in declaration order, opposite to C++. Eden declares
    // the fence manager after the caches: join its callback thread before any
    // query/buffer/texture owner captured by those callbacks is destroyed.
    fence_manager: GenericFenceManager<VkFence>,
    /// Non-owning counterpart of upstream `const Device& device`.
    ///
    /// Recorded commands copy this pointer-sized reference and resolve the
    /// logical Vulkan device when the scheduler executes them. Copying
    /// `ash::Device` into every command would copy its full dispatch table,
    /// unlike upstream's pointer capture through `this`.
    device: DeviceReference,
    syncpoints: Arc<SyncpointManager>,
    /// Shared owner counterpart of upstream
    /// `Tegra::MaxwellDeviceMemoryManager& device_memory`.
    #[allow(dead_code)]
    device_memory: Arc<MaxwellDeviceMemoryManager>,
    channel_caches: ChannelSetupCaches<ChannelInfo>,

    // Sub-components (matching zuyu's architecture)
    /// Non-owning counterpart of upstream `Scheduler& scheduler`.
    ///
    /// `RendererVulkan` owns the single boxed scheduler and outlives this
    /// rasterizer. The stable pointer preserves upstream ownership without a
    /// self-referential Rust struct.
    scheduler: OwnerReference<Scheduler>,
    /// Stable non-owning counterpart of upstream `MemoryAllocator&`.
    #[allow(dead_code)]
    memory_allocator: NonNull<MemoryAllocator>,
    /// Non-owning counterpart of upstream `StateTracker& state_tracker`.
    state_tracker: OwnerReference<StateTracker>,
    staging_pool: Box<StagingBufferPool>,
    // Must be destroyed before every stable runtime owner referenced by its
    // cached GraphicsPipeline objects.
    pipeline_cache: VulkanPipelineCache,
    // Boxed like `scheduler`/`staging_pool`/`render_pass_cache`: sub-components
    // capture `NonNull` pointers to these during construction (BlitImageHelper
    // and TextureCache point at the descriptor pool and the descriptor queues,
    // TextureCache at the blit helper). A by-value field would move when the
    // constructor returns `Self`, leaving those pointers dangling on the old
    // stack frame â€” observed as an UpdateDescriptorQueue whose `acquire()`
    // clamped the real instance while `add_buffer` grew a stale cursor until
    // allocation failure.
    #[allow(dead_code)]
    descriptor_pool: Box<DescriptorPool>,
    desc_queue: Box<UpdateDescriptorQueue>,
    compute_pass_desc_queue: Box<UpdateDescriptorQueue>,
    descriptor_buffer_ring: Box<DescriptorBufferRing>,
    blit_image: Box<BlitImageHelper>,
    fallback_uniform_buffer: vk::Buffer,
    fallback_uniform_memory: vk::DeviceMemory,
    fallback_sampler: vk::Sampler,
    shader_cache: crate::shader_cache::ShaderCache,
    query_cache: VulkanQueryCache,
    accelerate_dma: AccelerateDMA,
    common_buffer_cache: Box<VulkanCommonBufferCache>,
    texture_cache: Box<TextureCache>,
    fence_backend: VkFenceBackend,
    // Rust drops fields in declaration order. Keep this owner after every
    // cache that retains a non-owning pointer to it, matching C++ reverse
    // member destruction where RenderPassCache outlives TextureCache and
    // PipelineCache.
    #[allow(dead_code)]
    render_pass_cache: Box<RenderPassCache>,
    wfi_event: vk::Event,

    // Upstream FlushWork checks every eighth operation and flushes at 4096.
    draw_counter: u32,
    /// Monotonic draw sequence used only by env-gated diagnostics.
    draw_sequence: u64,
    /// Draws dropped because pipeline compilation failed (diagnostic).
    draw_skipped_pipeline: u64,
    driver_id: vk::DriverId,
    extended_dynamic_state_supported: bool,
    extended_dynamic_state2_supported: bool,
    extended_dynamic_state2_logic_op_supported: bool,
    extended_dynamic_state3_blending_supported: bool,
    extended_dynamic_state3_enables_supported: bool,
    color_write_enable_supported: bool,
    dynamic_state3_support: super::graphics_pipeline::DynamicState3Support,
    line_rasterization_supported: bool,
    smooth_lines_supported: bool,
    vertex_input_dynamic_state_supported: bool,
    depth_bounds_supported: bool,
    supports_d24_depth: bool,
    depth_range_unrestricted: bool,
    nv_viewport_swizzle: bool,
    extended_dynamic_state2: Option<ash::ext::extended_dynamic_state2::Device>,
    extended_dynamic_state3: Option<ash::ext::extended_dynamic_state3::Device>,
    color_write_enable: Option<ash::ext::color_write_enable::DeviceFn>,
    vertex_input_dynamic_state: Option<ash::ext::vertex_input_dynamic_state::DeviceFn>,
    draw_indirect_count: Option<ash::khr::draw_indirect_count::Device>,
    push_descriptor: Option<ash::khr::push_descriptor::Device>,
    max_viewports: u32,
    max_vertex_input_attributes: u32,
    max_vertex_input_bindings: u32,
    max_compute_work_group_count: [u32; 3],
    topology_list_primitive_restart_supported: bool,
    patch_list_primitive_restart_supported: bool,
    transform_feedback_supported: bool,
    transform_feedback_draw_supported: bool,
    // Channel-bound GPU memory manager, matching upstream rasterizer access to
    // the active channel's Tegra::MemoryManager.
    channel_memory_manager: Option<Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>>,
    // Non-owning counterpart of upstream's channel-bound
    // `Tegra::MemoryManager*`. `channel_memory_manager` remains the lifetime
    // owner; graphics pipeline descriptor reads use this pointer directly.
    channel_memory_manager_handle: Option<MemoryManagerHandle>,
    /// Rust owner bridge for upstream `Tegra::GPU& gpu` / `gpu.TickWork()`.
    gpu_tick_callback: Option<crate::renderer_base::GpuTickCallback>,
    /// Rust owner bridge for upstream `Tegra::GPU& gpu` /
    /// `gpu.InvalidateGPUCache()`.
    invalidate_gpu_cache_callback: Option<crate::renderer_base::InvalidateGpuCacheCallback>,
}

// Raw pointers are only used for mapped memory
unsafe impl Send for RasterizerVulkan {}

/// Stable, non-owning Rust representation of an upstream C++ reference member.
///
/// The owner boxes the referenced value and is declared after the borrower so
/// Rust drops the borrower first.
struct OwnerReference<T> {
    pointer: NonNull<T>,
}

impl<T> OwnerReference<T> {
    fn new(value: &mut T) -> Self {
        Self {
            pointer: NonNull::from(value),
        }
    }
}

impl<T> std::ops::Deref for OwnerReference<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.pointer.as_ref() }
    }
}

impl<T> std::ops::DerefMut for OwnerReference<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.pointer.as_mut() }
    }
}

/// Vulkan DMA accelerator owned by `RasterizerVulkan`.
///
/// This preserves Eden's separate `AccelerateDMA` owner and its references to
/// the buffer cache, texture cache and scheduler.  All three pointees have
/// stable storage for the rasterizer lifetime.
struct AccelerateDMA {
    buffer_cache: NonNull<VulkanCommonBufferCache>,
    texture_cache: NonNull<TextureCache>,
    #[allow(dead_code)]
    scheduler: NonNull<Scheduler>,
}

impl AccelerateDMA {
    fn new(
        buffer_cache: &mut VulkanCommonBufferCache,
        texture_cache: &mut TextureCache,
        scheduler: &mut Scheduler,
    ) -> Self {
        Self {
            buffer_cache: NonNull::from(buffer_cache),
            texture_cache: NonNull::from(texture_cache),
            scheduler: NonNull::from(scheduler),
        }
    }

    fn dma_buffer_image_copy(
        &mut self,
        copy_info: &dma::ImageCopy,
        buffer_operand: &dma::BufferOperand,
        image_operand: &dma::ImageOperand,
        is_image_upload: bool,
    ) -> bool {
        let buffer_cache = unsafe { self.buffer_cache.as_mut() };
        let texture_cache = unsafe { self.texture_cache.as_mut() };
        let buffer_mutex: *const _ = &buffer_cache.mutex;
        let texture_mutex: *const _ = &texture_cache.base.mutex;
        lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);

        let image_id = texture_cache
            .base
            .dma_image_id(image_operand, is_image_upload);
        if image_id == NULL_IMAGE_ID {
            return false;
        }
        let buffer_size = buffer_operand.pitch.wrapping_mul(buffer_operand.height);
        let post_op = if is_image_upload {
            ObtainBufferOperation::DoNothing
        } else {
            ObtainBufferOperation::MarkAsWritten
        };
        let (buffer_id, offset) = buffer_cache.obtain_buffer(
            buffer_operand.address,
            buffer_size,
            ObtainBufferSynchronize::FullSynchronize,
            post_op,
        );
        let buffer = vk::Buffer::from_raw(buffer_cache.resolve_backend_buffer_raw(buffer_id));
        if buffer == vk::Buffer::null() {
            return false;
        }
        texture_cache.dma_buffer_image_copy(
            copy_info,
            buffer_operand,
            image_operand,
            image_id,
            buffer,
            offset as vk::DeviceSize,
            is_image_upload,
        )
    }
}

impl AccelerateDMAInterface for AccelerateDMA {
    fn buffer_copy(&mut self, src_address: u64, dest_address: u64, amount: u64) -> bool {
        unsafe {
            let buffer_cache = self.buffer_cache.as_mut();
            let buffer_mutex: *const _ = &buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            buffer_cache.dma_copy(src_address, dest_address, amount)
        }
    }

    fn buffer_clear(&mut self, dst_address: u64, amount: u64, value: u32) -> bool {
        unsafe {
            let buffer_cache = self.buffer_cache.as_mut();
            let buffer_mutex: *const _ = &buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            buffer_cache.dma_clear(dst_address, amount, value)
        }
    }

    fn image_to_buffer(
        &mut self,
        copy_info: &dma::ImageCopy,
        src: &dma::ImageOperand,
        dst: &dma::BufferOperand,
    ) -> bool {
        self.dma_buffer_image_copy(copy_info, dst, src, false)
    }

    fn buffer_to_image(
        &mut self,
        copy_info: &dma::ImageCopy,
        src: &dma::BufferOperand,
        dst: &dma::ImageOperand,
    ) -> bool {
        self.dma_buffer_image_copy(copy_info, src, dst, true)
    }
}

#[cfg(test)]
mod owner_reference_tests {
    use super::OwnerReference;

    #[test]
    fn references_renderer_owned_stable_storage() {
        let mut owner = Box::new(0x1234_u64);
        let owner_address = std::ptr::from_ref(owner.as_ref());
        let mut reference = OwnerReference::new(owner.as_mut());

        assert_eq!(reference.pointer.as_ptr(), owner_address.cast_mut());
        *reference = 0x5678;
        assert_eq!(*owner, 0x5678);
    }
}

impl RasterizerVulkan {
    /// Low-bit mask used by upstream to check every eighth operation.
    #[cfg(not(target_os = "android"))]
    const DISPATCH_THRESHOLD: u32 = 7;
    #[cfg(target_os = "android")]
    const DISPATCH_THRESHOLD: u32 = 3;
    /// Hard flush threshold â€” full GPU submit every N draws.
    #[cfg(not(target_os = "android"))]
    const FLUSH_THRESHOLD: u32 = 4096;
    #[cfg(target_os = "android")]
    const FLUSH_THRESHOLD: u32 = 512;

    /// Create a new RasterizerVulkan.
    ///
    /// Takes Vulkan handles from the VulkanPresenter so they share the same
    /// device and queue.
    pub fn new(
        shader_notify: crate::shader_notify::ShaderNotifyHandle,
        vulkan_device: &Device,
        instance: ash::Instance,
        physical_device: vk::PhysicalDevice,
        driver_id: vk::DriverId,
        depth_bounds_supported: bool,
        depth_range_unrestricted: bool,
        nv_viewport_swizzle: bool,
        index_type_uint8_supported: bool,
        has_null_descriptor: bool,
        extended_dynamic_state_supported: bool,
        transform_feedback_supported: bool,
        host_query_reset_supported: bool,
        extended_dynamic_state2_supported: bool,
        extended_dynamic_state2_logic_op_supported: bool,
        extended_dynamic_state3_blending_supported: bool,
        extended_dynamic_state3_enables_supported: bool,
        color_write_enable_supported: bool,
        dynamic_state3_support: super::graphics_pipeline::DynamicState3Support,
        line_rasterization_supported: bool,
        smooth_lines_supported: bool,
        vertex_input_dynamic_state_supported: bool,
        topology_list_primitive_restart_supported: bool,
        patch_list_primitive_restart_supported: bool,
        must_emulate_bgr565: bool,
        ext_4444_formats_supported: bool,
        image_format_list_supported: bool,
        optimal_astc_supported: bool,
        custom_border_color_supported: bool,
        sampler_filter_minmax_supported: bool,
        max_viewports: u32,
        max_vertex_input_attributes: u32,
        max_vertex_input_bindings: u32,
        max_compute_work_group_count: [u32; 3],
        draw_indirect_count_supported: bool,
        push_descriptor_supported: bool,
        syncpoints: Arc<SyncpointManager>,
        device_memory: Arc<MaxwellDeviceMemoryManager>,
        memory_allocator: &mut MemoryAllocator,
        state_tracker: &mut StateTracker,
        scheduler: &mut Scheduler,
    ) -> Result<Self, RendererError> {
        info!("RasterizerVulkan: initializing renderer");
        let device = vulkan_device.get_logical();

        // Create staging buffer pool
        let mut staging_pool = Box::new(
            StagingBufferPool::new(vulkan_device, memory_allocator, scheduler)
                .map_err(|error| RendererError::VulkanError(error.result))?,
        );

        // Create descriptor pool. Boxed (with the descriptor queues and the
        // blit helper below) so the `NonNull` pointers captured by
        // sub-components stay valid when the constructed `Self` is moved.
        let mut descriptor_pool = Box::new(DescriptorPool::new(vulkan_device, scheduler));

        // Create descriptor update queue
        let mut desc_queue = Box::new(UpdateDescriptorQueue::new(
            vulkan_device,
            GUEST_FRAME_PAYLOAD_SIZE,
            vulkan_device.is_ext_descriptor_buffer_supported(),
        ));
        let mut compute_pass_desc_queue = Box::new(UpdateDescriptorQueue::new(
            vulkan_device,
            COMPUTE_FRAME_PAYLOAD_SIZE,
            false,
        ));
        let mut descriptor_buffer_ring = Box::new(
            DescriptorBufferRing::new(vulkan_device, memory_allocator).map_err(|error| {
                RendererError::InitFailed(format!("descriptor buffer ring: {error:?}"))
            })?,
        );
        let mut blit_image = Box::new(BlitImageHelper::new(
            vulkan_device,
            scheduler,
            state_tracker,
            descriptor_pool.as_mut(),
        ));

        let (fallback_uniform_buffer, fallback_uniform_memory, fallback_uniform_mapped) =
            create_host_buffer(
                &instance,
                physical_device,
                &device,
                0x10000,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
            )?;
        unsafe {
            // Upstream's physical null-buffer fallback is deterministically
            // zero-filled. Do the same for this legacy rasterizer fallback.
            std::ptr::write_bytes(fallback_uniform_mapped, 0, 0x10000);
        }
        let fallback_sampler = create_fallback_sampler(&device)?;

        // Create render pass cache
        let mut render_pass_cache = Box::new(RenderPassCache::new(vulkan_device));

        // Upstream BufferCacheParams uses
        // MemoryTrackerBase<Tegra::MaxwellDeviceMemoryManager>. Keep the
        // tracker connected to the shared device-memory manager so tracked
        // pages are protected and later CPU writes reach OnCPUWrite.
        let buffer_runtime = BufferCacheRuntime::new(
            vulkan_device,
            instance.clone(),
            physical_device,
            memory_allocator,
            scheduler,
            staging_pool.as_mut(),
            desc_queue.as_mut(),
            compute_pass_desc_queue.as_mut(),
            descriptor_pool.as_ref(),
            index_type_uint8_supported,
            has_null_descriptor,
            extended_dynamic_state_supported,
            transform_feedback_supported,
            max_vertex_input_bindings,
        )
        .map_err(|e| RendererError::InitFailed(format!("buffer cache runtime: {:?}", e)))?;
        let mut common_buffer_cache = Box::new(VulkanCommonBufferCache::new(
            device_memory.as_ref(),
            buffer_runtime,
        ));
        common_buffer_cache.set_device_memory(Box::new(DeviceMemoryAccessAdapter {
            device_memory: Arc::clone(&device_memory),
        }));

        // Create texture cache
        let shader_cache = if std::env::var_os("RUZU_NX2_MESH_DRAW").is_some() {
            crate::shader_cache::ShaderCache::new_mesh_compute(Arc::clone(&device_memory))
        } else {
            crate::shader_cache::ShaderCache::new(Arc::clone(&device_memory))
        };

        let mut texture_cache = Box::new(
            TextureCache::new(
                vulkan_device,
                device.clone(),
                instance.clone(),
                physical_device,
                Arc::clone(&device_memory),
                scheduler,
                &mut *memory_allocator,
                staging_pool.as_mut(),
                blit_image.as_mut(),
                render_pass_cache.as_mut(),
                descriptor_pool.as_mut(),
                compute_pass_desc_queue.as_mut(),
                image_format_list_supported,
                optimal_astc_supported,
                must_emulate_bgr565,
                ext_4444_formats_supported,
                custom_border_color_supported,
                sampler_filter_minmax_supported,
                has_null_descriptor,
            )
            .map_err(|e| RendererError::InitFailed(format!("texture cache: {:?}", e)))?,
        );

        // Create the pipeline-cache owner only after the stable cache storage
        // exists. Its cached GraphicsPipeline objects retain these same four
        // reference members, matching vk_graphics_pipeline.h.
        let pipeline_cache = VulkanPipelineCache::new(
            vulkan_device,
            descriptor_pool.as_mut(),
            shader_notify,
            render_pass_cache.as_mut(),
            scheduler,
            common_buffer_cache.as_mut(),
            texture_cache.as_mut(),
            desc_queue.as_mut(),
            descriptor_buffer_ring.as_mut(),
        );

        // Create query cache
        let query_cache = VulkanQueryCache::new(
            vulkan_device,
            &instance,
            device.clone(),
            scheduler,
            staging_pool.as_mut(),
            memory_allocator,
            common_buffer_cache.as_mut(),
            descriptor_pool.as_ref(),
            compute_pass_desc_queue.as_mut(),
            Arc::clone(&device_memory),
            driver_id,
            transform_feedback_supported,
            host_query_reset_supported,
        )
        .map_err(|e| RendererError::InitFailed(format!("query cache: {e:?}")))?;
        let accelerate_dma = AccelerateDMA::new(
            common_buffer_cache.as_mut(),
            texture_cache.as_mut(),
            scheduler,
        );

        let wfi_event_info = vk::EventCreateInfo::default();
        let wfi_event = unsafe {
            device
                .create_event(&wfi_event_info, None)
                .map_err(|e| RendererError::InitFailed(format!("wfi event: {:?}", e)))?
        };

        let draw_indirect_count = draw_indirect_count_supported
            .then(|| ash::khr::draw_indirect_count::Device::new(&instance, &device));
        let push_descriptor = push_descriptor_supported
            .then(|| ash::khr::push_descriptor::Device::new(&instance, &device));
        let extended_dynamic_state2 = extended_dynamic_state2_logic_op_supported
            .then(|| ash::ext::extended_dynamic_state2::Device::new(&instance, &device));
        let extended_dynamic_state3 = (extended_dynamic_state3_blending_supported
            || extended_dynamic_state3_enables_supported)
            .then(|| ash::ext::extended_dynamic_state3::Device::new(&instance, &device));
        let color_write_enable = color_write_enable_supported.then(|| {
            ash::ext::color_write_enable::DeviceFn::load(|name| unsafe {
                std::mem::transmute(instance.get_device_proc_addr(device.handle(), name.as_ptr()))
            })
        });
        let vertex_input_dynamic_state = vertex_input_dynamic_state_supported.then(|| {
            ash::ext::vertex_input_dynamic_state::DeviceFn::load(|name| unsafe {
                std::mem::transmute(instance.get_device_proc_addr(device.handle(), name.as_ptr()))
            })
        });

        // Safe Rust adaptation of upstream `scheduler.SetQueryCache(query_cache)`.
        // Install only the independently locked state used by Scheduler, and do
        // it after every fallible construction step has succeeded.
        if let Some(state) = query_cache.samples_query_state() {
            scheduler.set_samples_query_state(state);
        }
        if let Some(state) = query_cache.tfb_query_state() {
            scheduler.set_tfb_query_state(state);
        }
        scheduler.set_query_runtime_state(query_cache.query_runtime_state());

        let fence_wait_handle = scheduler.wait_handle();
        Ok(Self {
            device: DeviceReference::new(vulkan_device),
            syncpoints,
            device_memory,
            channel_caches: ChannelSetupCaches::new(),
            scheduler: OwnerReference::new(scheduler),
            memory_allocator: NonNull::from(&mut *memory_allocator),
            state_tracker: OwnerReference::new(state_tracker),
            staging_pool,
            pipeline_cache,
            descriptor_pool,
            desc_queue,
            compute_pass_desc_queue,
            descriptor_buffer_ring,
            blit_image,
            fallback_uniform_buffer,
            fallback_uniform_memory,
            fallback_sampler,
            render_pass_cache,
            shader_cache,
            accelerate_dma,
            common_buffer_cache,
            texture_cache,
            query_cache,
            fence_manager: GenericFenceManager::new(true),
            fence_backend: VkFenceBackend::new(fence_wait_handle),
            wfi_event,
            draw_counter: 0,
            draw_sequence: 0,
            draw_skipped_pipeline: 0,
            driver_id,
            extended_dynamic_state_supported,
            extended_dynamic_state2_supported,
            extended_dynamic_state2_logic_op_supported,
            extended_dynamic_state3_blending_supported,
            extended_dynamic_state3_enables_supported,
            color_write_enable_supported,
            dynamic_state3_support,
            line_rasterization_supported,
            smooth_lines_supported,
            vertex_input_dynamic_state_supported,
            depth_bounds_supported,
            supports_d24_depth: vulkan_device.supports_d24_depth_buffer(),
            depth_range_unrestricted,
            nv_viewport_swizzle,
            extended_dynamic_state2,
            extended_dynamic_state3,
            color_write_enable,
            vertex_input_dynamic_state,
            draw_indirect_count,
            push_descriptor,
            max_viewports: max_viewports.min(NUM_VIEWPORTS as u32).max(1),
            max_vertex_input_attributes,
            max_vertex_input_bindings,
            max_compute_work_group_count,
            topology_list_primitive_restart_supported,
            patch_list_primitive_restart_supported,
            transform_feedback_supported,
            transform_feedback_draw_supported: vulkan_device.is_transform_feedback_draw_supported(),
            channel_memory_manager: None,
            channel_memory_manager_handle: None,
            gpu_tick_callback: None,
            invalidate_gpu_cache_callback: None,
        })
    }

    /// Wire the GPU tick source into the Vulkan query-cache owner.
    ///
    /// Port of the Vulkan rasterizer-side query-cache wiring edge. The active
    /// runtime Vulkan owner still lacks the full upstream `RendererBase`
    /// plumbing, but the query-cache ownership belongs here rather than in a
    /// local query shortcut.
    pub fn set_gpu_ticks_getter(&mut self, getter: crate::renderer_base::GpuTicksGetter) {
        self.query_cache.set_gpu_ticks_getter(getter);
    }

    pub fn set_guest_memory_writer(&mut self, writer: crate::renderer_base::GuestMemoryWriter) {
        self.texture_cache.set_guest_memory_writer(writer);
    }

    pub fn set_gpu_tick_callback(&mut self, callback: crate::renderer_base::GpuTickCallback) {
        self.gpu_tick_callback = Some(callback);
    }

    pub fn set_invalidate_gpu_cache_callback(
        &mut self,
        callback: crate::renderer_base::InvalidateGpuCacheCallback,
    ) {
        self.invalidate_gpu_cache_callback = Some(callback);
    }

    fn allocate_mesh_transport(
        &mut self, bytes: u64,
        usage: crate::vulkan_common::vulkan_memory_allocator::MemoryUsage,
        allocations: &mut Vec<super::staging_buffer_pool::StagingBufferRef>,
    ) -> Result<super::staging_buffer_pool::StagingBufferRef, String> {
        if bytes == 0 || bytes > self.device.get().get_max_storage_buffer_range()
            || bytes > u64::from(u32::MAX) {
            return Err(format!("Mesh transport size is not addressable: {bytes}"));
        }
        let buffer = self.staging_pool.request_buffer(bytes, usage, true)
            .ok_or_else(|| format!("Mesh transport allocation failed: {bytes} bytes"))?;
        allocations.push(buffer);
        Ok(buffer)
    }

    fn upload_mesh_mapping(
        &mut self,
        records: &[shader_recompiler::backend::spirv::spirv_emit_context::MeshDispatchRecord],
        allocations: &mut Vec<super::staging_buffer_pool::StagingBufferRef>,
    ) -> Result<vk::DescriptorBufferInfo, String> {
        let bytes = records.len() as u64 * 12;
        let buffer = self.allocate_mesh_transport(bytes,
            crate::vulkan_common::vulkan_memory_allocator::MemoryUsage::Upload, allocations)?;
        // This deferred allocation is host-visible, exclusively held here,
        // and not referenced by a GPU command until dispatch_mesh_stage.
        let mapped = unsafe { std::slice::from_raw_parts_mut(buffer.mapped, bytes as usize) };
        for (dst, record) in mapped.chunks_exact_mut(12).zip(records) {
            for (word, value) in dst.chunks_exact_mut(4).zip(record.words()) {
                word.copy_from_slice(&value.to_le_bytes());
            }
        }
        self.staging_pool.flush_deferred(&buffer);
        Ok(vk::DescriptorBufferInfo { buffer: buffer.buffer, offset: buffer.offset, range: bytes })
    }

    /// Compute replacement for DrawMeshTasksEXT's task/payload/mesh execution.
    /// Task completion is read back before allocating the exact child output.
    /// All transport remains deferred across Finish and any descriptor rollover.
    fn dispatch_mesh_programs(
        &mut self,
        programs: &super::pipeline_cache::MeshComputePrograms,
        draw: &Maxwell3DDrawView<'_>,
        read_gpu: &dyn Fn(u64, &mut [u8]),
        group_count: u32,
    ) -> Result<Option<MeshComputeOutput>, String> {
        use crate::vulkan_common::vulkan_memory_allocator::MemoryUsage;
        use shader_recompiler::backend::spirv::spirv_emit_context::{MeshDispatchRecord, MeshOutputLayout};
        if group_count == 0 { return Ok(None); }
        let max_groups = self.max_compute_work_group_count[0];
        if max_groups == 0 { return Err("Device has no compute workgroups".into()); }
        let layout = MeshOutputLayout::new(&programs.mesh_info);
        let capacity = mesh_output_record_capacity(layout, self.device.get().get_max_storage_buffer_range())?;
        let payload_words = programs.task_info.as_ref().map_or(programs.mesh_info.payload_words, |info| info.payload_words);
        if payload_words == 0 || payload_words < programs.mesh_info.payload_words
            || (programs.task.is_some() && payload_words < 2)
            || programs.task.is_some() != programs.task_info.is_some() {
            return Err("Incompatible task/mesh payload layouts".into());
        }
        let parent_count = if programs.task.is_some() { group_count } else { 1 };
        let payload_word_count = u64::from(parent_count) * u64::from(payload_words);
        if payload_word_count > u64::from(u32::MAX) {
            return Err("Task payload exceeds shader word addressing".into());
        }
        let payload_bytes = payload_word_count * 4;
        let mut allocations = Vec::new();
        let result = (|| -> Result<Option<MeshComputeOutput>, String> {
            let payload = self.allocate_mesh_transport(payload_bytes, MemoryUsage::DeviceLocal, &mut allocations)?;
            let payload_descriptor = vk::DescriptorBufferInfo {
                buffer: payload.buffer, offset: payload.offset, range: payload_bytes,
            };
            self.scheduler.request_outside_render_pass_operation_context();
            let device = self.device;
            self.scheduler.record(move |cmdbuf| unsafe {
                device.get().get_logical().cmd_fill_buffer(cmdbuf, payload.buffer, payload.offset, payload_bytes, 0);
            });
            let mut image_views = Vec::new();
            // Bound speculative memory independently of device addressing.
            // This is a performance selection only: larger draws retain the
            // exact readback path, rather than clipping the guest count.
            const MAX_RESERVED_OUTPUT_BYTES: u64 = 1024 * 1024;
            let stride_bytes = u64::from(layout.stride_words) * 4;
            let mapping_limit = max_groups.min((self.device.get().get_max_storage_buffer_range() / 12) as u32);
            let reserved_plan = programs.task_workgroup_bound.filter(|_| programs.task.is_some())
                .and_then(|bound| MeshTaskDispatchPlan::bounded(group_count, payload_words, bound,
                    mapping_limit, capacity.min((MAX_RESERVED_OUTPUT_BYTES / stride_bytes) as u32)).ok());
            let mut reserved_output = None;
            let reserved_plan = reserved_plan.filter(|plan| {
                if plan.records.is_empty() { return true; }
                reserved_output = self.allocate_mesh_transport(plan.records.len() as u64 * stride_bytes,
                    MemoryUsage::DeviceLocal, &mut allocations).ok();
                reserved_output.is_some()
            });
            // maxDrawCount is only a host limit here: the proven task bound
            // guarantees the GPU count is never clipped by it.
            let task_count = reserved_output.is_some()
                .then_some(programs.task_workgroup_bound).flatten()
                .filter(|bound| self.draw_indirect_count.is_some()
                    && *bound <= self.device.get().get_max_draw_indirect_count())
                .map(|child_capacity| MeshTaskIndirectCount {
                    buffer: payload, parent_count: group_count, payload_words, child_capacity,
                });
            let plan = if let Some(task) = &programs.task {
                let mapping_limit = max_groups.min((self.device.get().get_max_storage_buffer_range() / 12) as u32);
                if mapping_limit == 0 { return Err("Mapping buffer cannot hold a task group".into()); }
                for first in (0..group_count).step_by(mapping_limit as usize) {
                    let count = (group_count - first).min(mapping_limit);
                    let mut records = Vec::new();
                    records.try_reserve_exact(count as usize).map_err(|_| "Task mapping allocation failed")?;
                    for parent in first..first + count {
                        records.push(MeshDispatchRecord { guest_workgroup_x: parent,
                            payload_base_words: parent * payload_words, output_record: 0 });
                    }
                    let mapping = self.upload_mesh_mapping(&records, &mut allocations)?;
                    let prepared = self.dispatch_mesh_stage(task, draw, read_gpu, mapping,
                        payload_descriptor, None, count).map_err(|e| format!("Task dispatch: {e:?}"))?;
                    image_views.extend(prepared.views);
                }
                if let Some(plan) = reserved_plan {
                    plan
                } else {
                let readback = self.allocate_mesh_transport(payload_bytes, MemoryUsage::Download, &mut allocations)?;
                self.scheduler.request_outside_render_pass_operation_context();
                let device = self.device;
                self.scheduler.record(move |cmdbuf| unsafe {
                    let logical = device.get().get_logical();
                    let task_done = vk::MemoryBarrier::default()
                        .src_access_mask(vk::AccessFlags::SHADER_WRITE).dst_access_mask(vk::AccessFlags::TRANSFER_READ);
                    logical.cmd_pipeline_barrier(cmdbuf, vk::PipelineStageFlags::COMPUTE_SHADER,
                        vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[task_done], &[], &[]);
                    logical.cmd_copy_buffer(cmdbuf, payload.buffer, readback.buffer,
                        &[vk::BufferCopy { src_offset: payload.offset, dst_offset: readback.offset, size: payload_bytes }]);
                    let host_ready = vk::MemoryBarrier::default()
                        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE).dst_access_mask(vk::AccessFlags::HOST_READ);
                    logical.cmd_pipeline_barrier(cmdbuf, vk::PipelineStageFlags::TRANSFER,
                        vk::PipelineStageFlags::HOST, vk::DependencyFlags::empty(), &[host_ready], &[], &[]);
                });
                {
                    // Host fallback's task-count readback is a GPU fence wait.
                    let _scope = ProfileScope::new(ProfileZone::Fence);
                    self.scheduler.finish();
                }
                self.staging_pool.invalidate_deferred(&readback);
                let bytes = unsafe { std::slice::from_raw_parts(readback.mapped, payload_bytes as usize) };
                let mut words = Vec::new();
                words.try_reserve_exact(payload_word_count as usize).map_err(|_| "Task readback allocation failed")?;
                words.extend(bytes.chunks_exact(4).map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap())));
                MeshTaskDispatchPlan::from_task_payloads(&words, payload_words, mapping_limit, capacity)?
                }
            } else {
                let mapping_limit = max_groups.min((self.device.get().get_max_storage_buffer_range() / 12) as u32);
                MeshTaskDispatchPlan::direct(group_count, mapping_limit, capacity)?
            };
            if plan.records.is_empty() { return Ok(None); }
            let stride_bytes = u64::from(layout.stride_words) * 4;
            let output_bytes = plan.records.len() as u64 * stride_bytes;
            let output = if let Some(output) = reserved_output {
                // Inactive child groups return before touching their records.
                // Zero indirect vertex/instance counts suppress those draws.
                self.scheduler.request_outside_render_pass_operation_context();
                let device = self.device;
                self.scheduler.record(move |cmdbuf| unsafe {
                    device.get().get_logical().cmd_fill_buffer(cmdbuf, output.buffer,
                        output.offset, output_bytes, 0);
                });
                output
            } else {
                self.allocate_mesh_transport(output_bytes, MemoryUsage::DeviceLocal, &mut allocations)?
            };
            let descriptor = vk::DescriptorBufferInfo { buffer: output.buffer, offset: output.offset, range: output_bytes };
            for batch in &plan.batches {
                let mapping = self.upload_mesh_mapping(&plan.records[batch.clone()], &mut allocations)?;
                let prepared = self.dispatch_mesh_stage(&programs.mesh, draw, read_gpu, mapping,
                    payload_descriptor, Some(descriptor), batch.len() as u32)
                    .map_err(|e| format!("Mesh dispatch: {e:?}"))?;
                image_views.extend(prepared.views);
            }
            // Raw output attributes and embedded indirect headers share this
            // allocation; make both consumers visible before returning it.
            let device = self.device;
            self.scheduler.record(move |cmdbuf| unsafe {
                let ready = vk::MemoryBarrier::default().src_access_mask(vk::AccessFlags::SHADER_WRITE | vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ | vk::AccessFlags::INDIRECT_COMMAND_READ);
                device.get().get_logical().cmd_pipeline_barrier(cmdbuf, vk::PipelineStageFlags::COMPUTE_SHADER | vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::ALL_GRAPHICS | vk::PipelineStageFlags::DRAW_INDIRECT,
                    vk::DependencyFlags::empty(), &[ready], &[], &[]);
            });
            Ok(Some(MeshComputeOutput { buffer: output, descriptor, record_count: plan.records.len() as u32,
                stride_bytes, image_views, task_count }))
        })();
        let retained_output = result.as_ref().ok().and_then(|output| output.as_ref());
        // On failure, queued commands still retire normally: FreeDeferred
        // stamps the current scheduler tick instead of immediately recycling.
        for mut buffer in allocations {
            if !retained_output.is_some_and(|output| output.retains(buffer.buffer)) {
                self.staging_pool.free_deferred(&mut buffer);
            }
        }
        result
    }

    /// Compute adaptation of a task/mesh launch. Resource ownership remains
    /// Maxwell graphics; command submission and the pre-dispatch barrier stay
    /// here, like upstream RasterizerVulkan::DispatchCompute. The pipeline
    /// cache and transport allocator must retain their owners through GPU use.
    #[allow(clippy::too_many_arguments)]
    fn dispatch_mesh_stage(
        &mut self,
        stage: &super::compute_pipeline::MeshComputeStage,
        draw: &Maxwell3DDrawView<'_>,
        read_gpu: &dyn Fn(u64, &mut [u8]),
        mapping: vk::DescriptorBufferInfo,
        payload: vk::DescriptorBufferInfo,
        output: Option<vk::DescriptorBufferInfo>,
        group_count: u32,
    ) -> Result<super::graphics_pipeline::PreparedMeshDescriptors, vk::Result> {
        if !mesh_dispatch_mapping_fits(group_count, self.max_compute_work_group_count[0], mapping.range) {
            // The caller batches dispatch records; never silently truncate the
            // guest's child group count to a host-device limit.
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }
        let texture_mutex: *const _ = &self.texture_cache.base.mutex;
        let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
        lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex,
            _buffer_guard, _texture_guard);
        let (set, prepared) = stage.prepare_descriptor_set(
            draw, read_gpu, &mut self.scheduler, &mut self.common_buffer_cache,
            &mut self.texture_cache, &mut self.desc_queue, self.fallback_sampler,
            draw.is_indexed(), &self.descriptor_pool, mapping, payload, output,
        )?;
        stage.record_bind(&mut self.scheduler, set, &prepared);
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let logical = device.get().get_logical();
            let barrier = vk::MemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE);
            logical.cmd_pipeline_barrier(cmdbuf,
                vk::PipelineStageFlags::ALL_GRAPHICS | vk::PipelineStageFlags::COMPUTE_SHADER
                    | vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::COMPUTE_SHADER, vk::DependencyFlags::empty(),
                &[barrier], &[], &[]);
            logical.cmd_dispatch(cmdbuf, group_count, 1, 1);
        });
        Ok(prepared)
    }

    /// Draw the compute mesh output with generated vertices and the unchanged
    /// guest fragment program. The caller retains output storage until the last
    /// indirect command has been recorded, including command-buffer rollover.
    #[allow(clippy::too_many_arguments)]
    fn draw_mesh_output(
        &mut self, draw: &mut Maxwell3DDrawView<'_>, output: &MeshComputeOutput,
        mesh: &shader_recompiler::shader_info::MeshInfo,
        dirty_flags: &mut [bool; 256], dirty_flags_are_live: bool,
        zpass_pixel_count_enabled: bool,
        read_gpu: &dyn Fn(u64, &mut [u8]), read_gpu_unsafe: &dyn Fn(u64, &mut [u8]) -> bool,
    ) -> bool {
        let pipeline = {
            let _scope = ProfileScope::new(ProfileZone::PipelineLookup);
            self.pipeline_cache.current_graphics_pipeline(draw, &mut self.shader_cache)
        };
        let Some(gp) = pipeline else {
            return false;
        };
        if !dirty_flags_are_live { *dirty_flags = *draw.dirty_flags(); }
        if let Some(memory) = self.channel_memory_manager_handle {
            gp.set_engine(draw, dirty_flags, memory, self.push_descriptor.clone(), self.fallback_sampler);
        } else {
            gp.set_engine_with_readers(draw, dirty_flags, read_gpu, read_gpu_unsafe,
                self.push_descriptor.clone(), self.fallback_sampler);
        }
        gp.set_mesh_output(output.descriptor, &output.image_views);
        let old_tick = self.scheduler.current_tick();
        let configured = {
            let _scope = ProfileScope::new(ProfileZone::Configure);
            gp.configure(false)
        };
        if !configured { return false; }
        let dynamic_vertex_input = gp.has_dynamic_vertex_input();
        if self.scheduler.current_tick() != old_tick && !dirty_flags_are_live {
            self.state_tracker.apply_command_buffer_invalidation(dirty_flags);
        }
        {
            let _scope = ProfileScope::new(ProfileZone::DynamicState);
            self.update_dynamic_states(draw, dirty_flags);
        }
        // Mesh output topology replaces the guest draw's group-launch topology.
        // Track the actual host state, and dirty overrides so ordinary draws
        // restore their own restart/bias/input values afterward.
        let topology = match mesh.topology {
            0 => PrimitiveTopology::Points, 1 => PrimitiveTopology::Lines, _ => PrimitiveTopology::Triangles,
        };
        self.state_tracker.change_primitive_topology(topology);
        let rasterizer = draw.rasterizer();
        let bias = match topology {
            PrimitiveTopology::Points => rasterizer.polygon_offset_point_enable,
            PrimitiveTopology::Lines => rasterizer.polygon_offset_line_enable,
            _ => rasterizer.polygon_offset_fill_enable,
        };
        let device = self.device;
        let eds1 = self.extended_dynamic_state_supported;
        let eds2 = self.extended_dynamic_state2_supported;
        let vertex_input = dynamic_vertex_input.then(|| self.vertex_input_dynamic_state.as_ref().unwrap().clone());
        self.scheduler.record(move |cmdbuf| unsafe {
            let logical = device.get().get_logical();
            if eds1 { logical.cmd_set_primitive_topology(cmdbuf, maxwell_to_vk::primitive_topology(topology)); }
            if eds2 {
                logical.cmd_set_primitive_restart_enable(cmdbuf, false);
                logical.cmd_set_depth_bias_enable(cmdbuf, bias);
            }
            if let Some(extension) = vertex_input {
                (extension.cmd_set_vertex_input_ext)(cmdbuf, 0, std::ptr::null(), 0, std::ptr::null());
            }
        });
        for flag in [super::state_tracker::dirty::PRIMITIVE_RESTART_ENABLE,
            super::state_tracker::dirty::DEPTH_BIAS_ENABLE, super::state_tracker::dirty::VERTEX_INPUT] {
            dirty_flags[flag as usize] = true;
            draw.set_dirty_flag(flag);
        }
        self.query_cache.notify_segment(true);
        self.handle_transform_feedback();
        self.query_cache.counter_enable(&mut self.scheduler, QueryType::ZPassPixelCount64 as u32,
            zpass_pixel_count_enabled);
        let buffer = output.buffer.buffer;
        let offset = output.buffer.offset;
        let count = output.record_count;
        let stride = output.stride_bytes;
        let max_draws = self.device.get().get_max_draw_indirect_count();
        let task_count = output.task_count;
        let indirect_count = self.draw_indirect_count.clone();
        let _record_scope = ProfileScope::new(ProfileZone::DrawRecord);
        self.scheduler.record(move |cmdbuf| unsafe {
            if let (Some(count), Some(extension)) = (task_count, indirect_count) {
                for (relative_offset, count_offset) in count.draw_offsets(stride) {
                    extension.cmd_draw_indirect_count(cmdbuf, buffer, offset + relative_offset,
                        count.buffer.buffer, count_offset, count.child_capacity, stride as u32);
                }
                return;
            }
            // Device creation requires and enables multiDrawIndirect. The
            // generated vertex shader addresses records via firstVertex and
            // never reads DrawIndex, so batching preserves every invocation.
            for (relative_offset, draws) in mesh_indirect_draw_batches(count, stride, max_draws) {
                device.get().get_logical().cmd_draw_indirect(cmdbuf, buffer,
                    offset + relative_offset, draws, stride as u32);
            }
        });
        true
    }

    /// Main draw entry point: compile/cache pipeline, configure resources,
    /// update dynamic state and record the draw (upstream RasterizerVulkan::Draw).
    fn prepare_draw(
        &mut self,
        draw: &mut Maxwell3DDrawView<'_>,
        instance_count: u32,
        zpass_pixel_count_enabled: bool,
        indirect_params: Option<crate::engines::draw_manager::IndirectParams>,
        dirty_flags: &mut [bool; 256],
        dirty_flags_are_live: bool,
        read_gpu: &dyn Fn(u64, &mut [u8]),
        read_gpu_unsafe: &dyn Fn(u64, &mut [u8]) -> bool,
    ) {
        let _draw_scope = ProfileScope::new(ProfileZone::Draw);
        let _gpu_tick_guard = GpuTickGuard::new(self.gpu_tick_callback.as_ref());
        // 1. Periodic flush
        {
            let _scope = ProfileScope::new(ProfileZone::FlushWork);
            self.flush_work();
            if let Some(memory_manager) = self.channel_memory_manager.as_ref().cloned() {
                memory_manager.lock().flush_caching();
            }
        }

        // These launch-time switches supplement the upstream mesh branch.
        // Cache them like upstream's static diagnostic switches: getenv on
        // Windows otherwise allocates UTF-16 storage for every ordinary draw.
        static MESH_DRAW: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *MESH_DRAW.get_or_init(|| std::env::var_os("RUZU_NX2_MESH_DRAW").is_some())
            && self.shader_cache.current_maxwell3d().is_some_and(|maxwell| maxwell.is_mesh_enabled()
                && maxwell.get_native_macro_engine_read(|engine| engine.native_turing)) {
            if indirect_params.is_some() {
                // Eglantine RasterizerVulkan::DrawIndirect likewise rejects
                // task/mesh indirect commands: their launch layout differs.
                static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    log::warn!("Indirect task/mesh draw skipped");
                }
                return;
            }
            let programs = {
                let _scope = ProfileScope::new(ProfileZone::PipelineLookup);
                self.pipeline_cache.current_mesh_compute_programs(&mut self.shader_cache)
            };
            let Some(programs) = programs else { return; };
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
            lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _mesh_buffer_guard, _mesh_texture_guard);
            let groups = make_draw_params(draw, instance_count).num_vertices;
            let output = {
                let _scope = ProfileScope::new(ProfileZone::Compute);
                self.dispatch_mesh_programs(&programs, draw, read_gpu, groups)
            };
            match output {
                Ok(Some(mut output)) => {
                    let drawn = self.draw_mesh_output(draw, &output, &programs.mesh_info,
                        dirty_flags, dirty_flags_are_live, zpass_pixel_count_enabled, read_gpu, read_gpu_unsafe);
                    static DRAWS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                    let number = DRAWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if number < 16 || number.is_power_of_two() {
                        log::info!("Mesh graphics draw #{number}: groups={groups} records={} drawn={drawn}", output.record_count);
                    }
                    self.staging_pool.free_deferred(&mut output.buffer);
                    if let Some(count) = &mut output.task_count {
                        self.staging_pool.free_deferred(&mut count.buffer);
                    }
                }
                Ok(None) => {}
                Err(error) => log::error!("Mesh draw transport failed: {error}"),
            }
            return;
        }

        // 2. Compile or lookup cached pipeline
        let pipeline_result = {
            let _scope = ProfileScope::new(ProfileZone::PipelineLookup);
            self.pipeline_cache.current_graphics_pipeline(draw, &mut self.shader_cache)
        };
        let gp = match pipeline_result {
            Some(gp) => gp,
            None => {
                self.draw_skipped_pipeline = self.draw_skipped_pipeline.wrapping_add(1);
                // A skipped draw leaves the previous frame's pixels in place;
                // with LOAD attachments this accumulates visibly (e.g. the
                if self.draw_skipped_pipeline <= 16 || self.draw_skipped_pipeline.is_power_of_two()
                {
                    log::warn!(
                        "[DRAW_SKIP] #{} pipeline compilation failed (draw={} rt0=0x{:X} fmt={} topology={:?} indexed={})",
                        self.draw_skipped_pipeline,
                        self.draw_counter,
                        draw.render_target(0).address,
                        draw.render_target(0).format,
                        draw.draw_state().topology,
                        draw.is_indexed(),
                    );
                }
                return;
            }
        };
        // `FixedPipelineState::Refresh` consumes Maxwell dirty flags before
        // Configure. Production draws already use that live array directly;
        // snapshot-only callers refresh their owned fallback here.
        if !dirty_flags_are_live {
            *dirty_flags = *draw.dirty_flags();
        }
        // Serialize every common-buffer-cache access on this draw
        // (uniform/storage descriptor binding AND the geometry binding below)
        // against concurrent CPU-write invalidation. A guest write on another
        // core reaches `BufferCache::on_cpu_write` -> `delete_buffer` ->
        // `slot_buffers.take()`, which frees the very slots this path reads via
        // `slot_buffers[buffer_id]` (unguarded in release, where SlotVector's
        // validate_index is a debug_assert). Without this lock the GPU thread
        // can index a slot the CPU thread just freed -> SlotVector panic /
        // use-after-free. The mutexes are reentrant, so the texture lock taken
        // inside `GraphicsPipeline::configure` is fine. Matches the locking the
        // async-flush paths already use for these two caches.
        let bc_draw_texture_mutex: *const _ = &self.texture_cache.base.mutex;
        let bc_draw_buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
        lock_two_reentrant_mutexes!(
            bc_draw_buffer_mutex,
            bc_draw_texture_mutex,
            _bc_draw_buffer_guard,
            _bc_draw_texture_guard
        );
        if let Some(gpu_memory) = self.channel_memory_manager_handle {
            gp.set_engine(
                draw,
                dirty_flags,
                gpu_memory,
                self.push_descriptor.clone(),
                self.fallback_sampler,
            );
        } else {
            gp.set_engine_with_readers(
                draw,
                dirty_flags,
                read_gpu,
                read_gpu_unsafe,
                self.push_descriptor.clone(),
                self.fallback_sampler,
            );
        }
        let command_buffer_tick_before_configure = self.scheduler.current_tick();
        let configured = {
            let _scope = ProfileScope::new(ProfileZone::Configure);
            gp.configure(draw.is_indexed())
        };
        if !configured {
            self.draw_skipped_pipeline = self.draw_skipped_pipeline.wrapping_add(1);
            warn!("RasterizerVulkan: draw skipped because graphics pipeline configuration failed");
            return;
        }
        if self.scheduler.current_tick() != command_buffer_tick_before_configure {
            // `StateTracker::InvalidateState` already writes the live Maxwell
            // flags, exactly as in Eden. Snapshot-only callers still need the
            // same invalidation copied into their fallback array.
            if !dirty_flags_are_live {
                self.state_tracker
                    .apply_command_buffer_invalidation(dirty_flags);
            }
            // Configure binds geometry before descriptor allocation. If the
            // latter rolls the command buffer, repeat those bindings on its
            // successor before recording the draw.
            self.common_buffer_cache
                .bind_host_geometry_buffers(draw.is_indexed());
        }
        let indirect_binding = indirect_params.map(|params| {
            let (buffer_id, offset) = self.common_buffer_cache.get_draw_indirect_buffer();
            let buffer = vk::Buffer::from_raw(
                self.common_buffer_cache
                    .resolve_backend_buffer_raw(buffer_id),
            );
            let count = params.include_count.then(|| {
                let (count_buffer_id, count_offset) =
                    self.common_buffer_cache.get_draw_indirect_count();
                (
                    vk::Buffer::from_raw(
                        self.common_buffer_cache
                            .resolve_backend_buffer_raw(count_buffer_id),
                    ),
                    count_offset,
                )
            });
            (params, buffer, offset, count)
        });
        // The guards (`bc_draw_buffer_guard`/`bc_draw_texture_guard`) are held
        // through the rest of this function, i.e. across texture
        // materialization and the draw emission below, matching upstream
        // `RasterizerVulkan::PrepareDraw` which keeps
        // `scoped_lock{buffer_cache.mutex, texture_cache.mutex}` around
        // Configure AND draw_func (vk_rasterizer.cpp:223-233). Releasing after
        // binding would still let a concurrent CPU-write free a slot the draw
        // depends on before it is recorded. RAII drops them on every exit,
        // including every early-return path.

        // 6. Update dynamic states via dirty flags. Upstream requests the
        // render pass in `GraphicsPipeline::ConfigureDraw` before
        // `RasterizerVulkan::UpdateDynamicStates`.
        {
            let _scope = ProfileScope::new(ProfileZone::DynamicState);
            self.update_dynamic_states(draw, dirty_flags);
        }
        self.query_cache.notify_segment(true);
        self.handle_transform_feedback();
        self.query_cache.counter_enable(
            &mut self.scheduler,
            QueryType::ZPassPixelCount64 as u32,
            zpass_pixel_count_enabled,
        );

        // 7. Issue draw call
        let _record_scope = ProfileScope::new(ProfileZone::DrawRecord);
        if let Some((params, buffer, offset, count)) = indirect_binding {
            if buffer == vk::Buffer::null() {
                warn!("RasterizerVulkan::draw_indirect skipped: missing indirect buffer");
                return;
            }
            if params.is_byte_count {
                let Some(transform_feedback) = self.query_cache.transform_feedback_dispatch()
                else {
                    warn!("RasterizerVulkan::draw_indirect byte-count path requires VK_EXT_transform_feedback");
                    return;
                };
                self.scheduler.record(move |cmdbuf| unsafe {
                    (transform_feedback.cmd_draw_indirect_byte_count_ext)(
                        cmdbuf,
                        1,
                        0,
                        buffer,
                        offset as vk::DeviceSize,
                        0,
                        params.stride as u32,
                    );
                });
                return;
            }
            if let Some((count_buffer, count_offset)) = count {
                if count_buffer == vk::Buffer::null() {
                    warn!("RasterizerVulkan::draw_indirect skipped: missing count buffer");
                    return;
                }
                let Some(draw_indirect_count) = self.draw_indirect_count.clone() else {
                    warn!("RasterizerVulkan::draw_indirect skipped: VK_KHR_draw_indirect_count is unavailable");
                    return;
                };
                self.scheduler.record(move |cmdbuf| unsafe {
                    if params.is_indexed {
                        draw_indirect_count.cmd_draw_indexed_indirect_count(
                            cmdbuf,
                            buffer,
                            offset as vk::DeviceSize,
                            count_buffer,
                            count_offset as vk::DeviceSize,
                            params.max_draw_counts as u32,
                            params.stride as u32,
                        );
                    } else {
                        draw_indirect_count.cmd_draw_indirect_count(
                            cmdbuf,
                            buffer,
                            offset as vk::DeviceSize,
                            count_buffer,
                            count_offset as vk::DeviceSize,
                            params.max_draw_counts as u32,
                            params.stride as u32,
                        );
                    }
                });
            } else {
                let device = self.device;
                self.scheduler.record(move |cmdbuf| unsafe {
                    let device = device.get().get_logical();
                    if params.is_indexed {
                        device.cmd_draw_indexed_indirect(
                            cmdbuf,
                            buffer,
                            offset as vk::DeviceSize,
                            params.max_draw_counts as u32,
                            params.stride as u32,
                        );
                    } else {
                        device.cmd_draw_indirect(
                            cmdbuf,
                            buffer,
                            offset as vk::DeviceSize,
                            params.max_draw_counts as u32,
                            params.stride as u32,
                        );
                    }
                });
                if is_active() && *common::settings::values().gpu_log_vulkan_calls.get_value() {
                    let (call_name, log_params) = indirect_draw_log_info(
                        params.is_indexed,
                        params.max_draw_counts,
                        params.stride,
                    );
                    get_instance().log_vulkan_call(
                        call_name,
                        &log_params,
                        vk::Result::SUCCESS.as_raw(),
                    );
                }
            }
        } else {
            let draw_params = make_draw_params(draw, instance_count);
            if draw_params.is_indexed {
                let device = self.device;
                self.scheduler.record(move |cmdbuf| unsafe {
                    let device = device.get().get_logical();
                    device.cmd_draw_indexed(
                        cmdbuf,
                        draw_params.num_vertices,
                        draw_params.num_instances,
                        draw_params.first_index,
                        draw_params.base_vertex,
                        draw_params.base_instance,
                    );
                });
            } else {
                let device = self.device;
                self.scheduler.record(move |cmdbuf| unsafe {
                    let device = device.get().get_logical();
                    device.cmd_draw(
                        cmdbuf,
                        draw_params.num_vertices,
                        draw_params.num_instances,
                        draw_params.base_vertex as u32,
                        draw_params.base_instance,
                    );
                });
            }
            if is_active() && *common::settings::values().gpu_log_vulkan_calls.get_value() {
                let (call_name, params) = direct_draw_log_info(draw_params);
                get_instance().log_vulkan_call(call_name, &params, vk::Result::SUCCESS.as_raw());
            }
        }
    }

    /// Port of `RasterizerVulkan::HandleTransformFeedback`.
    fn handle_transform_feedback(&mut self) {
        let Some((enabled, supported, tessellation_enabled)) =
            self.query_cache.transform_feedback_status()
        else {
            return;
        };
        if !supported {
            static WARN_UNSUPPORTED: Once = Once::new();
            WARN_UNSUPPORTED.call_once(|| {
                if enabled {
                    warn!(
                        "Transform feedback requested by guest but VK_EXT_transform_feedback is unavailable; queries disabled"
                    );
                } else {
                    info!("VK_EXT_transform_feedback not available on device");
                }
            });
            return;
        }
        self.query_cache.counter_enable(
            &mut self.scheduler,
            QueryType::StreamingByteCount as u32,
            enabled,
        );
        if enabled {
            if is_active() {
                get_instance()
                    .log_extension_usage("VK_EXT_transform_feedback", "HandleTransformFeedback");
            }
            if tessellation_enabled {
                warn!("Transform feedback with tessellation shaders is not implemented");
            }
        }
    }

    fn flush_work(&mut self) {
        self.draw_counter = self.draw_counter.wrapping_add(1);
        if self.draw_counter & Self::DISPATCH_THRESHOLD != Self::DISPATCH_THRESHOLD {
            return;
        }
        if self.draw_counter < Self::FLUSH_THRESHOLD {
            self.scheduler.dispatch_work();
            return;
        }
        self.scheduler.flush();
        self.draw_counter = 0;
        self.state_tracker.invalidate_command_buffer_state();
    }

    /// Submit and wait for all GPU work to complete.
    pub fn finish(&mut self) {
        // End render pass and submit
        self.scheduler.finish();
        self.draw_counter = 0;
        self.state_tracker.invalidate_command_buffer_state();
    }

    fn should_wait_async_flushes(&self) -> bool {
        let cache_wait = {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
            lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);
            self.texture_cache.should_wait_async_flushes()
                || self.common_buffer_cache.should_wait_async_flushes()
        };
        cache_wait || self.query_cache.should_wait_async_flushes()
    }

    fn should_flush_async(&self) -> bool {
        let cache_flush = {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
            lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);
            self.texture_cache.has_uncommitted_flushes()
                || self.common_buffer_cache.has_uncommitted_flushes()
        };
        cache_flush || self.query_cache.has_uncommitted_flushes()
    }

    fn pop_async_flushes(&mut self) {
        {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
            lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);
            self.texture_cache.pop_async_flushes();
            self.common_buffer_cache.pop_async_flushes();
        }
        self.query_cache.pop_async_flushes();
    }

    fn commit_async_flushes(&mut self) {
        {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
            lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);
            self.texture_cache.commit_async_flushes();
            self.common_buffer_cache.commit_async_flushes();
        }
        let this = self as *mut Self;
        self.query_cache
            .commit_async_flushes(&mut self.scheduler, move |operation| unsafe {
                (*this).sync_operation(operation);
            });
    }

    /// Callback adaptation of upstream `FenceManager::SignalOrdering`, which
    /// locks only the buffer cache and calls `BufferCache::AccumulateFlushes`.
    fn accumulate_flushes(&mut self) {
        unsafe {
            let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            self.common_buffer_cache.accumulate_flushes();
        }
    }

    fn queue_fence(&mut self, fence: &mut VkFence) {
        self.fence_backend.queue_fence(fence, &mut self.scheduler);
    }

    fn is_fence_signaled(&self, fence: &VkFence) -> bool {
        self.fence_backend.is_fence_signaled(fence)
    }

    fn wait_fence(&mut self, fence: &VkFence) {
        self.fence_backend.wait_fence(fence);
    }

    // â”€â”€ Dynamic state update methods â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[inline(never)]
    fn update_dynamic_states(
        &mut self,
        draw: &mut Maxwell3DDrawView<'_>,
        dirty_flags: &mut [bool; 256],
    ) {
        use super::state_tracker::dirty;

        let topology = draw.draw_state().topology;
        let topology_changed = self.state_tracker.change_primitive_topology(topology);
        if topology_changed {
            dirty_flags[dirty::DEPTH_BIAS_ENABLE as usize] = true;
            dirty_flags[dirty::PRIMITIVE_RESTART_ENABLE as usize] = true;
            draw.set_dirty_flag(dirty::DEPTH_BIAS_ENABLE);
            draw.set_dirty_flag(dirty::PRIMITIVE_RESTART_ENABLE);
        }

        self.update_viewports_state(draw);
        self.update_scissors_state(draw);
        self.update_depth_bias(draw);
        self.update_blend_constants(draw);
        self.update_depth_bounds(draw);
        self.update_stencil_faces(draw);
        self.update_line_width(draw);
        if self.extended_dynamic_state_supported {
            self.update_cull_mode(draw);
            self.update_depth_compare_op(draw);
            self.update_front_face(draw);
            self.update_stencil_op(draw);

            if self.state_tracker.touch_state_enable() {
                self.update_depth_bounds_test_enable(draw);
                self.update_depth_test_enable(draw);
                self.update_depth_write_enable(draw);
                self.update_stencil_test_enable(draw);
            }
            if topology_changed {
                let topology = maxwell_to_vk::primitive_topology(topology);
                let device = self.device;
                self.scheduler.record(move |cmdbuf| unsafe {
                    let device = device.get().get_logical();
                    device.cmd_set_primitive_topology(cmdbuf, topology);
                });
            }
        }
        if self.extended_dynamic_state2_supported {
            self.update_primitive_restart_enable(draw);
            self.update_rasterizer_discard_enable(draw);
            self.update_depth_bias_enable(draw);
        }
        if self.extended_dynamic_state2_logic_op_supported {
            self.update_logic_op(draw);
        }
        if self.extended_dynamic_state3_enables_supported {
            // AMD workaround: LogicOp is incompatible with float render
            // targets. Keep the driver guard outside the attribute scan, as
            // upstream does, and persist the resulting register mutation.
            if matches!(
                self.driver_id,
                vk::DriverId::AMD_OPEN_SOURCE | vk::DriverId::AMD_PROPRIETARY
            ) {
                let has_float_attribute = (0..32).any(|index| {
                    // Upstream reads the `VertexAttribute::type` bitfield
                    // directly from `regs.vertex_attrib_format` here.
                    ((draw.vertex_attrib_raw(index) >> 27) & 0x7)
                        == VertexAttribType::Float.to_raw()
                });
                if draw.logic_op().enabled {
                    draw.set_logic_op_enabled(!has_float_attribute);
                }
            }
            self.update_logic_op_enable(draw);
            self.update_depth_clamp_enable(draw);
            self.update_line_rasterization_mode(draw);
            self.update_line_stipple_enable(draw);
            self.update_conservative_rasterization_mode(draw);
            self.update_alpha_to_coverage_enable(draw);
            self.update_alpha_to_one_enable(draw);
        }
        if self.extended_dynamic_state3_blending_supported {
            self.update_blending(draw);
        } else if self.color_write_enable_supported {
            self.update_color_write_enable(draw);
        }
        if self.vertex_input_dynamic_state_supported {
            let has_dynamic_vertex_input = self
                .pipeline_cache
                .current_graphics_pipeline(draw, &mut self.shader_cache)
                .is_some_and(|pipeline| pipeline.has_dynamic_vertex_input());
            if has_dynamic_vertex_input {
                self.update_vertex_input(draw, dirty_flags);
            }
        }
    }

    #[inline(never)]
    fn update_logic_op_enable(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_logic_op_enable() {
            return;
        }
        if !self.dynamic_state3_support.logic_op_enable {
            return;
        }
        let enabled = draw.logic_op().enabled;
        let extension = self
            .extended_dynamic_state3
            .as_ref()
            .expect("dynamic state 3 loader missing")
            .clone();
        self.scheduler.record(move |cmdbuf| unsafe {
            extension.cmd_set_logic_op_enable(cmdbuf, enabled);
        });
    }

    #[inline(never)]
    fn update_depth_clamp_enable(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_depth_clamp_enable() {
            return;
        }
        if !self.dynamic_state3_support.depth_clamp_enable {
            return;
        }
        let enabled = draw.depth_clamp_enabled();
        let extension = self
            .extended_dynamic_state3
            .as_ref()
            .expect("dynamic state 3 loader missing")
            .clone();
        self.scheduler.record(move |cmdbuf| unsafe {
            extension.cmd_set_depth_clamp_enable(cmdbuf, enabled);
        });
    }

    #[inline(never)]
    fn update_conservative_rasterization_mode(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_conservative_rasterization_mode()
            || !self.dynamic_state3_support.conservative_rasterization_mode
        {
            return;
        }
        let mode = if draw.conservative_raster_enable() {
            vk::ConservativeRasterizationModeEXT::OVERESTIMATE
        } else {
            vk::ConservativeRasterizationModeEXT::DISABLED
        };
        let extension = self
            .extended_dynamic_state3
            .as_ref()
            .expect("dynamic state 3 loader missing")
            .clone();
        self.scheduler.record(move |cmdbuf| unsafe {
            extension.cmd_set_conservative_rasterization_mode(cmdbuf, mode);
        });
    }

    #[inline(never)]
    fn update_line_rasterization_mode(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.line_rasterization_supported
            || !self.state_tracker.touch_line_rasterization_mode()
            || !self.dynamic_state3_support.line_rasterization_mode
        {
            return;
        }
        let mode = if draw.line_state().line_anti_alias_enable && self.smooth_lines_supported {
            vk::LineRasterizationModeEXT::RECTANGULAR_SMOOTH
        } else {
            vk::LineRasterizationModeEXT::RECTANGULAR
        };
        let extension = self
            .extended_dynamic_state3
            .as_ref()
            .expect("dynamic state 3 loader missing")
            .clone();
        self.scheduler.record(move |cmdbuf| unsafe {
            extension.cmd_set_line_rasterization_mode(cmdbuf, mode);
        });
    }

    #[inline(never)]
    fn update_line_stipple_enable(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_line_stipple_enable()
            || !self.dynamic_state3_support.line_stipple_enable
        {
            return;
        }
        let enabled = draw.line_stipple().enabled;
        let extension = self
            .extended_dynamic_state3
            .as_ref()
            .expect("dynamic state 3 loader missing")
            .clone();
        self.scheduler.record(move |cmdbuf| unsafe {
            extension.cmd_set_line_stipple_enable(cmdbuf, enabled);
        });
    }

    #[inline(never)]
    fn update_alpha_to_coverage_enable(&mut self, draw: &mut Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_alpha_to_coverage_enable()
            || !self.dynamic_state3_support.alpha_to_coverage_enable
        {
            return;
        }
        let enabled = self
            .pipeline_cache
            .current_graphics_pipeline(draw, &mut self.shader_cache)
            .is_some_and(|pipeline| pipeline.supports_alpha_to_coverage())
            && draw.anti_alias_alpha_control().alpha_to_coverage;
        let extension = self
            .extended_dynamic_state3
            .as_ref()
            .expect("dynamic state 3 loader missing")
            .clone();
        self.scheduler.record(move |cmdbuf| unsafe {
            extension.cmd_set_alpha_to_coverage_enable(cmdbuf, enabled);
        });
    }

    #[inline(never)]
    fn update_alpha_to_one_enable(&mut self, draw: &mut Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_alpha_to_one_enable() {
            return;
        }
        if !self.dynamic_state3_support.alpha_to_one_enable {
            static WARN_ALPHA_TO_ONE: std::sync::Once = std::sync::Once::new();
            WARN_ALPHA_TO_ONE.call_once(|| {
                warn!("Alpha-to-one is not supported on this device; forcing it disabled");
            });
            return;
        }
        let enabled = self
            .pipeline_cache
            .current_graphics_pipeline(draw, &mut self.shader_cache)
            .is_some_and(|pipeline| pipeline.supports_alpha_to_one())
            && draw.anti_alias_alpha_control().alpha_to_one;
        let extension = self
            .extended_dynamic_state3
            .as_ref()
            .expect("dynamic state 3 loader missing")
            .clone();
        self.scheduler.record(move |cmdbuf| unsafe {
            extension.cmd_set_alpha_to_one_enable(cmdbuf, enabled);
        });
    }

    #[inline(never)]
    fn update_color_write_enable(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_color_mask() {
            return;
        }
        let enables = std::array::from_fn::<_, 8, _>(|index| {
            let mask = draw.color_mask(index);
            if mask.r || mask.g || mask.b || mask.a {
                vk::TRUE
            } else {
                vk::FALSE
            }
        });
        let functions = self
            .color_write_enable
            .as_ref()
            .expect("color-write-enable loader missing")
            .clone();
        self.scheduler.record(move |cmdbuf| unsafe {
            (functions.cmd_set_color_write_enable_ext)(
                cmdbuf,
                enables.len() as u32,
                enables.as_ptr(),
            );
        });
    }

    #[inline(never)]
    fn update_logic_op(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_logic_op() {
            return;
        }
        let raw = draw.logic_op().op;
        let op = if (0x1500..0x1510).contains(&raw) {
            vk::LogicOp::from_raw((raw - 0x1500) as i32)
        } else {
            vk::LogicOp::NO_OP
        };
        let extension = self
            .extended_dynamic_state2
            .as_ref()
            .expect("dynamic state 2 loader missing")
            .clone();
        self.scheduler.record(move |cmdbuf| unsafe {
            extension.cmd_set_logic_op(cmdbuf, op);
        });
    }

    #[inline(never)]
    fn update_blending(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_blending() {
            return;
        }
        let extension = self
            .extended_dynamic_state3
            .as_ref()
            .expect("dynamic state 3 loader missing")
            .clone();

        if self.state_tracker.touch_color_mask() {
            let masks = std::array::from_fn::<_, 8, _>(|index| {
                let mask = draw.color_mask(index);
                let mut flags = vk::ColorComponentFlags::empty();
                if mask.r {
                    flags |= vk::ColorComponentFlags::R;
                }
                if mask.g {
                    flags |= vk::ColorComponentFlags::G;
                }
                if mask.b {
                    flags |= vk::ColorComponentFlags::B;
                }
                if mask.a {
                    flags |= vk::ColorComponentFlags::A;
                }
                flags
            });
            let extension = extension.clone();
            self.scheduler.record(move |cmdbuf| unsafe {
                extension.cmd_set_color_write_mask(cmdbuf, 0, &masks);
            });
        }

        if self.state_tracker.touch_blend_enable() {
            let enables = std::array::from_fn::<_, 8, _>(|index| {
                let format = crate::surface::pixel_format_from_render_target_format(
                    draw.render_target(index).format,
                );
                (!crate::surface::is_pixel_format_integer(format) && draw.blend_at(index).enabled)
                    .into()
            });
            let extension = extension.clone();
            self.scheduler.record(move |cmdbuf| unsafe {
                extension.cmd_set_color_blend_enable(cmdbuf, 0, &enables);
            });
        }

        if self.state_tracker.touch_blend_equations() {
            let blend_equation =
                |blend: crate::engines::maxwell_3d::BlendInfo| vk::ColorBlendEquationEXT {
                    src_color_blend_factor: maxwell_to_vk::blend_factor(blend.color_src),
                    dst_color_blend_factor: maxwell_to_vk::blend_factor(blend.color_dst),
                    color_blend_op: maxwell_to_vk::blend_equation(blend.color_op),
                    src_alpha_blend_factor: maxwell_to_vk::blend_factor(blend.alpha_src),
                    dst_alpha_blend_factor: maxwell_to_vk::blend_factor(blend.alpha_dst),
                    alpha_blend_op: maxwell_to_vk::blend_equation(blend.alpha_op),
                };
            let equations = if !draw.blend_per_target_enabled() {
                let first = if draw.iterated_blend_enabled()
                    && common::settings::values().use_squashed_iterated_blend
                {
                    vk::ColorBlendEquationEXT {
                        src_color_blend_factor: vk::BlendFactor::ONE,
                        dst_color_blend_factor: vk::BlendFactor::ONE,
                        color_blend_op: vk::BlendOp::ADD,
                        src_alpha_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_COLOR,
                        dst_alpha_blend_factor: vk::BlendFactor::ZERO,
                        alpha_blend_op: vk::BlendOp::ADD,
                    }
                } else {
                    blend_equation(draw.global_blend())
                };
                [first; 8]
            } else {
                std::array::from_fn::<_, 8, _>(|index| blend_equation(draw.blend_at(index)))
            };
            self.scheduler.record(move |cmdbuf| unsafe {
                extension.cmd_set_color_blend_equation(cmdbuf, 0, &equations);
            });
        }
    }

    #[inline(never)]
    fn update_vertex_input(&mut self, draw: &Maxwell3DDrawView<'_>, dirty_flags: &mut [bool; 256]) {
        use super::state_tracker::dirty;

        let vertex_input_dirty = dirty_flags[dirty::VERTEX_INPUT as usize];
        let vertex_buffers_dirty = dirty_flags[crate::dirty_flags::flags::VERTEX_BUFFERS as usize];
        if !vertex_input_dirty && !vertex_buffers_dirty {
            return;
        }
        dirty_flags[dirty::VERTEX_INPUT as usize] = false;

        let mut bindings = VertexInputBindings::new();
        let mut attributes = VertexInputAttributes::new();
        let max_attributes = 32usize.min(self.max_vertex_input_attributes as usize);
        let max_bindings = 32usize.min(self.max_vertex_input_bindings as usize);

        for index in 0..max_attributes {
            let attribute = draw.vertex_attrib(index);
            let binding = attribute.buffer_index as usize;
            if attribute.constant || binding >= max_bindings {
                continue;
            }
            attributes.push(
                vk::VertexInputAttributeDescription2EXT::default()
                    .location(index as u32)
                    .binding(binding as u32)
                    .format(maxwell_to_vk::vertex_format(
                        self.device.get(),
                        attribute.attrib_type,
                        attribute.size,
                    ))
                    .offset(attribute.offset),
            );
        }

        for binding in 0..max_bindings {
            let stream = draw.vertex_stream(binding);
            let is_instanced = draw.vertex_stream_instance(binding) != 0;
            bindings.push(
                vk::VertexInputBindingDescription2EXT::default()
                    .binding(binding as u32)
                    .stride(stream.stride)
                    .input_rate(if is_instanced {
                        vk::VertexInputRate::INSTANCE
                    } else {
                        vk::VertexInputRate::VERTEX
                    })
                    .divisor(if is_instanced { stream.frequency } else { 1 }),
            );
        }

        for index in 0..32 {
            dirty_flags[dirty::VERTEX_ATTRIBUTE_0 as usize + index] = false;
        }
        for index in 0..32 {
            dirty_flags[dirty::VERTEX_BINDING_0 as usize + index] = false;
        }
        let extension = self
            .vertex_input_dynamic_state
            .as_ref()
            .expect("vertex input dynamic state loader missing")
            .clone();
        let descriptions = VertexInputDescriptions {
            bindings,
            attributes,
        };
        self.scheduler.record(move |cmdbuf| unsafe {
            descriptions.set(&extension, cmdbuf);
        });
    }

    #[inline(never)]
    fn update_primitive_restart_enable(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_primitive_restart_enable() {
            return;
        }
        let mut enabled = draw.primitive_restart().enabled;
        if self.driver_id == vk::DriverId::MOLTENVK {
            enabled = true;
        } else if enabled {
            let topology = maxwell_to_vk::primitive_topology(draw.draw_state().topology);
            enabled = (topology != vk::PrimitiveTopology::PATCH_LIST
                && self.topology_list_primitive_restart_supported)
                || supports_primitive_restart(topology)
                || (topology == vk::PrimitiveTopology::PATCH_LIST
                    && self.patch_list_primitive_restart_supported);
        }
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_primitive_restart_enable(cmdbuf, enabled);
        });
    }

    #[inline(never)]
    fn update_cull_mode(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_cull_mode() {
            return;
        }
        let rasterizer = draw.rasterizer();
        let cull_mode = if rasterizer.cull_enable {
            maxwell_to_vk::cull_face(rasterizer.cull_face)
        } else {
            vk::CullModeFlags::NONE
        };
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_cull_mode(cmdbuf, cull_mode);
        });
    }

    #[inline(never)]
    fn update_depth_bounds_test_enable(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_depth_bounds_test_enable() {
            return;
        }
        let mut enabled = draw.depth_bounds_enable();
        if enabled && !self.depth_bounds_supported {
            warn!("Depth bounds is enabled but not supported");
            enabled = false;
        }
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_depth_bounds_test_enable(cmdbuf, enabled);
        });
    }

    #[inline(never)]
    fn update_depth_test_enable(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_depth_test_enable() {
            return;
        }
        let enabled = draw.depth_stencil().depth_test_enable;
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_depth_test_enable(cmdbuf, enabled);
        });
    }

    #[inline(never)]
    fn update_depth_write_enable(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_depth_write_enable() {
            return;
        }
        let enabled = draw.depth_stencil().depth_write_enable;
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_depth_write_enable(cmdbuf, enabled);
        });
    }

    #[inline(never)]
    fn update_stencil_test_enable(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_stencil_test_enable() {
            return;
        }
        let enabled = draw.depth_stencil().stencil_enable;
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_stencil_test_enable(cmdbuf, enabled);
        });
    }

    #[inline(never)]
    fn update_rasterizer_discard_enable(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_rasterizer_discard_enable() {
            return;
        }
        let enabled = !draw.rasterize_enable();
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_rasterizer_discard_enable(cmdbuf, enabled);
        });
    }

    #[inline(never)]
    fn update_depth_bias_enable(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_depth_bias_enable() {
            return;
        }
        let rasterizer = draw.rasterizer();
        let enabled = match draw.draw_state().topology {
            PrimitiveTopology::Points => rasterizer.polygon_offset_point_enable,
            PrimitiveTopology::Lines
            | PrimitiveTopology::LineLoop
            | PrimitiveTopology::LineStrip
            | PrimitiveTopology::LinesAdjacency
            | PrimitiveTopology::LineStripAdjacency => rasterizer.polygon_offset_line_enable,
            PrimitiveTopology::Triangles
            | PrimitiveTopology::TriangleStrip
            | PrimitiveTopology::TriangleFan
            | PrimitiveTopology::Quads
            | PrimitiveTopology::QuadStrip
            | PrimitiveTopology::Polygon
            | PrimitiveTopology::TrianglesAdjacency
            | PrimitiveTopology::TriangleStripAdjacency
            | PrimitiveTopology::Patches => rasterizer.polygon_offset_fill_enable,
            invalid => panic!("invalid topology for depth-bias lookup: {invalid:?}"),
        };
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_depth_bias_enable(cmdbuf, enabled);
        });
    }

    #[inline(never)]
    fn update_depth_compare_op(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_depth_compare_op() {
            return;
        }
        let op = maxwell_to_vk::comparison_op(draw.depth_stencil().depth_func);
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_depth_compare_op(cmdbuf, op);
        });
    }

    #[inline(never)]
    fn update_front_face(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_front_face() {
            return;
        }
        let mut front_face = maxwell_to_vk::front_face(draw.rasterizer().front_face);
        if draw.window_origin_flip_y() {
            front_face = if front_face == vk::FrontFace::CLOCKWISE {
                vk::FrontFace::COUNTER_CLOCKWISE
            } else {
                vk::FrontFace::CLOCKWISE
            };
        }
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_front_face(cmdbuf, front_face);
        });
    }

    #[inline(never)]
    fn update_stencil_op(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_stencil_op() {
            return;
        }
        let depth_stencil = draw.depth_stencil();
        let front = depth_stencil.front;
        let back = depth_stencil.back;
        let two_side = depth_stencil.stencil_two_side;
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            if two_side {
                device.cmd_set_stencil_op(
                    cmdbuf,
                    vk::StencilFaceFlags::FRONT,
                    maxwell_to_vk::stencil_op(front.fail_op),
                    maxwell_to_vk::stencil_op(front.zpass_op),
                    maxwell_to_vk::stencil_op(front.zfail_op),
                    maxwell_to_vk::comparison_op(front.func),
                );
                device.cmd_set_stencil_op(
                    cmdbuf,
                    vk::StencilFaceFlags::BACK,
                    maxwell_to_vk::stencil_op(back.fail_op),
                    maxwell_to_vk::stencil_op(back.zpass_op),
                    maxwell_to_vk::stencil_op(back.zfail_op),
                    maxwell_to_vk::comparison_op(back.func),
                );
            } else {
                device.cmd_set_stencil_op(
                    cmdbuf,
                    vk::StencilFaceFlags::FRONT_AND_BACK,
                    maxwell_to_vk::stencil_op(front.fail_op),
                    maxwell_to_vk::stencil_op(front.zpass_op),
                    maxwell_to_vk::stencil_op(front.zfail_op),
                    maxwell_to_vk::comparison_op(front.func),
                );
            }
        });
    }

    #[inline(never)]
    fn update_viewports_state(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_viewports() {
            return;
        }
        let viewport_transforms = draw.viewport_transforms();
        self.record_viewports(
            &viewport_transforms,
            draw.viewport_scale_offset_enabled(),
            draw.window_origin_lower_left(),
            draw.surface_clip(),
            draw.depth_mode(),
        );
    }

    /// Mechanical Rust helper shared by the draw and clear snapshots. Upstream
    /// passes the same mutable Maxwell register object to `UpdateViewportsState`.
    fn record_viewports(
        &mut self,
        viewport_transforms: &[crate::engines::maxwell_3d::ViewportTransformInfo; NUM_VIEWPORTS],
        viewport_scale_offset_enabled: bool,
        window_origin_lower_left: bool,
        surface_clip: crate::engines::maxwell_3d::SurfaceClipInfo,
        depth_mode: crate::engines::maxwell_3d::DepthMode,
    ) {
        // Upstream dirties scissors whenever viewports are consumed. Both
        // states depend on viewport-scale enable and surface-clip registers.
        self.state_tracker.invalidate_scissors();
        let viewports = if !viewport_scale_offset_enabled {
            let mut y = surface_clip.y as f32;
            let mut height = (surface_clip.height as f32).max(1.0);
            if window_origin_lower_left {
                y += height;
                height = -height;
            }
            let viewport = vk::Viewport {
                x: surface_clip.x as f32,
                y,
                width: (surface_clip.width as f32).max(1.0),
                height,
                min_depth: 0.0,
                max_depth: 1.0,
            };
            vec![viewport; self.max_viewports as usize]
        } else {
            let scale = if self.texture_cache.base.is_rescaling {
                common::settings::values().resolution_info.up_factor
            } else {
                1.0
            };
            std::array::from_fn::<_, { NUM_VIEWPORTS }, _>(|index| {
                viewport_state(
                    viewport_transforms,
                    depth_mode,
                    window_origin_lower_left,
                    surface_clip.height,
                    index,
                    scale,
                    self.depth_range_unrestricted,
                    self.nv_viewport_swizzle,
                )
            })[..self.max_viewports as usize]
                .to_vec()
        };
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_viewport(cmdbuf, 0, &viewports);
        });
    }

    #[inline(never)]
    fn update_scissors_state(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_scissors() {
            return;
        }
        let surface_clip = draw.surface_clip();
        let viewport_scale_offset_enabled = draw.viewport_scale_offset_enabled();
        let window_origin_lower_left = draw.window_origin_lower_left();
        let scissor = if !viewport_scale_offset_enabled {
            let height = surface_clip.height.max(1);
            let y = if window_origin_lower_left {
                surface_clip
                    .height
                    .wrapping_sub(surface_clip.y.wrapping_add(height))
            } else {
                surface_clip.y
            };
            vk::Rect2D {
                offset: vk::Offset2D {
                    x: surface_clip.x as i32,
                    y: y as i32,
                },
                extent: vk::Extent2D {
                    width: surface_clip.width.max(1),
                    height,
                },
            }
        } else {
            vk::Rect2D::default()
        };
        let scissors = if viewport_scale_offset_enabled {
            let resolution = &common::settings::values().resolution_info;
            let (up_scale, down_shift) = if self.texture_cache.base.is_rescaling {
                (resolution.up_scale, resolution.down_shift)
            } else {
                (1, 0)
            };
            std::array::from_fn::<_, { NUM_VIEWPORTS }, _>(|index| {
                scissor_state(
                    draw.scissor(index),
                    window_origin_lower_left,
                    surface_clip.height,
                    up_scale,
                    down_shift,
                )
            })[..self.max_viewports as usize]
                .to_vec()
        } else {
            vec![scissor; self.max_viewports as usize]
        };
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_scissor(cmdbuf, 0, &scissors);
        });
    }

    #[inline(never)]
    fn update_depth_bias(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_depth_bias() {
            return;
        }
        // Upstream Maxwell depth-bias units are halved before being passed to
        // Vulkan (`RasterizerVulkan::UpdateDepthBias`).
        let rasterizer = draw.rasterizer();
        let constant = depth_bias_constant(
            rasterizer.depth_bias,
            draw.zeta().format,
            self.supports_d24_depth,
            self.channel_caches.program_id,
        );
        let clamp = rasterizer.depth_bias_clamp;
        let slope = rasterizer.slope_scale_depth_bias;
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_depth_bias(cmdbuf, constant, clamp, slope);
        });
    }

    #[inline(never)]
    fn update_blend_constants(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_blend_constants() {
            return;
        }
        let blend_color = draw.blend_color();
        let blend_constants = [blend_color.r, blend_color.g, blend_color.b, blend_color.a];
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_blend_constants(cmdbuf, &blend_constants);
        });
    }

    fn record_stencil_reference(&mut self, face: vk::StencilFaceFlags, value: u32) {
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_stencil_reference(cmdbuf, face, value);
        });
    }

    fn record_stencil_write_mask(&mut self, face: vk::StencilFaceFlags, value: u32) {
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_stencil_write_mask(cmdbuf, face, value);
        });
    }

    fn record_stencil_compare_mask(&mut self, face: vk::StencilFaceFlags, value: u32) {
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_stencil_compare_mask(cmdbuf, face, value);
        });
    }

    #[inline(never)]
    fn update_stencil_faces(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_stencil_properties() {
            return;
        }
        let mut update_references = self.state_tracker.touch_stencil_reference();
        let mut update_write_mask = self.state_tracker.touch_stencil_write_mask();
        let mut update_compare_masks = self.state_tracker.touch_stencil_compare();

        let depth_stencil = draw.depth_stencil();
        if self
            .state_tracker
            .touch_stencil_side(depth_stencil.stencil_two_side)
        {
            update_references = true;
            update_write_mask = true;
            update_compare_masks = true;
        }

        let front = depth_stencil.front;
        let back = depth_stencil.back;

        if update_references {
            let back_value = if depth_stencil.stencil_two_side {
                back.ref_value
            } else {
                front.ref_value
            };
            let front_changed = self
                .state_tracker
                .check_stencil_reference_front(front.ref_value);
            let back_changed = self.state_tracker.check_stencil_reference_back(back_value);
            if front_changed || back_changed {
                let split = depth_stencil.stencil_two_side && front.ref_value != back.ref_value;
                self.record_stencil_reference(
                    if split {
                        vk::StencilFaceFlags::FRONT
                    } else {
                        vk::StencilFaceFlags::FRONT_AND_BACK
                    },
                    front.ref_value,
                );
                if split {
                    self.record_stencil_reference(vk::StencilFaceFlags::BACK, back.ref_value);
                }
            }
        }

        if update_write_mask {
            let back_value = if depth_stencil.stencil_two_side {
                back.write_mask
            } else {
                front.write_mask
            };
            let front_changed = self
                .state_tracker
                .check_stencil_write_mask_front(front.write_mask);
            let back_changed = self.state_tracker.check_stencil_write_mask_back(back_value);
            if front_changed || back_changed {
                let split = depth_stencil.stencil_two_side && front.write_mask != back.write_mask;
                self.record_stencil_write_mask(
                    if split {
                        vk::StencilFaceFlags::FRONT
                    } else {
                        vk::StencilFaceFlags::FRONT_AND_BACK
                    },
                    front.write_mask,
                );
                if split {
                    self.record_stencil_write_mask(vk::StencilFaceFlags::BACK, back.write_mask);
                }
            }
        }

        if update_compare_masks {
            let back_value = if depth_stencil.stencil_two_side {
                back.func_mask
            } else {
                front.func_mask
            };
            let front_changed = self
                .state_tracker
                .check_stencil_compare_mask_front(front.func_mask);
            let back_changed = self
                .state_tracker
                .check_stencil_compare_mask_back(back_value);
            if front_changed || back_changed {
                let split = depth_stencil.stencil_two_side && front.func_mask != back.func_mask;
                self.record_stencil_compare_mask(
                    if split {
                        vk::StencilFaceFlags::FRONT
                    } else {
                        vk::StencilFaceFlags::FRONT_AND_BACK
                    },
                    front.func_mask,
                );
                if split {
                    self.record_stencil_compare_mask(vk::StencilFaceFlags::BACK, back.func_mask);
                }
            }
        }

        self.state_tracker.clear_stencil_reset();
    }

    #[inline(never)]
    fn update_depth_bounds(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_depth_bounds() {
            return;
        }
        let [min, max] = draw.depth_bounds();
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_depth_bounds(cmdbuf, min, max);
        });
    }

    #[inline(never)]
    fn update_line_width(&mut self, draw: &Maxwell3DDrawView<'_>) {
        if !self.state_tracker.touch_line_width() {
            return;
        }
        let rasterizer = draw.rasterizer();
        let width = if draw.line_state().line_anti_alias_enable {
            rasterizer.line_width_smooth
        } else {
            rasterizer.line_width_aliased
        };
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_line_width(cmdbuf, width);
        });
    }

    /// Port-facing entry point for upstream `RasterizerVulkan::AccelerateDisplay`.
    ///
    /// The texture-cache lookup body is still unported in this active rasterizer
    /// owner, so callers fall back to the raw framebuffer upload path.
    pub fn accelerate_display(
        &mut self,
        config: &FramebufferConfig,
        framebuffer_addr: u64,
        _pixel_stride: u32,
    ) -> Option<blit_screen::FramebufferTextureInfo> {
        if framebuffer_addr == 0 {
            return None;
        }
        let texture_cache: *mut TextureCache = &mut *self.texture_cache;
        // Upstream keeps TextureCache::mutex locked for the complete
        // AccelerateDisplay operation. Releasing it after the lookup lets the
        // GPU thread delete and recycle the returned ImageId before its image
        // handle/layout is consumed.
        let _texture_lock = unsafe { (*texture_cache).base.mutex.lock() };
        let framebuffer_view =
            unsafe { (*texture_cache).try_find_framebuffer_image_view(config, framebuffer_addr) };
        let Some(framebuffer_view) = framebuffer_view else {
            return None;
        };
        let image_id = framebuffer_view.common.view.image_id;
        self.query_cache.notify_segment(false);
        unsafe {
            (*texture_cache).prepare_framebuffer_for_present(image_id);
        }
        let resolution = common::settings::values().resolution_info.clone();
        let scaled_width = if framebuffer_view.common.scaled {
            resolution.scale_up_u32(framebuffer_view.width)
        } else {
            framebuffer_view.width
        };
        let scaled_height = if framebuffer_view.common.scaled {
            resolution.scale_up_u32(framebuffer_view.height)
        } else {
            framebuffer_view.height
        };
        Some(blit_screen::FramebufferTextureInfo {
            image: framebuffer_view.image,
            image_view: framebuffer_view.image_view,
            width: framebuffer_view.width,
            height: framebuffer_view.height,
            scaled_width,
            scaled_height,
        })
    }
}

impl RasterizerInterface for RasterizerVulkan {
    fn accelerate_conditional_rendering(&mut self) -> bool {
        if let Some(memory_manager) = self.channel_memory_manager.as_ref() {
            memory_manager.lock().flush_caching();
        }
        self.query_cache.accelerate_host_conditional_rendering()
    }

    fn load_disk_resources(
        &mut self,
        title_id: u64,
        stop_loading: crate::rasterizer_interface::DiskResourceLoadStop,
        callback: crate::rasterizer_interface::DiskResourceLoadCallback,
    ) {
        let shader_dir =
            common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::ShaderDir);
        self.pipeline_cache
            .load_disk_resources(title_id, &shader_dir, stop_loading, callback);
    }

    fn draw(
        &mut self,
        mut draw_view: crate::engines::draw_manager::Maxwell3DDrawView<'_>,
        instance_count: u32,
    ) {
        let draw_indexed = draw_view.is_indexed();
        self.draw_sequence = self.draw_sequence.wrapping_add(1);
        debug!(
            "RasterizerVulkan::draw indexed={} instances={}",
            draw_indexed, instance_count
        );
        let Some(memory_manager) = self.channel_memory_manager_handle else {
            warn!("RasterizerVulkan::draw skipped: no bound channel memory manager");
            return;
        };
        let zpass_pixel_count_enabled = draw_view.zpass_pixel_count_enabled();
        let read_gpu = |gpu_va: u64, output: &mut [u8]| {
            // SAFETY: the rasterizer retains the owning channel Arc and the
            // draw executes synchronously on the GPU thread.
            unsafe {
                memory_manager.as_ref().read_block(gpu_va, output);
            }
        };
        let read_gpu_unsafe = |gpu_va: u64, output: &mut [u8]| unsafe {
            memory_manager.as_ref().read_block_unsafe(gpu_va, output)
        };
        if let Some(mut dirty_flags) = draw_view.dirty_flags_ptr() {
            // SAFETY: production draws execute synchronously on the GPU thread.
            // The pointer targets the bound channel's stable Maxwell dirty
            // array, which is also the sole owner selected by StateTracker.
            unsafe {
                self.prepare_draw(
                    &mut draw_view,
                    instance_count,
                    zpass_pixel_count_enabled,
                    None,
                    dirty_flags.as_mut(),
                    true,
                    &read_gpu,
                    &read_gpu_unsafe,
                );
            }
        } else {
            let original_dirty_flags = *draw_view.dirty_flags();
            let mut dirty_flags = original_dirty_flags;
            self.prepare_draw(
                &mut draw_view,
                instance_count,
                zpass_pixel_count_enabled,
                None,
                &mut dirty_flags,
                false,
                &read_gpu,
                &read_gpu_unsafe,
            );
            propagate_consumed_dirty_flags(&mut draw_view, &original_dirty_flags, &dirty_flags);
        }
    }

    fn draw_indirect(
        &mut self,
        mut indirect_view: crate::engines::draw_manager::Maxwell3DIndirectView<'_>,
    ) {
        let params = *indirect_view.params();
        let Some(memory_manager) = self.channel_memory_manager_handle else {
            warn!("RasterizerVulkan::draw_indirect skipped: no bound channel memory manager");
            return;
        };

        self.draw_sequence = self.draw_sequence.wrapping_add(1);
        let zpass_pixel_count_enabled = indirect_view.draw_view_mut().zpass_pixel_count_enabled();
        let read_gpu = |gpu_va: u64, output: &mut [u8]| unsafe {
            memory_manager.as_ref().read_block(gpu_va, output);
        };
        let read_gpu_unsafe = |gpu_va: u64, output: &mut [u8]| unsafe {
            memory_manager.as_ref().read_block_unsafe(gpu_va, output)
        };
        let cache_params = crate::buffer_cache::buffer_cache_base::DrawIndirectParams {
            indirect_start_address: params.indirect_start_address,
            count_start_address: params.count_start_address,
            buffer_size: params.buffer_size as u64,
            max_draw_counts: params.max_draw_counts as u32,
            stride: params.stride as u32,
            include_count: params.include_count,
        };
        self.common_buffer_cache
            .set_draw_indirect(Some(cache_params));
        let instance_count = indirect_view.draw_view_mut().draw_state().instance_count;
        if let Some(mut dirty_flags) = indirect_view.dirty_flags_ptr() {
            // SAFETY: same live-channel invariant as the direct draw path.
            unsafe {
                self.prepare_draw(
                    indirect_view.draw_view_mut(),
                    instance_count,
                    zpass_pixel_count_enabled,
                    Some(params),
                    dirty_flags.as_mut(),
                    true,
                    &read_gpu,
                    &read_gpu_unsafe,
                );
            }
        } else {
            let original_dirty_flags = *indirect_view.draw_view_mut().dirty_flags();
            let mut dirty_flags = original_dirty_flags;
            self.prepare_draw(
                indirect_view.draw_view_mut(),
                instance_count,
                zpass_pixel_count_enabled,
                Some(params),
                &mut dirty_flags,
                false,
                &read_gpu,
                &read_gpu_unsafe,
            );
            propagate_consumed_dirty_flags(
                indirect_view.draw_view_mut(),
                &original_dirty_flags,
                &dirty_flags,
            );
        }
        self.common_buffer_cache.set_draw_indirect(None);
    }

    fn draw_texture(
        &mut self,
        mut draw_texture_view: crate::engines::draw_manager::Maxwell3DDrawTextureView<'_>,
    ) {
        let _gpu_tick_guard = GpuTickGuard::new(self.gpu_tick_callback.as_ref());
        self.flush_work();

        let draw_texture_state = draw_texture_view.draw_texture_state();
        let render_targets = draw_texture_view.render_targets();
        let descriptor_sync_regs = draw_texture_view.descriptor_sync_regs();

        let Some(memory_manager) = self.channel_memory_manager_handle else {
            log::warn!("RasterizerVulkan::draw_texture skipped: no bound channel memory manager");
            return;
        };
        let read_gpu_unsafe = |gpu_va: u64, output: &mut [u8]| unsafe {
            memory_manager.as_ref().read_block_unsafe(gpu_va, output)
        };

        // Upstream keeps TextureCache::mutex locked from descriptor
        // synchronization through the BlitImageHelper call.
        let texture_cache_mutex: *const _ = &self.texture_cache.base.mutex;
        let _texture_cache_guard = unsafe { (*texture_cache_mutex).lock() };
        self.texture_cache
            .base
            .synchronize_graphics_descriptors(descriptor_sync_regs);
        if let Some(mut dirty_flags) = draw_texture_view.dirty_flags_ptr() {
            // SAFETY: the draw-texture callback is serialized on the GPU
            // thread. This points at the same live channel flags used by
            // StateTracker and mirrors Eden's persistent Maxwell3D owner.
            unsafe {
                if !self.texture_cache.update_render_targets(
                    &render_targets,
                    dirty_flags.as_mut(),
                    &read_gpu_unsafe,
                    false,
                    None,
                ) {
                    log::warn!(
                        "RasterizerVulkan::draw_texture skipped: render-target update failed"
                    );
                    return;
                }
                self.update_dynamic_states(draw_texture_view.draw_view_mut(), dirty_flags.as_mut());
            }
        } else {
            let original_dirty_flags = *draw_texture_view.dirty_flags();
            let mut dirty_flags = original_dirty_flags;
            if !self.texture_cache.update_render_targets(
                &render_targets,
                &mut dirty_flags,
                &read_gpu_unsafe,
                false,
                None,
            ) {
                log::warn!("RasterizerVulkan::draw_texture skipped: render-target update failed");
                return;
            }
            self.update_dynamic_states(draw_texture_view.draw_view_mut(), &mut dirty_flags);
            propagate_consumed_dirty_flags(
                draw_texture_view.draw_view_mut(),
                &original_dirty_flags,
                &dirty_flags,
            );
        }
        self.query_cache.notify_segment(true);
        self.query_cache.counter_enable(
            &mut self.scheduler,
            QueryType::ZPassPixelCount64 as u32,
            draw_texture_view.zpass_pixel_count_enabled(),
        );

        let sampler_id = self
            .texture_cache
            .get_sampler_id(draw_texture_state.src_sampler, false);
        let Some(sampler) = self.texture_cache.sampler_handle(sampler_id) else {
            log::warn!(
                "RasterizerVulkan::draw_texture skipped: invalid sampler {}",
                draw_texture_state.src_sampler
            );
            return;
        };
        let Some(texture) = self
            .texture_cache
            .draw_texture_source(draw_texture_state.src_texture)
        else {
            log::warn!(
                "RasterizerVulkan::draw_texture skipped: invalid texture {}",
                draw_texture_state.src_texture
            );
            return;
        };
        let framebuffer = match self.texture_cache.get_framebuffer() {
            Ok(framebuffer) => framebuffer,
            Err(error) => {
                log::warn!(
                    "RasterizerVulkan::draw_texture skipped: framebuffer creation failed: {error:?}"
                );
                return;
            }
        };

        let cache_is_rescaling = self.texture_cache.base.is_rescaling;
        let src_rescaling = cache_is_rescaling && texture.is_rescaled;
        let dst_rescaling = cache_is_rescaling && framebuffer.is_rescaled();
        let resolution = common::settings::values().resolution_info.clone();
        let scale_src = |value: f32| {
            let value = value as i32;
            if src_rescaling {
                resolution.scale_up_i32(value)
            } else {
                value
            }
        };
        let scale_dst = |value: f32| {
            let value = value as i32;
            if dst_rescaling {
                resolution.scale_up_i32(value)
            } else {
                value
            }
        };
        let dst_region = blit_image::Region2D {
            start: blit_image::Offset2D {
                x: scale_dst(draw_texture_state.dst_x0),
                y: scale_dst(draw_texture_state.dst_y0),
            },
            end: blit_image::Offset2D {
                x: scale_dst(draw_texture_state.dst_x1),
                y: scale_dst(draw_texture_state.dst_y1),
            },
        };
        let src_region = blit_image::Region2D {
            start: blit_image::Offset2D {
                x: scale_src(draw_texture_state.src_x0),
                y: scale_src(draw_texture_state.src_y0),
            },
            end: blit_image::Offset2D {
                x: scale_src(draw_texture_state.src_x1),
                y: scale_src(draw_texture_state.src_y1),
            },
        };
        let mut src_size = texture.size;
        if src_rescaling {
            src_size.width = resolution.scale_up_u32(src_size.width);
            src_size.height = resolution.scale_up_u32(src_size.height);
        }

        self.blit_image.blit_color_with_sampler(
            framebuffer.blit_framebuffer_info(),
            texture.image_view,
            texture.image,
            sampler,
            &dst_region,
            &src_region,
            &src_size,
        );
    }

    fn clear(
        &mut self,
        mut clear_view: crate::engines::draw_manager::Maxwell3DClearView<'_>,
        layer_count: u32,
    ) {
        let _scope = ProfileScope::new(ProfileZone::Clear);
        // Preserve upstream ordering: submit pending work before flushing the
        // channel GPU-memory cache.
        self.flush_work();
        if let Some(mm) = self.channel_memory_manager.as_ref().cloned() {
            mm.lock().flush_caching();
        }

        let clear_state = clear_view.clear_state();
        let use_depth = clear_state.flags & (1 << 0) != 0;
        let use_stencil = clear_state.flags & (1 << 1) != 0;
        let use_r = clear_state.flags & (1 << 2) != 0;
        let use_g = clear_state.flags & (1 << 3) != 0;
        let use_b = clear_state.flags & (1 << 4) != 0;
        let use_a = clear_state.flags & (1 << 5) != 0;
        let use_color = use_r || use_g || use_b || use_a;
        if !use_color && !use_depth && !use_stencil {
            return;
        }

        let render_targets = clear_view.render_targets();
        let Some(memory_manager) = self.channel_memory_manager_handle else {
            warn!("RasterizerVulkan::clear skipped: no bound channel memory manager");
            return;
        };
        let read_gpu_unsafe = |gpu_va: u64, output: &mut [u8]| unsafe {
            memory_manager.as_ref().read_block_unsafe(gpu_va, output)
        };
        let clear_scissor = clear_view.use_scissor().then(|| {
            let scissor = clear_view.scissor(0);
            (scissor.min_x, scissor.min_y, scissor.max_x, scissor.max_y)
        });
        // Upstream holds texture_cache.mutex from UpdateRenderTargets through
        // the clear command. CPU invalidation may otherwise erase a slot while
        // alias synchronization is iterating slot_images.
        let texture_cache_mutex: *const _ = &self.texture_cache.base.mutex;
        let _texture_cache_guard = unsafe { (*texture_cache_mutex).lock() };
        if let Some(mut dirty_flags) = clear_view.dirty_flags_ptr() {
            // SAFETY: clear executes synchronously on the GPU thread and the
            // pointer targets the bound channel's stable dirty flag array.
            if !unsafe {
                self.texture_cache.update_render_targets(
                    &render_targets,
                    dirty_flags.as_mut(),
                    &read_gpu_unsafe,
                    true,
                    clear_scissor,
                )
            } {
                return;
            }
        } else {
            let original_flags = *clear_view.dirty_flags();
            let mut dirty_flags = original_flags;
            if !self.texture_cache.update_render_targets(
                &render_targets,
                &mut dirty_flags,
                &read_gpu_unsafe,
                true,
                clear_scissor,
            ) {
                return;
            }
            for (index, dirty) in dirty_flags.iter().enumerate() {
                if !dirty && original_flags[index] {
                    clear_view.clear_dirty_flag(index as u8);
                }
            }
        }
        let target = match self.texture_cache.get_framebuffer() {
            Ok(target) => target,
            Err(error) => {
                warn!("RasterizerVulkan::clear skipped: framebuffer creation failed: {error:?}");
                return;
            }
        };
        let render_area = target.render_area();
        let color_full_channels = use_r && use_g && use_b && use_a;
        let depth_stencil = clear_view.depth_stencil();
        let stencil_mask = depth_stencil.front.write_mask;
        let stencil_partial = use_stencil
            && target.has_aspect_stencil_bit()
            && stencil_mask != 0xFF
            && stencil_mask != 0;
        let ds_used = use_depth || use_stencil;
        let ds_deferrable = !ds_used
            || ((!target.has_aspect_depth_bit() || use_depth)
                && (!target.has_aspect_stencil_bit() || use_stencil)
                && !stencil_partial);
        let clear_layer = (clear_state.flags >> 10) & 0xFFFF;
        let (up_scale, down_shift) = clear_rescaling(self.texture_cache.base.is_rescaling);

        let mut clear_rect_2d = if clear_view.use_scissor() {
            scissor_state(
                clear_view.scissor(0),
                clear_view.window_origin_lower_left(),
                render_targets.surface_clip.height,
                up_scale,
                down_shift,
            )
        } else {
            vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: vk::Extent2D {
                    width: i32::MAX as u32,
                    height: i32::MAX as u32,
                },
            }
        };
        let clamp_axis = |offset: &mut i32, extent: &mut u32, limit: u32| {
            if *extent == 0 {
                *offset = (*offset).clamp(0, limit as i32);
                return;
            }
            if *offset < 0 {
                let shrink = (*extent).min(offset.wrapping_neg() as u32);
                *extent -= shrink;
                *offset = 0;
            }
            if limit == 0 {
                *offset = 0;
                *extent = 0;
                return;
            }
            if *offset >= limit as i32 {
                *offset = limit as i32;
                *extent = 0;
                return;
            }
            if (*offset as u64) + (*extent as u64) > limit as u64 {
                *extent = limit - *offset as u32;
            }
        };
        clamp_axis(
            &mut clear_rect_2d.offset.x,
            &mut clear_rect_2d.extent.width,
            render_area.width,
        );
        clamp_axis(
            &mut clear_rect_2d.offset.y,
            &mut clear_rect_2d.extent.height,
            render_area.height,
        );
        if clear_rect_2d.extent.width == 0 || clear_rect_2d.extent.height == 0 {
            return;
        }

        let clear_rect = vk::ClearRect {
            rect: clear_rect_2d,
            base_array_layer: clear_layer,
            layer_count,
        };

        // A scissored clear covering the whole render area can still be
        // deferred into the render-pass load op.
        let clear_covers_render_area = clear_rect.rect.offset.x == 0
            && clear_rect.rect.offset.y == 0
            && clear_rect.rect.extent.width >= render_area.width
            && clear_rect.rect.extent.height >= render_area.height;
        const ENABLE_DEFERRED_CLEAR: bool = true;
        let can_defer_clear = ENABLE_DEFERRED_CLEAR
            && (!clear_view.use_scissor() || clear_covers_render_area)
            && clear_layer == 0
            && !self.scheduler.is_render_pass_active()
            && (!use_color || color_full_channels)
            && ds_deferrable;
        if !can_defer_clear {
            self.scheduler.request_renderpass(&target);
        }

        self.query_cache.notify_segment(true);
        self.query_cache.counter_enable(
            &mut self.scheduler,
            QueryType::ZPassPixelCount64 as u32,
            clear_view.zpass_pixel_count_enabled(),
        );
        if self.state_tracker.touch_viewports() {
            let viewport_transforms = clear_view.viewport_transforms();
            self.record_viewports(
                &viewport_transforms,
                clear_view.viewport_scale_offset_enabled(),
                clear_view.window_origin_lower_left(),
                render_targets.surface_clip,
                clear_view.depth_mode(),
            );
        }

        let color_attachment = ((clear_state.flags >> 6) & 0xF) as usize;
        if use_color && target.has_aspect_color_bit(color_attachment) {
            let format = crate::surface::pixel_format_from_render_target_format(
                render_targets.render_targets[color_attachment].format,
            );
            let clear_value = make_color_clear_value(format, clear_state.color);
            if color_full_channels && can_defer_clear {
                self.scheduler
                    .defer_color_clear(&target, color_attachment as u32, clear_value);
            } else if color_full_channels {
                let attachment = vk::ClearAttachment {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    color_attachment: color_attachment as u32,
                    clear_value,
                };
                let device = self.device;
                self.scheduler.record(move |cmdbuf| unsafe {
                    let device = device.get().get_logical();
                    device.cmd_clear_attachments(cmdbuf, &[attachment], &[clear_rect]);
                });
            } else {
                let color_mask = u8::from(use_r)
                    | (u8::from(use_g) << 1)
                    | (u8::from(use_b) << 2)
                    | (u8::from(use_a) << 3);
                let dst_region = blit_image::Region2D {
                    start: blit_image::Offset2D {
                        x: clear_rect.rect.offset.x,
                        y: clear_rect.rect.offset.y,
                    },
                    end: blit_image::Offset2D {
                        x: clear_rect.rect.offset.x + clear_rect.rect.extent.width as i32,
                        y: clear_rect.rect.offset.y + clear_rect.rect.extent.height as i32,
                    },
                };
                self.blit_image.clear_color(
                    target.blit_framebuffer_info(),
                    color_mask,
                    clear_state.color,
                    &dst_region,
                );
            }
        }
        if !use_depth && !use_stencil {
            return;
        }
        let mut depth_stencil_aspects = vk::ImageAspectFlags::empty();
        if target.has_aspect_depth_bit() && use_depth {
            depth_stencil_aspects |= vk::ImageAspectFlags::DEPTH;
        }
        if target.has_aspect_stencil_bit() && use_stencil {
            depth_stencil_aspects |= vk::ImageAspectFlags::STENCIL;
        }
        if !depth_stencil_aspects.is_empty() {
            if stencil_partial {
                let dst_region = blit_image::Region2D {
                    start: blit_image::Offset2D {
                        x: clear_rect.rect.offset.x,
                        y: clear_rect.rect.offset.y,
                    },
                    end: blit_image::Offset2D {
                        x: clear_rect.rect.offset.x + clear_rect.rect.extent.width as i32,
                        y: clear_rect.rect.offset.y + clear_rect.rect.extent.height as i32,
                    },
                };
                self.blit_image.clear_depth_stencil(
                    target.blit_framebuffer_info(),
                    use_depth,
                    clear_state.depth,
                    stencil_mask as u8,
                    clear_state.stencil as u32,
                    depth_stencil.front.func_mask,
                    &dst_region,
                );
            } else if can_defer_clear {
                self.scheduler.defer_depth_stencil_clear(
                    &target,
                    vk::ClearValue {
                        depth_stencil: vk::ClearDepthStencilValue {
                            depth: clear_state.depth,
                            stencil: clear_state.stencil as u32,
                        },
                    },
                );
            } else {
                let attachment = vk::ClearAttachment {
                    aspect_mask: depth_stencil_aspects,
                    color_attachment: 0,
                    clear_value: vk::ClearValue {
                        depth_stencil: vk::ClearDepthStencilValue {
                            depth: clear_state.depth,
                            stencil: clear_state.stencil as u32,
                        },
                    },
                };
                let device = self.device;
                self.scheduler.record(move |cmdbuf| unsafe {
                    let device = device.get().get_logical();
                    device.cmd_clear_attachments(cmdbuf, &[attachment], &[clear_rect]);
                });
            }
        }
    }

    fn dispatch_compute(&mut self, dispatch: &DispatchCall) {
        let _scope = ProfileScope::new(ProfileZone::Compute);
        self.flush_work();
        if let Some(mm) = self.channel_memory_manager.as_ref().cloned() {
            mm.lock().flush_caching();
        }

        let Some(mut current_pipeline) = self
            .pipeline_cache
            .current_compute_pipeline(&mut self.shader_cache)
        else {
            return;
        };
        let Some(memory_manager) = self.channel_memory_manager_handle else {
            return;
        };
        let read_gpu = |address: u64, output: &mut [u8]| unsafe {
            memory_manager.as_ref().read_block_unsafe(address, output)
        };
        let buffer_cache_mutex: *const _ = Arc::as_ptr(&self.common_buffer_cache.mutex);
        let texture_cache_mutex: *const _ = &self.texture_cache.base.mutex;
        lock_two_reentrant_mutexes!(
            buffer_cache_mutex,
            texture_cache_mutex,
            _buffer_cache_guard,
            _texture_cache_guard
        );
        let configured = unsafe { current_pipeline.owner_mut() }.configure(
            dispatch,
            &mut self.scheduler,
            &mut self.common_buffer_cache,
            &mut self.texture_cache,
            self.fallback_sampler,
            self.push_descriptor.clone(),
            &read_gpu,
        );
        if !configured {
            return;
        }
        let compute_pipeline = unsafe { current_pipeline.owner_mut() }.pipeline_state();

        if let Some(indirect_address) = dispatch.indirect_compute_address {
            let (buffer_id, offset) = self.common_buffer_cache.obtain_buffer(
                indirect_address,
                12,
                ObtainBufferSynchronize::FullSynchronize,
                ObtainBufferOperation::DiscardWrite,
            );
            let raw_buffer = self
                .common_buffer_cache
                .resolve_backend_buffer_raw(buffer_id);
            if raw_buffer == 0 {
                return;
            }
            let indirect_buffer = vk::Buffer::from_raw(raw_buffer);
            let device = self.device;
            self.scheduler
                .request_outside_render_pass_operation_context();
            self.scheduler.record(move |cmdbuf| unsafe {
                if *compute_pipeline.lock().unwrap() == vk::Pipeline::null() {
                    return;
                }
                let device = device.get().get_logical();
                device.cmd_dispatch_indirect(cmdbuf, indirect_buffer, offset as u64);
            });
            return;
        }

        let dim = [
            dispatch.launch_description.grid_dim_x,
            dispatch.launch_description.grid_dim_y,
            dispatch.launch_description.grid_dim_z,
        ];
        let max_dim = self.max_compute_work_group_count;
        if dim[0] > max_dim[0] || dim[1] > max_dim[1] || dim[2] > max_dim[2] {
            return;
        }
        let barrier_device = self.device;
        self.scheduler
            .request_outside_render_pass_operation_context();
        self.scheduler.record(move |cmdbuf| unsafe {
            let barrier_device = barrier_device.get().get_logical();
            let barrier = vk::MemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ);
            barrier_device.cmd_pipeline_barrier(
                cmdbuf,
                vk::PipelineStageFlags::ALL_GRAPHICS
                    | vk::PipelineStageFlags::COMPUTE_SHADER
                    | vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &[barrier],
                &[],
                &[],
            );
        });
        let device = self.device;
        self.scheduler.record(move |cmdbuf| unsafe {
            if *compute_pipeline.lock().unwrap() == vk::Pipeline::null() {
                return;
            }
            let device = device.get().get_logical();
            device.cmd_dispatch(cmdbuf, dim[0], dim[1], dim[2]);
        });
        if is_active() && *common::settings::values().gpu_log_vulkan_calls.get_value() {
            get_instance().log_vulkan_call(
                "vkCmdDispatch",
                &dispatch_log_info(dim),
                vk::Result::SUCCESS.as_raw(),
            );
        }
    }

    fn reset_counter(&mut self, query_type: u32) {
        if !supports_counter_reset(query_type) {
            debug!(
                "RasterizerVulkan::reset_counter unimplemented counter reset={}",
                query_type
            );
            return;
        }
        let this = self as *mut Self;
        self.query_cache
            .counter_reset(&mut self.scheduler, query_type, move |operation| unsafe {
                (*this).sync_operation(operation);
            });
    }

    fn query(
        &mut self,
        gpu_addr: u64,
        query_type: u32,
        flags: QueryPropertiesFlags,
        payload: u32,
        subreport: u32,
    ) {
        let this = self as *mut Self;
        self.query_cache.query(
            &mut self.scheduler,
            gpu_addr,
            query_type,
            flags,
            payload,
            subreport,
            move |func| unsafe { (*this).signal_fence(func) },
            move |func| unsafe { (*this).sync_operation(func) },
        );
    }

    fn bind_graphics_uniform_buffer(&mut self, stage: usize, index: u32, gpu_addr: u64, size: u32) {
        self.common_buffer_cache
            .bind_graphics_uniform_buffer(stage, index, gpu_addr, size);
    }

    fn disable_graphics_uniform_buffer(&mut self, stage: usize, index: u32) {
        self.common_buffer_cache
            .disable_graphics_uniform_buffer(stage, index);
    }

    fn signal_fence(&mut self, func: Box<dyn FnOnce() + Send>) {
        let this = self as *mut Self;
        let this_for_pop = this as usize;
        self.fence_manager.signal_fence(
            func,
            move |is_stubbed| unsafe { (*this).fence_backend.create_fence(is_stubbed) },
            move |fence| unsafe { (*this).queue_fence(fence) },
            move || unsafe { (*this).should_wait_async_flushes() },
            move |fence| unsafe { (*this).is_fence_signaled(fence) },
            move || unsafe { (*(this_for_pop as *mut Self)).pop_async_flushes() },
            move || unsafe { (*this).should_flush_async() },
            move || unsafe { (*this).commit_async_flushes() },
            move || unsafe { (*this).flush_commands() },
            move || unsafe { (*this).invalidate_gpu_cache() },
        );
    }

    fn sync_operation(&mut self, func: Box<dyn FnOnce() + Send>) {
        self.fence_manager.sync_operation(func);
    }

    fn signal_sync_point(&mut self, id: u32) {
        let this = self as *mut Self;
        let this_for_pop = this as usize;
        let syncpoints = Arc::clone(&self.syncpoints);
        self.fence_manager.signal_sync_point(
            id,
            {
                let syncpoints = Arc::clone(&syncpoints);
                move |value| syncpoints.increment_guest(value)
            },
            move |value| syncpoints.increment_host(value),
            move |is_stubbed| unsafe { (*this).fence_backend.create_fence(is_stubbed) },
            move |fence| unsafe { (*this).queue_fence(fence) },
            move || unsafe { (*this).should_wait_async_flushes() },
            move |fence| unsafe { (*this).is_fence_signaled(fence) },
            move || unsafe { (*(this_for_pop as *mut Self)).pop_async_flushes() },
            move || unsafe { (*this).should_flush_async() },
            move || unsafe { (*this).commit_async_flushes() },
            move || unsafe { (*this).flush_commands() },
            move || unsafe { (*this).invalidate_gpu_cache() },
        );
    }

    fn signal_reference(&mut self) {
        let this = self as *mut Self;
        let this_for_pop = this as usize;
        self.fence_manager.signal_reference(
            move |is_stubbed| unsafe { (*this).fence_backend.create_fence(is_stubbed) },
            move |fence| unsafe { (*this).queue_fence(fence) },
            move || unsafe { (*this).should_wait_async_flushes() },
            move |fence| unsafe { (*this).is_fence_signaled(fence) },
            move || unsafe { (*(this_for_pop as *mut Self)).pop_async_flushes() },
            move || unsafe { (*this).should_flush_async() },
            move || unsafe { (*this).commit_async_flushes() },
            move || unsafe { (*this).flush_commands() },
            move || unsafe { (*this).invalidate_gpu_cache() },
        );
    }

    fn release_fences(&mut self, force: bool) {
        let this = self as *mut Self;
        let this_for_pop = this as usize;
        self.fence_manager.wait_pending_fences(
            force,
            move |is_stubbed| unsafe { (*this).fence_backend.create_fence(is_stubbed) },
            move |fence| unsafe { (*this).queue_fence(fence) },
            move || unsafe { (*this).should_wait_async_flushes() },
            move |fence| unsafe { (*this).is_fence_signaled(fence) },
            move |fence| unsafe { (*this).wait_fence(fence) },
            move || unsafe { (*(this_for_pop as *mut Self)).pop_async_flushes() },
            move || unsafe { (*this).should_flush_async() },
            move || unsafe { (*this).commit_async_flushes() },
            move || unsafe { (*this).flush_commands() },
            move || unsafe { (*this).invalidate_gpu_cache() },
        );
    }

    fn flush_all(&mut self) {}

    fn flush_region(&mut self, addr: u64, size: u64, which: CacheType) {
        if addr == 0 || size == 0 {
            return;
        }
        if which.contains(CacheType::TEXTURE_CACHE) {
            unsafe {
                let texture_mutex: *const _ = &self.texture_cache.base.mutex;
                let _texture_guard = (*texture_mutex).lock();
                self.texture_cache.download_memory(addr, size as usize);
            }
        }
        if which.contains(CacheType::BUFFER_CACHE) {
            unsafe {
                let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
                let _buffer_guard = (*buffer_mutex).lock();
                self.common_buffer_cache.download_memory(addr, size);
            }
        }
        if which.contains(CacheType::QUERY_CACHE) {
            self.query_cache.flush_region(addr, size as usize);
        }
    }

    fn must_flush_region(&self, addr: u64, size: u64, which: CacheType) -> bool {
        if which.contains(CacheType::BUFFER_CACHE) {
            let _buffer_guard = self.common_buffer_cache.mutex.lock();
            if self
                .common_buffer_cache
                .is_region_gpu_modified(addr, size as usize)
            {
                return true;
            }
        }
        if !common::settings::is_gpu_level_high(&common::settings::values()) {
            return false;
        }
        if which.contains(CacheType::TEXTURE_CACHE) {
            let _texture_guard = self.texture_cache.base.mutex.lock();
            return self
                .texture_cache
                .base
                .is_region_gpu_modified(addr, size as usize);
        }
        false
    }

    fn get_flush_area(&self, addr: u64, size: u64) -> RasterizerDownloadArea {
        unsafe {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let _texture_guard = (*texture_mutex).lock();
            let texture_cache: *mut TextureCache =
                &*self.texture_cache as *const TextureCache as *mut TextureCache;
            if let Some(area) = (*texture_cache).base.get_flush_area(addr, size as usize) {
                return area;
            }
        }
        RasterizerDownloadArea {
            start_address: align_down(addr, PAGE_SIZE_U64),
            end_address: align_up(addr.wrapping_add(size), PAGE_SIZE_U64),
            preemtive: true,
        }
    }

    fn invalidate_region(&mut self, addr: u64, size: u64, which: CacheType) {
        if addr == 0 || size == 0 {
            return;
        }
        if which.contains(CacheType::TEXTURE_CACHE) {
            unsafe {
                let texture_mutex: *const _ = &self.texture_cache.base.mutex;
                let _texture_guard = (*texture_mutex).lock();
                self.texture_cache.base.write_memory(addr, size as usize);
            }
        }
        if which.contains(CacheType::BUFFER_CACHE) {
            unsafe {
                let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
                let _buffer_guard = (*buffer_mutex).lock();
                self.common_buffer_cache.write_memory(addr, size);
            }
        }
        if which.contains(CacheType::QUERY_CACHE) {
            self.query_cache.invalidate_region(addr, size as usize);
        }
        if which.contains(CacheType::SHADER_CACHE) {
            self.shader_cache.invalidate_region(addr, size as usize);
        }
    }

    fn inner_invalidation(&mut self, sequences: &[(u64, usize)]) {
        // This override bypasses RasterizerInterface's default implementation,
        // so it must honor the same opt-in invalidation setting explicitly.
        if *common::settings::values()
            .skip_cpu_inner_invalidation
            .get_value()
        {
            return;
        }
        unsafe {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let _texture_guard = (*texture_mutex).lock();
            for &(addr, size) in sequences {
                self.texture_cache.base.write_memory(addr, size);
            }
        }
        unsafe {
            let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            for &(addr, size) in sequences {
                self.common_buffer_cache.write_memory(addr, size as u64);
            }
        }
        for &(addr, size) in sequences {
            self.query_cache.invalidate_region(addr, size);
            self.shader_cache.invalidate_region(addr, size);
        }
    }

    fn on_cache_invalidation(&mut self, addr: u64, size: u64) {
        if addr == 0 || size == 0 {
            return;
        }
        unsafe {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let _texture_guard = (*texture_mutex).lock();
            self.texture_cache.base.write_memory(addr, size as usize);
        }
        unsafe {
            let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            self.common_buffer_cache.write_memory(addr, size);
        }
        self.shader_cache.invalidate_region(addr, size as usize);
    }

    fn cached_write_memory(&mut self, addr: u64, size: u64) {
        if addr == 0 || size == 0 {
            return;
        }
        unsafe {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let _texture_guard = (*texture_mutex).lock();
            self.texture_cache.base.write_memory(addr, size as usize);
        }
        unsafe {
            let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            self.common_buffer_cache.cached_write_memory(addr, size);
        }
        self.shader_cache.invalidate_region(addr, size as usize);
    }

    fn on_cpu_write(&mut self, addr: u64, size: u64) -> bool {
        debug_assert!(addr != 0 || size != 0);
        let buffer_handled = unsafe {
            let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            self.common_buffer_cache.on_cpu_write(addr, size)
        };
        if buffer_handled {
            return true;
        }
        unsafe {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let _texture_guard = (*texture_mutex).lock();
            self.texture_cache.base.write_memory(addr, size as usize);
        }
        self.shader_cache.invalidate_region(addr, size as usize);
        false
    }

    fn invalidate_gpu_cache(&mut self) {
        if let Some(callback) = &self.invalidate_gpu_cache_callback {
            callback();
        }
    }

    fn unmap_memory(&mut self, addr: u64, size: u64) {
        unsafe {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let _texture_guard = (*texture_mutex).lock();
            self.texture_cache.base.unmap_memory(addr, size as usize);
        }
        unsafe {
            let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            self.common_buffer_cache.write_memory(addr, size);
        }
        self.shader_cache.on_cache_invalidation(addr, size as usize);
    }

    fn modify_gpu_memory(&mut self, as_id: usize, addr: u64, size: u64) {
        let texture_mutex: *const _ = &self.texture_cache.base.mutex;
        let _texture_guard = unsafe { (*texture_mutex).lock() };
        self.texture_cache
            .base
            .unmap_gpu_memory(as_id, addr, size as usize);
        // Upstream: `buffer_cache.UnmapGPUMemory(as_id, addr, size)` (outside the
        // texture cache lock; the buffer cache defers the invalidation itself).
        self.common_buffer_cache
            .unmap_gpu_memory(as_id, addr, size as usize);
    }

    fn flush_and_invalidate_region(&mut self, addr: u64, size: u64, which: CacheType) {
        if common::settings::is_gpu_level_high(&common::settings::values()) {
            self.flush_region(addr, size, which);
        }
        self.invalidate_region(addr, size, which);
    }

    fn wait_for_idle(&mut self) {
        let mut flags = vk::PipelineStageFlags::DRAW_INDIRECT
            | vk::PipelineStageFlags::VERTEX_INPUT
            | vk::PipelineStageFlags::VERTEX_SHADER
            | vk::PipelineStageFlags::TESSELLATION_CONTROL_SHADER
            | vk::PipelineStageFlags::TESSELLATION_EVALUATION_SHADER
            | vk::PipelineStageFlags::GEOMETRY_SHADER
            | vk::PipelineStageFlags::FRAGMENT_SHADER
            | vk::PipelineStageFlags::COMPUTE_SHADER
            | vk::PipelineStageFlags::TRANSFER;
        if self.transform_feedback_supported {
            flags |= vk::PipelineStageFlags::TRANSFORM_FEEDBACK_EXT;
        }

        let this = self as *mut Self;
        self.query_cache
            .notify_wfi(&mut self.scheduler, move |operation| unsafe {
                (*this).sync_operation(operation);
            });

        let device = self.device;
        let event = self.wfi_event;
        self.scheduler
            .request_outside_render_pass_operation_context();
        self.scheduler.record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_set_event(cmdbuf, event, flags);
            device.cmd_wait_events(
                cmdbuf,
                &[event],
                flags,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                &[],
                &[],
                &[],
            );
        });
        let this = self as *mut Self;
        let this_for_pop = this as usize;
        self.fence_manager.signal_ordering(
            move || unsafe { (*this).should_wait_async_flushes() },
            move |fence| unsafe { (*this).is_fence_signaled(fence) },
            move || unsafe { (*(this_for_pop as *mut Self)).pop_async_flushes() },
            move || unsafe { (*this).accumulate_flushes() },
        );
    }

    fn fragment_barrier(&mut self) {
        // Upstream `RasterizerVulkan::FragmentBarrier` ends the active render
        // pass. `Scheduler::request_outside_render_pass_operation_context` emits the attachment
        // write barrier needed before a later texture read.
        self.scheduler
            .request_outside_render_pass_operation_context();
    }

    fn tiled_cache_barrier(&mut self) {}

    fn flush_commands(&mut self) {
        if self.draw_counter == 0 {
            return;
        }
        self.draw_counter = 0;
        self.scheduler.flush();
    }

    fn tick_frame(&mut self) {
        self.draw_counter = 0;
        // Upstream `RasterizerVulkan::TickFrame` rotates both descriptor
        // queues to the next per-frame payload slice before anything else
        // (vk_rasterizer.cpp:765-766). Without this the ring never advances
        // and in-flight frames overwrite each other's descriptor payload.
        self.desc_queue.tick_frame();
        self.compute_pass_desc_queue.tick_frame();
        self.descriptor_buffer_ring.tick_frame();
        self.state_tracker.invalidate_command_buffer_state();
        self.fence_manager.tick_frame();
        self.staging_pool.new_frame();
        // Retire delayed-destruction rings against GPU completion, not the
        // submission counter (pipelined submissions run ahead of the GPU).
        let known_gpu_tick = self.scheduler.known_gpu_tick();
        unsafe {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let _texture_guard = (*texture_mutex).lock();
            self.texture_cache.tick_frame(known_gpu_tick);
        }
        unsafe {
            let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            self.common_buffer_cache.tick_frame();
        }
    }

    fn initialize_channel(&mut self, channel: &mut crate::control::channel_state::ChannelState) {
        self.channel_caches.create_channel(channel);
        {
            let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);
            self.texture_cache.create_channel(channel);
            self.common_buffer_cache.create_channel(channel);
        }
        self.shader_cache.create_channel(channel);
        self.pipeline_cache.create_channel(channel);
        self.query_cache.create_channel(channel);
        self.state_tracker.setup_tables(channel);
    }

    fn bind_channel(&mut self, channel: &mut crate::control::channel_state::ChannelState) {
        self.channel_caches.bind_to_channel(channel.bind_id);
        {
            let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);
            self.texture_cache.bind_to_channel(channel.bind_id);
            self.common_buffer_cache.bind_to_channel(channel.bind_id);
        }
        self.shader_cache.bind_to_channel(channel.bind_id);
        self.pipeline_cache.bind_to_channel(channel.bind_id);
        self.query_cache.bind_to_channel(channel.bind_id);
        self.state_tracker.change_channel(channel);
        self.state_tracker.invalidate_state();
        self.channel_memory_manager = self
            .channel_caches
            .current_channel_state()
            .and_then(ChannelCacheAccessor::gpu_memory_arc);
        self.channel_memory_manager_handle = self.channel_memory_manager.as_ref().map(|mm| {
            let memory = mm.lock();
            MemoryManagerHandle::from_ref(&memory)
        });
        if let Some(mm) = self.channel_memory_manager_handle {
            self.common_buffer_cache
                .set_gpu_memory(Box::new(GpuMemoryAccessAdapter { mm }));
        }
    }

    fn release_channel(&mut self, channel_id: i32) {
        self.state_tracker.release_channel(channel_id);
        self.channel_caches.erase_channel(channel_id);
        {
            let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);
            self.texture_cache.erase_channel(channel_id);
            self.common_buffer_cache.erase_channel(channel_id);
        }
        self.shader_cache.erase_channel(channel_id);
        self.pipeline_cache.erase_channel(channel_id);
        self.query_cache.erase_channel(channel_id);
        self.channel_memory_manager = self
            .channel_caches
            .current_channel_state()
            .and_then(ChannelCacheAccessor::gpu_memory_arc);
        self.channel_memory_manager_handle = self.channel_memory_manager.as_ref().map(|mm| {
            let memory = mm.lock();
            MemoryManagerHandle::from_ref(&memory)
        });
        if let Some(mm) = self.channel_memory_manager_handle {
            self.common_buffer_cache
                .set_gpu_memory(Box::new(GpuMemoryAccessAdapter { mm }));
        } else {
            self.common_buffer_cache.clear_gpu_memory();
        }
    }

    fn accelerate_surface_copy(
        &mut self,
        src: &crate::engines::fermi_2d::Surface,
        dst: &crate::engines::fermi_2d::Surface,
        copy_config: &crate::engines::fermi_2d::Config,
    ) -> bool {
        let texture_cache: *mut TextureCache = &mut *self.texture_cache;
        unsafe {
            let _texture_lock = (*texture_cache).base.mutex.lock();
            (*texture_cache).blit_image(dst, src, copy_config)
        }
    }

    fn access_accelerate_dma(&mut self) -> &mut dyn AccelerateDMAInterface {
        &mut self.accelerate_dma
    }

    fn accelerate_inline_to_memory(&mut self, address: u64, copy_size: usize, memory: &[u8]) {
        debug_assert!(copy_size <= memory.len());
        if copy_size == 0 {
            return;
        }

        let Some(mm) = self.channel_memory_manager.as_ref().cloned() else {
            return;
        };

        let mm = mm.lock();
        let cpu_addr = mm.gpu_to_cpu_address(address);
        // SAFETY: upstream accepts a span whose backing allocation is required
        // to contain copy_size bytes and forwards memory.data() plus copy_size
        // without a runtime bounds check. The debug assertion diagnoses a
        // broken caller contract without adding release-only truncation or a
        // Rust bounds panic that upstream does not have.
        let input = unsafe { std::slice::from_raw_parts(memory.as_ptr(), copy_size) };
        if cpu_addr.is_none() {
            mm.write_block(address, input);
            return;
        }
        mm.write_block_unsafe(address, input);
        drop(mm);

        let cpu_addr = cpu_addr.unwrap();
        unsafe {
            let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
            let _buffer_guard = (*buffer_mutex).lock();
            if !self
                .common_buffer_cache
                .inline_memory(cpu_addr, copy_size, input)
            {
                self.common_buffer_cache
                    .write_memory(cpu_addr, copy_size as u64);
            }
        }
        unsafe {
            let texture_mutex: *const _ = &self.texture_cache.base.mutex;
            let _texture_guard = (*texture_mutex).lock();
            self.texture_cache.base.write_memory(cpu_addr, copy_size);
        }
        self.shader_cache.invalidate_region(cpu_addr, copy_size);
        self.query_cache.invalidate_region(cpu_addr, copy_size);
    }

    fn has_draw_transform_feedback(&self) -> bool {
        self.transform_feedback_draw_supported
    }
}

impl Drop for RasterizerVulkan {
    fn drop(&mut self) {
        // Port of `RasterizerVulkan::~RasterizerVulkan`. `finish()` realizes
        // deferred clears while their framebuffer and render-pass-cache owners
        // are still alive, submits the command chunk, and waits for completion.
        self.scheduler.wait_worker();
        self.scheduler.finish();
        // The renderer destroys this rasterizer before the scheduler. Drop its
        // shared query-state handles before the Vulkan resources they describe.
        self.scheduler.clear_query_cache_state();
        let device = self.device.get().get_logical();
        unsafe {
            device.destroy_sampler(self.fallback_sampler, None);
            device.unmap_memory(self.fallback_uniform_memory);
            device.destroy_buffer(self.fallback_uniform_buffer, None);
            device.free_memory(self.fallback_uniform_memory, None);

            device.destroy_event(self.wfi_event, None);
        }
    }
}

// â”€â”€ State mapping helpers (reused from old renderer.rs) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
pub(crate) fn map_topology(topo: PrimitiveTopology) -> vk::PrimitiveTopology {
    match topo {
        PrimitiveTopology::Points => vk::PrimitiveTopology::POINT_LIST,
        PrimitiveTopology::Lines => vk::PrimitiveTopology::LINE_LIST,
        PrimitiveTopology::LineStrip => vk::PrimitiveTopology::LINE_STRIP,
        PrimitiveTopology::Triangles => vk::PrimitiveTopology::TRIANGLE_LIST,
        PrimitiveTopology::TriangleStrip => vk::PrimitiveTopology::TRIANGLE_STRIP,
        PrimitiveTopology::TriangleFan => vk::PrimitiveTopology::TRIANGLE_FAN,
        _ => vk::PrimitiveTopology::TRIANGLE_LIST,
    }
}

#[cfg(test)]
pub(crate) fn map_cull_mode(
    rasterizer: &crate::engines::maxwell_3d::RasterizerInfo,
) -> vk::CullModeFlags {
    if !rasterizer.cull_enable {
        return vk::CullModeFlags::NONE;
    }
    match rasterizer.cull_face {
        CullFace::Front => vk::CullModeFlags::FRONT,
        CullFace::Back => vk::CullModeFlags::BACK,
        CullFace::FrontAndBack => vk::CullModeFlags::FRONT_AND_BACK,
    }
}

#[cfg(test)]
pub(crate) fn map_front_face(ff: FrontFace) -> vk::FrontFace {
    match ff {
        FrontFace::CW => vk::FrontFace::CLOCKWISE,
        FrontFace::CCW => vk::FrontFace::COUNTER_CLOCKWISE,
    }
}

#[cfg(test)]
pub(crate) fn map_compare_op(op: ComparisonOp) -> vk::CompareOp {
    match op {
        ComparisonOp::Never => vk::CompareOp::NEVER,
        ComparisonOp::Less => vk::CompareOp::LESS,
        ComparisonOp::Equal => vk::CompareOp::EQUAL,
        ComparisonOp::LessEqual => vk::CompareOp::LESS_OR_EQUAL,
        ComparisonOp::Greater => vk::CompareOp::GREATER,
        ComparisonOp::NotEqual => vk::CompareOp::NOT_EQUAL,
        ComparisonOp::GreaterEqual => vk::CompareOp::GREATER_OR_EQUAL,
        ComparisonOp::Always => vk::CompareOp::ALWAYS,
    }
}

#[cfg(test)]
pub(crate) fn map_blend_factor(factor: BlendFactor) -> vk::BlendFactor {
    match factor {
        BlendFactor::Zero => vk::BlendFactor::ZERO,
        BlendFactor::One => vk::BlendFactor::ONE,
        BlendFactor::SrcColor => vk::BlendFactor::SRC_COLOR,
        BlendFactor::OneMinusSrcColor => vk::BlendFactor::ONE_MINUS_SRC_COLOR,
        BlendFactor::SrcAlpha => vk::BlendFactor::SRC_ALPHA,
        BlendFactor::OneMinusSrcAlpha => vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        BlendFactor::DstAlpha => vk::BlendFactor::DST_ALPHA,
        BlendFactor::OneMinusDstAlpha => vk::BlendFactor::ONE_MINUS_DST_ALPHA,
        BlendFactor::DstColor => vk::BlendFactor::DST_COLOR,
        BlendFactor::OneMinusDstColor => vk::BlendFactor::ONE_MINUS_DST_COLOR,
        BlendFactor::SrcAlphaSaturate => vk::BlendFactor::SRC_ALPHA_SATURATE,
        BlendFactor::ConstantColor => vk::BlendFactor::CONSTANT_COLOR,
        BlendFactor::OneMinusConstantColor => vk::BlendFactor::ONE_MINUS_CONSTANT_COLOR,
        BlendFactor::ConstantAlpha => vk::BlendFactor::CONSTANT_ALPHA,
        BlendFactor::OneMinusConstantAlpha => vk::BlendFactor::ONE_MINUS_CONSTANT_ALPHA,
        BlendFactor::Src1Color => vk::BlendFactor::SRC1_COLOR,
        BlendFactor::OneMinusSrc1Color => vk::BlendFactor::ONE_MINUS_SRC1_COLOR,
        BlendFactor::Src1Alpha => vk::BlendFactor::SRC1_ALPHA,
        BlendFactor::OneMinusSrc1Alpha => vk::BlendFactor::ONE_MINUS_SRC1_ALPHA,
    }
}

#[cfg(test)]
pub(crate) fn map_blend_equation(eq: BlendEquation) -> vk::BlendOp {
    match eq {
        BlendEquation::Add => vk::BlendOp::ADD,
        BlendEquation::Subtract => vk::BlendOp::SUBTRACT,
        BlendEquation::ReverseSubtract => vk::BlendOp::REVERSE_SUBTRACT,
        BlendEquation::Min => vk::BlendOp::MIN,
        BlendEquation::Max => vk::BlendOp::MAX,
    }
}

fn propagate_consumed_dirty_flags(
    draw: &mut Maxwell3DDrawView<'_>,
    original: &[bool; 256],
    current: &[bool; 256],
) {
    for (index, (&was_dirty, &is_dirty)) in original.iter().zip(current).enumerate() {
        if was_dirty && !is_dirty {
            draw.clear_dirty_flag(index as u8);
        }
    }
}

// â”€â”€ Vulkan resource creation helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn find_memory_type(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    type_filter: u32,
    properties: vk::MemoryPropertyFlags,
) -> Option<u32> {
    let mem_props = unsafe { instance.get_physical_device_memory_properties(physical_device) };
    for i in 0..mem_props.memory_type_count {
        if (type_filter & (1 << i)) != 0
            && mem_props.memory_types[i as usize]
                .property_flags
                .contains(properties)
        {
            return Some(i);
        }
    }
    None
}

fn create_fallback_sampler(device: &ash::Device) -> Result<vk::Sampler, RendererError> {
    let sampler_info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::NEAREST)
        .min_filter(vk::Filter::NEAREST)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .min_lod(0.0)
        .max_lod(0.0);

    unsafe {
        device
            .create_sampler(&sampler_info, None)
            .map_err(|e| RendererError::InitFailed(format!("fallback sampler: {:?}", e)))
    }
}

#[cfg(test)]
fn null_buffer_descriptor(
    has_null_descriptor: bool,
    fallback_buffer: vk::Buffer,
) -> (vk::Buffer, vk::DeviceSize, vk::DeviceSize) {
    if has_null_descriptor {
        // Keep the non-zero range used by BufferCacheRuntime; the buffer
        // handle itself is ignored by VK_EXT_robustness2 null descriptors.
        (vk::Buffer::null(), 0, 1)
    } else {
        (fallback_buffer, 0, 0x10000)
    }
}

fn create_host_buffer(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    device: &ash::Device,
    size: u64,
    usage: vk::BufferUsageFlags,
) -> Result<(vk::Buffer, vk::DeviceMemory, *mut u8), RendererError> {
    let buf_info = vk::BufferCreateInfo::default()
        .size(size)
        .usage(usage)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);

    let buffer = unsafe {
        device
            .create_buffer(&buf_info, None)
            .map_err(|e| RendererError::InitFailed(format!("buffer: {:?}", e)))?
    };

    let mem_reqs = unsafe { device.get_buffer_memory_requirements(buffer) };
    let mem_type = find_memory_type(
        instance,
        physical_device,
        mem_reqs.memory_type_bits,
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
    )
    .ok_or_else(|| RendererError::InitFailed("no host-visible memory".into()))?;

    let alloc_info = vk::MemoryAllocateInfo::default()
        .allocation_size(mem_reqs.size)
        .memory_type_index(mem_type);
    let memory = unsafe {
        device
            .allocate_memory(&alloc_info, None)
            .map_err(|e| RendererError::InitFailed(format!("buffer memory: {:?}", e)))?
    };
    unsafe {
        device
            .bind_buffer_memory(buffer, memory, 0)
            .map_err(|e| RendererError::InitFailed(format!("bind buffer: {:?}", e)))?;
    }

    let mapped = unsafe {
        device
            .map_memory(memory, 0, size, vk::MemoryMapFlags::empty())
            .map_err(|e| RendererError::InitFailed(format!("map buffer: {:?}", e)))?
            as *mut u8
    };

    Ok((buffer, memory, mapped))
}

/// Mechanical extraction of Clear's two resolution scalars. Eden reads the
/// global directly; Rust must release its settings read guard before invoking
/// RecordViewports (or any other operation that reads settings again). A pending
/// UI writer otherwise turns the nested read into a deadlock.
fn clear_rescaling(is_rescaling: bool) -> (u32, u32) {
    if is_rescaling {
        let values = common::settings::values();
        (values.resolution_info.up_scale, values.resolution_info.down_shift)
    } else {
        (1, 0)
    }
}

// â”€â”€ Tests â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_dispatch_requires_one_mapping_record_per_host_group() {
        assert!(mesh_dispatch_mapping_fits(2, 2, 24));
        assert!(!mesh_dispatch_mapping_fits(2, 1, 24));
        assert!(!mesh_dispatch_mapping_fits(2, 2, 23));
        assert!(mesh_dispatch_mapping_fits(0, 2, 0));
        assert!(!mesh_dispatch_mapping_fits(u32::MAX, u32::MAX, u32::MAX as u64));
        assert!(mesh_dispatch_mapping_fits(u32::MAX, u32::MAX, u64::from(u32::MAX) * 12));
    }

    #[test]
    fn clear_rescaling_releases_settings_before_viewport_work() {
        let expected = {
            let values = common::settings::values();
            (values.resolution_info.up_scale, values.resolution_info.down_shift)
        };
        let scale = clear_rescaling(true);
        assert_eq!(scale, expected);
        let (tx, rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let _settings = common::settings::values_mut();
            tx.send(()).unwrap();
        });
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .expect("Clear must not retain a settings reader while the UI writes");
        writer.join().unwrap();
        // Viewport work can acquire its own read while Clear retains the scalars.
        let _viewport_settings = common::settings::values();
        assert_eq!(scale, expected);
    }

    #[test]
    fn unscaled_clear_does_not_read_settings() {
        let writer = common::settings::values_mut();
        let (tx, rx) = std::sync::mpsc::channel();
        let clear = std::thread::spawn(move || tx.send(clear_rescaling(false)).unwrap());
        let result = rx.recv_timeout(std::time::Duration::from_secs(2));
        drop(writer);
        clear.join().unwrap();
        assert_eq!(result.unwrap(), (1, 0));
    }

    #[test]
    fn gpu_tick_scope_exit_borrows_callback_without_arc_clone() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let callback_calls = Arc::clone(&calls);
        let callback: crate::renderer_base::GpuTickCallback = Arc::new(move || {
            callback_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        let owners_before = Arc::strong_count(&callback);

        {
            let _guard = GpuTickGuard::new(Some(&callback));
            assert_eq!(Arc::strong_count(&callback), owners_before);
        }

        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn buffer_cache_gpu_memory_adapter_uses_bound_non_owning_pointer() {
        let memory = Arc::new(parking_lot::Mutex::new(
            crate::memory_manager::MemoryManager::new(0),
        ));
        let guard = memory.lock();
        let adapter = GpuMemoryAccessAdapter {
            mm: MemoryManagerHandle::from_ref(&guard),
        };

        // Deliberately keep the non-reentrant owner mutex held. Eden's buffer
        // cache calls its stored `Tegra::MemoryManager*` directly; the Rust
        // adapter must not acquire that mutex again for each address lookup.
        assert_eq!(adapter.gpu_to_cpu_address(0x1000), None);
        assert_eq!(adapter.read_u32(0x1000), Some(0));
    }

    #[test]
    fn rasterizer_borrows_upstream_device_owner() {
        fn device_reference(rasterizer: &RasterizerVulkan) -> DeviceReference {
            rasterizer.device
        }
        fn require_signature(_: fn(&RasterizerVulkan) -> DeviceReference) {}

        require_signature(device_reference);
        assert_eq!(
            std::mem::size_of::<DeviceReference>(),
            std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn gpu_draw_and_dispatch_log_payloads_match_upstream() {
        let indexed = DrawParams {
            base_instance: 9,
            num_instances: 2,
            base_vertex: -4,
            num_vertices: 12,
            first_index: 3,
            is_indexed: true,
        };
        assert_eq!(
            direct_draw_log_info(indexed),
            (
                "vkCmdDrawIndexed",
                "vertices=12, instances=2, firstIndex=3, baseVertex=-4, baseInstance=9".to_owned()
            )
        );
        assert_eq!(
            indirect_draw_log_info(false, 7, 20),
            ("vkCmdDrawIndirect", "drawCount=7, stride=20".to_owned())
        );
        assert_eq!(
            dispatch_log_info([8, 4, 2]),
            "groupCountX=8, groupCountY=4, groupCountZ=2"
        );
    }

    #[test]
    fn test_map_topology() {
        assert_eq!(
            map_topology(PrimitiveTopology::Triangles),
            vk::PrimitiveTopology::TRIANGLE_LIST
        );
        assert_eq!(
            map_topology(PrimitiveTopology::Points),
            vk::PrimitiveTopology::POINT_LIST
        );
        assert_eq!(
            map_topology(PrimitiveTopology::TriangleStrip),
            vk::PrimitiveTopology::TRIANGLE_STRIP
        );
    }

    #[test]
    fn test_map_cull_mode() {
        let mut rasterizer = crate::engines::maxwell_3d::RasterizerInfo::default();
        assert_eq!(map_cull_mode(&rasterizer), vk::CullModeFlags::NONE);

        rasterizer.cull_enable = true;
        rasterizer.cull_face = CullFace::Front;
        assert_eq!(map_cull_mode(&rasterizer), vk::CullModeFlags::FRONT);
        rasterizer.cull_face = CullFace::Back;
        assert_eq!(map_cull_mode(&rasterizer), vk::CullModeFlags::BACK);
        rasterizer.cull_face = CullFace::FrontAndBack;
        assert_eq!(
            map_cull_mode(&rasterizer),
            vk::CullModeFlags::FRONT_AND_BACK
        );
    }

    #[test]
    fn missing_buffer_uses_null_descriptor_when_supported() {
        let fallback = vk::Buffer::from_raw(0x1234);
        assert_eq!(
            null_buffer_descriptor(true, fallback),
            (vk::Buffer::null(), 0, 1)
        );
        assert_eq!(
            null_buffer_descriptor(false, fallback),
            (fallback, 0, 0x10000)
        );
    }

    #[test]
    fn test_map_compare_op() {
        assert_eq!(map_compare_op(ComparisonOp::Less), vk::CompareOp::LESS);
        assert_eq!(map_compare_op(ComparisonOp::Always), vk::CompareOp::ALWAYS);
        assert_eq!(map_compare_op(ComparisonOp::Never), vk::CompareOp::NEVER);
    }

    #[test]
    fn test_map_blend_factor() {
        assert_eq!(map_blend_factor(BlendFactor::One), vk::BlendFactor::ONE);
        assert_eq!(
            map_blend_factor(BlendFactor::SrcAlpha),
            vk::BlendFactor::SRC_ALPHA
        );
    }

    #[test]
    fn test_map_blend_equation() {
        assert_eq!(map_blend_equation(BlendEquation::Add), vk::BlendOp::ADD);
        assert_eq!(map_blend_equation(BlendEquation::Min), vk::BlendOp::MIN);
    }

    #[test]
    fn test_map_front_face() {
        assert_eq!(map_front_face(FrontFace::CW), vk::FrontFace::CLOCKWISE);
        assert_eq!(
            map_front_face(FrontFace::CCW),
            vk::FrontFace::COUNTER_CLOCKWISE
        );
    }

    #[test]
    fn viewport_identity_scale() {
        let viewport = get_viewport_state(
            320.0, 320.0, 240.0, 240.0, 0.5, 0.5, 1.0, false, false, false, 480.0, false,
        );
        assert_eq!(viewport.x, 0.0);
        assert_eq!(viewport.width, 640.0);
        assert_eq!(viewport.y, 0.0);
        assert_eq!(viewport.height, 480.0);
    }

    #[test]
    fn viewport_rescaling_matches_upstream_factor_and_rounding() {
        let upscaled = get_viewport_state(
            321.0, 319.0, 241.0, 239.0, 0.5, 0.5, 1.5, false, false, false, 480.0, false,
        );
        assert_eq!(upscaled.x, 3.0);
        assert_eq!(upscaled.width, 957.0);
        assert_eq!(upscaled.y, 3.0);
        assert_eq!(upscaled.height, 717.0);

        let downscaled = get_viewport_state(
            318.0, 320.0, 238.0, 240.0, 0.5, 0.5, 0.75, false, false, false, 480.0, false,
        );
        assert_eq!(downscaled.x, -2.0);
        assert_eq!(downscaled.width, 480.0);
        assert_eq!(downscaled.y, -2.0);
        assert_eq!(downscaled.height, 360.0);
    }

    #[test]
    fn viewport_depth_range_matches_extension_support() {
        let clamped = get_viewport_state(
            0.0, 1.0, 0.0, 1.0, 2.0, 2.0, 1.0, false, false, false, 1.0, true,
        );
        assert_eq!(clamped.min_depth, 1.0);
        assert_eq!(clamped.max_depth, 1.0);

        let unrestricted = get_viewport_state(
            0.0, 1.0, 0.0, 1.0, 2.0, 2.0, 1.0, false, false, false, 1.0, false,
        );
        assert_eq!(unrestricted.min_depth, 2.0);
        assert_eq!(unrestricted.max_depth, 4.0);
    }

    #[test]
    fn scissor_rescaling_matches_upstream_signed_rounding() {
        use crate::engines::maxwell_3d::{
            Maxwell3D, DRAW_BEGIN, DRAW_END, SCISSOR_BASE, SURFACE_CLIP_BASE, WINDOW_ORIGIN,
        };

        let mut engine = Maxwell3D::new();
        engine.write_reg(SURFACE_CLIP_BASE, 20 << 16);
        engine.write_reg(SURFACE_CLIP_BASE + 1, 20 << 16);
        engine.write_reg(WINDOW_ORIGIN, 1);
        engine.write_reg(SCISSOR_BASE, 1);
        engine.write_reg(SCISSOR_BASE + 1, 3 | (7 << 16));
        engine.write_reg(SCISSOR_BASE + 2, 5 | (9 << 16));
        engine.write_reg(DRAW_BEGIN, PrimitiveTopology::Triangles as u32);
        engine.write_reg(DRAW_END, 0);

        let draw = engine.take_draw_calls().remove(0);
        let scissor = scissor_state(
            draw.scissors[0],
            draw.window_origin_lower_left,
            draw.surface_clip.height,
            1,
            1,
        );
        assert_eq!(scissor.offset.x, 2);
        assert_eq!(scissor.offset.y, 6);
        assert_eq!(scissor.extent.width, 2);
        assert_eq!(scissor.extent.height, 2);
    }

    #[test]
    fn color_clear_value_matches_upstream_format_conversion() {
        let float_value = make_color_clear_value(
            crate::surface::PixelFormat::B10G11R11Float,
            [0.25, 0.5, 0.75, 1.0],
        );
        let uint_value =
            make_color_clear_value(crate::surface::PixelFormat::R8Uint, [0.25, 0.5, 0.75, 1.0]);
        let sint_value =
            make_color_clear_value(crate::surface::PixelFormat::R8Sint, [0.0, 0.5, 1.0, 0.25]);

        unsafe {
            assert_eq!(float_value.color.float32, [0.25, 0.5, 0.75, 1.0]);
            assert_eq!(uint_value.color.uint32, [4, 8, 12, 16]);
            assert_eq!(sint_value.color.int32, [-7, 0, 7, -3]);
        }
    }

    #[test]
    fn primitive_restart_topology_filter_matches_upstream() {
        assert!(!supports_primitive_restart(
            vk::PrimitiveTopology::POINT_LIST
        ));
        assert!(!supports_primitive_restart(
            vk::PrimitiveTopology::TRIANGLE_LIST
        ));
        assert!(!supports_primitive_restart(
            vk::PrimitiveTopology::PATCH_LIST
        ));
        assert!(supports_primitive_restart(
            vk::PrimitiveTopology::TRIANGLE_STRIP
        ));
        assert!(supports_primitive_restart(
            vk::PrimitiveTopology::LINE_STRIP_WITH_ADJACENCY
        ));
    }

    #[test]
    fn vertex_input_descriptions_keep_upstream_capacity_inline() {
        let mut bindings = VertexInputBindings::new();
        let mut attributes = VertexInputAttributes::new();
        for index in 0..32 {
            bindings.push(
                vk::VertexInputBindingDescription2EXT::default()
                    .binding(index),
            );
            attributes.push(
                vk::VertexInputAttributeDescription2EXT::default()
                    .location(index),
            );
        }
        assert!(!bindings.spilled());
        assert!(!attributes.spilled());
    }

    #[test]
    fn d24_depth_bias_workaround_matches_upstream_title_gate() {
        let ordinary = depth_bias_constant(4.0, 0x14, false, 0);
        let native_d24 = depth_bias_constant(4.0, 0x14, true, NEEDS_D24[0]);
        let adjusted = depth_bias_constant(4.0, 0x14, false, NEEDS_D24[0]);

        assert_eq!(ordinary, 2.0);
        assert_eq!(native_d24, 2.0);
        assert_eq!(adjusted, (2.0_f64 * 256.0 / f32::MAX as f64) as f32);
    }

    #[test]
    fn counter_reset_filter_matches_upstream_switch() {
        assert!(supports_counter_reset(QueryType::ZPassPixelCount64 as u32));
        assert!(supports_counter_reset(QueryType::StreamingByteCount as u32));
        assert!(supports_counter_reset(
            QueryType::StreamingPrimitivesSucceeded as u32
        ));
        assert!(supports_counter_reset(QueryType::VtgPrimitivesOut as u32));
        assert!(!supports_counter_reset(QueryType::Payload as u32));
        assert!(!supports_counter_reset(
            QueryType::StreamingPrimitivesNeeded as u32
        ));
    }

    #[test]
    fn dirty_flag_bridge_only_consumes_flags_dirty_at_draw_entry() {
        let draw_state = DrawState::default();
        let mut registers = Maxwell3DDrawRegisters::default();
        registers.dirty_flags[7] = true;
        let mut draw = Maxwell3DDrawView::with_register_snapshot(&draw_state, false, registers);
        let original = *draw.dirty_flags();
        let mut current = original;
        current[7] = false;
        current[8] = false;
        draw.set_dirty_flag(8);

        propagate_consumed_dirty_flags(&mut draw, &original, &current);

        assert!(!draw.dirty_flag(7));
        assert!(draw.dirty_flag(8));
    }
}

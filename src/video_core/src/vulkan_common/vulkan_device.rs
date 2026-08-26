// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of upstream `video_core/vulkan_common/vulkan_device.h` and
//! `video_core/vulkan_common/vulkan_device.cpp`.
//!
//! Manages a Vulkan physical+logical device pair, collecting device properties,
//! features, and extensions. Provides accessors matching the C++ `Device` class.

use ash::vk;
use ash::vk::Handle;
use std::collections::{BTreeSet, HashMap};
use std::ffi::{CStr, CString};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use crate::gpu_logging::{get_instance, DriverType, LogLevel};

use super::nsight_aftermath_tracker::NsightAftermathTracker;
use super::vma::VmaAllocator;
use super::vulkan_wrapper::{get_physical_device_tool_properties, LogicalDevice, VulkanError};

// ---------------------------------------------------------------------------
// Constants — port of constants from vulkan_device.h
// ---------------------------------------------------------------------------

/// Subgroup size of the guest emulated hardware (Nvidia has 32 threads per subgroup).
///
/// Port of `GuestWarpSize` from `vulkan_device.h`.
pub const GUEST_WARP_SIZE: u32 = 32;
const ONE_GIB: u64 = 1024 * 1024 * 1024;

/// Stable non-owning Rust counterpart of upstream's pervasive `const Device&`
/// members. Renderer owners are boxed before these references are created and
/// are destroyed after every dependent cache/pipeline worker has joined.
#[derive(Clone, Copy)]
pub(crate) struct DeviceReference(NonNull<Device>);

impl DeviceReference {
    pub(crate) fn new(device: &Device) -> Self {
        Self(NonNull::from(device))
    }

    pub(crate) fn get(&self) -> &Device {
        // SAFETY: construction/drop ordering is documented above and mirrors
        // the lifetime of the C++ reference members this replaces.
        unsafe { self.0.as_ref() }
    }

    #[cfg(test)]
    pub(crate) fn dangling_for_test() -> Self {
        Self(NonNull::dangling())
    }
}

unsafe impl Send for DeviceReference {}
unsafe impl Sync for DeviceReference {}

// ash 0.37 predates VK_EXT_depth_bias_control. Keep the Vulkan ABI payload
// local to its upstream owner until the workspace binding is upgraded.
#[repr(C)]
#[derive(Clone, Copy)]
struct PhysicalDeviceDepthBiasControlFeaturesExt {
    s_type: vk::StructureType,
    p_next: *mut std::ffi::c_void,
    depth_bias_control: vk::Bool32,
    least_representable_value_force_unorm_representation: vk::Bool32,
    float_representation: vk::Bool32,
    depth_bias_exact: vk::Bool32,
}

impl Default for PhysicalDeviceDepthBiasControlFeaturesExt {
    fn default() -> Self {
        Self {
            s_type: vk::StructureType::from_raw(1_000_283_000),
            p_next: std::ptr::null_mut(),
            depth_bias_control: vk::FALSE,
            least_representable_value_force_unorm_representation: vk::FALSE,
            float_representation: vk::FALSE,
            depth_bias_exact: vk::FALSE,
        }
    }
}

// ash 0.37 predates VK_KHR_maintenance5. Keep its feature/property ABI
// payloads in the upstream device owner until the workspace binding is
// upgraded.
#[repr(C)]
#[derive(Clone, Copy)]
struct PhysicalDeviceMaintenance5FeaturesKhr {
    s_type: vk::StructureType,
    p_next: *mut std::ffi::c_void,
    maintenance5: vk::Bool32,
}

impl Default for PhysicalDeviceMaintenance5FeaturesKhr {
    fn default() -> Self {
        Self {
            s_type: vk::StructureType::from_raw(1_000_470_000),
            p_next: std::ptr::null_mut(),
            maintenance5: vk::FALSE,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PhysicalDeviceMaintenance5PropertiesKhr {
    s_type: vk::StructureType,
    p_next: *mut std::ffi::c_void,
    early_fragment_multisample_coverage_after_sample_counting: vk::Bool32,
    early_fragment_sample_mask_test_before_sample_counting: vk::Bool32,
    depth_stencil_swizzle_one_support: vk::Bool32,
    polygon_mode_point_size: vk::Bool32,
    non_strict_single_pixel_wide_lines_use_parallelogram: vk::Bool32,
    non_strict_wide_lines_use_parallelogram: vk::Bool32,
}

impl Default for PhysicalDeviceMaintenance5PropertiesKhr {
    fn default() -> Self {
        Self {
            s_type: vk::StructureType::from_raw(1_000_470_001),
            p_next: std::ptr::null_mut(),
            early_fragment_multisample_coverage_after_sample_counting: vk::FALSE,
            early_fragment_sample_mask_test_before_sample_counting: vk::FALSE,
            depth_stencil_swizzle_one_support: vk::FALSE,
            polygon_mode_point_size: vk::FALSE,
            non_strict_single_pixel_wide_lines_use_parallelogram: vk::FALSE,
            non_strict_wide_lines_use_parallelogram: vk::FALSE,
        }
    }
}

fn pnext_chain_has_unique_structure_types(mut next: *const std::ffi::c_void) -> bool {
    let mut structure_types = Vec::new();
    while !next.is_null() {
        let node = unsafe { &*next.cast::<vk::BaseOutStructure>() };
        if structure_types.contains(&node.s_type) {
            return false;
        }
        structure_types.push(node.s_type);
        next = node.p_next.cast();
    }
    true
}

/// Port of the descriptor-indexing policy block in upstream
/// `Device::RemoveUnsuitableExtensions`.
fn apply_descriptor_indexing_policy(features: &mut vk::PhysicalDeviceDescriptorIndexingFeatures) {
    features.shader_input_attachment_array_dynamic_indexing = vk::FALSE;
    features.shader_uniform_texel_buffer_array_dynamic_indexing = vk::FALSE;
    features.shader_storage_texel_buffer_array_dynamic_indexing = vk::FALSE;
    features.shader_uniform_buffer_array_non_uniform_indexing = vk::FALSE;
    features.shader_storage_buffer_array_non_uniform_indexing = vk::FALSE;
    features.shader_input_attachment_array_non_uniform_indexing = vk::FALSE;
    features.descriptor_binding_uniform_buffer_update_after_bind = vk::FALSE;
    features.descriptor_binding_sampled_image_update_after_bind = vk::FALSE;
    features.descriptor_binding_storage_image_update_after_bind = vk::FALSE;
    features.descriptor_binding_storage_buffer_update_after_bind = vk::FALSE;
    features.descriptor_binding_uniform_texel_buffer_update_after_bind = vk::FALSE;
    features.descriptor_binding_storage_texel_buffer_update_after_bind = vk::FALSE;
    features.descriptor_binding_update_unused_while_pending = vk::FALSE;
    features.descriptor_binding_variable_descriptor_count = vk::FALSE;
    features.runtime_descriptor_array = vk::FALSE;
}

macro_rules! clear_feature_preserving_chain {
    ($feature:expr) => {{
        let p_next = $feature.p_next;
        $feature = Default::default();
        $feature.p_next = p_next;
        let _ = &$feature;
    }};
}

// ---------------------------------------------------------------------------
// FormatType — port of `Vulkan::FormatType`
// ---------------------------------------------------------------------------

/// Format usage descriptor.
///
/// Port of `Vulkan::FormatType` from `vulkan_device.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatType {
    Linear,
    Optimal,
    Buffer,
}

fn is_tiler_driver(driver_id: vk::DriverId) -> bool {
    matches!(
        driver_id,
        vk::DriverId::QUALCOMM_PROPRIETARY
            | vk::DriverId::ARM_PROPRIETARY
            | vk::DriverId::SAMSUNG_PROPRIETARY
            | vk::DriverId::MESA_TURNIP
    )
}

/// Snapshot of the Vulkan memory heaps tracked by `Device`.
///
/// Port-facing helper for runtime owners that currently store
/// `Instance + PhysicalDevice` instead of the full `Device` wrapper but still
/// need upstream `Device::GetDeviceLocalMemory`,
/// `Device::CanReportMemoryUsage`, and `Device::GetDeviceMemoryUsage`
/// semantics.
#[derive(Debug, Clone)]
pub struct DeviceMemoryInfo {
    pub device_local_memory: u64,
    pub can_report_memory_usage: bool,
    valid_heap_memory: Vec<usize>,
}

// ---------------------------------------------------------------------------
// NvidiaArchitecture — port of `Vulkan::NvidiaArchitecture`
// ---------------------------------------------------------------------------

/// NVIDIA GPU architecture classification.
///
/// Port of `Vulkan::NvidiaArchitecture` from `vulkan_device.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NvidiaArchitecture {
    KeplerOrOlder,
    Maxwell,
    Pascal,
    Volta,
    Turing,
    AmpereOrNewer,
}

// ---------------------------------------------------------------------------
// Format alternatives — port of namespace Alternatives from vulkan_device.cpp
// ---------------------------------------------------------------------------

/// Alternative format lists for unsupported formats.
///
/// Port of `namespace Alternatives` from `vulkan_device.cpp`.
mod alternatives {
    use ash::vk;

    pub const STENCIL8_UINT: &[vk::Format] = &[
        vk::Format::D16_UNORM_S8_UINT,
        vk::Format::D24_UNORM_S8_UINT,
        vk::Format::D32_SFLOAT_S8_UINT,
    ];

    pub const DEPTH24_UNORM_STENCIL8_UINT: &[vk::Format] = &[
        vk::Format::D32_SFLOAT_S8_UINT,
        vk::Format::D16_UNORM_S8_UINT,
    ];

    pub const DEPTH24_UNORM_DONTCARE8: &[vk::Format] =
        &[vk::Format::D32_SFLOAT, vk::Format::D16_UNORM];

    pub const DEPTH16_UNORM_STENCIL8_UINT: &[vk::Format] = &[
        vk::Format::D24_UNORM_S8_UINT,
        vk::Format::D32_SFLOAT_S8_UINT,
    ];

    pub const B5G6R5_UNORM_PACK16: &[vk::Format] = &[vk::Format::R5G6B5_UNORM_PACK16];

    pub const R4G4_UNORM_PACK8: &[vk::Format] = &[vk::Format::R8_UNORM];

    pub const R16G16B16_SFLOAT: &[vk::Format] = &[vk::Format::R16G16B16A16_SFLOAT];

    pub const R16G16B16_SSCALED: &[vk::Format] = &[vk::Format::R16G16B16A16_SSCALED];

    pub const R8G8B8_SSCALED: &[vk::Format] = &[vk::Format::R8G8B8A8_SSCALED];

    pub const R32G32B32_SFLOAT: &[vk::Format] = &[vk::Format::R32G32B32A32_SFLOAT];

    pub const A4B4G4R4_UNORM_PACK16: &[vk::Format] = &[vk::Format::R4G4B4A4_UNORM_PACK16];

    /// Returns the alternative formats for a given format, or `None` if no alternatives exist.
    pub fn get_format_alternatives(format: vk::Format) -> Option<&'static [vk::Format]> {
        match format {
            vk::Format::S8_UINT => Some(STENCIL8_UINT),
            vk::Format::D24_UNORM_S8_UINT => Some(DEPTH24_UNORM_STENCIL8_UINT),
            vk::Format::X8_D24_UNORM_PACK32 => Some(DEPTH24_UNORM_DONTCARE8),
            vk::Format::D16_UNORM_S8_UINT => Some(DEPTH16_UNORM_STENCIL8_UINT),
            vk::Format::B5G6R5_UNORM_PACK16 => Some(B5G6R5_UNORM_PACK16),
            vk::Format::R4G4_UNORM_PACK8 => Some(R4G4_UNORM_PACK8),
            vk::Format::R16G16B16_SFLOAT => Some(R16G16B16_SFLOAT),
            vk::Format::R16G16B16_SSCALED => Some(R16G16B16_SSCALED),
            vk::Format::R8G8B8_SSCALED => Some(R8G8B8_SSCALED),
            vk::Format::R32G32B32_SFLOAT => Some(R32G32B32_SFLOAT),
            vk::Format::A4B4G4R4_UNORM_PACK16_EXT => Some(A4B4G4R4_UNORM_PACK16),
            _ => None,
        }
    }
}

/// Returns the alternative formats for a given format.
///
/// Port-facing wrapper for upstream `namespace Alternatives`; runtime owners
/// that do not store the full `Device` wrapper still need the same fallback
/// list when reproducing `Device::GetSupportedFormat`.
pub fn format_alternatives(format: vk::Format) -> Option<&'static [vk::Format]> {
    alternatives::get_format_alternatives(format)
}

// ---------------------------------------------------------------------------
// Extensions tracking — port of `Device::Extensions` struct
// ---------------------------------------------------------------------------

/// Tracked device extensions.
///
/// Port of the `Device::Extensions` struct from `vulkan_device.h`.
/// Fields correspond to the `FOR_EACH_VK_FEATURE_*` and `FOR_EACH_VK_EXTENSION` macros.
#[derive(Debug, Default, Clone)]
pub struct DeviceExtensions {
    // VK features 1.1
    pub bit16_storage: bool,
    pub shader_atomic_int64: bool,
    pub shader_draw_parameters: bool,
    pub shader_float16_int8: bool,
    pub uniform_buffer_standard_layout: bool,
    pub variable_pointer: bool,

    // VK features 1.2
    pub descriptor_indexing: bool,
    pub host_query_reset: bool,
    pub bit8_storage: bool,
    pub timeline_semaphore: bool,
    pub buffer_device_address: bool,

    // VK features 1.3
    pub shader_demote_to_helper_invocation: bool,
    pub subgroup_size_control: bool,
    pub synchronization2: bool,

    // VK feature extensions
    pub custom_border_color: bool,
    pub color_write_enable: bool,
    pub depth_bias_control: bool,
    pub depth_clip_control: bool,
    pub descriptor_buffer: bool,
    pub extended_dynamic_state: bool,
    pub extended_dynamic_state2: bool,
    pub extended_dynamic_state2_extra: bool,
    pub extended_dynamic_state3: bool,
    pub format_a4b4g4r4: bool,
    pub index_type_uint8: bool,
    pub line_rasterization: bool,
    pub primitive_topology_list_restart: bool,
    pub provoking_vertex: bool,
    pub robustness2: bool,
    pub transform_feedback: bool,
    pub vertex_input_dynamic_state: bool,
    pub pipeline_executable_properties: bool,
    pub workgroup_memory_explicit_layout: bool,
    pub maintenance5: bool,
    /// Diagnostic-only `VK_EXT_device_fault`, enabled through
    /// `RUZU_VK_DEVICE_FAULT` when the host exposes the feature.
    pub device_fault: bool,

    // Misc extensions
    pub conditional_rendering: bool,
    pub conservative_rasterization: bool,
    pub depth_range_unrestricted: bool,
    pub memory_budget: bool,
    pub robustness_2: bool,
    pub sampler_filter_minmax: bool,
    pub shader_stencil_export: bool,
    pub shader_viewport_index_layer: bool,
    pub tooling_info: bool,
    pub vertex_attribute_divisor: bool,
    pub draw_indirect_count: bool,
    pub driver_properties: bool,
    pub push_descriptor: bool,
    pub sampler_mirror_clamp_to_edge: bool,
    pub shader_float_controls: bool,
    pub spirv_1_4: bool,
    pub swapchain: bool,
    pub swapchain_mutable_format: bool,
    pub image_format_list: bool,
    pub device_diagnostics_config: bool,
    pub geometry_shader_passthrough: bool,
    pub viewport_array2: bool,
    pub viewport_swizzle: bool,
    pub filter_cubic: bool,
    pub filter_cubic_img: bool,
    pub filter_cubic_weights: bool,
}

// ---------------------------------------------------------------------------
// Device — port of `Vulkan::Device`
// ---------------------------------------------------------------------------

/// Handles data specific to a physical device.
///
/// Port of `Vulkan::Device` from `vulkan_device.h`.
/// This struct collects device properties, features, extensions, and manages
/// the logical device and queues.
pub struct Device {
    /// Vulkan instance handle.
    instance: ash::Instance,
    /// Physical device handle.
    physical: vk::PhysicalDevice,
    /// Vulkan Memory Allocator. Declared before `logical` so VMA is destroyed
    /// before the logical device, matching `Device::~Device` upstream.
    allocator: VmaAllocator,
    /// Logical device wrapper.
    logical: LogicalDevice,
    descriptor_buffer: Option<ash::extensions::ext::DescriptorBuffer>,
    synchronization2: Option<ash::extensions::khr::Synchronization2>,
    /// Device dispatch (ash device handle).
    _dld: ash::Device,
    /// Main graphics queue.
    graphics_queue: vk::Queue,
    /// Main present queue.
    present_queue: vk::Queue,
    /// Instance version.
    instance_version: u32,
    /// Main graphics queue family index.
    graphics_family: u32,
    /// Main present queue family index.
    present_family: u32,

    /// Tracked extensions.
    pub extensions: DeviceExtensions,

    /// Physical device properties.
    pub device_properties: vk::PhysicalDeviceProperties,
    /// Physical device driver properties.
    pub driver_properties: vk::PhysicalDeviceDriverProperties,
    /// Subgroup properties.
    pub subgroup_properties: vk::PhysicalDeviceSubgroupProperties,
    /// Float controls properties.
    pub float_controls_properties: vk::PhysicalDeviceFloatControlsProperties,
    /// Push descriptor properties.
    pub push_descriptor_properties: vk::PhysicalDevicePushDescriptorPropertiesKHR,
    /// Subgroup size control properties.
    pub subgroup_size_control_properties: vk::PhysicalDeviceSubgroupSizeControlProperties,
    /// Transform feedback properties.
    pub transform_feedback_properties: vk::PhysicalDeviceTransformFeedbackPropertiesEXT,
    /// `VK_EXT_descriptor_buffer` properties queried with the physical-device
    /// property chain.
    pub descriptor_buffer_properties: vk::PhysicalDeviceDescriptorBufferPropertiesEXT,
    /// Raw `VkPhysicalDeviceTransformFeedbackFeaturesEXT::geometryStreams` feature bit.
    pub transform_feedback_geometry_streams_supported: bool,

    /// Core physical device features.
    pub device_features: vk::PhysicalDeviceFeatures,
    /// Feature bit from `VkPhysicalDeviceShaderFloat16Int8Features`.
    pub shader_float16_supported: bool,
    /// Whether `VkPhysicalDeviceTimelineSemaphoreFeatures::timelineSemaphore`
    /// is supported and enabled (backs the scheduler's MasterSemaphore).
    pub timeline_semaphore_supported: bool,
    /// Feature bit from `VkPhysicalDeviceHostQueryResetFeatures`.
    pub host_query_reset_supported: bool,
    /// Feature bit from `VkPhysicalDeviceShaderFloat16Int8Features`.
    pub shader_int8_supported: bool,
    /// Feature bits from `VkPhysicalDevice8BitStorageFeatures`.
    pub uniform_and_storage_buffer_8bit_supported: bool,
    pub storage_buffer_8bit_supported: bool,
    /// Feature bits from `VkPhysicalDevice16BitStorageFeatures`.
    pub uniform_and_storage_buffer_16bit_supported: bool,
    pub storage_buffer_16bit_supported: bool,
    /// Feature bits from `VkPhysicalDeviceWorkgroupMemoryExplicitLayoutFeaturesKHR`.
    pub workgroup_memory_explicit_layout_8bit_access_supported: bool,
    pub workgroup_memory_explicit_layout_16bit_access_supported: bool,
    /// Feature bit from `VkPhysicalDevicePrimitiveTopologyListRestartFeaturesEXT`.
    pub primitive_topology_list_restart_supported: bool,
    /// Feature bit from `VkPhysicalDevicePrimitiveTopologyListRestartFeaturesEXT`.
    pub primitive_topology_patch_list_restart_supported: bool,
    /// Feature bit from `VkPhysicalDevice4444FormatsFeaturesEXT`.
    pub format_a4b4g4r4_supported: bool,
    /// Feature bit from `VkPhysicalDeviceRobustness2FeaturesEXT`.
    pub null_descriptor_supported: bool,
    /// Vulkan 1.2 `shaderOutputLayer` core feature.
    pub shader_output_layer_supported: bool,
    /// `VK_EXT_depth_bias_control::depthBiasExact` after suitability filtering.
    pub exact_depth_bias_control_supported: bool,
    /// Feature bit from `VkPhysicalDeviceShaderAtomicInt64Features`. Upstream
    /// keeps this separate from the buffer-side bit so that
    /// `IsSharedInt64AtomicsSupported` and `IsExtShaderAtomicInt64Supported`
    /// answer different questions.
    pub shader_shared_int64_atomics_supported: bool,
    maintenance5_early_fragment_multisample_coverage_after_sample_counting: bool,
    maintenance5_early_fragment_sample_mask_test_before_sample_counting: bool,
    maintenance5_depth_stencil_swizzle_one_supported: bool,
    maintenance5_polygon_mode_point_size_supported: bool,
    /// Feature bits from `VkPhysicalDeviceDescriptorIndexingFeatures`.
    pub descriptor_binding_partially_bound_supported: bool,
    pub sampled_image_array_non_uniform_indexing_supported: bool,
    pub storage_image_array_non_uniform_indexing_supported: bool,
    pub uniform_texel_buffer_array_non_uniform_indexing_supported: bool,
    pub storage_texel_buffer_array_non_uniform_indexing_supported: bool,

    // Misc capability flags
    pub is_optimal_astc_supported: bool,
    pub is_blit_depth24_stencil8_supported: bool,
    pub is_blit_depth32_stencil8_supported: bool,
    pub is_warp_potentially_bigger: bool,
    pub is_integrated: bool,
    pub is_virtual: bool,
    pub is_non_gpu: bool,
    pub has_geometry_shader: bool,
    pub has_tessellation_shader: bool,
    pub has_broken_compute: bool,
    pub has_broken_cube_compatibility: bool,
    pub has_broken_parallel_compiling: bool,
    pub has_renderdoc: bool,
    pub has_nsight_graphics: bool,
    pub supports_d24_depth: bool,
    pub cant_blit_msaa: bool,
    pub must_emulate_scaled_formats: bool,
    pub must_emulate_bgr565: bool,
    pub dynamic_state3_blending: bool,
    pub dynamic_state3_enables: bool,
    pub dynamic_state3_depth_clamp_enable: bool,
    pub dynamic_state3_logic_op_enable: bool,
    pub dynamic_state3_line_raster_mode: bool,
    pub dynamic_state3_conservative_raster_mode: bool,
    pub dynamic_state3_line_stipple_enable: bool,
    pub dynamic_state3_alpha_to_coverage: bool,
    pub dynamic_state3_alpha_to_one: bool,
    pub provoking_vertex_last_supported: bool,
    pub transform_feedback_preserves_provoking_vertex: bool,
    pub smooth_lines_supported: bool,
    pub stippled_rectangular_lines_supported: bool,
    pub supports_conditional_barriers: bool,
    pub device_access_memory: u64,
    pub sets_per_pool: u32,
    sampler_heap_budget: usize,
    pub nvidia_arch: NvidiaArchitecture,

    /// Reported Vulkan extensions.
    pub supported_extensions: BTreeSet<String>,
    /// Loaded Vulkan extensions.
    pub loaded_extensions: BTreeSet<String>,
    /// Memory heaps used for device-accessible memory accounting.
    pub valid_heap_memory: Vec<usize>,
    /// Format properties dictionary.
    pub format_properties: HashMap<vk::Format, vk::FormatProperties>,

    /// Nsight Aftermath tracker.
    pub nsight_aftermath_tracker: Option<NsightAftermathTracker>,
}

impl Device {
    /// Creates a new Device from a physical device.
    ///
    /// Port of `Device::Device` from `vulkan_device.cpp`.
    /// This is the main constructor that probes device properties, selects queue families,
    /// creates the logical device, and initializes all tracked capabilities.
    ///
    pub fn new(
        entry: &ash::Entry,
        instance: ash::Instance,
        physical: vk::PhysicalDevice,
        surface: vk::SurfaceKHR,
    ) -> Result<Self, VulkanError> {
        // Query basic properties
        let mut device_properties = unsafe { instance.get_physical_device_properties(physical) };

        // Query queue families
        let queue_families =
            unsafe { instance.get_physical_device_queue_family_properties(physical) };

        let surface_loader = ash::extensions::khr::Surface::new(entry, &instance);
        let mut graphics_family = None;
        let mut present_family = None;
        for (index, properties) in queue_families.iter().enumerate() {
            if graphics_family.is_some()
                && (present_family.is_some() || surface == vk::SurfaceKHR::null())
            {
                break;
            }
            if properties.queue_count == 0 {
                continue;
            }
            if properties.queue_flags.contains(vk::QueueFlags::GRAPHICS) {
                graphics_family = Some(index as u32);
            }
            if surface != vk::SurfaceKHR::null()
                && unsafe {
                    surface_loader
                        .get_physical_device_surface_support(physical, index as u32, surface)
                        .map_err(VulkanError::new)?
                }
            {
                present_family = Some(index as u32);
            }
        }
        let graphics_family = graphics_family.ok_or_else(|| {
            log::error!("Device lacks a graphics queue");
            VulkanError::new(vk::Result::ERROR_FEATURE_NOT_PRESENT)
        })?;
        let present_family = if surface == vk::SurfaceKHR::null() {
            graphics_family
        } else {
            present_family.ok_or_else(|| {
                log::error!("Device lacks a present queue");
                VulkanError::new(vk::Result::ERROR_FEATURE_NOT_PRESENT)
            })?
        };

        // Enumerate device extensions
        let available_extensions = unsafe {
            instance
                .enumerate_device_extension_properties(physical)
                .unwrap_or_default()
        };
        let supported_extensions: BTreeSet<String> = available_extensions
            .iter()
            .map(|ext| {
                let name = unsafe { CStr::from_ptr(ext.extension_name.as_ptr()) };
                name.to_string_lossy().into_owned()
            })
            .collect();
        let has_shader_float_controls =
            supported_extensions.contains("VK_KHR_shader_float_controls");
        let mut driver_properties = vk::PhysicalDeviceDriverProperties::default();
        let mut float_controls_properties = vk::PhysicalDeviceFloatControlsProperties::default();
        let has_memory_budget = supported_extensions.contains("VK_EXT_memory_budget");
        let is_integrated = device_properties.device_type == vk::PhysicalDeviceType::INTEGRATED_GPU;
        let (memory_properties, memory_budget_properties) =
            physical_memory_properties(&instance, physical, has_memory_budget);

        // Build queue create infos
        let queue_priority = [1.0f32];
        let mut queue_create_infos = vec![vk::DeviceQueueCreateInfo::builder()
            .queue_family_index(graphics_family)
            .queue_priorities(&queue_priority)
            .build()];
        if present_family != graphics_family {
            queue_create_infos.push(
                vk::DeviceQueueCreateInfo::builder()
                    .queue_family_index(present_family)
                    .queue_priorities(&queue_priority)
                    .build(),
            );
        }

        let has_primitive_topology_list_restart =
            supported_extensions.contains("VK_EXT_primitive_topology_list_restart");
        // Read after the features2 query below.
        let has_portability_subset = supported_extensions.contains("VK_KHR_portability_subset");
        let has_extended_dynamic_state =
            supported_extensions.contains("VK_EXT_extended_dynamic_state");
        let has_extended_dynamic_state2 =
            supported_extensions.contains("VK_EXT_extended_dynamic_state2");
        let has_extended_dynamic_state3 =
            supported_extensions.contains("VK_EXT_extended_dynamic_state3");
        let has_descriptor_buffer = supported_extensions.contains("VK_EXT_descriptor_buffer");
        let has_buffer_device_address = device_properties.api_version >= vk::API_VERSION_1_2
            || supported_extensions.contains("VK_KHR_buffer_device_address");
        let has_vertex_input_dynamic_state =
            supported_extensions.contains("VK_EXT_vertex_input_dynamic_state");
        let has_depth_clip_control = supported_extensions.contains("VK_EXT_depth_clip_control");
        let has_custom_border_color = supported_extensions.contains("VK_EXT_custom_border_color");
        let has_color_write_enable = supported_extensions.contains("VK_EXT_color_write_enable");
        let has_depth_bias_control = supported_extensions.contains("VK_EXT_depth_bias_control");
        let has_line_rasterization = supported_extensions.contains("VK_EXT_line_rasterization");
        let has_transform_feedback = supported_extensions.contains("VK_EXT_transform_feedback");
        let has_pipeline_executable_properties =
            supported_extensions.contains("VK_KHR_pipeline_executable_properties");
        let has_workgroup_memory_explicit_layout =
            supported_extensions.contains("VK_KHR_workgroup_memory_explicit_layout");
        let has_maintenance5 = supported_extensions.contains("VK_KHR_maintenance5");
        let has_4444_formats = supported_extensions.contains("VK_EXT_4444_formats");
        let has_index_type_uint8 = supported_extensions.contains("VK_EXT_index_type_uint8");
        let has_vertex_attribute_divisor =
            supported_extensions.contains("VK_EXT_vertex_attribute_divisor");
        let has_provoking_vertex = supported_extensions.contains("VK_EXT_provoking_vertex");
        let has_robustness2 = supported_extensions.contains("VK_EXT_robustness2");
        let has_device_fault = supported_extensions.contains("VK_EXT_device_fault")
            && std::env::var_os("RUZU_VK_DEVICE_FAULT").is_some();
        let has_shader_demote_to_helper_invocation = supported_extensions
            .contains("VK_EXT_shader_demote_to_helper_invocation")
            || device_properties.api_version >= vk::API_VERSION_1_3;
        let has_draw_indirect_count = supported_extensions.contains("VK_KHR_draw_indirect_count");
        let has_synchronization2 = device_properties.api_version >= vk::API_VERSION_1_3
            || supported_extensions.contains("VK_KHR_synchronization2");
        let has_sampler_filter_minmax =
            supported_extensions.contains("VK_EXT_sampler_filter_minmax");
        let mut storage_16bit_features = vk::PhysicalDevice16BitStorageFeatures::default();
        let mut shader_atomic_int64_features =
            vk::PhysicalDeviceShaderAtomicInt64Features::default();
        let mut shader_draw_parameters_features =
            vk::PhysicalDeviceShaderDrawParametersFeatures::default();
        let mut shader_float16_int8_features =
            vk::PhysicalDeviceShaderFloat16Int8Features::default();
        let mut uniform_buffer_standard_layout_features =
            vk::PhysicalDeviceUniformBufferStandardLayoutFeatures::default();
        let mut variable_pointers_features = vk::PhysicalDeviceVariablePointersFeatures::default();
        let mut storage_8bit_features = vk::PhysicalDevice8BitStorageFeatures::default();
        let mut host_query_reset_features = vk::PhysicalDeviceHostQueryResetFeatures::default();
        let mut portability_subset_features =
            vk::PhysicalDevicePortabilitySubsetFeaturesKHR::default();
        let mut timeline_semaphore_features =
            vk::PhysicalDeviceTimelineSemaphoreFeatures::default();
        let mut synchronization2_features = vk::PhysicalDeviceSynchronization2Features::default();
        let mut subgroup_size_control_features =
            vk::PhysicalDeviceSubgroupSizeControlFeatures::default();
        let mut vulkan12_features = vk::PhysicalDeviceVulkan12Features::default();
        let mut buffer_device_address_features =
            vk::PhysicalDeviceBufferDeviceAddressFeatures::default();
        let mut descriptor_buffer_features =
            vk::PhysicalDeviceDescriptorBufferFeaturesEXT::default();
        let mut custom_border_color_features =
            vk::PhysicalDeviceCustomBorderColorFeaturesEXT::default();
        let mut color_write_enable_features =
            vk::PhysicalDeviceColorWriteEnableFeaturesEXT::default();
        let mut depth_bias_control_features = PhysicalDeviceDepthBiasControlFeaturesExt::default();
        let mut line_rasterization_features =
            vk::PhysicalDeviceLineRasterizationFeaturesEXT::default();
        let mut transform_feedback_features =
            vk::PhysicalDeviceTransformFeedbackFeaturesEXT::default();
        let mut pipeline_executable_properties_features =
            vk::PhysicalDevicePipelineExecutablePropertiesFeaturesKHR::default();
        let mut workgroup_memory_explicit_layout_features =
            vk::PhysicalDeviceWorkgroupMemoryExplicitLayoutFeaturesKHR::default();
        let mut maintenance5_features = PhysicalDeviceMaintenance5FeaturesKhr::default();
        let mut primitive_topology_list_restart_features =
            vk::PhysicalDevicePrimitiveTopologyListRestartFeaturesEXT::default();
        let mut extended_dynamic_state_features =
            vk::PhysicalDeviceExtendedDynamicStateFeaturesEXT::default();
        let mut extended_dynamic_state2_features =
            vk::PhysicalDeviceExtendedDynamicState2FeaturesEXT::default();
        let mut extended_dynamic_state3_features =
            vk::PhysicalDeviceExtendedDynamicState3FeaturesEXT::default();
        let mut vertex_input_dynamic_state_features =
            vk::PhysicalDeviceVertexInputDynamicStateFeaturesEXT::default();
        let mut depth_clip_control_features =
            vk::PhysicalDeviceDepthClipControlFeaturesEXT::default();
        let mut formats_4444_features = vk::PhysicalDevice4444FormatsFeaturesEXT::default();
        let mut index_type_uint8_features = vk::PhysicalDeviceIndexTypeUint8FeaturesEXT::default();
        let mut provoking_vertex_features = vk::PhysicalDeviceProvokingVertexFeaturesEXT::default();
        let mut robustness2_features = vk::PhysicalDeviceRobustness2FeaturesEXT::default();
        let mut device_fault_features = vk::PhysicalDeviceFaultFeaturesEXT::default();
        let mut shader_demote_features =
            vk::PhysicalDeviceShaderDemoteToHelperInvocationFeatures::default();
        let mut descriptor_indexing_features =
            vk::PhysicalDeviceDescriptorIndexingFeatures::default();
        let mut features2 = {
            let mut features2_builder = vk::PhysicalDeviceFeatures2::builder()
                .push_next(&mut storage_16bit_features)
                .push_next(&mut shader_atomic_int64_features)
                .push_next(&mut shader_draw_parameters_features)
                .push_next(&mut shader_float16_int8_features)
                .push_next(&mut uniform_buffer_standard_layout_features)
                .push_next(&mut variable_pointers_features);
            if device_properties.api_version >= vk::API_VERSION_1_2
                || supported_extensions.contains("VK_EXT_host_query_reset")
            {
                features2_builder = features2_builder.push_next(&mut host_query_reset_features);
            }
            if device_properties.api_version >= vk::API_VERSION_1_2
                || supported_extensions.contains("VK_KHR_8bit_storage")
            {
                features2_builder = features2_builder.push_next(&mut storage_8bit_features);
            }
            if device_properties.api_version >= vk::API_VERSION_1_2
                || supported_extensions.contains("VK_KHR_timeline_semaphore")
            {
                features2_builder = features2_builder.push_next(&mut timeline_semaphore_features);
            }
            if has_synchronization2 {
                features2_builder = features2_builder.push_next(&mut synchronization2_features);
            }
            if device_properties.api_version >= vk::API_VERSION_1_3
                || supported_extensions.contains("VK_EXT_subgroup_size_control")
            {
                features2_builder =
                    features2_builder.push_next(&mut subgroup_size_control_features);
            }
            if device_properties.api_version >= vk::API_VERSION_1_2
                || supported_extensions.contains("VK_EXT_descriptor_indexing")
            {
                features2_builder = features2_builder.push_next(&mut descriptor_indexing_features);
            }
            if device_properties.api_version >= vk::API_VERSION_1_2 {
                features2_builder = features2_builder.push_next(&mut vulkan12_features);
            } else if has_buffer_device_address {
                features2_builder =
                    features2_builder.push_next(&mut buffer_device_address_features);
            }
            if has_descriptor_buffer {
                features2_builder = features2_builder.push_next(&mut descriptor_buffer_features);
            }
            if has_portability_subset {
                features2_builder = features2_builder.push_next(&mut portability_subset_features);
            }
            if has_primitive_topology_list_restart {
                features2_builder =
                    features2_builder.push_next(&mut primitive_topology_list_restart_features);
            }
            if has_extended_dynamic_state {
                features2_builder =
                    features2_builder.push_next(&mut extended_dynamic_state_features);
            }
            if has_extended_dynamic_state2 {
                features2_builder =
                    features2_builder.push_next(&mut extended_dynamic_state2_features);
            }
            if has_extended_dynamic_state3 {
                features2_builder =
                    features2_builder.push_next(&mut extended_dynamic_state3_features);
            }
            if has_vertex_input_dynamic_state {
                features2_builder =
                    features2_builder.push_next(&mut vertex_input_dynamic_state_features);
            }
            if has_depth_clip_control {
                features2_builder = features2_builder.push_next(&mut depth_clip_control_features);
            }
            if has_custom_border_color {
                features2_builder = features2_builder.push_next(&mut custom_border_color_features);
            }
            if has_color_write_enable {
                features2_builder = features2_builder.push_next(&mut color_write_enable_features);
            }
            if has_line_rasterization {
                features2_builder = features2_builder.push_next(&mut line_rasterization_features);
            }
            if has_transform_feedback {
                features2_builder = features2_builder.push_next(&mut transform_feedback_features);
            }
            if has_pipeline_executable_properties {
                features2_builder =
                    features2_builder.push_next(&mut pipeline_executable_properties_features);
            }
            if has_workgroup_memory_explicit_layout {
                features2_builder =
                    features2_builder.push_next(&mut workgroup_memory_explicit_layout_features);
            }
            if has_4444_formats {
                features2_builder = features2_builder.push_next(&mut formats_4444_features);
            }
            if has_index_type_uint8 {
                features2_builder = features2_builder.push_next(&mut index_type_uint8_features);
            }
            if has_provoking_vertex {
                features2_builder = features2_builder.push_next(&mut provoking_vertex_features);
            }
            if has_robustness2 {
                features2_builder = features2_builder.push_next(&mut robustness2_features);
            }
            if has_device_fault {
                features2_builder = features2_builder.push_next(&mut device_fault_features);
            }
            if has_shader_demote_to_helper_invocation {
                features2_builder = features2_builder.push_next(&mut shader_demote_features);
            }
            let mut features2 = features2_builder.build();
            if has_depth_bias_control {
                depth_bias_control_features.p_next = features2.p_next;
                features2.p_next = (&mut depth_bias_control_features
                    as *mut PhysicalDeviceDepthBiasControlFeaturesExt)
                    .cast();
            }
            if has_maintenance5 {
                maintenance5_features.p_next = features2.p_next;
                features2.p_next = (&mut maintenance5_features
                    as *mut PhysicalDeviceMaintenance5FeaturesKhr)
                    .cast();
            }
            features2
        };
        unsafe {
            instance.get_physical_device_features2(physical, &mut features2);
        }
        let device_features = features2.features;
        apply_descriptor_indexing_policy(&mut descriptor_indexing_features);

        macro_rules! log_recommended_feature {
            ($feature:expr, $name:literal) => {
                if $feature == vk::FALSE {
                    log::info!("Device doesn't support feature {}", $name);
                }
            };
        }
        log_recommended_feature!(
            custom_border_color_features.custom_border_colors,
            "customBorderColors"
        );
        log_recommended_feature!(
            depth_bias_control_features.depth_bias_control,
            "depthBiasControl"
        );
        log_recommended_feature!(
            depth_bias_control_features.least_representable_value_force_unorm_representation,
            "leastRepresentableValueForceUnormRepresentation"
        );
        log_recommended_feature!(
            depth_bias_control_features.depth_bias_exact,
            "depthBiasExact"
        );
        log_recommended_feature!(
            descriptor_indexing_features.descriptor_binding_partially_bound,
            "descriptorBindingPartiallyBound"
        );
        log_recommended_feature!(
            descriptor_indexing_features.shader_sampled_image_array_non_uniform_indexing,
            "shaderSampledImageArrayNonUniformIndexing"
        );
        log_recommended_feature!(
            extended_dynamic_state_features.extended_dynamic_state,
            "extendedDynamicState"
        );
        log_recommended_feature!(formats_4444_features.format_a4b4g4r4, "formatA4B4G4R4");
        log_recommended_feature!(index_type_uint8_features.index_type_uint8, "indexTypeUint8");
        log_recommended_feature!(
            primitive_topology_list_restart_features.primitive_topology_list_restart,
            "primitiveTopologyListRestart"
        );
        log_recommended_feature!(
            provoking_vertex_features.provoking_vertex_last,
            "provokingVertexLast"
        );
        log_recommended_feature!(robustness2_features.null_descriptor, "nullDescriptor");
        log_recommended_feature!(
            robustness2_features.robust_buffer_access2,
            "robustBufferAccess2"
        );
        log_recommended_feature!(
            robustness2_features.robust_image_access2,
            "robustImageAccess2"
        );
        log_recommended_feature!(shader_float16_int8_features.shader_float16, "shaderFloat16");
        log_recommended_feature!(shader_float16_int8_features.shader_int8, "shaderInt8");
        log_recommended_feature!(
            timeline_semaphore_features.timeline_semaphore,
            "timelineSemaphore"
        );
        log_recommended_feature!(
            synchronization2_features.synchronization2,
            "synchronization2"
        );
        log_recommended_feature!(
            transform_feedback_features.transform_feedback,
            "transformFeedback"
        );
        log_recommended_feature!(
            uniform_buffer_standard_layout_features.uniform_buffer_standard_layout,
            "uniformBufferStandardLayout"
        );
        log_recommended_feature!(
            vertex_input_dynamic_state_features.vertex_input_dynamic_state,
            "vertexInputDynamicState"
        );
        log_recommended_feature!(device_features.fill_mode_non_solid, "fillModeNonSolid");
        log_recommended_feature!(device_features.geometry_shader, "geometryShader");
        log_recommended_feature!(device_features.large_points, "largePoints");
        log_recommended_feature!(device_features.shader_cull_distance, "shaderCullDistance");
        log_recommended_feature!(device_features.tessellation_shader, "tessellationShader");
        log_recommended_feature!(device_features.wide_lines, "wideLines");

        let has_push_descriptor = supported_extensions.contains("VK_KHR_push_descriptor");
        let mut subgroup_properties = vk::PhysicalDeviceSubgroupProperties::default();
        let mut push_descriptor_properties =
            vk::PhysicalDevicePushDescriptorPropertiesKHR::default();
        let mut subgroup_size_control_properties =
            vk::PhysicalDeviceSubgroupSizeControlProperties::default();
        let mut transform_feedback_properties =
            vk::PhysicalDeviceTransformFeedbackPropertiesEXT::default();
        let mut descriptor_buffer_properties =
            vk::PhysicalDeviceDescriptorBufferPropertiesEXT::default();
        let mut maintenance5_properties = PhysicalDeviceMaintenance5PropertiesKhr::default();
        let mut properties2_builder = vk::PhysicalDeviceProperties2::builder()
            .push_next(&mut driver_properties)
            .push_next(&mut subgroup_properties);
        if has_shader_float_controls {
            properties2_builder = properties2_builder.push_next(&mut float_controls_properties);
        }
        if has_push_descriptor {
            properties2_builder = properties2_builder.push_next(&mut push_descriptor_properties);
        }
        if has_descriptor_buffer {
            properties2_builder = properties2_builder.push_next(&mut descriptor_buffer_properties);
        }
        if supported_extensions.contains("VK_EXT_subgroup_size_control")
            || subgroup_size_control_features.subgroup_size_control != 0
        {
            properties2_builder =
                properties2_builder.push_next(&mut subgroup_size_control_properties);
        }
        if has_transform_feedback {
            properties2_builder = properties2_builder.push_next(&mut transform_feedback_properties);
        }
        let mut properties2 = properties2_builder.build();
        if has_maintenance5 {
            maintenance5_properties.p_next = properties2.p_next;
            properties2.p_next = (&mut maintenance5_properties
                as *mut PhysicalDeviceMaintenance5PropertiesKhr)
                .cast();
        }
        unsafe {
            instance.get_physical_device_properties2(physical, &mut properties2);
        }
        device_properties = properties2.properties;

        let mut supports_shader_float16 = shader_float16_int8_features.shader_float16 != 0;
        let supports_shader_int8 = shader_float16_int8_features.shader_int8 != 0;
        let supports_timeline_semaphore = timeline_semaphore_features.timeline_semaphore != 0;
        let supports_synchronization2 =
            has_synchronization2 && synchronization2_features.synchronization2 != 0;
        let supports_maintenance5 = has_maintenance5 && maintenance5_features.maintenance5 != 0;
        let mut supports_buffer_device_address = has_buffer_device_address
            && if device_properties.api_version >= vk::API_VERSION_1_2 {
                vulkan12_features.buffer_device_address != 0
            } else {
                buffer_device_address_features.buffer_device_address != 0
            };
        let supports_descriptor_buffer = has_descriptor_buffer
            && descriptor_buffer_features.descriptor_buffer != 0
            && supports_buffer_device_address;
        if has_descriptor_buffer
            && descriptor_buffer_features.descriptor_buffer != 0
            && !supports_buffer_device_address
        {
            log::warn!("Descriptor buffer needs buffer device address, disabling.");
        }
        // Upstream only enables buffer device address as the prerequisite for
        // VK_EXT_descriptor_buffer; do not expose or enable it on its own.
        if !supports_descriptor_buffer {
            supports_buffer_device_address = false;
        }
        let supports_primitive_topology_list_restart =
            primitive_topology_list_restart_features.primitive_topology_list_restart != 0;
        let supports_primitive_topology_patch_list_restart =
            primitive_topology_list_restart_features.primitive_topology_patch_list_restart != 0;
        let mut supports_extended_dynamic_state = has_extended_dynamic_state
            && extended_dynamic_state_features.extended_dynamic_state != 0;
        let mut supports_extended_dynamic_state2 = has_extended_dynamic_state2
            && extended_dynamic_state2_features.extended_dynamic_state2 != 0;
        let mut supports_extended_dynamic_state2_extra = supports_extended_dynamic_state2
            && extended_dynamic_state2_features.extended_dynamic_state2_logic_op != 0;
        let mut supports_vertex_input_dynamic_state = has_vertex_input_dynamic_state
            && vertex_input_dynamic_state_features.vertex_input_dynamic_state != 0;
        let mut supports_custom_border_color = has_custom_border_color
            && custom_border_color_features.custom_border_colors != 0
            && custom_border_color_features.custom_border_color_without_format != 0;
        let supports_depth_bias_control = has_depth_bias_control
            && depth_bias_control_features.depth_bias_control != 0
            && depth_bias_control_features.least_representable_value_force_unorm_representation
                != 0;
        let mut supports_color_write_enable =
            has_color_write_enable && color_write_enable_features.color_write_enable != 0;
        let exact_depth_bias_control_supported =
            supports_depth_bias_control && depth_bias_control_features.depth_bias_exact != 0;
        let descriptor_binding_partially_bound_supported =
            descriptor_indexing_features.descriptor_binding_partially_bound != 0;
        let sampled_image_array_non_uniform_indexing_supported =
            descriptor_indexing_features.shader_sampled_image_array_non_uniform_indexing != 0;
        let storage_image_array_non_uniform_indexing_supported =
            descriptor_indexing_features.shader_storage_image_array_non_uniform_indexing != 0;
        let uniform_texel_buffer_array_non_uniform_indexing_supported = descriptor_indexing_features
            .shader_uniform_texel_buffer_array_non_uniform_indexing
            != 0;
        let storage_texel_buffer_array_non_uniform_indexing_supported = descriptor_indexing_features
            .shader_storage_texel_buffer_array_non_uniform_indexing
            != 0;
        let supports_transform_feedback = has_transform_feedback
            && transform_feedback_features.transform_feedback != 0
            && transform_feedback_features.geometry_streams != 0
            && transform_feedback_properties.max_transform_feedback_streams >= 4
            && transform_feedback_properties.max_transform_feedback_buffers > 0
            && transform_feedback_properties.transform_feedback_queries != 0
            && transform_feedback_properties.transform_feedback_draw != 0;
        let supports_subgroup_size_control = subgroup_size_control_features.subgroup_size_control
            != 0
            && subgroup_size_control_properties.min_subgroup_size <= GUEST_WARP_SIZE
            && subgroup_size_control_properties.max_subgroup_size >= GUEST_WARP_SIZE;
        let supports_pipeline_executable_properties = has_pipeline_executable_properties
            && *common::settings::values()
                .renderer_shader_feedback
                .get_value()
            && pipeline_executable_properties_features.pipeline_executable_info != 0;
        let supports_workgroup_memory_explicit_layout = has_workgroup_memory_explicit_layout
            && device_features.shader_int16 != 0
            && workgroup_memory_explicit_layout_features.workgroup_memory_explicit_layout != 0
            && workgroup_memory_explicit_layout_features
                .workgroup_memory_explicit_layout8_bit_access
                != 0
            && workgroup_memory_explicit_layout_features
                .workgroup_memory_explicit_layout16_bit_access
                != 0
            && workgroup_memory_explicit_layout_features
                .workgroup_memory_explicit_layout_scalar_block_layout
                != 0;
        let supports_shader_atomic_int64 = shader_atomic_int64_features.shader_buffer_int64_atomics
            != 0
            && shader_atomic_int64_features.shader_shared_int64_atomics != 0;
        let shader_shared_int64_atomics_supported =
            shader_atomic_int64_features.shader_shared_int64_atomics != 0;

        let driver_id = driver_properties.driver_id;
        let is_radv = driver_id == vk::DriverId::MESA_RADV;
        let is_amd_driver = matches!(
            driver_id,
            vk::DriverId::AMD_PROPRIETARY | vk::DriverId::AMD_OPEN_SOURCE
        );
        let is_amd = is_radv || is_amd_driver;
        let is_intel_windows = driver_id == vk::DriverId::INTEL_PROPRIETARY_WINDOWS;
        let is_intel_anv = driver_id == vk::DriverId::INTEL_OPEN_SOURCE_MESA;
        let is_nvidia = driver_id == vk::DriverId::NVIDIA_PROPRIETARY;
        let is_mvk = driver_id == vk::DriverId::MOLTENVK;
        let is_qualcomm = driver_id == vk::DriverId::QUALCOMM_PROPRIETARY;
        let is_turnip = driver_id == vk::DriverId::MESA_TURNIP;
        let is_arm = driver_id == vk::DriverId::ARM_PROPRIETARY;
        let suitable = device_is_suitable(
            device_properties.api_version,
            surface != vk::SurfaceKHR::null(),
            &supported_extensions,
            &device_features,
            &storage_16bit_features,
            &storage_8bit_features,
            &host_query_reset_features,
            &shader_demote_features,
            &shader_draw_parameters_features,
            &variable_pointers_features,
            &device_properties.limits,
        );
        if !suitable && !(is_mvk || is_qualcomm || is_turnip || is_arm) {
            return Err(VulkanError::new(vk::Result::ERROR_INCOMPATIBLE_DRIVER));
        }
        if !suitable {
            log::warn!("Unsuitable driver, continuing anyway");
        }

        let nvidia_arch = if is_nvidia {
            get_nvidia_architecture(&instance, physical, &supported_extensions)
        } else {
            NvidiaArchitecture::AmpereOrNewer
        };
        let supports_sampler_filter_minmax = sampler_filter_minmax_supported(
            has_sampler_filter_minmax,
            is_amd,
            supports_shader_float16,
        );
        if has_sampler_filter_minmax && !supports_sampler_filter_minmax {
            log::warn!("AMD GCN4 and earlier have broken VK_EXT_sampler_filter_minmax");
        }
        let mut supports_push_descriptor = has_push_descriptor;
        let mut must_emulate_scaled_formats = false;
        let mut cant_blit_msaa = false;
        let mut has_broken_cube_compatibility = false;
        let mut has_broken_parallel_compiling = false;

        if is_qualcomm || is_turnip {
            log::warn!("Qualcomm and Turnip drivers have broken VK_EXT_custom_border_color");
            supports_custom_border_color = false;
            custom_border_color_features.custom_border_colors = vk::FALSE;
            custom_border_color_features.custom_border_color_without_format = vk::FALSE;
        }
        if is_qualcomm {
            must_emulate_scaled_formats = true;
            log::warn!("Qualcomm drivers have broken VK_EXT_color_write_enable");
            supports_color_write_enable = false;
            color_write_enable_features.color_write_enable = vk::FALSE;
            log::warn!("Qualcomm drivers have broken VK_EXT_extended_dynamic_state");
            supports_extended_dynamic_state = false;
            extended_dynamic_state_features.extended_dynamic_state = vk::FALSE;
            log::warn!("Qualcomm drivers have a slow VK_KHR_push_descriptor implementation");
            supports_push_descriptor = false;
        }
        if is_arm {
            must_emulate_scaled_formats = true;
            log::warn!("ARM drivers have broken VK_EXT_extended_dynamic_state");
            supports_extended_dynamic_state = false;
            extended_dynamic_state_features.extended_dynamic_state = vk::FALSE;
        }
        if is_nvidia {
            let nv_major_version = (device_properties.driver_version >> 22) & 0x3ff;
            if nvidia_arch >= NvidiaArchitecture::AmpereOrNewer {
                log::warn!("Ampere and newer have broken float16 math");
                supports_shader_float16 = false;
                shader_float16_int8_features.shader_float16 = vk::FALSE;
            } else if nvidia_arch <= NvidiaArchitecture::Volta && nv_major_version < 527 {
                log::warn!("Volta and older have broken VK_KHR_push_descriptor");
                supports_push_descriptor = false;
            }
            if nv_major_version >= 510 {
                log::warn!("NVIDIA drivers >= 510 do not support MSAA image blits");
                cant_blit_msaa = true;
            }
        }
        let masked_driver_version = (device_properties.driver_version << 3) >> 3;
        if is_radv
            && supports_extended_dynamic_state
            && masked_driver_version < vk::make_api_version(0, 21, 2, 0)
        {
            log::warn!("RADV versions older than 21.2 have broken VK_EXT_extended_dynamic_state");
            supports_extended_dynamic_state = false;
            extended_dynamic_state_features.extended_dynamic_state = vk::FALSE;
        }
        if is_radv && supports_extended_dynamic_state2 {
            if masked_driver_version < vk::make_api_version(0, 22, 3, 1) {
                log::warn!(
                    "RADV versions older than 22.3.1 have broken VK_EXT_extended_dynamic_state2"
                );
                supports_extended_dynamic_state2 = false;
                supports_extended_dynamic_state2_extra = false;
                extended_dynamic_state2_features.extended_dynamic_state2 = vk::FALSE;
                extended_dynamic_state2_features.extended_dynamic_state2_logic_op = vk::FALSE;
                extended_dynamic_state2_features.extended_dynamic_state2_patch_control_points =
                    vk::FALSE;
            }
        }
        if is_qualcomm
            && supports_extended_dynamic_state2
            && masked_driver_version >= vk::make_api_version(0, 0, 676, 0)
            && masked_driver_version < vk::make_api_version(0, 0, 680, 0)
        {
            log::warn!("Qualcomm Adreno 7xx drivers have broken VK_EXT_extended_dynamic_state2");
            supports_extended_dynamic_state2 = false;
            supports_extended_dynamic_state2_extra = false;
            extended_dynamic_state2_features.extended_dynamic_state2 = vk::FALSE;
            extended_dynamic_state2_features.extended_dynamic_state2_logic_op = vk::FALSE;
            extended_dynamic_state2_features.extended_dynamic_state2_patch_control_points =
                vk::FALSE;
        }
        if is_radv && has_extended_dynamic_state3 {
            log::warn!("RADV has broken extendedDynamicState3ColorBlendEquation");
            extended_dynamic_state3_features.extended_dynamic_state3_color_blend_enable = vk::FALSE;
            extended_dynamic_state3_features.extended_dynamic_state3_color_blend_equation =
                vk::FALSE;
            if masked_driver_version < vk::make_api_version(0, 23, 1, 0) {
                log::warn!("RADV versions older than 23.1.0 have broken depth clamp dynamic state");
                extended_dynamic_state3_features.extended_dynamic_state3_depth_clamp_enable =
                    vk::FALSE;
            }
        }
        if is_amd_driver && has_extended_dynamic_state3 {
            log::warn!("AMD drivers have broken extendedDynamicState3ColorBlendEquation");
            extended_dynamic_state3_features.extended_dynamic_state3_color_blend_enable = vk::FALSE;
            extended_dynamic_state3_features.extended_dynamic_state3_color_blend_equation =
                vk::FALSE;
        }
        if is_radv
            && supports_vertex_input_dynamic_state
            && supported_extensions.contains("VK_KHR_fragment_shading_rate")
        {
            log::warn!("RADV has broken VK_EXT_vertex_input_dynamic_state on RDNA2 hardware");
            supports_vertex_input_dynamic_state = false;
            vertex_input_dynamic_state_features.vertex_input_dynamic_state = vk::FALSE;
        }
        if is_qualcomm && supports_vertex_input_dynamic_state {
            log::warn!("Qualcomm drivers have broken VK_EXT_vertex_input_dynamic_state");
            supports_vertex_input_dynamic_state = false;
            vertex_input_dynamic_state_features.vertex_input_dynamic_state = vk::FALSE;
        }
        if is_intel_windows
            && supports_vertex_input_dynamic_state
            && masked_driver_version < vk::make_api_version(27, 20, 100, 0)
        {
            log::warn!("Intel has broken VK_EXT_vertex_input_dynamic_state");
            supports_vertex_input_dynamic_state = false;
            vertex_input_dynamic_state_features.vertex_input_dynamic_state = vk::FALSE;
        }
        if is_intel_windows && supports_shader_float16 {
            log::warn!("Intel has broken float16 math");
            supports_shader_float16 = false;
            shader_float16_int8_features.shader_float16 = vk::FALSE;
        }
        if is_intel_windows {
            log::warn!("Intel proprietary drivers do not support MSAA image blits");
            cant_blit_msaa = true;
        }
        if is_amd_driver && !supports_shader_float16 {
            log::warn!("AMD GCN4 and earlier have broken cube image compatibility");
            has_broken_cube_compatibility = true;
        }
        if is_qualcomm && masked_driver_version < vk::make_api_version(0, 255, 615, 512) {
            has_broken_parallel_compiling = true;
        }
        if is_intel_anv
            && supports_push_descriptor
            && masked_driver_version >= vk::make_api_version(0, 22, 3, 0)
            && masked_driver_version < vk::make_api_version(0, 23, 2, 0)
        {
            log::warn!("ANV 22.3.0 through 23.1.x has broken VK_KHR_push_descriptor");
            supports_push_descriptor = false;
        }
        if is_nvidia && supports_push_descriptor && nvidia_arch <= NvidiaArchitecture::Pascal {
            log::warn!("Pascal and older have broken VK_KHR_push_descriptor");
            supports_push_descriptor = false;
        }
        if is_mvk {
            log::warn!(
                "MoltenVK breaks with more than 16 vertex attributes/bindings; capping both limits"
            );
            cap_moltenvk_vertex_input_limits(&mut device_properties.limits);
            if device_properties.driver_version < 10_400 {
                log::warn!("MoltenVK < 1.4.0 has broken VK_KHR_push_descriptor");
                supports_push_descriptor = false;
            }
        }
        if is_turnip {
            log::warn!("Turnip requires higher-than-reported vertex binding limits");
            device_properties.limits.max_vertex_input_bindings = 32;
        }
        let sampler_heap_budget = if is_qualcomm {
            derive_sampler_heap_budget(
                device_properties.limits.max_sampler_allocation_count as usize,
            )
        } else {
            0
        };
        if sampler_heap_budget != 0 {
            let sampler_limit = device_properties.limits.max_sampler_allocation_count as usize;
            log::warn!(
                "Qualcomm driver reports max {} samplers; reserving {} (25%) and allowing Reden to use {} (75%) to avoid heap exhaustion",
                sampler_limit,
                sampler_limit / 4,
                sampler_heap_budget,
            );
        }

        let (dynamic_state_level, vertex_input_dynamic_state_enabled, emulate_bgr565) = {
            let values = common::settings::values();
            (
                *values.dyna_state.get_value(),
                *values.vertex_input_dynamic_state.get_value(),
                *values.emulate_bgr565.get_value(),
            )
        };
        match dynamic_state_level {
            common::settings::ExtendedDynamicState::Disabled => {
                supports_extended_dynamic_state = false;
                supports_extended_dynamic_state2 = false;
                supports_extended_dynamic_state2_extra = false;
                extended_dynamic_state_features.extended_dynamic_state = vk::FALSE;
                extended_dynamic_state2_features.extended_dynamic_state2 = vk::FALSE;
                extended_dynamic_state2_features.extended_dynamic_state2_logic_op = vk::FALSE;
                extended_dynamic_state2_features.extended_dynamic_state2_patch_control_points =
                    vk::FALSE;
                clear_feature_preserving_chain!(extended_dynamic_state3_features);
            }
            common::settings::ExtendedDynamicState::EDS1 => {
                supports_extended_dynamic_state2 = false;
                supports_extended_dynamic_state2_extra = false;
                extended_dynamic_state2_features.extended_dynamic_state2 = vk::FALSE;
                extended_dynamic_state2_features.extended_dynamic_state2_logic_op = vk::FALSE;
                extended_dynamic_state2_features.extended_dynamic_state2_patch_control_points =
                    vk::FALSE;
                clear_feature_preserving_chain!(extended_dynamic_state3_features);
            }
            common::settings::ExtendedDynamicState::EDS2 => {
                clear_feature_preserving_chain!(extended_dynamic_state3_features);
            }
            common::settings::ExtendedDynamicState::EDS3 => {}
        }

        // Port of the granular EDS3 capability policy in
        // `Device::RemoveUnsuitableExtensions`. Keep the extension enabled
        // when any supported EDS3 state is usable, and include the prerequisite
        // extensions/features used by upstream for line and conservative state.
        let mut dynamic_state3_blending = has_extended_dynamic_state3
            && extended_dynamic_state3_features.extended_dynamic_state3_color_blend_enable != 0
            && extended_dynamic_state3_features.extended_dynamic_state3_color_blend_equation != 0
            && extended_dynamic_state3_features.extended_dynamic_state3_color_write_mask != 0;
        let mut dynamic_state3_depth_clamp_enable = has_extended_dynamic_state3
            && extended_dynamic_state3_features.extended_dynamic_state3_depth_clamp_enable != 0;
        let mut dynamic_state3_logic_op_enable = has_extended_dynamic_state3
            && extended_dynamic_state3_features.extended_dynamic_state3_logic_op_enable != 0;
        let mut dynamic_state3_line_raster_mode = has_extended_dynamic_state3
            && extended_dynamic_state3_features.extended_dynamic_state3_line_rasterization_mode
                != 0
            && has_line_rasterization
            && line_rasterization_features.rectangular_lines != 0;
        let mut dynamic_state3_conservative_raster_mode = has_extended_dynamic_state3
            && extended_dynamic_state3_features
                .extended_dynamic_state3_conservative_rasterization_mode
                != 0
            && supported_extensions.contains("VK_EXT_conservative_rasterization");
        let mut dynamic_state3_line_stipple_enable = has_extended_dynamic_state3
            && extended_dynamic_state3_features.extended_dynamic_state3_line_stipple_enable != 0
            && has_line_rasterization
            && line_rasterization_features.stippled_rectangular_lines != 0;
        let mut dynamic_state3_alpha_to_coverage = has_extended_dynamic_state3
            && extended_dynamic_state3_features.extended_dynamic_state3_alpha_to_coverage_enable
                != 0;
        let mut dynamic_state3_alpha_to_one = has_extended_dynamic_state3
            && extended_dynamic_state3_features.extended_dynamic_state3_alpha_to_one_enable != 0
            && device_features.alpha_to_one != 0;
        let mut dynamic_state3_enables = dynamic_state3_depth_clamp_enable
            || dynamic_state3_logic_op_enable
            || dynamic_state3_line_raster_mode
            || dynamic_state3_conservative_raster_mode
            || dynamic_state3_line_stipple_enable
            || dynamic_state3_alpha_to_coverage
            || dynamic_state3_alpha_to_one;
        if !vertex_input_dynamic_state_enabled {
            supports_vertex_input_dynamic_state = false;
            vertex_input_dynamic_state_features.vertex_input_dynamic_state = vk::FALSE;
        }

        if !supports_extended_dynamic_state && supports_extended_dynamic_state2 {
            log::info!("Removing extendedDynamicState2 due to missing extendedDynamicState");
            supports_extended_dynamic_state2 = false;
            supports_extended_dynamic_state2_extra = false;
            extended_dynamic_state2_features.extended_dynamic_state2 = vk::FALSE;
            extended_dynamic_state2_features.extended_dynamic_state2_logic_op = vk::FALSE;
            extended_dynamic_state2_features.extended_dynamic_state2_patch_control_points =
                vk::FALSE;
        }
        if !supports_extended_dynamic_state2 && (dynamic_state3_blending || dynamic_state3_enables)
        {
            log::info!("Removing extendedDynamicState3 due to missing extendedDynamicState2");
            dynamic_state3_blending = false;
            dynamic_state3_enables = false;
            dynamic_state3_depth_clamp_enable = false;
            dynamic_state3_logic_op_enable = false;
            dynamic_state3_line_raster_mode = false;
            dynamic_state3_conservative_raster_mode = false;
            dynamic_state3_line_stipple_enable = false;
            dynamic_state3_alpha_to_coverage = false;
            dynamic_state3_alpha_to_one = false;
            clear_feature_preserving_chain!(extended_dynamic_state3_features);
        }
        let supports_depth_clip_control =
            has_depth_clip_control && depth_clip_control_features.depth_clip_control != 0;
        let supports_4444_formats = has_4444_formats && formats_4444_features.format_a4b4g4r4 != 0;
        let supports_vertex_attribute_divisor = has_vertex_attribute_divisor;
        let supports_provoking_vertex = has_provoking_vertex;
        let provoking_vertex_last_supported =
            supports_provoking_vertex && provoking_vertex_features.provoking_vertex_last != 0;
        let transform_feedback_preserves_provoking_vertex = supports_provoking_vertex
            && provoking_vertex_features.transform_feedback_preserves_provoking_vertex != 0;
        let supports_null_descriptor =
            configure_robustness2_features(&mut robustness2_features, has_robustness2);
        let supports_device_fault = has_device_fault && device_fault_features.device_fault != 0;
        let mut supports_shader_demote_to_helper_invocation = has_shader_demote_to_helper_invocation
            && shader_demote_features.shader_demote_to_helper_invocation != 0;
        if is_mvk && supports_shader_demote_to_helper_invocation {
            log::warn!(
                "MoltenVK advertises shader demote but fails to lower it below MSL 2.3; disabling"
            );
            supports_shader_demote_to_helper_invocation = false;
            shader_demote_features.shader_demote_to_helper_invocation = vk::FALSE;
        }
        log::info!(
            "Vulkan primitive topology restart: extension={} list={} patch={}",
            has_primitive_topology_list_restart,
            supports_primitive_topology_list_restart,
            supports_primitive_topology_patch_list_restart
        );
        if has_portability_subset {
            log::info!(
                "Vulkan portability subset features: triangle_fans={} image_view_format_reinterpretation={} image_view_format_swizzle={} separate_stencil_mask_ref={} vertex_attribute_access_beyond_stride={}",
                portability_subset_features.triangle_fans != 0,
                portability_subset_features.image_view_format_reinterpretation != 0,
                portability_subset_features.image_view_format_swizzle != 0,
                portability_subset_features.separate_stencil_mask_ref != 0,
                portability_subset_features.vertex_attribute_access_beyond_stride != 0,
            );
        }

        let mut loaded_extensions = initial_loaded_extensions(
            device_properties.api_version,
            &supported_extensions,
            supports_device_fault,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_EXT_custom_border_color",
            supports_custom_border_color,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_EXT_color_write_enable",
            supports_color_write_enable,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_EXT_depth_bias_control",
            supports_depth_bias_control,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_EXT_depth_clip_control",
            supports_depth_clip_control,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_EXT_descriptor_buffer",
            supports_descriptor_buffer,
        );
        if device_properties.api_version < vk::API_VERSION_1_2 {
            remove_extension_if_unsupported(
                &mut loaded_extensions,
                "VK_KHR_buffer_device_address",
                supports_buffer_device_address,
            );
        }
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_EXT_extended_dynamic_state",
            supports_extended_dynamic_state,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_EXT_extended_dynamic_state2",
            supports_extended_dynamic_state2,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_EXT_extended_dynamic_state3",
            dynamic_state3_blending || dynamic_state3_enables,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_EXT_provoking_vertex",
            supports_provoking_vertex,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_EXT_transform_feedback",
            supports_transform_feedback,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_EXT_vertex_input_dynamic_state",
            supports_vertex_input_dynamic_state,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_KHR_pipeline_executable_properties",
            supports_pipeline_executable_properties,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_KHR_workgroup_memory_explicit_layout",
            supports_workgroup_memory_explicit_layout,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_KHR_maintenance5",
            supports_maintenance5,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_EXT_shader_demote_to_helper_invocation",
            supports_shader_demote_to_helper_invocation,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_EXT_subgroup_size_control",
            supports_subgroup_size_control,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_EXT_sampler_filter_minmax",
            supports_sampler_filter_minmax,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_KHR_push_descriptor",
            supports_push_descriptor,
        );
        remove_extension_if_unsupported(
            &mut loaded_extensions,
            "VK_KHR_synchronization2",
            supports_synchronization2,
        );
        if !supports_custom_border_color {
            clear_feature_preserving_chain!(custom_border_color_features);
        }
        if !supports_color_write_enable {
            clear_feature_preserving_chain!(color_write_enable_features);
        }
        if !supports_depth_bias_control {
            clear_feature_preserving_chain!(depth_bias_control_features);
        }
        if !supports_depth_clip_control {
            clear_feature_preserving_chain!(depth_clip_control_features);
        }
        if !supports_descriptor_buffer {
            clear_feature_preserving_chain!(descriptor_buffer_features);
        }
        if !supports_buffer_device_address {
            if device_properties.api_version >= vk::API_VERSION_1_2 {
                vulkan12_features.buffer_device_address = vk::FALSE;
                vulkan12_features.buffer_device_address_capture_replay = vk::FALSE;
                vulkan12_features.buffer_device_address_multi_device = vk::FALSE;
            } else {
                clear_feature_preserving_chain!(buffer_device_address_features);
            }
        }
        if !supports_extended_dynamic_state {
            clear_feature_preserving_chain!(extended_dynamic_state_features);
        }
        if !supports_extended_dynamic_state2 {
            clear_feature_preserving_chain!(extended_dynamic_state2_features);
        }
        if !(dynamic_state3_blending || dynamic_state3_enables) {
            clear_feature_preserving_chain!(extended_dynamic_state3_features);
        }
        if !supports_provoking_vertex {
            clear_feature_preserving_chain!(provoking_vertex_features);
        }
        if !supports_transform_feedback {
            clear_feature_preserving_chain!(transform_feedback_features);
        }
        if !supports_vertex_input_dynamic_state {
            clear_feature_preserving_chain!(vertex_input_dynamic_state_features);
        }
        if !supports_pipeline_executable_properties {
            clear_feature_preserving_chain!(pipeline_executable_properties_features);
        }
        if !supports_workgroup_memory_explicit_layout {
            clear_feature_preserving_chain!(workgroup_memory_explicit_layout_features);
        }
        if !supports_subgroup_size_control {
            clear_feature_preserving_chain!(subgroup_size_control_features);
        }
        if !supports_synchronization2 {
            clear_feature_preserving_chain!(synchronization2_features);
        }
        if supports_device_fault {
            log::info!("Vulkan device-fault diagnostics enabled");
        }
        let _ = (&shader_float16_int8_features, &shader_demote_features);
        let format_properties = collect_format_properties(&instance, physical);
        let is_blit_depth24_stencil8_supported =
            test_depth_stencil_blits(&format_properties, vk::Format::D24_UNORM_S8_UINT);
        let is_blit_depth32_stencil8_supported =
            test_depth_stencil_blits(&format_properties, vk::Format::D32_SFLOAT_S8_UINT);
        let is_optimal_astc_supported = compute_is_optimal_astc_supported(
            &instance,
            physical,
            device_features.texture_compression_astc_ldr != 0,
        );
        let supports_d24_depth = format_properties
            .get(&vk::Format::D24_UNORM_S8_UINT)
            .is_some_and(|properties| {
                properties
                    .optimal_tiling_features
                    .contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT)
            });
        let is_warp_potentially_bigger = !supports_subgroup_size_control
            || subgroup_size_control_properties.max_subgroup_size > GUEST_WARP_SIZE;
        let supports_conditional_barriers = !(is_intel_anv || is_intel_windows);
        let driver_has_broken_compute =
            Self::check_broken_compute(driver_id, device_properties.driver_version);
        if driver_has_broken_compute {
            log::warn!("Intel proprietary drivers 0.405.0 until 0.405.286 have broken compute");
        }
        let has_broken_compute = driver_has_broken_compute
            && !*common::settings::values()
                .enable_compute_pipelines
                .get_value();
        let (has_renderdoc, has_nsight_graphics) = collect_tooling_info(
            entry,
            &instance,
            physical,
            loaded_extensions.contains("VK_EXT_tooling_info"),
        );
        let timeline_semaphore_supported =
            supports_timeline_semaphore && driver_id != vk::DriverId::MESA_TURNIP;
        let enabled_extensions: Vec<CString> = loaded_extensions
            .iter()
            .map(|name| CString::new(name.as_str()).unwrap())
            .collect();
        let enabled_extension_ptrs: Vec<*const std::os::raw::c_char> = enabled_extensions
            .iter()
            .map(|name| name.as_ptr())
            .collect();

        // Match upstream: reuse the exact feature chain returned by
        // vkGetPhysicalDeviceFeatures2. Rebuilding it loses core/promoted features and
        // copying an individual node also copies its linked pNext chain.
        let mut device_create_info = vk::DeviceCreateInfo::builder()
            .queue_create_infos(&queue_create_infos)
            .enabled_extension_names(&enabled_extension_ptrs)
            .build();
        device_create_info.p_next = (&features2 as *const vk::PhysicalDeviceFeatures2).cast();
        let enable_nsight_aftermath = *common::settings::values()
            .enable_nsight_aftermath
            .get_value()
            && loaded_extensions.contains("VK_NV_device_diagnostics_config");
        let mut diagnostics_nv = vk::DeviceDiagnosticsConfigCreateInfoNV::default();
        if enable_nsight_aftermath {
            diagnostics_nv.p_next = device_create_info.p_next;
            diagnostics_nv.flags = vk::DeviceDiagnosticsConfigFlagsNV::ENABLE_SHADER_DEBUG_INFO
                | vk::DeviceDiagnosticsConfigFlagsNV::ENABLE_RESOURCE_TRACKING
                | vk::DeviceDiagnosticsConfigFlagsNV::ENABLE_AUTOMATIC_CHECKPOINTS;
            device_create_info.p_next =
                (&diagnostics_nv as *const vk::DeviceDiagnosticsConfigCreateInfoNV).cast();
        }
        debug_assert!(pnext_chain_has_unique_structure_types(
            device_create_info.p_next
        ));

        let logical = LogicalDevice::create(&instance, physical, &device_create_info)?;
        let graphics_queue = logical.get_queue(graphics_family);
        let present_queue = logical.get_queue(present_family);
        let mut allocator_flags = vk_mem::AllocatorCreateFlags::EXTERNALLY_SYNCHRONIZED;
        if loaded_extensions.contains("VK_EXT_memory_budget") {
            allocator_flags |= vk_mem::AllocatorCreateFlags::EXT_MEMORY_BUDGET;
        }
        if supports_buffer_device_address {
            allocator_flags |= vk_mem::AllocatorCreateFlags::BUFFER_DEVICE_ADDRESS;
        }
        let allocator_info = vk_mem::AllocatorCreateInfo::new(&instance, &logical.device, physical)
            .flags(allocator_flags)
            .preferred_large_heap_block_size(if is_integrated {
                64 * 1024 * 1024
            } else {
                256 * 1024 * 1024
            })
            .vulkan_api_version(device_properties.api_version);
        let allocator = Arc::new(Mutex::new(
            vk_mem::Allocator::new(allocator_info).map_err(VulkanError::new)?,
        ));
        let descriptor_buffer = supports_descriptor_buffer
            .then(|| ash::extensions::ext::DescriptorBuffer::new(&instance, &logical.device));
        let synchronization2 = (supports_synchronization2
            && device_properties.api_version < vk::API_VERSION_1_3)
            .then(|| ash::extensions::khr::Synchronization2::new(&instance, &logical.device));

        let instance_version = unsafe {
            let props = instance.get_physical_device_properties(physical);
            props.api_version
        };

        let device_name =
            unsafe { CStr::from_ptr(device_properties.device_name.as_ptr()).to_string_lossy() };
        log::info!("Vulkan device: {}", device_name);
        log::info!(
            "Vulkan API version: {}.{}.{}",
            vk::api_version_major(device_properties.api_version),
            vk::api_version_minor(device_properties.api_version),
            vk::api_version_patch(device_properties.api_version),
        );

        let is_virtual = device_properties.device_type == vk::PhysicalDeviceType::VIRTUAL_GPU;
        let is_non_gpu = device_properties.device_type == vk::PhysicalDeviceType::CPU
            || device_properties.device_type == vk::PhysicalDeviceType::OTHER;
        let (device_access_memory, valid_heap_memory) = collect_physical_memory_info(
            &memory_properties,
            memory_budget_properties.as_ref(),
            is_integrated,
        );

        let device = Self {
            instance,
            physical,
            allocator,
            _dld: ash::Device::clone(&logical.device),
            logical,
            descriptor_buffer,
            synchronization2,
            graphics_queue,
            present_queue,
            instance_version,
            graphics_family,
            present_family,
            extensions: DeviceExtensions {
                bit16_storage: loaded_extensions.contains("VK_KHR_16bit_storage"),
                shader_atomic_int64: supports_shader_atomic_int64,
                shader_draw_parameters: loaded_extensions.contains("VK_KHR_shader_draw_parameters"),
                shader_float16_int8: loaded_extensions.contains("VK_KHR_shader_float16_int8"),
                uniform_buffer_standard_layout: loaded_extensions
                    .contains("VK_KHR_uniform_buffer_standard_layout"),
                variable_pointer: loaded_extensions.contains("VK_KHR_variable_pointers"),
                descriptor_indexing: loaded_extensions.contains("VK_EXT_descriptor_indexing"),
                host_query_reset: loaded_extensions.contains("VK_EXT_host_query_reset"),
                bit8_storage: loaded_extensions.contains("VK_KHR_8bit_storage"),
                timeline_semaphore: loaded_extensions.contains("VK_KHR_timeline_semaphore"),
                buffer_device_address: supports_buffer_device_address,
                subgroup_size_control: supports_subgroup_size_control,
                synchronization2: supports_synchronization2,
                custom_border_color: supports_custom_border_color,
                color_write_enable: supports_color_write_enable,
                depth_bias_control: supports_depth_bias_control,
                primitive_topology_list_restart: has_primitive_topology_list_restart,
                extended_dynamic_state: supports_extended_dynamic_state,
                extended_dynamic_state2: supports_extended_dynamic_state2,
                extended_dynamic_state2_extra: supports_extended_dynamic_state2_extra,
                extended_dynamic_state3: dynamic_state3_blending || dynamic_state3_enables,
                format_a4b4g4r4: has_4444_formats,
                line_rasterization: loaded_extensions.contains("VK_EXT_line_rasterization"),
                transform_feedback: supports_transform_feedback,
                vertex_input_dynamic_state: supports_vertex_input_dynamic_state,
                depth_clip_control: supports_depth_clip_control,
                descriptor_buffer: supports_descriptor_buffer,
                index_type_uint8: has_index_type_uint8,
                vertex_attribute_divisor: supports_vertex_attribute_divisor,
                provoking_vertex: supports_provoking_vertex,
                robustness2: has_robustness2,
                robustness_2: loaded_extensions.contains("VK_EXT_robustness2"),
                pipeline_executable_properties: supports_pipeline_executable_properties,
                workgroup_memory_explicit_layout: supports_workgroup_memory_explicit_layout,
                maintenance5: supports_maintenance5,
                device_fault: supports_device_fault,
                shader_demote_to_helper_invocation: supports_shader_demote_to_helper_invocation,
                draw_indirect_count: has_draw_indirect_count,
                sampler_filter_minmax: supports_sampler_filter_minmax,
                shader_float_controls: has_shader_float_controls,
                conditional_rendering: loaded_extensions.contains("VK_EXT_conditional_rendering"),
                conservative_rasterization: loaded_extensions
                    .contains("VK_EXT_conservative_rasterization"),
                depth_range_unrestricted: loaded_extensions
                    .contains("VK_EXT_depth_range_unrestricted"),
                memory_budget: loaded_extensions.contains("VK_EXT_memory_budget"),
                shader_stencil_export: loaded_extensions.contains("VK_EXT_shader_stencil_export"),
                shader_viewport_index_layer: loaded_extensions
                    .contains("VK_EXT_shader_viewport_index_layer"),
                tooling_info: loaded_extensions.contains("VK_EXT_tooling_info"),
                driver_properties: loaded_extensions.contains("VK_KHR_driver_properties"),
                push_descriptor: supports_push_descriptor,
                sampler_mirror_clamp_to_edge: loaded_extensions
                    .contains("VK_KHR_sampler_mirror_clamp_to_edge"),
                spirv_1_4: loaded_extensions.contains("VK_KHR_spirv_1_4"),
                swapchain: loaded_extensions.contains("VK_KHR_swapchain"),
                swapchain_mutable_format: loaded_extensions
                    .contains("VK_KHR_swapchain_mutable_format"),
                image_format_list: loaded_extensions.contains("VK_KHR_image_format_list"),
                device_diagnostics_config: loaded_extensions
                    .contains("VK_NV_device_diagnostics_config"),
                geometry_shader_passthrough: loaded_extensions
                    .contains("VK_NV_geometry_shader_passthrough"),
                viewport_array2: loaded_extensions.contains("VK_NV_viewport_array2"),
                viewport_swizzle: loaded_extensions.contains("VK_NV_viewport_swizzle"),
                filter_cubic: loaded_extensions.contains("VK_EXT_filter_cubic"),
                filter_cubic_img: loaded_extensions.contains("VK_IMG_filter_cubic"),
                filter_cubic_weights: loaded_extensions.contains("VK_QCOM_filter_cubic_weights"),
                ..DeviceExtensions::default()
            },
            device_properties,
            driver_properties,
            subgroup_properties,
            float_controls_properties,
            push_descriptor_properties,
            subgroup_size_control_properties,
            transform_feedback_properties,
            descriptor_buffer_properties,
            transform_feedback_geometry_streams_supported: transform_feedback_features
                .geometry_streams
                != 0,
            device_features,
            shader_float16_supported: supports_shader_float16,
            timeline_semaphore_supported,
            host_query_reset_supported: host_query_reset_features.host_query_reset != 0,
            shader_int8_supported: supports_shader_int8,
            uniform_and_storage_buffer_8bit_supported: storage_8bit_features
                .uniform_and_storage_buffer8_bit_access
                != 0,
            storage_buffer_8bit_supported: storage_8bit_features.storage_buffer8_bit_access != 0,
            uniform_and_storage_buffer_16bit_supported: storage_16bit_features
                .uniform_and_storage_buffer16_bit_access
                != 0,
            storage_buffer_16bit_supported: storage_16bit_features.storage_buffer16_bit_access != 0,
            workgroup_memory_explicit_layout_8bit_access_supported:
                workgroup_memory_explicit_layout_features
                    .workgroup_memory_explicit_layout8_bit_access
                    != 0,
            workgroup_memory_explicit_layout_16bit_access_supported:
                workgroup_memory_explicit_layout_features
                    .workgroup_memory_explicit_layout16_bit_access
                    != 0,
            primitive_topology_list_restart_supported: supports_primitive_topology_list_restart,
            primitive_topology_patch_list_restart_supported:
                supports_primitive_topology_patch_list_restart,
            format_a4b4g4r4_supported: supports_4444_formats,
            null_descriptor_supported: supports_null_descriptor,
            shader_output_layer_supported: vulkan12_features.shader_output_layer != 0,
            exact_depth_bias_control_supported,
            shader_shared_int64_atomics_supported,
            maintenance5_early_fragment_multisample_coverage_after_sample_counting:
                maintenance5_properties.early_fragment_multisample_coverage_after_sample_counting
                    != 0,
            maintenance5_early_fragment_sample_mask_test_before_sample_counting:
                maintenance5_properties.early_fragment_sample_mask_test_before_sample_counting != 0,
            maintenance5_depth_stencil_swizzle_one_supported: maintenance5_properties
                .depth_stencil_swizzle_one_support
                != 0,
            maintenance5_polygon_mode_point_size_supported: maintenance5_properties
                .polygon_mode_point_size
                != 0,
            descriptor_binding_partially_bound_supported,
            sampled_image_array_non_uniform_indexing_supported,
            storage_image_array_non_uniform_indexing_supported,
            uniform_texel_buffer_array_non_uniform_indexing_supported,
            storage_texel_buffer_array_non_uniform_indexing_supported,
            is_optimal_astc_supported,
            is_blit_depth24_stencil8_supported,
            is_blit_depth32_stencil8_supported,
            is_warp_potentially_bigger,
            is_integrated,
            is_virtual,
            is_non_gpu,
            has_geometry_shader: device_features.geometry_shader != 0,
            has_tessellation_shader: device_features.tessellation_shader != 0,
            has_broken_compute,
            has_broken_cube_compatibility,
            has_broken_parallel_compiling,
            has_renderdoc,
            has_nsight_graphics,
            supports_d24_depth,
            cant_blit_msaa,
            must_emulate_scaled_formats,
            must_emulate_bgr565: emulate_bgr565,
            dynamic_state3_blending,
            dynamic_state3_enables,
            dynamic_state3_depth_clamp_enable,
            dynamic_state3_logic_op_enable,
            dynamic_state3_line_raster_mode,
            dynamic_state3_conservative_raster_mode,
            dynamic_state3_line_stipple_enable,
            dynamic_state3_alpha_to_coverage,
            dynamic_state3_alpha_to_one,
            provoking_vertex_last_supported,
            transform_feedback_preserves_provoking_vertex,
            smooth_lines_supported: line_rasterization_features.smooth_lines != 0,
            stippled_rectangular_lines_supported: line_rasterization_features
                .stippled_rectangular_lines
                != 0,
            supports_conditional_barriers,
            device_access_memory,
            sets_per_pool: if is_amd_driver { 96 } else { 64 },
            sampler_heap_budget,
            nvidia_arch,
            supported_extensions,
            loaded_extensions,
            valid_heap_memory,
            format_properties,
            nsight_aftermath_tracker: enable_nsight_aftermath.then(NsightAftermathTracker::new),
        };
        device.initialize_gpu_logging();
        Ok(device)
    }

    /// Initialize Eden's optional GPU logging after the logical device and VMA
    /// allocator are ready.
    pub fn initialize_gpu_logging(&self) {
        let settings = common::settings::values();
        let log_level = LogLevel::from(*settings.gpu_log_level.get_value());
        if log_level == LogLevel::Off {
            return;
        }

        let driver_id = self.get_driver_id();
        let detected_driver = if driver_id == vk::DriverId::MESA_TURNIP {
            DriverType::Turnip
        } else if driver_id == vk::DriverId::QUALCOMM_PROPRIETARY {
            DriverType::Qualcomm
        } else {
            DriverType::Unknown
        };

        let logger = get_instance();
        logger.initialize(log_level, detected_driver);
        logger.enable_vulkan_call_tracking(*settings.gpu_log_vulkan_calls.get_value());
        logger.enable_memory_tracking(*settings.gpu_log_memory_tracking.get_value());
        logger.enable_driver_debug_info(*settings.gpu_log_driver_debug.get_value());
        logger.set_ring_buffer_size(*settings.gpu_log_ring_buffer_size.get_value() as usize);

        if !*settings.gpu_log_driver_debug.get_value() {
            return;
        }

        let device_name = unsafe {
            CStr::from_ptr(self.device_properties.device_name.as_ptr()).to_string_lossy()
        };
        let driver_name = unsafe {
            CStr::from_ptr(self.driver_properties.driver_name.as_ptr()).to_string_lossy()
        };
        let driver_info = unsafe {
            CStr::from_ptr(self.driver_properties.driver_info.as_ptr()).to_string_lossy()
        };
        let driver_version = self.device_properties.driver_version;
        let api_version = self.device_properties.api_version;
        let mut details = format!(
            "Device: {device_name}\n\
             Driver Name: {driver_name}\n\
             Driver Info: {driver_info}\n\
             Driver Version: {}.{}.{}\n\
             Vulkan API Version: {}.{}.{}\n\
             Driver ID: {}\n\
             Vendor ID: {:#04x}\n\
             Device ID: {:#04x}\n\
             \n=== Loaded Vulkan Extensions ===\n",
            vk::api_version_major(driver_version),
            vk::api_version_minor(driver_version),
            vk::api_version_patch(driver_version),
            vk::api_version_major(api_version),
            vk::api_version_minor(api_version),
            vk::api_version_patch(api_version),
            driver_id.as_raw(),
            self.device_properties.vendor_id,
            self.device_properties.device_id,
        );

        let (qcom_extensions, other_extensions): (Vec<_>, Vec<_>) = self
            .loaded_extensions
            .iter()
            .partition(|extension| extension.contains("QCOM") || extension.contains("qcom"));
        if !qcom_extensions.is_empty() {
            details.push_str("\nQualcomm Proprietary Extensions:\n");
            for extension in qcom_extensions {
                details.push_str(&format!("  - {extension}\n"));
            }
        }
        if !other_extensions.is_empty() {
            details.push_str("\nStandard Extensions:\n");
            for extension in other_extensions {
                details.push_str(&format!("  - {extension}\n"));
            }
        }
        details.push_str(&format!(
            "\nTotal Extensions Loaded: {}\n",
            self.loaded_extensions.len()
        ));
        logger.log_driver_debug_info(&details);
    }

    /// Shut down Eden's process-wide GPU logger before the allocator and
    /// logical device are destroyed.
    pub fn shutdown_gpu_logging(&self) {
        let logger = get_instance();
        if logger.is_initialized() {
            logger.shutdown();
        }
    }

    // -----------------------------------------------------------------------
    // Accessors — port of `Device` public accessors from vulkan_device.h
    // -----------------------------------------------------------------------

    /// Returns a format supported by the device for the passed requirements.
    ///
    /// Port of `Device::GetSupportedFormat`.
    pub fn get_supported_format(
        &self,
        wanted_format: vk::Format,
        wanted_usage: vk::FormatFeatureFlags,
        format_type: FormatType,
    ) -> vk::Format {
        if self.is_format_supported(wanted_format, wanted_usage, format_type) {
            return wanted_format;
        }
        let Some(alternatives) = alternatives::get_format_alternatives(wanted_format) else {
            log::error!(
                "Format={:?} with usage={:?} and type={:?} has no defined alternatives and host hardware does not support it",
                wanted_format,
                wanted_usage,
                format_type
            );
            return wanted_format;
        };
        for &alternative in alternatives {
            if self.is_format_supported(alternative, wanted_usage, format_type) {
                log::debug!(
                    "Emulating format={:?} with alternative format={:?} with usage={:?} and type={:?}",
                    wanted_format,
                    alternative,
                    wanted_usage,
                    format_type
                );
                return alternative;
            }
        }
        log::error!(
            "Format={:?} with usage={:?} and type={:?} is not supported by the host hardware and doesn't support any alternative",
            wanted_format,
            wanted_usage,
            format_type
        );
        wanted_format
    }

    /// Returns true if a format is supported.
    ///
    /// Port of `Device::IsFormatSupported`.
    pub fn is_format_supported(
        &self,
        wanted_format: vk::Format,
        wanted_usage: vk::FormatFeatureFlags,
        format_type: FormatType,
    ) -> bool {
        let Some(props) = self.format_properties.get(&wanted_format) else {
            log::error!("Unimplemented format query {:?}", wanted_format);
            return true;
        };
        let supported = match format_type {
            FormatType::Linear => props.linear_tiling_features,
            FormatType::Optimal => props.optimal_tiling_features,
            FormatType::Buffer => props.buffer_features,
        };
        (supported & wanted_usage) == wanted_usage
    }

    /// Gets the format properties for a format.
    ///
    /// Port-facing accessor for upstream `vk::PhysicalDevice::GetFormatProperties`.
    pub fn format_properties(&self, format: vk::Format) -> vk::FormatProperties {
        self.get_format_properties(format)
    }

    /// Gets the format properties for a format, caching results.
    fn get_format_properties(&self, format: vk::Format) -> vk::FormatProperties {
        if let Some(&cached) = self.format_properties.get(&format) {
            return cached;
        }
        unsafe {
            self.instance
                .get_physical_device_format_properties(self.physical, format)
        }
    }

    /// Reports a device loss.
    ///
    /// Port of `Device::ReportLoss`.
    pub fn report_loss(&self) {
        report_device_loss();
    }

    /// Reports a shader to Nsight Aftermath.
    ///
    /// Port of `Device::SaveShader`.
    pub fn save_shader(&self, spirv: &[u32]) {
        if let Some(tracker) = &self.nsight_aftermath_tracker {
            tracker.save_shader(spirv);
        }
    }

    /// Names a shader module for an attached Vulkan debugging tool.
    ///
    /// This is the Rust counterpart of upstream's
    /// `vk::ShaderModule::SetObjectNameEXT` used by `vk_pipeline_cache.cpp`.
    pub fn set_shader_module_name(&self, module: vk::ShaderModule, name: &str) {
        if !self.has_debugging_tool_attached() {
            return;
        }
        let Ok(name) = CString::new(name) else {
            log::warn!("Refusing Vulkan shader-module name containing NUL");
            return;
        };
        let functions = vk::ExtDebugUtilsFn::load(|function_name| unsafe {
            self.instance
                .get_device_proc_addr(self.logical.device.handle(), function_name.as_ptr())
                .map_or(std::ptr::null(), |function| {
                    function as *const std::ffi::c_void
                })
        });
        let name_info = vk::DebugUtilsObjectNameInfoEXT::builder()
            .object_type(vk::ObjectType::SHADER_MODULE)
            .object_handle(module.as_raw())
            .object_name(&name);
        let result = unsafe {
            (functions.set_debug_utils_object_name_ext)(self.logical.device.handle(), &*name_info)
        };
        if result != vk::Result::SUCCESS {
            log::warn!("Failed to name Vulkan shader module: {result:?}");
        }
    }

    /// Rust counterpart of `vk::Buffer::SetObjectNameEXT` used by the
    /// descriptor-buffer ring.
    pub fn set_buffer_name(&self, buffer: vk::Buffer, name: &str) {
        if !self.has_debugging_tool_attached() {
            return;
        }
        let Ok(name) = CString::new(name) else {
            log::warn!("Refusing Vulkan buffer name containing NUL");
            return;
        };
        let functions = vk::ExtDebugUtilsFn::load(|function_name| unsafe {
            self.instance
                .get_device_proc_addr(self.logical.device.handle(), function_name.as_ptr())
                .map_or(std::ptr::null(), |function| {
                    function as *const std::ffi::c_void
                })
        });
        let name_info = vk::DebugUtilsObjectNameInfoEXT::builder()
            .object_type(vk::ObjectType::BUFFER)
            .object_handle(buffer.as_raw())
            .object_name(&name);
        let result = unsafe {
            (functions.set_debug_utils_object_name_ext)(self.logical.device.handle(), &*name_info)
        };
        if result != vk::Result::SUCCESS {
            log::warn!("Failed to name Vulkan buffer: {result:?}");
        }
    }

    /// Returns the name of the VkDriverId reported from Vulkan.
    ///
    /// Port of `Device::GetDriverName`.
    pub fn get_driver_name(&self) -> String {
        if let Some(name) = driver_name_from_id(self.driver_properties.driver_id) {
            return name.to_string();
        }
        self.get_vendor_name()
    }

    /// Returns the vendor name reported from Vulkan.
    ///
    /// Port of `Device::GetVendorName`.
    pub fn get_vendor_name(&self) -> String {
        let name = unsafe { CStr::from_ptr(self.driver_properties.driver_name.as_ptr()) };
        name.to_string_lossy().into_owned()
    }

    /// Returns the logical ash device.
    pub fn get_logical(&self) -> &ash::Device {
        &self.logical.device
    }

    /// Returns the VMA allocator.
    pub fn get_allocator(&self) -> &VmaAllocator {
        &self.allocator
    }

    /// Ash needs the instance dispatch table to construct extension loaders.
    /// Upstream's Vulkan wrapper carries that dispatch through `vk::Device`.
    pub(crate) fn get_instance(&self) -> &ash::Instance {
        &self.instance
    }

    /// Returns the device dispatch loader.
    pub fn get_dispatch_loader(&self) -> &ash::Device {
        &self._dld
    }

    /// Returns whether the opt-in `VK_EXT_device_fault` diagnostic is enabled.
    pub fn is_device_fault_supported(&self) -> bool {
        self.extensions.device_fault
    }

    /// Returns the physical device.
    pub fn get_physical(&self) -> vk::PhysicalDevice {
        self.physical
    }

    /// Returns the main graphics queue.
    pub fn get_graphics_queue(&self) -> vk::Queue {
        self.graphics_queue
    }

    /// Returns the main present queue.
    pub fn get_present_queue(&self) -> vk::Queue {
        self.present_queue
    }

    /// Returns the main graphics queue family index.
    pub fn get_graphics_family(&self) -> u32 {
        self.graphics_family
    }

    /// Returns the main present queue family index.
    pub fn get_present_family(&self) -> u32 {
        self.present_family
    }

    /// Returns the current Vulkan API version.
    pub fn api_version(&self) -> u32 {
        self.device_properties.api_version
    }

    /// Returns the current driver version.
    pub fn get_driver_version(&self) -> u32 {
        self.device_properties.driver_version
    }

    /// Returns the device name.
    pub fn get_model_name(&self) -> String {
        let name = unsafe { CStr::from_ptr(self.device_properties.device_name.as_ptr()) };
        name.to_string_lossy().into_owned()
    }

    /// Returns the driver ID.
    pub fn get_driver_id(&self) -> vk::DriverId {
        self.driver_properties.driver_id
    }

    /// Returns true for tile-based deferred renderers.
    ///
    /// Port of `Device::IsTiler`.
    pub fn is_tiler(&self) -> bool {
        is_tiler_driver(self.get_driver_id())
    }

    /// Returns whether multiple typed views may alias one descriptor binding.
    /// Matches upstream `Device::IsDescriptorAliasingSupported`.
    pub fn is_descriptor_aliasing_supported(&self) -> bool {
        self.driver_properties.driver_id != vk::DriverId::QUALCOMM_PROPRIETARY
    }

    /// Returns true if clocks should be boosted.
    ///
    /// Port of `Device::ShouldBoostClocks`.
    pub fn should_boost_clocks(&self) -> bool {
        let validated_driver = matches!(
            self.driver_properties.driver_id,
            vk::DriverId::AMD_PROPRIETARY
                | vk::DriverId::AMD_OPEN_SOURCE
                | vk::DriverId::MESA_RADV
                | vk::DriverId::NVIDIA_PROPRIETARY
                | vk::DriverId::INTEL_PROPRIETARY_WINDOWS
                | vk::DriverId::INTEL_OPEN_SOURCE_MESA
                | vk::DriverId::QUALCOMM_PROPRIETARY
                | vk::DriverId::MESA_TURNIP
                | vk::DriverId::SAMSUNG_PROPRIETARY
        );
        let is_steam_deck = self.device_properties.vendor_id == 0x1002
            && matches!(self.device_properties.device_id, 0x163f | 0x1435);
        validated_driver && !is_steam_deck && !self.has_debugging_tool_attached()
    }

    /// Returns uniform buffer alignment requirement.
    pub fn get_uniform_buffer_alignment(&self) -> vk::DeviceSize {
        self.device_properties
            .limits
            .min_uniform_buffer_offset_alignment
    }

    /// Returns storage buffer alignment requirement.
    pub fn get_storage_buffer_alignment(&self) -> vk::DeviceSize {
        self.device_properties
            .limits
            .min_storage_buffer_offset_alignment
    }

    /// Returns the maximum range for storage buffers.
    pub fn get_max_storage_buffer_range(&self) -> vk::DeviceSize {
        self.device_properties.limits.max_storage_buffer_range as u64
    }

    /// Returns the maximum size for push constants.
    pub fn get_max_push_constants_size(&self) -> vk::DeviceSize {
        self.device_properties.limits.max_push_constants_size as u64
    }

    /// Returns the maximum size for shared memory.
    pub fn get_max_compute_shared_memory_size(&self) -> u32 {
        self.device_properties.limits.max_compute_shared_memory_size
    }

    /// Port of upstream `Device::GetMaxComputeWorkGroupCount`.
    pub fn get_max_compute_work_group_count(&self) -> [u32; 3] {
        self.device_properties.limits.max_compute_work_group_count
    }

    pub fn get_max_per_stage_descriptor_sampled_images(&self) -> u32 {
        self.device_properties
            .limits
            .max_per_stage_descriptor_sampled_images
    }

    pub fn get_max_per_stage_resources(&self) -> u32 {
        self.device_properties.limits.max_per_stage_resources
    }

    pub fn get_max_descriptor_set_samplers(&self) -> u32 {
        self.device_properties.limits.max_descriptor_set_samplers
    }

    pub fn get_max_descriptor_set_uniform_buffers(&self) -> u32 {
        self.device_properties
            .limits
            .max_descriptor_set_uniform_buffers
    }

    pub fn get_max_descriptor_set_uniform_buffers_dynamic(&self) -> u32 {
        self.device_properties
            .limits
            .max_descriptor_set_uniform_buffers_dynamic
    }

    pub fn get_max_descriptor_set_storage_buffers(&self) -> u32 {
        self.device_properties
            .limits
            .max_descriptor_set_storage_buffers
    }

    pub fn get_max_descriptor_set_storage_buffers_dynamic(&self) -> u32 {
        self.device_properties
            .limits
            .max_descriptor_set_storage_buffers_dynamic
    }

    pub fn get_max_descriptor_set_sampled_images(&self) -> u32 {
        self.device_properties
            .limits
            .max_descriptor_set_sampled_images
    }

    pub fn get_max_descriptor_set_storage_images(&self) -> u32 {
        self.device_properties
            .limits
            .max_descriptor_set_storage_images
    }

    pub fn get_max_descriptor_set_input_attachments(&self) -> u32 {
        self.device_properties
            .limits
            .max_descriptor_set_input_attachments
    }

    /// Returns the device's floating-point control properties.
    pub fn float_control_properties(&self) -> &vk::PhysicalDeviceFloatControlsProperties {
        &self.float_controls_properties
    }

    /// Returns true if ASTC is natively supported.
    pub fn is_optimal_astc_supported(&self) -> bool {
        self.device_features.texture_compression_astc_ldr != 0
    }

    /// Returns true if BCn is natively supported.
    pub fn is_optimal_bcn_supported(&self) -> bool {
        self.device_features.texture_compression_bc != 0
    }

    /// Returns true if ETC2/EAC is natively supported.
    pub fn is_optimal_etc2_supported(&self) -> bool {
        self.device_features.texture_compression_etc2 != 0
    }

    /// Returns true if the device supports float64 natively.
    pub fn is_float64_supported(&self) -> bool {
        self.device_features.shader_float64 != 0
    }

    /// Returns true if the device supports float16 natively.
    ///
    /// Port of `Device::IsFloat16Supported`.
    pub fn is_float16_supported(&self) -> bool {
        self.shader_float16_supported
    }

    pub fn is_int8_supported(&self) -> bool {
        self.shader_int8_supported
    }

    /// Port of upstream `Device::IsUniformAndStorageBuffer8BitAccessSupported`.
    pub fn is_uniform_and_storage_buffer_8bit_access_supported(&self) -> bool {
        self.uniform_and_storage_buffer_8bit_supported
    }

    /// Port of upstream `Device::IsStorageBuffer8BitAccessSupported`.
    pub fn is_storage_buffer_8bit_access_supported(&self) -> bool {
        self.storage_buffer_8bit_supported
    }

    /// Port of upstream `Device::IsUniformAndStorageBuffer16BitAccessSupported`.
    pub fn is_uniform_and_storage_buffer_16bit_access_supported(&self) -> bool {
        self.uniform_and_storage_buffer_16bit_supported
    }

    /// Port of upstream `Device::IsStorageBuffer16BitAccessSupported`.
    pub fn is_storage_buffer_16bit_access_supported(&self) -> bool {
        self.storage_buffer_16bit_supported
    }

    /// Returns true if UINT8 index buffers can be converted through the
    /// compute pass. Port of upstream `Device::SupportsUint8Indices`.
    pub fn supports_uint8_indices(&self) -> bool {
        self.storage_buffer_8bit_supported && self.storage_buffer_16bit_supported
    }

    /// Port of upstream `Device::IsWorkgroupMemoryExplicitLayout8BitAccessSupported`.
    pub fn is_workgroup_memory_explicit_layout_8bit_access_supported(&self) -> bool {
        self.workgroup_memory_explicit_layout_8bit_access_supported
    }

    /// Port of upstream `Device::IsWorkgroupMemoryExplicitLayout16BitAccessSupported`.
    pub fn is_workgroup_memory_explicit_layout_16bit_access_supported(&self) -> bool {
        self.workgroup_memory_explicit_layout_16bit_access_supported
    }

    pub fn is_guest_warp_size_supported(&self, stage: vk::ShaderStageFlags) -> bool {
        self.subgroup_size_control_properties
            .required_subgroup_size_stages
            .intersects(stage)
    }

    pub fn is_subgroup_feature_supported(&self, feature: vk::SubgroupFeatureFlags) -> bool {
        self.subgroup_properties
            .supported_operations
            .intersects(feature)
    }

    /// Port of upstream `Device::GetSubgroupSupportedStages`.
    pub fn get_subgroup_supported_stages(&self) -> vk::ShaderStageFlags {
        self.subgroup_properties.supported_stages
    }

    /// Port of upstream `Device::IsSampledImageArrayNonUniformIndexingSupported`.
    pub fn is_sampled_image_array_non_uniform_indexing_supported(&self) -> bool {
        self.sampled_image_array_non_uniform_indexing_supported
    }

    /// Port of upstream `Device::IsStorageImageArrayNonUniformIndexingSupported`.
    pub fn is_storage_image_array_non_uniform_indexing_supported(&self) -> bool {
        self.storage_image_array_non_uniform_indexing_supported
    }

    /// Port of upstream `Device::IsUniformTexelBufferArrayNonUniformIndexingSupported`.
    pub fn is_uniform_texel_buffer_array_non_uniform_indexing_supported(&self) -> bool {
        self.uniform_texel_buffer_array_non_uniform_indexing_supported
    }

    /// Port of upstream `Device::IsStorageTexelBufferArrayNonUniformIndexingSupported`.
    pub fn is_storage_texel_buffer_array_non_uniform_indexing_supported(&self) -> bool {
        self.storage_texel_buffer_array_non_uniform_indexing_supported
    }

    /// Port of upstream `Device::IsDescriptorBindingPartiallyBoundSupported`.
    pub fn is_descriptor_binding_partially_bound_supported(&self) -> bool {
        self.descriptor_binding_partially_bound_supported
    }

    /// Port of upstream `Device::IsExtDescriptorBufferSupported`.
    pub fn is_ext_descriptor_buffer_supported(&self) -> bool {
        self.extensions.descriptor_buffer
    }

    /// Port of upstream `Device::DescriptorBufferProperties`.
    pub fn descriptor_buffer_properties(&self) -> &vk::PhysicalDeviceDescriptorBufferPropertiesEXT {
        &self.descriptor_buffer_properties
    }

    /// Port of upstream `Device::IsBufferDeviceAddressSupported`.
    pub fn is_buffer_device_address_supported(&self) -> bool {
        self.extensions.buffer_device_address
    }

    pub fn descriptor_buffer_extension(&self) -> Option<&ash::extensions::ext::DescriptorBuffer> {
        self.descriptor_buffer.as_ref()
    }

    /// Returns true if timeline semaphores are supported and enabled.
    pub fn is_timeline_semaphore_supported(&self) -> bool {
        self.timeline_semaphore_supported
    }

    /// Port of upstream `Device::HasSynchronization2`.
    pub fn has_synchronization2(&self) -> bool {
        self.extensions.synchronization2
    }

    pub fn synchronization2_extension(&self) -> Option<&ash::extensions::khr::Synchronization2> {
        self.synchronization2.as_ref()
    }

    /// Port of upstream `Device::GetSamplerHeapBudget`.
    pub fn get_sampler_heap_budget(&self) -> Option<usize> {
        (self.sampler_heap_budget != 0).then_some(self.sampler_heap_budget)
    }

    /// Port of upstream `Device::IsTransformFeedbackDrawSupported`.
    pub fn is_transform_feedback_draw_supported(&self) -> bool {
        self.extensions.transform_feedback
            && self.transform_feedback_properties.transform_feedback_draw != 0
    }

    /// Returns true if host-side `vkResetQueryPool` is supported.
    ///
    /// Port of upstream `Device::IsHostQueryResetSupported`.
    pub fn is_host_query_reset_supported(&self) -> bool {
        self.host_query_reset_supported
    }

    /// Returns true if the device supports VK_EXT_primitive_topology_list_restart.
    ///
    /// Port of `Device::IsTopologyListPrimitiveRestartSupported`.
    pub fn is_topology_list_primitive_restart_supported(&self) -> bool {
        self.primitive_topology_list_restart_supported
    }

    /// Returns true if the device supports patch-list primitive restart.
    ///
    /// Port of `Device::IsPatchListPrimitiveRestartSupported`.
    pub fn is_patch_list_primitive_restart_supported(&self) -> bool {
        self.primitive_topology_patch_list_restart_supported
    }

    /// Port of upstream `Device::HasNullDescriptor`.
    pub fn has_null_descriptor(&self) -> bool {
        self.null_descriptor_supported
    }

    /// Returns true if the device supports int64 natively.
    pub fn is_shader_int64_supported(&self) -> bool {
        self.device_features.shader_int64 != 0
    }

    /// Returns true if the device supports int16 natively.
    pub fn is_shader_int16_supported(&self) -> bool {
        self.device_features.shader_int16 != 0
    }

    /// Returns true if depth bounds is supported.
    pub fn is_depth_bounds_supported(&self) -> bool {
        self.device_features.depth_bounds != 0
    }

    /// Returns true if the device supports binding multisample images as storage images.
    pub fn is_storage_image_multisample_supported(&self) -> bool {
        self.device_features.shader_storage_image_multisample != 0
    }

    /// Returns true if formatless image load is supported.
    pub fn is_formatless_image_load_supported(&self) -> bool {
        self.device_features
            .shader_storage_image_read_without_format
            != 0
    }

    /// Returns the maximum number of push descriptors.
    pub fn max_push_descriptors(&self) -> u32 {
        self.push_descriptor_properties.max_push_descriptors
    }

    /// Returns true if robust buffer access is enabled on the device.
    pub fn is_robust_buffer_access_enabled(&self) -> bool {
        self.device_features.robust_buffer_access != 0
    }

    /// Returns true when a known debugging tool is attached.
    pub fn has_debugging_tool_attached(&self) -> bool {
        self.has_renderdoc || self.has_nsight_graphics
    }

    /// Returns true if compute pipelines can cause crashing.
    pub fn has_broken_compute(&self) -> bool {
        self.has_broken_compute
    }

    pub fn has_broken_cube_image_compatibility(&self) -> bool {
        self.has_broken_cube_compatibility
    }

    pub fn has_broken_parallel_shader_compiling(&self) -> bool {
        self.has_broken_parallel_compiling
    }

    pub fn has_geometry_shader(&self) -> bool {
        self.has_geometry_shader
    }

    pub fn has_tessellation_shader(&self) -> bool {
        self.has_tessellation_shader
    }

    /// Returns true if the device is an NVIDIA GPU.
    pub fn is_nvidia(&self) -> bool {
        self.driver_properties.driver_id == vk::DriverId::NVIDIA_PROPRIETARY
    }

    /// Returns the NVIDIA architecture classification.
    pub fn get_nvidia_arch(&self) -> NvidiaArchitecture {
        self.nvidia_arch
    }

    /// Returns the minimum supported SPIR-V version.
    ///
    /// Port of `Device::SupportedSpirvVersion`.
    pub fn supported_spirv_version(&self) -> u32 {
        if self.instance_version >= vk::API_VERSION_1_3 {
            0x0001_0600
        } else if self.extensions.spirv_1_4 {
            0x0001_0400
        } else {
            0x0001_0300
        }
    }

    /// Checks if a driver version has broken compute.
    ///
    /// Port of `Device::CheckBrokenCompute`.
    pub const fn check_broken_compute(driver_id: vk::DriverId, driver_version: u32) -> bool {
        if driver_id.as_raw() == vk::DriverId::INTEL_PROPRIETARY_WINDOWS.as_raw() {
            let major = vk::api_version_major(driver_version);
            let minor = vk::api_version_minor(driver_version);
            let patch = vk::api_version_patch(driver_version);
            if major == 0 && minor == 405 && patch < 286 {
                return true;
            }
        }
        false
    }

    /// Returns the available extensions set.
    pub fn get_available_extensions(&self) -> &BTreeSet<String> {
        &self.supported_extensions
    }

    /// Returns the device local memory size.
    pub fn get_device_local_memory(&self) -> u64 {
        self.device_access_memory
    }

    /// Returns true if memory budget reporting is supported.
    pub fn can_report_memory_usage(&self) -> bool {
        self.extensions.memory_budget
    }

    /// Returns currently used memory across the heaps tracked by
    /// `CollectPhysicalMemoryInfo`.
    ///
    /// Port of `Device::GetDeviceMemoryUsage`.
    pub fn get_device_memory_usage(&self) -> u64 {
        if !self.extensions.memory_budget {
            return 0;
        }

        let (_, Some(budget)) = physical_memory_properties(&self.instance, self.physical, true)
        else {
            return 0;
        };
        device_memory_usage_from_budget(&budget, &self.valid_heap_memory)
    }

    /// Returns the number of descriptor sets per pool.
    pub fn get_sets_per_pool(&self) -> u32 {
        self.sets_per_pool
    }

    /// Returns the max vertex input attributes.
    pub fn get_max_vertex_input_attributes(&self) -> u32 {
        self.device_properties.limits.max_vertex_input_attributes
    }

    /// Returns the max vertex input bindings.
    pub fn get_max_vertex_input_bindings(&self) -> u32 {
        self.device_properties.limits.max_vertex_input_bindings
    }

    /// Returns the max viewports.
    pub fn get_max_viewports(&self) -> u32 {
        self.device_properties.limits.max_viewports
    }

    /// Returns the max user clip distances.
    pub fn get_max_user_clip_distances(&self) -> u32 {
        self.device_properties.limits.max_clip_distances
    }

    /// Returns true if multi-viewport is supported.
    pub fn supports_multi_viewport(&self) -> bool {
        self.device_features.multi_viewport != 0
    }

    // -----------------------------------------------------------------------
    // Extension query helpers — port of the Is*Supported accessors
    // -----------------------------------------------------------------------

    pub fn is_nv_viewport_swizzle_supported(&self) -> bool {
        self.extensions.viewport_swizzle
    }

    pub fn is_nv_viewport_array2_supported(&self) -> bool {
        self.extensions.viewport_array2
    }

    pub fn is_nv_geometry_shader_passthrough_supported(&self) -> bool {
        self.extensions.geometry_shader_passthrough
    }

    pub fn is_khr_push_descriptor_supported(&self) -> bool {
        self.extensions.push_descriptor
    }

    pub fn is_khr_draw_indirect_count_supported(&self) -> bool {
        self.extensions.draw_indirect_count
    }

    pub fn is_ext_transform_feedback_supported(&self) -> bool {
        self.extensions.transform_feedback
    }

    pub fn are_transform_feedback_geometry_streams_supported(&self) -> bool {
        self.transform_feedback_geometry_streams_supported
    }

    pub fn is_ext_custom_border_color_supported(&self) -> bool {
        self.extensions.custom_border_color
    }

    pub fn is_ext_color_write_enable_supported(&self) -> bool {
        self.extensions.color_write_enable
    }

    pub fn is_ext_extended_dynamic_state_supported(&self) -> bool {
        self.extensions.extended_dynamic_state
    }

    pub fn is_ext_extended_dynamic_state2_supported(&self) -> bool {
        self.extensions.extended_dynamic_state2
    }

    pub fn is_ext_extended_dynamic_state2_extras_supported(&self) -> bool {
        self.extensions.extended_dynamic_state2_extra
    }

    pub fn is_ext_extended_dynamic_state3_supported(&self) -> bool {
        self.extensions.extended_dynamic_state3
    }

    pub fn is_ext_vertex_input_dynamic_state_supported(&self) -> bool {
        self.extensions.vertex_input_dynamic_state
    }

    pub fn is_ext_depth_clip_control_supported(&self) -> bool {
        self.extensions.depth_clip_control
    }

    pub fn is_ext_depth_bias_control_supported(&self) -> bool {
        self.extensions.depth_bias_control
    }

    pub fn is_ext_index_type_uint8_supported(&self) -> bool {
        self.extensions.index_type_uint8
    }

    pub fn is_ext_sampler_filter_minmax_supported(&self) -> bool {
        self.extensions.sampler_filter_minmax
    }

    pub fn is_ext_shader_stencil_export_supported(&self) -> bool {
        self.extensions.shader_stencil_export
    }

    pub fn is_ext_depth_range_unrestricted_supported(&self) -> bool {
        self.extensions.depth_range_unrestricted
    }

    pub fn is_ext_shader_viewport_index_layer_supported(&self) -> bool {
        self.extensions.shader_viewport_index_layer
            || (self.instance_version >= vk::API_VERSION_1_2 && self.shader_output_layer_supported)
    }

    pub fn is_ext_subgroup_size_control_supported(&self) -> bool {
        self.extensions.subgroup_size_control
    }

    pub fn is_ext_conservative_rasterization_supported(&self) -> bool {
        self.extensions.conservative_rasterization
    }

    pub fn is_ext_provoking_vertex_supported(&self) -> bool {
        self.extensions.provoking_vertex
    }

    pub fn supports_provoking_vertex_first_mode(&self) -> bool {
        self.extensions.provoking_vertex
    }

    pub fn supports_provoking_vertex_last_mode(&self) -> bool {
        self.extensions.provoking_vertex && self.provoking_vertex_last_supported
    }

    pub fn supports_transform_feedback_provoking_vertex_preservation(&self) -> bool {
        self.extensions.provoking_vertex && self.transform_feedback_preserves_provoking_vertex
    }

    pub fn is_ext_shader_atomic_int64_supported(&self) -> bool {
        self.extensions.shader_atomic_int64
    }

    /// Port of upstream `Device::IsSharedInt64AtomicsSupported`. Distinct from
    /// the buffer-side query above: shader-shared 64-bit atomics are a separate
    /// feature bit.
    pub fn is_shared_int64_atomics_supported(&self) -> bool {
        self.extensions.shader_atomic_int64 && self.shader_shared_int64_atomics_supported
    }

    pub fn is_ext_conditional_rendering(&self) -> bool {
        self.extensions.conditional_rendering
    }

    pub fn is_ext_line_rasterization_supported(&self) -> bool {
        self.extensions.line_rasterization
    }

    pub fn is_ext_vertex_attribute_divisor_supported(&self) -> bool {
        self.extensions.vertex_attribute_divisor
    }

    pub fn is_ext_shader_demote_to_helper_invocation_supported(&self) -> bool {
        self.extensions.shader_demote_to_helper_invocation
    }

    pub fn is_khr_shader_float_controls_supported(&self) -> bool {
        self.extensions.shader_float_controls
    }

    pub fn is_khr_workgroup_memory_explicit_layout_supported(&self) -> bool {
        self.extensions.workgroup_memory_explicit_layout
    }

    pub fn is_khr_maintenance5_supported(&self) -> bool {
        self.extensions.maintenance5
    }

    pub fn supports_polygon_mode_point_size(&self) -> bool {
        self.extensions.maintenance5 && self.maintenance5_polygon_mode_point_size_supported
    }

    pub fn supports_depth_stencil_swizzle_one(&self) -> bool {
        self.extensions.maintenance5 && self.maintenance5_depth_stencil_swizzle_one_supported
    }

    pub fn supports_early_fragment_tests(&self) -> bool {
        self.extensions.maintenance5
            && self.maintenance5_early_fragment_multisample_coverage_after_sample_counting
            && self.maintenance5_early_fragment_sample_mask_test_before_sample_counting
    }

    pub fn is_khr_image_format_list_supported(&self) -> bool {
        self.extensions.image_format_list || self.instance_version >= vk::API_VERSION_1_2
    }

    pub fn is_khr_swapchain_mutable_format_enabled(&self) -> bool {
        self.extensions.swapchain_mutable_format
    }

    pub fn is_khr_pipeline_executable_properties_enabled(&self) -> bool {
        self.extensions.pipeline_executable_properties
    }

    pub fn is_khr_uniform_buffer_standard_layout_supported(&self) -> bool {
        self.extensions.uniform_buffer_standard_layout
    }

    pub fn is_blit_depth24_stencil8_supported(&self) -> bool {
        self.is_blit_depth24_stencil8_supported
    }

    pub fn is_blit_depth32_stencil8_supported(&self) -> bool {
        self.is_blit_depth32_stencil8_supported
    }

    pub fn is_warp_size_potentially_bigger_than_guest(&self) -> bool {
        self.is_warp_potentially_bigger
    }

    pub fn supports_d24_depth_buffer(&self) -> bool {
        self.supports_d24_depth
    }

    pub fn is_ext_extended_dynamic_state3_blending_supported(&self) -> bool {
        self.dynamic_state3_blending
    }

    pub fn is_ext_extended_dynamic_state3_enables_supported(&self) -> bool {
        self.dynamic_state3_enables
    }

    /// Port of `Device::IsExtFilterCubicSupported`.
    pub fn is_ext_filter_cubic_supported(&self) -> bool {
        self.extensions.filter_cubic
    }

    /// Port of `Device::IsQcomFilterCubicWeightsSupported`.
    pub fn is_qcom_filter_cubic_weights_supported(&self) -> bool {
        self.extensions.filter_cubic_weights
    }

    pub fn supports_dynamic_state3_depth_clamp_enable(&self) -> bool {
        self.dynamic_state3_depth_clamp_enable
    }

    pub fn supports_dynamic_state3_logic_op_enable(&self) -> bool {
        self.dynamic_state3_logic_op_enable
    }

    pub fn supports_dynamic_state3_line_rasterization_mode(&self) -> bool {
        self.dynamic_state3_line_raster_mode
    }

    pub fn supports_dynamic_state3_conservative_rasterization_mode(&self) -> bool {
        self.dynamic_state3_conservative_raster_mode
    }

    pub fn supports_dynamic_state3_line_stipple_enable(&self) -> bool {
        self.dynamic_state3_line_stipple_enable
    }

    pub fn supports_dynamic_state3_alpha_to_coverage_enable(&self) -> bool {
        self.dynamic_state3_alpha_to_coverage
    }

    pub fn supports_dynamic_state3_alpha_to_one_enable(&self) -> bool {
        self.dynamic_state3_alpha_to_one
    }

    pub fn supports_smooth_lines(&self) -> bool {
        self.smooth_lines_supported
    }

    pub fn supports_stippled_rectangular_lines(&self) -> bool {
        self.stippled_rectangular_lines_supported
    }

    pub fn is_molten_vk(&self) -> bool {
        self.get_driver_id() == vk::DriverId::MOLTENVK
    }

    pub fn supports_alpha_to_one(&self) -> bool {
        self.device_features.alpha_to_one != 0
    }

    pub fn supports_conditional_barriers(&self) -> bool {
        self.supports_conditional_barriers
    }

    pub fn has_exact_depth_bias_control(&self) -> bool {
        self.exact_depth_bias_control_supported
    }

    pub fn cant_blit_msaa(&self) -> bool {
        self.cant_blit_msaa
    }

    pub fn must_emulate_scaled_formats(&self) -> bool {
        self.must_emulate_scaled_formats
    }

    pub fn must_emulate_bgr565(&self) -> bool {
        self.must_emulate_bgr565
    }

    /// Returns true if the device supports `VK_EXT_4444_formats`.
    ///
    /// Port of upstream `Device::IsExt4444FormatsSupported`.
    pub fn is_ext_4444_formats_supported(&self) -> bool {
        self.format_a4b4g4r4_supported
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.shutdown_gpu_logging();
    }
}

/// Shared implementation of `Device::ReportLoss` for Vulkan owners which only
/// retain the logical device handle.
pub(crate) fn report_device_loss() {
    log::error!("Device loss occurred!");
    std::thread::sleep(std::time::Duration::from_secs(15));
}

fn physical_memory_properties(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    has_memory_budget: bool,
) -> (
    vk::PhysicalDeviceMemoryProperties,
    Option<vk::PhysicalDeviceMemoryBudgetPropertiesEXT>,
) {
    if has_memory_budget {
        let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
        let mut properties2 = vk::PhysicalDeviceMemoryProperties2::builder()
            .push_next(&mut budget)
            .build();
        unsafe {
            instance.get_physical_device_memory_properties2(physical, &mut properties2);
        }
        return (properties2.memory_properties, Some(budget));
    }

    let properties = unsafe { instance.get_physical_device_memory_properties(physical) };
    (properties, None)
}

/// Query the memory information used by upstream `Device` memory accessors.
pub fn query_device_memory_info(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
) -> DeviceMemoryInfo {
    let available_extensions = unsafe {
        instance
            .enumerate_device_extension_properties(physical)
            .unwrap_or_default()
    };
    let supported_extensions: BTreeSet<String> = available_extensions
        .iter()
        .map(|ext| {
            let name = unsafe { CStr::from_ptr(ext.extension_name.as_ptr()) };
            name.to_string_lossy().into_owned()
        })
        .collect();
    let has_memory_budget = supported_extensions.contains("VK_EXT_memory_budget");
    let device_properties = unsafe { instance.get_physical_device_properties(physical) };
    let is_integrated = device_properties.device_type == vk::PhysicalDeviceType::INTEGRATED_GPU;
    let (memory_properties, memory_budget) =
        physical_memory_properties(instance, physical, has_memory_budget);
    let (device_local_memory, valid_heap_memory) =
        collect_physical_memory_info(&memory_properties, memory_budget.as_ref(), is_integrated);

    DeviceMemoryInfo {
        device_local_memory,
        can_report_memory_usage: has_memory_budget,
        valid_heap_memory,
    }
}

fn collect_physical_memory_info(
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    memory_budget: Option<&vk::PhysicalDeviceMemoryBudgetPropertiesEXT>,
    is_integrated: bool,
) -> (u64, Vec<usize>) {
    let mut device_access_memory = 0_u64;
    let mut device_initial_usage = 0_u64;
    let mut local_memory = 0_u64;
    let mut valid_heap_memory = Vec::new();

    for element in 0..memory_properties.memory_heap_count as usize {
        let heap = memory_properties.memory_heaps[element];
        let is_heap_local = heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL);
        if !is_integrated && !is_heap_local {
            continue;
        }

        valid_heap_memory.push(element);
        if is_heap_local {
            local_memory = local_memory.wrapping_add(heap.size);
        }

        if let Some(budget) = memory_budget {
            device_initial_usage = device_initial_usage.wrapping_add(budget.heap_usage[element]);
            device_access_memory = device_access_memory.wrapping_add(budget.heap_budget[element]);
        } else {
            device_access_memory = device_access_memory.wrapping_add(heap.size);
        }
    }

    if !is_integrated {
        let reserve_memory = std::cmp::min(device_access_memory / 8, ONE_GIB);
        device_access_memory -= reserve_memory;

        if *common::settings::values().vram_usage_mode.get_value()
            != common::settings::VramUsageMode::Aggressive
        {
            let normal_memory = 6 * ONE_GIB;
            let scaler_memory =
                ONE_GIB * common::settings::values().resolution_info.scale_up_u32(1) as u64;
            device_access_memory =
                std::cmp::min(device_access_memory, normal_memory + scaler_memory);
        }

        return (device_access_memory, valid_heap_memory);
    }

    let available_memory = device_access_memory.wrapping_sub(device_initial_usage) as i64;
    let memory_size =
        integrated_memory_size(*common::settings::values().vram_usage_mode.get_value());
    let upper = std::cmp::min(available_memory - 8 * ONE_GIB as i64, memory_size as i64);
    let lower = std::cmp::min(local_memory as i64, memory_size as i64);
    (std::cmp::max(upper, lower) as u64, valid_heap_memory)
}

fn integrated_memory_size(mode: common::settings_enums::VramUsageMode) -> u64 {
    match mode {
        common::settings_enums::VramUsageMode::Conservative => 4 * ONE_GIB,
        common::settings_enums::VramUsageMode::Aggressive => 6 * ONE_GIB,
    }
}

fn device_memory_usage_from_budget(
    memory_budget: &vk::PhysicalDeviceMemoryBudgetPropertiesEXT,
    valid_heap_memory: &[usize],
) -> u64 {
    valid_heap_memory
        .iter()
        .map(|&heap| memory_budget.heap_usage[heap])
        .sum()
}

/// Query current memory usage with the same heap filter as `Device`.
pub fn query_device_memory_usage(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    memory_info: &DeviceMemoryInfo,
) -> u64 {
    if !memory_info.can_report_memory_usage {
        return 0;
    }
    let (_, Some(budget)) = physical_memory_properties(instance, physical, true) else {
        return 0;
    };
    device_memory_usage_from_budget(&budget, &memory_info.valid_heap_memory)
}

fn driver_name_from_id(driver_id: vk::DriverId) -> Option<&'static str> {
    match driver_id {
        vk::DriverId::AMD_PROPRIETARY => Some("AMD"),
        vk::DriverId::AMD_OPEN_SOURCE => Some("AMDVLK"),
        vk::DriverId::MESA_RADV => Some("RADV"),
        vk::DriverId::NVIDIA_PROPRIETARY => Some("NVIDIA"),
        vk::DriverId::INTEL_PROPRIETARY_WINDOWS => Some("Intel"),
        vk::DriverId::INTEL_OPEN_SOURCE_MESA => Some("ANV"),
        vk::DriverId::IMAGINATION_PROPRIETARY => Some("PowerVR"),
        vk::DriverId::QUALCOMM_PROPRIETARY => Some("Qualcomm"),
        vk::DriverId::ARM_PROPRIETARY => Some("Mali"),
        vk::DriverId::SAMSUNG_PROPRIETARY => Some("Xclipse"),
        vk::DriverId::GOOGLE_SWIFTSHADER => Some("SwiftShader"),
        vk::DriverId::BROADCOM_PROPRIETARY => Some("Broadcom"),
        vk::DriverId::MESA_LLVMPIPE => Some("Lavapipe"),
        vk::DriverId::MOLTENVK => Some("MoltenVK"),
        vk::DriverId::VERISILICON_PROPRIETARY => Some("Vivante"),
        vk::DriverId::MESA_TURNIP => Some("Turnip"),
        vk::DriverId::MESA_V3DV => Some("V3DV"),
        vk::DriverId::MESA_PANVK => Some("PanVK"),
        vk::DriverId::MESA_VENUS => Some("Venus"),
        vk::DriverId::MESA_DOZEN => Some("Dozen"),
        vk::DriverId::MESA_NVK => Some("NVK"),
        vk::DriverId::IMAGINATION_OPEN_SOURCE_MESA => Some("PVR"),
        _ => None,
    }
}

fn sampler_filter_minmax_supported(
    extension_available: bool,
    is_amd: bool,
    shader_float16_supported: bool,
) -> bool {
    extension_available && (!is_amd || shader_float16_supported)
}

fn derive_sampler_heap_budget(sampler_limit: usize) -> usize {
    if sampler_limit == 0 {
        return 0;
    }
    const MIN_SAMPLER_BUDGET: usize = 1024;
    let reserved = sampler_limit / 4;
    MIN_SAMPLER_BUDGET.max(sampler_limit - reserved)
}

fn initial_loaded_extensions(
    api_version: u32,
    supported_extensions: &BTreeSet<String>,
    enable_device_fault: bool,
) -> BTreeSet<String> {
    const FEATURE_EXTENSIONS: &[&str] = &[
        "VK_EXT_color_write_enable",
        "VK_EXT_custom_border_color",
        "VK_EXT_depth_bias_control",
        "VK_EXT_depth_clip_control",
        "VK_EXT_descriptor_buffer",
        "VK_EXT_extended_dynamic_state",
        "VK_EXT_extended_dynamic_state2",
        "VK_EXT_extended_dynamic_state3",
        "VK_EXT_4444_formats",
        "VK_EXT_index_type_uint8",
        "VK_EXT_line_rasterization",
        "VK_EXT_primitive_topology_list_restart",
        "VK_EXT_provoking_vertex",
        "VK_EXT_robustness2",
        "VK_EXT_transform_feedback",
        "VK_EXT_vertex_input_dynamic_state",
        "VK_KHR_maintenance5",
        "VK_KHR_pipeline_executable_properties",
        "VK_KHR_workgroup_memory_explicit_layout",
    ];
    const EXTENSIONS: &[&str] = &[
        "VK_EXT_conditional_rendering",
        "VK_EXT_conservative_rasterization",
        "VK_EXT_depth_range_unrestricted",
        "VK_EXT_memory_budget",
        "VK_EXT_robustness2",
        "VK_EXT_sampler_filter_minmax",
        "VK_EXT_shader_stencil_export",
        "VK_EXT_shader_viewport_index_layer",
        "VK_EXT_tooling_info",
        "VK_EXT_vertex_attribute_divisor",
        "VK_KHR_draw_indirect_count",
        "VK_KHR_driver_properties",
        "VK_KHR_push_descriptor",
        "VK_KHR_sampler_mirror_clamp_to_edge",
        "VK_KHR_shader_float_controls",
        "VK_KHR_spirv_1_4",
        "VK_KHR_swapchain",
        "VK_KHR_swapchain_mutable_format",
        "VK_KHR_image_format_list",
        "VK_NV_device_diagnostics_config",
        "VK_NV_geometry_shader_passthrough",
        "VK_NV_viewport_array2",
        "VK_NV_viewport_swizzle",
        "VK_EXT_filter_cubic",
        "VK_IMG_filter_cubic",
        "VK_QCOM_filter_cubic_weights",
        // Required by MoltenVK even though the upstream revision predates
        // explicit portability-subset handling.
        "VK_KHR_portability_subset",
    ];
    const FEATURES_1_2: &[&str] = &[
        "VK_EXT_descriptor_indexing",
        "VK_EXT_host_query_reset",
        "VK_KHR_8bit_storage",
        "VK_KHR_buffer_device_address",
        "VK_KHR_timeline_semaphore",
    ];
    const FEATURES_1_3: &[&str] = &[
        "VK_EXT_shader_demote_to_helper_invocation",
        "VK_EXT_subgroup_size_control",
        "VK_KHR_synchronization2",
    ];

    let mut loaded = BTreeSet::new();
    for &name in FEATURE_EXTENSIONS.iter().chain(EXTENSIONS) {
        if supported_extensions.contains(name) {
            loaded.insert(name.to_string());
        }
    }
    if api_version < vk::API_VERSION_1_2 {
        for &name in FEATURES_1_2 {
            if supported_extensions.contains(name) {
                loaded.insert(name.to_string());
            }
        }
    }
    if api_version < vk::API_VERSION_1_3 {
        for &name in FEATURES_1_3 {
            if supported_extensions.contains(name) {
                loaded.insert(name.to_string());
            }
        }
    }
    if enable_device_fault {
        loaded.insert("VK_EXT_device_fault".to_string());
    }
    loaded
}

fn remove_extension_if_unsupported(
    loaded_extensions: &mut BTreeSet<String>,
    extension: &str,
    is_supported: bool,
) {
    if !is_supported && loaded_extensions.remove(extension) {
        log::warn!("Removing unsuitable extension {}", extension);
    }
}

#[allow(clippy::too_many_arguments)]
fn device_is_suitable(
    api_version: u32,
    requires_swapchain: bool,
    extensions: &BTreeSet<String>,
    features: &vk::PhysicalDeviceFeatures,
    storage_16bit: &vk::PhysicalDevice16BitStorageFeatures,
    storage_8bit: &vk::PhysicalDevice8BitStorageFeatures,
    host_query_reset: &vk::PhysicalDeviceHostQueryResetFeatures,
    shader_demote: &vk::PhysicalDeviceShaderDemoteToHelperInvocationFeatures,
    shader_draw_parameters: &vk::PhysicalDeviceShaderDrawParametersFeatures,
    variable_pointers: &vk::PhysicalDeviceVariablePointersFeatures,
    limits: &vk::PhysicalDeviceLimits,
) -> bool {
    let mut suitable = api_version >= vk::API_VERSION_1_1;
    if !suitable {
        log::error!("Vulkan 1.1 or newer is required");
    }
    let loaded_extensions = initial_loaded_extensions(api_version, extensions, false);
    for extension in [
        "VK_EXT_conditional_rendering",
        "VK_EXT_conservative_rasterization",
        "VK_EXT_depth_bias_control",
        "VK_EXT_depth_range_unrestricted",
        "VK_EXT_extended_dynamic_state",
        "VK_EXT_extended_dynamic_state2",
        "VK_EXT_extended_dynamic_state3",
        "VK_EXT_external_memory_host",
        "VK_EXT_4444_formats",
        "VK_EXT_line_rasterization",
        "VK_EXT_robustness2",
        "VK_EXT_vertex_input_dynamic_state",
        "VK_NV_geometry_shader_passthrough",
        "VK_NV_viewport_array2",
        "VK_NV_viewport_swizzle",
    ] {
        if !loaded_extensions.contains(extension) {
            log::info!("Device doesn't support extension {}", extension);
        }
    }
    for extension in [
        "VK_EXT_vertex_attribute_divisor",
        "VK_KHR_driver_properties",
        "VK_KHR_sampler_mirror_clamp_to_edge",
        "VK_KHR_shader_float_controls",
    ] {
        if !extensions.contains(extension) {
            log::error!("Missing required extension {}", extension);
            suitable = false;
        }
    }
    if requires_swapchain && !extensions.contains("VK_KHR_swapchain") {
        log::error!("Missing required extension VK_KHR_swapchain");
        suitable = false;
    }
    if api_version < vk::API_VERSION_1_2 && !extensions.contains("VK_EXT_descriptor_indexing") {
        log::error!("Missing required extension VK_EXT_descriptor_indexing");
        suitable = false;
    }

    macro_rules! require_feature {
        ($value:expr, $name:literal) => {
            if $value == vk::FALSE {
                log::error!("Missing required feature {}", $name);
                suitable = false;
            }
        };
    }
    require_feature!(
        storage_16bit.storage_buffer16_bit_access,
        "storageBuffer16BitAccess"
    );
    require_feature!(
        storage_16bit.uniform_and_storage_buffer16_bit_access,
        "uniformAndStorageBuffer16BitAccess"
    );
    require_feature!(
        storage_8bit.storage_buffer8_bit_access,
        "storageBuffer8BitAccess"
    );
    require_feature!(
        storage_8bit.uniform_and_storage_buffer8_bit_access,
        "uniformAndStorageBuffer8BitAccess"
    );
    require_feature!(features.depth_bias_clamp, "depthBiasClamp");
    require_feature!(features.depth_clamp, "depthClamp");
    require_feature!(
        features.draw_indirect_first_instance,
        "drawIndirectFirstInstance"
    );
    require_feature!(features.dual_src_blend, "dualSrcBlend");
    require_feature!(
        features.fragment_stores_and_atomics,
        "fragmentStoresAndAtomics"
    );
    require_feature!(features.image_cube_array, "imageCubeArray");
    require_feature!(features.independent_blend, "independentBlend");
    require_feature!(features.logic_op, "logicOp");
    require_feature!(features.multi_draw_indirect, "multiDrawIndirect");
    require_feature!(features.multi_viewport, "multiViewport");
    require_feature!(features.occlusion_query_precise, "occlusionQueryPrecise");
    require_feature!(features.robust_buffer_access, "robustBufferAccess");
    require_feature!(features.sampler_anisotropy, "samplerAnisotropy");
    require_feature!(features.sample_rate_shading, "sampleRateShading");
    require_feature!(features.shader_clip_distance, "shaderClipDistance");
    require_feature!(
        features.shader_image_gather_extended,
        "shaderImageGatherExtended"
    );
    require_feature!(
        features.shader_storage_image_write_without_format,
        "shaderStorageImageWriteWithoutFormat"
    );
    require_feature!(
        features.vertex_pipeline_stores_and_atomics,
        "vertexPipelineStoresAndAtomics"
    );
    require_feature!(host_query_reset.host_query_reset, "hostQueryReset");
    require_feature!(
        shader_demote.shader_demote_to_helper_invocation,
        "shaderDemoteToHelperInvocation"
    );
    require_feature!(
        shader_draw_parameters.shader_draw_parameters,
        "shaderDrawParameters"
    );
    require_feature!(variable_pointers.variable_pointers, "variablePointers");
    require_feature!(
        variable_pointers.variable_pointers_storage_buffer,
        "variablePointersStorageBuffer"
    );

    for (minimum, value, name) in [
        (
            65_536,
            limits.max_uniform_buffer_range,
            "maxUniformBufferRange",
        ),
        (16, limits.max_viewports, "maxViewports"),
        (8, limits.max_color_attachments, "maxColorAttachments"),
        (8, limits.max_clip_distances, "maxClipDistances"),
    ] {
        if value < minimum {
            log::error!(
                "{} has to be {} or greater but it is {}",
                name,
                minimum,
                value
            );
            suitable = false;
        }
    }
    suitable
}

fn get_nvidia_architecture(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    extensions: &BTreeSet<String>,
) -> NvidiaArchitecture {
    if extensions.contains("VK_KHR_fragment_shading_rate") {
        let mut shading_rate = vk::PhysicalDeviceFragmentShadingRatePropertiesKHR::default();
        let mut properties = vk::PhysicalDeviceProperties2::builder()
            .push_next(&mut shading_rate)
            .build();
        unsafe { instance.get_physical_device_properties2(physical, &mut properties) };
        return if shading_rate.primitive_fragment_shading_rate_with_multiple_viewports != 0 {
            NvidiaArchitecture::AmpereOrNewer
        } else {
            NvidiaArchitecture::Turing
        };
    }
    if extensions.contains("VK_EXT_blend_operation_advanced") {
        let mut blend = vk::PhysicalDeviceBlendOperationAdvancedPropertiesEXT::default();
        let mut properties = vk::PhysicalDeviceProperties2::builder()
            .push_next(&mut blend)
            .build();
        unsafe { instance.get_physical_device_properties2(physical, &mut properties) };
        if blend.advanced_blend_max_color_attachments == 1 {
            return NvidiaArchitecture::Maxwell;
        }
        if extensions.contains("VK_EXT_conservative_rasterization") {
            let mut conservative =
                vk::PhysicalDeviceConservativeRasterizationPropertiesEXT::default();
            let mut properties = vk::PhysicalDeviceProperties2::builder()
                .push_next(&mut conservative)
                .build();
            unsafe { instance.get_physical_device_properties2(physical, &mut properties) };
            return if conservative.degenerate_lines_rasterized != 0 {
                NvidiaArchitecture::Volta
            } else {
                NvidiaArchitecture::Pascal
            };
        }
    }
    NvidiaArchitecture::KeplerOrOlder
}

fn collect_tooling_info(
    entry: &ash::Entry,
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    tooling_info_enabled: bool,
) -> (bool, bool) {
    if !tooling_info_enabled {
        return (false, false);
    }
    let tools = get_physical_device_tool_properties(entry, instance, physical);
    let mut has_renderdoc = false;
    let mut has_nsight_graphics = false;
    for tool in tools {
        let name = unsafe { CStr::from_ptr(tool.name.as_ptr()) }.to_string_lossy();
        log::info!("Attached debugging tool: {}", name);
        has_renderdoc |= name == "RenderDoc";
        has_nsight_graphics |= name == "NVIDIA Nsight Graphics";
    }
    (has_renderdoc, has_nsight_graphics)
}

fn collect_format_properties(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
) -> HashMap<vk::Format, vk::FormatProperties> {
    const FORMATS: &[vk::Format] = &[
        vk::Format::A1R5G5B5_UNORM_PACK16,
        vk::Format::A2B10G10R10_SINT_PACK32,
        vk::Format::A2B10G10R10_SNORM_PACK32,
        vk::Format::A2B10G10R10_SSCALED_PACK32,
        vk::Format::A2B10G10R10_UINT_PACK32,
        vk::Format::A2B10G10R10_UNORM_PACK32,
        vk::Format::A2B10G10R10_USCALED_PACK32,
        vk::Format::A2R10G10B10_UNORM_PACK32,
        vk::Format::A8B8G8R8_SINT_PACK32,
        vk::Format::A8B8G8R8_SNORM_PACK32,
        vk::Format::A8B8G8R8_SRGB_PACK32,
        vk::Format::A8B8G8R8_UINT_PACK32,
        vk::Format::A8B8G8R8_UNORM_PACK32,
        vk::Format::ASTC_10X10_SRGB_BLOCK,
        vk::Format::ASTC_10X10_UNORM_BLOCK,
        vk::Format::ASTC_10X5_SRGB_BLOCK,
        vk::Format::ASTC_10X5_UNORM_BLOCK,
        vk::Format::ASTC_10X6_SRGB_BLOCK,
        vk::Format::ASTC_10X6_UNORM_BLOCK,
        vk::Format::ASTC_10X8_SRGB_BLOCK,
        vk::Format::ASTC_10X8_UNORM_BLOCK,
        vk::Format::ASTC_12X10_SRGB_BLOCK,
        vk::Format::ASTC_12X10_UNORM_BLOCK,
        vk::Format::ASTC_12X12_SRGB_BLOCK,
        vk::Format::ASTC_12X12_UNORM_BLOCK,
        vk::Format::ASTC_4X4_SRGB_BLOCK,
        vk::Format::ASTC_4X4_UNORM_BLOCK,
        vk::Format::ASTC_5X4_SRGB_BLOCK,
        vk::Format::ASTC_5X4_UNORM_BLOCK,
        vk::Format::ASTC_5X5_SRGB_BLOCK,
        vk::Format::ASTC_5X5_UNORM_BLOCK,
        vk::Format::ASTC_6X5_SRGB_BLOCK,
        vk::Format::ASTC_6X5_UNORM_BLOCK,
        vk::Format::ASTC_6X6_SRGB_BLOCK,
        vk::Format::ASTC_6X6_UNORM_BLOCK,
        vk::Format::ASTC_8X5_SRGB_BLOCK,
        vk::Format::ASTC_8X5_UNORM_BLOCK,
        vk::Format::ASTC_8X6_SRGB_BLOCK,
        vk::Format::ASTC_8X6_UNORM_BLOCK,
        vk::Format::ASTC_8X8_SRGB_BLOCK,
        vk::Format::ASTC_8X8_UNORM_BLOCK,
        vk::Format::B10G11R11_UFLOAT_PACK32,
        vk::Format::B4G4R4A4_UNORM_PACK16,
        vk::Format::B5G5R5A1_UNORM_PACK16,
        vk::Format::B5G6R5_UNORM_PACK16,
        vk::Format::B8G8R8A8_SRGB,
        vk::Format::B8G8R8A8_UNORM,
        vk::Format::BC1_RGBA_SRGB_BLOCK,
        vk::Format::BC1_RGBA_UNORM_BLOCK,
        vk::Format::BC2_SRGB_BLOCK,
        vk::Format::BC2_UNORM_BLOCK,
        vk::Format::BC3_SRGB_BLOCK,
        vk::Format::BC3_UNORM_BLOCK,
        vk::Format::BC4_SNORM_BLOCK,
        vk::Format::BC4_UNORM_BLOCK,
        vk::Format::BC5_SNORM_BLOCK,
        vk::Format::BC5_UNORM_BLOCK,
        vk::Format::BC6H_SFLOAT_BLOCK,
        vk::Format::BC6H_UFLOAT_BLOCK,
        vk::Format::BC7_SRGB_BLOCK,
        vk::Format::BC7_UNORM_BLOCK,
        vk::Format::D16_UNORM,
        vk::Format::D16_UNORM_S8_UINT,
        vk::Format::X8_D24_UNORM_PACK32,
        vk::Format::D24_UNORM_S8_UINT,
        vk::Format::D32_SFLOAT,
        vk::Format::D32_SFLOAT_S8_UINT,
        vk::Format::E5B9G9R9_UFLOAT_PACK32,
        vk::Format::R16G16B16A16_SFLOAT,
        vk::Format::R16G16B16A16_SINT,
        vk::Format::R16G16B16A16_SNORM,
        vk::Format::R16G16B16A16_SSCALED,
        vk::Format::R16G16B16A16_UINT,
        vk::Format::R16G16B16A16_UNORM,
        vk::Format::R16G16B16A16_USCALED,
        vk::Format::R16G16B16_SFLOAT,
        vk::Format::R16G16B16_SINT,
        vk::Format::R16G16B16_SNORM,
        vk::Format::R16G16B16_SSCALED,
        vk::Format::R16G16B16_UINT,
        vk::Format::R16G16B16_UNORM,
        vk::Format::R16G16B16_USCALED,
        vk::Format::R16G16_SFLOAT,
        vk::Format::R16G16_SINT,
        vk::Format::R16G16_SNORM,
        vk::Format::R16G16_SSCALED,
        vk::Format::R16G16_UINT,
        vk::Format::R16G16_UNORM,
        vk::Format::R16G16_USCALED,
        vk::Format::R16_SFLOAT,
        vk::Format::R16_SINT,
        vk::Format::R16_SNORM,
        vk::Format::R16_SSCALED,
        vk::Format::R16_UINT,
        vk::Format::R16_UNORM,
        vk::Format::R16_USCALED,
        vk::Format::R32G32B32A32_SFLOAT,
        vk::Format::R32G32B32A32_SINT,
        vk::Format::R32G32B32A32_UINT,
        vk::Format::R32G32B32_SFLOAT,
        vk::Format::R32G32B32_SINT,
        vk::Format::R32G32B32_UINT,
        vk::Format::R32G32_SFLOAT,
        vk::Format::R32G32_SINT,
        vk::Format::R32G32_UINT,
        vk::Format::R32_SFLOAT,
        vk::Format::R32_SINT,
        vk::Format::R32_UINT,
        vk::Format::R4G4B4A4_UNORM_PACK16,
        vk::Format::A4B4G4R4_UNORM_PACK16_EXT,
        vk::Format::R4G4_UNORM_PACK8,
        vk::Format::R5G5B5A1_UNORM_PACK16,
        vk::Format::R5G6B5_UNORM_PACK16,
        vk::Format::R8G8B8A8_SINT,
        vk::Format::R8G8B8A8_SNORM,
        vk::Format::R8G8B8A8_SRGB,
        vk::Format::R8G8B8A8_SSCALED,
        vk::Format::R8G8B8A8_UINT,
        vk::Format::R8G8B8A8_UNORM,
        vk::Format::R8G8B8A8_USCALED,
        vk::Format::R8G8B8_SINT,
        vk::Format::R8G8B8_SNORM,
        vk::Format::R8G8B8_SSCALED,
        vk::Format::R8G8B8_UINT,
        vk::Format::R8G8B8_UNORM,
        vk::Format::R8G8B8_USCALED,
        vk::Format::R8G8_SINT,
        vk::Format::R8G8_SNORM,
        vk::Format::R8G8_SSCALED,
        vk::Format::R8G8_UINT,
        vk::Format::R8G8_UNORM,
        vk::Format::R8G8_USCALED,
        vk::Format::R8_SINT,
        vk::Format::R8_SNORM,
        vk::Format::R8_SSCALED,
        vk::Format::R8_UINT,
        vk::Format::R8_UNORM,
        vk::Format::R8_USCALED,
        vk::Format::S8_UINT,
        vk::Format::ETC2_R8G8B8_UNORM_BLOCK,
        vk::Format::ETC2_R8G8B8A8_UNORM_BLOCK,
        vk::Format::ETC2_R8G8B8A1_UNORM_BLOCK,
        vk::Format::ETC2_R8G8B8_SRGB_BLOCK,
        vk::Format::ETC2_R8G8B8A8_SRGB_BLOCK,
        vk::Format::ETC2_R8G8B8A1_SRGB_BLOCK,
        vk::Format::EAC_R11_UNORM_BLOCK,
        vk::Format::EAC_R11_SNORM_BLOCK,
        vk::Format::EAC_R11G11_UNORM_BLOCK,
        vk::Format::EAC_R11G11_SNORM_BLOCK,
    ];
    FORMATS
        .iter()
        .copied()
        .map(|format| {
            let properties =
                unsafe { instance.get_physical_device_format_properties(physical, format) };
            (format, properties)
        })
        .collect()
}

fn compute_is_optimal_astc_supported(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    texture_compression_astc_ldr: bool,
) -> bool {
    const ASTC_FORMATS: &[vk::Format] = &[
        vk::Format::ASTC_4X4_UNORM_BLOCK,
        vk::Format::ASTC_4X4_SRGB_BLOCK,
        vk::Format::ASTC_5X4_UNORM_BLOCK,
        vk::Format::ASTC_5X4_SRGB_BLOCK,
        vk::Format::ASTC_5X5_UNORM_BLOCK,
        vk::Format::ASTC_5X5_SRGB_BLOCK,
        vk::Format::ASTC_6X5_UNORM_BLOCK,
        vk::Format::ASTC_6X5_SRGB_BLOCK,
        vk::Format::ASTC_6X6_UNORM_BLOCK,
        vk::Format::ASTC_6X6_SRGB_BLOCK,
        vk::Format::ASTC_8X5_UNORM_BLOCK,
        vk::Format::ASTC_8X5_SRGB_BLOCK,
        vk::Format::ASTC_8X6_UNORM_BLOCK,
        vk::Format::ASTC_8X6_SRGB_BLOCK,
        vk::Format::ASTC_8X8_UNORM_BLOCK,
        vk::Format::ASTC_8X8_SRGB_BLOCK,
        vk::Format::ASTC_10X5_UNORM_BLOCK,
        vk::Format::ASTC_10X5_SRGB_BLOCK,
        vk::Format::ASTC_10X6_UNORM_BLOCK,
        vk::Format::ASTC_10X6_SRGB_BLOCK,
        vk::Format::ASTC_10X8_UNORM_BLOCK,
        vk::Format::ASTC_10X8_SRGB_BLOCK,
        vk::Format::ASTC_10X10_UNORM_BLOCK,
        vk::Format::ASTC_10X10_SRGB_BLOCK,
        vk::Format::ASTC_12X10_UNORM_BLOCK,
        vk::Format::ASTC_12X10_SRGB_BLOCK,
        vk::Format::ASTC_12X12_UNORM_BLOCK,
        vk::Format::ASTC_12X12_SRGB_BLOCK,
    ];
    if !texture_compression_astc_ldr {
        return false;
    }
    let required = vk::FormatFeatureFlags::SAMPLED_IMAGE
        | vk::FormatFeatureFlags::BLIT_SRC
        | vk::FormatFeatureFlags::BLIT_DST
        | vk::FormatFeatureFlags::TRANSFER_SRC
        | vk::FormatFeatureFlags::TRANSFER_DST;
    ASTC_FORMATS.iter().all(|&format| {
        let properties =
            unsafe { instance.get_physical_device_format_properties(physical, format) };
        !(properties.optimal_tiling_features & required).is_empty()
    })
}

fn test_depth_stencil_blits(
    format_properties: &HashMap<vk::Format, vk::FormatProperties>,
    format: vk::Format,
) -> bool {
    let required = vk::FormatFeatureFlags::BLIT_SRC | vk::FormatFeatureFlags::BLIT_DST;
    format_properties
        .get(&format)
        .is_some_and(|properties| properties.optimal_tiling_features.contains(required))
}

fn cap_moltenvk_vertex_input_limits(limits: &mut vk::PhysicalDeviceLimits) {
    limits.max_vertex_input_attributes = limits.max_vertex_input_attributes.min(16);
    limits.max_vertex_input_bindings = limits.max_vertex_input_bindings.min(16);
}

fn configure_robustness2_features(
    features: &mut vk::PhysicalDeviceRobustness2FeaturesEXT,
    extension_available: bool,
) -> bool {
    // Match Eden: robustness2 is enabled only for null descriptors. Enabling
    // the stricter buffer/image access features adds work without guest benefit.
    features.robust_buffer_access2 = vk::FALSE;
    features.robust_image_access2 = vk::FALSE;
    extension_available && features.null_descriptor != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance5_raw_payloads_match_vulkan_abi() {
        let pointer_size = std::mem::size_of::<*mut std::ffi::c_void>();
        assert_eq!(
            std::mem::size_of::<PhysicalDeviceMaintenance5FeaturesKhr>(),
            if pointer_size == 8 { 24 } else { 12 }
        );
        assert_eq!(
            std::mem::offset_of!(PhysicalDeviceMaintenance5FeaturesKhr, maintenance5),
            pointer_size * 2
        );
        assert_eq!(
            std::mem::size_of::<PhysicalDeviceMaintenance5PropertiesKhr>(),
            if pointer_size == 8 { 40 } else { 32 }
        );
        assert_eq!(
            std::mem::offset_of!(
                PhysicalDeviceMaintenance5PropertiesKhr,
                depth_stencil_swizzle_one_support
            ),
            pointer_size * 2 + 2 * std::mem::size_of::<vk::Bool32>()
        );
    }

    #[test]
    fn robustness2_enables_only_null_descriptors() {
        let mut features = vk::PhysicalDeviceRobustness2FeaturesEXT {
            robust_buffer_access2: vk::TRUE,
            robust_image_access2: vk::TRUE,
            null_descriptor: vk::TRUE,
            ..Default::default()
        };

        assert!(configure_robustness2_features(&mut features, true));
        assert_eq!(features.robust_buffer_access2, vk::FALSE);
        assert_eq!(features.robust_image_access2, vk::FALSE);
        assert_eq!(features.null_descriptor, vk::TRUE);
        assert!(!configure_robustness2_features(&mut features, false));
    }

    #[test]
    fn tiler_driver_set_matches_upstream() {
        for driver in [
            vk::DriverId::QUALCOMM_PROPRIETARY,
            vk::DriverId::ARM_PROPRIETARY,
            vk::DriverId::SAMSUNG_PROPRIETARY,
            vk::DriverId::MESA_TURNIP,
        ] {
            assert!(is_tiler_driver(driver), "{driver:?}");
        }
        assert!(!is_tiler_driver(vk::DriverId::NVIDIA_PROPRIETARY));
        assert!(!is_tiler_driver(vk::DriverId::MESA_RADV));
    }

    #[test]
    fn sampler_filter_minmax_matches_upstream_amd_blacklist() {
        assert!(!sampler_filter_minmax_supported(false, false, true));
        assert!(sampler_filter_minmax_supported(true, false, false));
        assert!(!sampler_filter_minmax_supported(true, true, false));
        assert!(sampler_filter_minmax_supported(true, true, true));
    }

    #[test]
    fn loaded_extension_set_matches_upstream_core_promotion_rules() {
        let supported = [
            "VK_EXT_custom_border_color",
            "VK_EXT_4444_formats",
            "VK_EXT_index_type_uint8",
            "VK_EXT_primitive_topology_list_restart",
            "VK_EXT_descriptor_indexing",
            "VK_EXT_host_query_reset",
            "VK_KHR_8bit_storage",
            "VK_KHR_timeline_semaphore",
            "VK_EXT_shader_demote_to_helper_invocation",
            "VK_EXT_subgroup_size_control",
            "VK_KHR_maintenance5",
            "VK_KHR_swapchain",
            "VK_EXT_filter_cubic",
            "VK_IMG_filter_cubic",
            "VK_QCOM_filter_cubic_weights",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();

        let vulkan_11 = initial_loaded_extensions(vk::API_VERSION_1_1, &supported, false);
        assert!(vulkan_11.contains("VK_EXT_host_query_reset"));
        assert!(vulkan_11.contains("VK_EXT_descriptor_indexing"));
        assert!(vulkan_11.contains("VK_KHR_8bit_storage"));
        assert!(vulkan_11.contains("VK_KHR_timeline_semaphore"));
        assert!(vulkan_11.contains("VK_EXT_shader_demote_to_helper_invocation"));
        assert!(vulkan_11.contains("VK_EXT_subgroup_size_control"));
        assert!(vulkan_11.contains("VK_KHR_maintenance5"));
        assert!(vulkan_11.contains("VK_EXT_filter_cubic"));
        assert!(vulkan_11.contains("VK_IMG_filter_cubic"));
        assert!(vulkan_11.contains("VK_QCOM_filter_cubic_weights"));

        let vulkan_13 = initial_loaded_extensions(vk::API_VERSION_1_3, &supported, false);
        assert!(!vulkan_13.contains("VK_EXT_host_query_reset"));
        assert!(!vulkan_13.contains("VK_EXT_descriptor_indexing"));
        assert!(!vulkan_13.contains("VK_KHR_8bit_storage"));
        assert!(!vulkan_13.contains("VK_KHR_timeline_semaphore"));
        assert!(!vulkan_13.contains("VK_EXT_shader_demote_to_helper_invocation"));
        assert!(!vulkan_13.contains("VK_EXT_subgroup_size_control"));
        assert!(vulkan_13.contains("VK_KHR_maintenance5"));
        assert!(vulkan_13.contains("VK_EXT_custom_border_color"));
        assert!(vulkan_13.contains("VK_EXT_4444_formats"));
        assert!(vulkan_13.contains("VK_EXT_index_type_uint8"));
        assert!(vulkan_13.contains("VK_EXT_primitive_topology_list_restart"));
        assert!(vulkan_13.contains("VK_KHR_swapchain"));
    }

    fn suitable_device_inputs() -> (
        BTreeSet<String>,
        vk::PhysicalDeviceFeatures,
        vk::PhysicalDevice16BitStorageFeatures,
        vk::PhysicalDevice8BitStorageFeatures,
        vk::PhysicalDeviceHostQueryResetFeatures,
        vk::PhysicalDeviceShaderDemoteToHelperInvocationFeatures,
        vk::PhysicalDeviceShaderDrawParametersFeatures,
        vk::PhysicalDeviceVariablePointersFeatures,
        vk::PhysicalDeviceLimits,
    ) {
        let extensions = [
            "VK_EXT_vertex_attribute_divisor",
            "VK_KHR_driver_properties",
            "VK_KHR_sampler_mirror_clamp_to_edge",
            "VK_KHR_shader_float_controls",
            "VK_KHR_swapchain",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let features = vk::PhysicalDeviceFeatures {
            depth_bias_clamp: vk::TRUE,
            depth_clamp: vk::TRUE,
            draw_indirect_first_instance: vk::TRUE,
            dual_src_blend: vk::TRUE,
            fragment_stores_and_atomics: vk::TRUE,
            image_cube_array: vk::TRUE,
            independent_blend: vk::TRUE,
            logic_op: vk::TRUE,
            multi_draw_indirect: vk::TRUE,
            multi_viewport: vk::TRUE,
            occlusion_query_precise: vk::TRUE,
            robust_buffer_access: vk::TRUE,
            sampler_anisotropy: vk::TRUE,
            sample_rate_shading: vk::TRUE,
            shader_clip_distance: vk::TRUE,
            shader_image_gather_extended: vk::TRUE,
            shader_storage_image_write_without_format: vk::TRUE,
            vertex_pipeline_stores_and_atomics: vk::TRUE,
            ..Default::default()
        };
        let storage_16bit = vk::PhysicalDevice16BitStorageFeatures {
            storage_buffer16_bit_access: vk::TRUE,
            uniform_and_storage_buffer16_bit_access: vk::TRUE,
            ..Default::default()
        };
        let storage_8bit = vk::PhysicalDevice8BitStorageFeatures {
            storage_buffer8_bit_access: vk::TRUE,
            uniform_and_storage_buffer8_bit_access: vk::TRUE,
            ..Default::default()
        };
        let host_query_reset = vk::PhysicalDeviceHostQueryResetFeatures {
            host_query_reset: vk::TRUE,
            ..Default::default()
        };
        let shader_demote = vk::PhysicalDeviceShaderDemoteToHelperInvocationFeatures {
            shader_demote_to_helper_invocation: vk::TRUE,
            ..Default::default()
        };
        let shader_draw_parameters = vk::PhysicalDeviceShaderDrawParametersFeatures {
            shader_draw_parameters: vk::TRUE,
            ..Default::default()
        };
        let variable_pointers = vk::PhysicalDeviceVariablePointersFeatures {
            variable_pointers: vk::TRUE,
            variable_pointers_storage_buffer: vk::TRUE,
            ..Default::default()
        };
        let limits = vk::PhysicalDeviceLimits {
            max_uniform_buffer_range: 65_536,
            max_viewports: 16,
            max_color_attachments: 8,
            max_clip_distances: 8,
            ..Default::default()
        };
        (
            extensions,
            features,
            storage_16bit,
            storage_8bit,
            host_query_reset,
            shader_demote,
            shader_draw_parameters,
            variable_pointers,
            limits,
        )
    }

    #[test]
    fn suitability_checks_upstream_mandatory_limits_and_extensions() {
        let (
            mut extensions,
            features,
            storage_16bit,
            storage_8bit,
            host_query_reset,
            shader_demote,
            shader_draw_parameters,
            variable_pointers,
            mut limits,
        ) = suitable_device_inputs();
        let check = |extensions: &BTreeSet<String>, limits: &vk::PhysicalDeviceLimits| {
            device_is_suitable(
                vk::API_VERSION_1_3,
                true,
                extensions,
                &features,
                &storage_16bit,
                &storage_8bit,
                &host_query_reset,
                &shader_demote,
                &shader_draw_parameters,
                &variable_pointers,
                limits,
            )
        };
        assert!(check(&extensions, &limits));
        limits.max_viewports = 15;
        assert!(!check(&extensions, &limits));
        limits.max_viewports = 16;
        extensions.remove("VK_KHR_shader_float_controls");
        assert!(!check(&extensions, &limits));
    }

    #[test]
    fn descriptor_indexing_extension_is_mandatory_before_vulkan_1_2() {
        let (
            mut extensions,
            features,
            storage_16bit,
            storage_8bit,
            host_query_reset,
            shader_demote,
            shader_draw_parameters,
            variable_pointers,
            limits,
        ) = suitable_device_inputs();
        extensions.insert("VK_EXT_descriptor_indexing".to_string());
        assert!(device_is_suitable(
            vk::API_VERSION_1_1,
            true,
            &extensions,
            &features,
            &storage_16bit,
            &storage_8bit,
            &host_query_reset,
            &shader_demote,
            &shader_draw_parameters,
            &variable_pointers,
            &limits,
        ));
        extensions.remove("VK_EXT_descriptor_indexing");
        assert!(!device_is_suitable(
            vk::API_VERSION_1_1,
            true,
            &extensions,
            &features,
            &storage_16bit,
            &storage_8bit,
            &host_query_reset,
            &shader_demote,
            &shader_draw_parameters,
            &variable_pointers,
            &limits,
        ));
    }

    #[test]
    fn depth_bias_control_payload_matches_vulkan_abi() {
        let (expected_size, expected_alignment) = if cfg!(target_pointer_width = "64") {
            (32, 8)
        } else {
            (24, 4)
        };
        assert_eq!(
            std::mem::size_of::<PhysicalDeviceDepthBiasControlFeaturesExt>(),
            expected_size
        );
        assert_eq!(
            std::mem::align_of::<PhysicalDeviceDepthBiasControlFeaturesExt>(),
            expected_alignment
        );
        assert_eq!(
            PhysicalDeviceDepthBiasControlFeaturesExt::default()
                .s_type
                .as_raw(),
            1_000_283_000
        );
    }

    #[test]
    fn format_alternatives_include_all_upstream_switch_cases() {
        assert_eq!(
            alternatives::get_format_alternatives(vk::Format::X8_D24_UNORM_PACK32),
            Some(alternatives::DEPTH24_UNORM_DONTCARE8)
        );
        assert_eq!(
            alternatives::get_format_alternatives(vk::Format::R4G4_UNORM_PACK8),
            Some(alternatives::R4G4_UNORM_PACK8)
        );
    }

    #[test]
    fn collect_physical_memory_info_sums_device_local_heaps_for_discrete_gpus() {
        let mut properties = vk::PhysicalDeviceMemoryProperties::default();
        properties.memory_heap_count = 3;
        properties.memory_heaps[0].size = 2 * 1024 * 1024;
        properties.memory_heaps[0].flags = vk::MemoryHeapFlags::DEVICE_LOCAL;
        properties.memory_heaps[1].size = 4 * 1024 * 1024;
        properties.memory_heaps[1].flags = vk::MemoryHeapFlags::empty();
        properties.memory_heaps[2].size = 8 * 1024 * 1024;
        properties.memory_heaps[2].flags = vk::MemoryHeapFlags::DEVICE_LOCAL;

        let (memory, heaps) = collect_physical_memory_info(&properties, None, false);

        assert_eq!(memory, 10 * 1024 * 1024 - (10 * 1024 * 1024 / 8));
        assert_eq!(heaps, vec![0, 2]);
    }

    #[test]
    fn collect_physical_memory_info_caps_discrete_gpu_memory_like_upstream() {
        let mut properties = vk::PhysicalDeviceMemoryProperties::default();
        properties.memory_heap_count = 1;
        properties.memory_heaps[0].size = 16 * ONE_GIB;
        properties.memory_heaps[0].flags = vk::MemoryHeapFlags::DEVICE_LOCAL;

        let (memory, heaps) = collect_physical_memory_info(&properties, None, false);

        assert_eq!(memory, 7 * ONE_GIB);
        assert_eq!(heaps, vec![0]);
    }

    #[test]
    fn collect_physical_memory_info_uses_budget_when_available() {
        let mut properties = vk::PhysicalDeviceMemoryProperties::default();
        properties.memory_heap_count = 1;
        properties.memory_heaps[0].size = 16 * ONE_GIB;
        properties.memory_heaps[0].flags = vk::MemoryHeapFlags::DEVICE_LOCAL;
        let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
        budget.heap_budget[0] = 8 * ONE_GIB;
        budget.heap_usage[0] = ONE_GIB;

        let (memory, heaps) = collect_physical_memory_info(&properties, Some(&budget), false);

        assert_eq!(memory, 7 * ONE_GIB);
        assert_eq!(heaps, vec![0]);
    }

    #[test]
    fn integrated_memory_limit_matches_vram_usage_mode() {
        assert_eq!(
            integrated_memory_size(common::settings_enums::VramUsageMode::Conservative),
            4 * ONE_GIB
        );
        assert_eq!(
            integrated_memory_size(common::settings_enums::VramUsageMode::Aggressive),
            6 * ONE_GIB
        );
    }

    #[test]
    fn device_memory_usage_from_budget_sums_valid_heaps() {
        let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
        budget.heap_usage[0] = ONE_GIB;
        budget.heap_usage[1] = 2 * ONE_GIB;
        budget.heap_usage[2] = 4 * ONE_GIB;

        assert_eq!(
            device_memory_usage_from_budget(&budget, &[0, 2]),
            5 * ONE_GIB
        );
    }

    #[test]
    fn driver_name_from_id_matches_upstream_names() {
        assert_eq!(
            driver_name_from_id(vk::DriverId::AMD_PROPRIETARY),
            Some("AMD")
        );
        assert_eq!(driver_name_from_id(vk::DriverId::MESA_RADV), Some("RADV"));
        assert_eq!(
            driver_name_from_id(vk::DriverId::NVIDIA_PROPRIETARY),
            Some("NVIDIA")
        );
        assert_eq!(
            driver_name_from_id(vk::DriverId::MOLTENVK),
            Some("MoltenVK")
        );
        assert_eq!(driver_name_from_id(vk::DriverId::MESA_NVK), Some("NVK"));
        assert_eq!(driver_name_from_id(vk::DriverId::from_raw(-1)), None);
    }

    #[test]
    fn moltenvk_vertex_input_limits_match_upstream_quirk() {
        let mut limits = vk::PhysicalDeviceLimits::default();
        limits.max_vertex_input_attributes = 31;
        limits.max_vertex_input_bindings = 31;

        cap_moltenvk_vertex_input_limits(&mut limits);

        assert_eq!(limits.max_vertex_input_attributes, 16);
        assert_eq!(limits.max_vertex_input_bindings, 16);
    }

    #[test]
    fn qualcomm_sampler_heap_budget_matches_upstream_reservation() {
        assert_eq!(derive_sampler_heap_budget(0), 0);
        assert_eq!(derive_sampler_heap_budget(4096), 3072);
        assert_eq!(derive_sampler_heap_budget(512), 1024);
    }

    #[test]
    fn feature_chain_rejects_duplicate_structure_types() {
        let mut duplicate = vk::PhysicalDeviceTimelineSemaphoreFeatures::default();
        let mut timeline = vk::PhysicalDeviceTimelineSemaphoreFeatures::default();
        timeline.p_next =
            (&mut duplicate as *mut vk::PhysicalDeviceTimelineSemaphoreFeatures).cast();

        assert!(!pnext_chain_has_unique_structure_types(
            (&timeline as *const vk::PhysicalDeviceTimelineSemaphoreFeatures).cast()
        ));

        let mut float16_int8 = vk::PhysicalDeviceShaderFloat16Int8Features::default();
        timeline.p_next =
            (&mut float16_int8 as *mut vk::PhysicalDeviceShaderFloat16Int8Features).cast();

        assert!(pnext_chain_has_unique_structure_types(
            (&timeline as *const vk::PhysicalDeviceTimelineSemaphoreFeatures).cast()
        ));
    }

    #[test]
    fn descriptor_indexing_policy_matches_upstream_enabled_subset() {
        let mut features = vk::PhysicalDeviceDescriptorIndexingFeatures {
            shader_uniform_texel_buffer_array_dynamic_indexing: vk::TRUE,
            shader_storage_texel_buffer_array_dynamic_indexing: vk::TRUE,
            shader_sampled_image_array_non_uniform_indexing: vk::TRUE,
            shader_storage_image_array_non_uniform_indexing: vk::TRUE,
            shader_uniform_texel_buffer_array_non_uniform_indexing: vk::TRUE,
            shader_storage_texel_buffer_array_non_uniform_indexing: vk::TRUE,
            descriptor_binding_partially_bound: vk::TRUE,
            runtime_descriptor_array: vk::TRUE,
            descriptor_binding_sampled_image_update_after_bind: vk::TRUE,
            ..Default::default()
        };

        apply_descriptor_indexing_policy(&mut features);

        assert_eq!(features.runtime_descriptor_array, vk::FALSE);
        assert_eq!(
            features.descriptor_binding_sampled_image_update_after_bind,
            vk::FALSE
        );
        assert_eq!(features.descriptor_binding_partially_bound, vk::TRUE);
        assert_eq!(
            features.shader_uniform_texel_buffer_array_dynamic_indexing,
            vk::FALSE
        );
        assert_eq!(
            features.shader_storage_texel_buffer_array_dynamic_indexing,
            vk::FALSE
        );
        assert_eq!(
            features.shader_sampled_image_array_non_uniform_indexing,
            vk::TRUE
        );
        assert_eq!(
            features.shader_storage_image_array_non_uniform_indexing,
            vk::TRUE
        );
        assert_eq!(
            features.shader_uniform_texel_buffer_array_non_uniform_indexing,
            vk::TRUE
        );
        assert_eq!(
            features.shader_storage_texel_buffer_array_non_uniform_indexing,
            vk::TRUE
        );
    }
}

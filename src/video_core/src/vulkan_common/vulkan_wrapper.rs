// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `video_core/vulkan_common/vulkan_wrapper.h` and
//! `video_core/vulkan_common/vulkan_wrapper.cpp`.
//!
//! Provides RAII wrappers and dispatch tables for Vulkan objects.
//! In the C++ codebase this is a large custom Vulkan abstraction layer with
//! owning handles, dispatch tables (InstanceDispatch / DeviceDispatch),
//! and utility free functions. In Rust, ash provides most of this natively.
//!
//! This module re-exports ash types and provides thin compatibility shims
//! so the rest of the port can use names similar to the C++ `vk::` namespace.

use ash::vk;
use ash::vk::Handle;
use std::ffi::{CStr, CString};

use super::vk_enum_string_helper::string_vk_result;

// ---------------------------------------------------------------------------
// Exception / error type — port of `vk::Exception`
// ---------------------------------------------------------------------------

/// Vulkan error generated from a `VkResult`.
///
/// Port of `vk::Exception`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VulkanError {
    pub result: vk::Result,
}

impl VulkanError {
    pub fn new(result: vk::Result) -> Self {
        Self { result }
    }
}

impl std::fmt::Display for VulkanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&string_vk_result(self.result))
    }
}

impl std::error::Error for VulkanError {}

/// Port of `vk::Check` — returns `Err` on non-success results.
pub fn check(result: vk::Result) -> Result<(), VulkanError> {
    if result != vk::Result::SUCCESS {
        Err(VulkanError::new(result))
    } else {
        Ok(())
    }
}

fn set_object_name_with(
    set_name: Option<vk::PFN_vkSetDebugUtilsObjectNameEXT>,
    device: vk::Device,
    object_type: vk::ObjectType,
    object_handle: u64,
    name: &CStr,
) -> Result<(), VulkanError> {
    let Some(set_name) = set_name else {
        return Ok(());
    };
    let name_info = vk::DebugUtilsObjectNameInfoEXT::builder()
        .object_type(object_type)
        .object_handle(object_handle)
        .object_name(name);
    check(unsafe { set_name(device, &*name_info) })
}

/// Rust counterpart of upstream's file-local `SetObjectName` helper.
pub(crate) fn set_object_name(
    instance: &ash::Instance,
    device: &ash::Device,
    object_type: vk::ObjectType,
    object_handle: u64,
    name: &str,
) -> Result<(), VulkanError> {
    let Ok(name) = CString::new(name) else {
        return Ok(());
    };
    let function_name =
        unsafe { CStr::from_bytes_with_nul_unchecked(b"vkSetDebugUtilsObjectNameEXT\0") };
    let set_name =
        unsafe { instance.get_device_proc_addr(device.handle(), function_name.as_ptr()) }.map(
            |function| unsafe {
                std::mem::transmute::<
                    unsafe extern "system" fn(),
                    vk::PFN_vkSetDebugUtilsObjectNameEXT,
                >(function)
            },
        );
    set_object_name_with(set_name, device.handle(), object_type, object_handle, &name)
}

/// Port of `vk::Framebuffer::SetObjectNameEXT`.
pub fn set_framebuffer_name(
    instance: &ash::Instance,
    device: &ash::Device,
    framebuffer: vk::Framebuffer,
    name: &str,
) -> Result<(), VulkanError> {
    set_object_name(
        instance,
        device,
        vk::ObjectType::FRAMEBUFFER,
        framebuffer.as_raw(),
        name,
    )
}

/// Port of `vk::Filter` — returns `Err` only on error results (negative).
pub fn filter(result: vk::Result) -> Result<vk::Result, VulkanError> {
    if result.as_raw() < 0 {
        Err(VulkanError::new(result))
    } else {
        Ok(result)
    }
}

/// Pipeline-stage groups from upstream's `vk` namespace.
pub const PIPELINE_STAGE_GRAPHICS_COMPUTE: vk::PipelineStageFlags =
    vk::PipelineStageFlags::from_raw(
        vk::PipelineStageFlags::ALL_GRAPHICS.as_raw()
            | vk::PipelineStageFlags::COMPUTE_SHADER.as_raw(),
    );
pub const PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER: vk::PipelineStageFlags =
    vk::PipelineStageFlags::from_raw(
        PIPELINE_STAGE_GRAPHICS_COMPUTE.as_raw() | vk::PipelineStageFlags::TRANSFER.as_raw(),
    );
pub const PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER_HOST: vk::PipelineStageFlags =
    vk::PipelineStageFlags::from_raw(
        PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER.as_raw() | vk::PipelineStageFlags::HOST.as_raw(),
    );
pub const PIPELINE_STAGE_HOST: vk::PipelineStageFlags = vk::PipelineStageFlags::HOST;

// ---------------------------------------------------------------------------
// Vendor IDs — used in physical device sorting
// ---------------------------------------------------------------------------

/// Nvidia vendor ID.
pub const VENDOR_ID_NVIDIA: u32 = 0x10DE;
/// AMD vendor ID.
pub const VENDOR_ID_AMD: u32 = 0x1002;
/// Intel vendor ID.
pub const VENDOR_ID_INTEL: u32 = 0x8086;

// ---------------------------------------------------------------------------
// Physical device sorting — port of anonymous-namespace helpers in vulkan_wrapper.cpp
// ---------------------------------------------------------------------------

/// Returns true if the device name contains "Microsoft" (Dozen driver).
fn is_microsoft_dozen(device_name: &str) -> bool {
    device_name.contains("Microsoft")
}

/// Sorts physical devices by preference.
///
/// Port of `SortPhysicalDevices` from `vulkan_wrapper.cpp`.
/// Preference order:
/// 1. Demote Microsoft Dozen devices
/// 2. Prefer Nvidia > AMD > Intel
/// 3. Prefer discrete GPUs
/// 4. Sort by name descending (higher model numbers first)
pub fn sort_physical_devices(devices: &mut Vec<vk::PhysicalDevice>, instance: &ash::Instance) {
    // We need properties for sorting. Collect them once.
    let get_props = |dev: vk::PhysicalDevice| -> vk::PhysicalDeviceProperties {
        unsafe { instance.get_physical_device_properties(dev) }
    };

    // Sort by name descending
    devices.sort_by(|&a, &b| {
        let name_a = unsafe { CStr::from_ptr(get_props(a).device_name.as_ptr()).to_string_lossy() };
        let name_b = unsafe { CStr::from_ptr(get_props(b).device_name.as_ptr()).to_string_lossy() };
        name_b.cmp(&name_a)
    });

    // Prefer discrete over non-discrete
    devices.sort_by(|&a, &b| {
        let a_discrete = get_props(a).device_type == vk::PhysicalDeviceType::DISCRETE_GPU;
        let b_discrete = get_props(b).device_type == vk::PhysicalDeviceType::DISCRETE_GPU;
        b_discrete.cmp(&a_discrete)
    });

    // Prefer Nvidia > AMD > Intel
    let vendor_priority = |vendor_id: u32| -> u32 {
        match vendor_id {
            VENDOR_ID_NVIDIA => 3,
            VENDOR_ID_AMD => 2,
            VENDOR_ID_INTEL => 1,
            _ => 0,
        }
    };
    devices.sort_by(|&a, &b| {
        let pa = vendor_priority(get_props(a).vendor_id);
        let pb = vendor_priority(get_props(b).vendor_id);
        pb.cmp(&pa)
    });

    // Demote Microsoft Dozen devices
    devices.sort_by(|&a, &b| {
        let name_a = unsafe { CStr::from_ptr(get_props(a).device_name.as_ptr()).to_string_lossy() };
        let name_b = unsafe { CStr::from_ptr(get_props(b).device_name.as_ptr()).to_string_lossy() };
        let a_dozen = is_microsoft_dozen(&name_a);
        let b_dozen = is_microsoft_dozen(&name_b);
        a_dozen.cmp(&b_dozen)
    });
}

fn enumerate_physical_device_tool_properties(
    get_properties: Option<vk::PFN_vkGetPhysicalDeviceToolProperties>,
    physical_device: vk::PhysicalDevice,
) -> Vec<vk::PhysicalDeviceToolProperties> {
    let Some(get_properties) = get_properties else {
        return Vec::new();
    };

    let mut count = 0;
    let _ = unsafe { get_properties(physical_device, &mut count, std::ptr::null_mut()) };
    let mut properties = vec![vk::PhysicalDeviceToolProperties::default(); count as usize];
    let _ = unsafe { get_properties(physical_device, &mut count, properties.as_mut_ptr()) };
    properties
}

/// Port of `PhysicalDevice::GetPhysicalDeviceToolProperties`.
///
/// Upstream deliberately loads the promoted core symbol as optional. Some
/// drivers advertise `VK_EXT_tooling_info` without exporting the suffixed EXT
/// command, so using ash's extension loader would install a panicking fallback.
pub fn get_physical_device_tool_properties(
    entry: &ash::Entry,
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
) -> Vec<vk::PhysicalDeviceToolProperties> {
    let name =
        unsafe { CStr::from_bytes_with_nul_unchecked(b"vkGetPhysicalDeviceToolProperties\0") };
    let function = unsafe { entry.get_instance_proc_addr(instance.handle(), name.as_ptr()) }.map(
        |function| unsafe {
            std::mem::transmute::<
                unsafe extern "system" fn(),
                vk::PFN_vkGetPhysicalDeviceToolProperties,
            >(function)
        },
    );
    enumerate_physical_device_tool_properties(function, physical_device)
}

// ---------------------------------------------------------------------------
// Instance wrapper — thin wrapper around ash::Instance
// ---------------------------------------------------------------------------

fn make_application_info(application_name: &CStr) -> vk::ApplicationInfo {
    vk::ApplicationInfo::builder()
        .application_name(application_name)
        .application_version(vk::make_api_version(0, 1, 3, 0))
        .engine_name(application_name)
        .engine_version(vk::make_api_version(0, 1, 3, 0))
        .api_version(vk::API_VERSION_1_3)
        .build()
}

/// RAII wrapper around an `ash::Instance`.
///
/// Port of `vk::Instance` from `vulkan_wrapper.h`.
/// Owns the Vulkan instance and its entry point, destroying the instance on drop.
pub struct Instance {
    pub entry: ash::Entry,
    pub instance: ash::Instance,
}

impl Instance {
    /// Creates a Vulkan instance.
    ///
    /// Port of `vk::Instance::Create`.
    pub fn create(
        entry: ash::Entry,
        version: u32,
        layers: &[*const std::os::raw::c_char],
        extensions: &[*const std::os::raw::c_char],
    ) -> Result<Self, VulkanError> {
        let _ = version;
        let application_name = CString::new("ruzu Emulator").unwrap();
        let application_info = make_application_info(&application_name);
        #[cfg(target_os = "macos")]
        let flags = vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR;
        #[cfg(not(target_os = "macos"))]
        let flags = vk::InstanceCreateFlags::empty();
        let create_info = vk::InstanceCreateInfo::builder()
            .application_info(&application_info)
            .enabled_layer_names(layers)
            .enabled_extension_names(extensions)
            .flags(flags)
            .build();

        let instance = unsafe {
            entry
                .create_instance(&create_info, None)
                .map_err(|e| VulkanError::new(e))?
        };
        let destroy_name = unsafe { CStr::from_bytes_with_nul_unchecked(b"vkDestroyInstance\0") };
        if unsafe { entry.get_instance_proc_addr(instance.handle(), destroy_name.as_ptr()) }
            .is_none()
        {
            return Err(VulkanError::new(vk::Result::ERROR_INITIALIZATION_FAILED));
        }

        Ok(Self { entry, instance })
    }

    /// Enumerates physical devices, sorted by preference.
    ///
    /// Port of `Instance::EnumeratePhysicalDevices`.
    pub fn enumerate_physical_devices(&self) -> Result<Vec<vk::PhysicalDevice>, VulkanError> {
        let mut devices = unsafe {
            self.instance
                .enumerate_physical_devices()
                .map_err(|e| VulkanError::new(e))?
        };
        sort_physical_devices(&mut devices, &self.instance);
        Ok(devices)
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            self.instance.destroy_instance(None);
        }
    }
}

// ---------------------------------------------------------------------------
// Device wrapper — thin wrapper around ash::Device
// ---------------------------------------------------------------------------

/// RAII wrapper around an `ash::Device`.
///
/// Port of `vk::Device` from `vulkan_wrapper.h`.
/// Owns the logical Vulkan device and destroys it on drop.
pub struct LogicalDevice {
    pub device: ash::Device,
}

impl LogicalDevice {
    /// Creates a logical device.
    ///
    /// Port of `vk::Device::Create`.
    pub fn create(
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
        create_info: &vk::DeviceCreateInfo,
    ) -> Result<Self, VulkanError> {
        let device = unsafe {
            instance
                .create_device(physical_device, create_info, None)
                .map_err(|e| VulkanError::new(e))?
        };
        Ok(Self { device })
    }

    /// Returns a queue from the device.
    ///
    /// Port of `Device::GetQueue`.
    pub fn get_queue(&self, family_index: u32) -> vk::Queue {
        unsafe { self.device.get_device_queue(family_index, 0) }
    }

    /// Waits for the device to be idle.
    pub fn wait_idle(&self) -> Result<(), VulkanError> {
        unsafe {
            self.device
                .device_wait_idle()
                .map_err(|e| VulkanError::new(e))
        }
    }
}

impl Drop for LogicalDevice {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_device(None);
        }
    }
}

// ---------------------------------------------------------------------------
// Available version query — port of `vk::AvailableVersion`
// ---------------------------------------------------------------------------

/// Queries the available Vulkan API version.
///
/// Port of `vk::AvailableVersion` from `vulkan_wrapper.cpp`.
pub fn available_version(entry: &ash::Entry) -> u32 {
    match entry.try_enumerate_instance_version() {
        Ok(Some(version)) => version,
        Ok(None) => vk::API_VERSION_1_0,
        Err(e) => {
            log::error!(
                "vkEnumerateInstanceVersion returned {}, assuming Vulkan 1.1",
                string_vk_result(e)
            );
            vk::API_VERSION_1_1
        }
    }
}

// ---------------------------------------------------------------------------
// Extension / layer enumeration helpers
// ---------------------------------------------------------------------------

/// Enumerates instance extension properties.
///
/// Port of `vk::EnumerateInstanceExtensionProperties`.
pub fn enumerate_instance_extension_properties(
    entry: &ash::Entry,
) -> Option<Vec<vk::ExtensionProperties>> {
    entry.enumerate_instance_extension_properties(None).ok()
}

/// Enumerates instance layer properties.
///
/// Port of `vk::EnumerateInstanceLayerProperties`.
pub fn enumerate_instance_layer_properties(entry: &ash::Entry) -> Option<Vec<vk::LayerProperties>> {
    entry.enumerate_instance_layer_properties().ok()
}

/// Port of `vk::GetDriverName`.
pub fn get_driver_name(driver: &vk::PhysicalDeviceDriverProperties) -> String {
    const MESA_HONEYKRISP: vk::DriverId = vk::DriverId::from_raw(26);
    const MESA_KOSMICKRISP: vk::DriverId = vk::DriverId::from_raw(28);
    let known_name = match driver.driver_id {
        vk::DriverId::AMD_PROPRIETARY => Some("AMD"),
        vk::DriverId::AMD_OPEN_SOURCE => Some("AMDVLK"),
        vk::DriverId::MESA_RADV => Some("RADV"),
        vk::DriverId::NVIDIA_PROPRIETARY => Some("Nvidia"),
        vk::DriverId::INTEL_PROPRIETARY_WINDOWS => Some("Intel"),
        vk::DriverId::INTEL_OPEN_SOURCE_MESA => Some("ANV"),
        vk::DriverId::IMAGINATION_PROPRIETARY => Some("PowerVR"),
        vk::DriverId::QUALCOMM_PROPRIETARY => Some("Qualcomm"),
        vk::DriverId::ARM_PROPRIETARY => Some("Mali"),
        vk::DriverId::GOOGLE_SWIFTSHADER => Some("SwiftShader"),
        vk::DriverId::BROADCOM_PROPRIETARY => Some("Broadcom"),
        vk::DriverId::MESA_LLVMPIPE => Some("llvmpipe"),
        vk::DriverId::MOLTENVK => Some("MoltenVK"),
        vk::DriverId::VERISILICON_PROPRIETARY => Some("Vivante"),
        vk::DriverId::MESA_TURNIP => Some("Turnip"),
        vk::DriverId::MESA_V3DV => Some("V3DV"),
        vk::DriverId::MESA_PANVK => Some("PanVK"),
        vk::DriverId::SAMSUNG_PROPRIETARY => Some("Xclipse"),
        vk::DriverId::MESA_VENUS => Some("Venus"),
        vk::DriverId::MESA_DOZEN => Some("Dozen"),
        vk::DriverId::MESA_NVK => Some("NVK"),
        vk::DriverId::IMAGINATION_OPEN_SOURCE_MESA => Some("PVR"),
        MESA_HONEYKRISP => Some("HoneyKrisp"),
        MESA_KOSMICKRISP => Some("KosmicKrisp"),
        _ => None,
    };
    known_name.map(str::to_owned).unwrap_or_else(|| {
        unsafe { CStr::from_ptr(driver.driver_name.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    })
}

// ---------------------------------------------------------------------------
// Re-exports for convenience — these map to C++ `vk::` namespace types
// ---------------------------------------------------------------------------

// The C++ codebase defines many RAII handle types (Image, Buffer, BufferView,
// Fence, Semaphore, etc.) with custom Drop. In Rust with ash, these are
// managed differently: ash provides raw handles and the user manages lifetime.
//
// For now we provide type aliases; full RAII wrappers can be added as the
// renderer port progresses.

/// Type alias matching C++ `vk::Span<T>`.
/// In Rust, `&[T]` serves the same purpose.
pub type Span<'a, T> = &'a [T];

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TOOL_CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "system" fn one_tool(
        _physical_device: vk::PhysicalDevice,
        count: *mut u32,
        properties: *mut vk::PhysicalDeviceToolProperties,
    ) -> vk::Result {
        if properties.is_null() {
            unsafe { *count = 1 };
            return vk::Result::SUCCESS;
        }
        unsafe {
            *count = 1;
            (&mut (*properties).name)[..5].copy_from_slice(&[
                b't' as std::os::raw::c_char,
                b'o' as std::os::raw::c_char,
                b'o' as std::os::raw::c_char,
                b'l' as std::os::raw::c_char,
                0,
            ]);
        }
        vk::Result::SUCCESS
    }

    unsafe extern "system" fn incomplete_tools(
        _physical_device: vk::PhysicalDevice,
        count: *mut u32,
        properties: *mut vk::PhysicalDeviceToolProperties,
    ) -> vk::Result {
        TOOL_CALLS.fetch_add(1, Ordering::Relaxed);
        if properties.is_null() {
            unsafe { *count = 2 };
            return vk::Result::ERROR_UNKNOWN;
        }
        unsafe { *count = 1 };
        vk::Result::INCOMPLETE
    }

    #[test]
    fn test_vulkan_error_display() {
        let err = VulkanError::new(vk::Result::ERROR_INITIALIZATION_FAILED);
        assert_eq!(err.to_string(), "VK_ERROR_INITIALIZATION_FAILED");
    }

    #[test]
    fn test_check_success() {
        assert!(check(vk::Result::SUCCESS).is_ok());
    }

    #[test]
    fn test_check_failure() {
        assert!(check(vk::Result::ERROR_OUT_OF_HOST_MEMORY).is_err());
    }

    #[test]
    fn test_filter_success() {
        assert_eq!(filter(vk::Result::SUCCESS).unwrap(), vk::Result::SUCCESS);
    }

    #[test]
    fn test_filter_positive() {
        // VK_NOT_READY is positive (not an error)
        assert!(filter(vk::Result::NOT_READY).is_ok());
    }

    #[test]
    fn test_filter_error() {
        assert!(filter(vk::Result::ERROR_DEVICE_LOST).is_err());
    }

    #[test]
    fn pipeline_stage_groups_match_upstream() {
        assert_eq!(
            PIPELINE_STAGE_GRAPHICS_COMPUTE,
            vk::PipelineStageFlags::ALL_GRAPHICS | vk::PipelineStageFlags::COMPUTE_SHADER
        );
        assert_eq!(
            PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER,
            PIPELINE_STAGE_GRAPHICS_COMPUTE | vk::PipelineStageFlags::TRANSFER
        );
        assert_eq!(
            PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER_HOST,
            PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER | vk::PipelineStageFlags::HOST
        );
        assert_eq!(PIPELINE_STAGE_HOST, vk::PipelineStageFlags::HOST);
    }

    #[test]
    fn missing_tooling_info_symbol_returns_no_tools() {
        assert!(
            enumerate_physical_device_tool_properties(None, vk::PhysicalDevice::null()).is_empty()
        );
    }

    #[test]
    fn tooling_info_uses_optional_core_dispatch() {
        let tools =
            enumerate_physical_device_tool_properties(Some(one_tool), vk::PhysicalDevice::null());
        assert_eq!(tools.len(), 1);
        assert_eq!(
            unsafe { CStr::from_ptr(tools[0].name.as_ptr()) }.to_bytes(),
            b"tool"
        );
    }

    #[test]
    fn tooling_info_preserves_upstream_two_call_contract() {
        TOOL_CALLS.store(0, Ordering::Relaxed);
        let tools = enumerate_physical_device_tool_properties(
            Some(incomplete_tools),
            vk::PhysicalDevice::null(),
        );
        assert_eq!(TOOL_CALLS.load(Ordering::Relaxed), 2);
        assert_eq!(tools.len(), 2);
    }

    #[test]
    fn missing_debug_name_symbol_is_an_upstream_noop() {
        let name = CString::new("framebuffer").unwrap();
        assert!(set_object_name_with(
            None,
            vk::Device::null(),
            vk::ObjectType::FRAMEBUFFER,
            1,
            &name,
        )
        .is_ok());
    }

    #[test]
    fn driver_names_match_upstream_and_fall_back_to_the_reported_name() {
        let mut driver = vk::PhysicalDeviceDriverProperties::default();
        driver.driver_id = vk::DriverId::NVIDIA_PROPRIETARY;
        assert_eq!(get_driver_name(&driver), "Nvidia");
        driver.driver_id = vk::DriverId::MESA_LLVMPIPE;
        assert_eq!(get_driver_name(&driver), "llvmpipe");
        driver.driver_id = vk::DriverId::from_raw(26);
        assert_eq!(get_driver_name(&driver), "HoneyKrisp");
        driver.driver_id = vk::DriverId::from_raw(28);
        assert_eq!(get_driver_name(&driver), "KosmicKrisp");

        driver.driver_id = vk::DriverId::from_raw(-1);
        driver.driver_name[..9].copy_from_slice(&[
            b'f' as _, b'a' as _, b'l' as _, b'l' as _, b'b' as _, b'a' as _, b'c' as _, b'k' as _,
            0,
        ]);
        assert_eq!(get_driver_name(&driver), "fallback");
    }

    #[test]
    fn instance_application_versions_match_upstream() {
        let name = CString::new("ruzu Emulator").unwrap();
        let info = make_application_info(&name);
        assert_eq!(info.application_version, vk::make_api_version(0, 1, 3, 0));
        assert_eq!(info.engine_version, vk::make_api_version(0, 1, 3, 0));
        assert_eq!(info.api_version, vk::API_VERSION_1_3);
        assert_eq!(unsafe { CStr::from_ptr(info.p_application_name) }, &*name);
        assert_eq!(unsafe { CStr::from_ptr(info.p_engine_name) }, &*name);
    }
}

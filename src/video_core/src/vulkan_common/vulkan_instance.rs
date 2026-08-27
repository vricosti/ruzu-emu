// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `zuyu/src/video_core/vulkan_common/vulkan_instance.h` and
//! `zuyu/src/video_core/vulkan_common/vulkan_instance.cpp`.
//!
//! Creates a Vulkan instance with the required extensions and validation layers.

use ash::vk;
use std::ffi::{CStr, CString};

use super::vulkan_wrapper::{self, Instance, VulkanError};

// ---------------------------------------------------------------------------
// Window system type — port of `Core::Frontend::WindowSystemType`
// ---------------------------------------------------------------------------

/// Window system type, matching `Core::Frontend::WindowSystemType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowSystemType {
    Headless,
    #[cfg(target_os = "windows")]
    Windows,
    #[cfg(target_os = "macos")]
    Cocoa,
    #[cfg(target_os = "android")]
    Android,
    #[cfg(target_os = "haiku")]
    Xcb,
    #[cfg(target_os = "linux")]
    X11,
    #[cfg(target_os = "linux")]
    Wayland,
}

// ---------------------------------------------------------------------------
// Helper: check if extensions are supported
// ---------------------------------------------------------------------------

/// Port of `AreExtensionsSupported` from `vulkan_instance.cpp`.
fn are_extensions_supported(properties: &[vk::ExtensionProperties], required: &[CString]) -> bool {
    for ext in required {
        let found = properties.iter().any(|prop| {
            let name = unsafe { CStr::from_ptr(prop.extension_name.as_ptr()) };
            name == ext.as_c_str()
        });
        if !found {
            log::error!(
                "Required instance extension {} is not available",
                ext.to_string_lossy()
            );
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Helper: build required extensions list
// ---------------------------------------------------------------------------

/// Port of `RequiredExtensions` from `vulkan_instance.cpp`.
fn required_extensions(
    properties: &[vk::ExtensionProperties],
    window_type: WindowSystemType,
    enable_validation: bool,
) -> Vec<CString> {
    let mut extensions = Vec::with_capacity(6);

    match window_type {
        WindowSystemType::Headless => {}
        #[cfg(target_os = "windows")]
        WindowSystemType::Windows => {
            extensions.push(CString::new("VK_KHR_win32_surface").unwrap());
        }
        #[cfg(target_os = "macos")]
        WindowSystemType::Cocoa => {
            extensions.push(CString::new("VK_EXT_metal_surface").unwrap());
        }
        #[cfg(target_os = "android")]
        WindowSystemType::Android => {
            extensions.push(CString::new("VK_KHR_android_surface").unwrap());
        }
        #[cfg(target_os = "haiku")]
        WindowSystemType::Xcb => {
            extensions.push(CString::new("VK_KHR_xcb_surface").unwrap());
        }
        #[cfg(target_os = "linux")]
        WindowSystemType::X11 => {
            extensions.push(CString::new("VK_KHR_xlib_surface").unwrap());
        }
        #[cfg(target_os = "linux")]
        WindowSystemType::Wayland => {
            extensions.push(CString::new("VK_KHR_wayland_surface").unwrap());
        }
    }

    if window_type != WindowSystemType::Headless {
        extensions.push(CString::new("VK_KHR_surface").unwrap());
    }

    #[cfg(target_os = "macos")]
    {
        let portability = CString::new("VK_KHR_portability_enumeration").unwrap();
        if are_extensions_supported(properties, &[portability.clone()]) {
            extensions.push(portability);
        }
    }

    if enable_validation {
        let debug_utils = CString::new("VK_EXT_debug_utils").unwrap();
        if are_extensions_supported(properties, &[debug_utils.clone()]) {
            extensions.push(debug_utils);
        }
    }

    extensions
}

// ---------------------------------------------------------------------------
// Helper: build layers list
// ---------------------------------------------------------------------------

/// Port of `Layers` from `vulkan_instance.cpp`.
fn layers(enable_validation: bool) -> Vec<CString> {
    let mut layers = Vec::new();
    if enable_validation {
        layers.push(CString::new("VK_LAYER_KHRONOS_validation").unwrap());
    }
    layers
}

/// Port of `RemoveUnavailableLayers` from `vulkan_instance.cpp`.
fn remove_unavailable_layers(entry: &ash::Entry, layers: &mut Vec<CString>) {
    let layer_properties = match vulkan_wrapper::enumerate_instance_layer_properties(entry) {
        Some(props) => props,
        None => {
            log::error!("Failed to query layer properties, disabling layers");
            layers.clear();
            return;
        }
    };
    layers.retain(|layer| {
        let found = layer_properties.iter().any(|prop| {
            let name = unsafe { CStr::from_ptr(prop.layer_name.as_ptr()) };
            name == layer.as_c_str()
        });
        if !found {
            log::error!(
                "Layer {} not available, removing it",
                layer.to_string_lossy()
            );
        }
        found
    });
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Creates a Vulkan instance.
///
/// Port of `Vulkan::CreateInstance` from `vulkan_instance.cpp`.
///
/// # Parameters
/// - `entry`: The loaded Vulkan entry point.
/// - `required_version`: Minimum required Vulkan API version.
/// - `window_type`: The window system type for surface extensions.
/// - `enable_validation`: Whether to enable validation layers.
///
/// # Errors
/// Returns `VulkanError` on failure.
pub fn create_instance(
    entry: ash::Entry,
    required_version: u32,
    window_type: WindowSystemType,
    enable_validation: bool,
) -> Result<Instance, VulkanError> {
    // Keep a single extension snapshot for both optional-extension probing and
    // final validation. Some drivers have returned different sets across two
    // consecutive enumerations, which can otherwise reject a valid launch.
    let extension_properties = vulkan_wrapper::enumerate_instance_extension_properties(&entry)
        .ok_or_else(|| {
            log::error!("Failed to query instance extension properties");
            VulkanError::new(vk::Result::ERROR_EXTENSION_NOT_PRESENT)
        })?;
    let extensions = required_extensions(&extension_properties, window_type, enable_validation);
    if !are_extensions_supported(&extension_properties, &extensions) {
        return Err(VulkanError::new(vk::Result::ERROR_EXTENSION_NOT_PRESENT));
    }

    let mut layer_list = layers(enable_validation);
    remove_unavailable_layers(&entry, &mut layer_list);

    let available = vulkan_wrapper::available_version(&entry);
    if available < required_version {
        log::error!(
            "Vulkan {}.{} is not supported, {}.{} is required",
            vk::api_version_major(available),
            vk::api_version_minor(available),
            vk::api_version_major(required_version),
            vk::api_version_minor(required_version),
        );
        return Err(VulkanError::new(vk::Result::ERROR_INCOMPATIBLE_DRIVER));
    }

    let layer_ptrs: Vec<*const std::os::raw::c_char> =
        layer_list.iter().map(|s| s.as_ptr()).collect();
    let ext_ptrs: Vec<*const std::os::raw::c_char> =
        extensions.iter().map(|s| s.as_ptr()).collect();

    Instance::create(entry, available, &layer_ptrs, &ext_ptrs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extension_property(name: &CStr) -> vk::ExtensionProperties {
        let mut property = vk::ExtensionProperties::default();
        for (destination, source) in property
            .extension_name
            .iter_mut()
            .zip(name.to_bytes_with_nul())
        {
            *destination = *source as std::os::raw::c_char;
        }
        property
    }

    #[test]
    fn optional_and_final_extension_checks_share_the_supplied_snapshot() {
        let surface = CString::new("VK_KHR_surface").unwrap();
        let debug_utils = CString::new("VK_EXT_debug_utils").unwrap();
        let properties = [
            extension_property(&surface),
            extension_property(&debug_utils),
        ];

        let extensions = required_extensions(&properties, WindowSystemType::Headless, true);
        assert_eq!(extensions, vec![debug_utils]);
        assert!(are_extensions_supported(&properties, &extensions));
    }

    #[cfg(target_os = "haiku")]
    #[test]
    fn haiku_requests_xcb_and_common_surface_extensions() {
        let xcb = CString::new("VK_KHR_xcb_surface").unwrap();
        let surface = CString::new("VK_KHR_surface").unwrap();
        let properties = [extension_property(&xcb), extension_property(&surface)];

        let extensions = required_extensions(&properties, WindowSystemType::Xcb, false);
        assert_eq!(extensions, vec![xcb, surface]);
        assert!(are_extensions_supported(&properties, &extensions));
    }
}

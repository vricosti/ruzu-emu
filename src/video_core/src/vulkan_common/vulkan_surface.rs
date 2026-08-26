// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `zuyu/src/video_core/vulkan_common/vulkan_surface.h` and
//! `zuyu/src/video_core/vulkan_common/vulkan_surface.cpp`.
//!
//! Creates a Vulkan surface from platform-specific window system information.
//! The C++ code dispatches on `WindowSystemType` and calls platform-specific
//! surface creation functions (Win32, Xlib, Wayland, Metal, Android).
//! In Rust, we use `ash`'s platform-specific surface creation extensions.

use ash::vk;

use super::vulkan_instance::WindowSystemType;
use super::vulkan_wrapper::VulkanError;

/// Platform-specific window system info needed to create a surface.
///
/// Port of `Core::Frontend::EmuWindow::WindowSystemInfo`.
#[derive(Clone, Copy)]
pub struct WindowSystemInfo {
    pub window_type: WindowSystemType,
    /// Platform-specific display connection (e.g., `*mut xlib::Display` on X11,
    /// `*mut wl_display` on Wayland). Null for platforms that don't need it.
    pub display_connection: *mut std::ffi::c_void,
    /// Platform-specific render surface handle (e.g., `HWND`, `Window`, `*mut wl_surface`,
    /// `*mut ANativeWindow`, `*const CAMetalLayer`).
    pub render_surface: *mut std::ffi::c_void,
}

// SAFETY: The raw pointers are owned by the windowing system and are valid
// for the lifetime of the window. The caller must ensure this.
unsafe impl Send for WindowSystemInfo {}
unsafe impl Sync for WindowSystemInfo {}

fn surface_initialization_error() -> VulkanError {
    VulkanError::new(vk::Result::ERROR_INITIALIZATION_FAILED)
}

/// Creates a Vulkan surface from the given window system info.
///
/// Port of `Vulkan::CreateSurface` from `vulkan_surface.cpp`.
///
/// # Safety
/// The pointers in `window_info` must be valid for the target platform.
///
/// # Errors
/// Returns `VulkanError` on failure.
pub unsafe fn create_surface(
    entry: &ash::Entry,
    instance: &ash::Instance,
    window_info: &WindowSystemInfo,
) -> Result<vk::SurfaceKHR, VulkanError> {
    let surface = match window_info.window_type {
        #[cfg(target_os = "linux")]
        WindowSystemType::X11 => {
            let xlib_surface_fn = ash::extensions::khr::XlibSurface::new(entry, instance);
            let create_info = vk::XlibSurfaceCreateInfoKHR::builder()
                .dpy(window_info.display_connection as *mut _)
                .window(window_info.render_surface as u64)
                .build();
            xlib_surface_fn
                .create_xlib_surface(&create_info, None)
                .map_err(|_| {
                    log::error!("Failed to initialize Xlib surface");
                    surface_initialization_error()
                })?
        }
        #[cfg(target_os = "linux")]
        WindowSystemType::Wayland => {
            let wayland_surface_fn = ash::extensions::khr::WaylandSurface::new(entry, instance);
            let create_info = vk::WaylandSurfaceCreateInfoKHR::builder()
                .display(window_info.display_connection as *mut _)
                .surface(window_info.render_surface as *mut _)
                .build();
            wayland_surface_fn
                .create_wayland_surface(&create_info, None)
                .map_err(|_| {
                    log::error!("Failed to initialize Wayland surface");
                    surface_initialization_error()
                })?
        }
        #[cfg(target_os = "windows")]
        WindowSystemType::Windows => {
            let win32_surface_fn = ash::extensions::khr::Win32Surface::new(entry, instance);
            let create_info = vk::Win32SurfaceCreateInfoKHR::builder()
                .hinstance(std::ptr::null_mut())
                .hwnd(window_info.render_surface as *const _)
                .build();
            win32_surface_fn
                .create_win32_surface(&create_info, None)
                .map_err(|_| {
                    log::error!("Failed to initialize Win32 surface");
                    surface_initialization_error()
                })?
        }
        #[cfg(target_os = "macos")]
        WindowSystemType::Cocoa => {
            let metal_surface_fn = ash::extensions::ext::MetalSurface::new(entry, instance);
            let create_info = vk::MetalSurfaceCreateInfoEXT::builder()
                .layer(window_info.render_surface as *const _)
                .build();
            metal_surface_fn
                .create_metal_surface(&create_info, None)
                .map_err(|_| {
                    log::error!("Failed to initialize Metal surface");
                    surface_initialization_error()
                })?
        }
        #[cfg(target_os = "android")]
        WindowSystemType::Android => {
            let android_surface_fn = ash::extensions::khr::AndroidSurface::new(entry, instance);
            let create_info = vk::AndroidSurfaceCreateInfoKHR::builder()
                .window(window_info.render_surface as *mut _)
                .build();
            android_surface_fn
                .create_android_surface(&create_info, None)
                .map_err(|_| {
                    log::error!("Failed to initialize Android surface");
                    surface_initialization_error()
                })?
        }
        #[cfg(target_os = "haiku")]
        WindowSystemType::Xcb => {
            let xcb_surface_fn = ash::extensions::khr::XcbSurface::new(entry, instance);
            let create_info = vk::XcbSurfaceCreateInfoKHR::builder()
                .connection(window_info.display_connection.cast())
                .window(window_info.render_surface as usize as u32)
                .build();
            xcb_surface_fn
                .create_xcb_surface(&create_info, None)
                .map_err(|_| {
                    log::error!("Failed to initialize Xcb surface");
                    surface_initialization_error()
                })?
        }
        WindowSystemType::Headless => {
            log::error!("Presentation not supported on this platform");
            return Err(surface_initialization_error());
        }
    };

    if surface == vk::SurfaceKHR::null() {
        log::error!("Presentation not supported on this platform");
        return Err(surface_initialization_error());
    }

    Ok(surface)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_surface_creation_failure_uses_upstream_initialization_error() {
        assert_eq!(
            surface_initialization_error().result,
            vk::Result::ERROR_INITIALIZATION_FAILED
        );
    }
}

// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `zuyu/src/video_core/vulkan_common/vulkan_library.h` and
//! `zuyu/src/video_core/vulkan_common/vulkan_library.cpp`.
//!
//! Loads the Vulkan library and returns an `ash::Entry`.
//! In C++ this uses `Common::DynamicLibrary` to dlopen libvulkan;
//! in Rust, `ash::Entry::linked()` or `ash::Entry::load()` handles this.

use super::vulkan_wrapper::VulkanError;
use ash::vk;

/// Opens the Vulkan library and returns an `ash::Entry`.
///
/// Port of `Vulkan::OpenLibrary`.
///
/// On Linux, this attempts to load `libvulkan.so.1` then `libvulkan.so`.
/// On macOS, this would look for MoltenVK paths.
/// On Android, the frontend provides the library.
///
/// Using `ash::Entry::linked()` requires linking against libvulkan at build time.
/// Using `ash::Entry::load()` dynamically loads it at runtime (matching the C++ approach).
pub fn open_library() -> Result<ash::Entry, VulkanError> {
    log::debug!("Looking for a Vulkan library");

    #[cfg(target_os = "macos")]
    {
        return open_library_macos();
    }

    #[cfg(not(target_os = "macos"))]
    {
        return open_library_default();
    }
}

#[cfg(not(target_os = "macos"))]
fn open_library_default() -> Result<ash::Entry, VulkanError> {
    // ash::Entry::load() will dlopen libvulkan.so / vulkan-1.dll / etc.
    // This matches the C++ behavior of dynamically loading the Vulkan library.
    match unsafe { ash::Entry::load() } {
        Ok(entry) => {
            log::debug!("Vulkan library loaded successfully");
            Ok(entry)
        }
        Err(e) => {
            log::error!("Failed to load Vulkan library: {}", e);
            Err(VulkanError::new(vk::Result::ERROR_INITIALIZATION_FAILED))
        }
    }
}

#[cfg(target_os = "macos")]
fn open_library_macos() -> Result<ash::Entry, VulkanError> {
    use std::path::PathBuf;

    let mut library_paths = Vec::<PathBuf>::new();

    // Upstream first allows an explicit Vulkan loader override.
    if let Some(path) = std::env::var_os("LIBVULKAN_PATH").filter(|path| !path.is_empty()) {
        library_paths.push(PathBuf::from(path));
    }

    // Upstream app bundle lookup:
    // Common::FS::GetBundleDirectory() / Contents/Frameworks/libvulkan.1.dylib
    // Common::FS::GetBundleDirectory() / Contents/Frameworks/libMoltenVK.dylib
    if let Some(bundle_dir) = bundle_directory() {
        library_paths.push(bundle_dir.join("Contents/Frameworks/libvulkan.1.dylib"));
        library_paths.push(bundle_dir.join("Contents/Frameworks/libMoltenVK.dylib"));
    }

    let mut errors = Vec::<String>::new();
    for library_path in library_paths {
        if !library_path.exists() {
            continue;
        }
        log::debug!("Trying Vulkan library: {}", library_path.display());
        match unsafe { ash::Entry::load_from(library_path.as_os_str()) } {
            Ok(entry) => {
                log::info!("Vulkan library loaded from {}", library_path.display());
                return Ok(entry);
            }
            Err(err) => {
                errors.push(format!("{}: {}", library_path.display(), err));
            }
        }
    }

    match unsafe { ash::Entry::load() } {
        Ok(entry) => {
            log::debug!("Vulkan library loaded through ash default loader");
            Ok(entry)
        }
        Err(err) => {
            if errors.is_empty() {
                log::error!("Failed to load Vulkan library: {}", err);
            } else {
                log::error!(
                    "Failed to load Vulkan library: {}; explicit attempts: {}",
                    err,
                    errors.join("; ")
                );
            }
            Err(VulkanError::new(vk::Result::ERROR_INITIALIZATION_FAILED))
        }
    }
}

#[cfg(target_os = "macos")]
fn bundle_directory() -> Option<std::path::PathBuf> {
    let exe_path = std::env::current_exe().ok()?;
    let mut path = exe_path.as_path();
    while let Some(parent) = path.parent() {
        if parent
            .extension()
            .is_some_and(|extension| extension == "app")
        {
            return Some(parent.to_path_buf());
        }
        path = parent;
    }
    None
}

#[cfg(test)]
mod tests {
    // Note: These tests require a Vulkan runtime to be present on the system.
    // They are kept minimal to avoid CI failures on headless systems.

    #[test]
    fn test_open_library_does_not_panic() {
        // Just verify we don't panic; the actual result depends on the host.
        let _result = super::open_library();
    }
}

// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `src/video_core/vulkan_common/vulkan.h`.
//!
//! Ash provides Vulkan's opaque handles, platform-specific structures, and function dispatch, so
//! Eden's `VK_NO_PROTOTYPES`, `VK_USE_PLATFORM_*`, macro sanitation, and forward declaration have
//! no Rust equivalents. Extension-name fallbacks not supplied by this Ash version remain owned
//! here, beside their upstream definitions.

/// `VK_KHR_MAINTENANCE_7_EXTENSION_NAME` from Eden's Vulkan header.
pub const KHR_MAINTENANCE_7_EXTENSION_NAME: &str = "VK_KHR_maintenance7";

/// `VK_KHR_MAINTENANCE_8_EXTENSION_NAME` from Eden's Vulkan header.
pub const KHR_MAINTENANCE_8_EXTENSION_NAME: &str = "VK_KHR_maintenance8";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisional_extension_names_match_eden_header() {
        assert_eq!(KHR_MAINTENANCE_7_EXTENSION_NAME, "VK_KHR_maintenance7");
        assert_eq!(KHR_MAINTENANCE_8_EXTENSION_NAME, "VK_KHR_maintenance8");
    }
}

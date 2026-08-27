// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `src/video_core/vulkan_common/vma.h` binding owner.
//!
//! Eden configures AMD Vulkan Memory Allocator with static Vulkan symbols disabled and dynamic
//! loading enabled. Its frontend translation units define `VMA_IMPLEMENTATION`. The Rust port
//! delegates compilation and bindings to `vk-mem`: that crate disables both VMA loaders and
//! passes the complete Ash Vulkan function table explicitly. Both integrations avoid a static
//! Vulkan-library dependency, but their function-routing mechanisms intentionally differ.
//!
//! [`super::vulkan_device`] owns allocator creation and Eden's allocator flags, while
//! [`super::vulkan_memory_allocator`] owns the higher-level allocation policy.

use std::sync::{Arc, Mutex};

/// Rust ownership wrapper for Eden's opaque `VmaAllocator` handle.
///
/// `vulkan_device` sets `VMA_ALLOCATOR_CREATE_EXTERNALLY_SYNCHRONIZED_BIT` through `vk-mem`'s
/// matching flag. This mutex supplies the external synchronization required by that mode.
pub type VmaAllocator = Arc<Mutex<vk_mem::Allocator>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn allocator_owner_is_shareable_and_externally_lockable() {
        assert_send_sync::<VmaAllocator>();

        let alias: Option<VmaAllocator> = None;
        let canonical: Option<Arc<Mutex<vk_mem::Allocator>>> = alias;
        assert!(canonical.is_none());
    }
}

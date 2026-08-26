// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `video_core/vulkan_common/nsight_aftermath_tracker.h` and
//! `video_core/vulkan_common/nsight_aftermath_tracker.cpp`.
//!
//! Nsight Aftermath GPU crash tracking integration.
//!
//! The C++ code conditionally compiles with `HAS_NSIGHT_AFTERMATH` to integrate
//! with NVIDIA's Nsight Aftermath SDK for GPU crash dump collection.
//! Since this is a Windows/NVIDIA-specific debugging tool with no Rust SDK bindings,
//! this module provides a stub implementation matching the no-op path when
//! `HAS_NSIGHT_AFTERMATH` is not defined.

/// GPU crash dump tracker for NVIDIA Nsight Aftermath.
///
/// Port of `Vulkan::NsightAftermathTracker`.
///
/// This is a stub implementation. The full implementation would require
/// bindings to `GFSDK_Aftermath_Lib.x64.dll` which is NVIDIA-proprietary
/// and Windows-only. The C++ code also stubs this when `HAS_NSIGHT_AFTERMATH`
/// is not defined.
pub struct NsightAftermathTracker;

impl NsightAftermathTracker {
    /// Creates a new tracker instance.
    ///
    /// Port of `NsightAftermathTracker::NsightAftermathTracker()`.
    /// Without the Aftermath SDK, this is a no-op.
    pub fn new() -> Self {
        Self
    }

    /// Saves a SPIR-V shader for crash dump correlation.
    ///
    /// Port of `NsightAftermathTracker::SaveShader`.
    /// Without the Aftermath SDK, this is a no-op.
    pub fn save_shader(&self, _spirv: &[u32]) {
        // No-op when Aftermath is not available, matching:
        // inline void NsightAftermathTracker::SaveShader(std::span<const u32>) const {}
    }
}

impl Default for NsightAftermathTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for NsightAftermathTracker {
    /// Port of `NsightAftermathTracker::~NsightAftermathTracker()`.
    /// Without the Aftermath SDK, this is a no-op.
    fn drop(&mut self) {
        // No-op when Aftermath is not available
    }
}

#[cfg(test)]
mod tests {
    use super::NsightAftermathTracker;

    #[test]
    fn unsupported_build_path_is_a_noop() {
        let tracker = NsightAftermathTracker::new();
        tracker.save_shader(&[0x0723_0203, 0, 0, 0]);
    }
}

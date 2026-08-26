// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! GPU logging and crash-state capture.
//!
//! This module mirrors Eden's `video_core/gpu_logging` directory.

#[cfg(target_os = "android")]
pub mod freedreno_debug;
pub mod gpu_logging;
pub mod gpu_state_capture;
pub mod qualcomm_debug;

pub use gpu_logging::{
    dump_spirv_shader, get_instance, get_shader_stage_name, is_active, DriverType, GpuLogger,
    GpuStateSnapshot, LogLevel, MemoryAllocationEntry, VulkanCallEntry,
};
pub use gpu_state_capture::GpuStateCapture;

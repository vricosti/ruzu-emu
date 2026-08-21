// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GPU subsystem for ruzu.
//!
//! Provides GPU memory management, GPFIFO command processing, engine dispatch,
//! syncpoint synchronization, and rendering backend abstraction.

pub mod buffer_cache;
pub mod cache_types;
pub mod capture;
pub mod cdma_pusher;
pub mod command_processor;
pub mod compatible_formats;
pub mod control;
pub mod delayed_destruction_ring;
pub mod descriptor_table;
pub mod dirty_flags;
pub mod dma_pusher;
pub mod engines;
pub mod fence_manager;
pub mod framebuffer_config;
pub mod fsr;
pub mod gpu;
pub mod gpu_context;
pub mod gpu_thread;
pub mod guest_memory;
pub mod host1x;
pub mod host_shaders;
pub mod invalidation_accumulator;
pub mod macro_engine;
pub mod macro_interpreter;
pub mod memory_manager;
pub mod present;
pub mod pte_kind;
pub mod query_cache;
pub mod query_cache_top;
pub mod rasterizer;
pub mod rasterizer_download_area;
pub mod rasterizer_interface;
pub mod renderer_base;
#[cfg(target_os = "macos")]
pub mod renderer_metal;
pub mod renderer_null;
pub mod renderer_opengl;
pub mod renderer_vulkan;
pub mod shader;
pub mod shader_cache;
pub mod shader_environment;
pub mod shader_notify;
// shader_recompiler is a separate crate (matches upstream top-level directory)
pub mod smaa_area_tex;
pub mod smaa_search_tex;
pub mod surface;
pub mod swapchain;
pub mod swizzle;
pub mod syncpoint;
pub mod texture_cache;
pub mod textures;
pub mod transform_feedback;
pub mod video_core;
pub mod vulkan_common;

#[cfg(test)]
pub(crate) mod test_support {
    use common::settings;
    use common::settings_enums::GpuAccuracy;
    use std::sync::{Mutex, MutexGuard};

    static GPU_ACCURACY_MUTEX: Mutex<()> = Mutex::new(());

    pub(crate) struct GpuAccuracyGuard {
        previous: GpuAccuracy,
        _lock: MutexGuard<'static, ()>,
    }

    impl GpuAccuracyGuard {
        pub(crate) fn set(value: GpuAccuracy) -> Self {
            let lock = GPU_ACCURACY_MUTEX
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut values = settings::values_mut();
            let previous = values.current_gpu_accuracy;
            values.current_gpu_accuracy = value;
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for GpuAccuracyGuard {
        fn drop(&mut self) {
            settings::values_mut().current_gpu_accuracy = self.previous;
        }
    }
}

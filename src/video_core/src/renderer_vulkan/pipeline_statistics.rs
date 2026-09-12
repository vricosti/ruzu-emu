// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `pipeline_statistics.h` / `pipeline_statistics.cpp`.
//!
//! Collects and reports Vulkan pipeline compilation statistics
//! (code size, register counts, etc.) when supported by the device.

use std::sync::Mutex;

use ash::vk;

use crate::vulkan_common::vulkan_device::Device;

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

/// Port of `PipelineStatistics::Stats` inner struct.
#[derive(Debug, Clone, Copy, Default)]
struct Stats {
    code_size: u64,
    register_count: u64,
    sgpr_count: u64,
    vgpr_count: u64,
    branches_count: u64,
    basic_block_count: u64,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Port of `GetUint64` from `pipeline_statistics.cpp`.
///
/// Extracts a u64 value from a pipeline executable statistic regardless
/// of the statistic's format.
fn get_uint64(statistic: &vk::PipelineExecutableStatisticKHR) -> u64 {
    match statistic.format {
        vk::PipelineExecutableStatisticFormatKHR::INT64 => unsafe { statistic.value.i64 as u64 },
        vk::PipelineExecutableStatisticFormatKHR::UINT64 => unsafe { statistic.value.u64 },
        vk::PipelineExecutableStatisticFormatKHR::FLOAT64 => unsafe { statistic.value.f64 as u64 },
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// PipelineStatistics
// ---------------------------------------------------------------------------

/// Port of `PipelineStatistics` class.
///
/// Collects per-pipeline statistics via `VK_KHR_pipeline_executable_properties`
/// and provides an aggregate report.
pub struct PipelineStatistics {
    collected_stats: Mutex<Vec<Stats>>,
}

impl PipelineStatistics {
    /// Port of `PipelineStatistics::PipelineStatistics`.
    pub fn new(_device: &Device) -> Self {
        PipelineStatistics {
            collected_stats: Mutex::new(Vec::new()),
        }
    }

    /// Port of `PipelineStatistics::Collect`.
    ///
    /// Queries pipeline executable properties and statistics for the given
    /// pipeline and accumulates them.
    pub fn collect(&self, device: &Device, pipeline: vk::Pipeline) {
        let pipeline_executable_properties_fn =
            ash::khr::pipeline_executable_properties::Device::new(
                device.get_instance(),
                device.get_logical(),
            );
        let pipeline_info = vk::PipelineInfoKHR::default().pipeline(pipeline);

        let properties = unsafe {
            pipeline_executable_properties_fn
                .get_pipeline_executable_properties(&pipeline_info)
                .expect("GetPipelineExecutablePropertiesKHR failed")
        };

        for (executable, _prop) in properties.iter().enumerate() {
            let executable_info = vk::PipelineExecutableInfoKHR::default()
                .pipeline(pipeline)
                .executable_index(executable as u32);

            let statistics = unsafe {
                pipeline_executable_properties_fn
                    .get_pipeline_executable_statistics(&executable_info)
                    .expect("GetPipelineExecutableStatisticsKHR failed")
            };

            if statistics.is_empty() {
                continue;
            }

            let mut stage_stats = Stats::default();
            for statistic in &statistics {
                // Safety: statistic.name is a null-terminated C string in a fixed-size array
                let name = unsafe {
                    std::ffi::CStr::from_ptr(statistic.name.as_ptr())
                        .to_str()
                        .unwrap_or("")
                };

                match name {
                    "Binary Size" | "Code size" | "Instruction Count" => {
                        stage_stats.code_size = get_uint64(statistic);
                    }
                    "Register Count" => {
                        stage_stats.register_count = get_uint64(statistic);
                    }
                    "SGPRs" | "numUsedSgprs" => {
                        stage_stats.sgpr_count = get_uint64(statistic);
                    }
                    "VGPRs" | "numUsedVgprs" => {
                        stage_stats.vgpr_count = get_uint64(statistic);
                    }
                    "Branches" => {
                        stage_stats.branches_count = get_uint64(statistic);
                    }
                    "Basic Block Count" => {
                        stage_stats.basic_block_count = get_uint64(statistic);
                    }
                    _ => {}
                }
            }

            let mut locked = self.collected_stats.lock().unwrap();
            locked.push(stage_stats);
        }
    }

    /// Port of `PipelineStatistics::Report`.
    ///
    /// Logs averaged pipeline statistics across all collected pipelines.
    pub fn report(&self) {
        let mut total = Stats::default();
        let num = {
            let locked = self.collected_stats.lock().unwrap();
            for stats in locked.iter() {
                total.code_size += stats.code_size;
                total.register_count += stats.register_count;
                total.sgpr_count += stats.sgpr_count;
                total.vgpr_count += stats.vgpr_count;
                total.branches_count += stats.branches_count;
                total.basic_block_count += stats.basic_block_count;
            }
            locked.len() as f64
        };

        let mut report = String::new();
        let add = |report: &mut String, prefix: &str, value: u64| {
            if value > 0 {
                report.push_str(&format!("{prefix}{:9.03}\n", value as f64 / num));
            }
        };
        add(&mut report, "Code size:      ", total.code_size);
        add(&mut report, "Register count: ", total.register_count);
        add(&mut report, "SGPRs:          ", total.sgpr_count);
        add(&mut report, "VGPRs:          ", total.vgpr_count);
        add(&mut report, "Branches count: ", total.branches_count);
        add(&mut report, "Basic blocks:   ", total.basic_block_count);

        log::info!(
            "\nAverage pipeline statistics\n\
             ==========================================\n\
             {}\n",
            report
        );
    }
}

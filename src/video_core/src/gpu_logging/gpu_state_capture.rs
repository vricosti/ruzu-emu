// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden `video_core/gpu_logging/gpu_state_capture.{h,cpp}`.

use super::gpu_logging::{get_instance, GpuStateSnapshot};

pub struct GpuStateCapture;

impl GpuStateCapture {
    pub fn capture_state() -> GpuStateSnapshot {
        get_instance().get_current_snapshot()
    }

    pub fn serialize_state(snapshot: &GpuStateSnapshot) -> String {
        let mut result = String::new();
        result.push_str("=== GPU STATE SNAPSHOT ===\n\n");
        result.push_str(&format!("Driver: {}\n", snapshot.driver_type as u8));
        result.push_str(&format!(
            "Recent Calls: {}\n\n",
            snapshot.recent_calls.len()
        ));
        result.push_str("=== RECENT VULKAN CALLS ===\n");
        for call in &snapshot.recent_calls {
            result.push_str(&format!(
                "{}: {}({}) -> {}\n",
                call.timestamp.as_micros(),
                call.call_name,
                call.parameters,
                call.result
            ));
        }
        result.push_str("\n=== MEMORY STATUS ===\n");
        result.push_str(&snapshot.memory_status);
        result.push_str("\n=== PIPELINE STATE ===\n");
        result.push_str(&snapshot.pipeline_state);
        result.push_str("\n=== DRIVER DEBUG INFO ===\n");
        result.push_str(&snapshot.driver_debug_info);
        result
    }

    pub fn write_crash_dump(crash_reason: &str) {
        get_instance().dump_state_to_file(crash_reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu_logging::{DriverType, VulkanCallEntry};
    use std::time::Duration;

    #[test]
    fn serialize_state_preserves_the_upstream_section_order() {
        let snapshot = GpuStateSnapshot {
            recent_calls: vec![VulkanCallEntry {
                timestamp: Duration::from_micros(7),
                call_name: "vkCmdTest".to_owned(),
                parameters: "x=1".to_owned(),
                result: 0,
                thread_id: 3,
            }],
            memory_status: "memory\n".to_owned(),
            pipeline_state: "pipeline\n".to_owned(),
            driver_debug_info: "driver\n".to_owned(),
            driver_type: DriverType::Turnip,
            ..GpuStateSnapshot::default()
        };
        let serialized = GpuStateCapture::serialize_state(&snapshot);
        let calls = serialized.find("=== RECENT VULKAN CALLS ===").unwrap();
        let memory = serialized.find("=== MEMORY STATUS ===").unwrap();
        let pipeline = serialized.find("=== PIPELINE STATE ===").unwrap();
        let driver = serialized.find("=== DRIVER DEBUG INFO ===").unwrap();
        assert!(calls < memory && memory < pipeline && pipeline < driver);
        assert!(serialized.contains("7: vkCmdTest(x=1) -> 0"));
    }
}

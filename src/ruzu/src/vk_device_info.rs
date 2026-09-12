// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! GTK counterpart of `yuzu/vk_device_info.{h,cpp}`.

use std::ffi::CStr;

use ash::vk;
use video_core::vulkan_common::vulkan_device::Device;
use video_core::vulkan_common::vulkan_instance::{self, WindowSystemType};
use video_core::vulkan_common::vulkan_library;

/// Vulkan driver information used by the configuration pages.
#[derive(Clone)]
pub struct Record {
    pub name: String,
    pub vsync_support: Vec<vk::PresentModeKHR>,
    pub has_broken_compute: bool,
}

/// `VkDeviceInfo::PopulateRecords`.
///
/// Device names and driver properties do not require a presentation surface.
/// The GTK frontend currently creates that surface only when emulation starts,
/// so present-mode discovery remains empty here and the Graphics page uses its
/// normal backend mode list. This preserves the upstream owner and data shape
/// while still exposing the physical device names in Properties.
pub fn populate_records(records: &mut Vec<Record>) {
    // Eden skips PopulateRecords after a crashed startup probe. Ruzu requests
    // the records lazily from Graphics instead of in the main-window ctor.
    if crate::uisettings::with(|values| values.has_broken_vulkan) {
        records.clear();
        return;
    }
    if let Err(error) = try_populate_records(records) {
        log::error!("Failed to enumerate Vulkan devices: {error}");
    }
}

fn try_populate_records(
    records: &mut Vec<Record>,
) -> Result<(), video_core::vulkan_common::vulkan_wrapper::VulkanError> {
    let entry = vulkan_library::open_library()?;
    let instance = vulkan_instance::create_instance(
        entry,
        vk::API_VERSION_1_1,
        WindowSystemType::Headless,
        false,
    )?;
    let physical_devices = instance.enumerate_physical_devices()?;

    records.clear();
    records.reserve(physical_devices.len());
    for physical_device in physical_devices {
        let properties = unsafe {
            instance
                .instance
                .get_physical_device_properties(physical_device)
        };
        let mut driver_properties = vk::PhysicalDeviceDriverProperties::default();
        let mut properties2 = vk::PhysicalDeviceProperties2::default()
            .push_next(&mut driver_properties);
        unsafe {
            instance
                .instance
                .get_physical_device_properties2(physical_device, &mut properties2);
        }

        let name = unsafe { CStr::from_ptr(properties.device_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        records.push(Record {
            name,
            vsync_support: Vec::new(),
            has_broken_compute: Device::check_broken_compute(
                driver_properties.driver_id,
                properties.driver_version,
            ),
        });
    }
    Ok(())
}

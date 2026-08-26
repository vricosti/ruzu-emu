// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `zuyu/src/video_core/vulkan_common/vulkan_debug_callback.h` and
//! `zuyu/src/video_core/vulkan_common/vulkan_debug_callback.cpp`.
//!
//! Creates a Vulkan debug utils messenger that logs validation layer messages.

use ash::vk;
use std::ffi::CStr;

use crate::gpu_logging::{get_instance, is_active};

use super::vulkan_wrapper::VulkanError;

/// RAII owner for the Vulkan debug messenger.
///
/// Counterpart of upstream's `vk::DebugUtilsMessenger`: the extension loader
/// carries the instance dispatch table required to destroy the handle.
pub struct DebugUtilsMessenger {
    loader: ash::extensions::ext::DebugUtils,
    handle: vk::DebugUtilsMessengerEXT,
}

impl Drop for DebugUtilsMessenger {
    fn drop(&mut self) {
        unsafe {
            self.loader.destroy_debug_utils_messenger(self.handle, None);
        }
    }
}

// ---------------------------------------------------------------------------
// Known false-positive message IDs to skip — port of the switch in DebugUtilCallback
// ---------------------------------------------------------------------------

/// Message IDs known to be false positives in validation layers.
///
/// Port of the `switch (data->messageIdNumber)` block from `vulkan_debug_callback.cpp`.
#[cfg(not(target_os = "android"))]
const IGNORED_MESSAGE_IDS: &[u32] = &[
    0x682a878a, // VUID-vkCmdBindVertexBuffers2EXT-pBuffers-parameter
    0x99fb7dfd, // UNASSIGNED-RequiredParameter (vkCmdBindVertexBuffers2EXT pBuffers[0])
    0xe8616bf2, // Bound VkDescriptorSet 0x0[] was destroyed
    0x1608dec0, // Image layout mismatch in vkUpdateDescriptorSet
    0x55362756, // Descriptor binding and framebuffer attachment overlap
];

fn is_ignored_message_id(message_id: u32) -> bool {
    IGNORED_MESSAGE_IDS.contains(&message_id)
}

/// Port of Eden's `GetMessageTypeName` helper used by GPU logging.
fn get_message_type_name(message_type: vk::DebugUtilsMessageTypeFlagsEXT) -> &'static str {
    if message_type.contains(vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION) {
        "Validation"
    } else if message_type.contains(vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE) {
        "Performance"
    } else {
        "General"
    }
}

#[cfg(target_os = "android")]
const IGNORED_MESSAGE_IDS: &[u32] = &[
    0xbf9cf353, // VUID-vkCmdBindVertexBuffers2-pBuffers-04111
    0x1093bebb, // VUID-vkCmdSetCullMode-None-03384
    0x9215850f, // VUID-vkCmdSetDepthTestEnable-None-03352
    0x86bf18dc, // VUID-vkCmdSetDepthWriteEnable-None-03354
    0x0792ad08, // VUID-vkCmdSetStencilOp-None-03351
    0x93e1ba4e, // VUID-vkCmdSetFrontFace-None-03383
    0xac9c13c5, // VUID-vkCmdSetStencilTestEnable-None-03350
    0xc9a2001b, // VUID-vkCmdSetDepthBoundsTestEnable-None-03349
    0x8b7159a7, // VUID-vkCmdSetDepthCompareOp-None-03353
    0xb13c8036, // VUID-vkCmdSetDepthBiasEnable-None-04872
    0xdff2e5c1, // VUID-vkCmdSetRasterizerDiscardEnable-None-04871
    0x0cc85f41, // VUID-vkCmdSetPrimitiveRestartEnable-None-04866
    0x1257b492, // VUID-vkCmdSetLogicOpEXT-None-0486
    0x398e0dab, // VUID-vkCmdSetVertexInputEXT-None-04790
    0x970c11a5, // VUID-vkCmdSetColorWriteMaskEXT-*
    0x6b453f78, // VUID-vkCmdSetColorBlendEnableEXT-*
    0xf66469d0, // VUID-vkCmdSetColorBlendEquationEXT-*
    0x1d43405e, // VUID-vkCmdSetLogicOpEnableEXT-*
    0x638462e8, // VUID-vkCmdSetDepthClampEnableEXT-*
    0xe0a2da61, // VUID-vkCmdDrawIndexed-format-07753
];

// ---------------------------------------------------------------------------
// Debug callback function
// ---------------------------------------------------------------------------

/// Vulkan debug utils messenger callback.
///
/// Port of `DebugUtilCallback` from `vulkan_debug_callback.cpp`.
unsafe extern "system" fn debug_utils_callback(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    message_type: vk::DebugUtilsMessageTypeFlagsEXT,
    callback_data: *const vk::DebugUtilsMessengerCallbackDataEXT,
    _user_data: *mut std::ffi::c_void,
) -> vk::Bool32 {
    let data = &*callback_data;

    // Skip known false-positive validation errors
    let message_id = data.message_id_number as u32;
    if is_ignored_message_id(message_id) {
        return vk::FALSE;
    }

    let message = if data.p_message.is_null() {
        "<null message>"
    } else {
        match CStr::from_ptr(data.p_message).to_str() {
            Ok(s) => s,
            Err(_) => "<invalid UTF-8>",
        }
    };

    if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        log::error!(target: "Render_Vulkan", "{}", message);
    } else if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING) {
        log::warn!(target: "Render_Vulkan", "{}", message);
    } else if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::INFO) {
        log::info!(target: "Render_Vulkan", "{}", message);
    } else if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::VERBOSE) {
        log::debug!(target: "Render_Vulkan", "{}", message);
    }

    // Route Vulkan validation messages to the GPU logger, matching Eden.
    if is_active() && *common::settings::values().gpu_log_vulkan_calls.get_value() {
        let result_code = if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
            -1
        } else if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING) {
            -2
        } else {
            0
        };
        let call_name = if data.p_message_id_name.is_null() {
            "VulkanDebug"
        } else {
            match CStr::from_ptr(data.p_message_id_name).to_str() {
                Ok(name) => name,
                Err(_) => "VulkanDebug",
            }
        };
        get_instance().log_vulkan_call(
            call_name,
            &format!("{}: {}", get_message_type_name(message_type), message),
            result_code,
        );
    }

    vk::FALSE
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Creates a debug utils messenger on the given instance.
///
/// Port of `Vulkan::CreateDebugUtilsCallback` from `vulkan_debug_callback.cpp`.
///
/// Returns an owner retaining both the messenger handle and the debug-utils
/// loader needed to destroy it.
pub fn create_debug_utils_callback(
    entry: &ash::Entry,
    instance: &ash::Instance,
) -> Result<DebugUtilsMessenger, VulkanError> {
    let debug_utils = ash::extensions::ext::DebugUtils::new(entry, instance);

    let create_info = vk::DebugUtilsMessengerCreateInfoEXT::builder()
        .message_severity(
            vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                | vk::DebugUtilsMessageSeverityFlagsEXT::INFO
                | vk::DebugUtilsMessageSeverityFlagsEXT::VERBOSE,
        )
        .message_type(
            vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
        )
        .pfn_user_callback(Some(debug_utils_callback))
        .build();

    let messenger = unsafe {
        debug_utils
            .create_debug_utils_messenger(&create_info, None)
            .map_err(|e| VulkanError::new(e))?
    };

    Ok(DebugUtilsMessenger {
        loader: debug_utils,
        handle: messenger,
    })
}

#[cfg(test)]
mod tests {
    use super::{get_message_type_name, is_ignored_message_id};
    use ash::vk;

    #[test]
    fn known_validation_false_positive_is_ignored() {
        #[cfg(not(target_os = "android"))]
        assert!(is_ignored_message_id(0x682a878a));
        #[cfg(target_os = "android")]
        {
            assert!(is_ignored_message_id(0xbf9cf353));
            assert!(is_ignored_message_id(0x1257b492));
        }
        assert!(!is_ignored_message_id(0));
    }

    #[test]
    fn gpu_log_message_type_priority_matches_upstream() {
        assert_eq!(
            get_message_type_name(vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION),
            "Validation"
        );
        assert_eq!(
            get_message_type_name(vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE),
            "Performance"
        );
        assert_eq!(
            get_message_type_name(
                vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                    | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE
            ),
            "Validation"
        );
        assert_eq!(
            get_message_type_name(vk::DebugUtilsMessageTypeFlagsEXT::GENERAL),
            "General"
        );
    }
}

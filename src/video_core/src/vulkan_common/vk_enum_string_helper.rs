// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `zuyu/src/video_core/vulkan_common/vk_enum_string_helper.h`.
//!
//! The upstream header includes `<vulkan/vk_enum_string_helper.h>`, which provides
//! `string_VkResult`, `string_VkFormat`, and similar functions that convert Vulkan
//! enum values to human-readable strings for debug output.
//!
//! Ash's `Debug` implementations provide most enum names.  The Vulkan headers used
//! by Eden are newer than Ash's registry, however, and the generated helper also
//! selects different canonical names for a few aliases.  Keep those differences in
//! this matching module rather than leaking version-specific formatting into users.

use ash::vk;

fn string_vk_enum(
    value: impl std::fmt::Debug,
    value_prefix: &str,
    value_suffix: &str,
    enum_name: &str,
) -> String {
    let name = format!("{value:?}");
    if name.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        format!("{value_prefix}{name}{value_suffix}")
    } else {
        format!("Unhandled {enum_name}")
    }
}

fn normalize_astc_format_name(name: &mut String) {
    if !name.starts_with("VK_FORMAT_ASTC_") {
        return;
    }
    let bytes = name.as_bytes();
    let mut normalized = String::with_capacity(name.len());
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte == b'X'
            && index > 0
            && index + 1 < bytes.len()
            && bytes[index - 1].is_ascii_digit()
            && bytes[index + 1].is_ascii_digit()
        {
            normalized.push('x');
        } else {
            normalized.push(char::from(byte));
        }
    }
    *name = normalized;
}

/// Returns a human-readable string for a `VkResult` value.
///
/// Port of `string_VkResult()` from `vk_enum_string_helper.h`.
pub fn string_vk_result(result: vk::Result) -> String {
    let canonical_name = match result.as_raw() {
        -1_000_011_001 => Some("VK_ERROR_VALIDATION_FAILED"),
        -1_000_174_001 => Some("VK_ERROR_NOT_PERMITTED"),
        -1_000_208_000 => Some("VK_ERROR_PRESENT_TIMING_QUEUE_FULL_EXT"),
        1_000_482_000 => Some("VK_INCOMPATIBLE_SHADER_BINARY_EXT"),
        1_000_483_000 => Some("VK_PIPELINE_BINARY_MISSING_KHR"),
        -1_000_483_000 => Some("VK_ERROR_NOT_ENOUGH_SPACE_KHR"),
        _ => None,
    };
    canonical_name.map_or_else(
        || string_vk_enum(result, "VK_", "", "VkResult"),
        str::to_owned,
    )
}

/// Returns a human-readable string for a `VkFormat` value.
///
/// Port of `string_VkFormat()` from `vk_enum_string_helper.h`.
pub fn string_vk_format(format: vk::Format) -> String {
    let canonical_name = match format.as_raw() {
        1_000_460_000 => Some("VK_FORMAT_R8_BOOL_ARM"),
        1_000_460_001 => Some("VK_FORMAT_R16_SFLOAT_FPENCODING_BFLOAT16_ARM"),
        1_000_460_002 => Some("VK_FORMAT_R8_SFLOAT_FPENCODING_FLOAT8E4M3_ARM"),
        1_000_460_003 => Some("VK_FORMAT_R8_SFLOAT_FPENCODING_FLOAT8E5M2_ARM"),
        1_000_464_000 => Some("VK_FORMAT_R16G16_SFIXED5_NV"),
        1_000_470_000 => Some("VK_FORMAT_A1B5G5R5_UNORM_PACK16"),
        1_000_470_001 => Some("VK_FORMAT_A8_UNORM"),
        1_000_609_000 => Some("VK_FORMAT_R10X6_UINT_PACK16_ARM"),
        1_000_609_001 => Some("VK_FORMAT_R10X6G10X6_UINT_2PACK16_ARM"),
        1_000_609_002 => Some("VK_FORMAT_R10X6G10X6B10X6A10X6_UINT_4PACK16_ARM"),
        1_000_609_003 => Some("VK_FORMAT_R12X4_UINT_PACK16_ARM"),
        1_000_609_004 => Some("VK_FORMAT_R12X4G12X4_UINT_2PACK16_ARM"),
        1_000_609_005 => Some("VK_FORMAT_R12X4G12X4B12X4A12X4_UINT_4PACK16_ARM"),
        1_000_609_006 => Some("VK_FORMAT_R14X2_UINT_PACK16_ARM"),
        1_000_609_007 => Some("VK_FORMAT_R14X2G14X2_UINT_2PACK16_ARM"),
        1_000_609_008 => Some("VK_FORMAT_R14X2G14X2B14X2A14X2_UINT_4PACK16_ARM"),
        1_000_609_009 => Some("VK_FORMAT_R14X2_UNORM_PACK16_ARM"),
        1_000_609_010 => Some("VK_FORMAT_R14X2G14X2_UNORM_2PACK16_ARM"),
        1_000_609_011 => Some("VK_FORMAT_R14X2G14X2B14X2A14X2_UNORM_4PACK16_ARM"),
        1_000_609_012 => Some("VK_FORMAT_G14X2_B14X2R14X2_2PLANE_420_UNORM_3PACK16_ARM"),
        1_000_609_013 => Some("VK_FORMAT_G14X2_B14X2R14X2_2PLANE_422_UNORM_3PACK16_ARM"),
        _ => None,
    };
    if let Some(name) = canonical_name {
        return name.to_owned();
    }
    let mut name = string_vk_enum(format, "VK_FORMAT_", "", "VkFormat");
    normalize_astc_format_name(&mut name);
    name
}

/// Returns a human-readable string for a `VkPresentModeKHR` value.
///
/// Port of `string_VkPresentModeKHR()` from `vk_enum_string_helper.h`.
pub fn string_vk_present_mode(mode: vk::PresentModeKHR) -> String {
    if mode.as_raw() == 1_000_361_000 {
        return "VK_PRESENT_MODE_FIFO_LATEST_READY_KHR".to_owned();
    }
    string_vk_enum(mode, "VK_PRESENT_MODE_", "_KHR", "VkPresentModeKHR")
}

/// Returns a human-readable string for a `VkColorSpaceKHR` value.
///
/// Port of `string_VkColorSpaceKHR()` from `vk_enum_string_helper.h`.
pub fn string_vk_color_space(color_space: vk::ColorSpaceKHR) -> String {
    if color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR {
        "VK_COLOR_SPACE_SRGB_NONLINEAR_KHR".to_owned()
    } else {
        string_vk_enum(color_space, "VK_COLOR_SPACE_", "", "VkColorSpaceKHR")
    }
}

/// Returns a human-readable string for a `VkImageLayout` value.
///
/// Port of `string_VkImageLayout()` from `vk_enum_string_helper.h`.
pub fn string_vk_image_layout(layout: vk::ImageLayout) -> String {
    let canonical_name = match layout.as_raw() {
        1_000_232_000 => Some("VK_IMAGE_LAYOUT_RENDERING_LOCAL_READ"),
        1_000_460_000 => Some("VK_IMAGE_LAYOUT_TENSOR_ALIASING_ARM"),
        1_000_553_000 => Some("VK_IMAGE_LAYOUT_VIDEO_ENCODE_QUANTIZATION_MAP_KHR"),
        1_000_620_000 => Some("VK_IMAGE_LAYOUT_ZERO_INITIALIZED_EXT"),
        _ => None,
    };
    if let Some(name) = canonical_name {
        return name.to_owned();
    }
    string_vk_enum(layout, "VK_IMAGE_LAYOUT_", "", "VkImageLayout")
}

/// Returns a human-readable string for a `VkImageTiling` value.
///
/// Port of `string_VkImageTiling()` from `vk_enum_string_helper.h`.
pub fn string_vk_image_tiling(tiling: vk::ImageTiling) -> String {
    string_vk_enum(tiling, "VK_IMAGE_TILING_", "", "VkImageTiling")
}

/// Returns a human-readable string for a `VkPhysicalDeviceType` value.
///
/// Port of `string_VkPhysicalDeviceType()` from `vk_enum_string_helper.h`.
pub fn string_vk_physical_device_type(device_type: vk::PhysicalDeviceType) -> String {
    string_vk_enum(
        device_type,
        "VK_PHYSICAL_DEVICE_TYPE_",
        "",
        "VkPhysicalDeviceType",
    )
}

/// Returns a human-readable string for a `VkObjectType` value.
///
/// Port of `string_VkObjectType()` from `vk_enum_string_helper.h`.
pub fn string_vk_object_type(object_type: vk::ObjectType) -> String {
    let canonical_name = match object_type.as_raw() {
        1_000_307_000 => Some("VK_OBJECT_TYPE_CUDA_MODULE_NV"),
        1_000_307_001 => Some("VK_OBJECT_TYPE_CUDA_FUNCTION_NV"),
        1_000_460_000 => Some("VK_OBJECT_TYPE_TENSOR_ARM"),
        1_000_460_001 => Some("VK_OBJECT_TYPE_TENSOR_VIEW_ARM"),
        1_000_483_000 => Some("VK_OBJECT_TYPE_PIPELINE_BINARY_KHR"),
        1_000_507_000 => Some("VK_OBJECT_TYPE_DATA_GRAPH_PIPELINE_SESSION_ARM"),
        1_000_556_000 => Some("VK_OBJECT_TYPE_EXTERNAL_COMPUTE_QUEUE_NV"),
        1_000_572_000 => Some("VK_OBJECT_TYPE_INDIRECT_COMMANDS_LAYOUT_EXT"),
        1_000_572_001 => Some("VK_OBJECT_TYPE_INDIRECT_EXECUTION_SET_EXT"),
        1_000_607_000 => Some("VK_OBJECT_TYPE_SHADER_INSTRUMENTATION_ARM"),
        _ => None,
    };
    if let Some(name) = canonical_name {
        return name.to_owned();
    }
    string_vk_enum(object_type, "VK_OBJECT_TYPE_", "", "VkObjectType")
}

/// Returns a human-readable string for a `VkSharingMode` value.
///
/// Port of `string_VkSharingMode()` from `vk_enum_string_helper.h`.
pub fn string_vk_sharing_mode(mode: vk::SharingMode) -> String {
    string_vk_enum(mode, "VK_SHARING_MODE_", "", "VkSharingMode")
}

/// Returns a human-readable string for a `VkDescriptorType` value.
///
/// Port of `string_VkDescriptorType()` from `vk_enum_string_helper.h`.
pub fn string_vk_descriptor_type(desc_type: vk::DescriptorType) -> String {
    let canonical_name = match desc_type.as_raw() {
        1_000_460_000 => Some("VK_DESCRIPTOR_TYPE_TENSOR_ARM"),
        1_000_570_000 => Some("VK_DESCRIPTOR_TYPE_PARTITIONED_ACCELERATION_STRUCTURE_NV"),
        _ => None,
    };
    if let Some(name) = canonical_name {
        return name.to_owned();
    }
    string_vk_enum(desc_type, "VK_DESCRIPTOR_TYPE_", "", "VkDescriptorType")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values_include_the_vulkan_prefix() {
        assert_eq!(string_vk_result(vk::Result::SUCCESS), "VK_SUCCESS");
        assert_eq!(
            string_vk_format(vk::Format::R8G8B8A8_UNORM),
            "VK_FORMAT_R8G8B8A8_UNORM"
        );
        assert_eq!(
            string_vk_present_mode(vk::PresentModeKHR::FIFO),
            "VK_PRESENT_MODE_FIFO_KHR"
        );
        assert_eq!(
            string_vk_color_space(vk::ColorSpaceKHR::SRGB_NONLINEAR),
            "VK_COLOR_SPACE_SRGB_NONLINEAR_KHR"
        );
        assert_eq!(
            string_vk_image_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL),
            "VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL"
        );
        assert_eq!(
            string_vk_image_tiling(vk::ImageTiling::OPTIMAL),
            "VK_IMAGE_TILING_OPTIMAL"
        );
        assert_eq!(
            string_vk_physical_device_type(vk::PhysicalDeviceType::DISCRETE_GPU),
            "VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU"
        );
        assert_eq!(
            string_vk_object_type(vk::ObjectType::IMAGE),
            "VK_OBJECT_TYPE_IMAGE"
        );
        assert_eq!(
            string_vk_sharing_mode(vk::SharingMode::CONCURRENT),
            "VK_SHARING_MODE_CONCURRENT"
        );
        assert_eq!(
            string_vk_descriptor_type(vk::DescriptorType::STORAGE_BUFFER),
            "VK_DESCRIPTOR_TYPE_STORAGE_BUFFER"
        );
    }

    #[test]
    fn unknown_values_use_the_generated_helpers_fallback_text() {
        assert_eq!(
            string_vk_result(vk::Result::from_raw(i32::MAX)),
            "Unhandled VkResult"
        );
        assert_eq!(
            string_vk_format(vk::Format::from_raw(i32::MAX)),
            "Unhandled VkFormat"
        );
    }

    #[test]
    fn generated_vulkan_1_4_names_override_ash_1_3_names() {
        let results = [
            (-1_000_011_001, "VK_ERROR_VALIDATION_FAILED"),
            (-1_000_174_001, "VK_ERROR_NOT_PERMITTED"),
            (-1_000_208_000, "VK_ERROR_PRESENT_TIMING_QUEUE_FULL_EXT"),
            (1_000_482_000, "VK_INCOMPATIBLE_SHADER_BINARY_EXT"),
            (1_000_483_000, "VK_PIPELINE_BINARY_MISSING_KHR"),
            (-1_000_483_000, "VK_ERROR_NOT_ENOUGH_SPACE_KHR"),
        ];
        for (raw, expected) in results {
            assert_eq!(string_vk_result(vk::Result::from_raw(raw)), expected);
        }

        let formats = [
            (1_000_460_000, "VK_FORMAT_R8_BOOL_ARM"),
            (
                1_000_460_001,
                "VK_FORMAT_R16_SFLOAT_FPENCODING_BFLOAT16_ARM",
            ),
            (
                1_000_460_002,
                "VK_FORMAT_R8_SFLOAT_FPENCODING_FLOAT8E4M3_ARM",
            ),
            (
                1_000_460_003,
                "VK_FORMAT_R8_SFLOAT_FPENCODING_FLOAT8E5M2_ARM",
            ),
            (1_000_464_000, "VK_FORMAT_R16G16_SFIXED5_NV"),
            (1_000_470_000, "VK_FORMAT_A1B5G5R5_UNORM_PACK16"),
            (1_000_470_001, "VK_FORMAT_A8_UNORM"),
            (1_000_609_000, "VK_FORMAT_R10X6_UINT_PACK16_ARM"),
            (1_000_609_001, "VK_FORMAT_R10X6G10X6_UINT_2PACK16_ARM"),
            (
                1_000_609_002,
                "VK_FORMAT_R10X6G10X6B10X6A10X6_UINT_4PACK16_ARM",
            ),
            (1_000_609_003, "VK_FORMAT_R12X4_UINT_PACK16_ARM"),
            (1_000_609_004, "VK_FORMAT_R12X4G12X4_UINT_2PACK16_ARM"),
            (
                1_000_609_005,
                "VK_FORMAT_R12X4G12X4B12X4A12X4_UINT_4PACK16_ARM",
            ),
            (1_000_609_006, "VK_FORMAT_R14X2_UINT_PACK16_ARM"),
            (1_000_609_007, "VK_FORMAT_R14X2G14X2_UINT_2PACK16_ARM"),
            (
                1_000_609_008,
                "VK_FORMAT_R14X2G14X2B14X2A14X2_UINT_4PACK16_ARM",
            ),
            (1_000_609_009, "VK_FORMAT_R14X2_UNORM_PACK16_ARM"),
            (1_000_609_010, "VK_FORMAT_R14X2G14X2_UNORM_2PACK16_ARM"),
            (
                1_000_609_011,
                "VK_FORMAT_R14X2G14X2B14X2A14X2_UNORM_4PACK16_ARM",
            ),
            (
                1_000_609_012,
                "VK_FORMAT_G14X2_B14X2R14X2_2PLANE_420_UNORM_3PACK16_ARM",
            ),
            (
                1_000_609_013,
                "VK_FORMAT_G14X2_B14X2R14X2_2PLANE_422_UNORM_3PACK16_ARM",
            ),
        ];
        for (raw, expected) in formats {
            assert_eq!(string_vk_format(vk::Format::from_raw(raw)), expected);
        }

        assert_eq!(
            string_vk_present_mode(vk::PresentModeKHR::from_raw(1_000_361_000)),
            "VK_PRESENT_MODE_FIFO_LATEST_READY_KHR"
        );

        let layouts = [
            (1_000_232_000, "VK_IMAGE_LAYOUT_RENDERING_LOCAL_READ"),
            (1_000_460_000, "VK_IMAGE_LAYOUT_TENSOR_ALIASING_ARM"),
            (
                1_000_553_000,
                "VK_IMAGE_LAYOUT_VIDEO_ENCODE_QUANTIZATION_MAP_KHR",
            ),
            (1_000_620_000, "VK_IMAGE_LAYOUT_ZERO_INITIALIZED_EXT"),
        ];
        for (raw, expected) in layouts {
            assert_eq!(
                string_vk_image_layout(vk::ImageLayout::from_raw(raw)),
                expected
            );
        }

        let object_types = [
            (1_000_307_000, "VK_OBJECT_TYPE_CUDA_MODULE_NV"),
            (1_000_307_001, "VK_OBJECT_TYPE_CUDA_FUNCTION_NV"),
            (1_000_460_000, "VK_OBJECT_TYPE_TENSOR_ARM"),
            (1_000_460_001, "VK_OBJECT_TYPE_TENSOR_VIEW_ARM"),
            (1_000_483_000, "VK_OBJECT_TYPE_PIPELINE_BINARY_KHR"),
            (
                1_000_507_000,
                "VK_OBJECT_TYPE_DATA_GRAPH_PIPELINE_SESSION_ARM",
            ),
            (1_000_556_000, "VK_OBJECT_TYPE_EXTERNAL_COMPUTE_QUEUE_NV"),
            (1_000_572_000, "VK_OBJECT_TYPE_INDIRECT_COMMANDS_LAYOUT_EXT"),
            (1_000_572_001, "VK_OBJECT_TYPE_INDIRECT_EXECUTION_SET_EXT"),
            (1_000_607_000, "VK_OBJECT_TYPE_SHADER_INSTRUMENTATION_ARM"),
        ];
        for (raw, expected) in object_types {
            assert_eq!(
                string_vk_object_type(vk::ObjectType::from_raw(raw)),
                expected
            );
        }

        let descriptor_types = [
            (1_000_460_000, "VK_DESCRIPTOR_TYPE_TENSOR_ARM"),
            (
                1_000_570_000,
                "VK_DESCRIPTOR_TYPE_PARTITIONED_ACCELERATION_STRUCTURE_NV",
            ),
        ];
        for (raw, expected) in descriptor_types {
            assert_eq!(
                string_vk_descriptor_type(vk::DescriptorType::from_raw(raw)),
                expected
            );
        }
    }

    #[test]
    fn astc_dimensions_use_the_generated_helpers_lowercase_separator() {
        assert_eq!(
            string_vk_format(vk::Format::ASTC_10X10_UNORM_BLOCK),
            "VK_FORMAT_ASTC_10x10_UNORM_BLOCK"
        );
        assert_eq!(
            string_vk_format(vk::Format::ASTC_4X4X4_SFLOAT_BLOCK_EXT),
            "VK_FORMAT_ASTC_4x4x4_SFLOAT_BLOCK_EXT"
        );
        assert_eq!(
            string_vk_format(vk::Format::R10X6_UNORM_PACK16),
            "VK_FORMAT_R10X6_UNORM_PACK16"
        );
    }
}

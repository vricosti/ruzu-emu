// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust counterpart of the combo-box label tables in
// `/home/vricosti/Dev/emulators/eden/src/qt_common/config/shared_translation.cpp`
// (`ConfigurationShared::ComboboxEnumeration`).
//
// Upstream maps each `Settings::` enum onto an ordered list of
// `(enum value, human label)` pairs, which the configuration pages feed into
// their `QComboBox`es. The enum's *canonical* name (used for serialization)
// deliberately differs from the label shown in the UI — e.g. `NvdecEmulation::Gpu`
// canonicalizes to "Gpu" but displays as "GPU Video Decoding (Default)".
//
// Each table below is `&[(variant, label)]`, in the same order as upstream's
// initializer list, because the combo-box row order is part of the UI contract.
// Pages select a row with `shared_widget::index_of` over the variant column.
//
// Divergence: upstream keys the map on a runtime `EnumMetadata<T>::Index()` so
// its generic widget builder can look tables up dynamically. The Rust port
// exposes one `const` table per enum instead — the call sites know their enum
// statically, so the runtime indirection buys nothing.

use common::settings_enums::{
    AnisotropyMode, AntiAliasing, AppletMode, AspectRatio, AstcDecodeMode, AstcRecompression,
    AudioMode, ConfirmStop, ConsoleMode, CpuAccuracy, CpuBackend, DmaAccuracy,
    ExtendedDynamicState, FramePacingMode, FullscreenMode, GpuAccuracy, GpuFenceBehavior,
    GpuUnswizzle, GpuUnswizzleChunk, GpuUnswizzleSize, Language, MemoryLayout, NvdecEmulation,
    Region, RendererBackend, ResolutionSetup, ScalingFilter, VramUsageMode,
};

/// Split a `&[(T, &str)]` table into its label column, for feeding a combo box.
pub fn labels<T>(table: &[(T, &'static str)]) -> Vec<&'static str> {
    table.iter().map(|(_, label)| *label).collect()
}

/// Index of `value` in `table`, or 0 when the stored value isn't listed.
pub fn index_of<T: PartialEq>(table: &[(T, &'static str)], value: &T) -> u32 {
    table
        .iter()
        .position(|(variant, _)| variant == value)
        .unwrap_or(0) as u32
}

/// Variant at combo-box row `index`, falling back to the first row.
pub fn value_at<T: Copy>(table: &[(T, &'static str)], index: u32) -> T {
    table
        .get(index as usize)
        .map(|(variant, _)| *variant)
        .unwrap_or(table[0].0)
}

pub const APPLET_MODE: &[(AppletMode, &str)] = &[
    (AppletMode::HLE, "Custom frontend"),
    (AppletMode::LLE, "Real applet"),
];

pub const GPU_UNSWIZZLE_SIZE: &[(GpuUnswizzleSize, &str)] = &[
    (GpuUnswizzleSize::VerySmall, "Very Small (16 MB)"),
    (GpuUnswizzleSize::Small, "Small (32 MB)"),
    (GpuUnswizzleSize::Normal, "Normal (128 MB)"),
    (GpuUnswizzleSize::Large, "Large (256 MB)"),
    (GpuUnswizzleSize::VeryLarge, "Very Large (512 MB)"),
];

pub const GPU_UNSWIZZLE_STREAM: &[(GpuUnswizzle, &str)] = &[
    (GpuUnswizzle::VeryLow, "Very Low (4 MB)"),
    (GpuUnswizzle::Low, "Low (8 MB)"),
    (GpuUnswizzle::Normal, "Normal (16 MB)"),
    (GpuUnswizzle::Medium, "Medium (32 MB)"),
    (GpuUnswizzle::High, "High (64 MB)"),
];

pub const GPU_UNSWIZZLE_CHUNK: &[(GpuUnswizzleChunk, &str)] = &[
    (GpuUnswizzleChunk::VeryLow, "Very Low (32)"),
    (GpuUnswizzleChunk::Low, "Low (64)"),
    (GpuUnswizzleChunk::Normal, "Normal (128)"),
    (GpuUnswizzleChunk::Medium, "Medium (256)"),
    (GpuUnswizzleChunk::High, "High (512)"),
];

pub const EXTENDED_DYNAMIC_STATE: &[(ExtendedDynamicState, &str)] = &[
    (ExtendedDynamicState::Disabled, "Disabled"),
    (ExtendedDynamicState::EDS1, "ExtendedDynamicState 1"),
    (ExtendedDynamicState::EDS2, "ExtendedDynamicState 2"),
    (ExtendedDynamicState::EDS3, "ExtendedDynamicState 3"),
];

pub const ASTC_DECODE_MODE: &[(AstcDecodeMode, &str)] = &[
    (AstcDecodeMode::Cpu, "CPU"),
    (AstcDecodeMode::Gpu, "GPU"),
    (AstcDecodeMode::CpuAsynchronous, "CPU Asynchronous"),
];

pub const ASTC_RECOMPRESSION: &[(AstcRecompression, &str)] = &[
    (
        AstcRecompression::Uncompressed,
        "Uncompressed (Best quality)",
    ),
    (AstcRecompression::Bc1, "BC1 (Low quality)"),
    (AstcRecompression::Bc3, "BC3 (Medium quality)"),
];

pub const VRAM_USAGE_MODE: &[(VramUsageMode, &str)] = &[
    (VramUsageMode::Conservative, "Conservative"),
    (VramUsageMode::Aggressive, "Aggressive"),
];

pub const GRAPHICS_API: &[(RendererBackend, &str)] = &[
    (RendererBackend::Vulkan, "Vulkan"),
    #[cfg(target_os = "macos")]
    (RendererBackend::Metal, "Metal"),
    (RendererBackend::OpenGlGlsl, "OpenGL GLSL"),
    (
        RendererBackend::OpenGlGlasm,
        "OpenGL GLASM (Assembly Shaders, NVIDIA Only)",
    ),
    (
        RendererBackend::OpenGlSpirV,
        "OpenGL SPIR-V (Experimental, AMD/Mesa Only)",
    ),
    (RendererBackend::Null, "Null"),
];

pub const GPU_ACCURACY: &[(GpuAccuracy, &str)] =
    &[(GpuAccuracy::Low, "Fast"), (GpuAccuracy::High, "Accurate")];

pub const DMA_ACCURACY: &[(DmaAccuracy, &str)] = &[
    (DmaAccuracy::Default, "Default"),
    (DmaAccuracy::Unsafe, "Unsafe (fast)"),
    (DmaAccuracy::Safe, "Safe (stable)"),
];

pub const FRAME_PACING_MODE: &[(FramePacingMode, &str)] = &[
    (FramePacingMode::TargetAuto, "Auto"),
    (FramePacingMode::Target30, "30 FPS"),
    (FramePacingMode::Target60, "60 FPS"),
    (FramePacingMode::Target90, "90 FPS"),
    (FramePacingMode::Target120, "120 FPS"),
];

pub const GPU_FENCE_BEHAVIOR: &[(GpuFenceBehavior, &str)] = &[
    (GpuFenceBehavior::Default, "Default"),
    (GpuFenceBehavior::Immediate, "Immediate"),
    (GpuFenceBehavior::Balanced, "Balanced"),
    (GpuFenceBehavior::Accurate, "Accurate"),
    (GpuFenceBehavior::Strict, "Strict"),
];

pub const CPU_ACCURACY: &[(CpuAccuracy, &str)] = &[
    (CpuAccuracy::Auto, "Auto"),
    (CpuAccuracy::Accurate, "Accurate"),
    (CpuAccuracy::Unsafe, "Unsafe"),
    (
        CpuAccuracy::Paranoid,
        "Paranoid (disables most optimizations)",
    ),
];

/// Upstream's `configure_cpu.ui` only shows the "Backend:" row on targets where
/// NCE exists (ARM64 hosts), so the x86-64 dialog never renders it — but the
/// table is part of `ComboboxEnumeration` upstream, so it is kept here too.
#[allow(dead_code)]
pub const CPU_BACKEND: &[(CpuBackend, &str)] =
    &[(CpuBackend::Dynarmic, "Dynarmic"), (CpuBackend::Nce, "NCE")];

pub const FULLSCREEN_MODE: &[(FullscreenMode, &str)] = &[
    (FullscreenMode::Borderless, "Borderless Windowed"),
    (FullscreenMode::Exclusive, "Exclusive Fullscreen"),
];

pub const NVDEC_EMULATION: &[(NvdecEmulation, &str)] = &[
    (NvdecEmulation::Off, "No Video Output"),
    (NvdecEmulation::Cpu, "CPU Video Decoding"),
    (NvdecEmulation::Gpu, "GPU Video Decoding (Default)"),
];

pub const RESOLUTION_SETUP: &[(ResolutionSetup, &str)] = &[
    (ResolutionSetup::Res1_4X, "0.25X (180p/270p) [EXPERIMENTAL]"),
    (ResolutionSetup::Res1_2X, "0.5X (360p/540p) [EXPERIMENTAL]"),
    (ResolutionSetup::Res3_4X, "0.75X (540p/810p) [EXPERIMENTAL]"),
    (ResolutionSetup::Res1X, "1X (720p/1080p)"),
    (
        ResolutionSetup::Res5_4X,
        "1.25X (900p/1350p) [EXPERIMENTAL]",
    ),
    (
        ResolutionSetup::Res3_2X,
        "1.5X (1080p/1620p) [EXPERIMENTAL]",
    ),
    (ResolutionSetup::Res2X, "2X (1440p/2160p)"),
    (ResolutionSetup::Res3X, "3X (2160p/3240p)"),
    (ResolutionSetup::Res4X, "4X (2880p/4320p)"),
    (ResolutionSetup::Res5X, "5X (3600p/5400p)"),
    (ResolutionSetup::Res6X, "6X (4320p/6480p)"),
    (ResolutionSetup::Res7X, "7X (5040p/7560p)"),
    (ResolutionSetup::Res8X, "8X (5760p/8640p)"),
];

pub const SCALING_FILTER: &[(ScalingFilter, &str)] = &[
    (ScalingFilter::NearestNeighbor, "Nearest Neighbor"),
    (ScalingFilter::Bilinear, "Bilinear"),
    (ScalingFilter::Bicubic, "Bicubic"),
    (ScalingFilter::Gaussian, "Gaussian"),
    (ScalingFilter::Lanczos, "Lanczos"),
    (ScalingFilter::ScaleForce, "ScaleForce"),
    (ScalingFilter::Fsr, "AMD FidelityFX Super Resolution"),
    (ScalingFilter::Area, "Area"),
    (ScalingFilter::Mmpx, "MMPX"),
    (ScalingFilter::ZeroTangent, "Zero-Tangent"),
    (ScalingFilter::BSpline, "B-Spline"),
    (ScalingFilter::Mitchell, "Mitchell"),
    (ScalingFilter::Spline1, "Spline-1"),
    (ScalingFilter::Sgsr, "Snapdragon Game Super Resolution"),
    (
        ScalingFilter::SgsrEdge,
        "Snapdragon Game Super Resolution EdgeDir",
    ),
];

pub const ANTI_ALIASING: &[(AntiAliasing, &str)] = &[
    (AntiAliasing::None, "None"),
    (AntiAliasing::Fxaa, "FXAA"),
    (AntiAliasing::Smaa, "SMAA"),
];

pub const ASPECT_RATIO: &[(AspectRatio, &str)] = &[
    (AspectRatio::R16_9, "Default (16:9)"),
    (AspectRatio::R4_3, "Force 4:3"),
    (AspectRatio::R21_9, "Force 21:9"),
    (AspectRatio::R16_10, "Force 16:10"),
    (AspectRatio::Stretch, "Stretch to Window"),
];

pub const ANISOTROPY_MODE: &[(AnisotropyMode, &str)] = &[
    (AnisotropyMode::Automatic, "Automatic"),
    (AnisotropyMode::Default, "Default"),
    (AnisotropyMode::X2, "2x"),
    (AnisotropyMode::X4, "4x"),
    (AnisotropyMode::X8, "8x"),
    (AnisotropyMode::X16, "16x"),
    (AnisotropyMode::X32, "32x"),
    (AnisotropyMode::X64, "64x"),
    (AnisotropyMode::None, "None"),
];

pub const LANGUAGE: &[(Language, &str)] = &[
    (Language::Japanese, "Japanese (日本語)"),
    (Language::EnglishAmerican, "American English"),
    (Language::French, "French (français)"),
    (Language::German, "German (Deutsch)"),
    (Language::Italian, "Italian (italiano)"),
    (Language::Spanish, "Spanish (español)"),
    (Language::Chinese, "Chinese"),
    (Language::Korean, "Korean (한국어)"),
    (Language::Dutch, "Dutch (Nederlands)"),
    (Language::Portuguese, "Portuguese (português)"),
    (Language::Russian, "Russian (Русский)"),
    (Language::Taiwanese, "Taiwanese"),
    (Language::EnglishBritish, "British English"),
    (Language::FrenchCanadian, "Canadian French"),
    (Language::SpanishLatin, "Latin American Spanish"),
    (Language::ChineseSimplified, "Simplified Chinese"),
    (
        Language::ChineseTraditional,
        "Traditional Chinese (正體中文)",
    ),
    (
        Language::PortugueseBrazilian,
        "Brazilian Portuguese (português do Brasil)",
    ),
];

pub const REGION: &[(Region, &str)] = &[
    (Region::Japan, "Japan"),
    (Region::Usa, "USA"),
    (Region::Europe, "Europe"),
    (Region::Australia, "Australia"),
    (Region::China, "China"),
    (Region::Korea, "Korea"),
    (Region::Taiwan, "Taiwan"),
];

pub const AUDIO_MODE: &[(AudioMode, &str)] = &[
    (AudioMode::Mono, "Mono"),
    (AudioMode::Stereo, "Stereo"),
    (AudioMode::Surround, "Surround"),
];

pub const MEMORY_LAYOUT: &[(MemoryLayout, &str)] = &[
    (MemoryLayout::Memory4Gb, "4GB DRAM (Default)"),
    (MemoryLayout::Memory6Gb, "6GB DRAM (Unsafe)"),
    (MemoryLayout::Memory8Gb, "8GB DRAM"),
    (MemoryLayout::Memory10Gb, "10GB DRAM (Unsafe)"),
    (MemoryLayout::Memory12Gb, "12GB DRAM (Unsafe)"),
];

// Exact status-bar context-menu maps from
// `qt_common/config/shared_translation.h`. These deliberately remain separate
// from the configuration combobox tables above: upstream uses shorter labels
// and `std::map` iteration order for the status menus.
pub const STATUS_ANTI_ALIASING: &[(AntiAliasing, &str)] = &[
    (AntiAliasing::None, "None"),
    (AntiAliasing::Fxaa, "FXAA"),
    (AntiAliasing::Smaa, "SMAA"),
];

pub const STATUS_SCALING_FILTER: &[(ScalingFilter, &str)] = &[
    (ScalingFilter::NearestNeighbor, "Nearest"),
    (ScalingFilter::Bilinear, "Bilinear"),
    (ScalingFilter::Bicubic, "Bicubic"),
    (ScalingFilter::Gaussian, "Gaussian"),
    (ScalingFilter::Lanczos, "Lanczos"),
    (ScalingFilter::ScaleForce, "ScaleForce"),
    (ScalingFilter::Fsr, "FSR"),
    (ScalingFilter::Area, "Area"),
    (ScalingFilter::ZeroTangent, "Zero-Tangent"),
    (ScalingFilter::BSpline, "B-Spline"),
    (ScalingFilter::Mitchell, "Mitchell"),
    (ScalingFilter::Spline1, "Spline-1"),
    (ScalingFilter::Mmpx, "MMPX"),
    (ScalingFilter::Sgsr, "SGSR"),
    (ScalingFilter::SgsrEdge, "SGSR EdgeDir"),
];

pub const STATUS_CONSOLE_MODE: &[(ConsoleMode, &str)] = &[
    (ConsoleMode::Handheld, "Handheld"),
    (ConsoleMode::Docked, "Docked"),
];

pub const STATUS_GPU_ACCURACY: &[(GpuAccuracy, &str)] =
    &[(GpuAccuracy::Low, "Fast"), (GpuAccuracy::High, "Accurate")];

pub const STATUS_RENDERER_BACKEND: &[(RendererBackend, &str)] = &[
    (RendererBackend::OpenGlGlsl, "OpenGL GLSL"),
    (RendererBackend::Vulkan, "Vulkan"),
    #[cfg(target_os = "macos")]
    (RendererBackend::Metal, "Metal"),
    (RendererBackend::Null, "Null"),
    (RendererBackend::OpenGlGlasm, "OpenGL GLASM"),
    (RendererBackend::OpenGlSpirV, "OpenGL SPIRV"),
];

pub const CONFIRM_STOP: &[(ConfirmStop, &str)] = &[
    (ConfirmStop::AskAlways, "Always ask (Default)"),
    (
        ConfirmStop::AskBasedOnGame,
        "Only if game specifies not to stop",
    ),
    (ConfirmStop::AskNever, "Never ask"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_ordered_like_upstream() {
        // Row order is part of the UI contract; upstream lists GPU last for
        // NVDEC and marks it "(Default)".
        assert_eq!(NVDEC_EMULATION[2].1, "GPU Video Decoding (Default)");
        assert_eq!(RESOLUTION_SETUP[3].1, "1X (720p/1080p)");
        assert_eq!(MEMORY_LAYOUT[2].1, "8GB DRAM");
        assert_eq!(MEMORY_LAYOUT[3].0, MemoryLayout::Memory10Gb);
        assert_eq!(MEMORY_LAYOUT[4].0, MemoryLayout::Memory12Gb);
        assert_eq!(labels(GPU_ACCURACY), vec!["Fast", "Accurate"]);
        assert_eq!(
            labels(DMA_ACCURACY),
            vec!["Default", "Unsafe (fast)", "Safe (stable)"]
        );
        assert_eq!(
            labels(FRAME_PACING_MODE),
            vec!["Auto", "30 FPS", "60 FPS", "90 FPS", "120 FPS"]
        );
        assert_eq!(
            labels(SCALING_FILTER),
            vec![
                "Nearest Neighbor",
                "Bilinear",
                "Bicubic",
                "Gaussian",
                "Lanczos",
                "ScaleForce",
                "AMD FidelityFX Super Resolution",
                "Area",
                "MMPX",
                "Zero-Tangent",
                "B-Spline",
                "Mitchell",
                "Spline-1",
                "Snapdragon Game Super Resolution",
                "Snapdragon Game Super Resolution EdgeDir",
            ]
        );
    }

    #[test]
    fn index_of_round_trips_through_value_at() {
        let idx = index_of(ASPECT_RATIO, &AspectRatio::R21_9);
        assert_eq!(idx, 2);
        assert_eq!(value_at(ASPECT_RATIO, idx), AspectRatio::R21_9);
    }

    #[test]
    fn index_of_falls_back_to_first_row() {
        assert_eq!(index_of(&[], &ScalingFilter::Bilinear), 0);
    }

    #[test]
    fn labels_extracts_the_display_column() {
        let l = labels(CPU_BACKEND);
        assert_eq!(l, vec!["Dynarmic", "NCE"]);
    }

    #[test]
    fn status_context_maps_match_upstream_std_map_order_and_labels() {
        assert_eq!(labels(STATUS_ANTI_ALIASING), vec!["None", "FXAA", "SMAA"]);
        assert_eq!(labels(STATUS_CONSOLE_MODE), vec!["Handheld", "Docked"]);
        assert_eq!(labels(STATUS_GPU_ACCURACY), vec!["Fast", "Accurate"]);
        assert_eq!(
            labels(STATUS_RENDERER_BACKEND),
            vec![
                "OpenGL GLSL",
                "Vulkan",
                "Null",
                "OpenGL GLASM",
                "OpenGL SPIRV",
            ]
        );
        assert_eq!(STATUS_SCALING_FILTER.len(), 15);
        assert_eq!(
            STATUS_SCALING_FILTER[8],
            (ScalingFilter::ZeroTangent, "Zero-Tangent")
        );
        assert_eq!(STATUS_SCALING_FILTER[12], (ScalingFilter::Mmpx, "MMPX"));
    }
}

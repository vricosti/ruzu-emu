//! Port of zuyu/src/common/settings_enums.h
//! Status: COMPLET
//! Derniere synchro: 2026-03-05

/// Macro to generate enum types with string canonicalization support.
/// Each enum is `#[repr(u32)]` to match the C++ `enum class : u32`.
macro_rules! settings_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $($variant:ident),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(u32)]
        $vis enum $name {
            $($variant),+
        }

        impl $name {
            /// Returns the string name of this enum variant.
            pub fn canonicalize(self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($variant)),+
                }
            }

            /// Parse a variant from its string name (case-sensitive).
            pub fn from_string(s: &str) -> Option<Self> {
                match s {
                    $(stringify!($variant) => Some(Self::$variant),)+
                    _ => None,
                }
            }

            /// Parse a variant from its numeric value.
            pub fn from_u32(val: u32) -> Option<Self> {
                let mut _idx = 0u32;
                $(
                    if val == _idx {
                        return Some(Self::$variant);
                    }
                    _idx += 1;
                )+
                None
            }

            /// Returns all variants as a slice of (name, value) pairs.
            pub fn canonicalizations() -> &'static [(&'static str, Self)] {
                &[
                    $((stringify!($variant), Self::$variant)),+
                ]
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.canonicalize())
            }
        }

        impl std::str::FromStr for $name {
            type Err = ();

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::from_string(value)
                    .or_else(|| value.parse::<u32>().ok().and_then(Self::from_u32))
                    .ok_or(())
            }
        }

        impl crate::settings_setting::SettingType for $name {
            fn to_config_string(&self) -> String {
                (*self as u32).to_string()
            }

            fn canonicalize_value(&self) -> String {
                self.canonicalize().to_string()
            }

            fn is_enum_type() -> bool {
                true
            }
        }

        impl Default for $name {
            fn default() -> Self {
                // First variant is the default
                settings_enum!(@first $($variant),+)
            }
        }
    };
    (@first $first:ident $(, $rest:ident)*) => {
        Self::$first
    };
}

// AudioEngine has special canonicalizations (lowercase), defined separately
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum AudioEngine {
    Auto = 0,
    Cubeb = 1,
    Sdl3 = 2,
    Null = 3,
    Oboe = 4,
}

impl AudioEngine {
    pub fn canonicalize(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cubeb => "cubeb",
            Self::Sdl3 => "sdl3",
            Self::Null => "null",
            Self::Oboe => "oboe",
        }
    }

    pub fn from_string(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "cubeb" => Some(Self::Cubeb),
            "sdl3" => Some(Self::Sdl3),
            "null" => Some(Self::Null),
            "oboe" => Some(Self::Oboe),
            _ => None,
        }
    }

    pub fn from_u32(val: u32) -> Option<Self> {
        match val {
            0 => Some(Self::Auto),
            1 => Some(Self::Cubeb),
            2 => Some(Self::Sdl3),
            3 => Some(Self::Null),
            4 => Some(Self::Oboe),
            _ => None,
        }
    }
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::Auto
    }
}

impl std::fmt::Display for AudioEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.canonicalize())
    }
}

impl std::str::FromStr for AudioEngine {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_string(value)
            .or_else(|| value.parse::<u32>().ok().and_then(Self::from_u32))
            .ok_or(())
    }
}

impl crate::settings_setting::SettingType for AudioEngine {
    fn to_config_string(&self) -> String {
        (*self as u32).to_string()
    }

    fn canonicalize_value(&self) -> String {
        self.canonicalize().to_string()
    }

    fn is_enum_type() -> bool {
        true
    }
}

settings_enum! {
    pub enum AudioMode {
        Mono,
        Stereo,
        Surround,
    }
}

settings_enum! {
    pub enum Language {
        Japanese,
        EnglishAmerican,
        French,
        German,
        Italian,
        Spanish,
        Chinese,
        Korean,
        Dutch,
        Portuguese,
        Russian,
        Taiwanese,
        EnglishBritish,
        FrenchCanadian,
        SpanishLatin,
        ChineseSimplified,
        ChineseTraditional,
        PortugueseBrazilian,
    }
}

settings_enum! {
    pub enum Region {
        Japan,
        Usa,
        Europe,
        Australia,
        China,
        Korea,
        Taiwan,
    }
}

settings_enum! {
    pub enum TimeZone {
        Auto, Default, Cet, Cst6Cdt, Cuba, Eet, Egypt, Eire, Est, Est5Edt, Gb, GbEire, Gmt,
        GmtPlusZero, GmtMinusZero, GmtZero, Greenwich, Hongkong, Hst, Iceland, Iran, Israel,
        Jamaica, Japan, Kwajalein, Libya, Met, Mst, Mst7Mdt, Navajo, Nz, NzChat, Poland,
        Portugal, Prc, Pst8Pdt, Roc, Rok, Singapore, Turkey, Uct, Universal, Utc, WSu, Wet, Zulu,
    }
}

settings_enum! {
    pub enum AnisotropyMode {
        Automatic,
        Default,
        X2,
        X4,
        X8,
        X16,
        X32,
        X64,
        None,
    }
}

settings_enum! {
    pub enum AstcDecodeMode {
        Cpu,
        Gpu,
        CpuAsynchronous,
    }
}

settings_enum! {
    pub enum AstcRecompression {
        Uncompressed,
        Bc1,
        Bc3,
    }
}

settings_enum! {
    pub enum FramePacingMode {
        Target_Auto,
        Target_30,
        Target_60,
        Target_90,
        Target_120,
    }
}

settings_enum! {
    pub enum DmaAccuracy {
        Default,
        Unsafe,
        Safe,
    }
}

settings_enum! {
    pub enum VSyncMode {
        Immediate,
        Mailbox,
        Fifo,
        FifoRelaxed,
    }
}

settings_enum! {
    pub enum VramUsageMode {
        Conservative,
        Aggressive,
    }
}

settings_enum! {
    /// Upstream `Settings::GpuLogLevel`.
    pub enum GpuLogLevel {
        Off,
        Errors,
        Standard,
        Verbose,
        All,
    }
}

/// Upstream `Settings::RendererBackend`.
///
/// The Rust identifiers follow Rust casing, while the canonical strings retain
/// Eden's exact `OpenGL_*` spellings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum RendererBackend {
    OpenGlGlsl = 0,
    Vulkan = 1,
    Null = 2,
    OpenGlGlasm = 3,
    OpenGlSpirV = 4,
}

impl RendererBackend {
    pub fn canonicalize(self) -> &'static str {
        match self {
            Self::OpenGlGlsl => "OpenGL_GLSL",
            Self::Vulkan => "Vulkan",
            Self::Null => "Null",
            Self::OpenGlGlasm => "OpenGL_GLASM",
            Self::OpenGlSpirV => "OpenGL_SPIRV",
        }
    }

    pub fn from_string(value: &str) -> Option<Self> {
        match value {
            "OpenGL_GLSL" => Some(Self::OpenGlGlsl),
            "Vulkan" => Some(Self::Vulkan),
            "Null" => Some(Self::Null),
            "OpenGL_GLASM" => Some(Self::OpenGlGlasm),
            "OpenGL_SPIRV" => Some(Self::OpenGlSpirV),
            _ => None,
        }
    }

    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::OpenGlGlsl),
            1 => Some(Self::Vulkan),
            2 => Some(Self::Null),
            3 => Some(Self::OpenGlGlasm),
            4 => Some(Self::OpenGlSpirV),
            _ => None,
        }
    }

    pub fn canonicalizations() -> &'static [(&'static str, Self)] {
        &[
            ("OpenGL_GLSL", Self::OpenGlGlsl),
            ("Vulkan", Self::Vulkan),
            ("Null", Self::Null),
            ("OpenGL_GLASM", Self::OpenGlGlasm),
            ("OpenGL_SPIRV", Self::OpenGlSpirV),
        ]
    }
}

impl std::fmt::Display for RendererBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.canonicalize())
    }
}

impl std::str::FromStr for RendererBackend {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_string(value)
            .or_else(|| value.parse::<u32>().ok().and_then(Self::from_u32))
            .ok_or(())
    }
}

impl crate::settings_setting::SettingType for RendererBackend {
    fn to_config_string(&self) -> String {
        (*self as u32).to_string()
    }

    fn canonicalize_value(&self) -> String {
        self.canonicalize().to_string()
    }

    fn is_enum_type() -> bool {
        true
    }
}

impl Default for RendererBackend {
    fn default() -> Self {
        Self::OpenGlGlsl
    }
}

settings_enum! {
    pub enum GpuAccuracy {
        Low,
        High,
    }
}

settings_enum! {
    pub enum GpuFenceBehavior {
        Default,
        Immediate,
        Balanced,
        Accurate,
        Strict,
    }
}

settings_enum! {
    pub enum CpuBackend {
        Dynarmic,
        Nce,
    }
}

settings_enum! {
    pub enum CpuAccuracy {
        Auto,
        Accurate,
        Unsafe,
        Paranoid,
    }
}

settings_enum! {
    pub enum CpuClock {
        Normal,
        Boost,
        Overclock,
    }
}

settings_enum! {
    pub enum GpuClock {
        Normal,
        Boost,
        Overclock,
    }
}

settings_enum! {
    pub enum GpuUnswizzleSize {
        VerySmall,
        Small,
        Normal,
        Large,
        VeryLarge,
    }
}

settings_enum! {
    pub enum GpuUnswizzle {
        VeryLow,
        Low,
        Normal,
        Medium,
        High,
    }
}

settings_enum! {
    pub enum GpuUnswizzleChunk {
        VeryLow,
        Low,
        Normal,
        Medium,
        High,
    }
}

settings_enum! {
    pub enum ExtendedDynamicState {
        Disabled,
        EDS1,
        EDS2,
        EDS3,
    }
}

settings_enum! {
    pub enum SpeedMode {
        Standard,
        Turbo,
        Slow,
    }
}

settings_enum! {
    pub enum MemoryLayout {
        Memory4Gb,
        Memory6Gb,
        Memory8Gb,
        Memory10Gb,
        Memory12Gb,
    }
}

settings_enum! {
    pub enum ConfirmStop {
        AskAlways,
        AskBasedOnGame,
        AskNever,
    }
}

settings_enum! {
    pub enum FullscreenMode {
        Borderless,
        Exclusive,
    }
}

settings_enum! {
    pub enum NvdecEmulation {
        Off,
        Cpu,
        Gpu,
    }
}

settings_enum! {
    pub enum ResolutionSetup {
        Res1_4X,
        Res1_2X,
        Res3_4X,
        Res1X,
        Res5_4X,
        Res3_2X,
        Res2X,
        Res3X,
        Res4X,
        Res5X,
        Res6X,
        Res7X,
        Res8X,
    }
}

settings_enum! {
    pub enum ScalingFilter {
        NearestNeighbor,
        Bilinear,
        Bicubic,
        Gaussian,
        Lanczos,
        ScaleForce,
        Fsr,
        Area,
        ZeroTangent,
        BSpline,
        Mitchell,
        Spline1,
        Mmpx,
        Sgsr,
        SgsrEdge,
    }
}

settings_enum! {
    pub enum AntiAliasing {
        None,
        Fxaa,
        Smaa,
    }
}

settings_enum! {
    pub enum AspectRatio {
        R16_9,
        R4_3,
        R21_9,
        R16_10,
        Stretch,
    }
}

settings_enum! {
    pub enum ConsoleMode {
        Handheld,
        Docked,
    }
}

settings_enum! {
    pub enum AppletMode {
        HLE,
        LLE,
    }
}

/// Category for settings, matching the C++ `enum class Category : u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u32)]
pub enum Category {
    Android = 0,
    Audio,
    Core,
    Cpu,
    CpuDebug,
    CpuUnsafe,
    Overlay,
    Renderer,
    RendererAdvanced,
    RendererHacks,
    RendererExtensions,
    RendererDebug,
    System,
    SystemAudio,
    DataStorage,
    Debugging,
    DebuggingGraphics,
    GpuDriver,
    Miscellaneous,
    Network,
    WebService,
    AddOns,
    Controls,
    Ui,
    UiAudio,
    UiGeneral,
    UiLayout,
    UiGameList,
    Screenshots,
    Shortcuts,
    Multiplayer,
    Services,
    Paths,
    Linux,
    LibraryApplet,
    MaxEnum,
}

impl Category {
    /// Translate a category into its INI section name, matching the C++ `TranslateCategory`.
    pub fn translate(&self) -> &'static str {
        match self {
            Category::Android => "Android",
            Category::Audio => "Audio",
            Category::Core => "Core",
            Category::Cpu | Category::CpuDebug | Category::CpuUnsafe => "Cpu",
            Category::Overlay => "Overlay",
            Category::Renderer
            | Category::RendererAdvanced
            | Category::RendererHacks
            | Category::RendererExtensions
            | Category::RendererDebug => "Renderer",
            Category::System | Category::SystemAudio => "System",
            Category::DataStorage => "Data Storage",
            Category::Debugging | Category::DebuggingGraphics => "Debugging",
            Category::GpuDriver => "GpuDriver",
            Category::LibraryApplet => "LibraryApplet",
            Category::Miscellaneous => "Miscellaneous",
            Category::Network => "Network",
            Category::WebService => "WebService",
            Category::AddOns => "DisabledAddOns",
            Category::Controls => "Controls",
            Category::Ui | Category::UiGeneral => "UI",
            Category::UiAudio => "UiAudio",
            Category::UiLayout => "UILayout",
            Category::UiGameList => "UIGameList",
            Category::Screenshots => "Screenshots",
            Category::Shortcuts => "Shortcuts",
            Category::Multiplayer => "Multiplayer",
            Category::Services => "Services",
            Category::Paths => "Paths",
            Category::Linux => "Linux",
            Category::MaxEnum => "Miscellaneous",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GpuAccuracy, RendererBackend, ScalingFilter};
    use std::str::FromStr;

    #[test]
    fn gpu_accuracy_has_the_two_upstream_levels_and_numeric_values() {
        assert_eq!(GpuAccuracy::Low as u32, 0);
        assert_eq!(GpuAccuracy::High as u32, 1);
        assert_eq!(
            GpuAccuracy::canonicalizations(),
            &[("Low", GpuAccuracy::Low), ("High", GpuAccuracy::High)]
        );
        assert_eq!(GpuAccuracy::from_str("0"), Ok(GpuAccuracy::Low));
        assert_eq!(GpuAccuracy::from_str("1"), Ok(GpuAccuracy::High));
        assert!(GpuAccuracy::from_str("2").is_err());
    }

    #[test]
    fn renderer_backend_discriminants_match_eden_fused_enum() {
        let expected = [
            RendererBackend::OpenGlGlsl,
            RendererBackend::Vulkan,
            RendererBackend::Null,
            RendererBackend::OpenGlGlasm,
            RendererBackend::OpenGlSpirV,
        ];
        for (index, backend) in expected.into_iter().enumerate() {
            assert_eq!(backend as u32, index as u32);
            assert_eq!(RendererBackend::from_u32(index as u32), Some(backend));
        }
        assert_eq!(expected[0].canonicalize(), "OpenGL_GLSL");
        assert_eq!(expected[3].canonicalize(), "OpenGL_GLASM");
        assert_eq!(expected[4].canonicalize(), "OpenGL_SPIRV");
        assert_eq!(RendererBackend::from_str("OpenGL_GLSL"), Ok(expected[0]));
        assert_eq!(RendererBackend::from_str("OpenGL_GLASM"), Ok(expected[3]));
        assert_eq!(RendererBackend::from_str("OpenGL_SPIRV"), Ok(expected[4]));
    }

    #[test]
    fn scaling_filter_discriminants_match_upstream_serialization() {
        let expected = [
            ScalingFilter::NearestNeighbor,
            ScalingFilter::Bilinear,
            ScalingFilter::Bicubic,
            ScalingFilter::Gaussian,
            ScalingFilter::Lanczos,
            ScalingFilter::ScaleForce,
            ScalingFilter::Fsr,
            ScalingFilter::Area,
            ScalingFilter::ZeroTangent,
            ScalingFilter::BSpline,
            ScalingFilter::Mitchell,
            ScalingFilter::Spline1,
            ScalingFilter::Mmpx,
            ScalingFilter::Sgsr,
            ScalingFilter::SgsrEdge,
        ];
        for (index, filter) in expected.into_iter().enumerate() {
            assert_eq!(filter as u32, index as u32);
            assert_eq!(ScalingFilter::from_u32(index as u32), Some(filter));
        }
        assert_eq!(ScalingFilter::SgsrEdge as u32, expected.len() as u32 - 1);
    }
}

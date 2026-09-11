// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/textures/texture.h` and `texture.cpp`.
//!
//! Tegra texture descriptor definitions: TIC/TSC entries, texture formats,
//! sampler state, and associated hash implementations.

use std::hash::{Hash, Hasher};

use common::cityhash::city_hash64;

// ── Texture Format ───────────────────────────────────────────────────────────

/// Tegra texture format encoded in bits [6:0] of TIC word 0.
///
/// Port of `Tegra::Texture::TextureFormat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TextureFormat {
    R32G32B32A32 = 0x01,
    R32G32B32 = 0x02,
    R16G16B16A16 = 0x03,
    R32G32 = 0x04,
    R32B24G8 = 0x05,
    Etc2Rgb = 0x06,
    X8B8G8R8 = 0x07,
    A8B8G8R8 = 0x08,
    A2B10G10R10 = 0x09,
    Etc2RgbPta = 0x0a,
    Etc2Rgba = 0x0b,
    R16G16 = 0x0c,
    G8R24 = 0x0d,
    G24R8 = 0x0e,
    R32 = 0x0f,
    Bc6hS16 = 0x10,
    Bc6hU16 = 0x11,
    A4B4G4R4 = 0x12,
    A5B5G5R1 = 0x13,
    A1B5G5R5 = 0x14,
    B5G6R5 = 0x15,
    B6G5R5 = 0x16,
    Bc7u = 0x17,
    G8R8 = 0x18,
    Eac = 0x19,
    Eacx2 = 0x1a,
    R16 = 0x1b,
    Y8Video = 0x1c,
    R8 = 0x1d,
    G4R4 = 0x1e,
    R1 = 0x1f,
    E5B9G9R9 = 0x20,
    B10G11R11 = 0x21,
    G8B8G8R8 = 0x22,
    B8G8R8G8 = 0x23,
    Dxt1 = 0x24,
    Dxt23 = 0x25,
    Dxt45 = 0x26,
    Dxn1 = 0x27,
    Dxn2 = 0x28,
    Z24S8 = 0x29,
    X8Z24 = 0x2a,
    S8Z24 = 0x2b,
    X4V4Z24Cov4R4V = 0x2c,
    X4V4Z24Cov8R8V = 0x2d,
    V8Z24Cov4R12V = 0x2e,
    Z32 = 0x2f,
    Z32X24S8 = 0x30,
    X8Z24X20V4S8Cov4R4V = 0x31,
    X8Z24X20V4S8Cov8R8V = 0x32,
    Z32X20V4X8Cov4R4V = 0x33,
    Z32X20V4X8Cov8R8V = 0x34,
    Z32X20V4S8Cov4R4V = 0x35,
    Z32X20V4S8Cov8R8V = 0x36,
    X8Z24X16V8S8Cov4R12V = 0x37,
    Z32X16V8X8Cov4R12V = 0x38,
    Z32X16V8S8Cov4R12V = 0x39,
    Z16 = 0x3a,
    V8Z24Cov8R24V = 0x3b,
    X8Z24X16V8S8Cov8R24V = 0x3c,
    Z32X16V8X8Cov8R24V = 0x3d,
    Z32X16V8S8Cov8R24V = 0x3e,
    Astc2d4x4 = 0x40,
    Astc2d5x5 = 0x41,
    Astc2d6x6 = 0x42,
    Astc2d8x8 = 0x44,
    Astc2d10x10 = 0x45,
    Astc2d12x12 = 0x46,
    Astc2d5x4 = 0x50,
    Astc2d6x5 = 0x51,
    Astc2d8x6 = 0x52,
    Astc2d10x8 = 0x53,
    Astc2d12x10 = 0x54,
    Astc2d8x5 = 0x55,
    Astc2d10x5 = 0x56,
    Astc2d10x6 = 0x57,
}

impl TextureFormat {
    pub fn from_raw(v: u32) -> Option<Self> {
        // Use a match to validate known discriminants.
        match v {
            0x01 => Some(Self::R32G32B32A32),
            0x02 => Some(Self::R32G32B32),
            0x03 => Some(Self::R16G16B16A16),
            0x04 => Some(Self::R32G32),
            0x05 => Some(Self::R32B24G8),
            0x06 => Some(Self::Etc2Rgb),
            0x07 => Some(Self::X8B8G8R8),
            0x08 => Some(Self::A8B8G8R8),
            0x09 => Some(Self::A2B10G10R10),
            0x0a => Some(Self::Etc2RgbPta),
            0x0b => Some(Self::Etc2Rgba),
            0x0c => Some(Self::R16G16),
            0x0d => Some(Self::G8R24),
            0x0e => Some(Self::G24R8),
            0x0f => Some(Self::R32),
            0x10 => Some(Self::Bc6hS16),
            0x11 => Some(Self::Bc6hU16),
            0x12 => Some(Self::A4B4G4R4),
            0x13 => Some(Self::A5B5G5R1),
            0x14 => Some(Self::A1B5G5R5),
            0x15 => Some(Self::B5G6R5),
            0x16 => Some(Self::B6G5R5),
            0x17 => Some(Self::Bc7u),
            0x18 => Some(Self::G8R8),
            0x19 => Some(Self::Eac),
            0x1a => Some(Self::Eacx2),
            0x1b => Some(Self::R16),
            0x1c => Some(Self::Y8Video),
            0x1d => Some(Self::R8),
            0x1e => Some(Self::G4R4),
            0x1f => Some(Self::R1),
            0x20 => Some(Self::E5B9G9R9),
            0x21 => Some(Self::B10G11R11),
            0x22 => Some(Self::G8B8G8R8),
            0x23 => Some(Self::B8G8R8G8),
            0x24 => Some(Self::Dxt1),
            0x25 => Some(Self::Dxt23),
            0x26 => Some(Self::Dxt45),
            0x27 => Some(Self::Dxn1),
            0x28 => Some(Self::Dxn2),
            0x29 => Some(Self::Z24S8),
            0x2a => Some(Self::X8Z24),
            0x2b => Some(Self::S8Z24),
            0x2c => Some(Self::X4V4Z24Cov4R4V),
            0x2d => Some(Self::X4V4Z24Cov8R8V),
            0x2e => Some(Self::V8Z24Cov4R12V),
            0x2f => Some(Self::Z32),
            0x30 => Some(Self::Z32X24S8),
            0x31 => Some(Self::X8Z24X20V4S8Cov4R4V),
            0x32 => Some(Self::X8Z24X20V4S8Cov8R8V),
            0x33 => Some(Self::Z32X20V4X8Cov4R4V),
            0x34 => Some(Self::Z32X20V4X8Cov8R8V),
            0x35 => Some(Self::Z32X20V4S8Cov4R4V),
            0x36 => Some(Self::Z32X20V4S8Cov8R8V),
            0x37 => Some(Self::X8Z24X16V8S8Cov4R12V),
            0x38 => Some(Self::Z32X16V8X8Cov4R12V),
            0x39 => Some(Self::Z32X16V8S8Cov4R12V),
            0x3a => Some(Self::Z16),
            0x3b => Some(Self::V8Z24Cov8R24V),
            0x3c => Some(Self::X8Z24X16V8S8Cov8R24V),
            0x3d => Some(Self::Z32X16V8X8Cov8R24V),
            0x3e => Some(Self::Z32X16V8S8Cov8R24V),
            0x40 => Some(Self::Astc2d4x4),
            0x41 => Some(Self::Astc2d5x5),
            0x42 => Some(Self::Astc2d6x6),
            0x44 => Some(Self::Astc2d8x8),
            0x45 => Some(Self::Astc2d10x10),
            0x46 => Some(Self::Astc2d12x12),
            0x50 => Some(Self::Astc2d5x4),
            0x51 => Some(Self::Astc2d6x5),
            0x52 => Some(Self::Astc2d8x6),
            0x53 => Some(Self::Astc2d10x8),
            0x54 => Some(Self::Astc2d12x10),
            0x55 => Some(Self::Astc2d8x5),
            0x56 => Some(Self::Astc2d10x5),
            0x57 => Some(Self::Astc2d10x6),
            _ => None,
        }
    }
}

// ── Texture Type ─────────────────────────────────────────────────────────────

/// Texture dimensionality / type.
///
/// Port of `Tegra::Texture::TextureType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TextureType {
    Texture1D = 0,
    Texture2D = 1,
    Texture3D = 2,
    TextureCubemap = 3,
    Texture1DArray = 4,
    Texture2DArray = 5,
    Texture1DBuffer = 6,
    Texture2DNoMipmap = 7,
    TextureCubeArray = 8,
}

impl TextureType {
    pub fn from_raw(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Texture1D),
            1 => Some(Self::Texture2D),
            2 => Some(Self::Texture3D),
            3 => Some(Self::TextureCubemap),
            4 => Some(Self::Texture1DArray),
            5 => Some(Self::Texture2DArray),
            6 => Some(Self::Texture1DBuffer),
            7 => Some(Self::Texture2DNoMipmap),
            8 => Some(Self::TextureCubeArray),
            _ => None,
        }
    }
}

// ── TIC Header Version ───────────────────────────────────────────────────────

/// TIC header version — determines the address layout interpretation.
///
/// Port of `Tegra::Texture::TICHeaderVersion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TicHeaderVersion {
    OneDBuffer = 0,
    PitchColorKey = 1,
    Pitch = 2,
    BlockLinear = 3,
    BlockLinearColorKey = 4,
}

impl TicHeaderVersion {
    pub fn from_raw(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::OneDBuffer),
            1 => Some(Self::PitchColorKey),
            2 => Some(Self::Pitch),
            3 => Some(Self::BlockLinear),
            4 => Some(Self::BlockLinearColorKey),
            _ => None,
        }
    }
}

// ── Component Type ───────────────────────────────────────────────────────────

/// Per-channel component type in the TIC.
///
/// Port of `Tegra::Texture::ComponentType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum ComponentType {
    Snorm = 1,
    Unorm = 2,
    Sint = 3,
    Uint = 4,
    SnormForceFp16 = 5,
    UnormForceFp16 = 6,
    Float = 7,
}

impl ComponentType {
    pub fn from_raw(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::Snorm),
            2 => Some(Self::Unorm),
            3 => Some(Self::Sint),
            4 => Some(Self::Uint),
            5 => Some(Self::SnormForceFp16),
            6 => Some(Self::UnormForceFp16),
            7 => Some(Self::Float),
            _ => None,
        }
    }
}

// ── Swizzle Source ───────────────────────────────────────────────────────────

/// Channel swizzle source selector.
///
/// Port of `Tegra::Texture::SwizzleSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum SwizzleSource {
    Zero = 0,
    /// Raw TIC value not named by the hardware documentation.
    Invalid = 1,
    R = 2,
    G = 3,
    B = 4,
    A = 5,
    OneInt = 6,
    OneFloat = 7,
}

impl SwizzleSource {
    pub fn from_raw(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Zero),
            1 => Some(Self::Invalid),
            2 => Some(Self::R),
            3 => Some(Self::G),
            4 => Some(Self::B),
            5 => Some(Self::A),
            6 => Some(Self::OneInt),
            7 => Some(Self::OneFloat),
            _ => None,
        }
    }
}

// ── MSAA Mode ────────────────────────────────────────────────────────────────

/// Multi-sample anti-aliasing mode.
///
/// Port of `Tegra::Texture::MsaaMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum MsaaMode {
    Msaa1x1 = 0,
    Msaa2x1 = 1,
    Msaa2x2 = 2,
    Msaa4x2 = 3,
    Msaa4x2D3d = 4,
    Msaa2x1D3d = 5,
    Msaa4x4 = 6,
    Msaa2x2Vc4 = 8,
    Msaa2x2Vc12 = 9,
    Msaa4x2Vc8 = 10,
    Msaa4x2Vc24 = 11,
}

impl MsaaMode {
    pub fn from_raw(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Msaa1x1),
            1 => Some(Self::Msaa2x1),
            2 => Some(Self::Msaa2x2),
            3 => Some(Self::Msaa4x2),
            4 => Some(Self::Msaa4x2D3d),
            5 => Some(Self::Msaa2x1D3d),
            6 => Some(Self::Msaa4x4),
            8 => Some(Self::Msaa2x2Vc4),
            9 => Some(Self::Msaa2x2Vc12),
            10 => Some(Self::Msaa4x2Vc8),
            11 => Some(Self::Msaa4x2Vc24),
            _ => None,
        }
    }
}

// ── Texture Handle ───────────────────────────────────────────────────────────

/// Combined TIC/TSC handle packed into a single `u32`.
///
/// Port of `Tegra::Texture::TextureHandle`.
///
/// Layout:
///   bits [19:0]  — `tic_id`
///   bits [31:20] — `tsc_id`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureHandle {
    pub raw: u32,
}

impl TextureHandle {
    pub const fn new(raw: u32) -> Self {
        Self { raw }
    }

    pub const fn tic_id(&self) -> u32 {
        self.raw & 0xF_FFFF // bits [19:0]
    }

    pub const fn tsc_id(&self) -> u32 {
        (self.raw >> 20) & 0xFFF // bits [31:20]
    }
}

/// Splits a raw texture handle into `(tic_id, tsc_id)`.
///
/// Port of `Tegra::Texture::TexturePair`.
pub fn texture_pair(raw: u32, via_header_index: bool) -> (u32, u32) {
    if via_header_index {
        (raw, raw)
    } else {
        let handle = TextureHandle::new(raw);
        (handle.tic_id(), handle.tsc_id())
    }
}

// ── TIC Entry ────────────────────────────────────────────────────────────────

/// Texture Image Control entry — 32 bytes (0x20).
///
/// Port of `Tegra::Texture::TICEntry`.
///
/// This is a raw bitfield union in C++; we store the backing `[u64; 4]` and
/// provide accessor methods that extract fields via bit manipulation.
///
/// `Default` is needed for `DescriptorTable<TicEntry>` (zero-initialized
/// sentinel). `PartialEq`/`Eq`/`Hash` are provided as manual impls below
/// because they compare on the `raw` field with a custom hash.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct TicEntry {
    pub raw: [u64; 4],
}

// Upstream: `static_assert(sizeof(TICEntry) == 0x20)`
const _: () = assert!(std::mem::size_of::<TicEntry>() == 0x20);

impl TicEntry {
    /// Word 0 (low 32 bits of raw[0]).
    fn word0(&self) -> u32 {
        self.raw[0] as u32
    }
    /// Word 1 (high 32 bits of raw[0]).
    fn word1(&self) -> u32 {
        (self.raw[0] >> 32) as u32
    }
    /// Word 2 (low 32 bits of raw[1]).
    fn word2(&self) -> u32 {
        self.raw[1] as u32
    }
    /// Word 3 (low 32 bits of raw[1] >> 32).
    fn word3(&self) -> u32 {
        (self.raw[1] >> 32) as u32
    }
    /// Word 4 (low 32 bits of raw[2]).
    fn word4(&self) -> u32 {
        self.raw[2] as u32
    }
    /// Word 5 (high 32 bits of raw[2]).
    fn word5(&self) -> u32 {
        (self.raw[2] >> 32) as u32
    }
    /// Word 6 (low 32 bits of raw[3]).
    fn word6(&self) -> u32 {
        self.raw[3] as u32
    }
    /// Word 7 (high 32 bits of raw[3]).
    fn word7(&self) -> u32 {
        (self.raw[3] >> 32) as u32
    }

    // ── Word 0 fields ──

    pub fn format(&self) -> u32 {
        self.word0() & 0x7F // bits [6:0]
    }

    pub fn r_type(&self) -> u32 {
        (self.word0() >> 7) & 0x7 // bits [9:7]
    }

    pub fn g_type(&self) -> u32 {
        (self.word0() >> 10) & 0x7 // bits [12:10]
    }

    pub fn b_type(&self) -> u32 {
        (self.word0() >> 13) & 0x7 // bits [15:13]
    }

    pub fn a_type(&self) -> u32 {
        (self.word0() >> 16) & 0x7 // bits [18:16]
    }

    pub fn x_source(&self) -> u32 {
        (self.word0() >> 19) & 0x7 // bits [21:19]
    }

    pub fn y_source(&self) -> u32 {
        (self.word0() >> 22) & 0x7 // bits [24:22]
    }

    pub fn z_source(&self) -> u32 {
        (self.word0() >> 25) & 0x7 // bits [27:25]
    }

    pub fn w_source(&self) -> u32 {
        (self.word0() >> 28) & 0x7 // bits [30:28]
    }

    // ── Word 1 — address_low ──

    pub fn address_low(&self) -> u32 {
        self.word1()
    }

    // ── Word 2 fields ──

    pub fn address_high(&self) -> u32 {
        self.word2() & 0xFFFF // bits [15:0]
    }

    pub fn layer_base_3_7(&self) -> u32 {
        (self.word2() >> 16) & 0x1F // bits [20:16]
    }

    pub fn header_version(&self) -> u32 {
        (self.word2() >> 21) & 0x7 // bits [23:21]
    }

    pub fn load_store_hint(&self) -> u32 {
        (self.word2() >> 24) & 0x1 // bit 24
    }

    pub fn view_coherency_hash(&self) -> u32 {
        (self.word2() >> 25) & 0xF // bits [28:25]
    }

    pub fn layer_base_8_10(&self) -> u32 {
        (self.word2() >> 29) & 0x7 // bits [31:29]
    }

    // ── Word 3 fields ──

    pub fn block_width(&self) -> u32 {
        self.word3() & 0x7 // bits [2:0]
    }

    pub fn block_height(&self) -> u32 {
        (self.word3() >> 3) & 0x7 // bits [5:3]
    }

    pub fn block_depth(&self) -> u32 {
        (self.word3() >> 6) & 0x7 // bits [8:6]
    }

    pub fn tile_width_spacing(&self) -> u32 {
        (self.word3() >> 10) & 0x7 // bits [12:10]
    }

    /// High 16 bits of the pitch value (overlaps block_width etc. in PitchColorKey mode).
    pub fn pitch_high(&self) -> u32 {
        self.word3() & 0xFFFF // bits [15:0]
    }

    pub fn use_header_opt_control(&self) -> u32 {
        (self.word3() >> 26) & 0x1 // bit 26
    }

    pub fn depth_texture(&self) -> u32 {
        (self.word3() >> 27) & 0x1 // bit 27
    }

    pub fn max_mip_level(&self) -> u32 {
        (self.word3() >> 28) & 0xF // bits [31:28]
    }

    /// Buffer mode: high 16 bits of (width - 1).
    pub fn buffer_high_width_minus_one(&self) -> u32 {
        self.word3() & 0xFFFF // bits [15:0]
    }

    // ── Word 4 fields ──

    pub fn width_minus_one(&self) -> u32 {
        self.word4() & 0xFFFF // bits [15:0]
    }

    pub fn layer_base_0_2(&self) -> u32 {
        (self.word4() >> 16) & 0x7 // bits [18:16]
    }

    pub fn srgb_conversion(&self) -> u32 {
        (self.word4() >> 22) & 0x1 // bit 22
    }

    pub fn texture_type(&self) -> u32 {
        (self.word4() >> 23) & 0xF // bits [26:23]
    }

    pub fn border_size(&self) -> u32 {
        (self.word4() >> 29) & 0x7 // bits [31:29]
    }

    /// Buffer mode: low 16 bits of (width - 1).
    pub fn buffer_low_width_minus_one(&self) -> u32 {
        self.word4() & 0xFFFF // bits [15:0]
    }

    // ── Word 5 fields ──

    pub fn height_minus_1(&self) -> u32 {
        self.word5() & 0xFFFF // bits [15:0]
    }

    pub fn depth_minus_1(&self) -> u32 {
        (self.word5() >> 16) & 0x3FFF // bits [29:16]
    }

    pub fn is_sparse(&self) -> u32 {
        (self.word5() >> 30) & 0x1 // bit 30
    }

    pub fn normalized_coords(&self) -> u32 {
        (self.word5() >> 31) & 0x1 // bit 31
    }

    // ── Word 6 fields ──

    pub fn mip_lod_bias(&self) -> u32 {
        (self.word6() >> 6) & 0x1FFF // bits [18:6]
    }

    pub fn max_anisotropy(&self) -> u32 {
        (self.word6() >> 27) & 0x7 // bits [29:27]
    }

    // ── Word 7 fields ──

    pub fn res_min_mip_level(&self) -> u32 {
        self.word7() & 0xF // bits [3:0]
    }

    pub fn res_max_mip_level(&self) -> u32 {
        (self.word7() >> 4) & 0xF // bits [7:4]
    }

    pub fn msaa_mode(&self) -> u32 {
        (self.word7() >> 8) & 0xF // bits [11:8]
    }

    pub fn min_lod_clamp(&self) -> u32 {
        (self.word7() >> 12) & 0xFFF // bits [23:12]
    }

    // ── Derived methods (matching upstream TICEntry methods) ──

    /// Port of `TICEntry::Address()`.
    pub fn address(&self) -> u64 {
        ((self.address_high() as u64) << 32) | (self.address_low() as u64)
    }

    /// Port of `TICEntry::Pitch()`.
    pub fn pitch(&self) -> u32 {
        let hv = self.header_version();
        assert!(
            hv == TicHeaderVersion::Pitch as u32 || hv == TicHeaderVersion::PitchColorKey as u32
        );
        // The pitch value is 21 bits, and is 32B aligned.
        self.pitch_high() << 5
    }

    /// Port of `TICEntry::Width()`.
    pub fn width(&self) -> u32 {
        if self.header_version() != TicHeaderVersion::OneDBuffer as u32 {
            self.width_minus_one() + 1
        } else {
            ((self.buffer_high_width_minus_one() << 16) | self.buffer_low_width_minus_one()) + 1
        }
    }

    /// Port of `TICEntry::Height()`.
    pub fn height(&self) -> u32 {
        self.height_minus_1() + 1
    }

    /// Port of `TICEntry::Depth()`.
    pub fn depth(&self) -> u32 {
        self.depth_minus_1() + 1
    }

    /// Port of `TICEntry::BaseLayer()`.
    pub fn base_layer(&self) -> u32 {
        self.layer_base_0_2() | (self.layer_base_3_7() << 3) | (self.layer_base_8_10() << 8)
    }

    /// Port of `TICEntry::IsBlockLinear()`.
    pub fn is_block_linear(&self) -> bool {
        let hv = self.header_version();
        hv == TicHeaderVersion::BlockLinear as u32
            || hv == TicHeaderVersion::BlockLinearColorKey as u32
    }

    /// Port of `TICEntry::IsPitchLinear()`.
    pub fn is_pitch_linear(&self) -> bool {
        let hv = self.header_version();
        hv == TicHeaderVersion::Pitch as u32 || hv == TicHeaderVersion::PitchColorKey as u32
    }

    /// Port of `TICEntry::IsBuffer()`.
    pub fn is_buffer(&self) -> bool {
        self.header_version() == TicHeaderVersion::OneDBuffer as u32
    }
}

impl PartialEq for TicEntry {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl Eq for TicEntry {}

impl Hash for TicEntry {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Port of `std::hash<TICEntry>` in texture.cpp.
        // SAFETY: `TicEntry` is a `repr(C)` wrapper over one fully initialized
        // `[u64; 4]` field, with its 0x20-byte size asserted above.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                self as *const Self as *const u8,
                std::mem::size_of::<Self>(),
            )
        };
        state.write_u64(city_hash64(bytes));
    }
}

// ── Wrap Mode ────────────────────────────────────────────────────────────────

/// Texture wrap mode for sampler state.
///
/// Port of `Tegra::Texture::WrapMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum WrapMode {
    Wrap = 0,
    Mirror = 1,
    ClampToEdge = 2,
    Border = 3,
    Clamp = 4,
    MirrorOnceClampToEdge = 5,
    MirrorOnceBorder = 6,
    MirrorOnceClampOgl = 7,
}

// ── Depth Compare Func ───────────────────────────────────────────────────────

/// Depth comparison function for shadow samplers.
///
/// Port of `Tegra::Texture::DepthCompareFunc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum DepthCompareFunc {
    Never = 0,
    Less = 1,
    Equal = 2,
    LessEqual = 3,
    Greater = 4,
    NotEqual = 5,
    GreaterEqual = 6,
    Always = 7,
}

impl DepthCompareFunc {
    /// Decode the three-bit TSC depth-comparison field.
    pub const fn from_raw(raw: u32) -> Self {
        match raw & 0x7 {
            0 => Self::Never,
            1 => Self::Less,
            2 => Self::Equal,
            3 => Self::LessEqual,
            4 => Self::Greater,
            5 => Self::NotEqual,
            6 => Self::GreaterEqual,
            _ => Self::Always,
        }
    }
}

// ── Texture Filter ───────────────────────────────────────────────────────────

/// Texture minification / magnification filter.
///
/// Port of `Tegra::Texture::TextureFilter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TextureFilter {
    Nearest = 1,
    Linear = 2,
}

// ── Texture Mipmap Filter ────────────────────────────────────────────────────

/// Mipmap filter mode.
///
/// Port of `Tegra::Texture::TextureMipmapFilter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TextureMipmapFilter {
    None = 1,
    Nearest = 2,
    Linear = 3,
}

// ── Sampler Reduction ────────────────────────────────────────────────────────

/// Sampler reduction filter mode.
///
/// Port of `Tegra::Texture::SamplerReduction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum SamplerReduction {
    WeightedAverage = 0,
    Min = 1,
    Max = 2,
}

// ── Anisotropy ───────────────────────────────────────────────────────────────

/// Anisotropic filtering level setting.
///
/// Port of `Tegra::Texture::Anisotropy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Anisotropy {
    Default,
    Filter2x,
    Filter4x,
    Filter8x,
    Filter16x,
}

// ── TSC Entry ────────────────────────────────────────────────────────────────

/// Texture Sampler Control entry — 32 bytes (0x20).
///
/// Port of `Tegra::Texture::TSCEntry`.
///
/// `Default` is needed for `DescriptorTable<TscEntry>` (zero-initialized
/// sentinel). `PartialEq`/`Eq`/`Hash` are provided as manual impls below.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct TscEntry {
    pub raw: [u64; 4],
}

// Upstream: `static_assert(sizeof(TSCEntry) == 0x20)`
const _: () = assert!(std::mem::size_of::<TscEntry>() == 0x20);

impl TscEntry {
    fn word0(&self) -> u32 {
        self.raw[0] as u32
    }
    fn word1(&self) -> u32 {
        (self.raw[0] >> 32) as u32
    }
    fn word2(&self) -> u32 {
        self.raw[1] as u32
    }
    fn word3(&self) -> u32 {
        (self.raw[1] >> 32) as u32
    }

    // ── Word 0 fields ──

    pub fn wrap_u(&self) -> u32 {
        self.word0() & 0x7 // bits [2:0]
    }

    pub fn wrap_v(&self) -> u32 {
        (self.word0() >> 3) & 0x7 // bits [5:3]
    }

    pub fn wrap_p(&self) -> u32 {
        (self.word0() >> 6) & 0x7 // bits [8:6]
    }

    pub fn depth_compare_enabled(&self) -> u32 {
        (self.word0() >> 9) & 0x1 // bit 9
    }

    pub fn depth_compare_func(&self) -> u32 {
        (self.word0() >> 10) & 0x7 // bits [12:10]
    }

    pub fn srgb_conversion(&self) -> u32 {
        (self.word0() >> 13) & 0x1 // bit 13
    }

    pub fn max_anisotropy_raw(&self) -> u32 {
        (self.word0() >> 20) & 0x7 // bits [22:20]
    }

    // ── Word 1 fields ──

    pub fn mag_filter(&self) -> u32 {
        self.word1() & 0x3 // bits [1:0]
    }

    pub fn min_filter(&self) -> u32 {
        (self.word1() >> 4) & 0x3 // bits [5:4]
    }

    pub fn mipmap_filter(&self) -> u32 {
        (self.word1() >> 6) & 0x3 // bits [7:6]
    }

    pub fn cubemap_anisotropy(&self) -> u32 {
        (self.word1() >> 8) & 0x1 // bit 8
    }

    pub fn cubemap_interface_filtering(&self) -> u32 {
        (self.word1() >> 9) & 0x1 // bit 9
    }

    pub fn reduction_filter(&self) -> u32 {
        (self.word1() >> 10) & 0x3 // bits [11:10]
    }

    pub fn mip_lod_bias_raw(&self) -> u32 {
        (self.word1() >> 12) & 0x1FFF // bits [24:12]
    }

    pub fn float_coord_normalization(&self) -> u32 {
        (self.word1() >> 25) & 0x1 // bit 25
    }

    pub fn trilin_opt(&self) -> u32 {
        (self.word1() >> 26) & 0x1F // bits [30:26]
    }

    // ── Word 2 fields ──

    pub fn min_lod_clamp(&self) -> u32 {
        self.word2() & 0xFFF // bits [11:0]
    }

    pub fn max_lod_clamp(&self) -> u32 {
        (self.word2() >> 12) & 0xFFF // bits [23:12]
    }

    pub fn srgb_border_color_r(&self) -> u32 {
        (self.word2() >> 24) & 0xFF // bits [31:24]
    }

    // ── Word 3 fields ──

    pub fn srgb_border_color_g(&self) -> u32 {
        (self.word3() >> 12) & 0xFF // bits [19:12]
    }

    pub fn srgb_border_color_b(&self) -> u32 {
        (self.word3() >> 20) & 0xFF // bits [27:20]
    }

    /// Border color as `[f32; 4]`, stored in raw[2..4] offset by the first 4 words.
    ///
    /// In the C++ union, `border_color` occupies words 4-7 (bytes 16-31).
    pub fn border_color(&self) -> [f32; 4] {
        let w4 = self.raw[2] as u32;
        let w5 = (self.raw[2] >> 32) as u32;
        let w6 = self.raw[3] as u32;
        let w7 = (self.raw[3] >> 32) as u32;
        [
            f32::from_bits(w4),
            f32::from_bits(w5),
            f32::from_bits(w6),
            f32::from_bits(w7),
        ]
    }

    // ── Derived methods ──

    /// Port of `TSCEntry::BorderColor()`.
    ///
    /// Upstream TODO: Handle SRGB correctly. Using SRGB conversion breaks shadows in
    /// some games (Xenoblade). Upstream has the sRGB conversion commented out.
    pub fn computed_border_color(&self) -> [f32; 4] {
        self.border_color()
    }

    /// Port of `TSCEntry::SrgbBorderColor()`: the linearized sRGB border color,
    /// alpha kept from the float border color.
    pub fn srgb_border_color(&self) -> [f32; 4] {
        [
            srgb_to_linear(self.srgb_border_color_r()),
            srgb_to_linear(self.srgb_border_color_g()),
            srgb_to_linear(self.srgb_border_color_b()),
            self.border_color()[3],
        ]
    }

    /// Port of `TSCEntry::MaxAnisotropy()`.
    pub fn computed_max_anisotropy(&self) -> f32 {
        let is_suitable_mipmap_filter = self.mipmap_filter() != TextureMipmapFilter::None as u32;
        let has_regular_lods = self.min_lod_clamp() == 0 && self.max_lod_clamp() >= 256;
        let is_bilinear_filter = self.min_filter() == TextureFilter::Linear as u32
            && self.reduction_filter() == SamplerReduction::WeightedAverage as u32;
        let max_anisotropy = self.max_anisotropy_raw();
        if max_anisotropy == 0
            && (!is_suitable_mipmap_filter
                || !has_regular_lods
                || !is_bilinear_filter
                || self.depth_compare_enabled() != 0)
        {
            return 1.0;
        }
        let settings = common::settings::values();
        let anisotropy_mode = *settings.max_anisotropy.get_value();
        let added_anisotropy: i32 = match anisotropy_mode {
            common::settings_enums::AnisotropyMode::Default
            | common::settings_enums::AnisotropyMode::X2
            | common::settings_enums::AnisotropyMode::X4
            | common::settings_enums::AnisotropyMode::X8
            | common::settings_enums::AnisotropyMode::X16 => anisotropy_mode as i32 - 1,
            common::settings_enums::AnisotropyMode::Automatic => {
                let resolution_scale =
                    settings.resolution_info.up_scale >> settings.resolution_info.down_shift;
                if resolution_scale > 1 {
                    (resolution_scale - 1) as i32
                } else {
                    0
                }
            }
        };
        (1u32 << (max_anisotropy as i32 + added_anisotropy) as u32) as f32
    }

    /// Port of `TSCEntry::MinLod()`.
    pub fn min_lod(&self) -> f32 {
        self.min_lod_clamp() as f32 / 256.0
    }

    /// Port of `TSCEntry::MaxLod()`.
    pub fn max_lod(&self) -> f32 {
        self.max_lod_clamp() as f32 / 256.0
    }

    /// Port of `TSCEntry::LodBias()`.
    pub fn lod_bias(&self) -> f32 {
        // Sign extend the 13-bit value.
        let mask: u32 = 1 << (13 - 1);
        let raw = self.mip_lod_bias_raw();
        let sign_extended = ((raw ^ mask).wrapping_sub(mask)) as i32;
        sign_extended as f32 / 256.0
    }
}

impl PartialEq for TscEntry {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl Eq for TscEntry {}

impl Hash for TscEntry {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Port of `std::hash<TSCEntry>` in texture.cpp.
        // SAFETY: `TscEntry` is a `repr(C)` wrapper over one fully initialized
        // `[u64; 4]` field, with its 0x20-byte size asserted above.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                self as *const Self as *const u8,
                std::mem::size_of::<Self>(),
            )
        };
        state.write_u64(city_hash64(bytes));
    }
}

// ── SRGB Conversion LUT ─────────────────────────────────────────────────────

/// Port of `SrgbToLinear`: decode one 8-bit sRGB channel to linear.
pub fn srgb_to_linear(value: u32) -> f32 {
    let encoded = value as f32 / 255.0;
    if encoded <= 0.04045 {
        return encoded / 12.92;
    }
    ((encoded + 0.055) / 1.055).powf(2.4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::hash::BuildIdentityHasher;
    use std::hash::BuildHasher;

    fn descriptor_hash<T: Hash>(value: &T) -> u64 {
        let mut hasher = BuildIdentityHasher.build_hasher();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn texture_handle_layout() {
        assert_eq!(std::mem::size_of::<TextureHandle>(), 4);
        let h = TextureHandle::new(0xFFF_FFFFF);
        assert_eq!(h.tic_id(), 0xF_FFFF);
        assert_eq!(h.tsc_id(), 0xFFF);
    }

    #[test]
    fn texture_pair_via_header() {
        assert_eq!(texture_pair(42, true), (42, 42));
    }

    #[test]
    fn texture_pair_split() {
        // tic_id = 0x12345 (20 bits), tsc_id = 0xABC (12 bits)
        let raw = 0xABC_12345u32;
        let (tic, tsc) = texture_pair(raw, false);
        assert_eq!(tic, 0x12345);
        assert_eq!(tsc, 0xABC);
    }

    #[test]
    fn tic_entry_size() {
        assert_eq!(std::mem::size_of::<TicEntry>(), 0x20);
    }

    #[test]
    fn tsc_entry_size() {
        assert_eq!(std::mem::size_of::<TscEntry>(), 0x20);
    }

    #[test]
    fn tic_and_tsc_hashes_match_upstream_cityhash64() {
        let zero = [0; 4];
        let patterned = [
            0x0123_4567_89ab_cdef,
            0xfedc_ba98_7654_3210,
            0x0f1e_2d3c_4b5a_6978,
            0x8877_6655_4433_2211,
        ];

        assert_eq!(
            descriptor_hash(&TicEntry { raw: zero }),
            0x7087_0616_03e5_3293
        );
        assert_eq!(
            descriptor_hash(&TicEntry { raw: patterned }),
            0x0b30_55df_79c6_67b2
        );
        assert_eq!(
            descriptor_hash(&TscEntry { raw: zero }),
            0x7087_0616_03e5_3293
        );
        assert_eq!(
            descriptor_hash(&TscEntry { raw: patterned }),
            0x0b30_55df_79c6_67b2
        );
    }

    #[test]
    fn tsc_lod_bias_sign_extend() {
        // Construct a TSCEntry with mip_lod_bias = 0x1000 (negative in 13-bit signed)
        // mip_lod_bias is in word1 bits [24:12]
        let mut entry = TscEntry { raw: [0; 4] };
        // Set bits [24:12] of word1 (high 32 bits of raw[0]) to 0x1000
        let word1 = 0x1000u32 << 12;
        entry.raw[0] |= (word1 as u64) << 32;
        let bias = entry.lod_bias();
        assert!(bias < 0.0, "Expected negative bias, got {}", bias);
    }

    #[test]
    fn tsc_max_anisotropy_rejects_unsuitable_zero_descriptor() {
        let entry = TscEntry::default();
        assert_eq!(entry.computed_max_anisotropy(), 1.0);
    }

    #[test]
    fn tsc_max_anisotropy_combines_guest_and_configured_values() {
        let mut entry = TscEntry::default();
        let word0 = 2u32 << 20;
        let word1 =
            ((TextureFilter::Linear as u32) << 4) | ((TextureMipmapFilter::Nearest as u32) << 6);
        let word2 = 256u32 << 12;
        entry.raw[0] = word0 as u64 | ((word1 as u64) << 32);
        entry.raw[1] = word2 as u64;

        let previous_mode = {
            let mut settings = common::settings::values_mut();
            let previous = *settings.max_anisotropy.get_value();
            settings
                .max_anisotropy
                .set_value(common::settings_enums::AnisotropyMode::X4);
            previous
        };
        let result = entry.computed_max_anisotropy();
        common::settings::values_mut()
            .max_anisotropy
            .set_value(previous_mode);

        // Guest exponent 2 plus forced X4 exponent 2 gives 2^(2 + 2).
        assert_eq!(result, 16.0);
    }
}

#[cfg(test)]
mod srgb_border_color_tests {
    use super::srgb_to_linear;

    #[test]
    fn srgb_to_linear_matches_upstream_formula() {
        assert_eq!(srgb_to_linear(0), 0.0);
        assert!((srgb_to_linear(255) - 1.0).abs() < 1e-6);
        // Linear segment below the 0.04045 knee: encoded / 12.92.
        assert!((srgb_to_linear(10) - (10.0 / 255.0) / 12.92).abs() < 1e-7);
        // Gamma segment: ((encoded + 0.055) / 1.055) ^ 2.4.
        assert!((srgb_to_linear(128) - 0.215_860_5).abs() < 1e-5);
        assert!((0..255).all(|v| srgb_to_linear(v) < srgb_to_linear(v + 1)));
    }
}

// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! IR type system for shader recompiler.
//!
//! Types use a bitmask approach matching zuyu's `type.h` — each type is a distinct
//! flag so that type compatibility can be checked with bitwise OR/AND.

use std::fmt;

/// IR types using bitmask flags for compatibility checking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Type {
    Void = 0,
    /// Generic instruction result (untyped SSA value).
    Opaque = 1 << 0,
    /// Register reference (R0-R254, RZ=255).
    Reg = 1 << 1,
    /// Predicate reference (P0-P6, PT=7).
    Pred = 1 << 2,
    /// Shader attribute reference.
    Attribute = 1 << 3,
    /// Tessellation patch reference.
    Patch = 1 << 4,
    /// 1-bit boolean.
    U1 = 1 << 5,
    /// 8-bit unsigned integer.
    U8 = 1 << 6,
    /// 16-bit unsigned integer.
    U16 = 1 << 7,
    /// 32-bit unsigned integer.
    U32 = 1 << 8,
    /// 64-bit unsigned integer.
    U64 = 1 << 9,
    /// 16-bit float.
    F16 = 1 << 10,
    /// 32-bit float.
    F32 = 1 << 11,
    /// 64-bit float.
    F64 = 1 << 12,
    /// 2-component u32 vector.
    U32x2 = 1 << 13,
    /// 3-component u32 vector.
    U32x3 = 1 << 14,
    /// 4-component u32 vector.
    U32x4 = 1 << 15,
    /// 2-component f16 vector.
    F16x2 = 1 << 16,
    /// 3-component f16 vector.
    F16x3 = 1 << 17,
    /// 4-component f16 vector.
    F16x4 = 1 << 18,
    /// 2-component f32 vector.
    F32x2 = 1 << 19,
    /// 3-component f32 vector.
    F32x3 = 1 << 20,
    /// 4-component f32 vector.
    F32x4 = 1 << 21,
    /// 2-component f64 vector.
    F64x2 = 1 << 22,
    /// 3-component f64 vector.
    F64x3 = 1 << 23,
    /// 4-component f64 vector.
    F64x4 = 1 << 24,
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Type::Void => "void",
            Type::Opaque => "opaque",
            Type::Reg => "reg",
            Type::Pred => "pred",
            Type::Attribute => "attr",
            Type::Patch => "patch",
            Type::U1 => "u1",
            Type::U8 => "u8",
            Type::U16 => "u16",
            Type::U32 => "u32",
            Type::U64 => "u64",
            Type::F16 => "f16",
            Type::F32 => "f32",
            Type::F64 => "f64",
            Type::U32x2 => "u32x2",
            Type::U32x3 => "u32x3",
            Type::U32x4 => "u32x4",
            Type::F16x2 => "f16x2",
            Type::F16x3 => "f16x3",
            Type::F16x4 => "f16x4",
            Type::F32x2 => "f32x2",
            Type::F32x3 => "f32x3",
            Type::F32x4 => "f32x4",
            Type::F64x2 => "f64x2",
            Type::F64x3 => "f64x3",
            Type::F64x4 => "f64x4",
        };
        write!(f, "{}", name)
    }
}

impl Type {
    /// Decode one concrete IR type flag without accepting combined masks.
    pub fn from_raw(value: u32) -> Option<Self> {
        Some(match value {
            x if x == Self::Void as u32 => Self::Void,
            x if x == Self::Opaque as u32 => Self::Opaque,
            x if x == Self::Reg as u32 => Self::Reg,
            x if x == Self::Pred as u32 => Self::Pred,
            x if x == Self::Attribute as u32 => Self::Attribute,
            x if x == Self::Patch as u32 => Self::Patch,
            x if x == Self::U1 as u32 => Self::U1,
            x if x == Self::U8 as u32 => Self::U8,
            x if x == Self::U16 as u32 => Self::U16,
            x if x == Self::U32 as u32 => Self::U32,
            x if x == Self::U64 as u32 => Self::U64,
            x if x == Self::F16 as u32 => Self::F16,
            x if x == Self::F32 as u32 => Self::F32,
            x if x == Self::F64 as u32 => Self::F64,
            x if x == Self::U32x2 as u32 => Self::U32x2,
            x if x == Self::U32x3 as u32 => Self::U32x3,
            x if x == Self::U32x4 as u32 => Self::U32x4,
            x if x == Self::F16x2 as u32 => Self::F16x2,
            x if x == Self::F16x3 as u32 => Self::F16x3,
            x if x == Self::F16x4 as u32 => Self::F16x4,
            x if x == Self::F32x2 as u32 => Self::F32x2,
            x if x == Self::F32x3 as u32 => Self::F32x3,
            x if x == Self::F32x4 as u32 => Self::F32x4,
            x if x == Self::F64x2 as u32 => Self::F64x2,
            x if x == Self::F64x3 as u32 => Self::F64x3,
            x if x == Self::F64x4 as u32 => Self::F64x4,
            _ => return None,
        })
    }

    /// Check if this is a scalar integer type.
    pub fn is_integer(self) -> bool {
        matches!(
            self,
            Type::U1 | Type::U8 | Type::U16 | Type::U32 | Type::U64
        )
    }

    /// Check if this is a scalar float type.
    pub fn is_float(self) -> bool {
        matches!(self, Type::F16 | Type::F32 | Type::F64)
    }

    /// Check if this is a vector type.
    pub fn is_vector(self) -> bool {
        (self as u32) >= (Type::U32x2 as u32)
    }

    /// Check if two types are compatible.
    /// Upstream: `AreTypesCompatible(Type lhs, Type rhs)` (type.cpp:33-35).
    /// Types are compatible if they're equal or either is Opaque.
    pub fn is_compatible_with(self, other: Type) -> bool {
        self == other || self == Type::Opaque || other == Type::Opaque
    }

    /// Bit width of scalar types (0 for non-scalar).
    pub fn bit_width(self) -> u32 {
        match self {
            Type::U1 => 1,
            Type::U8 => 8,
            Type::U16 | Type::F16 => 16,
            Type::U32 | Type::F32 => 32,
            Type::U64 | Type::F64 => 64,
            _ => 0,
        }
    }
}

/// Shader stage.
///
/// Re-exported from `crate::stage::Stage` so the recompiler has a single
/// `ShaderStage` type that matches upstream `Shader::Stage` exactly
/// (7 variants, with `VertexA` and `VertexB` distinct as upstream
/// `shader_recompiler/stage.h`). The previous 6-variant Rust-port
/// `enum ShaderStage` was deleted as part of the type unification pass.
pub use crate::stage::Stage as ShaderStage;

/// Floating-point rounding mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FpRounding {
    /// Don't care / default.
    #[default]
    DontCare,
    /// Round to nearest even.
    RN,
    /// Round toward negative infinity.
    RM,
    /// Round toward positive infinity.
    RP,
    /// Round toward zero.
    RZ,
}

/// Flush-to-zero / flush-multiply-to-zero mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FmzMode {
    /// Don't care / default.
    #[default]
    DontCare,
    /// Flush denorms to zero.
    FTZ,
    /// Flush multiply-add denorms to zero.
    FMZ,
    /// No flush.
    None,
}

/// Floating-point control flags packed into instruction flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FpControl {
    pub no_contraction: bool,
    pub rounding: FpRounding,
    pub fmz_mode: FmzMode,
}

impl FpControl {
    pub fn to_u32(self) -> u32 {
        let mut v = 0u32;
        if self.no_contraction {
            v |= 1;
        }
        v |= (self.rounding as u32) << 1;
        v |= (self.fmz_mode as u32) << 4;
        v
    }

    pub fn from_u32(v: u32) -> Self {
        Self {
            no_contraction: v & 1 != 0,
            rounding: match (v >> 1) & 0x7 {
                1 => FpRounding::RN,
                2 => FpRounding::RM,
                3 => FpRounding::RP,
                4 => FpRounding::RZ,
                _ => FpRounding::DontCare,
            },
            fmz_mode: match (v >> 4) & 0x7 {
                1 => FmzMode::FTZ,
                2 => FmzMode::FMZ,
                3 => FmzMode::None,
                _ => FmzMode::DontCare,
            },
        }
    }
}

/// Texture type for texture instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureType {
    Texture1D,
    Texture2D,
    Texture3D,
    TextureCube,
    Array1D,
    Array2D,
    ArrayCube,
    Shadow2D,
    ShadowArray2D,
}

/// Texture instruction info packed into instruction flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TextureInstInfo {
    /// Texture descriptor index.
    pub descriptor_index: u16,
    /// Texture type.
    pub texture_type: u8,
    /// Is depth texture.
    pub is_depth: bool,
    /// Has bias.
    pub has_bias: bool,
    /// Has LOD clamp.
    pub has_lod_clamp: bool,
    /// Use relaxed precision for image/texture operations.
    pub relaxed_precision: bool,
    /// Gather component.
    pub gather_component: u8,
    /// Number of derivatives.
    pub num_derivatives: u8,
    /// Image format.
    pub image_format: u8,
    /// Whether TEX.NDV mode is active.
    pub ndv_is_active: bool,
    /// Whether the sampled texture returns integer components.
    pub is_integer: bool,
}

impl TextureInstInfo {
    pub fn to_u32(self) -> u32 {
        let mut v = self.descriptor_index as u32;
        v |= (self.texture_type as u32) << 16;
        v |= (self.is_depth as u32) << 19;
        v |= (self.has_bias as u32) << 20;
        v |= (self.has_lod_clamp as u32) << 21;
        v |= (self.relaxed_precision as u32) << 22;
        v |= (self.gather_component as u32 & 0x3) << 23;
        v |= (self.num_derivatives as u32 & 0x3) << 25;
        v |= (self.image_format as u32 & 0x7) << 27;
        v |= (self.ndv_is_active as u32) << 30;
        v |= (self.is_integer as u32) << 31;
        v
    }

    pub fn from_u32(v: u32) -> Self {
        Self {
            descriptor_index: (v & 0xFFFF) as u16,
            texture_type: ((v >> 16) & 0x7) as u8,
            is_depth: (v >> 19) & 1 != 0,
            has_bias: (v >> 20) & 1 != 0,
            has_lod_clamp: (v >> 21) & 1 != 0,
            relaxed_precision: (v >> 22) & 1 != 0,
            gather_component: ((v >> 23) & 0x3) as u8,
            num_derivatives: ((v >> 25) & 0x3) as u8,
            image_format: ((v >> 27) & 0x7) as u8,
            ndv_is_active: (v >> 30) & 1 != 0,
            is_integer: (v >> 31) & 1 != 0,
        }
    }
}

/// Output topology for geometry shaders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputTopology {
    PointList,
    LineStrip,
    TriangleStrip,
}

#[cfg(test)]
mod tests {
    use super::TextureInstInfo;

    #[test]
    fn texture_inst_info_matches_upstream_bit_layout() {
        let info = TextureInstInfo {
            descriptor_index: 0x1234,
            texture_type: 5,
            is_depth: true,
            has_bias: true,
            has_lod_clamp: true,
            relaxed_precision: true,
            gather_component: 2,
            num_derivatives: 3,
            image_format: 6,
            ndv_is_active: true,
            is_integer: true,
        };

        let raw = info.to_u32();
        assert_eq!(raw & 0xffff, 0x1234);
        assert_eq!((raw >> 16) & 0x7, 5);
        assert_eq!((raw >> 19) & 1, 1);
        assert_eq!((raw >> 20) & 1, 1);
        assert_eq!((raw >> 21) & 1, 1);
        assert_eq!((raw >> 22) & 1, 1);
        assert_eq!((raw >> 23) & 0x3, 2);
        assert_eq!((raw >> 25) & 0x3, 3);
        assert_eq!((raw >> 27) & 0x7, 6);
        assert_eq!((raw >> 30) & 1, 1);
        assert_eq!((raw >> 31) & 1, 1);
        assert_eq!(TextureInstInfo::from_u32(raw), info);
    }
}

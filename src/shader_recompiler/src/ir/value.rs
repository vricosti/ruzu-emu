// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! IR Value type — tagged union of SSA instruction results, register/predicate
//! references, attribute references, and immediates.
//!
//! Matches zuyu's `value.h` Value class.

use std::fmt;

/// Reference to an IR instruction by index within a block's instruction list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstRef {
    /// Block index within the program.
    pub block: u32,
    /// Instruction index within the block.
    pub inst: u32,
}

/// Maxwell GPU register (R0-R254, RZ=255).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reg(pub u8);

impl Reg {
    /// Zero register — reads as 0, writes are discarded.
    pub const RZ: Reg = Reg(255);
    /// Number of user-accessible registers.
    pub const NUM_USER_REGS: usize = 255;
    /// Total register count including RZ.
    pub const NUM_REGS: usize = 256;

    pub fn index(self) -> usize {
        self.0 as usize
    }

    pub fn is_zero(self) -> bool {
        self.0 == 255
    }
}

impl fmt::Display for Reg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_zero() {
            write!(f, "RZ")
        } else {
            write!(f, "R{}", self.0)
        }
    }
}

/// Maxwell predicate register (P0-P6, PT=7 always true).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pred(pub u8);

impl Pred {
    /// Always-true predicate.
    pub const PT: Pred = Pred(7);
    pub const NUM_USER_PREDS: usize = 7;
    pub const NUM_PREDS: usize = 8;

    pub fn index(self) -> usize {
        self.0 as usize
    }

    pub fn is_true(self) -> bool {
        self.0 == 7
    }
}

impl fmt::Display for Pred {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_true() {
            write!(f, "PT")
        } else {
            write!(f, "P{}", self.0)
        }
    }
}

/// Shader attribute (inputs/outputs, system values).
///
/// Matches zuyu's attribute.h layout:
/// - 0-23: System values
/// - 24: PrimitiveId
/// - 25: Layer
/// - 26: ViewportIndex
/// - 27: PointSize
/// - 28-31: Position X/Y/Z/W
/// - 32-159: Generic0..31 X/Y/Z/W (32 attrs × 4 components)
/// - 160-175: Front/back fixed-function colors
/// - 176-183: ClipDistance0..7
/// - 184-185: Point sprite S/T
/// - 188-189: Tessellation evaluation point U/V
/// - 190-191: InstanceId/VertexId
/// - 232: ViewportMask
/// - 255: FrontFace
/// - 256+: Implementation-specific
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Attribute(pub u32);

impl Attribute {
    pub const PRIMITIVE_ID: Attribute = Attribute(24);
    pub const LAYER: Attribute = Attribute(25);
    pub const VIEWPORT_INDEX: Attribute = Attribute(26);
    pub const POINT_SIZE: Attribute = Attribute(27);
    pub const POSITION_X: Attribute = Attribute(28);
    pub const POSITION_Y: Attribute = Attribute(29);
    pub const POSITION_Z: Attribute = Attribute(30);
    pub const POSITION_W: Attribute = Attribute(31);
    pub const FRONT_COLOR_DIFFUSE_R: Attribute = Attribute(160);
    pub const BACK_COLOR_SPECULAR_A: Attribute = Attribute(175);
    pub const CLIP_DISTANCE_0: Attribute = Attribute(176);
    pub const CLIP_DISTANCE_7: Attribute = Attribute(183);
    pub const POINT_SPRITE_S: Attribute = Attribute(184);
    pub const POINT_SPRITE_T: Attribute = Attribute(185);
    pub const FOG_COORDINATE: Attribute = Attribute(186);
    pub const TESSELLATION_EVALUATION_POINT_U: Attribute = Attribute(188);
    pub const TESSELLATION_EVALUATION_POINT_V: Attribute = Attribute(189);
    pub const INSTANCE_ID: Attribute = Attribute(190);
    pub const VERTEX_ID: Attribute = Attribute(191);
    pub const FIXED_FNC_TEXTURE_0_S: Attribute = Attribute(192);
    pub const FIXED_FNC_TEXTURE_9_Q: Attribute = Attribute(231);
    pub const VIEWPORT_MASK: Attribute = Attribute(232);
    pub const FRONT_FACE: Attribute = Attribute(255);
    pub const BASE_INSTANCE: Attribute = Attribute(256);
    pub const BASE_VERTEX: Attribute = Attribute(257);
    pub const DRAW_ID: Attribute = Attribute(258);

    /// Get the generic attribute for index (0..31) and component (0..3).
    pub fn generic(index: u32, component: u32) -> Self {
        debug_assert!(index < 32 && component < 4);
        Attribute(32 + index * 4 + component)
    }

    /// Position component (0=X, 1=Y, 2=Z, 3=W).
    pub fn position(component: u32) -> Self {
        debug_assert!(component < 4);
        Attribute(28 + component)
    }

    /// Whether this is a generic attribute.
    pub fn is_generic(self) -> bool {
        (32..160).contains(&self.0)
    }

    /// Whether this is a legacy fixed-function varying.
    ///
    /// Mirrors upstream `IR::IsLegacyAttribute` in `frontend/ir/attribute.h`.
    pub fn is_legacy(self) -> bool {
        (Self::FRONT_COLOR_DIFFUSE_R.0..=Self::BACK_COLOR_SPECULAR_A.0).contains(&self.0)
            || self == Self::FOG_COORDINATE
            || (Self::FIXED_FNC_TEXTURE_0_S.0..=Self::FIXED_FNC_TEXTURE_9_Q.0).contains(&self.0)
    }

    /// Generic attribute index (0..31), valid only if `is_generic()`.
    pub fn generic_index(self) -> u32 {
        debug_assert!(self.is_generic());
        (self.0 - 32) / 4
    }

    /// Generic attribute component (0..3), valid only if `is_generic()`.
    pub fn generic_element(self) -> u32 {
        debug_assert!(self.is_generic());
        (self.0 - 32) % 4
    }

    /// Whether this is a position attribute.
    pub fn is_position(self) -> bool {
        (28..32).contains(&self.0)
    }

    /// Position component (0..3), valid only if `is_position()`.
    pub fn position_element(self) -> u32 {
        debug_assert!(self.is_position());
        self.0 - 28
    }

    pub fn is_clip_distance(self) -> bool {
        (Self::CLIP_DISTANCE_0.0..=Self::CLIP_DISTANCE_7.0).contains(&self.0)
    }

    pub fn clip_distance_index(self) -> u32 {
        debug_assert!(self.is_clip_distance());
        self.0 - Self::CLIP_DISTANCE_0.0
    }
}

#[cfg(test)]
mod attribute_tests {
    use super::Attribute;

    #[test]
    fn generic31_includes_all_four_components() {
        for component in 0..4 {
            let attribute = Attribute::generic(31, component);
            assert!(attribute.is_generic());
            assert_eq!(attribute.generic_index(), 31);
            assert_eq!(attribute.generic_element(), component);
        }
    }
}

impl fmt::Display for Attribute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_position() {
            let comp = ["X", "Y", "Z", "W"][self.position_element() as usize];
            write!(f, "Position.{}", comp)
        } else if self.is_generic() {
            let comp = ["X", "Y", "Z", "W"][self.generic_element() as usize];
            write!(f, "Generic{}.{}", self.generic_index(), comp)
        } else {
            match self.0 {
                24 => write!(f, "PrimitiveId"),
                25 => write!(f, "Layer"),
                26 => write!(f, "ViewportIndex"),
                27 => write!(f, "PointSize"),
                176..=183 => write!(f, "ClipDistance[{}]", self.0 - 176),
                184 => write!(f, "PointSprite.S"),
                185 => write!(f, "PointSprite.T"),
                188 => write!(f, "TessellationEvaluationPoint.U"),
                189 => write!(f, "TessellationEvaluationPoint.V"),
                190 => write!(f, "InstanceId"),
                191 => write!(f, "VertexId"),
                232 => write!(f, "ViewportMask"),
                255 => write!(f, "FrontFace"),
                _ => write!(f, "Attr({})", self.0),
            }
        }
    }
}

/// Tessellation patch attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Patch(pub u32);

impl Patch {
    pub const TESS_LOD_LEFT: Patch = Patch(0);
    pub const TESS_LOD_TOP: Patch = Patch(1);
    pub const TESS_LOD_RIGHT: Patch = Patch(2);
    pub const TESS_LOD_BOTTOM: Patch = Patch(3);
    pub const TESS_LOD_INTERIOR_U: Patch = Patch(4);
    pub const TESS_LOD_INTERIOR_V: Patch = Patch(5);

    pub fn generic(index: u32, component: u32) -> Self {
        debug_assert!(component < 4);
        Patch(6 + index * 4 + component)
    }

    pub fn is_generic(self) -> bool {
        (6..=125).contains(&self.0)
    }

    pub fn generic_index(self) -> u32 {
        debug_assert!(self.is_generic());
        (self.0 - 6) / 4
    }

    pub fn generic_element(self) -> u32 {
        debug_assert!(self.is_generic());
        (self.0 - 6) % 4
    }
}

/// IR Value — tagged union holding different kinds of values.
///
/// This is the fundamental unit passed between IR instructions as arguments
/// and results.
#[derive(Clone, Copy)]
pub enum Value {
    /// Result of an IR instruction (SSA reference).
    Inst(InstRef),
    /// Register reference.
    Reg(Reg),
    /// Predicate reference.
    Pred(Pred),
    /// Attribute reference.
    Attribute(Attribute),
    /// Patch reference.
    Patch(Patch),
    /// 1-bit immediate.
    ImmU1(bool),
    /// 8-bit unsigned immediate.
    ImmU8(u8),
    /// 16-bit unsigned immediate.
    ImmU16(u16),
    /// 32-bit unsigned immediate.
    ImmU32(u32),
    /// 64-bit unsigned immediate.
    ImmU64(u64),
    /// 16-bit float immediate (stored as u16 bits).
    ImmF16(u16),
    /// 32-bit float immediate.
    ImmF32(f32),
    /// 64-bit float immediate.
    ImmF64(f64),
    /// Empty / void.
    Void,
}

impl Value {
    /// Get the IR type of this value.
    /// Upstream: `Value::Type()` (value.h).
    /// For instruction references, returns Opaque (the actual type depends on the
    /// producing instruction's opcode return type).
    pub fn ir_type(&self) -> super::types::Type {
        use super::types::Type;
        match self {
            Value::Inst(_) => Type::Opaque,
            Value::Reg(_) => Type::Reg,
            Value::Pred(_) => Type::Pred,
            Value::Attribute(_) => Type::Attribute,
            Value::Patch(_) => Type::Patch,
            Value::ImmU1(_) => Type::U1,
            Value::ImmU8(_) => Type::U8,
            Value::ImmU16(_) => Type::U16,
            Value::ImmU32(_) => Type::U32,
            Value::ImmU64(_) => Type::U64,
            Value::ImmF16(_) => Type::F16,
            Value::ImmF32(_) => Type::F32,
            Value::ImmF64(_) => Type::F64,
            Value::Void => Type::Void,
        }
    }

    /// Whether this value is an instruction reference.
    pub fn is_inst(&self) -> bool {
        matches!(self, Value::Inst(_))
    }

    /// Whether this value is an immediate of any kind.
    pub fn is_immediate(&self) -> bool {
        matches!(
            self,
            Value::ImmU1(_)
                | Value::ImmU8(_)
                | Value::ImmU16(_)
                | Value::ImmU32(_)
                | Value::ImmU64(_)
                | Value::ImmF16(_)
                | Value::ImmF32(_)
                | Value::ImmF64(_)
        )
    }

    /// Whether this value is empty/void.
    pub fn is_void(&self) -> bool {
        matches!(self, Value::Void)
    }

    /// Get as instruction reference, panics if not.
    pub fn inst_ref(&self) -> InstRef {
        match self {
            Value::Inst(r) => *r,
            _ => panic!("Value is not an instruction reference"),
        }
    }

    /// Get as u32 immediate.
    pub fn imm_u32(&self) -> u32 {
        match self {
            Value::ImmU32(v) => *v,
            Value::ImmU1(v) => *v as u32,
            Value::ImmU8(v) => *v as u32,
            Value::ImmU16(v) => *v as u32,
            _ => panic!("Value is not a u32 immediate"),
        }
    }

    /// Get as f32 immediate.
    pub fn imm_f32(&self) -> f32 {
        match self {
            Value::ImmF32(v) => *v,
            _ => panic!("Value is not an f32 immediate"),
        }
    }

    /// Get as bool immediate.
    pub fn imm_u1(&self) -> bool {
        match self {
            Value::ImmU1(v) => *v,
            _ => panic!("Value is not a u1 immediate"),
        }
    }

    /// Get as u64 immediate.
    pub fn imm_u64(&self) -> u64 {
        match self {
            Value::ImmU64(v) => *v,
            Value::ImmU32(v) => *v as u64,
            _ => panic!("Value is not a u64 immediate"),
        }
    }

    /// Get as f64 immediate.
    pub fn imm_f64(&self) -> f64 {
        match self {
            Value::ImmF64(v) => *v,
            _ => panic!("Value is not an f64 immediate"),
        }
    }

    /// Get as register reference.
    pub fn reg(&self) -> Reg {
        match self {
            Value::Reg(r) => *r,
            _ => panic!("Value is not a register"),
        }
    }

    /// Get as predicate reference.
    pub fn pred(&self) -> Pred {
        match self {
            Value::Pred(p) => *p,
            _ => panic!("Value is not a predicate"),
        }
    }

    /// Get as attribute reference.
    pub fn attribute(&self) -> Attribute {
        match self {
            Value::Attribute(a) => *a,
            _ => panic!("Value is not an attribute"),
        }
    }

    /// Get as patch reference.
    pub fn patch(&self) -> Patch {
        match self {
            Value::Patch(p) => *p,
            _ => panic!("Value is not a patch"),
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Inst(r) => write!(f, "%{}:{}", r.block, r.inst),
            Value::Reg(r) => write!(f, "{}", r),
            Value::Pred(p) => write!(f, "{}", p),
            Value::Attribute(a) => write!(f, "{}", a),
            Value::Patch(p) => write!(f, "Patch({})", p.0),
            Value::ImmU1(v) => write!(f, "#{}", v),
            Value::ImmU8(v) => write!(f, "#{}u8", v),
            Value::ImmU16(v) => write!(f, "#{}u16", v),
            Value::ImmU32(v) => write!(f, "#0x{:08X}", v),
            Value::ImmU64(v) => write!(f, "#0x{:016X}u64", v),
            Value::ImmF16(v) => write!(f, "#{}f16", v),
            Value::ImmF32(v) => write!(f, "#{}", v),
            Value::ImmF64(v) => write!(f, "#{}f64", v),
            Value::Void => write!(f, "void"),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Inst(a), Value::Inst(b)) => a == b,
            (Value::Reg(a), Value::Reg(b)) => a == b,
            (Value::Pred(a), Value::Pred(b)) => a == b,
            (Value::Attribute(a), Value::Attribute(b)) => a == b,
            (Value::Patch(a), Value::Patch(b)) => a == b,
            (Value::ImmU1(a), Value::ImmU1(b)) => a == b,
            (Value::ImmU8(a), Value::ImmU8(b)) => a == b,
            (Value::ImmU16(a), Value::ImmU16(b)) => a == b,
            (Value::ImmU32(a), Value::ImmU32(b)) => a == b,
            (Value::ImmU64(a), Value::ImmU64(b)) => a == b,
            (Value::ImmF16(a), Value::ImmF16(b)) => a == b,
            (Value::ImmF32(a), Value::ImmF32(b)) => a.to_bits() == b.to_bits(),
            (Value::ImmF64(a), Value::ImmF64(b)) => a.to_bits() == b.to_bits(),
            (Value::Void, Value::Void) => true,
            _ => false,
        }
    }
}

impl Eq for Value {}

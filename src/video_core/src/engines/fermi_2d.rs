// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Fermi 2D engine stub (NV class 902D).
//!
//! Handles 2D blitting operations (surface copies, fills). Detects blit
//! trigger writes and logs parameters; actual pixel copy is not yet implemented.

use super::engine_interface::{EngineInterface, EngineInterfaceState};
use super::sw_blitter::blitter::SoftwareBlitEngine;
use super::{ClassId, Engine, PendingWrite, ENGINE_REG_COUNT};
use crate::gpu::RenderTargetFormat;
use crate::memory_manager::MemoryManager;
use crate::rasterizer_interface::{RasterizerHandle, RasterizerInterface};
use crate::surface;
use bytemuck::{Pod, Zeroable};
use parking_lot::Mutex;
use std::sync::Arc;

// ── Register constants (method = byte_offset / 4) ──────────────────────────

// Destination surface descriptor (0x80..0x89)
const DST_FORMAT: u32 = 0x80;
#[cfg(test)]
const DST_LINEAR: u32 = 0x81;
const DST_DEPTH: u32 = 0x83;
const DST_PITCH: u32 = 0x85;
const DST_WIDTH: u32 = 0x86;
const DST_HEIGHT: u32 = 0x87;
const DST_ADDR_HIGH: u32 = 0x88;
const DST_ADDR_LOW: u32 = 0x89;

// Source surface descriptor (0x8C..0x95)
const SRC_FORMAT: u32 = 0x8C;
#[cfg(test)]
const SRC_LINEAR: u32 = 0x8D;
#[cfg(test)]
const SRC_BLOCK_DIMENSIONS: u32 = 0x8E;
const SRC_DEPTH: u32 = 0x8F;
#[cfg(test)]
const SRC_LAYER: u32 = 0x90;
const SRC_PITCH: u32 = 0x91;
const SRC_WIDTH: u32 = 0x92;
const SRC_HEIGHT: u32 = 0x93;
const SRC_ADDR_HIGH: u32 = 0x94;
const SRC_ADDR_LOW: u32 = 0x95;

// General blit state
#[cfg(test)]
const OPERATION: u32 = 0xAB;

// pixels_from_memory (0x220..0x237)
#[cfg(test)]
const PIXELS_FROM_MEMORY_SAMPLE_MODE: u32 = 0x223;
#[cfg(test)]
const PIXELS_FROM_MEMORY_DST_X0: u32 = 0x22C;
#[cfg(test)]
const PIXELS_FROM_MEMORY_DST_Y0: u32 = 0x22D;
#[cfg(test)]
const PIXELS_FROM_MEMORY_DST_WIDTH: u32 = 0x22E;
#[cfg(test)]
const PIXELS_FROM_MEMORY_DST_HEIGHT: u32 = 0x22F;
#[cfg(test)]
const PIXELS_FROM_MEMORY_DU_DX_LOW: u32 = 0x230;
#[cfg(test)]
const PIXELS_FROM_MEMORY_DU_DX_HIGH: u32 = 0x231;
#[cfg(test)]
const PIXELS_FROM_MEMORY_DV_DY_LOW: u32 = 0x232;
#[cfg(test)]
const PIXELS_FROM_MEMORY_DV_DY_HIGH: u32 = 0x233;
#[cfg(test)]
const PIXELS_FROM_MEMORY_SRC_X0_LOW: u32 = 0x234;
#[cfg(test)]
const PIXELS_FROM_MEMORY_SRC_X0_HIGH: u32 = 0x235;
#[cfg(test)]
const PIXELS_FROM_MEMORY_SRC_Y0_LOW: u32 = 0x236;
const PIXELS_FROM_MEMORY_SRC_Y0_HIGH: u32 = 0x237;

// Blit trigger
const BLIT_TRIGGER: u32 = PIXELS_FROM_MEMORY_SRC_Y0_HIGH;
const NUM_REGS_WORDS: usize = 0x258;

const NULL_DERIVATIVE: i64 = 1i64 << 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Origin {
    #[default]
    Center = 0,
    Corner = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Filter {
    #[default]
    Point = 0,
    Bilinear = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Operation {
    SrcCopyAnd = 0,
    RopAnd = 1,
    Blend = 2,
    #[default]
    SrcCopy = 3,
    Rop = 4,
    SrcCopyPremult = 5,
    BlendPremult = 6,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum MemoryLayout {
    #[default]
    BlockLinear = 0,
    Pitch = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum CpuIndexWrap {
    #[default]
    Wrap = 0,
    NoWrap = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum SectorPromotion {
    #[default]
    NoPromotion = 0,
    PromoteTo2V = 1,
    PromoteTo2H = 2,
    PromoteTo4 = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum NumTpcs {
    #[default]
    All = 0,
    One = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum RenderEnableMode {
    #[default]
    False = 0,
    True = 1,
    Conditional = 2,
    RenderIfEqual = 3,
    RenderIfNotEqual = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum ColorKeyFormat {
    #[default]
    A16R5G6B5 = 0,
    A1R5G5B5 = 1,
    A8R8G8B8 = 2,
    A2R10G10B10 = 3,
    Y8 = 4,
    Y16 = 5,
    Y32 = 6,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum PatternSelect {
    #[default]
    MonoChrome8x8 = 0,
    MonoChrome64x1 = 1,
    MonoChrome1x64 = 2,
    Color = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum NotifyType {
    #[default]
    WriteOnly = 0,
    WriteThenAwaken = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum MonochromePatternColorFormat {
    #[default]
    A8X8R5G6B5 = 0,
    A1R5G5B5 = 1,
    A8R8G8B8 = 2,
    A8Y8 = 3,
    A8X8Y16 = 4,
    Y32 = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum MonochromePatternFormat {
    #[default]
    Cga6M1 = 0,
    LeM1 = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Surface {
    pub format: RenderTargetFormat,
    pub linear: MemoryLayout,
    pub block_dimensions: u32,
    pub depth: u32,
    pub layer: u32,
    pub pitch: u32,
    pub width: u32,
    pub height: u32,
    pub addr_upper: u32,
    pub addr_lower: u32,
}

impl Default for Surface {
    fn default() -> Self {
        Self {
            format: RenderTargetFormat::None,
            linear: MemoryLayout::BlockLinear,
            block_dimensions: 0,
            depth: 0,
            layer: 0,
            pitch: 0,
            width: 0,
            height: 0,
            addr_upper: 0,
            addr_lower: 0,
        }
    }
}

impl Surface {
    pub fn address(&self) -> u64 {
        ((self.addr_upper as u64) << 32) | self.addr_lower as u64
    }

    pub fn block_width(&self) -> u32 {
        self.block_dimensions & 0xF
    }

    pub fn block_height(&self) -> u32 {
        (self.block_dimensions >> 4) & 0xF
    }

    pub fn block_depth(&self) -> u32 {
        (self.block_dimensions >> 8) & 0xF
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
struct SurfaceRaw {
    format: RenderTargetFormatRaw,
    linear: MemoryLayoutRaw,
    block_dimensions: BlockDimensionsRaw,
    depth: u32,
    layer: u32,
    pitch: u32,
    width: u32,
    height: u32,
    addr_upper: u32,
    addr_lower: u32,
}

unsafe impl Zeroable for SurfaceRaw {}
unsafe impl Pod for SurfaceRaw {}

impl From<SurfaceRaw> for Surface {
    fn from(raw: SurfaceRaw) -> Self {
        Self {
            format: raw.format.get(),
            linear: raw.linear.get(),
            block_dimensions: raw.block_dimensions.raw,
            depth: raw.depth,
            layer: raw.layer,
            pitch: raw.pitch,
            width: raw.width,
            height: raw.height,
            addr_upper: raw.addr_upper,
            addr_lower: raw.addr_lower,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct BlockDimensionsRaw {
    raw: u32,
}

#[cfg(test)]
impl BlockDimensionsRaw {
    fn block_width(self) -> u32 {
        self.raw & 0xF
    }

    fn block_height(self) -> u32 {
        (self.raw >> 4) & 0xF
    }

    fn block_depth(self) -> u32 {
        (self.raw >> 8) & 0xF
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct RenderTargetFormatRaw(u32);

impl RenderTargetFormatRaw {
    fn get(self) -> RenderTargetFormat {
        Fermi2D::decode_render_target_format(self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct MemoryLayoutRaw(u32);

impl MemoryLayoutRaw {
    fn get(self) -> MemoryLayout {
        Fermi2D::decode_memory_layout(self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct SampleModeRaw(u32);

impl SampleModeRaw {
    fn origin(self) -> Origin {
        match self.0 & 1 {
            1 => Origin::Corner,
            _ => Origin::Center,
        }
    }

    fn filter(self) -> Filter {
        match (self.0 >> 4) & 1 {
            1 => Filter::Bilinear,
            _ => Filter::Point,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct BlockShapeRaw(u32);

#[cfg(test)]
impl BlockShapeRaw {
    fn get(self) -> u32 {
        self.0 & 0x7
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct CorralSizeRaw(u32);

#[cfg(test)]
impl CorralSizeRaw {
    fn get(self) -> u32 {
        self.0 & 0x1F
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct SafeOverlapRaw(u32);

#[cfg(test)]
impl SafeOverlapRaw {
    fn get(self) -> bool {
        (self.0 & 1) != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct NotifyTypeRaw(u32);

#[cfg(test)]
impl NotifyTypeRaw {
    fn get(self) -> NotifyType {
        match self.0 {
            1 => NotifyType::WriteThenAwaken,
            _ => NotifyType::WriteOnly,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct CpuIndexWrapRaw(u32);

#[cfg(test)]
impl CpuIndexWrapRaw {
    fn get(self) -> CpuIndexWrap {
        match self.0 {
            1 => CpuIndexWrap::NoWrap,
            _ => CpuIndexWrap::Wrap,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct Kind2dCheckEnableRaw(u32);

#[cfg(test)]
impl Kind2dCheckEnableRaw {
    fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct SectorPromotionRaw(u32);

#[cfg(test)]
impl SectorPromotionRaw {
    fn get(self) -> SectorPromotion {
        match self.0 {
            1 => SectorPromotion::PromoteTo2V,
            2 => SectorPromotion::PromoteTo2H,
            3 => SectorPromotion::PromoteTo4,
            _ => SectorPromotion::NoPromotion,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct NumTpcsRaw(u32);

#[cfg(test)]
impl NumTpcsRaw {
    fn get(self) -> NumTpcs {
        match self.0 {
            1 => NumTpcs::One,
            _ => NumTpcs::All,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct RenderEnableModeRaw(u32);

#[cfg(test)]
impl RenderEnableModeRaw {
    fn get(self) -> RenderEnableMode {
        match self.0 {
            1 => RenderEnableMode::True,
            2 => RenderEnableMode::Conditional,
            3 => RenderEnableMode::RenderIfEqual,
            4 => RenderEnableMode::RenderIfNotEqual,
            _ => RenderEnableMode::False,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct ColorKeyFormatRaw(u32);

#[cfg(test)]
impl ColorKeyFormatRaw {
    fn get(self) -> ColorKeyFormat {
        match self.0 & 0x7 {
            1 => ColorKeyFormat::A1R5G5B5,
            2 => ColorKeyFormat::A8R8G8B8,
            3 => ColorKeyFormat::A2R10G10B10,
            4 => ColorKeyFormat::Y8,
            5 => ColorKeyFormat::Y16,
            6 => ColorKeyFormat::Y32,
            _ => ColorKeyFormat::A16R5G6B5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct BoolBitRaw(u32);

impl BoolBitRaw {
    fn get(self) -> bool {
        (self.0 & 1) != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct RopRaw(u32);

#[cfg(test)]
impl RopRaw {
    fn get(self) -> u32 {
        self.0 & 0xFF
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct Beta1Raw(u32);

#[cfg(test)]
impl Beta1Raw {
    fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct OperationRaw(u32);

impl OperationRaw {
    fn get(self) -> Operation {
        match self.0 {
            0 => Operation::SrcCopyAnd,
            1 => Operation::RopAnd,
            2 => Operation::Blend,
            3 => Operation::SrcCopy,
            4 => Operation::Rop,
            5 => Operation::SrcCopyPremult,
            6 => Operation::BlendPremult,
            _ => Operation::SrcCopy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct PatternSelectRaw(u32);

#[cfg(test)]
impl PatternSelectRaw {
    fn get(self) -> PatternSelect {
        match self.0 & 0x3 {
            1 => PatternSelect::MonoChrome64x1,
            2 => PatternSelect::MonoChrome1x64,
            3 => PatternSelect::Color,
            _ => PatternSelect::MonoChrome8x8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct MonochromePatternColorFormatRaw(u32);

#[cfg(test)]
impl MonochromePatternColorFormatRaw {
    fn get(self) -> MonochromePatternColorFormat {
        match self.0 & 0x7 {
            1 => MonochromePatternColorFormat::A1R5G5B5,
            2 => MonochromePatternColorFormat::A8R8G8B8,
            3 => MonochromePatternColorFormat::A8Y8,
            4 => MonochromePatternColorFormat::A8X8Y16,
            5 => MonochromePatternColorFormat::Y32,
            _ => MonochromePatternColorFormat::A8X8R5G6B5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct MonochromePatternFormatRaw(u32);

#[cfg(test)]
impl MonochromePatternFormatRaw {
    fn get(self) -> MonochromePatternFormat {
        match self.0 & 1 {
            1 => MonochromePatternFormat::LeM1,
            _ => MonochromePatternFormat::Cga6M1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
struct BigEndianControlRaw(u32);

#[cfg(test)]
impl BigEndianControlRaw {
    fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
struct PixelsFromMemory {
    block_shape: BlockShapeRaw,
    corral_size: CorralSizeRaw,
    safe_overlap: SafeOverlapRaw,
    sample_mode: SampleModeRaw,
    padding: [u32; 8],
    dst_x0: i32,
    dst_y0: i32,
    dst_width: i32,
    dst_height: i32,
    du_dx: i64,
    dv_dy: i64,
    src_x0: i64,
    src_y0: i64,
}

unsafe impl Zeroable for PixelsFromMemory {}
unsafe impl Pod for PixelsFromMemory {}

impl PixelsFromMemory {
    #[cfg(test)]
    fn block_shape(&self) -> u32 {
        self.block_shape.get()
    }

    #[cfg(test)]
    fn corral_size(&self) -> u32 {
        self.corral_size.get()
    }

    #[cfg(test)]
    fn safe_overlap(&self) -> bool {
        self.safe_overlap.get()
    }

    fn origin(&self) -> Origin {
        self.sample_mode.origin()
    }

    fn filter(&self) -> Filter {
        self.sample_mode.filter()
    }

    fn dst_x0(&self) -> i32 {
        self.dst_x0
    }

    fn dst_y0(&self) -> i32 {
        self.dst_y0
    }

    fn dst_width(&self) -> i32 {
        self.dst_width
    }

    fn dst_height(&self) -> i32 {
        self.dst_height
    }

    fn du_dx(&self) -> i64 {
        self.du_dx
    }

    fn dv_dy(&self) -> i64 {
        self.dv_dy
    }

    fn src_x0(&self) -> i64 {
        self.src_x0
    }

    fn src_y0(&self) -> i64 {
        self.src_y0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
struct Beta4Raw {
    raw: u32,
}

#[cfg(test)]
impl Beta4Raw {
    fn b(self) -> u32 {
        self.raw & 0xFF
    }

    fn g(self) -> u32 {
        (self.raw >> 8) & 0xFF
    }

    fn r(self) -> u32 {
        (self.raw >> 16) & 0xFF
    }

    fn a(self) -> u32 {
        (self.raw >> 24) & 0xFF
    }
}

unsafe impl Zeroable for Beta4Raw {}
unsafe impl Pod for Beta4Raw {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
struct PatternOffsetRaw {
    raw: u32,
}

#[cfg(test)]
impl PatternOffsetRaw {
    fn x(self) -> u32 {
        self.raw & 0x3F
    }

    fn y(self) -> u32 {
        (self.raw >> 8) & 0x3F
    }
}

unsafe impl Zeroable for PatternOffsetRaw {}
unsafe impl Pod for PatternOffsetRaw {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
struct MonochromePatternRaw {
    color_format: MonochromePatternColorFormatRaw,
    format: MonochromePatternFormatRaw,
    color0: u32,
    color1: u32,
    pattern0: u32,
    pattern1: u32,
}

#[cfg(test)]
impl MonochromePatternRaw {
    fn color_format(self) -> MonochromePatternColorFormat {
        self.color_format.get()
    }

    fn format(self) -> MonochromePatternFormat {
        self.format.get()
    }

    fn color0(self) -> u32 {
        self.color0
    }

    fn color1(self) -> u32 {
        self.color1
    }

    fn pattern0(self) -> u32 {
        self.pattern0
    }

    fn pattern1(self) -> u32 {
        self.pattern1
    }
}

unsafe impl Zeroable for MonochromePatternRaw {}
unsafe impl Pod for MonochromePatternRaw {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct ColorPatternRaw {
    x8r8g8b8: [u32; 0x40],
    r5g6b5: [u32; 0x20],
    x1r5g5b5: [u32; 0x20],
    y8: [u32; 0x10],
}

#[cfg(test)]
impl ColorPatternRaw {
    fn x8r8g8b8(self, index: usize) -> u32 {
        self.x8r8g8b8[index]
    }

    fn r5g6b5(self, index: usize) -> u32 {
        self.r5g6b5[index]
    }

    fn x1r5g5b5(self, index: usize) -> u32 {
        self.x1r5g5b5[index]
    }

    fn y8(self, index: usize) -> u32 {
        self.y8[index]
    }
}

unsafe impl Zeroable for ColorPatternRaw {}
unsafe impl Pod for ColorPatternRaw {}

impl Default for ColorPatternRaw {
    fn default() -> Self {
        Self {
            x8r8g8b8: [0; 0x40],
            r5g6b5: [0; 0x20],
            x1r5g5b5: [0; 0x20],
            y8: [0; 0x10],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
struct PointRaw {
    x: u32,
    y: u32,
}

#[cfg(test)]
impl PointRaw {
    fn x(self) -> u32 {
        self.x
    }

    fn y(self) -> u32 {
        self.y
    }
}

unsafe impl Zeroable for PointRaw {}
unsafe impl Pod for PointRaw {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct RenderSolidRaw {
    prim_mode: u32,
    prim_color_format: u32,
    prim_color: u32,
    line_tie_break_bits: u32,
    pad_0x390: [u32; 0x14],
    prim_point_xy: u32,
    pad_0x3e4: [u32; 0x7],
    prim_point: [PointRaw; 0x40],
}

#[cfg(test)]
impl RenderSolidRaw {
    fn prim_mode(self) -> u32 {
        self.prim_mode
    }

    fn prim_color_format(self) -> u32 {
        self.prim_color_format
    }

    fn prim_color(self) -> u32 {
        self.prim_color
    }

    fn line_tie_break_bits(self) -> u32 {
        self.line_tie_break_bits
    }

    fn prim_point_xy(self) -> u32 {
        self.prim_point_xy
    }

    fn prim_point(self, index: usize) -> PointRaw {
        self.prim_point[index]
    }
}

unsafe impl Zeroable for RenderSolidRaw {}
unsafe impl Pod for RenderSolidRaw {}

impl Default for RenderSolidRaw {
    fn default() -> Self {
        Self {
            prim_mode: 0,
            prim_color_format: 0,
            prim_color: 0,
            line_tie_break_bits: 0,
            pad_0x390: [0; 0x14],
            prim_point_xy: 0,
            pad_0x3e4: [0; 0x7],
            prim_point: [PointRaw::default(); 0x40],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
struct PixelsFromCpuRaw {
    data_type: u32,
    color_format: u32,
    index_format: u32,
    mono_format: u32,
    wrap: u32,
    color0: u32,
    color1: u32,
    mono_opacity: u32,
    pad_0x620: [u32; 0x6],
    src_width: u32,
    src_height: u32,
    dx_du_frac: u32,
    dx_du_int: u32,
    dx_dv_frac: u32,
    dy_dv_int: u32,
    dst_x0_frac: u32,
    dst_x0_int: u32,
    dst_y0_frac: u32,
    dst_y0_int: u32,
    data: u32,
}

#[cfg(test)]
impl PixelsFromCpuRaw {
    fn data_type(self) -> u32 {
        self.data_type
    }

    fn color_format(self) -> u32 {
        self.color_format
    }

    fn index_format(self) -> u32 {
        self.index_format
    }

    fn mono_format(self) -> u32 {
        self.mono_format
    }

    fn wrap(self) -> u32 {
        self.wrap
    }

    fn color0(self) -> u32 {
        self.color0
    }

    fn color1(self) -> u32 {
        self.color1
    }

    fn mono_opacity(self) -> u32 {
        self.mono_opacity
    }

    fn src_width(self) -> u32 {
        self.src_width
    }

    fn src_height(self) -> u32 {
        self.src_height
    }

    fn dx_du_frac(self) -> u32 {
        self.dx_du_frac
    }

    fn dx_du_int(self) -> u32 {
        self.dx_du_int
    }

    fn dx_dv_frac(self) -> u32 {
        self.dx_dv_frac
    }

    fn dy_dv_int(self) -> u32 {
        self.dy_dv_int
    }

    fn dst_x0_frac(self) -> u32 {
        self.dst_x0_frac
    }

    fn dst_x0_int(self) -> u32 {
        self.dst_x0_int
    }

    fn dst_y0_frac(self) -> u32 {
        self.dst_y0_frac
    }

    fn dst_y0_int(self) -> u32 {
        self.dst_y0_int
    }

    fn data(self) -> u32 {
        self.data
    }
}

unsafe impl Zeroable for PixelsFromCpuRaw {}
unsafe impl Pod for PixelsFromCpuRaw {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct RegsPrefixRaw {
    object: u32,
    pad_0x004_to_0x100: [u32; 0x3F],
    no_operation: u32,
    notify: NotifyTypeRaw,
    pad_0x108_to_0x110: [u32; 0x2],
    wait_for_idle: u32,
    pad_0x114_to_0x140: [u32; 0xB],
    pm_trigger: u32,
    pad_0x144_to_0x180: [u32; 0xF],
    context_dma_notify: u32,
    dst_context_dma: u32,
    src_context_dma: u32,
    semaphore_context_dma: u32,
    pad_0x190_to_0x200: [u32; 0x1C],
}

unsafe impl Zeroable for RegsPrefixRaw {}
unsafe impl Pod for RegsPrefixRaw {}

impl Default for RegsPrefixRaw {
    fn default() -> Self {
        Self {
            object: 0,
            pad_0x004_to_0x100: [0; 0x3F],
            no_operation: 0,
            notify: NotifyTypeRaw::default(),
            pad_0x108_to_0x110: [0; 0x2],
            wait_for_idle: 0,
            pad_0x114_to_0x140: [0; 0xB],
            pm_trigger: 0,
            pad_0x144_to_0x180: [0; 0xF],
            context_dma_notify: 0,
            dst_context_dma: 0,
            src_context_dma: 0,
            semaphore_context_dma: 0,
            pad_0x190_to_0x200: [0; 0x1C],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct ActiveRegsRaw {
    dst: SurfaceRaw,
    pixels_from_cpu_index_wrap: CpuIndexWrapRaw,
    kind2d_check_enable: Kind2dCheckEnableRaw,
    src: SurfaceRaw,
    pixels_from_memory_sector_promotion: SectorPromotionRaw,
    pad_0x25c: u32,
    num_tpcs: NumTpcsRaw,
    render_enable_addr_upper: u32,
    render_enable_addr_lower: u32,
    render_enable_mode: RenderEnableModeRaw,
    pad_0x270: [u32; 4],
    clip_x0: u32,
    clip_y0: u32,
    clip_width: u32,
    clip_height: u32,
    clip_enable: BoolBitRaw,
    color_key_format: ColorKeyFormatRaw,
    color_key: u32,
    color_key_enable: BoolBitRaw,
    rop: RopRaw,
    beta1: Beta1Raw,
    beta4: Beta4Raw,
    operation: OperationRaw,
    pattern_offset: PatternOffsetRaw,
    pattern_select: PatternSelectRaw,
    pad_0x2b8_to_0x2e8: [u32; 0xC],
    monochrome_pattern: MonochromePatternRaw,
    color_pattern: ColorPatternRaw,
    pad_0x540_to_0x580: [u32; 0x10],
    render_solid: RenderSolidRaw,
    pixels_from_cpu: PixelsFromCpuRaw,
    pad_0x864_to_0x870: [u32; 0x3],
    big_endian_control: BigEndianControlRaw,
    pad_0x874_to_0x880: [u32; 0x3],
    pixels_from_memory: PixelsFromMemory,
}

unsafe impl Zeroable for ActiveRegsRaw {}
unsafe impl Pod for ActiveRegsRaw {}

impl Default for ActiveRegsRaw {
    fn default() -> Self {
        Self {
            dst: SurfaceRaw::default(),
            pixels_from_cpu_index_wrap: CpuIndexWrapRaw::default(),
            kind2d_check_enable: Kind2dCheckEnableRaw::default(),
            src: SurfaceRaw::default(),
            pixels_from_memory_sector_promotion: SectorPromotionRaw::default(),
            pad_0x25c: 0,
            num_tpcs: NumTpcsRaw::default(),
            render_enable_addr_upper: 0,
            render_enable_addr_lower: 0,
            render_enable_mode: RenderEnableModeRaw::default(),
            pad_0x270: [0; 4],
            clip_x0: 0,
            clip_y0: 0,
            clip_width: 0,
            clip_height: 0,
            clip_enable: BoolBitRaw::default(),
            color_key_format: ColorKeyFormatRaw::default(),
            color_key: 0,
            color_key_enable: BoolBitRaw::default(),
            rop: RopRaw::default(),
            beta1: Beta1Raw::default(),
            beta4: Beta4Raw::default(),
            operation: OperationRaw::default(),
            pattern_offset: PatternOffsetRaw::default(),
            pattern_select: PatternSelectRaw::default(),
            pad_0x2b8_to_0x2e8: [0; 0xC],
            monochrome_pattern: MonochromePatternRaw::default(),
            color_pattern: ColorPatternRaw::default(),
            pad_0x540_to_0x580: [0; 0x10],
            render_solid: RenderSolidRaw::default(),
            pixels_from_cpu: PixelsFromCpuRaw::default(),
            pad_0x864_to_0x870: [0; 0x3],
            big_endian_control: BigEndianControlRaw::default(),
            pad_0x874_to_0x880: [0; 0x3],
            pixels_from_memory: PixelsFromMemory::default(),
        }
    }
}

impl ActiveRegsRaw {
    #[cfg(test)]
    fn render_enable_address(self) -> u64 {
        ((self.render_enable_addr_upper as u64) << 32) | self.render_enable_addr_lower as u64
    }

    #[cfg(test)]
    fn render_enable_mode(self) -> RenderEnableMode {
        self.render_enable_mode.get()
    }

    fn clip_enabled(self) -> bool {
        self.clip_enable.get()
    }

    #[cfg(test)]
    fn color_key_format(self) -> ColorKeyFormat {
        self.color_key_format.get()
    }

    #[cfg(test)]
    fn color_key_enabled(self) -> bool {
        self.color_key_enable.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct RegsRawTail0x8e0To0x960 {
    words: [u32; 0x20],
}

unsafe impl Zeroable for RegsRawTail0x8e0To0x960 {}
unsafe impl Pod for RegsRawTail0x8e0To0x960 {}

impl Default for RegsRawTail0x8e0To0x960 {
    fn default() -> Self {
        Self { words: [0; 0x20] }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct RegsRaw {
    prefix: RegsPrefixRaw,
    active: ActiveRegsRaw,
    tail_0x8e0_to_0x960: RegsRawTail0x8e0To0x960,
}

unsafe impl Zeroable for RegsRaw {}
unsafe impl Pod for RegsRaw {}

impl Default for RegsRaw {
    fn default() -> Self {
        Self {
            prefix: RegsPrefixRaw::default(),
            active: ActiveRegsRaw::default(),
            tail_0x8e0_to_0x960: RegsRawTail0x8e0To0x960::default(),
        }
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
union RegsUnionRaw {
    structured: RegsRaw,
    reg_array: [u32; NUM_REGS_WORDS],
}

impl Default for RegsUnionRaw {
    fn default() -> Self {
        Self {
            reg_array: [0; NUM_REGS_WORDS],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct RegsRuntimeTailRaw {
    words: [u32; ENGINE_REG_COUNT - NUM_REGS_WORDS],
}

unsafe impl Zeroable for RegsRuntimeTailRaw {}
unsafe impl Pod for RegsRuntimeTailRaw {}

impl Default for RegsRuntimeTailRaw {
    fn default() -> Self {
        Self {
            words: [0; ENGINE_REG_COUNT - NUM_REGS_WORDS],
        }
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
struct RegsStorageRaw {
    regs: RegsUnionRaw,
    runtime_tail: RegsRuntimeTailRaw,
}

unsafe impl Zeroable for RegsStorageRaw {}
unsafe impl Pod for RegsStorageRaw {}

impl Default for RegsStorageRaw {
    fn default() -> Self {
        Self {
            regs: RegsUnionRaw::default(),
            runtime_tail: RegsRuntimeTailRaw::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct Config {
    pub operation: Operation,
    pub filter: Filter,
    pub must_accelerate: bool,
    pub dst_x0: i32,
    pub dst_y0: i32,
    pub dst_x1: i32,
    pub dst_y1: i32,
    pub src_x0: i32,
    pub src_y0: i32,
    pub src_x1: i32,
    pub src_y1: i32,
}

pub struct Fermi2D {
    regs: RegsStorageRaw,
    interface_state: EngineInterfaceState,
    memory_manager: Arc<Mutex<MemoryManager>>,
    /// Set when a blit trigger is detected; consumed by tests / future logic.
    pub pending_blit: bool,
    rasterizer: Option<RasterizerHandle>,
    sw_blitter: SoftwareBlitEngine,
    #[cfg(test)]
    call_method_last_flags: Vec<bool>,
}

impl Fermi2D {
    pub fn new(memory_manager: Arc<Mutex<MemoryManager>>) -> Self {
        let mut this = Self {
            regs: RegsStorageRaw::default(),
            interface_state: {
                let mut state = EngineInterfaceState::new();
                state.execution_mask[BLIT_TRIGGER as usize] = true;
                state
            },
            memory_manager: Arc::clone(&memory_manager),
            pending_blit: false,
            rasterizer: None,
            sw_blitter: SoftwareBlitEngine::new(memory_manager),
            #[cfg(test)]
            call_method_last_flags: Vec::new(),
        };
        {
            let words = this.words_mut();
            words[SRC_DEPTH as usize] = 1;
            words[DST_DEPTH as usize] = 1;
        }
        this
    }

    /// Corresponds to upstream `Fermi2D::CallMethod`.
    pub fn call_method(&mut self, method: u32, argument: u32, _is_last_call: bool) {
        #[cfg(test)]
        self.call_method_last_flags.push(_is_last_call);

        let idx = method as usize;
        assert!(
            idx < NUM_REGS_WORDS,
            "Invalid Fermi2D register 0x{method:X}, expected < 0x{NUM_REGS_WORDS:X}"
        );
        self.upstream_reg_array_mut()[idx] = argument;

        if method == BLIT_TRIGGER {
            self.handle_blit();
        }
    }

    /// Corresponds to upstream `Fermi2D::CallMultiMethod`.
    pub fn call_multi_method(
        &mut self,
        method: u32,
        args: &[u32],
        amount: u32,
        methods_pending: u32,
    ) {
        for (i, &arg) in args.iter().take(amount as usize).enumerate() {
            let is_last_call = methods_pending.wrapping_sub(i as u32) <= 1;
            self.call_method(method, arg, is_last_call);
        }
    }

    /// Corresponds to `Fermi2D::BindRasterizer`.
    pub fn bind_rasterizer(&mut self, rasterizer: &dyn RasterizerInterface) {
        self.rasterizer = Some(RasterizerHandle::from_ref(rasterizer));
    }

    // ── Typed accessors ────────────────────────────────────────────────

    pub fn dst_addr(&self) -> u64 {
        ((self.words()[DST_ADDR_HIGH as usize] as u64) << 32)
            | (self.words()[DST_ADDR_LOW as usize] as u64)
    }

    pub fn src_addr(&self) -> u64 {
        ((self.words()[SRC_ADDR_HIGH as usize] as u64) << 32)
            | (self.words()[SRC_ADDR_LOW as usize] as u64)
    }

    pub fn dst_width(&self) -> u32 {
        self.words()[DST_WIDTH as usize]
    }

    pub fn dst_height(&self) -> u32 {
        self.words()[DST_HEIGHT as usize]
    }

    pub fn dst_pitch(&self) -> u32 {
        self.words()[DST_PITCH as usize]
    }

    pub fn dst_format(&self) -> u32 {
        self.words()[DST_FORMAT as usize]
    }

    pub fn src_width(&self) -> u32 {
        self.words()[SRC_WIDTH as usize]
    }

    pub fn src_height(&self) -> u32 {
        self.words()[SRC_HEIGHT as usize]
    }

    pub fn src_pitch(&self) -> u32 {
        self.words()[SRC_PITCH as usize]
    }

    pub fn src_format(&self) -> u32 {
        self.words()[SRC_FORMAT as usize]
    }

    fn words(&self) -> &[u32] {
        let ptr = std::ptr::from_ref(&self.regs).cast::<u32>();
        // RegsStorageRaw is repr(C), starts with the register union, and its
        // tested size is exactly ENGINE_REG_COUNT words.
        unsafe { std::slice::from_raw_parts(ptr, ENGINE_REG_COUNT) }
    }

    fn words_mut(&mut self) -> &mut [u32] {
        let ptr = std::ptr::from_mut(&mut self.regs).cast::<u32>();
        // See words(): the runtime tail immediately follows the upstream
        // register union in the same contiguous repr(C) storage.
        unsafe { std::slice::from_raw_parts_mut(ptr, ENGINE_REG_COUNT) }
    }

    fn upstream_reg_array_mut(&mut self) -> &mut [u32; NUM_REGS_WORDS] {
        unsafe { &mut self.regs.regs.reg_array }
    }

    fn regs_head(&self) -> &RegsRaw {
        unsafe { &self.regs.regs.structured }
    }

    fn clip_enable(&self) -> u32 {
        u32::from(self.regs_head().active.clip_enabled())
    }

    fn operation(&self) -> Operation {
        self.regs_head().active.operation.get()
    }

    fn decode_memory_layout(raw: u32) -> MemoryLayout {
        match raw {
            1 => MemoryLayout::Pitch,
            _ => MemoryLayout::BlockLinear,
        }
    }

    fn decode_render_target_format(raw: u32) -> RenderTargetFormat {
        match raw {
            0x0 => RenderTargetFormat::None,
            0xC0 => RenderTargetFormat::R32G32B32A32Float,
            0xC1 => RenderTargetFormat::R32G32B32A32Sint,
            0xC2 => RenderTargetFormat::R32G32B32A32Uint,
            0xC3 => RenderTargetFormat::R32G32B32X32Float,
            0xC4 => RenderTargetFormat::R32G32B32X32Sint,
            0xC5 => RenderTargetFormat::R32G32B32X32Uint,
            0xC6 => RenderTargetFormat::R16G16B16A16Unorm,
            0xC7 => RenderTargetFormat::R16G16B16A16Snorm,
            0xC8 => RenderTargetFormat::R16G16B16A16Sint,
            0xC9 => RenderTargetFormat::R16G16B16A16Uint,
            0xCA => RenderTargetFormat::R16G16B16A16Float,
            0xCB => RenderTargetFormat::R32G32Float,
            0xCC => RenderTargetFormat::R32G32Sint,
            0xCD => RenderTargetFormat::R32G32Uint,
            0xCE => RenderTargetFormat::R16G16B16X16Float,
            0xCF => RenderTargetFormat::A8R8G8B8Unorm,
            0xD0 => RenderTargetFormat::A8R8G8B8Srgb,
            0xD1 => RenderTargetFormat::A2B10G10R10Unorm,
            0xD2 => RenderTargetFormat::A2B10G10R10Uint,
            0xD5 => RenderTargetFormat::A8B8G8R8Unorm,
            0xD6 => RenderTargetFormat::A8B8G8R8Srgb,
            0xD7 => RenderTargetFormat::A8B8G8R8Snorm,
            0xD8 => RenderTargetFormat::A8B8G8R8Sint,
            0xD9 => RenderTargetFormat::A8B8G8R8Uint,
            0xDA => RenderTargetFormat::R16G16Unorm,
            0xDB => RenderTargetFormat::R16G16Snorm,
            0xDC => RenderTargetFormat::R16G16Sint,
            0xDD => RenderTargetFormat::R16G16Uint,
            0xDE => RenderTargetFormat::R16G16Float,
            0xDF => RenderTargetFormat::A2R10G10B10Unorm,
            0xE0 => RenderTargetFormat::B10G11R11Float,
            0xE3 => RenderTargetFormat::R32Sint,
            0xE4 => RenderTargetFormat::R32Uint,
            0xE5 => RenderTargetFormat::R32Float,
            0xE6 => RenderTargetFormat::X8R8G8B8Unorm,
            0xE7 => RenderTargetFormat::X8R8G8B8Srgb,
            0xE8 => RenderTargetFormat::R5G6B5Unorm,
            0xE9 => RenderTargetFormat::A1R5G5B5Unorm,
            0xEA => RenderTargetFormat::R8G8Unorm,
            0xEB => RenderTargetFormat::R8G8Snorm,
            0xEC => RenderTargetFormat::R8G8Sint,
            0xED => RenderTargetFormat::R8G8Uint,
            0xEE => RenderTargetFormat::R16Unorm,
            0xEF => RenderTargetFormat::R16Snorm,
            0xF0 => RenderTargetFormat::R16Sint,
            0xF1 => RenderTargetFormat::R16Uint,
            0xF2 => RenderTargetFormat::R16Float,
            0xF3 => RenderTargetFormat::R8Unorm,
            0xF4 => RenderTargetFormat::R8Snorm,
            0xF5 => RenderTargetFormat::R8Sint,
            0xF6 => RenderTargetFormat::R8Uint,
            0xF8 => RenderTargetFormat::X1R5G5B5Unorm,
            0xF9 => RenderTargetFormat::X8B8G8R8Unorm,
            0xFA => RenderTargetFormat::X8B8G8R8Srgb,
            _ => RenderTargetFormat::None,
        }
    }

    fn active_regs(&self) -> ActiveRegsRaw {
        self.regs_head().active
    }

    #[cfg(test)]
    fn regs_raw(&self) -> RegsRaw {
        *self.regs_head()
    }

    fn src_surface(&self) -> Surface {
        self.active_regs().src.into()
    }

    fn dst_surface(&self) -> Surface {
        self.active_regs().dst.into()
    }

    fn pixels_from_memory(&self) -> PixelsFromMemory {
        self.active_regs().pixels_from_memory
    }

    fn prepare_blit(&self) -> (Surface, Surface, Config) {
        let mut src = self.src_surface();
        let dst = self.dst_surface();
        let args = self.pixels_from_memory();
        let dst_x0 = args.dst_x0();
        let dst_y0 = args.dst_y0();
        let dst_width = args.dst_width();
        let dst_height = args.dst_height();
        let du_dx = args.du_dx();
        let dv_dy = args.dv_dy();
        let mut src_x = args.src_x0();
        let mut src_y = args.src_y0();

        if args.origin() == Origin::Corner {
            src_x -= (du_dx >> 33) << 32;
            src_y -= (dv_dy >> 33) << 32;
        }

        let bytes_per_pixel = if src.format == RenderTargetFormat::None {
            0
        } else {
            surface::bytes_per_block(surface::pixel_format_from_render_target_format(
                src.format as u32,
            ))
        };
        let delegate_to_gpu = src.width > 512
            && src.height > 512
            && bytes_per_pixel <= 8
            && bytes_per_pixel != 0
            && src.format != dst.format;

        let mut config = Config {
            operation: self.operation(),
            filter: args.filter(),
            must_accelerate: du_dx != NULL_DERIVATIVE
                || dv_dy != NULL_DERIVATIVE
                || delegate_to_gpu,
            dst_x0,
            dst_y0,
            dst_x1: dst_x0 + dst_width,
            dst_y1: dst_y0 + dst_height,
            src_x0: (src_x >> 32) as i32,
            src_y0: (src_y >> 32) as i32,
            src_x1: ((src_x + du_dx * dst_width as i64) >> 32) as i32,
            src_y1: ((src_y + dv_dy * dst_height as i64) >> 32) as i32,
        };

        let need_align_to_pitch = src.linear == MemoryLayout::Pitch
            && src.width as i32 == config.src_x1
            && bytes_per_pixel != 0
            && config.src_x1 > (src.pitch / bytes_per_pixel) as i32
            && config.src_x0 > 0;
        if need_align_to_pitch {
            let address = src.address() + config.src_x0 as u64 * bytes_per_pixel as u64;
            src.addr_upper = (address >> 32) as u32;
            src.addr_lower = address as u32;
            src.width -= config.src_x0 as u32;
            config.src_x1 -= config.src_x0;
            config.src_x0 = 0;
        }

        (src, dst, config)
    }

    // ── Blit handling ──────────────────────────────────────────────────

    fn handle_blit(&mut self) {
        log::debug!(
            "Fermi2D: BLIT dst=0x{:X} ({}x{} pitch={} fmt={}) src=0x{:X} ({}x{} pitch={} fmt={})",
            self.dst_addr(),
            self.dst_width(),
            self.dst_height(),
            self.dst_pitch(),
            self.dst_format(),
            self.src_addr(),
            self.src_width(),
            self.src_height(),
            self.src_pitch(),
            self.src_format(),
        );
        if self.operation() != Operation::SrcCopy {
            log::warn!("Fermi2D: operation {:?} is not SrcCopy", self.operation());
        }
        if self.src_surface().layer != 0 {
            log::warn!("Fermi2D: source layer is not zero");
        }
        if self.dst_surface().layer != 0 {
            log::warn!("Fermi2D: destination layer is not zero");
        }
        if self.src_surface().depth != 1 {
            log::warn!("Fermi2D: source depth is not one");
        }
        if self.clip_enable() != 0 {
            log::warn!("Fermi2D: clipped blit enabled");
        }
        self.execute_blit();
    }

    fn execute_blit(&mut self) {
        self.pending_blit = false;
        let (src_surface, dst_surface, blit_config) = self.prepare_blit();
        self.memory_manager.lock().flush_caching();

        if let Some(rasterizer_handle) = self.rasterizer {
            let rasterizer = unsafe { rasterizer_handle.as_mut() };
            if rasterizer.accelerate_surface_copy(&src_surface, &dst_surface, &blit_config) {
                return;
            }
        }

        self.sw_blitter
            .blit(&src_surface, &dst_surface, &blit_config);
    }
}

impl EngineInterface for Fermi2D {
    fn call_method(&mut self, method: u32, method_argument: u32, is_last_call: bool) {
        Fermi2D::call_method(self, method, method_argument, is_last_call);
    }

    fn call_multi_method(
        &mut self,
        method: u32,
        base_start: &[u32],
        amount: u32,
        methods_pending: u32,
    ) {
        Fermi2D::call_multi_method(self, method, base_start, amount, methods_pending);
    }

    fn consume_sink_impl(&mut self) {
        let sink = std::mem::take(&mut self.interface_state.method_sink);
        for (method, value) in sink {
            let idx = method as usize;
            assert!(
                idx < NUM_REGS_WORDS,
                "Invalid Fermi2D register 0x{method:X}, expected < 0x{NUM_REGS_WORDS:X}"
            );
            self.upstream_reg_array_mut()[idx] = value;
        }
    }

    fn execution_mask(&self) -> &[bool] {
        &self.interface_state.execution_mask
    }

    fn push_method_sink(&mut self, method: u32, value: u32) {
        self.interface_state.method_sink.push((method, value));
    }

    fn set_current_dma_segment(&mut self, segment: u64) {
        self.interface_state.current_dma_segment = segment;
    }

    fn current_dirty(&self) -> bool {
        self.interface_state.current_dirty
    }

    fn set_current_dirty(&mut self, dirty: bool) {
        self.interface_state.current_dirty = dirty;
    }
}

#[cfg(test)]
impl Default for Fermi2D {
    fn default() -> Self {
        Self::new(Arc::new(Mutex::new(MemoryManager::new(0))))
    }
}

impl Engine for Fermi2D {
    fn class_id(&self) -> ClassId {
        ClassId::Twod
    }

    fn write_reg(&mut self, method: u32, value: u32) {
        log::trace!("Fermi2D: reg[0x{:X}] = 0x{:X}", method, value);
        <Self as EngineInterface>::call_method(self, method, value, true);
    }

    fn execute_pending(&mut self, _read_gpu: &dyn Fn(u64, &mut [u8])) -> Vec<PendingWrite> {
        if !self.pending_blit {
            return vec![];
        }
        self.execute_blit();
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
    use std::sync::Arc;

    fn install_test_device_memory(
        mm: &mut crate::memory_manager::MemoryManager,
        backing: &mut [u8],
    ) {
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x3000,
            unsafe { backing.as_mut_ptr().add(0x3000) },
            0x4000_3000,
            0x1000,
            5,
            true,
        );
        device_memory.smmu_map_with_cpu_backing(
            0x4000,
            unsafe { backing.as_mut_ptr().add(0x4000) },
            0x4000_4000,
            0x1000,
            5,
            true,
        );
        mm.set_device_memory_manager(device_memory);
    }

    #[test]
    fn test_address_accessors() {
        let mut eng = Fermi2D::default();
        eng.write_reg(DST_ADDR_HIGH, 0x0000_00AB);
        eng.write_reg(DST_ADDR_LOW, 0xCDEF_0000);
        assert_eq!(eng.dst_addr(), 0xAB_CDEF_0000);

        eng.write_reg(SRC_ADDR_HIGH, 0x12);
        eng.write_reg(SRC_ADDR_LOW, 0x3456_7890);
        assert_eq!(eng.src_addr(), 0x12_3456_7890);
    }

    #[test]
    fn test_surface_accessors() {
        let mut eng = Fermi2D::default();
        eng.write_reg(DST_WIDTH, 1280);
        eng.write_reg(DST_HEIGHT, 720);
        eng.write_reg(DST_PITCH, 5120);
        eng.write_reg(DST_FORMAT, 0xA5);
        assert_eq!(eng.dst_width(), 1280);
        assert_eq!(eng.dst_height(), 720);
        assert_eq!(eng.dst_pitch(), 5120);
        assert_eq!(eng.dst_format(), 0xA5);

        eng.write_reg(SRC_WIDTH, 640);
        eng.write_reg(SRC_HEIGHT, 480);
        eng.write_reg(SRC_PITCH, 2560);
        eng.write_reg(SRC_FORMAT, 0xB6);
        assert_eq!(eng.src_width(), 640);
        assert_eq!(eng.src_height(), 480);
        assert_eq!(eng.src_pitch(), 2560);
        assert_eq!(eng.src_format(), 0xB6);
    }

    #[test]
    fn test_new_initializes_src_and_dst_depth_to_one() {
        let eng = Fermi2D::default();
        assert_eq!(eng.words()[SRC_DEPTH as usize], 1);
        assert_eq!(eng.words()[DST_DEPTH as usize], 1);
    }

    #[test]
    fn test_surface_descriptor_offsets_match_upstream_layout() {
        let mut eng = Fermi2D::default();
        eng.write_reg(SRC_LINEAR, 1);
        eng.write_reg(SRC_BLOCK_DIMENSIONS, 0x0000_0321);
        eng.write_reg(SRC_DEPTH, 7);
        eng.write_reg(SRC_LAYER, 9);
        eng.write_reg(SRC_PITCH, 512);
        eng.write_reg(SRC_WIDTH, 128);
        eng.write_reg(SRC_HEIGHT, 64);
        eng.write_reg(SRC_ADDR_HIGH, 0x12);
        eng.write_reg(SRC_ADDR_LOW, 0x3456_7890);

        let src = eng.src_surface();
        assert_eq!(src.linear, MemoryLayout::Pitch);
        assert_eq!(src.block_width(), 1);
        assert_eq!(src.block_height(), 2);
        assert_eq!(src.block_depth(), 3);
        assert_eq!(src.depth, 7);
        assert_eq!(src.layer, 9);
        assert_eq!(src.pitch, 512);
        assert_eq!(src.width, 128);
        assert_eq!(src.height, 64);
        assert_eq!(src.address(), 0x12_3456_7890);
    }

    #[test]
    fn test_surface_size_matches_upstream() {
        assert_eq!(std::mem::size_of::<Surface>(), 0x28);
    }

    #[test]
    fn test_pixels_from_memory_size_matches_upstream() {
        assert_eq!(std::mem::size_of::<PixelsFromMemory>(), 0x60);
    }

    #[test]
    fn test_config_size_matches_upstream() {
        assert_eq!(std::mem::size_of::<Config>(), 0x2c);
    }

    #[test]
    fn test_active_regs_window_size_matches_upstream_active_span() {
        assert_eq!(std::mem::size_of::<ActiveRegsRaw>(), 0x6e0);
    }

    #[test]
    fn test_regs_raw_size_matches_num_regs() {
        assert_eq!(std::mem::size_of::<RegsRaw>(), NUM_REGS_WORDS * 4);
    }

    #[test]
    fn test_regs_storage_size_matches_engine_reg_count() {
        assert_eq!(std::mem::size_of::<RegsRawTail0x8e0To0x960>(), 0x80);
        assert_eq!(
            std::mem::size_of::<RegsRuntimeTailRaw>(),
            (ENGINE_REG_COUNT - NUM_REGS_WORDS) * 4
        );
        assert_eq!(std::mem::size_of::<RegsStorageRaw>(), ENGINE_REG_COUNT * 4);
        assert_eq!(std::mem::offset_of!(RegsStorageRaw, regs), 0x0);
        assert_eq!(
            std::mem::offset_of!(RegsStorageRaw, runtime_tail),
            NUM_REGS_WORDS * 4
        );
    }

    #[test]
    fn test_regs_prefix_offsets_match_upstream() {
        assert_eq!(std::mem::offset_of!(RegsRaw, prefix), 0x0);
        assert_eq!(std::mem::offset_of!(RegsPrefixRaw, object), 0x0);
        assert_eq!(std::mem::offset_of!(RegsPrefixRaw, no_operation), 0x100);
        assert_eq!(std::mem::offset_of!(RegsPrefixRaw, notify), 0x104);
        assert_eq!(std::mem::offset_of!(RegsPrefixRaw, wait_for_idle), 0x110);
        assert_eq!(std::mem::offset_of!(RegsPrefixRaw, pm_trigger), 0x140);
        assert_eq!(
            std::mem::offset_of!(RegsPrefixRaw, context_dma_notify),
            0x180
        );
        assert_eq!(std::mem::offset_of!(RegsPrefixRaw, dst_context_dma), 0x184);
        assert_eq!(std::mem::offset_of!(RegsPrefixRaw, src_context_dma), 0x188);
        assert_eq!(
            std::mem::offset_of!(RegsPrefixRaw, semaphore_context_dma),
            0x18c
        );
        assert_eq!(std::mem::offset_of!(RegsRaw, active), 0x200);
    }

    #[test]
    fn test_regs_raw_reads_header_fields_like_upstream() {
        let mut eng = Fermi2D::default();
        eng.write_reg(0x00, 0x1122_3344);
        eng.write_reg(0x40, 0x5566_7788);
        eng.write_reg(0x41, 0x99AA_BBCC);
        eng.write_reg(0x44, 0xDDEE_FF00);
        eng.write_reg(0x50, 0x1234_5678);
        eng.write_reg(0x60, 0x89AB_CDEF);
        eng.write_reg(0x61, 0x0246_8ACE);
        eng.write_reg(0x62, 0x1357_9BDF);
        eng.write_reg(0x63, 0x0BAD_F00D);

        let regs = eng.regs_raw();
        assert_eq!(regs.prefix.object, 0x1122_3344);
        assert_eq!(regs.prefix.no_operation, 0x5566_7788);
        assert_eq!(regs.prefix.notify.0, 0x99AA_BBCC);
        assert_eq!(regs.prefix.wait_for_idle, 0xDDEE_FF00);
        assert_eq!(regs.prefix.pm_trigger, 0x1234_5678);
        assert_eq!(regs.prefix.context_dma_notify, 0x89AB_CDEF);
        assert_eq!(regs.prefix.dst_context_dma, 0x0246_8ACE);
        assert_eq!(regs.prefix.src_context_dma, 0x1357_9BDF);
        assert_eq!(regs.prefix.semaphore_context_dma, 0x0BAD_F00D);
    }

    #[test]
    fn test_typed_raw_wrappers_decode_upstream_enum_values() {
        assert_eq!(NotifyTypeRaw(1).get(), NotifyType::WriteThenAwaken);
        assert!(BoolBitRaw(1).get());
        assert_eq!(
            RenderTargetFormatRaw(RenderTargetFormat::A8B8G8R8Unorm as u32).get(),
            RenderTargetFormat::A8B8G8R8Unorm
        );
        assert_eq!(MemoryLayoutRaw(1).get(), MemoryLayout::Pitch);
        let block = BlockDimensionsRaw { raw: 0x321 };
        assert_eq!(block.block_width(), 0x1);
        assert_eq!(block.block_height(), 0x2);
        assert_eq!(block.block_depth(), 0x3);
        assert_eq!(Kind2dCheckEnableRaw(0x12).get(), 0x12);
        assert_eq!(BlockShapeRaw(0xB).get(), 0x3);
        assert_eq!(CorralSizeRaw(0x3F).get(), 0x1F);
        assert!(SafeOverlapRaw(1).get());
        assert_eq!(RopRaw(0x123).get(), 0x23);
        assert_eq!(Beta1Raw(0x34).get(), 0x34);
        assert_eq!(OperationRaw(4).get(), Operation::Rop);
        assert_eq!(CpuIndexWrapRaw(1).get(), CpuIndexWrap::NoWrap);
        assert_eq!(SectorPromotionRaw(3).get(), SectorPromotion::PromoteTo4);
        assert_eq!(NumTpcsRaw(1).get(), NumTpcs::One);
        assert_eq!(
            RenderEnableModeRaw(4).get(),
            RenderEnableMode::RenderIfNotEqual
        );
        assert_eq!(ColorKeyFormatRaw(6).get(), ColorKeyFormat::Y32);
        let active = ActiveRegsRaw {
            render_enable_addr_upper: 0x1122_3344,
            render_enable_addr_lower: 0x5566_7788,
            render_enable_mode: RenderEnableModeRaw(3),
            clip_x0: 0x11,
            clip_y0: 0x22,
            clip_width: 0x33,
            clip_height: 0x44,
            clip_enable: BoolBitRaw(1),
            color_key_format: ColorKeyFormatRaw(6),
            color_key: 0xAABB_CCDD,
            color_key_enable: BoolBitRaw(1),
            ..Default::default()
        };
        assert_eq!(active.render_enable_address(), 0x1122_3344_5566_7788);
        assert_eq!(active.render_enable_mode(), RenderEnableMode::RenderIfEqual);
        assert_eq!(active.clip_x0, 0x11);
        assert_eq!(active.clip_y0, 0x22);
        assert_eq!(active.clip_width, 0x33);
        assert_eq!(active.clip_height, 0x44);
        assert!(active.clip_enabled());
        assert_eq!(active.color_key_format(), ColorKeyFormat::Y32);
        assert_eq!(active.color_key, 0xAABB_CCDD);
        assert!(active.color_key_enabled());
        assert_eq!(PatternSelectRaw(3).get(), PatternSelect::Color);
        assert_eq!(PatternOffsetRaw { raw: 0x2A15 }.x(), 0x15);
        assert_eq!(PatternOffsetRaw { raw: 0x2A15 }.y(), 0x2A);
        assert_eq!(
            MonochromePatternColorFormatRaw(5).get(),
            MonochromePatternColorFormat::Y32
        );
        assert_eq!(
            MonochromePatternFormatRaw(1).get(),
            MonochromePatternFormat::LeM1
        );
        assert_eq!(BigEndianControlRaw(0x56).get(), 0x56);
        let monochrome = MonochromePatternRaw {
            color_format: MonochromePatternColorFormatRaw(5),
            format: MonochromePatternFormatRaw(1),
            color0: 0x1122_3344,
            color1: 0x5566_7788,
            pattern0: 0x89AB_CDEF,
            pattern1: 0x0246_8ACE,
        };
        assert_eq!(monochrome.color_format(), MonochromePatternColorFormat::Y32);
        assert_eq!(monochrome.format(), MonochromePatternFormat::LeM1);
        assert_eq!(monochrome.color0(), 0x1122_3344);
        assert_eq!(monochrome.color1(), 0x5566_7788);
        assert_eq!(monochrome.pattern0(), 0x89AB_CDEF);
        assert_eq!(monochrome.pattern1(), 0x0246_8ACE);
        let color_pattern = ColorPatternRaw {
            x8r8g8b8: {
                let mut values = [0; 0x40];
                values[1] = 0x1122_3344;
                values
            },
            r5g6b5: {
                let mut values = [0; 0x20];
                values[2] = 0x5566;
                values
            },
            x1r5g5b5: {
                let mut values = [0; 0x20];
                values[3] = 0x7788;
                values
            },
            y8: {
                let mut values = [0; 0x10];
                values[4] = 0x99;
                values
            },
        };
        assert_eq!(color_pattern.x8r8g8b8(1), 0x1122_3344);
        assert_eq!(color_pattern.r5g6b5(2), 0x5566);
        assert_eq!(color_pattern.x1r5g5b5(3), 0x7788);
        assert_eq!(color_pattern.y8(4), 0x99);
        let render_solid = RenderSolidRaw {
            prim_mode: 0x11,
            prim_color_format: 0x22,
            prim_color: 0x3344_5566,
            line_tie_break_bits: 0x77,
            prim_point_xy: 0x88,
            prim_point: {
                let mut points = [PointRaw::default(); 0x40];
                points[3] = PointRaw { x: 0x99, y: 0xAA };
                points
            },
            ..Default::default()
        };
        assert_eq!(render_solid.prim_mode(), 0x11);
        assert_eq!(render_solid.prim_color_format(), 0x22);
        assert_eq!(render_solid.prim_color(), 0x3344_5566);
        assert_eq!(render_solid.line_tie_break_bits(), 0x77);
        assert_eq!(render_solid.prim_point_xy(), 0x88);
        let point = render_solid.prim_point(3);
        assert_eq!(point.x(), 0x99);
        assert_eq!(point.y(), 0xAA);
        let pixels_from_cpu = PixelsFromCpuRaw {
            data_type: 0x11,
            color_format: 0x22,
            index_format: 0x33,
            mono_format: 0x44,
            wrap: 0x55,
            color0: 0x6677_8899,
            color1: 0xAABB_CCDD,
            mono_opacity: 0xEE,
            src_width: 0x100,
            src_height: 0x200,
            data: 0x1357_9BDF,
            ..Default::default()
        };
        assert_eq!(pixels_from_cpu.data_type(), 0x11);
        assert_eq!(pixels_from_cpu.color_format(), 0x22);
        assert_eq!(pixels_from_cpu.index_format(), 0x33);
        assert_eq!(pixels_from_cpu.mono_format(), 0x44);
        assert_eq!(pixels_from_cpu.wrap(), 0x55);
        assert_eq!(pixels_from_cpu.color0(), 0x6677_8899);
        assert_eq!(pixels_from_cpu.color1(), 0xAABB_CCDD);
        assert_eq!(pixels_from_cpu.mono_opacity(), 0xEE);
        assert_eq!(pixels_from_cpu.src_width(), 0x100);
        assert_eq!(pixels_from_cpu.src_height(), 0x200);
        let pixels_from_cpu = PixelsFromCpuRaw {
            dx_du_frac: 0x10,
            dx_du_int: 0x20,
            dx_dv_frac: 0x30,
            dy_dv_int: 0x40,
            dst_x0_frac: 0x50,
            dst_x0_int: 0x60,
            dst_y0_frac: 0x70,
            dst_y0_int: 0x80,
            ..pixels_from_cpu
        };
        assert_eq!(pixels_from_cpu.dx_du_frac(), 0x10);
        assert_eq!(pixels_from_cpu.dx_du_int(), 0x20);
        assert_eq!(pixels_from_cpu.dx_dv_frac(), 0x30);
        assert_eq!(pixels_from_cpu.dy_dv_int(), 0x40);
        assert_eq!(pixels_from_cpu.dst_x0_frac(), 0x50);
        assert_eq!(pixels_from_cpu.dst_x0_int(), 0x60);
        assert_eq!(pixels_from_cpu.dst_y0_frac(), 0x70);
        assert_eq!(pixels_from_cpu.dst_y0_int(), 0x80);
        assert_eq!(pixels_from_cpu.data(), 0x1357_9BDF);
        let beta = Beta4Raw { raw: 0x4433_2211 };
        assert_eq!(beta.b(), 0x11);
        assert_eq!(beta.g(), 0x22);
        assert_eq!(beta.r(), 0x33);
        assert_eq!(beta.a(), 0x44);

        let args = PixelsFromMemory {
            block_shape: BlockShapeRaw(0xA),
            corral_size: CorralSizeRaw(0x22),
            safe_overlap: SafeOverlapRaw(1),
            ..Default::default()
        };
        assert_eq!(args.block_shape(), 0x2);
        assert_eq!(args.corral_size(), 0x2);
        assert!(args.safe_overlap());
    }

    #[test]
    fn test_storage_words_cover_union_head_and_rust_tail_contiguously() {
        let mut eng = Fermi2D::default();
        eng.write_reg((NUM_REGS_WORDS - 1) as u32, 0xCAFE_BABE);
        eng.words_mut()[NUM_REGS_WORDS] = 0xDEAD_BEEF;

        let words = eng.words();
        assert_eq!(words[NUM_REGS_WORDS - 1], 0xCAFE_BABE);
        assert_eq!(words[NUM_REGS_WORDS], 0xDEAD_BEEF);
    }

    #[test]
    #[should_panic(expected = "Invalid Fermi2D register")]
    fn test_call_method_rejects_registers_past_upstream_num_regs() {
        let mut eng = Fermi2D::default();
        eng.call_method(NUM_REGS_WORDS as u32, 0xDEAD_BEEF, true);
    }

    #[test]
    fn test_active_regs_window_offsets_match_upstream() {
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, dst), 0x0);
        assert_eq!(
            std::mem::offset_of!(ActiveRegsRaw, pixels_from_cpu_index_wrap),
            0x28
        );
        assert_eq!(
            std::mem::offset_of!(ActiveRegsRaw, kind2d_check_enable),
            0x2c
        );
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, src), 0x30);
        assert_eq!(
            std::mem::offset_of!(ActiveRegsRaw, pixels_from_memory_sector_promotion),
            0x58
        );
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, num_tpcs), 0x60);
        assert_eq!(
            std::mem::offset_of!(ActiveRegsRaw, render_enable_addr_upper),
            0x64
        );
        assert_eq!(
            std::mem::offset_of!(ActiveRegsRaw, render_enable_addr_lower),
            0x68
        );
        assert_eq!(
            std::mem::offset_of!(ActiveRegsRaw, render_enable_mode),
            0x6c
        );
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, clip_x0), 0x80);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, clip_y0), 0x84);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, clip_width), 0x88);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, clip_height), 0x8c);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, clip_enable), 0x90);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, color_key_format), 0x94);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, color_key), 0x98);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, color_key_enable), 0x9c);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, rop), 0xa0);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, beta1), 0xa4);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, beta4), 0xa8);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, operation), 0xac);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, pattern_offset), 0xb0);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, pattern_select), 0xb4);
        assert_eq!(
            std::mem::offset_of!(ActiveRegsRaw, monochrome_pattern),
            0xe8
        );
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, color_pattern), 0x100);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, render_solid), 0x380);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, pixels_from_cpu), 0x600);
        assert_eq!(
            std::mem::offset_of!(ActiveRegsRaw, big_endian_control),
            0x670
        );
        assert_eq!(
            std::mem::offset_of!(ActiveRegsRaw, pixels_from_memory),
            0x680
        );
    }

    #[test]
    fn test_upstream_assert_reg_position_contracts_match_flat_fields() {
        assert_eq!(
            std::mem::offset_of!(ActiveRegsRaw, render_enable_addr_upper),
            0x64
        );
        assert_eq!(
            std::mem::offset_of!(ActiveRegsRaw, render_enable_addr_lower),
            0x68
        );
        assert_eq!(
            std::mem::offset_of!(ActiveRegsRaw, render_enable_mode),
            0x6c
        );
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, clip_x0), 0x80);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, clip_y0), 0x84);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, clip_width), 0x88);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, clip_height), 0x8c);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, clip_enable), 0x90);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, color_key_format), 0x94);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, color_key), 0x98);
        assert_eq!(std::mem::offset_of!(ActiveRegsRaw, color_key_enable), 0x9c);
    }

    #[test]
    fn test_nested_active_blocks_match_upstream_sizes() {
        assert_eq!(std::mem::size_of::<MonochromePatternRaw>(), 0x18);
        assert_eq!(std::mem::size_of::<ColorPatternRaw>(), 0x240);
        assert_eq!(std::mem::size_of::<RenderSolidRaw>(), 0x280);
        assert_eq!(std::mem::size_of::<PixelsFromCpuRaw>(), 0x64);
    }

    #[test]
    fn test_pixels_from_memory_decodes_struct_like_upstream() {
        let mut eng = Fermi2D::default();
        eng.write_reg(PIXELS_FROM_MEMORY_SAMPLE_MODE - 3, 7);
        eng.write_reg(PIXELS_FROM_MEMORY_SAMPLE_MODE - 2, 8);
        eng.write_reg(PIXELS_FROM_MEMORY_SAMPLE_MODE - 1, 9);
        eng.write_reg(PIXELS_FROM_MEMORY_SAMPLE_MODE, 0x11);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_X0, (-3i32) as u32);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_Y0, 5);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_WIDTH, 6);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_HEIGHT, 7);
        eng.write_reg(PIXELS_FROM_MEMORY_DU_DX_LOW, 0x89AB_CDEF);
        eng.write_reg(PIXELS_FROM_MEMORY_DU_DX_HIGH, 0x0123_4567);
        eng.write_reg(PIXELS_FROM_MEMORY_DV_DY_LOW, 0x7654_3210);
        eng.write_reg(PIXELS_FROM_MEMORY_DV_DY_HIGH, 0xFEDC_BA98);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_X0_LOW, 0x1111_2222);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_X0_HIGH, 0x3333_4444);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_Y0_LOW, 0x5555_6666);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_Y0_HIGH, 0x7777_8888);

        let args = eng.pixels_from_memory();
        assert_eq!(args.block_shape(), 7 & 0x7);
        assert_eq!(args.corral_size(), 8 & 0x1F);
        assert!(args.safe_overlap());
        assert_eq!(args.origin(), Origin::Corner);
        assert_eq!(args.filter(), Filter::Bilinear);
        assert_eq!(args.dst_x0(), -3);
        assert_eq!(args.dst_y0(), 5);
        assert_eq!(args.dst_width(), 6);
        assert_eq!(args.dst_height(), 7);
        assert_eq!(args.du_dx(), 0x0123_4567_89AB_CDEFu64 as i64);
        assert_eq!(args.dv_dy(), 0xFEDC_BA98_7654_3210u64 as i64);
        assert_eq!(args.src_x0(), 0x3333_4444_1111_2222u64 as i64);
        assert_eq!(args.src_y0(), 0x7777_8888_5555_6666u64 as i64);
    }

    #[test]
    fn test_src_surface_decodes_typed_raw_surface_fields_like_upstream() {
        let mut eng = Fermi2D::default();
        eng.write_reg(SRC_FORMAT, RenderTargetFormat::A8B8G8R8Unorm as u32);
        eng.write_reg(SRC_LINEAR, MemoryLayout::Pitch as u32);
        eng.write_reg(SRC_BLOCK_DIMENSIONS, 0x321);
        eng.write_reg(SRC_DEPTH, 7);
        eng.write_reg(SRC_LAYER, 9);
        eng.write_reg(SRC_PITCH, 0x80);
        eng.write_reg(SRC_WIDTH, 0x40);
        eng.write_reg(SRC_HEIGHT, 0x20);
        eng.write_reg(SRC_ADDR_HIGH, 0x1122_3344);
        eng.write_reg(SRC_ADDR_LOW, 0x5566_7788);

        let src = eng.src_surface();
        assert_eq!(src.format, RenderTargetFormat::A8B8G8R8Unorm);
        assert_eq!(src.linear, MemoryLayout::Pitch);
        assert_eq!(src.block_dimensions, 0x321);
        assert_eq!(src.block_width(), 1);
        assert_eq!(src.block_height(), 2);
        assert_eq!(src.block_depth(), 3);
        assert_eq!(src.depth, 7);
        assert_eq!(src.layer, 9);
        assert_eq!(src.pitch, 0x80);
        assert_eq!(src.width, 0x40);
        assert_eq!(src.height, 0x20);
        assert_eq!(src.addr_upper, 0x1122_3344);
        assert_eq!(src.addr_lower, 0x5566_7788);
    }

    #[test]
    fn test_prepare_blit_decodes_pixels_from_memory_like_upstream() {
        let mut eng = Fermi2D::default();
        eng.write_reg(OPERATION, Operation::SrcCopy as u32);
        eng.write_reg(SRC_FORMAT, RenderTargetFormat::A8B8G8R8Unorm as u32);
        eng.write_reg(DST_FORMAT, RenderTargetFormat::A8B8G8R8Unorm as u32);
        eng.write_reg(SRC_LINEAR, MemoryLayout::Pitch as u32);
        eng.write_reg(SRC_PITCH, 40);
        eng.write_reg(SRC_WIDTH, 8);
        eng.write_reg(SRC_HEIGHT, 8);
        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(DST_WIDTH, 16);
        eng.write_reg(DST_HEIGHT, 8);
        eng.write_reg(PIXELS_FROM_MEMORY_SAMPLE_MODE, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_X0, 10);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_Y0, 20);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_WIDTH, 3);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_HEIGHT, 4);
        eng.write_reg(PIXELS_FROM_MEMORY_DU_DX_LOW, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DU_DX_HIGH, 1);
        eng.write_reg(PIXELS_FROM_MEMORY_DV_DY_LOW, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DV_DY_HIGH, 1);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_X0_LOW, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_X0_HIGH, 2);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_Y0_LOW, 0);
        // This register is the upstream blit trigger.  The test exercises only
        // PrepareBlit, so seed it directly instead of executing the deliberately
        // out-of-bounds synthetic blit.
        eng.words_mut()[PIXELS_FROM_MEMORY_SRC_Y0_HIGH as usize] = 3;

        let (_src, _dst, config) = eng.prepare_blit();
        assert_eq!(config.operation, Operation::SrcCopy);
        assert_eq!(config.filter, Filter::Point);
        assert!(!config.must_accelerate);
        assert_eq!(config.dst_x0, 10);
        assert_eq!(config.dst_y0, 20);
        assert_eq!(config.dst_x1, 13);
        assert_eq!(config.dst_y1, 24);
        assert_eq!(config.src_x0, 2);
        assert_eq!(config.src_y0, 3);
        assert_eq!(config.src_x1, 5);
        assert_eq!(config.src_y1, 7);
    }

    #[test]
    fn test_prepare_blit_corner_origin_adjusts_source_coordinates() {
        let mut eng = Fermi2D::default();
        eng.write_reg(OPERATION, Operation::SrcCopy as u32);
        eng.write_reg(SRC_FORMAT, RenderTargetFormat::A8B8G8R8Unorm as u32);
        eng.write_reg(DST_FORMAT, RenderTargetFormat::A8B8G8R8Unorm as u32);
        eng.write_reg(PIXELS_FROM_MEMORY_SAMPLE_MODE, 1);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_WIDTH, 1);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_HEIGHT, 1);
        eng.write_reg(PIXELS_FROM_MEMORY_DU_DX_LOW, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DU_DX_HIGH, 2);
        eng.write_reg(PIXELS_FROM_MEMORY_DV_DY_LOW, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DV_DY_HIGH, 2);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_X0_LOW, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_X0_HIGH, 5);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_Y0_LOW, 0);
        // This register is the upstream blit trigger; avoid executing while the
        // test is still assembling the synthetic PrepareBlit state.
        eng.words_mut()[PIXELS_FROM_MEMORY_SRC_Y0_HIGH as usize] = 7;

        let (_src, _dst, config) = eng.prepare_blit();
        assert_eq!(config.src_x0, 4);
        assert_eq!(config.src_y0, 6);
    }

    #[test]
    fn test_prepare_blit_aligns_pitch_linear_source_like_upstream() {
        let mut eng = Fermi2D::default();
        eng.write_reg(OPERATION, Operation::SrcCopy as u32);
        eng.write_reg(SRC_FORMAT, RenderTargetFormat::A8B8G8R8Unorm as u32);
        eng.write_reg(DST_FORMAT, RenderTargetFormat::R8Unorm as u32);
        eng.write_reg(SRC_LINEAR, MemoryLayout::Pitch as u32);
        eng.write_reg(SRC_PITCH, 16);
        eng.write_reg(SRC_WIDTH, 5);
        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x2000);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_WIDTH, 4);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_HEIGHT, 1);
        eng.write_reg(PIXELS_FROM_MEMORY_DU_DX_LOW, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DU_DX_HIGH, 1);
        eng.write_reg(PIXELS_FROM_MEMORY_DV_DY_LOW, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DV_DY_HIGH, 1);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_X0_LOW, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_X0_HIGH, 1);

        let (src, _dst, config) = eng.prepare_blit();
        assert_eq!(src.address(), 0x2004);
        assert_eq!(src.width, 4);
        assert_eq!(config.src_x0, 0);
        assert_eq!(config.src_x1, 4);
    }

    #[test]
    fn test_blit_trigger_executes_immediately() {
        let mut eng = Fermi2D::default();
        assert!(!eng.pending_blit);

        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x1000);
        eng.write_reg(DST_WIDTH, 1280);
        eng.write_reg(DST_HEIGHT, 720);
        eng.write_reg(DST_PITCH, 5120);
        eng.write_reg(DST_FORMAT, 0xA5);

        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x2000);
        eng.write_reg(SRC_WIDTH, 1280);
        eng.write_reg(SRC_HEIGHT, 720);
        eng.write_reg(SRC_PITCH, 5120);
        eng.write_reg(SRC_FORMAT, 0xA5);

        // Trigger blit
        eng.write_reg(BLIT_TRIGGER, 1);
        assert!(!eng.pending_blit);
    }

    #[test]
    fn test_call_method_blit_trigger_executes_immediately() {
        let mut eng = Fermi2D::default();
        assert!(!eng.pending_blit);

        eng.call_method(BLIT_TRIGGER, 1, true);
        assert!(!eng.pending_blit);
    }

    #[test]
    fn test_call_multi_method_propagates_upstream_last_call_flag() {
        let mut eng = Fermi2D::default();
        eng.call_method_last_flags.clear();

        eng.call_multi_method(0x100, &[1, 2, 3], 3, 3);
        assert_eq!(eng.call_method_last_flags, vec![false, false, true]);

        eng.call_method_last_flags.clear();
        eng.call_multi_method(0x100, &[1, 2], 2, 5);
        assert_eq!(eng.call_method_last_flags, vec![false, false]);

        eng.call_method_last_flags.clear();
        eng.call_multi_method(0x100, &[1, 2, 3], 2, 0);
        assert_eq!(eng.call_method_last_flags, vec![true, false]);
        assert_eq!(eng.words()[0x100], 2);
    }

    #[test]
    fn test_no_trigger_without_blit_method() {
        let mut eng = Fermi2D::default();
        eng.write_reg(0x100, 42); // Random register
        assert!(!eng.pending_blit);
    }

    #[test]
    fn test_bind_rasterizer_stores_reference() {
        let syncpoints =
            std::sync::Arc::new(crate::host1x::syncpoint_manager::SyncpointManager::new());
        let rasterizer = crate::renderer_null::null_rasterizer::RasterizerNull::new(syncpoints);
        let mut eng = Fermi2D::default();
        assert!(eng.rasterizer.is_none());
        eng.bind_rasterizer(&rasterizer);
        assert!(eng.rasterizer.is_some());
    }

    #[test]
    fn test_execute_pending_uses_rasterizer_surface_copy_hook() {
        let syncpoints =
            std::sync::Arc::new(crate::host1x::syncpoint_manager::SyncpointManager::new());
        let rasterizer = crate::renderer_null::null_rasterizer::RasterizerNull::new(syncpoints);
        let mut eng = Fermi2D::default();

        eng.bind_rasterizer(&rasterizer);
        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(SRC_FORMAT, 0xD5);
        eng.write_reg(SRC_LINEAR, MemoryLayout::Pitch as u32);
        eng.write_reg(SRC_WIDTH, 4);
        eng.write_reg(SRC_HEIGHT, 2);
        eng.write_reg(SRC_PITCH, 16);

        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x2000);
        eng.write_reg(DST_FORMAT, 0xD5);
        eng.write_reg(DST_LINEAR, MemoryLayout::Pitch as u32);
        eng.write_reg(DST_WIDTH, 4);
        eng.write_reg(DST_HEIGHT, 2);
        eng.write_reg(DST_PITCH, 16);

        eng.write_reg(BLIT_TRIGGER, 1);
        assert!(!eng.pending_blit);
    }

    #[test]
    fn test_blit_copies_pixels() {
        let mut eng = Fermi2D::default();

        // Set up matching src/dst: 4x2, pitch=16 (4 pixels * 4 bytes).
        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(SRC_FORMAT, 0xD5);
        eng.write_reg(SRC_LINEAR, MemoryLayout::Pitch as u32);
        eng.write_reg(SRC_WIDTH, 4);
        eng.write_reg(SRC_HEIGHT, 2);
        eng.write_reg(SRC_PITCH, 16);

        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x2000);
        eng.write_reg(DST_FORMAT, 0xD5);
        eng.write_reg(DST_LINEAR, MemoryLayout::Pitch as u32);
        eng.write_reg(DST_WIDTH, 4);
        eng.write_reg(DST_HEIGHT, 2);
        eng.write_reg(DST_PITCH, 16);
        eng.write_reg(PIXELS_FROM_MEMORY_SAMPLE_MODE, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_X0, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_Y0, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_WIDTH, 4);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_HEIGHT, 2);
        eng.write_reg(PIXELS_FROM_MEMORY_DU_DX_LOW, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DU_DX_HIGH, 1);
        eng.write_reg(PIXELS_FROM_MEMORY_DV_DY_LOW, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DV_DY_HIGH, 1);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_X0_LOW, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_X0_HIGH, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_Y0_LOW, 0);

        // Source data: 2 rows of 16 bytes each.
        let src_data: Vec<u8> = (0..32).collect();
        let mut backing = vec![0u8; 0x5000];
        backing[0x3000..0x3000 + src_data.len()].copy_from_slice(&src_data);
        {
            let mut mm = eng.memory_manager.lock();
            install_test_device_memory(&mut mm, &mut backing);
            mm.map(0x1000, 0x3000, src_data.len() as u64, 0xFF, false);
            mm.map(0x2000, 0x4000, src_data.len() as u64, 0xFF, false);
        }

        eng.write_reg(BLIT_TRIGGER, 0);

        assert!(!eng.pending_blit);
        assert_eq!(&backing[0x4000..0x4000 + src_data.len()], &src_data);
    }

    #[test]
    fn test_blit_different_pitches() {
        let mut eng = Fermi2D::default();

        // Source: pitch=8, 2 rows.
        eng.write_reg(SRC_ADDR_HIGH, 0);
        eng.write_reg(SRC_ADDR_LOW, 0x1000);
        eng.write_reg(SRC_FORMAT, 0xD5);
        eng.write_reg(SRC_LINEAR, MemoryLayout::Pitch as u32);
        eng.write_reg(SRC_WIDTH, 2);
        eng.write_reg(SRC_HEIGHT, 2);
        eng.write_reg(SRC_PITCH, 8);

        // Destination: pitch=16 (wider stride), 2 rows.
        eng.write_reg(DST_ADDR_HIGH, 0);
        eng.write_reg(DST_ADDR_LOW, 0x2000);
        eng.write_reg(DST_FORMAT, 0xD5);
        eng.write_reg(DST_LINEAR, MemoryLayout::Pitch as u32);
        eng.write_reg(DST_WIDTH, 4);
        eng.write_reg(DST_HEIGHT, 2);
        eng.write_reg(DST_PITCH, 16);
        eng.write_reg(PIXELS_FROM_MEMORY_SAMPLE_MODE, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_X0, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_Y0, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_WIDTH, 2);
        eng.write_reg(PIXELS_FROM_MEMORY_DST_HEIGHT, 2);
        eng.write_reg(PIXELS_FROM_MEMORY_DU_DX_LOW, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DU_DX_HIGH, 1);
        eng.write_reg(PIXELS_FROM_MEMORY_DV_DY_LOW, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_DV_DY_HIGH, 1);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_X0_LOW, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_X0_HIGH, 0);
        eng.write_reg(PIXELS_FROM_MEMORY_SRC_Y0_LOW, 0);

        // Source: 2 rows * 8 bytes = 16 bytes.
        let src_data: Vec<u8> = vec![
            1, 2, 3, 4, 5, 6, 7, 8, // row 0
            9, 10, 11, 12, 13, 14, 15, 16, // row 1
        ];
        let mut backing = vec![0u8; 0x5000];
        backing[0x3000..0x3000 + src_data.len()].copy_from_slice(&src_data);
        {
            let mut mm = eng.memory_manager.lock();
            install_test_device_memory(&mut mm, &mut backing);
            mm.map(0x1000, 0x3000, src_data.len() as u64, 0xFF, false);
            mm.map(0x2000, 0x4000, 32, 0xFF, false);
        }

        eng.write_reg(BLIT_TRIGGER, 0);
        assert!(!eng.pending_blit);

        // copy_width = min(8, 16) = 8; each row copies 8 bytes into a 16-byte stride.
        let dst = &backing[0x4000..0x4000 + 32];
        assert_eq!(dst.len(), 32); // 2 rows * 16 bytes
        assert_eq!(&dst[0..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&dst[8..16], &[0, 0, 0, 0, 0, 0, 0, 0]); // padding
        assert_eq!(&dst[16..24], &[9, 10, 11, 12, 13, 14, 15, 16]);
        assert_eq!(&dst[24..32], &[0, 0, 0, 0, 0, 0, 0, 0]); // padding
    }
}

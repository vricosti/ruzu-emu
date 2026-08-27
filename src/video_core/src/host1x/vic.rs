// SPDX-FileCopyrightText: Copyright 2024 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/host1x/vic.h` and `vic.cpp`.
//!
//! Video Image Composer (VIC) — reads decoded frames from the frame queue,
//! performs color conversion / compositing, and writes output surfaces.
//!
//! The scalar paths preserve the upstream FFmpeg-backed frame conversion and
//! compositing behavior; upstream's optional SSE4.1 paths remain host-side
//! optimizations rather than separate behavior.

use std::sync::Arc;

use log::info;

use common::scratch_buffer::ScratchBuffer;

use crate::cdma_pusher::ProcessMethodHook;
use crate::guest_memory::{GpuGuestMemoryScoped, GpuMemoryManagerHandle, GuestMemoryFlags};
use crate::host1x::ffmpeg::ffmpeg::Frame;
use crate::host1x::host1x::FrameQueue;
use crate::memory_manager::MemoryManager;

// --------------------------------------------------------------------------
// Pixel type
// --------------------------------------------------------------------------

/// RGBX pixel with 16-bit components.
///
/// Port of `Tegra::Host1x::Pixel`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Pixel {
    pub r: u16,
    pub g: u16,
    pub b: u16,
    pub a: u16,
}

// --------------------------------------------------------------------------
// VideoPixelFormat
// --------------------------------------------------------------------------

/// Video pixel formats used by VIC surfaces.
///
/// Port of `Tegra::Host1x::VideoPixelFormat`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoPixelFormat {
    A8 = 0,
    L8 = 1,
    A4L4 = 2,
    L4A4 = 3,
    R8 = 4,
    A8L8 = 5,
    L8A8 = 6,
    R8G8 = 7,
    G8R8 = 8,
    B5G6R5 = 9,
    R5G6B5 = 10,
    B6G5R5 = 11,
    R5G5B6 = 12,
    A1B5G5R5 = 13,
    A1R5G5B5 = 14,
    B5G5R5A1 = 15,
    R5G5B5A1 = 16,
    A5B5G5R1 = 17,
    A5R1G5B5 = 18,
    B5G5R1A5 = 19,
    R1G5B5A5 = 20,
    X1B5G5R5 = 21,
    X1R5G5B5 = 22,
    B5G5R5X1 = 23,
    R5G5B5X1 = 24,
    A4B4G5R4 = 25,
    A4R4G4B4 = 26,
    B4G4R4A4 = 27,
    R4G4B4A4 = 28,
    B8G8R8 = 29,
    R8G8B8 = 30,
    A8B8G8R8 = 31,
    A8R8G8B8 = 32,
    B8G8R8A8 = 33,
    R8G8B8A8 = 34,
    X8B8G8R8 = 35,
    X8R8G8B8 = 36,
    B8G8R8X8 = 37,
    R8G8B8X8 = 38,
    A8B10G10R10 = 39,
    A2R10G10B10 = 40,
    B10G10R10A2 = 41,
    R10G10B10A2 = 42,
    A4P4 = 43,
    P4A4 = 44,
    P8A8 = 45,
    A8P8 = 46,
    P8 = 47,
    P1 = 48,
    U8V8 = 49,
    V8U8 = 50,
    A8Y8U8V8 = 51,
    V8U8Y8A8 = 52,
    Y8U8V8 = 53,
    Y8V8U8 = 54,
    U8V8Y8 = 55,
    V8U8Y8 = 56,
    Y8U8Y8V8 = 57,
    Y8V8Y8U8 = 58,
    U8Y8V8Y8 = 59,
    V8Y8U8Y8 = 60,
    Y8UvN444 = 61,
    Y8VuN444 = 62,
    Y8UvN422 = 63,
    Y8VuN422 = 64,
    Y8UvN422R = 65,
    Y8VuN422R = 66,
    Y8UvN420 = 67,
    Y8VuN420 = 68,
    Y8U8V8N444 = 69,
    Y8U8V8N422 = 70,
    Y8U8V8N422R = 71,
    Y8U8V8N420 = 72,
    U8 = 73,
    V8 = 74,
}

fn video_pixel_format_from_u32(value: u32) -> Option<VideoPixelFormat> {
    match value {
        31 => Some(VideoPixelFormat::A8B8G8R8),
        32 => Some(VideoPixelFormat::A8R8G8B8),
        35 => Some(VideoPixelFormat::X8B8G8R8),
        67 => Some(VideoPixelFormat::Y8UvN420),
        68 => Some(VideoPixelFormat::Y8VuN420),
        _ => None,
    }
}

// --------------------------------------------------------------------------
// Offset (32-bit VIC variant)
// --------------------------------------------------------------------------

/// 32-bit offset used in VIC register structures.
///
/// Port of `Tegra::Host1x::Offset` in vic.h.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VicOffset {
    offset: u32,
}

impl VicOffset {
    pub fn address(&self) -> u32 {
        self.offset << 8
    }
}

const _: () = assert!(std::mem::size_of::<VicOffset>() == 0x4);

fn bits_u32(value: u32, shift: u32, width: u32) -> u32 {
    (value >> shift) & ((1u32 << width) - 1)
}

fn bits_u64(value: u64, shift: u32, width: u32) -> u64 {
    (value >> shift) & ((1u64 << width) - 1)
}

fn sign_extend_20(value: u64) -> i32 {
    let value = (value & 0xFFFFF) as i32;
    (value << 12) >> 12
}

fn assert_fail_soft(condition: bool, message: impl FnOnce() -> String) {
    if condition {
        return;
    }
    let message = message();
    log::error!("{message}");
    if *common::settings::values().use_debug_asserts.get_value() {
        panic!("{message}");
    }
}

// --------------------------------------------------------------------------
// PlaneOffsets
// --------------------------------------------------------------------------

/// Luma/chroma plane offsets for a surface.
///
/// Port of `Tegra::Host1x::PlaneOffsets`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PlaneOffsets {
    pub luma: VicOffset,
    pub chroma_u: VicOffset,
    pub chroma_v: VicOffset,
}

const _: () = assert!(std::mem::size_of::<PlaneOffsets>() == 0xC);

// --------------------------------------------------------------------------
// SurfaceIndex
// --------------------------------------------------------------------------

/// Surface indices for VIC slot surfaces.
///
/// Port of `Tegra::Host1x::SurfaceIndex`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceIndex {
    Current = 0,
    Previous = 1,
    Next = 2,
    NextNoiseReduced = 3,
    CurrentMotion = 4,
    PreviousMotion = 5,
    PreviousPreviousMotion = 6,
    CombinedMotion = 7,
}

// --------------------------------------------------------------------------
// DXVAHD enums
// --------------------------------------------------------------------------

/// Port of `Tegra::Host1x::DXVAHD_ALPHA_FILL_MODE`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DxvahdAlphaFillMode {
    Opaque = 0,
    Background = 1,
    Destination = 2,
    SourceStream = 3,
    Composited = 4,
    SourceAlpha = 5,
}

/// Port of `Tegra::Host1x::DXVAHD_FRAME_FORMAT`.
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DxvahdFrameFormat {
    Progressive = 0,
    InterlacedTopFieldFirst = 1,
    InterlacedBottomFieldFirst = 2,
    TopField = 3,
    BottomField = 4,
    SubpicProgressive = 5,
    SubpicInterlacedTopFieldFirst = 6,
    SubpicInterlacedBottomFieldFirst = 7,
    SubpicTopField = 8,
    SubpicBottomField = 9,
    TopFieldChromaBottom = 10,
    BottomFieldChromaTop = 11,
    SubpicTopFieldChromaBottom = 12,
    SubpicBottomFieldChromaTop = 13,
}

fn frame_format_from_u64(value: u64) -> Option<DxvahdFrameFormat> {
    match value {
        0 => Some(DxvahdFrameFormat::Progressive),
        3 => Some(DxvahdFrameFormat::TopField),
        4 => Some(DxvahdFrameFormat::BottomField),
        _ => None,
    }
}

/// Port of `Tegra::Host1x::DXVAHD_DEINTERLACE_MODE_PRIVATE`.
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DxvahdDeinterlaceModePrivate {
    Weave = 0,
    BobField = 1,
    Bob = 2,
    Newbob = 3,
    Disi1 = 4,
    WeaveLumaBobFieldChroma = 5,
    Max = 0xF,
}

fn deinterlace_mode_from_u64(value: u64) -> Option<DxvahdDeinterlaceModePrivate> {
    match value {
        0 => Some(DxvahdDeinterlaceModePrivate::Weave),
        1 => Some(DxvahdDeinterlaceModePrivate::BobField),
        4 => Some(DxvahdDeinterlaceModePrivate::Disi1),
        _ => None,
    }
}

/// Port of `Tegra::Host1x::BLK_KIND`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlkKind {
    Pitch = 0,
    Generic16Bx2 = 1,
    BlNaive = 2,
    BlKeplerXbarRaw = 3,
    Vp2Tiled = 15,
}

fn blk_kind_from_u32(value: u32) -> Option<BlkKind> {
    match value {
        0 => Some(BlkKind::Pitch),
        1 => Some(BlkKind::Generic16Bx2),
        2 => Some(BlkKind::BlNaive),
        3 => Some(BlkKind::BlKeplerXbarRaw),
        15 => Some(BlkKind::Vp2Tiled),
        _ => None,
    }
}

// --------------------------------------------------------------------------
// Blend factor enums
// --------------------------------------------------------------------------

/// Port of `Tegra::Host1x::BLEND_SRCFACTC`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendSrcFactC {
    K1 = 0,
    K1TimesDst = 1,
    NegK1TimesDst = 2,
    K1TimesSrc = 3,
    Zero = 4,
}

/// Port of `Tegra::Host1x::BLEND_DSTFACTC`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendDstFactC {
    K1 = 0,
    K2 = 1,
    K1TimesDst = 2,
    NegK1TimesDst = 3,
    NegK1TimesSrc = 4,
    Zero = 5,
    One = 6,
}

/// Port of `Tegra::Host1x::BLEND_SRCFACTA`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendSrcFactA {
    K1 = 0,
    K2 = 1,
    NegK1TimesDst = 2,
    Zero = 3,
    Max = 7,
}

/// Port of `Tegra::Host1x::BLEND_DSTFACTA`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendDstFactA {
    K2 = 0,
    NegK1TimesSrc = 1,
    Zero = 2,
    One = 3,
    Max = 7,
}

// --------------------------------------------------------------------------
// Config structs (bitfield structs represented as raw u32/u64 with accessors)
// --------------------------------------------------------------------------

/// Port of `Tegra::Host1x::PipeConfig` — 0x10 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PipeConfig {
    pub downsample_raw: u32,
    pub reserved2: u32,
    pub reserved3: u32,
    pub reserved4: u32,
}

const _: () = assert!(std::mem::size_of::<PipeConfig>() == 0x10);

/// Port of `Tegra::Host1x::OutputConfig` — 0x10 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct OutputConfig {
    pub flags0: u64,
    pub target_rect_lr: u32,
    pub target_rect_tb: u32,
}

const _: () = assert!(std::mem::size_of::<OutputConfig>() == 0x10);

impl OutputConfig {
    fn target_rect_left(self) -> u32 {
        bits_u32(self.target_rect_lr, 0, 14)
    }

    fn target_rect_right(self) -> u32 {
        bits_u32(self.target_rect_lr, 16, 14)
    }

    fn target_rect_top(self) -> u32 {
        bits_u32(self.target_rect_tb, 0, 14)
    }

    fn target_rect_bottom(self) -> u32 {
        bits_u32(self.target_rect_tb, 16, 14)
    }
}

/// Port of `Tegra::Host1x::OutputSurfaceConfig` — 0x10 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct OutputSurfaceConfig {
    pub format_flags: u32,
    pub surface_dims: u32,
    pub luma_dims: u32,
    pub chroma_dims: u32,
}

const _: () = assert!(std::mem::size_of::<OutputSurfaceConfig>() == 0x10);

impl OutputSurfaceConfig {
    fn out_pixel_format(self) -> Option<VideoPixelFormat> {
        video_pixel_format_from_u32(bits_u32(self.format_flags, 0, 7))
    }

    fn out_block_kind(self) -> Option<BlkKind> {
        blk_kind_from_u32(bits_u32(self.format_flags, 11, 4))
    }

    fn out_block_height(self) -> u32 {
        bits_u32(self.format_flags, 15, 4)
    }

    fn out_surface_width(self) -> u32 {
        bits_u32(self.surface_dims, 0, 14)
    }

    fn out_surface_height(self) -> u32 {
        bits_u32(self.surface_dims, 14, 14)
    }

    fn out_luma_width(self) -> u32 {
        bits_u32(self.luma_dims, 0, 14)
    }

    fn out_luma_height(self) -> u32 {
        bits_u32(self.luma_dims, 14, 14)
    }

    fn out_chroma_width(self) -> u32 {
        bits_u32(self.chroma_dims, 0, 14)
    }

    fn out_chroma_height(self) -> u32 {
        bits_u32(self.chroma_dims, 14, 14)
    }
}

/// Port of `Tegra::Host1x::MatrixStruct` — 0x20 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MatrixStruct {
    pub row0: u64,
    pub row1: u64,
    pub row2: u64,
    pub row3: u64,
}

const _: () = assert!(std::mem::size_of::<MatrixStruct>() == 0x20);

impl MatrixStruct {
    fn matrix_coeff00(self) -> i32 {
        sign_extend_20(bits_u64(self.row0, 0, 20))
    }

    fn matrix_coeff10(self) -> i32 {
        sign_extend_20(bits_u64(self.row0, 20, 20))
    }

    fn matrix_coeff20(self) -> i32 {
        sign_extend_20(bits_u64(self.row0, 40, 20))
    }

    fn matrix_r_shift(self) -> i32 {
        bits_u64(self.row0, 60, 4) as i32
    }

    fn matrix_coeff01(self) -> i32 {
        sign_extend_20(bits_u64(self.row1, 0, 20))
    }

    fn matrix_coeff11(self) -> i32 {
        sign_extend_20(bits_u64(self.row1, 20, 20))
    }

    fn matrix_coeff21(self) -> i32 {
        sign_extend_20(bits_u64(self.row1, 40, 20))
    }

    fn matrix_enable(self) -> bool {
        bits_u64(self.row1, 63, 1) != 0
    }

    fn matrix_coeff02(self) -> i32 {
        sign_extend_20(bits_u64(self.row2, 0, 20))
    }

    fn matrix_coeff12(self) -> i32 {
        sign_extend_20(bits_u64(self.row2, 20, 20))
    }

    fn matrix_coeff22(self) -> i32 {
        sign_extend_20(bits_u64(self.row2, 40, 20))
    }

    fn matrix_coeff03(self) -> i32 {
        sign_extend_20(bits_u64(self.row3, 0, 20))
    }

    fn matrix_coeff13(self) -> i32 {
        sign_extend_20(bits_u64(self.row3, 20, 20))
    }

    fn matrix_coeff23(self) -> i32 {
        sign_extend_20(bits_u64(self.row3, 40, 20))
    }
}

/// Port of `Tegra::Host1x::ClearRectStruct` — 0x10 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ClearRectStruct {
    pub rect0: u32,
    pub rect0_tb: u32,
    pub rect1: u32,
    pub rect1_tb: u32,
}

const _: () = assert!(std::mem::size_of::<ClearRectStruct>() == 0x10);

/// Port of `Tegra::Host1x::SlotConfig` — 0x40 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SlotConfig {
    pub data: [u64; 7],
    pub reserved22: u32,
    pub reserved23: u32,
}

const _: () = assert!(std::mem::size_of::<SlotConfig>() == 0x40);

impl SlotConfig {
    fn slot_enable(self) -> bool {
        bits_u64(self.data[0], 0, 1) != 0
    }

    fn frame_format(self) -> Option<DxvahdFrameFormat> {
        frame_format_from_u64(bits_u64(self.data[0], 16, 4))
    }

    fn deinterlace_mode(self) -> Option<DxvahdDeinterlaceModePrivate> {
        deinterlace_mode_from_u64(bits_u64(self.data[1], 40, 4))
    }

    fn soft_clamp_low(self) -> i32 {
        bits_u64(self.data[2], 0, 10) as i32
    }

    fn soft_clamp_high(self) -> i32 {
        bits_u64(self.data[2], 10, 10) as i32
    }

    fn planar_alpha(self) -> u16 {
        bits_u64(self.data[2], 32, 10) as u16
    }

    fn source_rect_left(self) -> u32 {
        bits_u64(self.data[4], 0, 30) as u32
    }

    fn source_rect_right(self) -> u32 {
        bits_u64(self.data[4], 32, 30) as u32
    }

    fn source_rect_top(self) -> u32 {
        bits_u64(self.data[5], 0, 30) as u32
    }

    fn source_rect_bottom(self) -> u32 {
        bits_u64(self.data[5], 32, 30) as u32
    }

    fn dest_rect_left(self) -> u32 {
        bits_u64(self.data[6], 0, 14) as u32
    }

    fn dest_rect_right(self) -> u32 {
        bits_u64(self.data[6], 16, 14) as u32
    }

    fn dest_rect_top(self) -> u32 {
        bits_u64(self.data[6], 32, 14) as u32
    }

    fn dest_rect_bottom(self) -> u32 {
        bits_u64(self.data[6], 48, 14) as u32
    }
}

/// Port of `Tegra::Host1x::SlotSurfaceConfig` — 0x10 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SlotSurfaceConfig {
    pub format_flags: u32,
    pub surface_dims: u32,
    pub luma_dims: u32,
    pub chroma_dims: u32,
}

const _: () = assert!(std::mem::size_of::<SlotSurfaceConfig>() == 0x10);

impl SlotSurfaceConfig {
    fn slot_pixel_format(self) -> Option<VideoPixelFormat> {
        video_pixel_format_from_u32(bits_u32(self.format_flags, 0, 7))
    }

    fn slot_surface_width(self) -> u32 {
        bits_u32(self.surface_dims, 0, 14)
    }

    fn slot_surface_height(self) -> u32 {
        bits_u32(self.surface_dims, 14, 14)
    }
}

/// Port of `Tegra::Host1x::LumaKeyStruct` — 0x10 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct LumaKeyStruct {
    pub data0: u64,
    pub data1: u64,
}

const _: () = assert!(std::mem::size_of::<LumaKeyStruct>() == 0x10);

/// Port of `Tegra::Host1x::BlendingSlotStruct` — 0x10 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct BlendingSlotStruct {
    pub alpha_k: u32,
    pub factors: u32,
    pub overrides: u32,
    pub override_a_masks: u32,
}

const _: () = assert!(std::mem::size_of::<BlendingSlotStruct>() == 0x10);

/// Port of `Tegra::Host1x::SlotStruct` — 0xB0 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SlotStruct {
    pub config: SlotConfig,
    pub surface_config: SlotSurfaceConfig,
    pub luma_key: LumaKeyStruct,
    pub color_matrix: MatrixStruct,
    pub gamut_matrix: MatrixStruct,
    pub blending: BlendingSlotStruct,
}

const _: () = assert!(std::mem::size_of::<SlotStruct>() == 0xB0);

/// Port of `Tegra::Host1x::ConfigStruct` — 0x610 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ConfigStruct {
    pub pipe_config: PipeConfig,
    pub output_config: OutputConfig,
    pub output_surface_config: OutputSurfaceConfig,
    pub out_color_matrix: MatrixStruct,
    pub clear_rects: [ClearRectStruct; 4],
    pub slot_structs: [SlotStruct; 8],
}

const _: () = assert!(std::mem::size_of::<ConfigStruct>() == 0x610);

impl Default for ConfigStruct {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

// --------------------------------------------------------------------------
// VicRegisters — 0x1118 bytes
// --------------------------------------------------------------------------

/// VIC register file.
///
/// Port of `Tegra::Host1x::VicRegisters`.
pub const VIC_NUM_REGS: usize = 0x446;

#[repr(C)]
#[derive(Clone)]
pub struct VicRegisters {
    pub reg_array: [u32; VIC_NUM_REGS],
}

const _: () = assert!(std::mem::size_of::<VicRegisters>() == 0x1118);

impl Default for VicRegisters {
    fn default() -> Self {
        Self {
            reg_array: [0u32; VIC_NUM_REGS],
        }
    }
}

// VIC register word indices. `VicMethod` below retains Eden's byte offsets.
pub const VIC_REG_EXECUTE: usize = 0x300 / 4;
pub const VIC_REG_SURFACES: usize = 0x400 / 4;
pub const VIC_REG_PICTURE_INDEX: usize = 0x700 / 4;
pub const VIC_REG_CONTROL_PARAMS: usize = 0x704 / 4;
pub const VIC_REG_CONFIG_STRUCT_OFFSET: usize = 0x708 / 4;
pub const VIC_REG_FILTER_STRUCT_OFFSET: usize = 0x70C / 4;
pub const VIC_REG_PALETTE_OFFSET: usize = 0x710 / 4;
pub const VIC_REG_HIST_OFFSET: usize = 0x714 / 4;
pub const VIC_REG_CONTEXT_ID: usize = 0x718 / 4;
pub const VIC_REG_FCE_UCODE_SIZE: usize = 0x71C / 4;
pub const VIC_REG_OUTPUT_SURFACE_LUMA: usize = 0x720 / 4;
pub const VIC_REG_OUTPUT_SURFACE_CHROMA_U: usize = 0x724 / 4;
pub const VIC_REG_OUTPUT_SURFACE_CHROMA_V: usize = 0x728 / 4;
pub const VIC_REG_FCE_UCODE_OFFSET: usize = 0x72C / 4;
pub const VIC_REG_SLOT_CONTEXT_IDS: usize = 0x740 / 4;
pub const VIC_REG_COMP_TAG_BUFFER_OFFSETS: usize = 0x760 / 4;
pub const VIC_REG_HISTORY_BUFFER_OFFSET: usize = 0x780 / 4;
pub const VIC_REG_PM_TRIGGER_END: usize = 0x1114 / 4;

/// VIC method enum for process_method dispatch.
///
/// Port of `Tegra::Host1x::Vic::Method`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VicMethod {
    Execute = 0x300,
    SetControlParams = 0x704,
    SetConfigStructOffset = 0x708,
    SetOutputSurfaceLumaOffset = 0x720,
    SetOutputSurfaceChromaOffset = 0x724,
    SetOutputSurfaceChromaUnusedOffset = 0x728,
}

impl VicMethod {
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0x300 => Some(Self::Execute),
            0x704 => Some(Self::SetControlParams),
            0x708 => Some(Self::SetConfigStructOffset),
            0x720 => Some(Self::SetOutputSurfaceLumaOffset),
            0x724 => Some(Self::SetOutputSurfaceChromaOffset),
            0x728 => Some(Self::SetOutputSurfaceChromaUnusedOffset),
            _ => None,
        }
    }
}

// --------------------------------------------------------------------------
// Vic
// --------------------------------------------------------------------------

/// Video Image Composer device.
///
/// Port of `Tegra::Host1x::Vic`.
pub struct Vic {
    regs: VicRegisters,
    swizzle_scratch: ScratchBuffer<u8>,
    output_surface: ScratchBuffer<Pixel>,
    slot_surface: ScratchBuffer<Pixel>,
    luma_scratch: ScratchBuffer<u8>,
    chroma_scratch: ScratchBuffer<u8>,
    id: i32,
    nvdec_id: i32,
    has_decoded_frame: bool,
    // Current Eden retains the VIC syncpoint from construction without
    // reading it in ProcessMethod or Execute.
    #[allow(dead_code)]
    syncpoint: u32,
    frame_queue: Arc<FrameQueue>,
    memory_manager: Arc<parking_lot::Mutex<MemoryManager>>,
}

impl Vic {
    pub fn new(
        id: i32,
        syncpt: u32,
        frame_queue: Arc<FrameQueue>,
        memory_manager: Arc<parking_lot::Mutex<MemoryManager>>,
    ) -> Self {
        info!("Created vic {}", id);
        Self {
            regs: VicRegisters::default(),
            swizzle_scratch: ScratchBuffer::new(),
            output_surface: ScratchBuffer::new(),
            slot_surface: ScratchBuffer::new(),
            luma_scratch: ScratchBuffer::new(),
            chroma_scratch: ScratchBuffer::new(),
            id,
            nvdec_id: -1,
            has_decoded_frame: false,
            syncpoint: syncpt,
            frame_queue,
            memory_manager,
        }
    }

    /// Write to the device state.
    ///
    /// Port of `Vic::ProcessMethod`.
    pub fn process_method(&mut self, method: u32, arg: u32) {
        log::trace!("Vic {} method {:#x}", self.id, method);
        let method_index = method as usize;
        if method_index < VIC_NUM_REGS {
            self.regs.reg_array[method_index] = arg;
        }

        if method.wrapping_mul(std::mem::size_of::<u32>() as u32) == VicMethod::Execute as u32 {
            self.execute();
        }
    }

    /// Execute the VIC pipeline.
    ///
    /// Port of `Vic::Execute`.
    fn execute(&mut self) {
        let mut config = ConfigStruct::default();
        let config_addr = self.config_struct_address();
        let config_bytes = unsafe {
            std::slice::from_raw_parts_mut(
                (&mut config as *mut ConfigStruct).cast::<u8>(),
                std::mem::size_of::<ConfigStruct>(),
            )
        };
        self.memory_manager
            .lock()
            .read_block(config_addr, config_bytes);

        let output_width = config.output_surface_config.out_surface_width() + 1;
        let output_height = config.output_surface_config.out_surface_height() + 1;
        self.output_surface
            .resize((output_width * output_height) as usize);

        if *common::settings::values().nvdec_emulation.get_value()
            == common::settings_enums::NvdecEmulation::Off
        {
            self.output_surface.fill(Pixel::default());
        } else {
            let mut decoded_frame = false;
            for index in 0..config.slot_structs.len() {
                let slot_config = config.slot_structs[index];
                if !slot_config.config.slot_enable() {
                    continue;
                }

                let luma_offset = self.surface_luma_address(index, SurfaceIndex::Current);
                if self.nvdec_id == -1 {
                    self.nvdec_id = self.frame_queue.vic_find_nvdec_fd_from_offset(luma_offset);
                }

                let Some(frame) = self.frame_queue.get_frame(self.nvdec_id, luma_offset) else {
                    log::trace!(
                        "Vic {} failed to get frame with offset 0x{:X}",
                        self.id,
                        luma_offset
                    );
                    continue;
                };

                match frame.get_pixel_format() {
                    AV_PIX_FMT_YUV420P => self.read_y8_v8u8_n420::<true>(&slot_config, &frame),
                    AV_PIX_FMT_NV12 => self.read_y8_v8u8_n420::<false>(&slot_config, &frame),
                    format => {
                        assert_fail_soft(false, || {
                            format!(
                                "Vic {} unimplemented FFmpeg frame pixel format {} for slot format {:?}",
                                self.id,
                                format,
                                slot_config.surface_config.slot_pixel_format()
                            )
                        });
                    }
                }

                self.blend(
                    &config,
                    &slot_config,
                    config
                        .output_surface_config
                        .out_pixel_format()
                        .unwrap_or(VideoPixelFormat::A8B8G8R8),
                );
                decoded_frame = true;
            }
            if decoded_frame {
                self.has_decoded_frame = true;
            } else if !self.has_decoded_frame {
                return;
            }
        }

        match config.output_surface_config.out_pixel_format() {
            Some(
                format @ (VideoPixelFormat::A8B8G8R8
                | VideoPixelFormat::X8B8G8R8
                | VideoPixelFormat::A8R8G8B8),
            ) => self.write_abgr(config.output_surface_config, format),
            Some(VideoPixelFormat::Y8VuN420) => {
                self.write_y8_v8u8_n420(config.output_surface_config)
            }
            format => {
                assert_fail_soft(false, || {
                    format!(
                        "Vic {} unknown/unimplemented output pixel format {:?}",
                        self.id, format
                    )
                });
            }
        }
    }

    fn config_struct_address(&self) -> u64 {
        (self.regs.reg_array[VIC_REG_CONFIG_STRUCT_OFFSET] as u64) << 8
    }

    fn output_luma_address(&self) -> u64 {
        (self.regs.reg_array[VIC_REG_OUTPUT_SURFACE_LUMA] as u64) << 8
    }

    fn output_chroma_address(&self) -> u64 {
        (self.regs.reg_array[VIC_REG_OUTPUT_SURFACE_CHROMA_U] as u64) << 8
    }

    fn surface_luma_address(&self, slot: usize, surface: SurfaceIndex) -> u64 {
        let index = VIC_REG_SURFACES + slot * 8 * 3 + surface as usize * 3;
        (self.regs.reg_array[index] as u64) << 8
    }

    fn read_y8_v8u8_n420<const PLANAR: bool>(&mut self, slot: &SlotStruct, frame: &Frame) {
        match slot.config.frame_format() {
            Some(DxvahdFrameFormat::Progressive) => {
                self.read_progressive_y8_v8u8_n420::<PLANAR, false>(slot, frame)
            }
            Some(DxvahdFrameFormat::TopField) => {
                self.read_interlaced_y8_v8u8_n420::<PLANAR, true>(slot, frame)
            }
            Some(DxvahdFrameFormat::BottomField) => {
                self.read_interlaced_y8_v8u8_n420::<PLANAR, false>(slot, frame)
            }
            format => log::error!(
                "Vic {} unknown deinterlace frame format {:?}",
                self.id,
                format
            ),
        }
    }

    fn read_interlaced_y8_v8u8_n420<const PLANAR: bool, const TOP_FIELD: bool>(
        &mut self,
        slot: &SlotStruct,
        frame: &Frame,
    ) {
        if !PLANAR {
            self.read_progressive_y8_v8u8_n420::<PLANAR, true>(slot, frame);
            return;
        }

        let out_luma_width = slot.surface_config.slot_surface_width() + 1;
        let out_luma_height = (slot.surface_config.slot_surface_height() + 1) * 2;
        self.slot_surface
            .resize((out_luma_width * out_luma_height) as usize);

        let in_luma_width = (frame.get_width().max(0) as u32).min(out_luma_width);
        let _in_luma_height = (frame.get_height().max(0) as u32).min(out_luma_height);
        let in_chroma_height = (frame.get_height().max(0) as u32 + 1) / 2;
        let in_luma_stride = frame.get_stride(0).max(0) as usize;
        let in_chroma_stride = frame.get_stride(1).max(0) as usize;
        let luma = unsafe {
            frame_plane(
                frame,
                0,
                in_luma_stride.saturating_mul((in_chroma_height * 2) as usize),
            )
        };
        let chroma_u = unsafe {
            frame_plane(
                frame,
                1,
                in_chroma_stride.saturating_mul(in_chroma_height as usize),
            )
        };
        let chroma_v = unsafe {
            frame_plane(
                frame,
                2,
                in_chroma_stride.saturating_mul(in_chroma_height as usize),
            )
        };
        let (Some(luma), Some(chroma_u), Some(chroma_v)) = (luma, chroma_u, chroma_v) else {
            return;
        };

        match slot.config.deinterlace_mode() {
            Some(DxvahdDeinterlaceModePrivate::Weave)
            | Some(DxvahdDeinterlaceModePrivate::BobField)
            | Some(DxvahdDeinterlaceModePrivate::Disi1) => {}
            mode => {
                log::error!("Vic {} unimplemented deinterlace mode {:?}", self.id, mode);
                return;
            }
        }

        let alpha = slot.config.planar_alpha();
        let start_y = u32::from(!TOP_FIELD);
        for y in (start_y..in_chroma_height * 2).step_by(2) {
            let src_luma = y as usize * in_luma_stride;
            let src_chroma = (y / 2) as usize * in_chroma_stride;
            let dst = y as usize * out_luma_width as usize;
            for x in 0..in_luma_width as usize {
                let l = luma.get(src_luma + x).copied().unwrap_or(0);
                let u = chroma_u.get(src_chroma + x / 2).copied().unwrap_or(0);
                let v = chroma_v.get(src_chroma + x / 2).copied().unwrap_or(0);
                self.slot_surface[dst + x] = Pixel {
                    r: (l as u16) << 2,
                    g: (u as u16) << 2,
                    b: (v as u16) << 2,
                    a: alpha,
                };
            }
            let other_y = if TOP_FIELD {
                y + 1
            } else {
                y.saturating_sub(1)
            };
            if other_y < out_luma_height {
                let other = other_y as usize * out_luma_width as usize;
                let row_len = out_luma_width as usize;
                self.slot_surface.copy_within(dst..dst + row_len, other);
            }
        }
    }

    fn read_progressive_y8_v8u8_n420<const PLANAR: bool, const INTERLACED: bool>(
        &mut self,
        slot: &SlotStruct,
        frame: &Frame,
    ) {
        let out_luma_width = slot.surface_config.slot_surface_width() + 1;
        let mut out_luma_height = slot.surface_config.slot_surface_height() + 1;
        if INTERLACED {
            out_luma_height *= 2;
        }
        self.slot_surface
            .resize_destructive((out_luma_width * out_luma_height) as usize);

        let in_luma_width = (frame.get_width().max(0) as u32).min(out_luma_width);
        let in_luma_height = (frame.get_height().max(0) as u32).min(out_luma_height);
        let in_luma_stride = frame.get_stride(0).max(0) as usize;
        let in_chroma_stride = frame.get_stride(1).max(0) as usize;
        let luma = unsafe {
            frame_plane(
                frame,
                0,
                in_luma_stride.saturating_mul(in_luma_height as usize),
            )
        };
        let chroma_u = unsafe {
            frame_plane(
                frame,
                1,
                in_chroma_stride.saturating_mul(((in_luma_height + 1) / 2) as usize),
            )
        };
        let chroma_v = if PLANAR {
            unsafe {
                frame_plane(
                    frame,
                    2,
                    in_chroma_stride.saturating_mul(((in_luma_height + 1) / 2) as usize),
                )
            }
        } else {
            None
        };
        let Some(luma) = luma else {
            return;
        };
        let Some(chroma_u) = chroma_u else {
            return;
        };
        if PLANAR && chroma_v.is_none() {
            return;
        }

        let alpha = slot.config.planar_alpha();
        for y in 0..in_luma_height as usize {
            let src_luma = y * in_luma_stride;
            let src_chroma = (y / 2) * in_chroma_stride;
            let dst = y * out_luma_width as usize;
            for x in 0..in_luma_width as usize {
                let l = luma.get(src_luma + x).copied().unwrap_or(0);
                let (u, v) = if PLANAR {
                    let v_plane = chroma_v.unwrap();
                    (
                        chroma_u.get(src_chroma + x / 2).copied().unwrap_or(0),
                        v_plane.get(src_chroma + x / 2).copied().unwrap_or(0),
                    )
                } else {
                    (
                        chroma_u.get(src_chroma + (x & !1)).copied().unwrap_or(0),
                        chroma_u
                            .get(src_chroma + (x & !1) + 1)
                            .copied()
                            .unwrap_or(0),
                    )
                };
                self.slot_surface[dst + x] = Pixel {
                    r: (l as u16) << 2,
                    g: (u as u16) << 2,
                    b: (v as u16) << 2,
                    a: alpha,
                };
            }
        }
    }

    fn blend(&mut self, config: &ConfigStruct, slot: &SlotStruct, _format: VideoPixelFormat) {
        let add_one = |value: u32| if value != 0 { value + 1 } else { 0 };

        let mut source_left = add_one(slot.config.source_rect_left());
        let mut source_right = add_one(slot.config.source_rect_right());
        let mut source_top = add_one(slot.config.source_rect_top());
        let mut source_bottom = add_one(slot.config.source_rect_bottom());

        let dest_left = add_one(slot.config.dest_rect_left());
        let dest_right = add_one(slot.config.dest_rect_right());
        let dest_top = add_one(slot.config.dest_rect_top());
        let dest_bottom = add_one(slot.config.dest_rect_bottom());

        let mut rect_left = add_one(config.output_config.target_rect_left());
        let mut rect_right = add_one(config.output_config.target_rect_right());
        let mut rect_top = add_one(config.output_config.target_rect_top());
        let mut rect_bottom = add_one(config.output_config.target_rect_bottom());

        rect_left = rect_left.max(dest_left);
        rect_right = rect_right.min(dest_right);
        rect_top = rect_top.max(dest_top);
        rect_bottom = rect_bottom.min(dest_bottom);

        source_left = source_left.max(rect_left);
        source_right = source_right.min(rect_right);
        source_top = source_top.max(rect_top);
        source_bottom = source_bottom.min(rect_bottom);

        if source_left >= source_right || source_top >= source_bottom {
            return;
        }

        let out_surface_width = config.output_surface_config.out_surface_width() + 1;
        let out_surface_height = config.output_surface_config.out_surface_height() + 1;
        let in_surface_width = slot.surface_config.slot_surface_width() + 1;

        source_bottom = source_bottom.min(out_surface_height);
        source_right = source_right.min(out_surface_width);

        if !slot.color_matrix.matrix_enable() {
            let copy_width = (source_right - source_left).min(rect_right - rect_left) as usize;
            for y in source_top..source_bottom {
                let dst_line = y as usize * out_surface_width as usize;
                let src_line = y as usize * in_surface_width as usize;
                let dst = dst_line + rect_left as usize;
                let src = src_line + source_left as usize;
                if dst + copy_width <= self.output_surface.len()
                    && src + copy_width <= self.slot_surface.len()
                {
                    self.output_surface.data_mut()[dst..dst + copy_width]
                        .copy_from_slice(&self.slot_surface.data()[src..src + copy_width]);
                }
            }
            return;
        }

        let matrix = slot.color_matrix;
        let shift = matrix.matrix_r_shift();
        let clamp_min = slot.config.soft_clamp_low();
        let clamp_max = slot.config.soft_clamp_high();
        for y in source_top..source_bottom {
            let src_base = y as usize * in_surface_width as usize + source_left as usize;
            let dst_base = y as usize * out_surface_width as usize + rect_left as usize;
            for x in source_left..source_right {
                let src_index = src_base + x as usize;
                let dst_index = dst_base + x as usize;
                if src_index >= self.slot_surface.len() || dst_index >= self.output_surface.len() {
                    continue;
                }
                let in_pixel = self.slot_surface[src_index];
                let mut r = in_pixel.r as i32 * matrix.matrix_coeff00()
                    + in_pixel.g as i32 * matrix.matrix_coeff01()
                    + in_pixel.b as i32 * matrix.matrix_coeff02();
                let mut g = in_pixel.r as i32 * matrix.matrix_coeff10()
                    + in_pixel.g as i32 * matrix.matrix_coeff11()
                    + in_pixel.b as i32 * matrix.matrix_coeff12();
                let mut b = in_pixel.r as i32 * matrix.matrix_coeff20()
                    + in_pixel.g as i32 * matrix.matrix_coeff21()
                    + in_pixel.b as i32 * matrix.matrix_coeff22();
                r >>= shift;
                g >>= shift;
                b >>= shift;
                r = (r + matrix.matrix_coeff03()) >> 8;
                g = (g + matrix.matrix_coeff13()) >> 8;
                b = (b + matrix.matrix_coeff23()) >> 8;
                self.output_surface[dst_index] = Pixel {
                    r: r.clamp(clamp_min, clamp_max) as u16,
                    g: g.clamp(clamp_min, clamp_max) as u16,
                    b: b.clamp(clamp_min, clamp_max) as u16,
                    a: (in_pixel.a as i32).clamp(clamp_min, clamp_max) as u16,
                };
            }
        }
    }

    fn write_y8_v8u8_n420(&mut self, output_surface_config: OutputSurfaceConfig) {
        const BYTES_PER_PIXEL: u32 = 1;
        let mut surface_width = output_surface_config.out_surface_width() + 1;
        let mut surface_height = output_surface_config.out_surface_height() + 1;
        let surface_stride = surface_width;
        let out_luma_width = output_surface_config.out_luma_width() + 1;
        let out_luma_height = output_surface_config.out_luma_height() + 1;
        let out_luma_stride =
            common::alignment::align_up((out_luma_width * BYTES_PER_PIXEL) as u64, 0x10) as u32;
        let out_luma_size = out_luma_height * out_luma_stride;
        let out_chroma_width = output_surface_config.out_chroma_width() + 1;
        let out_chroma_height = output_surface_config.out_chroma_height() + 1;
        let out_chroma_stride =
            common::alignment::align_up((out_chroma_width * BYTES_PER_PIXEL * 2) as u64, 0x10)
                as u32;
        let out_chroma_size = out_chroma_height * out_chroma_stride;
        surface_width = surface_width.min(out_luma_width);
        surface_height = surface_height.min(out_luma_height);

        let decode = |out_luma: &mut [u8], out_chroma: &mut [u8], input: &[Pixel]| {
            for y in 0..surface_height {
                let src_luma = y as usize * surface_stride as usize;
                let dst_luma = y as usize * out_luma_stride as usize;
                let src_chroma = y as usize * surface_stride as usize;
                let dst_chroma = (y / 2) as usize * out_chroma_stride as usize;
                let mut x = 0;
                while x < surface_width {
                    let p0 = input[src_luma + x as usize];
                    let p1 = input[src_luma + x as usize + 1];
                    out_luma[dst_luma + x as usize] = (p0.r >> 2) as u8;
                    out_luma[dst_luma + x as usize + 1] = (p1.r >> 2) as u8;
                    let chroma = input[src_chroma + x as usize];
                    out_chroma[dst_chroma + x as usize] = (chroma.g >> 2) as u8;
                    out_chroma[dst_chroma + x as usize + 1] = (chroma.b >> 2) as u8;
                    x += 2;
                }
            }
        };

        match output_surface_config.out_block_kind() {
            Some(BlkKind::Generic16Bx2) => {
                let block_height = output_surface_config.out_block_height();
                let luma_size = crate::textures::decoders::calculate_size(
                    true,
                    BYTES_PER_PIXEL,
                    out_luma_width,
                    out_luma_height,
                    1,
                    block_height,
                    0,
                );
                let chroma_size = crate::textures::decoders::calculate_size(
                    true,
                    BYTES_PER_PIXEL * 2,
                    out_chroma_width,
                    out_chroma_height,
                    1,
                    block_height,
                    0,
                );

                self.luma_scratch.resize_destructive(out_luma_size as usize);
                self.chroma_scratch
                    .resize_destructive(out_chroma_size as usize);
                decode(
                    self.luma_scratch.data_mut(),
                    self.chroma_scratch.data_mut(),
                    self.output_surface.data(),
                );

                let memory = GpuMemoryManagerHandle::new(Arc::clone(&self.memory_manager));
                let mut out_luma = GpuGuestMemoryScoped::<u8>::new(
                    &memory,
                    self.output_luma_address(),
                    luma_size,
                    GuestMemoryFlags::SAFE_WRITE,
                );
                let mut out_chroma = GpuGuestMemoryScoped::<u8>::new(
                    &memory,
                    self.output_chroma_address(),
                    chroma_size,
                    GuestMemoryFlags::SAFE_WRITE,
                );
                if block_height == 1 {
                    swizzle_surface(
                        unsafe { out_luma.as_mut_slice() },
                        out_luma_stride,
                        &self.luma_scratch,
                        out_luma_stride,
                        out_luma_height,
                    );
                } else {
                    crate::textures::decoders::swizzle_texture(
                        unsafe { out_luma.as_mut_slice() },
                        &self.luma_scratch,
                        BYTES_PER_PIXEL,
                        out_luma_width,
                        out_luma_height,
                        1,
                        block_height,
                        0,
                        1,
                    );
                }
                if block_height == 1 {
                    swizzle_surface(
                        unsafe { out_chroma.as_mut_slice() },
                        out_chroma_stride,
                        &self.chroma_scratch,
                        out_chroma_stride,
                        out_chroma_height,
                    );
                } else {
                    crate::textures::decoders::swizzle_texture(
                        unsafe { out_chroma.as_mut_slice() },
                        &self.chroma_scratch,
                        BYTES_PER_PIXEL,
                        out_chroma_width,
                        out_chroma_height,
                        1,
                        block_height,
                        0,
                        1,
                    );
                }
            }
            Some(BlkKind::Pitch) => {
                self.luma_scratch.resize_destructive(out_luma_size as usize);
                self.chroma_scratch
                    .resize_destructive(out_chroma_size as usize);
                decode(
                    self.luma_scratch.data_mut(),
                    self.chroma_scratch.data_mut(),
                    self.output_surface.data(),
                );
                let _ = self
                    .memory_manager
                    .lock()
                    .write_block(self.output_luma_address(), self.luma_scratch.data());
                let _ = self
                    .memory_manager
                    .lock()
                    .write_block(self.output_chroma_address(), self.chroma_scratch.data());
            }
            kind => panic!("Unexpected VIC output block kind {kind:?}"),
        }
    }

    fn write_abgr(&mut self, output_surface_config: OutputSurfaceConfig, format: VideoPixelFormat) {
        const BYTES_PER_PIXEL: u32 = 4;
        let mut surface_width = output_surface_config.out_surface_width() + 1;
        let mut surface_height = output_surface_config.out_surface_height() + 1;
        let surface_stride = surface_width;
        let out_luma_width = output_surface_config.out_luma_width() + 1;
        let out_luma_height = output_surface_config.out_luma_height() + 1;
        let out_luma_stride =
            common::alignment::align_up((out_luma_width * BYTES_PER_PIXEL) as u64, 0x10) as u32;
        let out_luma_size = out_luma_height * out_luma_stride;
        surface_width = surface_width.min(out_luma_width);
        surface_height = surface_height.min(out_luma_height);
        let decode = |out: &mut [u8], input: &[Pixel]| {
            for y in 0..surface_height {
                let src = y as usize * surface_stride as usize;
                let dst = y as usize * out_luma_stride as usize;
                for x in 0..surface_width as usize {
                    let pixel = input[src + x];
                    let offset = dst + x * 4;
                    if format == VideoPixelFormat::A8R8G8B8 {
                        out[offset] = (pixel.b >> 2) as u8;
                        out[offset + 1] = (pixel.g >> 2) as u8;
                        out[offset + 2] = (pixel.r >> 2) as u8;
                        out[offset + 3] = (pixel.a >> 2) as u8;
                    } else {
                        out[offset] = (pixel.r >> 2) as u8;
                        out[offset + 1] = (pixel.g >> 2) as u8;
                        out[offset + 2] = (pixel.b >> 2) as u8;
                        out[offset + 3] = (pixel.a >> 2) as u8;
                    }
                }
            }
        };

        match output_surface_config.out_block_kind() {
            Some(BlkKind::Generic16Bx2) => {
                let block_height = output_surface_config.out_block_height();
                let swizzled_size = crate::textures::decoders::calculate_size(
                    true,
                    BYTES_PER_PIXEL,
                    out_luma_width,
                    out_luma_height,
                    1,
                    block_height,
                    0,
                );
                self.luma_scratch.resize_destructive(out_luma_size as usize);
                decode(self.luma_scratch.data_mut(), self.output_surface.data());
                let output_address = self.output_luma_address();
                let memory = GpuMemoryManagerHandle::new(Arc::clone(&self.memory_manager));
                let mut out_luma = GpuGuestMemoryScoped::<u8>::new_with_backup(
                    &memory,
                    output_address,
                    swizzled_size,
                    GuestMemoryFlags::SAFE_WRITE,
                    &mut self.swizzle_scratch,
                );
                if block_height == 1 {
                    swizzle_surface(
                        unsafe { out_luma.as_mut_slice() },
                        out_luma_stride,
                        &self.luma_scratch,
                        out_luma_stride,
                        out_luma_height,
                    );
                } else {
                    crate::textures::decoders::swizzle_texture(
                        unsafe { out_luma.as_mut_slice() },
                        &self.luma_scratch,
                        BYTES_PER_PIXEL,
                        out_luma_width,
                        out_luma_height,
                        1,
                        block_height,
                        0,
                        1,
                    );
                }
            }
            Some(BlkKind::Pitch) => {
                let output_address = self.output_luma_address();
                let memory = GpuMemoryManagerHandle::new(Arc::clone(&self.memory_manager));
                self.luma_scratch.resize_destructive(out_luma_size as usize);
                let input = self.output_surface.data();
                let mut out_luma = GpuGuestMemoryScoped::<u8>::new_with_backup(
                    &memory,
                    output_address,
                    out_luma_size as usize,
                    GuestMemoryFlags::SAFE_WRITE,
                    &mut self.luma_scratch,
                );
                decode(unsafe { out_luma.as_mut_slice() }, input);
            }
            kind => panic!("Unexpected VIC output block kind {kind:?}"),
        }
    }
}

const AV_PIX_FMT_YUV420P: i32 = 0;
const AV_PIX_FMT_NV12: i32 = 23;

unsafe fn frame_plane(frame: &Frame, plane: usize, len: usize) -> Option<&[u8]> {
    let ptr = frame.get_plane_ptr(plane);
    if ptr.is_null() {
        return None;
    }
    Some(std::slice::from_raw_parts(ptr, len))
}

fn swizzle_surface(output: &mut [u8], out_stride: u32, input: &[u8], in_stride: u32, height: u32) {
    let x_mask = 0xFFFFFFD2u32;
    let y_mask = 0x2Cu32;
    let mut offs_x = 0u32;
    let mut offs_y = 0u32;

    for y in (0..height).step_by(2) {
        let dst_line = offs_y as usize * 16;
        let src_line = y as usize * (in_stride as usize / 16) * 16;
        let mut offs_line = offs_x;
        for x in (0..in_stride as usize).step_by(16) {
            let dst0 = dst_line + offs_line as usize * 16;
            let src0 = src_line + x;
            if dst0 + 16 <= output.len() && src0 + 16 <= input.len() {
                output[dst0..dst0 + 16].copy_from_slice(&input[src0..src0 + 16]);
            }
            let dst1 = dst0 + 16;
            let src1 = src0 + in_stride as usize;
            if dst1 + 16 <= output.len() && src1 + 16 <= input.len() {
                output[dst1..dst1 + 16].copy_from_slice(&input[src1..src1 + 16]);
            }
            offs_line = offs_line.wrapping_sub(x_mask) & x_mask;
        }
        offs_y = offs_y.wrapping_sub(y_mask) & y_mask;
        if offs_y == 0 {
            offs_x += out_stride;
        }
    }
}

impl Drop for Vic {
    fn drop(&mut self) {
        info!("Destroying vic {}", self.id);
        self.frame_queue.close(self.id);
    }
}

impl ProcessMethodHook for Vic {
    fn process_method(&mut self, method: u32, arg: u32) {
        Vic::process_method(self, method, arg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;

    const TEST_CONFIG_GPU_ADDR: u64 = 0x1_0000;
    const TEST_OUTPUT_GPU_ADDR: u64 = 0x1_1000;
    const TEST_CHROMA_GPU_ADDR: u64 = 0x1_1800;
    const TEST_DEVICE_ADDR: u64 = 0x8000;

    fn vic_with_mapped_config(config: &ConfigStruct) -> (Vic, Vec<u8>) {
        let mut backing = vec![0x5a; 0x2000];
        let config_bytes = unsafe {
            std::slice::from_raw_parts(
                (config as *const ConfigStruct).cast::<u8>(),
                std::mem::size_of::<ConfigStruct>(),
            )
        };
        backing[..config_bytes.len()].copy_from_slice(config_bytes);

        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            TEST_DEVICE_ADDR,
            backing.as_ptr(),
            0x4000_0000,
            backing.len(),
            1,
            true,
        );
        let mut memory = MemoryManager::new_with_geometry_and_device_memory(
            0,
            device_memory,
            32,
            1u64 << 31,
            16,
            12,
        );
        memory.map(
            TEST_CONFIG_GPU_ADDR,
            TEST_DEVICE_ADDR,
            backing.len() as u64,
            0,
            false,
        );
        let memory = Arc::new(parking_lot::Mutex::new(memory));
        let queue = Arc::new(FrameQueue::new());
        let mut vic = Vic::new(4, 0, queue, memory);
        vic.process_method(
            VIC_REG_CONFIG_STRUCT_OFFSET as u32,
            (TEST_CONFIG_GPU_ADDR >> 8) as u32,
        );
        vic.process_method(
            VIC_REG_OUTPUT_SURFACE_LUMA as u32,
            (TEST_OUTPUT_GPU_ADDR >> 8) as u32,
        );
        vic.process_method(
            VIC_REG_OUTPUT_SURFACE_CHROMA_U as u32,
            (TEST_CHROMA_GPU_ADDR >> 8) as u32,
        );
        (vic, backing)
    }

    fn abgr_config() -> ConfigStruct {
        let mut config = ConfigStruct::default();
        config.output_surface_config.format_flags = VideoPixelFormat::A8B8G8R8 as u32;
        config
    }

    #[test]
    fn vic_bitfield_accessors_match_upstream_layout() {
        assert_eq!(std::mem::size_of::<Pixel>(), 0x8);
        assert_eq!(std::mem::offset_of!(ConfigStruct, pipe_config), 0x0);
        assert_eq!(std::mem::offset_of!(ConfigStruct, output_config), 0x10);
        assert_eq!(
            std::mem::offset_of!(ConfigStruct, output_surface_config),
            0x20
        );
        assert_eq!(std::mem::offset_of!(ConfigStruct, out_color_matrix), 0x30);
        assert_eq!(std::mem::offset_of!(ConfigStruct, clear_rects), 0x50);
        assert_eq!(std::mem::offset_of!(ConfigStruct, slot_structs), 0x90);
        assert_eq!(VIC_REG_EXECUTE * 4, 0x300);
        assert_eq!(VIC_REG_SURFACES * 4, 0x400);
        assert_eq!(VIC_REG_PICTURE_INDEX * 4, 0x700);
        assert_eq!(VIC_REG_CONTROL_PARAMS * 4, 0x704);
        assert_eq!(VIC_REG_CONFIG_STRUCT_OFFSET * 4, 0x708);
        assert_eq!(VIC_REG_FILTER_STRUCT_OFFSET * 4, 0x70C);
        assert_eq!(VIC_REG_PALETTE_OFFSET * 4, 0x710);
        assert_eq!(VIC_REG_HIST_OFFSET * 4, 0x714);
        assert_eq!(VIC_REG_CONTEXT_ID * 4, 0x718);
        assert_eq!(VIC_REG_FCE_UCODE_SIZE * 4, 0x71C);
        assert_eq!(VIC_REG_OUTPUT_SURFACE_LUMA * 4, 0x720);
        assert_eq!(VIC_REG_OUTPUT_SURFACE_CHROMA_U * 4, 0x724);
        assert_eq!(VIC_REG_OUTPUT_SURFACE_CHROMA_V * 4, 0x728);
        assert_eq!(VIC_REG_FCE_UCODE_OFFSET * 4, 0x72C);
        assert_eq!(VIC_REG_SLOT_CONTEXT_IDS * 4, 0x740);
        assert_eq!(VIC_REG_COMP_TAG_BUFFER_OFFSETS * 4, 0x760);
        assert_eq!(VIC_REG_HISTORY_BUFFER_OFFSET * 4, 0x780);
        assert_eq!(VIC_REG_PM_TRIGGER_END * 4, 0x1114);

        let output = OutputSurfaceConfig {
            format_flags: (VideoPixelFormat::Y8VuN420 as u32)
                | ((BlkKind::Generic16Bx2 as u32) << 11)
                | (3 << 15),
            surface_dims: 1279 | (719 << 14),
            luma_dims: 1279 | (719 << 14),
            chroma_dims: 639 | (359 << 14),
        };
        assert_eq!(output.out_pixel_format(), Some(VideoPixelFormat::Y8VuN420));
        assert_eq!(output.out_block_kind(), Some(BlkKind::Generic16Bx2));
        assert_eq!(output.out_block_height(), 3);
        assert_eq!(output.out_surface_width(), 1279);
        assert_eq!(output.out_surface_height(), 719);
        assert_eq!(output.out_chroma_width(), 639);
        assert_eq!(output.out_chroma_height(), 359);

        let slot = SlotConfig {
            data: [
                1 | ((DxvahdFrameFormat::TopField as u64) << 16),
                (DxvahdDeinterlaceModePrivate::BobField as u64) << 40,
                16 | (1000 << 10) | (0x3ff << 32),
                0,
                10 | (110 << 32),
                20 | (220 << 32),
                30 | (130 << 16) | (40 << 32) | (240 << 48),
            ],
            reserved22: 0,
            reserved23: 0,
        };
        assert!(slot.slot_enable());
        assert_eq!(slot.frame_format(), Some(DxvahdFrameFormat::TopField));
        assert_eq!(
            slot.deinterlace_mode(),
            Some(DxvahdDeinterlaceModePrivate::BobField)
        );
        assert_eq!(slot.soft_clamp_low(), 16);
        assert_eq!(slot.soft_clamp_high(), 1000);
        assert_eq!(slot.planar_alpha(), 0x3ff);
        assert_eq!(slot.source_rect_left(), 10);
        assert_eq!(slot.source_rect_right(), 110);
        assert_eq!(slot.dest_rect_left(), 30);
        assert_eq!(slot.dest_rect_bottom(), 240);
    }

    #[test]
    fn process_method_uses_cdma_method_index_like_upstream() {
        let memory = Arc::new(parking_lot::Mutex::new(MemoryManager::new(0)));
        let queue = Arc::new(FrameQueue::new());
        let mut vic = Vic::new(4, 0, queue, memory);

        vic.process_method(VIC_REG_CONFIG_STRUCT_OFFSET as u32, 0x1234);

        assert_eq!(vic.regs.reg_array[VIC_REG_CONFIG_STRUCT_OFFSET], 0x1234);
        assert_eq!(vic.config_struct_address(), 0x1234 << 8);
        assert_eq!(VicMethod::from_u32(0x300), Some(VicMethod::Execute));
        assert_eq!(VicMethod::from_u32(VIC_REG_EXECUTE as u32), None);
    }

    #[test]
    fn execute_does_not_write_before_the_first_decoded_frame() {
        assert_ne!(
            *common::settings::values().nvdec_emulation.get_value(),
            common::settings_enums::NvdecEmulation::Off
        );
        let mut config = abgr_config();
        config.slot_structs[0].config.data[0] = 1;
        let (mut vic, backing) = vic_with_mapped_config(&config);

        vic.execute();

        assert!(!vic.has_decoded_frame);
        assert_eq!(&backing[0x1000..0x1010], &[0x5a; 0x10]);
    }

    #[test]
    fn execute_counts_an_available_frame_even_when_its_format_is_unsupported() {
        assert_ne!(
            *common::settings::values().nvdec_emulation.get_value(),
            common::settings_enums::NvdecEmulation::Off
        );
        let mut config = abgr_config();
        config.slot_structs[0].config.data[0] = 1;
        let (mut vic, backing) = vic_with_mapped_config(&config);
        vic.frame_queue.open(9);
        vic.frame_queue
            .push_present_order(9, 0, Arc::new(Frame::new()));

        vic.execute();

        assert!(vic.has_decoded_frame);
        assert_eq!(
            &backing[0x1000..0x1010],
            &[0, 0, 0, 0, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a]
        );
    }

    #[test]
    fn execute_reuses_the_last_surface_after_a_decoded_frame() {
        assert_ne!(
            *common::settings::values().nvdec_emulation.get_value(),
            common::settings_enums::NvdecEmulation::Off
        );
        let (mut vic, backing) = vic_with_mapped_config(&abgr_config());
        vic.has_decoded_frame = true;
        vic.output_surface.resize(1);
        vic.output_surface[0] = Pixel {
            r: 0x100,
            g: 0x200,
            b: 0x300,
            a: 0x3fc,
        };

        vic.execute();

        assert!(vic.has_decoded_frame);
        assert_eq!(
            &backing[0x1000..0x1010],
            &[
                0x40, 0x80, 0xc0, 0xff, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a,
                0x5a, 0x5a,
            ]
        );
    }

    #[test]
    fn yuv_block_linear_chroma_uses_edens_one_byte_swizzle_element() {
        let mut config = ConfigStruct::default();
        config.output_surface_config = OutputSurfaceConfig {
            format_flags: (VideoPixelFormat::Y8VuN420 as u32)
                | ((BlkKind::Generic16Bx2 as u32) << 11)
                | (2 << 15),
            surface_dims: 3 | (3 << 14),
            luma_dims: 3 | (3 << 14),
            chroma_dims: 1 | (1 << 14),
        };
        let (mut vic, backing) = vic_with_mapped_config(&config);
        vic.output_surface.resize(16);
        for (index, pixel) in vic.output_surface.iter_mut().enumerate() {
            pixel.r = (index as u16) << 2;
            pixel.g = ((0x40 + index) as u16) << 2;
            pixel.b = ((0x80 + index) as u16) << 2;
            pixel.a = 0x3fc;
        }

        vic.write_y8_v8u8_n420(config.output_surface_config);

        let chroma_width = 2;
        let chroma_height = 2;
        let block_height = 2;
        let expected_size = crate::textures::decoders::calculate_size(
            true,
            2,
            chroma_width,
            chroma_height,
            1,
            block_height,
            0,
        );
        let mut expected = vec![0x5a; expected_size];
        crate::textures::decoders::swizzle_texture(
            &mut expected,
            vic.chroma_scratch.data(),
            1,
            chroma_width,
            chroma_height,
            1,
            block_height,
            0,
            1,
        );
        let chroma_offset = (TEST_CHROMA_GPU_ADDR - TEST_CONFIG_GPU_ADDR) as usize;
        assert_eq!(
            &backing[chroma_offset..chroma_offset + expected_size],
            expected
        );
    }
}

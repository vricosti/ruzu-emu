// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Maxwell 3D engine — structured state tracking, clear operations, and draw
//! call recording.
//!
//! This is the main 3D rendering engine (NV class B197). It handles render
//! target configuration, clear operations, and draw call state tracking.
//! Register writes are stored in a flat array and side-effect methods (clear,
//! draw begin/end) are triggered on specific register writes.

use std::sync::Arc;

use parking_lot::Mutex;

use super::const_buffer_info::ConstBufferInfo;
use super::engine_interface::{EngineInterface, EngineInterfaceState};
use super::engine_upload;
use super::{PendingWrite, ENGINE_REG_COUNT};
use crate::dirty_flags;
use crate::engines::draw_manager as dm;
use crate::memory_manager::MemoryManager;
use crate::query_cache::query_cache::{RenderConditionState, RenderConditionStateSource};
use crate::query_cache::types::ComparisonMode;
use crate::query_cache::types::{QueryPropertiesFlags, QueryType};
use crate::r#macro::MacroEngine;
use crate::rasterizer_interface::{RasterizerHandle, RasterizerInterface};
use crate::textures::texture::{TicEntry, TscEntry};
use crate::transform_feedback::{StreamOutLayout, TransformFeedbackLayout, TransformFeedbackState};
use shader_recompiler::shader_info::ReplaceConstant;

#[derive(Clone, Copy)]
struct Maxwell3DPtr(*mut Maxwell3D);

unsafe impl Send for Maxwell3DPtr {}

// ── Register offset constants (method addresses) ────────────────────────────

/// Number of user clip distances exposed by Maxwell registers.
pub const NUM_CLIP_DISTANCES: u32 = 8;

/// Render target array base. 8 targets, 0x10 words (0x40 bytes) each.
/// Convert upstream byte offset (from ASSERT_REG_POSITION) to word index.
/// Matches upstream `MAXWELL3D_REG_INDEX(field) = offsetof(Regs, field) / sizeof(u32)`.
macro_rules! reg_index {
    ($byte_offset:expr) => {
        $byte_offset / 4
    };
    ($base:expr, +$field:expr) => {
        $base / 4 + $field
    };
}
pub(crate) const RT_BASE: u32 = reg_index!(0x0800);
/// Words per render target.
pub(crate) const RT_STRIDE: u32 = 0x10;

// Offsets within each render target (relative to RT_BASE + i*RT_STRIDE):
pub(crate) const RT_OFF_ADDRESS_HIGH: u32 = 0x00;
pub(crate) const RT_OFF_ADDRESS_LOW: u32 = 0x01;
pub(crate) const RT_OFF_WIDTH: u32 = 0x02;
pub(crate) const RT_OFF_HEIGHT: u32 = 0x03;
pub(crate) const RT_OFF_FORMAT: u32 = 0x04;
pub(crate) const RT_OFF_TILE_MODE: u32 = 0x05;
pub(crate) const RT_OFF_DEPTH: u32 = 0x06;
pub(crate) const RT_OFF_ARRAY_PITCH: u32 = 0x07;
pub(crate) const RT_OFF_BASE_LAYER: u32 = 0x08;

/// Clear color RGBA: 4 consecutive f32-as-u32 registers.
pub(crate) const CLEAR_COLOR_BASE: u32 = reg_index!(0x0D80);
/// Clear depth value (f32 as u32 bits).
#[allow(dead_code)]
const CLEAR_DEPTH: u32 = reg_index!(0x0D90);
/// Clear stencil value.
#[allow(dead_code)]
const CLEAR_STENCIL: u32 = reg_index!(0x0DA0);
/// Clear control register.
pub(crate) const CLEAR_CONTROL: u32 = reg_index!(0x10F8);
/// Clear surface trigger register.
pub(crate) const CLEAR_SURFACE: u32 = reg_index!(0x19D0);
pub(crate) const RASTER_BOUNDING_BOX: u32 = reg_index!(0x02EC);
pub(crate) const RASTERIZE_ENABLE: u32 = reg_index!(0x037C);

// ── Viewport registers ──────────────────────────────────────────────────────

/// Viewport transform base. 16 viewports, 8 words each.
/// Words: scale_x, scale_y, scale_z, translate_x, translate_y, translate_z, swizzle, snap.
pub(crate) const VP_TRANSFORM_BASE: u32 = reg_index!(0x0A00);
pub(crate) const VP_TRANSFORM_STRIDE: u32 = 8;
pub(crate) const VIEWPORT_BASE: u32 = reg_index!(0x0C00);
pub(crate) const VIEWPORT_STRIDE: u32 = 4;

// ── Scissor registers ───────────────────────────────────────────────────────

/// Scissor base. 16 scissors, 4 words each.
/// Words: enable, min_x|max_x(packed), min_y|max_y(packed), pad.
pub(crate) const SCISSOR_BASE: u32 = reg_index!(0x0E00);
pub(crate) const SCISSOR_STRIDE: u32 = 4;

// ── Vertex buffer registers ─────────────────────────────────────────────────

/// Vertex buffer first vertex.
pub(crate) const VB_FIRST: u32 = reg_index!(0x0D74); // upstream byte 0x0D74
/// Vertex buffer vertex count.
pub(crate) const VB_COUNT: u32 = reg_index!(0x0D78); // upstream byte 0x0D78

/// Vertex stream array base. 32 streams, 4 words each.
/// Words: stride|enable, addr_high, addr_low, frequency.
pub(crate) const VERTEX_STREAM_BASE: u32 = reg_index!(0x1C00);
pub(crate) const VERTEX_STREAM_STRIDE: u32 = 4;
/// Vertex stream instancing enable array. 32 words.
pub(crate) const VERTEX_STREAM_INSTANCE_BASE: u32 = reg_index!(0x1880);

/// Vertex stream limit array base. 32 streams, 2 words each.
#[allow(dead_code)]
pub(crate) const VERTEX_STREAM_LIMIT_BASE: u32 = reg_index!(0x1F00);

// ── Index buffer registers ──────────────────────────────────────────────────

/// Index buffer base (7 words).
/// Words: addr_high, addr_low, limit_high, limit_low, format, first, count.
pub(crate) const IB_BASE: u32 = reg_index!(0x17C8);
const IB_OFF_ADDR_HIGH: u32 = 0;
const IB_OFF_ADDR_LOW: u32 = 1;
#[allow(dead_code)]
const IB_OFF_LIMIT_HIGH: u32 = 2;
#[allow(dead_code)]
const IB_OFF_LIMIT_LOW: u32 = 3;
const IB_OFF_FORMAT: u32 = 4;
pub(crate) const IB_OFF_FIRST: u32 = 5;
pub(crate) const IB_OFF_COUNT: u32 = 6;

// ── Draw registers ──────────────────────────────────────────────────────────

/// Draw end trigger (previously DRAW_REG).
pub(crate) const DRAW_END: u32 = reg_index!(0x1614); // upstream byte 0x1614
/// Draw begin: sets topology and instance mode.
pub(crate) const DRAW_BEGIN: u32 = reg_index!(0x1618); // upstream byte 0x1618

/// Signed base vertex offset for indexed draws (i32).
pub(crate) const GLOBAL_BASE_VERTEX_INDEX: u32 = reg_index!(0x1434);
/// Base instance offset for instanced draws.
pub(crate) const GLOBAL_BASE_INSTANCE_INDEX: u32 = reg_index!(0x1438);
/// Base vertex value consumed by HLE indexed draw macros.
pub(crate) const VERTEX_ID_BASE: u32 = reg_index!(0x1118);
/// Conservative raster enable consumed by HLE raster bounding-box macro.
pub(crate) const CONSERVATIVE_RASTER_ENABLE: u32 = reg_index!(0x1148);
/// Automatic draw byte count consumed by HLE byte-count draw macros.
pub(crate) const DRAW_AUTO_BYTE_COUNT: u32 = reg_index!(0x123C);
/// Automatic draw stride consumed by HLE byte-count draw macros.
pub(crate) const DRAW_AUTO_STRIDE: u32 = reg_index!(0x1318);
/// Each write pushes 4 LE bytes of inline index data.
pub(crate) const DRAW_INLINE_INDEX: u32 = reg_index!(0x15E8);

// ── Report semaphore registers ────────────────────────────────────────────────

/// Report semaphore block: 4 words (addr_high, addr_low, payload, query).
/// Writing to REPORT_SEMAPHORE_BASE + 3 triggers the operation.
const REPORT_SEMAPHORE_BASE: u32 = reg_index!(0x1B00);
/// Trigger register for report semaphore (writing here fires the operation).
const REPORT_SEMAPHORE_TRIGGER: u32 = REPORT_SEMAPHORE_BASE + 3;

// ── Depth/Stencil registers ─────────────────────────────────────────────────

pub(crate) const DEPTH_MODE: u32 = reg_index!(0x0D7C);
pub(crate) const ALPHA_TEST_ENABLED: u32 = reg_index!(0x12EC);
pub(crate) const ALPHA_TEST_REF: u32 = reg_index!(0x1310);
pub(crate) const ALPHA_TEST_FUNC: u32 = reg_index!(0x1314);
pub(crate) const DEPTH_TEST_ENABLE: u32 = reg_index!(0x12CC);
pub(crate) const DEPTH_WRITE_ENABLE: u32 = reg_index!(0x12E8);
pub(crate) const DEPTH_TEST_FUNC: u32 = reg_index!(0x130C);
pub(crate) const POINT_SIZE: u32 = reg_index!(0x1518);

pub(crate) const STENCIL_ENABLE: u32 = reg_index!(0x1380);
pub(crate) const STENCIL_FRONT_OP_BASE: u32 = reg_index!(0x1384);
pub(crate) const STENCIL_FRONT_REF: u32 = reg_index!(0x1394);
pub(crate) const STENCIL_FRONT_FUNC_MASK: u32 = reg_index!(0x1398);
pub(crate) const STENCIL_FRONT_MASK: u32 = reg_index!(0x139C);

pub(crate) const STENCIL_TWO_SIDE_ENABLE: u32 = reg_index!(0x1594);
pub(crate) const STENCIL_BACK_OP_BASE: u32 = reg_index!(0x1598);
pub(crate) const STENCIL_BACK_REF: u32 = reg_index!(0x0F54);
pub(crate) const STENCIL_BACK_MASK: u32 = reg_index!(0x0F58);
pub(crate) const STENCIL_BACK_FUNC_MASK: u32 = reg_index!(0x0F5C);

// ── Blend registers ─────────────────────────────────────────────────────────

/// 4 consecutive f32 registers: R, G, B, A blend constant color.
pub(crate) const BLEND_COLOR_BASE: u32 = reg_index!(0x131C);

/// Global blend struct base.
/// +0 separate_alpha, +1 color_op, +2 color_src, +3 color_dst,
/// +4 alpha_op, +5 alpha_src, +6 (color_key), +7 alpha_dst,
/// +8 single_rop_ctrl, +9..+16 enable[0..7]
pub(crate) const BLEND_BASE: u32 = reg_index!(0x133C);

/// Whether per-target blend overrides are active.
pub(crate) const BLEND_PER_TARGET_ENABLED: u32 = reg_index!(0x12E4);

/// Per-target blend base. 8 entries, stride 8.
/// +0 sep_alpha, +1 color_op, +2 color_src, +3 color_dst,
/// +4 alpha_op, +5 alpha_src, +6 alpha_dst
pub(crate) const BLEND_PER_TARGET_BASE: u32 = reg_index!(0x1E00);
pub(crate) const BLEND_PER_TARGET_STRIDE: u32 = 8;

// ── Rasterizer registers ────────────────────────────────────────────────────

pub(crate) const POLYGON_MODE_FRONT: u32 = reg_index!(0x0DAC);
pub(crate) const POLYGON_MODE_BACK: u32 = reg_index!(0x0DB0);
pub(crate) const PATCH_VERTICES: u32 = reg_index!(0x0DCC);
pub(crate) const POLYGON_OFFSET_POINT_ENABLE: u32 = reg_index!(0x0DC0);
pub(crate) const POLYGON_OFFSET_LINE_ENABLE: u32 = reg_index!(0x0DC4);
pub(crate) const POLYGON_OFFSET_FILL_ENABLE: u32 = reg_index!(0x0DC8);
pub(crate) const FILL_VIA_TRIANGLE_MODE: u32 = reg_index!(0x113C);
pub(crate) const PRIMITIVE_RESTART_BASE: u32 = reg_index!(0x1644);
pub(crate) const PRIMITIVE_RESTART_WORDS: u32 = 2;
pub(crate) const ANTI_ALIAS_ALPHA_CONTROL: u32 = reg_index!(0x153C);
pub(crate) const FRAG_COLOR_CLAMP: u32 = reg_index!(0x13A8);
pub(crate) const POINT_SIZE_ATTRIBUTE: u32 = reg_index!(0x1910);
pub(crate) const POINT_SPRITE_ENABLE: u32 = reg_index!(0x1520);
pub(crate) const LINE_WIDTH_SMOOTH: u32 = reg_index!(0x13B0);
pub(crate) const LINE_WIDTH_ALIASED: u32 = reg_index!(0x13B4);
pub(crate) const LINE_ANTI_ALIAS_ENABLE: u32 = reg_index!(0x1570);
pub(crate) const LINE_STIPPLE_ENABLE: u32 = reg_index!(0x166C);
pub(crate) const LINE_STIPPLE_PARAMS: u32 = reg_index!(0x1680);
pub(crate) const SLOPE_SCALE_DEPTH_BIAS: u32 = reg_index!(0x156C);
pub(crate) const DEPTH_BIAS: u32 = reg_index!(0x15BC);
pub(crate) const DEPTH_BIAS_CLAMP: u32 = reg_index!(0x187C);
pub(crate) const CULL_TEST_ENABLE: u32 = reg_index!(0x1918);
pub(crate) const FRONT_FACE: u32 = reg_index!(0x191C);
pub(crate) const PROVOKING_VERTEX: u32 = reg_index!(0x1684);
pub(crate) const DEPTH_BOUNDS_ENABLE: u32 = reg_index!(0x19BC);
pub(crate) const DEPTH_BOUNDS_BASE: u32 = reg_index!(0x0F9C);
pub(crate) const CULL_FACE: u32 = reg_index!(0x1920);
pub(crate) const FRAMEBUFFER_SRGB: u32 = reg_index!(0x15B8);
pub(crate) const VIEWPORT_SCALE_OFFSET_ENABLED: u32 = reg_index!(0x192C);
pub(crate) const VIEWPORT_CLIP_CONTROL: u32 = reg_index!(0x193C);
pub(crate) const USER_CLIP_ENABLE: u32 = reg_index!(0x1510);
pub(crate) const LOGIC_OP: u32 = reg_index!(0x19C4);
pub(crate) const LOGIC_OP_WORDS: u32 = 2;

// ── Shader program registers ────────────────────────────────────────────────

/// Program region base: addr_high at +0, addr_low at +1.
const PROGRAM_REGION_BASE: u32 = reg_index!(0x1608);
const MANDATED_EARLY_Z: u32 = reg_index!(0x0210);
const TESSELLATION_PARAMS: u32 = reg_index!(0x0320);
pub(crate) const TRANSFORM_FEEDBACK_CONTROLS_BASE: u32 = reg_index!(0x0700);
const TRANSFORM_FEEDBACK_CONTROL_STRIDE: u32 = 0x10 / 4;
pub(crate) const TRANSFORM_FEEDBACK_BUFFERS_BASE: u32 = reg_index!(0x0380);
pub(crate) const TRANSFORM_FEEDBACK_BUFFER_STRIDE: u32 = 0x20 / 4;
pub(crate) const TRANSFORM_FEEDBACK_BUFFER_START_OFFSET: u32 = 0x10 / 4;
pub(crate) const TRANSFORM_FEEDBACK_ENABLED: u32 = reg_index!(0x0744);
const STREAM_OUT_LAYOUT_BASE: u32 = reg_index!(0x2800);

// ── Vertex attribute registers ────────────────────────────────────────────

/// Vertex attribute array base. 32 entries, 1 word each.
/// Per entry: bits[4:0]=buffer, bit[6]=constant, bits[20:7]=offset,
///            bits[26:21]=size, bits[29:27]=type, bit[31]=bgra.
pub(crate) const VERTEX_ATTRIB_BASE: u32 = reg_index!(0x1160);
pub(crate) const NUM_VERTEX_ATTRIBS: u32 = 32;
#[cfg(test)]
pub(crate) const NUM_VERTEX_ARRAYS: u32 = 32;

// ── Shader pipeline registers ─────────────────────────────────────────────

/// Shader pipeline base. 6 stages, 0x10 words each.
/// Per stage: +0 packed(enable|type), +1 offset, +3 register_count, +4 binding_group.
pub(crate) const PIPELINE_BASE: u32 = reg_index!(0x2000);
pub(crate) const PIPELINE_STRIDE: u32 = 0x10;
pub(crate) const NUM_SHADER_PROGRAMS: usize = 6;

/// Shader program region: GPU virtual base of the shared shader code region.
/// Two consecutive u32 registers — high then low — at byte offset 0x1608.
/// Upstream: `Maxwell3D::Regs::ProgramRegion` (`address_high`, `address_low`).
const PROGRAM_REGION_HIGH: u32 = reg_index!(0x1608);
const PROGRAM_REGION_LOW: u32 = reg_index!(0x160C);
const BINDLESS_TEXTURE_CONST_BUFFER_SLOT: u32 = reg_index!(0x2608);

// ── Color write mask registers ────────────────────────────────────────────

/// If nonzero, all RTs share color_mask[0].
pub(crate) const COLOR_MASK_COMMON: u32 = reg_index!(0x0F90);
const COLOR_TARGET_MRT_ENABLE: u32 = reg_index!(0x0FAC);
/// Per-RT color write mask array. 8 entries, 1 word each.
/// Per RT: R=bit[0], G=bit[4], B=bit[8], A=bit[12].
pub(crate) const COLOR_MASK_BASE: u32 = reg_index!(0x1A00);

// ── Render target control register ────────────────────────────────────────

/// RT control: count in bits[3:0], target map in bits[6:4],[9:7],... (3 bits each).
pub(crate) const RT_CONTROL: u32 = reg_index!(0x121C);

// ── Constant buffer registers ───────────────────────────────────────────────

/// CB config: +0 size, +1 addr_high, +2 addr_low, +3 offset.
pub(crate) const CB_CONFIG_BASE: u32 = reg_index!(0x2380);

/// CB data: 16 words of inline push at `const_buffer.buffer`.
/// Upstream `Regs::ConstantBuffer` has 4 header words
/// `(size, address_high, address_low, offset)` before `buffer[16]`.
const CB_DATA_BASE: u32 = reg_index!(0x2390);
const CB_DATA_END: u32 = reg_index!(0x23D0); // exclusive

/// CB bind base. 5 stages, stride 8, trigger at +4.
pub(crate) const CB_BIND_BASE: u32 = reg_index!(0x2400);
pub(crate) const CB_BIND_STRIDE: u32 = 8;
/// CB bind trigger registers (one per shader stage).
const CB_BIND_TRIGGER_0: u32 = reg_index!(0x2410);
const CB_BIND_TRIGGER_1: u32 = reg_index!(0x2430);
const CB_BIND_TRIGGER_2: u32 = reg_index!(0x2450);
const CB_BIND_TRIGGER_3: u32 = reg_index!(0x2470);
const CB_BIND_TRIGGER_4: u32 = reg_index!(0x2490);

/// Number of shader stages (vertex, tess ctrl, tess eval, geometry, fragment).
pub(crate) const NUM_SHADER_STAGES: usize = 5;
/// Maximum constant buffer slots per stage.
pub(crate) const MAX_CB_SLOTS: usize = 18;

// ── Texture/Sampler pool registers ──────────────────────────────────────────

/// Texture sampler pool base: +0 addr_high, +1 addr_low, +2 limit.
pub(crate) const TEX_SAMPLER_POOL_BASE: u32 = reg_index!(0x155C);

/// Texture header pool base: +0 addr_high, +1 addr_low, +2 limit.
pub(crate) const TEX_HEADER_POOL_BASE: u32 = reg_index!(0x1574);

/// Sampler binding mode register.
/// 0 = Independently (tic_id and tsc_id are separate in texture handle)
/// 1 = ViaHeaderBinding (tic_id == tsc_id, linked)
const SAMPLER_BINDING: u32 = reg_index!(0x1234);

// ── MME (Macro Method Executor) registers ──────────────────────────────────

/// Pointer into macro code upload buffer (auto-increments on instruction write).
/// Upstream byte offset 0x0114, word index 0x45.
const LOAD_MME_INSTRUCTION_PTR: u32 = reg_index!(0x0114);
/// Code word to upload at the current pointer.
/// Upstream byte offset 0x0118, word index 0x46.
const LOAD_MME_INSTRUCTION: u32 = reg_index!(0x0118);
/// Pointer into 128-slot position table (auto-increments on bind).
/// Upstream byte offset 0x011C, word index 0x47.
const LOAD_MME_START_ADDR_PTR: u32 = reg_index!(0x011C);
/// Start offset for the current macro slot.
/// Upstream byte offset 0x0120, word index 0x48.
const LOAD_MME_START_ADDR: u32 = reg_index!(0x0120);
/// First macro method register. Methods 0xE00..0xFFF invoke macros.
/// Now uses word indices matching upstream (ENGINE_REG_COUNT = 0xE00).
const MACRO_METHODS_START: u32 = reg_index!(0x3800);
/// Exclusive end of macro method range (0x1000 * 4).

// ── Additional register offsets (upstream ASSERT_REG_POSITION values) ──────

/// Wait-for-idle register. Writing triggers a rasterizer idle wait.
const WAIT_FOR_IDLE: u32 = reg_index!(0x0110);
/// Shadow RAM control register.
const SHADOW_RAM_CONTROL: u32 = reg_index!(0x0124);
/// Launch DMA register (triggers inline upload execution).
pub(crate) const LAUNCH_DMA: u32 = reg_index!(0x01B0);
/// Inline data register (data words for DMA upload).
pub(crate) const INLINE_DATA: u32 = reg_index!(0x01B4);
pub(crate) const UPLOAD_REGS_BASE: usize = reg_index!(0x0180) as usize;
/// Sync info register (triggers sync point signaling).
const SYNC_INFO: u32 = reg_index!(0x02C8);
/// Fragment barrier register.
const FRAGMENT_BARRIER: u32 = reg_index!(0x0DE0);
/// `regs.iterated_blend` (`ASSERT_REG_POSITION(iterated_blend, 0x0DD0)`).
const ITERATED_BLEND: u32 = reg_index!(0x0DD0);
/// Draw texture trigger (writing `regs.draw_texture.src_y0` triggers the draw).
pub(crate) const DRAW_TEXTURE_SRC_Y0: u32 = reg_index!(0x10AC);
/// `regs.surface_clip`.
pub(crate) const SURFACE_CLIP_BASE: u32 = reg_index!(0x0FF4);
const SURFACE_CLIP_HEIGHT_OFFSET: usize = 1;
/// `regs.draw_texture`.
const DRAW_TEXTURE_BASE: u32 = reg_index!(0x1080);
const DRAW_TEXTURE_DST_X0_OFFSET: usize = 0;
const DRAW_TEXTURE_DST_Y0_OFFSET: usize = 1;
const DRAW_TEXTURE_DST_WIDTH_OFFSET: usize = 2;
const DRAW_TEXTURE_DST_HEIGHT_OFFSET: usize = 3;
const DRAW_TEXTURE_DX_DU_LOW_OFFSET: usize = 4;
const DRAW_TEXTURE_DX_DU_HIGH_OFFSET: usize = 5;
const DRAW_TEXTURE_DY_DV_LOW_OFFSET: usize = 6;
const DRAW_TEXTURE_DY_DV_HIGH_OFFSET: usize = 7;
const DRAW_TEXTURE_SRC_SAMPLER_OFFSET: usize = 8;
const DRAW_TEXTURE_SRC_TEXTURE_OFFSET: usize = 9;
const DRAW_TEXTURE_SRC_X0_OFFSET: usize = 10;
const DRAW_TEXTURE_SRC_Y0_OFFSET: usize = 11;
/// `regs.window_origin`.
pub(crate) const WINDOW_ORIGIN: u32 = reg_index!(0x13AC);
/// Vertex array instance first (triggers instanced array draw).
pub(crate) const VERTEX_ARRAY_INSTANCE_FIRST: u32 = reg_index!(0x1214);
/// Vertex array instance subsequent (triggers subsequent instance draw).
pub(crate) const VERTEX_ARRAY_INSTANCE_SUBSEQUENT: u32 = reg_index!(0x1218);
/// Inline index 4x8 (index0 triggers 4-byte inline index push).
pub(crate) const INLINE_INDEX_4X8_INDEX0: u32 = reg_index!(0x1300);
/// Invalidate texture data cache register.
const INVALIDATE_TEXTURE_DATA_CACHE: u32 = reg_index!(0x0F74);
/// Tiled cache barrier register.
const TILED_CACHE_BARRIER: u32 = reg_index!(0x0F7C);
/// Clear report value register (triggers counter reset).
const CLEAR_REPORT_VALUE: u32 = reg_index!(0x1530);
/// Upstream `regs.zeta_enable`.
pub(crate) const ZETA_ENABLE: u32 = reg_index!(0x1538);
/// Upstream `regs.anti_alias_samples_mode`.
pub(crate) const ANTI_ALIAS_SAMPLES_MODE: u32 = reg_index!(0x15D0);
const ZPASS_PIXEL_COUNT_ENABLE: u32 = reg_index!(0x1514);
pub(crate) const ZETA_BASE: u32 = reg_index!(0x0FE0);
pub(crate) const ZETA_SIZE_BASE: u32 = reg_index!(0x1228);
/// Render enable block base: +0 addr_high, +1 addr_low, +2 mode.
const RENDER_ENABLE_BASE: u32 = reg_index!(0x1550);
/// Render enable mode register (triggers query condition evaluation).
const RENDER_ENABLE_MODE: u32 = RENDER_ENABLE_BASE + 2;
/// Render enable override register.
const RENDER_ENABLE_OVERRIDE: u32 = reg_index!(0x1944);
const PRIMITIVE_TOPOLOGY_CONTROL: u32 = reg_index!(0x1948);
/// Inline index 2x16 even (triggers 2-short inline index push).
pub(crate) const INLINE_INDEX_2X16_EVEN: u32 = reg_index!(0x15EC);
/// Topology override register.
pub(crate) const TOPOLOGY_OVERRIDE: u32 = reg_index!(0x1970);
/// Index buffer 32-bit first register.
pub(crate) const INDEX_BUFFER32_FIRST: u32 = reg_index!(0x17E4);
/// Index buffer 16-bit first register.
pub(crate) const INDEX_BUFFER16_FIRST: u32 = reg_index!(0x17E8);
/// Index buffer 8-bit first register.
pub(crate) const INDEX_BUFFER8_FIRST: u32 = reg_index!(0x17EC);
/// Index buffer 32-bit subsequent register.
pub(crate) const INDEX_BUFFER32_SUBSEQUENT: u32 = reg_index!(0x17F0);
/// Index buffer 16-bit subsequent register.
pub(crate) const INDEX_BUFFER16_SUBSEQUENT: u32 = reg_index!(0x17F4);
/// Index buffer 8-bit subsequent register.
pub(crate) const INDEX_BUFFER8_SUBSEQUENT: u32 = reg_index!(0x17F8);
/// Report semaphore query trigger (writing here fires the semaphore query).
// Matches upstream MAXWELL3D_REG_INDEX(report_semaphore.query).
const REPORT_SEMAPHORE_QUERY: u32 = REPORT_SEMAPHORE_TRIGGER;
/// Falcon register array element used by `ProcessFirmwareCall4`.
/// Upstream owner is `MAXWELL3D_REG_INDEX(falcon[4])`, i.e. byte offset
/// `0x2300 + 4 * sizeof(u32) = 0x2310`.
const FALCON4: u32 = reg_index!(0x2310);
/// Shadow scratch memory base (0x3400). Used by firmware stubs.
pub(crate) const SHADOW_SCRATCH_BASE: u32 = reg_index!(0x3400);

/// Macro registers start offset (method index space, not byte offset).
/// Methods >= 0xE00 are macro triggers.
const MACRO_REGISTERS_START: u32 = reg_index!(0x3800);

// ── Common render target formats ────────────────────────────────────────────

#[allow(dead_code)]
const RT_FORMAT_A8R8G8B8_UNORM: u32 = reg_index!(0x00CC);
#[allow(dead_code)]
const RT_FORMAT_R16G16B16A16_FLOAT: u32 = reg_index!(0x00C8);
#[allow(dead_code)]
const RT_FORMAT_R32_FLOAT: u32 = reg_index!(0x00E4);
#[allow(dead_code)]
const RT_FORMAT_R16G16_FLOAT: u32 = reg_index!(0x00DC);
#[allow(dead_code)]
const RT_FORMAT_R32G32_FLOAT: u32 = reg_index!(0x00C8);
#[allow(dead_code)]
const RT_FORMAT_R16_FLOAT: u32 = reg_index!(0x00F0);
#[allow(dead_code)]
pub(crate) const RT_FORMAT_R8_UNORM: u32 = reg_index!(0x00F0);
#[allow(dead_code)]
const RT_FORMAT_R16G16_UNORM: u32 = reg_index!(0x00D8);
#[allow(dead_code)]
pub(crate) const RT_FORMAT_B5G6R5_UNORM: u32 = reg_index!(0x00E8);
#[allow(dead_code)]
const RT_FORMAT_A2B10G10R10_UNORM: u32 = reg_index!(0x00D0);
#[allow(dead_code)]
const RT_FORMAT_R11G11B10_FLOAT: u32 = reg_index!(0x00E0);

// ── Draw state types ────────────────────────────────────────────────────────

/// GPU primitive topology.
///
/// Port of upstream `Maxwell3D::Regs::PrimitiveTopology`
/// (`maxwell_3d.h:871`). The discriminant values match upstream exactly
/// (Points = 0x0, …, Patches = 0xE) — they are the raw register values
/// the GPU writes into the topology field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u32)]
pub enum PrimitiveTopology {
    #[default]
    Points = 0,
    Lines = 1,
    LineLoop = 2,
    LineStrip = 3,
    Triangles = 4,
    TriangleStrip = 5,
    TriangleFan = 6,
    Quads = 7,
    QuadStrip = 8,
    Polygon = 9,
    LinesAdjacency = 10,
    LineStripAdjacency = 11,
    TrianglesAdjacency = 12,
    TriangleStripAdjacency = 13,
    Patches = 14,
    // C++ can retain these `PrimitiveTopologyOverride` bit patterns through
    // its explicit enum-to-enum cast even though PrimitiveTopology does not
    // declare names for them. Rust needs explicit discriminants to preserve
    // the same values without constructing an invalid enum.
    LegacyPoints = 0x1001,
    LegacyIndexedLines = 0x1002,
    LegacyIndexedTriangles = 0x1003,
    LegacyLines = 0x100F,
    LegacyLineStrip = 0x1010,
    LegacyIndexedLineStrip = 0x1011,
    LegacyTriangles = 0x1012,
    LegacyTriangleStrip = 0x1013,
    LegacyIndexedTriangleStrip = 0x1014,
    LegacyTriangleFan = 0x1015,
    LegacyIndexedTriangleFan = 0x1016,
    LegacyTriangleFanImm = 0x1017,
    LegacyLinesImm = 0x1018,
    LegacyIndexedTriangles2 = 0x101A,
    LegacyIndexedLines2 = 0x101B,
}

/// Selects whether draws use the topology encoded by begin methods or the
/// separate topology-override register.
///
/// Corresponds to `Maxwell3D::Regs::PrimitiveTopologyControl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum PrimitiveTopologyControl {
    #[default]
    UseInBeginMethods = 0,
    UseSeparateState = 1,
}

impl PrimitiveTopologyControl {
    pub fn from_raw(raw: u32) -> Self {
        match raw {
            1 => Self::UseSeparateState,
            _ => Self::UseInBeginMethods,
        }
    }
}

/// Values accepted by the separate topology-override register.
///
/// Corresponds to `Maxwell3D::Regs::PrimitiveTopologyOverride`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum PrimitiveTopologyOverride {
    #[default]
    None = 0x0,
    Points = 0x1,
    Lines = 0x2,
    LineStrip = 0x3,
    Triangles = 0x4,
    TriangleStrip = 0x5,
    LinesAdjacency = 0xA,
    LineStripAdjacency = 0xB,
    TrianglesAdjacency = 0xC,
    TriangleStripAdjacency = 0xD,
    Patches = 0xE,
    LegacyPoints = 0x1001,
    LegacyIndexedLines = 0x1002,
    LegacyIndexedTriangles = 0x1003,
    LegacyLines = 0x100F,
    LegacyLineStrip = 0x1010,
    LegacyIndexedLineStrip = 0x1011,
    LegacyTriangles = 0x1012,
    LegacyTriangleStrip = 0x1013,
    LegacyIndexedTriangleStrip = 0x1014,
    LegacyTriangleFan = 0x1015,
    LegacyIndexedTriangleFan = 0x1016,
    LegacyTriangleFanImm = 0x1017,
    LegacyLinesImm = 0x1018,
    LegacyIndexedTriangles2 = 0x101A,
    LegacyIndexedLines2 = 0x101B,
}

impl PrimitiveTopologyOverride {
    pub fn from_raw(raw: u32) -> Self {
        match raw {
            0x0 => Self::None,
            0x1 => Self::Points,
            0x2 => Self::Lines,
            0x3 => Self::LineStrip,
            0x4 => Self::Triangles,
            0x5 => Self::TriangleStrip,
            0xA => Self::LinesAdjacency,
            0xB => Self::LineStripAdjacency,
            0xC => Self::TrianglesAdjacency,
            0xD => Self::TriangleStripAdjacency,
            0xE => Self::Patches,
            0x1001 => Self::LegacyPoints,
            0x1002 => Self::LegacyIndexedLines,
            0x1003 => Self::LegacyIndexedTriangles,
            0x100F => Self::LegacyLines,
            0x1010 => Self::LegacyLineStrip,
            0x1011 => Self::LegacyIndexedLineStrip,
            0x1012 => Self::LegacyTriangles,
            0x1013 => Self::LegacyTriangleStrip,
            0x1014 => Self::LegacyIndexedTriangleStrip,
            0x1015 => Self::LegacyTriangleFan,
            0x1016 => Self::LegacyIndexedTriangleFan,
            0x1017 => Self::LegacyTriangleFanImm,
            0x1018 => Self::LegacyLinesImm,
            0x101A => Self::LegacyIndexedTriangles2,
            0x101B => Self::LegacyIndexedLines2,
            _ => Self::None,
        }
    }
}

impl PrimitiveTopology {
    pub fn from_raw(value: u32) -> Self {
        match value & 0xFFFF {
            0 => Self::Points,
            1 => Self::Lines,
            2 => Self::LineLoop,
            3 => Self::LineStrip,
            4 => Self::Triangles,
            5 => Self::TriangleStrip,
            6 => Self::TriangleFan,
            7 => Self::Quads,
            8 => Self::QuadStrip,
            9 => Self::Polygon,
            10 => Self::LinesAdjacency,
            11 => Self::LineStripAdjacency,
            12 => Self::TrianglesAdjacency,
            13 => Self::TriangleStripAdjacency,
            14 => Self::Patches,
            0x1001 => Self::LegacyPoints,
            0x1002 => Self::LegacyIndexedLines,
            0x1003 => Self::LegacyIndexedTriangles,
            0x100F => Self::LegacyLines,
            0x1010 => Self::LegacyLineStrip,
            0x1011 => Self::LegacyIndexedLineStrip,
            0x1012 => Self::LegacyTriangles,
            0x1013 => Self::LegacyTriangleStrip,
            0x1014 => Self::LegacyIndexedTriangleStrip,
            0x1015 => Self::LegacyTriangleFan,
            0x1016 => Self::LegacyIndexedTriangleFan,
            0x1017 => Self::LegacyTriangleFanImm,
            0x1018 => Self::LegacyLinesImm,
            0x101A => Self::LegacyIndexedTriangles2,
            0x101B => Self::LegacyIndexedLines2,
            _ => {
                log::warn!(
                    "Maxwell3D: unknown topology {}, defaulting to Triangles",
                    value & 0xFFFF
                );
                Self::Triangles
            }
        }
    }

    pub fn is_hle_safe(self) -> bool {
        matches!(
            self,
            Self::Points
                | Self::Lines
                | Self::LineLoop
                | Self::LineStrip
                | Self::Triangles
                | Self::TriangleStrip
                | Self::TriangleFan
                | Self::LinesAdjacency
                | Self::LineStripAdjacency
                | Self::TrianglesAdjacency
                | Self::TriangleStripAdjacency
                | Self::Patches
        )
    }
}

/// Index buffer element format.
///
/// Port of upstream `Maxwell3D::Regs::IndexFormat` (`maxwell_3d.h:932`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u32)]
pub enum IndexFormat {
    #[default]
    UnsignedByte = 0,
    UnsignedShort = 1,
    UnsignedInt = 2,
}

impl IndexFormat {
    pub fn from_raw(value: u32) -> Self {
        match value {
            0 => Self::UnsignedByte,
            1 => Self::UnsignedShort,
            2 => Self::UnsignedInt,
            _ => {
                log::warn!(
                    "Maxwell3D: unknown index format {}, defaulting to UnsignedInt",
                    value
                );
                Self::UnsignedInt
            }
        }
    }

    pub fn size_bytes(&self) -> u32 {
        match self {
            Self::UnsignedByte => 1,
            Self::UnsignedShort => 2,
            Self::UnsignedInt => 4,
        }
    }
}

// ── Shadow RAM control ─────────────────────────────────────────────────────

/// Shadow RAM control mode — how register writes interact with shadow state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ShadowRamControl {
    /// Track writes into shadow state.
    Track = 0,
    /// Track with filter.
    TrackWithFilter = 1,
    /// Only write to the real hardware register.
    Passthrough = 2,
    /// Replay from shadow state instead of using written values.
    Replay = 3,
}

impl ShadowRamControl {
    pub fn from_raw(value: u32) -> Self {
        match value {
            0 => Self::Track,
            1 => Self::TrackWithFilter,
            2 => Self::Passthrough,
            3 => Self::Replay,
            _ => Self::Passthrough,
        }
    }
}

/// Clear report value types for counter reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ClearReport {
    ZPassPixelCount = 0x01,
    StreamingPrimitivesSucceeded = 0x03,
    PrimitivesGenerated = 0x12,
    VtgPrimitivesOut = 0x15,
}

// ── Draw mode types ─────────────────────────────────────────────────────────

/// Instance mode extracted from DRAW_BEGIN bits[27:26].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceId {
    First = 0,
    Subsequent = 1,
    Unchanged = 2,
}

impl InstanceId {
    pub fn from_raw(value: u32) -> Self {
        match (value >> 26) & 0x3 {
            0 => Self::First,
            1 => Self::Subsequent,
            _ => Self::Unchanged,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct VertexArrayParams {
    start: u32,
    count: u32,
    topology: PrimitiveTopology,
}

impl VertexArrayParams {
    fn from_raw(raw: u32) -> Self {
        Self {
            start: raw & 0xFFFF,
            count: (raw >> 16) & 0xFFF,
            topology: PrimitiveTopology::from_raw((raw >> 28) & 0x7),
        }
    }
}

/// Sampler binding mode — how texture handles encode TIC/TSC indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SamplerBinding {
    /// TIC and TSC indices are packed independently in the texture handle.
    /// Handle: bits[19:0] = tic_id, bits[31:20] = tsc_id.
    Independently = 0,
    /// TIC and TSC share the same index (linked binding).
    ViaHeaderBinding = 1,
}

/// Report semaphore operation type from query word bits[1:0].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportOperation {
    Release = 0,
    Acquire = 1,
    ReportOnly = 2,
    Trap = 3,
}

impl ReportOperation {
    pub fn from_raw(value: u32) -> Self {
        match value & 0x3 {
            0 => Self::Release,
            1 => Self::Acquire,
            2 => Self::ReportOnly,
            _ => Self::Trap,
        }
    }
}

fn stop_unimplemented_query_operation(
    operation: ReportOperation,
    query_word: u32,
    gpu_va: u64,
    payload: u32,
) -> ! {
    panic!(
        "Maxwell3D::ProcessQueryGet unimplemented query operation {:?} query=0x{:08X} gpu_va=0x{:X} payload=0x{:08X}",
        operation, query_word, gpu_va, payload,
    );
}

// ── Pipeline state enums ────────────────────────────────────────────────────

/// Depth/stencil comparison function. Supports both D3D (1-8) and GL (0x200-0x207) encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonOp {
    Never,
    Less,
    Equal,
    LessEqual,
    Greater,
    NotEqual,
    GreaterEqual,
    Always,
}

impl ComparisonOp {
    pub fn from_raw(value: u32) -> Self {
        match value {
            1 | 0x200 => Self::Never,
            2 | 0x201 => Self::Less,
            3 | 0x202 => Self::Equal,
            4 | 0x203 => Self::LessEqual,
            5 | 0x204 => Self::Greater,
            6 | 0x205 => Self::NotEqual,
            7 | 0x206 => Self::GreaterEqual,
            8 | 0x207 => Self::Always,
            _ => {
                log::trace!(
                    "Maxwell3D: unknown ComparisonOp 0x{:X}, defaulting to Always",
                    value
                );
                Self::Always
            }
        }
    }
}

/// Blend equation. Supports both D3D (1-5) and GL (0x8006-0x800B) encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendEquation {
    Add,
    Subtract,
    ReverseSubtract,
    Min,
    Max,
}

impl BlendEquation {
    pub fn from_raw(value: u32) -> Self {
        match value {
            1 | 0x8006 => Self::Add,
            2 | 0x800A => Self::Subtract,
            3 | 0x800B => Self::ReverseSubtract,
            4 | 0x8007 => Self::Min,
            5 | 0x8008 => Self::Max,
            _ => {
                log::warn!(
                    "Maxwell3D: unknown BlendEquation 0x{:X}, defaulting to Add",
                    value
                );
                Self::Add
            }
        }
    }
}

/// Blend factor. Supports both D3D (0x1-0x13) and GL (0x4000+) encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendFactor {
    Zero,
    One,
    SrcColor,
    OneMinusSrcColor,
    SrcAlpha,
    OneMinusSrcAlpha,
    DstAlpha,
    OneMinusDstAlpha,
    DstColor,
    OneMinusDstColor,
    SrcAlphaSaturate,
    Src1Color,
    OneMinusSrc1Color,
    Src1Alpha,
    OneMinusSrc1Alpha,
    ConstantColor,
    OneMinusConstantColor,
    ConstantAlpha,
    OneMinusConstantAlpha,
}

impl BlendFactor {
    pub fn from_raw(value: u32) -> Self {
        match value {
            0x01 | 0x4000 => Self::Zero,
            0x02 | 0x4001 => Self::One,
            0x03 | 0x4300 => Self::SrcColor,
            0x04 | 0x4301 => Self::OneMinusSrcColor,
            0x05 | 0x4302 => Self::SrcAlpha,
            0x06 | 0x4303 => Self::OneMinusSrcAlpha,
            0x07 | 0x4304 => Self::DstAlpha,
            0x08 | 0x4305 => Self::OneMinusDstAlpha,
            0x09 | 0x4306 => Self::DstColor,
            0x0A | 0x4307 => Self::OneMinusDstColor,
            0x0B | 0x4308 => Self::SrcAlphaSaturate,
            0x0C | 0xC003 => Self::ConstantAlpha,
            0x0D | 0xC004 => Self::OneMinusConstantAlpha,
            0x0E | 0xC001 => Self::ConstantColor,
            0x0F | 0xC002 => Self::OneMinusConstantColor,
            0x10 | 0xC900 => Self::Src1Color,
            0x11 | 0xC901 => Self::OneMinusSrc1Color,
            0x12 | 0xC902 => Self::Src1Alpha,
            0x13 | 0xC903 => Self::OneMinusSrc1Alpha,
            _ => {
                log::warn!(
                    "Maxwell3D: unknown BlendFactor 0x{:X}, defaulting to One",
                    value
                );
                Self::One
            }
        }
    }
}

/// Stencil operation. Supports both D3D (1-8) and GL (0x0-0x8508) encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StencilOp {
    Keep,
    Zero,
    Replace,
    IncrSat,
    DecrSat,
    Invert,
    Incr,
    Decr,
}

impl StencilOp {
    pub fn from_raw(value: u32) -> Self {
        match value {
            1 | 0x1E00 => Self::Keep,
            2 | 0x0000 => Self::Zero,
            3 | 0x1E01 => Self::Replace,
            4 | 0x1E02 => Self::IncrSat,
            5 | 0x1E03 => Self::DecrSat,
            6 | 0x150A => Self::Invert,
            7 | 0x8507 => Self::Incr,
            8 | 0x8508 => Self::Decr,
            _ => {
                log::warn!(
                    "Maxwell3D: unknown StencilOp 0x{:X}, defaulting to Keep",
                    value
                );
                Self::Keep
            }
        }
    }
}

/// Cull face mode (GL encoding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CullFace {
    Front,
    Back,
    FrontAndBack,
}

impl CullFace {
    pub fn from_raw(value: u32) -> Self {
        match value {
            0x0404 => Self::Front,
            0x0405 => Self::Back,
            0x0408 => Self::FrontAndBack,
            _ => {
                log::warn!(
                    "Maxwell3D: unknown CullFace 0x{:X}, defaulting to Back",
                    value
                );
                Self::Back
            }
        }
    }
}

/// Front face winding order (GL encoding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontFace {
    CW,
    CCW,
}

impl FrontFace {
    pub fn from_raw(value: u32) -> Self {
        match value {
            0x0900 => Self::CW,
            0x0901 => Self::CCW,
            _ => {
                log::warn!(
                    "Maxwell3D: unknown FrontFace 0x{:X}, defaulting to CCW",
                    value
                );
                Self::CCW
            }
        }
    }
}

/// Polygon rasterization mode (GL encoding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolygonMode {
    Point,
    Line,
    Fill,
}

impl PolygonMode {
    pub fn from_raw(value: u32) -> Self {
        match value {
            0x1B00 => Self::Point,
            0x1B01 => Self::Line,
            0x1B02 => Self::Fill,
            _ => {
                log::warn!(
                    "Maxwell3D: unknown PolygonMode 0x{:X}, defaulting to Fill",
                    value
                );
                Self::Fill
            }
        }
    }
}

/// Upstream `Maxwell3D::Regs::FillViaTriangleMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillViaTriangleMode {
    Disabled,
    FillAll,
    FillBoundingBox,
}

impl FillViaTriangleMode {
    pub fn from_raw(value: u32) -> Self {
        match value {
            0 => Self::Disabled,
            1 => Self::FillAll,
            2 => Self::FillBoundingBox,
            _ => {
                log::warn!(
                    "Maxwell3D: unknown FillViaTriangleMode 0x{:X}, defaulting to Disabled",
                    value
                );
                Self::Disabled
            }
        }
    }
}

impl Default for FillViaTriangleMode {
    fn default() -> Self {
        Self::Disabled
    }
}

/// Depth range mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthMode {
    MinusOneToOne,
    ZeroToOne,
}

impl DepthMode {
    pub fn from_raw(value: u32) -> Self {
        match value {
            0 => Self::MinusOneToOne,
            1 => Self::ZeroToOne,
            _ => {
                log::warn!(
                    "Maxwell3D: unknown DepthMode {}, defaulting to ZeroToOne",
                    value
                );
                Self::ZeroToOne
            }
        }
    }
}

// ── Vertex attribute enums ────────────────────────────────────────────────

/// Vertex attribute component size/layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexAttribSize {
    R32G32B32A32,
    R32G32B32,
    R16G16B16A16,
    R32G32,
    R16G16B16,
    R8G8B8A8,
    R16G16,
    R32,
    R8G8B8,
    R8G8,
    R16,
    R8,
    A2B10G10R10,
    B10G11R11,
    G8R8,
    X8B8G8R8,
    A8,
    Invalid,
}

impl VertexAttribSize {
    pub fn from_raw(value: u32) -> Self {
        match value {
            0x01 => Self::R32G32B32A32,
            0x02 => Self::R32G32B32,
            0x03 => Self::R16G16B16A16,
            0x04 => Self::R32G32,
            0x05 => Self::R16G16B16,
            0x0A => Self::R8G8B8A8,
            0x0F => Self::R16G16,
            0x12 => Self::R32,
            0x13 => Self::R8G8B8,
            0x18 => Self::R8G8,
            0x1B => Self::R16,
            0x1D => Self::R8,
            0x30 => Self::A2B10G10R10,
            0x31 => Self::B10G11R11,
            0x32 => Self::G8R8,
            0x33 => Self::X8B8G8R8,
            0x34 => Self::A8,
            _ => Self::Invalid,
        }
    }

    /// Inverse of `from_raw`: the Maxwell hardware encoding of this size.
    ///
    /// Used when packing `FixedPipelineState` attribute bits, which must hold
    /// the raw hardware values like upstream (`attribute.size.Assign(
    /// input.size.Value())` in fixed_pipeline_state.cpp) — not Rust enum
    /// ordinals.
    pub fn to_raw(self) -> u32 {
        match self {
            Self::R32G32B32A32 => 0x01,
            Self::R32G32B32 => 0x02,
            Self::R16G16B16A16 => 0x03,
            Self::R32G32 => 0x04,
            Self::R16G16B16 => 0x05,
            Self::R8G8B8A8 => 0x0A,
            Self::R16G16 => 0x0F,
            Self::R32 => 0x12,
            Self::R8G8B8 => 0x13,
            Self::R8G8 => 0x18,
            Self::R16 => 0x1B,
            Self::R8 => 0x1D,
            Self::A2B10G10R10 => 0x30,
            Self::B10G11R11 => 0x31,
            Self::G8R8 => 0x32,
            Self::X8B8G8R8 => 0x33,
            Self::A8 => 0x34,
            Self::Invalid => 0x00,
        }
    }

    /// Size in bytes of one vertex attribute element.
    pub fn size_bytes(&self) -> u32 {
        match self {
            Self::R32G32B32A32 => 16,
            Self::R32G32B32 => 12,
            Self::R16G16B16A16 => 8,
            Self::R32G32 => 8,
            Self::R16G16B16 => 6,
            Self::R8G8B8A8 => 4,
            Self::R16G16 => 4,
            Self::R32 => 4,
            Self::R8G8B8 => 3,
            Self::R8G8 => 2,
            Self::R16 => 2,
            Self::R8 => 1,
            Self::A2B10G10R10 => 4,
            Self::B10G11R11 => 4,
            Self::G8R8 => 2,
            Self::X8B8G8R8 => 4,
            Self::A8 => 1,
            Self::Invalid => 0,
        }
    }

    /// Number of components.
    pub fn component_count(&self) -> u32 {
        match self {
            Self::R32G32B32A32
            | Self::R16G16B16A16
            | Self::R8G8B8A8
            | Self::A2B10G10R10
            | Self::X8B8G8R8 => 4,
            Self::R32G32B32 | Self::R16G16B16 | Self::R8G8B8 | Self::B10G11R11 => 3,
            Self::R32G32 | Self::R16G16 | Self::R8G8 | Self::G8R8 => 2,
            Self::R32 | Self::R16 | Self::R8 | Self::A8 => 1,
            // Upstream's `ComponentCount()` asserts on an unknown size and then
            // returns 1. Returning 0 here would hand `glVertexAttribFormat` a
            // component count GL rejects outright.
            Self::Invalid => 1,
        }
    }
}

/// Vertex attribute numeric type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexAttribType {
    SNorm,
    UNorm,
    SInt,
    UInt,
    UScaled,
    SScaled,
    Float,
    Invalid,
}

impl VertexAttribType {
    pub fn from_raw(value: u32) -> Self {
        match value {
            1 => Self::SNorm,
            2 => Self::UNorm,
            3 => Self::SInt,
            4 => Self::UInt,
            5 => Self::UScaled,
            6 => Self::SScaled,
            7 => Self::Float,
            _ => Self::Invalid,
        }
    }

    /// Inverse of `from_raw`: the Maxwell hardware encoding of this type.
    ///
    /// See `VertexAttribSize::to_raw` — `FixedPipelineState` attribute bits
    /// must hold raw hardware values, not Rust enum ordinals.
    pub fn to_raw(self) -> u32 {
        match self {
            Self::SNorm => 1,
            Self::UNorm => 2,
            Self::SInt => 3,
            Self::UInt => 4,
            Self::UScaled => 5,
            Self::SScaled => 6,
            Self::Float => 7,
            Self::Invalid => 0,
        }
    }
}

// ── Shader stage enum ─────────────────────────────────────────────────────

/// Shader stage type in the pipeline program array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderStageType {
    VertexA,
    VertexB,
    TessInit,
    Tessellation,
    Geometry,
    Fragment,
    Invalid,
}

impl ShaderStageType {
    pub fn from_raw(value: u32) -> Self {
        match value {
            0 => Self::VertexA,
            1 => Self::VertexB,
            2 => Self::TessInit,
            3 => Self::Tessellation,
            4 => Self::Geometry,
            5 => Self::Fragment,
            _ => Self::Invalid,
        }
    }

    pub fn as_index(self) -> Option<u32> {
        match self {
            Self::VertexA => Some(0),
            Self::VertexB => Some(1),
            Self::TessInit => Some(2),
            Self::Tessellation => Some(3),
            Self::Geometry => Some(4),
            Self::Fragment => Some(5),
            Self::Invalid => None,
        }
    }
}

// ── Texture/Sampler descriptor enums ─────────────────────────────────────────

/// Texture image format (7-bit field from TIC word 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureFormat {
    R32G32B32A32,
    R32G32B32,
    R16G16B16A16,
    R32G32,
    R32B24G8,
    R16G16,
    R32,
    B5G6R5,
    A1R5G5B5,
    R8G8,
    R16,
    R8,
    A8B8G8R8,
    A2B10G10R10,
    R16G16B16X16,
    R32G32B32X32,
    B10G11R11,
    G8R24,
    R32G8X24,
    R8G8B8A8,
    Bc1Rgba,
    Bc2,
    Bc3,
    Bc4,
    Bc5,
    Bc7,
    Bc6HSf16,
    Bc6HUf16,
    Etc2Rgb,
    Etc2RgbA1,
    Etc2RgbA8,
    Eac,
    EacX2,
    Astc2d4x4,
    Astc2d5x4,
    Astc2d5x5,
    Astc2d6x5,
    Astc2d6x6,
    Astc2d8x5,
    Astc2d8x6,
    Astc2d8x8,
    Astc2d10x5,
    Astc2d10x6,
    Astc2d10x8,
    Astc2d10x10,
    Astc2d12x10,
    Astc2d12x12,
    Invalid,
}

impl TextureFormat {
    pub fn from_raw(value: u32) -> Self {
        match value & 0x7F {
            0x01 => Self::R32G32B32A32,
            0x02 => Self::R32G32B32,
            0x03 => Self::R16G16B16A16,
            0x04 => Self::R32G32,
            0x05 => Self::R32B24G8,
            0x08 => Self::R16G16,
            0x09 => Self::R32,
            0x0E => Self::B5G6R5,
            0x0F => Self::A1R5G5B5,
            0x10 => Self::R8G8,
            0x11 => Self::R16,
            0x12 => Self::R8,
            0x1D => Self::A8B8G8R8,
            0x1E => Self::A2B10G10R10,
            0x1F => Self::R16G16B16X16,
            0x20 => Self::R32G32B32X32,
            0x21 => Self::B10G11R11,
            0x22 => Self::G8R24,
            0x23 => Self::R32G8X24,
            0x24 => Self::R8G8B8A8,
            0x25 => Self::Bc1Rgba,
            0x26 => Self::Bc2,
            0x27 => Self::Bc3,
            0x28 => Self::Bc4,
            0x29 => Self::Bc5,
            0x2A => Self::Bc7,
            0x2B => Self::Bc6HSf16,
            0x2C => Self::Bc6HUf16,
            0x2D => Self::Etc2Rgb,
            0x2E => Self::Etc2RgbA1,
            0x2F => Self::Etc2RgbA8,
            0x30 => Self::Eac,
            0x31 => Self::EacX2,
            0x40 => Self::Astc2d4x4,
            0x41 => Self::Astc2d5x4,
            0x42 => Self::Astc2d5x5,
            0x43 => Self::Astc2d6x5,
            0x44 => Self::Astc2d6x6,
            0x45 => Self::Astc2d8x5,
            0x46 => Self::Astc2d8x6,
            0x47 => Self::Astc2d8x8,
            0x48 => Self::Astc2d10x5,
            0x49 => Self::Astc2d10x6,
            0x4A => Self::Astc2d10x8,
            0x4B => Self::Astc2d10x10,
            0x4C => Self::Astc2d12x10,
            0x4D => Self::Astc2d12x12,
            _ => Self::Invalid,
        }
    }
}

/// Texture type (4-bit field from TIC word 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureType {
    Texture1D,
    Texture2D,
    Texture3D,
    Cubemap,
    Array1D,
    Array2D,
    Buffer1D,
    Texture2DNoMip,
    CubemapArray,
    Invalid,
}

impl TextureType {
    pub fn from_raw(value: u32) -> Self {
        match value & 0xF {
            0 => Self::Texture1D,
            1 => Self::Texture2D,
            2 => Self::Texture3D,
            3 => Self::Cubemap,
            4 => Self::Array1D,
            5 => Self::Array2D,
            6 => Self::Buffer1D,
            7 => Self::Texture2DNoMip,
            8 => Self::CubemapArray,
            _ => Self::Invalid,
        }
    }
}

/// Texture component type (3-bit field, per-channel in TIC word 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentType {
    SNorm,
    UNorm,
    SInt,
    UInt,
    SNormForceFp16,
    UNormForceFp16,
    Float,
    Invalid,
}

impl ComponentType {
    pub fn from_raw(value: u32) -> Self {
        match value & 0x7 {
            1 => Self::SNorm,
            2 => Self::UNorm,
            3 => Self::SInt,
            4 => Self::UInt,
            5 => Self::SNormForceFp16,
            6 => Self::UNormForceFp16,
            7 => Self::Float,
            _ => Self::Invalid,
        }
    }
}

/// Texture swizzle source (3-bit field, XYZW in TIC word 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwizzleSource {
    Zero,
    R,
    G,
    B,
    A,
    OneInt,
    OneFloat,
    Invalid,
}

impl SwizzleSource {
    pub fn from_raw(value: u32) -> Self {
        match value & 0x7 {
            0 => Self::Zero,
            2 => Self::R,
            3 => Self::G,
            4 => Self::B,
            5 => Self::A,
            6 => Self::OneInt,
            7 => Self::OneFloat,
            _ => Self::Invalid,
        }
    }
}

/// TIC header version (3-bit field from TIC word 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicHeaderVersion {
    OneDBuffer,
    PitchColorKey,
    Pitch,
    BlockLinear,
    BlockLinearColorKey,
    Invalid,
}

impl TicHeaderVersion {
    pub fn from_raw(value: u32) -> Self {
        match value & 0x7 {
            0 => Self::OneDBuffer,
            1 => Self::PitchColorKey,
            2 => Self::Pitch,
            3 => Self::BlockLinear,
            4 => Self::BlockLinearColorKey,
            _ => Self::Invalid,
        }
    }
}

/// Texture wrap/address mode (3-bit field in TSC word 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapMode {
    Wrap,
    Mirror,
    ClampToEdge,
    Border,
    Clamp,
    MirrorOnceClampToEdge,
    MirrorOnceBorder,
    MirrorOnceClampOgl,
    Invalid,
}

impl WrapMode {
    pub fn from_raw(value: u32) -> Self {
        match value & 0x7 {
            0 => Self::Wrap,
            1 => Self::Mirror,
            2 => Self::ClampToEdge,
            3 => Self::Border,
            4 => Self::Clamp,
            5 => Self::MirrorOnceClampToEdge,
            6 => Self::MirrorOnceBorder,
            7 => Self::MirrorOnceClampOgl,
            _ => Self::Invalid,
        }
    }
}

/// Texture magnification/minification filter (2-bit field in TSC word 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureFilter {
    Nearest,
    Linear,
    Invalid,
}

impl TextureFilter {
    pub fn from_raw(value: u32) -> Self {
        match value & 0x3 {
            1 => Self::Nearest,
            2 => Self::Linear,
            _ => Self::Invalid,
        }
    }
}

/// Mipmap filter mode (2-bit field in TSC word 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MipmapFilter {
    None,
    Nearest,
    Linear,
    Invalid,
}

impl MipmapFilter {
    pub fn from_raw(value: u32) -> Self {
        match value & 0x3 {
            1 => Self::None,
            2 => Self::Nearest,
            3 => Self::Linear,
            _ => Self::Invalid,
        }
    }
}

/// Depth compare function for sampler (3-bit field in TSC word 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthCompareFunc {
    Never,
    Less,
    Equal,
    LessEqual,
    Greater,
    NotEqual,
    GreaterEqual,
    Always,
}

impl DepthCompareFunc {
    pub fn from_raw(value: u32) -> Self {
        match value & 0x7 {
            0 => Self::Never,
            1 => Self::Less,
            2 => Self::Equal,
            3 => Self::LessEqual,
            4 => Self::Greater,
            5 => Self::NotEqual,
            6 => Self::GreaterEqual,
            7 => Self::Always,
            _ => unreachable!(),
        }
    }
}

// ── Texture/Sampler descriptor structs ──────────────────────────────────────

/// Parsed texture image control descriptor (TIC, 8 words = 32 bytes).
#[derive(Debug, Clone, PartialEq)]
pub struct TextureDescriptor {
    pub format: TextureFormat,
    pub r_type: ComponentType,
    pub g_type: ComponentType,
    pub b_type: ComponentType,
    pub a_type: ComponentType,
    pub x_source: SwizzleSource,
    pub y_source: SwizzleSource,
    pub z_source: SwizzleSource,
    pub w_source: SwizzleSource,
    pub address: u64,
    pub header_version: TicHeaderVersion,
    pub texture_type: TextureType,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub max_mip_level: u32,
    pub block_height: u32,
    pub block_depth: u32,
    pub srgb_conversion: bool,
    pub normalized_coords: bool,
}

impl TextureDescriptor {
    /// Parse a TIC descriptor from 8 raw u32 words.
    pub fn from_words(words: &[u32; 8]) -> Self {
        let word0 = words[0];
        let word1 = words[1];
        let word2 = words[2];
        let word3 = words[3];
        let word4 = words[4];
        let word5 = words[5];

        let addr_low = word1 as u64;
        let addr_high = (word2 & 0xFFFF) as u64;

        Self {
            format: TextureFormat::from_raw(word0 & 0x7F),
            r_type: ComponentType::from_raw((word0 >> 7) & 0x7),
            g_type: ComponentType::from_raw((word0 >> 10) & 0x7),
            b_type: ComponentType::from_raw((word0 >> 13) & 0x7),
            a_type: ComponentType::from_raw((word0 >> 16) & 0x7),
            x_source: SwizzleSource::from_raw((word0 >> 19) & 0x7),
            y_source: SwizzleSource::from_raw((word0 >> 22) & 0x7),
            z_source: SwizzleSource::from_raw((word0 >> 25) & 0x7),
            w_source: SwizzleSource::from_raw((word0 >> 28) & 0x7),
            address: (addr_high << 32) | addr_low,
            header_version: TicHeaderVersion::from_raw((word2 >> 21) & 0x7),
            texture_type: TextureType::from_raw((word4 >> 23) & 0xF),
            width: (word4 & 0xFFFF) + 1,
            height: (word5 & 0xFFFF) + 1,
            depth: ((word5 >> 16) & 0x3FFF) + 1,
            max_mip_level: (word3 >> 28) & 0xF,
            block_height: (word3 >> 3) & 0x7,
            block_depth: (word3 >> 6) & 0x7,
            srgb_conversion: (word4 & (1 << 22)) != 0,
            normalized_coords: (word5 & (1 << 31)) != 0,
        }
    }
}

/// Parsed texture sampler control descriptor (TSC, 8 words = 32 bytes).
#[derive(Debug, Clone, PartialEq)]
pub struct SamplerDescriptor {
    pub wrap_u: WrapMode,
    pub wrap_v: WrapMode,
    pub wrap_p: WrapMode,
    pub depth_compare_enabled: bool,
    pub depth_compare_func: DepthCompareFunc,
    pub max_anisotropy: u32,
    pub mag_filter: TextureFilter,
    pub min_filter: TextureFilter,
    pub mipmap_filter: MipmapFilter,
    pub min_lod: f32,
    pub max_lod: f32,
    pub mip_lod_bias: f32,
    pub border_color: [f32; 4],
}

impl SamplerDescriptor {
    /// Parse a TSC descriptor from 8 raw u32 words.
    pub fn from_words(words: &[u32; 8]) -> Self {
        let word0 = words[0];
        let word1 = words[1];
        let word2 = words[2];

        // mip_lod_bias is a 13-bit sign-extended fixed-point value at word1[24:12].
        let raw_bias = (word1 >> 12) & 0x1FFF;
        let bias_signed = if raw_bias & 0x1000 != 0 {
            // Sign-extend from 13 bits.
            (raw_bias | 0xFFFF_E000) as i32
        } else {
            raw_bias as i32
        };

        Self {
            wrap_u: WrapMode::from_raw(word0 & 0x7),
            wrap_v: WrapMode::from_raw((word0 >> 3) & 0x7),
            wrap_p: WrapMode::from_raw((word0 >> 6) & 0x7),
            depth_compare_enabled: (word0 & (1 << 9)) != 0,
            depth_compare_func: DepthCompareFunc::from_raw((word0 >> 10) & 0x7),
            max_anisotropy: (word0 >> 20) & 0x7,
            mag_filter: TextureFilter::from_raw(word1 & 0x3),
            min_filter: TextureFilter::from_raw((word1 >> 4) & 0x3),
            mipmap_filter: MipmapFilter::from_raw((word1 >> 6) & 0x3),
            min_lod: (word2 & 0xFFF) as f32 / 256.0,
            max_lod: ((word2 >> 12) & 0xFFF) as f32 / 256.0,
            mip_lod_bias: bias_signed as f32 / 256.0,
            border_color: [
                f32::from_bits(words[4]),
                f32::from_bits(words[5]),
                f32::from_bits(words[6]),
                f32::from_bits(words[7]),
            ],
        }
    }

    /// Convert the raw 3-bit max_anisotropy value to a multiplier (1/2/4/8/16).
    pub fn anisotropy_multiplier(&self) -> u32 {
        match self.max_anisotropy {
            0 => 1,
            1 => 2,
            2 => 4,
            3 => 8,
            4 => 16,
            _ => 16, // clamp to max
        }
    }
}

// ── Info structs ────────────────────────────────────────────────────────────

/// Information about an active vertex stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VertexStreamInfo {
    pub index: u32,
    pub address: u64,
    pub stride: u32,
    pub frequency: u32,
    pub enabled: bool,
}

/// One `Maxwell3D::Regs::TransformFeedback::Buffer` decoded from the live
/// register file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransformFeedbackBufferInfo {
    pub enable: u32,
    pub address: u64,
    pub size: i32,
    pub start_offset: i32,
}

/// Number of hardware viewports/scissors.
pub const NUM_VIEWPORTS: usize = 16;

/// Viewport computed from scale/translate registers.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ViewportInfo {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub depth_near: f32,
    pub depth_far: f32,
}

/// Raw `regs.viewport_transform[index]` snapshot.
///
/// Mirrors upstream `Maxwell3D::Regs::ViewportTransform`; the Rust port keeps
/// the raw fields separate from `ViewportInfo` so OpenGL viewport sync can use
/// upstream's signed scale/depth formulas instead of the older absolute-value
/// helper.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ViewportTransformInfo {
    pub scale_x: f32,
    pub scale_y: f32,
    pub scale_z: f32,
    pub translate_x: f32,
    pub translate_y: f32,
    pub translate_z: f32,
    pub swizzle: u32,
    pub snap_grid_precision: u32,
}

/// Raw `regs.surface_clip` snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SurfaceClipInfo {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Scissor rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScissorInfo {
    pub enabled: bool,
    pub min_x: u32,
    pub max_x: u32,
    pub min_y: u32,
    pub max_y: u32,
}

/// Blend state for a single render target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlendInfo {
    pub enabled: bool,
    pub separate_alpha: bool,
    pub color_op: BlendEquation,
    pub color_src: BlendFactor,
    pub color_dst: BlendFactor,
    pub alpha_op: BlendEquation,
    pub alpha_src: BlendFactor,
    pub alpha_dst: BlendFactor,
}

impl Default for BlendInfo {
    fn default() -> Self {
        Self {
            enabled: false,
            separate_alpha: false,
            color_op: BlendEquation::Add,
            color_src: BlendFactor::One,
            color_dst: BlendFactor::Zero,
            alpha_op: BlendEquation::Add,
            alpha_src: BlendFactor::One,
            alpha_dst: BlendFactor::Zero,
        }
    }
}

/// Blend constant color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlendColorInfo {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Default for BlendColorInfo {
    fn default() -> Self {
        Self {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        }
    }
}

/// Stencil state for one face.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StencilFaceInfo {
    pub fail_op: StencilOp,
    pub zfail_op: StencilOp,
    pub zpass_op: StencilOp,
    pub func: ComparisonOp,
    pub ref_value: u32,
    pub func_mask: u32,
    pub write_mask: u32,
}

impl Default for StencilFaceInfo {
    fn default() -> Self {
        Self {
            fail_op: StencilOp::Keep,
            zfail_op: StencilOp::Keep,
            zpass_op: StencilOp::Keep,
            func: ComparisonOp::Always,
            ref_value: 0,
            func_mask: 0xFFFF_FFFF,
            write_mask: 0xFFFF_FFFF,
        }
    }
}

/// Combined depth and stencil state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepthStencilInfo {
    pub depth_test_enable: bool,
    pub depth_write_enable: bool,
    pub depth_func: ComparisonOp,
    pub depth_mode: DepthMode,
    pub stencil_enable: bool,
    pub stencil_two_side: bool,
    pub front: StencilFaceInfo,
    pub back: StencilFaceInfo,
}

impl Default for DepthStencilInfo {
    fn default() -> Self {
        Self {
            depth_test_enable: false,
            depth_write_enable: false,
            depth_func: ComparisonOp::Always,
            depth_mode: DepthMode::ZeroToOne,
            stencil_enable: false,
            stencil_two_side: false,
            front: StencilFaceInfo::default(),
            back: StencilFaceInfo::default(),
        }
    }
}

/// Vertex attribute info unpacked from a single register word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VertexAttribInfo {
    pub buffer_index: u32,
    pub constant: bool,
    pub offset: u32,
    pub size: VertexAttribSize,
    pub attrib_type: VertexAttribType,
    pub bgra: bool,
}

impl Default for VertexAttribInfo {
    fn default() -> Self {
        Self {
            buffer_index: 0,
            constant: false,
            offset: 0,
            size: VertexAttribSize::Invalid,
            attrib_type: VertexAttribType::Invalid,
            bgra: false,
        }
    }
}

/// Shader stage info for one of the 6 pipeline program slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShaderStageInfo {
    pub enabled: bool,
    pub program_type: ShaderStageType,
    pub offset: u32,
    pub register_count: u32,
    pub binding_group: u32,
}

impl Default for ShaderStageInfo {
    fn default() -> Self {
        Self {
            enabled: false,
            program_type: ShaderStageType::VertexA,
            offset: 0,
            register_count: 0,
            binding_group: 0,
        }
    }
}

/// Per-render-target color write mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorMaskInfo {
    pub r: bool,
    pub g: bool,
    pub b: bool,
    pub a: bool,
}

impl Default for ColorMaskInfo {
    fn default() -> Self {
        Self {
            r: true,
            g: true,
            b: true,
            a: true,
        }
    }
}

/// Render target control: count and target mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtControlInfo {
    pub count: u32,
    pub map: [u32; 8],
}

impl Default for RtControlInfo {
    fn default() -> Self {
        Self {
            count: 1,
            map: [0, 1, 2, 3, 4, 5, 6, 7],
        }
    }
}

/// Rasterizer state.
#[derive(Debug, Clone, PartialEq)]
pub struct RasterizerInfo {
    pub cull_enable: bool,
    pub front_face: FrontFace,
    pub cull_face: CullFace,
    pub polygon_mode_front: PolygonMode,
    pub polygon_mode_back: PolygonMode,
    pub fill_via_triangle_mode: FillViaTriangleMode,
    pub line_width_smooth: f32,
    pub line_width_aliased: f32,
    pub polygon_offset_point_enable: bool,
    pub polygon_offset_line_enable: bool,
    pub polygon_offset_fill_enable: bool,
    pub depth_bias: f32,
    pub slope_scale_depth_bias: f32,
    pub depth_bias_clamp: f32,
}

impl Default for RasterizerInfo {
    fn default() -> Self {
        Self {
            cull_enable: false,
            front_face: FrontFace::CW,
            cull_face: CullFace::Back,
            polygon_mode_front: PolygonMode::Fill,
            polygon_mode_back: PolygonMode::Fill,
            fill_via_triangle_mode: FillViaTriangleMode::Disabled,
            line_width_smooth: 1.0,
            line_width_aliased: 1.0,
            polygon_offset_point_enable: false,
            polygon_offset_line_enable: false,
            polygon_offset_fill_enable: false,
            depth_bias: 0.0,
            slope_scale_depth_bias: 0.0,
            depth_bias_clamp: 0.0,
        }
    }
}

/// Upstream `Maxwell3D::Regs::PrimitiveRestart`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PrimitiveRestartInfo {
    pub enabled: bool,
    pub index: u32,
}

/// Upstream `Maxwell3D::Regs::LogicOp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogicOpInfo {
    pub enabled: bool,
    pub op: u32,
}

impl Default for LogicOpInfo {
    fn default() -> Self {
        Self {
            enabled: false,
            op: 0x1503,
        }
    }
}

/// Upstream `Maxwell3D::Regs::AntiAliasAlphaControl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AntiAliasAlphaControlInfo {
    pub alpha_to_coverage: bool,
    pub alpha_to_one: bool,
}

/// Upstream point-size state consumed by `RasterizerOpenGL::SyncPointState`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PointStateInfo {
    pub point_sprite_enable: bool,
    pub point_size_attribute_enabled: bool,
    pub point_size: f32,
}

/// Upstream line-width state consumed by `RasterizerOpenGL::SyncLineState`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LineStateInfo {
    pub line_anti_alias_enable: bool,
    pub line_width_smooth: f32,
    pub line_width_aliased: f32,
}

/// Upstream `Regs::LineStippleParams` plus its enable register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineStippleInfo {
    pub enabled: bool,
    pub factor: u32,
    pub pattern: u32,
}

/// Render target configuration for one color target.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderTargetInfo {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub tile_mode: u32,
    pub depth: u32,
    pub array_pitch: u32,
    pub base_layer: u32,
}

/// Depth/stencil target configuration (`regs.zeta` + `regs.zeta_size`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ZetaInfo {
    pub enabled: bool,
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub tile_mode: u32,
    pub array_pitch: u32,
    pub depth: u32,
}

/// A recorded draw call with all relevant state at the time of DRAW_END.
#[derive(Debug, Clone)]
pub struct DrawCall {
    pub topology: PrimitiveTopology,
    pub vertex_first: u32,
    pub vertex_count: u32,
    pub indexed: bool,
    pub index_buffer_addr: u64,
    pub index_buffer_addr_end: u64,
    pub index_buffer_count: u32,
    pub index_buffer_first: u32,
    pub index_format: IndexFormat,
    pub vertex_streams: [VertexStreamInfo; 32],
    /// Per-binding instance-rate enable bits (`regs.vertex_stream_instances`).
    pub vertex_stream_instances: [u32; 32],
    pub vertex_stream_limits: [crate::engines::draw_manager::VertexStreamLimit; 32],
    pub viewports: [ViewportInfo; NUM_VIEWPORTS],
    pub viewport_transforms: [ViewportTransformInfo; NUM_VIEWPORTS],
    pub scissors: [ScissorInfo; NUM_VIEWPORTS],
    pub viewport_scale_offset_enabled: bool,
    pub window_origin_lower_left: bool,
    pub window_origin_flip_y: bool,
    pub surface_clip: SurfaceClipInfo,
    pub blend: [BlendInfo; 8],
    pub blend_per_target_enabled: bool,
    pub global_blend: BlendInfo,
    pub iterated_blend_enabled: bool,
    pub blend_color: BlendColorInfo,
    pub depth_stencil: DepthStencilInfo,
    pub rasterizer: RasterizerInfo,
    pub rasterize_enable: bool,
    pub primitive_restart: PrimitiveRestartInfo,
    pub logic_op: LogicOpInfo,
    pub depth_clamp_enabled: bool,
    pub conservative_raster_enable: bool,
    pub engine_state: EngineHint,
    pub provoking_vertex_last: bool,
    pub depth_bounds_enable: bool,
    pub depth_bounds: [f32; 2],
    pub mandated_early_z: bool,
    pub alpha_test_enabled: bool,
    pub alpha_test_func: ComparisonOp,
    pub alpha_test_ref: f32,
    pub point_size: f32,
    pub tessellation_primitive: u32,
    pub tessellation_spacing: u32,
    pub tessellation_clockwise: bool,
    pub patch_vertices: u32,
    pub anti_alias_samples_mode: u32,
    pub anti_alias_alpha_control: AntiAliasAlphaControlInfo,
    pub line_anti_alias_enable: bool,
    pub line_stipple: LineStippleInfo,
    pub program_base_address: u64,
    pub cb_bindings: [[ConstBufferInfo; MAX_CB_SLOTS]; NUM_SHADER_STAGES],
    pub vertex_attribs: [VertexAttribInfo; NUM_VERTEX_ATTRIBS as usize],
    pub shader_stages: [ShaderStageInfo; NUM_SHADER_PROGRAMS],
    pub color_masks: [ColorMaskInfo; 8],
    pub rt_control: RtControlInfo,
    pub tex_header_pool_addr: u64,
    pub tex_header_pool_limit: u32,
    pub tex_sampler_pool_addr: u64,
    pub tex_sampler_pool_limit: u32,
    /// Instance count (1 for non-instanced, N for instanced batches).
    pub instance_count: u32,
    /// Base instance offset from GLOBAL_BASE_INSTANCE_INDEX.
    pub base_instance: u32,
    /// Base vertex offset from GLOBAL_BASE_VERTEX_INDEX (signed).
    pub base_vertex: i32,
    /// Non-empty only for InlineIndex draws.
    pub inline_index_data: Vec<u8>,
    /// Sampler binding mode for this draw call.
    pub sampler_binding: SamplerBinding,
    /// Render target configurations for up to 8 color targets.
    pub render_targets: [RenderTargetInfo; 8],
    /// Depth/stencil target configuration (`regs.zeta` + `regs.zeta_size`).
    pub zeta: ZetaInfo,
    /// Transform feedback enable/state captured at draw time.
    pub transform_feedback_enabled: bool,
    pub transform_feedback_state: TransformFeedbackState,
    /// Dirty flags captured with this draw, matching upstream state-tracker
    /// decisions for render-target refresh.
    pub dirty_flags: [bool; 256],
}

impl DrawCall {
    /// Returns the render-target register group captured for this draw.
    ///
    /// Rust records Maxwell state before entering the backend. Keeping this
    /// conversion on the snapshot owner prevents renderer backends from
    /// independently reconstructing the upstream render-target group.
    pub fn render_targets(&self) -> crate::engines::draw_manager::Maxwell3DRenderTargets {
        crate::engines::draw_manager::Maxwell3DRenderTargets {
            rt_control: self.rt_control,
            render_targets: self.render_targets,
            zeta: self.zeta,
            anti_alias_samples_mode: self.anti_alias_samples_mode,
            surface_clip: self.surface_clip,
        }
    }
}

/// Port of `Maxwell3D::DirtyState`.
///
/// Upstream owns both the live dirty flags and the register-to-flag lookup
/// tables inside the Maxwell3D owner. The selected rasterizer populates the
/// backend-specific tables from `InitializeChannel`.
pub struct DirtyState {
    pub flags: [bool; 256],
    pub tables: dirty_flags::DirtyTables,
}

impl DirtyState {
    fn new() -> Self {
        let mut flags = [false; 256];
        flags.fill(true);
        let tables = [[dirty_flags::flags::NULL_ENTRY; ENGINE_REG_COUNT]; 2];
        Self { flags, tables }
    }
}

// ── Engine struct ───────────────────────────────────────────────────────────

pub struct Maxwell3D {
    pub(crate) regs: Box<[u32; ENGINE_REG_COUNT]>,
    /// Shadow copy of registers for shadow RAM tracking.
    shadow_state: Box<[u32; ENGINE_REG_COUNT]>,
    /// Engine interface state: execution mask, method sink, dirty tracking.
    pub interface_state: EngineInterfaceState,
    /// Whether conditional rendering is active.
    execute_on: bool,
    /// Upstream owner `DirtyState dirty`.
    pub(crate) dirty: DirtyState,
    /// Upstream owner `DrawManager draw_manager`.
    ///
    /// Rust keeps the object in a `Box` so `with_draw_manager` can transfer
    /// only its pointer while splitting the mutable DrawManager/Maxwell3D
    /// borrows. The DrawManager allocation and address remain stable.
    draw_manager: Option<Box<dm::DrawManager>>,
    /// Draw-manager state currently borrowed out by `with_draw_manager`.
    ///
    /// Upstream keeps `draw_manager` permanently owned by `Maxwell3D`, so
    /// backend code can read `draw_manager->GetDrawState()` during a
    /// rasterizer callback. Rust temporarily moves the manager out to split
    /// the two mutable borrows; retain a scoped address to the active state
    /// for that callback so channel-bound caches observe the same owner state
    /// without cloning it.
    active_draw_manager_state: Option<usize>,
    /// Constant buffer bindings: 5 shader stages x 18 slots.
    cb_bindings: [[ConstBufferInfo; MAX_CB_SLOTS]; NUM_SHADER_STAGES],
    /// Pending semaphore writes to be returned by execute_pending.
    pending_semaphore_writes: Vec<PendingWrite>,
    /// Bound rasterizer backend.
    ///
    /// Upstream stores `VideoCore::RasterizerInterface* rasterizer`.
    ///
    /// Rust uses `RasterizerHandle`, a centralized non-owning pointer wrapper
    /// matching that upstream contract.
    rasterizer: Option<RasterizerHandle>,
    /// Start offsets of each macro in macro memory.
    macro_positions: [u32; 0x80],
    /// MME macro engine for uploaded programmable macro execution.
    macro_engine: MacroEngine,
    /// Upstream owner `Upload::State upload_state`.
    upload_state: engine_upload::State,
    /// Upstream owner `MemoryManager& memory_manager`.
    memory_manager: Option<Arc<Mutex<MemoryManager>>>,
    /// Method of the macro currently being fed parameters (0 = none).
    executing_macro: u32,
    /// Accumulated parameters for the current macro call.
    macro_params: Vec<u32>,
    /// GPU addresses for macro parameter words.
    macro_addresses: Vec<u64>,
    /// (segment_addr, word_count) pairs for macro parameter memory segments.
    macro_segments: Vec<(u64, u32)>,
    /// Whether the current macro has dirty memory.
    current_macro_dirty: bool,
    /// Rust adaptation for the upstream `Core::System& system` dependency used
    /// here only to read guest memory through the active GPU owner.
    guest_memory_reader: Option<Arc<dyn Fn(u64, &mut [u8]) + Send + Sync>>,
    /// Owner-local bridge for guest memory writes needed by inline upload paths.
    guest_memory_writer: Option<Arc<dyn Fn(u64, &[u8]) + Send + Sync>>,
    /// Rust owner-local bridge for upstream `system.GPU().GetTicks()` query timestamp writes.
    gpu_ticks_getter: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
    /// Upstream owner `EngineHint engine_state`.
    pub(crate) engine_state: EngineHint,
    /// Upstream owner `replace_table`.
    pub(crate) replace_table: std::collections::HashMap<u64, HleReplacementAttributeType>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum EngineHint {
    None = 0,
    OnHleMacro = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HleReplacementAttributeType {
    BaseVertex,
    BaseInstance,
    DrawId,
}

impl Maxwell3D {
    /// Port of upstream `Maxwell3D::GetRegisterValue` used by macro engines.
    pub(crate) fn get_register_value(&self, method: u32) -> u32 {
        assert!(
            (method as usize) < self.regs.len(),
            "Invalid Maxwell3D register"
        );
        self.regs[method as usize]
    }

    pub(crate) fn register_array_ptr(&self) -> *const u32 {
        self.regs.as_ptr()
    }
    fn draw_manager(&self) -> &dm::DrawManager {
        self.draw_manager
            .as_deref()
            .expect("DrawManager is only detached inside with_draw_manager")
    }

    pub(crate) fn draw_manager_mut(&mut self) -> &mut dm::DrawManager {
        self.draw_manager
            .as_deref_mut()
            .expect("DrawManager is only detached inside with_draw_manager")
    }

    fn sync_draw_manager_from_local(&self, draw_manager: &mut dm::DrawManager) {
        let vertex_buffer = dm::VertexBuffer {
            first: self.regs[VB_FIRST as usize],
            count: self.regs[VB_COUNT as usize],
        };
        let index_buffer = dm::IndexBuffer {
            first: self.index_buffer_first(),
            count: self.index_buffer_count(),
            format: self.index_buffer_format(),
        };
        let base_instance = self.base_instance();
        let base_index = self.base_vertex() as u32;
        let draw_state = &mut draw_manager.draw_state;
        draw_state.vertex_buffer = vertex_buffer;
        draw_state.index_buffer = index_buffer;
        draw_state.base_instance = base_instance;
        draw_state.base_index = base_index;
    }

    pub(crate) fn with_draw_manager<R>(
        &mut self,
        f: impl FnOnce(&mut dm::DrawManager, &mut Self) -> R,
    ) -> R {
        struct RestoreDrawManager {
            slot: *mut Option<Box<dm::DrawManager>>,
            draw_manager: Option<Box<dm::DrawManager>>,
        }

        impl Drop for RestoreDrawManager {
            fn drop(&mut self) {
                // SAFETY: `slot` points to the originating Maxwell3D field.
                // The field remains alive for this guard's scope and is empty
                // until the retained Box is restored here.
                unsafe {
                    *self.slot = self.draw_manager.take();
                }
            }
        }

        let draw_manager = self
            .draw_manager
            .take()
            .expect("nested with_draw_manager call");
        let mut restore = RestoreDrawManager {
            slot: std::ptr::addr_of_mut!(self.draw_manager),
            draw_manager: Some(draw_manager),
        };
        let draw_manager = restore.draw_manager.as_deref_mut().unwrap();
        self.sync_draw_manager_from_local(draw_manager);
        f(draw_manager, self)
    }

    fn with_active_draw_manager_state<R>(
        &mut self,
        draw_state: &dm::DrawState,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        struct RestoreActiveDrawState {
            slot: *mut Option<usize>,
            previous: Option<usize>,
        }

        impl Drop for RestoreActiveDrawState {
            fn drop(&mut self) {
                unsafe {
                    *self.slot = self.previous;
                }
            }
        }

        let previous = self
            .active_draw_manager_state
            .replace((draw_state as *const dm::DrawState) as usize);
        let _restore = RestoreActiveDrawState {
            slot: &mut self.active_draw_manager_state,
            previous,
        };
        f(self)
    }

    fn new_impl(memory_manager: Arc<Mutex<MemoryManager>>) -> Self {
        Self::new_impl_common(
            engine_upload::State::new_with_memory_manager(Arc::clone(&memory_manager)),
            Some(memory_manager),
        )
    }

    #[cfg(test)]
    fn new_test_impl() -> Self {
        Self::new_impl_common(engine_upload::State::new(), None)
    }

    fn new_impl_common(
        upload_state: engine_upload::State,
        memory_manager: Option<Arc<Mutex<MemoryManager>>>,
    ) -> Self {
        #[cfg(target_arch = "x86_64")]
        let is_macro_interpreted = *common::settings::values().disable_macro_jit.get_value();
        #[cfg(not(target_arch = "x86_64"))]
        let is_macro_interpreted = true;

        // Build execution mask: mark which methods trigger immediate execution.
        let mut execution_mask = vec![false; u16::MAX as usize];
        for i in 0..execution_mask.len() {
            execution_mask[i] = Self::is_method_executable(i as u32);
        }

        let mut engine = Self {
            regs: Box::new([0u32; ENGINE_REG_COUNT]),
            shadow_state: Box::new([0u32; ENGINE_REG_COUNT]),
            interface_state: EngineInterfaceState {
                execution_mask,
                method_sink: Vec::new(),
                current_dirty: false,
                current_dma_segment: 0,
            },
            execute_on: true,
            dirty: DirtyState::new(),
            draw_manager: Some(Box::new(dm::DrawManager::new())),
            active_draw_manager_state: None,
            cb_bindings: [[ConstBufferInfo::default(); MAX_CB_SLOTS]; NUM_SHADER_STAGES],
            pending_semaphore_writes: Vec::new(),
            rasterizer: None,
            macro_positions: [0u32; 0x80],
            macro_engine: MacroEngine::new(is_macro_interpreted),
            upload_state,
            memory_manager,
            executing_macro: 0,
            macro_params: Vec::new(),
            macro_addresses: Vec::new(),
            macro_segments: Vec::new(),
            current_macro_dirty: false,
            guest_memory_reader: None,
            guest_memory_writer: None,
            gpu_ticks_getter: None,
            engine_state: EngineHint::None,
            replace_table: std::collections::HashMap::new(),
        };
        engine.initialize_register_defaults();
        engine
    }

    #[cfg(test)]
    pub fn new() -> Self {
        Self::new_test_impl()
    }

    pub fn new_with_memory_manager(memory_manager: Arc<Mutex<MemoryManager>>) -> Self {
        Self::new_impl(memory_manager)
    }

    fn initialize_register_defaults(&mut self) {
        for viewport in 0..16usize {
            let base = VIEWPORT_BASE as usize + viewport * VIEWPORT_STRIDE as usize;
            self.regs[base + 2] = f32::to_bits(0.0);
            self.regs[base + 3] = f32::to_bits(1.0);
        }

        for viewport in 0..16usize {
            let base = VP_TRANSFORM_BASE as usize + viewport * VP_TRANSFORM_STRIDE as usize;
            // Port of Maxwell3D's constructor: PositiveX/Y/Z/W are packed in
            // 3-bit fields at offsets 0, 4, 8, and 12 respectively.
            self.regs[base + 6] = 0x6420;
        }

        self.regs[BLEND_BASE as usize + 1] = 1;
        self.regs[BLEND_BASE as usize + 2] = 0x02;
        self.regs[BLEND_BASE as usize + 3] = 0x01;
        self.regs[BLEND_BASE as usize + 4] = 1;
        self.regs[BLEND_BASE as usize + 5] = 0x02;
        self.regs[BLEND_BASE as usize + 7] = 0x01;

        for rt in 0..8usize {
            let base = BLEND_PER_TARGET_BASE as usize + rt * BLEND_PER_TARGET_STRIDE as usize;
            self.regs[base + 1] = 1;
            self.regs[base + 2] = 0x02;
            self.regs[base + 3] = 0x01;
            self.regs[base + 4] = 1;
            self.regs[base + 5] = 0x02;
            self.regs[base + 6] = 0x01;
        }

        let front_base = STENCIL_FRONT_OP_BASE as usize;
        self.regs[front_base] = 1;
        self.regs[front_base + 1] = 1;
        self.regs[front_base + 2] = 1;
        self.regs[front_base + 3] = 0x207;
        self.regs[STENCIL_FRONT_FUNC_MASK as usize] = 0xFFFF_FFFF;
        self.regs[STENCIL_FRONT_MASK as usize] = 0xFFFF_FFFF;
        self.regs[STENCIL_TWO_SIDE_ENABLE as usize] = 1;

        let back_base = STENCIL_BACK_OP_BASE as usize;
        self.regs[back_base] = 1;
        self.regs[back_base + 1] = 1;
        self.regs[back_base + 2] = 1;
        self.regs[back_base + 3] = 0x207;
        self.regs[STENCIL_BACK_FUNC_MASK as usize] = 0xFFFF_FFFF;
        self.regs[STENCIL_BACK_MASK as usize] = 0xFFFF_FFFF;

        self.regs[DEPTH_TEST_FUNC as usize] = 0x207;
        self.regs[FRONT_FACE as usize] = 0x0900;
        self.regs[CULL_FACE as usize] = 0x0405;
        self.regs[POINT_SIZE as usize] = f32::to_bits(1.0);

        for rt in 0..8usize {
            self.regs[COLOR_MASK_BASE as usize + rt] = 0x1111;
        }

        for attrib in 0..NUM_VERTEX_ATTRIBS as usize {
            self.regs[VERTEX_ATTRIB_BASE as usize + attrib] |= 1 << 6;
        }

        self.regs[RASTERIZE_ENABLE as usize] = 1;
        self.regs[COLOR_TARGET_MRT_ENABLE as usize] = 1;
        self.regs[FRAMEBUFFER_SRGB as usize] = 1;
        self.regs[LINE_WIDTH_ALIASED as usize] = f32::to_bits(1.0);
        self.regs[LINE_WIDTH_SMOOTH as usize] = f32::to_bits(1.0);
        self.regs[POLYGON_MODE_BACK as usize] = 0x1B02;
        self.regs[POLYGON_MODE_FRONT as usize] = 0x1B02;

        self.shadow_state.copy_from_slice(&self.regs[..]);
    }

    /// Whether conditional rendering allows execution.
    pub fn should_execute(&self) -> bool {
        self.execute_on
    }

    pub fn bind_rasterizer(&mut self, rasterizer: &dyn RasterizerInterface) {
        self.rasterizer = Some(RasterizerHandle::from_ref(rasterizer));
        self.upload_state.bind_rasterizer(rasterizer);
    }

    #[cfg(test)]
    pub fn set_memory_manager(&mut self, memory_manager: Arc<Mutex<MemoryManager>>) {
        self.upload_state
            .bind_memory_manager(Arc::clone(&memory_manager));
        self.memory_manager = Some(memory_manager);
    }

    /// Rust owner-local bridge for upstream stored `MemoryManager&`.
    pub fn memory_manager(&self) -> Option<Arc<Mutex<MemoryManager>>> {
        self.memory_manager.as_ref().map(Arc::clone)
    }

    pub fn set_guest_memory_reader(
        &mut self,
        guest_memory_reader: Arc<dyn Fn(u64, &mut [u8]) + Send + Sync>,
    ) {
        self.guest_memory_reader = Some(guest_memory_reader);
    }

    /// Rust owner-local bridge for the guest CPU-memory read callback used by
    /// the current `MemoryManager` adaptation.
    pub fn guest_memory_reader(&self) -> Option<Arc<dyn Fn(u64, &mut [u8]) + Send + Sync>> {
        self.guest_memory_reader.as_ref().map(Arc::clone)
    }

    /// Rust owner-local bridge for upstream `Tegra::MemoryManager& gpu_memory`
    /// access from shared shader-cache owners.
    pub fn make_gpu_memory_reader(&self) -> Option<Arc<dyn Fn(u64, &mut [u8]) + Send + Sync>> {
        let memory_manager = self.memory_manager.as_ref().cloned()?;
        Some(Arc::new(move |gpu_addr, output| {
            memory_manager.lock().read_block(gpu_addr, output);
        }))
    }

    pub fn set_guest_memory_writer(
        &mut self,
        guest_memory_writer: Arc<dyn Fn(u64, &[u8]) + Send + Sync>,
    ) {
        self.guest_memory_writer = Some(guest_memory_writer);
    }

    pub fn set_gpu_ticks_getter(&mut self, gpu_ticks_getter: Arc<dyn Fn() -> u64 + Send + Sync>) {
        self.gpu_ticks_getter = Some(gpu_ticks_getter);
    }

    /// Upstream reads `regs.rasterize_enable` directly.
    pub fn rasterize_enable(&self) -> bool {
        self.regs[RASTERIZE_ENABLE as usize] != 0
    }

    pub fn engine_state(&self) -> EngineHint {
        self.engine_state
    }

    pub fn current_topology(&self) -> PrimitiveTopology {
        self.draw_manager_state().topology
    }

    /// Upstream OpenGL owners read `maxwell3d->draw_manager->GetDrawState().topology`.
    ///
    /// The Rust tree still has a reduced `Maxwell3D`/`DrawManager` split, so
    /// this accessor resolves the topology through the matching
    /// `draw_manager.rs` owner logic instead of exposing another backend-local
    /// reconstruction.
    pub fn draw_manager_topology(&self) -> PrimitiveTopology {
        self.draw_manager_state().topology
    }

    /// Upstream `maxwell3d->regs.zeta_enable`.
    pub fn zeta_enable(&self) -> bool {
        self.regs[ZETA_ENABLE as usize] != 0
    }

    pub fn zeta_info(&self) -> ZetaInfo {
        let zeta = ZETA_BASE as usize;
        let zeta_size = ZETA_SIZE_BASE as usize;
        let address_high = self.regs[zeta] as u64;
        let address_low = self.regs[zeta + 1] as u64;
        ZetaInfo {
            enabled: self.zeta_enable(),
            address: (address_high << 32) | address_low,
            format: self.regs[zeta + 2],
            tile_mode: self.regs[zeta + 3],
            array_pitch: self.regs[zeta + 4],
            width: self.regs[zeta_size],
            height: self.regs[zeta_size + 1],
            depth: self.regs[zeta_size + 2],
        }
    }

    /// Upstream `maxwell3d->regs.anti_alias_samples_mode`.
    pub fn anti_alias_samples_mode(&self) -> u32 {
        self.regs[ANTI_ALIAS_SAMPLES_MODE as usize]
    }

    /// Upstream `maxwell3d->draw_manager->GetDrawState()`.
    pub fn draw_manager_state(&self) -> &crate::engines::draw_manager::DrawState {
        if let Some(address) = self.active_draw_manager_state {
            // `with_active_draw_manager_state` installs this address from an
            // immutable borrow and restores it before that borrow expires.
            return unsafe { &*(address as *const dm::DrawState) };
        }
        self.draw_manager().get_draw_state()
    }

    /// Upstream reads `regs.mandated_early_z != 0` directly.
    pub fn mandated_early_z(&self) -> bool {
        self.regs[MANDATED_EARLY_Z as usize] != 0
    }

    /// Upstream reads `regs.patch_vertices`.
    pub fn patch_vertices(&self) -> u32 {
        self.regs[PATCH_VERTICES as usize]
    }

    /// Upstream reads `regs.alpha_test_enabled != 0` directly.
    pub fn alpha_test_enabled(&self) -> bool {
        self.regs[ALPHA_TEST_ENABLED as usize] != 0
    }

    /// Upstream reads `regs.alpha_test_func` directly.
    pub fn alpha_test_func(&self) -> ComparisonOp {
        ComparisonOp::from_raw(self.regs[ALPHA_TEST_FUNC as usize])
    }

    /// Upstream reads `regs.alpha_test_ref` directly.
    pub fn alpha_test_ref(&self) -> f32 {
        f32::from_bits(self.regs[ALPHA_TEST_REF as usize])
    }

    /// Upstream reads `regs.tessellation.params.domain_type.Value()`.
    pub fn tessellation_domain_type(&self) -> u32 {
        self.regs[TESSELLATION_PARAMS as usize] & 0x3
    }

    /// Upstream reads `regs.tessellation.params.spacing.Value()`.
    pub fn tessellation_spacing(&self) -> u32 {
        (self.regs[TESSELLATION_PARAMS as usize] >> 4) & 0x3
    }

    /// Upstream compares `regs.tessellation.params.output_primitives` against `Triangles_CW`.
    pub fn tessellation_clockwise(&self) -> bool {
        ((self.regs[TESSELLATION_PARAMS as usize] >> 8) & 0x3) == 2
    }

    /// Upstream reads `regs.tessellation.params.output_primitives.Value()`.
    pub fn tessellation_output_primitives(&self) -> u32 {
        (self.regs[TESSELLATION_PARAMS as usize] >> 8) & 0x3
    }

    /// Upstream reads `regs.transform_feedback_enabled != 0` directly.
    pub fn transform_feedback_enabled(&self) -> bool {
        self.regs[TRANSFORM_FEEDBACK_ENABLED as usize] != 0
    }

    /// Port of the `SetXfbState(..., regs)` owner read inputs from upstream.
    pub fn transform_feedback_state(&self) -> TransformFeedbackState {
        let layouts = std::array::from_fn(|index| {
            let base = (TRANSFORM_FEEDBACK_CONTROLS_BASE
                + index as u32 * TRANSFORM_FEEDBACK_CONTROL_STRIDE) as usize;
            TransformFeedbackLayout {
                stream: self.regs[base],
                varying_count: self.regs[base + 1],
                stride: self.regs[base + 2],
            }
        });
        let varyings = std::array::from_fn(|buffer| {
            std::array::from_fn(|entry| {
                StreamOutLayout::from_raw(
                    self.regs[STREAM_OUT_LAYOUT_BASE as usize + buffer * 32 + entry],
                )
            })
        });
        TransformFeedbackState { layouts, varyings }
    }

    /// Read one transform-feedback buffer from
    /// `regs.transform_feedback.buffers[index]`.
    pub fn transform_feedback_buffer_info(&self, index: u32) -> TransformFeedbackBufferInfo {
        let base =
            (TRANSFORM_FEEDBACK_BUFFERS_BASE + index * TRANSFORM_FEEDBACK_BUFFER_STRIDE) as usize;
        TransformFeedbackBufferInfo {
            enable: self.regs[base],
            address: ((self.regs[base + 1] as u64) << 32) | self.regs[base + 2] as u64,
            size: self.regs[base + 3] as i32,
            start_offset: self.regs[base + TRANSFORM_FEEDBACK_BUFFER_START_OFFSET as usize] as i32,
        }
    }

    pub fn set_engine_state(&mut self, state: EngineHint) {
        self.engine_state = state;
    }

    pub fn set_hle_replacement_attribute_type(
        &mut self,
        bank: u32,
        offset: u32,
        name: HleReplacementAttributeType,
    ) {
        let key = ((bank as u64) << 32) | offset as u64;
        self.replace_table.insert(key, name);
    }

    pub fn get_replace_const_buffer(&self, bank: u32, offset: u32) -> Option<ReplaceConstant> {
        let key = ((bank as u64) << 32) | offset as u64;
        let value = self.replace_table.get(&key).copied()?;
        Some(match value {
            HleReplacementAttributeType::BaseVertex => ReplaceConstant::BaseVertex,
            HleReplacementAttributeType::BaseInstance => ReplaceConstant::BaseInstance,
            HleReplacementAttributeType::DrawId => ReplaceConstant::DrawID,
        })
    }

    pub fn post_vtg_shader_attrib_skip_mask(&self) -> [u32; 8] {
        let base = 0x1240 / 4;
        let mut result = [0u32; 8];
        result.copy_from_slice(&self.regs[base..base + 8]);
        result
    }

    /// Upstream reads `regs.viewport_scale_offset_enabled` directly.
    pub fn viewport_transform_state(&self) -> u32 {
        self.regs[VIEWPORT_SCALE_OFFSET_ENABLED as usize]
    }

    /// Upstream reads `regs.bindless_texture_const_buffer_slot` directly.
    pub fn bindless_texture_const_buffer_slot(&self) -> u32 {
        self.regs[BINDLESS_TEXTURE_CONST_BUFFER_SLOT as usize]
    }

    /// Upstream reads `regs.sampler_binding` directly.
    pub fn sampler_binding(&self) -> SamplerBinding {
        if self.regs[SAMPLER_BINDING as usize] == SamplerBinding::ViaHeaderBinding as u32 {
            SamplerBinding::ViaHeaderBinding
        } else {
            SamplerBinding::Independently
        }
    }

    fn read_gpu_block(&self, addr: u64, output: &mut [u8]) -> bool {
        let Some(memory_manager) = self.memory_manager.as_ref().cloned() else {
            return false;
        };
        memory_manager.lock().read_block(addr, output);
        true
    }

    pub(crate) fn with_rasterizer_mut<R>(
        &mut self,
        f: impl FnOnce(&mut dyn RasterizerInterface) -> R,
    ) -> Option<R> {
        let handle = self.rasterizer?;
        Some(unsafe { handle.with_mut(f) })
    }

    pub fn get_macro_address(&self, index: usize) -> u64 {
        self.macro_addresses[index]
    }

    pub fn refresh_parameters(&mut self) {
        if !self.current_macro_dirty {
            return;
        }
        let mut parameters = std::mem::take(&mut self.macro_params);
        self.refresh_parameters_impl(&mut parameters);
        self.macro_params = parameters;
    }

    pub fn any_parameters_dirty(&self) -> bool {
        self.current_macro_dirty
    }

    pub(crate) fn refresh_parameters_impl(&mut self, parameters: &mut [u32]) {
        if !common::settings::is_gpu_level_high(&common::settings::values()) {
            return;
        }
        let mut current_index = 0usize;
        for &(segment_addr, word_count) in &self.macro_segments {
            let word_count = word_count as usize;
            if segment_addr == 0 {
                current_index += word_count;
                continue;
            }

            let bytes = bytemuck::cast_slice_mut(
                &mut parameters[current_index..current_index + word_count],
            );
            let memory_manager = self
                .memory_manager
                .as_ref()
                .expect("nonzero macro segment requires GPU memory");
            let memory_manager = memory_manager.lock();
            memory_manager.read_block(segment_addr, bytes);
            current_index += word_count;
        }
    }

    // ── Render target accessors ──────────────────────────────────────────

    /// GPU virtual address of render target `index` (0..7).
    fn rt_address(&self, index: usize) -> u64 {
        let base = (RT_BASE + index as u32 * RT_STRIDE) as usize;
        let high = self.regs[base + RT_OFF_ADDRESS_HIGH as usize] as u64;
        let low = self.regs[base + RT_OFF_ADDRESS_LOW as usize] as u64;
        (high << 32) | low
    }

    fn upload_registers(&self) -> engine_upload::Registers {
        engine_upload::Registers {
            line_length_in: self.regs[UPLOAD_REGS_BASE],
            line_count: self.regs[UPLOAD_REGS_BASE + 1],
            dest: engine_upload::DestRegisters {
                address_high: self.regs[UPLOAD_REGS_BASE + 2],
                address_low: self.regs[UPLOAD_REGS_BASE + 3],
                pitch: self.regs[UPLOAD_REGS_BASE + 4],
                block_dims: self.regs[UPLOAD_REGS_BASE + 5],
                width: self.regs[UPLOAD_REGS_BASE + 6],
                height: self.regs[UPLOAD_REGS_BASE + 7],
                depth: self.regs[UPLOAD_REGS_BASE + 8],
                layer: self.regs[UPLOAD_REGS_BASE + 9],
                x: self.regs[UPLOAD_REGS_BASE + 10],
                y: self.regs[UPLOAD_REGS_BASE + 11],
            },
        }
    }

    fn launch_dma_is_linear(&self) -> bool {
        (self.regs[LAUNCH_DMA as usize] & 0x1) != 0
    }

    fn process_inline_upload_word(&mut self, data: u32, is_last_call: bool) {
        let regs = self.upload_registers();
        self.upload_state
            .process_data_word(&regs, data, is_last_call);
    }

    fn process_inline_upload_multi(&mut self, data: &[u32]) {
        if self.memory_manager.is_none() {
            return;
        }
        let regs = self.upload_registers();
        self.upload_state.process_data_multi(&regs, data);
    }

    /// Process a batch of packed inline indices without replaying every method write.
    /// Matches upstream `Maxwell3D::ProcessInlineIndexMultiData`.
    fn process_inline_index_multi_data(&mut self, method: u32, data: &[u32]) {
        let Some(&last_argument) = data.last() else {
            return;
        };
        let argument = self.process_shadow_ram(method, last_argument);
        self.process_dirty_registers(method, argument);
        self.with_draw_manager(|draw_manager, _| {
            draw_manager.set_inline_index_buffer_multi(method, data);
        });
    }

    /// Width of render target `index`.
    fn rt_width(&self, index: usize) -> u32 {
        let base = (RT_BASE + index as u32 * RT_STRIDE) as usize;
        self.regs[base + RT_OFF_WIDTH as usize]
    }

    /// Height of render target `index`.
    fn rt_height(&self, index: usize) -> u32 {
        let base = (RT_BASE + index as u32 * RT_STRIDE) as usize;
        self.regs[base + RT_OFF_HEIGHT as usize]
    }

    /// Format of render target `index`.
    fn rt_format(&self, index: usize) -> u32 {
        let base = (RT_BASE + index as u32 * RT_STRIDE) as usize;
        self.regs[base + RT_OFF_FORMAT as usize]
    }

    /// Full render target config snapshot. Mirrors upstream
    /// `Maxwell3D::Regs::RenderTargetConfig` fields consumed by
    /// `ImageInfo(RenderTargetConfig, MsaaMode)`.
    fn rt_info(&self, index: usize) -> RenderTargetInfo {
        let base = (RT_BASE + index as u32 * RT_STRIDE) as usize;
        RenderTargetInfo {
            address: self.rt_address(index),
            width: self.regs[base + RT_OFF_WIDTH as usize],
            height: self.regs[base + RT_OFF_HEIGHT as usize],
            format: self.regs[base + RT_OFF_FORMAT as usize],
            tile_mode: self.regs[base + RT_OFF_TILE_MODE as usize],
            depth: self.regs[base + RT_OFF_DEPTH as usize],
            array_pitch: self.regs[base + RT_OFF_ARRAY_PITCH as usize],
            base_layer: self.regs[base + RT_OFF_BASE_LAYER as usize],
        }
    }

    /// Read the 4-component clear color as f32 values.
    fn clear_color_rgba(&self) -> [f32; 4] {
        let base = CLEAR_COLOR_BASE as usize;
        [
            f32::from_bits(self.regs[base]),
            f32::from_bits(self.regs[base + 1]),
            f32::from_bits(self.regs[base + 2]),
            f32::from_bits(self.regs[base + 3]),
        ]
    }

    // ── Vertex stream accessors ──────────────────────────────────────────

    /// Read vertex stream `index` (0..31) info from registers.
    pub fn vertex_stream_info(&self, index: u32) -> VertexStreamInfo {
        let base = (VERTEX_STREAM_BASE + index * VERTEX_STREAM_STRIDE) as usize;
        let word0 = self.regs[base]; // stride in bits[11:0], enable in bit 12
        let addr_high = self.regs[base + 1] as u64;
        let addr_low = self.regs[base + 2] as u64;
        let frequency = self.regs[base + 3];
        VertexStreamInfo {
            index,
            address: (addr_high << 32) | addr_low,
            stride: word0 & 0xFFF,
            frequency,
            enabled: (word0 & (1 << 12)) != 0,
        }
    }

    /// Read whether vertex stream `index` is accessed per instance.
    pub fn vertex_stream_instance(&self, index: u32) -> u32 {
        self.regs[(VERTEX_STREAM_INSTANCE_BASE + index) as usize]
    }

    /// Read vertex stream limit `index` (0..31) from registers.
    pub fn vertex_stream_limit(&self, index: u32) -> dm::VertexStreamLimit {
        let base = VERTEX_STREAM_LIMIT_BASE as usize + index as usize * 2;
        let high = self.regs[base] as u64;
        let low = self.regs[base + 1] as u64;
        dm::VertexStreamLimit {
            address: (high << 32) | low,
        }
    }

    // ── Index buffer accessors ───────────────────────────────────────────

    /// Index buffer GPU address.
    pub fn index_buffer_addr(&self) -> u64 {
        let base = IB_BASE as usize;
        let high = self.regs[base + IB_OFF_ADDR_HIGH as usize] as u64;
        let low = self.regs[base + IB_OFF_ADDR_LOW as usize] as u64;
        (high << 32) | low
    }

    /// Index buffer element format.
    pub fn index_buffer_format(&self) -> IndexFormat {
        IndexFormat::from_raw(self.regs[(IB_BASE + IB_OFF_FORMAT) as usize])
    }

    /// Index buffer first index.
    pub fn index_buffer_first(&self) -> u32 {
        self.regs[(IB_BASE + IB_OFF_FIRST) as usize]
    }

    /// Index buffer element count.
    pub fn index_buffer_count(&self) -> u32 {
        self.regs[(IB_BASE + IB_OFF_COUNT) as usize]
    }

    // ── Viewport accessors ───────────────────────────────────────────────

    /// Compute viewport info for viewport `index` (0..15) from scale/translate.
    pub fn viewport_info(&self, index: u32) -> ViewportInfo {
        let base = (VP_TRANSFORM_BASE + index * VP_TRANSFORM_STRIDE) as usize;
        let scale_x = f32::from_bits(self.regs[base]);
        let scale_y = f32::from_bits(self.regs[base + 1]);
        let scale_z = f32::from_bits(self.regs[base + 2]);
        let translate_x = f32::from_bits(self.regs[base + 3]);
        let translate_y = f32::from_bits(self.regs[base + 4]);
        let translate_z = f32::from_bits(self.regs[base + 5]);

        // Viewport transform: x = translate - |scale|, width = 2*|scale|
        let width = scale_x.abs() * 2.0;
        let height = scale_y.abs() * 2.0;
        ViewportInfo {
            x: translate_x - scale_x.abs(),
            y: translate_y - scale_y.abs(),
            width,
            height,
            depth_near: translate_z - scale_z.abs(),
            depth_far: translate_z + scale_z.abs(),
        }
    }

    /// Read raw viewport transform state for viewport `index` (0..15).
    pub fn viewport_transform_info(&self, index: u32) -> ViewportTransformInfo {
        let base = (VP_TRANSFORM_BASE + index * VP_TRANSFORM_STRIDE) as usize;
        ViewportTransformInfo {
            scale_x: f32::from_bits(self.regs[base]),
            scale_y: f32::from_bits(self.regs[base + 1]),
            scale_z: f32::from_bits(self.regs[base + 2]),
            translate_x: f32::from_bits(self.regs[base + 3]),
            translate_y: f32::from_bits(self.regs[base + 4]),
            translate_z: f32::from_bits(self.regs[base + 5]),
            swizzle: self.regs[base + 6],
            snap_grid_precision: self.regs[base + 7],
        }
    }

    /// Read raw surface clip rectangle.
    pub fn surface_clip_info(&self) -> SurfaceClipInfo {
        let x_width = self.regs[SURFACE_CLIP_BASE as usize];
        let y_height = self.regs[SURFACE_CLIP_BASE as usize + 1];
        SurfaceClipInfo {
            x: x_width & 0xFFFF,
            width: (x_width >> 16) & 0xFFFF,
            y: y_height & 0xFFFF,
            height: (y_height >> 16) & 0xFFFF,
        }
    }

    // ── Scissor accessors ────────────────────────────────────────────────

    /// Read scissor info for scissor `index` (0..15).
    pub fn scissor_info(&self, index: u32) -> ScissorInfo {
        let base = (SCISSOR_BASE + index * SCISSOR_STRIDE) as usize;
        let enable = self.regs[base];
        let x_packed = self.regs[base + 1]; // min_x[15:0] | max_x[31:16]
        let y_packed = self.regs[base + 2]; // min_y[15:0] | max_y[31:16]
        ScissorInfo {
            enabled: (enable & 1) != 0,
            min_x: x_packed & 0xFFFF,
            max_x: (x_packed >> 16) & 0xFFFF,
            min_y: y_packed & 0xFFFF,
            max_y: (y_packed >> 16) & 0xFFFF,
        }
    }

    // ── Blend accessors ──────────────────────────────────────────────────

    /// Read blend constant color.
    pub fn blend_color_info(&self) -> BlendColorInfo {
        let base = BLEND_COLOR_BASE as usize;
        BlendColorInfo {
            r: f32::from_bits(self.regs[base]),
            g: f32::from_bits(self.regs[base + 1]),
            b: f32::from_bits(self.regs[base + 2]),
            a: f32::from_bits(self.regs[base + 3]),
        }
    }

    /// Whether blend is enabled for render target `rt` (0..7).
    pub fn blend_enable(&self, rt: usize) -> bool {
        self.regs[(BLEND_BASE + 9 + rt as u32) as usize] != 0
    }

    /// Read global (non-per-target) blend info for render target `rt`.
    pub fn global_blend_info(&self, rt: usize) -> BlendInfo {
        let base = BLEND_BASE as usize;
        BlendInfo {
            enabled: self.blend_enable(rt),
            separate_alpha: self.regs[base] != 0,
            color_op: BlendEquation::from_raw(self.regs[base + 1]),
            color_src: BlendFactor::from_raw(self.regs[base + 2]),
            color_dst: BlendFactor::from_raw(self.regs[base + 3]),
            alpha_op: BlendEquation::from_raw(self.regs[base + 4]),
            alpha_src: BlendFactor::from_raw(self.regs[base + 5]),
            alpha_dst: BlendFactor::from_raw(self.regs[base + 7]),
        }
    }

    /// Read per-target blend info for render target `rt` (0..7).
    pub fn blend_per_target_info(&self, rt: usize) -> BlendInfo {
        let base = (BLEND_PER_TARGET_BASE + rt as u32 * BLEND_PER_TARGET_STRIDE) as usize;
        BlendInfo {
            enabled: self.blend_enable(rt),
            separate_alpha: self.regs[base] != 0,
            color_op: BlendEquation::from_raw(self.regs[base + 1]),
            color_src: BlendFactor::from_raw(self.regs[base + 2]),
            color_dst: BlendFactor::from_raw(self.regs[base + 3]),
            alpha_op: BlendEquation::from_raw(self.regs[base + 4]),
            alpha_src: BlendFactor::from_raw(self.regs[base + 5]),
            alpha_dst: BlendFactor::from_raw(self.regs[base + 6]),
        }
    }

    /// Effective blend info: per-target if enabled, otherwise global.
    pub fn effective_blend_info(&self, rt: usize) -> BlendInfo {
        if self.regs[BLEND_PER_TARGET_ENABLED as usize] != 0 {
            self.blend_per_target_info(rt)
        } else {
            self.global_blend_info(rt)
        }
    }

    pub fn blend_per_target_enabled(&self) -> bool {
        self.regs[BLEND_PER_TARGET_ENABLED as usize] != 0
    }

    /// Read `regs.iterated_blend.enable`.
    pub fn iterated_blend_enabled(&self) -> bool {
        (self.regs[ITERATED_BLEND as usize] & 1) != 0
    }

    // ── Depth/Stencil accessors ──────────────────────────────────────────

    /// Read combined depth and stencil state.
    pub fn depth_stencil_info(&self) -> DepthStencilInfo {
        let front_base = STENCIL_FRONT_OP_BASE as usize;
        let back_base = STENCIL_BACK_OP_BASE as usize;

        let front = StencilFaceInfo {
            fail_op: StencilOp::from_raw(self.regs[front_base]),
            zfail_op: StencilOp::from_raw(self.regs[front_base + 1]),
            zpass_op: StencilOp::from_raw(self.regs[front_base + 2]),
            func: ComparisonOp::from_raw(self.regs[front_base + 3]),
            ref_value: self.regs[STENCIL_FRONT_REF as usize],
            func_mask: self.regs[STENCIL_FRONT_FUNC_MASK as usize],
            write_mask: self.regs[STENCIL_FRONT_MASK as usize],
        };

        let back = StencilFaceInfo {
            fail_op: StencilOp::from_raw(self.regs[back_base]),
            zfail_op: StencilOp::from_raw(self.regs[back_base + 1]),
            zpass_op: StencilOp::from_raw(self.regs[back_base + 2]),
            func: ComparisonOp::from_raw(self.regs[back_base + 3]),
            ref_value: self.regs[STENCIL_BACK_REF as usize],
            func_mask: self.regs[STENCIL_BACK_FUNC_MASK as usize],
            write_mask: self.regs[STENCIL_BACK_MASK as usize],
        };

        DepthStencilInfo {
            depth_test_enable: self.regs[DEPTH_TEST_ENABLE as usize] != 0,
            depth_write_enable: self.regs[DEPTH_WRITE_ENABLE as usize] != 0,
            depth_func: ComparisonOp::from_raw(self.regs[DEPTH_TEST_FUNC as usize]),
            depth_mode: DepthMode::from_raw(self.regs[DEPTH_MODE as usize]),
            stencil_enable: self.regs[STENCIL_ENABLE as usize] != 0,
            stencil_two_side: self.regs[STENCIL_TWO_SIDE_ENABLE as usize] != 0,
            front,
            back,
        }
    }

    // ── Rasterizer accessors ─────────────────────────────────────────────

    /// Read rasterizer state.
    pub fn rasterizer_info(&self) -> RasterizerInfo {
        RasterizerInfo {
            cull_enable: self.regs[CULL_TEST_ENABLE as usize] != 0,
            front_face: FrontFace::from_raw(self.regs[FRONT_FACE as usize]),
            cull_face: CullFace::from_raw(self.regs[CULL_FACE as usize]),
            polygon_mode_front: PolygonMode::from_raw(self.regs[POLYGON_MODE_FRONT as usize]),
            polygon_mode_back: PolygonMode::from_raw(self.regs[POLYGON_MODE_BACK as usize]),
            fill_via_triangle_mode: FillViaTriangleMode::from_raw(
                self.regs[FILL_VIA_TRIANGLE_MODE as usize],
            ),
            line_width_smooth: f32::from_bits(self.regs[LINE_WIDTH_SMOOTH as usize]),
            line_width_aliased: f32::from_bits(self.regs[LINE_WIDTH_ALIASED as usize]),
            polygon_offset_point_enable: self.regs[POLYGON_OFFSET_POINT_ENABLE as usize] != 0,
            polygon_offset_line_enable: self.regs[POLYGON_OFFSET_LINE_ENABLE as usize] != 0,
            polygon_offset_fill_enable: self.regs[POLYGON_OFFSET_FILL_ENABLE as usize] != 0,
            depth_bias: f32::from_bits(self.regs[DEPTH_BIAS as usize]),
            slope_scale_depth_bias: f32::from_bits(self.regs[SLOPE_SCALE_DEPTH_BIAS as usize]),
            depth_bias_clamp: f32::from_bits(self.regs[DEPTH_BIAS_CLAMP as usize]),
        }
    }

    /// Upstream `maxwell3d->regs.primitive_restart`.
    pub fn primitive_restart_info(&self) -> PrimitiveRestartInfo {
        let base = PRIMITIVE_RESTART_BASE as usize;
        PrimitiveRestartInfo {
            enabled: self.regs[base] != 0,
            index: self.regs[base + 1],
        }
    }

    /// Upstream `maxwell3d->regs.logic_op`.
    pub fn logic_op_info(&self) -> LogicOpInfo {
        let base = LOGIC_OP as usize;
        LogicOpInfo {
            enabled: self.regs[base] != 0,
            op: self.regs[base + 1],
        }
    }

    /// Upstream `maxwell3d->regs.frag_color_clamp.AnyEnabled()`.
    pub fn frag_color_clamp_any_enabled(&self) -> bool {
        let raw = self.regs[FRAG_COLOR_CLAMP as usize];
        (0..8).any(|index| ((raw >> (index * 4)) & 1) != 0)
    }

    /// Upstream `maxwell3d->regs.anti_alias_alpha_control`.
    pub fn anti_alias_alpha_control_info(&self) -> AntiAliasAlphaControlInfo {
        let raw = self.regs[ANTI_ALIAS_ALPHA_CONTROL as usize];
        AntiAliasAlphaControlInfo {
            alpha_to_coverage: (raw & 0x1) != 0,
            alpha_to_one: ((raw >> 4) & 0x1) != 0,
        }
    }

    /// Upstream point-size registers read by `SyncPointState`.
    pub fn point_state_info(&self) -> PointStateInfo {
        PointStateInfo {
            point_sprite_enable: self.regs[POINT_SPRITE_ENABLE as usize] != 0,
            point_size_attribute_enabled: (self.regs[POINT_SIZE_ATTRIBUTE as usize] & 0x1) != 0,
            point_size: f32::from_bits(self.regs[POINT_SIZE as usize]),
        }
    }

    /// Upstream line-width registers read by `SyncLineState`.
    pub fn line_state_info(&self) -> LineStateInfo {
        LineStateInfo {
            line_anti_alias_enable: self.regs[LINE_ANTI_ALIAS_ENABLE as usize] != 0,
            line_width_smooth: f32::from_bits(self.regs[LINE_WIDTH_SMOOTH as usize]),
            line_width_aliased: f32::from_bits(self.regs[LINE_WIDTH_ALIASED as usize]),
        }
    }

    pub fn line_stipple_info(&self) -> LineStippleInfo {
        let raw = self.regs[LINE_STIPPLE_PARAMS as usize];
        LineStippleInfo {
            enabled: self.regs[LINE_STIPPLE_ENABLE as usize] != 0,
            factor: raw & 0xff,
            pattern: (raw >> 8) & 0xffff,
        }
    }

    /// Upstream `SyncDepthClamp` interpretation of
    /// `regs.viewport_clip_control.geometry_clip`.
    pub fn depth_clamp_enabled(&self) -> bool {
        let geometry_clip = (self.regs[VIEWPORT_CLIP_CONTROL as usize] >> 11) & 0x7;
        !matches!(geometry_clip, 1 | 3 | 5)
    }

    /// Upstream `regs.conservative_raster_enable != 0`, consumed by
    /// `FixedPipelineState::Refresh`.
    pub fn conservative_raster_enable(&self) -> bool {
        self.regs[CONSERVATIVE_RASTER_ENABLE as usize] != 0
    }

    /// Upstream `regs.provoking_vertex == ProvokingVertex::Last`.
    pub fn provoking_vertex_last(&self) -> bool {
        self.regs[PROVOKING_VERTEX as usize] == 1
    }

    /// Upstream `regs.depth_bounds_enable != 0`.
    pub fn depth_bounds_enable(&self) -> bool {
        self.regs[DEPTH_BOUNDS_ENABLE as usize] != 0
    }

    /// Upstream `regs.depth_bounds`.
    pub fn depth_bounds(&self) -> [f32; 2] {
        [
            f32::from_bits(self.regs[DEPTH_BOUNDS_BASE as usize]),
            f32::from_bits(self.regs[DEPTH_BOUNDS_BASE as usize + 1]),
        ]
    }

    // ── Shader program accessors ─────────────────────────────────────────

    /// Read the shader program region base address.
    pub fn program_base_address(&self) -> u64 {
        let base = PROGRAM_REGION_BASE as usize;
        let high = self.regs[base] as u64;
        let low = self.regs[base + 1] as u64;
        (high << 32) | low
    }

    // ── Texture/Sampler pool accessors ────────────────────────────────────

    /// GPU address of the texture header (TIC) pool.
    pub fn tex_header_pool_address(&self) -> u64 {
        let base = TEX_HEADER_POOL_BASE as usize;
        let high = self.regs[base] as u64;
        let low = self.regs[base + 1] as u64;
        (high << 32) | low
    }

    /// Maximum descriptor index in the texture header pool.
    pub fn tex_header_pool_limit(&self) -> u32 {
        self.regs[(TEX_HEADER_POOL_BASE + 2) as usize]
    }

    /// GPU address of the texture sampler (TSC) pool.
    pub fn tex_sampler_pool_address(&self) -> u64 {
        let base = TEX_SAMPLER_POOL_BASE as usize;
        let high = self.regs[base] as u64;
        let low = self.regs[base + 1] as u64;
        (high << 32) | low
    }

    /// Maximum descriptor index in the texture sampler pool.
    pub fn tex_sampler_pool_limit(&self) -> u32 {
        self.regs[(TEX_SAMPLER_POOL_BASE + 2) as usize]
    }

    // ── Descriptor table methods ─────────────────────────────────────────

    /// Port of upstream `Maxwell3D::GetTICEntry`.
    #[allow(dead_code)]
    fn get_tic_entry(&self, index: u32) -> TicEntry {
        let tic_address_gpu = self
            .tex_header_pool_address()
            .wrapping_add((index as u64).wrapping_mul(std::mem::size_of::<TicEntry>() as u64));
        let mut tic_entry = TicEntry::default();
        let tic_bytes = unsafe {
            std::slice::from_raw_parts_mut(
                (&mut tic_entry as *mut TicEntry).cast::<u8>(),
                std::mem::size_of::<TicEntry>(),
            )
        };
        let memory_manager = self
            .memory_manager
            .as_ref()
            .expect("Maxwell3D::get_tic_entry requires a MemoryManager owner");
        memory_manager
            .lock()
            .read_block_unsafe(tic_address_gpu, tic_bytes);
        tic_entry
    }

    /// Port of upstream `Maxwell3D::GetTSCEntry`.
    #[allow(dead_code)]
    fn get_tsc_entry(&self, index: u32) -> TscEntry {
        let tsc_address_gpu = self
            .tex_sampler_pool_address()
            .wrapping_add((index as u64).wrapping_mul(std::mem::size_of::<TscEntry>() as u64));
        let mut tsc_entry = TscEntry::default();
        let tsc_bytes = unsafe {
            std::slice::from_raw_parts_mut(
                (&mut tsc_entry as *mut TscEntry).cast::<u8>(),
                std::mem::size_of::<TscEntry>(),
            )
        };
        let memory_manager = self
            .memory_manager
            .as_ref()
            .expect("Maxwell3D::get_tsc_entry requires a MemoryManager owner");
        memory_manager
            .lock()
            .read_block_unsafe(tsc_address_gpu, tsc_bytes);
        tsc_entry
    }

    /// Decode a texture handle into `(tic_id, tsc_id)` based on sampler
    /// binding mode.
    pub fn decode_texture_handle(&self, handle: u32) -> (u32, u32) {
        let linked = self.regs[SAMPLER_BINDING as usize] == SamplerBinding::ViaHeaderBinding as u32;
        if linked {
            // Same index for both TIC and TSC.
            (handle, handle)
        } else {
            // Independent: bits[19:0] = tic_id, bits[31:20] = tsc_id.
            let tic_id = handle & 0xF_FFFF; // 20 bits
            let tsc_id = (handle >> 20) & 0xFFF; // 12 bits
            (tic_id, tsc_id)
        }
    }

    // ── Vertex attribute accessors ────────────────────────────────────────

    /// Raw `regs.vertex_attrib_format[index]` word.
    #[inline(always)]
    pub fn vertex_attrib_raw(&self, index: u32) -> u32 {
        self.regs[(VERTEX_ATTRIB_BASE + index) as usize]
    }

    /// Read vertex attribute info for `index` (0..31).
    pub fn vertex_attrib_info(&self, index: u32) -> VertexAttribInfo {
        let raw = self.vertex_attrib_raw(index);
        VertexAttribInfo {
            buffer_index: raw & 0x1F,
            constant: (raw & (1 << 6)) != 0,
            offset: (raw >> 7) & 0x3FFF,
            size: VertexAttribSize::from_raw((raw >> 21) & 0x3F),
            attrib_type: VertexAttribType::from_raw((raw >> 27) & 0x7),
            bgra: (raw & (1 << 31)) != 0,
        }
    }

    // ── Shader pipeline accessors ────────────────────────────────────────

    /// Read shader stage info for pipeline slot `index` (0..5).
    pub fn shader_stage_info(&self, index: u32) -> ShaderStageInfo {
        let base = (PIPELINE_BASE + index * PIPELINE_STRIDE) as usize;
        let word0 = self.regs[base];
        let enabled = self.is_shader_stage_enabled(index);
        let program_type = ShaderStageType::from_raw((word0 >> 4) & 0xF);
        ShaderStageInfo {
            enabled,
            program_type,
            offset: self.regs[base + 1],
            register_count: self.regs[base + 3],
            binding_group: self.regs[base + 4],
        }
    }

    /// Whether shader stage `index` is enabled.
    /// VertexB (index 1) always returns true — the GPU requires it.
    pub fn is_shader_stage_enabled(&self, index: u32) -> bool {
        if index == 1 {
            return true;
        }
        let base = (PIPELINE_BASE + index * PIPELINE_STRIDE) as usize;
        (self.regs[base] & 1) != 0
    }

    /// Port of upstream `regs.IsShaderConfigEnabled(ShaderType)`.
    pub fn shader_config_enabled(&self, stage: ShaderStageType) -> bool {
        stage
            .as_index()
            .is_some_and(|index| self.is_shader_stage_enabled(index))
    }

    /// GPU virtual base address of the shader program region.
    ///
    /// Upstream: `Maxwell3D::Regs::ProgramRegion::Address()` —
    /// `(address_high << 32) | address_low`.
    pub fn program_region_address(&self) -> u64 {
        let high = self.regs[PROGRAM_REGION_HIGH as usize] as u64;
        let low = self.regs[PROGRAM_REGION_LOW as usize] as u64;
        (high << 32) | low
    }

    /// Return the GPU virtual address of the entry point for each of the
    /// 6 Maxwell shader stages. Disabled stages report `0`.
    ///
    /// Upstream rasterizers compute these inline as
    /// `regs.program_region.Address() + regs.pipelines[i].offset`. ruzu's
    /// live draw view calls this through `Maxwell3DAccess` instead of storing
    /// these values in `DrawState`.
    pub fn shader_program_addresses(&self) -> [u64; 6] {
        let base = self.program_region_address();
        let mut out = [0u64; 6];
        for i in 0..6u32 {
            if !self.is_shader_stage_enabled(i) {
                continue;
            }
            let info = self.shader_stage_info(i);
            out[i as usize] = base + info.offset as u64;
        }
        out
    }

    /// Consume `dirty.flags[VideoCommon::Dirty::Shaders]`.
    ///
    /// Upstream owner: `ShaderCache::RefreshStages` reads and clears
    /// `maxwell3d->dirty.flags[VideoCommon::Dirty::Shaders]` before
    /// rebuilding stage hashes.
    pub(crate) fn consume_dirty_shaders(&mut self) -> bool {
        let dirty = &mut self.dirty.flags[dirty_flags::flags::SHADERS as usize];
        if !*dirty {
            return false;
        }
        *dirty = false;
        true
    }

    // ── Color mask accessors ─────────────────────────────────────────────

    /// Read color write mask for render target `rt` (0..7).
    /// If COLOR_MASK_COMMON is set, all RTs share mask[0].
    pub fn color_mask_info(&self, rt: usize) -> ColorMaskInfo {
        let effective_rt = if self.color_mask_common() { 0 } else { rt };
        let raw = self.regs[(COLOR_MASK_BASE + effective_rt as u32) as usize];
        ColorMaskInfo {
            r: (raw & (1 << 0)) != 0,
            g: (raw & (1 << 4)) != 0,
            b: (raw & (1 << 8)) != 0,
            a: (raw & (1 << 12)) != 0,
        }
    }

    pub fn color_mask_common(&self) -> bool {
        self.regs[COLOR_MASK_COMMON as usize] != 0
    }

    // ── Render target control accessors ──────────────────────────────────

    /// Read render target control: count and per-RT target mapping.
    pub fn rt_control_info(&self) -> RtControlInfo {
        let raw = self.regs[RT_CONTROL as usize];
        let count = raw & 0xF;
        let mut map = [0u32; 8];
        for i in 0..8 {
            map[i] = (raw >> (4 + i * 3)) & 0x7;
        }
        RtControlInfo { count, map }
    }

    // ── Constant buffer accessors ────────────────────────────────────────

    /// Read constant buffer bindings for a shader stage (0..4).
    pub fn const_buffer_bindings(&self, stage: usize) -> &[ConstBufferInfo; MAX_CB_SLOTS] {
        &self.cb_bindings[stage]
    }

    // ── Draw call accessors ──────────────────────────────────────────────

    /// Drain accumulated draw call records.
    #[cfg(test)]
    pub fn take_draw_calls(&mut self) -> Vec<DrawCall> {
        self.with_draw_manager(|draw_manager, _| draw_manager.take_compat_draw_calls())
    }

    #[cfg(not(test))]
    pub fn take_draw_calls(&mut self) -> Vec<DrawCall> {
        Vec::new()
    }

    // ── Instance / base vertex accessors ─────────────────────────────────

    /// Base vertex index (signed) from GLOBAL_BASE_VERTEX_INDEX.
    pub fn base_vertex(&self) -> i32 {
        self.regs[GLOBAL_BASE_VERTEX_INDEX as usize] as i32
    }

    /// Base instance index from GLOBAL_BASE_INSTANCE_INDEX.
    pub fn base_instance(&self) -> u32 {
        self.regs[GLOBAL_BASE_INSTANCE_INDEX as usize]
    }

    // ── Report semaphore accessors ───────────────────────────────────────

    /// Report semaphore GPU virtual address (high << 32 | low).
    pub fn report_semaphore_address(&self) -> u64 {
        let high = self.regs[REPORT_SEMAPHORE_BASE as usize] as u64;
        let low = self.regs[(REPORT_SEMAPHORE_BASE + 1) as usize] as u64;
        (high << 32) | low
    }

    /// Report semaphore payload value.
    pub fn report_semaphore_payload(&self) -> u32 {
        self.regs[(REPORT_SEMAPHORE_BASE + 2) as usize]
    }
}

impl RenderConditionStateSource for Maxwell3D {
    fn render_condition_state(&self) -> RenderConditionState {
        let comparison_mode = match self.regs[RENDER_ENABLE_MODE as usize] {
            0 => ComparisonMode::False,
            1 => ComparisonMode::True,
            2 => ComparisonMode::Conditional,
            3 => ComparisonMode::IfEqual,
            4 => ComparisonMode::IfNotEqual,
            _ => ComparisonMode::False,
        };
        let address = ((self.regs[RENDER_ENABLE_BASE as usize] as u64) << 32)
            | self.regs[(RENDER_ENABLE_BASE + 1) as usize] as u64;
        RenderConditionState {
            override_mode: self.regs[RENDER_ENABLE_OVERRIDE as usize],
            comparison_mode,
            address,
        }
    }
}

#[cfg(test)]
impl Default for Maxwell3D {
    fn default() -> Self {
        Self::new()
    }
}

impl dm::Maxwell3DAccess for Maxwell3D {
    fn should_execute(&self) -> bool {
        self.should_execute()
    }

    fn global_base_instance_index(&self) -> u32 {
        self.base_instance()
    }

    fn global_base_vertex_index(&self) -> u32 {
        self.base_vertex() as u32
    }

    fn index_buffer(&self) -> dm::IndexBuffer {
        dm::IndexBuffer {
            first: self.index_buffer_first(),
            count: self.index_buffer_count(),
            format: self.index_buffer_format(),
        }
    }

    fn vertex_buffer(&self) -> dm::VertexBuffer {
        dm::VertexBuffer {
            first: self.regs[VB_FIRST as usize],
            count: self.regs[VB_COUNT as usize],
        }
    }

    fn primitive_topology_control(&self) -> dm::PrimitiveTopologyControl {
        dm::PrimitiveTopologyControl::from_raw(self.regs[PRIMITIVE_TOPOLOGY_CONTROL as usize])
    }

    fn topology_override(&self) -> dm::PrimitiveTopologyOverride {
        dm::PrimitiveTopologyOverride::from_raw(self.regs[TOPOLOGY_OVERRIDE as usize])
    }

    fn topology_override_raw(&self) -> u32 {
        self.regs[TOPOLOGY_OVERRIDE as usize]
    }

    fn draw_topology(&self) -> PrimitiveTopology {
        PrimitiveTopology::from_raw(self.regs[DRAW_BEGIN as usize])
    }

    fn draw_instance_id(&self) -> (bool, bool) {
        let instance_id = InstanceId::from_raw(self.regs[DRAW_BEGIN as usize]);
        (
            instance_id == InstanceId::First,
            instance_id == InstanceId::Subsequent,
        )
    }

    fn inline_index_2x16_values(&self) -> (u32, u32) {
        let raw = self.regs[INLINE_INDEX_2X16_EVEN as usize];
        (raw & 0xFFFF, (raw >> 16) & 0xFFFF)
    }

    fn inline_index_4x8_values(&self) -> [u32; 4] {
        let raw = self.regs[INLINE_INDEX_4X8_INDEX0 as usize];
        [
            raw & 0xFF,
            (raw >> 8) & 0xFF,
            (raw >> 16) & 0xFF,
            (raw >> 24) & 0xFF,
        ]
    }

    fn vertex_array_instance_first_params(&self, argument: u32) -> (PrimitiveTopology, u32, u32) {
        let params = VertexArrayParams::from_raw(argument);
        (params.topology, params.start, params.count)
    }

    fn vertex_array_instance_subsequent_params(
        &self,
        argument: u32,
    ) -> (PrimitiveTopology, u32, u32) {
        let params = VertexArrayParams::from_raw(argument);
        (params.topology, params.start, params.count)
    }

    fn set_dirty_flag(&mut self, index: u8) {
        self.dirty.flags[index as usize] = true;
    }

    fn dirty_flags(&self) -> &[bool; 256] {
        &self.dirty.flags
    }

    fn dirty_flag(&self, index: u8) -> bool {
        self.dirty.flags[index as usize]
    }

    fn clear_dirty_flag(&mut self, index: u8) {
        self.dirty.flags[index as usize] = false;
    }

    fn set_logic_op_enabled(&mut self, enabled: bool) {
        self.regs[LOGIC_OP as usize] = u32::from(enabled);
    }

    fn dirty_flags_ptr(&mut self) -> Option<std::ptr::NonNull<[bool; 256]>> {
        Some(std::ptr::NonNull::from(&mut self.dirty.flags))
    }

    fn with_rasterizer_mut(&mut self, f: &mut dyn FnMut(&mut dyn RasterizerInterface)) -> bool {
        Maxwell3D::with_rasterizer_mut(self, |rasterizer| f(rasterizer)).is_some()
    }

    fn draw_rasterizer(
        &mut self,
        draw_state: &dm::DrawState,
        draw_indexed: bool,
        instance_count: u32,
    ) -> bool {
        let Some(handle) = self.rasterizer else {
            return false;
        };
        self.with_active_draw_manager_state(draw_state, |this| unsafe {
            handle.with_mut(|rasterizer| {
                rasterizer.draw(
                    dm::Maxwell3DDrawView::live(draw_state, draw_indexed, this),
                    instance_count,
                );
            })
        });
        true
    }

    fn draw_indirect_rasterizer(
        &mut self,
        draw_state: &dm::DrawState,
        indirect_params: &dm::IndirectParams,
    ) -> bool {
        let Some(handle) = self.rasterizer else {
            return false;
        };
        self.with_active_draw_manager_state(draw_state, |this| unsafe {
            handle.with_mut(|rasterizer| {
                rasterizer.draw_indirect(dm::Maxwell3DIndirectView::live(
                    draw_state,
                    indirect_params,
                    this,
                ));
            })
        });
        true
    }

    fn draw_texture_rasterizer(
        &mut self,
        draw_state: &dm::DrawState,
        draw_texture_state: dm::DrawTextureState,
    ) -> bool {
        let Some(handle) = self.rasterizer else {
            return false;
        };
        self.with_active_draw_manager_state(draw_state, |this| unsafe {
            handle.with_mut(|rasterizer| {
                rasterizer.draw_texture(dm::Maxwell3DDrawTextureView::live(
                    draw_state,
                    draw_texture_state,
                    this,
                ));
            })
        });
        true
    }

    fn clear_rasterizer(&mut self, layer_count: u32) -> bool {
        let Some(handle) = self.rasterizer else {
            return false;
        };
        unsafe {
            handle.with_mut(|rasterizer| {
                rasterizer.clear(dm::Maxwell3DClearView::live(self), layer_count);
            });
        }
        true
    }

    fn shader_program_addresses(&self) -> [u64; 6] {
        self.shader_program_addresses()
    }

    fn index_buffer_addr(&self) -> u64 {
        self.index_buffer_addr()
    }

    fn index_buffer_addr_end(&self) -> u64 {
        let base = IB_BASE as usize;
        let high = self.regs[base + IB_OFF_LIMIT_HIGH as usize] as u64;
        let low = self.regs[base + IB_OFF_LIMIT_LOW as usize] as u64;
        (high << 32) | low
    }

    fn draw_texture_params(&self) -> dm::DrawTextureParams {
        let base = DRAW_TEXTURE_BASE as usize;
        let dx_du = ((self.regs[base + DRAW_TEXTURE_DX_DU_HIGH_OFFSET] as u64) << 32)
            | self.regs[base + DRAW_TEXTURE_DX_DU_LOW_OFFSET] as u64;
        let dy_dv = ((self.regs[base + DRAW_TEXTURE_DY_DV_HIGH_OFFSET] as u64) << 32)
            | self.regs[base + DRAW_TEXTURE_DY_DV_LOW_OFFSET] as u64;
        dm::DrawTextureParams {
            dst_x0: self.regs[base + DRAW_TEXTURE_DST_X0_OFFSET] as i32,
            dst_y0: self.regs[base + DRAW_TEXTURE_DST_Y0_OFFSET] as i32,
            dst_width: self.regs[base + DRAW_TEXTURE_DST_WIDTH_OFFSET] as i32,
            dst_height: self.regs[base + DRAW_TEXTURE_DST_HEIGHT_OFFSET] as i32,
            dx_du: dx_du as i64,
            dy_dv: dy_dv as i64,
            src_sampler: self.regs[base + DRAW_TEXTURE_SRC_SAMPLER_OFFSET],
            src_texture: self.regs[base + DRAW_TEXTURE_SRC_TEXTURE_OFFSET],
            src_x0: self.regs[base + DRAW_TEXTURE_SRC_X0_OFFSET] as i32,
            src_y0: self.regs[base + DRAW_TEXTURE_SRC_Y0_OFFSET] as i32,
        }
    }

    fn window_origin_lower_left(&self) -> bool {
        (self.regs[WINDOW_ORIGIN as usize] & 1) != 0
    }

    fn window_origin_flip_y(&self) -> bool {
        ((self.regs[WINDOW_ORIGIN as usize] >> 4) & 1) != 0
    }

    fn viewport_transform_scale_y(&self, index: u32) -> f32 {
        let base = (VP_TRANSFORM_BASE + index * VP_TRANSFORM_STRIDE) as usize;
        f32::from_bits(self.regs[base + 1])
    }

    fn viewport_transform_info(&self, index: u32) -> ViewportTransformInfo {
        self.viewport_transform_info(index)
    }

    fn viewport_scale_offset_enabled(&self) -> bool {
        self.viewport_transform_state() != 0
    }

    fn surface_clip_info(&self) -> SurfaceClipInfo {
        self.surface_clip_info()
    }

    fn framebuffer_srgb(&self) -> bool {
        self.regs[FRAMEBUFFER_SRGB as usize] != 0
    }

    fn user_clip_enable_raw(&self) -> u32 {
        self.regs[USER_CLIP_ENABLE as usize]
    }

    fn surface_clip_height(&self) -> u32 {
        (self.regs[SURFACE_CLIP_BASE as usize + SURFACE_CLIP_HEIGHT_OFFSET] >> 16) & 0xFFFF
    }

    fn clear_surface_flags(&self) -> u32 {
        self.regs[CLEAR_SURFACE as usize]
    }

    fn clear_control_use_scissor(&self) -> bool {
        ((self.regs[CLEAR_CONTROL as usize] >> 8) & 1) != 0
    }

    fn clear_control_use_viewport_clip0(&self) -> bool {
        ((self.regs[CLEAR_CONTROL as usize] >> 12) & 1) != 0
    }

    fn rt_address(&self, index: usize) -> u64 {
        self.rt_address(index)
    }

    fn rt_width(&self, index: usize) -> u32 {
        self.rt_width(index)
    }

    fn rt_height(&self, index: usize) -> u32 {
        self.rt_height(index)
    }

    fn rt_format(&self, index: usize) -> u32 {
        self.rt_format(index)
    }

    fn rt_info(&self, index: usize) -> RenderTargetInfo {
        self.rt_info(index)
    }

    fn clear_color_rgba(&self) -> [f32; 4] {
        self.clear_color_rgba()
    }

    fn clear_depth(&self) -> f32 {
        f32::from_bits(self.regs[CLEAR_DEPTH as usize])
    }

    fn clear_stencil(&self) -> i32 {
        self.regs[CLEAR_STENCIL as usize] as i32
    }

    fn vertex_stream_info(&self, index: u32) -> VertexStreamInfo {
        self.vertex_stream_info(index)
    }

    fn vertex_stream_instance(&self, index: u32) -> u32 {
        self.vertex_stream_instance(index)
    }

    fn vertex_stream_limit(&self, index: u32) -> dm::VertexStreamLimit {
        self.vertex_stream_limit(index)
    }

    fn viewport_info(&self, index: u32) -> ViewportInfo {
        self.viewport_info(index)
    }

    fn scissor_info(&self, index: u32) -> ScissorInfo {
        self.scissor_info(index)
    }

    fn effective_blend_info(&self, rt: usize) -> BlendInfo {
        self.effective_blend_info(rt)
    }

    fn blend_per_target_enabled(&self) -> bool {
        self.blend_per_target_enabled()
    }

    fn iterated_blend_enabled(&self) -> bool {
        self.iterated_blend_enabled()
    }

    fn global_blend_info(&self, rt: usize) -> BlendInfo {
        self.global_blend_info(rt)
    }

    fn blend_color_info(&self) -> BlendColorInfo {
        self.blend_color_info()
    }

    fn depth_stencil_info(&self) -> DepthStencilInfo {
        self.depth_stencil_info()
    }

    fn rasterizer_info(&self) -> RasterizerInfo {
        self.rasterizer_info()
    }

    fn rasterize_enable(&self) -> bool {
        self.rasterize_enable()
    }

    fn primitive_restart_info(&self) -> PrimitiveRestartInfo {
        self.primitive_restart_info()
    }

    fn logic_op_info(&self) -> LogicOpInfo {
        self.logic_op_info()
    }

    fn frag_color_clamp_any_enabled(&self) -> bool {
        self.frag_color_clamp_any_enabled()
    }

    fn anti_alias_alpha_control_info(&self) -> AntiAliasAlphaControlInfo {
        self.anti_alias_alpha_control_info()
    }

    fn point_state_info(&self) -> PointStateInfo {
        self.point_state_info()
    }

    fn line_state_info(&self) -> LineStateInfo {
        self.line_state_info()
    }

    fn line_stipple_info(&self) -> LineStippleInfo {
        self.line_stipple_info()
    }

    fn depth_clamp_enabled(&self) -> bool {
        self.depth_clamp_enabled()
    }

    fn conservative_raster_enable(&self) -> bool {
        self.conservative_raster_enable()
    }

    fn engine_state(&self) -> EngineHint {
        self.engine_state()
    }

    fn provoking_vertex_last(&self) -> bool {
        self.provoking_vertex_last()
    }

    fn depth_bounds_enable(&self) -> bool {
        self.depth_bounds_enable()
    }

    fn depth_bounds(&self) -> [f32; 2] {
        self.depth_bounds()
    }

    fn mandated_early_z(&self) -> bool {
        self.mandated_early_z()
    }

    fn alpha_test_enabled(&self) -> bool {
        self.alpha_test_enabled()
    }

    fn alpha_test_func(&self) -> ComparisonOp {
        self.alpha_test_func()
    }

    fn alpha_test_ref(&self) -> f32 {
        self.alpha_test_ref()
    }

    fn tessellation_domain_type(&self) -> u32 {
        self.tessellation_domain_type()
    }

    fn tessellation_spacing(&self) -> u32 {
        self.tessellation_spacing()
    }

    fn tessellation_clockwise(&self) -> bool {
        self.tessellation_clockwise()
    }

    fn patch_vertices(&self) -> u32 {
        self.patch_vertices()
    }

    fn transform_feedback_enabled(&self) -> bool {
        self.transform_feedback_enabled()
    }

    fn transform_feedback_state(&self) -> TransformFeedbackState {
        self.transform_feedback_state()
    }

    fn shader_config_enabled(&self, stage: ShaderStageType) -> bool {
        self.shader_config_enabled(stage)
    }

    fn program_base_address(&self) -> u64 {
        self.program_base_address()
    }

    fn const_buffer_binding(&self, stage: usize, slot: usize) -> ConstBufferInfo {
        self.cb_bindings[stage][slot]
    }

    fn vertex_attrib_info(&self, index: u32) -> VertexAttribInfo {
        self.vertex_attrib_info(index)
    }

    fn shader_stage_info(&self, index: u32) -> ShaderStageInfo {
        self.shader_stage_info(index)
    }

    fn color_mask_info(&self, rt: usize) -> ColorMaskInfo {
        self.color_mask_info(rt)
    }

    fn color_mask_common(&self) -> bool {
        self.color_mask_common()
    }

    fn rt_control_info(&self) -> RtControlInfo {
        self.rt_control_info()
    }

    fn zeta_info(&self) -> ZetaInfo {
        self.zeta_info()
    }

    fn anti_alias_samples_mode(&self) -> u32 {
        self.anti_alias_samples_mode()
    }

    fn zpass_pixel_count_enabled(&self) -> bool {
        self.regs[ZPASS_PIXEL_COUNT_ENABLE as usize] != 0
    }

    fn tex_header_pool_address(&self) -> u64 {
        self.tex_header_pool_address()
    }

    fn tex_header_pool_limit(&self) -> u32 {
        self.tex_header_pool_limit()
    }

    fn tex_sampler_pool_address(&self) -> u64 {
        self.tex_sampler_pool_address()
    }

    fn tex_sampler_pool_limit(&self) -> u32 {
        self.tex_sampler_pool_limit()
    }

    fn sampler_binding(&self) -> SamplerBinding {
        if self.regs[SAMPLER_BINDING as usize] == 1 {
            SamplerBinding::ViaHeaderBinding
        } else {
            SamplerBinding::Independently
        }
    }
}

impl Maxwell3D {
    pub(crate) fn dirty_flags_mut(&mut self) -> &mut [bool; 256] {
        &mut self.dirty.flags
    }

    #[cfg(test)]
    pub(crate) fn dirty_tables(&self) -> &dirty_flags::DirtyTables {
        &self.dirty.tables
    }

    pub(crate) fn dirty_tables_mut(&mut self) -> &mut dirty_flags::DirtyTables {
        &mut self.dirty.tables
    }

    // ── Upstream-matching execution mask ─────────────────────────────────

    /// Determine whether a method triggers immediate execution (matching
    /// upstream `Maxwell3D::IsMethodExecutable`).
    fn is_method_executable(method: u32) -> bool {
        if method >= MACRO_REGISTERS_START {
            return true;
        }
        match method {
            DRAW_END | DRAW_BEGIN | VB_FIRST | VB_COUNT => true,
            m if m == IB_BASE + IB_OFF_FIRST => true,
            m if m == IB_BASE + IB_OFF_COUNT => true,
            DRAW_INLINE_INDEX => true,
            INDEX_BUFFER32_SUBSEQUENT | INDEX_BUFFER16_SUBSEQUENT | INDEX_BUFFER8_SUBSEQUENT => {
                true
            }
            INDEX_BUFFER32_FIRST | INDEX_BUFFER16_FIRST | INDEX_BUFFER8_FIRST => true,
            INLINE_INDEX_2X16_EVEN | INLINE_INDEX_4X8_INDEX0 => true,
            VERTEX_ARRAY_INSTANCE_FIRST | VERTEX_ARRAY_INSTANCE_SUBSEQUENT => true,
            DRAW_TEXTURE_SRC_Y0 => true,
            WAIT_FOR_IDLE | SHADOW_RAM_CONTROL => true,
            LOAD_MME_INSTRUCTION_PTR | LOAD_MME_INSTRUCTION | LOAD_MME_START_ADDR => true,
            FALCON4 => true,
            m if m >= CB_DATA_BASE && m < CB_DATA_END => true,
            CB_BIND_TRIGGER_0 | CB_BIND_TRIGGER_1 | CB_BIND_TRIGGER_2 | CB_BIND_TRIGGER_3
            | CB_BIND_TRIGGER_4 => true,
            TOPOLOGY_OVERRIDE | CLEAR_SURFACE => true,
            REPORT_SEMAPHORE_QUERY => true,
            RENDER_ENABLE_MODE | CLEAR_REPORT_VALUE | SYNC_INFO => true,
            LAUNCH_DMA | INLINE_DATA => true,
            FRAGMENT_BARRIER | INVALIDATE_TEXTURE_DATA_CACHE | TILED_CACHE_BARRIER => true,
            _ => false,
        }
    }

    // ── Shadow RAM processing (matching upstream ProcessShadowRam) ──────

    /// Process shadow RAM for a register write, returning the effective
    /// argument value. Matches upstream `Maxwell3D::ProcessShadowRam`.
    fn process_shadow_ram(&mut self, method: u32, argument: u32) -> u32 {
        let control = ShadowRamControl::from_raw(self.shadow_state[SHADOW_RAM_CONTROL as usize]);
        match control {
            ShadowRamControl::Track | ShadowRamControl::TrackWithFilter => {
                self.shadow_state[method as usize] = argument;
                argument
            }
            ShadowRamControl::Replay => self.shadow_state[method as usize],
            ShadowRamControl::Passthrough => argument,
        }
    }

    // ── Dirty register tracking (matching upstream ProcessDirtyRegisters) ─

    /// Update the register value and mark every owner table dirty. Matches
    /// upstream `Maxwell3D::ProcessDirtyRegisters`, which deliberately marks
    /// state dirty even when the guest re-emits the same register value.
    fn process_dirty_registers(&mut self, method: u32, argument: u32) {
        let idx = method as usize;
        if idx >= ENGINE_REG_COUNT {
            return;
        }

        self.regs[idx] = argument;

        for table in &self.dirty.tables {
            let dirty_index = table[idx] as usize;
            if dirty_index < self.dirty.flags.len() {
                self.dirty.flags[dirty_index] = true;
            }
        }
    }

    // ── Method call dispatch (matching upstream ProcessMethodCall) ───────

    /// Dispatch a method call with side effects. Matches upstream
    /// `Maxwell3D::ProcessMethodCall`.
    fn process_method_call(
        &mut self,
        method: u32,
        argument: u32,
        nonshadow_argument: u32,
        is_last_call: bool,
    ) {
        match method {
            WAIT_FOR_IDLE => {
                let _ = self.with_rasterizer_mut(|rasterizer| rasterizer.wait_for_idle());
            }
            SHADOW_RAM_CONTROL => {
                self.shadow_state[SHADOW_RAM_CONTROL as usize] = nonshadow_argument;
            }
            LOAD_MME_INSTRUCTION_PTR => {
                let ptr = self.regs[LOAD_MME_INSTRUCTION_PTR as usize];
                self.macro_engine.clear_code(ptr);
            }
            LOAD_MME_INSTRUCTION => {
                let ptr = self.regs[LOAD_MME_INSTRUCTION_PTR as usize];
                self.macro_engine.add_code(ptr, argument);
            }
            LOAD_MME_START_ADDR => {
                self.process_macro_bind(argument);
            }
            FALCON4 => {
                self.process_firmware_call4();
            }
            m if m >= CB_DATA_BASE && m < CB_DATA_END => {
                self.process_cb_data(argument);
            }
            CB_BIND_TRIGGER_0 => self.process_cb_bind(0),
            CB_BIND_TRIGGER_1 => self.process_cb_bind(1),
            CB_BIND_TRIGGER_2 => self.process_cb_bind(2),
            CB_BIND_TRIGGER_3 => self.process_cb_bind(3),
            CB_BIND_TRIGGER_4 => self.process_cb_bind(4),
            REPORT_SEMAPHORE_QUERY => {
                self.process_query_get();
            }
            RENDER_ENABLE_MODE => {
                self.process_query_condition();
            }
            CLEAR_REPORT_VALUE => {
                self.process_counter_reset();
            }
            SYNC_INFO => {
                self.process_sync_point();
            }
            LAUNCH_DMA => {
                let regs = self.upload_registers();
                self.upload_state
                    .process_exec(&regs, self.launch_dma_is_linear());
            }
            INLINE_DATA => {
                self.process_inline_upload_word(argument, is_last_call);
            }
            FRAGMENT_BARRIER => {
                let _ = self.with_rasterizer_mut(|rasterizer| rasterizer.fragment_barrier());
            }
            INVALIDATE_TEXTURE_DATA_CACHE => {
                let _ = self.with_rasterizer_mut(|rasterizer| {
                    rasterizer.invalidate_gpu_cache();
                    rasterizer.wait_for_idle();
                });
            }
            TILED_CACHE_BARRIER => {
                let _ = self.with_rasterizer_mut(|rasterizer| rasterizer.tiled_cache_barrier());
            }
            _ => {
                self.with_draw_manager(|draw_manager, this| {
                    draw_manager.process_method_call(method, argument, this);
                });
            }
        }
    }

    // ── Upstream ProcessCBData / ProcessCBMultiData / ProcessCBBind ──────

    /// Handle CB_DATA write: write to const buffer at current offset and
    /// auto-increment offset by 4. Matches upstream `ProcessCBData`.
    fn process_cb_data(&mut self, value: u32) {
        self.process_cb_multi_data(&[value]);
    }

    /// Batch write to const buffer. Matches upstream `ProcessCBMultiData`.
    pub(crate) fn process_cb_multi_data(&mut self, data: &[u32]) {
        let cb_base = CB_CONFIG_BASE as usize;
        let addr_high = self.regs[cb_base + 1] as u64;
        let addr_low = self.regs[cb_base + 2] as u64;
        let buffer_address = (addr_high << 32) | addr_low;

        assert_ne!(buffer_address, 0);

        let offset = self.regs[cb_base + 3];
        let size = self.regs[cb_base];
        assert!(offset <= size);

        let copy_size = data.len() as u32 * 4;
        let address = buffer_address.wrapping_add(offset as u64);

        if let Some(memory_manager) = self.memory_manager.as_ref().cloned() {
            let mut bytes = Vec::with_capacity(copy_size as usize);
            for value in data {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            let mut memory_manager = memory_manager.lock();
            memory_manager.write_block_cached(address, &bytes);
        }

        // Increment the current buffer position.
        self.regs[cb_base + 3] = offset.wrapping_add(copy_size);
    }

    /// Handle CB_BIND trigger for a shader stage. Matches upstream `ProcessCBBind`.
    pub(crate) fn process_cb_bind(&mut self, stage_index: usize) {
        let bind_base = (CB_BIND_BASE + stage_index as u32 * CB_BIND_STRIDE) as usize;
        let raw_config = self.regs[bind_base + 4];

        let valid = (raw_config & 1) != 0;
        let slot = ((raw_config >> 4) & 0x1F) as usize;

        if slot >= MAX_CB_SLOTS {
            log::warn!(
                "Maxwell3D: CB_BIND stage {} slot {} out of range",
                stage_index,
                slot
            );
            return;
        }

        let cb_base = CB_CONFIG_BASE as usize;
        let size = self.regs[cb_base];
        let addr_high = self.regs[cb_base + 1] as u64;
        let addr_low = self.regs[cb_base + 2] as u64;
        let address = (addr_high << 32) | addr_low;
        self.cb_bindings[stage_index][slot] = ConstBufferInfo {
            address,
            size,
            enabled: valid,
        };

        if valid {
            log::trace!(
                "Maxwell3D: CB_BIND stage={} slot={} addr=0x{:X} size={}",
                stage_index,
                slot,
                address,
                size
            );
            let _ = self.with_rasterizer_mut(|rasterizer| {
                rasterizer.bind_graphics_uniform_buffer(stage_index, slot as u32, address, size)
            });
        } else {
            log::trace!(
                "Maxwell3D: CB_BIND stage={} slot={} disabled",
                stage_index,
                slot
            );
            let _ = self.with_rasterizer_mut(|rasterizer| {
                rasterizer.disable_graphics_uniform_buffer(stage_index, slot as u32)
            });
        }
    }

    // ── Upstream ProcessMacroBind / ProcessFirmwareCall4 ─────────────────

    /// Bind a macro start address. Matches upstream `ProcessMacroBind`.
    fn process_macro_bind(&mut self, data: u32) {
        let ptr = self.regs[LOAD_MME_START_ADDR_PTR as usize];
        self.macro_positions[ptr as usize] = data;
        self.regs[LOAD_MME_START_ADDR_PTR as usize] = ptr + 1;
        log::info!(
            "Maxwell3D::process_macro_bind slot={} start=0x{:X}",
            ptr,
            data
        );
    }

    /// Handle firmware call 4. Matches upstream `ProcessFirmwareCall4`.
    fn process_firmware_call4(&mut self) {
        // Firmware call 4 changes some registers depending on its parameters.
        // These registers don't affect emulation, so set shadow_scratch[0] = 1.
        self.regs[SHADOW_SCRATCH_BASE as usize] = 1;
    }

    pub(crate) fn max_current_vertices(&self) -> u32 {
        let mut num_vertices = 0u32;
        for index in 0..32u32 {
            let array = self.vertex_stream_info(index);
            if !array.enabled {
                continue;
            }

            let attribute = self.vertex_attrib_info(index);
            if attribute.constant {
                num_vertices = num_vertices.max(1);
                continue;
            }

            let limit = self.vertex_stream_limit(index);
            let gpu_addr_begin = array.address;
            let gpu_addr_end = limit.address.saturating_add(1);
            let address_size = gpu_addr_end.saturating_sub(gpu_addr_begin) as u32;
            let vertex_stride = attribute.size.size_bytes().max(array.stride).max(1);
            num_vertices = num_vertices.max(address_size / vertex_stride);
            break;
        }
        num_vertices
    }

    /// Estimate the index-buffer range consumed by an indirect draw.
    ///
    /// Port of `Maxwell3D::EstimateIndexBufferSize`.
    pub(crate) fn estimate_index_buffer_size(&self) -> usize {
        let start_address = self.index_buffer_addr();
        let end_address = <Self as dm::Maxwell3DAccess>::index_buffer_addr_end(self);
        let byte_size = self.index_buffer_format().size_bytes() as usize;
        // Upstream: `max_size = 1ull << (byte_size * CHAR_BIT)`, i.e. the number
        // of distinct index values, not the largest one.
        let max_size = 1u64 << (byte_size * 8);
        let cap = self
            .max_current_vertices()
            .saturating_mul(4)
            .saturating_mul(byte_size as u32) as usize;
        let lower_cap = (end_address.saturating_sub(start_address) as usize).min(cap);
        let max_layout_size = (byte_size as u64).saturating_mul(max_size);
        let layout_elements = self.memory_manager.as_ref().map_or(0, |memory_manager| {
            memory_manager
                .lock()
                .get_memory_layout_size_bounded(start_address, max_layout_size) as usize
                / byte_size
        });
        layout_elements.min(lower_cap)
    }

    // ── Upstream ProcessQueryGet / ProcessQueryCondition / etc ───────────

    /// Upstream: `Maxwell3D::StampQueryResult`.
    fn stamp_query_result(&mut self, payload: u64, long_query: bool) -> bool {
        let Some(memory_manager) = self.memory_manager.as_ref() else {
            return false;
        };
        let sequence_address = self.report_semaphore_address();
        let mut memory_manager = memory_manager.lock();
        if long_query {
            let gpu_ticks = self
                .gpu_ticks_getter
                .as_ref()
                .map(|getter| getter())
                .unwrap_or(0);
            memory_manager.write::<u64>(sequence_address + 8, gpu_ticks);
            memory_manager.write::<u64>(sequence_address, payload);
        } else {
            memory_manager.write::<u32>(sequence_address, payload as u32);
        }
        true
    }

    /// Handle report semaphore query. Matches upstream `ProcessQueryGet`.
    fn process_query_get(&mut self) {
        let query_word = self.regs[(REPORT_SEMAPHORE_BASE + 3) as usize];
        let operation = ReportOperation::from_raw(query_word);
        match operation {
            ReportOperation::Release | ReportOperation::ReportOnly => {
                let gpu_va = self.report_semaphore_address();
                let payload = self.report_semaphore_payload();
                let short_query = (query_word >> 28) & 1 != 0;
                let query_type = (query_word >> 23) & 0x1F;
                let subreport = (query_word >> 5) & 0x7;
                let mut flags = QueryPropertiesFlags::empty();
                if !short_query {
                    flags |= QueryPropertiesFlags::HAS_TIMEOUT;
                }
                if short_query && operation == ReportOperation::Release {
                    flags |= QueryPropertiesFlags::IS_A_FENCE;
                }
                let mut queried = false;

                log::debug!(
                    "Maxwell3D: query_get {:?} va=0x{:X} payload=0x{:X} short={} type={} subreport={}",
                    operation,
                    gpu_va,
                    payload,
                    short_query,
                    query_type,
                    subreport,
                );

                let _ = self.with_rasterizer_mut(|rasterizer| {
                    queried = true;
                    rasterizer.query(gpu_va, query_type, flags, payload, subreport);
                });

                if !queried {
                    if !self.stamp_query_result(payload as u64, !short_query) {
                        let gpu_ticks = self
                            .gpu_ticks_getter
                            .as_ref()
                            .map(|getter| getter())
                            .unwrap_or(0);
                        let data = if short_query {
                            payload.to_le_bytes().to_vec()
                        } else {
                            let mut buf = Vec::with_capacity(16);
                            buf.extend_from_slice(&(payload as u64).to_le_bytes());
                            buf.extend_from_slice(&gpu_ticks.to_le_bytes());
                            buf
                        };
                        self.pending_semaphore_writes
                            .push(PendingWrite { gpu_va, data });
                    }
                }
            }
            ReportOperation::Acquire => {
                stop_unimplemented_query_operation(
                    operation,
                    query_word,
                    self.report_semaphore_address(),
                    self.report_semaphore_payload(),
                );
            }
            ReportOperation::Trap => {
                stop_unimplemented_query_operation(
                    operation,
                    query_word,
                    self.report_semaphore_address(),
                    self.report_semaphore_payload(),
                );
            }
        }
    }

    /// Handle render enable / query condition. Matches upstream `ProcessQueryCondition`.
    fn process_query_condition(&mut self) {
        let condition_address = ((self.regs[RENDER_ENABLE_BASE as usize] as u64) << 32)
            | self.regs[(RENDER_ENABLE_BASE + 1) as usize] as u64;
        let accelerated = self
            .with_rasterizer_mut(|rasterizer| {
                rasterizer.accelerate_conditional_rendering_with_address(condition_address, 24)
            })
            .unwrap_or(false);
        if accelerated {
            self.execute_on = true;
            return;
        }

        let override_val = self.regs[RENDER_ENABLE_OVERRIDE as usize];
        match override_val {
            0 => {
                let mode = self.regs[RENDER_ENABLE_MODE as usize];
                match mode {
                    0 => self.execute_on = false,
                    1 => self.execute_on = true,
                    2..=4 => {
                        let mut compare_bytes = [0u8; 24];
                        if !self.read_gpu_block(condition_address, &mut compare_bytes) {
                            self.execute_on = true;
                            return;
                        }
                        let read_word = |index: usize| -> u32 {
                            let start = index * 4;
                            u32::from_le_bytes(compare_bytes[start..start + 4].try_into().unwrap())
                        };
                        let initial_sequence = read_word(0);
                        let initial_mode = read_word(1);
                        let current_sequence = read_word(4);
                        let current_mode = read_word(5);
                        self.execute_on = match mode {
                            2 => initial_sequence != 0 && initial_mode != 0,
                            3 => {
                                initial_sequence == current_sequence && initial_mode == current_mode
                            }
                            4 => {
                                initial_sequence != current_sequence || initial_mode != current_mode
                            }
                            _ => unreachable!(),
                        };
                    }
                    _ => {
                        log::warn!("Maxwell3D: unknown render_enable mode {}", mode);
                        self.execute_on = true;
                    }
                }
            }
            1 => {
                self.execute_on = true;
            }
            2 => {
                self.execute_on = false;
            }
            _ => {
                log::warn!("Maxwell3D: unknown render_enable override {}", override_val);
                self.execute_on = true;
            }
        }
    }

    /// Handle counter reset. Matches upstream `ProcessCounterReset`.
    fn process_counter_reset(&mut self) {
        let clear_report = self.regs[CLEAR_REPORT_VALUE as usize];
        log::debug!("Maxwell3D: counter_reset report=0x{:X}", clear_report);
        let query_type = match clear_report {
            1 => QueryType::ZPassPixelCount64 as u32,
            2 => QueryType::StreamingPrimitivesSucceeded as u32,
            3 => QueryType::PrimitivesGenerated as u32,
            4 => QueryType::VtgPrimitivesOut as u32,
            _ => QueryType::Payload as u32,
        };
        let _ = self.with_rasterizer_mut(|rasterizer| rasterizer.reset_counter(query_type));
    }

    /// Handle sync point. Matches upstream `ProcessSyncPoint`.
    fn process_sync_point(&mut self) {
        let sync_point = self.regs[SYNC_INFO as usize] & 0xFFFF;
        log::debug!("Maxwell3D: sync_point {}", sync_point);
        let _ = self.with_rasterizer_mut(|rasterizer| rasterizer.signal_sync_point(sync_point));
    }

    // ── Macro processing (matching upstream ProcessMacro / CallMacroMethod) ─

    /// Process a macro method call. Matches upstream `Maxwell3D::ProcessMacro`.
    fn process_macro(&mut self, method: u32, base_start: &[u32], is_last_call: bool) {
        if self.executing_macro == 0 {
            // A macro call must begin by writing the macro method's register.
            assert!(
                (method % 2) == 0,
                "Can't start macro execution by writing to the ARGS register"
            );
            self.executing_macro = method;
        }

        self.macro_params.extend_from_slice(base_start);
        for i in 0..base_start.len() {
            self.macro_addresses
                .push(self.interface_state.current_dma_segment + i as u64 * 4);
        }
        self.macro_segments.push((
            self.interface_state.current_dma_segment,
            base_start.len() as u32,
        ));
        self.current_macro_dirty |= self.interface_state.current_dirty;
        self.interface_state.current_dirty = false;

        // Call the macro when there are no more parameters in the command buffer.
        if is_last_call {
            self.consume_sink();
            self.call_macro_method(self.executing_macro);
        }
    }

    /// Execute a macro. Matches upstream `Maxwell3D::CallMacroMethod`.
    fn call_macro_method(&mut self, method: u32) {
        self.executing_macro = 0;

        let entry = ((method - MACRO_REGISTERS_START) >> 1) % 128;
        // Upstream passes a const reference to the channel-owned
        // `macro_params`, and `RefreshParameters` updates that same storage
        // before an HLE fallback or LLE execution consumes it. Move the
        // vector into this call so Rust can update the exact parameter slice
        // without creating an immutable/mutable alias or a stale clone.
        let mut params = std::mem::take(&mut self.macro_params);
        let macro_method = self.macro_positions[entry as usize];
        let self_raw = std::ptr::from_mut(self);
        let self_ptr = Maxwell3DPtr(self_raw);
        self.macro_engine.execute(
            self_raw,
            macro_method,
            &mut params,
            move |parameters| unsafe { (&mut *self_ptr.0).refresh_parameters_impl(parameters) },
        );

        // Upstream calls draw_manager->DrawDeferred() here.
        self.with_draw_manager(|draw_manager, this| {
            draw_manager.draw_deferred(this);
        });

        params.clear();
        debug_assert!(self.macro_params.is_empty());
        self.macro_params = params;
        self.macro_addresses.clear();
        self.macro_segments.clear();
        self.current_macro_dirty = false;
    }

    /// Consume the method sink. Matches upstream `ConsumeSink`.
    pub fn consume_sink(&mut self) {
        if self.interface_state.method_sink.is_empty() {
            return;
        }
        self.consume_sink_inner();
    }

    /// Internal sink consumption matching upstream `ConsumeSinkImpl`.
    fn consume_sink_inner(&mut self) {
        let control = ShadowRamControl::from_raw(self.shadow_state[SHADOW_RAM_CONTROL as usize]);
        let mut sink = std::mem::take(&mut self.interface_state.method_sink);
        match control {
            ShadowRamControl::Track | ShadowRamControl::TrackWithFilter => {
                for (method, value) in &sink {
                    self.shadow_state[*method as usize] = *value;
                    self.process_dirty_registers(*method, *value);
                }
            }
            ShadowRamControl::Replay => {
                for (method, _value) in &sink {
                    let shadow_val = self.shadow_state[*method as usize];
                    self.process_dirty_registers(*method, shadow_val);
                }
            }
            ShadowRamControl::Passthrough => {
                for (method, value) in &sink {
                    self.process_dirty_registers(*method, *value);
                }
            }
        }
        sink.clear();
        debug_assert!(self.interface_state.method_sink.is_empty());
        self.interface_state.method_sink = sink;
    }

    /// Execute the pending macro (if any) and reset state.
    pub fn flush_macro(&mut self) {
        if self.executing_macro == 0 || self.macro_params.is_empty() {
            return;
        }
        let method = self.executing_macro;
        let mut params = std::mem::take(&mut self.macro_params);
        let entry = ((method - MACRO_METHODS_START) >> 1) % 128;
        let macro_method = self.macro_positions[entry as usize];
        let self_raw = std::ptr::from_mut(self);
        let self_ptr = Maxwell3DPtr(self_raw);
        self.macro_engine.execute(
            self_raw,
            macro_method,
            &mut params,
            move |parameters| unsafe { (&mut *self_ptr.0).refresh_parameters_impl(parameters) },
        );
        self.executing_macro = 0;
        params.clear();
        debug_assert!(self.macro_params.is_empty());
        self.macro_params = params;
    }
}

// ── EngineInterface implementation (upstream CallMethod / CallMultiMethod) ──

impl EngineInterface for Maxwell3D {
    /// Write a single value to the register identified by `method`.
    /// Matches upstream `Maxwell3D::CallMethod`.
    fn call_method(&mut self, method: u32, method_argument: u32, is_last_call: bool) {
        // It is an error to write to a register other than the current macro's
        // ARG register before it has finished execution.
        if self.executing_macro != 0 {
            debug_assert!(
                method == self.executing_macro + 1,
                "Writing to method 0x{:X} while macro 0x{:X} is executing",
                method,
                self.executing_macro
            );
        }

        // Methods >= 0xE00 are macro triggers.
        if method >= MACRO_REGISTERS_START {
            self.process_macro(method, &[method_argument], is_last_call);
            return;
        }

        assert!(
            (method as usize) < ENGINE_REG_COUNT,
            "Invalid Maxwell3D register 0x{:X}, increase ENGINE_REG_COUNT",
            method
        );

        let argument = self.process_shadow_ram(method, method_argument);
        self.process_dirty_registers(method, argument);
        self.process_method_call(method, argument, method_argument, is_last_call);
    }

    /// Write multiple values to the register identified by `method`.
    /// Matches upstream `Maxwell3D::CallMultiMethod`.
    fn call_multi_method(
        &mut self,
        method: u32,
        base_start: &[u32],
        amount: u32,
        methods_pending: u32,
    ) {
        let amount = amount as usize;

        // Methods >= 0xE00 are macro triggers.
        if method >= MACRO_REGISTERS_START {
            self.process_macro(
                method,
                &base_start[..amount],
                amount as u32 == methods_pending,
            );
            return;
        }

        match method {
            m if m >= CB_DATA_BASE && m < CB_DATA_END => {
                self.process_cb_multi_data(&base_start[..amount]);
            }
            INLINE_DATA => {
                assert!(methods_pending == amount as u32);
                self.process_inline_upload_multi(&base_start[..amount]);
            }
            DRAW_INLINE_INDEX | INLINE_INDEX_2X16_EVEN | INLINE_INDEX_4X8_INDEX0
                if ShadowRamControl::from_raw(self.shadow_state[SHADOW_RAM_CONTROL as usize])
                    != ShadowRamControl::Replay =>
            {
                self.process_inline_index_multi_data(method, &base_start[..amount]);
            }
            _ => {
                for i in 0..amount {
                    let is_last = methods_pending.wrapping_sub(i as u32) <= 1;
                    self.call_method(method, base_start[i], is_last);
                }
            }
        }
    }

    fn consume_sink_impl(&mut self) {
        // Call the inherent method (not the trait method) to avoid infinite recursion.
        self.consume_sink_inner();
    }

    fn has_pending_methods(&self) -> bool {
        !self.interface_state.method_sink.is_empty()
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
impl Maxwell3D {
    pub(crate) fn write_reg(&mut self, method: u32, value: u32) {
        <Self as EngineInterface>::call_method(self, method, value, true);
    }

    pub(crate) fn execute_pending(
        &mut self,
        _read_gpu: &dyn Fn(u64, &mut [u8]),
    ) -> Vec<PendingWrite> {
        std::mem::take(&mut self.pending_semaphore_writes)
    }
}

#[cfg(test)]
#[path = "maxwell_3d_test.rs"]
mod tests;

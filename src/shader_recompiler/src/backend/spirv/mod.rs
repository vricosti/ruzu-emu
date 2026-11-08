// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SPIR-V backend subdirectory — individual emit files matching upstream
//! `backend/spirv/emit_spirv_*.cpp`.
//!
//! Each file here corresponds 1:1 to an upstream C++ file in
//! `shader_recompiler/backend/spirv/`.

pub mod emit_spirv;
pub mod emit_spirv_atomic;
pub mod emit_spirv_barriers;
pub mod emit_spirv_bitwise_conversion;
pub mod emit_spirv_composite;
pub mod emit_spirv_context_get_set;
pub mod emit_spirv_control_flow;
pub mod emit_spirv_convert;
pub mod emit_spirv_floating_point;
pub mod emit_spirv_image;
pub mod emit_spirv_image_atomic;
pub mod emit_spirv_integer;
pub mod emit_spirv_logical;
pub mod emit_spirv_memory;
pub mod emit_spirv_mesh;
pub mod emit_spirv_select;
pub mod emit_spirv_shared_memory;
pub mod emit_spirv_special;
pub mod emit_spirv_undefined;
pub mod emit_spirv_warp;
pub mod spirv_emit_context;

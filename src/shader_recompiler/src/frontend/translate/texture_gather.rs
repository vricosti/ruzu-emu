// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/texture_gather.cpp
//!
//! Implements TLD4 and TLD4_b (texture gather — returns one component from
//! a 2x2 footprint of texels).

use super::{field, TranslatorVisitor};
use crate::frontend::maxwell_opcodes::MaxwellOpcode;
use crate::ir::types::TextureInstInfo;
use crate::ir::value::{Pred, Value};
use crate::shader_info::TextureType as ShaderTextureType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
enum TextureType {
    E1D = 0,
    Array1D = 1,
    E2D = 2,
    Array2D = 3,
    E3D = 4,
    Array3D = 5,
    Cube = 6,
    ArrayCube = 7,
}

impl TextureType {
    fn from_bits(raw: u64) -> Self {
        match raw & 0x7 {
            0 => Self::E1D,
            1 => Self::Array1D,
            2 => Self::E2D,
            3 => Self::Array2D,
            4 => Self::E3D,
            5 => Self::Array3D,
            6 => Self::Cube,
            _ => Self::ArrayCube,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
enum OffsetType {
    None = 0,
    Aoffi = 1,
    Ptp = 2,
    Invalid = 3,
}

impl OffsetType {
    fn from_bits(raw: u64) -> Self {
        match raw & 0x3 {
            0 => Self::None,
            1 => Self::Aoffi,
            2 => Self::Ptp,
            _ => Self::Invalid,
        }
    }
}

fn get_type(texture_type: TextureType) -> ShaderTextureType {
    match texture_type {
        TextureType::E1D => ShaderTextureType::Color1D,
        TextureType::Array1D => ShaderTextureType::ColorArray1D,
        TextureType::E2D => ShaderTextureType::Color2D,
        TextureType::Array2D => ShaderTextureType::ColorArray2D,
        TextureType::E3D => ShaderTextureType::Color3D,
        TextureType::Array3D => panic!("3D array texture type"),
        TextureType::Cube => ShaderTextureType::ColorCube,
        TextureType::ArrayCube => ShaderTextureType::ColorArrayCube,
    }
}

fn read_array(v: &mut TranslatorVisitor, reg: u32) -> Value {
    let value = v.x(reg);
    v.ir.convert_u_to_f(32, 16, value, crate::ir::types::FpControl::default())
}

fn make_coords(v: &mut TranslatorVisitor, reg: u32, texture_type: TextureType) -> Value {
    match texture_type {
        TextureType::E1D => v.f(reg),
        TextureType::Array1D => {
            let x = v.f(reg + 1);
            let array = read_array(v, reg);
            v.ir.composite_construct_f32x2(x, array)
        }
        TextureType::E2D => {
            let x = v.f(reg);
            let y = v.f(reg + 1);
            v.ir.composite_construct_f32x2(x, y)
        }
        TextureType::Array2D => {
            let x = v.f(reg + 1);
            let y = v.f(reg + 2);
            let array = read_array(v, reg);
            v.ir.composite_construct_f32x3(x, y, array)
        }
        TextureType::E3D | TextureType::Cube => {
            let x = v.f(reg);
            let y = v.f(reg + 1);
            let z = v.f(reg + 2);
            v.ir.composite_construct_f32x3(x, y, z)
        }
        TextureType::Array3D => panic!("3D array texture type"),
        TextureType::ArrayCube => {
            let x = v.f(reg + 1);
            let y = v.f(reg + 2);
            let z = v.f(reg + 3);
            let array = read_array(v, reg);
            v.ir.composite_construct_f32x4(x, y, z, array)
        }
    }
}

fn make_offset(v: &mut TranslatorVisitor, reg: &mut u32, texture_type: TextureType) -> Value {
    let value = v.x(*reg);
    *reg += 1;
    let bitsize = Value::ImmU32(6);
    match texture_type {
        TextureType::E1D | TextureType::Array1D => {
            v.ir.bit_field_s_extract(value, Value::ImmU32(0), bitsize)
        }
        TextureType::E2D | TextureType::Array2D => {
            let x = v.ir.bit_field_s_extract(value, Value::ImmU32(0), bitsize);
            let y = v.ir.bit_field_s_extract(value, Value::ImmU32(8), bitsize);
            v.ir.composite_construct_u32x2(x, y)
        }
        TextureType::E3D | TextureType::Array3D => {
            let x = v.ir.bit_field_s_extract(value, Value::ImmU32(0), bitsize);
            let y = v.ir.bit_field_s_extract(value, Value::ImmU32(8), bitsize);
            let z = v.ir.bit_field_s_extract(value, Value::ImmU32(16), bitsize);
            v.ir.composite_construct_u32x3(x, y, z)
        }
        TextureType::Cube | TextureType::ArrayCube => panic!("Illegal offset on CUBE sample"),
    }
}

fn make_offset_ptp(v: &mut TranslatorVisitor, reg: &mut u32) -> (Value, Value) {
    let value1 = v.x(*reg);
    *reg += 1;
    let value2 = v.x(*reg);
    *reg += 1;
    let bitsize = Value::ImmU32(6);

    let a0 = v.ir.bit_field_s_extract(value1, Value::ImmU32(0), bitsize);
    let a1 = v.ir.bit_field_s_extract(value1, Value::ImmU32(8), bitsize);
    let a2 = v.ir.bit_field_s_extract(value1, Value::ImmU32(16), bitsize);
    let a3 = v.ir.bit_field_s_extract(value1, Value::ImmU32(24), bitsize);
    let b0 = v.ir.bit_field_s_extract(value2, Value::ImmU32(0), bitsize);
    let b1 = v.ir.bit_field_s_extract(value2, Value::ImmU32(8), bitsize);
    let b2 = v.ir.bit_field_s_extract(value2, Value::ImmU32(16), bitsize);
    let b3 = v.ir.bit_field_s_extract(value2, Value::ImmU32(24), bitsize);

    (
        v.ir.composite_construct_u32x4(a0, a1, a2, a3),
        v.ir.composite_construct_u32x4(b0, b1, b2, b3),
    )
}

fn impl_tld4(
    v: &mut TranslatorVisitor,
    insn: u64,
    component: u32,
    offset_type: OffsetType,
    is_bindless: bool,
) {
    let dest_reg = field(insn, 0, 8);
    let coord_reg = field(insn, 8, 8);
    let mut meta_reg = field(insn, 20, 8);
    let texture_type = TextureType::from_bits(field(insn, 28, 3) as u64);
    let mask = field(insn, 31, 4);
    let dc = field(insn, 50, 1) != 0;
    let sparse_pred = Pred(field(insn, 51, 3) as u8);
    let cbuf_offset = field(insn, 36, 13) * 4;

    let coords = make_coords(v, coord_reg, texture_type);
    let handle = if is_bindless {
        let handle = v.x(meta_reg);
        meta_reg += 1;
        handle
    } else {
        Value::ImmU32(cbuf_offset)
    };

    let mut offset = Value::Void;
    let mut offset2 = Value::Void;
    match offset_type {
        OffsetType::None => {}
        OffsetType::Aoffi => {
            offset = make_offset(v, &mut meta_reg, texture_type);
        }
        OffsetType::Ptp => {
            (offset, offset2) = make_offset_ptp(v, &mut meta_reg);
        }
        OffsetType::Invalid => panic!("Invalid offset type {:?}", offset_type),
    }

    let dref = if dc {
        let dref = v.f(meta_reg);
        meta_reg += 1;
        dref
    } else {
        Value::Void
    };
    let _ = meta_reg;

    let texture_type = get_type(texture_type);
    let info = TextureInstInfo {
        texture_type: texture_type as u8,
        is_depth: dc,
        gather_component: component as u8,
        ..Default::default()
    };

    let sample = if dc {
        v.ir.image_gather_dref_full(handle, coords, offset, offset2, dref, info.to_u32())
    } else {
        v.ir.image_gather_full(handle, coords, offset, offset2, info.to_u32())
    };

    let mut reg = dest_reg;
    for element in 0..4 {
        if ((mask >> element) & 1) == 0 {
            continue;
        }
        let value = v.ir.composite_extract_f32x4(sample, Value::ImmU32(element));
        v.set_f(reg, value);
        reg += 1;
    }

    if sparse_pred != Pred::PT {
        let sparse = v.ir.get_sparse_from_op(sample);
        let resident = v.ir.logical_not(sparse);
        v.ir.set_pred(sparse_pred, resident);
    }
}

/// TLD4 — Texture Gather (bound form).
///
/// Upstream: `TranslatorVisitor::TLD4(u64 insn)`.
pub fn tld4(tv: &mut TranslatorVisitor, insn: u64, _opcode: MaxwellOpcode) {
    let component = field(insn, 56, 2);
    let offset = OffsetType::from_bits(field(insn, 54, 2) as u64);
    impl_tld4(tv, insn, component, offset, false);
}

/// TLD4_b — Texture Gather (bindless form).
///
/// Upstream: `TranslatorVisitor::TLD4_b(u64 insn)`.
pub fn tld4_b(tv: &mut TranslatorVisitor, insn: u64, _opcode: MaxwellOpcode) {
    let component = field(insn, 38, 2);
    let offset = OffsetType::from_bits(field(insn, 36, 2) as u64);
    impl_tld4(tv, insn, component, offset, true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_layer_preserves_upstream_u16_conversion() {
        let mut program = crate::ir::program::Program::new(crate::ir::types::ShaderStage::Fragment);
        let block = program.add_block();
        let mut visitor = TranslatorVisitor::new(&mut program, block);

        let _ = read_array(&mut visitor, 0);

        assert!(visitor
            .ir
            .program
            .block(block)
            .iter()
            .any(|inst| { inst.opcode == crate::ir::opcodes::Opcode::ConvertF32U16 }));
    }
}

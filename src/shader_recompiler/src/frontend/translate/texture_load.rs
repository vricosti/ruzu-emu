// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/texture_load.cpp
//!
//! Implements TLD and TLD_b (texture load with explicit integer coordinates).

use super::{bit, field, TranslatorVisitor};
use crate::frontend::maxwell_opcodes::MaxwellOpcode;
use crate::ir::types::TextureInstInfo;
use crate::ir::value::{Pred, Value};
use crate::shader_info::TextureType as ShaderTextureType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextureType {
    D1,
    Array1D,
    D2,
    Array2D,
    D3,
    Array3D,
    Cube,
    ArrayCube,
}

impl TextureType {
    fn from_u32(value: u32) -> Self {
        match value {
            0 => Self::D1,
            1 => Self::Array1D,
            2 => Self::D2,
            3 => Self::Array2D,
            4 => Self::D3,
            5 => Self::Array3D,
            6 => Self::Cube,
            7 => Self::ArrayCube,
            _ => unreachable!(),
        }
    }
}

fn get_type(type_: TextureType) -> ShaderTextureType {
    match type_ {
        TextureType::D1 => ShaderTextureType::Color1D,
        TextureType::Array1D => ShaderTextureType::ColorArray1D,
        TextureType::D2 => ShaderTextureType::Color2D,
        TextureType::Array2D => ShaderTextureType::ColorArray2D,
        TextureType::D3 => ShaderTextureType::Color3D,
        TextureType::Array3D => panic!("3D array texture type"),
        TextureType::Cube => ShaderTextureType::ColorCube,
        TextureType::ArrayCube => ShaderTextureType::ColorArrayCube,
    }
}

fn read_array(v: &mut TranslatorVisitor, reg: u32) -> Value {
    let value = v.x(reg);
    v.ir.bit_field_u_extract(value, Value::ImmU32(0), Value::ImmU32(16))
}

fn make_coords(v: &mut TranslatorVisitor, reg: u32, type_: TextureType) -> Value {
    match type_ {
        TextureType::D1 => v.x(reg),
        TextureType::Array1D => {
            let x = v.x(reg + 1);
            let array = read_array(v, reg);
            v.ir.composite_construct_u32x2(x, array)
        }
        TextureType::D2 => {
            let x = v.x(reg);
            let y = v.x(reg + 1);
            v.ir.composite_construct_u32x2(x, y)
        }
        TextureType::Array2D => {
            let x = v.x(reg + 1);
            let y = v.x(reg + 2);
            let array = read_array(v, reg);
            v.ir.composite_construct_u32x3(x, y, array)
        }
        TextureType::D3 | TextureType::Cube => {
            let x = v.x(reg);
            let y = v.x(reg + 1);
            let z = v.x(reg + 2);
            v.ir.composite_construct_u32x3(x, y, z)
        }
        TextureType::Array3D => panic!("3D array texture type"),
        TextureType::ArrayCube => {
            let x = v.x(reg + 1);
            let y = v.x(reg + 2);
            let z = v.x(reg + 3);
            let array = read_array(v, reg);
            v.ir.composite_construct_u32x4(x, y, z, array)
        }
    }
}

fn make_offset(v: &mut TranslatorVisitor, reg: &mut u32, type_: TextureType) -> Value {
    let value = v.x(*reg);
    *reg += 1;
    match type_ {
        TextureType::D1 | TextureType::Array1D => {
            v.ir.bit_field_s_extract(value, Value::ImmU32(0), Value::ImmU32(4))
        }
        TextureType::D2 | TextureType::Array2D => {
            let x =
                v.ir.bit_field_s_extract(value, Value::ImmU32(0), Value::ImmU32(4));
            let y =
                v.ir.bit_field_s_extract(value, Value::ImmU32(4), Value::ImmU32(4));
            v.ir.composite_construct_u32x2(x, y)
        }
        TextureType::D3 | TextureType::Array3D => {
            let x =
                v.ir.bit_field_s_extract(value, Value::ImmU32(0), Value::ImmU32(4));
            let y =
                v.ir.bit_field_s_extract(value, Value::ImmU32(4), Value::ImmU32(4));
            let z =
                v.ir.bit_field_s_extract(value, Value::ImmU32(8), Value::ImmU32(4));
            v.ir.composite_construct_u32x3(x, y, z)
        }
        TextureType::Cube | TextureType::ArrayCube => panic!("Illegal offset on CUBE sample"),
    }
}

fn impl_tld(tv: &mut TranslatorVisitor, insn: u64, is_bindless: bool) {
    let dst = tv.dst_reg(insn);
    let coord_reg = field(insn, 8, 8);
    let mut meta_reg = field(insn, 20, 8);
    let type_ = TextureType::from_u32(field(insn, 28, 3));
    let mask = field(insn, 31, 4);
    let aoffi = bit(insn, 35);
    let multisample_enabled = bit(insn, 50);
    let sparse_pred = Pred(field(insn, 51, 3) as u8);
    let clamp = bit(insn, 54);
    let lod_enabled = bit(insn, 55);
    let cbuf_offset = field(insn, 36, 13) * 4;

    let coords = make_coords(tv, coord_reg, type_);
    let handle = if is_bindless {
        let handle = tv.x(meta_reg);
        meta_reg += 1;
        handle
    } else {
        Value::ImmU32(cbuf_offset)
    };
    let lod = if lod_enabled {
        let lod = tv.x(meta_reg);
        meta_reg += 1;
        lod
    } else {
        Value::ImmU32(0)
    };
    let offset = if aoffi {
        make_offset(tv, &mut meta_reg, type_)
    } else {
        Value::Void
    };
    let multisample = if multisample_enabled {
        let multisample = tv.x(meta_reg);
        meta_reg += 1;
        multisample
    } else {
        Value::Void
    };
    let _ = meta_reg;

    if clamp {
        panic!("TLD.CL - CLAMP is not implemented");
    }

    let texture_type = get_type(type_);
    let info = TextureInstInfo {
        texture_type: texture_type as u8,
        ..Default::default()
    };

    let sample = tv
        .ir
        .image_fetch_full(handle, coords, offset, lod, multisample, info.to_u32());

    let mut reg = dst;
    for element in 0..4 {
        if ((mask >> element) & 1) == 0 {
            continue;
        }
        let value = tv
            .ir
            .composite_extract_f32x4(sample, Value::ImmU32(element));
        tv.set_f(reg, value);
        reg += 1;
    }

    if sparse_pred != Pred::PT {
        let sparse = tv.ir.get_sparse_from_op(sample);
        let resident = tv.ir.logical_not(sparse);
        tv.ir.set_pred(sparse_pred, resident);
    }
}

/// TLD — Texture Load (bound form).
///
/// Uses integer coordinates and an explicit LOD level, fetching texel data
/// directly without filtering.
///
/// Upstream: `TranslatorVisitor::TLD(u64 insn)`
pub fn tld(tv: &mut TranslatorVisitor, insn: u64, _opcode: MaxwellOpcode) {
    impl_tld(tv, insn, false);
}

/// TLD_b — Texture Load (bindless form).
///
/// Upstream: `TranslatorVisitor::TLD_b(u64 insn)`
pub fn tld_b(tv: &mut TranslatorVisitor, insn: u64, opcode: MaxwellOpcode) {
    let _ = opcode;
    impl_tld(tv, insn, true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::opcodes::Opcode;
    use crate::ir::program::Program;
    use crate::ir::types::ShaderStage;

    fn fresh_program() -> Program {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        program
    }

    #[test]
    fn tld_bound_emits_upstream_image_fetch_payload() {
        let mut program = fresh_program();
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn = 1u64
            | (8u64 << 8)
            | (20u64 << 20)
            | (2u64 << 28)
            | (0xFu64 << 31)
            | (1u64 << 35)
            | (3u64 << 36)
            | (1u64 << 50)
            | (1u64 << 55);

        tld(&mut tv, insn, MaxwellOpcode::TLD);

        let fetch = tv.ir.program.blocks[0]
            .iter()
            .find(|inst| inst.opcode == Opcode::BoundImageFetch)
            .expect("TLD should emit a bound image fetch before texture pass");
        assert_eq!(fetch.args.len(), 5);
        assert_eq!(fetch.args[0], Value::ImmU32(12));
        assert!(!fetch.args[2].is_void(), "AOFFI offset must be preserved");
        assert!(!fetch.args[3].is_void(), "LOD operand must be preserved");
        assert!(
            !fetch.args[4].is_void(),
            "multisample operand must be preserved"
        );
    }

    #[test]
    fn tld_bindless_emits_bindless_image_fetch() {
        let mut program = fresh_program();
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn = 1u64 | (8u64 << 8) | (20u64 << 20) | (2u64 << 28) | (0x1u64 << 31);

        tld_b(&mut tv, insn, MaxwellOpcode::TLD_b);

        assert!(tv.ir.program.blocks[0]
            .iter()
            .any(|inst| inst.opcode == Opcode::BindlessImageFetch));
    }
}

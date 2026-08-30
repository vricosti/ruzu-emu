// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/texture_gradient.cpp
//!
//! Upstream `TXD`/`TXD_b` translate into `ImageGradient` IR via the
//! texture-pass.

use super::{field, TranslatorVisitor};
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

fn make_offset(v: &mut TranslatorVisitor, reg: u32, has_lod_clamp: bool) -> Value {
    let value = v.x(reg);
    let base = if has_lod_clamp { 12 } else { 16 };
    let x =
        v.ir.bit_field_s_extract(value, Value::ImmU32(base), Value::ImmU32(4));
    let y =
        v.ir.bit_field_s_extract(value, Value::ImmU32(base + 4), Value::ImmU32(4));
    v.ir.composite_construct_u32x2(x, y)
}

fn read_array(v: &mut TranslatorVisitor, reg: u32, has_lod_clamp: bool) -> Value {
    let value = v.x(reg);
    let count = if has_lod_clamp { 12 } else { 16 };
    let array_index =
        v.ir.bit_field_u_extract(value, Value::ImmU32(0), Value::ImmU32(count));
    v.ir.convert_u_to_f(32, 16, array_index, crate::ir::types::FpControl::default())
}

fn impl_txd(v: &mut TranslatorVisitor, insn: u64, is_bindless: bool) {
    let aoffi = field(insn, 35, 1) != 0;
    let has_lod_clamp = field(insn, 50, 1) != 0;
    if has_lod_clamp {
        std::panic::panic_any(crate::exception::NotImplementedException::new(
            "TXD.LC - CLAMP is not implemented",
        ));
    }

    let dest_reg = field(insn, 0, 8);
    let mut base_reg = field(insn, 8, 8);
    let derivative_reg = field(insn, 20, 8);
    let texture_type = TextureType::from_bits(field(insn, 28, 3) as u64);
    let mask = field(insn, 31, 4);
    let sparse_pred = Pred(field(insn, 51, 3) as u8);
    let cbuf_offset = field(insn, 36, 13) * 4;

    let handle = if is_bindless {
        let handle = v.x(base_reg);
        base_reg += 1;
        handle
    } else {
        Value::ImmU32(cbuf_offset)
    };

    let (coords, num_derivatives, last_reg) = match texture_type {
        TextureType::E1D => (v.f(base_reg), 1, base_reg + 1),
        TextureType::Array1D => {
            let last_reg = base_reg + 1;
            let x = v.f(base_reg);
            let array = read_array(v, last_reg, has_lod_clamp);
            (v.ir.composite_construct_f32x2(x, array), 1, last_reg)
        }
        TextureType::E2D => {
            let last_reg = base_reg + 2;
            let x = v.f(base_reg);
            let y = v.f(base_reg + 1);
            (v.ir.composite_construct_f32x2(x, y), 2, last_reg)
        }
        TextureType::Array2D => {
            let last_reg = base_reg + 2;
            let x = v.f(base_reg);
            let y = v.f(base_reg + 1);
            let array = read_array(v, last_reg, has_lod_clamp);
            (v.ir.composite_construct_f32x3(x, y, array), 2, last_reg)
        }
        _ => panic!("Invalid texture type"),
    };

    let derivatives = match num_derivatives {
        1 => {
            let dx = v.f(derivative_reg);
            let dy = v.f(derivative_reg + 1);
            v.ir.composite_construct_f32x2(dx, dy)
        }
        2 => {
            let dx0 = v.f(derivative_reg);
            let dy0 = v.f(derivative_reg + 1);
            let dx1 = v.f(derivative_reg + 2);
            let dy1 = v.f(derivative_reg + 3);
            v.ir.composite_construct_f32x4(dx0, dy0, dx1, dy1)
        }
        _ => panic!("Invalid texture type"),
    };

    let offset = if aoffi {
        make_offset(v, last_reg, has_lod_clamp)
    } else {
        Value::Void
    };
    let lod_clamp = Value::Void;
    let texture_type = get_type(texture_type);
    let info = TextureInstInfo {
        texture_type: texture_type as u8,
        num_derivatives,
        has_lod_clamp,
        ..Default::default()
    };
    let sample = v.ir.image_gradient_full(
        handle,
        coords,
        derivatives,
        offset,
        lod_clamp,
        info.to_u32(),
    );

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

/// TXD — Texture Gradient (bound form).
///
/// Upstream: `TranslatorVisitor::TXD(u64 insn)`.
pub fn txd(tv: &mut TranslatorVisitor, insn: u64) {
    impl_txd(tv, insn, false);
}

/// TXD_b — Texture Gradient (bindless form).
///
/// Upstream: `TranslatorVisitor::TXD_b(u64 insn)`.
pub fn txd_b(tv: &mut TranslatorVisitor, insn: u64) {
    impl_txd(tv, insn, true);
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
    fn array_layer_preserves_upstream_u16_conversion() {
        let mut program = fresh_program();
        let mut visitor = TranslatorVisitor::new(&mut program, 0);

        let _ = read_array(&mut visitor, 0, false);

        assert!(visitor
            .ir
            .program
            .block(0)
            .iter()
            .any(|inst| { inst.opcode == Opcode::ConvertF32U16 }));
    }

    #[test]
    fn txd_bound_array2d_emits_full_gradient_payload() {
        let mut program = fresh_program();
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn = 4u64
            | (8u64 << 8)
            | (20u64 << 20)
            | (3u64 << 28)
            | (0xFu64 << 31)
            | (1u64 << 35)
            | (7u64 << 36);

        txd(&mut tv, insn);

        let gradient = tv.ir.program.blocks[0]
            .iter()
            .find(|inst| inst.opcode == Opcode::BoundImageGradient)
            .expect("TXD should emit BoundImageGradient before texture pass");
        assert_eq!(gradient.args.len(), 5);
        assert_eq!(gradient.args[0], Value::ImmU32(28));
        assert!(!gradient.args[2].is_void(), "derivatives must be preserved");
        assert!(
            !gradient.args[3].is_void(),
            "AOFFI offset must be preserved"
        );
        assert!(
            gradient.args[4].is_void(),
            "LC is not implemented in upstream GLSL"
        );
    }

    #[test]
    fn txd_bindless_emits_bindless_gradient() {
        let mut program = fresh_program();
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn = 4u64 | (8u64 << 8) | (20u64 << 20) | (2u64 << 28) | (0x1u64 << 31);

        txd_b(&mut tv, insn);

        assert!(tv.ir.program.blocks[0]
            .iter()
            .any(|inst| inst.opcode == Opcode::BindlessImageGradient));
    }
}

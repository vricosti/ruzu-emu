// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden's frontend/maxwell/translate/impl/surface_atomic_operations.cpp.

use super::{field, TranslatorVisitor};
use crate::ir::types::TextureInstInfo;
use crate::ir::value::Value;
use crate::shader_info::{ImageFormat, TextureType};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
enum SurfaceType {
    E1D = 0,
    Buffer1D = 1,
    Array1D = 2,
    E2D = 3,
    Array2D = 4,
    E3D = 5,
}

impl SurfaceType {
    fn from_bits(raw: u64) -> Self {
        match raw & 0x7 {
            0 => Self::E1D,
            1 => Self::Buffer1D,
            2 => Self::Array1D,
            3 => Self::E2D,
            4 => Self::Array2D,
            5 => Self::E3D,
            value => panic!("Invalid type {}", value),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
enum Size {
    U32 = 0,
    S32 = 1,
    U64 = 2,
    S64 = 3,
    F32FtzRn = 4,
    F16x2FtzRn = 5,
    Sd32 = 6,
    Sd64 = 7,
}

impl Size {
    fn from_bits(raw: u64) -> Self {
        match raw & 0x7 {
            0 => Self::U32,
            1 => Self::S32,
            2 => Self::U64,
            3 => Self::S64,
            4 => Self::F32FtzRn,
            5 => Self::F16x2FtzRn,
            6 => Self::Sd32,
            7 => Self::Sd64,
            _ => unreachable!(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
enum AtomicOp {
    Add = 0,
    Min = 1,
    Max = 2,
    Inc = 3,
    Dec = 4,
    And = 5,
    Or = 6,
    Xor = 7,
    Exch = 8,
}

impl AtomicOp {
    fn from_bits(raw: u64) -> Self {
        match raw {
            0 => Self::Add,
            1 => Self::Min,
            2 => Self::Max,
            3 => Self::Inc,
            4 => Self::Dec,
            5 => Self::And,
            6 => Self::Or,
            7 => Self::Xor,
            8 => Self::Exch,
            value => panic!("Atomic Operation {}", value),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
enum Clamp {
    Ign = 0,
    Default = 1,
    Trap = 2,
}

impl Clamp {
    fn from_bits(raw: u64) -> Self {
        match raw & 0x3 {
            0 => Self::Ign,
            1 => Self::Default,
            2 => Self::Trap,
            value => panic!("Clamp {}", value),
        }
    }
}

fn get_type(surface_type: SurfaceType) -> TextureType {
    match surface_type {
        SurfaceType::E1D => TextureType::Color1D,
        SurfaceType::Buffer1D => TextureType::Buffer,
        SurfaceType::Array1D => TextureType::ColorArray1D,
        SurfaceType::E2D => TextureType::Color2D,
        SurfaceType::Array2D => TextureType::ColorArray2D,
        SurfaceType::E3D => TextureType::Color3D,
    }
}

fn make_coords(v: &mut TranslatorVisitor, reg: u32, surface_type: SurfaceType) -> Value {
    match surface_type {
        SurfaceType::E1D | SurfaceType::Buffer1D => v.x(reg),
        SurfaceType::Array1D => {
            let x = v.x(reg);
            let layer = v.x(reg + 1);
            let layer =
                v.ir.bit_field_u_extract(layer, Value::ImmU32(0), Value::ImmU32(16));
            v.ir.composite_construct_u32x2(x, layer)
        }
        SurfaceType::E2D => {
            let x = v.x(reg);
            let y = v.x(reg + 1);
            v.ir.composite_construct_u32x2(x, y)
        }
        SurfaceType::E3D => {
            let x = v.x(reg);
            let y = v.x(reg + 1);
            let z = v.x(reg + 2);
            v.ir.composite_construct_u32x3(x, y, z)
        }
        SurfaceType::Array2D => {
            let x = v.x(reg);
            let y = v.x(reg + 1);
            let layer = v.x(reg + 2);
            let layer =
                v.ir.bit_field_u_extract(layer, Value::ImmU32(0), Value::ImmU32(16));
            v.ir.composite_construct_u32x3(x, y, layer)
        }
    }
}

fn format(size: Size) -> ImageFormat {
    match size {
        Size::U32 | Size::S32 | Size::Sd32 => ImageFormat::R32Uint,
        value => panic!("Invalid size {:?}", value),
    }
}

fn is_size_int32(size: Size) -> bool {
    matches!(size, Size::U32 | Size::S32 | Size::Sd32)
}

fn apply_atomic_op(
    tv: &mut TranslatorVisitor,
    handle: Value,
    coords: Value,
    op_b: Value,
    info: u32,
    op: AtomicOp,
    is_signed: bool,
) -> Value {
    match op {
        AtomicOp::Add => tv.ir.image_atomic_iadd_32(handle, coords, op_b, info),
        AtomicOp::Min => tv
            .ir
            .image_atomic_imin_32(handle, coords, op_b, is_signed, info),
        AtomicOp::Max => tv
            .ir
            .image_atomic_imax_32(handle, coords, op_b, is_signed, info),
        AtomicOp::Inc => tv.ir.image_atomic_inc_32(handle, coords, op_b, info),
        AtomicOp::Dec => tv.ir.image_atomic_dec_32(handle, coords, op_b, info),
        AtomicOp::And => tv.ir.image_atomic_and_32(handle, coords, op_b, info),
        AtomicOp::Or => tv.ir.image_atomic_or_32(handle, coords, op_b, info),
        AtomicOp::Xor => tv.ir.image_atomic_xor_32(handle, coords, op_b, info),
        AtomicOp::Exch => tv.ir.image_atomic_exchange_32(handle, coords, op_b, info),
    }
}

#[allow(clippy::too_many_arguments)]
fn image_atom_op(
    tv: &mut TranslatorVisitor,
    dest_reg: u32,
    operand_reg: u32,
    coord_reg: u32,
    bindless_reg: u32,
    op: AtomicOp,
    clamp: Clamp,
    size: Size,
    surface_type: SurfaceType,
    bound_offset: u32,
    is_bindless: bool,
    write_result: bool,
) {
    if clamp != Clamp::Ign {
        panic!("Clamp {:?}", clamp);
    }
    if !is_size_int32(size) {
        panic!("Size {:?}", size);
    }

    let is_signed = size == Size::S32;
    let image_format = format(size);
    let texture_type = get_type(surface_type);
    let coords = make_coords(tv, coord_reg, surface_type);
    let handle = if is_bindless {
        tv.x(bindless_reg)
    } else {
        Value::ImmU32(bound_offset * 4)
    };
    let info = TextureInstInfo {
        texture_type: texture_type as u8,
        image_format: image_format as u8,
        ..Default::default()
    };

    let op_b = tv.x(operand_reg);
    let color = apply_atomic_op(tv, handle, coords, op_b, info.to_u32(), op, is_signed);

    if write_result {
        tv.set_x(dest_reg, color);
    }
}

/// SUATOM — Surface Atomic Operation.
pub fn suatom(tv: &mut TranslatorVisitor, insn: u64) {
    let is_bindless = field(insn, 54, 1) != 0;
    let op = AtomicOp::from_bits(field(insn, 29, 4) as u64);
    let surface_type = SurfaceType::from_bits(field(insn, 33, 3) as u64);
    let size = Size::from_bits(field(insn, 51, 3) as u64);
    let clamp = Clamp::from_bits(field(insn, 49, 2) as u64);
    let dest_reg = field(insn, 0, 8);
    let coord_reg = field(insn, 8, 8);
    let operand_reg = field(insn, 20, 8);
    let bound_offset = field(insn, 36, 13);
    let bindless_reg = field(insn, 39, 8);

    image_atom_op(
        tv,
        dest_reg,
        operand_reg,
        coord_reg,
        bindless_reg,
        op,
        clamp,
        size,
        surface_type,
        bound_offset,
        is_bindless,
        true,
    );
}

/// SURED — Surface Reduction.
pub fn sured(tv: &mut TranslatorVisitor, insn: u64) {
    let is_bound = field(insn, 51, 1) != 0;
    let op = AtomicOp::from_bits(field(insn, 24, 3) as u64);
    let surface_type = SurfaceType::from_bits(field(insn, 33, 3) as u64);
    let size = Size::from_bits(field(insn, 20, 3) as u64);
    let clamp = Clamp::from_bits(field(insn, 49, 2) as u64);
    let operand_reg = field(insn, 0, 8);
    let coord_reg = field(insn, 8, 8);
    let bound_offset = field(insn, 36, 13);
    let bindless_reg = field(insn, 39, 8);

    image_atom_op(
        tv,
        255,
        operand_reg,
        coord_reg,
        bindless_reg,
        op,
        clamp,
        size,
        surface_type,
        bound_offset,
        !is_bound,
        false,
    );
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
    fn suatom_bound_signed_min_writes_result() {
        let mut program = fresh_program();
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn = 4u64
            | (8u64 << 8)
            | (12u64 << 20)
            | (AtomicOp::Min as u64) << 29
            | (SurfaceType::E2D as u64) << 33
            | (7u64 << 36)
            | (Size::S32 as u64) << 51;

        suatom(&mut tv, insn);

        let atomic = tv.ir.program.blocks[0]
            .iter()
            .find(|inst| inst.opcode == Opcode::BoundImageAtomicSMin32)
            .expect("SUATOM should emit a bound signed image atomic min");
        assert_eq!(atomic.args[0], Value::ImmU32(28));
        let info = TextureInstInfo::from_u32(atomic.flags);
        assert_eq!(info.texture_type, TextureType::Color2D as u8);
        assert_eq!(info.image_format, ImageFormat::R32Uint as u8);
    }

    #[test]
    fn sured_bindless_add_does_not_write_result() {
        let mut program = fresh_program();
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn = 4u64
            | (8u64 << 8)
            | (Size::U32 as u64) << 20
            | (AtomicOp::Add as u64) << 24
            | (SurfaceType::E2D as u64) << 33
            | (39u64 << 39);

        sured(&mut tv, insn);

        let atomic = tv.ir.program.blocks[0]
            .iter()
            .find(|inst| inst.opcode == Opcode::BindlessImageAtomicIAdd32)
            .expect("SURED should emit a bindless image atomic add");
        assert_eq!(atomic.args.len(), 3);
        assert!(tv.ir.program.blocks[0]
            .iter()
            .all(|inst| inst.opcode != Opcode::SetRegister));
    }

    #[test]
    fn sured_operation_field_is_independent_of_size() {
        for (op, size, expected) in [
            (AtomicOp::Or, Size::U32, Opcode::BoundImageAtomicOr32),
            (AtomicOp::Min, Size::S32, Opcode::BoundImageAtomicSMin32),
            (AtomicOp::Xor, Size::U32, Opcode::BoundImageAtomicXor32),
        ] {
            let mut program = fresh_program();
            let mut tv = TranslatorVisitor::new(&mut program, 0);
            // Original surface_atomic_operations.cpp: op 24..26, size 20..22.
            let insn = 4
                | (8 << 8)
                | ((size as u64) << 20)
                | ((op as u64) << 24)
                | (3 << 33)
                | (12 << 36)
                | (1 << 51);
            sured(&mut tv, insn);
            let atomic = tv.ir.program.blocks[0]
                .iter()
                .find(|inst| inst.opcode == expected)
                .expect("SURED must decode operation separately from size");
            assert_eq!(atomic.args[0], Value::ImmU32(48));
            assert!(tv.ir.program.blocks[0]
                .iter()
                .all(|inst| inst.opcode != Opcode::SetRegister));
        }
    }

    #[test]
    fn surface_array_coordinates_extract_unsigned_low_16_bit_layer() {
        for (kind, dimension, components) in [
            (
                SurfaceType::Array1D,
                TextureType::ColorArray1D,
                Opcode::CompositeConstructU32x2,
            ),
            (
                SurfaceType::Array2D,
                TextureType::ColorArray2D,
                Opcode::CompositeConstructU32x3,
            ),
        ] {
            let mut program = fresh_program();
            let mut tv = TranslatorVisitor::new(&mut program, 0);
            sured(&mut tv, 4 | (8 << 8) | ((kind as u64) << 33) | (1 << 51));
            let block = &tv.ir.program.blocks[0];
            let extract = block
                .iter()
                .find(|inst| inst.opcode == Opcode::BitFieldUExtract)
                .expect("array layer is unsigned 16-bit");
            assert_eq!(&extract.args[1..], &[Value::ImmU32(0), Value::ImmU32(16)]);
            assert!(block.iter().any(|inst| inst.opcode == components));
            let atomic = block
                .iter()
                .find(|inst| inst.opcode == Opcode::BoundImageAtomicIAdd32)
                .unwrap();
            assert_eq!(
                TextureInstInfo::from_u32(atomic.flags).texture_type,
                dimension as u8
            );
        }
    }
}

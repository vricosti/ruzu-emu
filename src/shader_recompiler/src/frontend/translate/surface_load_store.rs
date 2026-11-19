// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/surface_load_store.cpp
//!
//! `SULD`/`SUST` translate to storage-image `ImageRead`/`ImageWrite` IR.

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
    U8 = 0,
    S8 = 1,
    U16 = 2,
    S16 = 3,
    B32 = 4,
    B64 = 5,
    B128 = 6,
}

impl Size {
    fn from_bits(raw: u64) -> Self {
        match raw & 0x7 {
            0 => Self::U8,
            1 => Self::S8,
            2 => Self::U16,
            3 => Self::S16,
            4 => Self::B32,
            5 => Self::B64,
            6 => Self::B128,
            value => panic!("Invalid size {}", value),
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

const R: u32 = 1 << 0;
const G: u32 = 1 << 1;
const B: u32 = 1 << 2;
const A: u32 = 1 << 3;
const MASK: [u32; 16] = [
    0,
    R,
    G,
    R | G,
    B,
    R | B,
    G | B,
    R | G | B,
    A,
    R | A,
    G | A,
    R | G | A,
    B | A,
    R | B | A,
    G | B | A,
    R | G | B | A,
];

fn format(size: Size) -> ImageFormat {
    match size {
        Size::U8 => ImageFormat::R8Uint,
        Size::S8 => ImageFormat::R8Sint,
        Size::U16 => ImageFormat::R16Uint,
        Size::S16 => ImageFormat::R16Sint,
        Size::B32 => ImageFormat::R32Uint,
        Size::B64 => ImageFormat::R32G32Uint,
        Size::B128 => ImageFormat::R32G32B32A32Uint,
    }
}

fn size_in_regs(size: Size) -> u32 {
    match size {
        Size::U8 | Size::S8 | Size::U16 | Size::S16 | Size::B32 => 1,
        Size::B64 => 2,
        Size::B128 => 4,
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

fn array_coord(v: &mut TranslatorVisitor, reg: u32, index: u32) -> Value {
    let value = v.x(reg + index);
    v.ir.bit_field_u_extract(value, Value::ImmU32(0), Value::ImmU32(16))
}

fn make_coords(v: &mut TranslatorVisitor, reg: u32, surface_type: SurfaceType) -> Value {
    match surface_type {
        SurfaceType::E1D | SurfaceType::Buffer1D => v.x(reg),
        SurfaceType::Array1D => {
            let x = v.x(reg);
            let a = array_coord(v, reg, 1);
            v.ir.composite_construct_u32x2(x, a)
        }
        SurfaceType::E2D => {
            let x = v.x(reg);
            let y = v.x(reg + 1);
            v.ir.composite_construct_u32x2(x, y)
        }
        SurfaceType::Array2D => {
            let x = v.x(reg);
            let y = v.x(reg + 1);
            let a = array_coord(v, reg, 2);
            v.ir.composite_construct_u32x3(x, y, a)
        }
        SurfaceType::E3D => {
            let x = v.x(reg);
            let y = v.x(reg + 1);
            let z = v.x(reg + 2);
            v.ir.composite_construct_u32x3(x, y, z)
        }
    }
}

fn swizzle_mask(swizzle: u32) -> u32 {
    if swizzle == 0 || swizzle as usize >= MASK.len() {
        panic!("Invalid swizzle {}", swizzle);
    }
    MASK[swizzle as usize]
}

fn check_alignment(reg: u32, alignment: u32) {
    if reg != 255 && reg % alignment != 0 {
        panic!("Unaligned destination register");
    }
}

fn make_color(ir: &mut crate::ir::emitter::Emitter<'_>, reg: u32, num_regs: u32) -> Value {
    let mut colors = [Value::ImmU32(0); 4];
    for i in 0..num_regs {
        colors[i as usize] = ir.get_reg(crate::ir::value::Reg(u64::from(reg)) + i as i32);
    }
    ir.composite_construct_u32x4(colors[0], colors[1], colors[2], colors[3])
}

/// SULD — Surface Load.
pub fn suld(tv: &mut TranslatorVisitor, insn: u64) {
    let is_bound = field(insn, 51, 1) != 0;
    let is_typed = field(insn, 52, 1) != 0;
    let ba = field(insn, 23, 1) != 0;
    let surface_type = SurfaceType::from_bits(field(insn, 33, 3) as u64);
    let cache = field(insn, 24, 2);
    let size_bits = field(insn, 20, 3);
    let swizzle = field(insn, 20, 4);
    let clamp = Clamp::from_bits(field(insn, 49, 2) as u64);
    let dest_reg = field(insn, 0, 8);
    let coord_reg = field(insn, 8, 8);
    let bound_offset = field(insn, 36, 13) * 4;
    let bindless_reg = field(insn, 39, 8);

    if clamp != Clamp::Ign {
        panic!("Clamp {:?}", clamp);
    }
    if cache != 0 && cache != 1 {
        panic!("Cache {}", cache);
    }
    if is_typed && ba {
        panic!("BA");
    }

    let image_format = if is_typed {
        let size = Size::from_bits(size_bits as u64);
        format(size)
    } else {
        ImageFormat::Typeless
    };
    let texture_type = get_type(surface_type);
    let coords = make_coords(tv, coord_reg, surface_type);
    let handle = if is_bound {
        Value::ImmU32(bound_offset)
    } else {
        tv.x(bindless_reg)
    };
    let info = TextureInstInfo {
        texture_type: texture_type as u8,
        image_format: image_format as u8,
        ..Default::default()
    };

    let result = tv.ir.image_read(handle, coords, info.to_u32());
    let mut dest_reg = dest_reg;
    if is_typed {
        let size = Size::from_bits(size_bits as u64);
        for component in 0..size_in_regs(size) {
            let value = tv
                .ir
                .composite_extract_u32x4(result, Value::ImmU32(component));
            tv.set_x(dest_reg + component, value);
        }
    } else {
        let mask = swizzle_mask(swizzle);
        let bits = mask.count_ones();
        check_alignment(dest_reg, if bits == 3 { 4 } else { bits });
        for component in 0..4 {
            if ((mask >> component) & 1) == 0 {
                continue;
            }
            let value = tv
                .ir
                .composite_extract_u32x4(result, Value::ImmU32(component));
            tv.set_x(dest_reg, value);
            dest_reg += 1;
        }
    }
}

/// SUST — Surface Store.
pub fn sust(tv: &mut TranslatorVisitor, insn: u64) {
    let is_bound = field(insn, 51, 1) != 0;
    let is_typed = field(insn, 52, 1) != 0;
    let ba = field(insn, 23, 1) != 0;
    let surface_type = SurfaceType::from_bits(field(insn, 33, 3) as u64);
    let cache = field(insn, 24, 2);
    let size_bits = field(insn, 20, 3);
    let swizzle = field(insn, 20, 4);
    let clamp = Clamp::from_bits(field(insn, 49, 2) as u64);
    let data_reg = field(insn, 0, 8);
    let coord_reg = field(insn, 8, 8);
    let bound_offset = field(insn, 36, 13) * 4;
    let bindless_reg = field(insn, 39, 8);

    if clamp != Clamp::Ign {
        panic!("Clamp {:?}", clamp);
    }
    if cache != 0 && cache != 1 {
        panic!("Cache {}", cache);
    }
    if is_typed && ba {
        panic!("BA");
    }

    let image_format = if is_typed {
        let size = Size::from_bits(size_bits as u64);
        format(size)
    } else {
        ImageFormat::Typeless
    };
    let texture_type = get_type(surface_type);
    let coords = make_coords(tv, coord_reg, surface_type);
    let handle = if is_bound {
        Value::ImmU32(bound_offset)
    } else {
        tv.x(bindless_reg)
    };
    let info = TextureInstInfo {
        texture_type: texture_type as u8,
        image_format: image_format as u8,
        ..Default::default()
    };
    let color = if is_typed {
        let size = Size::from_bits(size_bits as u64);
        make_color(&mut tv.ir, data_reg, size_in_regs(size))
    } else {
        let mask = swizzle_mask(swizzle);
        if mask != 0xf {
            panic!("Non-full mask");
        }
        make_color(&mut tv.ir, data_reg, 4)
    };
    tv.ir.image_write(handle, coords, color, info.to_u32());
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
    fn suld_typed_bound_emits_bound_image_read() {
        let mut program = fresh_program();
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn = 4u64
            | (8u64 << 8)
            | (4u64 << 20)
            | (3u64 << 33)
            | (7u64 << 36)
            | (1u64 << 51)
            | (1u64 << 52);

        suld(&mut tv, insn);

        let read = tv.ir.program.blocks[0]
            .iter()
            .find(|inst| inst.opcode == Opcode::BoundImageRead)
            .expect("SULD should emit BoundImageRead before texture pass");
        assert_eq!(read.args[0], Value::ImmU32(28));
        assert_eq!(read.args.len(), 2);
        let info = TextureInstInfo::from_u32(read.flags);
        assert_eq!(info.texture_type, TextureType::Color2D as u8);
        assert_eq!(info.image_format, ImageFormat::R32Uint as u8);
    }

    #[test]
    fn surface_zero_register_components_remain_zero_registers() {
        let mut program = fresh_program();
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        make_color(&mut tv.ir, 255, 4);
        let reads: Vec<_> = program.blocks[0].iter()
            .filter(|inst| inst.opcode == Opcode::GetRegister)
            .map(|inst| inst.args[0]).collect();
        assert_eq!(reads, vec![Value::Reg(crate::ir::value::Reg::RZ); 4]);
    }

    #[test]
    fn sust_untyped_bindless_full_mask_emits_bindless_image_write() {
        let mut program = fresh_program();
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn = 4u64 | (8u64 << 8) | (0xfu64 << 20) | (3u64 << 33) | (39u64 << 39);

        sust(&mut tv, insn);

        let write = tv.ir.program.blocks[0]
            .iter()
            .find(|inst| inst.opcode == Opcode::BindlessImageWrite)
            .expect("SUST should emit BindlessImageWrite before texture pass");
        assert_eq!(write.args.len(), 3);
        let info = TextureInstInfo::from_u32(write.flags);
        assert_eq!(info.texture_type, TextureType::Color2D as u8);
        assert_eq!(info.image_format, ImageFormat::Typeless as u8);
    }
}

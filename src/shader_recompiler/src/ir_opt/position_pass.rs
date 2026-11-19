// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `ir_opt/position_pass.cpp`
//!
//! Transforms position outputs when viewport transform state indicates
//! that the shader needs to apply render area scaling. Rewrites
//! PositionX/Y attribute stores to apply FMA with render area dimensions.

use crate::environment::Environment;
use crate::ir::basic_block::Block;
use crate::ir::instruction::Inst;
use crate::ir::opcodes::Opcode;
use crate::ir::program::Program;
use crate::ir::types::ShaderStage;
use crate::ir::value::{Attribute, InstRef, Value};

struct PositionInst {
    block: u32,
    inst: u32,
    attr: Attribute,
}

/// Port of upstream `Optimization::PositionPass`.
pub fn position_pass(env: &mut dyn Environment, program: &mut Program) {
    let stage = env.shader_stage();
    // Task programs can retain a vertex binding slot in their environment;
    // their stage-buffer positions are handled by the mesh output path.
    if stage != ShaderStage::VertexB || env.mesh_shader().is_some() {
        return;
    }
    let viewport_transform_state = env.read_viewport_transform_state();
    if std::env::var_os("RUZU_TRACE_POSITION_PASS").is_some() {
        eprintln!(
            "[POSITION_PASS] stage={:?} viewport_transform_state=0x{:X} post_order_blocks={} blocks={}",
            stage,
            viewport_transform_state,
            program.post_order_blocks.len(),
            program.blocks.len(),
        );
    }
    if viewport_transform_state != 0 {
        return;
    }

    program.info.uses_render_area = true;

    let mut to_replace = Vec::new();
    for block_index in program.post_order_blocks.clone() {
        let Some(block) = program.blocks.get(block_index as usize) else {
            continue;
        };
        for (inst_index, inst) in block.indexed_iter() {
            if inst.opcode != Opcode::SetAttribute {
                continue;
            }
            let Value::Attribute(attr) = inst.arg(0) else {
                continue;
            };
            if matches!(*attr, Attribute::POSITION_X | Attribute::POSITION_Y) {
                to_replace.push(PositionInst {
                    block: block_index,
                    inst: inst_index,
                    attr: *attr,
                });
            }
        }
    }

    for position_inst in to_replace {
        let block = &mut program.blocks[position_inst.block as usize];
        let value = *block.inst(position_inst.inst).arg(1);
        let component = match position_inst.attr {
            Attribute::POSITION_X => 0,
            Attribute::POSITION_Y => 1,
            _ => continue,
        };
        let render_area_dimension =
            render_area_dimension(block, position_inst.block, position_inst.inst, component);
        let recip = insert_before(
            block,
            position_inst.block,
            position_inst.inst,
            Opcode::FPRecip32,
            vec![render_area_dimension],
        );
        let scaled_recip = insert_before(
            block,
            position_inst.block,
            position_inst.inst,
            Opcode::FPMul32,
            vec![recip, Value::ImmF32(2.0)],
        );
        let scaled_position = insert_before(
            block,
            position_inst.block,
            position_inst.inst,
            Opcode::FPFma32,
            vec![value, scaled_recip, Value::ImmF32(-1.0)],
        );
        block
            .inst_mut(position_inst.inst)
            .set_arg(1, scaled_position);
    }
}

fn render_area_dimension(
    block: &mut Block,
    block_index: u32,
    before: u32,
    component: u32,
) -> Value {
    let render_area = insert_before(block, block_index, before, Opcode::RenderArea, vec![]);
    insert_before(
        block,
        block_index,
        before,
        Opcode::CompositeExtractF32x4,
        vec![render_area, Value::ImmU32(component)],
    )
}

fn insert_before(
    block: &mut Block,
    block_index: u32,
    before: u32,
    opcode: Opcode,
    args: Vec<Value>,
) -> Value {
    let inst = block.insert_inst_before(before, Inst::new(opcode, args));
    Value::Inst(InstRef {
        block: block_index,
        inst,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::TexturePassCaches;
    use crate::program_header::ProgramHeader;
    use crate::shader_info::{ReplaceConstant, TexturePixelFormat, TextureType};

    struct MockEnvironment {
        texture_pass_caches: TexturePassCaches,
        sph: ProgramHeader,
        stage: ShaderStage,
        viewport_transform_state: u32,
    }

    impl MockEnvironment {
        fn new(stage: ShaderStage, viewport_transform_state: u32) -> Self {
            Self {
                texture_pass_caches: TexturePassCaches::default(),
                sph: ProgramHeader::default(),
                stage,
                viewport_transform_state,
            }
        }
    }

    impl Environment for MockEnvironment {
        fn texture_pass_caches(&mut self) -> &mut TexturePassCaches {
            &mut self.texture_pass_caches
        }

        fn read_instruction(&mut self, _address: u32) -> u64 {
            0
        }

        fn read_cbuf_value(&mut self, _cbuf_index: u32, _cbuf_offset: u32) -> u32 {
            0
        }

        fn read_texture_type(&mut self, _raw_handle: u32) -> TextureType {
            TextureType::Color2D
        }

        fn read_texture_pixel_format(&mut self, _raw_handle: u32) -> TexturePixelFormat {
            TexturePixelFormat::A8B8G8R8Unorm
        }

        fn is_texture_pixel_format_integer(&mut self, _raw_handle: u32) -> bool {
            false
        }

        fn read_viewport_transform_state(&mut self) -> u32 {
            self.viewport_transform_state
        }

        fn texture_bound_buffer(&self) -> u32 {
            0
        }

        fn local_memory_size(&self) -> u32 {
            0
        }

        fn shared_memory_size(&self) -> u32 {
            0
        }

        fn workgroup_size(&self) -> [u32; 3] {
            [1, 1, 1]
        }

        fn has_hle_macro_state(&self) -> bool {
            false
        }

        fn get_replace_const_buffer(
            &mut self,
            _bank: u32,
            _offset: u32,
        ) -> Option<ReplaceConstant> {
            None
        }

        fn dump(&mut self, _pipeline_hash: u64, _shader_hash: u64) {}

        fn sph(&self) -> &ProgramHeader {
            &self.sph
        }

        fn gp_passthrough_mask(&self) -> &[u32; 8] {
            static MASK: [u32; 8] = [0; 8];
            &MASK
        }

        fn shader_stage(&self) -> ShaderStage {
            self.stage
        }

        fn start_address(&self) -> u32 {
            0
        }

        fn is_proprietary_driver(&self) -> bool {
            false
        }
    }

    fn program_with_position_store(stage: ShaderStage, attr: Attribute) -> Program {
        let mut program = Program::new(stage);
        let mut block = Block::new();
        block.append_inst(Inst::new(
            Opcode::SetAttribute,
            vec![
                Value::Attribute(attr),
                Value::ImmF32(16.0),
                Value::ImmU32(0),
            ],
        ));
        program.blocks.push(block);
        program.post_order_blocks.push(0);
        program
    }

    fn first_inst_with_opcode(program: &Program, opcode: Opcode) -> &Inst {
        program.blocks[0]
            .iter()
            .find(|inst| inst.opcode == opcode)
            .unwrap_or_else(|| panic!("missing {opcode:?}"))
    }

    #[test]
    fn position_pass_rewrites_position_x_when_viewport_transform_is_disabled() {
        let mut env = MockEnvironment::new(ShaderStage::VertexB, 0);
        let mut program = program_with_position_store(ShaderStage::VertexB, Attribute::POSITION_X);

        position_pass(&mut env, &mut program);

        assert!(program.info.uses_render_area);
        let opcodes: Vec<_> = program.blocks[0]
            .indexed_iter()
            .map(|(_, inst)| inst.opcode)
            .collect();
        assert_eq!(
            opcodes,
            vec![
                Opcode::RenderArea,
                Opcode::CompositeExtractF32x4,
                Opcode::FPRecip32,
                Opcode::FPMul32,
                Opcode::FPFma32,
                Opcode::SetAttribute,
            ]
        );
        assert_eq!(
            *first_inst_with_opcode(&program, Opcode::CompositeExtractF32x4).arg(1),
            Value::ImmU32(0),
            "PositionX must use RenderAreaWidth"
        );
        let Value::Inst(fma_ref) = *program.blocks[0].inst(0).arg(1) else {
            panic!("SetAttribute value must be rewritten to the FPFma result");
        };
        assert!(matches!(
            program.blocks[0].inst(fma_ref.inst).opcode,
            Opcode::FPFma32
        ));
    }

    #[test]
    fn position_pass_rewrites_position_y_to_render_area_height() {
        let mut env = MockEnvironment::new(ShaderStage::VertexB, 0);
        let mut program = program_with_position_store(ShaderStage::VertexB, Attribute::POSITION_Y);

        position_pass(&mut env, &mut program);

        assert_eq!(
            *first_inst_with_opcode(&program, Opcode::CompositeExtractF32x4).arg(1),
            Value::ImmU32(1),
            "PositionY must use RenderAreaHeight"
        );
    }

    #[test]
    fn position_pass_keeps_vertexb_when_viewport_transform_state_is_enabled() {
        let mut env = MockEnvironment::new(ShaderStage::VertexB, 1);
        let mut program = program_with_position_store(ShaderStage::VertexB, Attribute::POSITION_X);

        position_pass(&mut env, &mut program);

        assert!(!program.info.uses_render_area);
        let opcodes: Vec<_> = program.blocks[0]
            .indexed_iter()
            .map(|(_, inst)| inst.opcode)
            .collect();
        assert_eq!(opcodes, vec![Opcode::SetAttribute]);
    }

    #[test]
    fn position_pass_keeps_non_vertexb_stage() {
        let mut env = MockEnvironment::new(ShaderStage::Geometry, 0);
        let mut program = program_with_position_store(ShaderStage::Geometry, Attribute::POSITION_X);

        position_pass(&mut env, &mut program);

        assert!(!program.info.uses_render_area);
        let opcodes: Vec<_> = program.blocks[0]
            .indexed_iter()
            .map(|(_, inst)| inst.opcode)
            .collect();
        assert_eq!(opcodes, vec![Opcode::SetAttribute]);
    }
}

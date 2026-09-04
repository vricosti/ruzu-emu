// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/load_store_attribute.cpp
//!
//! Implements ALD, AST (attribute load/store for tessellation/geometry stages)
//! and IPA (interpolate pixel attribute for fragment shaders).

use super::{bit, field, TranslatorVisitor};
use crate::ir::value::{Attribute, Patch, Reg, Value};
use crate::program_header::PixelImap;

const INTERPOLATION_MODE_MULTIPLY: u32 = 1;
const INTERPOLATION_MODE_SC: u32 = 3;

/// Walk the indexed-attribute element loop, computing
/// `final_offset = index_value + (element * 4)`. Port of upstream's
/// `HandleIndexed` helper lambda in `load_store_attribute.cpp`.
fn handle_indexed<F>(tv: &mut TranslatorVisitor, index_reg: u32, num_elements: u32, mut f: F)
where
    F: FnMut(&mut TranslatorVisitor, u32, Value),
{
    let index_value = tv.x(index_reg);
    for element in 0..num_elements {
        let final_offset = if element == 0 {
            index_value.clone()
        } else {
            let imm = Value::ImmU32(element * 4);
            tv.ir.iadd_32(index_value.clone(), imm)
        };
        f(tv, element, final_offset);
    }
}

/// ALD — Attribute Load.
///
/// Upstream: `TranslatorVisitor::ALD(u64 insn)`. Supports both direct
/// attribute reads (via `GetAttribute`/`GetPatch`) and indexed reads
/// (via `GetAttributeIndexed`). Indirect patch reads panic — upstream
/// throws `NotImplementedException("Indirect patch read")`.
pub fn ald(tv: &mut TranslatorVisitor, insn: u64) {
    let dst = tv.dst_reg(insn);
    let index_reg = field(insn, 8, 8);
    let attr_offset = field(insn, 20, 10);
    let patch = bit(insn, 31);
    let vertex_reg = field(insn, 39, 8);
    let size = field(insn, 47, 2);

    if attr_offset % 4 != 0 {
        panic!("Unaligned absolute offset {}", attr_offset);
    }
    let vertex = tv.x(vertex_reg);
    let num = num_elements(size);
    if index_reg == Reg::RZ.0 as u32 {
        let attr_base = attr_offset / 4;
        for element in 0..num {
            if patch {
                let patch_id = Patch(attr_base + element);
                let value = tv.ir.get_patch(patch_id);
                tv.set_f(dst + element, value);
            } else {
                let attr = Attribute(attr_base + element);
                let value = tv.ir.get_attribute(attr, vertex.clone());
                tv.set_f(dst + element, value);
            }
        }
        return;
    }
    if patch {
        panic!("Indirect patch read");
    }
    handle_indexed(tv, index_reg, num, |tv, element, final_offset| {
        let value = tv.ir.get_attribute_indexed(final_offset, vertex.clone());
        tv.set_f(dst + element, value);
    });
}

/// AST — Attribute Store.
///
/// Upstream: `TranslatorVisitor::AST(u64 insn)`. Same patch + indexed
/// dispatch as ALD; indexed patch store panics matching upstream's
/// `NotImplementedException("Indexed tessellation patch store")`.
pub fn ast(tv: &mut TranslatorVisitor, insn: u64) {
    let src_reg = tv.dst_reg(insn);
    let index_reg = field(insn, 8, 8);
    let attr_offset = field(insn, 20, 10);
    let patch = bit(insn, 31);
    let vertex_reg = field(insn, 39, 8);
    let size = field(insn, 47, 2);

    if attr_offset % 4 != 0 {
        panic!("Unaligned absolute offset {}", attr_offset);
    }
    if index_reg != Reg::RZ.0 as u32 {
        panic!("Indexed store");
    }
    let vertex = tv.x(vertex_reg);
    let num = num_elements(size);
    if index_reg == Reg::RZ.0 as u32 {
        let attr_base = attr_offset / 4;
        for element in 0..num {
            let value = tv.f(src_reg + element);
            if patch {
                let patch_id = Patch(attr_base + element);
                tv.ir.set_patch(patch_id, value);
            } else {
                let attr = Attribute(attr_base + element);
                tv.ir.set_attribute(attr, value, vertex.clone());
            }
        }
        return;
    }
}

/// IPA — Interpolate Pixel Attribute.
///
/// Upstream: `TranslatorVisitor::IPA(u64 insn)`
pub fn ipa(tv: &mut TranslatorVisitor, insn: u64) {
    let dst = tv.dst_reg(insn);
    let index_reg = field(insn, 8, 8);
    let multiplier = field(insn, 20, 8);
    let attr = Attribute(field(insn, 30, 8));
    let indexed = bit(insn, 38);
    let saturated = bit(insn, 51);
    let _sample_mode = field(insn, 52, 2);
    let interpolation_mode = field(insn, 54, 2);

    let is_indexed = indexed && index_reg != Reg::RZ.0 as u32;
    let vertex = Value::ImmU32(0);
    let mut result = if is_indexed {
        let index_val = tv.x(index_reg);
        tv.ir.get_attribute_indexed(index_val, vertex)
    } else {
        tv.ir.get_attribute(attr, vertex)
    };

    let is_legacy = attr.is_legacy();
    if attr.is_generic() || is_legacy {
        let mut is_perspective = is_legacy && interpolation_mode != INTERPOLATION_MODE_SC;
        if !is_legacy {
            let sph = tv
                .sph
                .as_ref()
                .expect("IPA generic interpolation requires a program header");
            let input_map = sph.ps_generic_input_map(attr.generic_index());
            let effective_imap = input_map
                .into_iter()
                .find(|component| *component != PixelImap::Unused)
                .unwrap_or(PixelImap::Unused);
            is_perspective = matches!(effective_imap, PixelImap::Perspective | PixelImap::Unused);
        }
        if is_perspective {
            let position_w = tv.ir.get_attribute(Attribute::POSITION_W, Value::ImmU32(0));
            result = tv.ir.fp_mul_32(result, position_w);
        }
    }

    if interpolation_mode == INTERPOLATION_MODE_MULTIPLY {
        let multiplier = tv.f(multiplier);
        result = tv.ir.fp_mul_32(result, multiplier);
    }

    if saturated {
        if attr == Attribute::FRONT_FACE {
            panic!("IPA.SAT on FrontFace");
        }
        result = tv.ir.fp_saturate_32(result);
    }

    tv.set_f(dst, result);
}

fn num_elements(size: u32) -> u32 {
    match size {
        0 => 1,
        1 => 2,
        2 => 3,
        3 => 4,
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::opcodes::Opcode;
    use crate::ir::program::Program;
    use crate::ir::types::ShaderStage;
    use crate::program_header::ProgramHeader;

    fn ipa_insn(attr: Attribute, interpolation_mode: u32, saturated: bool) -> u64 {
        ((attr.0 as u64) << 30) | ((saturated as u64) << 51) | ((interpolation_mode as u64) << 54)
    }

    fn translate_ipa(
        attr: Attribute,
        interpolation_map: u8,
        interpolation_mode: u32,
    ) -> Vec<Opcode> {
        let mut program = Program::new(ShaderStage::Fragment);
        let block = program.add_block();
        let mut sph = ProgramHeader::default();
        sph.raw[6 + attr.generic_index() as usize / 4] =
            (interpolation_map as u32) << ((attr.generic_index() % 4) * 8);
        {
            let mut visitor = TranslatorVisitor::new_with_sph(&mut program, block, Some(sph));
            ipa(&mut visitor, ipa_insn(attr, interpolation_mode, false));
        }
        program.blocks[block as usize]
            .iter()
            .map(|inst| inst.opcode)
            .collect()
    }

    #[test]
    fn ipa_uses_first_active_component_interpolation_for_the_whole_vector() {
        // Mesa homebrew shaders may describe one vector as ScreenLinear followed by
        // Perspective. Upstream uses the first active component for every IPA in it.
        let opcodes = translate_ipa(Attribute::generic(0, 1), 0b10_10_10_11, 0);
        assert!(!opcodes.contains(&Opcode::FPMul32));
    }

    #[test]
    fn ipa_applies_perspective_to_perspective_and_unused_vectors() {
        for interpolation_map in [0b10_10_10_10, 0] {
            let opcodes = translate_ipa(Attribute::generic(0, 0), interpolation_map, 0);
            assert!(opcodes.contains(&Opcode::FPMul32));
        }
    }

    #[test]
    fn ipa_legacy_sc_skips_perspective_while_pass_applies_it() {
        for (mode, expected) in [(INTERPOLATION_MODE_SC, false), (0, true)] {
            let mut program = Program::new(ShaderStage::Fragment);
            let block = program.add_block();
            {
                let mut visitor = TranslatorVisitor::new(&mut program, block);
                ipa(
                    &mut visitor,
                    ipa_insn(Attribute::FOG_COORDINATE, mode, false),
                );
            }
            let has_mul = program.blocks[block as usize]
                .iter()
                .any(|inst| inst.opcode == Opcode::FPMul32);
            assert_eq!(has_mul, expected);
        }
    }

    #[test]
    #[should_panic(expected = "IPA.SAT on FrontFace")]
    fn ipa_rejects_saturated_front_face() {
        let mut program = Program::new(ShaderStage::Fragment);
        let block = program.add_block();
        let mut visitor = TranslatorVisitor::new(&mut program, block);
        ipa(&mut visitor, ipa_insn(Attribute::FRONT_FACE, 0, true));
    }

    #[test]
    fn dead_ipa_does_not_leave_stale_attribute_usage() {
        let mut program = Program::new(ShaderStage::Fragment);
        let block = program.add_block();
        let mut sph = ProgramHeader::default();
        sph.raw[6] = 0b10_10_10_10;
        {
            let mut visitor = TranslatorVisitor::new_with_sph(&mut program, block, Some(sph));
            ipa(&mut visitor, ipa_insn(Attribute::generic(0, 0), 0, false));
        }

        crate::ir_opt::dead_code_elimination_pass::dead_code_elimination_pass(&mut program);
        crate::ir_opt::collect_shader_info_pass::collect_shader_info_pass(&mut program);

        assert!(!program
            .info
            .loads
            .any_component(Attribute::POSITION_X.0 as usize));
        assert!(!program.info.loads.generic_any(0));
    }
}

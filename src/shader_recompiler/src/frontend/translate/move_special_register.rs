// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/move_special_register.cpp

use super::{field, TranslatorVisitor};
use crate::ir::value::Value;

/// Special register indices.
/// Matches upstream `enum class SpecialRegister : u64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpecialRegister {
    LaneId,
    InvocationId,
    YDirection,
    ThreadKill,
    Affinity,
    InvocationInfo,
    WscalefactorXy,
    WscalefactorZ,
    Tid,
    TidX,
    TidY,
    TidZ,
    CtaidX,
    CtaidY,
    CtaidZ,
    EqMask,
    LtMask,
    LeMask,
    GtMask,
    GeMask,
    Unknown(u32),
}

impl SpecialRegister {
    fn from_u32(v: u32) -> Self {
        match v {
            0 => SpecialRegister::LaneId,
            17 => SpecialRegister::InvocationId,
            18 => SpecialRegister::YDirection,
            19 => SpecialRegister::ThreadKill,
            28 => SpecialRegister::Affinity,
            29 => SpecialRegister::InvocationInfo,
            30 => SpecialRegister::WscalefactorXy,
            31 => SpecialRegister::WscalefactorZ,
            32 => SpecialRegister::Tid,
            33 => SpecialRegister::TidX,
            34 => SpecialRegister::TidY,
            35 => SpecialRegister::TidZ,
            37 => SpecialRegister::CtaidX,
            38 => SpecialRegister::CtaidY,
            39 => SpecialRegister::CtaidZ,
            56 => SpecialRegister::EqMask,
            57 => SpecialRegister::LtMask,
            58 => SpecialRegister::LeMask,
            59 => SpecialRegister::GtMask,
            60 => SpecialRegister::GeMask,
            other => SpecialRegister::Unknown(other),
        }
    }
}

/// S2R — Move special register to general-purpose register.
///
/// Matches upstream `TranslatorVisitor::S2R(u64 insn)`.
pub fn s2r(tv: &mut TranslatorVisitor, insn: u64) {
    let dest_reg = field(insn, 0, 8);
    let sr_idx = field(insn, 20, 8);
    let sr = SpecialRegister::from_u32(sr_idx);

    let result: Value = match sr {
        SpecialRegister::InvocationId => tv.ir.invocation_id(),
        SpecialRegister::ThreadKill => {
            let is_helper = tv.ir.is_helper_invocation();
            tv.ir
                .select_u32(is_helper, Value::ImmU32(u32::MAX), Value::ImmU32(0))
        }
        SpecialRegister::InvocationInfo => tv.ir.invocation_info(),
        SpecialRegister::Tid => {
            // Pack TID.x[7:0] in [7:0], TID.y[7:0] in [23:16], TID.z[5:0] in [31:26]
            // Matches upstream S2R / SR_TID handling. LocalInvocationId is a U32x3:
            // upstream `IREmitter::CompositeExtract` picks CompositeExtractU32x3 for it.
            let tid = tv.ir.local_invocation_id();
            let x = tv.ir.composite_extract_u32x3(tid, Value::ImmU32(0));
            let y = tv.ir.composite_extract_u32x3(tid, Value::ImmU32(1));
            let z = tv.ir.composite_extract_u32x3(tid, Value::ImmU32(2));
            let xy = tv
                .ir
                .bit_field_insert(x, y, Value::ImmU32(16), Value::ImmU32(8));
            tv.ir
                .bit_field_insert(xy, z, Value::ImmU32(26), Value::ImmU32(6))
        }
        // Upstream `IREmitter::LocalInvocationIdX/Y/Z` and `WorkgroupIdX/Y/Z`
        // (ir_emitter.cpp:344-370): CompositeExtract of the U32x3 vector, which
        // `IREmitter::CompositeExtract` emits as CompositeExtractU32x3.
        SpecialRegister::TidX => {
            let tid = tv.ir.local_invocation_id();
            tv.ir.composite_extract_u32x3(tid, Value::ImmU32(0))
        }
        SpecialRegister::TidY => {
            let tid = tv.ir.local_invocation_id();
            tv.ir.composite_extract_u32x3(tid, Value::ImmU32(1))
        }
        SpecialRegister::TidZ => {
            let tid = tv.ir.local_invocation_id();
            tv.ir.composite_extract_u32x3(tid, Value::ImmU32(2))
        }
        SpecialRegister::CtaidX => {
            let wg = tv.ir.workgroup_id();
            tv.ir.composite_extract_u32x3(wg, Value::ImmU32(0))
        }
        SpecialRegister::CtaidY => {
            let wg = tv.ir.workgroup_id();
            tv.ir.composite_extract_u32x3(wg, Value::ImmU32(1))
        }
        SpecialRegister::CtaidZ => {
            let wg = tv.ir.workgroup_id();
            tv.ir.composite_extract_u32x3(wg, Value::ImmU32(2))
        }
        SpecialRegister::WscalefactorXy => {
            log::warn!("S2R: SR_WSCALEFACTOR_XY stubbed");
            tv.ir.sr_w_scale_factor_xy()
        }
        SpecialRegister::WscalefactorZ => {
            log::warn!("S2R: SR_WSCALEFACTOR_Z stubbed");
            tv.ir.sr_w_scale_factor_z()
        }
        SpecialRegister::Affinity => {
            log::warn!("S2R: SR_AFFINITY stubbed, returning 0");
            Value::ImmU32(0)
        }
        SpecialRegister::EqMask => tv.ir.subgroup_eq_mask(),
        SpecialRegister::LtMask => tv.ir.subgroup_lt_mask(),
        SpecialRegister::LeMask => tv.ir.subgroup_le_mask(),
        SpecialRegister::GtMask => tv.ir.subgroup_gt_mask(),
        SpecialRegister::GeMask => tv.ir.subgroup_ge_mask(),
        SpecialRegister::YDirection => {
            // YDirection returns F32; bitcast to U32 to store in integer register
            let f = tv.ir.y_direction();
            tv.ir.bit_cast_u32_f32(f)
        }
        SpecialRegister::LaneId => tv.ir.lane_id(),
        SpecialRegister::Unknown(idx) => {
            log::warn!(
                "S2R: unknown special register {} (0x{:X}), returning 0",
                idx,
                idx
            );
            Value::ImmU32(0)
        }
    };

    tv.set_x(dest_reg, result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::opcodes::Opcode;
    use crate::ir::program::Program;
    use crate::ir::types::ShaderStage;

    #[test]
    fn w_scale_factor_special_registers_survive_to_the_backend() {
        for (special_register, expected) in [
            (30u64, Opcode::SRWScaleFactorXY),
            (31u64, Opcode::SRWScaleFactorZ),
        ] {
            let mut program = Program::new(ShaderStage::VertexB);
            program.blocks.push(Block::new());
            let mut visitor = TranslatorVisitor::new(&mut program, 0);

            s2r(&mut visitor, special_register << 20);

            let opcodes: Vec<_> = visitor.ir.program.blocks[0]
                .iter()
                .map(|inst| inst.opcode)
                .collect();
            assert_eq!(opcodes, vec![expected, Opcode::SetRegister]);
        }
    }

    /// Upstream `IREmitter::LocalInvocationIdX/Y/Z` and `WorkgroupIdX/Y/Z` extract from
    /// the U32x3 vector with CompositeExtractU32x3, which the native storage write extent
    /// analysis (global_memory_to_storage_buffer_pass) matches to bound stores by the ids.
    #[test]
    fn invocation_and_workgroup_ids_extract_from_a_u32x3() {
        for (special_register, vector, element) in [
            (33u64, Opcode::LocalInvocationId, 0u32),
            (34, Opcode::LocalInvocationId, 1),
            (35, Opcode::LocalInvocationId, 2),
            (37, Opcode::WorkgroupId, 0),
            (38, Opcode::WorkgroupId, 1),
            (39, Opcode::WorkgroupId, 2),
        ] {
            let mut program = Program::new(ShaderStage::Compute);
            program.blocks.push(Block::new());
            let mut visitor = TranslatorVisitor::new(&mut program, 0);

            s2r(&mut visitor, special_register << 20);

            let insts: Vec<_> = visitor.ir.program.blocks[0].iter().collect();
            let opcodes: Vec<_> = insts.iter().map(|inst| inst.opcode).collect();
            assert_eq!(
                opcodes,
                vec![vector, Opcode::CompositeExtractU32x3, Opcode::SetRegister],
                "special register {special_register}"
            );
            assert_eq!(insts[1].args[1], Value::ImmU32(element));
        }
        // SR_TID packs the three components, each a CompositeExtractU32x3.
        let mut program = Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        let mut visitor = TranslatorVisitor::new(&mut program, 0);
        s2r(&mut visitor, 32u64 << 20);
        let extracts = visitor.ir.program.blocks[0]
            .iter()
            .filter(|inst| inst.opcode == Opcode::CompositeExtractU32x3)
            .count();
        assert_eq!(extracts, 3);
        assert!(visitor.ir.program.blocks[0]
            .iter()
            .all(|inst| inst.opcode != Opcode::CompositeExtractU32x2));
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later

//! Native control-patch transport. Eden's EmitContext owns the corresponding
//! per-vertex arrays and patch variables; Metal carries them between compute
//! and post-tessellation vertex stages in retained device buffers instead.

use crate::ir::{Attribute, Patch, Program};
use crate::runtime_info::{AttributeType, RuntimeInfo};
use crate::stage::Stage;

use super::MslError;

#[derive(Debug, Clone)]
pub struct TessellationControlLayout {
    pub output_vertices: u32,
    pub input_generics: u32,
    producer_input_generics: u32,
    pub output_generics: u32,
    pub patch_generics: u32,
}

impl TessellationControlLayout {
    pub fn new(program: &Program, runtime: &RuntimeInfo) -> Result<Self, MslError> {
        if program.stage != Stage::TessellationControl || !(1..=32).contains(&program.invocations) {
            return Err(MslError::UnsupportedProgramFeature(
                "tessellation control invocation count",
            ));
        }
        let mut input_generics = 0;
        let mut producer_input_generics = 0;
        let mut output_generics = 0;
        let mut patch_generics = 0;
        for index in 0..32 {
            if runtime.previous_stage_stores.generic_any(index) {
                producer_input_generics |= 1 << index;
            }
            if program.info.loads.generic_any(index)
                && runtime.previous_stage_stores.generic_any(index)
                && runtime.generic_input_types[index] != AttributeType::Disabled
            {
                input_generics |= 1 << index;
            }
            if program.info.stores.generic_any(index) {
                output_generics |= 1 << index;
            }
        }
        for (index, used) in program.info.uses_patches.iter().enumerate() {
            if *used {
                patch_generics |= 1 << index;
            }
        }
        Ok(Self {
            output_vertices: program.invocations,
            input_generics,
            producer_input_generics,
            output_generics,
            patch_generics,
        })
    }

    pub fn declarations(&self) -> String {
        let mut result = String::new();
        for (name, generics) in [
            // The vertex producer transports all its generic outputs. Removing
            // an unread field here would change every subsequent record's stride.
            ("MslControlInput", self.producer_input_generics),
            ("MslControlOutput", self.output_generics),
        ] {
            result.push_str(&format!("struct {name} {{\n    float4 position;\n"));
            if name == "MslControlOutput" {
                result.push_str("    float point_size;\n    float clip_distance[8];\n");
            }
            for index in 0..32 {
                if generics & (1 << index) != 0 {
                    result.push_str(&format!("    float4 attr{index};\n"));
                }
            }
            result.push_str("};\n\n");
        }
        // Keep factors as guest f32 until the native tessellator upload converts
        // them to half. Do not conflate the four outer/two inner index spaces.
        result.push_str("struct MslControlPatch {\n    float outer[4];\n    float inner[2];\n");
        for index in 0..30 {
            if self.patch_generics & (1 << index) != 0 {
                result.push_str(&format!("    float4 generic{index};\n"));
            }
        }
        result.push_str("};\n\n");
        result
    }

    pub fn input_stride(&self) -> usize {
        16 * (1 + self.producer_input_generics.count_ones() as usize)
    }

    pub fn input_generic_mask(&self) -> u32 {
        self.producer_input_generics
    }

    pub fn output_stride(&self) -> usize {
        // float4 position, float point size, eight scalar clip distances,
        // then vec4-aligned generic outputs; the struct itself is aligned to 16.
        64 + 16 * self.output_generics.count_ones() as usize
    }

    pub fn patch_stride(&self) -> usize {
        if self.patch_generics == 0 {
            24
        } else {
            32 + 16 * self.patch_generics.count_ones() as usize
        }
    }

    pub fn input_expression(&self, attribute: Attribute, vertex: &str) -> Result<String, MslError> {
        if attribute == Attribute::PRIMITIVE_ID {
            return Ok("as_type<float>(patch_id)".into());
        }
        if attribute.is_position() {
            let component = "xyzw".as_bytes()[attribute.position_element() as usize] as char;
            return Ok(format!("patch_input[{vertex}].position.{component}"));
        }
        if attribute.is_generic() {
            let index = attribute.generic_index();
            let element = attribute.generic_element();
            // Eden DefineInputs gates the complete vec4 by Generic(index).
            // Defaults apply only when no interface variable was declared.
            if self.input_generics & (1 << index) == 0 {
                return Ok(if element == 3 { "1.0f" } else { "0.0f" }.into());
            }
            let component = "xyzw".as_bytes()[element as usize] as char;
            return Ok(format!("patch_input[{vertex}].attr{index}.{component}"));
        }
        Err(MslError::UnsupportedAttribute(attribute.0))
    }

    pub fn output_expression(&self, attribute: Attribute) -> Result<String, MslError> {
        let field = if attribute.is_position() {
            format!(
                "position.{}",
                "xyzw".as_bytes()[attribute.position_element() as usize] as char
            )
        } else if attribute.is_generic()
            && self.output_generics & (1 << attribute.generic_index()) != 0
        {
            format!(
                "attr{}.{}",
                attribute.generic_index(),
                "xyzw".as_bytes()[attribute.generic_element() as usize] as char
            )
        } else if attribute == Attribute::POINT_SIZE {
            "point_size".into()
        } else if attribute.is_clip_distance() {
            format!("clip_distance[{}]", attribute.clip_distance_index())
        } else {
            return Err(MslError::UnsupportedAttribute(attribute.0));
        };
        // OutputAttrPointer in Eden indexes TCS stores by InvocationId. The IR
        // SetAttribute vertex argument is not an arbitrary output destination.
        Ok(format!("patch_output[invocation_id].{field}"))
    }

    pub fn patch_expression(&self, patch: Patch, writing: bool) -> Result<String, MslError> {
        if patch.is_generic() && self.patch_generics & (1 << patch.generic_index()) != 0 {
            return Ok(format!(
                "patch.generic{}.{}",
                patch.generic_index(),
                "xyzw".as_bytes()[patch.generic_element() as usize] as char
            ));
        }
        if writing {
            match patch.0 {
                0..=3 => return Ok(format!("patch.outer[{}]", patch.0)),
                4..=5 => return Ok(format!("patch.inner[{}]", patch.0 - 4)),
                _ => {}
            }
        }
        Err(MslError::UnsupportedProgramFeature(
            "unsupported tessellation patch access",
        ))
    }
}

/// TES consumes the producer's concrete control-point/patch types as template
/// arguments. A consumer-only struct would change the stride whenever TCS
/// writes an attribute that TES does not read.
#[derive(Debug, Clone)]
pub struct TessellationEvaluationLayout {
    input_generics: u32,
    patch_generics: u32,
}

impl TessellationEvaluationLayout {
    pub fn new(program: &Program, runtime: &RuntimeInfo) -> Result<Self, MslError> {
        if program.stage != Stage::TessellationEval {
            return Err(MslError::UnsupportedStage(program.stage));
        }
        if program.info.uses_subgroup_shuffles
            || program.info.uses_subgroup_vote
            || program.info.uses_subgroup_mask
            || program.info.uses_fswzadd
            || !evaluation_lane_uses_are_ignored(program)
        {
            return Err(MslError::UnsupportedProgramFeature(
                "observable subgroup operation in post-tessellation vertex stage",
            ));
        }
        let mut input_generics = 0;
        for index in 0..32 {
            if program.info.loads.generic_any(index)
                && runtime.previous_stage_stores.generic_any(index)
                && runtime.generic_input_types[index] != AttributeType::Disabled
            {
                input_generics |= 1 << index;
            }
        }
        let mut patch_generics = 0;
        for (index, used) in program.info.uses_patches.iter().enumerate() {
            if *used {
                patch_generics |= 1 << index;
            }
        }
        Ok(Self {
            input_generics,
            patch_generics,
        })
    }

    pub fn input_expression(&self, attribute: Attribute, vertex: &str) -> Result<String, MslError> {
        match attribute {
            Attribute::TESSELLATION_EVALUATION_POINT_U => return Ok("tess_coord.x".into()),
            Attribute::TESSELLATION_EVALUATION_POINT_V => return Ok("tess_coord.y".into()),
            Attribute::PRIMITIVE_ID => return Ok("as_type<float>(patch_id)".into()),
            _ => {}
        }
        if attribute.is_position() {
            let component = "xyzw".as_bytes()[attribute.position_element() as usize] as char;
            return Ok(format!("patch_input[{vertex}].position.{component}"));
        }
        if attribute.is_generic() {
            let element = attribute.generic_element();
            if self.input_generics & (1 << attribute.generic_index()) == 0 {
                return Ok(if element == 3 { "1.0f" } else { "0.0f" }.into());
            }
            let component = "xyzw".as_bytes()[element as usize] as char;
            return Ok(format!(
                "patch_input[{vertex}].attr{}.{component}",
                attribute.generic_index()
            ));
        }
        Err(MslError::UnsupportedAttribute(attribute.0))
    }

    pub fn patch_expression(&self, patch: Patch) -> Result<String, MslError> {
        if patch.is_generic() && self.patch_generics & (1 << patch.generic_index()) != 0 {
            return Ok(format!(
                "patch.generic{}.{}",
                patch.generic_index(),
                "xyzw".as_bytes()[patch.generic_element() as usize] as char
            ));
        }
        Err(MslError::UnsupportedProgramFeature(
            "non-generic tessellation patch load",
        ))
    }
}

// Metal post-tessellation vertex functions have no SIMD lane builtin. Permit
// only values whose every use is an ignored built-in vertex operand, as in
// Eden EmitGetAttribute(TessCoord/PrimitiveId); never invent an observable lane.
fn evaluation_lane_uses_are_ignored(program: &Program) -> bool {
    use crate::ir::{Opcode, Value};
    let is_lane = |value: &Value| match value {
        Value::Inst(reference) => program
            .blocks
            .get(reference.block as usize)
            .and_then(|block| block.instructions.get(reference.inst as usize))
            .and_then(Option::as_ref)
            .is_some_and(|inst| inst.opcode == Opcode::LaneId),
        _ => false,
    };
    program
        .blocks
        .iter()
        .flat_map(|block| block.iter())
        .all(|inst| {
            inst.phi_args.iter().all(|(_, value)| !is_lane(value))
                && inst.args.iter().enumerate().all(|(index, value)| {
                    !is_lane(value)
                        || (inst.opcode == Opcode::GetAttribute
                            && index == 1
                            && matches!(
                                inst.arg(0),
                                Value::Attribute(
                                    Attribute::TESSELLATION_EVALUATION_POINT_U
                                        | Attribute::TESSELLATION_EVALUATION_POINT_V
                                        | Attribute::PRIMITIVE_ID
                                )
                            ))
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{
        bindings::Bindings,
        msl::{emit_msl::emit_msl_tessellation_control_function, MslOptions},
    };
    use crate::ir::{emitter::Emitter, SyntaxNode, Value};
    use crate::ir_opt::collect_shader_info_pass::collect_shader_info_pass;
    use crate::profile::Profile;

    #[test]
    fn control_transport_strides_follow_msl_alignment() {
        let mut program = Program::new(Stage::TessellationControl);
        program.invocations = 3;
        let mut runtime = RuntimeInfo::default();
        let layout = TessellationControlLayout::new(&program, &runtime).unwrap();
        assert_eq!((layout.input_stride(), layout.output_stride(), layout.patch_stride()), (16, 64, 24));
        runtime.previous_stage_stores.set(Attribute::generic(29, 0).0 as usize, true);
        program.info.stores.set(Attribute::generic(31, 0).0 as usize, true);
        program.info.uses_patches[29] = true;
        let layout = TessellationControlLayout::new(&program, &runtime).unwrap();
        assert_eq!((layout.input_stride(), layout.output_stride(), layout.patch_stride()), (32, 80, 48));
        assert_eq!(layout.input_generic_mask(), 1 << 29);
        for index in 0..32 {
            runtime.previous_stage_stores.set(Attribute::generic(index, 0).0 as usize, true);
            program.info.stores.set(Attribute::generic(index, 0).0 as usize, true);
        }
        program.info.uses_patches.fill(true);
        let layout = TessellationControlLayout::new(&program, &runtime).unwrap();
        assert_eq!((layout.input_stride(), layout.output_stride(), layout.patch_stride()), (528, 576, 512));
        assert_eq!(layout.input_generic_mask(), u32::MAX);
    }

    #[test]
    fn control_transport_preserves_patch_and_invocation_ownership() {
        let mut program = Program::new(Stage::TessellationControl);
        program.invocations = 3;
        program.add_block();
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        let mut ir = Emitter::new(&mut program, 0);
        ir.prologue();
        let invocation = ir.invocation_id();
        let input = ir.get_attribute(Attribute::generic(2, 0), invocation);
        // This operand is deliberately different: Eden indexes the output by
        // InvocationId, not the SetAttribute vertex operand.
        ir.set_attribute(Attribute::generic(2, 0), input, Value::ImmU32(29));
        let info = ir.invocation_info();
        let info = ir.bit_cast_f32_u32(info);
        ir.set_attribute(Attribute::POSITION_X, info, Value::ImmU32(0));
        ir.set_patch(Patch::generic(1, 2), input);
        ir.barrier();
        let value = ir.get_patch(Patch::generic(1, 2));
        ir.set_patch(Patch::TESS_LOD_INTERIOR_U, value);
        ir.epilogue();
        collect_shader_info_pass(&mut program);
        let mut runtime = RuntimeInfo::default();
        runtime
            .previous_stage_stores
            .set(Attribute::generic(2, 0).0 as usize, true);
        let emitted = emit_msl_tessellation_control_function(
            &program,
            &Profile::default(),
            &runtime,
            &MslOptions::default(),
            &mut Bindings::default(),
        )
        .unwrap();
        let source = emitted.source.source;
        assert_eq!(emitted.entry_point, "ruzu_control");
        assert!(source.contains("patch_output[invocation_id].attr2.x ="));
        assert!(!source.contains("patch_output[0x0000001Du]"));
        assert!(source.contains("patch_input[v_0_1].attr2.x"));
        assert!(source.contains("patch_vertices << 16u"));
        assert!(source.contains("patch.generic1.z"));
        assert!(source.contains("patch.inner[0]"));
        assert!(source
            .contains("threadgroup_barrier(mem_flags::mem_device | mem_flags::mem_threadgroup)"));
        assert!(!source.contains("return output"));
        assert!(!source.contains("[[position]]"));
        assert!(!source.contains("[[buffer("));
    }

    #[test]
    fn control_input_defaults_require_an_absent_or_disabled_interface() {
        let mut program = Program::new(Stage::TessellationControl);
        program.invocations = 3;
        for component in 0..4 {
            program
                .info
                .loads
                .set(Attribute::generic(7, component).0 as usize, true);
        }
        let mut runtime = RuntimeInfo::default();
        runtime
            .previous_stage_stores
            .set(Attribute::generic(7, 0).0 as usize, true);
        let layout = TessellationControlLayout::new(&program, &runtime).unwrap();
        assert_eq!(
            layout
                .input_expression(Attribute::generic(7, 0), "id")
                .unwrap(),
            "patch_input[id].attr7.x"
        );
        assert_eq!(
            layout
                .input_expression(Attribute::generic(7, 1), "id")
                .unwrap(),
            "patch_input[id].attr7.y"
        );
        assert_eq!(
            layout
                .input_expression(Attribute::generic(7, 2), "id")
                .unwrap(),
            "patch_input[id].attr7.z"
        );
        assert_eq!(
            layout
                .input_expression(Attribute::generic(7, 3), "id")
                .unwrap(),
            "patch_input[id].attr7.w"
        );
        assert_eq!(
            layout
                .input_expression(Attribute::generic(8, 0), "id")
                .unwrap(),
            "0.0f"
        );
        assert_eq!(
            layout
                .input_expression(Attribute::generic(8, 3), "id")
                .unwrap(),
            "1.0f"
        );
        runtime.generic_input_types[7] = AttributeType::Disabled;
        let disabled = TessellationControlLayout::new(&program, &runtime).unwrap();
        assert_eq!(
            disabled
                .input_expression(Attribute::generic(7, 0), "id")
                .unwrap(),
            "0.0f"
        );
        assert_eq!(
            disabled
                .input_expression(Attribute::generic(7, 3), "id")
                .unwrap(),
            "1.0f"
        );
    }

    #[test]
    fn control_input_transport_keeps_unread_producer_attributes() {
        let mut program = Program::new(Stage::TessellationControl);
        program.invocations = 3;
        program.info.loads.set(Attribute::generic(7, 0).0 as usize, true);
        let mut runtime = RuntimeInfo::default();
        for index in [1, 7, 29] { runtime.previous_stage_stores.set(Attribute::generic(index, 0).0 as usize, true); }
        let layout = TessellationControlLayout::new(&program, &runtime).unwrap();
        assert_eq!(layout.input_generics, 1 << 7);
        assert_eq!(layout.input_stride(), 64);
        let declaration = layout.declarations();
        let input = declaration.split("struct MslControlOutput").next().unwrap();
        assert!(input.contains("float4 attr1;"));
        assert!(input.contains("float4 attr7;"));
        assert!(input.contains("float4 attr29;"));
        assert_eq!(layout.input_expression(Attribute::generic(7, 0), "vertex").unwrap(), "patch_input[vertex].attr7.x");
        runtime.generic_input_types[7] = AttributeType::Disabled;
        let disabled = TessellationControlLayout::new(&program, &runtime).unwrap();
        assert_eq!(disabled.input_stride(), 64, "disabling a consumer input cannot change producer stride");
        assert_eq!(disabled.input_expression(Attribute::generic(7, 0), "vertex").unwrap(), "0.0f");
    }

    #[test]
    fn control_factor_indices_match_guest_outer_inner_arrays() {
        let mut program = Program::new(Stage::TessellationControl);
        program.invocations = 32;
        let layout = TessellationControlLayout::new(&program, &RuntimeInfo::default()).unwrap();
        assert_eq!(layout.output_vertices, 32);
        for (patch, expected) in [
            (0, "patch.outer[0]"),
            (1, "patch.outer[1]"),
            (2, "patch.outer[2]"),
            (3, "patch.outer[3]"),
            (4, "patch.inner[0]"),
            (5, "patch.inner[1]"),
        ] {
            assert_eq!(
                layout.patch_expression(Patch(patch), true).unwrap(),
                expected
            );
            assert!(
                layout.patch_expression(Patch(patch), false).is_err(),
                "Eden does not load non-generic patches"
            );
        }
        assert!(layout.patch_expression(Patch(126), true).is_err());
        for invalid in [0, 33] {
            program.invocations = invalid;
            assert!(TessellationControlLayout::new(&program, &RuntimeInfo::default()).is_err());
        }
    }

    #[test]
    fn callable_control_does_not_advertise_a_complete_tessellation_entry_point() {
        use crate::backend::msl::emit_msl::emit_msl_with_options_and_bindings;
        let program = Program::new(Stage::TessellationControl);
        assert!(emit_msl_with_options_and_bindings(
            &program,
            &Profile::default(),
            &RuntimeInfo::default(),
            &MslOptions::default(),
            &mut Bindings::default()
        )
        .is_err());
        for stage in [
            Stage::VertexB,
            Stage::Fragment,
            Stage::Geometry,
            Stage::Compute,
            Stage::TessellationEval,
        ] {
            assert!(emit_msl_tessellation_control_function(
                &Program::new(stage),
                &Profile::default(),
                &RuntimeInfo::default(),
                &MslOptions::default(),
                &mut Bindings::default()
            )
            .is_err());
        }
    }

    #[test]
    fn evaluation_uses_producer_types_and_ignores_domain_coordinate_vertex_operand() {
        use crate::backend::msl::emit_msl::emit_msl_tessellation_evaluation_function;
        let mut program = Program::new(Stage::TessellationEval);
        program.add_block();
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        let mut ir = Emitter::new(&mut program, 0);
        ir.prologue();
        let lane = ir.lane_id();
        let u = ir.get_attribute(Attribute::TESSELLATION_EVALUATION_POINT_U, lane);
        let v = ir.get_attribute(Attribute::TESSELLATION_EVALUATION_POINT_V, lane);
        let vertex = ir.get_attribute(Attribute::generic(4, 0), Value::ImmU32(2));
        let patch = ir.get_patch(Patch::generic(6, 1));
        let id = ir.get_attribute(Attribute::PRIMITIVE_ID, Value::ImmU32(19));
        let info = ir.invocation_info();
        let info = ir.bit_cast_f32_u32(info);
        for (attribute, value) in [
            (Attribute::POSITION_X, u),
            (Attribute::POSITION_Y, v),
            (Attribute::POSITION_Z, vertex),
            (Attribute::POSITION_W, Value::ImmF32(1.0)),
            (Attribute::generic(0, 0), patch),
            (Attribute::generic(0, 1), id),
            (Attribute::generic(0, 2), info),
        ] {
            ir.set_attribute(attribute, value, Value::ImmU32(0));
        }
        ir.epilogue();
        collect_shader_info_pass(&mut program);
        let mut runtime = RuntimeInfo::default();
        runtime
            .previous_stage_stores
            .set(Attribute::generic(4, 0).0 as usize, true);
        let artifact = emit_msl_tessellation_evaluation_function(
            &program,
            &Profile::default(),
            &runtime,
            &MslOptions::default(),
            &mut Bindings::default(),
        )
        .unwrap();
        let source = &artifact.source.source;
        assert_eq!(artifact.entry_point, "ruzu_evaluate");
        assert!(source.contains("template<typename ControlPoint, typename PatchData>"));
        assert!(source.contains("const device ControlPoint* patch_input"));
        assert!(!source.contains("struct ControlPoint"));
        assert!(source.contains("patch_input[0x00000002u].attr4.x"));
        assert!(source.contains("tess_coord.x"));
        assert!(source.contains("tess_coord.y"));
        assert!(source.contains("patch.generic6.y"));
        assert!(source.contains("patch_vertices << 16u"));
        assert!(source.contains("as_type<float>(patch_id)"));
        assert!(source.contains("return output"));
        assert!(!source.contains("[[stage_in]]"));
        assert!(
            !source.contains("v_0_1 ="),
            "ignored lane must not be synthesized as zero"
        );
    }

    #[test]
    fn evaluation_layout_rejects_patch_stores_and_wrong_stage() {
        use crate::backend::msl::emit_msl::emit_msl_tessellation_evaluation_function;
        let mut program = Program::new(Stage::TessellationEval);
        program.add_block();
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        Emitter::new(&mut program, 0).set_patch(Patch::generic(0, 0), Value::ImmF32(1.0));
        collect_shader_info_pass(&mut program);
        assert!(emit_msl_tessellation_evaluation_function(
            &program,
            &Profile::default(),
            &RuntimeInfo::default(),
            &MslOptions::default(),
            &mut Bindings::default()
        )
        .is_err());
        assert!(TessellationEvaluationLayout::new(
            &Program::new(Stage::VertexB),
            &RuntimeInfo::default()
        )
        .is_err());
        let layout = TessellationEvaluationLayout::new(&program, &RuntimeInfo::default()).unwrap();
        assert!(layout.patch_expression(Patch::TESS_LOD_LEFT).is_err());
        assert_eq!(
            layout
                .input_expression(Attribute::generic(31, 3), "id")
                .unwrap(),
            "1.0f"
        );
    }

    #[test]
    fn evaluation_rejects_observable_lane_instead_of_substituting_zero() {
        use crate::backend::msl::emit_msl::emit_msl_tessellation_evaluation_function;
        let mut program = Program::new(Stage::TessellationEval);
        program.add_block();
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        let mut ir = Emitter::new(&mut program, 0);
        let lane = ir.lane_id();
        let value = ir.bit_cast_f32_u32(lane);
        ir.set_attribute(Attribute::POSITION_X, value, Value::ImmU32(0));
        collect_shader_info_pass(&mut program);
        assert!(emit_msl_tessellation_evaluation_function(
            &program,
            &Profile::default(),
            &RuntimeInfo::default(),
            &MslOptions::default(),
            &mut Bindings::default()
        )
        .is_err());
    }
}

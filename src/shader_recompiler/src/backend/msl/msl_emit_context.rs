// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! MSL source-emission context.
//!
//! The context owns native MSL source construction and the mapping from the
//! common IR's stable `InstRef` values to MSL SSA locals. It does not own or
//! duplicate Maxwell translation passes.

use std::collections::HashMap;

use crate::ir::instruction::Inst;
use crate::ir::types::Type;
use crate::ir::value::{InstRef, Value};
use crate::stage::Stage;

use super::{
    MslBindingLayout, MslError, MslOptions, MslShaderArtifact, MslShaderSource, MslVersion,
};

pub struct MslEmitContext {
    stage: Stage,
    source: String,
    definitions: HashMap<InstRef, String>,
    returns_output: bool,
    uses_no_contraction_add: bool,
    uses_no_contraction_mul: bool,
    language_version: MslVersion,
}

impl MslEmitContext {
    pub fn new(program: &crate::ir::Program, options: &MslOptions) -> Result<Self, MslError> {
        let stage = program.stage;
        match stage {
            Stage::VertexA => return Err(MslError::UnmergedVertexA),
            Stage::VertexB | Stage::Fragment => {}
            Stage::TessellationControl
            | Stage::TessellationEval
            | Stage::Geometry
            | Stage::Compute => return Err(MslError::UnsupportedStage(stage)),
        }

        let mut source = String::new();
        let returns_output = match stage {
            Stage::VertexB => {
                source.push_str(concat!(
                    "struct MslVertexOut {\n",
                    "    float4 position [[position]];\n",
                    "};\n\n",
                    "vertex MslVertexOut main0() {\n",
                    "    MslVertexOut output = {};\n",
                    "    output.position = float4(0.0f);\n",
                ));
                true
            }
            Stage::Fragment if program.info.stores_frag_color.iter().any(|store| *store) => {
                source.push_str("struct MslFragmentOut {\n");
                for (index, stored) in program.info.stores_frag_color.iter().enumerate() {
                    if *stored {
                        source.push_str(&format!("    float4 color{index} [[color({index})]];\n"));
                    }
                }
                source.push_str("};\n\nfragment MslFragmentOut main0() {\n");
                source.push_str("    MslFragmentOut output = {};\n");
                true
            }
            Stage::Fragment => {
                source.push_str("fragment void main0() {\n");
                false
            }
            _ => unreachable!("stage was validated above"),
        };

        Ok(Self {
            stage,
            source,
            definitions: HashMap::new(),
            returns_output,
            uses_no_contraction_add: false,
            uses_no_contraction_mul: false,
            language_version: options.language_version,
        })
    }

    fn type_name(ty: Type) -> Result<&'static str, MslError> {
        match ty {
            Type::U1 => Ok("bool"),
            Type::U32 => Ok("uint"),
            Type::F32 => Ok("float"),
            _ => Err(MslError::UnsupportedType(ty)),
        }
    }

    fn unsupported_value_name(value: &Value) -> &'static str {
        match value {
            Value::Inst(_) => "undefined instruction",
            Value::Reg(_) => "register",
            Value::Pred(_) => "predicate",
            Value::Attribute(_) => "attribute",
            Value::Patch(_) => "patch",
            Value::ImmU1(_) => "u1 immediate",
            Value::ImmU8(_) => "u8 immediate",
            Value::ImmU16(_) => "u16 immediate",
            Value::ImmU32(_) => "u32 immediate",
            Value::ImmU64(_) => "u64 immediate",
            Value::ImmF16(_) => "f16 immediate",
            Value::ImmF32(_) => "f32 immediate",
            Value::ImmF64(_) => "f64 immediate",
            Value::Void => "void",
        }
    }

    pub fn value_expression(
        &self,
        value: &Value,
        inst_ref: InstRef,
        arg: u32,
    ) -> Result<String, MslError> {
        match value {
            Value::Inst(reference) => {
                self.definitions
                    .get(reference)
                    .cloned()
                    .ok_or(MslError::UnsupportedValue {
                        block: inst_ref.block,
                        inst: inst_ref.inst,
                        arg,
                        value: "undefined instruction",
                    })
            }
            Value::ImmU1(value) => Ok(if *value { "true" } else { "false" }.to_owned()),
            Value::ImmU32(value) => Ok(format!("0x{value:08X}u")),
            Value::ImmF32(value) => Ok(format!("as_type<float>(0x{:08X}u)", value.to_bits())),
            other => Err(MslError::UnsupportedValue {
                block: inst_ref.block,
                inst: inst_ref.inst,
                arg,
                value: Self::unsupported_value_name(other),
            }),
        }
    }

    pub fn define(
        &mut self,
        inst_ref: InstRef,
        ty: Type,
        expression: String,
        precise: bool,
    ) -> Result<(), MslError> {
        let name = format!("v_{}_{}", inst_ref.block, inst_ref.inst);
        debug_assert!(!precise, "precision must be expressed by the MSL operation");
        self.source.push_str(&format!(
            "    {} {name} = {expression};\n",
            Self::type_name(ty)?
        ));
        self.definitions.insert(inst_ref, name);
        Ok(())
    }

    pub fn emit_binary(
        &mut self,
        program: &crate::ir::Program,
        inst_ref: InstRef,
        inst: &Inst,
        ty: Type,
        operator: &'static str,
    ) -> Result<(), MslError> {
        self.emit_binary_with_precision(program, inst_ref, inst, ty, operator, false)
    }

    pub fn emit_binary_with_precision(
        &mut self,
        _program: &crate::ir::Program,
        inst_ref: InstRef,
        inst: &Inst,
        ty: Type,
        operator: &'static str,
        precise: bool,
    ) -> Result<(), MslError> {
        let lhs = self.value_expression(inst.arg(0), inst_ref, 0)?;
        let rhs = self.value_expression(inst.arg(1), inst_ref, 1)?;
        let expression = if precise {
            match operator {
                "+" => {
                    self.uses_no_contraction_add = true;
                    format!("spvFAdd({lhs}, {rhs})")
                }
                "*" => {
                    self.uses_no_contraction_mul = true;
                    format!("spvFMul({lhs}, {rhs})")
                }
                _ => {
                    return Err(MslError::UnsupportedProgramFeature(
                        "NoContraction operation",
                    ))
                }
            }
        } else {
            format!("({lhs}) {operator} ({rhs})")
        };
        self.define(inst_ref, ty, expression, false)
    }

    pub fn emit_identity(
        &mut self,
        program: &crate::ir::Program,
        inst_ref: InstRef,
        inst: &Inst,
    ) -> Result<(), MslError> {
        let expression = self.value_expression(inst.arg(0), inst_ref, 0)?;
        let ty = match inst.arg(0) {
            Value::Inst(reference) => program
                .block(reference.block)
                .inst(reference.inst)
                .return_type(),
            value => value.ir_type(),
        };
        self.define(inst_ref, ty, expression, false)
    }

    pub fn emit_set_position(
        &mut self,
        inst_ref: InstRef,
        component: u32,
        value: &Value,
    ) -> Result<(), MslError> {
        let expression = self.value_expression(value, inst_ref, 1)?;
        let swizzle = ["x", "y", "z", "w"][component as usize];
        self.source
            .push_str(&format!("    output.position.{swizzle} = {expression};\n"));
        Ok(())
    }

    pub fn emit_set_frag_color(
        &mut self,
        inst_ref: InstRef,
        render_target: u32,
        component: u32,
        value: &Value,
    ) -> Result<(), MslError> {
        let expression = self.value_expression(value, inst_ref, 2)?;
        let swizzle = ["x", "y", "z", "w"][component as usize];
        self.source.push_str(&format!(
            "    output.color{render_target}.{swizzle} = {expression};\n"
        ));
        Ok(())
    }

    pub fn finish(mut self) -> MslShaderArtifact {
        if self.returns_output {
            self.source.push_str("    return output;\n");
        }
        self.source.push_str("}\n");
        let mut source = String::from("#include <metal_stdlib>\nusing namespace metal;\n\n");
        if self.uses_no_contraction_add {
            source.push_str(concat!(
                "template<typename T>\n",
                "[[clang::optnone]] T spvFAdd(T lhs, T rhs) {\n",
                "    return fma(T(1), lhs, rhs);\n",
                "}\n\n",
            ));
        }
        if self.uses_no_contraction_mul {
            source.push_str(concat!(
                "template<typename T>\n",
                "[[clang::optnone]] T spvFMul(T lhs, T rhs) {\n",
                "    return fma(lhs, rhs, T(0));\n",
                "}\n\n",
            ));
        }
        source.push_str(&self.source);
        MslShaderArtifact {
            source: MslShaderSource {
                source,
                stage: self.stage,
            },
            bindings: MslBindingLayout::default(),
            entry_point: "main0".to_owned(),
            language_version: self.language_version,
        }
    }
}

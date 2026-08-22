// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! MSL source-emission context.
//!
//! The context owns native MSL source construction and the mapping from the
//! common IR's stable `InstRef` values to MSL SSA locals. It does not own or
//! duplicate Maxwell translation passes.

use std::collections::HashMap;

use crate::backend::bindings::Bindings;
use crate::ir::instruction::Inst;
use crate::ir::types::Type;
use crate::ir::value::{InstRef, Value};
use crate::profile::Profile;
use crate::stage::Stage;

use super::{
    MslBindingLayout, MslError, MslExecutionInfo, MslOptions, MslResourceBinding, MslResourceKind,
    MslShaderArtifact, MslShaderSource, MslVersion,
};

pub struct MslEmitContext {
    stage: Stage,
    source: String,
    definitions: HashMap<InstRef, String>,
    constant_buffers: HashMap<u32, String>,
    bindings: MslBindingLayout,
    returns_output: bool,
    uses_no_contraction_add: bool,
    uses_no_contraction_mul: bool,
    uses_no_contraction_fma: bool,
    language_version: MslVersion,
    execution: MslExecutionInfo,
    has_broken_robust: bool,
}

impl MslEmitContext {
    pub fn new(
        program: &crate::ir::Program,
        profile: &Profile,
        options: &MslOptions,
        binding_counters: &mut Bindings,
    ) -> Result<Self, MslError> {
        let stage = program.stage;
        match stage {
            Stage::VertexA => return Err(MslError::UnmergedVertexA),
            Stage::VertexB | Stage::Fragment | Stage::Compute => {}
            Stage::TessellationControl | Stage::TessellationEval | Stage::Geometry => {
                return Err(MslError::UnsupportedStage(stage))
            }
        }

        let mut bindings = MslBindingLayout::default();
        let mut constant_buffers = HashMap::new();
        let mut parameters = Vec::new();
        let binding_counter = if profile.unified_descriptor_binding {
            &mut binding_counters.unified
        } else {
            &mut binding_counters.uniform_buffer
        };
        for descriptor in &program.info.constant_buffer_descriptors {
            if descriptor.count != 1 {
                return Err(MslError::UnsupportedProgramFeature(
                    "constant buffer descriptor indexing",
                ));
            }
            let descriptor_binding = *binding_counter;
            *binding_counter += descriptor.count;
            let buffer_index = bindings.buffer_count;
            bindings.buffer_count += 1;
            bindings.resources.push(MslResourceBinding {
                descriptor_set: 0,
                binding: descriptor_binding,
                kind: MslResourceKind::UniformBuffer,
                buffer_index,
                texture_index: 0,
                sampler_index: 0,
                count: None,
            });
            let name = format!("c{}", descriptor.index);
            parameters.push(format!("constant uint4* {name} [[buffer({buffer_index})]]"));
            constant_buffers.insert(descriptor.index, name);
        }
        let parameters = parameters.join(", ");
        let mut source = String::new();
        let returns_output = match stage {
            Stage::VertexB => {
                source.push_str(concat!(
                    "struct MslVertexOut {\n",
                    "    float4 position [[position]];\n",
                    "};\n\n",
                ));
                source.push_str(&format!("vertex MslVertexOut main0({parameters}) {{\n"));
                source.push_str(concat!(
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
                source.push_str(&format!(
                    "}};\n\nfragment MslFragmentOut main0({parameters}) {{\n"
                ));
                source.push_str("    MslFragmentOut output = {};\n");
                true
            }
            Stage::Fragment => {
                source.push_str(&format!("fragment void main0({parameters}) {{\n"));
                false
            }
            Stage::Compute => {
                source.push_str(&format!("kernel void main0({parameters}) {{\n"));
                false
            }
            _ => unreachable!("stage was validated above"),
        };

        Ok(Self {
            stage,
            source,
            definitions: HashMap::new(),
            constant_buffers,
            bindings,
            returns_output,
            uses_no_contraction_add: false,
            uses_no_contraction_mul: false,
            uses_no_contraction_fma: false,
            language_version: options.language_version,
            execution: MslExecutionInfo {
                workgroup_size: (stage == Stage::Compute).then_some(program.workgroup_size),
            },
            has_broken_robust: profile.has_broken_robust,
        })
    }

    fn type_name(ty: Type) -> Result<&'static str, MslError> {
        match ty {
            Type::U1 => Ok("bool"),
            Type::U32 => Ok("uint"),
            Type::U64 => Ok("ulong"),
            Type::F16 => Ok("half"),
            Type::F32 => Ok("float"),
            Type::U32x2 => Ok("uint2"),
            _ => Err(MslError::UnsupportedType(ty)),
        }
    }

    pub fn constant_buffer_element_expression(
        &self,
        inst_ref: InstRef,
        binding: u32,
        offset: &Value,
        element_offset: u32,
    ) -> Result<String, MslError> {
        let name = self
            .constant_buffers
            .get(&binding)
            .ok_or(MslError::MissingConstantBuffer(binding))?;
        let offset_expression = self.value_expression(offset, inst_ref, 1)?;
        let vector_index = match offset {
            Value::ImmU32(offset) => format!("{}u", offset / 16),
            _ => format!("(({offset_expression}) >> 4u)"),
        };
        let vector = if self.has_broken_robust && !matches!(offset, Value::ImmU32(_)) {
            format!("(({vector_index}) <= 0x0000FFFFu ? {name}[{vector_index}] : uint4(0u))")
        } else {
            format!("{name}[{vector_index}]")
        };
        let component = match offset {
            Value::ImmU32(offset) => format!("{}u", (offset / 4) % 4 + element_offset),
            _ if element_offset == 0 => {
                format!("((({offset_expression}) >> 2u) & 3u)")
            }
            _ => format!("((((({offset_expression}) >> 2u) & 3u)) + {element_offset}u)"),
        };
        Ok(format!("{vector}[{component}]"))
    }

    pub fn bit_offset_expression(
        &self,
        inst_ref: InstRef,
        offset: &Value,
        width: u32,
    ) -> Result<String, MslError> {
        let expression = self.value_expression(offset, inst_ref, 1)?;
        Ok(match (offset, width) {
            (Value::ImmU32(offset), 8) => format!("{}u", (offset % 4) * 8),
            (Value::ImmU32(offset), 16) => format!("{}u", ((offset / 2) % 2) * 16),
            (_, 8) => format!("((({expression}) << 3u) & 24u)"),
            (_, 16) => format!("((({expression}) << 3u) & 16u)"),
            _ => unreachable!("CBUF extraction width must be 8 or 16"),
        })
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
            Value::ImmU64(value) => Ok(format!("0x{value:016X}ul")),
            Value::ImmF16(value) => Ok(format!("as_type<half>(ushort(0x{value:04X}u))")),
            Value::ImmF32(value) => Ok(format!("as_type<float>(0x{:08X}u)", value.to_bits())),
            other => Err(MslError::UnsupportedValue {
                block: inst_ref.block,
                inst: inst_ref.inst,
                arg,
                value: Self::unsupported_value_name(other),
            }),
        }
    }

    pub fn is_defined(&self, inst_ref: InstRef) -> bool {
        self.definitions.contains_key(&inst_ref)
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

    pub fn emit_fma(&mut self, inst_ref: InstRef, inst: &Inst, ty: Type) -> Result<(), MslError> {
        let a = self.value_expression(inst.arg(0), inst_ref, 0)?;
        let b = self.value_expression(inst.arg(1), inst_ref, 1)?;
        let c = self.value_expression(inst.arg(2), inst_ref, 2)?;
        let control = crate::ir::types::FpControl::from_u32(inst.flags);
        let expression = if control.no_contraction {
            self.uses_no_contraction_fma = true;
            format!("spvFma({a}, {b}, {c})")
        } else {
            format!("fma({a}, {b}, {c})")
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
        if self.uses_no_contraction_fma {
            source.push_str(concat!(
                "template<typename T>\n",
                "[[clang::optnone]] T spvFma(T a, T b, T c) {\n",
                "    return fma(a, b, c);\n",
                "}\n\n",
            ));
        }
        source.push_str(&self.source);
        MslShaderArtifact {
            source: MslShaderSource {
                source,
                stage: self.stage,
            },
            bindings: self.bindings,
            entry_point: "main0".to_owned(),
            language_version: self.language_version,
            execution: self.execution,
        }
    }
}

// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Explicit function ABI for native MSL stage composition. Eden has no object/
//! mesh backend; this replaces neither its resource binding allocation nor IR.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MslFunctionKind {
    StageEntryPoint,
    VertexFunction,
    GeometryFunction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MslParameterAttribute {
    None,
    Buffer(u32),
    Texture(u32),
    Sampler(u32),
    Builtin(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MslParameter {
    pub ty: String,
    pub name: String,
    pub attribute: MslParameterAttribute,
}

impl MslParameter {
    pub fn new(
        ty: impl Into<String>,
        name: impl Into<String>,
        attribute: MslParameterAttribute,
    ) -> Self {
        Self {
            ty: ty.into(),
            name: name.into(),
            attribute,
        }
    }

    pub fn builtin(ty: &str, name: &str, attribute: &'static str) -> Self {
        Self::new(ty, name, MslParameterAttribute::Builtin(attribute))
    }

    pub fn declaration(&self, kind: MslFunctionKind) -> String {
        let declaration = format!("{} {}", self.ty, self.name);
        if kind != MslFunctionKind::StageEntryPoint {
            return declaration;
        }
        let attribute = match self.attribute {
            MslParameterAttribute::None => return declaration,
            MslParameterAttribute::Buffer(index) => format!("buffer({index})"),
            MslParameterAttribute::Texture(index) => format!("texture({index})"),
            MslParameterAttribute::Sampler(index) => format!("sampler({index})"),
            MslParameterAttribute::Builtin(name) => name.to_owned(),
        };
        format!("{declaration} [[{attribute}]]")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MslFunctionInterface {
    pub kind: MslFunctionKind,
    pub parameters: Vec<MslParameter>,
}

impl MslFunctionInterface {
    pub fn declarations(&self) -> String {
        self.parameters
            .iter()
            .map(|parameter| parameter.declaration(self.kind))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::bindings::Bindings;
    use crate::backend::msl::{
        emit_msl::{emit_msl_vertex_function, emit_msl_with_options_and_bindings},
        MslOptions,
    };
    use crate::ir::{
        emitter::Emitter,
        value::{Attribute, Value},
        Program, SyntaxNode,
    };
    use crate::ir_opt::collect_shader_info_pass::collect_shader_info_pass;
    use crate::profile::Profile;
    use crate::runtime_info::{AttributeType, RuntimeInfo};
    use crate::stage::Stage;

    #[test]
    fn callable_vertex_preserves_resource_and_builtin_abi() {
        let mut program = Program::new(Stage::VertexB);
        program.add_block();
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        let mut ir = Emitter::new(&mut program, 0);
        ir.prologue();
        let input = ir.get_attribute(Attribute::generic(0, 0), Value::ImmU32(0));
        let cbuf = ir.get_cbuf_f32(Value::ImmU32(1), Value::ImmU32(0));
        let value = ir.fp_add_32(input, cbuf);
        ir.set_attribute(Attribute::POSITION_X, value, Value::ImmU32(0));
        let id = ir.get_attribute(Attribute::VERTEX_ID, Value::ImmU32(0));
        ir.set_attribute(Attribute::generic(0, 0), id, Value::ImmU32(0));
        ir.epilogue();
        collect_shader_info_pass(&mut program);
        let mut runtime = RuntimeInfo::default();
        runtime.generic_input_types[0] = AttributeType::Float;
        runtime
            .previous_stage_stores
            .set(Attribute::generic(0, 0).0 as usize, true);
        let profile = Profile {
            support_vertex_instance_id: true,
            ..Default::default()
        };
        let options = MslOptions::default();
        let mut native_bindings = Bindings::default();
        let native = emit_msl_with_options_and_bindings(
            &program,
            &profile,
            &runtime,
            &options,
            &mut native_bindings,
        )
        .unwrap();
        let mut function_bindings = Bindings::default();
        let function = emit_msl_vertex_function(
            &program,
            &profile,
            &runtime,
            &options,
            &mut function_bindings,
        )
        .unwrap();
        assert_eq!(native.bindings, function.bindings);
        assert_eq!(
            native.interface.as_ref().unwrap().parameters,
            function.interface.as_ref().unwrap().parameters
        );
        assert_eq!(function.entry_point, "ruzu_vertex");
        let interface = function.interface.as_ref().unwrap();
        assert_eq!(interface.kind, MslFunctionKind::VertexFunction);
        assert_eq!(
            interface
                .parameters
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["input", "c1", "vertex_id"]
        );
        assert_eq!(
            interface.parameters[1].attribute,
            MslParameterAttribute::Buffer(0)
        );
        assert!(function.source.source.contains("inline MslVertexOut ruzu_vertex(MslVertexIn input, constant uint4* c1, uint vertex_id)"));
        assert!(!function.source.source.contains("[[buffer("));
        assert!(!function.source.source.contains("[[vertex_id]]"));
        // Only the declaration changes; prologue, IR and every return are shared.
        let native_header = format!(
            "vertex MslVertexOut main0({})",
            native.interface.as_ref().unwrap().declarations()
        );
        let function_header = format!(
            "inline MslVertexOut ruzu_vertex({})",
            interface.declarations()
        );
        assert_eq!(
            native
                .source
                .source
                .replacen(&native_header, &function_header, 1),
            function.source.source
        );
    }

    #[test]
    fn callable_geometry_preserves_emission_body_and_resource_bindings() {
        use crate::backend::msl::emit_msl::emit_msl_geometry_function;
        let mut program = Program::new(Stage::Geometry);
        program.output_vertices = 3;
        program.add_block();
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        let mut ir = Emitter::new(&mut program, 0);
        ir.prologue();
        let value = ir.get_cbuf_f32(Value::ImmU32(1), Value::ImmU32(0));
        ir.set_attribute(Attribute::POSITION_X, value, Value::ImmU32(0));
        ir.set_attribute(Attribute::LAYER, Value::ImmF32(f32::from_bits(2)), Value::ImmU32(0));
        ir.emit_vertex(Value::ImmU32(0));
        ir.end_primitive(Value::ImmU32(0));
        ir.epilogue();
        collect_shader_info_pass(&mut program);
        let options = MslOptions {
            language_version: crate::backend::msl::MslVersion::V3_0,
            ..Default::default()
        };
        let native = emit_msl_with_options_and_bindings(&program, &Profile::default(),
            &RuntimeInfo::default(), &options, &mut Bindings::default()).unwrap();
        let callable = emit_msl_geometry_function(&program, &Profile::default(),
            &RuntimeInfo::default(), &options, &mut Bindings::default()).unwrap();
        assert_eq!(native.bindings, callable.bindings);
        assert_eq!(callable.entry_point, "ruzu_geometry");
        assert_eq!(callable.interface.as_ref().unwrap().kind, MslFunctionKind::GeometryFunction);
        assert!(callable.source.source.contains("template<typename GeometryOutput>\ninline void ruzu_geometry("));
        assert!(callable.source.source.contains("const thread MslGeometryPayload& geometry_input"));
        assert!(callable.source.source.contains("thread GeometryOutput& geometry_mesh"));
        assert!(!callable.source.source.contains("[[payload]]"));
        assert!(!callable.source.source.contains("[[buffer("));
        assert!(!callable.source.source.contains("metal::mesh<"));
        let body = "    uint geometry_vertex = 0u;";
        assert_eq!(native.source.source.split_once(body).unwrap().1,
            callable.source.source.split_once(body).unwrap().1);
        for stage in [Stage::VertexB, Stage::Fragment, Stage::Compute] {
            assert!(emit_msl_geometry_function(&Program::new(stage), &Profile::default(),
                &RuntimeInfo::default(), &options, &mut Bindings::default()).is_err());
        }
    }

    #[test]
    fn callable_vertex_rejects_wrong_stage_and_discarded_outputs() {
        let mut bindings = Bindings::default();
        assert!(emit_msl_vertex_function(
            &Program::new(Stage::Fragment),
            &Profile::default(),
            &RuntimeInfo::default(),
            &MslOptions::default(),
            &mut bindings
        )
        .is_err());
        let options = MslOptions {
            disable_rasterization: true,
            ..Default::default()
        };
        assert!(emit_msl_vertex_function(
            &Program::new(Stage::VertexB),
            &Profile::default(),
            &RuntimeInfo::default(),
            &options,
            &mut bindings
        )
        .is_err());
    }
}

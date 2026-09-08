// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native mesh representation of the Maxwell geometry stage. Eden has no MSL
//! equivalent; EmitVertex/EndPrimitive semantics follow emit_spirv_special.cpp.
//! The payload declaration defines the ABI for preceding-stage integration.

use super::{MslError, MslOptions, MslVersion};
use crate::ir::{types::OutputTopology, Program};
use crate::runtime_info::RuntimeInfo;

#[derive(Debug, Clone, Copy)]
pub struct GeometryLayout {
    pub input_vertices: u32,
    pub output_vertices: u32,
    pub output_primitives: u32,
    pub topology: OutputTopology,
    pub provoking_vertex_last: bool,
    pub stores_layer: bool,
    pub stores_viewport_index: bool,
}

impl GeometryLayout {
    pub fn new(
        program: &Program,
        runtime: &RuntimeInfo,
        options: &MslOptions,
    ) -> Result<Self, MslError> {
        if options.language_version < MslVersion::V3_0 {
            return Err(MslError::UnsupportedProgramFeature(
                "mesh shaders require MSL 3.0",
            ));
        }
        if runtime.xfb_count != 0 {
            return Err(MslError::UnsupportedProgramFeature(
                "mesh transform feedback",
            ));
        }
        if program.output_vertices > 256 || program.invocations > 1024 {
            return Err(MslError::UnsupportedProgramFeature(
                "Metal mesh output or invocation limit",
            ));
        }
        let output_primitives = match program.output_topology {
            OutputTopology::PointList => program.output_vertices,
            OutputTopology::LineStrip => program.output_vertices.saturating_sub(1),
            OutputTopology::TriangleStrip => program.output_vertices.saturating_sub(2),
        };
        // Metal validates total mesh storage when compiling this concrete
        // template. Counting generic float4s here would omit point size, clip
        // distances, padding and indices and falsely accept oversized meshes.
        Ok(Self {
            input_vertices: runtime.input_topology.vertices(),
            // Metal template dimensions must remain nonzero, even for shaders
            // that emit no complete primitives. The actual count starts at zero.
            output_vertices: program.output_vertices.max(1),
            output_primitives: output_primitives.max(1),
            topology: program.output_topology,
            provoking_vertex_last: options.geometry_provoking_vertex_last,
            stores_layer: program.info.stores.get(crate::ir::value::Attribute::LAYER.0 as usize),
            stores_viewport_index: program.info.stores.get(crate::ir::value::Attribute::VIEWPORT_INDEX.0 as usize),
        })
    }

    pub fn vertex_declaration(runtime: &RuntimeInfo) -> String {
        let mut source = String::from("struct MslGeometryVertexIn {\n    float4 position;\n");
        for index in 0..32 {
            if runtime.previous_stage_stores.generic_any(index) {
                source.push_str(&format!("    float4 in_attr{index};\n"));
            }
        }
        source.push_str("};\n");
        source
    }

    pub fn vertex_stride(runtime: &RuntimeInfo) -> usize {
        // Every member is float4 with size/alignment 16. No smaller trailing field.
        16 * (1 + Self::generic_mask(runtime).count_ones() as usize)
    }

    pub fn generic_mask(runtime: &RuntimeInfo) -> u32 {
        (0..32).fold(0, |mask, i| {
            mask | (u32::from(runtime.previous_stage_stores.generic_any(i)) << i)
        })
    }

    pub fn payload_size(&self, runtime: &RuntimeInfo) -> usize {
        (Self::vertex_stride(runtime) * self.input_vertices as usize + 4).next_multiple_of(16)
    }

    pub fn payload_declaration(&self, runtime: &RuntimeInfo) -> String {
        let mut source = Self::vertex_declaration(runtime);
        source.push_str(&format!(
            "struct MslGeometryPayload {{\n    MslGeometryVertexIn vertices[{}];\n    uint primitive_id;\n}};\n\n",
            self.input_vertices,
        ));
        source
    }

    pub fn mesh_declaration(&self) -> String {
        let topology = match self.topology {
            OutputTopology::PointList => "point",
            OutputTopology::LineStrip => "line",
            OutputTopology::TriangleStrip => "triangle",
        };
        let primitive_type = if self.has_primitive_outputs() {
            "MslGeometryPrimitiveOut"
        } else {
            "void"
        };
        let declaration = self.primitive_declaration();
        format!("{declaration}using MslGeometryMesh = metal::mesh<MslVertexOut, {primitive_type}, {}, {}, metal::topology::{topology}>;\n",
            self.output_vertices, self.output_primitives)
    }

    pub fn primitive_declaration(&self) -> &'static str {
        match (self.stores_layer, self.stores_viewport_index) {
            (true, false) => "struct MslGeometryPrimitiveOut { uint layer [[render_target_array_index]]; };\n",
            (false, true) => "struct MslGeometryPrimitiveOut { uint viewport [[viewport_array_index]]; };\n",
            (true, true) => "struct MslGeometryPrimitiveOut { uint layer [[render_target_array_index]]; uint viewport [[viewport_array_index]]; };\n",
            (false, false) => "",
        }
    }

    pub fn has_primitive_outputs(&self) -> bool {
        self.stores_layer || self.stores_viewport_index
    }

    pub fn emit_primitive_output(&self) -> &'static str {
        if !self.has_primitive_outputs() {
            return "";
        }
        // Vulkan requires identical Layer/ViewportIndex values on all vertices of a
        // primitive. Capture it now, before a later EmitVertex can change it.
        match self.topology {
            OutputTopology::PointList => "geometry_mesh.set_primitive(geometry_primitive - 1u, geometry_primitive_output);\n",
            OutputTopology::LineStrip => "if (geometry_strip_vertices >= 2u) geometry_mesh.set_primitive(geometry_primitive - 1u, geometry_primitive_output);\n",
            OutputTopology::TriangleStrip => "if (geometry_strip_vertices >= 3u) geometry_mesh.set_primitive(geometry_primitive - 1u, geometry_primitive_output);\n",
        }
    }

    pub fn emit_vertex(&self) -> &'static str {
        // set_vertex copies all outputs now, rather than retaining the mutable
        // output record. Strip winding resets at each EndPrimitive.
        match (self.topology, self.provoking_vertex_last) {
            (OutputTopology::PointList, _) => concat!(
                "geometry_mesh.set_vertex(geometry_vertex, output);\n",
                "geometry_mesh.set_index(geometry_primitive++, geometry_vertex);\n",
                "++geometry_vertex;\n",
            ),
            (OutputTopology::LineStrip, false) => concat!(
                "geometry_mesh.set_vertex(geometry_vertex, output);\n",
                "if (geometry_strip_vertices >= 1u) {\n",
                "    geometry_mesh.set_index(2u * geometry_primitive, geometry_vertex - 1u);\n",
                "    geometry_mesh.set_index(2u * geometry_primitive + 1u, geometry_vertex);\n",
                "    ++geometry_primitive;\n",
                "}\n++geometry_vertex;\n++geometry_strip_vertices;\n",
            ),
            (OutputTopology::LineStrip, true) => concat!(
                "geometry_mesh.set_vertex(geometry_vertex, output);\n",
                "if (geometry_strip_vertices >= 1u) {\n",
                "    geometry_mesh.set_index(2u * geometry_primitive, geometry_vertex);\n",
                "    geometry_mesh.set_index(2u * geometry_primitive + 1u, geometry_vertex - 1u);\n",
                "    ++geometry_primitive;\n",
                "}\n++geometry_vertex;\n++geometry_strip_vertices;\n",
            ),
            (OutputTopology::TriangleStrip, false) => concat!(
                "geometry_mesh.set_vertex(geometry_vertex, output);\n",
                "if (geometry_strip_vertices >= 2u) {\n",
                "    uint odd = geometry_strip_vertices & 1u;\n",
                "    geometry_mesh.set_index(3u * geometry_primitive, geometry_vertex - 2u);\n",
                "    geometry_mesh.set_index(3u * geometry_primitive + 1u, geometry_vertex - 1u + odd);\n",
                "    geometry_mesh.set_index(3u * geometry_primitive + 2u, geometry_vertex - odd);\n",
                "    ++geometry_primitive;\n",
                "}\n++geometry_vertex;\n++geometry_strip_vertices;\n",
            ),
            (OutputTopology::TriangleStrip, true) => concat!(
                "geometry_mesh.set_vertex(geometry_vertex, output);\n",
                "if (geometry_strip_vertices >= 2u) {\n",
                "    uint odd = geometry_strip_vertices & 1u;\n",
                "    geometry_mesh.set_index(3u * geometry_primitive, geometry_vertex);\n",
                "    geometry_mesh.set_index(3u * geometry_primitive + 1u, geometry_vertex - 2u + odd);\n",
                "    geometry_mesh.set_index(3u * geometry_primitive + 2u, geometry_vertex - 1u - odd);\n",
                "    ++geometry_primitive;\n",
                "}\n++geometry_vertex;\n++geometry_strip_vertices;\n",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::msl::emit_msl::emit_msl_with_options;
    use crate::ir::{
        emitter::Emitter,
        program::SyntaxNode,
        value::{Attribute, Value},
    };
    use crate::profile::Profile;
    use crate::stage::Stage;

    fn geometry_program() -> Program {
        let mut program = Program::new(Stage::Geometry);
        program.output_vertices = 6;
        program.add_block();
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        program
    }

    fn source(program: &Program, runtime: &RuntimeInfo) -> Result<String, MslError> {
        emit_msl_with_options(
            program,
            &Profile::default(),
            runtime,
            &MslOptions {
                language_version: MslVersion::V3_0,
                ..Default::default()
            },
        )
        .map(|artifact| artifact.source.source)
    }

    #[test]
    fn geometry_payload_layout_tracks_sparse_generic_members() {
        let program = geometry_program();
        let mut runtime = RuntimeInfo {
            input_topology: crate::runtime_info::InputTopology::Triangles,
            ..Default::default()
        };
        runtime
            .previous_stage_stores
            .set(Attribute::generic(1, 2).0 as usize, true);
        runtime
            .previous_stage_stores
            .set(Attribute::generic(31, 0).0 as usize, true);
        let layout = GeometryLayout::new(
            &program,
            &runtime,
            &MslOptions {
                language_version: MslVersion::V3_0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(GeometryLayout::generic_mask(&runtime), 0x8000_0002);
        assert_eq!(GeometryLayout::vertex_stride(&runtime), 48);
        assert_eq!(layout.payload_size(&runtime), 160);
        assert_eq!(GeometryLayout::vertex_declaration(&runtime), "struct MslGeometryVertexIn {\n    float4 position;\n    float4 in_attr1;\n    float4 in_attr31;\n};\n");
    }

    #[test]
    fn geometry_emission_captures_outputs_and_finalizes_count() {
        let mut program = geometry_program();
        let mut emitter = Emitter::new(&mut program, 0);
        emitter.prologue();
        emitter.set_attribute(Attribute::POSITION_X, Value::ImmF32(0.25), Value::ImmU32(0));
        emitter.emit_vertex(Value::ImmU32(0));
        emitter.set_attribute(Attribute::POSITION_X, Value::ImmF32(0.75), Value::ImmU32(0));
        emitter.end_primitive(Value::ImmU32(0));
        emitter.epilogue();
        let source = source(&program, &RuntimeInfo::default()).unwrap();
        assert!(source.contains("metal::mesh<MslVertexOut, void, 6, 4, metal::topology::triangle>"));
        assert_eq!(
            source
                .matches("set_vertex(geometry_vertex, output)")
                .count(),
            1
        );
        let emit = source.find("set_vertex(geometry_vertex, output)").unwrap();
        let stores: Vec<_> = source.match_indices("output.position.x =").collect();
        assert!(stores[0].0 < emit && emit < stores[1].0);
        assert!(source.contains("geometry_strip_vertices = 0u;"));
        assert!(source.contains("geometry_mesh.set_primitive_count(geometry_primitive);\nreturn;"));
        assert!(!source.contains("return output;"));
        // Eden initializes generic/position defaults only in VertexB's prologue.
        assert!(!source.contains("output.position = float4(0.0f, 0.0f, 0.0f, 1.0f);"));
    }

    #[test]
    fn attribute_stores_ignore_the_vertex_operand_like_upstream() {
        for stage in [crate::stage::Stage::VertexB, crate::stage::Stage::Geometry] {
            let mut reference = None;
            for vertex in [0, 9, u32::MAX] {
                let mut program = geometry_program();
                program.stage = stage;
                if stage == crate::stage::Stage::VertexB {
                    program.output_vertices = 0;
                }
                Emitter::new(&mut program, 0).set_attribute(
                    Attribute::POSITION_X, Value::ImmF32(0.5), Value::ImmU32(vertex));
                crate::ir_opt::collect_shader_info_pass::collect_shader_info_pass(&mut program);
                let actual = source(&program, &RuntimeInfo::default()).unwrap();
                if let Some(expected) = &reference {
                    assert_eq!(&actual, expected);
                } else {
                    reference = Some(actual);
                }
            }
        }
    }

    #[test]
    fn geometry_layer_is_a_bitcast_and_is_snapshotted_per_complete_primitive() {
        let mut program = geometry_program();
        let mut emitter = Emitter::new(&mut program, 0);
        emitter.set_attribute(Attribute::LAYER, Value::ImmF32(f32::from_bits(7)), Value::ImmU32(0));
        emitter.emit_vertex(Value::ImmU32(0));
        emitter.set_attribute(Attribute::LAYER, Value::ImmF32(f32::from_bits(2)), Value::ImmU32(0));
        emitter.end_primitive(Value::ImmU32(0));
        crate::ir_opt::collect_shader_info_pass::collect_shader_info_pass(&mut program);
        let source = source(&program, &RuntimeInfo::default()).unwrap();
        assert!(source.contains("uint layer [[render_target_array_index]]"));
        assert!(source.contains("geometry_primitive_output.layer = as_type<uint>("));
        assert!(source.contains("as_type<uint>(as_type<float>(0x00000007u))"));
        assert!(source.contains("as_type<uint>(as_type<float>(0x00000002u))"));
        assert!(source.contains("metal::mesh<MslVertexOut, MslGeometryPrimitiveOut,"));
        assert!(source.contains("if (geometry_strip_vertices >= 3u) geometry_mesh.set_primitive"));
        let capture = source.find("geometry_mesh.set_primitive(").unwrap();
        let stores: Vec<_> = source.match_indices("geometry_primitive_output.layer =").collect();
        assert!(stores[0].0 < capture && capture < stores[1].0);
    }

    #[test]
    fn viewport_output_preserves_bits_snapshot_and_capability_gate() {
        for layer in [false, true] {
            let mut program = geometry_program();
            let mut emitter = Emitter::new(&mut program, 0);
            if layer {
                emitter.set_attribute(Attribute::LAYER, Value::ImmF32(f32::from_bits(2)), Value::ImmU32(0));
            }
            emitter.set_attribute(Attribute::VIEWPORT_INDEX, Value::ImmF32(f32::from_bits(15)), Value::ImmU32(0));
            emitter.emit_vertex(Value::ImmU32(0));
            emitter.set_attribute(Attribute::VIEWPORT_INDEX, Value::ImmF32(f32::from_bits(1)), Value::ImmU32(0));
            crate::ir_opt::collect_shader_info_pass::collect_shader_info_pass(&mut program);
            let options = MslOptions { language_version: MslVersion::V3_0, ..Default::default() };
            let runtime = RuntimeInfo::default();
            let profile = Profile { support_multi_viewport: true, ..Default::default() };
            let artifact = emit_msl_with_options(&program, &profile, &runtime, &options).unwrap();
            let text = artifact.source.source;
            assert!(text.contains("uint viewport [[viewport_array_index]]"));
            assert_eq!(text.contains("uint layer [[render_target_array_index]]"), layer);
            assert!(text.contains("as_type<uint>(as_type<float>(0x0000000Fu))"));
            let captures = text.find("geometry_mesh.set_primitive(").unwrap();
            let stores: Vec<_> = text.match_indices("geometry_primitive_output.viewport =").collect();
            assert!(stores[0].0 < captures && captures < stores[1].0);
            assert!(emit_msl_with_options(&program, &Profile::default(), &runtime, &options).is_err());
        }
    }

    #[test]
    fn geometry_load_uses_dynamic_vertex_and_invocation_info() {
        let mut program = geometry_program();
        let mut emitter = Emitter::new(&mut program, 0);
        let vertex = emitter.invocation_id();
        let value = emitter.get_attribute(Attribute::POSITION_X, vertex);
        emitter.set_attribute(Attribute::POSITION_X, value, Value::ImmU32(0));
        let info = emitter.invocation_info();
        let info = emitter.bit_cast_f32_u32(info);
        emitter.set_attribute(Attribute::POSITION_Y, info, Value::ImmU32(0));
        crate::ir_opt::collect_shader_info_pass::collect_shader_info_pass(&mut program);
        let runtime = RuntimeInfo {
            input_topology: crate::runtime_info::InputTopology::Triangles,
            ..Default::default()
        };
        let source = source(&program, &runtime).unwrap();
        assert!(source.contains("geometry_input.vertices["));
        assert!(source.contains("geometry_group.x"));
        assert!(source.contains("196608u"));
    }

    #[test]
    fn geometry_rejects_nonzero_stream_instead_of_silently_using_zero() {
        let mut program = geometry_program();
        Emitter::new(&mut program, 0).emit_vertex(Value::ImmU32(1));
        assert!(matches!(
            source(&program, &RuntimeInfo::default()),
            Err(MslError::UnsupportedProgramFeature(
                "nonzero or dynamic geometry stream"
            ))
        ));
    }

    #[test]
    fn empty_geometry_keeps_zero_actual_primitive_count() {
        let mut program = geometry_program();
        program.output_vertices = 0;
        let source = source(&program, &RuntimeInfo::default()).unwrap();
        assert!(source.contains("metal::mesh<MslVertexOut, void, 1, 1,"));
        assert!(source.contains("uint geometry_primitive = 0u;"));
        assert!(!source.contains("set_vertex("));
    }
}

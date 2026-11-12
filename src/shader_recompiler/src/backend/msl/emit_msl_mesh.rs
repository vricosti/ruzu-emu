// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native-MSL counterpart of `backend/spirv/emit_spirv_mesh.rs`: the compute
//! replacement for guest task/mesh programs and the generated vertex reader of
//! its output transport. Layouts (`MeshOutputLayout`, `MeshDispatchRecord`) are
//! shared with the SPIR-V backend so both hosts use one transport format.

use crate::backend::spirv::spirv_emit_context::{MeshDispatchRecord, MeshOutputLayout};
use crate::ir::value::{InstRef, Value};
use crate::ir::Inst;
use crate::shader_info::MeshInfo;

use super::msl_emit_context::MslEmitContext;
use super::MslError;

/// Mesh/task state of a compute kernel emitted for a guest task or mesh program.
#[derive(Debug, Clone)]
pub(super) struct MslMeshState {
    pub is_task: bool,
    pub payload_words: u32,
    pub threads: u32,
    pub info: MeshInfo,
    pub layout: MeshOutputLayout,
    pub task_count_guard: bool,
    pub zero_fill: bool,
    /// Workgroup storage exceeds the device's `threadgroup` limit and lives in
    /// one slice per physical group of the `mesh_workgroup_storage` buffer.
    pub device_storage: bool,
}

impl MslMeshState {
    /// Words of workgroup storage one group needs.
    pub fn workgroup_words(&self) -> u32 {
        let mesh_words = if self.is_task {
            0
        } else {
            self.layout.attribute_words + self.layout.map_words + 1
        };
        self.payload_words + mesh_words
    }

    /// Bytes of `threadgroup` storage the kernel would declare.
    pub fn threadgroup_bytes(&self) -> u64 {
        u64::from(self.workgroup_words()) * 4
    }

    /// Collective barrier over the workgroup storage.
    fn barrier(&self) -> &'static str {
        if self.device_storage {
            "    threadgroup_barrier(mem_flags::mem_device);\n"
        } else {
            "    threadgroup_barrier(mem_flags::mem_threadgroup);\n"
        }
    }
}

/// `RUZU_NX2_WORKGROUP_ZERO=0` restores initializing only the counts, as in
/// `emit_spirv_mesh.rs`.
pub(super) fn workgroup_zero_fill() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("RUZU_NX2_WORKGROUP_ZERO")
            .is_none_or(|value| !value.to_string_lossy().starts_with('0'))
    })
}

/// Lane-striped copy of `words` words: each invocation handles
/// `local_invocation_id.x + k * threads`. `body` receives the index name.
fn striped(words: u32, threads: u32, clamp: bool, body: impl Fn(&str) -> String) -> String {
    let mut source = String::new();
    for first in (0..words).step_by(threads as usize) {
        source.push_str("    {\n");
        if clamp {
            source.push_str(&format!(
                "        const uint i = min(local_invocation_id.x + {first}u, {}u);\n",
                words - 1
            ));
            source.push_str(&format!("        {}\n", body("i")));
        } else {
            source.push_str(&format!(
                "        const uint i = local_invocation_id.x + {first}u;\n"
            ));
            if first + threads > words {
                source.push_str(&format!("        if (i < {words}u) {{ {} }}\n", body("i")));
            } else {
                source.push_str(&format!("        {}\n", body("i")));
            }
        }
        source.push_str("    }\n");
    }
    source
}

/// Declarations and the preheader of a task/mesh kernel: workgroup storage,
/// the dispatch mapping import, the unused-child guard and the payload import.
pub(super) fn kernel_preheader(state: &MslMeshState) -> String {
    let mut source = String::new();
    if state.device_storage {
        // Metal caps `threadgroup` storage (32 KiB on Apple GPUs). Above it
        // each physical group owns a slice of a renderer-bound device buffer;
        // the layout within the slice is the threadgroup declaration order.
        source.push_str(&format!(
            "    device uint* task_payload = mesh_workgroup_storage + workgroup_id.x * {}u;\n",
            state.workgroup_words()
        ));
        if !state.is_task {
            source.push_str(&format!(
                "    device uint* isbe_attributes = task_payload + {}u;\n    device uint* isbe_map = isbe_attributes + {}u;\n    device uint* isbe_primitive_count = isbe_map + {}u;\n",
                state.payload_words, state.layout.attribute_words, state.layout.map_words
            ));
        }
    } else {
        source.push_str(&format!("    threadgroup uint task_payload[{}];\n", state.payload_words));
        if !state.is_task {
            source.push_str(&format!(
                "    threadgroup uint isbe_attributes[{}];\n    threadgroup uint isbe_map[{}];\n    threadgroup uint isbe_primitive_count[1];\n",
                state.layout.attribute_words, state.layout.map_words
            ));
        }
    }
    // The physical dispatch is one-dimensional; guest X comes from the mapping.
    source.push_str(&format!(
        "    const uint mesh_record = workgroup_id.x * {}u;\n",
        MeshDispatchRecord::WORDS
    ));
    source.push_str("    const uint3 guest_workgroup_id = uint3(mesh_dispatch[mesh_record], 0u, 0u);\n");
    source.push_str("    const uint mesh_payload_base = mesh_dispatch[mesh_record + 1u];\n");
    source.push_str("    const uint mesh_output_record = mesh_dispatch[mesh_record + 2u];\n");
    if !state.is_task {
        if state.task_count_guard {
            // Uniform across the physical workgroup: unused reserved children
            // leave before any guest access or barrier.
            source.push_str(
                "    if (guest_workgroup_id.x >= mesh_payload_transport[mesh_payload_base + 1u]) { return; }\n",
            );
        }
        source.push_str(&striped(state.payload_words, state.threads, false, |i| {
            format!("task_payload[{i}] = mesh_payload_transport[mesh_payload_base + {i}];")
        }));
        source.push_str(state.barrier());
    }
    source
}

pub(super) fn emit_prologue(context: &mut MslEmitContext, state: &MslMeshState) {
    let mut source = String::new();
    if state.is_task {
        // A task that exits without a count write emits no children.
        source.push_str("    task_payload[1u] = 0u;\n");
        if state.zero_fill {
            source.push_str(&striped(state.payload_words, state.threads, true, |i| {
                format!("task_payload[{i}] = 0u;")
            }));
        }
    } else {
        source.push_str("    isbe_primitive_count[0] = 0u;\n");
        if state.zero_fill {
            source.push_str(&striped(state.layout.attribute_words, state.threads, true, |i| {
                format!("isbe_attributes[{i}] = 0u;")
            }));
            source.push_str(&striped(state.layout.map_words, state.threads, true, |i| {
                format!("isbe_map[{i}] = 0u;")
            }));
        }
    }
    source.push_str(state.barrier());
    context.emit_raw(&source);
}

pub(super) fn emit_epilogue(context: &mut MslEmitContext, state: &MslMeshState) {
    let mut source = String::from(state.barrier());
    if state.is_task {
        source.push_str(&striped(state.payload_words, state.threads, false, |i| {
            format!("mesh_payload_transport[mesh_payload_base + {i}] = task_payload[{i}];")
        }));
    } else {
        let layout = state.layout;
        source.push_str(&format!(
            "    const uint mesh_output_base = mesh_output_record * {}u;\n",
            layout.stride_words
        ));
        // Indirect draw header: one writer per group.
        source.push_str(&format!(
            concat!(
                "    if (local_invocation_id.x == 0u) {{\n",
                "        mesh_output[mesh_output_base] = min(isbe_primitive_count[0], {}u) * {}u;\n",
                "        mesh_output[mesh_output_base + 1u] = 1u;\n",
                "        mesh_output[mesh_output_base + 2u] = mesh_output_record * {}u;\n",
                "        mesh_output[mesh_output_base + 3u] = 0u;\n",
                "    }}\n",
            ),
            state.info.max_primitives,
            state.info.primitive_vertices(),
            layout.vertex_span
        ));
        source.push_str(&striped(layout.attribute_words, state.threads, false, |i| {
            format!(
                "mesh_output[mesh_output_base + {}u + {i}] = isbe_attributes[{i}];",
                layout.attribute_base
            )
        }));
        source.push_str(&striped(layout.map_words, state.threads, false, |i| {
            format!(
                "mesh_output[mesh_output_base + {}u + {i}] = isbe_map[{i}];",
                layout.map_base
            )
        }));
    }
    context.emit_raw(&source);
}

fn state(context: &MslEmitContext) -> Result<MslMeshState, MslError> {
    context
        .mesh_state()
        .cloned()
        .ok_or(MslError::UnsupportedProgramFeature(
            "stage-buffer access outside a task/mesh compute program",
        ))
}

pub(super) fn emit_isbe_read(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let state = state(context)?;
    let expression = if state.is_task {
        "guest_workgroup_id.x".to_owned()
    } else {
        let address = context.value_expression(inst.arg(0), inst_ref, 0)?;
        format!(
            "(({address}) == 0u ? guest_workgroup_id.x : task_payload[min(({address}) >> 2u, {}u)])",
            state.payload_words - 1
        )
    };
    context.define(inst_ref, crate::ir::Type::U32, expression, false)
}

pub(super) fn emit_isbe_write_attribute(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let state = state(context)?;
    let address = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let value = context.value_expression(inst.arg(1), inst_ref, 1)?;
    if state.is_task {
        context.emit_statement(&format!(
            "task_payload[min(({address}) >> 2u, {}u)] = {value};",
            state.payload_words - 1
        ));
    } else {
        context.emit_statement(&format!(
            "isbe_attributes[min(({address}) >> 2u, {}u)] = {value};",
            state.layout.attribute_words - 1
        ));
    }
    Ok(())
}

pub(super) fn emit_isbe_write_map_u8(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let state = state(context)?;
    if state.is_task {
        return Err(MslError::UnsupportedProgramFeature(
            "stage buffer map write outside mesh programs",
        ));
    }
    let address = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let value = context.value_expression(inst.arg(1), inst_ref, 1)?;
    context.emit_statement(&format!(
        "isbe_map[min({address}, {}u)] = ({value}) & 0xFFu;",
        state.layout.map_words - 1
    ));
    Ok(())
}

pub(super) fn emit_isbe_write_map_u32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let state = state(context)?;
    // The map is written from its end: the count's address is byte 3, not 0.
    if state.is_task || *inst.arg(0) != Value::ImmU32(3) {
        return Err(MslError::UnsupportedProgramFeature("32-bit stage buffer map write"));
    }
    let value = context.value_expression(inst.arg(1), inst_ref, 1)?;
    context.emit_statement(&format!("isbe_primitive_count[0] = {value};"));
    Ok(())
}

/// Expression reading one ISBE output line for a pulled vertex; the MSL form
/// of `emit_mesh_output_attribute_read`. `vertex` already includes the
/// indirect command's first vertex.
fn output_attribute_read(mesh: &MeshInfo, layout: MeshOutputLayout, line: u32, per_primitive: bool) -> String {
    let lines = if per_primitive { mesh.primitive_lines.len() } else { mesh.vertex_lines.len() } as u32;
    let element = if per_primitive {
        format!("(mesh_corner / {}u)", mesh.primitive_vertices())
    } else {
        format!("mesh_output[mesh_base + {}u + mesh_corner]", layout.map_base + 4)
    };
    let line_base = layout.attribute_base
        + if per_primitive { layout.primitive_base } else { 0 }
        + line * 32;
    // CopyLines word layout: region + (element / 32)*lines*32 + element%32 + line*32.
    format!(
        "mesh_output[mesh_base + (({element}) >> 5u) * {}u + (({element}) & 31u) + {line_base}u]",
        lines * 32
    )
}

/// Statements of the generated vertex entry point: the MSL form of
/// `emit_mesh_vertex_outputs` (CopyLines, ConvertDepth, per-primitive lines).
pub(super) fn vertex_body(
    mesh: &MeshInfo,
    fixed_point_size: Option<f32>,
    convert_depth: bool,
) -> Result<String, MslError> {
    use crate::ir::attribute::Attribute;
    let layout = MeshOutputLayout::new(mesh);
    if layout.vertex_span == 0 {
        return Err(MslError::UnsupportedProgramFeature("zero-capacity mesh output"));
    }
    let mut source = String::new();
    source.push_str(&format!(
        "    const uint mesh_base = (vertex_id / {span}u) * {stride}u;\n    const uint mesh_corner = vertex_id % {span}u;\n",
        span = layout.vertex_span,
        stride = layout.stride_words
    ));
    source.push_str("    output.position = float4(0.0f, 0.0f, 0.0f, 1.0f);\n");
    if mesh.primitive_vertices() == 1 {
        source.push_str(&format!(
            "    output.point_size = as_type<float>(0x{:08X}u);\n",
            fixed_point_size.unwrap_or(1.0).to_bits()
        ));
    }
    let mut copy = |source: &mut String, per_primitive: bool| -> Result<(), MslError> {
        let lines = if per_primitive { &mesh.primitive_lines } else { &mesh.vertex_lines };
        for (line, &attribute) in lines.iter().enumerate() {
            let read = output_attribute_read(mesh, layout, line as u32, per_primitive);
            let swizzle = |element: u32| ["x", "y", "z", "w"][element as usize];
            match attribute {
                32..=159 => {
                    let generic = (attribute - 32) / 4;
                    source.push_str(&format!(
                        "    output.out_attr{generic}.{} = as_type<float>({read});\n",
                        swizzle((attribute - 32) % 4)
                    ));
                }
                28..=31 if !per_primitive => source.push_str(&format!(
                    "    output.position.{} = as_type<float>({read});\n",
                    swizzle(attribute - 28)
                )),
                27 if !per_primitive => {
                    source.push_str(&format!("    output.point_size = as_type<float>({read});\n"))
                }
                24 if per_primitive => {
                    return Err(MslError::UnsupportedProgramFeature("mesh PrimitiveId vertex fallback"))
                }
                25 | 26 if per_primitive => {
                    return Err(MslError::UnsupportedProgramFeature("mesh layer/viewport vertex output"))
                }
                // Same ignored builtins as DefineMesh.
                _ => {}
            }
        }
        Ok(())
    };
    copy(&mut source, false)?;
    if convert_depth {
        let z = mesh.vertex_lines.iter().position(|&a| a == Attribute::PositionZ as u32);
        let w = mesh.vertex_lines.iter().position(|&a| a == Attribute::PositionW as u32);
        if z.is_some() && w.is_some() {
            source.push_str("    output.position.z = (output.position.z + output.position.w) * 0.5f;\n");
        }
    }
    copy(&mut source, true)?;
    Ok(source)
}

#[cfg(test)]
mod tests {
    use crate::backend::bindings::Bindings;
    use crate::backend::msl::emit_msl::{emit_msl_mesh_compute, emit_msl_mesh_vertex};
    use crate::backend::msl::MslOptions;
    use crate::ir::basic_block::Block;
    use crate::ir::opcodes::Opcode;
    use crate::ir::value::Value;
    use crate::ir::{self, SyntaxNode};
    use crate::profile::Profile;
    use crate::runtime_info::RuntimeInfo;
    use crate::shader_info::MeshInfo;
    use crate::stage::Stage;

    fn mesh_info() -> MeshInfo {
        let mut mesh = MeshInfo { payload_words: 4, max_vertices: 64, max_primitives: 40, topology: 2, ..MeshInfo::default() };
        // Position X..W then generic 0 x; one per-primitive generic 1 x.
        mesh.vertex_lines.extend([28, 29, 30, 31, 32]);
        mesh.primitive_lines.push(36);
        mesh
    }

    fn compute_program(is_task: bool) -> ir::Program {
        let mut program = ir::Program::new(Stage::Compute);
        program.blocks.push(Block::new());
        program.workgroup_size = [32, 1, 1];
        program.mesh = mesh_info();
        program.info.is_task = is_task;
        program.info.is_mesh = !is_task;
        let block = &mut program.blocks[0];
        block.append_new_inst(Opcode::Prologue, vec![]);
        block.append_new_inst(Opcode::IsbeRead, vec![Value::ImmU32(8)]);
        block.append_new_inst(Opcode::IsbeWriteAttribute, vec![Value::ImmU32(4), Value::ImmU32(7)]);
        if !is_task {
            block.append_new_inst(Opcode::IsbeWriteMapU8, vec![Value::ImmU32(5), Value::ImmU32(0x1FF)]);
            block.append_new_inst(Opcode::IsbeWriteMapU32, vec![Value::ImmU32(3), Value::ImmU32(2)]);
        }
        block.append_new_inst(Opcode::Epilogue, vec![]);
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        program
    }

    #[test]
    fn task_kernel_exports_payload_through_the_dispatch_mapping() {
        let artifact = emit_msl_mesh_compute(&compute_program(true), &Profile::default(),
            &RuntimeInfo::default(), &MslOptions::default(), &mut Bindings::default(), false).unwrap();
        let source = &artifact.source.source;
        assert!(source.contains("device uint* mesh_dispatch [[buffer(0)]]"));
        assert!(source.contains("device uint* mesh_payload_transport [[buffer(1)]]"));
        assert_eq!(artifact.bindings.mesh_output_buffer_index, None);
        assert!(source.contains("threadgroup uint task_payload[4];"));
        assert!(!source.contains("isbe_attributes"));
        assert!(source.contains("const uint3 guest_workgroup_id = uint3(mesh_dispatch[mesh_record], 0u, 0u);"));
        // A task reads its own group; attribute writes land in the payload.
        assert!(source.contains("= guest_workgroup_id.x;"));
        assert!(source.contains("task_payload[1u] = 0u;"));
        assert!(source.contains("task_payload[min((0x00000004u) >> 2u, 3u)] = 0x00000007u;"));
        assert!(source.contains("mesh_payload_transport[mesh_payload_base + i] = task_payload[i];"));
    }

    #[test]
    fn mesh_kernel_imports_payload_and_exports_indirect_records() {
        let artifact = emit_msl_mesh_compute(&compute_program(false), &Profile::default(),
            &RuntimeInfo::default(), &MslOptions::default(), &mut Bindings::default(), true).unwrap();
        let source = &artifact.source.source;
        assert_eq!(artifact.bindings.mesh_dispatch_buffer_index, Some(0));
        assert_eq!(artifact.bindings.mesh_payload_buffer_index, Some(1));
        assert_eq!(artifact.bindings.mesh_output_buffer_index, Some(2));
        // layout: attribute_words = 2*5*32 + 2*1*32 = 384, map_words = 4 + 3*40.
        assert!(source.contains("threadgroup uint isbe_attributes[384];"));
        assert!(source.contains("threadgroup uint isbe_map[124];"));
        assert!(source.contains("if (guest_workgroup_id.x >= mesh_payload_transport[mesh_payload_base + 1u]) { return; }"));
        assert!(source.contains("if (i < 4u) { task_payload[i] = mesh_payload_transport[mesh_payload_base + i]; }"));
        assert!(source.contains("((0x00000008u) == 0u ? guest_workgroup_id.x : task_payload[min((0x00000008u) >> 2u, 3u)])"));
        assert!(source.contains("isbe_attributes[min((0x00000004u) >> 2u, 383u)] = 0x00000007u;"));
        assert!(source.contains("isbe_map[min(0x00000005u, 123u)] = (0x000001FFu) & 0xFFu;"));
        assert!(source.contains("threadgroup uint isbe_primitive_count[1];"));
        assert!(source.contains("isbe_primitive_count[0] = 0x00000002u;"));
        assert_eq!(artifact.bindings.mesh_workgroup_buffer_index, None);
        assert!(source.contains("const uint mesh_output_base = mesh_output_record * 512u;"));
        assert!(source.contains("mesh_output[mesh_output_base] = min(isbe_primitive_count[0], 40u) * 3u;"));
        assert!(source.contains("mesh_output[mesh_output_base + 2u] = mesh_output_record * 120u;"));
        assert!(source.contains("mesh_output[mesh_output_base + 4u + i] = isbe_attributes[i];"));
        assert!(source.contains("mesh_output[mesh_output_base + 388u + i] = isbe_map[i];"));
    }

    #[test]
    fn nonconverged_programs_are_rejected() {
        let mut program = compute_program(false);
        program.syntax_list.insert(1, SyntaxNode::Return);
        assert!(emit_msl_mesh_compute(&program, &Profile::default(), &RuntimeInfo::default(),
            &MslOptions::default(), &mut Bindings::default(), false).is_err());
    }

    #[test]
    fn workgroup_storage_above_the_threadgroup_limit_uses_a_device_buffer_slice() {
        // 4 + 384 + 124 + 1 words = 2052 bytes, above this 1 KiB limit.
        let options = MslOptions { max_threadgroup_memory_length: 1024, ..MslOptions::default() };
        let artifact = emit_msl_mesh_compute(&compute_program(false), &Profile::default(),
            &RuntimeInfo::default(), &options, &mut Bindings::default(), false).unwrap();
        let source = &artifact.source.source;
        assert_eq!(artifact.bindings.mesh_workgroup_buffer_index, Some(0));
        assert_eq!(artifact.bindings.mesh_workgroup_words, 513);
        assert_eq!(artifact.bindings.mesh_dispatch_buffer_index, Some(1));
        assert!(source.contains("device uint* mesh_workgroup_storage [[buffer(0)]]"));
        assert!(!source.contains("threadgroup uint"));
        assert!(source.contains("device uint* task_payload = mesh_workgroup_storage + workgroup_id.x * 513u;"));
        assert!(source.contains("device uint* isbe_attributes = task_payload + 4u;"));
        assert!(source.contains("device uint* isbe_map = isbe_attributes + 384u;"));
        assert!(source.contains("device uint* isbe_primitive_count = isbe_map + 124u;"));
        assert!(source.contains("threadgroup_barrier(mem_flags::mem_device);"));
        assert!(!source.contains("mem_flags::mem_threadgroup"));
        // Task kernels keep only the payload.
        let task = emit_msl_mesh_compute(&compute_program(true), &Profile::default(),
            &RuntimeInfo::default(), &MslOptions { max_threadgroup_memory_length: 8, ..MslOptions::default() },
            &mut Bindings::default(), false).unwrap();
        assert_eq!(task.bindings.mesh_workgroup_words, 4);
        assert!(task.source.source.contains("device uint* task_payload = mesh_workgroup_storage + workgroup_id.x * 4u;"));
    }

    #[test]
    fn generated_vertex_reads_lines_through_the_map_and_per_primitive_slots() {
        let profile = Profile { support_vertex_instance_id: true, ..Profile::default() };
        let runtime = RuntimeInfo { convert_depth_mode: true, ..RuntimeInfo::default() };
        let (artifact, info) = emit_msl_mesh_vertex(&mesh_info(), &profile, &runtime,
            &MslOptions::default(), &mut Bindings::default()).unwrap();
        let source = &artifact.source.source;
        println!("{source}");
        assert_eq!(artifact.bindings.mesh_output_buffer_index, Some(0));
        assert!(source.contains("const device uint* mesh_output [[buffer(0)]]"));
        assert!(source.contains("const uint mesh_base = (vertex_id / 120u) * 512u;"));
        assert!(source.contains("const uint mesh_corner = vertex_id % 120u;"));
        // Vertex line 0 (PositionX): element from the map at map_base + 4.
        assert!(source.contains("output.position.x = as_type<float>(mesh_output[mesh_base + ((mesh_output[mesh_base + 392u + mesh_corner]) >> 5u) * 160u + ((mesh_output[mesh_base + 392u + mesh_corner]) & 31u) + 4u]);"));
        // Per-primitive generic 1.x: primitive_base 320 + attribute_base 4.
        assert!(source.contains("output.out_attr1.x = as_type<float>(mesh_output[mesh_base + (((mesh_corner / 3u)) >> 5u) * 32u + (((mesh_corner / 3u)) & 31u) + 324u]);"));
        assert!(source.contains("output.position.z = (output.position.z + output.position.w) * 0.5f;"));
        assert!(info.stores.generic_any(0) && info.stores.generic_any(1));
    }
}

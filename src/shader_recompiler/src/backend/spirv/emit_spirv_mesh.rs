// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Workgroup-memory portion of `backend/spirv/emit_spirv_mesh.cpp`, Eglantine
//! 900ab5a8251dcdb5eee42dd2837fa936ade15303.
//!
//! The mesh-only output operations are independent of the host execution model:
//! native MeshEXT and the compute fallback use the same workgroup storage.
//! Guest task/mesh classification is carried by Info separately from the host
//! execution model. These operations are not connected to native opcode
//! translation yet. A generated vertex entrypoint consumes the exported output;
//! task/mesh dispatch integration is still required before activating this path.

use super::spirv_emit_context::SpirvEmitContext;
use crate::exception::NotImplementedException;
use crate::ir::Value;
use rspirv::dr::Operand;
use rspirv::spirv::{self, Word};

/// Conservative allocation bound for a compute replacement of TaskEpilogue.
/// Native EmitMeshTasksEXT does not need this host-side proof. Unknown values
/// retain the full u32 range; callers must retain the readback path on None.
/// This never changes a guest count or infers a bound from sampled executions.
pub fn task_mesh_workgroup_bound(program: &crate::ir::Program) -> Option<u32> {
    use crate::ir::{opcodes::Opcode, value::InstRef};
    use std::collections::HashMap;
    type Range = (u32, u32);
    const UNKNOWN: Range = (0, u32::MAX);
    fn range(
        program: &crate::ir::Program,
        value: Value,
        cache: &mut HashMap<InstRef, Range>,
        depth: usize,
    ) -> Range {
        if let Value::ImmU32(value) = value { return (value, value); }
        let Value::Inst(reference) = value else { return UNKNOWN; };
        if depth >= 64 { return UNKNOWN; }
        if let Some(value) = cache.get(&reference) { return *value; }
        // Break cycles conservatively, including loop-carried phi values.
        cache.insert(reference, UNKNOWN);
        let Some(inst) = program.blocks.get(reference.block as usize)
            .and_then(|block| block.instructions.get(reference.inst as usize))
            .and_then(Option::as_ref) else { return UNKNOWN; };
        let mut arg = |index: usize| inst.args.get(index).map_or(UNKNOWN,
            |value| range(program, *value, cache, depth + 1));
        let result = match inst.opcode {
            Opcode::Identity => arg(0),
            Opcode::BitCount32 => (0, 32),
            // Guest warps have 32 lanes. EmitLaneId masks larger host warps
            // to this range, matching the upstream SPIR-V warp emitter.
            Opcode::LaneId => (0, 31),
            Opcode::BitwiseAnd32 => (0, arg(0).1.min(arg(1).1)),
            Opcode::UMin32 => {
                let a = arg(0); let b = arg(1);
                (a.0.min(b.0), a.1.min(b.1))
            }
            Opcode::IAdd32 | Opcode::IMul32 => {
                let a = arg(0); let b = arg(1);
                let apply = |a: u32, b: u32| if inst.opcode == Opcode::IAdd32 {
                    a.checked_add(b)
                } else { a.checked_mul(b) };
                match (apply(a.0, b.0), apply(a.1, b.1)) {
                    (Some(low), Some(high)) => (low, high),
                    _ => UNKNOWN,
                }
            }
            Opcode::ShiftRightLogical32 => {
                let a = arg(0); let b = arg(1);
                if b.1 < 32 { (a.0 >> b.1, a.1 >> b.0) } else { UNKNOWN }
            }
            Opcode::SelectU32 => {
                let a = arg(1); let b = arg(2);
                (a.0.min(b.0), a.1.max(b.1))
            }
            _ => UNKNOWN,
        };
        cache.insert(reference, result);
        result
    }
    if !program.info.is_task || program.info.is_mesh || program.mesh.payload_words < 2 {
        return None;
    }
    let mut cache = HashMap::new();
    let mut bound = 0;
    for inst in program.blocks.iter().flat_map(|block| block.instructions.iter().flatten()) {
        if inst.opcode != Opcode::IsbeWriteAttribute { continue; }
        let address = range(program, *inst.args.first()?, &mut cache, 0);
        // PayloadWord uses min(address >> 2, payload_words - 1). With two
        // words, every address >= 4 aliases the emission count.
        let last = program.mesh.payload_words - 1;
        let low = (address.0 >> 2).min(last);
        let high = (address.1 >> 2).min(last);
        if low <= 1 && high >= 1 {
            bound = bound.max(range(program, *inst.args.get(1)?, &mut cache, 0).1);
        }
    }
    (bound != u32::MAX).then_some(bound)
}

fn barrier(ctx: &mut SpirvEmitContext) {
    let scope = ctx.constant_u32(spirv::Scope::Workgroup as u32);
    let semantics = ctx.constant_u32(
        (spirv::MemorySemantics::ACQUIRE_RELEASE | spirv::MemorySemantics::WORKGROUP_MEMORY).bits(),
    );
    ctx.builder
        .control_barrier(scope, scope, semantics)
        .unwrap();
}

/// Compute-only association supplied implicitly by native task dispatch.
/// Run in the preheader before guest code or payload import. Host dispatches
/// are one-dimensional; guest X is relative to the parent task for mesh.
pub fn emit_mesh_dispatch_import(ctx: &mut SpirvEmitContext) {
    use super::spirv_emit_context::MeshDispatchRecord;
    assert!(ctx.is_task ^ ctx.is_mesh);
    assert_ne!(ctx.mesh_dispatch_buffer, 0);
    assert_ne!(ctx.workgroup_id, 0);
    assert_eq!(
        ctx.mesh_guest_workgroup_id, 0,
        "dispatch mapping is imported once"
    );
    // Deliberately load the physical builtin, before installing its guest view.
    let group = ctx
        .builder
        .load(ctx.u32_vec3_type, None, ctx.workgroup_id, None, [])
        .unwrap();
    let x = ctx
        .builder
        .composite_extract(ctx.u32_type, None, group, [0])
        .unwrap();
    let words = ctx.constant_u32(MeshDispatchRecord::WORDS);
    let base = ctx.builder.i_mul(ctx.u32_type, None, x, words).unwrap();
    let mut fields = [0; 3];
    for (offset, field) in fields.iter_mut().enumerate() {
        let offset = ctx.constant_u32(offset as u32);
        let index = ctx.builder.i_add(ctx.u32_type, None, base, offset).unwrap();
        let pointer = ctx
            .builder
            .access_chain(
                ctx.mesh_dispatch_buffer_u32,
                None,
                ctx.mesh_dispatch_buffer,
                [ctx.const_zero_u32, index],
            )
            .unwrap();
        *field = ctx
            .builder
            .load(ctx.u32_type, None, pointer, None, [])
            .unwrap();
    }
    ctx.mesh_guest_workgroup_id = ctx
        .builder
        .composite_construct(
            ctx.u32_vec3_type,
            None,
            [fields[0], ctx.const_zero_u32, ctx.const_zero_u32],
        )
        .unwrap();
    ctx.mesh_payload_base = fields[1];
    ctx.mesh_output_record = fields[2];
}

/// Reject unused reserved children before payload import, guest memory accesses
/// or barriers. The predicate is uniform across the whole physical workgroup.
/// Rasterizer clears the output allocation so rejected groups have zero draws.
pub fn emit_mesh_task_count_guard(ctx: &mut SpirvEmitContext) {
    assert!(ctx.is_mesh && !ctx.is_task && ctx.mesh_payload_buffer != 0);
    let one = ctx.constant_u32(1);
    let index = ctx.builder.i_add(ctx.u32_type, None, ctx.mesh_payload_base, one).unwrap();
    let pointer = ctx.builder.access_chain(ctx.mesh_payload_buffer_u32, None,
        ctx.mesh_payload_buffer, [ctx.const_zero_u32, index]).unwrap();
    let count = ctx.builder.load(ctx.u32_type, None, pointer, None, []).unwrap();
    let child = ctx.builder.composite_extract(ctx.u32_type, None,
        ctx.mesh_guest_workgroup_id, [0]).unwrap();
    let unused = ctx.builder.u_greater_than_equal(ctx.bool_type, None, child, count).unwrap();
    let reject = ctx.builder.id();
    let active = ctx.builder.id();
    ctx.builder.selection_merge(active, spirv::SelectionControl::NONE).unwrap();
    ctx.builder.branch_conditional(unused, reject, active, []).unwrap();
    ctx.builder.begin_block(Some(reject)).unwrap();
    ctx.builder.ret().unwrap();
    ctx.builder.begin_block(Some(active)).unwrap();
}

fn local_invocation(ctx: &mut SpirvEmitContext) -> Word {
    assert_ne!(
        ctx.local_invocation_id, 0,
        "local invocation input prerequisite"
    );
    let invocation = ctx
        .builder
        .load(ctx.u32_vec3_type, None, ctx.local_invocation_id, None, [])
        .unwrap();
    ctx.builder
        .composite_extract(ctx.u32_type, None, invocation, [0])
        .unwrap()
}

/// Upstream anonymous If helper: leaves the builder at the merge block.
fn if_then(ctx: &mut SpirvEmitContext, condition: Word, body: impl FnOnce(&mut SpirvEmitContext)) {
    let then_label = ctx.builder.id();
    let merge_label = ctx.builder.id();
    ctx.builder
        .selection_merge(merge_label, spirv::SelectionControl::NONE)
        .unwrap();
    ctx.builder
        .branch_conditional(condition, then_label, merge_label, [])
        .unwrap();
    ctx.builder.begin_block(Some(then_label)).unwrap();
    body(ctx);
    ctx.builder.branch(merge_label).unwrap();
    ctx.builder.begin_block(Some(merge_label)).unwrap();
}

/// Compute-only replacement for Vulkan's implicit task-payload handoff.
/// `base` is a word offset into the transport buffer, supplied from the parent
/// task mapping by the dispatch path. It is not the mesh child's workgroup ID.
fn transfer_payload(ctx: &mut SpirvEmitContext, base: Word, export: bool) {
    assert_ne!(ctx.mesh_payload, 0, "payload declaration prerequisite");
    assert_ne!(ctx.mesh_payload_buffer, 0, "payload transport prerequisite");
    assert!(
        ctx.mesh_threads > 0,
        "nonempty compute workgroup prerequisite"
    );
    let invocation = local_invocation(ctx);
    let words = ctx.mesh_payload_words;
    let threads = ctx.mesh_threads;
    for first in (0..words).step_by(threads as usize) {
        let first_id = ctx.constant_u32(first);
        let index = ctx
            .builder
            .i_add(ctx.u32_type, None, invocation, first_id)
            .unwrap();
        let copy = |ctx: &mut SpirvEmitContext| {
            let local = ctx
                .builder
                .access_chain(ctx.mesh_payload_u32, None, ctx.mesh_payload, [index])
                .unwrap();
            let global_index = ctx.builder.i_add(ctx.u32_type, None, base, index).unwrap();
            let global = ctx
                .builder
                .access_chain(
                    ctx.mesh_payload_buffer_u32,
                    None,
                    ctx.mesh_payload_buffer,
                    [ctx.const_zero_u32, global_index],
                )
                .unwrap();
            let (source, target) = if export {
                (local, global)
            } else {
                (global, local)
            };
            let word = ctx
                .builder
                .load(ctx.u32_type, None, source, None, [])
                .unwrap();
            ctx.builder.store(target, word, None, []).unwrap();
        };
        if first + threads > words {
            let end = ctx.constant_u32(words);
            let condition = ctx
                .builder
                .u_less_than(ctx.bool_type, None, index, end)
                .unwrap();
            if_then(ctx, condition, copy);
        } else {
            copy(ctx);
        }
    }
}

/// Reached collectively after task code. The host must issue a Vulkan shader
/// write -> shader read barrier before mesh dispatches read this transport.
/// This exports the payload, including count at word 1; it does not launch mesh.
pub fn emit_task_payload_export(ctx: &mut SpirvEmitContext, base: Word) {
    assert!(
        ctx.is_task && !ctx.is_mesh,
        "task export requires a guest task program"
    );
    barrier(ctx);
    transfer_payload(ctx, base, true);
}

/// Executed before mesh guest code in a separate preheader, so the final block
/// of a conditional tail copy cannot alter guest entry-block phi predecessors.
/// The parent-task mapping and dispatch barrier must already be established.
pub fn emit_mesh_payload_import(ctx: &mut SpirvEmitContext, base: Word) {
    assert!(
        ctx.is_mesh && !ctx.is_task,
        "mesh import requires a guest mesh program"
    );
    transfer_payload(ctx, base, false);
    barrier(ctx);
}

fn clamped(ctx: &mut SpirvEmitContext, index: Word, count: u32) -> Word {
    let last = ctx.constant_u32(count - 1);
    ctx.builder
        .ext_inst(
            ctx.u32_type,
            None,
            ctx.glsl_ext,
            38, // GLSL.std.450 UMin, as in Sirit's OpUMin.
            [Operand::IdRef(index), Operand::IdRef(last)],
        )
        .unwrap()
}

fn output_word(ctx: &mut SpirvEmitContext, index: Word) -> Word {
    assert_ne!(
        ctx.mesh_output_buffer, 0,
        "mesh output transport prerequisite"
    );
    ctx.builder
        .access_chain(
            ctx.mesh_output_buffer_u32,
            None,
            ctx.mesh_output_buffer,
            [ctx.const_zero_u32, index],
        )
        .unwrap()
}

/// Compute replacement for the output handoff in upstream MeshEpilogue.
/// Exports the original packed regions for vertex pulling, without a depth
/// conversion here (that belongs at vertex output, exactly once). `record` is
/// the globally assigned mesh workgroup slot, not the per-parent child index.
/// The host must synchronize storage writes before indirect/vertex reads.
pub fn emit_mesh_output_export(ctx: &mut SpirvEmitContext, record: Word) {
    assert!(
        ctx.is_mesh && !ctx.is_task,
        "mesh output requires a guest mesh program"
    );
    assert!(
        ctx.mesh_primitive_count != 0 && ctx.mesh_threads > 0,
        "mesh shared storage prerequisite"
    );
    let layout = ctx.mesh_output_layout;
    assert_eq!(layout.attribute_words, ctx.mesh_attribute_words);
    assert_eq!(layout.map_words, ctx.mesh_map_bytes);
    barrier(ctx);
    let raw_count = ctx
        .builder
        .load(ctx.u32_type, None, ctx.mesh_primitive_count, None, [])
        .unwrap();
    // Unlike Clamped, the inclusive limit is max_primitives, not max-1.
    let max_count = ctx.constant_u32(ctx.mesh_info.max_primitives);
    let count = ctx
        .builder
        .ext_inst(
            ctx.u32_type,
            None,
            ctx.glsl_ext,
            38,
            [Operand::IdRef(raw_count), Operand::IdRef(max_count)],
        )
        .unwrap();
    let stride = ctx.constant_u32(layout.stride_words);
    let base = ctx
        .builder
        .i_mul(ctx.u32_type, None, record, stride)
        .unwrap();
    let invocation = local_invocation(ctx);
    let leader = ctx
        .builder
        .i_equal(ctx.bool_type, None, invocation, ctx.const_zero_u32)
        .unwrap();
    if_then(ctx, leader, |ctx| {
        let corners = ctx.constant_u32(ctx.mesh_info.primitive_vertices());
        let vertex_count = ctx
            .builder
            .i_mul(ctx.u32_type, None, count, corners)
            .unwrap();
        let span = ctx.constant_u32(layout.vertex_span);
        let first_vertex = ctx.builder.i_mul(ctx.u32_type, None, record, span).unwrap();
        // VkDrawIndirectCommand; one writer, avoiding identical-value SSBO races.
        for (offset, value) in [
            vertex_count,
            ctx.const_one_u32,
            first_vertex,
            ctx.const_zero_u32,
        ]
        .into_iter()
        .enumerate()
        {
            let offset = ctx.constant_u32(offset as u32);
            let index = ctx.builder.i_add(ctx.u32_type, None, base, offset).unwrap();
            let pointer = output_word(ctx, index);
            ctx.builder.store(pointer, value, None, []).unwrap();
        }
    });
    // Raw ISBE layout is preserved, including its 32-element group padding.
    // Every reserved map byte still occupies its own U32 word.
    for (source, offset, words) in [
        (
            ctx.mesh_attributes,
            layout.attribute_base,
            layout.attribute_words,
        ),
        (ctx.mesh_map, layout.map_base, layout.map_words),
    ] {
        let offset = ctx.constant_u32(offset);
        let destination_base = ctx.builder.i_add(ctx.u32_type, None, base, offset).unwrap();
        for first in (0..words).step_by(ctx.mesh_threads as usize) {
            let first_id = ctx.constant_u32(first);
            let index = ctx
                .builder
                .i_add(ctx.u32_type, None, invocation, first_id)
                .unwrap();
            let copy = |ctx: &mut SpirvEmitContext| {
                let source = shared_word(ctx, source, index);
                let target_index = ctx
                    .builder
                    .i_add(ctx.u32_type, None, destination_base, index)
                    .unwrap();
                let target = output_word(ctx, target_index);
                let value = ctx
                    .builder
                    .load(ctx.u32_type, None, source, None, [])
                    .unwrap();
                ctx.builder.store(target, value, None, []).unwrap();
            };
            if first + ctx.mesh_threads > words {
                let end = ctx.constant_u32(words);
                let in_range = ctx
                    .builder
                    .u_less_than(ctx.bool_type, None, index, end)
                    .unwrap();
                if_then(ctx, in_range, copy);
            } else {
                copy(ctx);
            }
        }
    }
}

/// Read the ISBE word for one output line in the replacement vertex shader.
/// `vertex_index` includes VkDrawIndirectCommand::firstVertex. Per-primitive
/// lines are fetched by primitive number; vertex lines use the map's U8 index.
/// Returns unmodified bits; CopyLines-style bitcast/output and ConvertDepth
/// must be applied by the generated vertex entrypoint, not by this transport.
pub fn emit_mesh_output_attribute_read(
    ctx: &mut SpirvEmitContext,
    vertex_index: Word,
    line: u32,
    per_primitive: bool,
) -> Word {
    let layout = ctx.mesh_output_layout;
    assert!(
        layout.vertex_span > 0,
        "zero-capacity mesh has no vertex invocations"
    );
    let lines = if per_primitive {
        ctx.mesh_info.primitive_lines.len()
    } else {
        ctx.mesh_info.vertex_lines.len()
    } as u32;
    assert!(line < lines, "output line must exist in mesh metadata");
    let span = ctx.constant_u32(layout.vertex_span);
    let record = ctx
        .builder
        .u_div(ctx.u32_type, None, vertex_index, span)
        .unwrap();
    let corner = ctx
        .builder
        .u_mod(ctx.u32_type, None, vertex_index, span)
        .unwrap();
    let stride = ctx.constant_u32(layout.stride_words);
    let base = ctx
        .builder
        .i_mul(ctx.u32_type, None, record, stride)
        .unwrap();
    let element = if per_primitive {
        let corners = ctx.constant_u32(ctx.mesh_info.primitive_vertices());
        ctx.builder
            .u_div(ctx.u32_type, None, corner, corners)
            .unwrap()
    } else {
        let map_offset = ctx.constant_u32(layout.map_base + 4);
        let map_base = ctx
            .builder
            .i_add(ctx.u32_type, None, base, map_offset)
            .unwrap();
        let index = ctx
            .builder
            .i_add(ctx.u32_type, None, map_base, corner)
            .unwrap();
        let pointer = output_word(ctx, index);
        ctx.builder
            .load(ctx.u32_type, None, pointer, None, [])
            .unwrap()
    };
    // CopyLines word layout: region + (element / 32)*lines*32 + element%32 + line*32.
    let shift = ctx.constant_u32(5);
    let group = ctx
        .builder
        .shift_right_logical(ctx.u32_type, None, element, shift)
        .unwrap();
    let mask = ctx.constant_u32(31);
    let slot = ctx
        .builder
        .bitwise_and(ctx.u32_type, None, element, mask)
        .unwrap();
    let group_words = ctx.constant_u32(lines * 32);
    let group_base = ctx
        .builder
        .i_mul(ctx.u32_type, None, group, group_words)
        .unwrap();
    let line_base = ctx.constant_u32(
        layout.attribute_base
            + if per_primitive {
                layout.primitive_base
            } else {
                0
            }
            + line * 32,
    );
    let index = ctx
        .builder
        .i_add(ctx.u32_type, None, group_base, slot)
        .unwrap();
    let index = ctx
        .builder
        .i_add(ctx.u32_type, None, index, line_base)
        .unwrap();
    let index = ctx.builder.i_add(ctx.u32_type, None, base, index).unwrap();
    let pointer = output_word(ctx, index);
    ctx.builder
        .load(ctx.u32_type, None, pointer, None, [])
        .unwrap()
}

/// CopyLines for a pulled vertex: primitive lines are copied to every corner,
/// preserving their constant value across the rasterized primitive.
fn copy_lines(ctx: &mut SpirvEmitContext, vertex_index: Word, per_primitive: bool) {
    let targets = if per_primitive {
        ctx.mesh_primitive_targets.clone()
    } else {
        ctx.mesh_vertex_targets.clone()
    };
    for (line, target) in targets.into_iter().enumerate() {
        if target.variable == 0 {
            continue;
        }
        let variable = target.variable;
        let component = target.element;
        let is_float = target.is_float;
        let word = emit_mesh_output_attribute_read(ctx, vertex_index, line as u32, per_primitive);
        let value = if is_float {
            ctx.builder.bitcast(ctx.f32_type, None, word).unwrap()
        } else {
            word
        };
        let pointer = if let Some(component) = component {
            let component = ctx.constant_u32(component);
            ctx.builder
                .access_chain(ctx.output_f32_ptr, None, variable, [component])
                .unwrap()
        } else {
            variable
        };
        ctx.builder.store(pointer, value, None, []).unwrap();
    }
}

/// Graphics-side replacement for MeshEpilogue's output copies. Declarations
/// and unsupported-interface validation belong to define_mesh_vertex_outputs.
pub fn emit_mesh_vertex_outputs(ctx: &mut SpirvEmitContext) {
    assert_eq!(ctx.stage, crate::ir::types::ShaderStage::VertexB);
    assert_ne!(ctx.vertex_index, 0);
    let vertex = ctx
        .builder
        .load(ctx.u32_type, None, ctx.vertex_index, None, [])
        .unwrap();
    if ctx.mesh_info.primitive_vertices() == 1 {
        let size = ctx.constant_f32(ctx.runtime_info.fixed_state_point_size.unwrap_or(1.0));
        ctx.builder
            .store(ctx.output_point_size, size, None, [])
            .unwrap();
    }
    copy_lines(ctx, vertex, false);
    emit_mesh_convert_depth(ctx, vertex);
    copy_lines(ctx, vertex, true);
}

/// Upstream ConvertDepth, applied by the generated vertex entrypoint after
/// copying position. VertexIndex includes firstVertex; transport stays raw.
pub fn emit_mesh_convert_depth(ctx: &mut SpirvEmitContext, vertex_index: Word) {
    if !ctx.runtime_info.convert_depth_mode
        || ctx.profile.support_native_ndc
        || ctx.output_position == 0
    {
        return;
    }
    let lines = &ctx.mesh_info.vertex_lines;
    let z_line = lines
        .iter()
        .position(|&a| a == crate::ir::attribute::Attribute::PositionZ as u32);
    let w_line = lines
        .iter()
        .position(|&a| a == crate::ir::attribute::Attribute::PositionW as u32);
    let (Some(z_line), Some(w_line)) = (z_line, w_line) else {
        return;
    };
    let z = emit_mesh_output_attribute_read(ctx, vertex_index, z_line as u32, false);
    let w = emit_mesh_output_attribute_read(ctx, vertex_index, w_line as u32, false);
    let z = ctx.builder.bitcast(ctx.f32_type, None, z).unwrap();
    let w = ctx.builder.bitcast(ctx.f32_type, None, w).unwrap();
    let sum = ctx.builder.f_add(ctx.f32_type, None, z, w).unwrap();
    let half = ctx.constant_f32(0.5);
    let depth = ctx.builder.f_mul(ctx.f32_type, None, sum, half).unwrap();
    let component = ctx.constant_u32(2);
    let pointer = ctx
        .builder
        .access_chain(ctx.output_f32_ptr, None, ctx.output_position, [component])
        .unwrap();
    ctx.builder.store(pointer, depth, None, []).unwrap();
}

fn shared_word(ctx: &mut SpirvEmitContext, array: Word, index: Word) -> Word {
    ctx.builder
        .access_chain(ctx.mesh_shared_u32, None, array, [index])
        .unwrap()
}

fn payload_word(ctx: &mut SpirvEmitContext, index: Word) -> Word {
    assert_ne!(ctx.mesh_payload, 0, "payload declaration prerequisite");
    let index = clamped(ctx, index, ctx.mesh_payload_words);
    ctx.builder
        .access_chain(ctx.mesh_payload_u32, None, ctx.mesh_payload, [index])
        .unwrap()
}

pub fn emit_isbe_read(ctx: &mut SpirvEmitContext, address: Word) -> Word {
    assert!(
        ctx.is_task ^ ctx.is_mesh,
        "ISBE requires a guest task or mesh program"
    );
    assert_ne!(ctx.workgroup_id, 0, "workgroup input prerequisite");
    let workgroup = super::emit_spirv_context_get_set::emit_workgroup_id(ctx);
    let workgroup = ctx
        .builder
        .composite_extract(ctx.u32_type, None, workgroup, [0])
        .unwrap();
    if ctx.is_task {
        return workgroup;
    }
    let shift = ctx.constant_u32(2);
    let index = ctx
        .builder
        .shift_right_logical(ctx.u32_type, None, address, shift)
        .unwrap();
    let pointer = payload_word(ctx, index);
    let word = ctx
        .builder
        .load(ctx.u32_type, None, pointer, None, [])
        .unwrap();
    let is_zero = ctx
        .builder
        .i_equal(ctx.bool_type, None, address, ctx.const_zero_u32)
        .unwrap();
    ctx.builder
        .select(ctx.u32_type, None, is_zero, workgroup, word)
        .unwrap()
}

pub fn emit_isbe_write_attribute(
    ctx: &mut SpirvEmitContext,
    address: Word,
    value: Word,
) -> Result<(), NotImplementedException> {
    if ctx.is_task && !ctx.is_mesh {
        let shift = ctx.constant_u32(2);
        let index = ctx
            .builder
            .shift_right_logical(ctx.u32_type, None, address, shift)
            .unwrap();
        let pointer = payload_word(ctx, index);
        ctx.builder.store(pointer, value, None, []).unwrap();
        return Ok(());
    }
    emit_mesh_write_attribute(ctx, address, value)
}

/// Mesh branch of upstream EmitIsbeWriteAttribute, mechanically extracted in
/// the same file. Task writes belong to the payload and must not call this.
pub fn emit_mesh_write_attribute(
    ctx: &mut SpirvEmitContext,
    address: Word,
    value: Word,
) -> Result<(), NotImplementedException> {
    if !ctx.is_mesh || ctx.is_task || ctx.mesh_attributes == 0 {
        return Err(NotImplementedException::new(
            "Mesh output storage undefined",
        ));
    }
    let shift = ctx.constant_u32(2);
    let index = ctx
        .builder
        .shift_right_logical(ctx.u32_type, None, address, shift)
        .unwrap();
    let index = clamped(ctx, index, ctx.mesh_attribute_words);
    let pointer = shared_word(ctx, ctx.mesh_attributes, index);
    ctx.builder.store(pointer, value, None, []).unwrap();
    Ok(())
}

pub fn emit_isbe_write_map_u8(
    ctx: &mut SpirvEmitContext,
    address: Word,
    value: Word,
) -> Result<(), NotImplementedException> {
    if !ctx.is_mesh || ctx.is_task || ctx.mesh_map == 0 {
        return Err(NotImplementedException::new(
            "Stage buffer map write outside mesh programs",
        ));
    }
    let index = clamped(ctx, address, ctx.mesh_map_bytes);
    let pointer = shared_word(ctx, ctx.mesh_map, index);
    let mask = ctx.constant_u32(0xff);
    let value = ctx
        .builder
        .bitwise_and(ctx.u32_type, None, value, mask)
        .unwrap();
    ctx.builder.store(pointer, value, None, []).unwrap();
    Ok(())
}

pub fn emit_isbe_write_map_u32(
    ctx: &mut SpirvEmitContext,
    address: Value,
    value: Word,
) -> Result<(), NotImplementedException> {
    // The map is written from its end: the count's address is byte 3, not 0.
    if !ctx.is_mesh || ctx.is_task || ctx.mesh_primitive_count == 0 || address != Value::ImmU32(3) {
        return Err(NotImplementedException::new(
            "32-bit stage buffer map write",
        ));
    }
    ctx.builder
        .store(ctx.mesh_primitive_count, value, None, [])
        .unwrap();
    Ok(())
}

pub fn emit_mesh_prologue(ctx: &mut SpirvEmitContext) {
    if !ctx.is_mesh {
        return;
    }
    assert!(
        !ctx.is_task && ctx.mesh_primitive_count != 0,
        "mesh output storage prerequisite"
    );
    // Every invocation initializes count. Do not introduce a conditional block:
    // the entry label remains a predecessor of later phi instructions.
    ctx.builder
        .store(ctx.mesh_primitive_count, ctx.const_zero_u32, None, [])
        .unwrap();
    barrier(ctx);
}

/// Compute transport correction: a task that exits without an ISBE count
/// write emits no children. Pinned upstream leaves payload word1 undefined;
/// real culled task paths then exported stale workgroup bytes as huge counts.
/// Initialize only the emission count, before guest writes, without changing
/// the entry label or clamping any value subsequently written by the guest.
pub fn emit_task_payload_prologue(ctx: &mut SpirvEmitContext) {
    assert!(ctx.is_task && !ctx.is_mesh && ctx.mesh_payload_words >= 2);
    let one = ctx.constant_u32(1);
    let count = ctx.builder.access_chain(ctx.mesh_payload_u32, None, ctx.mesh_payload, [one]).unwrap();
    ctx.builder.store(count, ctx.const_zero_u32, None, []).unwrap();
    barrier(ctx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::ShaderStage;
    use crate::{ir, profile::Profile, runtime_info::RuntimeInfo, shader_info::MeshInfo};

    #[test]
    fn task_count_bound_checks_dynamic_aliases_overflow_and_payload_clamping() {
        use ir::{opcodes::Opcode, value::InstRef};
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.info.is_task = true;
        program.mesh.payload_words = 352;
        program.add_block();
        let mut append = |opcode, args| {
            let inst = program.blocks[0].append_new_inst(opcode, args);
            Value::Inst(InstRef { block: 0, inst })
        };
        let unknown = append(Opcode::UndefU32, vec![]);
        let count = append(Opcode::BitCount32, vec![unknown]);
        let offset = append(Opcode::IMul32, vec![count, Value::ImmU32(40)]);
        let address = append(Opcode::IAdd32, vec![offset, Value::ImmU32(16)]);
        append(Opcode::IsbeWriteAttribute, vec![address, unknown]);
        append(Opcode::IsbeWriteAttribute, vec![Value::ImmU32(4), count]);
        assert_eq!(task_mesh_workgroup_bound(&program), Some(32));
        // The same dynamic write aliases word 1 when PayloadWord clamps to it.
        program.mesh.payload_words = 2;
        assert_eq!(task_mesh_workgroup_bound(&program), None);
        program.mesh.payload_words = 352;
        // A wrapping address must not be treated as definitely above byte 7.
        let Value::Inst(reference) = address else { unreachable!() };
        program.blocks[0].instructions[reference.inst as usize].as_mut().unwrap()
            .args[1] = Value::ImmU32(u32::MAX);
        assert_eq!(task_mesh_workgroup_bound(&program), None);
    }

    #[test]
    fn task_count_bound_handles_unknown_addresses_and_cycles_conservatively() {
        use ir::{opcodes::Opcode, value::InstRef};
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.info.is_task = true;
        program.mesh.payload_words = 64;
        program.add_block();
        assert_eq!(task_mesh_workgroup_bound(&program), Some(0));
        let cycle = Value::Inst(InstRef { block: 0, inst: 0 });
        program.blocks[0].append_new_inst(Opcode::Identity, vec![cycle]);
        program.blocks[0].append_new_inst(Opcode::IsbeWriteAttribute,
            vec![cycle, Value::ImmU32(31)]);
        assert_eq!(task_mesh_workgroup_bound(&program), Some(31));
        program.blocks[0].append_new_inst(Opcode::IsbeWriteAttribute,
            vec![Value::ImmU32(7), cycle]);
        assert_eq!(task_mesh_workgroup_bound(&program), None);
        program.info.is_task = false;
        assert_eq!(task_mesh_workgroup_bound(&program), None);
    }

    #[test]
    fn task_count_bound_covers_lane_writes_even_when_payload_address_is_unknown() {
        use ir::{opcodes::Opcode, value::InstRef};
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.info.is_task = true;
        program.mesh.payload_words = 64;
        program.add_block();
        let unknown = Value::Inst(InstRef { block: 0, inst: 0 });
        let lane = Value::Inst(InstRef { block: 0, inst: 1 });
        let count = Value::Inst(InstRef { block: 0, inst: 2 });
        program.blocks[0].append_new_inst(Opcode::UndefU32, vec![]);
        program.blocks[0].append_new_inst(Opcode::LaneId, vec![]);
        program.blocks[0].append_new_inst(Opcode::BitCount32, vec![unknown]);
        program.blocks[0].append_new_inst(Opcode::IsbeWriteAttribute, vec![unknown, lane]);
        assert_eq!(task_mesh_workgroup_bound(&program), Some(31));
        program.blocks[0].append_new_inst(Opcode::IsbeWriteAttribute, vec![Value::ImmU32(4), count]);
        assert_eq!(task_mesh_workgroup_bound(&program), Some(32));
        program.blocks[0].instructions[1].as_mut().unwrap().opcode = Opcode::UndefU32;
        assert_eq!(task_mesh_workgroup_bound(&program), None);
    }

    #[test]
    fn dispatch_mapping_preserves_parent_payload_and_guest_group_for_both_read_paths() {
        use super::super::spirv_emit_context::MeshDispatchRecord;
        assert_eq!(std::mem::size_of::<MeshDispatchRecord>(), 12);
        assert_eq!(std::mem::align_of::<MeshDispatchRecord>(), 4);
        for task in [true, false] {
            let mut program = ir::Program::new(ShaderStage::Compute);
            program.info.is_task = task;
            program.info.is_mesh = !task;
            program.info.uses_workgroup_id = true;
            let mut ctx =
                SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
            ctx.define_global_variables(
                &program,
                &mut crate::backend::bindings::Bindings::default(),
            );
            ctx.define_mesh_dispatch_buffer(9);
            ctx.define_mesh_payload_workgroup(8);
            ctx.builder
                .begin_function(
                    ctx.void_type,
                    None,
                    spirv::FunctionControl::NONE,
                    ctx.void_fn_type,
                )
                .unwrap();
            ctx.builder.begin_block(None).unwrap();
            emit_mesh_dispatch_import(&mut ctx);
            let vector = super::super::emit_spirv_context_get_set::emit_workgroup_id(&mut ctx);
            let components: Vec<_> = (0..3)
                .map(|i| {
                    ctx.builder
                        .composite_extract(ctx.u32_type, None, vector, [i])
                        .unwrap()
                })
                .collect();
            let zero = ctx.const_zero_u32;
            let four = ctx.constant_u32(4);
            let read_zero = emit_isbe_read(&mut ctx, zero);
            let read_four = emit_isbe_read(&mut ctx, four);
            ctx.builder.ret().unwrap();
            ctx.builder.end_function().unwrap();
            let flat: Vec<_> = ctx
                .builder
                .module_ref()
                .functions
                .last()
                .unwrap()
                .blocks
                .iter()
                .flat_map(|b| &b.instructions)
                .collect();
            // Two children share their parent's payload, while physical groups,
            // guest child indices and output records intentionally differ.
            for (host, record) in [
                (
                    31,
                    MeshDispatchRecord {
                        guest_workgroup_x: 0,
                        payload_base_words: 352,
                        output_record: 71,
                    },
                ),
                (
                    32,
                    MeshDispatchRecord {
                        guest_workgroup_x: 1,
                        payload_base_words: 352,
                        output_record: 72,
                    },
                ),
                (
                    33,
                    MeshDispatchRecord {
                        guest_workgroup_x: 0,
                        payload_base_words: 704,
                        output_record: 73,
                    },
                ),
            ] {
                let mut memory = std::collections::HashMap::new();
                memory.insert((ctx.workgroup_id, 0), host);
                for (offset, value) in record.words().into_iter().enumerate() {
                    memory.insert(
                        (
                            ctx.mesh_dispatch_buffer,
                            host * MeshDispatchRecord::WORDS + offset as u32,
                        ),
                        value,
                    );
                }
                memory.insert((ctx.mesh_payload, 0), 0xaaaaaaaa);
                memory.insert((ctx.mesh_payload, 1), 0x12345678);
                let eval = |id| scalar_with_memory(&ctx, &flat, id, 0, &memory);
                assert_eq!(eval(components[0]), record.guest_workgroup_x);
                assert_eq!(eval(components[1]), 0);
                assert_eq!(eval(components[2]), 0);
                assert_eq!(eval(read_zero), record.guest_workgroup_x);
                assert_eq!(
                    eval(read_four),
                    if task {
                        record.guest_workgroup_x
                    } else {
                        0x12345678
                    }
                );
                assert_eq!(eval(ctx.mesh_payload_base), record.payload_base_words);
                assert_eq!(eval(ctx.mesh_output_record), record.output_record);
            }
        }
    }

    #[test]
    fn stage_buffer_ir_survives_dce_and_reaches_mesh_emitters() {
        use crate::ir_opt::{
            collect_shader_info_pass::collect_shader_info_pass,
            dead_code_elimination_pass::dead_code_elimination_pass,
        };
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.info.is_mesh = true;
        program.workgroup_size = [8, 1, 1];
        program.blocks.push(ir::Block::new());
        program.post_order_blocks.push(0);
        let mut emitter = ir::Emitter::new(&mut program, 0);
        let read = emitter.isbe_read(Value::ImmU32(8));
        emitter.isbe_read(Value::ImmU32(12)); // Unused read is removable.
        emitter.isbe_write_attribute(Value::ImmU32(4), read);
        emitter.isbe_write_map_u8(Value::ImmU32(7), Value::ImmU32(0x123));
        emitter.isbe_write_map_u32(Value::ImmU32(3), Value::ImmU32(5));
        dead_code_elimination_pass(&mut program);
        collect_shader_info_pass(&mut program);
        let live: Vec<_> = program.blocks[0]
            .iter()
            .filter(|i| i.opcode != ir::Opcode::Void)
            .collect();
        assert_eq!(live.len(), 4);
        assert_eq!(live[0].opcode, ir::Opcode::IsbeRead);
        assert_eq!(live[0].opcode.meta().return_type, ir::Type::U32);
        for inst in &live[1..] {
            assert!(inst.may_have_side_effects());
            assert_eq!(inst.opcode.meta().return_type, ir::Type::Void);
            assert_eq!(
                inst.opcode.meta().arg_types,
                &[ir::Type::U32, ir::Type::U32]
            );
        }
        assert!(program.info.uses_workgroup_id && program.info.uses_local_invocation_id);
        assert_eq!(program.info.nvn_buffer_base, 0x310);
        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        ctx.define_global_variables(&program, &mut crate::backend::bindings::Bindings::default());
        ctx.define_mesh_shared_memory(&MeshInfo {
            max_vertices: 32,
            max_primitives: 16,
            vertex_lines: [28].into_iter().collect(),
            ..Default::default()
        });
        ctx.define_mesh_payload_workgroup(8);
        ctx.builder
            .begin_function(
                ctx.void_type,
                None,
                spirv::FunctionControl::NONE,
                ctx.void_fn_type,
            )
            .unwrap();
        ctx.builder.begin_block(None).unwrap();
        ctx.emit_block_instructions(&program, 0);
        ctx.builder.ret().unwrap();
        ctx.builder.end_function().unwrap();
        let flat: Vec<_> = ctx
            .builder
            .module_ref()
            .functions
            .last()
            .unwrap()
            .blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .collect();
        let stores: Vec<_> = flat
            .iter()
            .filter(|i| i.class.opcode == spirv::Op::Store)
            .collect();
        assert_eq!(stores.len(), 3);
        let attr_pointer = definition(&flat, id(&stores[0].operands[0]));
        assert_eq!(
            attr_pointer.operands[0],
            Operand::IdRef(ctx.mesh_attributes)
        );
        let map_pointer = definition(&flat, id(&stores[1].operands[0]));
        assert_eq!(map_pointer.operands[0], Operand::IdRef(ctx.mesh_map));
        assert_eq!(scalar(&ctx, &flat, id(&stores[1].operands[1]), 0), 0x23);
        assert_eq!(
            stores[2].operands[0],
            Operand::IdRef(ctx.mesh_primitive_count)
        );
        assert_eq!(constant(&ctx, &stores[2].operands[1]), 5);
    }

    fn context(mesh: bool) -> SpirvEmitContext {
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.info.is_mesh = mesh;
        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        if mesh {
            ctx.define_mesh_shared_memory(&MeshInfo {
                max_vertices: 32,
                max_primitives: 16,
                topology: 4,
                vertex_lines: [28, 29, 30, 31].into_iter().collect(),
                ..Default::default()
            });
        }
        ctx.builder
            .begin_function(
                ctx.void_type,
                None,
                spirv::FunctionControl::NONE,
                ctx.void_fn_type,
            )
            .unwrap();
        ctx.builder.begin_block(None).unwrap();
        ctx
    }

    fn instructions(ctx: &SpirvEmitContext) -> &[rspirv::dr::Instruction] {
        &ctx.builder.module_ref().functions.last().unwrap().blocks[0].instructions
    }

    fn payload_context(is_task: bool) -> SpirvEmitContext {
        payload_context_with_shape(is_task, 352, 32)
    }

    fn payload_context_with_shape(is_task: bool, words: u32, threads: u32) -> SpirvEmitContext {
        let mut program = ir::Program::new(ShaderStage::Compute);
        assert!(!program.info.is_task && !program.info.is_mesh);
        program.info.is_task = is_task;
        program.info.is_mesh = !is_task;
        program.info.uses_workgroup_id = true;
        program.info.uses_local_invocation_id = true;
        program.workgroup_size = [threads, 1, 1];
        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        ctx.define_global_variables(&program, &mut crate::backend::bindings::Bindings::default());
        ctx.define_mesh_payload_workgroup(words);
        ctx.define_mesh_payload_buffer(7);
        ctx.builder
            .begin_function(
                ctx.void_type,
                None,
                spirv::FunctionControl::NONE,
                ctx.void_fn_type,
            )
            .unwrap();
        ctx.builder.begin_block(None).unwrap();
        ctx
    }

    #[test]
    fn task_reads_index_for_every_address_and_writes_clamped_payload_words() {
        let mut ctx = payload_context(true);
        let address = ctx.constant_u32(u32::MAX);
        let value = emit_isbe_read(&mut ctx, address);
        emit_isbe_write_attribute(&mut ctx, address, value).unwrap();
        let ops = instructions(&ctx);
        assert_eq!(
            ops.iter().map(|i| i.class.opcode).collect::<Vec<_>>(),
            [
                spirv::Op::Load,
                spirv::Op::CompositeExtract,
                spirv::Op::ShiftRightLogical,
                spirv::Op::ExtInst,
                spirv::Op::AccessChain,
                spirv::Op::Store,
            ]
        );
        assert_eq!(ops[0].operands, [Operand::IdRef(ctx.workgroup_id)]);
        assert_eq!(
            ops[1].operands,
            [
                Operand::IdRef(ops[0].result_id.unwrap()),
                Operand::LiteralBit32(0)
            ]
        );
        assert_eq!(ops[1].result_id, Some(value));
        assert_eq!(ops[2].operands[0], Operand::IdRef(address));
        assert_eq!(constant(&ctx, &ops[2].operands[1]), 2);
        assert_eq!(ops[3].operands[1], Operand::LiteralExtInstInteger(38));
        assert_eq!(constant(&ctx, &ops[3].operands[3]), 351);
        assert_eq!(
            ops[4].operands,
            [
                Operand::IdRef(ctx.mesh_payload),
                Operand::IdRef(ops[3].result_id.unwrap())
            ]
        );
        assert_eq!(
            ops[5].operands,
            [
                Operand::IdRef(ops[4].result_id.unwrap()),
                Operand::IdRef(value)
            ]
        );
    }

    #[test]
    fn mesh_reads_index_only_at_byte_zero_and_otherwise_clamped_payload() {
        for address in [0, 1, 3, 4, 1407, 1408, u32::MAX] {
            let mut ctx = payload_context(false);
            let address = ctx.constant_u32(address);
            let result = emit_isbe_read(&mut ctx, address);
            let ops = instructions(&ctx);
            assert_eq!(
                ops.iter().map(|i| i.class.opcode).collect::<Vec<_>>(),
                [
                    spirv::Op::Load,
                    spirv::Op::CompositeExtract,
                    spirv::Op::ShiftRightLogical,
                    spirv::Op::ExtInst,
                    spirv::Op::AccessChain,
                    spirv::Op::Load,
                    spirv::Op::IEqual,
                    spirv::Op::Select,
                ]
            );
            assert_eq!(ops[2].operands[0], Operand::IdRef(address));
            assert_eq!(constant(&ctx, &ops[2].operands[1]), 2);
            assert_eq!(constant(&ctx, &ops[3].operands[3]), 351);
            assert_eq!(
                ops[4].operands,
                [
                    Operand::IdRef(ctx.mesh_payload),
                    Operand::IdRef(ops[3].result_id.unwrap())
                ]
            );
            assert_eq!(ops[5].operands, [Operand::IdRef(ops[4].result_id.unwrap())]);
            assert_eq!(
                ops[6].operands,
                [Operand::IdRef(address), Operand::IdRef(ctx.const_zero_u32)]
            );
            assert_eq!(ops[7].result_id, Some(result));
            assert_eq!(
                ops[7].operands,
                [
                    Operand::IdRef(ops[6].result_id.unwrap()),
                    Operand::IdRef(ops[1].result_id.unwrap()),
                    Operand::IdRef(ops[5].result_id.unwrap())
                ]
            );
        }
    }

    #[test]
    fn guest_task_classification_rejects_map_writes_even_if_storage_exists() {
        let mut ctx = context(true);
        ctx.is_mesh = false;
        ctx.is_task = true;
        let zero = ctx.const_zero_u32;
        assert!(emit_isbe_write_map_u8(&mut ctx, zero, zero).is_err());
        assert!(emit_isbe_write_map_u32(&mut ctx, Value::ImmU32(3), zero).is_err());
        emit_mesh_prologue(&mut ctx);
        assert!(instructions(&ctx).is_empty());
    }

    #[test]
    fn compute_payload_transfers_cover_each_word_once_and_preserve_parent_base() {
        // Captured shapes plus non-divisible tails; includes more lanes than words.
        for (words, threads) in [(352, 32), (352, 8), (64, 32), (32, 1), (32, 63), (352, 63)] {
            for export in [false, true] {
                let mut ctx = payload_context_with_shape(export, words, threads);
                let base = ctx.constant_u32(1056); // Third 352-word parent, not child workgroup ID.
                if export {
                    emit_task_payload_export(&mut ctx, base);
                } else {
                    emit_mesh_payload_import(&mut ctx, base);
                }
                ctx.builder.ret().unwrap();
                ctx.builder.end_function().unwrap();
                let function = ctx.builder.module_ref().functions.last().unwrap();
                let flat: Vec<_> = function
                    .blocks
                    .iter()
                    .flat_map(|b| &b.instructions)
                    .collect();
                let barriers: Vec<_> = flat
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| i.class.opcode == spirv::Op::ControlBarrier)
                    .collect();
                assert_eq!(barriers.len(), 1);
                if export {
                    assert_eq!(barriers[0].0, 0);
                } else {
                    assert_eq!(barriers[0].0, flat.len() - 2);
                } // Immediately before return.
                let mut coverage = Vec::new();
                for lane in 0..threads {
                    let mut block_index = 0;
                    'blocks: loop {
                        for inst in &function.blocks[block_index].instructions {
                            match inst.class.opcode {
                                spirv::Op::Branch | spirv::Op::BranchConditional => {
                                    let target = if inst.class.opcode == spirv::Op::Branch {
                                        id(&inst.operands[0])
                                    } else {
                                        let condition =
                                            scalar(&ctx, &flat, id(&inst.operands[0]), lane);
                                        id(&inst.operands[if condition != 0 { 1 } else { 2 }])
                                    };
                                    block_index = function
                                        .blocks
                                        .iter()
                                        .position(|b| {
                                            b.label.as_ref().unwrap().result_id == Some(target)
                                        })
                                        .unwrap();
                                    continue 'blocks;
                                }
                                spirv::Op::Store => {
                                    let target = definition(&flat, id(&inst.operands[0]));
                                    let load = definition(&flat, id(&inst.operands[1]));
                                    assert_eq!(load.class.opcode, spirv::Op::Load);
                                    let source = definition(&flat, id(&load.operands[0]));
                                    let (local, global) = if export {
                                        (source, target)
                                    } else {
                                        (target, source)
                                    };
                                    assert_eq!(local.class.opcode, spirv::Op::AccessChain);
                                    assert_eq!(global.class.opcode, spirv::Op::AccessChain);
                                    assert_eq!(id(&local.operands[0]), ctx.mesh_payload);
                                    assert_eq!(id(&global.operands[0]), ctx.mesh_payload_buffer);
                                    assert_eq!(constant(&ctx, &global.operands[1]), 0);
                                    let index = scalar(&ctx, &flat, id(&local.operands[1]), lane);
                                    let global_index =
                                        scalar(&ctx, &flat, id(&global.operands[2]), lane);
                                    assert!(index < words);
                                    assert_eq!(global_index, 1056 + index);
                                    coverage.push(index);
                                }
                                spirv::Op::Return => break 'blocks,
                                _ => {}
                            }
                        }
                        panic!("unterminated transfer block");
                    }
                }
                coverage.sort_unstable();
                assert_eq!(coverage, (0..words).collect::<Vec<_>>());
            }
        }
    }

    fn id(operand: &Operand) -> Word {
        let Operand::IdRef(id) = operand else {
            panic!("expected id")
        };
        *id
    }

    fn definition<'a>(
        instructions: &[&'a rspirv::dr::Instruction],
        id: Word,
    ) -> &'a rspirv::dr::Instruction {
        instructions
            .iter()
            .find(|i| i.result_id == Some(id))
            .unwrap()
    }

    // Evaluate only the scalar address/branch expressions actually emitted by
    // the transfer kernel. Traversing branches catches tail-lane writes that a
    // host-only size calculation or opcode-presence test would miss.
    fn scalar(
        ctx: &SpirvEmitContext,
        instructions: &[&rspirv::dr::Instruction],
        value: Word,
        lane: u32,
    ) -> u32 {
        scalar_with_memory(ctx, instructions, value, lane, &Default::default())
    }

    fn scalar_with_memory(
        ctx: &SpirvEmitContext,
        instructions: &[&rspirv::dr::Instruction],
        value: Word,
        lane: u32,
        memory: &std::collections::HashMap<(Word, u32), u32>,
    ) -> u32 {
        if let Some(inst) = ctx
            .builder
            .module_ref()
            .types_global_values
            .iter()
            .find(|i| i.result_id == Some(value))
        {
            let [Operand::LiteralBit32(n)] = inst.operands.as_slice() else {
                panic!("expected scalar constant")
            };
            return *n;
        }
        let inst = definition(instructions, value);
        let operand = |index: usize| {
            scalar_with_memory(ctx, instructions, id(&inst.operands[index]), lane, memory)
        };
        match inst.class.opcode {
            spirv::Op::CompositeExtract => {
                let Operand::LiteralBit32(component) = inst.operands[1] else {
                    panic!("component");
                };
                let load = definition(instructions, id(&inst.operands[0]));
                if load.class.opcode == spirv::Op::CompositeConstruct {
                    return scalar_with_memory(
                        ctx,
                        instructions,
                        id(&load.operands[component as usize]),
                        lane,
                        memory,
                    );
                }
                assert_eq!(load.class.opcode, spirv::Op::Load);
                if load.operands == [Operand::IdRef(ctx.workgroup_id)] {
                    return memory[&(ctx.workgroup_id, component)];
                }
                assert_eq!(component, 0);
                assert_eq!(load.operands, [Operand::IdRef(ctx.local_invocation_id)]);
                lane
            }
            spirv::Op::Select => {
                if operand(0) != 0 {
                    operand(1)
                } else {
                    operand(2)
                }
            }
            spirv::Op::Bitcast => operand(0),
            spirv::Op::FAdd => (f32::from_bits(operand(0)) + f32::from_bits(operand(1))).to_bits(),
            spirv::Op::FMul => (f32::from_bits(operand(0)) * f32::from_bits(operand(1))).to_bits(),
            spirv::Op::IAdd => operand(0).wrapping_add(operand(1)),
            spirv::Op::IMul => operand(0).wrapping_mul(operand(1)),
            spirv::Op::UDiv => operand(0) / operand(1),
            spirv::Op::UMod => operand(0) % operand(1),
            spirv::Op::ULessThan => u32::from(operand(0) < operand(1)),
            spirv::Op::IEqual => u32::from(operand(0) == operand(1)),
            spirv::Op::ShiftRightLogical => operand(0) >> operand(1),
            spirv::Op::BitwiseAnd => operand(0) & operand(1),
            spirv::Op::ExtInst => {
                assert_eq!(inst.operands[1], Operand::LiteralExtInstInteger(38));
                operand(2).min(operand(3))
            }
            spirv::Op::Load => {
                let pointer = id(&inst.operands[0]);
                if let Some(value) = memory.get(&(pointer, 0)) {
                    return *value;
                }
                let chain = definition(instructions, pointer);
                assert_eq!(chain.class.opcode, spirv::Op::AccessChain);
                let base = id(&chain.operands[0]);
                let offset = scalar_with_memory(
                    ctx,
                    instructions,
                    id(chain.operands.last().unwrap()),
                    lane,
                    memory,
                );
                *memory
                    .get(&(base, offset))
                    .expect("initialized memory read")
            }
            _ => panic!("unexpected address instruction {:?}", inst.class.opcode),
        }
    }

    fn constant(ctx: &SpirvEmitContext, operand: &Operand) -> u32 {
        let (Operand::IdRef(id) | Operand::IdScope(id) | Operand::IdMemorySemantics(id)) = operand
        else {
            panic!("expected constant id")
        };
        let inst = ctx
            .builder
            .module_ref()
            .types_global_values
            .iter()
            .find(|inst| inst.result_id == Some(*id))
            .unwrap();
        let [Operand::LiteralBit32(value)] = inst.operands.as_slice() else {
            panic!("constant")
        };
        *value
    }

    fn output_context(mesh: &MeshInfo, threads: u32, compute: bool) -> SpirvEmitContext {
        let mut program = ir::Program::new(if compute {
            ShaderStage::Compute
        } else {
            ShaderStage::VertexB
        });
        program.info.is_mesh = compute;
        program.info.uses_local_invocation_id = compute;
        program.workgroup_size = [threads, 1, 1];
        let mut ctx = SpirvEmitContext::new(
            &program,
            &Profile {
                supported_spirv: 0x0001_0400,
                ..Default::default()
            },
            &RuntimeInfo::default(),
        );
        ctx.define_global_variables(&program, &mut crate::backend::bindings::Bindings::default());
        if compute {
            ctx.define_mesh_shared_memory(mesh);
        }
        ctx.define_mesh_output_buffer(mesh, 8);
        ctx.builder
            .begin_function(
                ctx.void_type,
                None,
                spirv::FunctionControl::NONE,
                ctx.void_fn_type,
            )
            .unwrap();
        ctx.builder.begin_block(None).unwrap();
        ctx
    }

    #[test]
    fn pulled_vertex_copies_both_regions_and_shader_point_size_overrides_fixed_size() {
        for topology in [0, 4] {
            let mesh = MeshInfo {
                max_vertices: 64,
                max_primitives: 64,
                topology,
                vertex_lines: [28, 29, 30, 31, 27, 36, 37].into_iter().collect(),
                primitive_lines: [32, 33].into_iter().collect(),
                ..Default::default()
            };
            let program = ir::Program::new(ShaderStage::VertexB);
            let mut ctx = SpirvEmitContext::new(
                &program,
                &Profile::default(),
                &RuntimeInfo {
                    convert_depth_mode: true,
                    fixed_state_point_size: Some(7.0),
                    ..Default::default()
                },
            );
            ctx.define_mesh_vertex_outputs(&mesh, 8).unwrap();
            ctx.builder
                .begin_function(
                    ctx.void_type,
                    None,
                    spirv::FunctionControl::NONE,
                    ctx.void_fn_type,
                )
                .unwrap();
            ctx.builder.begin_block(None).unwrap();
            emit_mesh_vertex_outputs(&mut ctx);
            ctx.builder.ret().unwrap();
            ctx.builder.end_function().unwrap();
            let flat: Vec<_> = ctx
                .builder
                .module_ref()
                .functions
                .last()
                .unwrap()
                .blocks
                .iter()
                .flat_map(|b| &b.instructions)
                .collect();
            let layout = ctx.mesh_output_layout;
            let corner = 33 * mesh.primitive_vertices();
            let base = layout.stride_words;
            let mut memory = std::collections::HashMap::new();
            memory.insert((ctx.vertex_index, 0), layout.vertex_span + corner);
            memory.insert(
                (ctx.mesh_output_buffer, base + layout.map_base + 4 + corner),
                35,
            );
            let vertex_bits = [
                0.25_f32.to_bits(),
                (-0.5_f32).to_bits(),
                (-2.0_f32).to_bits(),
                4.0_f32.to_bits(),
                3.0_f32.to_bits(),
                0xffa1_2345,
                0x1234_5678,
            ];
            for (line, bits) in vertex_bits.into_iter().enumerate() {
                memory.insert(
                    (
                        ctx.mesh_output_buffer,
                        base + layout.attribute_base + 7 * 32 + 3 + line as u32 * 32,
                    ),
                    bits,
                );
            }
            for (line, bits) in [0x8765_4321, 0xdead_beef].into_iter().enumerate() {
                memory.insert(
                    (
                        ctx.mesh_output_buffer,
                        base + layout.attribute_base
                            + layout.primitive_base
                            + 2 * 32
                            + 1
                            + line as u32 * 32,
                    ),
                    bits,
                );
            }
            let mut outputs = std::collections::HashMap::new();
            let mut point_writes = Vec::new();
            for store in flat.iter().filter(|i| i.class.opcode == spirv::Op::Store) {
                let pointer = id(&store.operands[0]);
                let key = if let Some(chain) = flat.iter().find(|i| i.result_id == Some(pointer)) {
                    assert_eq!(chain.class.opcode, spirv::Op::AccessChain);
                    (id(&chain.operands[0]), constant(&ctx, &chain.operands[1]))
                } else {
                    (pointer, 0)
                };
                let bits = scalar_with_memory(&ctx, &flat, id(&store.operands[1]), 0, &memory);
                outputs.insert(key, bits);
                if key.0 == ctx.output_point_size {
                    point_writes.push(bits);
                }
            }
            for (element, bits) in [
                vertex_bits[0],
                vertex_bits[1],
                1.0_f32.to_bits(),
                vertex_bits[3],
            ]
            .into_iter()
            .enumerate()
            {
                assert_eq!(outputs[&(ctx.output_position, element as u32)], bits);
            }
            assert_eq!(outputs[&(ctx.output_generics[1][0].id, 0)], vertex_bits[5]);
            assert_eq!(outputs[&(ctx.output_generics[1][1].id, 1)], vertex_bits[6]);
            assert_eq!(outputs[&(ctx.output_generics[0][0].id, 0)], 0x8765_4321);
            assert_eq!(outputs[&(ctx.output_generics[0][1].id, 1)], 0xdead_beef);
            assert_eq!(
                point_writes,
                if topology == 0 {
                    vec![7.0_f32.to_bits(), 3.0_f32.to_bits()]
                } else {
                    vec![3.0_f32.to_bits()]
                }
            );
        }
    }

    #[test]
    fn mesh_depth_conversion_uses_mapped_position_and_upstream_guards() {
        for mode in 0..6 {
            let mesh = MeshInfo {
                max_vertices: 64,
                max_primitives: 16,
                topology: 4,
                // Deliberately reordered: attribute number is not a line offset.
                vertex_lines: (if mode == 4 {
                    vec![30]
                } else if mode == 5 {
                    vec![31]
                } else {
                    vec![31, 28, 30]
                })
                .into_iter()
                .collect(),
                ..Default::default()
            };
            let mut ctx = output_context(&mesh, 32, false);
            ctx.runtime_info.convert_depth_mode = mode != 1;
            ctx.profile.support_native_ndc = mode == 2;
            if mode == 3 {
                ctx.output_position = 0;
            }
            let vertex = ctx.constant_u32(ctx.mesh_output_layout.vertex_span * 2 + 7);
            emit_mesh_convert_depth(&mut ctx, vertex);
            ctx.builder.ret().unwrap();
            ctx.builder.end_function().unwrap();
            let flat: Vec<_> = ctx
                .builder
                .module_ref()
                .functions
                .last()
                .unwrap()
                .blocks
                .iter()
                .flat_map(|b| &b.instructions)
                .collect();
            let stores: Vec<_> = flat
                .iter()
                .filter(|i| i.class.opcode == spirv::Op::Store)
                .collect();
            if mode != 0 {
                assert!(stores.is_empty());
                assert!(!flat.iter().any(|i| i.class.opcode == spirv::Op::Load));
                continue;
            }
            assert_eq!(stores.len(), 1);
            let pointer = definition(&flat, id(&stores[0].operands[0]));
            assert_eq!(pointer.operands[0], Operand::IdRef(ctx.output_position));
            assert_eq!(constant(&ctx, &pointer.operands[1]), 2);
            let layout = ctx.mesh_output_layout;
            let base = 2 * layout.stride_words;
            for (z, w) in [(-1.0_f32, 1.0_f32), (1.0, 1.0), (2.0, 4.0), (-8.0, 2.0)] {
                let mut memory = std::collections::HashMap::new();
                memory.insert((ctx.mesh_output_buffer, base + layout.map_base + 4 + 7), 33);
                let first = base + layout.attribute_base + 3 * 32 + 1;
                memory.insert((ctx.mesh_output_buffer, first), w.to_bits());
                memory.insert((ctx.mesh_output_buffer, first + 2 * 32), z.to_bits());
                assert_eq!(
                    scalar_with_memory(&ctx, &flat, id(&stores[0].operands[1]), 0, &memory),
                    ((z + w) * 0.5).to_bits()
                );
            }
        }
    }

    #[test]
    fn mesh_export_writes_exact_indirect_header_and_copies_all_isbe_words_once() {
        for (vertices, primitives, topology, vlines, plines, threads) in [
            (32, 16, 4, 6, 1, 8),      // Captured small mesh.
            (256, 192, 4, 12, 13, 32), // Captured largest mesh.
            (33, 33, 1, 2, 3, 63),     // Partial groups and partial lane copies.
            (33, 33, 0, 2, 1, 32),
            (0, 0, 4, 0, 0, 32),
        ] {
            let mesh = MeshInfo {
                max_vertices: vertices,
                max_primitives: primitives,
                topology,
                vertex_lines: (28..28 + vlines).collect(),
                primitive_lines: (32..32 + plines).collect(),
                ..Default::default()
            };
            let mut ctx = output_context(&mesh, threads, true);
            let record = ctx.constant_u32(2);
            emit_mesh_output_export(&mut ctx, record);
            ctx.builder.ret().unwrap();
            ctx.builder.end_function().unwrap();
            let function = ctx.builder.module_ref().functions.last().unwrap();
            let flat: Vec<_> = function
                .blocks
                .iter()
                .flat_map(|b| &b.instructions)
                .collect();
            assert_eq!(flat[0].class.opcode, spirv::Op::ControlBarrier);
            assert_eq!(
                flat.iter()
                    .filter(|i| i.class.opcode == spirv::Op::ControlBarrier)
                    .count(),
                1
            );
            let layout = ctx.mesh_output_layout;
            let base = 2 * layout.stride_words;
            for raw_count in [0, primitives / 2, u32::MAX] {
                let mut memory = std::collections::HashMap::new();
                memory.insert((ctx.mesh_primitive_count, 0), raw_count);
                for word in 0..layout.attribute_words {
                    memory.insert((ctx.mesh_attributes, word), 0x8000_0000 ^ word);
                }
                for word in 0..layout.map_words {
                    memory.insert((ctx.mesh_map, word), 0xabcd_0000 ^ word);
                }
                let mut writes = Vec::new();
                for lane in 0..threads {
                    let mut block_index = 0;
                    'blocks: loop {
                        for inst in &function.blocks[block_index].instructions {
                            match inst.class.opcode {
                                spirv::Op::Branch | spirv::Op::BranchConditional => {
                                    let target = if inst.class.opcode == spirv::Op::Branch {
                                        id(&inst.operands[0])
                                    } else {
                                        id(&inst.operands[if scalar_with_memory(
                                            &ctx,
                                            &flat,
                                            id(&inst.operands[0]),
                                            lane,
                                            &memory,
                                        ) != 0
                                        {
                                            1
                                        } else {
                                            2
                                        }])
                                    };
                                    block_index = function
                                        .blocks
                                        .iter()
                                        .position(|b| {
                                            b.label.as_ref().unwrap().result_id == Some(target)
                                        })
                                        .unwrap();
                                    continue 'blocks;
                                }
                                spirv::Op::Store => {
                                    let pointer = definition(&flat, id(&inst.operands[0]));
                                    assert_eq!(pointer.class.opcode, spirv::Op::AccessChain);
                                    assert_eq!(
                                        pointer.operands[0],
                                        Operand::IdRef(ctx.mesh_output_buffer)
                                    );
                                    assert_eq!(constant(&ctx, &pointer.operands[1]), 0);
                                    let index = scalar_with_memory(
                                        &ctx,
                                        &flat,
                                        id(&pointer.operands[2]),
                                        lane,
                                        &memory,
                                    );
                                    let value = scalar_with_memory(
                                        &ctx,
                                        &flat,
                                        id(&inst.operands[1]),
                                        lane,
                                        &memory,
                                    );
                                    writes.push((index, value));
                                }
                                spirv::Op::Return => break 'blocks,
                                _ => {}
                            }
                        }
                        panic!("unterminated mesh export");
                    }
                }
                writes.sort_unstable();
                let mut expected = vec![
                    (base, raw_count.min(primitives) * mesh.primitive_vertices()),
                    (base + 1, 1),
                    (base + 2, 2 * layout.vertex_span),
                    (base + 3, 0),
                ];
                expected.extend(
                    (0..layout.attribute_words)
                        .map(|w| (base + layout.attribute_base + w, 0x8000_0000 ^ w)),
                );
                expected.extend(
                    (0..layout.map_words).map(|w| (base + layout.map_base + w, 0xabcd_0000 ^ w)),
                );
                assert_eq!(writes, expected); // Also proves no duplicate/racing header writers.
            }
        }
    }

    #[test]
    fn vertex_pulling_reads_indexed_vertex_lines_and_shared_primitive_lines_across_groups() {
        // Largest captured layout, probing both sides of the 32-element boundary.
        let mesh = MeshInfo {
            max_vertices: 256,
            max_primitives: 192,
            topology: 4,
            vertex_lines: (0..12).collect(),
            primitive_lines: (0..13).collect(),
            ..Default::default()
        };
        for primitive in [0, 31, 32, 191] {
            for corner in 0..3 {
                for mapped_vertex in [0, 31, 32, 255] {
                    for per_primitive in [false, true] {
                        let mut ctx = output_context(&mesh, 1, false);
                        let layout = ctx.mesh_output_layout;
                        let base = 2 * layout.stride_words;
                        let vertex_index =
                            ctx.constant_u32(2 * layout.vertex_span + primitive * 3 + corner);
                        let line = if per_primitive { 12 } else { 11 };
                        let value = emit_mesh_output_attribute_read(
                            &mut ctx,
                            vertex_index,
                            line,
                            per_primitive,
                        );
                        let flat: Vec<_> = ctx
                            .builder
                            .module_ref()
                            .functions
                            .last()
                            .unwrap()
                            .blocks
                            .iter()
                            .flat_map(|b| &b.instructions)
                            .collect();
                        let mut memory = std::collections::HashMap::new();
                        memory.insert(
                            (
                                ctx.mesh_output_buffer,
                                base + layout.map_base + 4 + primitive * 3 + corner,
                            ),
                            mapped_vertex,
                        );
                        let (element, lines, region) = if per_primitive {
                            (primitive, 13, layout.primitive_base)
                        } else {
                            (mapped_vertex, 12, 0)
                        };
                        let expected_index = base
                            + 4
                            + region
                            + (element / 32) * lines * 32
                            + element % 32
                            + line * 32;
                        memory.insert((ctx.mesh_output_buffer, expected_index), 0xffa1_2345);
                        assert_eq!(
                            scalar_with_memory(&ctx, &flat, value, 0, &memory),
                            0xffa1_2345
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn mesh_prologue_initializes_count_before_collective_barrier_without_branch() {
        let mut ctx = context(true);
        emit_mesh_prologue(&mut ctx);
        let ops = instructions(&ctx);
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].class.opcode, spirv::Op::Store);
        assert_eq!(ops[0].operands[0], Operand::IdRef(ctx.mesh_primitive_count));
        assert_eq!(constant(&ctx, &ops[0].operands[1]), 0);
        assert_eq!(ops[1].class.opcode, spirv::Op::ControlBarrier);
        assert_eq!(constant(&ctx, &ops[1].operands[0]), 2); // Workgroup execution
        assert_eq!(constant(&ctx, &ops[1].operands[1]), 2); // Workgroup memory
        assert_eq!(constant(&ctx, &ops[1].operands[2]), 0x108); // AcquireRelease | WorkgroupMemory
    }

    #[test]
    fn task_prologue_defaults_unwritten_count_and_preserves_explicit_guest_writes() {
        let mut ctx = payload_context(true);
        emit_task_payload_prologue(&mut ctx);
        let ops = instructions(&ctx);
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0].class.opcode, spirv::Op::AccessChain);
        assert_eq!(ops[0].operands[0], Operand::IdRef(ctx.mesh_payload));
        assert_eq!(constant(&ctx, &ops[0].operands[1]), 1);
        assert_eq!(ops[1].class.opcode, spirv::Op::Store);
        assert_eq!(ops[1].operands[0], Operand::IdRef(ops[0].result_id.unwrap()));
        assert_eq!(constant(&ctx, &ops[1].operands[1]), 0);
        assert_eq!(ops[2].class.opcode, spirv::Op::ControlBarrier);
        assert_eq!(constant(&ctx, &ops[2].operands[2]), 0x108);
        let address = ctx.constant_u32(4);
        let value = ctx.constant_u32(0x8000_0001);
        emit_isbe_write_attribute(&mut ctx, address, value).unwrap();
        let ops = instructions(&ctx);
        let last = ops.last().unwrap();
        assert_eq!(last.class.opcode, spirv::Op::Store);
        assert_eq!(constant(&ctx, &last.operands[1]), 0x8000_0001);
    }

    #[test]
    fn mesh_attribute_and_map_address_units_and_masks_are_preserved() {
        let mut ctx = context(true);
        let address = ctx.constant_u32(u32::MAX);
        let value = ctx.constant_u32(0x123456ab);
        emit_mesh_write_attribute(&mut ctx, address, value).unwrap();
        emit_isbe_write_map_u8(&mut ctx, address, value).unwrap();
        let ops = instructions(&ctx);
        assert_eq!(
            ops.iter().map(|i| i.class.opcode).collect::<Vec<_>>(),
            [
                spirv::Op::ShiftRightLogical,
                spirv::Op::ExtInst,
                spirv::Op::AccessChain,
                spirv::Op::Store,
                spirv::Op::ExtInst,
                spirv::Op::AccessChain,
                spirv::Op::BitwiseAnd,
                spirv::Op::Store,
            ]
        );
        assert_eq!(ops[0].operands[0], Operand::IdRef(address));
        assert_eq!(constant(&ctx, &ops[0].operands[1]), 2);
        assert_eq!(ops[1].operands[1], Operand::LiteralExtInstInteger(38));
        assert_eq!(
            ops[1].operands[2],
            Operand::IdRef(ops[0].result_id.unwrap())
        );
        assert_eq!(constant(&ctx, &ops[1].operands[3]), 127);
        assert_eq!(
            ops[2].operands,
            [
                Operand::IdRef(ctx.mesh_attributes),
                Operand::IdRef(ops[1].result_id.unwrap())
            ]
        );
        assert_eq!(
            ops[3].operands,
            [
                Operand::IdRef(ops[2].result_id.unwrap()),
                Operand::IdRef(value)
            ]
        );
        assert_eq!(ops[4].operands[1], Operand::LiteralExtInstInteger(38));
        assert_eq!(ops[4].operands[2], Operand::IdRef(address)); // No divide by 4 for map bytes.
        assert_eq!(constant(&ctx, &ops[4].operands[3]), 51);
        assert_eq!(
            ops[5].operands,
            [
                Operand::IdRef(ctx.mesh_map),
                Operand::IdRef(ops[4].result_id.unwrap())
            ]
        );
        assert_eq!(ops[6].operands[0], Operand::IdRef(value));
        assert_eq!(constant(&ctx, &ops[6].operands[1]), 255);
        assert_eq!(
            ops[7].operands,
            [
                Operand::IdRef(ops[5].result_id.unwrap()),
                Operand::IdRef(ops[6].result_id.unwrap())
            ]
        );
    }

    #[test]
    fn mesh_count_requires_immediate_three_and_rejects_before_emission() {
        let mut ctx = context(true);
        let value = ctx.constant_u32(u32::MAX);
        for address in [
            Value::ImmU32(0),
            Value::ImmU32(4),
            Value::Inst(crate::ir::value::InstRef { block: 0, inst: 0 }),
        ] {
            assert!(emit_isbe_write_map_u32(&mut ctx, address, value).is_err());
            assert!(instructions(&ctx).is_empty());
        }
        emit_isbe_write_map_u32(&mut ctx, Value::ImmU32(3), value).unwrap();
        assert_eq!(instructions(&ctx).len(), 1);
        assert_eq!(
            instructions(&ctx)[0].operands,
            [
                Operand::IdRef(ctx.mesh_primitive_count),
                Operand::IdRef(value)
            ]
        );
        // Upstream clamps count in the epilogue, never at this write.
    }

    #[test]
    fn ordinary_or_task_context_cannot_write_mesh_output_regions() {
        let mut ctx = context(false);
        let zero = ctx.const_zero_u32;
        assert!(emit_mesh_write_attribute(&mut ctx, zero, zero).is_err());
        assert!(emit_isbe_write_map_u8(&mut ctx, zero, zero).is_err());
        assert!(emit_isbe_write_map_u32(&mut ctx, Value::ImmU32(3), zero).is_err());
        emit_mesh_prologue(&mut ctx);
        assert!(instructions(&ctx).is_empty());
    }
}

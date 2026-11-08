// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `frontend/maxwell/translate_program.cpp`
//!
//! Top-level shader translation: takes a CFG and environment, translates
//! each block using the TranslatorVisitor, builds the structured control
//! flow AST, and returns an IR::Program.

use crate::ir::basic_block::Block;
use crate::ir::emitter::Emitter;
use crate::ir::opcodes::Opcode;
use crate::ir::program::{Program, SyntaxNode};
use crate::ir::types::{OutputTopology, ShaderStage};
use crate::ir::value::{Attribute, InstRef, Value};
use crate::ir_opt;
use crate::program_header::{PixelImap, ProgramHeader};
use crate::runtime_info::{AttributeType, RuntimeInfo};
use crate::shader_info::{Interpolation, StorageBufferDescriptor};
use crate::varying_state::VaryingState;
use crate::{environment::Environment, host_translate_info::HostTranslateInfo};
use std::collections::{BTreeMap, VecDeque};

/// Translate a Maxwell shader program from instruction words to IR.
///
/// Upstream takes prebuilt CFG and object-pool owners plus `Environment&` /
/// `HostTranslateInfo&`. Ruzu's public compatibility entry point still takes
/// only instruction words and a stage, so it delegates to the currently ported
/// CFG/structured-CF/translation/pass driver in `pipeline_cache.rs`.
pub fn translate_program(instructions: &[u64], stage: crate::ir::types::ShaderStage) -> Program {
    crate::pipeline_cache::translate_program(instructions, stage)
}

/// `AddMeshLines` from Eglantine translate_program.cpp at
/// 900ab5a8251dcdb5eee42dd2837fa936ade15303. Stage activation awaits the
/// task/mesh execution prerequisites recorded in NX2_PORT_STATE.md.
pub fn add_mesh_lines(
    sph: &ProgramHeader,
    lines: &mut arrayvec::ArrayVec<u32, 136>,
) -> Result<(), crate::exception::NotImplementedException> {
    // OmapSystemC bit 11 is reserved; all other bits name unsupported outputs.
    if sph.vtg_omap_systemc() & 0xf7ff != 0 {
        return Err(crate::exception::NotImplementedException::new(
            "Mesh outputs past the generic attributes",
        ));
    }
    for bit in 0..8 {
        if (sph.vtg_omap_systemb() >> bit) & 1 != 0 {
            lines.push(Attribute::PRIMITIVE_ID.0 + bit);
        }
    }
    for generic in 0..32 {
        for (element, enabled) in sph.vtg_output_generic(generic).into_iter().enumerate() {
            if enabled {
                lines.push(Attribute::generic(generic as u32, element as u32).0);
            }
        }
    }
    Ok(())
}

/// Mechanical extraction of the Task/Mesh initialization arm in upstream
/// `TranslateProgram` (900ab5a8). Kept here with its original owner while the
/// execution backend is ported. `is_task` is the upstream stage comparison;
/// this helper does not activate a stage or substitute its execution semantics.
pub fn initialize_mesh_program(
    program: &mut Program,
    registers: [u32; 3],
    is_task: bool,
    sph: &ProgramHeader,
    primitive_header: &ProgramHeader,
) -> Result<(), crate::exception::NotImplementedException> {
    let [shader_a, shader_b, init] = registers;
    let lines = ((init >> 12) & 0x1ff).max((init >> 22) & 0x1ff);
    program.mesh.payload_words = lines.max(1) * 32;
    program.mesh.topology = shader_a & 0xf;
    program.mesh.max_vertices = (shader_a >> 4) & 0x1ff;
    program.mesh.max_primitives = (shader_a >> 13) & 0x3ff;
    let threads = if is_task { init & 0x3f } else { (shader_b >> 20) & 0x3f };
    program.workgroup_size = [threads.max(1), 1, 1];
    if !is_task {
        if !matches!(program.mesh.topology, 0 | 1 | 4) {
            return Err(crate::exception::NotImplementedException::new(format!(
                "Mesh output topology {} (SET_MESH_SHADER_A {shader_a:#010x})",
                program.mesh.topology,
            )));
        }
        add_mesh_lines(sph, &mut program.mesh.vertex_lines)?;
        add_mesh_lines(primitive_header, &mut program.mesh.primitive_lines)?;
    }
    Ok(())
}

/// TranslateProgram records the stage-buffer outputs after usage collection;
/// ordinary SetAttribute scans cannot discover these stores.
pub fn collect_mesh_output_stores(program: &mut Program) {
    if program.info.is_mesh {
        for &line in program
            .mesh
            .vertex_lines
            .iter()
            .chain(&program.mesh.primitive_lines)
        {
            program.info.stores.set(line as usize, true);
        }
    }
}

#[cfg(test)]
mod mesh_line_tests {
    use super::*;

    #[test]
    fn mesh_initialization_matches_captured_mkw_registers() {
        let mut vertex = ProgramHeader::default();
        vertex.raw[13] = 0xf0 << 8;
        let mut primitive = ProgramHeader::default();
        primitive.raw[13] = 1 << 16;
        for (registers, vertices, primitives, threads, payload) in [
            ([0x00020204, 0x00800000, 0x02c0b020], 32, 16, 8, 352),
            ([0x00080804, 0x02000000, 0x00802020], 128, 64, 32, 64),
            ([0x00080804, 0x02000000, 0x02c0b020], 128, 64, 32, 352),
            ([0x00181004, 0x02000000, 0x00802020], 256, 192, 32, 64),
        ] {
            for is_task in [false, true] {
                // Initialization is independently testable before stage execution exists.
                let mut program = Program::new(ShaderStage::Compute);
                initialize_mesh_program(&mut program, registers, is_task, &vertex, &primitive).unwrap();
                assert_eq!(program.workgroup_size, [if is_task { 32 } else { threads }, 1, 1]);
                assert_eq!(program.mesh.payload_words, payload);
                assert_eq!(program.mesh.max_vertices, vertices);
                assert_eq!(program.mesh.max_primitives, primitives);
                assert_eq!(program.mesh.topology, 4);
                assert_eq!(program.mesh.vertex_lines.as_slice(), if is_task { &[][..] } else { &[28, 29, 30, 31] });
                assert_eq!(program.mesh.primitive_lines.as_slice(), if is_task { &[][..] } else { &[32] });
            }
        }
    }

    #[test]
    fn mesh_initialization_preserves_zero_minima_and_validation_order() {
        let header = ProgramHeader::default();
        let mut program = Program::new(ShaderStage::Compute);
        initialize_mesh_program(&mut program, [0; 3], false, &header, &header).unwrap();
        assert_eq!(program.workgroup_size, [1, 1, 1]);
        assert_eq!(program.mesh.payload_words, 32);
        assert_eq!((program.mesh.max_vertices, program.mesh.max_primitives), (0, 0));
        assert!(initialize_mesh_program(&mut program, [2, 0, 0], false, &header, &header).is_err());
        assert_eq!(program.mesh.topology, 2);
        // Tasks do not validate mesh topology or output maps.
        let mut invalid = header.clone();
        invalid.raw[18] = 1;
        initialize_mesh_program(&mut program, [2, 0, 0], true, &invalid, &invalid).unwrap();
        // The larger local/passed line count wins, with exactly nine bits per field.
        initialize_mesh_program(&mut program, [4, 63 << 20, (511 << 12) | (1 << 22)], false, &header, &header).unwrap();
        assert_eq!(program.mesh.payload_words, 511 * 32);
        assert_eq!(program.workgroup_size, [63, 1, 1]);
    }

    #[test]
    fn mesh_lines_preserve_attribute_order_and_all_136_outputs() {
        let mut sph = ProgramHeader::default();
        sph.raw[13] = 0xffff_ff00;
        sph.raw[14..17].fill(u32::MAX);
        sph.raw[17] = 0xffff;
        let mut lines = arrayvec::ArrayVec::new();
        add_mesh_lines(&sph, &mut lines).unwrap();
        assert_eq!(lines.as_slice(), &(24..160).collect::<Vec<_>>());

        let mut sparse = ProgramHeader::default();
        sparse.raw[13] = (0x81 << 8) | (0x85 << 16);
        let mut lines = arrayvec::ArrayVec::new();
        add_mesh_lines(&sparse, &mut lines).unwrap();
        assert_eq!(lines.as_slice(), &[24, 31, 32, 34, 39]);
    }

    #[test]
    fn mesh_lines_reject_unsupported_system_outputs_before_appending() {
        for bit in 0..16 {
            let mut sph = ProgramHeader::default();
            sph.raw[18] = 1 << bit;
            sph.raw[13] = 1 << 8;
            let mut lines = arrayvec::ArrayVec::new();
            lines.push(99);
            let result = add_mesh_lines(&sph, &mut lines);
            if bit == 11 {
                assert!(result.is_ok());
                assert_eq!(lines.as_slice(), &[99, 24]);
            } else {
                assert_eq!(result.unwrap_err().to_string(), "Mesh outputs past the generic attributes is not implemented");
                assert_eq!(lines.as_slice(), &[99]);
            }
        }
    }
}

/// Remove CFG blocks that upstream identifies as unreachable after structured
/// control-flow construction.
///
/// Upstream stores block pointers, so erasing an owner does not renumber live
/// references. Rust stores block indices and therefore remaps every live block
/// reference while preserving the same keep predicate and ordering.
pub fn remove_unreachable_blocks(program: &mut Program) {
    if program.blocks.len() == program.post_order_blocks.len() {
        return;
    }
    let keep = program
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| index == 0 || !block.imm_predecessors.is_empty())
        .collect::<Vec<_>>();
    let mut remap = vec![None; keep.len()];
    let mut next = 0u32;
    for (index, keep_block) in keep.iter().copied().enumerate() {
        if keep_block {
            remap[index] = Some(next);
            next += 1;
        }
    }
    if next as usize == program.blocks.len() {
        return;
    }

    program.blocks = std::mem::take(&mut program.blocks)
        .into_iter()
        .enumerate()
        .filter_map(|(index, mut block)| {
            keep[index].then(|| {
                remap_block_indices(&mut block, &remap);
                block
            })
        })
        .collect();
    program.syntax_list = std::mem::take(&mut program.syntax_list)
        .into_iter()
        .map(|node| remap_syntax_node_indices(node, &remap))
        .collect();
    program.post_order_blocks = std::mem::take(&mut program.post_order_blocks)
        .into_iter()
        .filter_map(|block| remap[block as usize])
        .collect();
    regenerate_block_order_from_syntax(program);
}

fn mapped_block(remap: &[Option<u32>], block: u32) -> u32 {
    remap
        .get(block as usize)
        .and_then(|mapped| *mapped)
        .unwrap_or_else(|| panic!("live IR references removed unreachable block {block}"))
}

fn remap_value_indices(value: Value, remap: &[Option<u32>]) -> Value {
    match value {
        Value::Inst(InstRef { block, inst }) => Value::Inst(InstRef {
            block: mapped_block(remap, block),
            inst,
        }),
        other => other,
    }
}

fn remap_block_indices(block: &mut Block, remap: &[Option<u32>]) {
    for inst in block.iter_mut() {
        for arg in &mut inst.args {
            *arg = remap_value_indices(*arg, remap);
        }
        for (predecessor, value) in &mut inst.phi_args {
            *predecessor = mapped_block(remap, *predecessor);
            *value = remap_value_indices(*value, remap);
        }
        if let Some(associated) = &mut inst.associated {
            for reference in [
                &mut associated.zero_inst,
                &mut associated.sign_inst,
                &mut associated.carry_inst,
                &mut associated.overflow_inst,
            ] {
                if let Some(reference) = reference {
                    reference.block = mapped_block(remap, reference.block);
                }
            }
        }
    }
    for value in &mut block.ssa_reg_values {
        *value = remap_value_indices(*value, remap);
    }
    for predecessor in &mut block.imm_predecessors {
        *predecessor = mapped_block(remap, *predecessor);
    }
    for successor in &mut block.imm_successors {
        *successor = mapped_block(remap, *successor);
    }
}

fn remap_syntax_node_indices(node: SyntaxNode, remap: &[Option<u32>]) -> SyntaxNode {
    match node {
        SyntaxNode::Block(block) => SyntaxNode::Block(mapped_block(remap, block)),
        SyntaxNode::If { cond, body, merge } => SyntaxNode::If {
            cond: remap_value_indices(cond, remap),
            body: mapped_block(remap, body),
            merge: mapped_block(remap, merge),
        },
        SyntaxNode::EndIf { merge } => SyntaxNode::EndIf {
            merge: mapped_block(remap, merge),
        },
        SyntaxNode::Loop {
            body,
            continue_block,
            merge,
        } => SyntaxNode::Loop {
            body: mapped_block(remap, body),
            continue_block: mapped_block(remap, continue_block),
            merge: mapped_block(remap, merge),
        },
        SyntaxNode::Repeat {
            cond,
            loop_header,
            merge,
        } => SyntaxNode::Repeat {
            cond: remap_value_indices(cond, remap),
            loop_header: mapped_block(remap, loop_header),
            merge: mapped_block(remap, merge),
        },
        SyntaxNode::Break { cond, merge, skip } => SyntaxNode::Break {
            cond: remap_value_indices(cond, remap),
            merge: mapped_block(remap, merge),
            skip: mapped_block(remap, skip),
        },
        SyntaxNode::Return => SyntaxNode::Return,
        SyntaxNode::Unreachable => SyntaxNode::Unreachable,
    }
}

/// Run the optimization sequence owned by upstream `TranslateProgram`.
pub fn optimize_program_with_env(
    env: &mut dyn Environment,
    program: &mut Program,
    host_info: &HostTranslateInfo,
    sph: Option<&ProgramHeader>,
) {
    optimize_program(program, host_info, Some(env), sph, None);
}

/// Compatibility entry point for callers that do not own an upstream
/// `Environment`. The pass order still has a single owner here; only the
/// environment-dependent folds and state queries are unavailable.
pub(crate) fn optimize_program_without_env(
    program: &mut Program,
    host_info: &HostTranslateInfo,
    sph: Option<&ProgramHeader>,
    texture_bound_buffer: Option<u32>,
) {
    optimize_program(program, host_info, None, sph, texture_bound_buffer);
}

fn optimize_program(
    program: &mut Program,
    host_info: &HostTranslateInfo,
    mut env: Option<&mut dyn Environment>,
    sph: Option<&ProgramHeader>,
    texture_bound_buffer: Option<u32>,
) {
    if !host_info.support_float64 {
        ir_opt::lower_fp64_to_fp32::lower_fp64_to_fp32(program);
    }
    if !host_info.support_float16 {
        ir_opt::lower_fp16_to_fp32::lower_fp16_to_fp32(program);
    }
    if !host_info.support_int64 {
        ir_opt::lower_int64_to_int32::lower_int64_to_int32(program);
    }
    if !host_info.support_conditional_barrier {
        ir_opt::conditional_barrier_pass::conditional_barrier_pass(program);
    }
    ir_opt::ssa_rewrite_pass::ssa_rewrite_pass(program);
    if let Some(env) = env.as_deref_mut() {
        ir_opt::constant_propagation_pass::constant_propagation_pass_with_env(env, program);
        ir_opt::position_pass::position_pass(env, program);
    } else {
        ir_opt::constant_propagation_pass::constant_propagation_pass(program);
    }
    ir_opt::global_memory_to_storage_buffer_pass::global_memory_to_storage_buffer_pass(
        program, host_info,
    );
    if let Some(env) = env.as_deref_mut() {
        ir_opt::texture_pass::texture_pass(env, program, host_info);
    } else if let Some(texture_bound_buffer) = texture_bound_buffer {
        ir_opt::texture_pass::texture_pass_bound_textures(program, texture_bound_buffer);
    }
    {
        let settings = common::settings::values();
        if settings.resolution_info.active || *settings.rescale_hack.get_value() {
            drop(settings);
            ir_opt::rescaling_pass::rescaling_pass(program);
        }
    }
    ir_opt::dead_code_elimination_pass::dead_code_elimination_pass(program);
    let renderer_debug = *common::settings::values().renderer_debug.get_value();
    if renderer_debug {
        ir_opt::verification_pass::verification_pass(program);
    }
    if let Some(sph) = sph {
        ir_opt::collect_shader_info_pass::collect_shader_info_pass_with_sph(program, sph);
    } else {
        ir_opt::collect_shader_info_pass::collect_shader_info_pass(program);
    }
    collect_mesh_output_stores(program);
    ir_opt::layer_pass::layer_pass(program, host_info);
    ir_opt::vendor_workaround_pass::vendor_workaround_pass(program);
}

/// Merge dual vertex programs (VertexA + VertexB) into a single VertexB program.
///
/// Port of upstream `MergeDualVertexPrograms` in
/// `frontend/maxwell/translate_program.cpp`. Upstream syntax nodes store block
/// pointers, so appending VertexB syntax after VertexA is pointer-safe. Rust
/// syntax nodes store block indices; this port explicitly remaps every VertexB
/// block reference and every `Value::Inst` reference by the VertexA block count.
pub fn merge_dual_vertex_programs(
    vertex_a: &mut Program,
    vertex_b: &mut Program,
    env_vertex_b: &mut dyn Environment,
) -> Program {
    let vertex_b_block_offset = vertex_a.blocks.len() as u32;

    ir_opt::dual_vertex_pass::vertex_a_transform_pass(vertex_a);
    ir_opt::dual_vertex_pass::vertex_b_transform_pass(vertex_b);

    let mut result = Program::new(ShaderStage::VertexB);
    result.syntax_list = vertex_a
        .syntax_list
        .iter()
        .filter(|node| !matches!(node, SyntaxNode::Return))
        .cloned()
        .collect();
    result.syntax_list.extend(
        vertex_b
            .syntax_list
            .iter()
            .map(|node| remap_syntax_node_blocks(node, vertex_b_block_offset)),
    );

    result.blocks = vertex_a.blocks.clone();
    result.blocks.extend(
        vertex_b
            .blocks
            .iter()
            .cloned()
            .map(|block| remap_block(block, vertex_b_block_offset)),
    );
    regenerate_block_order_from_syntax(&mut result);

    result.post_order_blocks = vertex_b
        .post_order_blocks
        .iter()
        .map(|&block| block + vertex_b_block_offset)
        .collect();
    result
        .post_order_blocks
        .extend(vertex_a.post_order_blocks.iter().copied());

    result.info = vertex_a.info.clone();
    for (dst, src) in result
        .info
        .loads
        .mask
        .iter_mut()
        .zip(vertex_b.info.loads.mask)
    {
        *dst |= src;
    }
    for (dst, src) in result
        .info
        .stores
        .mask
        .iter_mut()
        .zip(vertex_b.info.stores.mask)
    {
        *dst |= src;
    }
    result.local_memory_size = vertex_a.local_memory_size.max(vertex_b.local_memory_size);

    ir_opt::texture_pass::join_texture_info(&mut result.info, &mut vertex_b.info);
    ir_opt::global_memory_to_storage_buffer_pass::join_storage_info(
        &mut result.info,
        &mut vertex_b.info,
    );
    ir_opt::dead_code_elimination_pass::dead_code_elimination_pass(&mut result);
    if *common::settings::values().renderer_debug.get_value() {
        ir_opt::verification_pass::verification_pass(&result);
    }
    ir_opt::collect_shader_info_pass::collect_shader_info_pass_with_sph(
        &mut result,
        env_vertex_b.sph(),
    );

    result
}

/// Convert legacy (fixed-function) varyings to generic attributes.
pub fn convert_legacy_to_generic(program: &mut Program, runtime_info: &RuntimeInfo) {
    if program.info.stores.legacy() {
        let mut unused_output_generics = VecDeque::new();
        for index in 0..NUM_GENERICS {
            if !program.info.stores.generic_any(index) {
                unused_output_generics.push_back(generic_x(index));
            }
        }

        program.info.legacy_stores_mapping = generate_legacy_to_generic_mappings(
            &program.info.stores,
            unused_output_generics,
            &BTreeMap::new(),
        );

        let block_indices = program.post_order_blocks.clone();
        let mappings = program.info.legacy_stores_mapping.clone();
        let mut mapped_store_attrs = Vec::new();
        for block_index in block_indices {
            for inst in program.block_mut(block_index).iter_mut() {
                if inst.opcode != Opcode::SetAttribute {
                    continue;
                }
                let Some(Value::Attribute(attr)) = inst.args.first().copied() else {
                    continue;
                };
                if !attr.is_legacy() {
                    continue;
                }
                if let Some(&mapped_attr) = mappings.get(&(attr.0 as u64)) {
                    mapped_store_attrs.push(mapped_attr);
                    inst.args[0] = Value::Attribute(Attribute(mapped_attr as u32));
                }
            }
        }
        for mapped_attr in mapped_store_attrs {
            program.info.stores.set(mapped_attr as usize, true);
        }
    }

    if program.info.loads.legacy() {
        let mut unused_input_generics = VecDeque::new();
        for index in 0..NUM_GENERICS {
            let input_type = runtime_info.generic_input_types[index];
            if !runtime_info.previous_stage_stores.generic_any(index)
                || !program.info.loads.generic_any(index)
                || input_type == AttributeType::Disabled
            {
                unused_input_generics.push_back(generic_x(index));
            }
        }

        let mappings = generate_legacy_to_generic_mappings(
            &program.info.loads,
            unused_input_generics,
            &runtime_info.previous_stage_legacy_stores_mapping,
        );

        let block_indices = program.post_order_blocks.clone();
        let mut mapped_load_attrs = Vec::new();
        for block_index in block_indices {
            for inst in program.block_mut(block_index).iter_mut() {
                if inst.opcode != Opcode::GetAttribute {
                    continue;
                }
                let Some(Value::Attribute(attr)) = inst.args.first().copied() else {
                    continue;
                };
                if !attr.is_legacy() {
                    continue;
                }
                if let Some(&mapped_attr) = mappings.get(&(attr.0 as u64)) {
                    mapped_load_attrs.push(mapped_attr);
                    inst.args[0] = Value::Attribute(Attribute(mapped_attr as u32));
                }
            }
        }
        for mapped_attr in mapped_load_attrs {
            program.info.loads.set(mapped_attr as usize, true);
        }
    }
}

/// Port of upstream `CollectInterpolationInfo`.
pub fn collect_interpolation_info(sph: &ProgramHeader, program: &mut Program) {
    if program.stage != ShaderStage::Fragment {
        return;
    }
    for index in 0..NUM_GENERICS {
        let mut imap = None;
        for value in sph.ps_generic_input_map(index as u32) {
            if value == PixelImap::Unused {
                continue;
            }
            if imap.is_some_and(|current| current != value) {
                log::warn!(
                    "Per-component interpolation not implemented for generic input {}",
                    index
                );
                continue;
            }
            imap = Some(value);
        }
        let Some(imap) = imap else {
            continue;
        };
        program.info.interpolation[index] = match imap {
            PixelImap::Unused | PixelImap::Perspective => Interpolation::Smooth,
            PixelImap::Constant => Interpolation::Flat,
            PixelImap::ScreenLinear => Interpolation::NoPerspective,
        };
    }
}

/// Port of upstream `AddNVNStorageBuffers`.
pub fn add_nvn_storage_buffers(program: &mut Program) {
    if !program.info.uses_global_memory {
        return;
    }
    const DRIVER_CBUF: u32 = 0;
    const DESCRIPTOR_SIZE: u32 = 0x10;
    const NUM_BUFFERS: u32 = 16;
    let base = match program.stage {
        ShaderStage::VertexA | ShaderStage::VertexB => 0x110,
        ShaderStage::TessellationControl => 0x210,
        ShaderStage::TessellationEval | ShaderStage::Compute => 0x310,
        ShaderStage::Geometry => 0x410,
        ShaderStage::Fragment => 0x510,
    };

    let descriptors = &mut program.info.storage_buffers_descriptors;
    for index in 0..NUM_BUFFERS {
        if program.info.nvn_buffer_used & (1u16 << index) == 0 {
            continue;
        }
        let offset = base + index * DESCRIPTOR_SIZE;
        if let Some(descriptor) = descriptors
            .iter_mut()
            .find(|descriptor| descriptor.cbuf_offset == offset)
        {
            descriptor.is_written |= program.info.stores_global_memory;
            continue;
        }
        descriptors.push(StorageBufferDescriptor {
            cbuf_index: DRIVER_CBUF,
            cbuf_offset: offset,
            count: 1,
            is_written: program.info.stores_global_memory,
        });
    }
}

/// Port of upstream `GenerateGeometryPassthrough`.
pub fn generate_geometry_passthrough(
    _host_info: &crate::host_translate_info::HostTranslateInfo,
    source_program: &Program,
    output_topology: OutputTopology,
) -> Program {
    let mut program = Program::new(ShaderStage::Geometry);
    program.output_topology = output_topology;
    program.output_vertices = output_vertices_for_topology(output_topology);
    program.is_geometry_passthrough = false;

    program.info.loads.mask = source_program.info.stores.mask;
    program.info.stores.mask = source_program.info.stores.mask;
    program.info.stores.set(Attribute::LAYER.0 as usize, true);
    program
        .info
        .stores
        .set(source_program.info.emulated_layer as usize, false);

    let current_block = program.add_block();
    program.syntax_list.push(SyntaxNode::Block(current_block));
    let passthrough_mask = program.info.stores.clone();
    emit_geometry_passthrough(
        &mut program,
        current_block,
        &passthrough_mask,
        true,
        Some(Attribute(source_program.info.emulated_layer as u32)),
    );

    let return_block = program.add_block();
    Emitter::new(&mut program, return_block).epilogue();
    program.block_mut(current_block).add_successor(return_block);
    program
        .block_mut(return_block)
        .add_predecessor(current_block);

    program.syntax_list.push(SyntaxNode::Block(return_block));
    program.syntax_list.push(SyntaxNode::Return);
    regenerate_block_order_from_syntax(&mut program);
    program.post_order_blocks = crate::ir::post_order::post_order(&program.blocks, current_block);
    ir_opt::ssa_rewrite_pass::ssa_rewrite_pass(&mut program);

    program
}

fn emit_geometry_passthrough(
    program: &mut Program,
    block: u32,
    passthrough_mask: &VaryingState,
    passthrough_position: bool,
    passthrough_layer_attr: Option<Attribute>,
) {
    let output_vertices = program.output_vertices;
    let mut ir = Emitter::new(program, block);
    for i in 0..output_vertices {
        for j in 0..32 {
            if !passthrough_mask.generic_any(j) {
                continue;
            }
            for component in 0..4 {
                let attr = Attribute::generic(j as u32, component);
                let value = ir.get_attribute(attr, Value::ImmU32(i));
                ir.set_attribute(attr, value, Value::ImmU32(0));
            }
        }

        if passthrough_position {
            for component in 0..4 {
                let attr = Attribute::position(component);
                let value = ir.get_attribute(attr, Value::ImmU32(i));
                ir.set_attribute(attr, value, Value::ImmU32(0));
            }
        }

        if let Some(layer_attr) = passthrough_layer_attr {
            let value = ir.get_attribute(layer_attr, Value::ImmU32(0));
            ir.set_attribute(Attribute::LAYER, value, Value::ImmU32(0));
        }

        ir.emit_vertex(Value::ImmU32(0));
    }
    ir.end_primitive(Value::ImmU32(0));
}

fn output_vertices_for_topology(output_topology: OutputTopology) -> u32 {
    match output_topology {
        OutputTopology::PointList => 1,
        OutputTopology::LineStrip => 2,
        OutputTopology::TriangleStrip => 3,
    }
}

fn remap_syntax_node_blocks(node: &SyntaxNode, offset: u32) -> SyntaxNode {
    match node {
        SyntaxNode::Block(block) => SyntaxNode::Block(block + offset),
        SyntaxNode::If { cond, body, merge } => SyntaxNode::If {
            cond: remap_value_blocks(*cond, offset),
            body: body + offset,
            merge: merge + offset,
        },
        SyntaxNode::EndIf { merge } => SyntaxNode::EndIf {
            merge: merge + offset,
        },
        SyntaxNode::Loop {
            body,
            continue_block,
            merge,
        } => SyntaxNode::Loop {
            body: body + offset,
            continue_block: continue_block + offset,
            merge: merge + offset,
        },
        SyntaxNode::Repeat {
            cond,
            loop_header,
            merge,
        } => SyntaxNode::Repeat {
            cond: remap_value_blocks(*cond, offset),
            loop_header: loop_header + offset,
            merge: merge + offset,
        },
        SyntaxNode::Break { cond, merge, skip } => SyntaxNode::Break {
            cond: remap_value_blocks(*cond, offset),
            merge: merge + offset,
            skip: skip + offset,
        },
        SyntaxNode::Return => SyntaxNode::Return,
        SyntaxNode::Unreachable => SyntaxNode::Unreachable,
    }
}

fn remap_block(mut block: Block, offset: u32) -> Block {
    for inst in block.iter_mut() {
        for arg in &mut inst.args {
            *arg = remap_value_blocks(*arg, offset);
        }
    }
    for value in &mut block.ssa_reg_values {
        *value = remap_value_blocks(*value, offset);
    }
    for predecessor in &mut block.imm_predecessors {
        *predecessor += offset;
    }
    for successor in &mut block.imm_successors {
        *successor += offset;
    }
    block
}

fn remap_value_blocks(value: Value, offset: u32) -> Value {
    match value {
        Value::Inst(InstRef { block, inst }) => Value::Inst(InstRef {
            block: block + offset,
            inst,
        }),
        other => other,
    }
}

const NUM_GENERICS: usize = 32;
const NUM_FIXEDFNCTEXTURE: usize = 10;
const COLOR_FRONT_DIFFUSE_R: u32 = Attribute::FRONT_COLOR_DIFFUSE_R.0;
const FOG_COORDINATE: u32 = Attribute::FOG_COORDINATE.0;
const FIXED_FNC_TEXTURE0_S: u32 = Attribute::FIXED_FNC_TEXTURE_0_S.0;

fn generic_x(index: usize) -> u64 {
    (Attribute::generic(index as u32, 0).0) as u64
}

fn generate_legacy_to_generic_mappings(
    state: &VaryingState,
    mut unused_generics: VecDeque<u64>,
    previous_stage_mapping: &BTreeMap<u64, u64>,
) -> BTreeMap<u64, u64> {
    let mut mapping = BTreeMap::new();

    for index in 0..4 {
        let attr = COLOR_FRONT_DIFFUSE_R as usize + index * 4;
        if state.any_component(attr) {
            update_legacy_mapping(
                &mut mapping,
                &mut unused_generics,
                previous_stage_mapping,
                attr as u64,
                4,
            );
        }
    }

    if state.get(FOG_COORDINATE as usize) {
        update_legacy_mapping(
            &mut mapping,
            &mut unused_generics,
            previous_stage_mapping,
            FOG_COORDINATE as u64,
            1,
        );
    }

    for index in 0..NUM_FIXEDFNCTEXTURE {
        let attr = FIXED_FNC_TEXTURE0_S as usize + index * 4;
        if state.any_component(attr) {
            update_legacy_mapping(
                &mut mapping,
                &mut unused_generics,
                previous_stage_mapping,
                attr as u64,
                4,
            );
        }
    }

    mapping
}

fn update_legacy_mapping(
    mapping: &mut BTreeMap<u64, u64>,
    unused_generics: &mut VecDeque<u64>,
    previous_stage_mapping: &BTreeMap<u64, u64>,
    attr: u64,
    count: u64,
) {
    if previous_stage_mapping.contains_key(&attr) {
        for i in 0..count {
            let key = attr + i;
            mapping.insert(key, previous_stage_mapping[&key]);
        }
    } else {
        let generic = unused_generics
            .front()
            .copied()
            .expect("no free generic attribute for legacy varying conversion");
        for i in 0..count {
            mapping.insert(attr + i, generic + i);
        }
        unused_generics.pop_front();
    }
}

#[cfg(test)]
mod convert_legacy_tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::instruction::Inst;

    #[test]
    fn convert_legacy_to_generic_rewrites_store_and_records_mapping() {
        let mut program = Program::new(ShaderStage::VertexB);
        let mut block = Block::new();
        block.append_inst(Inst::new(
            Opcode::SetAttribute,
            vec![
                Value::Attribute(Attribute(COLOR_FRONT_DIFFUSE_R)),
                Value::ImmF32(1.0),
                Value::ImmU32(0),
            ],
        ));
        program.blocks.push(block);
        program.post_order_blocks.push(0);
        program
            .info
            .stores
            .set(COLOR_FRONT_DIFFUSE_R as usize, true);

        convert_legacy_to_generic(&mut program, &RuntimeInfo::default());

        let mapped = program.info.legacy_stores_mapping[&(COLOR_FRONT_DIFFUSE_R as u64)];
        assert_eq!(mapped, Attribute::generic(0, 0).0 as u64);
        assert!(program.info.stores.get(mapped as usize));
        assert_eq!(
            program.blocks[0].inst(0).args[0],
            Value::Attribute(Attribute(mapped as u32))
        );
    }

    #[test]
    fn convert_legacy_to_generic_reuses_previous_stage_mapping_for_load() {
        let mut runtime_info = RuntimeInfo::default();
        runtime_info
            .previous_stage_legacy_stores_mapping
            .insert(FOG_COORDINATE as u64, Attribute::generic(7, 0).0 as u64);

        let mut program = Program::new(ShaderStage::Fragment);
        let mut block = Block::new();
        block.append_inst(Inst::new(
            Opcode::GetAttribute,
            vec![
                Value::Attribute(Attribute(FOG_COORDINATE)),
                Value::ImmU32(0),
            ],
        ));
        program.blocks.push(block);
        program.post_order_blocks.push(0);
        program.info.loads.set(FOG_COORDINATE as usize, true);

        convert_legacy_to_generic(&mut program, &runtime_info);

        assert!(program.info.loads.get(Attribute::generic(7, 0).0 as usize));
        assert_eq!(
            program.blocks[0].inst(0).args[0],
            Value::Attribute(Attribute::generic(7, 0))
        );
    }
}

#[cfg(test)]
mod interpolation_tests {
    use super::*;

    #[test]
    fn collect_interpolation_info_matches_ps_imap() {
        let mut sph = ProgramHeader::default();
        sph.raw[6] = 0b01_01_01_01 | (0b11_11_11_11 << 8) | (0b10_10_10_10 << 16);
        let mut program = Program::new(ShaderStage::Fragment);

        collect_interpolation_info(&sph, &mut program);

        assert_eq!(program.info.interpolation[0], Interpolation::Flat);
        assert_eq!(program.info.interpolation[1], Interpolation::NoPerspective);
        assert_eq!(program.info.interpolation[2], Interpolation::Smooth);
        assert_eq!(program.info.interpolation[3], Interpolation::Smooth);
    }

    #[test]
    fn generate_geometry_passthrough_builds_layer_emulation_program() {
        let mut source = Program::new(ShaderStage::VertexB);
        source
            .info
            .stores
            .set(Attribute::generic(2, 0).0 as usize, true);
        source
            .info
            .stores
            .set(Attribute::generic(2, 1).0 as usize, true);
        source
            .info
            .stores
            .set(Attribute::POSITION_X.0 as usize, true);
        source.info.emulated_layer = Attribute::generic(7, 0).0 as u64;

        let program = generate_geometry_passthrough(
            &crate::host_translate_info::HostTranslateInfo::default(),
            &source,
            OutputTopology::LineStrip,
        );

        assert_eq!(program.stage, ShaderStage::Geometry);
        assert_eq!(program.output_topology, OutputTopology::LineStrip);
        assert_eq!(program.output_vertices, 2);
        assert_eq!(program.syntax_list.len(), 3);
        assert!(matches!(program.syntax_list[0], SyntaxNode::Block(0)));
        assert!(matches!(program.syntax_list[1], SyntaxNode::Block(1)));
        assert!(matches!(program.syntax_list[2], SyntaxNode::Return));
        assert_eq!(program.blocks.len(), 2);
        assert_eq!(program.blocks[0].imm_successors, vec![1]);
        assert_eq!(program.blocks[1].imm_predecessors, vec![0]);
        assert!(program.info.loads.generic_any(2));
        assert!(program.info.stores.get(Attribute::LAYER.0 as usize));
        assert!(!program.info.stores.get(Attribute::generic(7, 0).0 as usize));

        let opcodes: Vec<_> = program.blocks[0]
            .indexed_iter()
            .map(|(_, inst)| inst.opcode)
            .collect();
        assert!(opcodes.contains(&Opcode::GetAttribute));
        assert!(opcodes.contains(&Opcode::SetAttribute));
        assert_eq!(
            opcodes
                .iter()
                .filter(|&&op| op == Opcode::EmitVertex)
                .count(),
            2
        );
        assert_eq!(
            opcodes
                .iter()
                .filter(|&&op| op == Opcode::EndPrimitive)
                .count(),
            1
        );
        assert_eq!(program.blocks[1].front().opcode, Opcode::Epilogue);
    }

    #[test]
    fn generate_geometry_passthrough_copies_position_for_each_vertex() {
        let mut source = Program::new(ShaderStage::VertexB);
        source.info.emulated_layer = Attribute::generic(0, 0).0 as u64;

        let program = generate_geometry_passthrough(
            &crate::host_translate_info::HostTranslateInfo::default(),
            &source,
            OutputTopology::TriangleStrip,
        );

        let position_x_load_vertices: Vec<_> = program.blocks[0]
            .iter()
            .filter(|inst| {
                inst.opcode == Opcode::GetAttribute
                    && inst.args[0] == Value::Attribute(Attribute::POSITION_X)
            })
            .map(|inst| inst.args[1])
            .collect();
        assert_eq!(
            position_x_load_vertices,
            vec![Value::ImmU32(0), Value::ImmU32(1), Value::ImmU32(2)]
        );
    }
}

pub(crate) fn regenerate_block_order_from_syntax(program: &mut Program) {
    let mut order = 0;
    for node in &program.syntax_list {
        if let SyntaxNode::Block(block_index) = *node {
            if let Some(block) = program.blocks.get_mut(block_index as usize) {
                block.order = order;
                order += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::instruction::Inst;
    use crate::ir::opcodes::Opcode;

    #[test]
    fn block_order_follows_abstract_syntax_list_like_generate_blocks() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks = vec![Block::new(), Block::new(), Block::new()];
        program.syntax_list = vec![
            SyntaxNode::Block(2),
            SyntaxNode::Block(0),
            SyntaxNode::Block(1),
        ];

        regenerate_block_order_from_syntax(&mut program);

        assert_eq!(program.blocks[2].order, 0);
        assert_eq!(program.blocks[0].order, 1);
        assert_eq!(program.blocks[1].order, 2);
    }

    struct MergeEnvironment {
        texture_pass_caches: crate::environment::TexturePassCaches,
        sph: ProgramHeader,
    }

    impl Default for MergeEnvironment {
        fn default() -> Self {
            Self {
                texture_pass_caches: Default::default(),
                sph: ProgramHeader::default(),
            }
        }
    }

    impl Environment for MergeEnvironment {
        fn texture_pass_caches(&mut self) -> &mut crate::environment::TexturePassCaches {
            &mut self.texture_pass_caches
        }

        fn read_instruction(&mut self, _address: u32) -> u64 {
            0
        }

        fn read_cbuf_value(&mut self, _cbuf_index: u32, _cbuf_offset: u32) -> u32 {
            0
        }

        fn read_texture_type(&mut self, _raw_handle: u32) -> crate::shader_info::TextureType {
            crate::shader_info::TextureType::Color2D
        }

        fn read_texture_pixel_format(
            &mut self,
            _raw_handle: u32,
        ) -> crate::shader_info::TexturePixelFormat {
            crate::shader_info::TexturePixelFormat::A8B8G8R8Unorm
        }

        fn is_texture_pixel_format_integer(&mut self, _raw_handle: u32) -> bool {
            false
        }

        fn read_viewport_transform_state(&mut self) -> u32 {
            1
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
            [1; 3]
        }

        fn has_hle_macro_state(&self) -> bool {
            false
        }

        fn get_replace_const_buffer(
            &mut self,
            _bank: u32,
            _offset: u32,
        ) -> Option<crate::shader_info::ReplaceConstant> {
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
            ShaderStage::VertexB
        }

        fn start_address(&self) -> u32 {
            0
        }

        fn is_proprietary_driver(&self) -> bool {
            false
        }
    }

    #[test]
    fn translate_program_uses_cfg_driver_instead_of_empty_stub() {
        let program = translate_program(&[0, 0x50B0_0000_0000_0000], ShaderStage::VertexB);

        assert_eq!(program.stage, ShaderStage::VertexB);
        assert!(
            !program.blocks.is_empty(),
            "translate_program must build IR blocks instead of returning Program::new(stage)"
        );
    }

    #[test]
    fn remove_unreachable_blocks_preserves_and_remaps_live_indices() {
        let mut program = Program::new(ShaderStage::VertexB);
        let mut entry = Block::new();
        entry.add_successor(2);
        let unreachable = Block::new();
        let mut exit = Block::new();
        exit.add_predecessor(0);
        exit.append_inst(Inst::new(
            Opcode::Identity,
            vec![Value::Inst(InstRef { block: 0, inst: 0 })],
        ));
        entry.append_inst(Inst::new(Opcode::UndefU32, vec![]));
        program.blocks = vec![entry, unreachable, exit];
        program.post_order_blocks = vec![2, 0];
        program.syntax_list = vec![
            SyntaxNode::Block(0),
            SyntaxNode::Block(2),
            SyntaxNode::Return,
        ];

        remove_unreachable_blocks(&mut program);

        assert_eq!(program.blocks.len(), 2);
        assert_eq!(program.post_order_blocks, vec![1, 0]);
        assert!(matches!(program.syntax_list[1], SyntaxNode::Block(1)));
        assert_eq!(program.block(1).imm_predecessors, vec![0]);
        assert_eq!(
            program.block(1).inst(0).args[0],
            Value::Inst(InstRef { block: 0, inst: 0 })
        );
    }

    #[test]
    fn merge_dual_vertex_programs_remaps_vertex_b_block_references() {
        let mut vertex_a = Program::new(ShaderStage::VertexA);
        let mut va_block = Block::new();
        va_block.append_inst(Inst::new(Opcode::Epilogue, Vec::new()));
        vertex_a.blocks.push(va_block);
        vertex_a.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        vertex_a.post_order_blocks = vec![0];
        vertex_a.local_memory_size = 0x20;
        vertex_a.info.loads.mask[0] = 0x1;

        let mut vertex_b = Program::new(ShaderStage::VertexB);
        let mut vb_block = Block::new();
        vb_block.add_successor(0);
        vb_block.append_inst(Inst::new(Opcode::Prologue, Vec::new()));
        vb_block.append_inst(Inst::new(
            Opcode::Identity,
            vec![Value::Inst(InstRef { block: 0, inst: 0 })],
        ));
        vertex_b.blocks.push(vb_block);
        vertex_b.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        vertex_b.post_order_blocks = vec![0];
        vertex_b.local_memory_size = 0x40;
        vertex_b.info.stores.mask[0] = 0x2;

        let mut env = MergeEnvironment::default();
        let result = merge_dual_vertex_programs(&mut vertex_a, &mut vertex_b, &mut env);

        assert_eq!(result.stage, ShaderStage::VertexB);
        assert_eq!(result.local_memory_size, 0x40);
        assert_eq!(result.info.loads.mask[0], 0x1);
        assert_eq!(result.info.stores.mask[0], 0x2);
        assert!(matches!(result.syntax_list[0], SyntaxNode::Block(0)));
        assert!(matches!(result.syntax_list[1], SyntaxNode::Block(1)));
        assert!(matches!(result.syntax_list[2], SyntaxNode::Return));
        assert_eq!(result.post_order_blocks, vec![1, 0]);
        assert_eq!(result.blocks[1].imm_successors, vec![1]);
    }

    #[test]
    fn add_nvn_storage_buffers_adds_and_merges_driver_descriptors() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.info.uses_global_memory = true;
        program.info.stores_global_memory = true;
        program.info.nvn_buffer_used = (1 << 2) | (1 << 5);
        program.info.storage_buffers_descriptors.push(
            crate::shader_info::StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0x530,
                count: 1,
                is_written: false,
            },
        );

        add_nvn_storage_buffers(&mut program);

        assert_eq!(program.info.storage_buffers_descriptors.len(), 2);
        assert!(program.info.storage_buffers_descriptors[0].is_written);
        assert_eq!(program.info.storage_buffers_descriptors[1].cbuf_index, 0);
        assert_eq!(
            program.info.storage_buffers_descriptors[1].cbuf_offset,
            0x560
        );
        assert_eq!(program.info.storage_buffers_descriptors[1].count, 1);
        assert!(program.info.storage_buffers_descriptors[1].is_written);
    }
}

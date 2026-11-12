// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Compute-backed guest task/mesh programs for the native Metal renderer.
//!
//! Counterpart of `MeshComputeStage`/`MeshComputePrograms` in the Vulkan
//! renderer (`compute_pipeline.rs`, `pipeline_cache.rs`). A guest task or mesh
//! program runs as a compute kernel over the transport described in
//! `shader_recompiler/backend/msl/emit_msl_mesh.rs`; its resources stay the
//! Maxwell graphics slots of `resource_stage`.

use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::MTLComputePipelineState;
use shader_recompiler::shader_info::{Info as ShaderInfo, MeshInfo};

use super::metal_shader::MetalShaderModule;

/// One guest task or mesh program lowered to a compute kernel.
pub struct MetalMeshComputeStage {
    /// Maxwell graphics stage (0 or 2) whose resources the kernel consumes.
    pub resource_stage: usize,
    pub info: ShaderInfo,
    pub shader: Arc<MetalShaderModule>,
    pub state: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    /// Guest local size; the physical dispatch is one-dimensional.
    pub threads: u32,
}

/// Cache-owned compute portion of a guest mesh graphics pipeline. The task
/// stage is absent for direct mesh draws.
pub struct MetalMeshComputePrograms {
    pub task: Option<MetalMeshComputeStage>,
    pub task_info: Option<MeshInfo>,
    pub task_workgroup_bound: Option<u32>,
    pub mesh: MetalMeshComputeStage,
    pub mesh_info: MeshInfo,
}

impl MetalMeshComputePrograms {
    /// Kernel consuming the resources of a Maxwell graphics stage.
    pub fn kernel(&self, resource_stage: usize) -> Option<&MetalMeshComputeStage> {
        self.task
            .iter()
            .chain(std::iter::once(&self.mesh))
            .find(|stage| stage.resource_stage == resource_stage)
    }
}

/// Graphics half of a compute-backed mesh pipeline: the generated reader of
/// the output transport, drawn with the unchanged guest fragment program.
pub struct MetalMeshShaderStages {
    pub vertex: Arc<MetalShaderModule>,
    pub programs: Arc<MetalMeshComputePrograms>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_output_buffer_is_the_generated_vertex_shaders_only_resource() {
        use shader_recompiler::backend::bindings::Bindings;
        use shader_recompiler::backend::msl::{emit_msl::emit_msl_mesh_vertex, MslOptions};
        let mut mesh = MeshInfo { payload_words: 4, max_vertices: 64, max_primitives: 40, topology: 2,
            ..MeshInfo::default() };
        mesh.vertex_lines.extend([28, 29, 30, 31, 32]);
        let (artifact, _) = emit_msl_mesh_vertex(&mesh, &Default::default(), &Default::default(),
            &MslOptions::default(), &mut Bindings::default()).unwrap();
        assert_eq!(artifact.bindings.mesh_output_buffer_index, Some(0));
        assert_eq!(artifact.bindings.buffer_count, 1);
        assert!(artifact.bindings.resources.is_empty());
    }

    /// Runs one mesh kernel with `threadgroup` storage and once more with the
    /// device-buffer slices used above Metal's threadgroup limit; both must
    /// export identical output records.
    #[test]
    fn device_backed_workgroup_storage_matches_threadgroup_storage_on_the_gpu() {
        use objc2_foundation::NSString;
        use objc2_metal::{MTLCommandBuffer as _, MTLCommandEncoder as _, MTLCommandQueue as _,
            MTLComputeCommandEncoder as _, MTLDevice as _, MTLLibrary as _, MTLSize};
        use shader_recompiler::backend::bindings::Bindings;
        use shader_recompiler::backend::msl::{emit_msl::emit_msl_mesh_compute, MslOptions};
        use shader_recompiler::backend::spirv::spirv_emit_context::MeshOutputLayout;
        use shader_recompiler::ir::{basic_block::Block, opcodes::Opcode, value::Value, Program, SyntaxNode};
        use shader_recompiler::stage::Stage;
        use crate::renderer_metal::metal_buffer::MetalBuffer;
        use crate::renderer_metal::metal_device::MetalDevice;
        use crate::renderer_metal::metal_shader::compile_msl_library;

        let device = MetalDevice::new().expect("Metal device");
        let mut program = Program::new(Stage::Compute);
        program.blocks.push(Block::new());
        program.workgroup_size = [32, 1, 1];
        program.mesh = MeshInfo { payload_words: 4, max_vertices: 64, max_primitives: 40, topology: 2,
            ..MeshInfo::default() };
        program.mesh.vertex_lines.extend([28, 29, 30, 31, 32]);
        program.mesh.primitive_lines.push(36);
        program.info.is_mesh = true;
        let block = &mut program.blocks[0];
        block.append_new_inst(Opcode::Prologue, vec![]);
        block.append_new_inst(Opcode::IsbeWriteAttribute, vec![Value::ImmU32(4), Value::ImmU32(7)]);
        block.append_new_inst(Opcode::IsbeWriteMapU8, vec![Value::ImmU32(5), Value::ImmU32(0x1FF)]);
        block.append_new_inst(Opcode::IsbeWriteMapU32, vec![Value::ImmU32(3), Value::ImmU32(2)]);
        block.append_new_inst(Opcode::Epilogue, vec![]);
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        let layout = MeshOutputLayout::new(&program.mesh);
        const GROUPS: usize = 3;
        let stride = layout.stride_words as usize * 4;

        let run = |limit: u32| -> Vec<u32> {
            let options = MslOptions { language_version: device.profile().msl_language_version,
                max_threadgroup_memory_length: limit, ..MslOptions::default() };
            let artifact = emit_msl_mesh_compute(&program, &Default::default(), &Default::default(),
                &options, &mut Bindings::default(), false).unwrap();
            let bindings = &artifact.bindings;
            assert_eq!(bindings.mesh_workgroup_buffer_index.is_some(), limit < 2052);
            let library = compile_msl_library(device.device(), &artifact.source.source,
                artifact.language_version).expect("mesh kernel must compile");
            let function = library.newFunctionWithName(&NSString::from_str(&artifact.entry_point)).unwrap();
            let pipeline = device.device().newComputePipelineStateWithFunction_error(&function).unwrap();
            let mapping = MetalBuffer::new(&device, GROUPS * 12).unwrap();
            // Guest group, payload base, output record: records are reversed
            // so a kernel indexing by physical group would be detected.
            let records: Vec<u32> = (0..GROUPS as u32).flat_map(|g| [g, 0, GROUPS as u32 - 1 - g]).collect();
            mapping.write(0, bytemuck::cast_slice(&records)).unwrap();
            let payload = MetalBuffer::new(&device, 16).unwrap();
            payload.write(0, &[0; 16]).unwrap();
            let output = MetalBuffer::new(&device, GROUPS * stride).unwrap();
            output.write(0, &vec![0xEE; GROUPS * stride]).unwrap();
            let scratch = bindings.mesh_workgroup_buffer_index.map(|index| {
                (index, MetalBuffer::new(&device, GROUPS * bindings.mesh_workgroup_words as usize * 4).unwrap())
            });
            let command = device.command_queue().commandBuffer().unwrap();
            let encoder = command.computeCommandEncoder().unwrap();
            encoder.setComputePipelineState(&pipeline);
            unsafe {
                if let Some((index, scratch)) = &scratch {
                    encoder.setBuffer_offset_atIndex(Some(scratch.handle()), 0, *index as usize);
                }
                encoder.setBuffer_offset_atIndex(Some(mapping.handle()), 0,
                    bindings.mesh_dispatch_buffer_index.unwrap() as usize);
                encoder.setBuffer_offset_atIndex(Some(payload.handle()), 0,
                    bindings.mesh_payload_buffer_index.unwrap() as usize);
                encoder.setBuffer_offset_atIndex(Some(output.handle()), 0,
                    bindings.mesh_output_buffer_index.unwrap() as usize);
            }
            encoder.dispatchThreadgroups_threadsPerThreadgroup(
                MTLSize { width: GROUPS, height: 1, depth: 1 },
                MTLSize { width: 32, height: 1, depth: 1 });
            encoder.endEncoding();
            command.commit();
            command.waitUntilCompleted();
            let mut bytes = vec![0u8; GROUPS * stride];
            output.read(0, &mut bytes).unwrap();
            bytes.chunks_exact(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect()
        };
        let threadgroup = run(u32::MAX);
        let device_backed = run(1024);
        assert_eq!(threadgroup, device_backed);
        for record in 0..GROUPS {
            let words = &threadgroup[record * layout.stride_words as usize..];
            // Two primitives of three vertices, one instance, first vertex of the record.
            assert_eq!(&words[..4], &[6, 1, record as u32 * layout.vertex_span, 0]);
            assert_eq!(words[layout.attribute_base as usize + 1], 7);
            assert_eq!(words[layout.map_base as usize + 5], 0xFF);
        }
    }
}

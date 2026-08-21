// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SPIR-V to Metal Shading Language translation.
//!
//! SPIR-V is used only as the shader compiler's backend-neutral binary IR.
//! Runtime compilation and resource binding are native MSL/Metal operations.

use std::num::NonZeroU32;

use spirv_cross2::compile::msl::{
    BindTarget, CompilerOptions, MetalPlatform, MslVersion, ResourceBinding,
};
use spirv_cross2::targets::Msl;
use spirv_cross2::{Compiler, Module, SpirvCrossError};

/// Explicit mapping from one SPIR-V descriptor to Metal resource indices.
///
/// Metal has independent buffer, texture and sampler namespaces. Keeping all
/// three indices explicit avoids inheriting Vulkan descriptor-set semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetalResourceBinding {
    pub descriptor_set: u32,
    pub binding: u32,
    pub buffer_index: u32,
    pub texture_index: u32,
    pub sampler_index: u32,
    pub count: Option<NonZeroU32>,
}

#[derive(Debug, Clone)]
pub struct MetalShaderSource {
    pub source: String,
    pub execution_model: spirv_cross2::spirv::ExecutionModel,
}

/// Translate shader-recompiler SPIR-V to native MSL.
pub fn compile_spirv_to_msl(
    words: &[u32],
    resource_bindings: &[MetalResourceBinding],
) -> Result<MetalShaderSource, SpirvCrossError> {
    let module = Module::from_words(words);
    let mut compiler = Compiler::<Msl>::new(module)?;
    let execution_model = compiler.execution_model()?;

    for binding in resource_bindings {
        compiler.add_resource_binding(
            execution_model,
            ResourceBinding::from_qualified(binding.descriptor_set, binding.binding),
            &BindTarget {
                buffer: binding.buffer_index,
                texture: binding.texture_index,
                sampler: binding.sampler_index,
                count: binding.count,
            },
        )?;
    }

    let mut options = CompilerOptions::default();
    options.version = MslVersion::new(2, 3, 0);
    options.platform = MetalPlatform::MacOS;
    options.argument_buffers = false;
    // Maxwell SPIR-V already uses the Vulkan/Metal [0, w] depth convention.
    options.common.fixup_clipspace = false;
    let artifact = compiler.compile(&options)?;
    Ok(MetalShaderSource {
        source: artifact.as_ref().to_owned(),
        execution_model,
    })
}

#[cfg(test)]
mod tests {
    use shader_recompiler::backend::emit_spirv;
    use shader_recompiler::ir::basic_block::Block;
    use shader_recompiler::ir::Program;
    use shader_recompiler::profile::Profile;
    use shader_recompiler::runtime_info::RuntimeInfo;
    use shader_recompiler::stage::Stage;

    use super::*;

    #[test]
    fn translates_recompiler_vertex_spirv_to_msl() {
        let mut program = Program::new(Stage::VertexB);
        program.blocks.push(Block::new());
        let words = emit_spirv(&program, &Profile::default(), &RuntimeInfo::default());

        let msl = compile_spirv_to_msl(&words, &[]).expect("SPIR-V must translate to MSL");
        assert_eq!(
            msl.execution_model,
            spirv_cross2::spirv::ExecutionModel::Vertex
        );
        assert!(msl.source.contains("vertex"));
        assert!(msl.source.contains("main0"));
    }
}

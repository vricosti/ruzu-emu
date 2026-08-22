// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SPIR-V to Metal Shading Language translation.
//!
//! SPIR-V is used only as the shader compiler's backend-neutral binary IR.
//! Runtime compilation and resource binding are native MSL/Metal operations.

use std::num::NonZeroU32;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLCompileOptions, MTLDevice, MTLFunction, MTLLanguageVersion, MTLLibrary, MTLMathMode,
};
use spirv_cross2::compile::msl::{
    BindTarget, CompilerOptions, MetalPlatform, MslVersion as SpirvCrossMslVersion, ResourceBinding,
};
use spirv_cross2::reflect::{ArrayDimension, Resource, TypeInner};
use spirv_cross2::targets::Msl;
use spirv_cross2::{Compiler, Module, SpirvCrossError};
use thiserror::Error;

use shader_recompiler::backend::msl::MslError;
use shader_recompiler::backend::msl::MslVersion;
pub use shader_recompiler::backend::msl::{
    MslBindingLayout as MetalShaderBindingLayout, MslExecutionInfo as MetalExecutionInfo,
    MslResourceBinding as MetalResourceBinding, MslResourceKind as MetalResourceKind,
    MslShaderArtifact as MetalShaderArtifact, MslShaderSource as MetalShaderSource,
};
use shader_recompiler::ir::Program;
use shader_recompiler::profile::Profile;
use shader_recompiler::runtime_info::RuntimeInfo;
use shader_recompiler::stage::Stage;

use super::metal_device::MetalDeviceProfile;

/// SPIRV-Cross policy for the baseline Apple7 renderer.
///
/// MSL 2.3 is available on the minimum supported Apple Silicon macOS release.
/// Later language features are enabled only after the device profile and the
/// native compiler version are advanced together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetalShaderCompileOptions {
    pub language_version: MslVersion,
    pub argument_buffers: bool,
    pub fixed_subgroup_size: u32,
    pub enable_frag_depth_builtin: bool,
    pub enable_frag_stencil_ref_builtin: bool,
    pub enable_frag_output_mask: u32,
    pub compute_workgroup_size: Option<[u32; 3]>,
}

impl Default for MetalShaderCompileOptions {
    fn default() -> Self {
        Self {
            language_version: MslVersion::V2_3,
            // Direct bindings are the first complete runtime ABI. Enabling
            // argument buffers requires a matching CPU-side argument encoder.
            argument_buffers: false,
            fixed_subgroup_size: 32,
            enable_frag_depth_builtin: true,
            enable_frag_stencil_ref_builtin: true,
            enable_frag_output_mask: u32::MAX,
            compute_workgroup_size: None,
        }
    }
}

impl MetalShaderCompileOptions {
    pub fn for_device(profile: &MetalDeviceProfile) -> Self {
        Self {
            language_version: profile.msl_language_version,
            ..Self::default()
        }
    }

    pub fn for_compute_device(profile: &MetalDeviceProfile, workgroup_size: [u32; 3]) -> Self {
        Self {
            compute_workgroup_size: Some(workgroup_size),
            ..Self::for_device(profile)
        }
    }
}

#[derive(Debug, Error)]
pub enum MetalShaderError {
    #[error(transparent)]
    Translation(#[from] SpirvCrossError),
    #[error("SPIR-V resource {resource} is missing {decoration}")]
    MissingDecoration {
        resource: String,
        decoration: &'static str,
    },
    #[error("SPIR-V resource {resource} has a non-literal or runtime descriptor array")]
    NonLiteralDescriptorArray { resource: String },
    #[error("SPIR-V resource {resource} has an overflowing descriptor array size")]
    DescriptorArrayOverflow { resource: String },
    #[error(
        "Metal direct {namespace} binding limit exceeded: requested {requested}, limit {limit}"
    )]
    ResourceLimit {
        namespace: &'static str,
        requested: u32,
        limit: u32,
    },
    #[error("Metal direct bindings do not yet support SPIR-V resource class {0}")]
    UnsupportedResourceClass(&'static str),
    #[error("multiple SPIR-V resource classes use set {set} binding {binding}")]
    AliasedResourceBinding { set: u32, binding: u32 },
    #[error("MSL requires unsupported auxiliary buffer {0}")]
    UnsupportedAuxiliaryBuffer(&'static str),
    #[error("SPIRV-Cross returned unsupported execution model {0:?}")]
    UnsupportedExecutionModel(spirv_cross2::spirv::ExecutionModel),
    #[error("Metal failed to compile MSL: {0}")]
    LibraryCompile(String),
    #[error("Metal library does not contain entry point {0}")]
    MissingEntryPoint(String),
    #[error("MSL language version {major}.{minor} is unavailable on this macOS version")]
    UnsupportedLanguageVersion { major: u8, minor: u8 },
}

#[derive(Debug, Error)]
pub enum DirectMslValidationError {
    #[error(transparent)]
    Emission(#[from] MslError),
    #[error(transparent)]
    Compilation(#[from] MetalShaderError),
    #[error("direct MSL stage {direct:?} differs from active SPIR-V/MSL stage {active:?}")]
    StageMismatch { direct: Stage, active: Stage },
    #[error("direct MSL resource ABI differs from the active SPIR-V/MSL resource ABI")]
    BindingLayoutMismatch,
    #[error("direct MSL execution metadata differs from the active SPIR-V/MSL metadata")]
    ExecutionInfoMismatch,
}

/// Native shader objects retained for the lifetime of a Metal pipeline.
#[derive(Clone)]
pub struct MetalShaderModule {
    source: MetalShaderSource,
    bindings: MetalShaderBindingLayout,
    language_version: MslVersion,
    execution: MetalExecutionInfo,
    library: Retained<ProtocolObject<dyn MTLLibrary>>,
    function: Retained<ProtocolObject<dyn MTLFunction>>,
}

impl MetalShaderModule {
    pub fn source(&self) -> &MetalShaderSource {
        &self.source
    }

    pub fn bindings(&self) -> &MetalShaderBindingLayout {
        &self.bindings
    }

    pub fn language_version(&self) -> MslVersion {
        self.language_version
    }

    pub fn execution(&self) -> MetalExecutionInfo {
        self.execution
    }

    pub fn library(&self) -> &ProtocolObject<dyn MTLLibrary> {
        &self.library
    }

    pub fn function(&self) -> &ProtocolObject<dyn MTLFunction> {
        &self.function
    }
}

#[derive(Debug)]
struct ReflectedResource {
    descriptor_set: u32,
    binding: u32,
    kind: MetalResourceKind,
    count: Option<NonZeroU32>,
}

fn resource_name(resource: &Resource<'_>) -> String {
    let name = resource.name.as_ref();
    if name.is_empty() {
        "<unnamed>".to_owned()
    } else {
        name.to_owned()
    }
}

fn literal_decoration(
    compiler: &Compiler<Msl>,
    resource: &Resource<'_>,
    decoration: spirv_cross2::spirv::Decoration,
    name: &'static str,
) -> Result<u32, MetalShaderError> {
    compiler
        .decoration(resource.id.clone(), decoration)?
        .and_then(|value| value.as_literal())
        .ok_or_else(|| MetalShaderError::MissingDecoration {
            resource: resource_name(resource),
            decoration: name,
        })
}

fn descriptor_array_count(
    compiler: &Compiler<Msl>,
    resource: &Resource<'_>,
) -> Result<Option<NonZeroU32>, MetalShaderError> {
    let ty = compiler.type_description(resource.type_id.clone())?;
    let TypeInner::Array { dimensions, .. } = ty.inner else {
        return Ok(None);
    };
    let mut count = 1u32;
    for dimension in dimensions {
        let ArrayDimension::Literal(dimension) = dimension else {
            return Err(MetalShaderError::NonLiteralDescriptorArray {
                resource: resource_name(resource),
            });
        };
        let Some(non_zero_dimension) = NonZeroU32::new(dimension) else {
            return Err(MetalShaderError::NonLiteralDescriptorArray {
                resource: resource_name(resource),
            });
        };
        count = count.checked_mul(non_zero_dimension.get()).ok_or_else(|| {
            MetalShaderError::DescriptorArrayOverflow {
                resource: resource_name(resource),
            }
        })?;
    }
    NonZeroU32::new(count)
        .map(Some)
        .ok_or_else(|| MetalShaderError::NonLiteralDescriptorArray {
            resource: resource_name(resource),
        })
}

fn reflect_resource(
    compiler: &Compiler<Msl>,
    resource: Resource<'_>,
    kind: MetalResourceKind,
) -> Result<ReflectedResource, MetalShaderError> {
    Ok(ReflectedResource {
        descriptor_set: literal_decoration(
            compiler,
            &resource,
            spirv_cross2::spirv::Decoration::DescriptorSet,
            "DescriptorSet decoration",
        )?,
        binding: literal_decoration(
            compiler,
            &resource,
            spirv_cross2::spirv::Decoration::Binding,
            "Binding decoration",
        )?,
        kind,
        count: descriptor_array_count(compiler, &resource)?,
    })
}

fn require_empty_resource_class(class: &'static str, count: usize) -> Result<(), MetalShaderError> {
    if count == 0 {
        Ok(())
    } else {
        Err(MetalShaderError::UnsupportedResourceClass(class))
    }
}

fn allocate_slots(
    cursor: &mut u32,
    count: u32,
    limit: u32,
    namespace: &'static str,
) -> Result<u32, MetalShaderError> {
    let base = *cursor;
    let requested = base
        .checked_add(count)
        .ok_or(MetalShaderError::ResourceLimit {
            namespace,
            requested: u32::MAX,
            limit,
        })?;
    if requested > limit {
        return Err(MetalShaderError::ResourceLimit {
            namespace,
            requested,
            limit,
        });
    }
    *cursor = requested;
    Ok(base)
}

/// Reflect and compact one SPIR-V module into Metal's three direct-binding
/// namespaces. Numeric gaps in Vulkan bindings do not consume Metal slots;
/// actual SPIR-V descriptor arrays do.
pub fn reflect_direct_resource_bindings(
    words: &[u32],
    profile: &MetalDeviceProfile,
) -> Result<MetalShaderBindingLayout, MetalShaderError> {
    let module = Module::from_words(words);
    let compiler = Compiler::<Msl>::new(module)?;
    let resources = compiler.shader_resources()?.all_resources()?;

    require_empty_resource_class("subpass input", resources.subpass_inputs.len())?;
    require_empty_resource_class("atomic counter", resources.atomic_counters.len())?;
    require_empty_resource_class(
        "acceleration structure",
        resources.acceleration_structures.len(),
    )?;
    require_empty_resource_class("plain uniform", resources.gl_plain_uniforms.len())?;
    require_empty_resource_class(
        "shader record buffer",
        resources.shader_record_buffers.len(),
    )?;

    let has_push_constants = !resources.push_constant_buffers.is_empty();
    let mut reflected = Vec::new();
    for resource in resources.uniform_buffers {
        reflected.push(reflect_resource(
            &compiler,
            resource,
            MetalResourceKind::UniformBuffer,
        )?);
    }
    for resource in resources.storage_buffers {
        reflected.push(reflect_resource(
            &compiler,
            resource,
            MetalResourceKind::StorageBuffer,
        )?);
    }
    for resource in resources.storage_images {
        reflected.push(reflect_resource(
            &compiler,
            resource,
            MetalResourceKind::StorageImage,
        )?);
    }
    for resource in resources.sampled_images {
        reflected.push(reflect_resource(
            &compiler,
            resource,
            MetalResourceKind::SampledImage,
        )?);
    }
    for resource in resources.separate_images {
        reflected.push(reflect_resource(
            &compiler,
            resource,
            MetalResourceKind::SeparateImage,
        )?);
    }
    for resource in resources.separate_samplers {
        reflected.push(reflect_resource(
            &compiler,
            resource,
            MetalResourceKind::SeparateSampler,
        )?);
    }
    reflected.sort_by_key(|resource| (resource.descriptor_set, resource.binding, resource.kind));
    for pair in reflected.windows(2) {
        if pair[0].descriptor_set == pair[1].descriptor_set && pair[0].binding == pair[1].binding {
            return Err(MetalShaderError::AliasedResourceBinding {
                set: pair[0].descriptor_set,
                binding: pair[0].binding,
            });
        }
    }

    let mut layout = MetalShaderBindingLayout::default();
    if has_push_constants {
        layout.push_constant_buffer_index = Some(allocate_slots(
            &mut layout.buffer_count,
            1,
            profile.max_buffer_bindings_per_stage,
            "buffer",
        )?);
    }
    for resource in reflected {
        let count = resource.count.map_or(1, NonZeroU32::get);
        let mut binding = MetalResourceBinding {
            descriptor_set: resource.descriptor_set,
            binding: resource.binding,
            kind: resource.kind,
            buffer_index: 0,
            texture_index: 0,
            sampler_index: 0,
            count: resource.count,
        };
        match resource.kind {
            MetalResourceKind::UniformBuffer | MetalResourceKind::StorageBuffer => {
                binding.buffer_index = allocate_slots(
                    &mut layout.buffer_count,
                    count,
                    profile.max_buffer_bindings_per_stage,
                    "buffer",
                )?;
            }
            MetalResourceKind::StorageImage | MetalResourceKind::SeparateImage => {
                binding.texture_index = allocate_slots(
                    &mut layout.texture_count,
                    count,
                    profile.max_texture_bindings_per_stage,
                    "texture",
                )?;
            }
            MetalResourceKind::SampledImage => {
                binding.texture_index = allocate_slots(
                    &mut layout.texture_count,
                    count,
                    profile.max_texture_bindings_per_stage,
                    "texture",
                )?;
                binding.sampler_index = allocate_slots(
                    &mut layout.sampler_count,
                    count,
                    profile.max_sampler_bindings_per_stage,
                    "sampler",
                )?;
            }
            MetalResourceKind::SeparateSampler => {
                binding.sampler_index = allocate_slots(
                    &mut layout.sampler_count,
                    count,
                    profile.max_sampler_bindings_per_stage,
                    "sampler",
                )?;
            }
        }
        layout.resources.push(binding);
    }
    Ok(layout)
}

/// Translate shader-recompiler SPIR-V to native MSL.
pub fn compile_spirv_to_msl(
    words: &[u32],
    resource_bindings: &[MetalResourceBinding],
) -> Result<MetalShaderSource, MetalShaderError> {
    compile_spirv_to_msl_with_options(
        words,
        resource_bindings,
        &MetalShaderCompileOptions::default(),
    )
}

pub fn compile_spirv_to_msl_with_options(
    words: &[u32],
    resource_bindings: &[MetalResourceBinding],
    metal_options: &MetalShaderCompileOptions,
) -> Result<MetalShaderSource, MetalShaderError> {
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

    let options = make_compiler_options(metal_options);
    let artifact = compiler.compile(&options)?;
    Ok(MetalShaderSource {
        source: artifact.as_ref().to_owned(),
        stage: stage_from_execution_model(execution_model)?,
    })
}

fn stage_from_execution_model(
    execution_model: spirv_cross2::spirv::ExecutionModel,
) -> Result<Stage, MetalShaderError> {
    use spirv_cross2::spirv::ExecutionModel;

    match execution_model {
        ExecutionModel::Vertex => Ok(Stage::VertexB),
        ExecutionModel::TessellationControl => Ok(Stage::TessellationControl),
        ExecutionModel::TessellationEvaluation => Ok(Stage::TessellationEval),
        ExecutionModel::Geometry => Ok(Stage::Geometry),
        ExecutionModel::Fragment => Ok(Stage::Fragment),
        ExecutionModel::GLCompute => Ok(Stage::Compute),
        other => Err(MetalShaderError::UnsupportedExecutionModel(other)),
    }
}

fn make_compiler_options(metal_options: &MetalShaderCompileOptions) -> CompilerOptions {
    let mut options = CompilerOptions::default();
    options.version = SpirvCrossMslVersion::new(
        metal_options.language_version.major as u32,
        metal_options.language_version.minor as u32,
        0,
    );
    options.platform = MetalPlatform::MacOS;
    options.argument_buffers = metal_options.argument_buffers;
    options.texture_buffer_native = true;
    options.fixed_subgroup_size = metal_options.fixed_subgroup_size;
    options.enable_frag_depth_builtin = metal_options.enable_frag_depth_builtin;
    options.enable_frag_stencil_ref_builtin = metal_options.enable_frag_stencil_ref_builtin;
    options.enable_frag_output_mask = metal_options.enable_frag_output_mask;
    options.pad_fragment_output_components = true;
    options.manual_helper_invocation_updates = true;
    options.readwrite_texture_fences = true;
    options.agx_manual_cube_grad_fixup = true;
    options.force_fragment_with_side_effects_execution = true;
    // Maxwell SPIR-V already uses the Vulkan/Metal [0, w] depth convention.
    options.common.fixup_clipspace = false;
    options
}

fn compile_spirv_to_msl_with_layout(
    words: &[u32],
    bindings: &MetalShaderBindingLayout,
    metal_options: &MetalShaderCompileOptions,
) -> Result<MetalShaderSource, MetalShaderError> {
    let module = Module::from_words(words);
    let mut compiler = Compiler::<Msl>::new(module)?;
    let execution_model = compiler.execution_model()?;
    for binding in &bindings.resources {
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
    if let Some(buffer_index) = bindings.push_constant_buffer_index {
        compiler.add_resource_binding(
            execution_model,
            ResourceBinding::PushConstantBuffer,
            &BindTarget {
                buffer: buffer_index,
                texture: 0,
                sampler: 0,
                count: None,
            },
        )?;
    }
    let options = make_compiler_options(metal_options);
    let artifact = compiler.compile(&options)?;
    let requirements = artifact.buffer_requirements();
    if requirements.needs_swizzle_buffer {
        return Err(MetalShaderError::UnsupportedAuxiliaryBuffer(
            "texture swizzle buffer",
        ));
    }
    if requirements.needs_buffer_size_buffer {
        return Err(MetalShaderError::UnsupportedAuxiliaryBuffer(
            "storage-buffer size buffer",
        ));
    }
    if requirements.needs_output_buffer {
        return Err(MetalShaderError::UnsupportedAuxiliaryBuffer(
            "shader output buffer",
        ));
    }
    if requirements.needs_patch_output_buffer {
        return Err(MetalShaderError::UnsupportedAuxiliaryBuffer(
            "patch output buffer",
        ));
    }
    if requirements.needs_input_threadgroup_buffer {
        return Err(MetalShaderError::UnsupportedAuxiliaryBuffer(
            "input threadgroup buffer",
        ));
    }
    Ok(MetalShaderSource {
        source: artifact.as_ref().to_owned(),
        stage: stage_from_execution_model(execution_model)?,
    })
}

/// Translate a shader-recompiler module and compile it with Apple's native
/// Metal compiler. `main0` is SPIRV-Cross's stable entry-point name.
pub fn compile_native_shader(
    device: &ProtocolObject<dyn MTLDevice>,
    profile: &MetalDeviceProfile,
    words: &[u32],
    options: &MetalShaderCompileOptions,
) -> Result<MetalShaderModule, MetalShaderError> {
    let bindings = reflect_direct_resource_bindings(words, profile)?;
    let source = compile_spirv_to_msl_with_layout(words, &bindings, options)?;
    compile_native_msl_artifact(
        device,
        MetalShaderArtifact {
            source,
            bindings,
            entry_point: "main0".to_owned(),
            language_version: options.language_version,
            execution: MetalExecutionInfo {
                workgroup_size: options.compute_workgroup_size,
            },
        },
    )
}

/// Compile an already-lowered MSL artifact into the native objects retained
/// by the pipeline cache. This is the stable boundary for a future direct
/// Maxwell-IR-to-MSL emitter.
pub fn compile_native_msl_artifact(
    device: &ProtocolObject<dyn MTLDevice>,
    artifact: MetalShaderArtifact,
) -> Result<MetalShaderModule, MetalShaderError> {
    let language_version = artifact.language_version;
    let execution = artifact.execution;
    let compile_options = MTLCompileOptions::new();
    compile_options.setLanguageVersion(metal_language_version(artifact.language_version)?);
    if objc2::available!(macos = 15.0, ..) {
        compile_options.setMathMode(MTLMathMode::Safe);
    } else {
        #[allow(deprecated)]
        compile_options.setFastMathEnabled(false);
    }
    let source_string = NSString::from_str(&artifact.source.source);
    let library = device
        .newLibraryWithSource_options_error(&source_string, Some(&compile_options))
        .map_err(|error| {
            MetalShaderError::LibraryCompile(error.localizedDescription().to_string())
        })?;
    let entry_point = NSString::from_str(&artifact.entry_point);
    let function = library
        .newFunctionWithName(&entry_point)
        .ok_or_else(|| MetalShaderError::MissingEntryPoint(artifact.entry_point.clone()))?;
    Ok(MetalShaderModule {
        source: artifact.source,
        bindings: artifact.bindings,
        language_version,
        execution,
        library,
        function,
    })
}

fn metal_language_version(version: MslVersion) -> Result<MTLLanguageVersion, MetalShaderError> {
    let available = match version {
        MslVersion::V2_3 => Some(MTLLanguageVersion::Version2_3),
        MslVersion::V2_4 if objc2::available!(macos = 12.0, ..) => {
            Some(MTLLanguageVersion::Version2_4)
        }
        MslVersion::V3_0 if objc2::available!(macos = 13.0, ..) => {
            Some(MTLLanguageVersion::Version3_0)
        }
        MslVersion::V3_1 if objc2::available!(macos = 14.0, ..) => {
            Some(MTLLanguageVersion::Version3_1)
        }
        MslVersion::V3_2 if objc2::available!(macos = 15.0, ..) => {
            Some(MTLLanguageVersion::Version3_2)
        }
        MslVersion::V4_0 if objc2::available!(macos = 26.0, ..) => {
            Some(MTLLanguageVersion::Version4_0)
        }
        _ => None,
    };
    available.ok_or(MetalShaderError::UnsupportedLanguageVersion {
        major: version.major,
        minor: version.minor,
    })
}

/// Compile the direct-MSL output for the same backend-neutral IR as an active
/// SPIR-V/MSL module and verify their externally visible shader contract.
///
/// This function is validation-only: callers retain and use `active`, and an
/// unsupported direct opcode is reported rather than replaced with a shader
/// fallback.
pub fn validate_direct_msl_against_active_module(
    device: &ProtocolObject<dyn MTLDevice>,
    program: &Program,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    active: &MetalShaderModule,
) -> Result<MetalShaderModule, DirectMslValidationError> {
    let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
        program,
        profile,
        runtime_info,
        &shader_recompiler::backend::msl::MslOptions {
            language_version: active.language_version(),
        },
    )?;
    if artifact.source.stage != active.source().stage {
        return Err(DirectMslValidationError::StageMismatch {
            direct: artifact.source.stage,
            active: active.source().stage,
        });
    }
    if artifact.bindings != *active.bindings() {
        return Err(DirectMslValidationError::BindingLayoutMismatch);
    }
    if artifact.execution != active.execution() {
        return Err(DirectMslValidationError::ExecutionInfoMismatch);
    }
    Ok(compile_native_msl_artifact(device, artifact)?)
}

#[cfg(test)]
mod tests {
    use shader_recompiler::backend::emit_spirv;
    use shader_recompiler::ir::basic_block::Block;
    use shader_recompiler::ir::emitter::Emitter;
    use shader_recompiler::ir::opcodes::Opcode;
    use shader_recompiler::ir::types::FpControl;
    use shader_recompiler::ir::value::{InstRef, Value};
    use shader_recompiler::ir::Program;
    use shader_recompiler::profile::Profile;
    use shader_recompiler::runtime_info::RuntimeInfo;
    use shader_recompiler::shader_info::{
        ConstantBufferDescriptor, TextureDescriptor, TextureType,
    };
    use shader_recompiler::stage::Stage;

    use super::*;
    use crate::renderer_metal::metal_device::MetalDevice;
    use crate::renderer_metal::metal_pipeline_cache::make_shader_profile;

    fn resource_program(texture_count: u32) -> Program {
        let mut program = Program::new(Stage::Fragment);
        program.blocks.push(Block::new());
        program
            .info
            .constant_buffer_descriptors
            .push(ConstantBufferDescriptor { index: 0, count: 2 });
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type: TextureType::Color2D,
            is_depth: false,
            is_multisample: false,
            is_integer: false,
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: texture_count,
            size_shift: 0,
        });
        program.info.uses_rescaling_uniform = true;
        program
    }

    fn empty_program(stage: Stage) -> Program {
        let mut program = Program::new(stage);
        program.blocks.push(Block::new());
        program
    }

    #[test]
    fn translates_recompiler_vertex_spirv_to_msl() {
        let mut program = Program::new(Stage::VertexB);
        program.blocks.push(Block::new());
        let words = emit_spirv(&program, &Profile::default(), &RuntimeInfo::default());

        let msl = compile_spirv_to_msl(&words, &[]).expect("SPIR-V must translate to MSL");
        assert_eq!(msl.stage, Stage::VertexB);
        assert!(msl.source.contains("vertex"));
        assert!(msl.source.contains("main0"));
    }

    #[test]
    fn compiles_recompiler_vertex_spirv_to_native_metal_function() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut program = Program::new(Stage::VertexB);
        program.blocks.push(Block::new());
        let words = emit_spirv(&program, &Profile::default(), &RuntimeInfo::default());

        let shader = compile_native_shader(
            device.device(),
            device.profile(),
            &words,
            &MetalShaderCompileOptions::default(),
        )
        .expect("recompiler SPIR-V must compile as a native Metal function");

        assert_eq!(shader.source().stage, Stage::VertexB);
        assert!(!shader.library().functionNames().is_empty());
        assert_eq!(shader.function().name().to_string(), "main0");
    }

    #[test]
    fn compiles_direct_msl_vertex_artifact_to_native_metal_function() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut program = Program::new(Stage::VertexB);
        program.blocks.push(Block::new());
        let artifact = shader_recompiler::backend::msl::emit_msl(
            &program,
            &Profile::default(),
            &RuntimeInfo::default(),
        )
        .expect("minimal vertex IR must lower directly to MSL");

        let shader = compile_native_msl_artifact(device.device(), artifact)
            .expect("direct MSL must compile as a native Metal function");

        assert_eq!(shader.source().stage, Stage::VertexB);
        assert_eq!(shader.function().name().to_string(), "main0");
    }

    #[test]
    fn compiles_direct_msl_fragment_artifact_to_native_metal_function() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut program = Program::new(Stage::Fragment);
        program.blocks.push(Block::new());
        let artifact = shader_recompiler::backend::msl::emit_msl(
            &program,
            &Profile::default(),
            &RuntimeInfo::default(),
        )
        .expect("minimal fragment IR must lower directly to MSL");

        let shader = compile_native_msl_artifact(device.device(), artifact)
            .expect("direct MSL must compile as a native Metal function");

        assert_eq!(shader.source().stage, Stage::Fragment);
        assert_eq!(shader.function().name().to_string(), "main0");
    }

    #[test]
    fn compiles_direct_msl_compute_artifact_with_workgroup_metadata() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut program = empty_program(Stage::Compute);
        program.workgroup_size = [8, 4, 2];
        let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
            &program,
            &Profile::default(),
            &RuntimeInfo::default(),
            &shader_recompiler::backend::msl::MslOptions {
                language_version: device.profile().msl_language_version,
            },
        )
        .expect("minimal compute IR must lower directly to MSL");

        let shader = compile_native_msl_artifact(device.device(), artifact)
            .expect("direct compute MSL must compile as a native Metal function");

        assert_eq!(shader.source().stage, Stage::Compute);
        assert_eq!(shader.execution().workgroup_size, Some([8, 4, 2]));
        assert_eq!(shader.function().name().to_string(), "main0");
    }

    #[test]
    fn compiles_direct_msl_ssa_and_vertex_output_with_metal() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut program = empty_program(Stage::VertexB);
        program.info.stores.set(28, true);
        let value = program.blocks[0].append_new_inst(
            Opcode::FPAdd32,
            vec![Value::ImmF32(-0.0), Value::ImmF32(1.0)],
        );
        program.blocks[0].inst_mut(value).flags = FpControl {
            no_contraction: true,
            ..Default::default()
        }
        .to_u32();
        program.blocks[0].append_new_inst(
            Opcode::SetAttribute,
            vec![
                Value::Attribute(shader_recompiler::ir::Attribute::POSITION_X),
                Value::Inst(InstRef {
                    block: 0,
                    inst: value,
                }),
                Value::ImmU32(0),
            ],
        );
        let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
            &program,
            &Profile::default(),
            &RuntimeInfo::default(),
            &shader_recompiler::backend::msl::MslOptions {
                language_version: device.profile().msl_language_version,
            },
        )
        .expect("supported vertex IR must lower directly to MSL");

        let shader = compile_native_msl_artifact(device.device(), artifact)
            .expect("direct SSA MSL must compile as a native Metal function");

        assert_eq!(shader.source().stage, Stage::VertexB);
        assert_eq!(
            shader.language_version(),
            device.profile().msl_language_version
        );
        assert!(shader
            .source()
            .source
            .contains("[[clang::optnone]] T spvFAdd"));
    }

    #[test]
    fn compiles_direct_msl_scalar_opcode_families_with_metal() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut program = empty_program(Stage::VertexB);
        {
            let mut emitter = Emitter::new(&mut program, 0);
            let add = emitter.iadd_32(Value::ImmU32(u32::MAX), Value::ImmU32(1));
            emitter.get_zero_from_op(add);
            emitter.get_sign_from_op(add);
            emitter.get_carry_from_op(add);
            emitter.get_overflow_from_op(add);
        }
        let block = &mut program.blocks[0];
        block.append_new_inst(
            Opcode::ShiftRightArithmetic32,
            vec![Value::ImmU32(0x8000_0000), Value::ImmU32(4)],
        );
        block.append_new_inst(
            Opcode::SMin32,
            vec![Value::ImmU32(u32::MAX), Value::ImmU32(1)],
        );
        block.append_new_inst(
            Opcode::SClamp32,
            vec![
                Value::ImmU32(u32::MAX),
                Value::ImmU32(0xFFFF_FFF0),
                Value::ImmU32(1),
            ],
        );
        block.append_new_inst(
            Opcode::BitFieldInsert,
            vec![
                Value::ImmU32(0xFFFF_0000),
                Value::ImmU32(0x1234_5678),
                Value::ImmU32(4),
                Value::ImmU32(8),
            ],
        );
        block.append_new_inst(
            Opcode::BitFieldSExtract,
            vec![
                Value::ImmU32(0x8000_0000),
                Value::ImmU32(8),
                Value::ImmU32(16),
            ],
        );
        block.append_new_inst(Opcode::BitReverse32, vec![Value::ImmU32(1)]);
        block.append_new_inst(Opcode::BitCount32, vec![Value::ImmU32(0xF0F0_0000)]);
        block.append_new_inst(Opcode::FindSMsb32, vec![Value::ImmU32(u32::MAX)]);
        block.append_new_inst(Opcode::FindUMsb32, vec![Value::ImmU32(0)]);
        block.append_new_inst(
            Opcode::LogicalXor,
            vec![Value::ImmU1(true), Value::ImmU1(false)],
        );
        block.append_new_inst(
            Opcode::SelectF32,
            vec![Value::ImmU1(true), Value::ImmF32(-1.0), Value::ImmF32(1.0)],
        );
        block.append_new_inst(Opcode::BitCastF32U32, vec![Value::ImmU32(0x3F80_0000)]);
        block.append_new_inst(Opcode::FPAbs32, vec![Value::ImmF32(-1.0)]);
        let fma = block.append_new_inst(
            Opcode::FPFma32,
            vec![Value::ImmF32(2.0), Value::ImmF32(3.0), Value::ImmF32(4.0)],
        );
        block.inst_mut(fma).flags = FpControl {
            no_contraction: true,
            ..Default::default()
        }
        .to_u32();
        block.append_new_inst(
            Opcode::FPClamp32,
            vec![Value::ImmF32(2.0), Value::ImmF32(0.0), Value::ImmF32(1.0)],
        );
        block.append_new_inst(Opcode::FPRoundEven32, vec![Value::ImmF32(1.5)]);
        block.append_new_inst(Opcode::FPRecipSqrt32, vec![Value::ImmF32(4.0)]);
        block.append_new_inst(
            Opcode::FPOrdNotEqual32,
            vec![Value::ImmF32(f32::NAN), Value::ImmF32(1.0)],
        );
        block.append_new_inst(
            Opcode::FPUnordEqual32,
            vec![Value::ImmF32(f32::NAN), Value::ImmF32(1.0)],
        );
        block.append_new_inst(Opcode::ConvertS32F32, vec![Value::ImmF32(-2.0)]);
        block.append_new_inst(Opcode::ConvertF32S32, vec![Value::ImmU32(0xFFFF_FFFE)]);

        let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
            &program,
            &Profile::default(),
            &RuntimeInfo::default(),
            &shader_recompiler::backend::msl::MslOptions {
                language_version: device.profile().msl_language_version,
            },
        )
        .expect("scalar IR must lower directly to MSL");
        assert!(artifact
            .source
            .source
            .contains("[[clang::optnone]] T spvFma"));
        assert!(artifact.source.source.contains("spvFma("));

        let shader = compile_native_msl_artifact(device.device(), artifact)
            .expect("direct scalar MSL must compile as a native Metal function");

        assert_eq!(shader.source().stage, Stage::VertexB);
        assert_eq!(shader.function().name().to_string(), "main0");
    }

    #[test]
    fn compiles_direct_msl_half_and_int64_with_metal() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let mut program = empty_program(Stage::VertexB);
        program.info.uses_fp16 = true;
        program.info.uses_fp16_denorms_preserve = true;
        let block = &mut program.blocks[0];
        let add16 = block.append_new_inst(
            Opcode::FPAdd16,
            vec![Value::ImmF16(0x3C00), Value::ImmF16(0x4000)],
        );
        block.inst_mut(add16).flags = FpControl {
            no_contraction: true,
            ..Default::default()
        }
        .to_u32();
        block.append_new_inst(Opcode::FPNeg16, vec![Value::ImmF16(0xBC00)]);
        block.append_new_inst(Opcode::FPAbs16, vec![Value::ImmF16(0xBC00)]);
        block.append_new_inst(
            Opcode::FPMul16,
            vec![Value::ImmF16(0x3C00), Value::ImmF16(0x4000)],
        );
        block.append_new_inst(
            Opcode::FPFma16,
            vec![
                Value::ImmF16(0x3C00),
                Value::ImmF16(0x4000),
                Value::ImmF16(0x4200),
            ],
        );
        block.append_new_inst(
            Opcode::FPClamp16,
            vec![
                Value::ImmF16(0x4000),
                Value::ImmF16(0x0000),
                Value::ImmF16(0x3C00),
            ],
        );
        block.append_new_inst(Opcode::FPRoundEven16, vec![Value::ImmF16(0x3E00)]);
        block.append_new_inst(
            Opcode::FPUnordNotEqual16,
            vec![Value::ImmF16(0x7E00), Value::ImmF16(0x3C00)],
        );
        block.append_new_inst(Opcode::ConvertS16F16, vec![Value::ImmF16(0xBC00)]);
        block.append_new_inst(Opcode::ConvertS32F16, vec![Value::ImmF16(0xBC00)]);
        block.append_new_inst(Opcode::ConvertU16F16, vec![Value::ImmF16(0x3C00)]);
        block.append_new_inst(Opcode::ConvertU32F16, vec![Value::ImmF16(0x3C00)]);
        block.append_new_inst(Opcode::ConvertF16F32, vec![Value::ImmF32(1.0)]);
        block.append_new_inst(Opcode::ConvertF32F16, vec![Value::ImmF16(0x3C00)]);
        block.append_new_inst(Opcode::ConvertF16S8, vec![Value::ImmU32(0xFF)]);
        block.append_new_inst(Opcode::ConvertF16S16, vec![Value::ImmU32(0xFFFF)]);
        block.append_new_inst(Opcode::ConvertF16S32, vec![Value::ImmU32(u32::MAX)]);
        block.append_new_inst(Opcode::ConvertF16U8, vec![Value::ImmU32(0xFF)]);
        block.append_new_inst(Opcode::ConvertF16U16, vec![Value::ImmU32(0xFFFF)]);
        block.append_new_inst(Opcode::ConvertF16U32, vec![Value::ImmU32(u32::MAX)]);
        if profile.support_int64 {
            program.info.uses_int64 = true;
            block.append_new_inst(
                Opcode::IAdd64,
                vec![Value::ImmU64(u64::MAX), Value::ImmU64(1)],
            );
            block.append_new_inst(Opcode::ISub64, vec![Value::ImmU64(7), Value::ImmU64(2)]);
            block.append_new_inst(Opcode::INeg64, vec![Value::ImmU64(1)]);
            block.append_new_inst(Opcode::IAbs64, vec![Value::ImmU64(u64::MAX)]);
            block.append_new_inst(
                Opcode::ShiftLeftLogical64,
                vec![Value::ImmU64(1), Value::ImmU32(63)],
            );
            block.append_new_inst(
                Opcode::ShiftRightLogical64,
                vec![Value::ImmU64(u64::MAX), Value::ImmU32(4)],
            );
            block.append_new_inst(
                Opcode::ShiftRightArithmetic64,
                vec![Value::ImmU64(u64::MAX), Value::ImmU32(4)],
            );
            block.append_new_inst(
                Opcode::SelectU64,
                vec![Value::ImmU1(true), Value::ImmU64(1), Value::ImmU64(2)],
            );
            block.append_new_inst(Opcode::ConvertS64F16, vec![Value::ImmF16(0xBC00)]);
            block.append_new_inst(Opcode::ConvertS64F32, vec![Value::ImmF32(-1.0)]);
            block.append_new_inst(Opcode::ConvertU64F16, vec![Value::ImmF16(0x3C00)]);
            block.append_new_inst(Opcode::ConvertU64F32, vec![Value::ImmF32(1.0)]);
            block.append_new_inst(Opcode::ConvertU64U32, vec![Value::ImmU32(7)]);
            block.append_new_inst(Opcode::ConvertU32U64, vec![Value::ImmU64(7)]);
            block.append_new_inst(Opcode::ConvertF16S64, vec![Value::ImmU64(u64::MAX)]);
            block.append_new_inst(Opcode::ConvertF16U64, vec![Value::ImmU64(7)]);
            block.append_new_inst(Opcode::ConvertF32S64, vec![Value::ImmU64(u64::MAX)]);
            block.append_new_inst(Opcode::ConvertF32U64, vec![Value::ImmU64(7)]);
        }

        let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
            &program,
            &profile,
            &RuntimeInfo::default(),
            &shader_recompiler::backend::msl::MslOptions {
                language_version: device.profile().msl_language_version,
            },
        )
        .expect("native half/int64 IR must lower directly to MSL when supported");
        assert!(artifact.source.source.contains("half v_0_0 = spvFAdd("));
        if profile.support_int64 {
            assert!(artifact.source.source.contains("ulong v_0_20 ="));
        }

        let shader = compile_native_msl_artifact(device.device(), artifact)
            .expect("direct half/int64 MSL must compile as a native Metal function");

        assert_eq!(shader.source().stage, Stage::VertexB);
        assert_eq!(shader.function().name().to_string(), "main0");
    }

    #[test]
    fn direct_bindings_compact_independent_metal_namespaces() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let words = emit_spirv(&resource_program(2), &profile, &RuntimeInfo::default());

        let layout = reflect_direct_resource_bindings(&words, device.profile())
            .expect("resource layout must be representable with direct Metal bindings");

        assert_eq!(layout.push_constant_buffer_index, Some(0));
        assert_eq!(layout.buffer_count, 2);
        assert_eq!(layout.texture_count, 2);
        assert_eq!(layout.sampler_count, 2);
        assert_eq!(layout.resources.len(), 2);
        assert_eq!(layout.resources[0].kind, MetalResourceKind::UniformBuffer);
        assert_eq!(layout.resources[0].binding, 0);
        assert_eq!(layout.resources[0].buffer_index, 1);
        assert_eq!(layout.resources[0].count, None);
        assert_eq!(layout.resources[1].kind, MetalResourceKind::SampledImage);
        assert_eq!(layout.resources[1].binding, 2);
        assert_eq!(layout.resources[1].texture_index, 0);
        assert_eq!(layout.resources[1].sampler_index, 0);
        assert_eq!(layout.resources[1].count, NonZeroU32::new(2));
    }

    #[test]
    fn direct_bindings_reject_sampler_arrays_past_device_limit() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let words = emit_spirv(
            &resource_program(device.profile().max_sampler_bindings_per_stage + 1),
            &profile,
            &RuntimeInfo::default(),
        );

        assert!(matches!(
            reflect_direct_resource_bindings(&words, device.profile()),
            Err(MetalShaderError::ResourceLimit {
                namespace: "sampler",
                ..
            })
        ));
    }

    #[test]
    fn validates_direct_vertex_msl_against_spirv_from_the_same_ir() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let program = empty_program(Stage::VertexB);
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::default(),
        )
        .unwrap();

        let direct = validate_direct_msl_against_active_module(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &active,
        )
        .unwrap();

        assert_eq!(direct.source().stage, Stage::VertexB);
        assert_eq!(direct.bindings(), active.bindings());
    }
}

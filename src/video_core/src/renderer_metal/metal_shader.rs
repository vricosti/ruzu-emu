// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Metal Shading Language compilation.
//!
//! The runtime path consumes the shader recompiler's backend-neutral IR and
//! emits MSL directly. The SPIR-V/SPIRV-Cross path remains available only as a
//! validation oracle and for focused compatibility tests. Both paths finish at
//! the same native Metal module and resource ABI.

#[cfg(any(test, feature = "metal-spirv-validation"))]
use std::num::NonZeroU32;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLCompileOptions, MTLDevice, MTLFunction, MTLGPUFamily, MTLLanguageVersion, MTLLibrary,
    MTLMathMode, MTLReadWriteTextureTier,
};
#[cfg(any(test, feature = "metal-spirv-validation"))]
use spirv_cross2::compile::msl::{
    BindTarget, CompilerOptions, MetalPlatform, MslVersion as SpirvCrossMslVersion, ResourceBinding,
};
#[cfg(any(test, feature = "metal-spirv-validation"))]
use spirv_cross2::reflect::{ArrayDimension, Resource, TypeInner};
#[cfg(any(test, feature = "metal-spirv-validation"))]
use spirv_cross2::targets::Msl;
#[cfg(any(test, feature = "metal-spirv-validation"))]
use spirv_cross2::{Compiler, Module, SpirvCrossError};
use thiserror::Error;

use shader_recompiler::backend::bindings::Bindings;
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
#[cfg(any(test, feature = "metal-spirv-validation"))]
use shader_recompiler::stage::Stage;

use super::metal_device::MetalDeviceProfile;

/// Metal shader compilation policy.
///
/// MSL 2.3 is the compatibility floor, not a backend ceiling. The device
/// profile selects the newest language version available on the running
/// macOS release; backend features still require their matching device
/// capability in addition to the language version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetalShaderCompileOptions {
    pub language_version: MslVersion,
    pub argument_buffers: bool,
    pub fixed_subgroup_size: u32,
    pub enable_frag_depth_builtin: bool,
    pub enable_frag_stencil_ref_builtin: bool,
    pub enable_frag_output_mask: u32,
    pub enable_point_size_builtin: bool,
    pub disable_rasterization: bool,
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
            enable_point_size_builtin: true,
            disable_rasterization: false,
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
    #[cfg(any(test, feature = "metal-spirv-validation"))]
    #[error(transparent)]
    Translation(#[from] SpirvCrossError),
    #[cfg(any(test, feature = "metal-spirv-validation"))]
    #[error("SPIR-V resource {resource} is missing {decoration}")]
    MissingDecoration {
        resource: String,
        decoration: &'static str,
    },
    #[cfg(any(test, feature = "metal-spirv-validation"))]
    #[error("SPIR-V resource {resource} has a non-literal or runtime descriptor array")]
    NonLiteralDescriptorArray { resource: String },
    #[cfg(any(test, feature = "metal-spirv-validation"))]
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
    #[cfg(any(test, feature = "metal-spirv-validation"))]
    #[error("Metal direct bindings do not yet support SPIR-V resource class {0}")]
    UnsupportedResourceClass(&'static str),
    #[cfg(any(test, feature = "metal-spirv-validation"))]
    #[error("multiple SPIR-V resource classes use set {set} binding {binding}")]
    AliasedResourceBinding { set: u32, binding: u32 },
    #[cfg(any(test, feature = "metal-spirv-validation"))]
    #[error("MSL requires unsupported auxiliary buffer {0}")]
    UnsupportedAuxiliaryBuffer(&'static str),
    #[cfg(any(test, feature = "metal-spirv-validation"))]
    #[error("SPIRV-Cross returned unsupported execution model {0:?}")]
    UnsupportedExecutionModel(spirv_cross2::spirv::ExecutionModel),
    #[error("Metal failed to compile MSL: {0}")]
    LibraryCompile(String),
    #[error("Metal library does not contain entry point {0}")]
    MissingEntryPoint(String),
    #[error("MSL language version {major}.{minor} is unavailable on this macOS version")]
    UnsupportedLanguageVersion { major: u8, minor: u8 },
}

#[cfg(any(test, feature = "metal-spirv-validation"))]
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

#[derive(Debug, Error)]
pub enum DirectMslCompileError {
    #[error(transparent)]
    Emission(#[from] MslError),
    #[error(transparent)]
    Compilation(#[from] MetalShaderError),
    #[error(
        "direct MSL execution metadata {emitted:?} differs from requested metadata {requested:?}"
    )]
    ExecutionInfoMismatch {
        emitted: MetalExecutionInfo,
        requested: MetalExecutionInfo,
    },
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

#[cfg(any(test, feature = "metal-spirv-validation"))]
#[derive(Debug)]
struct ReflectedResource {
    descriptor_set: u32,
    binding: u32,
    kind: MetalResourceKind,
    count: Option<NonZeroU32>,
}

#[cfg(any(test, feature = "metal-spirv-validation"))]
fn resource_name(resource: &Resource<'_>) -> String {
    let name = resource.name.as_ref();
    if name.is_empty() {
        "<unnamed>".to_owned()
    } else {
        name.to_owned()
    }
}

#[cfg(any(test, feature = "metal-spirv-validation"))]
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

#[cfg(any(test, feature = "metal-spirv-validation"))]
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

#[cfg(any(test, feature = "metal-spirv-validation"))]
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

#[cfg(any(test, feature = "metal-spirv-validation"))]
fn require_empty_resource_class(class: &'static str, count: usize) -> Result<(), MetalShaderError> {
    if count == 0 {
        Ok(())
    } else {
        Err(MetalShaderError::UnsupportedResourceClass(class))
    }
}

#[cfg(any(test, feature = "metal-spirv-validation"))]
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
#[cfg(any(test, feature = "metal-spirv-validation"))]
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
#[cfg(any(test, feature = "metal-spirv-validation"))]
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

#[cfg(any(test, feature = "metal-spirv-validation"))]
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

#[cfg(any(test, feature = "metal-spirv-validation"))]
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

#[cfg(any(test, feature = "metal-spirv-validation"))]
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
    options.enable_point_size_builtin = metal_options.enable_point_size_builtin;
    options.disable_rasterization = metal_options.disable_rasterization;
    options.pad_fragment_output_components = true;
    options.manual_helper_invocation_updates = true;
    options.readwrite_texture_fences = true;
    options.agx_manual_cube_grad_fixup = true;
    options.force_fragment_with_side_effects_execution = true;
    // Maxwell SPIR-V already uses the Vulkan/Metal [0, w] depth convention.
    options.common.fixup_clipspace = false;
    options
}

#[cfg(any(test, feature = "metal-spirv-validation"))]
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
#[cfg(any(test, feature = "metal-spirv-validation"))]
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
                fixed_subgroup_size: options.fixed_subgroup_size,
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

fn direct_msl_options(
    device: &ProtocolObject<dyn MTLDevice>,
    options: &MetalShaderCompileOptions,
) -> shader_recompiler::backend::msl::MslOptions {
    shader_recompiler::backend::msl::MslOptions {
        language_version: options.language_version,
        fixed_subgroup_size: options.fixed_subgroup_size,
        supports_query_texture_lod: device.supportsQueryTextureLOD(),
        supports_read_write_textures: device.readWriteTextureSupport()
            != MTLReadWriteTextureTier::TierNone,
        supports_texture_atomics: options.language_version >= MslVersion::V3_1
            && (device.supportsFamily(MTLGPUFamily::Apple6)
                || device.supportsFamily(MTLGPUFamily::Mac2)),
        enable_point_size_builtin: options.enable_point_size_builtin,
        disable_rasterization: options.disable_rasterization,
    }
}

fn emit_direct_msl_artifact_with_bindings(
    device: &ProtocolObject<dyn MTLDevice>,
    program: &Program,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    options: &MetalShaderCompileOptions,
    bindings: &mut Bindings,
) -> Result<MetalShaderArtifact, MslError> {
    shader_recompiler::backend::msl::emit_msl_with_options_and_bindings(
        program,
        profile,
        runtime_info,
        &direct_msl_options(device, options),
        bindings,
    )
}

/// Emit MSL directly from Maxwell IR and compile it into a native Metal
/// module. An unsupported feature is an error; this path never falls back to
/// SPIR-V or SPIRV-Cross.
pub fn compile_direct_msl_shader_with_bindings(
    device: &ProtocolObject<dyn MTLDevice>,
    program: &Program,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    options: &MetalShaderCompileOptions,
    bindings: &mut Bindings,
) -> Result<MetalShaderModule, DirectMslCompileError> {
    let artifact = emit_direct_msl_artifact_with_bindings(
        device,
        program,
        profile,
        runtime_info,
        options,
        bindings,
    )?;
    let requested_execution = MetalExecutionInfo {
        workgroup_size: options.compute_workgroup_size,
        fixed_subgroup_size: options.fixed_subgroup_size,
    };
    if artifact.execution != requested_execution {
        return Err(DirectMslCompileError::ExecutionInfoMismatch {
            emitted: artifact.execution,
            requested: requested_execution,
        });
    }
    Ok(compile_native_msl_artifact(device, artifact)?)
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
#[cfg(any(test, feature = "metal-spirv-validation"))]
pub fn validate_direct_msl_against_active_module(
    device: &ProtocolObject<dyn MTLDevice>,
    program: &Program,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    active: &MetalShaderModule,
) -> Result<MetalShaderModule, DirectMslValidationError> {
    let mut bindings = Bindings::default();
    validate_direct_msl_against_active_module_with_bindings(
        device,
        program,
        profile,
        runtime_info,
        active,
        &mut bindings,
    )
}

#[cfg(any(test, feature = "metal-spirv-validation"))]
pub fn validate_direct_msl_against_active_module_with_bindings(
    device: &ProtocolObject<dyn MTLDevice>,
    program: &Program,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    active: &MetalShaderModule,
    bindings: &mut Bindings,
) -> Result<MetalShaderModule, DirectMslValidationError> {
    let options = MetalShaderCompileOptions {
        language_version: active.language_version(),
        fixed_subgroup_size: active.execution().fixed_subgroup_size,
        compute_workgroup_size: active.execution().workgroup_size,
        ..MetalShaderCompileOptions::default()
    };
    let artifact = emit_direct_msl_artifact_with_bindings(
        device,
        program,
        profile,
        runtime_info,
        &options,
        bindings,
    )?;
    let direct = compile_native_msl_artifact(device, artifact)?;
    validate_direct_msl_module_against_compatibility(&direct, active)?;
    Ok(direct)
}

/// Compare an already-compiled direct-MSL module with the validation-only
/// SPIRV-Cross module produced from the same backend-neutral IR.
#[cfg(any(test, feature = "metal-spirv-validation"))]
pub fn validate_direct_msl_module_against_compatibility(
    direct: &MetalShaderModule,
    compatibility: &MetalShaderModule,
) -> Result<(), DirectMslValidationError> {
    if direct.source().stage != compatibility.source().stage {
        return Err(DirectMslValidationError::StageMismatch {
            direct: direct.source().stage,
            active: compatibility.source().stage,
        });
    }
    if direct.bindings() != compatibility.bindings() {
        return Err(DirectMslValidationError::BindingLayoutMismatch);
    }
    if direct.execution() != compatibility.execution() {
        return Err(DirectMslValidationError::ExecutionInfoMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use objc2_metal::{MTLDevice as _, MTLPrimitiveTopologyClass, MTLRenderPipelineDescriptor};
    use shader_recompiler::backend::emit_spirv;
    use shader_recompiler::ir::basic_block::Block;
    use shader_recompiler::ir::emitter::Emitter;
    use shader_recompiler::ir::instruction::Inst;
    use shader_recompiler::ir::opcodes::Opcode;
    use shader_recompiler::ir::types::{FpControl, TextureInstInfo, Type};
    use shader_recompiler::ir::value::{InstRef, Value};
    use shader_recompiler::ir::Program;
    use shader_recompiler::profile::Profile;
    use shader_recompiler::runtime_info::{AttributeType, CompareFunction, RuntimeInfo};
    use shader_recompiler::shader_info::{
        ConstantBufferDescriptor, ImageBufferDescriptor, ImageDescriptor, ImageFormat,
        Interpolation, StorageBufferDescriptor, TextureBufferDescriptor, TextureDescriptor,
        TextureType,
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

    fn structured_phi_program() -> Program {
        let mut program = Program::new(Stage::Compute);
        program.blocks = (0..3).map(|_| Block::new()).collect();
        let cond =
            program.blocks[0].append_new_inst(Opcode::ConditionRef, vec![Value::ImmU1(true)]);
        let mut phi = Inst::phi();
        phi.flags = Type::U32 as u32;
        phi.add_phi_operand(0, Value::ImmU32(10));
        phi.add_phi_operand(1, Value::ImmU32(20));
        let phi = program.blocks[2].append_inst(phi);
        program.blocks[2].append_new_inst(
            Opcode::Identity,
            vec![Value::Inst(InstRef {
                block: 2,
                inst: phi,
            })],
        );
        program.syntax_list = vec![
            shader_recompiler::ir::SyntaxNode::Block(0),
            shader_recompiler::ir::SyntaxNode::If {
                cond: Value::Inst(InstRef {
                    block: 0,
                    inst: cond,
                }),
                body: 1,
                merge: 2,
            },
            shader_recompiler::ir::SyntaxNode::Block(1),
            shader_recompiler::ir::SyntaxNode::EndIf { merge: 2 },
            shader_recompiler::ir::SyntaxNode::Block(2),
            shader_recompiler::ir::SyntaxNode::Return,
        ];
        program
    }

    fn structured_loop_program() -> Program {
        let mut program = Program::new(Stage::Compute);
        program.blocks = (0..4).map(|_| Block::new()).collect();
        let break_cond =
            program.blocks[1].append_new_inst(Opcode::ConditionRef, vec![Value::ImmU1(false)]);
        let repeat_cond =
            program.blocks[2].append_new_inst(Opcode::ConditionRef, vec![Value::ImmU1(false)]);
        program.syntax_list = vec![
            shader_recompiler::ir::SyntaxNode::Block(0),
            shader_recompiler::ir::SyntaxNode::Loop {
                body: 1,
                continue_block: 2,
                merge: 3,
            },
            shader_recompiler::ir::SyntaxNode::Block(1),
            shader_recompiler::ir::SyntaxNode::Break {
                cond: Value::Inst(InstRef {
                    block: 1,
                    inst: break_cond,
                }),
                merge: 3,
                skip: 2,
            },
            shader_recompiler::ir::SyntaxNode::Block(2),
            shader_recompiler::ir::SyntaxNode::Repeat {
                cond: Value::Inst(InstRef {
                    block: 2,
                    inst: repeat_cond,
                }),
                loop_header: 0,
                merge: 3,
            },
            shader_recompiler::ir::SyntaxNode::Block(3),
            shader_recompiler::ir::SyntaxNode::Return,
        ];
        program
    }

    fn sample_coordinates(program: &mut Program, texture_type: TextureType) -> Value {
        match texture_type {
            TextureType::Color1D => Value::ImmF32(0.25),
            TextureType::ColorArray1D | TextureType::Color2D | TextureType::Color2DRect => {
                let coords = program.blocks[0].append_new_inst(
                    Opcode::CompositeConstructF32x2,
                    vec![Value::ImmF32(0.25), Value::ImmF32(0.75)],
                );
                Value::Inst(InstRef {
                    block: 0,
                    inst: coords,
                })
            }
            TextureType::ColorArray2D | TextureType::Color3D | TextureType::ColorCube => {
                let coords = program.blocks[0].append_new_inst(
                    Opcode::CompositeConstructF32x3,
                    vec![Value::ImmF32(0.25), Value::ImmF32(0.5), Value::ImmF32(0.75)],
                );
                Value::Inst(InstRef {
                    block: 0,
                    inst: coords,
                })
            }
            TextureType::ColorArrayCube => {
                let coords = program.blocks[0].append_new_inst(
                    Opcode::CompositeConstructF32x4,
                    vec![
                        Value::ImmF32(0.25),
                        Value::ImmF32(0.5),
                        Value::ImmF32(0.75),
                        Value::ImmF32(1.0),
                    ],
                );
                Value::Inst(InstRef {
                    block: 0,
                    inst: coords,
                })
            }
            TextureType::Buffer => unreachable!("sampled texture test does not use buffers"),
        }
    }

    fn store_sample_result(program: &mut Program, sample: u32, vector: bool) {
        program.info.stores_frag_color[0] = true;
        for component in 0..4 {
            let value = if vector {
                let extracted = program.blocks[0].append_new_inst(
                    Opcode::CompositeExtractF32x4,
                    vec![
                        Value::Inst(InstRef {
                            block: 0,
                            inst: sample,
                        }),
                        Value::ImmU32(component),
                    ],
                );
                Value::Inst(InstRef {
                    block: 0,
                    inst: extracted,
                })
            } else {
                Value::Inst(InstRef {
                    block: 0,
                    inst: sample,
                })
            };
            program.blocks[0].append_new_inst(
                Opcode::SetFragColor,
                vec![Value::ImmU32(0), Value::ImmU32(component), value],
            );
        }
    }

    fn sampled_texture_program(texture_count: u32, texture_type: TextureType) -> Program {
        let mut program = Program::new(Stage::Fragment);
        program.blocks.push(Block::new());
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type,
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
        program.info.uses_sampled_1d = matches!(
            texture_type,
            TextureType::Color1D | TextureType::ColorArray1D
        );
        let coords = sample_coordinates(&mut program, texture_type);
        let sample = program.blocks[0].append_new_inst(
            Opcode::ImageSampleExplicitLod,
            vec![
                Value::ImmU32(texture_count.saturating_sub(1)),
                coords,
                Value::ImmF32(1.0),
                Value::Void,
            ],
        );
        program.blocks[0].inst_mut(sample).flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: match texture_type {
                TextureType::Color2DRect => TextureType::Color2D as u8,
                texture_type => texture_type as u8,
            },
            ..Default::default()
        }
        .to_u32();
        store_sample_result(&mut program, sample, true);
        program
    }

    fn depth_sampled_texture_program(texture_type: TextureType) -> Program {
        let mut program = Program::new(Stage::Fragment);
        program.blocks.push(Block::new());
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type,
            is_depth: true,
            is_multisample: false,
            is_integer: false,
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: 1,
            size_shift: 0,
        });
        program.info.uses_shadow_lod = true;
        let coords = sample_coordinates(&mut program, texture_type);
        let sample = program.blocks[0].append_new_inst(
            Opcode::ImageSampleDrefExplicitLod,
            vec![
                Value::ImmU32(0),
                coords,
                Value::ImmF32(0.5),
                Value::ImmF32(1.0),
                Value::Void,
            ],
        );
        program.blocks[0].inst_mut(sample).flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: match texture_type {
                TextureType::Color2DRect => TextureType::Color2D as u8,
                texture_type => texture_type as u8,
            },
            is_depth: true,
            ..Default::default()
        }
        .to_u32();
        store_sample_result(&mut program, sample, false);
        program
    }

    fn sampled_texture_operands_program(is_depth: bool) -> Program {
        let mut program = Program::new(Stage::Fragment);
        program.blocks.push(Block::new());
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type: TextureType::Color2D,
            is_depth,
            is_multisample: false,
            is_integer: false,
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: 1,
            size_shift: 0,
        });
        let coords = sample_coordinates(&mut program, TextureType::Color2D);
        let bias_lod_clamp = program.blocks[0].append_new_inst(
            Opcode::CompositeConstructF32x2,
            vec![Value::ImmF32(0.5), Value::ImmF32(1.25)],
        );
        let offset = program.blocks[0].append_new_inst(
            Opcode::CompositeConstructU32x2,
            vec![Value::ImmU32((-1i32) as u32), Value::ImmU32(2)],
        );
        let value = |inst| Value::Inst(InstRef { block: 0, inst });
        let (opcode, args) = if is_depth {
            (
                Opcode::ImageSampleDrefImplicitLod,
                vec![
                    Value::ImmU32(0),
                    coords,
                    Value::ImmF32(0.4),
                    value(bias_lod_clamp),
                    value(offset),
                ],
            )
        } else {
            (
                Opcode::ImageSampleImplicitLod,
                vec![
                    Value::ImmU32(0),
                    coords,
                    value(bias_lod_clamp),
                    value(offset),
                ],
            )
        };
        let sample = program.blocks[0].append_new_inst(opcode, args);
        program.blocks[0].inst_mut(sample).flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Color2D as u8,
            is_depth,
            has_bias: true,
            has_lod_clamp: true,
            ndv_is_active: true,
            ..Default::default()
        }
        .to_u32();
        store_sample_result(&mut program, sample, !is_depth);
        program
    }

    fn fetch_coordinates(program: &mut Program, texture_type: TextureType) -> Value {
        let (opcode, values) = match texture_type {
            TextureType::Color1D => return Value::ImmU32(4),
            TextureType::ColorArray1D | TextureType::Color2D | TextureType::Color2DRect => (
                Opcode::CompositeConstructU32x2,
                vec![Value::ImmU32(4), Value::ImmU32(2)],
            ),
            TextureType::ColorArray2D | TextureType::Color3D | TextureType::ColorCube => (
                Opcode::CompositeConstructU32x3,
                vec![Value::ImmU32(4), Value::ImmU32(2), Value::ImmU32(1)],
            ),
            TextureType::ColorArrayCube => (
                Opcode::CompositeConstructU32x4,
                vec![
                    Value::ImmU32(4),
                    Value::ImmU32(2),
                    Value::ImmU32(1),
                    Value::ImmU32(0),
                ],
            ),
            TextureType::Buffer => unreachable!("sampled fetch test does not use buffers"),
        };
        let coords = program.blocks[0].append_new_inst(opcode, values);
        Value::Inst(InstRef {
            block: 0,
            inst: coords,
        })
    }

    fn storage_coordinates(program: &mut Program, texture_type: TextureType) -> Value {
        let (opcode, values) = match texture_type {
            TextureType::Color1D => return Value::ImmU32(4),
            TextureType::ColorArray1D | TextureType::Color2D => (
                Opcode::CompositeConstructU32x2,
                vec![Value::ImmU32(4), Value::ImmU32(2)],
            ),
            TextureType::ColorArray2D | TextureType::Color3D => (
                Opcode::CompositeConstructU32x3,
                vec![Value::ImmU32(4), Value::ImmU32(2), Value::ImmU32(1)],
            ),
            _ => unreachable!("invalid storage image test dimension"),
        };
        let coords = program.blocks[0].append_new_inst(opcode, values);
        Value::Inst(InstRef {
            block: 0,
            inst: coords,
        })
    }

    fn storage_image_program(
        texture_type: TextureType,
        count: u32,
        is_integer: bool,
        is_read: bool,
        is_written: bool,
    ) -> Program {
        let mut program = Program::new(Stage::Fragment);
        program.blocks.push(Block::new());
        let format = if is_integer {
            ImageFormat::R32Uint
        } else {
            ImageFormat::Typeless
        };
        program.info.uses_typeless_image_reads = format == ImageFormat::Typeless && is_read;
        program.info.uses_typeless_image_writes = format == ImageFormat::Typeless && is_written;
        program.info.uses_image_1d = matches!(
            texture_type,
            TextureType::Color1D | TextureType::ColorArray1D
        );
        program.info.image_descriptors.push(ImageDescriptor {
            texture_type,
            format,
            is_written,
            is_read,
            is_integer,
            cbuf_index: 0,
            cbuf_offset: 0,
            count,
            size_shift: 0,
        });
        let coords = storage_coordinates(&mut program, texture_type);
        let flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: texture_type as u8,
            image_format: format as u8,
            ..Default::default()
        }
        .to_u32();
        let read = if is_read {
            let read = program.blocks[0].append_new_inst(
                Opcode::ImageRead,
                vec![Value::ImmU32(count.saturating_sub(1)), coords],
            );
            program.blocks[0].inst_mut(read).flags = flags;
            store_query_result(&mut program, read);
            Some(read)
        } else {
            None
        };
        if is_written {
            let color = read.map_or_else(
                || {
                    let color = program.blocks[0].append_new_inst(
                        Opcode::CompositeConstructU32x4,
                        vec![
                            Value::ImmU32(1),
                            Value::ImmU32(2),
                            Value::ImmU32(3),
                            Value::ImmU32(4),
                        ],
                    );
                    Value::Inst(InstRef {
                        block: 0,
                        inst: color,
                    })
                },
                |read| {
                    Value::Inst(InstRef {
                        block: 0,
                        inst: read,
                    })
                },
            );
            let write = program.blocks[0].append_new_inst(
                Opcode::ImageWrite,
                vec![Value::ImmU32(count.saturating_sub(1)), coords, color],
            );
            program.blocks[0].inst_mut(write).flags = flags;
        }
        program
    }

    fn texture_buffer_program() -> Program {
        let mut program = Program::new(Stage::Fragment);
        program.blocks.push(Block::new());
        program
            .info
            .texture_buffer_descriptors
            .push(TextureBufferDescriptor {
                has_secondary: false,
                cbuf_index: 0,
                cbuf_offset: 0,
                shift_left: 0,
                secondary_cbuf_index: 0,
                secondary_cbuf_offset: 0,
                secondary_shift_left: 0,
                count: 1,
                size_shift: 0,
            });
        let flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Buffer as u8,
            ..Default::default()
        }
        .to_u32();
        let fetch = program.blocks[0].append_new_inst(
            Opcode::ImageFetch,
            vec![
                Value::ImmU32(0),
                Value::ImmU32(7),
                Value::ImmU32(2),
                Value::ImmU32(4),
                Value::Void,
            ],
        );
        program.blocks[0].inst_mut(fetch).flags = flags;
        store_sample_result(&mut program, fetch, true);
        let query = program.blocks[0].append_new_inst(
            Opcode::ImageQueryDimensions,
            // SPIRV-Cross currently emits the invalid
            // `texture_buffer::get_num_mip_levels()` call unless the IR asks
            // to skip mip levels. Direct MSL's non-skipping `mips = 1` path
            // is covered independently in shader_recompiler tests.
            vec![Value::ImmU32(0), Value::ImmU32(0), Value::ImmU1(true)],
        );
        program.blocks[0].inst_mut(query).flags = flags;
        store_query_result(&mut program, query);
        program
    }

    fn image_buffer_program(is_read: bool, is_written: bool) -> Program {
        let mut program = Program::new(Stage::Fragment);
        program.blocks.push(Block::new());
        program.info.uses_image_buffers = true;
        program
            .info
            .image_buffer_descriptors
            .push(ImageBufferDescriptor {
                format: ImageFormat::R32Uint,
                is_written,
                is_read,
                is_integer: true,
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                size_shift: 0,
            });
        let flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Buffer as u8,
            image_format: ImageFormat::R32Uint as u8,
            ..Default::default()
        }
        .to_u32();
        if is_read {
            let read = program.blocks[0]
                .append_new_inst(Opcode::ImageRead, vec![Value::ImmU32(0), Value::ImmU32(7)]);
            program.blocks[0].inst_mut(read).flags = flags;
            store_query_result(&mut program, read);
        }
        if is_written {
            let color = program.blocks[0].append_new_inst(
                Opcode::CompositeConstructU32x4,
                vec![
                    Value::ImmU32(1),
                    Value::ImmU32(2),
                    Value::ImmU32(3),
                    Value::ImmU32(4),
                ],
            );
            let write = program.blocks[0].append_new_inst(
                Opcode::ImageWrite,
                vec![
                    Value::ImmU32(0),
                    Value::ImmU32(7),
                    Value::Inst(InstRef {
                        block: 0,
                        inst: color,
                    }),
                ],
            );
            program.blocks[0].inst_mut(write).flags = flags;
        }
        program
    }

    fn image_buffer_atomic_program() -> Program {
        let mut program = image_buffer_program(true, true);
        program.info.uses_atomic_image_u32 = true;
        let flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Buffer as u8,
            image_format: ImageFormat::R32Uint as u8,
            ..Default::default()
        }
        .to_u32();
        for opcode in [
            Opcode::ImageAtomicIAdd32,
            Opcode::ImageAtomicSMin32,
            Opcode::ImageAtomicUMin32,
            Opcode::ImageAtomicSMax32,
            Opcode::ImageAtomicUMax32,
            Opcode::ImageAtomicAnd32,
            Opcode::ImageAtomicOr32,
            Opcode::ImageAtomicXor32,
            Opcode::ImageAtomicExchange32,
        ] {
            let atomic = program.blocks[0].append_new_inst(
                opcode,
                vec![
                    Value::ImmU32(0),
                    Value::ImmU32(7),
                    Value::ImmU32(0x8000_0001),
                ],
            );
            program.blocks[0].inst_mut(atomic).flags = flags;
        }
        program
    }

    fn storage_image_atomic_program() -> Program {
        let mut program = Program::new(Stage::Fragment);
        program.blocks.push(Block::new());
        program.info.uses_atomic_image_u32 = true;
        program.info.image_descriptors.push(ImageDescriptor {
            texture_type: TextureType::Color2D,
            format: ImageFormat::R32Uint,
            is_written: true,
            is_read: true,
            is_integer: true,
            cbuf_index: 0,
            cbuf_offset: 0,
            count: 1,
            size_shift: 0,
        });
        let coords = storage_coordinates(&mut program, TextureType::Color2D);
        let flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Color2D as u8,
            image_format: ImageFormat::R32Uint as u8,
            ..Default::default()
        }
        .to_u32();
        for opcode in [
            Opcode::ImageAtomicIAdd32,
            Opcode::ImageAtomicSMin32,
            Opcode::ImageAtomicUMin32,
            Opcode::ImageAtomicSMax32,
            Opcode::ImageAtomicUMax32,
            Opcode::ImageAtomicAnd32,
            Opcode::ImageAtomicOr32,
            Opcode::ImageAtomicXor32,
            Opcode::ImageAtomicExchange32,
        ] {
            let atomic = program.blocks[0].append_new_inst(
                opcode,
                vec![Value::ImmU32(0), coords, Value::ImmU32(0x8000_0001)],
            );
            program.blocks[0].inst_mut(atomic).flags = flags;
        }
        program
    }

    fn store_query_result(program: &mut Program, query: u32) {
        program.info.stores_frag_color[0] = true;
        for component in 0..4 {
            let extracted = program.blocks[0].append_new_inst(
                Opcode::CompositeExtractU32x4,
                vec![
                    Value::Inst(InstRef {
                        block: 0,
                        inst: query,
                    }),
                    Value::ImmU32(component),
                ],
            );
            let value = program.blocks[0].append_new_inst(
                Opcode::BitCastF32U32,
                vec![Value::Inst(InstRef {
                    block: 0,
                    inst: extracted,
                })],
            );
            program.blocks[0].append_new_inst(
                Opcode::SetFragColor,
                vec![
                    Value::ImmU32(0),
                    Value::ImmU32(component),
                    Value::Inst(InstRef {
                        block: 0,
                        inst: value,
                    }),
                ],
            );
        }
    }

    fn fetched_texture_program(
        texture_type: TextureType,
        is_depth: bool,
        is_integer: bool,
        is_multisample: bool,
        with_offset: bool,
    ) -> Program {
        let mut program = Program::new(Stage::Fragment);
        program.blocks.push(Block::new());
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type,
            is_depth,
            is_multisample,
            is_integer,
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: 1,
            size_shift: 0,
        });
        program.info.uses_sampled_1d = matches!(
            texture_type,
            TextureType::Color1D | TextureType::ColorArray1D
        );
        let coords = fetch_coordinates(&mut program, texture_type);
        let offset = if with_offset {
            let offset = program.blocks[0].append_new_inst(
                Opcode::CompositeConstructU32x2,
                vec![Value::ImmU32(1), Value::ImmU32(2)],
            );
            Value::Inst(InstRef {
                block: 0,
                inst: offset,
            })
        } else {
            Value::Void
        };
        let fetch = program.blocks[0].append_new_inst(
            Opcode::ImageFetch,
            vec![
                Value::ImmU32(0),
                coords,
                offset,
                Value::ImmU32(1),
                if is_multisample {
                    Value::ImmU32(0)
                } else {
                    Value::Void
                },
            ],
        );
        program.blocks[0].inst_mut(fetch).flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: match texture_type {
                TextureType::Color2DRect => TextureType::Color2D as u8,
                texture_type => texture_type as u8,
            },
            is_depth,
            ..Default::default()
        }
        .to_u32();
        store_sample_result(&mut program, fetch, true);
        program
    }

    fn texture_query_program(texture_type: TextureType, is_multisample: bool) -> Program {
        let mut program = Program::new(Stage::Fragment);
        program.blocks.push(Block::new());
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type,
            is_depth: false,
            is_multisample,
            is_integer: false,
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: 1,
            size_shift: 0,
        });
        program.info.uses_sampled_1d = matches!(
            texture_type,
            TextureType::Color1D | TextureType::ColorArray1D
        );
        let query = program.blocks[0].append_new_inst(
            Opcode::ImageQueryDimensions,
            vec![
                Value::ImmU32(0),
                Value::ImmU32(0),
                Value::ImmU1(is_multisample),
            ],
        );
        program.blocks[0].inst_mut(query).flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: match texture_type {
                TextureType::Color2DRect => TextureType::Color2D as u8,
                texture_type => texture_type as u8,
            },
            ..Default::default()
        }
        .to_u32();
        store_query_result(&mut program, query);
        program
    }

    fn texture_lod_query_program(texture_type: TextureType) -> Program {
        let mut program = Program::new(Stage::Fragment);
        program.blocks.push(Block::new());
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type,
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
            count: 1,
            size_shift: 0,
        });
        program.info.uses_sampled_1d = matches!(
            texture_type,
            TextureType::Color1D | TextureType::ColorArray1D
        );
        let coords = sample_coordinates(&mut program, texture_type);
        let query = program.blocks[0]
            .append_new_inst(Opcode::ImageQueryLod, vec![Value::ImmU32(0), coords]);
        program.blocks[0].inst_mut(query).flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: match texture_type {
                TextureType::Color2DRect => TextureType::Color2D as u8,
                texture_type => texture_type as u8,
            },
            ..Default::default()
        }
        .to_u32();
        store_sample_result(&mut program, query, true);
        program
    }

    fn texture_gradient_program(
        texture_type: TextureType,
        with_offset: bool,
        with_lod_clamp: bool,
    ) -> Program {
        let mut program = Program::new(Stage::Fragment);
        program.blocks.push(Block::new());
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type,
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
            count: 1,
            size_shift: 0,
        });
        program.info.uses_sampled_1d = matches!(
            texture_type,
            TextureType::Color1D | TextureType::ColorArray1D
        );
        let coords = sample_coordinates(&mut program, texture_type);
        let num_derivatives = match texture_type {
            TextureType::Color1D | TextureType::ColorArray1D => 1,
            TextureType::Color2D | TextureType::Color2DRect | TextureType::ColorArray2D => 2,
            TextureType::Color3D | TextureType::ColorCube | TextureType::ColorArrayCube => 3,
            TextureType::Buffer => unreachable!(),
        };
        let derivatives = program.blocks[0].append_new_inst(
            if num_derivatives == 1 {
                Opcode::CompositeConstructF32x2
            } else {
                Opcode::CompositeConstructF32x4
            },
            if num_derivatives == 1 {
                vec![Value::ImmF32(0.1), Value::ImmF32(0.2)]
            } else {
                vec![
                    Value::ImmF32(0.1),
                    Value::ImmF32(0.2),
                    Value::ImmF32(0.3),
                    Value::ImmF32(0.4),
                ]
            },
        );
        let fourth_argument = if num_derivatives == 3 {
            let second = program.blocks[0].append_new_inst(
                Opcode::CompositeConstructF32x2,
                vec![Value::ImmF32(0.5), Value::ImmF32(0.6)],
            );
            Value::Inst(InstRef {
                block: 0,
                inst: second,
            })
        } else if with_offset {
            if num_derivatives == 1 {
                Value::ImmU32(u32::MAX)
            } else {
                let offset = program.blocks[0].append_new_inst(
                    Opcode::CompositeConstructU32x2,
                    vec![Value::ImmU32(u32::MAX), Value::ImmU32(2)],
                );
                Value::Inst(InstRef {
                    block: 0,
                    inst: offset,
                })
            }
        } else {
            Value::Void
        };
        let gradient = program.blocks[0].append_new_inst(
            Opcode::ImageGradient,
            vec![
                Value::ImmU32(0),
                coords,
                Value::Inst(InstRef {
                    block: 0,
                    inst: derivatives,
                }),
                fourth_argument,
                if with_lod_clamp {
                    Value::ImmF32(0.5)
                } else {
                    Value::Void
                },
            ],
        );
        program.blocks[0].inst_mut(gradient).flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: match texture_type {
                TextureType::Color2DRect => TextureType::Color2D as u8,
                texture_type => texture_type as u8,
            },
            num_derivatives,
            has_lod_clamp: with_lod_clamp,
            ..Default::default()
        }
        .to_u32();
        store_sample_result(&mut program, gradient, true);
        program
    }

    #[derive(Clone, Copy)]
    enum GatherOffset {
        None,
        Single,
        Ptp,
    }

    fn gathered_texture_program(
        texture_type: TextureType,
        is_depth: bool,
        is_integer: bool,
        offset_kind: GatherOffset,
    ) -> Program {
        let mut program = Program::new(Stage::Fragment);
        program.blocks.push(Block::new());
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type,
            is_depth,
            is_multisample: false,
            is_integer,
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: 1,
            size_shift: 0,
        });
        let coords = sample_coordinates(&mut program, texture_type);
        let make_u32x4 = |program: &mut Program, values: [u32; 4]| {
            let inst = program.blocks[0].append_new_inst(
                Opcode::CompositeConstructU32x4,
                values.into_iter().map(Value::ImmU32).collect(),
            );
            Value::Inst(InstRef { block: 0, inst })
        };
        let (offset, offset2) = match offset_kind {
            GatherOffset::None => (Value::Void, Value::Void),
            GatherOffset::Single => {
                let inst = program.blocks[0].append_new_inst(
                    Opcode::CompositeConstructU32x2,
                    vec![Value::ImmU32(u32::MAX), Value::ImmU32(2)],
                );
                (Value::Inst(InstRef { block: 0, inst }), Value::Void)
            }
            GatherOffset::Ptp => (
                make_u32x4(&mut program, [u32::MAX, 0, 2, u32::MAX]),
                make_u32x4(&mut program, [0, 3, u32::MAX - 1, 1]),
            ),
        };
        let opcode = if is_depth {
            Opcode::ImageGatherDref
        } else {
            Opcode::ImageGather
        };
        let mut args = vec![Value::ImmU32(0), coords, offset, offset2];
        if is_depth {
            args.push(Value::ImmF32(0.5));
        }
        let gather = program.blocks[0].append_new_inst(opcode, args);
        program.blocks[0].inst_mut(gather).flags = TextureInstInfo {
            descriptor_index: 0,
            texture_type: match texture_type {
                TextureType::Color2DRect => TextureType::Color2D as u8,
                texture_type => texture_type as u8,
            },
            is_depth,
            gather_component: 2,
            ..Default::default()
        }
        .to_u32();
        store_sample_result(&mut program, gather, true);
        program
    }

    fn empty_program(stage: Stage) -> Program {
        let mut program = Program::new(stage);
        program.blocks.push(Block::new());
        program
    }

    fn render_area_program() -> Program {
        let mut program = empty_program(Stage::Fragment);
        program.info.uses_render_area = true;
        program.blocks[0].append_new_inst(Opcode::RenderArea, vec![]);
        program
    }

    fn rescaling_program(stage: Stage) -> Program {
        let mut program = empty_program(stage);
        program.info.uses_rescaling_uniform = true;
        if stage != Stage::Compute {
            program.blocks[0].append_new_inst(Opcode::ResolutionDownFactor, vec![]);
        }
        program.blocks[0].append_new_inst(Opcode::IsTextureScaled, vec![Value::ImmU32(3)]);
        program.blocks[0].append_new_inst(Opcode::IsImageScaled, vec![Value::ImmU32(5)]);
        program
    }

    fn subgroup_program() -> Program {
        let mut program = empty_program(Stage::Fragment);
        program.info.uses_fswzadd = true;
        program.info.uses_subgroup_invocation_id = true;
        program.info.uses_subgroup_shuffles = true;
        program.info.uses_subgroup_vote = true;
        program.info.uses_subgroup_mask = true;
        {
            let mut emitter = Emitter::new(&mut program, 0);
            emitter.lane_id();
            emitter.vote_all(Value::ImmU1(true));
            emitter.vote_any(Value::ImmU1(false));
            emitter.vote_equal(Value::ImmU1(true));
            emitter.subgroup_ballot(Value::ImmU1(true));
            emitter.subgroup_eq_mask();
            emitter.subgroup_lt_mask();
            emitter.subgroup_le_mask();
            emitter.subgroup_gt_mask();
            emitter.subgroup_ge_mask();
            let shuffle = emitter.shuffle_index(
                Value::ImmU32(0x1234_5678),
                Value::ImmU32(3),
                Value::ImmU32(31),
                Value::ImmU32(0),
            );
            emitter.get_in_bounds_from_op(shuffle);
            emitter.shuffle_up(
                Value::ImmU32(1),
                Value::ImmU32(2),
                Value::ImmU32(31),
                Value::ImmU32(0),
            );
            emitter.shuffle_down(
                Value::ImmU32(1),
                Value::ImmU32(2),
                Value::ImmU32(31),
                Value::ImmU32(0),
            );
            emitter.shuffle_butterfly(
                Value::ImmU32(1),
                Value::ImmU32(2),
                Value::ImmU32(31),
                Value::ImmU32(0),
            );
        }
        program.blocks[0].append_new_inst(
            Opcode::FSwizzleAdd,
            vec![Value::ImmF32(1.0), Value::ImmF32(2.0), Value::ImmU32(0xE4)],
        );
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
    fn compiles_direct_msl_fragment_builtins_and_demote_with_active_abi() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let mut program = empty_program(Stage::Fragment);
        program.info.uses_sample_id = true;
        program.info.uses_is_helper_invocation = true;
        program.info.uses_demote_to_helper_invocation = true;
        program.blocks[0].append_new_inst(Opcode::SampleId, vec![]);
        program.blocks[0].append_new_inst(Opcode::DemoteToHelperInvocation, vec![]);
        program.blocks[0].append_new_inst(Opcode::IsHelperInvocation, vec![]);

        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::for_device(device.profile()),
        )
        .expect("active fragment built-in SPIR-V/MSL must compile");
        let shader = validate_direct_msl_against_active_module(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &active,
        )
        .expect("direct fragment built-in MSL must compile with the active ABI");

        assert_eq!(shader.bindings(), active.bindings());
        assert!(shader.source().source.contains("[[sample_id]]"));
        assert!(shader.source().source.contains("simd_is_helper_thread()"));
        assert!(shader.source().source.contains("discard_fragment()"));
    }

    #[test]
    fn compiles_direct_msl_structured_control_flow_with_active_abi() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();

        for (name, program) in [
            ("if-phi", structured_phi_program()),
            ("loop", structured_loop_program()),
        ] {
            let spirv = emit_spirv(&program, &profile, &runtime_info);
            let active = compile_native_shader(
                device.device(),
                device.profile(),
                &spirv,
                &MetalShaderCompileOptions::for_compute_device(
                    device.profile(),
                    program.workgroup_size,
                ),
            )
            .unwrap_or_else(|error| panic!("active {name} SPIR-V/MSL must compile: {error}"));
            let direct = validate_direct_msl_against_active_module(
                device.device(),
                &program,
                &profile,
                &runtime_info,
                &active,
            )
            .unwrap_or_else(|error| {
                panic!("direct {name} MSL must compile with the active ABI: {error}")
            });

            assert_eq!(direct.bindings(), active.bindings());
            assert_eq!(direct.execution(), active.execution());
        }
    }

    #[test]
    fn compiles_direct_render_area_with_active_push_constant_abi() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let program = render_area_program();
        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::for_device(device.profile()),
        )
        .expect("active render-area SPIR-V/MSL must compile");
        let direct = validate_direct_msl_against_active_module(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &active,
        )
        .expect("direct render-area MSL must compile with the active push-constant ABI");

        assert_eq!(direct.bindings(), active.bindings());
        assert_eq!(direct.bindings().push_constant_buffer_index, Some(0));
        assert!(direct
            .source()
            .source
            .contains("render_area_push_constants.render_area"));
    }

    #[test]
    fn compiles_direct_rescaling_with_active_push_constant_abi() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        for stage in [Stage::Fragment, Stage::Compute] {
            let program = rescaling_program(stage);
            let spirv = emit_spirv(&program, &profile, &runtime_info);
            let options = if stage == Stage::Compute {
                MetalShaderCompileOptions::for_compute_device(
                    device.profile(),
                    program.workgroup_size,
                )
            } else {
                MetalShaderCompileOptions::for_device(device.profile())
            };
            let active = compile_native_shader(device.device(), device.profile(), &spirv, &options)
                .unwrap_or_else(|error| panic!("active {stage:?} rescaling must compile: {error}"));
            let direct = validate_direct_msl_against_active_module(
                device.device(),
                &program,
                &profile,
                &runtime_info,
                &active,
            )
            .unwrap_or_else(|error| {
                panic!("direct {stage:?} rescaling must match the active ABI: {error}")
            });

            assert_eq!(direct.bindings(), active.bindings());
            assert_eq!(direct.bindings().push_constant_buffer_index, Some(0));
            assert_eq!(
                direct
                    .source()
                    .source
                    .contains("rescaling_push_constants.down_factor"),
                stage != Stage::Compute
            );
            assert!(direct
                .source()
                .source
                .contains("rescaling_push_constants.rescaling_textures"));
            assert!(direct
                .source()
                .source
                .contains("rescaling_push_constants.rescaling_images"));
        }
    }

    #[test]
    fn compiles_direct_msl_generic_stage_interfaces_with_active_abi() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());

        let mut vertex = empty_program(Stage::VertexB);
        let vertex_attribute = shader_recompiler::ir::Attribute::generic(0, 0);
        vertex.info.loads.set(vertex_attribute.0 as usize, true);
        vertex.info.stores.set(vertex_attribute.0 as usize, true);
        let value = vertex.blocks[0].append_new_inst(
            Opcode::GetAttribute,
            vec![Value::Attribute(vertex_attribute), Value::ImmU32(0)],
        );
        vertex.blocks[0].append_new_inst(
            Opcode::SetAttribute,
            vec![
                Value::Attribute(vertex_attribute),
                Value::Inst(InstRef {
                    block: 0,
                    inst: value,
                }),
                Value::ImmU32(0),
            ],
        );
        let mut vertex_runtime = RuntimeInfo::default();
        vertex_runtime
            .previous_stage_stores
            .set(vertex_attribute.0 as usize, true);
        vertex_runtime.generic_input_types[0] = AttributeType::Float;

        let vertex_spirv = emit_spirv(&vertex, &profile, &vertex_runtime);
        let active_vertex = compile_native_shader(
            device.device(),
            device.profile(),
            &vertex_spirv,
            &MetalShaderCompileOptions::for_device(device.profile()),
        )
        .expect("active generic vertex SPIR-V/MSL must compile");
        let direct_vertex = validate_direct_msl_against_active_module(
            device.device(),
            &vertex,
            &profile,
            &vertex_runtime,
            &active_vertex,
        )
        .expect("direct generic vertex MSL must compile with the active ABI");
        assert_eq!(direct_vertex.bindings(), active_vertex.bindings());
        assert!(direct_vertex.source().source.contains("[[attribute(0)]]"));
        assert!(direct_vertex.source().source.contains("[[user(locn0)]]"));

        let mut fragment = empty_program(Stage::Fragment);
        let fragment_attribute = shader_recompiler::ir::Attribute::generic(0, 0);
        fragment.info.loads.set(fragment_attribute.0 as usize, true);
        fragment.info.interpolation[0] = Interpolation::NoPerspective;
        fragment.blocks[0].append_new_inst(
            Opcode::GetAttribute,
            vec![Value::Attribute(fragment_attribute), Value::ImmU32(0)],
        );
        let mut fragment_runtime = RuntimeInfo::default();
        fragment_runtime
            .previous_stage_stores
            .set(fragment_attribute.0 as usize, true);

        let fragment_spirv = emit_spirv(&fragment, &profile, &fragment_runtime);
        let active_fragment = compile_native_shader(
            device.device(),
            device.profile(),
            &fragment_spirv,
            &MetalShaderCompileOptions::for_device(device.profile()),
        )
        .expect("active generic fragment SPIR-V/MSL must compile");
        let direct_fragment = validate_direct_msl_against_active_module(
            device.device(),
            &fragment,
            &profile,
            &fragment_runtime,
            &active_fragment,
        )
        .expect("direct generic fragment MSL must compile with the active ABI");
        assert_eq!(direct_fragment.bindings(), active_fragment.bindings());
        assert!(direct_fragment
            .source()
            .source
            .contains("[[user(locn0), center_no_perspective]]"));
    }

    #[test]
    fn compiles_direct_msl_stage_builtins_with_active_abi() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();

        let mut vertex = empty_program(Stage::VertexB);
        for attribute in [
            shader_recompiler::ir::Attribute::INSTANCE_ID,
            shader_recompiler::ir::Attribute::VERTEX_ID,
            shader_recompiler::ir::Attribute::BASE_INSTANCE,
            shader_recompiler::ir::Attribute::BASE_VERTEX,
        ] {
            vertex.info.loads.set(attribute.0 as usize, true);
            vertex.blocks[0].append_new_inst(
                Opcode::GetAttribute,
                vec![Value::Attribute(attribute), Value::ImmU32(0)],
            );
            vertex.blocks[0].append_new_inst(
                Opcode::GetAttributeU32,
                vec![Value::Attribute(attribute), Value::ImmU32(0)],
            );
        }
        let vertex_spirv = emit_spirv(&vertex, &profile, &runtime_info);
        let active_vertex = compile_native_shader(
            device.device(),
            device.profile(),
            &vertex_spirv,
            &MetalShaderCompileOptions::for_device(device.profile()),
        )
        .expect("active vertex built-in SPIR-V/MSL must compile");
        let direct_vertex = validate_direct_msl_against_active_module(
            device.device(),
            &vertex,
            &profile,
            &runtime_info,
            &active_vertex,
        )
        .expect("direct vertex built-in MSL must compile with the active ABI");
        assert_eq!(direct_vertex.bindings(), active_vertex.bindings());
        assert!(direct_vertex.source().source.contains("[[vertex_id]]"));
        assert!(direct_vertex.source().source.contains("[[instance_id]]"));
        assert!(direct_vertex.source().source.contains("[[base_vertex]]"));
        assert!(direct_vertex.source().source.contains("[[base_instance]]"));

        let mut compatibility_profile = profile.clone();
        compatibility_profile.support_vertex_instance_id = false;
        let compatibility = shader_recompiler::backend::msl::emit_msl_with_options(
            &vertex,
            &compatibility_profile,
            &runtime_info,
            &shader_recompiler::backend::msl::MslOptions {
                language_version: device.profile().msl_language_version,
                fixed_subgroup_size: 32,
                supports_query_texture_lod: device.profile().supports_query_texture_lod,
                supports_read_write_textures: device.profile().supports_read_write_textures(),
                supports_texture_atomics: device.profile().supports_texture_atomics(),
                enable_point_size_builtin: true,
                disable_rasterization: false,
            },
        )
        .expect("compatibility vertex built-ins must lower directly");
        assert!(compatibility
            .source
            .source
            .contains("instance_index - base_instance"));
        compile_native_msl_artifact(device.device(), compatibility)
            .expect("compatibility vertex built-ins must compile natively");

        let mut fragment = empty_program(Stage::Fragment);
        for attribute in [
            shader_recompiler::ir::Attribute::PRIMITIVE_ID,
            shader_recompiler::ir::Attribute::LAYER,
            shader_recompiler::ir::Attribute::POSITION_X,
            shader_recompiler::ir::Attribute::POSITION_W,
            shader_recompiler::ir::Attribute::FRONT_FACE,
            shader_recompiler::ir::Attribute::POINT_SPRITE_S,
            shader_recompiler::ir::Attribute::POINT_SPRITE_T,
        ] {
            fragment.info.loads.set(attribute.0 as usize, true);
            fragment.blocks[0].append_new_inst(
                Opcode::GetAttribute,
                vec![Value::Attribute(attribute), Value::ImmU32(0)],
            );
        }
        fragment.blocks[0].append_new_inst(
            Opcode::GetAttributeU32,
            vec![
                Value::Attribute(shader_recompiler::ir::Attribute::PRIMITIVE_ID),
                Value::ImmU32(0),
            ],
        );
        let fragment_spirv = emit_spirv(&fragment, &profile, &runtime_info);
        let active_fragment = compile_native_shader(
            device.device(),
            device.profile(),
            &fragment_spirv,
            &MetalShaderCompileOptions::for_device(device.profile()),
        )
        .expect("active fragment built-in SPIR-V/MSL must compile");
        let direct_fragment = validate_direct_msl_against_active_module(
            device.device(),
            &fragment,
            &profile,
            &runtime_info,
            &active_fragment,
        )
        .expect("direct fragment built-in MSL must compile with the active ABI");
        assert_eq!(direct_fragment.bindings(), active_fragment.bindings());
        let source = &direct_fragment.source().source;
        assert!(source.contains("[[primitive_id]]"));
        assert!(source.contains("[[render_target_array_index]]"));
        assert!(source.contains("[[position]]"));
        assert!(source.contains("[[front_facing]]"));
        assert!(source.contains("[[point_coord]]"));
    }

    #[test]
    fn compiles_direct_msl_fragment_depth_mask_and_early_tests_with_active_abi() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo {
            convert_depth_mode: true,
            force_early_z: true,
            ..RuntimeInfo::default()
        };
        let mut program = empty_program(Stage::Fragment);
        program.info.stores_frag_depth = true;
        program.info.stores_sample_mask = true;
        program.blocks[0].append_new_inst(Opcode::SetFragDepth, vec![Value::ImmF32(0.25)]);
        program.blocks[0].append_new_inst(Opcode::SetSampleMask, vec![Value::ImmU32(0x5A)]);

        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::for_device(device.profile()),
        )
        .expect("active fragment depth/mask SPIR-V/MSL must compile");
        let direct = validate_direct_msl_against_active_module(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &active,
        )
        .expect("direct fragment depth/mask MSL must compile with the active ABI");

        assert_eq!(direct.bindings(), active.bindings());
        let source = &direct.source().source;
        assert!(!source.contains("[[depth(any)]]"));
        assert!(source.contains("[[sample_mask]]"));
        assert!(source.contains("[[early_fragment_tests]] fragment"));
    }

    #[test]
    fn compiles_direct_msl_vertex_special_outputs_with_active_abi() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo {
            convert_depth_mode: true,
            fixed_state_point_size: Some(2.5),
            ..RuntimeInfo::default()
        };
        let mut program = empty_program(Stage::VertexB);
        let point_size = shader_recompiler::ir::Attribute::POINT_SIZE;
        let clip0 = shader_recompiler::ir::Attribute::CLIP_DISTANCE_0;
        program.info.stores.set(point_size.0 as usize, true);
        program.info.stores.set(clip0.0 as usize, true);
        program.info.used_clip_distances = 1;
        program.blocks[0].append_new_inst(Opcode::Prologue, vec![]);
        program.blocks[0].append_new_inst(
            Opcode::SetAttribute,
            vec![
                Value::Attribute(point_size),
                Value::ImmF32(1.5),
                Value::ImmU32(0),
            ],
        );
        program.blocks[0].append_new_inst(
            Opcode::SetAttribute,
            vec![
                Value::Attribute(clip0),
                Value::ImmF32(-0.25),
                Value::ImmU32(0),
            ],
        );
        program.blocks[0].append_new_inst(Opcode::Epilogue, vec![]);

        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::for_device(device.profile()),
        )
        .expect("active vertex special-output SPIR-V/MSL must compile");
        let direct = validate_direct_msl_against_active_module(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &active,
        )
        .expect("direct vertex special-output MSL must compile with the active ABI");

        assert_eq!(direct.bindings(), active.bindings());
        let source = &direct.source().source;
        assert!(source.contains("[[point_size]]"));
        assert!(source.contains("[[clip_distance]]"));
        assert!(source.contains("output.position.z ="));
    }

    #[test]
    fn suppresses_point_size_for_triangle_render_pipelines() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo {
            fixed_state_point_size: Some(2.5),
            ..RuntimeInfo::default()
        };
        let mut program = empty_program(Stage::VertexB);
        let point_size = shader_recompiler::ir::Attribute::POINT_SIZE;
        program.info.stores.set(point_size.0 as usize, true);
        program.blocks[0].append_new_inst(Opcode::Prologue, vec![]);
        program.blocks[0].append_new_inst(
            Opcode::SetAttribute,
            vec![
                Value::Attribute(point_size),
                Value::ImmF32(1.5),
                Value::ImmU32(0),
            ],
        );
        program.blocks[0].append_new_inst(Opcode::Epilogue, vec![]);

        let options = MetalShaderCompileOptions {
            enable_point_size_builtin: false,
            ..MetalShaderCompileOptions::for_device(device.profile())
        };
        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let compatibility =
            compile_native_shader(device.device(), device.profile(), &spirv, &options)
                .expect("SPIRV-Cross must suppress PointSize for a triangle pipeline");
        let direct = compile_direct_msl_shader_with_bindings(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &options,
            &mut Bindings::default(),
        )
        .expect("direct MSL must suppress PointSize for a triangle pipeline");

        for shader in [&compatibility, &direct] {
            assert!(!shader.source().source.contains("[[point_size]]"));
            let descriptor = MTLRenderPipelineDescriptor::new();
            descriptor.setVertexFunction(Some(shader.function()));
            unsafe {
                descriptor.setInputPrimitiveTopology(MTLPrimitiveTopologyClass::Triangle);
            }
            device
                .device()
                .newRenderPipelineStateWithDescriptor_error(&descriptor)
                .expect("triangle pipeline must accept a shader that declared guest PointSize");
        }
    }

    #[test]
    fn disables_vertex_outputs_for_non_rasterizing_render_pipelines() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let mut program = empty_program(Stage::VertexB);
        let position = shader_recompiler::ir::Attribute::POSITION_X;
        program.info.stores.set(position.0 as usize, true);
        program.blocks[0].append_new_inst(Opcode::Prologue, vec![]);
        program.blocks[0].append_new_inst(
            Opcode::SetAttribute,
            vec![
                Value::Attribute(position),
                Value::ImmF32(1.0),
                Value::ImmU32(0),
            ],
        );
        program.blocks[0].append_new_inst(Opcode::Epilogue, vec![]);

        let options = MetalShaderCompileOptions {
            disable_rasterization: true,
            ..MetalShaderCompileOptions::for_device(device.profile())
        };
        let spirv = emit_spirv(&program, &profile, &RuntimeInfo::default());
        let compatibility =
            compile_native_shader(device.device(), device.profile(), &spirv, &options)
                .expect("SPIRV-Cross must return void when rasterization is disabled");
        let direct = compile_direct_msl_shader_with_bindings(
            device.device(),
            &program,
            &profile,
            &RuntimeInfo::default(),
            &options,
            &mut Bindings::default(),
        )
        .expect("direct MSL must return void when rasterization is disabled");

        for shader in [&compatibility, &direct] {
            assert!(shader.source().source.contains("vertex void main0("));
            let descriptor = MTLRenderPipelineDescriptor::new();
            descriptor.setVertexFunction(Some(shader.function()));
            descriptor.setRasterizationEnabled(false);
            unsafe {
                descriptor.setInputPrimitiveTopology(MTLPrimitiveTopologyClass::Triangle);
            }
            device
                .device()
                .newRenderPipelineStateWithDescriptor_error(&descriptor)
                .expect("non-rasterizing pipeline must accept a void vertex entry point");
        }
    }

    #[test]
    fn compiles_direct_msl_alpha_test_and_dual_source_with_active_abi() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo {
            alpha_test_func: Some(CompareFunction::NotEqual),
            alpha_test_reference: 0.5,
            dual_source_blend: true,
            ..RuntimeInfo::default()
        };
        let mut program = empty_program(Stage::Fragment);
        program.info.stores_frag_color[0] = true;
        program.blocks[0].append_new_inst(Opcode::Prologue, vec![]);
        program.blocks[0].append_new_inst(
            Opcode::SetFragColor,
            vec![Value::ImmU32(0), Value::ImmU32(3), Value::ImmF32(0.75)],
        );
        program.blocks[0].append_new_inst(Opcode::Epilogue, vec![]);

        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::for_device(device.profile()),
        )
        .expect("active alpha-test dual-source SPIR-V/MSL must compile");
        let direct = validate_direct_msl_against_active_module(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &active,
        )
        .expect("direct alpha-test dual-source MSL must compile with the active ABI");

        assert_eq!(direct.bindings(), active.bindings());
        let source = &direct.source().source;
        assert!(source.contains("[[color(0), index(0)]]"));
        assert!(source.contains("[[color(0), index(1)]]"));
        assert!(source.contains("discard_fragment()"));
    }

    #[test]
    fn compiles_direct_msl_derivatives_with_active_abi() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let mut program = empty_program(Stage::Fragment);
        program.info.uses_derivatives = true;
        program.info.stores_frag_color[0] = true;
        let derivatives = [
            (Opcode::DPdxFine, 1.0),
            (Opcode::DPdxCoarse, 2.0),
            (Opcode::DPdyFine, 3.0),
            (Opcode::DPdyCoarse, 4.0),
        ]
        .map(|(opcode, value)| {
            program.blocks[0].append_new_inst(opcode, vec![Value::ImmF32(value)])
        });
        for (component, derivative) in derivatives.into_iter().enumerate() {
            program.blocks[0].append_new_inst(
                Opcode::SetFragColor,
                vec![
                    Value::ImmU32(0),
                    Value::ImmU32(component as u32),
                    Value::Inst(InstRef {
                        block: 0,
                        inst: derivative,
                    }),
                ],
            );
        }

        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::for_device(device.profile()),
        )
        .expect("active derivative SPIR-V/MSL must compile");
        let direct = validate_direct_msl_against_active_module(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &active,
        )
        .expect("direct derivative MSL must compile with the active ABI");

        assert_eq!(direct.bindings(), active.bindings());
        let source = &direct.source().source;
        assert_eq!(source.matches("dfdx(").count(), 2);
        assert_eq!(source.matches("dfdy(").count(), 2);
    }

    #[test]
    fn compiles_direct_msl_warp_family_with_active_abi() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let program = subgroup_program();

        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::for_device(device.profile()),
        )
        .expect("active warp SPIR-V/MSL must compile");
        let direct = validate_direct_msl_against_active_module(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &active,
        )
        .expect("direct warp MSL must compile with the active ABI");

        assert_eq!(direct.bindings(), active.bindings());
        let source = &direct.source().source;
        assert!(source.contains("thread_index_in_simdgroup"));
        assert!(source.contains("simd_ballot("));
        assert!(source.contains("simd_shuffle("));
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
                fixed_subgroup_size: 32,
                supports_query_texture_lod: device.profile().supports_query_texture_lod,
                supports_read_write_textures: device.profile().supports_read_write_textures(),
                supports_texture_atomics: device.profile().supports_texture_atomics(),
                enable_point_size_builtin: true,
                disable_rasterization: false,
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
    fn compiles_direct_msl_compute_position_builtins_with_active_abi() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let mut program = empty_program(Stage::Compute);
        program.workgroup_size = [8, 4, 2];
        program.info.uses_workgroup_id = true;
        program.info.uses_local_invocation_id = true;
        program.blocks[0].append_new_inst(Opcode::WorkgroupId, vec![]);
        program.blocks[0].append_new_inst(Opcode::LocalInvocationId, vec![]);

        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::for_compute_device(
                device.profile(),
                program.workgroup_size,
            ),
        )
        .expect("active compute built-in SPIR-V/MSL must compile");
        let shader = validate_direct_msl_against_active_module(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &active,
        )
        .expect("direct compute built-in MSL must compile with the active ABI");

        assert_eq!(shader.bindings(), active.bindings());
        assert_eq!(shader.execution().workgroup_size, Some([8, 4, 2]));
        assert!(shader
            .source()
            .source
            .contains("[[threadgroup_position_in_grid]]"));
        assert!(shader
            .source()
            .source
            .contains("[[thread_position_in_threadgroup]]"));
    }

    #[test]
    fn compiles_direct_msl_local_memory_with_active_abi() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let mut program = empty_program(Stage::Compute);
        program.local_memory_size = 18;
        program.info.uses_local_memory = true;
        let load = program.blocks[0].append_new_inst(Opcode::LoadLocal, vec![Value::ImmU32(2)]);
        program.blocks[0].append_new_inst(
            Opcode::WriteLocal,
            vec![
                Value::ImmU32(3),
                Value::Inst(InstRef {
                    block: 0,
                    inst: load,
                }),
            ],
        );

        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::for_compute_device(
                device.profile(),
                program.workgroup_size,
            ),
        )
        .expect("active local-memory SPIR-V/MSL must compile");
        let shader = validate_direct_msl_against_active_module(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &active,
        )
        .expect("direct local-memory MSL must compile with the active ABI");

        assert_eq!(shader.bindings(), active.bindings());
        assert!(shader.source().source.contains("thread uint lmem[5]"));
    }

    #[test]
    fn compiles_direct_msl_shared_memory_at_msl_2_3_baseline() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut program = empty_program(Stage::Compute);
        program.shared_memory_size = 64;
        program.info.uses_int8 = true;
        let load = program.blocks[0].append_new_inst(Opcode::LoadSharedU32, vec![Value::ImmU32(4)]);
        program.blocks[0].append_new_inst(
            Opcode::WriteSharedU8,
            vec![
                Value::ImmU32(3),
                Value::Inst(InstRef {
                    block: 0,
                    inst: load,
                }),
            ],
        );
        program.blocks[0].append_new_inst(Opcode::Barrier, vec![]);
        program.blocks[0].append_new_inst(Opcode::WorkgroupMemoryBarrier, vec![]);
        program.blocks[0].append_new_inst(Opcode::DeviceMemoryBarrier, vec![]);
        let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
            &program,
            &Profile::default(),
            &RuntimeInfo::default(),
            &shader_recompiler::backend::msl::MslOptions {
                language_version: shader_recompiler::backend::msl::MslVersion::V2_3,
                fixed_subgroup_size: 32,
                supports_query_texture_lod: false,
                supports_read_write_textures: false,
                supports_texture_atomics: false,
                enable_point_size_builtin: true,
                disable_rasterization: false,
            },
        )
        .expect("shared-memory compute IR must lower directly to MSL 2.3");

        let shader = compile_native_msl_artifact(device.device(), artifact)
            .expect("direct shared-memory MSL 2.3 must compile natively");
        assert_eq!(shader.source().stage, Stage::Compute);
        assert!(shader.source().source.contains("threadgroup uint smem[16]"));
    }

    #[test]
    fn compiles_direct_msl_memory_fences_for_selected_language_version() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut program = empty_program(Stage::Compute);
        program.blocks[0].append_new_inst(Opcode::WorkgroupMemoryBarrier, vec![]);
        program.blocks[0].append_new_inst(Opcode::DeviceMemoryBarrier, vec![]);
        let language_version = device.profile().msl_language_version;
        let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
            &program,
            &Profile::default(),
            &RuntimeInfo::default(),
            &shader_recompiler::backend::msl::MslOptions {
                language_version,
                fixed_subgroup_size: 32,
                supports_query_texture_lod: device.profile().supports_query_texture_lod,
                supports_read_write_textures: device.profile().supports_read_write_textures(),
                supports_texture_atomics: device.profile().supports_texture_atomics(),
                enable_point_size_builtin: true,
                disable_rasterization: false,
            },
        )
        .expect("memory-barrier IR must lower directly to MSL");
        if language_version >= shader_recompiler::backend::msl::MslVersion::V3_2 {
            assert!(artifact.source.source.contains("atomic_thread_fence"));
        } else {
            assert!(artifact.source.source.contains("threadgroup_barrier"));
        }

        let shader = compile_native_msl_artifact(device.device(), artifact)
            .expect("direct memory-barrier MSL must compile as a native Metal function");
        assert_eq!(shader.source().stage, Stage::Compute);
        assert_eq!(shader.function().name().to_string(), "main0");
    }

    #[test]
    fn compiles_direct_msl_shared_and_storage_atomics_with_active_abi() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let mut program = empty_program(Stage::Compute);
        program.shared_memory_size = 64;
        program.info.uses_shared_increment = true;
        program.info.storage_buffers_descriptors.push(
            shader_recompiler::shader_info::StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: true,
            },
        );
        program.blocks[0].append_new_inst(
            Opcode::SharedAtomicInc32,
            vec![Value::ImmU32(4), Value::ImmU32(7)],
        );
        program.blocks[0].append_new_inst(
            Opcode::StorageAtomicSMin32,
            vec![Value::ImmU32(0), Value::ImmU32(8), Value::ImmU32(u32::MAX)],
        );
        shader_recompiler::ir_opt::collect_shader_info_pass::collect_shader_info_pass(&mut program);
        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::for_compute_device(
                device.profile(),
                program.workgroup_size,
            ),
        )
        .expect("active shared/storage atomic SPIR-V/MSL must compile");
        let shader = validate_direct_msl_against_active_module(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &active,
        )
        .expect("direct 32-bit memory atomic MSL must compile with the active ABI");
        assert_eq!(shader.source().stage, Stage::Compute);
        assert_eq!(shader.bindings(), active.bindings());
        assert!(shader.source().source.contains("spvAtomicInc"));
        assert!(shader.source().source.contains("atomic_fetch_min_explicit"));
    }

    #[test]
    fn compiles_direct_msl_storage_fp_atomics_with_metal() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let mut program = empty_program(Stage::Compute);
        program.info.storage_buffers_descriptors.push(
            shader_recompiler::shader_info::StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: true,
            },
        );
        program.blocks[0].append_new_inst(
            Opcode::StorageAtomicAddF32,
            vec![Value::ImmU32(0), Value::ImmU32(12), Value::ImmF32(0.5)],
        );
        let half_x =
            program.blocks[0].append_new_inst(Opcode::ConvertF16F32, vec![Value::ImmF32(1.0)]);
        let half_y =
            program.blocks[0].append_new_inst(Opcode::ConvertF16F32, vec![Value::ImmF32(2.0)]);
        let half_value = program.blocks[0].append_new_inst(
            Opcode::CompositeConstructF16x2,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: half_x,
                }),
                Value::Inst(InstRef {
                    block: 0,
                    inst: half_y,
                }),
            ],
        );
        program.blocks[0].append_new_inst(
            Opcode::StorageAtomicMinF16x2,
            vec![
                Value::ImmU32(0),
                Value::ImmU32(16),
                Value::Inst(InstRef {
                    block: 0,
                    inst: half_value,
                }),
            ],
        );
        let float_value = program.blocks[0].append_new_inst(
            Opcode::CompositeConstructF32x2,
            vec![Value::ImmF32(1.0), Value::ImmF32(2.0)],
        );
        program.blocks[0].append_new_inst(
            Opcode::StorageAtomicMaxF32x2,
            vec![
                Value::ImmU32(0),
                Value::ImmU32(20),
                Value::Inst(InstRef {
                    block: 0,
                    inst: float_value,
                }),
            ],
        );
        shader_recompiler::ir_opt::collect_shader_info_pass::collect_shader_info_pass(&mut program);

        let mut bindings = Bindings::default();
        let shader = compile_direct_msl_shader_with_bindings(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &MetalShaderCompileOptions::for_compute_device(
                device.profile(),
                program.workgroup_size,
            ),
            &mut bindings,
        )
        .expect("direct floating-point storage atomic MSL must compile with Metal");

        assert!(shader.source().source.contains("spvAtomicAddF32"));
        assert!(shader.source().source.contains("spvAtomicMinF16x2"));
        assert!(shader.source().source.contains("spvAtomicMaxF32x2"));
    }

    #[test]
    fn compiles_direct_msl_wide_atomic_fallbacks_with_metal() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        if !profile.support_int64 {
            return;
        }
        let runtime_info = RuntimeInfo::default();
        let mut program = empty_program(Stage::Compute);
        program.shared_memory_size = 64;
        program.info.storage_buffers_descriptors.push(
            shader_recompiler::shader_info::StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: true,
            },
        );
        let pair = program.blocks[0].append_new_inst(
            Opcode::CompositeConstructU32x2,
            vec![Value::ImmU32(3), Value::ImmU32(5)],
        );
        program.blocks[0].append_new_inst(
            Opcode::SharedAtomicExchange64,
            vec![Value::ImmU32(0), Value::ImmU64(7)],
        );
        program.blocks[0].append_new_inst(
            Opcode::StorageAtomicSMin64,
            vec![Value::ImmU32(0), Value::ImmU32(8), Value::ImmU64(9)],
        );
        program.blocks[0].append_new_inst(
            Opcode::StorageAtomicSMax32x2,
            vec![
                Value::ImmU32(0),
                Value::ImmU32(16),
                Value::Inst(InstRef {
                    block: 0,
                    inst: pair,
                }),
            ],
        );
        shader_recompiler::ir_opt::collect_shader_info_pass::collect_shader_info_pass(&mut program);

        let mut bindings = Bindings::default();
        let shader = compile_direct_msl_shader_with_bindings(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &MetalShaderCompileOptions::for_compute_device(
                device.profile(),
                program.workgroup_size,
            ),
            &mut bindings,
        )
        .expect("direct wide atomic fallback MSL must compile with Metal");

        assert!(shader.source().source.contains("spv_shared_wide_"));
        assert!(shader.source().source.contains("as_type<ulong>(min"));
        assert!(shader.source().source.contains("as_type<uint2>(max"));
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
                fixed_subgroup_size: 32,
                supports_query_texture_lod: device.profile().supports_query_texture_lod,
                supports_read_write_textures: device.profile().supports_read_write_textures(),
                supports_texture_atomics: device.profile().supports_texture_atomics(),
                enable_point_size_builtin: true,
                disable_rasterization: false,
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
        block.append_new_inst(Opcode::ConvertF32S8, vec![Value::ImmU32(0x1234_12FE)]);
        block.append_new_inst(Opcode::ConvertF32S16, vec![Value::ImmU32(0x1234_FFFE)]);
        block.append_new_inst(Opcode::ConvertF32U8, vec![Value::ImmU32(0x1234_12FE)]);
        block.append_new_inst(Opcode::ConvertF32U16, vec![Value::ImmU32(0x1234_FFFE)]);
        block.append_new_inst(Opcode::ConvertS16F32, vec![Value::ImmF32(-2.0)]);
        block.append_new_inst(Opcode::ConvertU16F32, vec![Value::ImmF32(65535.0)]);
        block.append_new_inst(
            Opcode::SelectU16,
            vec![
                Value::ImmU1(true),
                Value::ImmU32(0x1234),
                Value::ImmU32(0x5678),
            ],
        );
        block.append_new_inst(Opcode::YDirection, vec![]);

        let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
            &program,
            &Profile::default(),
            &RuntimeInfo::default(),
            &shader_recompiler::backend::msl::MslOptions {
                language_version: device.profile().msl_language_version,
                fixed_subgroup_size: 32,
                supports_query_texture_lod: device.profile().supports_query_texture_lod,
                supports_read_write_textures: device.profile().supports_read_write_textures(),
                supports_texture_atomics: device.profile().supports_texture_atomics(),
                enable_point_size_builtin: true,
                disable_rasterization: false,
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
        block.append_new_inst(Opcode::UnpackFloat2x16, vec![Value::ImmU32(0xC000_3C00)]);
        let half_pair = block.append_new_inst(
            Opcode::CompositeConstructF32x2,
            vec![Value::ImmF32(1.0), Value::ImmF32(-2.0)],
        );
        block.append_new_inst(
            Opcode::PackHalf2x16,
            vec![Value::Inst(InstRef {
                block: 0,
                inst: half_pair,
            })],
        );
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
                fixed_subgroup_size: 32,
                supports_query_texture_lod: device.profile().supports_query_texture_lod,
                supports_read_write_textures: device.profile().supports_read_write_textures(),
                supports_texture_atomics: device.profile().supports_texture_atomics(),
                enable_point_size_builtin: true,
                disable_rasterization: false,
            },
        )
        .expect("native half/int64 IR must lower directly to MSL when supported");
        assert!(artifact.source.source.contains("half v_0_0 = spvFAdd("));
        if profile.support_int64 {
            assert!(artifact.source.source.contains("ulong v_0_23 ="));
        }

        let shader = compile_native_msl_artifact(device.device(), artifact)
            .expect("direct half/int64 MSL must compile as a native Metal function");

        assert_eq!(shader.source().stage, Stage::VertexB);
        assert_eq!(shader.function().name().to_string(), "main0");
    }

    #[test]
    fn compiles_direct_msl_bitwise_conversion_family_with_metal() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let mut program = empty_program(Stage::VertexB);
        program.info.uses_fp16 = true;
        let block = &mut program.blocks[0];
        block.append_new_inst(Opcode::BitCastU16F16, vec![Value::ImmF16(0xBC00)]);
        block.append_new_inst(Opcode::BitCastF16U16, vec![Value::ImmU32(0x3C00)]);
        let half_pair = block.append_new_inst(
            Opcode::CompositeConstructF16x2,
            vec![Value::ImmF16(0x3C00), Value::ImmF16(0xC000)],
        );
        block.append_new_inst(
            Opcode::PackFloat2x16,
            vec![Value::Inst(InstRef {
                block: 0,
                inst: half_pair,
            })],
        );
        block.append_new_inst(Opcode::UnpackHalf2x16, vec![Value::ImmU32(0xC000_3C00)]);
        if profile.support_int64 {
            program.info.uses_int64 = true;
            let uint_pair = block.append_new_inst(
                Opcode::CompositeConstructU32x2,
                vec![Value::ImmU32(0x89AB_CDEF), Value::ImmU32(0x0123_4567)],
            );
            let packed = block.append_new_inst(
                Opcode::PackUint2x32,
                vec![Value::Inst(InstRef {
                    block: 0,
                    inst: uint_pair,
                })],
            );
            block.append_new_inst(
                Opcode::UnpackUint2x32,
                vec![Value::Inst(InstRef {
                    block: 0,
                    inst: packed,
                })],
            );
        }

        let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
            &program,
            &profile,
            &RuntimeInfo::default(),
            &shader_recompiler::backend::msl::MslOptions {
                language_version: device.profile().msl_language_version,
                fixed_subgroup_size: 32,
                supports_query_texture_lod: device.profile().supports_query_texture_lod,
                supports_read_write_textures: device.profile().supports_read_write_textures(),
                supports_texture_atomics: device.profile().supports_texture_atomics(),
                enable_point_size_builtin: true,
                disable_rasterization: false,
            },
        )
        .expect("bitwise conversion IR must lower directly to MSL");
        assert!(artifact
            .source
            .source
            .contains("float2(as_type<half2>(0xC0003C00u))"));
        if profile.support_int64 {
            assert!(artifact.source.source.contains("as_type<ulong>("));
            assert!(artifact.source.source.contains("as_type<uint2>("));
        }

        let shader = compile_native_msl_artifact(device.device(), artifact)
            .expect("direct bitwise-conversion MSL must compile as a native Metal function");
        assert_eq!(shader.source().stage, Stage::VertexB);
        assert_eq!(shader.function().name().to_string(), "main0");
    }

    #[test]
    fn compiles_direct_msl_global_memory_helpers_with_metal() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        assert!(profile.support_int64);
        let mut program = empty_program(Stage::Compute);
        program.info.uses_global_memory = true;
        program.info.stores_global_memory = true;
        program.info.uses_int64 = true;
        program.info.nvn_buffer_used = 1;
        program
            .info
            .constant_buffer_descriptors
            .push(ConstantBufferDescriptor { index: 0, count: 1 });
        program
            .info
            .storage_buffers_descriptors
            .push(StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0x110,
                count: 1,
                is_written: true,
            });
        let block = &mut program.blocks[0];
        block.append_new_inst(Opcode::LoadGlobalU8, vec![Value::ImmU64(0x1001)]);
        block.append_new_inst(Opcode::LoadGlobalS8, vec![Value::ImmU64(0x1002)]);
        block.append_new_inst(Opcode::LoadGlobalU16, vec![Value::ImmU64(0x1002)]);
        block.append_new_inst(Opcode::LoadGlobalS16, vec![Value::ImmU64(0x1000)]);
        let load32 = block.append_new_inst(Opcode::LoadGlobal32, vec![Value::ImmU64(0x1000)]);
        let load64 = block.append_new_inst(Opcode::LoadGlobal64, vec![Value::ImmU64(0x1008)]);
        let load128 = block.append_new_inst(Opcode::LoadGlobal128, vec![Value::ImmU64(0x1010)]);
        block.append_new_inst(
            Opcode::WriteGlobalU8,
            vec![Value::ImmU64(0x1041), Value::ImmU32(0xAB)],
        );
        block.append_new_inst(
            Opcode::WriteGlobalS16,
            vec![Value::ImmU64(0x1042), Value::ImmU32(0xFFFF_8000)],
        );
        block.append_new_inst(
            Opcode::WriteGlobal32,
            vec![
                Value::ImmU64(0x1020),
                Value::Inst(InstRef {
                    block: 0,
                    inst: load32,
                }),
            ],
        );
        block.append_new_inst(
            Opcode::WriteGlobal64,
            vec![
                Value::ImmU64(0x1028),
                Value::Inst(InstRef {
                    block: 0,
                    inst: load64,
                }),
            ],
        );
        block.append_new_inst(
            Opcode::WriteGlobal128,
            vec![
                Value::ImmU64(0x1030),
                Value::Inst(InstRef {
                    block: 0,
                    inst: load128,
                }),
            ],
        );

        let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
            &program,
            &profile,
            &RuntimeInfo::default(),
            &shader_recompiler::backend::msl::MslOptions {
                language_version: device.profile().msl_language_version,
                fixed_subgroup_size: 32,
                supports_query_texture_lod: device.profile().supports_query_texture_lod,
                supports_read_write_textures: device.profile().supports_read_write_textures(),
                supports_texture_atomics: device.profile().supports_texture_atomics(),
                enable_point_size_builtin: true,
                disable_rasterization: false,
            },
        )
        .expect("global-memory IR must lower directly to MSL");
        let shader = compile_native_msl_artifact(device.device(), artifact)
            .expect("direct global-memory MSL must compile as a native Metal function");
        assert_eq!(shader.source().stage, Stage::Compute);
        assert_eq!(shader.function().name().to_string(), "main0");
    }

    #[test]
    fn compiles_direct_msl_global_atomics_with_metal() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        assert!(profile.support_int64);
        let mut program = empty_program(Stage::Compute);
        program.info.uses_global_memory = true;
        program.info.stores_global_memory = true;
        program.info.uses_int64 = true;
        program.info.nvn_buffer_used = 1;
        program
            .info
            .constant_buffer_descriptors
            .push(ConstantBufferDescriptor { index: 0, count: 1 });
        program
            .info
            .storage_buffers_descriptors
            .push(StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0x110,
                count: 1,
                is_written: true,
            });
        program.blocks[0].append_new_inst(
            Opcode::GlobalAtomicIAdd32,
            vec![Value::ImmU64(0x1000), Value::ImmU32(1)],
        );
        program.blocks[0].append_new_inst(
            Opcode::GlobalAtomicInc32,
            vec![Value::ImmU64(0x1004), Value::ImmU32(7)],
        );
        program.blocks[0].append_new_inst(
            Opcode::GlobalAtomicSMin64,
            vec![Value::ImmU64(0x1008), Value::ImmU64(9)],
        );
        let address = program.blocks[0].append_new_inst(
            Opcode::CompositeConstructU32x2,
            vec![Value::ImmU32(0x1010), Value::ImmU32(0)],
        );
        let pair = program.blocks[0].append_new_inst(
            Opcode::CompositeConstructU32x2,
            vec![Value::ImmU32(3), Value::ImmU32(5)],
        );
        program.blocks[0].append_new_inst(
            Opcode::GlobalAtomicSMax32x2,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: address,
                }),
                Value::Inst(InstRef {
                    block: 0,
                    inst: pair,
                }),
            ],
        );
        program.blocks[0].append_new_inst(
            Opcode::GlobalAtomicAddF32,
            vec![Value::ImmU64(0x1018), Value::ImmF32(0.5)],
        );
        let half_value = program.blocks[0].append_new_inst(
            Opcode::CompositeConstructF16x2,
            vec![Value::ImmF16(0x3C00), Value::ImmF16(0x4000)],
        );
        program.blocks[0].append_new_inst(
            Opcode::GlobalAtomicMinF16x2,
            vec![
                Value::ImmU64(0x101C),
                Value::Inst(InstRef {
                    block: 0,
                    inst: half_value,
                }),
            ],
        );
        let float_value = program.blocks[0].append_new_inst(
            Opcode::CompositeConstructF32x2,
            vec![Value::ImmF32(1.0), Value::ImmF32(2.0)],
        );
        program.blocks[0].append_new_inst(
            Opcode::GlobalAtomicMaxF32x2,
            vec![
                Value::ImmU64(0x1020),
                Value::Inst(InstRef {
                    block: 0,
                    inst: float_value,
                }),
            ],
        );

        let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
            &program,
            &profile,
            &RuntimeInfo::default(),
            &shader_recompiler::backend::msl::MslOptions {
                language_version: device.profile().msl_language_version,
                fixed_subgroup_size: 32,
                supports_query_texture_lod: device.profile().supports_query_texture_lod,
                supports_read_write_textures: device.profile().supports_read_write_textures(),
                supports_texture_atomics: device.profile().supports_texture_atomics(),
                enable_point_size_builtin: true,
                disable_rasterization: false,
            },
        )
        .expect("global atomic IR must lower directly to MSL");
        let shader = compile_native_msl_artifact(device.device(), artifact)
            .expect("direct global atomic MSL must compile as a native Metal function");
        assert_eq!(shader.source().stage, Stage::Compute);
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

    #[test]
    fn compiles_and_validates_direct_sampled_texture_msl() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let program = sampled_texture_program(2, TextureType::Color2D);
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::default(),
        )
        .expect("active sampled-texture SPIR-V/MSL must compile");
        assert!(active.source().source.contains(".sample("));
        assert!(active.source().source.contains("level(1.0)"));

        let direct = validate_direct_msl_against_active_module(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &active,
        )
        .expect("direct sampled-texture MSL must compile with the same ABI");

        assert_eq!(direct.bindings(), active.bindings());
        assert_eq!(direct.bindings().texture_count, 2);
        assert_eq!(direct.bindings().sampler_count, 2);
        assert!(direct
            .source()
            .source
            .contains("array<texture2d<float>, 2> tex0"));
    }

    #[test]
    fn compiles_direct_sampled_texture_dimensions_with_active_abi() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        for texture_type in [
            TextureType::Color1D,
            TextureType::ColorArray1D,
            TextureType::Color2DRect,
            TextureType::ColorArray2D,
            TextureType::Color3D,
            TextureType::ColorCube,
            TextureType::ColorArrayCube,
        ] {
            let program = sampled_texture_program(1, texture_type);
            let spirv = emit_spirv(&program, &profile, &runtime_info);
            let active = compile_native_shader(
                device.device(),
                device.profile(),
                &spirv,
                &MetalShaderCompileOptions::default(),
            )
            .unwrap_or_else(|error| {
                panic!("active {texture_type:?} SPIR-V/MSL must compile: {error}")
            });
            let direct = validate_direct_msl_against_active_module(
                device.device(),
                &program,
                &profile,
                &runtime_info,
                &active,
            )
            .unwrap_or_else(|error| {
                panic!("direct {texture_type:?} MSL must compile with active ABI: {error}")
            });
            assert_eq!(direct.bindings(), active.bindings(), "{texture_type:?}");
        }
    }

    #[test]
    fn compiles_direct_depth_sample_dimensions_with_active_abi() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        for texture_type in [
            TextureType::Color2D,
            TextureType::Color2DRect,
            TextureType::ColorArray2D,
            TextureType::ColorCube,
            TextureType::ColorArrayCube,
        ] {
            let program = depth_sampled_texture_program(texture_type);
            let spirv = emit_spirv(&program, &profile, &runtime_info);
            let active = compile_native_shader(
                device.device(),
                device.profile(),
                &spirv,
                &MetalShaderCompileOptions::default(),
            )
            .unwrap_or_else(|error| {
                panic!("active depth {texture_type:?} SPIR-V/MSL must compile: {error}")
            });
            let direct = validate_direct_msl_against_active_module(
                device.device(),
                &program,
                &profile,
                &runtime_info,
                &active,
            )
            .unwrap_or_else(|error| {
                panic!("direct depth {texture_type:?} MSL must compile with active ABI: {error}")
            });
            assert_eq!(direct.bindings(), active.bindings(), "{texture_type:?}");
            assert!(direct.source().source.contains(".sample_compare("));
        }
    }

    #[test]
    fn compiles_direct_sample_operands_with_active_abi() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        for is_depth in [false, true] {
            let program = sampled_texture_operands_program(is_depth);
            let spirv = emit_spirv(&program, &profile, &runtime_info);
            let active = compile_native_shader(
                device.device(),
                device.profile(),
                &spirv,
                &MetalShaderCompileOptions::default(),
            )
            .unwrap_or_else(|error| {
                panic!("active depth={is_depth} sample operands must compile: {error}")
            });
            let direct = validate_direct_msl_against_active_module(
                device.device(),
                &program,
                &profile,
                &runtime_info,
                &active,
            )
            .unwrap_or_else(|error| {
                panic!("direct depth={is_depth} sample operands must compile: {error}")
            });

            assert_eq!(direct.bindings(), active.bindings());
            assert!(direct.source().source.contains("bias("));
            assert!(direct.source().source.contains("min_lod_clamp("));
            assert!(direct.source().source.contains("int2(-1, 2)"));
        }
    }

    #[test]
    fn compiles_direct_sample_operands_at_msl_2_3_baseline() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        for is_depth in [false, true] {
            let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
                &sampled_texture_operands_program(is_depth),
                &make_shader_profile(device.profile()),
                &RuntimeInfo::default(),
                &shader_recompiler::backend::msl::MslOptions {
                    language_version: shader_recompiler::backend::msl::MslVersion::V2_3,
                    fixed_subgroup_size: 32,
                    supports_query_texture_lod: device.profile().supports_query_texture_lod,
                    supports_read_write_textures: device.profile().supports_read_write_textures(),
                    supports_texture_atomics: false,
                    enable_point_size_builtin: true,
                    disable_rasterization: false,
                },
            )
            .unwrap_or_else(|error| {
                panic!("direct depth={is_depth} sample operands must lower at MSL 2.3: {error}")
            });
            let shader =
                compile_native_msl_artifact(device.device(), artifact).unwrap_or_else(|error| {
                    panic!(
                        "direct depth={is_depth} sample operands must compile at MSL 2.3: {error}"
                    )
                });
            assert_eq!(
                shader.language_version(),
                shader_recompiler::backend::msl::MslVersion::V2_3
            );
        }
    }

    #[test]
    fn compiles_direct_texture_fetch_dimensions_with_active_abi() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        for texture_type in [
            TextureType::Color1D,
            TextureType::ColorArray1D,
            TextureType::Color2D,
            TextureType::Color2DRect,
            TextureType::ColorArray2D,
            TextureType::Color3D,
            TextureType::ColorCube,
            TextureType::ColorArrayCube,
        ] {
            let program = fetched_texture_program(texture_type, false, false, false, false);
            let spirv = emit_spirv(&program, &profile, &runtime_info);
            let active = compile_native_shader(
                device.device(),
                device.profile(),
                &spirv,
                &MetalShaderCompileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("active {texture_type:?} fetch must compile: {error}"));
            let direct = validate_direct_msl_against_active_module(
                device.device(),
                &program,
                &profile,
                &runtime_info,
                &active,
            )
            .unwrap_or_else(|error| panic!("direct {texture_type:?} fetch must compile: {error}"));
            assert_eq!(direct.bindings(), active.bindings(), "{texture_type:?}");
            assert!(direct.source().source.contains(".read("));
        }
    }

    #[test]
    fn compiles_direct_integer_offset_and_multisample_fetches() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        for program in [
            fetched_texture_program(TextureType::Color2D, false, true, false, false),
            fetched_texture_program(TextureType::ColorArray2D, false, false, false, true),
            fetched_texture_program(TextureType::Color2D, true, false, false, false),
            fetched_texture_program(TextureType::ColorCube, true, false, false, false),
            fetched_texture_program(TextureType::Color2D, false, false, true, false),
            fetched_texture_program(TextureType::ColorArray2D, false, false, true, false),
            fetched_texture_program(TextureType::Color2D, true, false, true, false),
        ] {
            let spirv = emit_spirv(&program, &profile, &runtime_info);
            let active = compile_native_shader(
                device.device(),
                device.profile(),
                &spirv,
                &MetalShaderCompileOptions::default(),
            )
            .expect("active fetch variant must compile");
            let direct = validate_direct_msl_against_active_module(
                device.device(),
                &program,
                &profile,
                &runtime_info,
                &active,
            )
            .expect("direct fetch variant must compile with the active ABI");
            assert_eq!(direct.bindings(), active.bindings());
        }
    }

    #[test]
    fn compiles_direct_texture_gathers_with_active_abi() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let mut programs = Vec::new();
        for texture_type in [
            TextureType::Color2D,
            TextureType::Color2DRect,
            TextureType::ColorArray2D,
            TextureType::ColorCube,
            TextureType::ColorArrayCube,
        ] {
            programs.push(gathered_texture_program(
                texture_type,
                false,
                false,
                GatherOffset::None,
            ));
            programs.push(gathered_texture_program(
                texture_type,
                true,
                false,
                GatherOffset::None,
            ));
        }
        for texture_type in [
            TextureType::Color2D,
            TextureType::Color2DRect,
            TextureType::ColorArray2D,
        ] {
            for offset_kind in [GatherOffset::Single, GatherOffset::Ptp] {
                programs.push(gathered_texture_program(
                    texture_type,
                    false,
                    false,
                    offset_kind,
                ));
                programs.push(gathered_texture_program(
                    texture_type,
                    true,
                    false,
                    offset_kind,
                ));
            }
        }
        for offset_kind in [GatherOffset::None, GatherOffset::Single, GatherOffset::Ptp] {
            programs.push(gathered_texture_program(
                TextureType::Color2D,
                false,
                true,
                offset_kind,
            ));
        }

        for program in programs {
            let spirv = emit_spirv(&program, &profile, &runtime_info);
            let active = compile_native_shader(
                device.device(),
                device.profile(),
                &spirv,
                &MetalShaderCompileOptions::default(),
            )
            .expect("active gather SPIR-V/MSL must compile");
            let direct = validate_direct_msl_against_active_module(
                device.device(),
                &program,
                &profile,
                &runtime_info,
                &active,
            )
            .expect("direct gather MSL must compile with the active ABI");
            assert_eq!(direct.bindings(), active.bindings());
            assert!(direct.source().source.contains(".gather"));
        }
    }

    #[test]
    fn direct_ptp_gather_uses_four_gathers_and_the_metal_w_lane() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let profile = make_shader_profile(device.profile());
        let program =
            gathered_texture_program(TextureType::Color2D, false, false, GatherOffset::Ptp);
        let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
            &program,
            &profile,
            &RuntimeInfo::default(),
            &shader_recompiler::backend::msl::MslOptions {
                language_version: device.profile().msl_language_version,
                fixed_subgroup_size: 32,
                supports_query_texture_lod: device.profile().supports_query_texture_lod,
                supports_read_write_textures: device.profile().supports_read_write_textures(),
                supports_texture_atomics: device.profile().supports_texture_atomics(),
                enable_point_size_builtin: true,
                disable_rasterization: false,
            },
        )
        .expect("PTP gather must lower directly to MSL");
        assert_eq!(artifact.source.source.matches(".gather(").count(), 4);
        assert_eq!(artifact.source.source.matches(").w").count(), 4);
        compile_native_msl_artifact(device.device(), artifact)
            .expect("direct PTP gather MSL must compile natively");
    }

    #[test]
    fn compiles_direct_storage_images_with_active_abi() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let runtime_profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let typeless_read = storage_image_program(TextureType::Color2D, 1, false, true, false);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &emit_spirv(&typeless_read, &runtime_profile, &runtime_info),
            &MetalShaderCompileOptions::for_device(device.profile()),
        )
        .expect("active unsupported typeless load must compile to zero");
        let direct = validate_direct_msl_against_active_module(
            device.device(),
            &typeless_read,
            &runtime_profile,
            &runtime_info,
            &active,
        )
        .expect("direct unsupported typeless load must compile to zero");
        assert_eq!(direct.bindings(), active.bindings());
        assert!(direct.source().source.contains("= uint4(0u);"));
        assert!(!direct.source().source.contains(".read("));

        let mut profile = runtime_profile;
        // Exercise the float load conversion as well as the integer path.
        // Runtime Metal profiles keep typeless loads disabled and therefore
        // follow upstream's explicit zero-result path.
        profile.support_typeless_image_loads = true;
        let mut programs = Vec::new();
        for texture_type in [
            TextureType::Color1D,
            TextureType::ColorArray1D,
            TextureType::Color2D,
            TextureType::ColorArray2D,
            TextureType::Color3D,
        ] {
            programs.push(storage_image_program(texture_type, 1, true, true, false));
            programs.push(storage_image_program(texture_type, 1, false, false, true));
        }
        if device.profile().supports_read_write_textures() {
            programs.push(storage_image_program(
                TextureType::Color2D,
                1,
                true,
                true,
                true,
            ));
        }

        for program in programs {
            let spirv = emit_spirv(&program, &profile, &runtime_info);
            let active = compile_native_shader(
                device.device(),
                device.profile(),
                &spirv,
                &MetalShaderCompileOptions::for_device(device.profile()),
            )
            .expect("active storage-image SPIR-V/MSL must compile");
            let direct = validate_direct_msl_against_active_module(
                device.device(),
                &program,
                &profile,
                &runtime_info,
                &active,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "direct storage-image MSL must compile: {error}\nactive MSL:\n{}",
                    active.source().source,
                )
            });
            assert_eq!(direct.bindings(), active.bindings());
            assert_eq!(
                direct.bindings().resources[0].kind,
                MetalResourceKind::StorageImage
            );
        }

        // Eden's current SPIR-V storage-image declaration does not preserve
        // descriptor-array count in reflection. Validate the direct ABI and
        // native MSL independently so the native backend does not inherit
        // that limitation.
        let array = storage_image_program(TextureType::Color2D, 2, true, true, false);
        let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
            &array,
            &profile,
            &runtime_info,
            &shader_recompiler::backend::msl::MslOptions {
                language_version: device.profile().msl_language_version,
                fixed_subgroup_size: 32,
                supports_query_texture_lod: device.profile().supports_query_texture_lod,
                supports_read_write_textures: device.profile().supports_read_write_textures(),
                supports_texture_atomics: device.profile().supports_texture_atomics(),
                enable_point_size_builtin: true,
                disable_rasterization: false,
            },
        )
        .expect("direct storage-image descriptor array must lower");
        assert_eq!(artifact.bindings.resources[0].count.unwrap().get(), 2);
        compile_native_msl_artifact(device.device(), artifact)
            .expect("direct storage-image descriptor array must compile natively");
    }

    #[test]
    fn compiles_direct_texture_and_image_buffers_with_active_abi() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let mut programs = vec![
            (texture_buffer_program(), MetalResourceKind::SeparateImage),
            (
                image_buffer_program(true, false),
                MetalResourceKind::StorageImage,
            ),
            (
                image_buffer_program(false, true),
                MetalResourceKind::StorageImage,
            ),
        ];
        if device.profile().supports_read_write_textures() {
            programs.push((
                image_buffer_program(true, true),
                MetalResourceKind::StorageImage,
            ));
        }

        for (program, expected_kind) in programs {
            let spirv = emit_spirv(&program, &profile, &runtime_info);
            let active = compile_native_shader(
                device.device(),
                device.profile(),
                &spirv,
                &MetalShaderCompileOptions::for_device(device.profile()),
            )
            .expect("active buffer-image SPIR-V/MSL must compile");
            let direct = validate_direct_msl_against_active_module(
                device.device(),
                &program,
                &profile,
                &runtime_info,
                &active,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "direct buffer-image MSL must compile: {error}\nactive MSL:\n{}",
                    active.source().source,
                )
            });

            assert_eq!(direct.bindings(), active.bindings());
            assert_eq!(direct.bindings().resources.len(), 1);
            assert_eq!(direct.bindings().resources[0].kind, expected_kind);
        }

        // Eden carries image-buffer array counts through shader metadata.
        // SPIR-V reflection does not preserve that count reliably, so prove
        // the native declaration and ABI independently.
        let mut array = image_buffer_program(true, false);
        array.info.image_buffer_descriptors[0].count = 2;
        let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
            &array,
            &profile,
            &runtime_info,
            &shader_recompiler::backend::msl::MslOptions {
                language_version: device.profile().msl_language_version,
                fixed_subgroup_size: 32,
                supports_query_texture_lod: device.profile().supports_query_texture_lod,
                supports_read_write_textures: device.profile().supports_read_write_textures(),
                supports_texture_atomics: device.profile().supports_texture_atomics(),
                enable_point_size_builtin: true,
                disable_rasterization: false,
            },
        )
        .expect("direct image-buffer descriptor array must lower");
        assert_eq!(artifact.bindings.resources[0].count.unwrap().get(), 2);
        compile_native_msl_artifact(device.device(), artifact)
            .expect("direct image-buffer descriptor array must compile natively");
    }

    #[test]
    fn compiles_direct_texture_atomics_with_active_abi() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        if !device.profile().supports_texture_atomics() {
            return;
        }
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let program = storage_image_atomic_program();
        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::for_device(device.profile()),
        )
        .expect("active texture-atomic SPIR-V/MSL must compile");
        let direct = validate_direct_msl_against_active_module(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &active,
        )
        .unwrap_or_else(|error| {
            panic!(
                "direct texture-atomic MSL must compile: {error}\nactive MSL:\n{}",
                active.source().source,
            )
        });

        assert_eq!(direct.bindings(), active.bindings());
        assert!(direct.source().source.contains("atomic_fetch_add"));
        assert!(direct.source().source.contains("atomic_exchange"));
    }

    #[test]
    fn compiles_direct_image_buffer_atomics_with_active_abi() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        if !device.profile().supports_texture_atomics() {
            return;
        }
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let program = image_buffer_atomic_program();
        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::for_device(device.profile()),
        )
        .expect("active image-buffer atomic SPIR-V/MSL must compile");
        let direct = validate_direct_msl_against_active_module(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &active,
        )
        .unwrap_or_else(|error| {
            panic!(
                "direct image-buffer atomic MSL must compile: {error}\nactive MSL:\n{}",
                active.source().source,
            )
        });

        assert_eq!(direct.bindings(), active.bindings());
        assert_eq!(
            direct.bindings().resources[0].kind,
            MetalResourceKind::StorageImage
        );
        assert!(direct.source().source.contains("atomic_fetch_add"));
        assert!(direct.source().source.contains("atomic_exchange"));
    }

    #[test]
    fn compiles_direct_texture_dimension_queries_with_active_abi() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        for (texture_type, is_multisample) in [
            (TextureType::Color1D, false),
            (TextureType::ColorArray1D, false),
            (TextureType::Color2D, false),
            (TextureType::Color2DRect, false),
            (TextureType::ColorArray2D, false),
            (TextureType::Color3D, false),
            (TextureType::ColorCube, false),
            (TextureType::ColorArrayCube, false),
            (TextureType::Color2D, true),
            (TextureType::ColorArray2D, true),
        ] {
            let program = texture_query_program(texture_type, is_multisample);
            let spirv = emit_spirv(&program, &profile, &runtime_info);
            let active = compile_native_shader(
                device.device(),
                device.profile(),
                &spirv,
                &MetalShaderCompileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("active {texture_type:?} query must compile: {error}"));
            let direct = validate_direct_msl_against_active_module(
                device.device(),
                &program,
                &profile,
                &runtime_info,
                &active,
            )
            .unwrap_or_else(|error| panic!("direct {texture_type:?} query must compile: {error}"));
            assert_eq!(direct.bindings(), active.bindings(), "{texture_type:?}");
        }
    }

    #[test]
    fn compiles_direct_texture_lod_queries_with_active_abi() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        if !device.profile().supports_query_texture_lod {
            return;
        }
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        for texture_type in [
            TextureType::Color2D,
            TextureType::Color2DRect,
            TextureType::ColorArray2D,
            TextureType::Color3D,
            TextureType::ColorCube,
            TextureType::ColorArrayCube,
        ] {
            let program = texture_lod_query_program(texture_type);
            let spirv = emit_spirv(&program, &profile, &runtime_info);
            let active = compile_native_shader(
                device.device(),
                device.profile(),
                &spirv,
                &MetalShaderCompileOptions::for_device(device.profile()),
            )
            .unwrap_or_else(|error| {
                panic!("active {texture_type:?} LOD query must compile: {error}")
            });
            assert!(active.source().source.contains("calculate_clamped_lod"));
            assert!(active.source().source.contains("calculate_unclamped_lod"));
            let direct = validate_direct_msl_against_active_module(
                device.device(),
                &program,
                &profile,
                &runtime_info,
                &active,
            )
            .unwrap_or_else(|error| {
                panic!("direct {texture_type:?} LOD query must compile: {error}")
            });
            assert_eq!(direct.bindings(), active.bindings(), "{texture_type:?}");
            assert!(direct.source().source.contains(".calculate_clamped_lod("));
            assert!(direct.source().source.contains(".calculate_unclamped_lod("));
        }
    }

    #[test]
    fn compiles_direct_texture_gradients_with_active_abi() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        for texture_type in [
            TextureType::Color1D,
            TextureType::ColorArray1D,
            TextureType::Color2D,
            TextureType::Color2DRect,
            TextureType::ColorArray2D,
            TextureType::Color3D,
            TextureType::ColorCube,
            TextureType::ColorArrayCube,
        ] {
            let program = texture_gradient_program(texture_type, false, false);
            let spirv = emit_spirv(&program, &profile, &runtime_info);
            let active = compile_native_shader(
                device.device(),
                device.profile(),
                &spirv,
                &MetalShaderCompileOptions::for_device(device.profile()),
            )
            .unwrap_or_else(|error| {
                panic!("active {texture_type:?} gradient must compile: {error}")
            });
            let direct = validate_direct_msl_against_active_module(
                device.device(),
                &program,
                &profile,
                &runtime_info,
                &active,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "direct {texture_type:?} gradient must compile: {error}\nactive MSL:\n{}",
                    active.source().source
                )
            });
            assert_eq!(direct.bindings(), active.bindings(), "{texture_type:?}");
            if matches!(
                texture_type,
                TextureType::Color1D | TextureType::ColorArray1D
            ) {
                assert!(!direct.source().source.contains("gradient1d"));
            } else {
                assert!(direct.source().source.contains("gradient"));
            }
        }

        let program = texture_gradient_program(TextureType::Color2D, true, true);
        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::for_device(device.profile()),
        )
        .expect("active 2D offset/clamped gradient must compile");
        let direct = validate_direct_msl_against_active_module(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &active,
        )
        .expect("direct 2D offset/clamped gradient must compile");
        assert!(direct.source().source.contains("int2(-1, 2)"));
        assert!(direct.source().source.contains("min_lod_clamp("));
    }

    #[test]
    fn compiles_direct_multisample_fetch_at_msl_2_3_baseline() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let program = fetched_texture_program(TextureType::ColorArray2D, false, false, true, false);
        let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
            &program,
            &make_shader_profile(device.profile()),
            &RuntimeInfo::default(),
            &shader_recompiler::backend::msl::MslOptions {
                language_version: shader_recompiler::backend::msl::MslVersion::V2_3,
                fixed_subgroup_size: 32,
                supports_query_texture_lod: device.profile().supports_query_texture_lod,
                supports_read_write_textures: device.profile().supports_read_write_textures(),
                supports_texture_atomics: false,
                enable_point_size_builtin: true,
                disable_rasterization: false,
            },
        )
        .expect("multisample fetch must lower at the MSL 2.3 baseline");

        let shader = compile_native_msl_artifact(device.device(), artifact)
            .expect("multisample fetch must compile at the MSL 2.3 baseline");
        assert_eq!(
            shader.language_version(),
            shader_recompiler::backend::msl::MslVersion::V2_3
        );
    }

    #[test]
    fn compiles_and_validates_direct_constant_buffer_msl() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let mut program = empty_program(Stage::Fragment);
        program
            .info
            .constant_buffer_descriptors
            .push(ConstantBufferDescriptor { index: 3, count: 1 });
        program.info.uses_int8 = true;
        program.info.uses_int16 = true;
        program.info.used_constant_buffer_types = shader_recompiler::ir::Type::U8 as u32
            | shader_recompiler::ir::Type::U16 as u32
            | shader_recompiler::ir::Type::U32 as u32
            | shader_recompiler::ir::Type::F32 as u32
            | shader_recompiler::ir::Type::U32x2 as u32;
        program.blocks[0]
            .append_new_inst(Opcode::GetCbufU8, vec![Value::ImmU32(3), Value::ImmU32(5)]);
        program.blocks[0]
            .append_new_inst(Opcode::GetCbufS16, vec![Value::ImmU32(3), Value::ImmU32(6)]);
        program.blocks[0].append_new_inst(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(3), Value::ImmU32(20)],
        );
        program.blocks[0].append_new_inst(
            Opcode::GetCbufF32,
            vec![Value::ImmU32(3), Value::ImmU32(24)],
        );
        program.blocks[0].append_new_inst(
            Opcode::GetCbufU32x2,
            vec![Value::ImmU32(3), Value::ImmU32(8)],
        );
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::for_device(device.profile()),
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

        assert_eq!(direct.bindings(), active.bindings());
        assert_eq!(direct.bindings().resources.len(), 1);
        assert_eq!(
            direct.bindings().resources[0].kind,
            MetalResourceKind::UniformBuffer
        );
        assert!(direct
            .source()
            .source
            .contains("constant uint4* c3 [[buffer(0)]]"));
    }

    #[test]
    fn compiles_and_validates_direct_indirect_constant_buffer_msl() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let mut program = empty_program(Stage::Compute);
        program.info.uses_cbuf_indirect = true;
        program.info.uses_int8 = true;
        program.info.uses_int16 = true;
        program.info.used_indirect_cbuf_types = Type::U8 as u32
            | Type::U16 as u32
            | Type::U32 as u32
            | Type::F32 as u32
            | Type::U32x2 as u32;
        for index in 0..shader_recompiler::shader_info::Info::MAX_INDIRECT_CBUFS as u32 {
            program
                .info
                .constant_buffer_descriptors
                .push(ConstantBufferDescriptor { index, count: 1 });
            program.info.constant_buffer_mask |= 1 << index;
            program.info.constant_buffer_used_sizes[index as usize] = 0x1_0000;
        }
        let binding = program.blocks[0]
            .append_new_inst(Opcode::IAdd32, vec![Value::ImmU32(5), Value::ImmU32(2)]);
        program.blocks[0].append_new_inst(
            Opcode::GetCbufU32,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: binding,
                }),
                Value::ImmU32(20),
            ],
        );
        let dynamic_binding = Value::Inst(InstRef {
            block: 0,
            inst: binding,
        });
        program.blocks[0].append_new_inst(
            Opcode::GetCbufU8,
            vec![dynamic_binding.clone(), Value::ImmU32(5)],
        );
        program.blocks[0].append_new_inst(
            Opcode::GetCbufS16,
            vec![dynamic_binding.clone(), Value::ImmU32(6)],
        );
        program.blocks[0].append_new_inst(
            Opcode::GetCbufF32,
            vec![dynamic_binding.clone(), Value::ImmU32(24)],
        );
        program.blocks[0].append_new_inst(
            Opcode::GetCbufU32x2,
            vec![dynamic_binding, Value::ImmU32(8)],
        );
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::for_compute_device(
                device.profile(),
                program.workgroup_size,
            ),
        )
        .expect("active indirect CBUF SPIR-V/MSL must compile");

        let direct = validate_direct_msl_against_active_module(
            device.device(),
            &program,
            &profile,
            &runtime_info,
            &active,
        )
        .expect("direct indirect CBUF MSL must compile with the active ABI");

        assert_eq!(direct.bindings(), active.bindings());
        assert_eq!(
            direct.bindings().resources.len(),
            shader_recompiler::shader_info::Info::MAX_INDIRECT_CBUFS
        );
        assert!(direct
            .source()
            .source
            .contains("inline uint4 spvLoadConstU32x4("));
    }

    #[test]
    fn validates_direct_constant_buffer_bindings_across_graphics_stages() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let mut vertex = empty_program(Stage::VertexB);
        vertex
            .info
            .constant_buffer_descriptors
            .push(ConstantBufferDescriptor { index: 0, count: 1 });
        let mut fragment = empty_program(Stage::Fragment);
        fragment
            .info
            .constant_buffer_descriptors
            .push(ConstantBufferDescriptor { index: 1, count: 1 });
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let options = MetalShaderCompileOptions::for_device(device.profile());
        let mut spirv_bindings = Bindings::default();
        let mut direct_bindings = Bindings::default();

        for (expected_binding, program) in [vertex, fragment].iter().enumerate() {
            let spirv = shader_recompiler::backend::emit_spirv_with_bindings(
                program,
                &profile,
                &runtime_info,
                &mut spirv_bindings,
            );
            let active =
                compile_native_shader(device.device(), device.profile(), &spirv, &options).unwrap();
            let direct = validate_direct_msl_against_active_module_with_bindings(
                device.device(),
                program,
                &profile,
                &runtime_info,
                &active,
                &mut direct_bindings,
            )
            .unwrap();

            assert_eq!(direct.bindings(), active.bindings());
            assert_eq!(
                direct.bindings().resources[0].binding,
                expected_binding as u32
            );
        }
        assert_eq!(spirv_bindings.unified, 2);
        assert_eq!(direct_bindings.unified, 2);
    }

    #[test]
    fn compiles_direct_graphics_stages_with_shared_bindings_without_spirv() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let mut vertex = empty_program(Stage::VertexB);
        vertex
            .info
            .constant_buffer_descriptors
            .push(ConstantBufferDescriptor { index: 0, count: 1 });
        let mut fragment = empty_program(Stage::Fragment);
        fragment
            .info
            .constant_buffer_descriptors
            .push(ConstantBufferDescriptor { index: 1, count: 1 });
        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let options = MetalShaderCompileOptions::for_device(device.profile());
        let mut bindings = Bindings::default();

        for (expected_binding, program) in [vertex, fragment].iter().enumerate() {
            let direct = compile_direct_msl_shader_with_bindings(
                device.device(),
                program,
                &profile,
                &runtime_info,
                &options,
                &mut bindings,
            )
            .expect("direct graphics MSL must compile without a SPIR-V module");

            assert_eq!(direct.source().stage, program.stage);
            assert_eq!(direct.bindings().resources.len(), 1);
            assert_eq!(
                direct.bindings().resources[0].binding,
                expected_binding as u32
            );
        }
        assert_eq!(bindings.unified, 2);
    }

    #[test]
    fn compiles_and_validates_direct_storage_buffer_msl() {
        let Ok(device) = MetalDevice::new() else {
            return;
        };
        let mut program = empty_program(Stage::Compute);
        program
            .info
            .constant_buffer_descriptors
            .push(ConstantBufferDescriptor { index: 0, count: 1 });
        program
            .info
            .storage_buffers_descriptors
            .push(StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 2,
                is_written: true,
            });
        program.info.uses_int8 = true;
        program.info.uses_int16 = true;
        program.info.used_storage_buffer_types = shader_recompiler::ir::Type::U8 as u32
            | shader_recompiler::ir::Type::U16 as u32
            | shader_recompiler::ir::Type::U32 as u32
            | shader_recompiler::ir::Type::U32x2 as u32
            | shader_recompiler::ir::Type::U32x4 as u32;
        program.blocks[0].append_new_inst(
            Opcode::LoadStorageU8,
            vec![Value::ImmU32(1), Value::ImmU32(1)],
        );
        let load64 = program.blocks[0].append_new_inst(
            Opcode::LoadStorage64,
            vec![Value::ImmU32(0), Value::ImmU32(8)],
        );
        program.blocks[0].append_new_inst(
            Opcode::WriteStorage32,
            vec![Value::ImmU32(0), Value::ImmU32(4), Value::ImmU32(0x1234)],
        );
        program.blocks[0].append_new_inst(
            Opcode::WriteStorage64,
            vec![
                Value::ImmU32(0),
                Value::ImmU32(16),
                Value::Inst(InstRef {
                    block: 0,
                    inst: load64,
                }),
            ],
        );

        let profile = make_shader_profile(device.profile());
        let runtime_info = RuntimeInfo::default();
        let spirv = emit_spirv(&program, &profile, &runtime_info);
        let active = compile_native_shader(
            device.device(),
            device.profile(),
            &spirv,
            &MetalShaderCompileOptions::for_compute_device(
                device.profile(),
                program.workgroup_size,
            ),
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

        assert_eq!(direct.bindings(), active.bindings());
        assert_eq!(direct.bindings().resources.len(), 2);
        assert_eq!(
            direct.bindings().resources[1].kind,
            MetalResourceKind::StorageBuffer
        );
        assert!(direct
            .source()
            .source
            .contains("device uint* ssbo0 [[buffer(1)]]"));

        let mut subword_program = empty_program(Stage::Compute);
        subword_program
            .info
            .storage_buffers_descriptors
            .push(StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: true,
            });
        subword_program.info.uses_int16 = true;
        subword_program.info.used_storage_buffer_types = shader_recompiler::ir::Type::U16 as u32;
        subword_program.blocks[0].append_new_inst(
            Opcode::WriteStorageU16,
            vec![Value::ImmU32(0), Value::ImmU32(2), Value::ImmU32(0x1234)],
        );
        let artifact = shader_recompiler::backend::msl::emit_msl_with_options(
            &subword_program,
            &profile,
            &runtime_info,
            &shader_recompiler::backend::msl::MslOptions {
                language_version: device.profile().msl_language_version,
                fixed_subgroup_size: 32,
                supports_query_texture_lod: device.profile().supports_query_texture_lod,
                supports_read_write_textures: device.profile().supports_read_write_textures(),
                supports_texture_atomics: device.profile().supports_texture_atomics(),
                enable_point_size_builtin: true,
                disable_rasterization: false,
            },
        )
        .unwrap();
        assert!(artifact
            .source
            .source
            .contains("atomic_compare_exchange_weak_explicit"));
        compile_native_msl_artifact(device.device(), artifact).unwrap();
    }
}

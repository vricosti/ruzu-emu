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
    pub geometry_provoking_vertex_last: bool,
    pub compute_workgroup_size: Option<[u32; 3]>,
}

impl Default for MetalShaderCompileOptions {
    fn default() -> Self {
        Self {
            language_version: MslVersion::V2_3,
            // Validation-only SPIRV-Cross option. Native MSL selects its sampler
            // argument-buffer ABI from the descriptor count, independently.
            argument_buffers: false,
            fixed_subgroup_size: 32,
            enable_frag_depth_builtin: true,
            enable_frag_stencil_ref_builtin: true,
            enable_frag_output_mask: u32::MAX,
            enable_point_size_builtin: true,
            disable_rasterization: false,
            geometry_provoking_vertex_last: false,
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
        "Metal {namespace} binding limit exceeded: requested {requested}, limit {limit}"
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
            interface: None,
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
    validate_native_binding_layout(&MetalDeviceProfile::query(device), &artifact.bindings)?;
    let language_version = artifact.language_version;
    let execution = artifact.execution;
    let library = compile_msl_library(device, &artifact.source.source, artifact.language_version)?;
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

pub(crate) fn validate_native_binding_layout(
    profile: &MetalDeviceProfile,
    layout: &MetalShaderBindingLayout,
) -> Result<(), MetalShaderError> {
    let sampler_limit = if layout.sampler_argument_buffer_index.is_some() {
        if profile.supports_argument_buffer_tier2() {
            profile.max_argument_buffer_samplers_per_stage
        } else {
            profile.max_sampler_bindings_per_stage
        }
    } else {
        profile.max_sampler_bindings_per_stage
    };
    for (namespace, requested, limit) in [
        ("buffer", layout.buffer_count, profile.max_buffer_bindings_per_stage),
        ("texture", layout.texture_count, profile.max_texture_bindings_per_stage),
        ("sampler", layout.sampler_count, sampler_limit),
    ] {
        if requested > limit {
            return Err(MetalShaderError::ResourceLimit { namespace, requested, limit });
        }
    }
    Ok(())
}

pub(crate) fn compile_msl_library(
    device: &ProtocolObject<dyn MTLDevice>,
    source: &str,
    version: MslVersion,
) -> Result<Retained<ProtocolObject<dyn MTLLibrary>>, MetalShaderError> {
    let compile_options = MTLCompileOptions::new();
    compile_options.setLanguageVersion(metal_language_version(version)?);
    if objc2::available!(macos = 15.0, ..) {
        compile_options.setMathMode(MTLMathMode::Safe);
    } else {
        #[allow(deprecated)]
        compile_options.setFastMathEnabled(false);
    }
    let source_string = NSString::from_str(source);
    device
        .newLibraryWithSource_options_error(&source_string, Some(&compile_options))
        .map_err(|error| MetalShaderError::LibraryCompile(error.localizedDescription().to_string()))
}

pub(crate) fn direct_msl_options(
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
        geometry_provoking_vertex_last: options.geometry_provoking_vertex_last,
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

    #[test]
    fn native_mesh_shader_prerequisite_compiles_into_pipeline() {
        use objc2_metal::{MTLMeshRenderPipelineDescriptor, MTLPipelineOption};

        let device = MetalDevice::new().expect("Metal device");
        if !device.profile().supports_mesh_shaders() {
            eprintln!("Mesh shaders unavailable on {}", device.name());
            return;
        }
        let source = NSString::from_str(
            "#include <metal_stdlib>\n\
             using namespace metal;\n\
             struct Vertex { float4 position [[position]]; };\n\
             using Output = metal::mesh<Vertex, void, 3, 1, topology::triangle>;\n\
             [[mesh]] void geometry_probe(Output mesh_output) {\n\
                 mesh_output.set_vertex(0, Vertex{float4(-1, -1, 0, 1)});\n\
                 mesh_output.set_vertex(1, Vertex{float4(1, -1, 0, 1)});\n\
                 mesh_output.set_vertex(2, Vertex{float4(0, 1, 0, 1)});\n\
                 mesh_output.set_index(0, 0);\n\
                 mesh_output.set_index(1, 1);\n\
                 mesh_output.set_index(2, 2);\n\
                 mesh_output.set_primitive_count(1);\n\
             }\n",
        );
        let options = MTLCompileOptions::new();
        options.setLanguageVersion(
            metal_language_version(device.profile().msl_language_version).unwrap(),
        );
        let library = device
            .device()
            .newLibraryWithSource_options_error(&source, Some(&options))
            .expect("compile native mesh shader");
        let function = library
            .newFunctionWithName(&NSString::from_str("geometry_probe"))
            .expect("mesh function");
        let descriptor = MTLMeshRenderPipelineDescriptor::new();
        // SAFETY: this function belongs to the same device and the descriptor
        // is local to this test; neither is mutated concurrently.
        unsafe { descriptor.setMeshFunction(Some(&function)) };
        descriptor.setMaxTotalThreadsPerMeshThreadgroup(1);
        descriptor.setRasterizationEnabled(false);
        device
            .device()
            .newRenderPipelineStateWithMeshDescriptor_options_reflection_error(
                &descriptor,
                MTLPipelineOption::empty(),
                None,
            )
            .expect("create native mesh pipeline");
    }

    #[test]
    fn callable_geometry_captures_outputs_and_executes_side_effects_once() {
        use crate::renderer_metal::{metal_buffer::MetalBuffer, metal_scheduler::MetalScheduler};
        use objc2_metal::{MTLComputeCommandEncoder as _, MTLSize};
        use shader_recompiler::backend::msl::emit_msl::emit_msl_geometry_function;
        use shader_recompiler::backend::msl::MslOptions;
        use shader_recompiler::ir::{value::Attribute, SyntaxNode};
        use shader_recompiler::ir_opt::collect_shader_info_pass::collect_shader_info_pass;
        let device = MetalDevice::new().unwrap();
        let mut program = Program::new(Stage::Geometry);
        program.output_topology = shader_recompiler::ir::types::OutputTopology::LineStrip;
        program.output_vertices = 6;
        program.add_block();
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        let mut ir = Emitter::new(&mut program, 0);
        ir.prologue();
        for vertex in 0..6 {
            if vertex == 3 { ir.end_primitive(Value::ImmU32(0)); }
            ir.set_attribute(Attribute::POSITION_X, Value::ImmF32(vertex as f32), Value::ImmU32(0));
            ir.set_attribute(Attribute::LAYER,
                Value::ImmF32(f32::from_bits(if vertex < 3 { 1 } else { 2 })), Value::ImmU32(0));
            ir.emit_vertex(Value::ImmU32(0));
        }
        ir.epilogue();
        program.blocks[0].append_new_inst(Opcode::StorageAtomicIAdd32,
            vec![Value::ImmU32(0), Value::ImmU32(0), Value::ImmU32(1)]);
        collect_shader_info_pass(&mut program);
        program.info.storage_buffers_descriptors.push(StorageBufferDescriptor {
            cbuf_index: 0, cbuf_offset: 0, count: 1, is_written: true,
        });
        let options = MslOptions { language_version: MslVersion::V3_0, ..Default::default() };
        let artifact = emit_msl_geometry_function(&program, &Profile::default(),
            &RuntimeInfo::default(), &options, &mut Bindings::default()).unwrap();
        assert_eq!(artifact.bindings.buffer_count, 1);
        // This transport is test-owned. It snapshots the same mesh interface
        // into thread storage and exports scalar results, with no host struct ABI.
        let source = format!("{}\n{}", artifact.source.source, r#"
struct Capture {
    MslVertexOut vertices[6];
    uint indices[10];
    MslGeometryPrimitiveOut primitives[5];
    uint count;
    void set_vertex(uint i, MslVertexOut v) thread { vertices[i] = v; }
    void set_index(uint i, uchar v) thread { indices[i] = v; }
    void set_primitive(uint i, MslGeometryPrimitiveOut p) thread { primitives[i] = p; }
    void set_primitive_count(uint n) thread { count = n; }
};
kernel void capture(device uint* counter [[buffer(0)]], device uint* result [[buffer(1)]]) {
    MslGeometryPayload input = {};
    Capture output = {};
    ruzu_geometry(counter, input, uint3(0), output);
    result[0] = output.count;
    for (uint i = 0; i < output.count; ++i) result[1 + i] = output.primitives[i].layer;
    for (uint i = 0; i < output.count * 2; ++i) result[5 + i] = output.indices[i];
    for (uint i = 0; i < 6; ++i) result[13 + i] = as_type<uint>(output.vertices[i].position.x);
}
"#);
        let library = compile_msl_library(device.device(), &source, MslVersion::V3_0).unwrap();
        let function = library.newFunctionWithName(&NSString::from_str("capture")).unwrap();
        let pipeline = device.device().newComputePipelineStateWithFunction_error(&function).unwrap();
        let counter = MetalBuffer::new(&device, 4).unwrap();
        counter.write(0, &[0; 4]).unwrap();
        let result = MetalBuffer::new(&device, 19 * 4).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        scheduler.with_compute_encoder(|encoder| unsafe {
            encoder.setComputePipelineState(&pipeline);
            encoder.setBuffer_offset_atIndex(Some(counter.handle()), 0, 0);
            encoder.setBuffer_offset_atIndex(Some(result.handle()), 0, 1);
            let one = MTLSize { width: 1, height: 1, depth: 1 };
            encoder.dispatchThreads_threadsPerThreadgroup(one, one);
        }).unwrap();
        scheduler.finish_all().unwrap();
        let mut count = [0; 4];
        counter.read(0, &mut count).unwrap();
        assert_eq!(u32::from_ne_bytes(count), 1, "one guest execution, not one per output primitive");
        let mut bytes = [0; 19 * 4];
        result.read(0, &mut bytes).unwrap();
        let words: Vec<_> = bytes.chunks_exact(4).map(|b| u32::from_ne_bytes(b.try_into().unwrap())).collect();
        assert_eq!(&words[..5], &[4, 1, 1, 2, 2]);
        assert_eq!(&words[5..13], &[0, 1, 1, 2, 3, 4, 4, 5]);
        assert_eq!(&words[13..], &(0..6).map(|v| (v as f32).to_bits()).collect::<Vec<_>>());
    }

    #[test]
    fn direct_geometry_mesh_rasterizes_captured_outputs_and_primitive_cuts() {
        use crate::renderer_metal::{
            metal_buffer::MetalBuffer,
            metal_compute_pass::ConditionalRenderingArgumentsPass,
            metal_staging_buffer_pool::MetalStagingBufferPool,
            metal_geometry_pipeline::{
                bind_vertex_resources, MetalGeometryShaderStages, MetalGeometryShader,
            },
            metal_graphics_pipeline::{MetalPreparedStage, MetalStageBufferBinding},
            metal_pipeline_cache::{
                metal_rasterization_enabled, MetalPipelineCache, MetalRenderPipelineKey, MetalVertexAttributeState,
                MetalVertexBufferLayoutState, MetalVertexInputState,
            },
            metal_primitive_assembler::{MetalPrimitiveAssembler, MetalPrimitiveAssemblyParams},
            metal_scheduler::MetalScheduler,
        };
        use objc2_metal::{
            MTLClearColor, MTLComputeCommandEncoder as _, MTLLoadAction, MTLPixelFormat,
            MTLRenderCommandEncoder as _, MTLStoreAction,
        };
        use crate::renderer_metal::{metal_image::MetalImage, metal_image_view::MetalImageView,
            metal_framebuffer::MetalFramebuffer};
        use crate::surface::PixelFormat;
        use crate::texture_cache::{image_info::ImageInfo, image_view_base::ImageViewBase,
            image_view_info::ImageViewInfo, render_targets::RenderTargets};
        use crate::texture_cache::types::{BufferImageCopy, Extent2D, Extent3D,
            ImageType, ImageViewType, SubresourceExtent, SubresourceRange, NUM_RT};
        use shader_recompiler::backend::msl::{
            emit_msl::{emit_msl_vertex_function, emit_msl_with_options},
            emit_msl_geometry::GeometryLayout,
            MslOptions,
        };
        use shader_recompiler::ir::{value::Attribute, SyntaxNode};
        use shader_recompiler::ir_opt::collect_shader_info_pass::collect_shader_info_pass;
        use shader_recompiler::runtime_info::InputTopology;

        let device = MetalDevice::new().expect("Metal device");
        if !device.profile().supports_mesh_shaders() {
            eprintln!("Mesh shaders unavailable on {}", device.name());
            return;
        }
        let mut vertex_program = Program::new(Stage::VertexB);
        vertex_program.add_block();
        vertex_program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        let mut ir = Emitter::new(&mut vertex_program, 0);
        ir.prologue();
        for (component, position) in [
            Attribute::POSITION_X,
            Attribute::POSITION_Y,
            Attribute::POSITION_Z,
            Attribute::POSITION_W,
        ]
        .into_iter()
        .enumerate()
        {
            let mut value =
                ir.get_attribute(Attribute::generic(0, component as u32), Value::ImmU32(0));
            if component == 0 {
                let translation = ir.get_cbuf_f32(Value::ImmU32(1), Value::ImmU32(0));
                ir.set_attribute(Attribute::generic(1, 0), translation, Value::ImmU32(0));
                value = ir.fp_add_32(value, translation);
            }
            ir.set_attribute(position, value, Value::ImmU32(0));
        }
        ir.epilogue();
        collect_shader_info_pass(&mut vertex_program);
        let mut vertex_runtime = RuntimeInfo::default();
        vertex_runtime.generic_input_types[0] = AttributeType::Float;
        vertex_runtime
            .previous_stage_stores
            .set(Attribute::generic(0, 0).0 as usize, true);
        let msl_options = MslOptions {
            language_version: MslVersion::V3_0,
            ..Default::default()
        };
        let vertex_artifact = emit_msl_vertex_function(
            &vertex_program,
            &Profile::default(),
            &vertex_runtime,
            &msl_options,
            &mut Bindings::default(),
        )
        .unwrap();
        let vertex_buffer_index = vertex_artifact.bindings.buffer_count as u8;
        let mut vertex_layout = MetalVertexInputState::default();
        vertex_layout.attributes[0] = MetalVertexAttributeState {
            format: objc2_metal::MTLVertexFormat::Float4,
            offset: 0,
            buffer_index: vertex_buffer_index,
        };
        vertex_layout.layouts[0] = MetalVertexBufferLayoutState {
            stride: 16,
            step_function: objc2_metal::MTLVertexStepFunction::PerVertex,
            step_rate: 1,
            buffer_index: vertex_buffer_index,
            enabled: true,
        };
        let geometry_runtime = RuntimeInfo {
            input_topology: InputTopology::Triangles,
            previous_stage_stores: vertex_program.info.stores,
            ..Default::default()
        };
        let mut pipeline_cache = MetalPipelineCache::new(device.clone());
        let assembler = MetalPrimitiveAssembler::new(&device).unwrap();
        let index_buffer = MetalBuffer::new(&device, 10).unwrap();
        index_buffer
            .write(
                0,
                &[u16::MAX, 9, 10, 11, u16::MAX]
                    .into_iter()
                    .flat_map(u16::to_ne_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let vertex_data = MetalBuffer::new(&device, 48).unwrap();
        let vertex_values = [
            -0.65f32, -0.8, 0.0, 1.0, 0.15, -0.8, 0.0, 1.0, -0.25, 0.8, 0.0, 1.0,
        ];
        vertex_data
            .write(
                0,
                &vertex_values
                    .into_iter()
                    .flat_map(f32::to_ne_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let cbuf = std::sync::Arc::new(MetalBuffer::new(&device, 16).unwrap());
        cbuf.write(
            0,
            &[0.25f32, 0.0, 0.0, 0.0]
                .into_iter()
                .flat_map(f32::to_ne_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let mut smooth_reference = [None, None];
        use shader_recompiler::ir::types::OutputTopology::{PointList as Point, LineStrip as Line, TriangleStrip as Tri};
        let conditional_arguments = ConditionalRenderingArgumentsPass::new(&device).unwrap();
        for (((count, cull_both, flat_strip, provoking_last, topology, smooth_strip, layered, ordered), viewport_mode), condition) in [
            (0u32, false, false, false, Tri, false, false, false), (2, false, false, false, Tri, false, false, false),
            (3, false, false, false, Tri, false, false, false), (4, false, false, false, Tri, false, false, false),
            (6, false, false, false, Tri, false, false, false), (6, true, false, false, Tri, false, false, false),
            (4, false, true, false, Tri, false, false, false), (4, false, true, true, Tri, false, false, false),
            (3, false, true, false, Line, false, false, false), (3, false, true, true, Line, false, false, false),
            (4, false, true, false, Tri, true, false, false), (4, false, true, true, Tri, true, false, false),
            (3, false, true, false, Line, true, false, false), (3, false, true, true, Line, true, false, false),
            (6, false, false, false, Tri, false, true, false), (6, false, false, true, Tri, false, true, false),
            (6, false, false, false, Line, false, true, false), (6, false, false, true, Line, false, true, false),
            (0, false, false, false, Tri, false, true, false),
            (1, false, false, false, Tri, false, true, false),
            (2, false, false, false, Tri, false, true, false),
            (4, false, false, false, Tri, false, true, false),
            (6, true, false, false, Tri, false, true, false),
            (0, false, false, false, Line, false, true, false),
            (1, false, false, false, Line, false, true, false),
            (2, false, false, false, Line, false, true, false),
            (4, false, false, false, Line, false, true, false),
            (5, false, false, false, Line, false, true, false),
            (6, false, false, false, Tri, false, true, true),
            (6, false, false, true, Tri, false, true, true),
            (0, false, false, false, Point, false, true, false),
            (1, false, false, false, Point, false, true, false),
            (3, false, false, false, Point, false, true, false),
            (4, false, false, false, Point, false, true, false),
            (6, false, false, false, Point, false, true, false),
        ].into_iter().flat_map(|case| {
            // 0: Layer only; 1: independent Layer + ViewportIndex;
            // 2: ViewportIndex without Layer (the capture record has one field).
            [Some((case, 0u8)), case.6.then_some((case, 1)), case.6.then_some((case, 2))]
                .into_iter().flatten()
        }).flat_map(|case| [None, Some((0u32, false)), Some((1, false)), Some((0, true)), Some((1, true))]
            .into_iter().map(move |condition| (case, condition))) {
            let writes_layer = layered && viewport_mode != 2;
            let writes_viewport = viewport_mode != 0;
            let geometry_profile = Profile {
                support_multi_viewport: device.profile().max_viewports() > 1,
                ..Default::default()
            };
            let line_strip = topology == Line;
            let points = topology == Point;
            let mut program = Program::new(Stage::Geometry);
            program.output_vertices = 6;
            program.invocations = if ordered { 2 } else { 1 };
            program.output_topology = topology;
            program.add_block();
            program.syntax_list = vec![SyntaxNode::Block(0)];
            Emitter::new(&mut program, 0).prologue();
            let mut condition_block = 0;
            for vertex in 0..6 {
                let body = program.add_block();
                let merge = program.add_block();
                let mut ir = Emitter::new(&mut program, condition_block);
                let count_value = ir.get_cbuf_u32(Value::ImmU32(2), Value::ImmU32(0));
                let condition = ir.u_less_than(Value::ImmU32(vertex), count_value);
                let condition = ir.condition_ref(condition);
                program.syntax_list.push(SyntaxNode::If {
                    cond: condition,
                    body,
                    merge,
                });
                program.syntax_list.push(SyntaxNode::Block(body));
                let mut ir = Emitter::new(&mut program, body);
                if vertex == 3 && !flat_strip {
                    ir.end_primitive(Value::ImmU32(0));
                }
                for (component, attribute) in [
                    Attribute::POSITION_X,
                    Attribute::POSITION_Y,
                    Attribute::POSITION_Z,
                    Attribute::POSITION_W,
                ]
                .into_iter()
                .enumerate()
                {
                    let mut value = ir.get_attribute(attribute, Value::ImmU32(vertex % 3));
                    if component == 0 {
                        let offset =
                            ir.get_attribute(Attribute::generic(1, 0), Value::ImmU32(vertex % 3));
                        let offset = ir
                            .fp_mul_32(offset, Value::ImmF32(if vertex < 3 || ordered { -2.0 } else { 2.0 }));
                        value = ir.fp_add_32(value, offset);
                    }
                    if flat_strip {
                        // Two triangles in one strip, with different colors at
                        // every vertex: winding alone cannot validate flat data.
                        let position = [
                            [-0.8, -0.8, 0.0, 1.0], [-0.8, 0.8, 0.0, 1.0],
                            [0.8, -0.8, 0.0, 1.0], [0.8, 0.8, 0.0, 1.0],
                        ];
                        value = Value::ImmF32(position[(vertex % 4) as usize][component]);
                    }
                    ir.set_attribute(attribute, value, Value::ImmU32(0));
                    let color = if flat_strip {
                        [
                            [1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0],
                            [0.0, 0.0, 1.0, 1.0], [1.0, 1.0, 0.0, 1.0],
                        ][(vertex % 4) as usize][component]
                    } else if component == 3
                        || (vertex < 3 && component == 0)
                        || (vertex >= 3 && component == 2)
                    {
                        1.0
                    } else {
                        0.0
                    };
                    ir.set_attribute(
                        Attribute::generic(0, component as u32),
                        Value::ImmF32(color),
                        Value::ImmU32(0),
                    );
                }
                if writes_layer {
                    ir.set_attribute(Attribute::LAYER,
                        Value::ImmF32(f32::from_bits(if vertex < 3 || ordered { 1 } else { 2 })),
                        Value::ImmU32(0));
                }
                if writes_viewport {
                    let viewport = if vertex < 3 || ordered { 1 } else { 2 };
                    // Opposite to Layer in the combined case: aliasing these
                    // two output fields must not accidentally pass the oracle.
                    let viewport = if writes_layer { 3 - viewport } else { viewport };
                    ir.set_attribute(Attribute::VIEWPORT_INDEX,
                        Value::ImmF32(f32::from_bits(viewport)),
                        Value::ImmU32(0));
                }
                ir.emit_vertex(Value::ImmU32(0));
                program.syntax_list.push(SyntaxNode::EndIf { merge });
                program.syntax_list.push(SyntaxNode::Block(merge));
                condition_block = merge;
            }
            Emitter::new(&mut program, condition_block).epilogue();
            program.blocks[condition_block as usize].append_new_inst(
                Opcode::StorageAtomicIAdd32,
                vec![Value::ImmU32(0), Value::ImmU32(0), Value::ImmU32(1)],
            );
            let mut ir = Emitter::new(&mut program, condition_block);
            let invocation = ir.invocation_id();
            let invocation_weight = ir.iadd_32(invocation, Value::ImmU32(1));
            program.blocks[condition_block as usize].append_new_inst(
                Opcode::StorageAtomicIAdd32,
                vec![Value::ImmU32(0), Value::ImmU32(4), invocation_weight],
            );
            program.syntax_list.push(SyntaxNode::Return);
            collect_shader_info_pass(&mut program);
            program.info.storage_buffers_descriptors.push(StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: true,
            });
            let mut runtime = geometry_runtime.clone();
            if points { runtime.fixed_state_point_size = Some(4.0); }
            let geometry_options = direct_msl_options(device.device(), &MetalShaderCompileOptions {
                language_version: MslVersion::V3_0,
                enable_point_size_builtin: points,
                geometry_provoking_vertex_last: provoking_last,
                ..Default::default()
            });
            assert_eq!(geometry_options.geometry_provoking_vertex_last, provoking_last);
            let artifact = emit_msl_with_options(
                &program,
                &geometry_profile,
                &runtime,
                &geometry_options,
            )
            .unwrap();
            let fragment_input = if flat_strip && !smooth_strip {
                "struct FragmentIn { float4 color [[user(locn0), flat]]; };"
            } else {
                "struct FragmentIn { float4 color [[user(locn0)]]; };"
            };
            let fragment_function = if writes_layer {
                "fragment float4 geometry_fragment(FragmentIn input [[stage_in]], uint layer [[render_target_array_index]]) { return float4(input.color.rgb, float(layer) / 2.0f); }"
            } else {
                "fragment float4 geometry_fragment(FragmentIn input [[stage_in]]) { return input.color; }"
            };
            let source = format!("{}\n{fragment_input}\n{fragment_function}\n", artifact.source.source);
            let library = compile_msl_library(
                device.device(),
                &source,
                geometry_options.language_version,
            )
                .unwrap_or_else(|error| panic!("{error}\n{source}"));
            let geometry_layout = GeometryLayout::new(&program, &runtime, &geometry_options).unwrap();
            let mesh = library
                .newFunctionWithName(&NSString::from_str("main0"))
                .unwrap();
            let fragment = library
                .newFunctionWithName(&NSString::from_str("geometry_fragment"))
                .unwrap();
            let mesh_module = MetalShaderModule {
                source: artifact.source.clone(),
                bindings: artifact.bindings,
                execution: artifact.execution,
                language_version: artifact.language_version,
                library: library.clone(),
                function: mesh,
            };
            let fragment_module = MetalShaderModule {
                source: MetalShaderSource {
                    source,
                    stage: Stage::Fragment,
                },
                bindings: Default::default(),
                execution: Default::default(),
                language_version: MslVersion::V3_0,
                library,
                function: fragment,
            };
            let stages = MetalGeometryShaderStages {
                vertex: vertex_artifact.clone(),
                shader: if layered {
                    MetalGeometryShader::Capture(shader_recompiler::backend::msl::emit_msl::emit_msl_geometry_function(
                        &program, &geometry_profile, &runtime, &geometry_options, &mut Bindings::default()).unwrap())
                } else {
                    MetalGeometryShader::Mesh(std::sync::Arc::new(mesh_module))
                },
                runtime,
                layout: geometry_layout,
                invocations: program.invocations,
            };
            let mut key = MetalRenderPipelineKey::new(1, 2);
            key.shader_variant_hash = u64::from(flat_strip)
                | (u64::from(provoking_last) << 1)
                | (u64::from(line_strip) << 2)
                | (u64::from(smooth_strip) << 3)
                | (u64::from(layered) << 4)
                | (u64::from(ordered) << 5)
                | (u64::from(points) << 6)
                | (u64::from(viewport_mode) << 7);
            key.vertex_input = vertex_layout;
            key.color_attachments[0].format = MTLPixelFormat::RGBA8Unorm;
            if ordered {
                key.color_attachments[0].blending_enabled = true;
                key.color_attachments[0].source_rgb = objc2_metal::MTLBlendFactor::SourceAlpha;
                key.color_attachments[0].destination_rgb = objc2_metal::MTLBlendFactor::OneMinusSourceAlpha;
            }
            let mut fixed = crate::renderer_vulkan::fixed_pipeline_state::FixedPipelineState::default();
            fixed.dynamic_state.set_rasterize_enable(true);
            fixed.dynamic_state.set_cull_enable(cull_both);
            fixed.dynamic_state.set_cull_face(crate::engines::maxwell_3d::CullFace::FrontAndBack);
            key.rasterization_enabled = metal_rasterization_enabled(&fixed, Some(stages.layout.topology));
            let pipeline = pipeline_cache
                .get_or_create_geometry_pipeline(key, &stages, Some(&fragment_module))
                .unwrap();
            let hit = pipeline_cache
                .get_or_create_geometry_pipeline(key, &stages, Some(&fragment_module))
                .unwrap();
            assert!(std::sync::Arc::ptr_eq(&pipeline, &hit));
            let layers = if writes_layer { 3 } else { 1 };
            let image_info = ImageInfo {
                format: PixelFormat::A8B8G8R8Unorm,
                image_type: ImageType::E2D,
                size: Extent3D { width: 32, height: 32, depth: 1 },
                resources: SubresourceExtent { levels: 1, layers },
                ..Default::default()
            };
            let image = MetalImage::new(&device, &image_info).unwrap();
            let view_info = ImageViewInfo::for_render_target(
                ImageViewType::E2DArray, image_info.format,
                SubresourceRange { extent: image_info.resources, ..Default::default() });
            let mut view_base = Box::new(ImageViewBase::new(&view_info, &image_info,
                common::slot_vector::SlotId { index: 1 }, 0x1000));
            let view = MetalImageView::new(std::ptr::NonNull::from(view_base.as_mut()), &view_info, &image).unwrap();
            let mut attachments = [None; NUM_RT];
            attachments[0] = Some(&view);
            let framebuffer = MetalFramebuffer::new(attachments, None,
                &RenderTargets { size: Extent2D { width: 32, height: 32 }, ..Default::default() }).unwrap();
            let pass = framebuffer.render_pass_descriptor();
            assert_eq!(pass.renderTargetArrayLength(), layers as usize);
            let attachment = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
            attachment.setLoadAction(MTLLoadAction::Clear);
            attachment.setStoreAction(MTLStoreAction::Store);
            attachment.setClearColor(MTLClearColor {
                red: 0.0,
                green: 0.0,
                blue: 0.0,
                alpha: 1.0,
            });
            let mut scheduler = MetalScheduler::new(&device);
            let assembly = assembler
                .record(
                    &mut scheduler,
                    MetalPrimitiveAssemblyParams {
                        topology: crate::engines::maxwell_3d::PrimitiveTopology::Triangles,
                        count: 5,
                        base_vertex: -9,
                        instances: if ordered { 2 } else { 1 },
                        index_bytes: 2,
                        restart_index: Some(u16::MAX as u32),
                    },
                    Some((&index_buffer, 0)),
                )
                .unwrap();
            let vertex_resources = MetalPreparedStage {
                buffers: vec![MetalStageBufferBinding {
                    index: 0,
                    buffer: cbuf.clone(),
                    offset: 0,
                }],
                ..Default::default()
            };
            let count_buffer = std::sync::Arc::new(MetalBuffer::new(&device, 16).unwrap());
            count_buffer
                .write(0, bytemuck::cast_slice(&[count, 0, 0, 0]))
                .unwrap();
            let geometry_counter = std::sync::Arc::new(MetalBuffer::new(&device, 8).unwrap());
            geometry_counter.write(0, &[0; 8]).unwrap();
            let geometry_resources = MetalPreparedStage {
                buffers: vec![
                    MetalStageBufferBinding { index: 0, buffer: count_buffer, offset: 0 },
                    MetalStageBufferBinding { index: 1, buffer: geometry_counter.clone(), offset: 0 },
                ],
                ..Default::default()
            };
            let mut vertex_sizes = [0u64; 31];
            vertex_sizes[vertex_buffer_index as usize] = vertex_data.length() as u64;
            let bind = |encoder: &objc2::runtime::ProtocolObject<dyn objc2_metal::MTLComputeCommandEncoder>| unsafe {
                bind_vertex_resources(encoder, &vertex_resources);
                encoder.setBuffer_offset_atIndex(
                    Some(vertex_data.handle()), 0, vertex_buffer_index as usize,
                );
            };
            let mut staging_pool = MetalStagingBufferPool::new(&device).unwrap();
            let original_arguments = assembly.dispatch_arguments.clone();
            let (assembly, vertices) = if let Some((value, inverted)) = condition {
                let upload = MetalBuffer::new(&device, 4).unwrap();
                upload.write(0, &value.to_ne_bytes()).unwrap();
                let predicate = MetalBuffer::new_private(&device, 8).unwrap();
                upload.encode_copy(&mut scheduler, &predicate, 0, 4, 4).unwrap();
                pipeline.record_conditional_inputs(
                    &mut scheduler, &mut staging_pool, &conditional_arguments, &predicate, 4,
                    inverted, &assembly, 0, &vertex_sizes, bind,
                ).unwrap()
            } else {
                let vertices = pipeline.vertex.record(&mut scheduler, &assembly, 0, &vertex_sizes, bind).unwrap();
                (assembly, vertices)
            };
            let captured = pipeline.capture_output(&mut scheduler, &assembly, &vertices, &geometry_resources).unwrap();
            scheduler.begin_render_pass(&pass).unwrap();
            scheduler
                .with_render_encoder(|encoder| {
                    encoder.setRenderPipelineState(&pipeline.state);
                    if writes_viewport {
                        let mut viewports = [objc2_metal::MTLViewport {
                            originX: 0.0, originY: 0.0, width: 32.0, height: 32.0, znear: 0.0, zfar: 1.0,
                        }; 3];
                        let mut scissors = [
                            objc2_metal::MTLScissorRect { x: 0, y: 0, width: 0, height: 0 },
                            objc2_metal::MTLScissorRect { x: 0, y: 0, width: 16, height: 32 },
                            objc2_metal::MTLScissorRect { x: 16, y: 0, width: 16, height: 32 },
                        ];
                        if writes_layer { scissors.swap(1, 2); }
                        unsafe {
                            encoder.setViewports_count(std::ptr::NonNull::from(&mut viewports[0]), 3);
                            encoder.setScissorRects_count(std::ptr::NonNull::from(&mut scissors[0]), 3);
                        }
                    }
                    if flat_strip {
                        encoder.setFrontFacingWinding(objc2_metal::MTLWinding::Clockwise);
                        encoder.setCullMode(objc2_metal::MTLCullMode::Back);
                    }
                    pipeline
                        .record_draw(encoder, &assembly, &vertices, &geometry_resources, captured.as_ref())
                        .unwrap();
                })
                .unwrap();
            let readback = MetalBuffer::new(&device, 32 * 32 * 4 * layers as usize).unwrap();
            let mut copy = BufferImageCopy {
                buffer_size: readback.length(),
                image_extent: image_info.size,
                ..Default::default()
            };
            copy.image_subresource.num_layers = layers;
            image.download_memory(&mut scheduler, &readback, 0, &[copy]).unwrap();
            let original_readback = MetalBuffer::new(&device, 12).unwrap();
            original_arguments.encode_copy(&mut scheduler, &original_readback, 0, 0, 12).unwrap();
            // Test-only synchronization makes the CPU readback observable.
            scheduler.finish_all().unwrap();
            let mut counter = [0; 8];
            geometry_counter.read(0, &mut counter).unwrap();
            let mut original_bytes = [0; 12];
            original_readback.read(0, &mut original_bytes).unwrap();
            assert_eq!(original_bytes.chunks_exact(4).map(|b| u32::from_ne_bytes(b.try_into().unwrap())).collect::<Vec<_>>(),
                [1, if ordered { 2 } else { 1 }, 1], "conditional draw must not modify reusable assembly");
            if condition.is_some_and(|(value, inverted)| (value != 0) == inverted) {
                assert_eq!(counter, [0; 8], "disabled geometry must not execute storage atomics");
                let mut all_pixels = vec![0; readback.length()];
                readback.read(0, &mut all_pixels).unwrap();
                assert!(all_pixels.chunks_exact(4).all(|p| p == [0, 0, 0, 255]),
                    "disabled geometry must not rasterize any layer");
                continue;
            }
            assert_eq!(u32::from_ne_bytes(counter[..4].try_into().unwrap()), if ordered { 4 } else { 1 },
                "one geometry execution per input primitive, invocation and instance, even when culled");
            assert_eq!(u32::from_ne_bytes(counter[4..].try_into().unwrap()), if ordered { 6 } else { 1 },
                "InvocationId must distinguish invocations within each instance");
            let mut pixels = [0u8; 32 * 32 * 4];
            readback.read(0, &mut pixels).unwrap();
            if writes_viewport && !writes_layer {
                let row = if line_strip || points { 28 } else { 16 };
                let pixel = |x: usize| &pixels[(row * 32 + x) * 4..(row * 32 + x) * 4 + 4];
                if ordered {
                    // Without Layer the fragment alpha remains 1, so the last
                    // primitive overwrites earlier colors instead of blending.
                    assert_eq!(pixel(8), &[0, 0, 255, 255]);
                } else {
                    let minimum = if points { 1 } else if line_strip { 2 } else { 3 };
                    assert_eq!(pixel(if points { 2 } else { 8 }),
                        if count >= minimum && !cull_both { &[255, 0, 0, 255] } else { &[0, 0, 0, 255] });
                    assert_eq!(pixel(if points { 18 } else { 24 }),
                        if count >= 3 + minimum && !cull_both { &[0, 0, 255, 255] } else { &[0, 0, 0, 255] });
                }
                continue;
            }
            if writes_layer {
                assert!(pixels.chunks_exact(4).all(|p| p == [0, 0, 0, 255]),
                    "neither primitive targets layer zero");
                let mut layer_pixels = [0; 32 * 32 * 4];
                for layer in [1, 2] {
                    readback.read(layer * layer_pixels.len(), &mut layer_pixels).unwrap();
                    let row = if line_strip || points { 28 } else { 16 };
                    let pixel = |x: usize| &layer_pixels[(row * 32 + x) * 4..(row * 32 + x) * 4 + 4];
                    if ordered {
                        if layer == 1 {
                            // Four ordered red/blue pairs, alpha=1/2. Reversing
                            // the primitives swaps the dominant channel.
                            for (&actual, expected) in pixel(8).iter().zip([85u8, 0, 170, 128]) {
                                assert!(actual.abs_diff(expected) <= 1, "ordered blends: {:?}", pixel(8));
                            }
                            assert_eq!(pixel(24), &[0, 0, 0, 255]);
                        } else {
                            assert!(layer_pixels.chunks_exact(4).all(|p| p == [0, 0, 0, 255]));
                        }
                        continue;
                    }
                    let minimum = if points { 1 } else if line_strip { 2 } else { 3 };
                    assert_eq!(pixel(if points { 2 } else { 8 }), if layer == 1 && count >= minimum && !cull_both { &[255, 0, 0, 128] } else { &[0, 0, 0, 255] },
                        "left pixel layer={layer} lines={line_strip} last={provoking_last}");
                    assert_eq!(pixel(if points { 18 } else { 24 }), if layer == 2 && count >= 3 + minimum && !cull_both { &[0, 0, 255, 255] } else { &[0, 0, 0, 255] },
                        "right pixel layer={layer} lines={line_strip} last={provoking_last}");
                    if !points {
                        assert_eq!(pixel(16), &[0, 0, 0, 255], "cut must not connect layer primitives");
                    }
                }
                continue;
            }
            let pixel_at = |x: usize, y: usize| &pixels[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4];
            let pixel = |x| pixel_at(x, 16);
            if smooth_strip {
                if line_strip {
                    assert!(pixel_at(3, 16)[..2].iter().all(|&component| component > 0));
                } else {
                    assert!(pixel(8)[..3].iter().all(|&component| component > 0));
                }
                if let Some(reference) = &smooth_reference[usize::from(line_strip)] {
                    assert_eq!(&pixels, reference, "provoking mode must not alter smooth interpolation");
                } else {
                    smooth_reference[usize::from(line_strip)] = Some(pixels);
                }
                continue;
            }
            assert_eq!(
                if line_strip { pixel(3) } else { pixel(8) },
                if line_strip && provoking_last {
                    &[0, 255, 0, 255]
                } else if flat_strip && provoking_last {
                    &[0, 0, 255, 255]
                } else if count >= 3 && !cull_both {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 0, 255]
                },
                "first primitive, count={count}, line_strip={line_strip}, last={provoking_last}"
            );
            assert_eq!(
                if line_strip { pixel_at(24, 24) } else { pixel(24) },
                if line_strip && provoking_last {
                    &[0, 0, 255, 255]
                } else if flat_strip && provoking_last {
                    &[255, 255, 0, 255]
                } else if flat_strip {
                    &[0, 255, 0, 255]
                } else if count >= 6 && !cull_both {
                    &[0, 0, 255, 255]
                } else {
                    &[0, 0, 0, 255]
                },
                "second primitive, count={count}, line_strip={line_strip}, last={provoking_last}"
            );
            if !flat_strip {
                assert_eq!(
                    pixel(16),
                    &[0, 0, 0, 255],
                    "cut must not join separate strips, count={count}"
                );
            }
        }
    }

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

    #[test]
    fn direct_msl_loop_value_remains_visible_after_loop_exit() {
        let device = MetalDevice::new().unwrap();
        let mut program = structured_loop_program();
        let value = program.blocks[1]
            .append_new_inst(Opcode::IAdd32, vec![Value::ImmU32(19), Value::ImmU32(23)]);
        program.blocks[3].append_new_inst(
            Opcode::Identity,
            vec![Value::Inst(InstRef {
                block: 1,
                inst: value,
            })],
        );
        let artifact = shader_recompiler::backend::msl::emit_msl(
            &program,
            &make_shader_profile(device.profile()),
            &RuntimeInfo::default(),
        )
        .unwrap();
        compile_native_msl_artifact(device.device(), artifact).unwrap();
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

    #[test]
    fn direct_msl_sampler_arguments_preserve_seventeen_distinct_native_lod_states() {
        use std::ptr::NonNull;
        use super::super::{metal_buffer::MetalBuffer, metal_scheduler::MetalScheduler,
            metal_update_descriptor::MetalSamplerArgumentBuffer};
        use objc2_metal::{MTLBlitCommandEncoder, MTLRenderCommandEncoder, MTLTexture,
            MTLClearColor, MTLLoadAction, MTLOrigin, MTLPixelFormat, MTLPrimitiveType,
            MTLRegion, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor, MTLSamplerDescriptor,
            MTLSamplerMinMagFilter, MTLSamplerMipFilter, MTLSize, MTLStorageMode,
            MTLStoreAction, MTLTextureDescriptor, MTLTextureUsage, MTLViewport};
        let device = MetalDevice::new().unwrap();
        if !device.profile().supports_argument_buffer_tier2() { return; }
        let profile = make_shader_profile(device.profile());
        let td = unsafe { MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            MTLPixelFormat::RGBA8Unorm, 2, 2, true,
        ) };
        td.setStorageMode(MTLStorageMode::Shared);
        td.setUsage(MTLTextureUsage::ShaderRead);
        let texture = device.device().newTextureWithDescriptor(&td).unwrap();
        for (level, size, bytes) in [(0, 2, [255u8, 0, 0, 255].repeat(4)), (1, 1, vec![0, 255, 0, 255])] {
            unsafe { texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                MTLRegion { origin: MTLOrigin { x: 0, y: 0, z: 0 }, size: MTLSize { width: size, height: size, depth: 1 } },
                level, NonNull::new(bytes.as_ptr().cast_mut()).unwrap().cast(), size * 4,
            ) };
        }
        let samplers = (0..17).map(|index| {
            let descriptor = MTLSamplerDescriptor::new();
            descriptor.setSupportArgumentBuffers(true);
            descriptor.setMinFilter(MTLSamplerMinMagFilter::Nearest);
            descriptor.setMagFilter(MTLSamplerMinMagFilter::Nearest);
            descriptor.setMipFilter(MTLSamplerMipFilter::Linear);
            descriptor.setLodMinClamp(index as f32 / 16.0);
            device.device().newSamplerStateWithDescriptor(&descriptor).unwrap()
        }).collect::<Vec<_>>();
        let target_desc = unsafe { MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            MTLPixelFormat::RGBA8Unorm, 17, 1, false,
        ) };
        target_desc.setUsage(MTLTextureUsage::RenderTarget);
        let target = device.device().newTextureWithDescriptor(&target_desc).unwrap();
        let pass = MTLRenderPassDescriptor::renderPassDescriptor();
        let color = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        color.setTexture(Some(&target));
        color.setLoadAction(MTLLoadAction::Clear);
        color.setStoreAction(MTLStoreAction::Store);
        color.setClearColor(MTLClearColor { red: 0., green: 0., blue: 0., alpha: 0. });
        for array in [false, true] {
            let mut program = Program::new(Stage::Fragment);
            program.blocks.push(Block::new());
            let mut descriptor = sampled_texture_program(17, TextureType::Color2D).info.texture_descriptors.remove(0);
            descriptor.count = if array { 17 } else { 1 };
            program.info.texture_descriptors = vec![descriptor; if array { 1 } else { 17 }];
            program.info.constant_buffer_descriptors.push(ConstantBufferDescriptor { index: 0, count: 1 });
            let value = |inst| Value::Inst(InstRef { block: 0, inst });
            let selector = program.blocks[0].append_new_inst(Opcode::GetCbufU32, vec![Value::ImmU32(0), Value::ImmU32(0)]);
            let coords = sample_coordinates(&mut program, TextureType::Color2D);
            let mut selected = vec![Value::ImmF32(0.); 4];
            for index in 0..if array { 1 } else { 17 } {
                let sample = program.blocks[0].append_new_inst(Opcode::ImageSampleExplicitLod,
                    vec![if array { value(selector) } else { Value::ImmU32(0) }, coords.clone(), Value::ImmF32(0.), Value::Void]);
                program.blocks[0].inst_mut(sample).flags = TextureInstInfo {
                    descriptor_index: index as u16, texture_type: TextureType::Color2D as u8, ..Default::default()
                }.to_u32();
                let condition = program.blocks[0].append_new_inst(Opcode::IEqual, vec![value(selector), Value::ImmU32(index)]);
                for component in 0..4 {
                    let extracted = program.blocks[0].append_new_inst(Opcode::CompositeExtractF32x4,
                        vec![value(sample), Value::ImmU32(component)]);
                    selected[component as usize] = if array { value(extracted) } else {
                        value(program.blocks[0].append_new_inst(Opcode::SelectF32,
                            vec![value(condition), value(extracted), selected[component as usize].clone()]))
                    };
                }
            }
            let result = program.blocks[0].append_new_inst(Opcode::CompositeConstructF32x4, selected);
            store_sample_result(&mut program, result, true);
            let mut artifact = shader_recompiler::backend::msl::emit_msl_with_options(
                &program, &profile, &RuntimeInfo::default(),
                &direct_msl_options(device.device(), &MetalShaderCompileOptions::for_device(device.profile())),
            ).unwrap();
            assert_eq!(artifact.bindings.sampler_argument_buffer_index, Some(1));
            assert!(!artifact.source.source.contains("[[sampler("));
            artifact.source.source.push_str(r#"
vertex float4 argument_test_vertex(uint id [[vertex_id]]) {
    return float4(id == 1u ? 3.0 : -1.0, id == 2u ? 3.0 : -1.0, 0.0, 1.0);
}
"#);
            let module = compile_native_msl_artifact(device.device(), artifact).unwrap();
            let arguments = MetalSamplerArgumentBuffer::new(&device, module.bindings(),
                samplers.iter().enumerate().map(|(i, sampler)| (i as u32, sampler))).unwrap().unwrap();
            let descriptor = MTLRenderPipelineDescriptor::new();
            descriptor.setVertexFunction(Some(&module.library().newFunctionWithName(&NSString::from_str("argument_test_vertex")).unwrap()));
            descriptor.setFragmentFunction(Some(module.function()));
            unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) }.setPixelFormat(MTLPixelFormat::RGBA8Unorm);
            let pipeline = device.device().newRenderPipelineStateWithDescriptor_error(&descriptor).unwrap();
            let mut scheduler = MetalScheduler::new(&device);
            scheduler.retain_sampler_states(samplers.iter().cloned()).unwrap();
            scheduler.begin_render_pass(&pass).unwrap();
            scheduler.with_render_encoder(|encoder| unsafe {
                encoder.setRenderPipelineState(&pipeline);
                encoder.setFragmentBuffer_offset_atIndex(Some(arguments.buffer.handle()), 0, arguments.index as usize);
                for index in 0..17 { encoder.setFragmentTexture_atIndex(Some(&texture), index); }
                for index in 0..17u32 {
                    let selector = [index, 0, 0, 0];
                    encoder.setFragmentBytes_length_atIndex(NonNull::from(&selector).cast(), 16, 0);
                    encoder.setViewport(MTLViewport { originX: index as f64, originY: 0., width: 1., height: 1., znear: 0., zfar: 1. });
                    encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
                }
            }).unwrap();
            drop(arguments);
            let download = MetalBuffer::new(&device, 256).unwrap();
            scheduler.with_blit_encoder(|encoder| unsafe {
                encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                    &target, 0, 0, MTLOrigin { x: 0, y: 0, z: 0 }, MTLSize { width: 17, height: 1, depth: 1 },
                    download.handle(), 0, 256, 256,
                );
            }).unwrap();
            scheduler.finish_all().unwrap();
            let mut pixels = [0; 256];
            download.read(0, &mut pixels).unwrap();
            for index in 0..17 {
                let green = (255. * index as f32 / 16.).round() as i16;
                for (component, expected) in [255 - green, green, 0, 255].into_iter().enumerate() {
                    assert!((pixels[index*4+component] as i16 - expected).abs() <= 2,
                        "array={array} sampler={index} actual={:?}", &pixels[index*4..index*4+4]);
                }
            }
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
                geometry_provoking_vertex_last: false,
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
                geometry_provoking_vertex_last: false,
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
                geometry_provoking_vertex_last: false,
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
                geometry_provoking_vertex_last: false,
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
                geometry_provoking_vertex_last: false,
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
                geometry_provoking_vertex_last: false,
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
                geometry_provoking_vertex_last: false,
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
                geometry_provoking_vertex_last: false,
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
                geometry_provoking_vertex_last: false,
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
                geometry_provoking_vertex_last: false,
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
                    geometry_provoking_vertex_last: false,
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
                geometry_provoking_vertex_last: false,
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
                geometry_provoking_vertex_last: false,
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
                geometry_provoking_vertex_last: false,
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
                geometry_provoking_vertex_last: false,
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
                geometry_provoking_vertex_last: false,
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

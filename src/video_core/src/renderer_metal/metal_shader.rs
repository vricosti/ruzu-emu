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
    BindTarget, CompilerOptions, MetalPlatform, MslVersion, ResourceBinding,
};
use spirv_cross2::reflect::{ArrayDimension, Resource, TypeInner};
use spirv_cross2::targets::Msl;
use spirv_cross2::{Compiler, Module, SpirvCrossError};
use thiserror::Error;

use super::metal_device::MetalDeviceProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MetalResourceKind {
    UniformBuffer,
    StorageBuffer,
    StorageImage,
    SampledImage,
    SeparateImage,
    SeparateSampler,
}

/// Explicit mapping from one SPIR-V descriptor to Metal resource indices.
///
/// Metal has independent buffer, texture and sampler namespaces. Keeping all
/// three indices explicit avoids inheriting Vulkan descriptor-set semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetalResourceBinding {
    pub descriptor_set: u32,
    pub binding: u32,
    pub kind: MetalResourceKind,
    pub buffer_index: u32,
    pub texture_index: u32,
    pub sampler_index: u32,
    pub count: Option<NonZeroU32>,
}

/// Complete direct-binding ABI retained by a compiled Metal shader.
///
/// The runtime must consume these exact indices when encoding a draw or
/// dispatch. Metal's buffer, texture and sampler namespaces are independent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetalShaderBindingLayout {
    pub resources: Vec<MetalResourceBinding>,
    pub push_constant_buffer_index: Option<u32>,
    pub buffer_count: u32,
    pub texture_count: u32,
    pub sampler_count: u32,
}

#[derive(Debug, Clone)]
pub struct MetalShaderSource {
    pub source: String,
    pub execution_model: spirv_cross2::spirv::ExecutionModel,
}

/// Backend-neutral input to Apple's native MSL compiler.
///
/// Today this artifact is produced by SPIRV-Cross. A direct shader-recompiler
/// MSL backend can produce the same source/binding contract without changing
/// the Metal pipeline, rasterizer, or cache owners.
#[derive(Debug, Clone)]
pub struct MetalShaderArtifact {
    pub source: MetalShaderSource,
    pub bindings: MetalShaderBindingLayout,
    pub entry_point: String,
}

/// SPIRV-Cross policy for the baseline Apple7 renderer.
///
/// MSL 2.3 is available on the minimum supported Apple Silicon macOS release.
/// Later language features are enabled only after the device profile and the
/// native compiler version are advanced together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetalShaderCompileOptions {
    pub argument_buffers: bool,
    pub fixed_subgroup_size: u32,
    pub enable_frag_depth_builtin: bool,
    pub enable_frag_stencil_ref_builtin: bool,
    pub enable_frag_output_mask: u32,
}

impl Default for MetalShaderCompileOptions {
    fn default() -> Self {
        Self {
            // Direct bindings are the first complete runtime ABI. Enabling
            // argument buffers requires a matching CPU-side argument encoder.
            argument_buffers: false,
            fixed_subgroup_size: 32,
            enable_frag_depth_builtin: true,
            enable_frag_stencil_ref_builtin: true,
            enable_frag_output_mask: u32::MAX,
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
    #[error("Metal failed to compile MSL: {0}")]
    LibraryCompile(String),
    #[error("Metal library does not contain entry point {0}")]
    MissingEntryPoint(String),
}

/// Native shader objects retained for the lifetime of a Metal pipeline.
#[derive(Clone)]
pub struct MetalShaderModule {
    source: MetalShaderSource,
    bindings: MetalShaderBindingLayout,
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
) -> Result<MetalShaderSource, SpirvCrossError> {
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

    let options = make_compiler_options(metal_options);
    let artifact = compiler.compile(&options)?;
    Ok(MetalShaderSource {
        source: artifact.as_ref().to_owned(),
        execution_model,
    })
}

fn make_compiler_options(metal_options: &MetalShaderCompileOptions) -> CompilerOptions {
    let mut options = CompilerOptions::default();
    options.version = MslVersion::new(2, 3, 0);
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
        execution_model,
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
    let compile_options = MTLCompileOptions::new();
    compile_options.setLanguageVersion(MTLLanguageVersion::Version2_3);
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
        library,
        function,
    })
}

#[cfg(test)]
mod tests {
    use shader_recompiler::backend::emit_spirv;
    use shader_recompiler::ir::basic_block::Block;
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

        assert_eq!(
            shader.source().execution_model,
            spirv_cross2::spirv::ExecutionModel::Vertex
        );
        assert!(!shader.library().functionNames().is_empty());
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
}

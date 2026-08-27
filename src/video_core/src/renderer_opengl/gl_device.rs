// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's video_core/renderer_opengl/gl_device.h and gl_device.cpp
//! Queries OpenGL device capabilities and exposes them as boolean flags.

use common::settings_enums::RendererBackend;
use log::{info, warn};
use shader_recompiler::stage::Stage;
use std::ffi::CStr;

// TODO: Needs to explicitly enable ARB_TESSELLATION_SHADER for
// GL_MAX_TESS_CONTROL_UNIFORM_BLOCKS.
const LIMIT_UBOS: [u32; shader_recompiler::stage::MAX_STAGE_TYPES as usize] = [
    gl::MAX_VERTEX_UNIFORM_BLOCKS,
    gl::MAX_TESS_CONTROL_UNIFORM_BLOCKS,
    gl::MAX_TESS_EVALUATION_UNIFORM_BLOCKS,
    gl::MAX_GEOMETRY_UNIFORM_BLOCKS,
    gl::MAX_FRAGMENT_UNIFORM_BLOCKS,
    gl::MAX_COMPUTE_UNIFORM_BLOCKS,
];

/// OpenGL device capabilities, matching Eden's `Device` class.
pub struct Device {
    max_uniform_buffers: [u32; shader_recompiler::stage::MAX_STAGE_TYPES as usize],
    uniform_buffer_alignment: usize,
    shader_storage_alignment: usize,
    max_vertex_attributes: u32,
    max_varyings: u32,
    max_compute_shared_memory_size: u32,
    max_glasm_storage_buffer_blocks: u32,
    max_user_clip_distances: u32,

    has_warp_intrinsics: bool,
    has_shader_ballot: bool,
    has_vertex_viewport_layer: bool,
    has_image_load_formatted: bool,
    has_texture_shadow_lod: bool,
    has_vertex_buffer_unified_memory: bool,
    has_astc: bool,
    has_variable_aoffi: bool,
    has_component_indexing_bug: bool,
    has_precise_bug: bool,
    has_broken_texture_view_formats: bool,
    has_fast_buffer_sub_data: bool,
    has_nv_viewport_array2: bool,
    has_derivative_control: bool,
    has_debugging_tool_attached: bool,
    use_assembly_shaders: bool,
    use_asynchronous_shaders: bool,
    use_driver_cache: bool,
    has_depth_buffer_float: bool,
    has_geometry_shader_passthrough: bool,
    has_nv_gpu_shader_5: bool,
    has_shader_int64: bool,
    has_amd_shader_half_float: bool,
    has_sparse_texture_2: bool,
    has_draw_texture: bool,
    warp_size_potentially_larger_than_guest: bool,
    need_fastmath_off: bool,
    has_cbuf_ftou_bug: bool,
    has_bool_ref_bug: bool,
    can_report_memory: bool,
    strict_context_required: bool,
    supports_conditional_barriers: bool,
    has_lmem_perf_bug: bool,

    vendor_name: String,
}

impl Device {
    /// Create a new Device by querying GL state. Must be called with a current GL context.
    pub fn new(strict_context_required: bool) -> Result<Self, String> {
        let major_version = gl_get_integer(gl::MAJOR_VERSION);
        let minor_version = gl_get_integer(gl::MINOR_VERSION);
        if major_version < 4 || (major_version == 4 && minor_version < 6) {
            log::error!("OpenGL 4.6 is not available");
            return Err("OpenGL 4.6 is not available".to_string());
        }
        #[cfg(target_os = "haiku")]
        if !gl::CreateProgramPipelines::is_loaded() {
            log::error!(
                "You must compile Mesa +22 manually or use a different libGL.so (GLES is not supported)"
            );
            return Err("Outdated mesa".to_string());
        }

        let vendor_name = gl_string(gl::VENDOR);
        let gl_version = gl_string(gl::VERSION);
        let renderer_name = gl_string(gl::RENDERER);
        let extensions = get_extensions();

        // Match upstream's exact vendor predicates. In particular, Mesa
        // radeonsi reports `AMD`; upstream does not treat that as `IsAmd()`
        // for shader-profile policy such as gather subpixel offsets.
        let is_nvidia = vendor_name == "NVIDIA Corporation";
        let is_amd = vendor_name == "ATI Technologies Inc.";
        let is_intel = vendor_name == "Intel";

        let has_slow_software_astc =
            !is_nvidia && !is_amd && has_slow_software_astc(&vendor_name, &renderer_name);

        let disable_fast_buffer_sub_data = is_nvidia && gl_version == "4.6.0 NVIDIA 443.24";
        if disable_fast_buffer_sub_data {
            warn!("Beta driver 443.24 is known to have issues. There might be performance issues.");
        }

        let max_uniform_buffers = build_max_uniform_buffers();
        let uniform_buffer_alignment = gl_get_integer(gl::UNIFORM_BUFFER_OFFSET_ALIGNMENT) as usize;
        let shader_storage_alignment =
            gl_get_integer(gl::SHADER_STORAGE_BUFFER_OFFSET_ALIGNMENT) as usize;
        let max_vertex_attributes = gl_get_integer(gl::MAX_VERTEX_ATTRIBS) as u32;
        let max_varyings = gl_get_integer(gl::MAX_VARYING_VECTORS) as u32;
        let max_compute_shared_memory_size =
            gl_get_integer(gl::MAX_COMPUTE_SHARED_MEMORY_SIZE) as u32;
        let max_glasm_storage_buffer_blocks =
            gl_get_integer(gl::MAX_VERTEX_SHADER_STORAGE_BLOCKS) as u32;
        let max_user_clip_distances = gl_get_integer(gl::MAX_CLIP_DISTANCES) as u32;

        let has_warp_intrinsics = has_extension_in(&extensions, "GL_NV_gpu_shader5")
            && has_extension_in(&extensions, "GL_NV_shader_thread_group")
            && has_extension_in(&extensions, "GL_NV_shader_thread_shuffle");
        let has_shader_ballot = has_extension_in(&extensions, "GL_ARB_shader_ballot");
        let has_vertex_viewport_layer =
            has_extension_in(&extensions, "GL_ARB_shader_viewport_layer_array");
        let has_image_load_formatted =
            has_extension_in(&extensions, "GL_EXT_shader_image_load_formatted");
        let has_texture_shadow_lod = has_extension_in(&extensions, "GL_EXT_texture_shadow_lod");
        let has_astc = !has_slow_software_astc && is_astc_supported();
        let has_variable_aoffi = Self::test_variable_aoffi();
        let has_component_indexing_bug = false;
        let has_precise_bug = Self::test_precise_bug();
        let has_broken_texture_view_formats = cfg!(not(target_family = "unix")) && is_intel;
        let has_nv_viewport_array2 = has_extension_in(&extensions, "GL_NV_viewport_array2");
        let has_derivative_control = has_extension_in(&extensions, "GL_ARB_derivative_control");
        let has_vertex_buffer_unified_memory =
            has_extension_in(&extensions, "GL_NV_vertex_buffer_unified_memory");
        let has_debugging_tool_attached = is_debug_tool_attached(&extensions);
        let has_depth_buffer_float = has_extension_in(&extensions, "GL_NV_depth_buffer_float");
        let has_geometry_shader_passthrough =
            has_extension_in(&extensions, "GL_NV_geometry_shader_passthrough");
        let has_nv_gpu_shader_5 = has_extension_in(&extensions, "GL_NV_gpu_shader5");
        let has_shader_int64 = has_extension_in(&extensions, "GL_ARB_gpu_shader_int64");
        let has_amd_shader_half_float =
            has_extension_in(&extensions, "GL_AMD_gpu_shader_half_float");
        let has_sparse_texture_2 = has_extension_in(&extensions, "GL_ARB_sparse_texture2");
        let has_draw_texture = has_extension_in(&extensions, "GL_NV_draw_texture");
        let warp_size_potentially_larger_than_guest = !is_nvidia && !is_intel;
        let need_fastmath_off = is_nvidia;
        let can_report_memory = has_extension_in(&extensions, "GL_NVX_gpu_memory_info");
        let has_fast_buffer_sub_data = is_nvidia && !disable_fast_buffer_sub_data;

        let shader_backend = *common::settings::values().renderer_backend.get_value();
        let use_assembly_shaders = shader_backend == RendererBackend::OpenGlGlasm
            && has_extension_in(&extensions, "GL_NV_gpu_program5")
            && has_extension_in(&extensions, "GL_NV_compute_program5")
            && has_extension_in(&extensions, "GL_NV_transform_feedback")
            && has_extension_in(&extensions, "GL_NV_transform_feedback2");
        if shader_backend == RendererBackend::OpenGlGlasm && !use_assembly_shaders {
            log::error!("Assembly shaders enabled but not supported - expect instability!");
        }
        let has_cbuf_ftou_bug = shader_backend == RendererBackend::OpenGlGlsl
            && is_nvidia
            && nvidia_driver_major_version(&gl_version) >= 495;
        let has_bool_ref_bug = has_cbuf_ftou_bug;
        let has_lmem_perf_bug = is_nvidia;

        let blacklist_async_shaders =
            (is_intel && !cfg!(target_family = "unix")) || strict_context_required;
        let requested_async_shaders = *common::settings::values()
            .use_asynchronous_shaders
            .get_value();
        let use_asynchronous_shaders = requested_async_shaders && !blacklist_async_shaders;
        let use_driver_cache = is_nvidia;
        let supports_conditional_barriers = !is_intel;

        info!("Renderer_VariableAOFFI: {}", has_variable_aoffi);
        info!(
            "Renderer_ComponentIndexingBug: {}",
            has_component_indexing_bug
        );
        info!("Renderer_PreciseBug: {}", has_precise_bug);
        info!(
            "Renderer_BrokenTextureViewFormats: {}",
            has_broken_texture_view_formats
        );
        if requested_async_shaders && !use_asynchronous_shaders {
            warn!("Asynchronous shader compilation enabled but not supported");
        }

        Ok(Device {
            max_uniform_buffers,
            uniform_buffer_alignment,
            shader_storage_alignment,
            max_vertex_attributes,
            max_varyings,
            max_compute_shared_memory_size,
            max_glasm_storage_buffer_blocks,
            max_user_clip_distances,
            has_warp_intrinsics,
            has_shader_ballot,
            has_vertex_viewport_layer,
            has_image_load_formatted,
            has_texture_shadow_lod,
            has_astc,
            has_variable_aoffi,
            has_component_indexing_bug,
            has_precise_bug,
            has_broken_texture_view_formats,
            has_fast_buffer_sub_data,
            has_nv_viewport_array2,
            has_derivative_control,
            has_vertex_buffer_unified_memory,
            has_debugging_tool_attached,
            use_assembly_shaders,
            use_asynchronous_shaders,
            use_driver_cache,
            has_depth_buffer_float,
            has_geometry_shader_passthrough,
            has_nv_gpu_shader_5,
            has_shader_int64,
            has_amd_shader_half_float,
            has_sparse_texture_2,
            has_draw_texture,
            warp_size_potentially_larger_than_guest,
            need_fastmath_off,
            has_cbuf_ftou_bug,
            has_bool_ref_bug,
            can_report_memory,
            strict_context_required,
            supports_conditional_barriers,
            has_lmem_perf_bug,
            vendor_name,
        })
    }

    // --- Accessors ---

    pub fn vendor_name(&self) -> &str {
        normalized_vendor_name(&self.vendor_name)
    }

    pub fn get_current_dedicated_video_memory(&self) -> u64 {
        const GL_GPU_MEMORY_INFO_TOTAL_AVAILABLE_MEMORY_NVX: u32 = 0x9048;
        let mut current_available_memory_kb: i32 = 0;
        unsafe {
            gl::GetIntegerv(
                GL_GPU_MEMORY_INFO_TOTAL_AVAILABLE_MEMORY_NVX,
                &mut current_available_memory_kb,
            );
        }
        (current_available_memory_kb as u64).wrapping_mul(1024)
    }

    pub fn max_uniform_buffers(&self, stage: Stage) -> u32 {
        self.max_uniform_buffers[stage as usize]
    }
    pub fn uniform_buffer_alignment(&self) -> usize {
        self.uniform_buffer_alignment
    }
    pub fn shader_storage_buffer_alignment(&self) -> usize {
        self.shader_storage_alignment
    }
    pub fn max_vertex_attributes(&self) -> u32 {
        self.max_vertex_attributes
    }
    pub fn max_varyings(&self) -> u32 {
        self.max_varyings
    }
    pub fn max_compute_shared_memory_size(&self) -> u32 {
        self.max_compute_shared_memory_size
    }
    pub fn max_user_clip_distances(&self) -> u32 {
        self.max_user_clip_distances
    }
    pub fn max_glasm_storage_buffer_blocks(&self) -> u32 {
        self.max_glasm_storage_buffer_blocks
    }
    pub fn has_warp_intrinsics(&self) -> bool {
        self.has_warp_intrinsics
    }
    pub fn has_shader_ballot(&self) -> bool {
        self.has_shader_ballot
    }
    pub fn has_vertex_viewport_layer(&self) -> bool {
        self.has_vertex_viewport_layer
    }
    pub fn has_image_load_formatted(&self) -> bool {
        self.has_image_load_formatted
    }
    pub fn has_texture_shadow_lod(&self) -> bool {
        self.has_texture_shadow_lod
    }
    pub fn has_vertex_buffer_unified_memory(&self) -> bool {
        self.has_vertex_buffer_unified_memory
    }
    pub fn has_astc(&self) -> bool {
        self.has_astc
    }
    pub fn has_variable_aoffi(&self) -> bool {
        self.has_variable_aoffi
    }
    pub fn has_component_indexing_bug(&self) -> bool {
        self.has_component_indexing_bug
    }
    pub fn has_precise_bug(&self) -> bool {
        self.has_precise_bug
    }
    pub fn has_broken_texture_view_formats(&self) -> bool {
        self.has_broken_texture_view_formats
    }
    pub fn has_fast_buffer_sub_data(&self) -> bool {
        self.has_fast_buffer_sub_data
    }
    pub fn has_nv_viewport_array2(&self) -> bool {
        self.has_nv_viewport_array2
    }
    pub fn has_derivative_control(&self) -> bool {
        self.has_derivative_control
    }
    pub fn has_debugging_tool_attached(&self) -> bool {
        self.has_debugging_tool_attached
    }
    pub fn use_assembly_shaders(&self) -> bool {
        self.use_assembly_shaders
    }
    pub fn use_asynchronous_shaders(&self) -> bool {
        self.use_asynchronous_shaders
    }
    pub fn use_driver_cache(&self) -> bool {
        self.use_driver_cache
    }
    pub fn has_depth_buffer_float(&self) -> bool {
        self.has_depth_buffer_float
    }
    pub fn has_geometry_shader_passthrough(&self) -> bool {
        self.has_geometry_shader_passthrough
    }
    pub fn has_nv_gpu_shader5(&self) -> bool {
        self.has_nv_gpu_shader_5
    }
    pub fn has_shader_int64(&self) -> bool {
        self.has_shader_int64
    }
    pub fn has_amd_shader_half_float(&self) -> bool {
        self.has_amd_shader_half_float
    }
    pub fn has_sparse_texture2(&self) -> bool {
        self.has_sparse_texture_2
    }
    pub fn has_draw_texture(&self) -> bool {
        self.has_draw_texture
    }
    pub fn is_warp_size_potentially_larger_than_guest(&self) -> bool {
        self.warp_size_potentially_larger_than_guest
    }
    pub fn needs_fastmath_off(&self) -> bool {
        self.need_fastmath_off
    }
    pub fn has_cbuf_ftou_bug(&self) -> bool {
        self.has_cbuf_ftou_bug
    }
    pub fn has_bool_ref_bug(&self) -> bool {
        self.has_bool_ref_bug
    }
    pub fn is_amd(&self) -> bool {
        self.vendor_name == "ATI Technologies Inc."
    }
    pub fn is_intel(&self) -> bool {
        self.vendor_name == "Intel"
    }
    pub fn can_report_memory_usage(&self) -> bool {
        self.can_report_memory
    }

    pub fn strict_context_required(&self) -> bool {
        self.strict_context_required
    }
    pub fn supports_conditional_barriers(&self) -> bool {
        self.supports_conditional_barriers
    }
    pub fn has_lmem_perf_bug(&self) -> bool {
        self.has_lmem_perf_bug
    }

    fn test_variable_aoffi() -> bool {
        test_program(
            c"#version 430 core
// This is a unit test, please ignore me on apitrace bug reports.
uniform sampler2D tex;
uniform ivec2 variable_offset;
out vec4 output_attribute;
void main() {
    output_attribute = textureOffset(tex, vec2(0), variable_offset);
}
",
        )
    }

    fn test_precise_bug() -> bool {
        !test_program(
            c"#version 430 core
in vec3 coords;
out float out_value;
uniform sampler2DShadow tex;
void main() {
    precise float tmp_value = vec4(texture(tex, coords)).x;
    out_value = tmp_value;
}
",
        )
    }
}

// --- GL helper functions ---

fn gl_string(name: gl::types::GLenum) -> String {
    unsafe {
        let ptr = gl::GetString(name);
        if ptr.is_null() {
            return String::new();
        }
        CStr::from_ptr(ptr as *const _)
            .to_string_lossy()
            .into_owned()
    }
}

fn gl_get_integer(pname: gl::types::GLenum) -> i32 {
    let mut val: i32 = 0;
    unsafe {
        gl::GetIntegerv(pname, &mut val);
    }
    val
}

fn build_max_uniform_buffers() -> [u32; shader_recompiler::stage::MAX_STAGE_TYPES as usize] {
    std::array::from_fn(|index| gl_get_integer(LIMIT_UBOS[index]) as u32)
}

fn test_program(glsl: &'static CStr) -> bool {
    unsafe {
        let source = glsl.as_ptr();
        let program = gl::CreateShaderProgramv(gl::VERTEX_SHADER, 1, &source);
        let mut link_status = 0;
        gl::GetProgramiv(program, gl::LINK_STATUS, &mut link_status);
        gl::DeleteProgram(program);
        link_status == gl::TRUE as i32
    }
}

fn get_extensions() -> Vec<String> {
    let num = gl_get_integer(gl::NUM_EXTENSIONS);
    let mut exts = Vec::new();
    for i in 0..num {
        unsafe {
            let ptr = gl::GetStringi(gl::EXTENSIONS, i as u32);
            if !ptr.is_null() {
                let s = CStr::from_ptr(ptr as *const _)
                    .to_string_lossy()
                    .into_owned();
                exts.push(s);
            }
        }
    }
    exts
}

fn has_extension_in(extensions: &[String], extension: &str) -> bool {
    extensions.iter().any(|candidate| candidate == extension)
}

fn is_debug_tool_attached(extensions: &[String]) -> bool {
    let nsight = std::env::var_os("NVTX_INJECTION64_PATH").is_some()
        || std::env::var_os("NSIGHT_LAUNCHED").is_some();
    nsight
        || has_extension_in(extensions, "GL_EXT_debug_tool")
        || *common::settings::values().renderer_debug.get_value()
}

/// Query an OpenGL extension from the context-global extension set, matching
/// the GLAD feature flags consumed directly by upstream renderer modules.
pub(crate) fn has_extension(name: &str) -> bool {
    has_extension_in(&get_extensions(), name)
}

fn has_slow_software_astc(vendor_name: &str, renderer: &str) -> bool {
    if cfg!(target_family = "unix") {
        if vendor_name == "AMD" {
            return true;
        }
        if vendor_name == "Intel" {
            return renderer.contains("DG");
        }
        if vendor_name == "nouveau" || vendor_name == "X.Org" {
            return true;
        }
    }
    matches!(
        vendor_name,
        "Collabora Ltd" | "Microsoft Corporation" | "Mesa/X.org"
    )
}

fn is_astc_supported() -> bool {
    const GL_FULL_SUPPORT: i32 = 0x82B7;
    const GL_COMPRESSED_RGBA_ASTC_4X4_KHR: u32 = 0x93B0;
    const GL_COMPRESSED_RGBA_ASTC_5X4_KHR: u32 = 0x93B1;
    const GL_COMPRESSED_RGBA_ASTC_5X5_KHR: u32 = 0x93B2;
    const GL_COMPRESSED_RGBA_ASTC_6X5_KHR: u32 = 0x93B3;
    const GL_COMPRESSED_RGBA_ASTC_6X6_KHR: u32 = 0x93B4;
    const GL_COMPRESSED_RGBA_ASTC_8X5_KHR: u32 = 0x93B5;
    const GL_COMPRESSED_RGBA_ASTC_8X6_KHR: u32 = 0x93B6;
    const GL_COMPRESSED_RGBA_ASTC_8X8_KHR: u32 = 0x93B7;
    const GL_COMPRESSED_RGBA_ASTC_10X5_KHR: u32 = 0x93B8;
    const GL_COMPRESSED_RGBA_ASTC_10X6_KHR: u32 = 0x93B9;
    const GL_COMPRESSED_RGBA_ASTC_10X8_KHR: u32 = 0x93BA;
    const GL_COMPRESSED_RGBA_ASTC_10X10_KHR: u32 = 0x93BB;
    const GL_COMPRESSED_RGBA_ASTC_12X10_KHR: u32 = 0x93BC;
    const GL_COMPRESSED_RGBA_ASTC_12X12_KHR: u32 = 0x93BD;
    const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_4X4_KHR: u32 = 0x93D0;
    const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_5X4_KHR: u32 = 0x93D1;
    const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_5X5_KHR: u32 = 0x93D2;
    const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_6X5_KHR: u32 = 0x93D3;
    const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_6X6_KHR: u32 = 0x93D4;
    const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8X5_KHR: u32 = 0x93D5;
    const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8X6_KHR: u32 = 0x93D6;
    const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8X8_KHR: u32 = 0x93D7;
    const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10X5_KHR: u32 = 0x93D8;
    const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10X6_KHR: u32 = 0x93D9;
    const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10X8_KHR: u32 = 0x93DA;
    const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10X10_KHR: u32 = 0x93DB;
    const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_12X10_KHR: u32 = 0x93DC;
    const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_12X12_KHR: u32 = 0x93DD;
    const TARGETS: [u32; 2] = [gl::TEXTURE_2D, gl::TEXTURE_2D_ARRAY];
    const FORMATS: [u32; 28] = [
        GL_COMPRESSED_RGBA_ASTC_4X4_KHR,
        GL_COMPRESSED_RGBA_ASTC_5X4_KHR,
        GL_COMPRESSED_RGBA_ASTC_5X5_KHR,
        GL_COMPRESSED_RGBA_ASTC_6X5_KHR,
        GL_COMPRESSED_RGBA_ASTC_6X6_KHR,
        GL_COMPRESSED_RGBA_ASTC_8X5_KHR,
        GL_COMPRESSED_RGBA_ASTC_8X6_KHR,
        GL_COMPRESSED_RGBA_ASTC_8X8_KHR,
        GL_COMPRESSED_RGBA_ASTC_10X5_KHR,
        GL_COMPRESSED_RGBA_ASTC_10X6_KHR,
        GL_COMPRESSED_RGBA_ASTC_10X8_KHR,
        GL_COMPRESSED_RGBA_ASTC_10X10_KHR,
        GL_COMPRESSED_RGBA_ASTC_12X10_KHR,
        GL_COMPRESSED_RGBA_ASTC_12X12_KHR,
        GL_COMPRESSED_SRGB8_ALPHA8_ASTC_4X4_KHR,
        GL_COMPRESSED_SRGB8_ALPHA8_ASTC_5X4_KHR,
        GL_COMPRESSED_SRGB8_ALPHA8_ASTC_5X5_KHR,
        GL_COMPRESSED_SRGB8_ALPHA8_ASTC_6X5_KHR,
        GL_COMPRESSED_SRGB8_ALPHA8_ASTC_6X6_KHR,
        GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8X5_KHR,
        GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8X6_KHR,
        GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8X8_KHR,
        GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10X5_KHR,
        GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10X6_KHR,
        GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10X8_KHR,
        GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10X10_KHR,
        GL_COMPRESSED_SRGB8_ALPHA8_ASTC_12X10_KHR,
        GL_COMPRESSED_SRGB8_ALPHA8_ASTC_12X12_KHR,
    ];
    const REQUIRED_SUPPORT: [u32; 6] = [
        gl::VERTEX_TEXTURE,
        gl::TESS_CONTROL_TEXTURE,
        gl::TESS_EVALUATION_TEXTURE,
        gl::GEOMETRY_TEXTURE,
        gl::FRAGMENT_TEXTURE,
        gl::COMPUTE_TEXTURE,
    ];
    for target in TARGETS {
        for format in FORMATS {
            for support in REQUIRED_SUPPORT {
                let mut value = 0;
                unsafe {
                    gl::GetInternalformativ(target, format, support, 1, &mut value);
                }
                if value != GL_FULL_SUPPORT {
                    return false;
                }
            }
        }
    }
    true
}

fn nvidia_driver_major_version(gl_version: &str) -> i32 {
    let driver_version = &gl_version[13..];
    let version_major = driver_version
        .split_once('.')
        .map_or(driver_version, |(major, _)| major);
    atoi_prefix(version_major)
}

/// Parse the same numeric prefix accepted by the `std::atoi` call in Eden.
fn atoi_prefix(value: &str) -> i32 {
    let value = value.trim_start_matches(|character: char| character.is_ascii_whitespace());
    let (negative, digits) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let digit_count = digits
        .as_bytes()
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digit_count == 0 {
        return 0;
    }
    let parsed = digits[..digit_count].parse::<i64>().unwrap_or(0);
    (if negative { -parsed } else { parsed }) as i32
}

fn normalized_vendor_name(vendor_name: &str) -> &str {
    match vendor_name {
        "NVIDIA Corporation" => "NVIDIA",
        "ATI Technologies Inc." => "AMD",
        "Intel" => "Intel",
        "Intel Open Source Technology Center" => "i965",
        "Mesa Project" => "i915",
        "Mesa/X.org" => "Mesa",
        "AMD" => "RadeonSI",
        "nouveau" => "Nouveau",
        "X.Org" => "R600",
        "Collabora Ltd" => "Zink",
        "Intel Corporation" => "OpenSWR",
        "Microsoft Corporation" => "D3D12",
        "NVIDIA" => "Tegra",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_names_match_upstream_driver_names() {
        assert_eq!(normalized_vendor_name("NVIDIA Corporation"), "NVIDIA");
        assert_eq!(normalized_vendor_name("ATI Technologies Inc."), "AMD");
        assert_eq!(normalized_vendor_name("AMD"), "RadeonSI");
        assert_eq!(normalized_vendor_name("Mesa/X.org"), "Mesa");
        assert_eq!(normalized_vendor_name("NVIDIA"), "Tegra");
        assert_eq!(normalized_vendor_name("Unknown Driver"), "Unknown Driver");
    }

    #[test]
    fn nvidia_driver_version_parser_matches_upstream_substr_and_atoi() {
        assert_eq!(nvidia_driver_major_version("4.6.0 NVIDIA 495.44"), 495);
        assert_eq!(nvidia_driver_major_version("4.6.0 NVIDIA  550beta.2"), 550);
        assert_eq!(nvidia_driver_major_version("4.6.0 NVIDIA +560rc.1"), 560);
        assert_eq!(nvidia_driver_major_version("xxxxxxxxxxxxxinvalid.1"), 0);
        assert_eq!(atoi_prefix("-2147483648suffix"), i32::MIN);
    }
}

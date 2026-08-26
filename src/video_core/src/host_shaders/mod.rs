// Host shader sources embedded from Eden's video_core/host_shaders/.
//
// Each shader file from the upstream C++ build is embedded as a const &str
// in the appropriate submodule, grouped by shader type.
// The shaders can be compiled to SPIR-V at build time or runtime.

pub mod compute_shaders;
pub mod fragment_shaders;
pub mod glsl_includes;
pub mod spirv_shaders;
pub mod vertex_shaders;

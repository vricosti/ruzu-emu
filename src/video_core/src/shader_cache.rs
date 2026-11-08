// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/shader_cache.h and video_core/shader_cache.cpp
//!
//! Shader binary caching and invalidation.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use shader_recompiler::exception::{
    InvalidArgument, LogicError, NotImplementedException, RuntimeError, ShaderException,
};

/// Rust representation of the shader exception held by upstream's `std::exception_ptr`.
/// Preserve the concrete payload when rethrowing; unrelated panics are never shader rejections.
#[derive(Clone)]
enum RejectedShaderException {
    Base(ShaderException),
    Logic(LogicError),
    Runtime(RuntimeError),
    NotImplemented(NotImplementedException),
    InvalidArgument(InvalidArgument),
}

impl RejectedShaderException {
    fn from_payload(payload: &(dyn std::any::Any + Send)) -> Option<Self> {
        if let Some(error) = payload.downcast_ref::<ShaderException>() {
            Some(Self::Base(error.clone()))
        } else if let Some(error) = payload.downcast_ref::<LogicError>() {
            Some(Self::Logic(error.clone()))
        } else if let Some(error) = payload.downcast_ref::<RuntimeError>() {
            Some(Self::Runtime(error.clone()))
        } else if let Some(error) = payload.downcast_ref::<NotImplementedException>() {
            Some(Self::NotImplemented(error.clone()))
        } else {
            payload
                .downcast_ref::<InvalidArgument>()
                .map(|error| Self::InvalidArgument(error.clone()))
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Base(error) => error.to_string(),
            Self::Logic(error) => error.to_string(),
            Self::Runtime(error) => error.to_string(),
            Self::NotImplemented(error) => error.to_string(),
            Self::InvalidArgument(error) => error.to_string(),
        }
    }

    fn rethrow(self) -> ! {
        let payload: Box<dyn std::any::Any + Send> = match self {
            Self::Base(error) => Box::new(error),
            Self::Logic(error) => Box::new(error),
            Self::Runtime(error) => Box::new(error),
            Self::NotImplemented(error) => Box::new(error),
            Self::InvalidArgument(error) => Box::new(error),
        };
        std::panic::resume_unwind(payload)
    }
}

use crate::control::channel_state::ChannelState;
use crate::control::channel_state_cache::{ChannelInfo, ChannelSetupCaches};
use crate::engines::kepler_compute::KeplerCompute;
use crate::engines::maxwell_3d::{Maxwell3D, ShaderStageType, PIPELINE_BASE, PIPELINE_STRIDE};
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::shader_environment::{
    ComputeEnvironment, GenericEnvironment, GenericEnvironmentOwner, GraphicsEnvironment,
};

/// Virtual address type.
pub type VAddr = u64;

const YUZU_PAGEBITS: u64 = 14;
const YUZU_PAGESIZE: u64 = 1 << YUZU_PAGEBITS;
pub const NUM_PROGRAMS: usize = 6;

/// Information about a compiled shader.
#[derive(Debug, Default)]
pub struct ShaderInfo {
    pub unique_hash: u64,
    pub size_bytes: usize,
}

/// An entry in the shader lookup cache.
struct Entry {
    addr_start: VAddr,
    addr_end: VAddr,
    data: *mut ShaderInfo,
    is_memory_marked: bool,
}

impl Entry {
    fn overlaps(&self, start: VAddr, end: VAddr) -> bool {
        start < self.addr_end && self.addr_start < end
    }
}

/// Shader cache that tracks compiled shaders by their guest memory address.
///
/// Handles invalidation when guest memory is modified.
pub struct ShaderCache {
    /// Shared `MaxwellDeviceMemoryManager` instance — same `Arc` is held
    /// by `Host1x::memory_manager`, the buffer cache, and the texture
    /// cache. Mirrors upstream's `MaxwellDeviceMemoryManager& device_memory`
    /// reference member.
    device_memory: Arc<MaxwellDeviceMemoryManager>,
    channel_caches: ChannelSetupCaches<ChannelInfo>,
    lookup_mutex: Mutex<()>,
    invalidation_mutex: Mutex<()>,
    lookup_cache: HashMap<u64, Box<Entry>>,
    invalidation_cache: HashMap<u64, Vec<*mut Entry>>,
    storage: Vec<Box<ShaderInfo>>,
    marked_for_removal: Vec<*mut Entry>,
    shader_infos: [Option<*const ShaderInfo>; NUM_PROGRAMS],
    last_shaders_valid: bool,
    // Immutable cache policy: ordinary and compute-backed mesh consumers must
    // not share native rejection entries or previously lowered code identities.
    mesh_compute: bool,
    rejected_native: Mutex<HashMap<u64, RejectedShaderException>>,
    // Bounds of every rejected address; deliberately never shrink after invalidation.
    rejected_lowest: AtomicU64,
    rejected_highest: AtomicU64,
}

// Safety: Entry pointers are only used within locked sections.
unsafe impl Send for ShaderCache {}
unsafe impl Sync for ShaderCache {}

pub struct GraphicsEnvironments {
    pub envs: [GraphicsEnvironment; NUM_PROGRAMS],
    pub env_ptrs: [Option<usize>; NUM_PROGRAMS],
}

impl GraphicsEnvironments {
    pub fn span(&self) -> Vec<&GenericEnvironment> {
        self.env_ptrs
            .iter()
            .flatten()
            .map(|&index| self.envs[index].generic_environment())
            .collect()
    }
}

impl Default for GraphicsEnvironments {
    fn default() -> Self {
        Self {
            envs: std::array::from_fn(|_| GraphicsEnvironment::default()),
            env_ptrs: [None; NUM_PROGRAMS],
        }
    }
}

fn shader_stage_type_from_index(index: usize) -> ShaderStageType {
    match index {
        0 => ShaderStageType::VertexA,
        1 => ShaderStageType::VertexB,
        2 => ShaderStageType::TessInit,
        3 => ShaderStageType::Tessellation,
        4 => ShaderStageType::Geometry,
        5 => ShaderStageType::Fragment,
        _ => ShaderStageType::Invalid,
    }
}

impl ShaderCache {
    /// Port of upstream `ShaderCache::ShaderCache(MaxwellDeviceMemoryManager& device_memory_)`.
    /// Takes a shared `Arc` rather than a reference: the same instance is
    /// held by `Host1x`, the buffer cache, and the texture cache.
    pub fn new(device_memory: Arc<MaxwellDeviceMemoryManager>) -> Self {
        Self {
            device_memory,
            channel_caches: ChannelSetupCaches::new(),
            lookup_mutex: Mutex::new(()),
            invalidation_mutex: Mutex::new(()),
            lookup_cache: HashMap::new(),
            invalidation_cache: HashMap::new(),
            storage: Vec::new(),
            marked_for_removal: Vec::new(),
            shader_infos: [None; NUM_PROGRAMS],
            last_shaders_valid: false,
            mesh_compute: false,
            rejected_native: Mutex::new(HashMap::new()),
            rejected_lowest: AtomicU64::new(u64::MAX),
            rejected_highest: AtomicU64::new(0),
        }
    }

    /// Construct the cache for a consumer that owns compute-backed task/mesh
    /// dispatch. The ordinary graphics renderer continues to use `new`.
    pub fn new_mesh_compute(device_memory: Arc<MaxwellDeviceMemoryManager>) -> Self {
        Self { mesh_compute: true, ..Self::new(device_memory) }
    }

    /// Access the shared `MaxwellDeviceMemoryManager`. Same `Arc` as
    /// `Host1x::memory_manager()`.
    pub fn device_memory(&self) -> &Arc<MaxwellDeviceMemoryManager> {
        &self.device_memory
    }

    /// Port of the shared `ShaderCache` channel-owner `CreateChannel` edge.
    pub fn create_channel(&mut self, channel: &ChannelState) {
        self.channel_caches.create_channel(channel);
    }

    /// Port of the shared `ShaderCache` channel-owner `BindToChannel` edge.
    pub fn bind_to_channel(&mut self, channel_id: i32) {
        self.channel_caches.bind_to_channel(channel_id);
    }

    /// Port of the shared `ShaderCache` channel-owner `EraseChannel` edge.
    pub fn erase_channel(&mut self, channel_id: i32) {
        self.channel_caches.erase_channel(channel_id);
    }

    /// Reduced Rust accessor for the currently bound shared channel owner.
    pub fn current_channel_info(&self) -> Option<&ChannelInfo> {
        self.channel_caches.current_channel_state()
    }

    /// Reduced Rust accessor for the shared shader-stage cache state.
    pub fn last_shaders_valid(&self) -> bool {
        self.last_shaders_valid
    }

    /// Reduced Rust accessor for the shared shader-info owner slots.
    pub fn shader_info_slots(&self) -> &[Option<*const ShaderInfo>; NUM_PROGRAMS] {
        &self.shader_infos
    }

    /// Port of `ShaderCache::MeshStateHash`: only the first twenty primitive
    /// header words participate in the upstream pipeline specialization key.
    fn mesh_state_hash(&self) -> u64 {
        use crate::engines::maxwell_3d::{MESH_SHADER_A, MESH_SHADER_B, MESH_INIT_SHADER};
        let maxwell = self.current_maxwell3d().expect("mesh hash requires a bound channel");
        let mut state = [0u8; (3 + 20) * 4];
        for (index, register) in [MESH_SHADER_A, MESH_SHADER_B, MESH_INIT_SHADER].into_iter().enumerate() {
            state[index * 4..index * 4 + 4]
                .copy_from_slice(&maxwell.get_register_value(register).to_le_bytes());
        }
        if maxwell.shader_config_enabled(ShaderStageType::Geometry) {
            let slot = PIPELINE_BASE + ShaderStageType::Geometry.as_index().unwrap() * PIPELINE_STRIDE;
            let address = (u64::from(maxwell.get_register_value(slot + 5) & 0xff) << 32)
                | u64::from(maxwell.get_register_value(slot + 6));
            self.current_channel_info().unwrap().gpu_memory.as_ref()
                .expect("mesh hash requires GPU memory").lock()
                .read_block_unsafe(address, &mut state[12..]);
        }
        common::cityhash::city_hash64(&state) | 1
    }

    /// Port of `ShaderCache::RefreshStages`.
    pub fn refresh_stages(&mut self, unique_hashes: &mut [u64; NUM_PROGRAMS]) -> bool {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let Some(channel) = self.current_channel_info() else {
                self.last_shaders_valid = false;
                return false;
            };
            let maxwell_ptr = channel.maxwell3d as *mut Maxwell3D;
            if maxwell_ptr.is_null() {
                self.last_shaders_valid = false;
                return false;
            }
            let maxwell3d = unsafe { &mut *maxwell_ptr };
            if !maxwell3d.consume_dirty_shaders() {
                return self.last_shaders_valid;
            }
            let Some(gpu_memory) = channel.gpu_memory.as_ref().map(Arc::clone) else {
                self.last_shaders_valid = false;
                return false;
            };
            if maxwell3d.memory_manager().is_none() {
                self.last_shaders_valid = false;
                return false;
            }
            let base_addr = maxwell3d.program_region_address();
            let native = maxwell3d.get_native_macro_engine_read(|engine| engine.native_turing);
            let mesh = native && maxwell3d.is_mesh_enabled();
            let rasterize_enable = maxwell3d.rasterize_enable();
            let stage_infos: [crate::engines::maxwell_3d::ShaderStageInfo; NUM_PROGRAMS] =
                std::array::from_fn(|index| maxwell3d.shader_stage_info(index as u32));
            let stage_enabled: [bool; NUM_PROGRAMS] =
                std::array::from_fn(|index| maxwell3d.is_shader_stage_enabled(index as u32));
            for (index, unique_hash) in unique_hashes.iter_mut().enumerate() {
                let stage_info = stage_infos[index];
                let program_type = shader_stage_type_from_index(index);
                if !stage_enabled[index] {
                    *unique_hash = 0;
                    continue;
                }
                if program_type == ShaderStageType::Fragment && !rasterize_enable {
                    *unique_hash = 0;
                    continue;
                }
                if mesh && program_type == ShaderStageType::Geometry {
                    // Header-only primitive output map, consumed by the mesh environment.
                    *unique_hash = 0;
                    continue;
                }

                let slot = PIPELINE_BASE + index as u32 * PIPELINE_STRIDE;
                let shader_addr = if native {
                    ((maxwell3d.get_register_value(slot + 5) as u64 & 0xff) << 32)
                        | maxwell3d.get_register_value(slot + 6) as u64
                } else {
                    base_addr + stage_info.offset as u64
                };
                let cpu_shader_addr = {
                    let memory = gpu_memory.lock();
                    memory.gpu_to_cpu_address(shader_addr)
                };
                let Some(cpu_shader_addr) = cpu_shader_addr else {
                    self.last_shaders_valid = false;
                    return false;
                };

                let shader_ptr = if let Some(shader_info) = self.try_get(cpu_shader_addr) {
                    shader_info as *const ShaderInfo
                } else {
                    if native && self.is_rejected(cpu_shader_addr) {
                        maxwell3d.get_native_macro_engine(|engine| {
                            engine.native_execution_failed = true
                        });
                        self.last_shaders_valid = false;
                        return false;
                    }
                    if program_type == ShaderStageType::Invalid {
                        *unique_hash = 0;
                        continue;
                    }
                    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.rethrow_if_rejected(cpu_shader_addr);
                        let constructor = if self.mesh_compute && mesh
                            && matches!(program_type, ShaderStageType::VertexB | ShaderStageType::Tessellation) {
                            GraphicsEnvironment::from_maxwell3d_live_mesh
                        } else {
                            GraphicsEnvironment::from_maxwell3d
                        };
                        let mut env = constructor(
                            maxwell3d,
                            program_type,
                            if native { shader_addr } else { base_addr },
                            if native { 0 } else { stage_info.offset },
                        );
                        self.make_shader_info(&mut env, cpu_shader_addr) as *const ShaderInfo
                    }));
                    match built {
                        Ok(info) => info,
                        Err(payload) => {
                            if let Some(error) = RejectedShaderException::from_payload(&*payload) {
                                self.remember_rejection(cpu_shader_addr, error);
                                static DESCRIBED: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
                                if native
                                    && DESCRIBED
                                        .get_or_init(Default::default)
                                        .lock()
                                        .insert(shader_addr)
                                {
                                    let mut bytes = [0u8; 20];
                                    gpu_memory.lock().read_block_unsafe(shader_addr, &mut bytes);
                                    let header: Vec<u32> = bytes
                                        .chunks_exact(4)
                                        .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                                        .collect();
                                    let config: Vec<u32> = (0..16)
                                        .map(|word| maxwell3d.get_register_value(slot + word))
                                        .collect();
                                    log::error!("Rejected program {shader_addr:#x} is bound to stage {index}: header {header:08x?}, slot {config:08x?}");
                                }
                            }
                            std::panic::resume_unwind(payload)
                        }
                    }
                };

                let shader_info = unsafe { &*shader_ptr };
                self.shader_infos[index] = Some(shader_ptr);
                *unique_hash = shader_info.unique_hash;
                if mesh && matches!(program_type, ShaderStageType::VertexB | ShaderStageType::Tessellation) {
                    *unique_hash ^= self.mesh_state_hash();
                }
            }

            self.last_shaders_valid = true;
            true
        }));
        match result {
            Ok(valid) => valid,
            Err(payload) => {
                let Some(error) = RejectedShaderException::from_payload(&*payload) else {
                    std::panic::resume_unwind(payload);
                };
                let channel = self
                    .current_channel_info()
                    .expect("shader exception requires a bound channel");
                let maxwell3d = unsafe { &mut *(channel.maxwell3d as *mut Maxwell3D) };
                if !maxwell3d.get_native_macro_engine_read(|engine| engine.native_turing) {
                    std::panic::resume_unwind(payload);
                }
                let gpu_memory = channel.gpu_memory.as_ref().cloned();
                maxwell3d.get_native_macro_engine(|engine| engine.native_execution_failed = true);
                self.last_shaders_valid = false;
                let message = error.message();
                static LOGGED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
                if LOGGED
                    .get_or_init(Default::default)
                    .lock()
                    .insert(message.clone())
                {
                    // Include the actual NVN mesh configuration to distinguish
                    // missing task/mesh prerequisites from ordinary vertex code.
                    use crate::engines::maxwell_3d::{
                        MESH_CONTROL, MESH_SHADER_A, MESH_SHADER_B, MESH_INIT_SHADER,
                    };
                    log::error!(
                        "Native graphics shader rejected: {message}; mesh_enabled={} mesh_regs={:08x?}",
                        maxwell3d.is_mesh_enabled(),
                        [MESH_CONTROL, MESH_SHADER_A, MESH_SHADER_B, MESH_INIT_SHADER]
                            .map(|reg| maxwell3d.regs[reg as usize]),
                    );
                    // Diagnostic extension: an earlier rejected task prevents
                    // RefreshStages from visiting its mesh/primitive-header slots.
                    // Preserve mapped snapshots of those slots for offline analysis,
                    // without accepting or executing the unsupported programs.
                    if maxwell3d.is_mesh_enabled() {
                        if let (Some(directory), Some(memory)) =
                            (std::env::var_os("EDEN_NX2_SHADER_DUMP"), &gpu_memory)
                        {
                            let directory = std::path::PathBuf::from(directory);
                            let memory = memory.lock();
                            let mut metadata = format!("{message}\nmesh_regs={:08x?}\n",
                                [MESH_CONTROL, MESH_SHADER_A, MESH_SHADER_B, MESH_INIT_SHADER]
                                    .map(|reg| maxwell3d.regs[reg as usize]));
                            for stage in [1, 3, 4] {
                                let slot = PIPELINE_BASE + stage * PIPELINE_STRIDE;
                                let address = ((u64::from(maxwell3d.get_register_value(slot + 5)) & 0xff) << 32)
                                    | u64::from(maxwell3d.get_register_value(slot + 6));
                                let enabled = maxwell3d.is_shader_stage_enabled(stage);
                                metadata.push_str(&format!("stage={stage} enabled={enabled} address={address:#x}\n"));
                                if !enabled { continue; }
                                let path = directory.join(format!("mesh-stage{stage}-{address:016x}.snapshot.bin"));
                                if path.exists() { continue; }
                                // A snapshot is explicitly not an instruction count.
                                // Include the native 128-byte SPH; stage 4 is header-only.
                                let limit = if stage == 4 { 128 } else { 1024 * 1024 };
                                let mut bytes = Vec::new();
                                while bytes.len() < limit {
                                    let count = (limit - bytes.len()).min(128);
                                    let next = address + bytes.len() as u64;
                                    if !memory.is_fully_mapped_range(next, count as u64) { break; }
                                    let mut block = [0u8; 128];
                                    memory.read_block_unsafe(next, &mut block[..count]);
                                    bytes.extend_from_slice(&block[..count]);
                                }
                                if let Err(error) = std::fs::create_dir_all(&directory)
                                    .and_then(|_| std::fs::write(&path, &bytes)) {
                                    log::warn!("Native mesh snapshot {}: {error}", path.display());
                                }
                            }
                            let key = common::cityhash::city_hash64(metadata.as_bytes());
                            if let Err(error) = std::fs::write(directory.join(format!("mesh-pipeline-{key:016x}.txt")), metadata) {
                                log::warn!("Native mesh pipeline metadata: {error}");
                            }
                        }
                    }
                }
                static DUMPED_ERRORS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
                let mut dumped = DUMPED_ERRORS.get_or_init(Default::default).lock();
                if message.contains("constant buffer")
                    && dumped.len() < 12
                    && dumped
                        .iter()
                        .filter(|previous| **previous == message)
                        .count()
                        < 3
                {
                    dumped.push(message.clone());
                    log::info!(
                        "VS cbuf dump for: {}",
                        message.chars().take(40).collect::<String>()
                    );
                    for slot in 3..8 {
                        let buffer = &maxwell3d.const_buffer_bindings(0)[slot];
                        if !buffer.enabled {
                            continue;
                        }
                        let mut bytes = [0u8; 96];
                        if let Some(memory) = &gpu_memory {
                            memory.lock().read_block_unsafe(buffer.address, &mut bytes);
                        }
                        let words: Vec<u32> = bytes
                            .chunks_exact(4)
                            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                            .collect();
                        log::info!(
                            "VS cbuf {slot} at {:#x} size {:#x}: {words:08x?}",
                            buffer.address,
                            buffer.size
                        );
                    }
                }
                false
            }
        }
    }

    /// Port of `ShaderCache::ComputeShader`.
    pub fn compute_shader(&mut self) -> Option<&ShaderInfo> {
        let channel = self.current_channel_info()?;
        let kepler_ptr = channel.kepler_compute as *const KeplerCompute;
        if kepler_ptr.is_null() {
            return None;
        }
        let gpu_memory = channel.gpu_memory.as_ref().map(Arc::clone)?;
        let kepler_compute = unsafe { &*kepler_ptr };

        let program_base = kepler_compute.code_address();
        let qmd = kepler_compute.launch_description();
        let shader_addr = program_base + qmd.program_start as u64;
        let cpu_shader_addr = {
            let memory = gpu_memory.lock();
            memory.gpu_to_cpu_address(shader_addr)?
        };
        if let Some(shader_ptr) = self
            .try_get(cpu_shader_addr)
            .map(|shader| shader as *const _)
        {
            return Some(unsafe { &*shader_ptr });
        }

        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.rethrow_if_rejected(cpu_shader_addr);
            let mut env =
                ComputeEnvironment::from_kepler_compute(kepler_compute, Arc::clone(&gpu_memory));
            self.make_shader_info(&mut env, cpu_shader_addr) as *const ShaderInfo
        }));
        match built {
            Ok(info) => Some(unsafe { &*info }),
            Err(payload) => {
                if let Some(error) = RejectedShaderException::from_payload(&*payload) {
                    self.remember_rejection(cpu_shader_addr, error);
                }
                std::panic::resume_unwind(payload)
            }
        }
    }

    /// Port of `ShaderCache::RethrowIfRejected`.
    fn rethrow_if_rejected(&self, addr: VAddr) {
        let rejected = self.rejected_native.lock();
        if let Some(error) = rejected.get(&addr) {
            error.clone().rethrow();
        }
    }

    /// Port of `ShaderCache::IsRejected`.
    fn is_rejected(&self, addr: VAddr) -> bool {
        self.rejected_native.lock().contains_key(&addr)
    }

    /// Port of `ShaderCache::RememberRejection`; the caught exception is explicit in Rust.
    fn remember_rejection(&self, addr: VAddr, error: RejectedShaderException) {
        let mut rejected = self.rejected_native.lock();
        rejected.entry(addr).or_insert(error);
        self.rejected_lowest.store(
            self.rejected_lowest.load(Ordering::Relaxed).min(addr),
            Ordering::Relaxed,
        );
        self.rejected_highest.store(
            self.rejected_highest.load(Ordering::Relaxed).max(addr),
            Ordering::Relaxed,
        );
    }

    /// Port of `ShaderCache::GetGraphicsEnvironments`.
    pub fn get_graphics_environments(
        &self,
        result: &mut GraphicsEnvironments,
        unique_hashes: &[u64; NUM_PROGRAMS],
    ) {
        result.env_ptrs = [None; NUM_PROGRAMS];

        let Some(maxwell3d) = self.current_maxwell3d() else {
            return;
        };
        if maxwell3d.memory_manager().is_none() {
            return;
        }
        let base_addr = maxwell3d.program_region_address();
        let native = maxwell3d.get_native_macro_engine_read(|engine| engine.native_turing);
        let mut env_index = 0usize;

        for (index, unique_hash) in unique_hashes.iter().enumerate() {
            if *unique_hash == 0 {
                continue;
            }
            let Some(shader_ptr) = self.shader_infos[index] else {
                continue;
            };
            let stage_info = maxwell3d.shader_stage_info(index as u32);
            let program_type = shader_stage_type_from_index(index);
            if program_type == ShaderStageType::Invalid {
                continue;
            }

            let slot = PIPELINE_BASE + index as u32 * PIPELINE_STRIDE;
            let address = if native {
                ((maxwell3d.get_register_value(slot + 5) as u64 & 0xff) << 32)
                    | maxwell3d.get_register_value(slot + 6) as u64
            } else {
                base_addr
            };
            let shader_info = unsafe { &*shader_ptr };
            let constructor = if self.mesh_compute && native && maxwell3d.is_mesh_enabled()
                && matches!(program_type, ShaderStageType::VertexB | ShaderStageType::Tessellation) {
                GraphicsEnvironment::from_maxwell3d_live_mesh
            } else {
                GraphicsEnvironment::from_maxwell3d
            };
            let mut env = constructor(
                maxwell3d,
                program_type,
                address,
                if native { 0 } else { stage_info.offset },
            );
            env.set_cached_size(shader_info.size_bytes);
            result.envs[index] = env;
            result.env_ptrs[env_index] = Some(index);
            env_index += 1;
        }
    }

    /// Removes shaders inside a given region.
    pub fn invalidate_region(&mut self, addr: VAddr, size: usize) {
        // Port of `ShaderCache::InvalidateRegion`: upstream takes
        // `invalidation_mutex` before touching invalidation_cache and
        // marked_for_removal. Ruzu reaches this object through raw rasterizer
        // pointers from multiple CPU threads, so `&mut self` is not enough to
        // serialize the host HashMaps.
        let invalidation_mutex: *const Mutex<()> = &self.invalidation_mutex;
        let _invalidation_guard = unsafe { (*invalidation_mutex).lock() };
        self.invalidate_pages_in_region(addr, size);
        self.remove_pending_shaders();
    }

    /// Unmarks a memory region as cached and marks it for removal.
    pub fn on_cache_invalidation(&mut self, addr: VAddr, size: usize) {
        // Port of `ShaderCache::OnCacheInvalidation`.
        let invalidation_mutex: *const Mutex<()> = &self.invalidation_mutex;
        let _invalidation_guard = unsafe { (*invalidation_mutex).lock() };
        self.invalidate_pages_in_region(addr, size);
    }

    /// Flushes delayed removal operations.
    pub fn sync_guest_host(&mut self) {
        // Port of `ShaderCache::SyncGuestHost`.
        let invalidation_mutex: *const Mutex<()> = &self.invalidation_mutex;
        let _invalidation_guard = unsafe { (*invalidation_mutex).lock() };
        self.remove_pending_shaders();
    }

    /// Port of `ShaderCache::Register`.
    pub fn register(&mut self, data: Box<ShaderInfo>, addr: VAddr, size: usize) {
        // Upstream takes both mutexes here:
        // `std::scoped_lock lock{invalidation_mutex, lookup_mutex}`.
        let invalidation_mutex: *const Mutex<()> = &self.invalidation_mutex;
        let lookup_mutex: *const Mutex<()> = &self.lookup_mutex;
        let _invalidation_guard = unsafe { (*invalidation_mutex).lock() };
        let _lookup_guard = unsafe { (*lookup_mutex).lock() };

        let addr_end = addr + size as u64;
        let data_ptr = (&*data as *const ShaderInfo).cast_mut();
        let entry = self.new_entry(addr, addr_end, data_ptr);

        let page_end = (addr_end + YUZU_PAGESIZE - 1) >> YUZU_PAGEBITS;
        for page in (addr >> YUZU_PAGEBITS)..page_end {
            self.invalidation_cache.entry(page).or_default().push(entry);
        }

        self.storage.push(data);
        self.device_memory.update_pages_cached_count(addr, size, 1);
    }

    fn invalidate_pages_in_region(&mut self, addr: VAddr, size: usize) {
        let addr_end = addr + size as u64;
        if addr_end > self.rejected_lowest.load(Ordering::Relaxed)
            && addr <= self.rejected_highest.load(Ordering::Relaxed)
        {
            self.rejected_native
                .lock()
                .retain(|&rejected, _| rejected < addr || rejected >= addr_end);
        }
        let page_end = (addr_end + YUZU_PAGESIZE - 1) >> YUZU_PAGEBITS;
        for page in (addr >> YUZU_PAGEBITS)..page_end {
            self.invalidate_page_entries(page, addr, addr_end);
        }
    }

    fn remove_pending_shaders(&mut self) {
        if self.marked_for_removal.is_empty() {
            return;
        }

        // Remove duplicates (port of std::ranges::sort + std::unique in upstream).
        self.marked_for_removal.sort_by_key(|p| *p as usize);
        self.marked_for_removal.dedup();

        let mut removed_shaders: Vec<*mut ShaderInfo> = Vec::new();

        // Upstream `RemovePendingShaders` takes lookup_mutex while removing
        // entries from lookup_cache. Callers already hold invalidation_mutex.
        let lookup_mutex: *const Mutex<()> = &self.lookup_mutex;
        let _lookup_guard = unsafe { (*lookup_mutex).lock() };

        for &entry_ptr in &self.marked_for_removal {
            let entry = unsafe { &*entry_ptr };
            removed_shaders.push(entry.data);

            // Remove from lookup cache.
            assert!(
                self.lookup_cache.remove(&entry.addr_start).is_some(),
                "shader pending removal must exist in the lookup cache"
            );
        }
        self.marked_for_removal.clear();

        // Remove from storage (port of RemoveShadersFromStorage).
        if !removed_shaders.is_empty() {
            self.remove_shaders_from_storage(&removed_shaders);
        }
    }

    /// Port of `ShaderCache::InvalidatePageEntries`.
    fn invalidate_page_entries(&mut self, page: u64, addr: VAddr, addr_end: VAddr) {
        loop {
            let Some(entry_ptr) = self.invalidation_cache.get(&page).and_then(|entries| {
                entries
                    .iter()
                    .copied()
                    .find(|&entry| unsafe { (*entry).overlaps(addr, addr_end) })
            }) else {
                break;
            };
            let entry = unsafe { &mut *entry_ptr };
            self.unmark_memory(entry);
            self.remove_entry_from_invalidation_cache(entry);
            self.marked_for_removal.push(entry_ptr);
        }
    }

    /// Port of `ShaderCache::RemoveEntryFromInvalidationCache`.
    fn remove_entry_from_invalidation_cache(&mut self, entry: &Entry) {
        let page_end = (entry.addr_end + YUZU_PAGESIZE - 1) >> YUZU_PAGEBITS;
        for page in (entry.addr_start >> YUZU_PAGEBITS)..page_end {
            let entries = self
                .invalidation_cache
                .get_mut(&page)
                .expect("shader entry page must exist in the invalidation cache");
            let position = entries
                .iter()
                .position(|existing| std::ptr::eq(*existing, entry))
                .expect("shader entry must exist in every covered invalidation page");
            entries.remove(position);
        }
    }

    /// Port of `ShaderCache::UnmarkMemory`.
    fn unmark_memory(&mut self, entry: &mut Entry) {
        if !entry.is_memory_marked {
            return;
        }
        entry.is_memory_marked = false;
        self.device_memory.update_pages_cached_count(
            entry.addr_start,
            (entry.addr_end - entry.addr_start) as usize,
            -1,
        );
    }

    /// Port of `ShaderCache::RemoveShadersFromStorage`.
    fn remove_shaders_from_storage(&mut self, removed_shaders: &[*mut ShaderInfo]) {
        self.storage.retain(|shader| {
            let ptr: *mut ShaderInfo = (&**shader as *const ShaderInfo).cast_mut();
            !removed_shaders.contains(&ptr)
        });
    }

    /// Port of `ShaderCache::NewEntry`.
    fn new_entry(&mut self, addr: VAddr, addr_end: VAddr, data: *mut ShaderInfo) -> *mut Entry {
        let mut entry = Box::new(Entry {
            addr_start: addr,
            addr_end,
            data,
            is_memory_marked: true,
        });
        let entry_ptr: *mut Entry = &mut *entry;
        self.lookup_cache.insert(addr, entry);
        entry_ptr
    }

    /// Try to get a cached shader at the given address.
    pub fn try_get(&self, addr: VAddr) -> Option<&ShaderInfo> {
        let _lock = self.lookup_mutex.lock();
        self.lookup_cache
            .get(&addr)
            .map(|entry| unsafe { &*entry.data })
    }

    /// Port of `ShaderCache::MakeShaderInfo`.
    pub fn make_shader_info<E>(&mut self, env: &mut E, cpu_addr: VAddr) -> &ShaderInfo
    where
        E: shader_recompiler::environment::Environment + GenericEnvironmentOwner,
    {
        let mut info = Box::new(ShaderInfo::default());
        if let Some(cached_hash) = env.generic_environment_mut().analyze() {
            info.unique_hash = cached_hash;
            info.size_bytes = env.generic_environment().cached_size_bytes();
        } else {
            self.walk_shader_control_flow(env);
            info.unique_hash = env.generic_environment().calculate_hash();
            info.size_bytes = env.generic_environment().read_size_bytes();
        }
        let source_size = env.generic_environment().source_size_bytes();
        let size_bytes = if source_size != 0 {
            source_size
        } else {
            info.size_bytes
        };
        self.register(info, cpu_addr, size_bytes);
        self.try_get(cpu_addr)
            .expect("registered shader info must be reachable through lookup cache")
    }

    pub fn current_maxwell3d(&self) -> Option<&Maxwell3D> {
        let channel = self.current_channel_info()?;
        let ptr = channel.maxwell3d as *const Maxwell3D;
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { &*ptr })
        }
    }

    pub fn current_kepler_compute(&self) -> Option<&KeplerCompute> {
        let channel = self.current_channel_info()?;
        let ptr = channel.kepler_compute as *const KeplerCompute;
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { &*ptr })
        }
    }

    pub fn current_gpu_memory(
        &self,
    ) -> Option<Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>> {
        self.current_channel_info()?
            .gpu_memory
            .as_ref()
            .map(Arc::clone)
    }

    fn walk_shader_control_flow<E>(&self, env: &mut E)
    where
        E: shader_recompiler::environment::Environment + GenericEnvironmentOwner,
    {
        let start_address = env.generic_environment().start_address();
        let _cfg = shader_recompiler::frontend::control_flow::FlowCfg::new(
            env,
            shader_recompiler::frontend::location::Location::new(start_address),
            false,
        );
    }
}

impl Default for ShaderCache {
    /// Convenience default: fresh empty `MaxwellDeviceMemoryManager`.
    /// Production code constructs `ShaderCache` from
    /// `Host1x::memory_manager()`. Used by tests and standalone benches
    /// that don't need cross-cache invalidation.
    fn default() -> Self {
        Self::new(Arc::new(MaxwellDeviceMemoryManager::default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::engine_interface::EngineInterface;
    use crate::engines::kepler_compute::LaunchParams;
    use crate::engines::maxwell_3d::Maxwell3D;
    use crate::memory_manager::MemoryManager;

    #[test]
    fn mesh_cache_builds_live_native_environments_through_compute_spirv() {
        use crate::engines::maxwell_3d::{MESH_CONTROL, MESH_SHADER_A, MESH_SHADER_B, MESH_INIT_SHADER};
        use shader_recompiler::environment::Environment;
        let gpu_base = 0x1_0000_0000u64;
        let mut backing = vec![0u8; 0x1000];
        for (index, words) in [[0xffff7927u32, 0x3ff, 0x8800, 0], [0x794d, 0, 0x03800000, 0]]
            .into_iter().enumerate() {
            for (word_index, word) in words.into_iter().enumerate() {
                let offset = 128 + index * 16 + word_index * 4;
                backing[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
            }
        }
        // Task writes payload word0; only mesh may write the primitive-count map.
        for (index, words) in [[0xffff7927u32, 0xff, 0xb800, 0], [0x794d, 0, 0x03800000, 0]]
            .into_iter().enumerate() {
            for (word_index, word) in words.into_iter().enumerate() {
                let offset = 0x300 + 128 + index * 16 + word_index * 4;
                backing[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
            }
        }
        let (memory, device_memory) = make_owner_backed_memory_manager(gpu_base, 0x2000, &backing);
        let mut maxwell = Maxwell3D::new();
        crate::dirty_flags::setup_dirty_flags(&mut maxwell.dirty.tables);
        maxwell.set_memory_manager(Arc::clone(&memory));
        maxwell.set_native_turing_macro_mode(true);
        for (register, value) in [MESH_CONTROL, MESH_SHADER_A, MESH_SHADER_B, MESH_INIT_SHADER]
            .into_iter().zip([1, 0x20204, 0x800000, 0x2c0b020]) {
            maxwell.regs[register as usize] = value;
        }
        for index in [1, 3] {
            let slot = (PIPELINE_BASE + index * PIPELINE_STRIDE) as usize;
            maxwell.regs[slot] = 1;
            maxwell.regs[slot + 5] = 1;
            maxwell.regs[slot + 6] = if index == 1 { 0x300 } else { 0 };
        }
        let mut channel = ChannelState::new(7);
        channel.memory_manager = Some(memory);
        channel.maxwell_3d = Some(Box::new(maxwell));
        let mut cache = ShaderCache::new_mesh_compute(device_memory);
        cache.create_channel(&channel);
        cache.bind_to_channel(7);
        let mut hashes = [0; NUM_PROGRAMS];
        assert!(cache.refresh_stages(&mut hashes));
        let code_hashes: [u64; NUM_PROGRAMS] = std::array::from_fn(|index|
            cache.shader_infos[index].map_or(0, |info| unsafe { &*info }.unique_hash));
        for iteration in 0..2 {
            let mut environments = GraphicsEnvironments::default();
            cache.get_graphics_environments(&mut environments, &hashes);
            assert_eq!(environments.env_ptrs[..2], [Some(1), Some(3)]);
            for index in [1, 3] {
                let env = &mut environments.envs[index];
                let code_hash = code_hashes[index];
                assert_eq!(env.mesh_shader().unwrap().is_task, index == 1);
                assert_eq!(env.generic_environment_mut().analyze().unwrap(), code_hash);
                assert_eq!(hashes[index], code_hash ^ cache.mesh_state_hash());
                let code = env.generic_environment().cached_instruction_slice().to_vec();
                let base = env.generic_environment().cached_instruction_start();
                let program = shader_recompiler::pipeline_cache::translate_program_from_env_with_host_info(
                    &code, base, env, &Default::default());
                assert_eq!(program.stage, shader_recompiler::ShaderStage::Compute);
                assert_eq!(program.info.is_task, index == 1);
                assert_eq!(program.info.is_mesh, index == 3);
                let profile = shader_recompiler::Profile {
                    unified_descriptor_binding: true,
                    supported_spirv: 0x0001_0400,
                    ..Default::default()
                };
                let spirv = shader_recompiler::backend::spirv::emit_spirv::emit_mesh_compute_spirv(
                    &program, &profile, &Default::default(), &mut Default::default()).unwrap();
                assert_eq!(spirv.code[0], 0x07230203);
            }
            if iteration == 0 {
                channel.maxwell_3d.as_mut().unwrap().call_method(MESH_SHADER_A, 0x80804, true);
                assert!(cache.refresh_stages(&mut hashes));
            }
        }
    }

    #[test]
    fn mesh_refresh_specializes_cached_code_and_skips_header_only_geometry() {
        use crate::engines::maxwell_3d::{MESH_CONTROL, MESH_SHADER_A, MESH_SHADER_B, MESH_INIT_SHADER};
        let gpu_base = 0x1_0000_0000u64;
        let cpu_base = 0x2000;
        let mut backing = vec![0u8; 0x1000];
        for index in 0..32usize {
            backing[0x200 + index * 4..0x204 + index * 4]
                .copy_from_slice(&(0xa500_0000 | index as u32).to_le_bytes());
        }
        let (memory, device_memory) = make_owner_backed_memory_manager(gpu_base, cpu_base, &backing);
        let mut maxwell = Maxwell3D::new();
        crate::dirty_flags::setup_dirty_flags(&mut maxwell.dirty.tables);
        maxwell.set_memory_manager(Arc::clone(&memory));
        maxwell.set_native_turing_macro_mode(true);
        maxwell.regs[MESH_CONTROL as usize] = 1;
        for (register, value) in [MESH_SHADER_A, MESH_SHADER_B, MESH_INIT_SHADER]
            .into_iter().zip([0x20204, 0x800000, 0x2c0b020]) {
            maxwell.regs[register as usize] = value;
        }
        for index in [1, 3, 4] {
            let slot = (PIPELINE_BASE + index * PIPELINE_STRIDE) as usize;
            maxwell.regs[slot] = 1;
            maxwell.regs[slot + 5] = 0xffff_ff01;
            maxwell.regs[slot + 6] = if index == 4 { 0x200 } else { 0 };
        }
        let mut channel = ChannelState::new(7);
        channel.memory_manager = Some(memory);
        channel.maxwell_3d = Some(Box::new(maxwell));
        let mut cache = ShaderCache::new(device_memory);
        cache.create_channel(&channel);
        cache.bind_to_channel(7);
        // Existing code identity must be specialized even on a lookup hit.
        cache.register(Box::new(ShaderInfo { unique_hash: 0x12345678, size_bytes: 16 }), cpu_base, 16);
        let mut expected = Vec::new();
        for word in [0x20204u32, 0x800000, 0x2c0b020] { expected.extend_from_slice(&word.to_le_bytes()); }
        expected.extend_from_slice(&backing[0x200..0x250]);
        let expected_hash = common::cityhash::city_hash64(&expected) | 1;
        assert_eq!(cache.mesh_state_hash(), expected_hash);
        let mut hashes = [0; NUM_PROGRAMS];
        assert!(cache.refresh_stages(&mut hashes));
        assert_eq!(hashes[1], 0x12345678 ^ expected_hash);
        assert_eq!(hashes[3], hashes[1]);
        assert_eq!(hashes[4], 0);
        let old = hashes[1];
        channel.maxwell_3d.as_mut().unwrap().call_method(MESH_SHADER_A, 0x80804, true);
        assert!(cache.refresh_stages(&mut hashes));
        assert_ne!(hashes[1], old);
        assert_eq!(hashes[1], 0x12345678 ^ cache.mesh_state_hash());
        assert_eq!(hashes[3], hashes[1]);
        assert_eq!(cache.try_get(cpu_base).unwrap().unique_hash, 0x12345678);
    }
    use crate::shader_environment::GenericEnvironment;
    use parking_lot::Mutex as ParkingLotMutex;
    use std::sync::{Arc, OnceLock};

    fn make_cpu_reader(
        cpu_base: u64,
        backing: Arc<Vec<u8>>,
    ) -> Arc<dyn Fn(u64, &mut [u8]) + Send + Sync> {
        Arc::new(move |cpu_addr: u64, dst: &mut [u8]| {
            dst.fill(0);
            let offset = cpu_addr.saturating_sub(cpu_base) as usize;
            if offset >= backing.len() {
                return;
            }
            let available = backing.len() - offset;
            let count = available.min(dst.len());
            dst[..count].copy_from_slice(&backing[offset..offset + count]);
        })
    }

    fn make_owner_backed_memory_manager(
        gpu_base: u64,
        device_addr: u64,
        backing: &[u8],
    ) -> (
        Arc<ParkingLotMutex<MemoryManager>>,
        Arc<MaxwellDeviceMemoryManager>,
    ) {
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            device_addr,
            backing.as_ptr(),
            0x4000_0000,
            backing.len(),
            1,
            true,
        );
        let memory_manager = Arc::new(ParkingLotMutex::new(
            MemoryManager::new_with_geometry_and_device_memory(
                1,
                Arc::clone(&device_memory),
                40,
                0x1_0000_0000,
                16,
                12,
            ),
        ));
        memory_manager
            .lock()
            .map(gpu_base, device_addr, backing.len() as u64, 0, false);
        (memory_manager, device_memory)
    }

    #[test]
    fn rejected_shaders_preserve_first_error_and_invalidate_only_start_addresses() {
        let mut cache = ShaderCache::default();
        assert_eq!(cache.rejected_lowest.load(Ordering::Relaxed), u64::MAX);
        assert_eq!(cache.rejected_highest.load(Ordering::Relaxed), 0);
        cache.rethrow_if_rejected(0x2000);
        cache.remember_rejection(
            0x2000,
            RejectedShaderException::Runtime(RuntimeError::new("first")),
        );
        cache.remember_rejection(
            0x2000,
            RejectedShaderException::Logic(LogicError::new("second")),
        );
        cache.remember_rejection(
            0x3000,
            RejectedShaderException::Runtime(RuntimeError::new("other")),
        );
        for _ in 0..2 {
            let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cache.rethrow_if_rejected(0x2000)
            }))
            .unwrap_err();
            assert_eq!(error.downcast_ref::<RuntimeError>().unwrap().0, "first");
        }
        cache.on_cache_invalidation(0x1fff, 1); // end is exclusive
        cache.invalidate_region(0x2001, 0xfff); // a write after the start does not clear it
        assert!(cache.is_rejected(0x2000));
        assert!(cache.is_rejected(0x3000));
        cache.on_cache_invalidation(0x2000, 0);
        assert!(cache.is_rejected(0x2000));
        cache.on_cache_invalidation(0x2000, 1); // removal is immediate, before SyncGuestHost
        assert!(!cache.is_rejected(0x2000));
        cache.invalidate_region(0x3000, 1);
        assert!(!cache.is_rejected(0x3000));
        assert_eq!(cache.rejected_lowest.load(Ordering::Relaxed), 0x2000);
        assert_eq!(cache.rejected_highest.load(Ordering::Relaxed), 0x3000);
        cache.remember_rejection(
            0x2000,
            RejectedShaderException::Logic(LogicError::new("retry")),
        );
        let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cache.rethrow_if_rejected(0x2000)
        }))
        .unwrap_err();
        assert_eq!(error.downcast_ref::<LogicError>().unwrap().0, "retry");
    }

    #[test]
    fn rejected_shader_exception_preserves_concrete_type_and_ignores_unrelated_panics() {
        let errors: Vec<Box<dyn std::any::Any + Send>> = vec![
            Box::new(ShaderException::new("base")),
            Box::new(LogicError::new("logic")),
            Box::new(RuntimeError::new("runtime")),
            Box::new(NotImplementedException::new("operation")),
            Box::new(InvalidArgument::new("argument")),
        ];
        for error in errors {
            let expected_type = (*error).type_id();
            let rejected = RejectedShaderException::from_payload(&*error).unwrap();
            let rethrown = std::panic::catch_unwind(|| rejected.rethrow()).unwrap_err();
            assert_eq!((*rethrown).type_id(), expected_type);
        }
        assert!(RejectedShaderException::from_payload(&"assertion failed").is_none());
        assert!(RejectedShaderException::from_payload(&String::from("unrelated panic")).is_none());
    }

    #[test]
    fn shader_cache_starts_with_upstream_shared_state_defaults() {
        let cache = ShaderCache::default();
        assert!(cache.current_channel_info().is_none());
        assert_eq!(cache.shader_info_slots(), &[None; NUM_PROGRAMS]);
        assert!(!cache.last_shaders_valid());
    }

    #[test]
    fn shader_cache_channel_owner_tracks_bound_channel_info() {
        let mut cache = ShaderCache::default();
        let mut channel = ChannelState::new(7);
        channel.program_id = 0x1234;
        channel.memory_manager = Some(Arc::new(ParkingLotMutex::new(MemoryManager::new(0))));
        channel.maxwell_3d = Some(Box::default());
        channel.kepler_compute = Some(Box::default());

        cache.create_channel(&channel);
        cache.bind_to_channel(7);

        let info = cache
            .current_channel_info()
            .expect("channel should be bound into shared shader-cache owner");
        assert_eq!(info.program_id, 0x1234);
        assert_ne!(info.maxwell3d, 0);
        assert_ne!(info.kepler_compute, 0);
        assert!(info.gpu_memory.is_some());
    }

    #[test]
    fn register_creates_lookup_and_invalidation_entries() {
        let mut cache = ShaderCache::default();
        cache.register(
            Box::new(ShaderInfo {
                unique_hash: 0x1234,
                size_bytes: 0x200,
            }),
            0x4000,
            0x200,
        );

        let shader = cache
            .try_get(0x4000)
            .expect("registered shader should be cached");
        assert_eq!(shader.unique_hash, 0x1234);
        assert!(cache
            .invalidation_cache
            .values()
            .any(|entries| !entries.is_empty()));
    }

    #[test]
    fn native_graphics_cache_uses_pipeline_addresses_and_retries_after_source_write() {
        const CHILD: &str = "RUZU_NATIVE_SHADER_REJECTION_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    std::thread::current().name().unwrap(),
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .env_remove("EDEN_NX2_REJECT")
                .env_remove("EDEN_NX2_SHADER_DUMP")
                .env_remove("EDEN_NATIVE_SHADER_OVERRIDE_DIR")
                .env_remove("EDEN_NATIVE_DRAW_DIAGNOSTICS")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let gpu_base = 0x1_4000_0000u64;
        let mut backing = vec![0u8; 0x1000];
        // An invalid native opcode gives a real lowering rejection, not a seeded cache entry.
        backing[128..132].copy_from_slice(&0x7ffu32.to_le_bytes());
        let (memory, device_memory) = make_owner_backed_memory_manager(gpu_base, 0x9000, &backing);
        let mut maxwell = Maxwell3D::new();
        maxwell.set_memory_manager(Arc::clone(&memory));
        maxwell.set_native_turing_macro_mode(true);
        for (method, value) in [
            (0x582, 0),
            (0x583, 0xdead0000),
            (0x811, 0x800),
            (0x815, 0xfeed0001),
            (0x816, 0x40000000),
        ] {
            <Maxwell3D as EngineInterface>::call_method(&mut maxwell, method, value, true);
        }
        let mut channel = ChannelState::new(42);
        channel.memory_manager = Some(memory);
        channel.maxwell_3d = Some(Box::new(maxwell));
        let mut cache = ShaderCache::new(device_memory);
        cache.create_channel(&channel);
        cache.bind_to_channel(42);
        let mut hashes = [0; NUM_PROGRAMS];
        assert!(!cache.refresh_stages(&mut hashes));
        assert!(cache.is_rejected(0x9000));
        assert!(channel
            .maxwell_3d
            .as_ref()
            .unwrap()
            .get_native_macro_engine_read(|engine| engine.native_execution_failed));
        // Repair the source, but leave its remembered rejection intact: this must skip lowering.
        backing[128..144].fill(0);
        backing[128..132].copy_from_slice(&0x794du32.to_le_bytes());
        backing[136..140].copy_from_slice(&0x03800000u32.to_le_bytes());
        channel.maxwell_3d.as_mut().unwrap().dirty.flags
            [crate::dirty_flags::flags::SHADERS as usize] = true;
        assert!(!cache.refresh_stages(&mut hashes));
        cache.on_cache_invalidation(0x9000, 1);
        channel.maxwell_3d.as_mut().unwrap().dirty.flags
            [crate::dirty_flags::flags::SHADERS as usize] = true;
        assert!(cache.refresh_stages(&mut hashes));
        assert_ne!(hashes[1], 0);
        assert_eq!(cache.try_get(0x9000).unwrap().size_bytes, 120);
        let mut envs = GraphicsEnvironments::default();
        cache.get_graphics_environments(&mut envs, &hashes);
        assert_eq!(envs.env_ptrs[0], Some(1));
        assert_eq!(envs.envs[1].generic_environment().source_size_bytes(), 144);
        assert_eq!(envs.envs[1].generic_environment().cached_size_bytes(), 120);
        // Ordinary graphics errors retain their typed propagation instead of failing a native draw.
        cache.invalidate_region(0x9000, 144);
        cache.remember_rejection(
            0x9000,
            RejectedShaderException::Runtime(RuntimeError::new("ordinary error")),
        );
        let maxwell = channel.maxwell_3d.as_mut().unwrap();
        maxwell.set_native_turing_macro_mode(false);
        for (method, value) in [(0x582, 1), (0x583, 0x40000000), (0x811, 0)] {
            <Maxwell3D as EngineInterface>::call_method(maxwell, method, value, true);
        }
        maxwell.dirty.flags[crate::dirty_flags::flags::SHADERS as usize] = true;
        let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cache.refresh_stages(&mut hashes)
        }))
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<RuntimeError>().unwrap().0,
            "ordinary error"
        );
    }

    #[test]
    fn native_shader_invalidation_tracks_source_bytes_not_lowered_size() {
        const CHILD: &str = "RUZU_NATIVE_SHADER_CACHE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    std::thread::current().name().unwrap(),
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .env_remove("EDEN_NX2_REJECT")
                .env_remove("EDEN_NX2_SHADER_DUMP")
                .env_remove("EDEN_NATIVE_SHADER_OVERRIDE_DIR")
                .env_remove("EDEN_NATIVE_DRAW_DIAGNOSTICS")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let gpu_base = 0x4000_0000;
        let mut backing = vec![0u8; 0x1000];
        for (index, word) in [0x794du32, 0, 0x03800000, 0].into_iter().enumerate() {
            backing[128 + index * 4..132 + index * 4].copy_from_slice(&word.to_le_bytes());
        }
        let (memory, device_memory) = make_owner_backed_memory_manager(gpu_base, 0x9000, &backing);
        let mut maxwell = Maxwell3D::new();
        maxwell.set_memory_manager(memory);
        maxwell.set_native_turing_macro_mode(true);
        let mut env =
            GraphicsEnvironment::from_maxwell3d(&maxwell, ShaderStageType::VertexB, gpu_base, 0);
        let mut cache = ShaderCache::new(device_memory);
        let info = cache.make_shader_info(&mut env, 0x4000_0000);
        assert_eq!(info.size_bytes, 120); // converted code, padding and map
        cache.invalidate_region(0x4000_0000 + 144, 1);
        assert!(cache.try_get(0x4000_0000).is_some());
        cache.invalidate_region(0x4000_0000 + 143, 1);
        assert!(cache.try_get(0x4000_0000).is_none());
    }

    #[test]
    fn invalidate_region_erases_current_page_entry_in_place() {
        let mut cache = ShaderCache::default();
        cache.register(
            Box::new(ShaderInfo {
                unique_hash: 0x1234,
                size_bytes: 0x40,
            }),
            0x4000,
            0x40,
        );

        assert!(cache.try_get(0x4000).is_some());
        assert_eq!(
            cache
                .invalidation_cache
                .get(&(0x4000 >> YUZU_PAGEBITS))
                .map(Vec::len),
            Some(1)
        );

        cache.invalidate_region(0x4000, 4);

        assert!(cache.try_get(0x4000).is_none());
        assert!(cache.marked_for_removal.is_empty());
        assert!(cache.storage.is_empty());
        assert_eq!(
            cache
                .invalidation_cache
                .get(&(0x4000 >> YUZU_PAGEBITS))
                .map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn make_shader_info_registers_analyzed_shader() {
        let program_base = 0x1_0000_0000;
        let sentinel_offset = 0x80usize;
        let sentinel = 0xE2400FFFFF87000Fu64;

        let mut backing = vec![0u8; 0x2000];
        backing[sentinel_offset..sentinel_offset + 8].copy_from_slice(&sentinel.to_le_bytes());
        let backing = Arc::new(backing);
        let reader = Arc::new(move |gpu_addr: u64, dst: &mut [u8]| {
            dst.fill(0);
            let offset = (gpu_addr - program_base) as usize;
            if offset >= backing.len() {
                return;
            }
            let available = backing.len() - offset;
            let count = available.min(dst.len());
            dst[..count].copy_from_slice(&backing[offset..offset + count]);
        });

        let env = GenericEnvironment::new()
            .with_gpu_read(reader)
            .with_program(program_base, 0);
        let mut env = ComputeEnvironment::from_generic_environment_for_test(env);
        let _ = env.generic_environment_mut().read_instruction(0);
        let mut cache = ShaderCache::default();

        let shader = cache.make_shader_info(&mut env, 0x9000);
        assert_ne!(shader.unique_hash, 0);
        assert!(shader.size_bytes >= 8);
        assert!(cache.try_get(0x9000).is_some());
    }

    #[test]
    fn make_shader_info_slow_path_walks_branch_target_before_hashing() {
        const PRED_PT: u64 = 7;
        const FLOW_T: u64 = 15;

        fn encode_control_flow(opcode_top16: u64, branch_offset: i32) -> u64 {
            (opcode_top16 << 48)
                | (((branch_offset as u32 as u64) & 0x00FF_FFFF) << 20)
                | (PRED_PT << 16)
                | FLOW_T
        }

        let program_base = 0x2_0000_0000;
        let mut backing = vec![0u8; 0x80];
        let bra = encode_control_flow(0xE240, 0x18);
        let exit = encode_control_flow(0xE300, 0);
        backing[0x08..0x10].copy_from_slice(&bra.to_le_bytes());
        backing[0x28..0x30].copy_from_slice(&exit.to_le_bytes());
        let backing = Arc::new(backing);
        let reader = Arc::new(move |gpu_addr: u64, dst: &mut [u8]| {
            dst.fill(0);
            let offset = (gpu_addr - program_base) as usize;
            if offset >= backing.len() {
                return;
            }
            let available = backing.len() - offset;
            let count = available.min(dst.len());
            dst[..count].copy_from_slice(&backing[offset..offset + count]);
        });

        let env = GenericEnvironment::new()
            .with_gpu_read(reader)
            .with_program(program_base, 0);
        let mut env = ComputeEnvironment::from_generic_environment_for_test(env);
        let mut cache = ShaderCache::default();

        let shader = cache.make_shader_info(&mut env, 0xA000);
        assert_ne!(shader.unique_hash, 0);
        assert_eq!(shader.size_bytes, 0x38);
    }

    #[test]
    fn refresh_stages_hashes_enabled_vertexb_shader_for_bound_channel() {
        let gpu_base = 0x1_0000_0000;
        let cpu_base = 0x2000;
        let mut backing = vec![0u8; 0x2000];
        backing[0x180..0x188].copy_from_slice(&0xE2400FFFFF87000Fu64.to_le_bytes());
        let backing = Arc::new(backing);

        let (memory_manager, device_memory) =
            make_owner_backed_memory_manager(gpu_base, cpu_base, backing.as_slice());

        let mut maxwell = Maxwell3D::new();
        maxwell.set_memory_manager(Arc::clone(&memory_manager));
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x582, 1, true);
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x583, 0, true);
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x810, 1 | (1 << 4), true);
        <Maxwell3D as EngineInterface>::call_method(&mut maxwell, 0x811, 0x100, true);

        let mut channel = ChannelState::new(7);
        channel.program_id = 0x1234;
        channel.memory_manager = Some(Arc::clone(&memory_manager));
        channel.maxwell_3d = Some(Box::new(maxwell));
        channel.kepler_compute = Some(Box::default());

        let mut cache = ShaderCache::new(device_memory);
        cache.create_channel(&channel);
        cache.bind_to_channel(7);

        let stale_disabled_stage = Box::new(ShaderInfo {
            unique_hash: 0xDEAD_BEEF,
            size_bytes: 8,
        });
        cache.shader_infos[0] = Some(&*stale_disabled_stage);

        let mut unique_hashes = [0xBAD0_C0DE; NUM_PROGRAMS];
        assert!(cache.refresh_stages(&mut unique_hashes));
        assert_eq!(unique_hashes[0], 0);
        assert!(
            cache.shader_info_slots()[0].is_some(),
            "Eden leaves the stale info pointer untouched when a stage is disabled"
        );
        assert_ne!(unique_hashes[1], 0);
        assert!(cache.shader_info_slots()[1].is_some());
        assert!(cache.last_shaders_valid());
    }

    #[test]
    fn refresh_stages_respects_shader_dirty_gate() {
        let memory_manager = Arc::new(ParkingLotMutex::new(MemoryManager::new(0)));
        let mut maxwell = Maxwell3D::new();
        maxwell.set_memory_manager(Arc::clone(&memory_manager));
        maxwell.set_guest_memory_reader(make_cpu_reader(0, Arc::new(vec![0; 0x1000])));

        let mut channel = ChannelState::new(8);
        channel.program_id = 0x4321;
        channel.memory_manager = Some(Arc::clone(&memory_manager));
        channel.maxwell_3d = Some(Box::new(maxwell));
        channel.kepler_compute = Some(Box::default());

        let mut cache = ShaderCache::default();
        cache.create_channel(&channel);
        cache.bind_to_channel(8);

        let mut unique_hashes = [0xDEAD_BEEFu64; NUM_PROGRAMS];
        assert!(!cache.refresh_stages(&mut unique_hashes));
        unique_hashes = [0xDEAD_BEEFu64; NUM_PROGRAMS];
        assert!(!cache.refresh_stages(&mut unique_hashes));
        assert_eq!(unique_hashes, [0xDEAD_BEEFu64; NUM_PROGRAMS]);
        assert!(cache.shader_info_slots().iter().all(Option::is_none));
    }

    #[test]
    fn compute_shader_builds_from_bound_channel_compute_state() {
        let gpu_base = 0x1_0000_0000;
        let cpu_base = 0x4000;
        let mut backing = vec![0u8; 0x2000];
        backing[0x180..0x188].copy_from_slice(&0xE2400FFFFF87000Fu64.to_le_bytes());
        let backing = Arc::new(backing);

        let (memory_manager, device_memory) =
            make_owner_backed_memory_manager(gpu_base, cpu_base, backing.as_slice());

        let mut maxwell = Maxwell3D::new();
        maxwell.set_memory_manager(Arc::clone(&memory_manager));

        let mut channel = ChannelState::new(9);
        channel.program_id = 0x5678;
        channel.memory_manager = Some(Arc::clone(&memory_manager));
        channel.maxwell_3d = Some(Box::new(maxwell));
        channel.kepler_compute = Some(Box::default());
        let kepler = channel
            .kepler_compute
            .as_mut()
            .expect("compute engine should exist for bound-channel shader-cache test");
        kepler.call_method(0x582, 1, true);
        kepler.call_method(0x583, 0, true);
        kepler.launch_description = LaunchParams {
            program_start: 0x100,
            block_dim_x: 32,
            block_dim_y: 1,
            block_dim_z: 1,
            shared_alloc: 0x80,
            local_pos_alloc: 0x40,
            ..LaunchParams::default()
        };

        let mut cache = ShaderCache::new(device_memory);
        cache.create_channel(&channel);
        cache.bind_to_channel(9);

        let shader = cache
            .compute_shader()
            .expect("bound compute channel should build a shader info");
        assert_ne!(shader.unique_hash, 0);
        assert!(shader.size_bytes >= 8);
        assert!(cache.try_get(cpu_base + 0x100).is_some());
        cache.invalidate_region(cpu_base + 0x100, 1);
        cache.remember_rejection(
            cpu_base + 0x100,
            RejectedShaderException::Logic(LogicError::new("compute rejection")),
        );
        for _ in 0..2 {
            let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cache.compute_shader();
            }))
            .unwrap_err();
            assert_eq!(
                error.downcast_ref::<LogicError>().unwrap().0,
                "compute rejection"
            );
        }
        cache.on_cache_invalidation(cpu_base + 0x100, 1);
        assert!(cache.compute_shader().is_some());
    }
}

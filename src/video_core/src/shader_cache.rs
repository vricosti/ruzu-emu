// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/shader_cache.h and video_core/shader_cache.cpp
//!
//! Shader binary caching and invalidation.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::control::channel_state::ChannelState;
use crate::control::channel_state_cache::{ChannelInfo, ChannelSetupCaches};
use crate::engines::kepler_compute::KeplerCompute;
use crate::engines::maxwell_3d::{Maxwell3D, ShaderStageType};
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
        }
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

    /// Port of `ShaderCache::RefreshStages`.
    pub fn refresh_stages(&mut self, unique_hashes: &mut [u64; NUM_PROGRAMS]) -> bool {
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

            let shader_addr = base_addr + stage_info.offset as u64;
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
                if program_type == ShaderStageType::Invalid {
                    *unique_hash = 0;
                    continue;
                }
                let mut env = GraphicsEnvironment::from_maxwell3d(
                    maxwell3d,
                    program_type,
                    base_addr,
                    stage_info.offset,
                );
                self.make_shader_info(&mut env, cpu_shader_addr) as *const ShaderInfo
            };

            let shader_info = unsafe { &*shader_ptr };
            self.shader_infos[index] = Some(shader_ptr);
            *unique_hash = shader_info.unique_hash;
        }

        self.last_shaders_valid = true;
        true
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

        let mut env =
            ComputeEnvironment::from_kepler_compute(kepler_compute, Arc::clone(&gpu_memory));
        Some(self.make_shader_info(&mut env, cpu_shader_addr))
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

            let shader_info = unsafe { &*shader_ptr };
            let mut env = GraphicsEnvironment::from_maxwell3d(
                maxwell3d,
                program_type,
                base_addr,
                stage_info.offset,
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
        let size_bytes = info.size_bytes;
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
    use crate::shader_environment::GenericEnvironment;
    use parking_lot::Mutex as ParkingLotMutex;
    use std::sync::Arc;

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
    }
}

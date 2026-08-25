// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/shader_environment.h and video_core/shader_environment.cpp
//!
//! Shader execution environment for reading shader instructions and metadata
//! from GPU memory during shader compilation.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

#[cfg(test)]
use crate::engines::kepler_compute::ConstBufferConfig;
use crate::engines::kepler_compute::KeplerCompute;
#[cfg(test)]
use crate::engines::maxwell_3d::ConstBufferBinding;
use crate::engines::maxwell_3d::{EngineHint, Maxwell3D, SamplerBinding, ShaderStageType};
use crate::memory_manager::MemoryManager;
use crate::texture_cache::format_lookup_table::pixel_format_from_texture_info_raw;
use crate::textures::texture::{texture_pair, TextureType as TegraTextureType, TicEntry};
use common::fs::fs_util::path_to_utf8_string;
use common::fs::path_util::{get_ruzu_path, RuzuPath};
use parking_lot::Mutex as ParkingLotMutex;
use shader_recompiler::exception::LogicError;
use shader_recompiler::program_header::ProgramHeader;

/// GPU virtual address type.
pub type GPUVAddr = u64;

/// Magic number for pipeline cache files.
pub const MAGIC_NUMBER: [u8; 8] = *b"yuzucach";

/// Instruction size in bytes.
const INST_SIZE: usize = 8;

/// `TryFindSize` block fetch granularity. Upstream
/// `GenericEnvironment::TryFindSize` reads 0x1000 bytes per iteration.
const TRY_FIND_SIZE_BLOCK_BYTES: usize = 0x1000;

/// `TryFindSize` upper bound. Upstream caps the search at 0x100000 bytes.
const TRY_FIND_SIZE_MAX_BYTES: usize = 0x100000;

/// Maxwell self-branch sentinel A. Marks the tail of a shader.
/// Upstream: `GenericEnvironment::TryFindSize` line 251.
const SELF_BRANCH_A: u64 = 0xE2400FFFFF87000F;

/// Maxwell self-branch sentinel B. Marks the tail of a shader.
/// Upstream: `GenericEnvironment::TryFindSize` line 252.
const SELF_BRANCH_B: u64 = 0xE2400FFFFF07000F;

/// Maxwell EXIT instruction used as the shader tail on non-proprietary
/// drivers. Upstream deliberately ignores this marker for proprietary-driver
/// shaders, which keep using the self-branch sentinels above.
const EXIT_VALUE: u64 = 0xE30000000007000F;

/// GPU-memory reader callback shape: read `bytes.len()` bytes starting at the
/// given GPU virtual address.
///
/// Rust compatibility helper for reduced callers that do not yet provide the
/// upstream `Tegra::MemoryManager*` owner.
///
/// `Send + Sync` so it can be cloned across threads / fibers safely.
pub type GpuMemoryReader = Arc<dyn Fn(GPUVAddr, &mut [u8]) + Send + Sync>;

/// Shader stage enumeration.
///
/// Re-exported from `shader_recompiler::stage::Stage`, which itself
/// matches upstream `Shader::Stage` exactly. The previous local copy
/// was deleted as part of the cross-crate type-unification pass.
pub use shader_recompiler::stage::Stage as ShaderStage;

// `TextureType`, `TexturePixelFormat`, and `ReplaceConstant` are the
// upstream-faithful types from `shader_recompiler::shader_info`, matching
// the upstream `Shader::TextureType`, `Shader::TexturePixelFormat`, and
// `Shader::ReplaceConstant` declarations. Re-exported here so existing
// consumers of `shader_environment::TextureType` etc. continue to resolve.
//
// The previous local copies had variant-ordering / discriminant mismatches
// compared to upstream (e.g. ReplaceConstant had BaseVertex=0 vs upstream
// BaseInstance=0). Deleted and replaced with re-exports as part of the
// cross-crate type-unification pass.
pub use shader_recompiler::shader_info::{ReplaceConstant, TexturePixelFormat, TextureType};

/// Make a constant buffer key from index and offset.
pub fn make_cbuf_key(index: u32, offset: u32) -> u64 {
    ((index as u64) << 32) | (offset as u64)
}

fn stage_to_prefix(stage: ShaderStage) -> &'static str {
    match stage {
        ShaderStage::VertexB => "VB",
        ShaderStage::TessellationControl => "TC",
        ShaderStage::TessellationEval => "TE",
        ShaderStage::Geometry => "GS",
        ShaderStage::Fragment => "FS",
        ShaderStage::Compute => "CS",
        ShaderStage::VertexA => "VA",
    }
}

fn dump_impl(
    pipeline_hash: u64,
    shader_hash: u64,
    code: &[u64],
    initial_offset: u32,
    stage: ShaderStage,
) {
    let shader_dir = get_ruzu_path(RuzuPath::DumpDir);
    let base_dir = shader_dir.join("shaders");
    if let Err(error) = fs::create_dir_all(&base_dir) {
        log::error!(
            "Failed to create shader dump directories {}: {}",
            path_to_utf8_string(&base_dir),
            error
        );
        return;
    }
    let prefix = stage_to_prefix(stage);
    let path = base_dir.join(format!(
        "{pipeline_hash:016x}_{prefix}_{shader_hash:016x}.ash"
    ));
    let initial_offset = initial_offset as usize;
    if initial_offset % INST_SIZE != 0 || initial_offset > code.len() * INST_SIZE {
        log::error!(
            "shader_environment::dump_impl: invalid initial_offset={} for {:?}",
            initial_offset,
            path
        );
        return;
    }
    let jump_index = initial_offset / INST_SIZE;
    let code_size = code.len() * INST_SIZE - initial_offset;
    let code_bytes =
        unsafe { std::slice::from_raw_parts(code[jump_index..].as_ptr() as *const u8, code_size) };
    let mut file = match std::fs::File::create(&path) {
        Ok(file) => file,
        Err(error) => {
            log::error!(
                "Failed to create shader dump file {}: {}",
                path_to_utf8_string(&path),
                error
            );
            return;
        }
    };
    if let Err(error) = file.write_all(code_bytes) {
        log::error!(
            "Failed to write shader dump file {}: {}",
            path_to_utf8_string(&path),
            error
        );
        return;
    }

    // Upstream pads to the next 32-byte boundary after accounting for the
    // skipped terminal self-branch instruction.
    let padding_needed = (32 - ((code_size + INST_SIZE) % 32)) % 32;
    let zeroes = vec![0u8; INST_SIZE + padding_needed];
    if let Err(error) = file.write_all(&zeroes) {
        log::error!(
            "Failed to finalize shader dump file {}: {}",
            path_to_utf8_string(&path),
            error
        );
    }
}

/// Generic shader environment base.
///
/// Provides instruction reading, constant buffer access, and texture info
/// lookup from GPU memory.
///
/// Upstream: `VideoCommon::GenericEnvironment` (`shader_environment.h:29`),
/// which holds a `Tegra::MemoryManager*` and uses
/// `gpu_memory->Read<u64>` / `gpu_memory->ReadBlock` to fetch shader bytes.
/// The Rust port stores the closest upstream-shaped `MemoryManager` owner.
/// Tests may still inject a [`GpuMemoryReader`] for reduced fixtures that do
/// not construct the full channel/MemoryManager owner graph.
#[derive(Clone)]
pub struct GenericEnvironment {
    texture_pass_caches: shader_recompiler::environment::TexturePassCaches,
    program_base: GPUVAddr,
    start_address: u32,
    stage: ShaderStage,

    code: Vec<u64>,
    texture_types: HashMap<u32, TextureType>,
    texture_pixel_formats: HashMap<u32, TexturePixelFormat>,
    cbuf_values: HashMap<u64, u32>,
    cbuf_replacements: HashMap<u64, ReplaceConstant>,

    local_memory_size: u32,
    texture_bound: u32,
    shared_memory_size: u32,
    workgroup_size: [u32; 3],

    read_lowest: u32,
    read_highest: u32,
    cached_lowest: u32,
    cached_highest: u32,
    initial_offset: u32,
    viewport_transform_state: u32,
    sph: ProgramHeader,
    gp_passthrough_mask: [u32; 8],

    has_unbound_instructions: bool,
    has_hle_engine_state: bool,
    is_proprietary_driver: bool,

    /// GPU memory reader. Rust adaptation of upstream's
    /// stored `Tegra::MemoryManager*` owner. When present, shader reads go
    /// through this owner first.
    gpu_memory: Option<Arc<ParkingLotMutex<MemoryManager>>>,

    /// Test-only GPU-memory reader for reduced fixtures without a
    /// `MemoryManager` owner. Runtime shader environments must use
    /// `gpu_memory`, matching upstream's `Tegra::MemoryManager*`.
    #[cfg(test)]
    gpu_read: Option<GpuMemoryReader>,
}

/// Rust counterpart of accessing the `GenericEnvironment` base subobject
/// through an upstream `Shader::Environment&`.
///
/// The concrete owner must remain available for virtual environment reads;
/// reducing it to `&mut GenericEnvironment` would lose the graphics/compute
/// callbacks required by the Maxwell control-flow analyzer.
pub trait GenericEnvironmentOwner {
    fn generic_environment(&self) -> &GenericEnvironment;
    fn generic_environment_mut(&mut self) -> &mut GenericEnvironment;
}

impl GenericEnvironment {
    pub fn new() -> Self {
        Self {
            texture_pass_caches: Default::default(),
            program_base: 0,
            start_address: 0,
            stage: ShaderStage::VertexB,
            code: Vec::new(),
            texture_types: HashMap::new(),
            texture_pixel_formats: HashMap::new(),
            cbuf_values: HashMap::new(),
            cbuf_replacements: HashMap::new(),
            local_memory_size: 0,
            texture_bound: 0,
            shared_memory_size: 0,
            workgroup_size: [0; 3],
            read_lowest: u32::MAX,
            read_highest: 0,
            cached_lowest: u32::MAX,
            cached_highest: 0,
            initial_offset: 0,
            viewport_transform_state: 1,
            sph: ProgramHeader::default(),
            gp_passthrough_mask: [0; 8],
            has_unbound_instructions: false,
            has_hle_engine_state: false,
            is_proprietary_driver: false,
            gpu_memory: None,
            #[cfg(test)]
            gpu_read: None,
        }
    }

    /// Set the GPU memory reader.
    ///
    /// Rust adaptation of upstream's
    /// `GenericEnvironment::GenericEnvironment(Tegra::MemoryManager& gpu_memory_, ...)`
    /// constructor: stores the reader so subsequent `read_instruction`,
    /// `set_cached_size`, `analyze`, and `calculate_hash` calls can fetch
    /// shader bytes from guest GPU memory.
    #[cfg(test)]
    pub fn with_gpu_read(mut self, reader: GpuMemoryReader) -> Self {
        self.gpu_read = Some(reader);
        self
    }

    /// Store the closest available upstream owner replacement for
    /// `Tegra::MemoryManager*`.
    pub fn with_gpu_memory(mut self, gpu_memory: Arc<ParkingLotMutex<MemoryManager>>) -> Self {
        self.gpu_memory = Some(gpu_memory);
        self
    }

    /// Set program base / start address. Mirrors the upstream constructor's
    /// `program_base_` and `start_address_` parameters.
    pub fn with_program(mut self, program_base: GPUVAddr, start_address: u32) -> Self {
        self.program_base = program_base;
        self.start_address = start_address;
        self
    }

    /// Rust adaptation for owner-local stage assignment on reduced callers
    /// that construct `GenericEnvironment` directly.
    pub fn with_stage(mut self, stage: ShaderStage) -> Self {
        self.stage = stage;
        self
    }

    /// Rust adaptation for callers that construct the generic base owner
    /// directly instead of going through `GraphicsEnvironment`.
    pub fn with_initial_offset(mut self, initial_offset: u32) -> Self {
        self.initial_offset = initial_offset;
        self
    }

    /// Whether this environment can perform runtime GPU reads.
    ///
    /// Upstream stores a non-null `Tegra::MemoryManager*` in
    /// `GenericEnvironment`. Rust default environments are possible before a
    /// stage slot is populated, so callers must not ask them to analyze.
    pub fn has_runtime_gpu_memory_owner(&self) -> bool {
        let has_owner = self.gpu_memory.is_some();
        #[cfg(test)]
        let has_owner = has_owner || self.gpu_read.is_some();
        has_owner
    }

    /// Port of the upstream base-owner `StartAddress()` accessor.
    pub fn start_address(&self) -> u32 {
        self.start_address
    }

    pub fn texture_bound_buffer(&self) -> u32 {
        self.texture_bound
    }

    pub fn local_memory_size(&self) -> u32 {
        self.local_memory_size
    }

    pub fn shared_memory_size(&self) -> u32 {
        self.shared_memory_size
    }

    pub fn workgroup_size(&self) -> [u32; 3] {
        self.workgroup_size
    }

    /// Read an instruction at the given address.
    ///
    /// Port of upstream `GenericEnvironment::ReadInstruction`
    /// (`shader_environment.cpp:141`).
    pub fn read_instruction(&mut self, address: u32) -> u64 {
        self.read_lowest = self.read_lowest.min(address);
        self.read_highest = self.read_highest.max(address);

        if address >= self.cached_lowest && address < self.cached_highest {
            return self.code[((address - self.cached_lowest) / INST_SIZE as u32) as usize];
        }
        self.has_unbound_instructions = true;
        // Upstream: `gpu_memory->Read<u64>(program_base + address);`
        // We use the injected reader. With no reader installed, this slot
        // is unreachable through the normal pipeline-build path (the cache
        // gates pipeline creation on `has_gpu_memory_reader`).
        let mut buf = [0u8; INST_SIZE];
        if !self.read_gpu_bytes(self.program_base + address as u64, &mut buf) {
            return 0;
        }
        u64::from_le_bytes(buf)
    }

    /// Try to analyze the shader and return its CityHash64 hash.
    ///
    /// Port of upstream `GenericEnvironment::Analyze` (cpp:152).
    pub fn analyze(&mut self) -> Option<u64> {
        let size = match self.try_find_size() {
            Some(size) => size,
            None => {
                if std::env::var_os("RUZU_TRACE_SHADER_ANALYZE").is_some() {
                    let first_words: Vec<String> = self
                        .code
                        .iter()
                        .take(8)
                        .map(|word| format!("{word:016X}"))
                        .collect();
                    eprintln!(
                        "[SHADER_ANALYZE] try_find_size_failed program_base=0x{:X} start=0x{:X} cached_words={} first_words=[{}]",
                        self.program_base,
                        self.start_address,
                        self.code.len(),
                        first_words.join(","),
                    );
                }
                return None;
            }
        };
        self.cached_lowest = self.start_address;
        self.cached_highest = self.start_address + size as u32;
        let bytes = self.code_bytes(size as usize);
        if std::env::var_os("RUZU_TRACE_SHADER_ANALYZE").is_some() {
            eprintln!(
                "[SHADER_ANALYZE] try_find_size_ok program_base=0x{:X} start=0x{:X} size=0x{:X} cached_size=0x{:X}",
                self.program_base,
                self.start_address,
                size,
                self.cached_size_bytes(),
            );
        }
        Some(common::cityhash::city_hash64(bytes))
    }

    /// Walk GPU memory in `BLOCK_SIZE` chunks looking for the
    /// SELF_BRANCH_A / SELF_BRANCH_B sentinels that mark the shader's
    /// tail. Returns the byte offset of the sentinel, or `None` if no
    /// sentinel is found within `MAXIMUM_SIZE` bytes.
    ///
    /// Port of upstream `GenericEnvironment::TryFindSize`
    /// (`shader_environment.cpp:247`).
    fn try_find_size(&mut self) -> Option<u64> {
        let mut guest_addr = self.program_base + self.start_address as u64;
        let mut offset: usize = 0;
        let mut size: usize = TRY_FIND_SIZE_BLOCK_BYTES;
        while size <= TRY_FIND_SIZE_MAX_BYTES {
            self.code.resize(size / INST_SIZE, 0);

            // Read the next BLOCK_SIZE chunk into `code` at `offset`.
            let words_offset = offset / INST_SIZE;
            let words_in_block = TRY_FIND_SIZE_BLOCK_BYTES / INST_SIZE;
            let mut block_bytes = [0u8; TRY_FIND_SIZE_BLOCK_BYTES];
            if !self.read_gpu_bytes(guest_addr, &mut block_bytes) {
                return None;
            }
            for i in 0..words_in_block {
                let chunk: [u8; 8] = block_bytes[i * 8..(i + 1) * 8]
                    .try_into()
                    .expect("8-byte chunk");
                self.code[words_offset + i] = u64::from_le_bytes(chunk);
            }

            // Scan this block for the self-branch sentinels. Match upstream
            // exactly: byte index in [0, BLOCK_SIZE) stepping by INST_SIZE.
            for index in (0..TRY_FIND_SIZE_BLOCK_BYTES).step_by(INST_SIZE) {
                let inst = self.code[words_offset + index / INST_SIZE];
                if inst == SELF_BRANCH_A || inst == SELF_BRANCH_B {
                    if std::env::var_os("RUZU_TRACE_SHADER_WORDS").is_some() {
                        let matched_byte_offset = offset + index;
                        eprintln!(
                            "[TRY_FIND_SIZE] sentinel matched at byte_offset=0x{:X} word_index={} sentinel=0x{:016X} (start_address=0x{:X} program_base=0x{:X})",
                            matched_byte_offset,
                            words_offset + index / INST_SIZE,
                            inst,
                            self.start_address,
                            self.program_base,
                        );
                    }
                    return Some((offset + index) as u64);
                }
                if !self.is_proprietary_driver && inst == EXIT_VALUE {
                    return Some((offset + index + INST_SIZE) as u64);
                }
            }

            guest_addr += TRY_FIND_SIZE_BLOCK_BYTES as u64;
            size += TRY_FIND_SIZE_BLOCK_BYTES;
            offset += TRY_FIND_SIZE_BLOCK_BYTES;
        }
        None
    }

    /// Set the cached code size and fetch the bytes from GPU memory.
    ///
    /// Port of upstream `GenericEnvironment::SetCachedSize` (cpp:162).
    pub fn set_cached_size(&mut self, size_bytes: usize) {
        self.cached_lowest = self.start_address;
        self.cached_highest = self.start_address + size_bytes as u32;
        self.code.resize(self.cached_size_words(), 0);
        // Upstream: `gpu_memory->ReadBlock(program_base + cached_lowest,
        //                                  code.data(), code.size() * sizeof(u64));`
        let bytes_to_read = self.code.len() * INST_SIZE;
        let mut buf = vec![0u8; bytes_to_read];
        if self.read_gpu_bytes(self.program_base + self.cached_lowest as u64, &mut buf) {
            for (i, chunk) in buf.chunks_exact(8).enumerate() {
                self.code[i] = u64::from_le_bytes(chunk.try_into().expect("8 bytes"));
            }
        }
    }

    pub fn cached_size_words(&self) -> usize {
        self.cached_size_bytes() / INST_SIZE
    }

    pub fn cached_size_bytes(&self) -> usize {
        (self.cached_highest as usize) - (self.cached_lowest as usize) + INST_SIZE
    }

    /// Borrow the cached shader body as instruction words.
    ///
    /// This is the owner-local replacement for foreign-file reads of the
    /// upstream base-class `code`/`cached_highest` members.
    pub fn cached_code_slice(&self) -> &[u64] {
        let words = self.cached_size_words();
        &self.code[..words.min(self.code.len())]
    }

    /// Byte offset corresponding to `cached_code_slice()[0]`.
    pub fn cached_code_start(&self) -> u32 {
        self.cached_lowest
    }

    /// Borrow the executable shader instructions after the SPH/program header.
    ///
    /// Upstream OpenGL builds the Maxwell CFG from
    /// `env.StartAddress() + sizeof(Shader::ProgramHeader)`, while the cached
    /// code buffer starts at `env.StartAddress()` so it can also contain the
    /// SPH. This accessor keeps that ownership decision in the environment
    /// file until the Rust recompiler takes an `Environment` directly.
    pub fn cached_instruction_slice(&self) -> &[u64] {
        let code = self.cached_code_slice();
        let words_to_skip = (self.initial_offset / INST_SIZE as u32) as usize;
        if words_to_skip >= code.len() {
            &[]
        } else {
            &code[words_to_skip..]
        }
    }

    /// Byte offset corresponding to `cached_instruction_slice()[0]`.
    pub fn cached_instruction_start(&self) -> u32 {
        self.cached_lowest.saturating_add(self.initial_offset)
    }

    /// Port of the upstream `Environment::ShaderStage()` view for reduced
    /// callers that operate on the generic base owner directly.
    pub fn shader_stage(&self) -> ShaderStage {
        self.stage
    }

    pub fn read_size_bytes(&self) -> usize {
        (self.read_highest as usize) - (self.read_lowest as usize) + INST_SIZE
    }

    pub fn can_be_serialized(&self) -> bool {
        !self.has_unbound_instructions
    }

    /// Hash the slice of GPU memory actually touched by translation.
    ///
    /// Port of upstream `GenericEnvironment::CalculateHash` (cpp:185).
    pub fn calculate_hash(&self) -> u64 {
        let size = self.read_size_bytes();
        let mut buf = vec![0u8; size];
        if !self.read_gpu_bytes(self.program_base + self.read_lowest as u64, &mut buf) {
            return 0;
        }
        common::cityhash::city_hash64(&buf)
    }

    pub fn has_hle_macro_state(&self) -> bool {
        self.has_hle_engine_state
    }

    /// Port of upstream protected `GenericEnvironment::ReadTextureInfo(...)`.
    fn read_texture_info(
        &self,
        tic_addr: GPUVAddr,
        tic_limit: u32,
        via_header_index: bool,
        raw: u32,
    ) -> TicEntry {
        let (tic_index, _) = texture_pair(raw, via_header_index);
        if tic_index > tic_limit {
            // Upstream uses ASSERT here. ASSERT is fail-soft unless the user
            // explicitly enables debug asserts, so a Rust `assert!` would
            // incorrectly kill the GPU thread in normal release builds.
            log::error!(
                "shader_environment.cpp: assert handle.first <= tic_limit (tic_index={tic_index:#x}, tic_limit={tic_limit:#x})"
            );
            if *common::settings::values().use_debug_asserts.get_value() {
                panic!("assertion failed: tic_index <= tic_limit");
            }
        }
        let mut raw_bytes = [0u8; 32];
        if let Some(gpu_memory) = self.gpu_memory.as_ref() {
            gpu_memory
                .lock()
                .read_block(tic_addr + tic_index as u64 * 32, &mut raw_bytes);
        } else {
            #[cfg(test)]
            if let Some(reader) = self.gpu_read.as_ref() {
                reader(tic_addr + tic_index as u64 * 32, &mut raw_bytes);
            } else {
                return TicEntry::default();
            }
            #[cfg(not(test))]
            return TicEntry::default();
        }
        // SAFETY: TicEntry is a repr(C) wrapper around [u64; 4], so every
        // 32-byte bit pattern is valid. read_unaligned matches upstream's
        // ReadBlock directly into a TICEntry without imposing host alignment.
        unsafe { std::ptr::read_unaligned(raw_bytes.as_ptr().cast::<TicEntry>()) }
    }

    pub fn dump(&mut self, pipeline_hash: u64, shader_hash: u64) {
        dump_impl(
            pipeline_hash,
            shader_hash,
            &self.code,
            self.initial_offset,
            self.stage,
        );
    }

    /// Borrow the parsed shader program header captured by `analyze`.
    pub fn sph(&self) -> &ProgramHeader {
        &self.sph
    }

    /// Helper for `analyze`: borrow the first `size` bytes of `code` as a
    /// byte slice. The recompiler stores instructions as `Vec<u64>` and
    /// CityHash64 wants `&[u8]`, so we reinterpret in place rather than
    /// re-fetch the bytes from GPU memory.
    fn code_bytes(&self, size: usize) -> &[u8] {
        let total_bytes = self.code.len() * INST_SIZE;
        let len = size.min(total_bytes);
        unsafe { std::slice::from_raw_parts(self.code.as_ptr() as *const u8, len) }
    }

    fn read_gpu_bytes(&self, gpu_addr: GPUVAddr, output: &mut [u8]) -> bool {
        if let Some(gpu_memory) = self.gpu_memory.as_ref() {
            gpu_memory.lock().read_block(gpu_addr, output);
        } else {
            #[cfg(test)]
            if let Some(reader) = self.gpu_read.as_ref() {
                reader(gpu_addr, output);
                return true;
            }
            #[cfg(test)]
            return false;
            #[cfg(not(test))]
            panic!("GenericEnvironment runtime GPU read requires a live MemoryManager owner");
        }
        true
    }

    fn read_u32(&self, gpu_addr: GPUVAddr) -> u32 {
        let mut bytes = [0u8; 4];
        if !self.read_gpu_bytes(gpu_addr, &mut bytes) {
            return 0;
        }
        u32::from_le_bytes(bytes)
    }
}

impl Default for GenericEnvironment {
    fn default() -> Self {
        Self::new()
    }
}

/// Graphics shader environment.
pub struct GraphicsEnvironment {
    base: GenericEnvironment,
    maxwell3d: *const Maxwell3D,
    stage_index: usize,
    file_backed: bool,
    /// Rust-only fallback state for tests. Upstream live-owner paths read
    /// these values directly from `maxwell3d`.
    #[cfg(test)]
    detached_state: GraphicsEnvironmentDetachedState,
}

#[cfg(test)]
#[derive(Clone, Copy)]
struct GraphicsEnvironmentDetachedState {
    const_buffers: [ConstBufferBinding; 18],
    tex_header_pool_addr: u64,
    tex_header_pool_limit: u32,
    sampler_binding: SamplerBinding,
}

impl GraphicsEnvironment {
    pub fn new() -> Self {
        Self {
            base: GenericEnvironment::new(),
            maxwell3d: std::ptr::null(),
            stage_index: 0,
            file_backed: false,
            #[cfg(test)]
            detached_state: GraphicsEnvironmentDetachedState {
                const_buffers: [ConstBufferBinding::default(); 18],
                tex_header_pool_addr: 0,
                tex_header_pool_limit: 0,
                sampler_binding: SamplerBinding::Independently,
            },
        }
    }

    /// Port of `GraphicsEnvironment::GraphicsEnvironment(...)`.
    pub fn from_maxwell3d(
        maxwell3d: &Maxwell3D,
        program: ShaderStageType,
        program_base: GPUVAddr,
        start_address: u32,
    ) -> Self {
        let mut env = Self::new();
        env.maxwell3d = maxwell3d as *const Maxwell3D;
        env.base = GenericEnvironment::new().with_program(program_base, start_address);
        #[cfg(not(test))]
        {
            let gpu_memory = maxwell3d.memory_manager().expect(
                "GraphicsEnvironment runtime construction requires a live MemoryManager owner",
            );
            env.base = std::mem::take(&mut env.base).with_gpu_memory(gpu_memory);
        }
        #[cfg(test)]
        {
            if let Some(gpu_memory) = maxwell3d.memory_manager() {
                env.base = std::mem::take(&mut env.base).with_gpu_memory(gpu_memory);
            } else if let Some(gpu_reader) = maxwell3d.guest_memory_reader() {
                env.base = std::mem::take(&mut env.base).with_gpu_read(gpu_reader);
            }
        }
        match program {
            ShaderStageType::VertexA => {
                env.base.stage = ShaderStage::VertexA;
                env.stage_index = 0;
            }
            ShaderStageType::VertexB => {
                env.base.stage = ShaderStage::VertexB;
                env.stage_index = 0;
            }
            ShaderStageType::TessInit => {
                env.base.stage = ShaderStage::TessellationControl;
                env.stage_index = 1;
            }
            ShaderStageType::Tessellation => {
                env.base.stage = ShaderStage::TessellationEval;
                env.stage_index = 2;
            }
            ShaderStageType::Geometry => {
                env.base.stage = ShaderStage::Geometry;
                env.stage_index = 3;
            }
            ShaderStageType::Fragment => {
                env.base.stage = ShaderStage::Fragment;
                env.stage_index = 4;
            }
            ShaderStageType::Invalid => {}
        }
        env.base.initial_offset = std::mem::size_of::<ProgramHeader>() as u32;
        env.base.gp_passthrough_mask = maxwell3d.post_vtg_shader_attrib_skip_mask();
        env.base.texture_bound = maxwell3d.bindless_texture_const_buffer_slot();
        env.base.is_proprietary_driver = env.base.texture_bound == 2;
        env.base.has_hle_engine_state = maxwell3d.engine_state() == EngineHint::OnHleMacro;
        let has_gpu_memory_owner = env.base.gpu_memory.is_some();
        #[cfg(test)]
        let has_gpu_memory_owner = has_gpu_memory_owner || env.base.gpu_read.is_some();
        if has_gpu_memory_owner {
            let mut bytes = [0u8; std::mem::size_of::<ProgramHeader>()];
            env.base
                .read_gpu_bytes(program_base + start_address as u64, &mut bytes);
            for (index, chunk) in bytes.chunks_exact(4).enumerate() {
                env.base.sph.raw[index] =
                    u32::from_le_bytes(chunk.try_into().expect("4-byte SPH word"));
            }
            env.base.local_memory_size = env.base.sph.local_memory_size() as u32
                + env.base.sph.shader_local_memory_crs_size();
        }
        env
    }

    pub fn from_file_environment(env: FileEnvironment) -> Self {
        let stage = env.stage;
        let mut result = Self::new();
        result.stage_index = match stage {
            ShaderStage::VertexA | ShaderStage::VertexB => 0,
            ShaderStage::TessellationControl => 1,
            ShaderStage::TessellationEval => 2,
            ShaderStage::Geometry => 3,
            ShaderStage::Fragment => 4,
            ShaderStage::Compute => 0,
        };
        result.base = env.into_generic_environment();
        result.file_backed = true;
        result
    }

    fn maxwell3d(&self) -> &Maxwell3D {
        unsafe { self.maxwell3d.as_ref() }
            .expect("GraphicsEnvironment requires a live Maxwell3D owner outside tests")
    }

    #[cfg(test)]
    fn graphics_const_buffer_binding_for_test(
        &self,
        cbuf_index: u32,
    ) -> Option<ConstBufferBinding> {
        unsafe { self.maxwell3d.as_ref() }
            .and_then(|maxwell3d| {
                maxwell3d
                    .const_buffer_bindings(self.stage_index)
                    .get(cbuf_index as usize)
                    .copied()
            })
            .or_else(|| {
                self.detached_state
                    .const_buffers
                    .get(cbuf_index as usize)
                    .copied()
            })
    }

    #[cfg(test)]
    fn texture_header_state_for_test(&self) -> (u64, u32, bool) {
        if let Some(maxwell3d) = unsafe { self.maxwell3d.as_ref() } {
            (
                maxwell3d.tex_header_pool_address(),
                maxwell3d.tex_header_pool_limit(),
                maxwell3d.sampler_binding() == SamplerBinding::ViaHeaderBinding,
            )
        } else {
            (
                self.detached_state.tex_header_pool_addr,
                self.detached_state.tex_header_pool_limit,
                self.detached_state.sampler_binding == SamplerBinding::ViaHeaderBinding,
            )
        }
    }

    pub fn read_cbuf_value(&mut self, cbuf_index: u32, cbuf_offset: u32) -> u32 {
        if self.file_backed {
            return self
                .base
                .cbuf_values
                .get(&make_cbuf_key(cbuf_index, cbuf_offset))
                .copied()
                .unwrap_or_else(|| {
                    std::panic::panic_any(LogicError::new("Uncached read texture type"))
                });
        }
        #[cfg(test)]
        let binding = self.graphics_const_buffer_binding_for_test(cbuf_index);
        #[cfg(not(test))]
        let binding = self
            .maxwell3d()
            .const_buffer_bindings(self.stage_index)
            .get(cbuf_index as usize)
            .copied();
        let binding = binding.unwrap_or_else(|| {
            panic!(
                "GraphicsEnvironment::read_cbuf_value: invalid cbuf index {} for stage {}",
                cbuf_index, self.stage_index
            )
        });
        assert!(
            binding.enabled,
            "GraphicsEnvironment::read_cbuf_value: disabled cbuf {} for stage {}",
            cbuf_index, self.stage_index
        );
        if std::env::var_os("RUZU_TRACE_SHADER_WORDS").is_some() {
            eprintln!(
                "[SHADER_CBUF_READ] stage_index={} cbuf={} offset=0x{:X} addr=0x{:X} size=0x{:X} enabled={}",
                self.stage_index,
                cbuf_index,
                cbuf_offset,
                binding.address,
                binding.size,
                binding.enabled,
            );
        }
        let mut value = 0u32;
        if cbuf_offset < binding.size {
            value = self.base.read_u32(binding.address + cbuf_offset as u64);
        }
        self.base
            .cbuf_values
            .insert(make_cbuf_key(cbuf_index, cbuf_offset), value);
        value
    }

    pub fn read_texture_type(&mut self, handle: u32) -> TextureType {
        if self.file_backed {
            return self
                .base
                .texture_types
                .get(&handle)
                .copied()
                .unwrap_or_else(|| {
                    std::panic::panic_any(LogicError::new("Uncached read texture type"))
                });
        }
        #[cfg(test)]
        let (tex_header_pool_addr, tex_header_pool_limit, via_header_binding) =
            self.texture_header_state_for_test();
        #[cfg(not(test))]
        let maxwell3d = self.maxwell3d();
        #[cfg(not(test))]
        let tex_header_pool_addr = maxwell3d.tex_header_pool_address();
        #[cfg(not(test))]
        let tex_header_pool_limit = maxwell3d.tex_header_pool_limit();
        #[cfg(not(test))]
        let via_header_binding = maxwell3d.sampler_binding() == SamplerBinding::ViaHeaderBinding;
        let entry = self.base.read_texture_info(
            tex_header_pool_addr,
            tex_header_pool_limit,
            via_header_binding,
            handle,
        );
        let result = convert_texture_type(&entry);
        self.base.texture_types.insert(handle, result);
        result
    }

    pub fn read_texture_pixel_format(&mut self, handle: u32) -> TexturePixelFormat {
        if self.file_backed {
            return self
                .base
                .texture_pixel_formats
                .get(&handle)
                .copied()
                .unwrap_or_else(|| {
                    std::panic::panic_any(LogicError::new("Uncached read texture pixel format"))
                });
        }
        #[cfg(test)]
        let (tex_header_pool_addr, tex_header_pool_limit, via_header_binding) =
            self.texture_header_state_for_test();
        #[cfg(not(test))]
        let maxwell3d = self.maxwell3d();
        #[cfg(not(test))]
        let tex_header_pool_addr = maxwell3d.tex_header_pool_address();
        #[cfg(not(test))]
        let tex_header_pool_limit = maxwell3d.tex_header_pool_limit();
        #[cfg(not(test))]
        let via_header_binding = maxwell3d.sampler_binding() == SamplerBinding::ViaHeaderBinding;
        let entry = self.base.read_texture_info(
            tex_header_pool_addr,
            tex_header_pool_limit,
            via_header_binding,
            handle,
        );
        let result = convert_texture_pixel_format(&entry);
        self.base.texture_pixel_formats.insert(handle, result);
        result
    }

    pub fn is_texture_pixel_format_integer(&mut self, handle: u32) -> bool {
        is_integer_pixel_format(self.read_texture_pixel_format(handle))
    }

    pub fn read_viewport_transform_state(&mut self) -> u32 {
        if self.file_backed {
            return self.base.viewport_transform_state;
        }
        self.base.viewport_transform_state = self.maxwell3d().viewport_transform_state();
        self.base.viewport_transform_state
    }

    pub fn get_replace_const_buffer(&mut self, bank: u32, offset: u32) -> Option<ReplaceConstant> {
        if self.file_backed {
            return self
                .base
                .cbuf_replacements
                .get(&make_cbuf_key(bank, offset))
                .copied();
        }
        if !self.base.has_hle_engine_state {
            return None;
        }
        let maxwell3d = unsafe { self.maxwell3d.as_ref() }?;
        let replacement = maxwell3d.get_replace_const_buffer(bank, offset)?;
        self.base
            .cbuf_replacements
            .insert(make_cbuf_key(bank, offset), replacement);
        Some(replacement)
    }

    /// Rust adaptation of upstream private inheritance access to the
    /// `GenericEnvironment` base owner.
    pub fn generic_environment(&self) -> &GenericEnvironment {
        &self.base
    }

    /// Rust adaptation of upstream private inheritance access to the
    /// mutable `GenericEnvironment` base owner.
    pub fn generic_environment_mut(&mut self) -> &mut GenericEnvironment {
        &mut self.base
    }

    /// Owner-local forwarding surface for upstream `GenericEnvironment::SetCachedSize`.
    pub fn set_cached_size(&mut self, size_bytes: usize) {
        self.base.set_cached_size(size_bytes);
    }

    #[cfg(test)]
    fn set_detached_const_buffer_binding(&mut self, index: usize, binding: ConstBufferBinding) {
        self.detached_state.const_buffers[index] = binding;
        self.maxwell3d = std::ptr::null();
    }

    #[cfg(test)]
    fn set_detached_texture_header_pool_state(
        &mut self,
        tex_header_pool_addr: u64,
        tex_header_pool_limit: u32,
        sampler_binding: SamplerBinding,
    ) {
        self.detached_state.tex_header_pool_addr = tex_header_pool_addr;
        self.detached_state.tex_header_pool_limit = tex_header_pool_limit;
        self.detached_state.sampler_binding = sampler_binding;
        self.maxwell3d = std::ptr::null();
    }
}

impl Default for GraphicsEnvironment {
    fn default() -> Self {
        Self::new()
    }
}

impl shader_recompiler::environment::Environment for GraphicsEnvironment {
    fn texture_pass_caches(&mut self) -> &mut shader_recompiler::environment::TexturePassCaches {
        &mut self.base.texture_pass_caches
    }

    fn read_instruction(&mut self, address: u32) -> u64 {
        self.base.read_instruction(address)
    }

    fn read_cbuf_value(&mut self, cbuf_index: u32, cbuf_offset: u32) -> u32 {
        GraphicsEnvironment::read_cbuf_value(self, cbuf_index, cbuf_offset)
    }

    fn read_texture_type(&mut self, raw_handle: u32) -> TextureType {
        GraphicsEnvironment::read_texture_type(self, raw_handle)
    }

    fn read_texture_pixel_format(&mut self, raw_handle: u32) -> TexturePixelFormat {
        GraphicsEnvironment::read_texture_pixel_format(self, raw_handle)
    }

    fn is_texture_pixel_format_integer(&mut self, raw_handle: u32) -> bool {
        GraphicsEnvironment::is_texture_pixel_format_integer(self, raw_handle)
    }

    fn read_viewport_transform_state(&mut self) -> u32 {
        GraphicsEnvironment::read_viewport_transform_state(self)
    }

    fn texture_bound_buffer(&self) -> u32 {
        self.base.texture_bound_buffer()
    }

    fn local_memory_size(&self) -> u32 {
        self.base.local_memory_size()
    }

    fn shared_memory_size(&self) -> u32 {
        self.base.shared_memory_size()
    }

    fn workgroup_size(&self) -> [u32; 3] {
        self.base.workgroup_size()
    }

    fn has_hle_macro_state(&self) -> bool {
        self.base.has_hle_macro_state()
    }

    fn get_replace_const_buffer(&mut self, bank: u32, offset: u32) -> Option<ReplaceConstant> {
        GraphicsEnvironment::get_replace_const_buffer(self, bank, offset)
    }

    fn dump(&mut self, pipeline_hash: u64, shader_hash: u64) {
        self.base.dump(pipeline_hash, shader_hash);
    }

    fn sph(&self) -> &ProgramHeader {
        &self.base.sph
    }

    fn gp_passthrough_mask(&self) -> &[u32; 8] {
        &self.base.gp_passthrough_mask
    }

    fn shader_stage(&self) -> ShaderStage {
        self.base.stage
    }

    fn start_address(&self) -> u32 {
        self.base.start_address
    }

    fn is_proprietary_driver(&self) -> bool {
        self.base.is_proprietary_driver
    }
}

impl GenericEnvironmentOwner for GraphicsEnvironment {
    fn generic_environment(&self) -> &GenericEnvironment {
        GraphicsEnvironment::generic_environment(self)
    }

    fn generic_environment_mut(&mut self) -> &mut GenericEnvironment {
        GraphicsEnvironment::generic_environment_mut(self)
    }
}

/// Compute shader environment.
pub struct ComputeEnvironment {
    base: GenericEnvironment,
    kepler_compute: *const KeplerCompute,
    file_backed: bool,
    /// Rust-only fallback state for tests. Upstream live-owner paths read
    /// these values directly from `kepler_compute`.
    #[cfg(test)]
    detached_state: ComputeEnvironmentDetachedState,
}

#[cfg(test)]
#[derive(Clone, Copy)]
struct ComputeEnvironmentDetachedState {
    const_buffer_enable_mask: u32,
    const_buffers: [ConstBufferConfig; 8],
    tic_address: u64,
    tic_limit: u32,
    linked_tsc: bool,
}

impl ComputeEnvironment {
    pub fn new() -> Self {
        let mut base = GenericEnvironment::new();
        base.stage = ShaderStage::Compute;
        Self {
            base,
            kepler_compute: std::ptr::null(),
            file_backed: false,
            #[cfg(test)]
            detached_state: ComputeEnvironmentDetachedState {
                const_buffer_enable_mask: 0,
                const_buffers: [ConstBufferConfig::default(); 8],
                tic_address: 0,
                tic_limit: 0,
                linked_tsc: false,
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn from_generic_environment_for_test(base: GenericEnvironment) -> Self {
        Self {
            base,
            kepler_compute: std::ptr::null(),
            file_backed: false,
            detached_state: ComputeEnvironmentDetachedState {
                const_buffer_enable_mask: 0,
                const_buffers: [ConstBufferConfig::default(); 8],
                tic_address: 0,
                tic_limit: 0,
                linked_tsc: false,
            },
        }
    }

    /// Port of `ComputeEnvironment::ComputeEnvironment(...)`.
    pub fn from_kepler_compute(
        kepler_compute: &KeplerCompute,
        gpu_memory: Arc<ParkingLotMutex<MemoryManager>>,
    ) -> Self {
        let mut env = Self::new();
        env.kepler_compute = kepler_compute as *const KeplerCompute;
        let qmd = kepler_compute.launch_description();
        env.base = GenericEnvironment::new()
            .with_gpu_memory(gpu_memory)
            .with_program(kepler_compute.code_address(), qmd.program_start);
        env.base.stage = ShaderStage::Compute;
        env.base.local_memory_size = qmd.local_pos_alloc + qmd.local_crs_alloc;
        env.base.texture_bound = kepler_compute.tex_cb_index();
        env.base.is_proprietary_driver = env.base.texture_bound == 2;
        env.base.shared_memory_size = qmd.shared_alloc;
        env.base.workgroup_size = [qmd.block_dim_x, qmd.block_dim_y, qmd.block_dim_z];
        env
    }

    pub fn from_file_environment(env: FileEnvironment) -> Self {
        let mut result = Self::new();
        result.base = env.into_generic_environment();
        result.file_backed = true;
        result
    }

    fn kepler_compute(&self) -> &KeplerCompute {
        unsafe { self.kepler_compute.as_ref() }
            .expect("ComputeEnvironment requires a live KeplerCompute owner outside tests")
    }

    #[cfg(test)]
    fn qmd_for_test(&self) -> Option<(u32, [ConstBufferConfig; 8], u64, u32, bool)> {
        if !self.kepler_compute.is_null() {
            let kepler_compute = self.kepler_compute();
            let qmd = kepler_compute.launch_description();
            Some((
                qmd.const_buffer_enable_mask,
                qmd.const_buffers,
                kepler_compute.tic_address(),
                kepler_compute.tic_limit(),
                qmd.linked_tsc,
            ))
        } else {
            Some((
                self.detached_state.const_buffer_enable_mask,
                self.detached_state.const_buffers,
                self.detached_state.tic_address,
                self.detached_state.tic_limit,
                self.detached_state.linked_tsc,
            ))
        }
    }

    pub fn read_cbuf_value(&mut self, cbuf_index: u32, cbuf_offset: u32) -> u32 {
        if self.file_backed {
            return self
                .base
                .cbuf_values
                .get(&make_cbuf_key(cbuf_index, cbuf_offset))
                .copied()
                .unwrap_or_else(|| {
                    std::panic::panic_any(LogicError::new("Uncached read texture type"))
                });
        }
        #[cfg(test)]
        let (enable_mask, cbufs, _, _, _) = self
            .qmd_for_test()
            .expect("ComputeEnvironment detached test state should always exist");
        #[cfg(not(test))]
        let qmd = self.kepler_compute().launch_description();
        #[cfg(not(test))]
        let enable_mask = qmd.const_buffer_enable_mask;
        #[cfg(not(test))]
        let cbufs = qmd.const_buffers;
        let enabled = ((enable_mask >> cbuf_index) & 1) != 0;
        let cbuf = cbufs.get(cbuf_index as usize).copied().unwrap_or_else(|| {
            panic!(
                "ComputeEnvironment::read_cbuf_value: invalid cbuf index {}",
                cbuf_index
            )
        });
        assert!(
            enabled,
            "ComputeEnvironment::read_cbuf_value: disabled cbuf {}",
            cbuf_index
        );
        let mut value = 0u32;
        if cbuf_offset < cbuf.size {
            value = self.base.read_u32(cbuf.address + cbuf_offset as u64);
        }
        self.base
            .cbuf_values
            .insert(make_cbuf_key(cbuf_index, cbuf_offset), value);
        value
    }

    pub fn read_texture_type(&mut self, handle: u32) -> TextureType {
        if self.file_backed {
            return self
                .base
                .texture_types
                .get(&handle)
                .copied()
                .unwrap_or_else(|| {
                    std::panic::panic_any(LogicError::new("Uncached read texture type"))
                });
        }
        #[cfg(test)]
        let (_, _, tic_address, tic_limit, linked_tsc) = self
            .qmd_for_test()
            .expect("ComputeEnvironment detached test state should always exist");
        #[cfg(not(test))]
        let kepler_compute = self.kepler_compute();
        #[cfg(not(test))]
        let tic_address = kepler_compute.tic_address();
        #[cfg(not(test))]
        let tic_limit = kepler_compute.tic_limit();
        #[cfg(not(test))]
        let linked_tsc = kepler_compute.launch_description().linked_tsc;
        let entry = self
            .base
            .read_texture_info(tic_address, tic_limit, linked_tsc, handle);
        let result = convert_texture_type(&entry);
        self.base.texture_types.insert(handle, result);
        result
    }

    pub fn read_texture_pixel_format(&mut self, handle: u32) -> TexturePixelFormat {
        if self.file_backed {
            return self
                .base
                .texture_pixel_formats
                .get(&handle)
                .copied()
                .unwrap_or_else(|| {
                    std::panic::panic_any(LogicError::new("Uncached read texture pixel format"))
                });
        }
        #[cfg(test)]
        let (_, _, tic_address, tic_limit, linked_tsc) = self
            .qmd_for_test()
            .expect("ComputeEnvironment detached test state should always exist");
        #[cfg(not(test))]
        let kepler_compute = self.kepler_compute();
        #[cfg(not(test))]
        let tic_address = kepler_compute.tic_address();
        #[cfg(not(test))]
        let tic_limit = kepler_compute.tic_limit();
        #[cfg(not(test))]
        let linked_tsc = kepler_compute.launch_description().linked_tsc;
        let entry = self
            .base
            .read_texture_info(tic_address, tic_limit, linked_tsc, handle);
        let result = convert_texture_pixel_format(&entry);
        self.base.texture_pixel_formats.insert(handle, result);
        result
    }

    pub fn is_texture_pixel_format_integer(&mut self, handle: u32) -> bool {
        is_integer_pixel_format(self.read_texture_pixel_format(handle))
    }

    pub fn read_viewport_transform_state(&self) -> u32 {
        self.base.viewport_transform_state
    }

    pub fn get_replace_const_buffer(&self, _bank: u32, _offset: u32) -> Option<ReplaceConstant> {
        None
    }

    /// Rust adaptation of upstream private inheritance access to the
    /// `GenericEnvironment` base owner.
    pub fn generic_environment(&self) -> &GenericEnvironment {
        &self.base
    }

    /// Rust adaptation of upstream private inheritance access to the
    /// mutable `GenericEnvironment` base owner.
    pub fn generic_environment_mut(&mut self) -> &mut GenericEnvironment {
        &mut self.base
    }
}

impl Default for ComputeEnvironment {
    fn default() -> Self {
        Self::new()
    }
}

impl shader_recompiler::environment::Environment for ComputeEnvironment {
    fn texture_pass_caches(&mut self) -> &mut shader_recompiler::environment::TexturePassCaches {
        &mut self.base.texture_pass_caches
    }

    fn read_instruction(&mut self, address: u32) -> u64 {
        self.base.read_instruction(address)
    }

    fn read_cbuf_value(&mut self, cbuf_index: u32, cbuf_offset: u32) -> u32 {
        ComputeEnvironment::read_cbuf_value(self, cbuf_index, cbuf_offset)
    }

    fn read_texture_type(&mut self, raw_handle: u32) -> TextureType {
        ComputeEnvironment::read_texture_type(self, raw_handle)
    }

    fn read_texture_pixel_format(&mut self, raw_handle: u32) -> TexturePixelFormat {
        ComputeEnvironment::read_texture_pixel_format(self, raw_handle)
    }

    fn is_texture_pixel_format_integer(&mut self, raw_handle: u32) -> bool {
        ComputeEnvironment::is_texture_pixel_format_integer(self, raw_handle)
    }

    fn read_viewport_transform_state(&mut self) -> u32 {
        ComputeEnvironment::read_viewport_transform_state(self)
    }

    fn texture_bound_buffer(&self) -> u32 {
        self.base.texture_bound_buffer()
    }

    fn local_memory_size(&self) -> u32 {
        self.base.local_memory_size()
    }

    fn shared_memory_size(&self) -> u32 {
        self.base.shared_memory_size()
    }

    fn workgroup_size(&self) -> [u32; 3] {
        self.base.workgroup_size()
    }

    fn has_hle_macro_state(&self) -> bool {
        self.base.has_hle_macro_state()
    }

    fn get_replace_const_buffer(&mut self, bank: u32, offset: u32) -> Option<ReplaceConstant> {
        ComputeEnvironment::get_replace_const_buffer(self, bank, offset)
    }

    fn dump(&mut self, pipeline_hash: u64, shader_hash: u64) {
        self.base.dump(pipeline_hash, shader_hash);
    }

    fn sph(&self) -> &ProgramHeader {
        &self.base.sph
    }

    fn gp_passthrough_mask(&self) -> &[u32; 8] {
        &self.base.gp_passthrough_mask
    }

    fn shader_stage(&self) -> ShaderStage {
        self.base.stage
    }

    fn start_address(&self) -> u32 {
        self.base.start_address
    }

    fn is_proprietary_driver(&self) -> bool {
        self.base.is_proprietary_driver
    }
}

impl GenericEnvironmentOwner for ComputeEnvironment {
    fn generic_environment(&self) -> &GenericEnvironment {
        ComputeEnvironment::generic_environment(self)
    }

    fn generic_environment_mut(&mut self) -> &mut GenericEnvironment {
        ComputeEnvironment::generic_environment_mut(self)
    }
}

/// File-based shader environment for loading from pipeline cache.
pub struct FileEnvironment {
    texture_pass_caches: shader_recompiler::environment::TexturePassCaches,
    code: Vec<u64>,
    texture_types: HashMap<u32, TextureType>,
    texture_pixel_formats: HashMap<u32, TexturePixelFormat>,
    cbuf_values: HashMap<u64, u32>,
    cbuf_replacements: HashMap<u64, ReplaceConstant>,
    workgroup_size: [u32; 3],
    local_memory_size: u32,
    shared_memory_size: u32,
    texture_bound: u32,
    read_lowest: u32,
    read_highest: u32,
    initial_offset: u32,
    viewport_transform_state: u32,
    sph: ProgramHeader,
    gp_passthrough_mask: [u32; 8],
    stage: ShaderStage,
    start_address: u32,
    is_proprietary_driver: bool,
}

impl FileEnvironment {
    pub fn new() -> Self {
        Self {
            texture_pass_caches: Default::default(),
            code: Vec::new(),
            texture_types: HashMap::new(),
            texture_pixel_formats: HashMap::new(),
            cbuf_values: HashMap::new(),
            cbuf_replacements: HashMap::new(),
            workgroup_size: [0; 3],
            local_memory_size: 0,
            shared_memory_size: 0,
            texture_bound: 0,
            read_lowest: 0,
            read_highest: 0,
            initial_offset: 0,
            viewport_transform_state: 1,
            sph: ProgramHeader::default(),
            gp_passthrough_mask: [0; 8],
            stage: ShaderStage::VertexB,
            start_address: 0,
            is_proprietary_driver: false,
        }
    }

    /// Deserialize from a pipeline cache file.
    ///
    /// Port of `FileEnvironment::Deserialize`. Reads all fields written by
    /// `GenericEnvironment::Serialize` plus stage-specific trailing fields.
    pub fn deserialize(&mut self, file: &mut std::fs::File) -> std::io::Result<()> {
        use std::io::{Read, Seek};

        let read_u32 = |f: &mut std::fs::File| -> std::io::Result<u32> {
            let mut buf = [0u8; 4];
            f.read_exact(&mut buf)?;
            Ok(u32::from_le_bytes(buf))
        };
        let read_u64 = |f: &mut std::fs::File| -> std::io::Result<u64> {
            let mut buf = [0u8; 8];
            f.read_exact(&mut buf)?;
            Ok(u64::from_le_bytes(buf))
        };

        let code_size = read_u64(file)?;
        let num_texture_types = read_u64(file)?;
        let num_texture_pixel_formats = read_u64(file)?;
        let num_cbuf_values = read_u64(file)?;
        let num_cbuf_replacement_values = read_u64(file)?;
        self.local_memory_size = read_u32(file)?;
        self.texture_bound = read_u32(file)?;
        self.start_address = read_u32(file)?;
        self.read_lowest = read_u32(file)?;
        self.read_highest = read_u32(file)?;
        self.viewport_transform_state = read_u32(file)?;
        let stage_raw = read_u32(file)?;
        self.stage = deserialize_enum_u32(stage_raw, ShaderStage::VertexA as u32, "ShaderStage")?;

        let trailing_size = if self.stage == ShaderStage::Compute {
            4 * std::mem::size_of::<u32>() as u64
        } else {
            std::mem::size_of::<ProgramHeader>() as u64
                + if self.stage == ShaderStage::Geometry {
                    std::mem::size_of::<[u32; 8]>() as u64
                } else {
                    0
                }
        };
        let payload_size = code_size
            .checked_add(num_texture_types.checked_mul(8).ok_or_else(|| {
                invalid_cache_data("texture-type count overflows the cache entry")
            })?)
            .and_then(|size| size.checked_add(num_texture_pixel_formats.checked_mul(8)?))
            .and_then(|size| size.checked_add(num_cbuf_values.checked_mul(12)?))
            .and_then(|size| size.checked_add(num_cbuf_replacement_values.checked_mul(12)?))
            .and_then(|size| size.checked_add(trailing_size))
            .ok_or_else(|| invalid_cache_data("shader-environment payload size overflows"))?;
        let position = file.stream_position()?;
        let remaining = file
            .metadata()?
            .len()
            .checked_sub(position)
            .ok_or_else(|| invalid_cache_data("cache cursor is past the end of the file"))?;
        if payload_size > remaining {
            return Err(invalid_cache_data(format!(
                "shader-environment payload is {payload_size} bytes with only {remaining} remaining"
            )));
        }

        // Read code words (code_size bytes, rounded up to u64 boundary)
        let code_size = usize::try_from(code_size)
            .map_err(|_| invalid_cache_data("shader code size does not fit host usize"))?;
        let num_words = code_size.div_ceil(8);
        if num_words > isize::MAX as usize / std::mem::size_of::<u64>() {
            return Err(invalid_cache_data("shader code allocation is too large"));
        }
        self.code
            .try_reserve_exact(num_words)
            .map_err(|_| invalid_cache_data("shader code allocation is too large"))?;
        self.code.resize(num_words, 0);
        let code_bytes =
            unsafe { std::slice::from_raw_parts_mut(self.code.as_mut_ptr() as *mut u8, code_size) };
        file.read_exact(code_bytes)?;

        let num_texture_types = usize::try_from(num_texture_types)
            .map_err(|_| invalid_cache_data("texture-type count does not fit host usize"))?;
        let num_texture_pixel_formats = usize::try_from(num_texture_pixel_formats)
            .map_err(|_| invalid_cache_data("texture-format count does not fit host usize"))?;
        let num_cbuf_values = usize::try_from(num_cbuf_values)
            .map_err(|_| invalid_cache_data("constant-buffer count does not fit host usize"))?;
        let num_cbuf_replacement_values = usize::try_from(num_cbuf_replacement_values)
            .map_err(|_| invalid_cache_data("replacement count does not fit host usize"))?;
        self.texture_types
            .try_reserve(num_texture_types)
            .map_err(|_| invalid_cache_data("texture-type allocation is too large"))?;
        self.texture_pixel_formats
            .try_reserve(num_texture_pixel_formats)
            .map_err(|_| invalid_cache_data("texture-format allocation is too large"))?;
        self.cbuf_values
            .try_reserve(num_cbuf_values)
            .map_err(|_| invalid_cache_data("constant-buffer allocation is too large"))?;
        self.cbuf_replacements
            .try_reserve(num_cbuf_replacement_values)
            .map_err(|_| invalid_cache_data("replacement allocation is too large"))?;

        for _ in 0..num_texture_types {
            let key = read_u32(file)?;
            let type_raw = read_u32(file)?;
            let texture_type =
                deserialize_enum_u32(type_raw, TextureType::Color2DRect as u32, "TextureType")?;
            self.texture_types.insert(key, texture_type);
        }
        for _ in 0..num_texture_pixel_formats {
            let key = read_u32(file)?;
            let fmt_raw = read_u32(file)?;
            let fmt = deserialize_enum_u32(
                fmt_raw,
                TexturePixelFormat::D32FloatS8Uint as u32,
                "TexturePixelFormat",
            )?;
            self.texture_pixel_formats.insert(key, fmt);
        }
        for _ in 0..num_cbuf_values {
            let key = read_u64(file)?;
            let value = read_u32(file)?;
            self.cbuf_values.insert(key, value);
        }
        for _ in 0..num_cbuf_replacement_values {
            let key = read_u64(file)?;
            let rc_raw = read_u32(file)?;
            let rc =
                deserialize_enum_u32(rc_raw, ReplaceConstant::DrawID as u32, "ReplaceConstant")?;
            self.cbuf_replacements.insert(key, rc);
        }
        // Stage-specific trailing fields
        if self.stage == ShaderStage::Compute {
            self.workgroup_size[0] = read_u32(file)?;
            self.workgroup_size[1] = read_u32(file)?;
            self.workgroup_size[2] = read_u32(file)?;
            self.shared_memory_size = read_u32(file)?;
            self.initial_offset = 0;
        } else {
            let mut sph_buf = [0u8; std::mem::size_of::<ProgramHeader>()];
            file.read_exact(&mut sph_buf)?;
            for (index, chunk) in sph_buf.chunks_exact(4).enumerate() {
                self.sph.raw[index] =
                    u32::from_le_bytes(chunk.try_into().expect("4-byte SPH word"));
            }
            self.initial_offset = std::mem::size_of::<ProgramHeader>() as u32;
            if self.stage == ShaderStage::Geometry {
                for item in &mut self.gp_passthrough_mask {
                    *item = read_u32(file)?;
                }
            }
        }
        self.is_proprietary_driver = self.texture_bound == 2;
        Ok(())
    }

    pub fn read_instruction(&self, address: u32) -> u64 {
        if address < self.read_lowest || address > self.read_highest {
            std::panic::panic_any(LogicError::new(format!("Out of bounds address {address}")));
        }
        self.code[((address - self.read_lowest) / 8) as usize]
    }

    pub fn read_cbuf_value(&self, cbuf_index: u32, cbuf_offset: u32) -> u32 {
        let key = make_cbuf_key(cbuf_index, cbuf_offset);
        self.cbuf_values
            .get(&key)
            .copied()
            .unwrap_or_else(|| std::panic::panic_any(LogicError::new("Uncached read texture type")))
    }

    pub fn read_texture_type(&self, handle: u32) -> TextureType {
        self.texture_types
            .get(&handle)
            .copied()
            .unwrap_or_else(|| std::panic::panic_any(LogicError::new("Uncached read texture type")))
    }

    pub fn read_texture_pixel_format(&self, handle: u32) -> TexturePixelFormat {
        self.texture_pixel_formats
            .get(&handle)
            .copied()
            .unwrap_or_else(|| {
                std::panic::panic_any(LogicError::new("Uncached read texture pixel format"))
            })
    }

    pub fn is_texture_pixel_format_integer(&self, handle: u32) -> bool {
        is_integer_pixel_format(self.read_texture_pixel_format(handle))
    }

    pub fn read_viewport_transform_state(&self) -> u32 {
        self.viewport_transform_state
    }

    pub fn shader_stage(&self) -> ShaderStage {
        self.stage
    }

    pub fn start_address(&self) -> u32 {
        self.start_address
    }

    pub fn cached_instruction_slice(&self) -> &[u64] {
        let first_word = (self.initial_offset / INST_SIZE as u32) as usize;
        self.code.get(first_word..).unwrap_or(&[])
    }

    pub fn cached_instruction_start(&self) -> u32 {
        self.read_lowest.saturating_add(self.initial_offset)
    }

    fn into_generic_environment(self) -> GenericEnvironment {
        let has_hle_engine_state = !self.cbuf_replacements.is_empty();
        GenericEnvironment {
            texture_pass_caches: self.texture_pass_caches,
            program_base: 0,
            start_address: self.start_address,
            stage: self.stage,
            code: self.code,
            texture_types: self.texture_types,
            texture_pixel_formats: self.texture_pixel_formats,
            cbuf_values: self.cbuf_values,
            cbuf_replacements: self.cbuf_replacements,
            local_memory_size: self.local_memory_size,
            texture_bound: self.texture_bound,
            shared_memory_size: self.shared_memory_size,
            workgroup_size: self.workgroup_size,
            read_lowest: self.read_lowest,
            read_highest: self.read_highest,
            cached_lowest: self.read_lowest,
            cached_highest: self.read_highest,
            initial_offset: self.initial_offset,
            viewport_transform_state: self.viewport_transform_state,
            sph: self.sph,
            gp_passthrough_mask: self.gp_passthrough_mask,
            has_unbound_instructions: false,
            has_hle_engine_state,
            is_proprietary_driver: self.is_proprietary_driver,
            gpu_memory: None,
            #[cfg(test)]
            gpu_read: None,
        }
    }

    pub fn has_hle_macro_state(&self) -> bool {
        !self.cbuf_replacements.is_empty()
    }

    pub fn get_replace_const_buffer(&self, bank: u32, offset: u32) -> Option<ReplaceConstant> {
        self.cbuf_replacements
            .get(&make_cbuf_key(bank, offset))
            .copied()
    }

    pub fn dump(&mut self, pipeline_hash: u64, shader_hash: u64) {
        dump_impl(
            pipeline_hash,
            shader_hash,
            &self.code,
            self.initial_offset,
            self.stage,
        );
    }
}

impl Default for FileEnvironment {
    fn default() -> Self {
        Self::new()
    }
}

impl shader_recompiler::environment::Environment for FileEnvironment {
    fn texture_pass_caches(&mut self) -> &mut shader_recompiler::environment::TexturePassCaches {
        &mut self.texture_pass_caches
    }

    fn is_file_environment(&self) -> bool {
        true
    }

    fn read_instruction(&mut self, address: u32) -> u64 {
        FileEnvironment::read_instruction(self, address)
    }

    fn read_cbuf_value(&mut self, cbuf_index: u32, cbuf_offset: u32) -> u32 {
        FileEnvironment::read_cbuf_value(self, cbuf_index, cbuf_offset)
    }

    fn read_texture_type(&mut self, raw_handle: u32) -> TextureType {
        FileEnvironment::read_texture_type(self, raw_handle)
    }

    fn read_texture_pixel_format(&mut self, raw_handle: u32) -> TexturePixelFormat {
        FileEnvironment::read_texture_pixel_format(self, raw_handle)
    }

    fn is_texture_pixel_format_integer(&mut self, raw_handle: u32) -> bool {
        FileEnvironment::is_texture_pixel_format_integer(self, raw_handle)
    }

    fn read_viewport_transform_state(&mut self) -> u32 {
        FileEnvironment::read_viewport_transform_state(self)
    }

    fn texture_bound_buffer(&self) -> u32 {
        self.texture_bound
    }

    fn local_memory_size(&self) -> u32 {
        self.local_memory_size
    }

    fn shared_memory_size(&self) -> u32 {
        self.shared_memory_size
    }

    fn workgroup_size(&self) -> [u32; 3] {
        self.workgroup_size
    }

    fn has_hle_macro_state(&self) -> bool {
        FileEnvironment::has_hle_macro_state(self)
    }

    fn get_replace_const_buffer(&mut self, bank: u32, offset: u32) -> Option<ReplaceConstant> {
        FileEnvironment::get_replace_const_buffer(self, bank, offset)
    }

    fn dump(&mut self, pipeline_hash: u64, shader_hash: u64) {
        FileEnvironment::dump(self, pipeline_hash, shader_hash);
    }

    fn sph(&self) -> &ProgramHeader {
        &self.sph
    }

    fn gp_passthrough_mask(&self) -> &[u32; 8] {
        &self.gp_passthrough_mask
    }

    fn shader_stage(&self) -> ShaderStage {
        self.stage
    }

    fn start_address(&self) -> u32 {
        self.start_address
    }

    fn is_proprietary_driver(&self) -> bool {
        self.is_proprietary_driver
    }
}

/// Serialize a pipeline to the cache file.
///
/// Port of `VideoCommon::SerializePipeline`.
/// Appends a pipeline entry to the binary cache file at `filename`.
/// If the file is new (empty), writes the magic header first.
/// Only serializes if all environments can be serialized (no unbound instructions).
pub fn serialize_pipeline(
    key: &[u8],
    envs: &[&GenericEnvironment],
    filename: &Path,
    cache_version: u32,
) {
    use std::io::Write;

    if envs.is_empty() {
        log::error!("serialize_pipeline: refusing to write a pipeline without environments");
        return;
    }

    // All envs must be serializable
    if !envs.iter().all(|e| e.can_be_serialized()) {
        return;
    }

    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(filename)
    {
        Ok(file) => file,
        Err(e) => {
            log::error!("serialize_pipeline: failed to open {:?}: {}", filename, e);
            return;
        }
    };

    let write_result = (|| -> std::io::Result<()> {
        if file.metadata()?.len() == 0 {
            file.write_all(&MAGIC_NUMBER)?;
            file.write_all(&cache_version.to_le_bytes())?;
        }

        let num_envs = envs.len() as u32;
        file.write_all(&num_envs.to_le_bytes())?;
        for env in envs {
            serialize_generic_environment(env, &mut file)?;
        }
        file.write_all(key)?;
        Ok(())
    })();

    if let Err(e) = write_result {
        log::error!("serialize_pipeline: {}", e);
        if let Err(remove_error) = std::fs::remove_file(filename) {
            log::error!(
                "serialize_pipeline: failed to delete pipeline cache file {:?}: {}",
                filename,
                remove_error
            );
        }
    }
}

/// Serializes a GenericEnvironment to a file in the binary format used by the pipeline cache.
fn serialize_generic_environment(
    env: &GenericEnvironment,
    file: &mut std::fs::File,
) -> std::io::Result<()> {
    use std::io::Write;

    let code_size = env.cached_size_bytes() as u64;
    let num_texture_types = env.texture_types.len() as u64;
    let num_texture_pixel_formats = env.texture_pixel_formats.len() as u64;
    let num_cbuf_values = env.cbuf_values.len() as u64;
    let num_cbuf_replacement_values = env.cbuf_replacements.len() as u64;

    file.write_all(&code_size.to_le_bytes())?;
    file.write_all(&num_texture_types.to_le_bytes())?;
    file.write_all(&num_texture_pixel_formats.to_le_bytes())?;
    file.write_all(&num_cbuf_values.to_le_bytes())?;
    file.write_all(&num_cbuf_replacement_values.to_le_bytes())?;
    file.write_all(&env.local_memory_size.to_le_bytes())?;
    file.write_all(&env.texture_bound.to_le_bytes())?;
    file.write_all(&env.start_address.to_le_bytes())?;
    file.write_all(&env.cached_lowest.to_le_bytes())?;
    file.write_all(&env.cached_highest.to_le_bytes())?;
    file.write_all(&env.viewport_transform_state.to_le_bytes())?;
    let stage_raw = env.stage as u32;
    file.write_all(&stage_raw.to_le_bytes())?;

    // Write code as bytes
    let code_byte_slice =
        unsafe { std::slice::from_raw_parts(env.code.as_ptr() as *const u8, code_size as usize) };
    file.write_all(code_byte_slice)?;

    for (&key, &texture_type) in &env.texture_types {
        let type_raw = texture_type as u32;
        file.write_all(&key.to_le_bytes())?;
        file.write_all(&type_raw.to_le_bytes())?;
    }
    for (&key, &fmt) in &env.texture_pixel_formats {
        file.write_all(&key.to_le_bytes())?;
        file.write_all(&(fmt as u32).to_le_bytes())?;
    }
    for (&key, &val) in &env.cbuf_values {
        file.write_all(&key.to_le_bytes())?;
        file.write_all(&val.to_le_bytes())?;
    }
    for (&key, &rc) in &env.cbuf_replacements {
        let rc_raw = rc as u32;
        file.write_all(&key.to_le_bytes())?;
        file.write_all(&rc_raw.to_le_bytes())?;
    }

    // Stage-specific trailing fields
    if env.stage == ShaderStage::Compute {
        file.write_all(&env.workgroup_size[0].to_le_bytes())?;
        file.write_all(&env.workgroup_size[1].to_le_bytes())?;
        file.write_all(&env.workgroup_size[2].to_le_bytes())?;
        file.write_all(&env.shared_memory_size.to_le_bytes())?;
    } else {
        for word in env.sph.raw {
            file.write_all(&word.to_le_bytes())?;
        }
        if env.stage == ShaderStage::Geometry {
            for item in env.gp_passthrough_mask {
                file.write_all(&item.to_le_bytes())?;
            }
        }
    }

    Ok(())
}

/// Load pipelines from a cache file.
///
/// Port of `VideoCommon::LoadPipelines`.
/// Reads the binary cache file and calls `load_compute` or `load_graphics` for each entry.
pub fn load_pipelines<'a, FStop>(
    stop_loading: FStop,
    filename: &Path,
    expected_cache_version: u32,
    mut load_compute: Box<
        dyn FnMut(&mut std::fs::File, FileEnvironment) -> std::io::Result<()> + 'a,
    >,
    mut load_graphics: Box<
        dyn FnMut(&mut std::fs::File, Vec<FileEnvironment>) -> std::io::Result<()> + 'a,
    >,
) where
    FStop: Fn() -> bool,
{
    use std::io::{Read, Seek, SeekFrom};

    let mut file = match std::fs::OpenOptions::new().read(true).open(filename) {
        Ok(file) => file,
        Err(_) => return,
    };
    let mut remove_cache = false;
    let mut invalid_magic = false;
    let mut outdated_version = false;

    let load_result = (|| -> std::io::Result<()> {
        let end = file.seek(SeekFrom::End(0))?;
        file.seek(SeekFrom::Start(0))?;

        let mut magic = [0u8; 8];
        let mut version_buf = [0u8; 4];
        file.read_exact(&mut magic)?;
        file.read_exact(&mut version_buf)?;
        let cache_version = u32::from_le_bytes(version_buf);
        if magic != MAGIC_NUMBER || cache_version != expected_cache_version {
            remove_cache = true;
            invalid_magic = magic != MAGIC_NUMBER;
            outdated_version = cache_version != expected_cache_version;
            return Ok(());
        }

        while file.stream_position()? != end {
            if stop_loading() {
                return Ok(());
            }

            let mut buf4 = [0u8; 4];
            file.read_exact(&mut buf4)?;
            let num_envs = u32::from_le_bytes(buf4) as usize;
            if num_envs == 0 || num_envs > crate::shader_cache::NUM_PROGRAMS {
                return Err(invalid_cache_data(format!(
                    "pipeline environment count {num_envs} is outside 1..={}",
                    crate::shader_cache::NUM_PROGRAMS
                )));
            }

            let mut envs: Vec<FileEnvironment> = Vec::new();
            envs.try_reserve_exact(num_envs)
                .map_err(|_| invalid_cache_data("pipeline environment allocation is too large"))?;
            for _ in 0..num_envs {
                let mut env = FileEnvironment::new();
                env.deserialize(&mut file)?;
                envs.push(env);
            }

            if envs
                .first()
                .is_some_and(|env| env.shader_stage() == ShaderStage::Compute)
            {
                if envs.len() != 1 {
                    return Err(invalid_cache_data(
                        "compute pipeline cache entry has more than one environment",
                    ));
                }
                if let Some(env) = envs.into_iter().next() {
                    load_compute(&mut file, env)?;
                }
            } else {
                load_graphics(&mut file, envs)?;
            }
        }

        Ok(())
    })();

    if remove_cache {
        drop(file);
        if std::fs::remove_file(filename).is_ok() {
            if invalid_magic {
                log::error!("Invalid pipeline cache file");
            }
            if outdated_version {
                log::info!("Deleting old pipeline cache");
            }
        } else {
            log::error!(
                "Invalid pipeline cache file and failed to delete it in {:?}",
                filename
            );
        }
        return;
    }

    if let Err(e) = load_result {
        log::error!("load_pipelines: {}", e);
        drop(file);
        if let Err(remove_error) = std::fs::remove_file(filename) {
            log::error!(
                "Failed to delete invalid pipeline cache file {:?}: {}",
                filename,
                remove_error
            );
        }
    }
}

fn invalid_cache_data(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

fn convert_texture_type(entry: &TicEntry) -> TextureType {
    match TegraTextureType::from_raw(entry.texture_type()) {
        Some(TegraTextureType::Texture1D) => TextureType::Color1D,
        Some(TegraTextureType::Texture2D) | Some(TegraTextureType::Texture2DNoMipmap) => {
            if entry.normalized_coords() != 0 {
                TextureType::Color2D
            } else {
                TextureType::Color2DRect
            }
        }
        Some(TegraTextureType::Texture3D) => TextureType::Color3D,
        Some(TegraTextureType::TextureCubemap) => TextureType::ColorCube,
        Some(TegraTextureType::Texture1DArray) => TextureType::ColorArray1D,
        Some(TegraTextureType::Texture2DArray) => TextureType::ColorArray2D,
        Some(TegraTextureType::Texture1DBuffer) => TextureType::Buffer,
        Some(TegraTextureType::TextureCubeArray) => TextureType::ColorArrayCube,
        None => {
            log::error!(
                "ConvertTextureType unimplemented texture_type={}",
                entry.texture_type()
            );
            TextureType::Color2D
        }
    }
}

fn convert_texture_pixel_format(entry: &TicEntry) -> TexturePixelFormat {
    let pixel_format = pixel_format_from_texture_info_raw(
        entry.format(),
        entry.r_type(),
        entry.g_type(),
        entry.b_type(),
        entry.a_type(),
        entry.srgb_conversion() != 0,
    );
    unsafe { std::mem::transmute::<u32, TexturePixelFormat>(pixel_format as u32) }
}

/// `convert_texture_pixel_format` transmutes a `PixelFormat` straight into a
/// `TexturePixelFormat`. They are declared in two different crates and neither
/// references the other, so nothing but this check keeps their discriminants
/// aligned — and a mismatch is undefined behaviour, not a compile error.
#[cfg(test)]
fn assert_pixel_format_layouts_match() {
    use crate::surface::PixelFormat;

    // Pin the absolute discriminants: comparing the two enums to each other is
    // not enough, since inserting the same variant into both would keep them
    // equal while silently invalidating every shader cache on disk.
    assert_eq!(PixelFormat::E5B9G9R9Float as u32, 94);
    assert_eq!(PixelFormat::Etc2RgbUnorm as u32, 95);
    assert_eq!(PixelFormat::D32Float as u32, 105);
    assert_eq!(PixelFormat::D32FloatS8Uint as u32, 111);
    assert_eq!(PixelFormat::MaxDepthStencilFormat as u32, 112);
    assert_eq!(TexturePixelFormat::D32FloatS8Uint as u32, 111);

    // Boundaries around every block that has ever been inserted.
    let pairs: [(PixelFormat, TexturePixelFormat); 8] = [
        (
            PixelFormat::A8B8G8R8Unorm,
            TexturePixelFormat::A8B8G8R8Unorm,
        ),
        (
            PixelFormat::Astc2d6x5Srgb,
            TexturePixelFormat::Astc2d6x5Srgb,
        ),
        (
            PixelFormat::E5B9G9R9Float,
            TexturePixelFormat::E5B9G9R9Float,
        ),
        (PixelFormat::Etc2RgbUnorm, TexturePixelFormat::Etc2RgbUnorm),
        (
            PixelFormat::EacR11G11Snorm,
            TexturePixelFormat::EacR11G11Snorm,
        ),
        (PixelFormat::D32Float, TexturePixelFormat::D32Float),
        (PixelFormat::S8Uint, TexturePixelFormat::S8Uint),
        (
            PixelFormat::D32FloatS8Uint,
            TexturePixelFormat::D32FloatS8Uint,
        ),
    ];
    for (surface, shader) in pairs {
        assert_eq!(
            surface as u32, shader as u32,
            "{surface:?} and {shader:?} must share a discriminant"
        );
    }
}

fn is_integer_pixel_format(format: TexturePixelFormat) -> bool {
    matches!(
        format,
        TexturePixelFormat::A8B8G8R8Sint
            | TexturePixelFormat::A8B8G8R8Uint
            | TexturePixelFormat::R8Sint
            | TexturePixelFormat::R8Uint
            | TexturePixelFormat::R16G16B16A16Sint
            | TexturePixelFormat::R16G16B16A16Uint
            | TexturePixelFormat::R32G32B32A32Uint
            | TexturePixelFormat::R32G32B32A32Sint
            | TexturePixelFormat::R16Uint
            | TexturePixelFormat::R16Sint
            | TexturePixelFormat::R16G16Uint
            | TexturePixelFormat::R16G16Sint
            | TexturePixelFormat::R8G8Sint
            | TexturePixelFormat::R8G8Uint
            | TexturePixelFormat::R32G32Uint
            | TexturePixelFormat::R32G32Sint
            | TexturePixelFormat::R32Uint
            | TexturePixelFormat::R32Sint
            | TexturePixelFormat::A2B10G10R10Uint
    )
}

fn deserialize_enum_u32<T>(raw: u32, max: u32, type_name: &str) -> std::io::Result<T> {
    if raw <= max {
        // SAFETY: `T` is a repr(u32) enum and `raw` is range-checked against
        // the highest contiguous upstream discriminant before transmuting.
        Ok(unsafe { std::mem::transmute_copy::<u32, T>(&raw) })
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid {} discriminant {}", type_name, raw),
        ))
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn texture_pixel_format_layout_matches_surface_pixel_format() {
        super::assert_pixel_format_layouts_match();
    }
    use super::*;
    use crate::engines::engine_interface::EngineInterface;
    use crate::engines::kepler_compute::{KeplerCompute, LaunchParams};
    use crate::engines::maxwell_3d::{Maxwell3D, ShaderStageType};
    use crate::memory_manager::MemoryManager;
    use parking_lot::Mutex as ParkingLotMutex;
    use std::sync::Mutex;

    #[test]
    fn read_texture_info_limit_assert_is_fail_soft_like_upstream() {
        let reads = Arc::new(Mutex::new(Vec::new()));
        let reads_clone = Arc::clone(&reads);
        let reader: GpuMemoryReader = Arc::new(move |gpu_addr, dst| {
            reads_clone.lock().unwrap().push((gpu_addr, dst.len()));
            dst.fill(0);
        });
        let env = GenericEnvironment::new().with_gpu_read(reader);

        let entry = env.read_texture_info(0x1000, 0x10ff, false, u32::MAX);

        assert!(entry == TicEntry::default());
        assert_eq!(reads.lock().unwrap().as_slice(), &[(0x0200_0fe0, 32)]);
    }

    /// Build a 64 KiB GPU-memory mock with a Maxwell self-branch sentinel
    /// at byte offset `sentinel_offset` (relative to the program base) and
    /// return a `(reader, log)` pair. The log records every read so tests
    /// can assert that walking matches upstream's block stride.
    fn make_mock_gpu_with_sentinel(
        program_base: u64,
        sentinel_offset: usize,
        sentinel_value: u64,
    ) -> (GpuMemoryReader, Arc<Mutex<Vec<(u64, usize)>>>) {
        let mut backing = vec![0u8; 64 * 1024];
        backing[sentinel_offset..sentinel_offset + 8]
            .copy_from_slice(&sentinel_value.to_le_bytes());
        let backing = Arc::new(backing);
        let log: Arc<Mutex<Vec<(u64, usize)>>> = Arc::new(Mutex::new(Vec::new()));
        let log_clone = Arc::clone(&log);
        let reader: GpuMemoryReader = Arc::new(move |gpu_addr, dst| {
            log_clone.lock().unwrap().push((gpu_addr, dst.len()));
            let offset = (gpu_addr - program_base) as usize;
            let end = (offset + dst.len()).min(backing.len());
            if offset < backing.len() {
                let n = end - offset;
                dst[..n].copy_from_slice(&backing[offset..end]);
            }
        });
        (reader, log)
    }

    fn make_owner_backed_memory_manager(
        gpu_base: u64,
        device_addr: u64,
        backing: &[u8],
    ) -> Arc<ParkingLotMutex<MemoryManager>> {
        let device_memory = Arc::new(
            crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default(),
        );
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
        memory_manager
    }

    #[test]
    fn try_find_size_finds_self_branch_a_in_first_block() {
        let program_base: u64 = 0x1_0000_0000;
        let sentinel_offset = 0x80; // bytes from start_address
        let (reader, log) =
            make_mock_gpu_with_sentinel(program_base, sentinel_offset, SELF_BRANCH_A);
        let mut env = GenericEnvironment::new()
            .with_gpu_read(reader)
            .with_program(program_base, 0);

        let size = env.try_find_size().expect("sentinel must be found");
        assert_eq!(size as usize, sentinel_offset);

        // Exactly one BLOCK_SIZE read at the program base.
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0], (program_base, TRY_FIND_SIZE_BLOCK_BYTES));
    }

    #[test]
    fn try_find_size_finds_self_branch_b_across_multiple_blocks() {
        let program_base: u64 = 0x2_0000_0000;
        // Sentinel in the third block.
        let sentinel_offset = TRY_FIND_SIZE_BLOCK_BYTES * 2 + 0x40;
        let (reader, log) =
            make_mock_gpu_with_sentinel(program_base, sentinel_offset, SELF_BRANCH_B);
        let mut env = GenericEnvironment::new()
            .with_gpu_read(reader)
            .with_program(program_base, 0);

        let size = env.try_find_size().expect("sentinel must be found");
        assert_eq!(size as usize, sentinel_offset);

        // Three reads, each BLOCK_SIZE, advancing by BLOCK_SIZE.
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 3);
        for (i, entry) in log.iter().enumerate() {
            assert_eq!(
                *entry,
                (
                    program_base + (i * TRY_FIND_SIZE_BLOCK_BYTES) as u64,
                    TRY_FIND_SIZE_BLOCK_BYTES
                )
            );
        }
    }

    #[test]
    fn try_find_size_stops_after_exit_on_non_proprietary_driver() {
        let program_base: u64 = 0x2_1000_0000;
        let exit_offset = 0x88;
        let (reader, log) = make_mock_gpu_with_sentinel(program_base, exit_offset, EXIT_VALUE);
        let mut env = GenericEnvironment::new()
            .with_gpu_read(reader)
            .with_program(program_base, 0);

        let size = env.try_find_size().expect("EXIT must terminate this shader");
        assert_eq!(size as usize, exit_offset + INST_SIZE);
        assert_eq!(log.lock().unwrap().as_slice(), &[(program_base, 0x1000)]);
    }

    #[test]
    fn try_find_size_ignores_exit_on_proprietary_driver() {
        let program_base: u64 = 0x2_2000_0000;
        let exit_offset = 0x80;
        let sentinel_offset = 0x100;
        let mut backing = vec![0u8; TRY_FIND_SIZE_BLOCK_BYTES];
        backing[exit_offset..exit_offset + INST_SIZE].copy_from_slice(&EXIT_VALUE.to_le_bytes());
        backing[sentinel_offset..sentinel_offset + INST_SIZE]
            .copy_from_slice(&SELF_BRANCH_A.to_le_bytes());
        let backing = Arc::new(backing);
        let reader: GpuMemoryReader = Arc::new(move |gpu_addr, dst| {
            let offset = (gpu_addr - program_base) as usize;
            let end = (offset + dst.len()).min(backing.len());
            if offset < backing.len() {
                dst[..end - offset].copy_from_slice(&backing[offset..end]);
            }
        });
        let mut env = GenericEnvironment::new()
            .with_gpu_read(reader)
            .with_program(program_base, 0);
        env.is_proprietary_driver = true;

        assert_eq!(
            env.try_find_size().expect("self branch must terminate this shader") as usize,
            sentinel_offset
        );
    }

    #[test]
    fn try_find_size_returns_none_when_no_sentinel_within_max() {
        // Backing memory contains no sentinel anywhere; capping at MAXIMUM_SIZE.
        let program_base: u64 = 0x3_0000_0000;
        let backing = Arc::new(vec![
            0u8;
            TRY_FIND_SIZE_MAX_BYTES + TRY_FIND_SIZE_BLOCK_BYTES
        ]);
        let reader: GpuMemoryReader = Arc::new(move |gpu_addr, dst| {
            let offset = (gpu_addr - program_base) as usize;
            let end = (offset + dst.len()).min(backing.len());
            if offset < backing.len() {
                let n = end - offset;
                dst[..n].copy_from_slice(&backing[offset..end]);
            }
        });
        let mut env = GenericEnvironment::new()
            .with_gpu_read(reader)
            .with_program(program_base, 0);

        assert!(env.try_find_size().is_none());
    }

    #[test]
    fn analyze_returns_cityhash_of_shader_bytes() {
        let program_base: u64 = 0x4_0000_0000;
        let sentinel_offset = 0x40;
        let (reader, _log) =
            make_mock_gpu_with_sentinel(program_base, sentinel_offset, SELF_BRANCH_A);

        // Analyze should return Some(hash) and seed cached_lowest/highest.
        let mut env = GenericEnvironment::new()
            .with_gpu_read(reader)
            .with_program(program_base, 0);
        let hash = env.analyze().expect("analyze must succeed");
        assert_ne!(hash, 0);
        assert_eq!(env.cached_lowest, 0);
        assert_eq!(env.cached_highest, sentinel_offset as u32);
    }

    #[test]
    fn cached_code_slice_stops_at_cached_size_after_block_scan() {
        let program_base: u64 = 0x4_1000_0000;
        let sentinel_offset = 0x40;
        let (reader, _log) =
            make_mock_gpu_with_sentinel(program_base, sentinel_offset, SELF_BRANCH_A);
        let mut env = GenericEnvironment::new()
            .with_gpu_read(reader)
            .with_program(program_base, 0);

        env.analyze().expect("analyze must find sentinel");

        // TryFindSize reads a full 0x1000-byte block, but upstream's cached
        // shader body is bounded by CachedSizeWords(), including the sentinel.
        assert_eq!(env.code.len(), TRY_FIND_SIZE_BLOCK_BYTES / INST_SIZE);
        assert_eq!(
            env.cached_code_slice().len(),
            sentinel_offset / INST_SIZE + 1
        );
        assert_eq!(env.cached_code_slice().last().copied(), Some(SELF_BRANCH_A));
    }

    #[test]
    fn cached_instruction_slice_skips_program_header_like_upstream_cfg() {
        let program_base: u64 = 0x4_2000_0000;
        let header_size = std::mem::size_of::<ProgramHeader>();
        let sentinel_offset = header_size + INST_SIZE;
        let mut backing = vec![0u8; TRY_FIND_SIZE_BLOCK_BYTES];
        backing[0..8].copy_from_slice(&0x6000_0000_0000_0000u64.to_le_bytes());
        backing[header_size..header_size + INST_SIZE]
            .copy_from_slice(&0x1111_2222_3333_4444u64.to_le_bytes());
        backing[sentinel_offset..sentinel_offset + INST_SIZE]
            .copy_from_slice(&SELF_BRANCH_A.to_le_bytes());
        let backing = Arc::new(backing);
        let reader: GpuMemoryReader = Arc::new(move |gpu_addr, dst| {
            let offset = (gpu_addr - program_base) as usize;
            let end = (offset + dst.len()).min(backing.len());
            if offset < backing.len() {
                dst[..end - offset].copy_from_slice(&backing[offset..end]);
            }
        });
        let mut env = GenericEnvironment::new()
            .with_gpu_read(reader)
            .with_program(program_base, 0)
            .with_initial_offset(header_size as u32);

        env.analyze().expect("analyze must find sentinel");

        assert_eq!(env.cached_instruction_start(), header_size as u32);
        assert_eq!(
            env.cached_instruction_slice(),
            &[0x1111_2222_3333_4444u64, SELF_BRANCH_A]
        );
    }

    #[test]
    fn read_instruction_falls_back_to_gpu_memory_outside_cache() {
        let program_base: u64 = 0x5_0000_0000;
        let target_addr: u32 = 0x200;
        let target_value: u64 = 0xDEAD_BEEF_CAFE_BABE;
        // Backing with the target value at offset target_addr.
        let mut backing = vec![0u8; 0x1000];
        backing[target_addr as usize..target_addr as usize + 8]
            .copy_from_slice(&target_value.to_le_bytes());
        let backing = Arc::new(backing);
        let reader: GpuMemoryReader = Arc::new(move |gpu_addr, dst| {
            let offset = (gpu_addr - program_base) as usize;
            let end = (offset + dst.len()).min(backing.len());
            if offset < backing.len() {
                dst[..end - offset].copy_from_slice(&backing[offset..end]);
            }
        });
        let mut env = GenericEnvironment::new()
            .with_gpu_read(reader)
            .with_program(program_base, 0);

        // No SetCachedSize was called, so the address misses the cache and
        // must be served from the reader.
        let value = env.read_instruction(target_addr);
        assert_eq!(value, target_value);
        assert!(env.has_unbound_instructions);
    }

    #[test]
    fn graphics_environment_from_maxwell3d_sets_stage_mapping() {
        let maxwell = Maxwell3D::new();
        let env = GraphicsEnvironment::from_maxwell3d(&maxwell, ShaderStageType::Fragment, 0, 0);

        assert_eq!(env.base.stage, ShaderStage::Fragment);
        assert_eq!(env.stage_index, 4);
    }

    #[test]
    fn graphics_environment_from_maxwell3d_reads_sph_and_local_memory() {
        let gpu_base = 0x4000_0000;
        let device_addr = 0x9000;
        let mut bytes = [0u8; std::mem::size_of::<ProgramHeader>()];
        let mut raw = [0u32; 20];
        raw[1] = 0x20;
        raw[2] = 0x1;
        raw[3] = 0x10;
        for (index, word) in raw.iter().enumerate() {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        let mut backing = vec![0u8; 0x1000];
        backing[..bytes.len()].copy_from_slice(&bytes);
        let mm = make_owner_backed_memory_manager(gpu_base, device_addr, &backing);
        let reader: GpuMemoryReader = Arc::new(move |_, _| {
            panic!("owner-backed GraphicsEnvironment must not use fallback reader");
        });
        let mut maxwell = Maxwell3D::new();
        maxwell.set_memory_manager(Arc::clone(&mm));
        maxwell.set_guest_memory_reader(reader);
        let env =
            GraphicsEnvironment::from_maxwell3d(&maxwell, ShaderStageType::VertexB, gpu_base, 0);

        assert_eq!(
            env.base.initial_offset as usize,
            std::mem::size_of::<ProgramHeader>()
        );
        assert_eq!(env.base.sph.raw[1], 0x20);
        assert_eq!(env.base.sph.raw[2], 0x1);
        assert_eq!(env.base.sph.raw[3], 0x10);
        assert_eq!(env.base.local_memory_size, 0x0100_0020 + 0x10);
        assert!(env.base.gpu_memory.is_some());
        assert!(env.base.gpu_read.is_none());
    }

    #[test]
    fn graphics_environment_reads_cbuf_value_from_snapshot_binding() {
        let gpu_base = 0x1_0000_0000;
        let mut backing = vec![0u8; 0x2000];
        backing[0x100..0x104].copy_from_slice(&0xDEADBEEFu32.to_le_bytes());
        let backing = Arc::new(backing);
        let reader: GpuMemoryReader = Arc::new(move |gpu_addr, dst| {
            let offset = (gpu_addr - gpu_base) as usize;
            let end = (offset + dst.len()).min(backing.len());
            if offset < backing.len() {
                dst[..end - offset].copy_from_slice(&backing[offset..end]);
            }
        });

        let mut env = GraphicsEnvironment::new();
        env.base = GenericEnvironment::new()
            .with_gpu_read(reader)
            .with_program(gpu_base, 0);
        env.set_detached_const_buffer_binding(
            0,
            ConstBufferBinding {
                enabled: true,
                address: gpu_base,
                size: 0x1000,
            },
        );

        let value = env.read_cbuf_value(0, 0x100);
        assert_eq!(value, 0xDEADBEEF);
        assert_eq!(
            env.base.cbuf_values.get(&make_cbuf_key(0, 0x100)).copied(),
            Some(0xDEADBEEF)
        );
    }

    #[test]
    fn graphics_environment_get_replace_const_buffer_uses_maxwell_owner() {
        let mut maxwell = Maxwell3D::new();
        maxwell.set_engine_state(EngineHint::OnHleMacro);
        maxwell.set_hle_replacement_attribute_type(
            2,
            0x40,
            crate::engines::maxwell_3d::HleReplacementAttributeType::BaseInstance,
        );
        let mut env = GraphicsEnvironment::from_maxwell3d(&maxwell, ShaderStageType::VertexB, 0, 0);

        let replacement = env.get_replace_const_buffer(2, 0x40);
        assert_eq!(replacement, Some(ReplaceConstant::BaseInstance));
        assert_eq!(
            env.base
                .cbuf_replacements
                .get(&make_cbuf_key(2, 0x40))
                .copied(),
            Some(ReplaceConstant::BaseInstance)
        );
    }

    #[test]
    fn graphics_environment_reads_live_maxwell_state_after_construction() {
        let gpu_base = 0x1_0000_0000;
        let cpu_base = 0xA000;
        let mut backing = vec![0u8; 0x2000];
        backing[0x100..0x104].copy_from_slice(&0xAABBCCDDu32.to_le_bytes());

        let mut maxwell = Maxwell3D::new();
        let mm = make_owner_backed_memory_manager(gpu_base, cpu_base, &backing);
        maxwell.set_memory_manager(Arc::clone(&mm));
        let mut env =
            GraphicsEnvironment::from_maxwell3d(&maxwell, ShaderStageType::VertexB, gpu_base, 0);

        maxwell.call_method(0x8E0, 0x1000, true);
        maxwell.call_method(0x8E1, 0x1, true);
        maxwell.call_method(0x8E2, 0x0, true);
        maxwell.call_method(0x904, 0x21, true);
        maxwell.call_method(0x901, 0x21, true);
        maxwell.call_method(0x64B, 0x1234, true);

        assert_eq!(env.read_cbuf_value(2, 0x100), 0xAABBCCDD);
        assert_eq!(env.read_viewport_transform_state(), 0x1234);
    }

    #[test]
    fn graphics_environment_reads_texture_pixel_format_from_detached_state() {
        let gpu_base = 0x4_0000_0000;
        let mut raw = [0u8; 32];
        let word0: u32 = 0x1D
            | (2 << 7)
            | (2 << 10)
            | (2 << 13)
            | (2 << 16)
            | (2 << 19)
            | (3 << 22)
            | (4 << 25)
            | (5 << 28);
        raw[0..4].copy_from_slice(&word0.to_le_bytes());
        let reader: GpuMemoryReader = Arc::new(move |gpu_addr, dst| {
            dst.fill(0);
            let expected_addr = gpu_base + 3 * 32;
            if gpu_addr == expected_addr {
                dst.copy_from_slice(&raw);
            }
        });

        let mut env = GraphicsEnvironment::new();
        env.base = GenericEnvironment::new()
            .with_gpu_read(reader)
            .with_program(gpu_base, 0);
        env.set_detached_texture_header_pool_state(gpu_base, 3, SamplerBinding::Independently);

        let format = env.read_texture_pixel_format(3);
        assert_eq!(format, TexturePixelFormat::R8Unorm);
        assert_eq!(
            env.base.texture_pixel_formats.get(&3).copied(),
            Some(format)
        );
    }

    #[test]
    fn graphics_environment_preserves_r32_float_tic_format() {
        let gpu_base = 0x4_1000_0000;
        let mut raw = [0u8; 32];
        let word0: u32 = 0x0F | (7 << 7) | (7 << 10) | (7 << 13) | (7 << 16);
        raw[0..4].copy_from_slice(&word0.to_le_bytes());
        let reader: GpuMemoryReader = Arc::new(move |gpu_addr, dst| {
            dst.fill(0);
            if gpu_addr == gpu_base {
                dst.copy_from_slice(&raw);
            }
        });

        let mut env = GraphicsEnvironment::new();
        env.base = GenericEnvironment::new()
            .with_gpu_read(reader)
            .with_program(gpu_base, 0);
        env.set_detached_texture_header_pool_state(gpu_base, 0, SamplerBinding::Independently);

        assert_eq!(
            env.read_texture_pixel_format(0),
            TexturePixelFormat::R32Float
        );
    }

    #[test]
    #[should_panic(expected = "disabled cbuf 2")]
    fn graphics_environment_panics_on_disabled_live_cbuf() {
        let maxwell = Maxwell3D::new();
        let mut env = GraphicsEnvironment::from_maxwell3d(&maxwell, ShaderStageType::VertexB, 0, 0);
        let _ = env.read_cbuf_value(2, 0);
    }

    #[test]
    fn compute_environment_from_kepler_compute_includes_local_crs_alloc() {
        let gpu_base = 0x2_0000_0000;
        let cpu_base = 0xA000;
        let memory_manager = Arc::new(ParkingLotMutex::new(MemoryManager::new(0)));
        memory_manager
            .lock()
            .map(gpu_base, cpu_base, 0x2000, 0, false);

        let mut kepler = KeplerCompute::new(Arc::clone(&memory_manager));
        kepler.launch_description = LaunchParams {
            program_start: 0x100,
            shared_alloc: 0x80,
            block_dim_x: 16,
            block_dim_y: 2,
            block_dim_z: 1,
            local_pos_alloc: 0x40,
            local_crs_alloc: 0x20,
            linked_tsc: true,
            ..LaunchParams::default()
        };
        kepler.call_method(0x582, 0x2, true);
        kepler.call_method(0x583, 0, true);

        let env = ComputeEnvironment::from_kepler_compute(&kepler, Arc::clone(&memory_manager));
        assert_eq!(env.base.local_memory_size, 0x60);
        assert_eq!(env.base.shared_memory_size, 0x80);
        assert_eq!(env.base.workgroup_size, [16, 2, 1]);
    }

    #[test]
    fn compute_environment_reads_live_qmd_state_after_construction() {
        let gpu_base = 0x3000_0000;
        let mut backing = vec![0u8; 0x2000];
        backing[0x180..0x184].copy_from_slice(&0xCAFEBABEu32.to_le_bytes());

        let memory_manager = make_owner_backed_memory_manager(gpu_base, 0xB000, &backing);
        let mut kepler = KeplerCompute::new(Arc::clone(&memory_manager));
        let mut env = ComputeEnvironment::from_kepler_compute(&kepler, Arc::clone(&memory_manager));

        kepler.launch_description.const_buffer_enable_mask = 1;
        kepler.launch_description.const_buffers[0] = ConstBufferConfig {
            address: gpu_base,
            size: 0x400,
        };

        assert_eq!(env.read_cbuf_value(0, 0x180), 0xCAFEBABE);
    }

    #[test]
    #[should_panic(expected = "disabled cbuf 0")]
    fn compute_environment_panics_on_disabled_live_cbuf() {
        let memory_manager = Arc::new(ParkingLotMutex::new(MemoryManager::new(0)));
        let kepler = KeplerCompute::new(Arc::clone(&memory_manager));
        let mut env = ComputeEnvironment::from_kepler_compute(&kepler, memory_manager);
        let _ = env.read_cbuf_value(0, 0);
    }

    #[test]
    fn graphics_environment_implements_shader_environment_trait() {
        use shader_recompiler::environment::Environment;

        let mut maxwell = Maxwell3D::new();
        maxwell.set_engine_state(EngineHint::OnHleMacro);
        maxwell.set_hle_replacement_attribute_type(
            1,
            0x20,
            crate::engines::maxwell_3d::HleReplacementAttributeType::DrawId,
        );
        let mut env =
            GraphicsEnvironment::from_maxwell3d(&maxwell, ShaderStageType::Geometry, 0, 0);

        let trait_env: &mut dyn Environment = &mut env;
        assert_eq!(trait_env.shader_stage(), ShaderStage::Geometry);
        assert_eq!(trait_env.start_address(), 0);
        assert_eq!(
            trait_env.gp_passthrough_mask(),
            &maxwell.post_vtg_shader_attrib_skip_mask()
        );
        assert_eq!(
            trait_env.get_replace_const_buffer(1, 0x20),
            Some(ReplaceConstant::DrawID)
        );
    }

    #[test]
    fn file_environment_implements_shader_environment_trait() {
        use shader_recompiler::environment::Environment;

        let mut env = FileEnvironment::new();
        env.stage = ShaderStage::Compute;
        env.texture_bound = 7;
        env.local_memory_size = 0x44;
        env.shared_memory_size = 0x88;
        env.workgroup_size = [4, 5, 6];
        env.cbuf_values.insert(make_cbuf_key(3, 0x10), 0x12345678);
        env.texture_types.insert(0x20, TextureType::ColorArray2D);
        env.texture_pixel_formats
            .insert(0x20, TexturePixelFormat::R32Uint);
        env.cbuf_replacements
            .insert(make_cbuf_key(3, 0x10), ReplaceConstant::BaseVertex);

        let trait_env: &mut dyn Environment = &mut env;
        assert_eq!(trait_env.read_cbuf_value(3, 0x10), 0x12345678);
        assert_eq!(trait_env.read_texture_type(0x20), TextureType::ColorArray2D);
        assert_eq!(
            trait_env.read_texture_pixel_format(0x20),
            TexturePixelFormat::R32Uint
        );
        assert!(trait_env.is_texture_pixel_format_integer(0x20));
        assert_eq!(trait_env.texture_bound_buffer(), 7);
        assert_eq!(trait_env.local_memory_size(), 0x44);
        assert_eq!(trait_env.shared_memory_size(), 0x88);
        assert_eq!(trait_env.workgroup_size(), [4, 5, 6]);
        assert_eq!(
            trait_env.get_replace_const_buffer(3, 0x10),
            Some(ReplaceConstant::BaseVertex)
        );
    }

    fn make_test_cache_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "ruzu-shader-env-{}-{}-{}.bin",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time after epoch")
                .as_nanos()
        ))
    }

    fn read_test_cache_key(file: &mut std::fs::File, size: usize) -> Vec<u8> {
        use std::io::Read;

        let mut key = vec![0; size];
        file.read_exact(&mut key)
            .expect("read serialized cache key");
        key
    }

    #[test]
    fn load_pipelines_dispatches_compute_by_first_environment_stage() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let path = make_test_cache_path("compute-dispatch");
        let mut env = GenericEnvironment::new();
        env.stage = ShaderStage::Compute;
        env.cached_lowest = 0;
        env.cached_highest = 0;
        env.code = vec![0];
        env.shared_memory_size = 0x20;
        env.workgroup_size = [1, 2, 3];
        serialize_pipeline(b"test", &[&env], &path, 7);

        let compute_calls = Arc::new(AtomicUsize::new(0));
        let graphics_calls = Arc::new(AtomicUsize::new(0));
        let compute_calls_clone = Arc::clone(&compute_calls);
        let graphics_calls_clone = Arc::clone(&graphics_calls);
        load_pipelines(
            || false,
            &path,
            7,
            Box::new(move |file, env| {
                assert_eq!(env.shader_stage(), ShaderStage::Compute);
                assert_eq!(read_test_cache_key(file, 4), b"test");
                compute_calls_clone.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }),
            Box::new(move |file, _| {
                let _ = read_test_cache_key(file, 4);
                graphics_calls_clone.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }),
        );

        assert_eq!(compute_calls.load(Ordering::Relaxed), 1);
        assert_eq!(graphics_calls.load(Ordering::Relaxed), 0);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn load_pipelines_deletes_invalid_cache_file() {
        use std::io::Write;

        let path = make_test_cache_path("invalid-header");
        let mut file = std::fs::File::create(&path).expect("create invalid cache");
        file.write_all(b"badcache").expect("write magic");
        file.write_all(&999u32.to_le_bytes())
            .expect("write version");
        drop(file);

        load_pipelines(
            || false,
            &path,
            7,
            Box::new(|_, _| Ok(())),
            Box::new(|_, _| Ok(())),
        );

        assert!(!path.exists());
    }

    #[test]
    fn load_pipelines_deletes_truncated_cache_file() {
        use std::io::Write;

        let path = make_test_cache_path("truncated-entry");
        let mut file = std::fs::File::create(&path).expect("create truncated cache");
        file.write_all(&MAGIC_NUMBER).expect("write magic");
        file.write_all(&7u32.to_le_bytes()).expect("write version");
        file.write_all(&1u32.to_le_bytes())
            .expect("write env count");
        file.write_all(&0u64.to_le_bytes())
            .expect("write partial code size");
        drop(file);

        load_pipelines(
            || false,
            &path,
            7,
            Box::new(|_, _| Ok(())),
            Box::new(|_, _| Ok(())),
        );

        assert!(!path.exists());
    }

    #[test]
    fn load_pipelines_deletes_cache_with_invalid_stage_discriminant() {
        use std::io::{Seek, SeekFrom, Write};

        let path = make_test_cache_path("invalid-stage");
        let mut env = GenericEnvironment::new();
        env.stage = ShaderStage::Compute;
        env.cached_lowest = 0;
        env.cached_highest = 0;
        env.code = vec![0];
        env.shared_memory_size = 0x20;
        env.workgroup_size = [1, 2, 3];
        serialize_pipeline(b"test", &[&env], &path, 7);

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open serialized cache");
        file.seek(SeekFrom::Start(80))
            .expect("seek to stage discriminant");
        file.write_all(&u32::MAX.to_le_bytes())
            .expect("overwrite stage discriminant");
        drop(file);

        load_pipelines(
            || false,
            &path,
            7,
            Box::new(|_, _| Ok(())),
            Box::new(|_, _| Ok(())),
        );

        assert!(!path.exists());
    }

    #[test]
    fn load_pipelines_deletes_zero_environment_entry() {
        use std::io::Write;

        let path = make_test_cache_path("zero-environments");
        let mut file = std::fs::File::create(&path).expect("create invalid cache");
        file.write_all(&MAGIC_NUMBER).expect("write magic");
        file.write_all(&7u32.to_le_bytes()).expect("write version");
        file.write_all(&0u32.to_le_bytes())
            .expect("write empty environment count");
        file.write_all(&[0; 512]).expect("write stale cache key");
        drop(file);

        load_pipelines(
            || false,
            &path,
            7,
            Box::new(|_, _| Ok(())),
            Box::new(|_, _| Ok(())),
        );

        assert!(!path.exists());
    }

    #[test]
    fn load_pipelines_rejects_oversized_environment_count_before_allocating() {
        use std::io::Write;

        let path = make_test_cache_path("oversized-environment-count");
        let mut file = std::fs::File::create(&path).expect("create invalid cache");
        file.write_all(&MAGIC_NUMBER).expect("write magic");
        file.write_all(&7u32.to_le_bytes()).expect("write version");
        file.write_all(&u32::MAX.to_le_bytes())
            .expect("write oversized environment count");
        drop(file);

        load_pipelines(
            || false,
            &path,
            7,
            Box::new(|_, _| Ok(())),
            Box::new(|_, _| Ok(())),
        );

        assert!(!path.exists());
    }

    #[test]
    fn load_pipelines_rejects_oversized_shader_payload_before_allocating() {
        use std::io::Write;

        let path = make_test_cache_path("oversized-shader-payload");
        let mut file = std::fs::File::create(&path).expect("create invalid cache");
        file.write_all(&MAGIC_NUMBER).expect("write magic");
        file.write_all(&7u32.to_le_bytes()).expect("write version");
        file.write_all(&1u32.to_le_bytes())
            .expect("write environment count");
        file.write_all(&u64::MAX.to_le_bytes())
            .expect("write oversized code size");
        file.write_all(&[0; 60])
            .expect("write remainder of environment header");
        drop(file);

        load_pipelines(
            || false,
            &path,
            7,
            Box::new(|_, _| Ok(())),
            Box::new(|_, _| Ok(())),
        );

        assert!(!path.exists());
    }

    #[test]
    fn serialize_pipeline_does_not_write_empty_environment_entries() {
        let path = make_test_cache_path("empty-serialization");

        serialize_pipeline(b"key", &[], &path, 7);

        assert!(!path.exists());
    }

    #[test]
    fn file_environment_adapters_use_serialized_values_without_live_engines() {
        let mut file = FileEnvironment::new();
        file.stage = ShaderStage::Fragment;
        file.code = vec![0xDEAD_BEEF_CAFE_BABE];
        file.read_lowest = 0;
        file.read_highest = INST_SIZE as u32;
        file.initial_offset = 0;
        file.cbuf_values.insert(make_cbuf_key(2, 0x30), 0x1234_5678);
        file.texture_types.insert(9, TextureType::ColorCube);
        file.texture_pixel_formats
            .insert(9, TexturePixelFormat::A8B8G8R8Unorm);

        let mut env = GraphicsEnvironment::from_file_environment(file);
        assert_eq!(
            shader_recompiler::environment::Environment::read_instruction(&mut env, 0),
            0xDEAD_BEEF_CAFE_BABE
        );
        assert_eq!(
            shader_recompiler::environment::Environment::read_cbuf_value(&mut env, 2, 0x30),
            0x1234_5678
        );
        assert_eq!(
            shader_recompiler::environment::Environment::read_texture_type(&mut env, 9),
            TextureType::ColorCube
        );
        assert_eq!(
            shader_recompiler::environment::Environment::read_texture_pixel_format(&mut env, 9),
            TexturePixelFormat::A8B8G8R8Unorm
        );
    }
}

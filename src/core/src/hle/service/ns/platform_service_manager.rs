// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ns/platform_service_manager.cpp/.h

use std::collections::BTreeMap;
use crate::core::SystemRef;
use crate::file_sys::system_archive::data::{
    font_chinese_simplified, font_chinese_traditional, font_extended_chinese_simplified,
    font_korean, font_nintendo_extended, font_standard,
};
use crate::hle::kernel::k_shared_memory::KSharedMemory;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::cmif_serialization::{CmifRequest, CmifResponse};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum FontArchives {
    Extension = 0x0100000000000810,
    Standard = 0x0100000000000811,
    Korean = 0x0100000000000812,
    ChineseTraditional = 0x0100000000000813,
    ChineseSimple = 0x0100000000000814,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SharedFontType {
    JapanUSEuropeStandard = 0,
    ChineseSimplified = 1,
    ExtendedChineseSimplified = 2,
    ChineseTraditional = 3,
    KoreanHangul = 4,
    NintendoExtended = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum LoadState {
    Loading = 0,
    Loaded = 1,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FontRegion {
    pub offset: u32,
    pub size: u32,
}

pub const SHARED_FONT_MEM_SIZE: u64 = 0x1100000;
const EMPTY_REGION: FontRegion = FontRegion { offset: 0, size: 0 };
const MAX_ELEMENT_COUNT: usize = 6;

fn shared_font_priority_count(
    font_codes_size: usize,
    font_offsets_size: usize,
    font_sizes_size: usize,
    region_count: usize,
) -> usize {
    [
        MAX_ELEMENT_COUNT,
        font_codes_size / std::mem::size_of::<u32>(),
        font_offsets_size / std::mem::size_of::<u32>(),
        font_sizes_size / std::mem::size_of::<u32>(),
        region_count,
    ]
    .into_iter()
    .min()
    .unwrap_or(0)
}

fn copy_shared_font_bytes(shared_font_bytes: &[u8], shared_memory: &KSharedMemory) -> bool {
    let dst = shared_memory.get_pointer_mut(0);
    if dst.is_null() {
        return false;
    }

    unsafe {
        std::ptr::copy_nonoverlapping(shared_font_bytes.as_ptr(), dst, shared_font_bytes.len());
    }
    true
}

const SHARED_FONTS: [(FontArchives, &[u8]); 7] = [
    (FontArchives::Standard, &font_standard::FONT_STANDARD),
    (
        FontArchives::ChineseSimple,
        &font_chinese_simplified::FONT_CHINESE_SIMPLIFIED,
    ),
    (
        FontArchives::ChineseSimple,
        &font_extended_chinese_simplified::FONT_EXTENDED_CHINESE_SIMPLIFIED,
    ),
    (
        FontArchives::ChineseTraditional,
        &font_chinese_traditional::FONT_CHINESE_TRADITIONAL,
    ),
    (FontArchives::Korean, &font_korean::FONT_KOREAN),
    (
        FontArchives::Extension,
        &font_nintendo_extended::FONT_NINTENDO_EXTENDED,
    ),
    (
        FontArchives::Extension,
        &font_nintendo_extended::FONT_NINTENDO_EXTENDED,
    ),
];

pub const IPLATFORM_SERVICE_MANAGER_COMMANDS: &[(u32, bool, &str)] = &[
    (0, true, "RequestLoad"),
    (1, true, "GetLoadState"),
    (2, true, "GetSize"),
    (3, true, "GetSharedMemoryAddressOffset"),
    (4, true, "GetSharedMemoryNativeHandle"),
    (5, true, "GetSharedFontInOrderOfPriority"),
    (6, true, "GetSharedFontInOrderOfPriorityForSystem"),
    (100, false, "RequestApplicationFunctionAuthorization"),
    (
        101,
        false,
        "RequestApplicationFunctionAuthorizationByProcessId",
    ),
    (
        102,
        false,
        "RequestApplicationFunctionAuthorizationByApplicationId",
    ),
    (103, false, "RefreshApplicationFunctionBlackListDebugRecord"),
    (
        104,
        false,
        "RequestApplicationFunctionAuthorizationByProgramId",
    ),
    (105, false, "GetFunctionBlackListSystemVersionToAuthorize"),
    (106, false, "GetFunctionBlackListVersion"),
    (1000, false, "LoadNgWordDataForPlatformRegionChina"),
    (1001, false, "GetNgWordDataSizeForPlatformRegionChina"),
];

fn align_up_8(value: usize) -> usize {
    (value + 7) & !7
}

/// XOR-encrypted font header constants. Mirrors upstream
/// `EXPECTED_RESULT` / `EXPECTED_MAGIC` from `platform_service_manager.cpp`.
/// Each font file's first u32 (after byte-swap) is `EXPECTED_MAGIC`. Decrypting
/// it should yield `EXPECTED_RESULT`. The XOR key is `MAGIC ^ EXPECTED_RESULT`.
const EXPECTED_RESULT: u32 = 0x7F9A0218;
const EXPECTED_MAGIC: u32 = 0x36F81A1E;

/// Upstream `(FontArchives, file_name)` table — the encrypted font file inside
/// each system NCA's RomFS. Matches
/// `zuyu/src/core/hle/service/ns/platform_service_manager.h::SHARED_FONTS`.
pub const SHARED_FONT_FILE_NAMES: [(FontArchives, &str); 7] = [
    (FontArchives::Standard, "nintendo_udsg-r_std_003.bfttf"),
    (
        FontArchives::ChineseSimple,
        "nintendo_udsg-r_org_zh-cn_003.bfttf",
    ),
    (
        FontArchives::ChineseSimple,
        "nintendo_udsg-r_ext_zh-cn_003.bfttf",
    ),
    (
        FontArchives::ChineseTraditional,
        "nintendo_udjxh-db_zh-tw_003.bfttf",
    ),
    (FontArchives::Korean, "nintendo_udsg-r_ko_003.bfttf"),
    (FontArchives::Extension, "nintendo_ext_003.bfttf"),
    (FontArchives::Extension, "nintendo_ext2_003.bfttf"),
];

/// Decrypt one font's u32 stream into the shared blob, mirroring upstream
/// `DecryptSharedFont`:
///   key = input[0] ^ EXPECTED_RESULT
///   blob[offset..] = swap32(input[i] ^ key)
///   blob[offset+4..offset+8] = swap32(transformed[1]) ^ key   (re-encrypted size)
fn decrypt_shared_font(input: &[u32], output: &mut Vec<u8>, offset: &mut usize) -> bool {
    if input.is_empty() || input[0] != EXPECTED_MAGIC {
        return false;
    }
    let byte_size = input.len() * 4;
    if *offset + byte_size > output.len() {
        return false;
    }
    let key = input[0] ^ EXPECTED_RESULT;
    let mut transformed: Vec<u32> = input.iter().map(|w| (*w ^ key).swap_bytes()).collect();
    if transformed.len() >= 2 {
        transformed[1] = transformed[1].swap_bytes() ^ key;
    }
    for (i, w) in transformed.iter().enumerate() {
        let dst = *offset + i * 4;
        output[dst..dst + 4].copy_from_slice(&w.to_le_bytes());
    }
    *offset += byte_size;
    true
}

/// Port of upstream `DecryptSharedFontToTTF`.
pub fn decrypt_shared_font_to_ttf(input: &[u32], output: &mut [u8]) -> bool {
    if input.len() < 2 || input[0] != EXPECTED_MAGIC {
        return false;
    }
    let byte_size = (input.len() - 2) * 4;
    if output.len() < byte_size {
        return false;
    }

    let key = input[0] ^ EXPECTED_RESULT;
    for (index, word) in input[2..].iter().enumerate() {
        let transformed = (*word ^ key).swap_bytes();
        let dst = index * 4;
        output[dst..dst + 4].copy_from_slice(&transformed.to_le_bytes());
    }
    true
}

/// Port of upstream `EncryptSharedFont`.
pub fn encrypt_shared_font(input: &[u32], output: &mut [u8], offset: &mut usize) -> bool {
    let total_words = input.len().saturating_add(2);
    let total_bytes = total_words * 4;
    let Some(end) = offset.checked_add(total_bytes) else {
        return false;
    };
    if end > output.len() || end > SHARED_FONT_MEM_SIZE as usize {
        return false;
    }

    let key = (EXPECTED_RESULT ^ EXPECTED_MAGIC).swap_bytes();
    let header_magic = EXPECTED_MAGIC.swap_bytes();
    let header_size = ((input.len() * 4) as u32).swap_bytes() ^ key;

    let start = *offset;
    output[start..start + 4].copy_from_slice(&header_magic.to_le_bytes());
    output[start + 4..start + 8].copy_from_slice(&header_size.to_le_bytes());
    for (index, word) in input.iter().enumerate() {
        let encrypted = *word ^ key;
        let dst = start + 8 + index * 4;
        output[dst..dst + 4].copy_from_slice(&encrypted.to_le_bytes());
    }
    *offset += total_bytes;
    true
}

/// Helper function matching upstream `GetU32Swapped`.
#[allow(dead_code)]
fn get_u32_swapped(data: &[u8]) -> Option<u32> {
    let bytes: [u8; 4] = data.get(..4)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes).swap_bytes())
}

/// Try to load a single shared font from the system NAND. Returns the
/// big-endian-as-u32 file contents on success.
fn try_load_font_from_nand(
    system: &SystemRef,
    font: FontArchives,
    file_name: &str,
) -> Option<Vec<u32>> {
    use crate::file_sys::nca_metadata::ContentRecordType;
    use crate::file_sys::registered_cache::ContentProvider;
    use crate::file_sys::romfs::extract_romfs;
    use crate::file_sys::system_archive::system_archive::synthesize_system_archive;

    let fsc_arc = system.get().get_filesystem_controller();
    let fsc = fsc_arc.lock().unwrap();
    let title_id = font as u64;

    // Get the NCA from system NAND, or synthesize the archive if missing.
    let romfs = if let Some(nand) = fsc.get_system_nand_contents() {
        if let Some(nca) = nand.get_entry(title_id, ContentRecordType::Data) {
            nca.get_romfs()
                .or_else(|| synthesize_system_archive(title_id))
        } else {
            synthesize_system_archive(title_id)
        }
    } else {
        synthesize_system_archive(title_id)
    };
    drop(fsc);

    let romfs = romfs?;
    let extracted = extract_romfs(Some(romfs))?;
    let font_fp = extracted.get_file(file_name)?;

    let total_bytes = font_fp.get_size();
    if total_bytes == 0 || total_bytes % 4 != 0 {
        log::warn!(
            "pl:u: font {:#x}/{} has invalid size {} (not u32-aligned)",
            title_id,
            file_name,
            total_bytes
        );
        return None;
    }
    let bytes = font_fp.read_bytes(total_bytes as usize, 0);
    if bytes.len() != total_bytes as usize {
        log::warn!(
            "pl:u: font {:#x}/{} short read {}/{}",
            title_id,
            file_name,
            bytes.len(),
            total_bytes
        );
        return None;
    }
    // File is stored big-endian as u32, mirroring upstream's transform with
    // `Common::swap32` after read.
    let mut words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]).swap_bytes())
        .collect();
    if words.is_empty() {
        return None;
    }
    log::info!(
        "pl:u: loaded font {:#x}/{} from NAND ({} bytes)",
        title_id,
        file_name,
        words.len() * 4
    );
    let _ = &mut words; // suppress unused-mut warning if compiler tightens
    Some(words)
}

/// Build the shared font blob. Mirrors upstream's `IPlatformServiceManager`
/// constructor body: tries to load each font from system NAND (decrypting via
/// `DecryptSharedFont`), falls back to the embedded byte arrays if loading
/// fails.
fn build_shared_font_blob_with_system(system: &SystemRef) -> (Vec<u8>, Vec<FontRegion>) {
    let mut blob = vec![0u8; SHARED_FONT_MEM_SIZE as usize];
    let mut regions = Vec::with_capacity(SHARED_FONTS.len());
    let mut offset: usize = 0;
    let mut nand_count = 0usize;
    let mut fallback_count = 0usize;

    for (i, (font_archive, file_name)) in SHARED_FONT_FILE_NAMES.iter().enumerate() {
        // Path A: attempt NAND load + XOR-decrypt (matches upstream byte layout).
        if let Some(input) = try_load_font_from_nand(system, *font_archive, file_name) {
            // Region: skip the 8-byte (magic + size) header.
            let payload_size = input.len() * 4;
            let region = FontRegion {
                offset: (offset + 8) as u32,
                size: (payload_size as u32).saturating_sub(8),
            };
            let start = offset;
            if decrypt_shared_font(&input, &mut blob, &mut offset) {
                regions.push(region);
                nand_count += 1;
                continue;
            } else {
                log::warn!(
                    "pl:u: decrypt failed for font#{} {} — reverting offset",
                    i,
                    file_name
                );
                offset = start;
            }
        }
        // Path B: embedded fallback. Re-encrypt into the same header-bearing
        // BFTTF shared-memory layout upstream uses for system-archive fonts.
        let (_, font_bytes) = SHARED_FONTS[i];
        let font_words: Vec<u32> = font_bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let size = font_words.len() * 4;
        if offset + size + 8 > blob.len() {
            log::error!("pl:u: blob overflow at font #{} fallback", i);
            break;
        }
        let region_offset = offset + 8;
        if !encrypt_shared_font(&font_words, &mut blob, &mut offset) {
            log::error!("pl:u: encrypted fallback write failed at font #{}", i);
            break;
        }
        regions.push(FontRegion {
            offset: region_offset as u32,
            size: size as u32,
        });
        offset = align_up_8(offset);
        fallback_count += 1;
    }

    log::info!(
        "pl:u: built shared font blob: {} regions ({} from NAND, {} fallback embedded), used 0x{:x}/0x{:x} bytes",
        regions.len(),
        nand_count,
        fallback_count,
        offset,
        blob.len()
    );
    (blob, regions)
}

/// Test-only fallback (no SystemRef available): builds blob purely from the
/// embedded byte arrays. Kept for the existing unit tests.
#[cfg(test)]
fn build_shared_font_blob() -> (Vec<u8>, Vec<FontRegion>) {
    let mut blob = vec![0u8; SHARED_FONT_MEM_SIZE as usize];
    let mut regions = Vec::with_capacity(SHARED_FONTS.len());
    let mut offset = 0usize;
    for (_, font_bytes) in SHARED_FONTS {
        let font_words: Vec<u32> = font_bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let size = font_words.len() * 4;
        if offset + size + 8 > blob.len() {
            break;
        }
        let region_offset = offset + 8;
        if !encrypt_shared_font(&font_words, &mut blob, &mut offset) {
            break;
        }
        regions.push(FontRegion {
            offset: region_offset as u32,
            size: size as u32,
        });
        offset = align_up_8(offset);
    }
    (blob, regions)
}

pub struct IPlatformServiceManager {
    system: SystemRef,
    service_name: &'static str,
    shared_font_bytes: Vec<u8>,
    shared_font_regions: Vec<FontRegion>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IPlatformServiceManager {
    pub fn new(system: SystemRef, service_name: &'static str) -> Self {
        let (shared_font_bytes, shared_font_regions) = build_shared_font_blob_with_system(&system);
        Self {
            system,
            service_name,
            shared_font_bytes,
            shared_font_regions,
            handlers: build_handler_map(&[
                (0, Some(Self::request_load_handler), "RequestLoad"),
                (1, Some(Self::get_load_state_handler), "GetLoadState"),
                (2, Some(Self::get_size_handler), "GetSize"),
                (
                    3,
                    Some(Self::get_shared_memory_address_offset_handler),
                    "GetSharedMemoryAddressOffset",
                ),
                (
                    4,
                    Some(Self::get_shared_memory_native_handle_handler),
                    "GetSharedMemoryNativeHandle",
                ),
                (
                    5,
                    Some(Self::get_shared_font_in_order_of_priority_handler),
                    "GetSharedFontInOrderOfPriority",
                ),
                (
                    6,
                    Some(Self::get_shared_font_in_order_of_priority_handler),
                    "GetSharedFontInOrderOfPriorityForSystem",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    fn get_shared_font_region(&self, index: usize) -> FontRegion {
        self.shared_font_regions
            .get(index)
            .copied()
            .unwrap_or(EMPTY_REGION)
    }

    // ---------------- Business-logic methods ----------------
    // Mirror upstream's `Result Method(Out<T>, In<T>, ...)` signatures.

    fn request_load(&self, font_id: u32) -> ResultCode {
        log::info!("pl:u RequestLoad font_id={}", font_id);
        RESULT_SUCCESS
    }

    fn get_load_state(&self, out_state: &mut u32, font_id: u32) -> ResultCode {
        log::info!("pl:u GetLoadState font_id={} -> Loaded", font_id);
        *out_state = LoadState::Loaded as u32;
        RESULT_SUCCESS
    }

    fn get_size(&self, out_size: &mut u32, font_id: u32) -> ResultCode {
        let size = self.get_shared_font_region(font_id as usize).size;
        log::info!("pl:u GetSize font_id={} -> size=0x{:x}", font_id, size);
        *out_size = size;
        RESULT_SUCCESS
    }

    fn get_shared_memory_address_offset(&self, out_offset: &mut u32, font_id: u32) -> ResultCode {
        let offset = self.get_shared_font_region(font_id as usize).offset;
        log::info!(
            "pl:u GetSharedMemoryAddressOffset font_id={} -> offset=0x{:x}",
            font_id,
            offset
        );
        *out_offset = offset;
        RESULT_SUCCESS
    }

    /// Copy the font blob into the kernel-owned shared-memory object and register
    /// it with the caller's process. The IPC layer creates the copy handle from
    /// the returned object id, matching upstream `OutCopyHandle<KSharedMemory>`.
    fn get_shared_memory_native_handle(&self, ctx: &mut HLERequestContext) -> Option<u64> {
        log::info!("pl:u GetSharedMemoryNativeHandle begin");
        let object_id = (|| -> Option<u64> {
            let (object_id, shared_memory) = self.system.get().kernel()?.get_font_shared_mem()?;
            if !copy_shared_font_bytes(&self.shared_font_bytes, &shared_memory) {
                log::error!("pl:u GetSharedMemoryNativeHandle font shared memory is unavailable");
                return None;
            }

            let thread = ctx.get_thread()?;
            let parent = thread.lock().unwrap().parent.as_ref()?.upgrade()?;
            let mut process = parent.lock().unwrap();
            process.register_shared_memory_object(object_id, shared_memory);
            Some(object_id)
        })();
        log::info!(
            "pl:u GetSharedMemoryNativeHandle -> object_id=0x{:x}",
            object_id.unwrap_or(0)
        );
        object_id
    }

    fn get_shared_font_in_order_of_priority(
        &self,
        ctx: &mut HLERequestContext,
        language_code: u64,
        out_loaded: &mut bool,
        out_count: &mut u32,
    ) -> ResultCode {
        log::info!(
            "pl:u GetSharedFontInOrderOfPriority language_code=0x{:x}",
            language_code
        );
        let max_size = shared_font_priority_count(
            ctx.get_write_buffer_size(0),
            ctx.get_write_buffer_size(1),
            ctx.get_write_buffer_size(2),
            self.shared_font_regions.len(),
        );
        let mut font_codes = vec![0u8; max_size * 4];
        let mut font_offsets = vec![0u8; max_size * 4];
        let mut font_sizes = vec![0u8; max_size * 4];
        for i in 0..max_size {
            let region = self.get_shared_font_region(i);
            font_codes[i * 4..(i + 1) * 4].copy_from_slice(&(i as u32).to_le_bytes());
            font_offsets[i * 4..(i + 1) * 4].copy_from_slice(&region.offset.to_le_bytes());
            font_sizes[i * 4..(i + 1) * 4].copy_from_slice(&region.size.to_le_bytes());
        }
        ctx.write_buffer(&font_codes, 0);
        ctx.write_buffer(&font_offsets, 1);
        ctx.write_buffer(&font_sizes, 2);
        *out_loaded = true;
        *out_count = max_size as u32;
        RESULT_SUCCESS
    }

    // ---------------- Per-cmd dispatch shims ----------------

    fn request_load_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut request = CmifRequest::new(ctx);
        let font_id = request.raw::<u32>();
        let result = service.request_load(font_id);
        let mut response = CmifResponse::result_only(ctx, result);
        let _ = &mut response;
    }

    fn get_load_state_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut request = CmifRequest::new(ctx);
        let font_id = request.raw::<u32>();
        let mut state: u32 = 0;
        let result = service.get_load_state(&mut state, font_id);
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(result);
        response.push_u32(state);
    }

    fn get_size_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut request = CmifRequest::new(ctx);
        let font_id = request.raw::<u32>();
        let mut size: u32 = 0;
        let result = service.get_size(&mut size, font_id);
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(result);
        response.push_u32(size);
    }

    fn get_shared_memory_address_offset_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut request = CmifRequest::new(ctx);
        let font_id = request.raw::<u32>();
        let mut offset: u32 = 0;
        let result = service.get_shared_memory_address_offset(&mut offset, font_id);
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(result);
        response.push_u32(offset);
    }

    fn get_shared_memory_native_handle_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let object_id = service.get_shared_memory_native_handle(ctx);
        let mut response = CmifResponse::new(ctx, 2, 1, 0);
        response.push_result(RESULT_SUCCESS);
        if let Some(object_id) = object_id {
            response.push_copy_object_id(object_id);
        } else {
            response.push_copy_objects(0);
        }
    }

    fn get_shared_font_in_order_of_priority_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut request = CmifRequest::new(ctx);
        let language_code = request.u64();
        let mut loaded = false;
        let mut count: u32 = 0;
        let result = service.get_shared_font_in_order_of_priority(
            ctx,
            language_code,
            &mut loaded,
            &mut count,
        );
        let mut response = CmifResponse::new(ctx, 4, 0, 0);
        response.push_result(result);
        response.push_bool(loaded);
        response.push_u32(count);
    }
}

impl SessionRequestHandler for IPlatformServiceManager {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        ServiceFramework::get_service_name(self)
    }
}

impl ServiceFramework for IPlatformServiceManager {
    fn get_service_name(&self) -> &str {
        self.service_name
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_memory::{dram_memory_map, DeviceMemory};
    use crate::hle::kernel::k_memory_manager::{KMemoryManager, Pool};
    use crate::hle::kernel::k_shared_memory::MemoryPermission;

    #[test]
    fn shared_font_copy_overwrites_the_complete_kernel_buffer() {
        const MEMORY_SIZE: usize = 0x400_0000;
        let device_memory = DeviceMemory::with_size(MEMORY_SIZE);
        let mut memory_manager = KMemoryManager::new();
        memory_manager.initialize_pool(Pool::SECURE, dram_memory_map::BASE, MEMORY_SIZE);
        let mut shared_memory = KSharedMemory::new();
        assert!(shared_memory
            .initialize(
                &device_memory,
                &mut memory_manager,
                MemoryPermission::None,
                MemoryPermission::Read,
                SHARED_FONT_MEM_SIZE as usize,
            )
            .is_success());

        let (blob, _) = build_shared_font_blob();
        unsafe {
            std::ptr::write_bytes(
                shared_memory.get_pointer_mut(0),
                0xa5,
                SHARED_FONT_MEM_SIZE as usize,
            );
        }

        assert!(copy_shared_font_bytes(&blob, &shared_memory));
        let copied = unsafe {
            std::slice::from_raw_parts(shared_memory.get_pointer(0), SHARED_FONT_MEM_SIZE as usize)
        };
        assert_eq!(copied, blob);
    }

    #[test]
    fn shared_font_blob_builds_seven_regions() {
        let (_blob, regions) = build_shared_font_blob();
        assert_eq!(regions.len(), SHARED_FONTS.len());
        assert!(regions.iter().all(|region| region.size > 0));
    }

    #[test]
    fn shared_font_blob_offsets_are_monotonic() {
        let (_blob, regions) = build_shared_font_blob();
        for pair in regions.windows(2) {
            assert!(pair[0].offset < pair[1].offset);
        }
    }

    #[test]
    fn shared_font_priority_count_is_limited_by_guest_buffers() {
        assert_eq!(shared_font_priority_count(24, 24, 24, 7), 6);
        assert_eq!(shared_font_priority_count(8, 24, 24, 7), 2);
        assert_eq!(shared_font_priority_count(24, 12, 24, 7), 3);
        assert_eq!(shared_font_priority_count(24, 24, 4, 7), 1);
        assert_eq!(shared_font_priority_count(24, 24, 24, 4), 4);
    }

    #[test]
    fn fallback_shared_font_blob_uses_encrypted_header_layout() {
        let (blob, regions) = build_shared_font_blob();
        let first = regions[0];
        assert_eq!(first.offset, 8);
        assert_eq!(get_u32_swapped(&blob[0..4]), Some(EXPECTED_MAGIC));
        let key = EXPECTED_MAGIC ^ EXPECTED_RESULT;
        assert_eq!(
            get_u32_swapped(&blob[4..8]).map(|value| value ^ key),
            Some(first.size)
        );
    }

    #[test]
    fn encrypt_shared_font_round_trips_to_ttf_bytes() {
        let input_bytes = *b"0123456789ABCDEF";
        let input_words: Vec<u32> = input_bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let mut encrypted = vec![0u8; 64];
        let mut offset = 0usize;

        assert!(encrypt_shared_font(
            &input_words,
            &mut encrypted,
            &mut offset
        ));
        assert_eq!(offset, input_bytes.len() + 8);

        let encrypted_words: Vec<u32> = encrypted[..offset]
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]).swap_bytes())
            .collect();
        let mut output = vec![0u8; input_bytes.len()];

        assert!(decrypt_shared_font_to_ttf(&encrypted_words, &mut output));
        assert_eq!(output, input_bytes);
    }
}

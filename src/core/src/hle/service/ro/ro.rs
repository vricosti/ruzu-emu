// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ro/ro.h
//! Port of zuyu/src/core/hle/service/ro/ro.cpp
//!
//! RO service — read-only module loading ("ldr:ro" and "ro:1").

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use super::ro_nro_utils;
use super::ro_results;
use super::ro_types::{ModuleId, NroHeader, NrrHeader, NrrKind};
use crate::hle::kernel::k_process::ProcessLock;
use crate::hle::kernel::svc_common::PseudoHandle;
use crate::hle::result::ResultCode;
use crate::hle::service::cmif_serialization::CmifRequest;
use crate::hle::service::cmif_types::ClientProcessId;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

// Convenience definitions — matches upstream ro.cpp.
const MAX_SESSIONS: usize = 0x3;
const MAX_NRR_INFOS: usize = 0x100;
const MAX_NRO_INFOS: usize = 0x100;

const INVALID_PROCESS_ID: u64 = 0xFFFFFFFF_FFFFFFFF;
const INVALID_CONTEXT_ID: u64 = 0xFFFFFFFF_FFFFFFFF;

/// SHA-256 hash type.
type Sha256Hash = [u8; 32];

/// Rust counterpart of the `std::mt19937_64` engine owned by upstream's
/// `RoContext`.
struct Mt19937_64 {
    state: [u64; Self::N],
    index: usize,
}

impl Mt19937_64 {
    const N: usize = 312;
    const M: usize = 156;
    const MATRIX_A: u64 = 0xB502_6F5A_A966_19E9;
    const UPPER_MASK: u64 = 0xFFFF_FFFF_8000_0000;
    const LOWER_MASK: u64 = 0x0000_0000_7FFF_FFFF;

    fn new(seed: u64) -> Self {
        let mut state = [0; Self::N];
        state[0] = seed;
        for index in 1..Self::N {
            state[index] = 6_364_136_223_846_793_005_u64
                .wrapping_mul(state[index - 1] ^ (state[index - 1] >> 62))
                .wrapping_add(index as u64);
        }
        Self {
            state,
            index: Self::N,
        }
    }

    fn next_u64(&mut self) -> u64 {
        if self.index >= Self::N {
            self.twist();
        }

        let mut value = self.state[self.index];
        self.index += 1;
        value ^= (value >> 29) & 0x5555_5555_5555_5555;
        value ^= (value << 17) & 0x71D6_7FFF_EDA6_0000;
        value ^= (value << 37) & 0xFFF7_EEE0_0000_0000;
        value ^= value >> 43;
        value
    }

    fn twist(&mut self) {
        for index in 0..Self::N {
            let value = (self.state[index] & Self::UPPER_MASK)
                | (self.state[(index + 1) % Self::N] & Self::LOWER_MASK);
            let mut mixed = value >> 1;
            if value & 1 != 0 {
                mixed ^= Self::MATRIX_A;
            }
            self.state[index] = self.state[(index + Self::M) % Self::N] ^ mixed;
        }
        self.index = 0;
    }
}

impl Default for Mt19937_64 {
    fn default() -> Self {
        // `std::mt19937_64`'s default seed.
        Self::new(5489)
    }
}

/// Per-NRO tracking information.
///
/// Corresponds to `NroInfo` in upstream ro.cpp.
#[derive(Clone)]
struct NroInfo {
    base_address: u64,
    nro_heap_address: u64,
    nro_heap_size: u64,
    bss_heap_address: u64,
    bss_heap_size: u64,
    code_size: u64,
    rw_size: u64,
    module_id: ModuleId,
}

impl Default for NroInfo {
    fn default() -> Self {
        Self {
            base_address: 0,
            nro_heap_address: 0,
            nro_heap_size: 0,
            bss_heap_address: 0,
            bss_heap_size: 0,
            code_size: 0,
            rw_size: 0,
            module_id: ModuleId::default(),
        }
    }
}

/// Per-NRR tracking information.
///
/// Corresponds to `NrrInfo` in upstream ro.cpp.
#[derive(Clone, Default)]
struct NrrInfo {
    nrr_heap_address: u64,
    nrr_heap_size: u64,
    hashes: Vec<Sha256Hash>,
}

/// Per-process context for RO service.
///
/// Corresponds to `ProcessContext` in upstream ro.cpp.
struct ProcessContext {
    nro_in_use: [bool; MAX_NRO_INFOS],
    nrr_in_use: [bool; MAX_NRR_INFOS],
    nro_infos: Vec<NroInfo>,
    nrr_infos: Vec<NrrInfo>,
    /// Upstream: `Kernel::KProcess* m_process`.
    process: Option<Arc<ProcessLock>>,
    process_id: u64,
    in_use: bool,
}

impl Default for ProcessContext {
    fn default() -> Self {
        Self {
            nro_in_use: [false; MAX_NRO_INFOS],
            nrr_in_use: [false; MAX_NRR_INFOS],
            nro_infos: (0..MAX_NRO_INFOS).map(|_| NroInfo::default()).collect(),
            nrr_infos: (0..MAX_NRR_INFOS).map(|_| NrrInfo::default()).collect(),
            process: None,
            process_id: INVALID_PROCESS_ID,
            in_use: false,
        }
    }
}

impl ProcessContext {
    /// Initialize the context for a process.
    ///
    /// Corresponds to `ProcessContext::Initialize` in upstream.
    fn initialize(&mut self, process: Option<Arc<ProcessLock>>, process_id: u64) {
        assert!(!self.in_use);

        self.nro_in_use = [false; MAX_NRO_INFOS];
        self.nrr_in_use = [false; MAX_NRR_INFOS];
        for info in self.nro_infos.iter_mut() {
            *info = NroInfo::default();
        }
        for info in self.nrr_infos.iter_mut() {
            *info = NrrInfo::default();
        }

        self.process = process;
        self.process_id = process_id;
        self.in_use = true;
    }

    /// Finalize (release) the context.
    ///
    /// Corresponds to `ProcessContext::Finalize` in upstream.
    fn finalize(&mut self) {
        assert!(self.in_use);

        self.nro_in_use = [false; MAX_NRO_INFOS];
        self.nrr_in_use = [false; MAX_NRR_INFOS];
        for info in self.nro_infos.iter_mut() {
            *info = NroInfo::default();
        }
        for info in self.nrr_infos.iter_mut() {
            *info = NrrInfo::default();
        }

        self.process = None;
        self.process_id = INVALID_PROCESS_ID;
        self.in_use = false;
    }

    fn get_process(&self) -> Option<&Arc<ProcessLock>> {
        self.process.as_ref()
    }

    fn get_process_id(&self) -> u64 {
        self.process_id
    }

    fn is_free(&self) -> bool {
        !self.in_use
    }

    /// Find NRR info by heap address.
    fn get_nrr_info_index_by_address(&self, nrr_heap_address: u64) -> Result<usize, ResultCode> {
        for i in 0..MAX_NRR_INFOS {
            if self.nrr_in_use[i] && self.nrr_infos[i].nrr_heap_address == nrr_heap_address {
                return Ok(i);
            }
        }
        Err(ro_results::RESULT_NOT_REGISTERED)
    }

    /// Find a free NRR slot.
    fn get_free_nrr_info_index(&self) -> Result<usize, ResultCode> {
        for i in 0..MAX_NRR_INFOS {
            if !self.nrr_in_use[i] {
                return Ok(i);
            }
        }
        Err(ro_results::RESULT_TOO_MANY_NRR)
    }

    /// Find NRO info by base address.
    fn get_nro_info_index_by_address(&self, nro_address: u64) -> Result<usize, ResultCode> {
        for i in 0..MAX_NRO_INFOS {
            if self.nro_in_use[i] && self.nro_infos[i].base_address == nro_address {
                return Ok(i);
            }
        }
        Err(ro_results::RESULT_NOT_LOADED)
    }

    /// Find NRO info by module ID.
    fn get_nro_info_index_by_module_id(&self, module_id: &ModuleId) -> Result<usize, ResultCode> {
        for i in 0..MAX_NRO_INFOS {
            if self.nro_in_use[i] && self.nro_infos[i].module_id == *module_id {
                return Ok(i);
            }
        }
        Err(ro_results::RESULT_NOT_LOADED)
    }

    /// Find a free NRO slot.
    fn get_free_nro_info_index(&self) -> Result<usize, ResultCode> {
        for i in 0..MAX_NRO_INFOS {
            if !self.nro_in_use[i] {
                return Ok(i);
            }
        }
        Err(ro_results::RESULT_TOO_MANY_NRO)
    }

    /// Validate that the NRO hash is present in a registered NRR.
    ///
    /// Corresponds to `ProcessContext::ValidateHasNroHash` in upstream ro.cpp.
    /// Reads NRO data from process memory, computes SHA-256, and searches NRR hash lists.
    fn validate_has_nro_hash(
        &self,
        base_address: u64,
        nro_header: &NroHeader,
    ) -> Result<(), ResultCode> {
        // Calculate hash.
        let hash: Sha256Hash = {
            let size = nro_header.get_size() as u64;

            let process_arc = self
                .process
                .as_ref()
                .ok_or(ro_results::RESULT_INVALID_PROCESS)?;
            let process = process_arc.lock().unwrap();
            let nro_data = process.read_memory_vec(base_address, size as usize);

            let mut hasher = Sha256::new();
            hasher.update(&nro_data);
            let result = hasher.finalize();
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&result);
            hash
        };

        for i in 0..MAX_NRR_INFOS {
            // Ensure we only check NRRs that are in use.
            if !self.nrr_in_use[i] {
                continue;
            }

            // Locate the hash within the hash list.
            if self.nrr_infos[i].hashes.iter().any(|h| *h == hash) {
                // The hash is valid!
                return Ok(());
            }
        }

        Err(ro_results::RESULT_NOT_AUTHORIZED)
    }

    /// Validate an NRO header and check constraints.
    ///
    /// Corresponds to `ProcessContext::ValidateNro` in upstream ro.cpp.
    fn validate_nro(
        &self,
        base_address: u64,
        expected_nro_size: u64,
        expected_bss_size: u64,
    ) -> Result<(ModuleId, u64, u64, u64), ResultCode> {
        // Ensure we have a process to work on.
        let process_arc = self
            .process
            .as_ref()
            .ok_or(ro_results::RESULT_INVALID_PROCESS)?;

        // Read the NRO header from process memory.
        let header: NroHeader = {
            let process = process_arc.lock().unwrap();
            let header_bytes =
                process.read_memory_vec(base_address, std::mem::size_of::<NroHeader>());
            assert!(header_bytes.len() >= std::mem::size_of::<NroHeader>());
            unsafe { std::ptr::read_unaligned(header_bytes.as_ptr() as *const NroHeader) }
        };

        // Validate magic.
        if !header.is_magic_valid() {
            return Err(ro_results::RESULT_INVALID_NRO);
        }

        // Read sizes from header.
        let nro_size = header.get_size() as u64;
        let text_ofs = header.get_text_offset() as u64;
        let text_size = header.get_text_size() as u64;
        let ro_ofs = header.get_ro_offset() as u64;
        let ro_size = header.get_ro_size() as u64;
        let rw_ofs = header.get_rw_offset() as u64;
        let rw_size = header.get_rw_size() as u64;
        let bss_size = header.get_bss_size() as u64;

        const PAGE_SIZE: u64 = 0x1000;

        // Validate sizes meet expected.
        if nro_size != expected_nro_size {
            return Err(ro_results::RESULT_INVALID_NRO);
        }
        if bss_size != expected_bss_size {
            return Err(ro_results::RESULT_INVALID_NRO);
        }

        // Validate all sizes are aligned.
        if text_size % PAGE_SIZE != 0 {
            return Err(ro_results::RESULT_INVALID_NRO);
        }
        if ro_size % PAGE_SIZE != 0 {
            return Err(ro_results::RESULT_INVALID_NRO);
        }
        if rw_size % PAGE_SIZE != 0 {
            return Err(ro_results::RESULT_INVALID_NRO);
        }
        if bss_size % PAGE_SIZE != 0 {
            return Err(ro_results::RESULT_INVALID_NRO);
        }

        // Validate sections are in order.
        if text_ofs > ro_ofs {
            return Err(ro_results::RESULT_INVALID_NRO);
        }
        if ro_ofs > rw_ofs {
            return Err(ro_results::RESULT_INVALID_NRO);
        }

        // Validate sections are sequential and contiguous.
        if text_ofs != 0 {
            return Err(ro_results::RESULT_INVALID_NRO);
        }
        if text_ofs + text_size != ro_ofs {
            return Err(ro_results::RESULT_INVALID_NRO);
        }
        if ro_ofs + ro_size != rw_ofs {
            return Err(ro_results::RESULT_INVALID_NRO);
        }
        if rw_ofs + rw_size != nro_size {
            return Err(ro_results::RESULT_INVALID_NRO);
        }

        // Verify NRO hash.
        self.validate_has_nro_hash(base_address, &header)?;

        // Check if NRO has already been loaded.
        let module_id = header.get_module_id();
        if self.get_nro_info_index_by_module_id(module_id).is_ok() {
            return Err(ro_results::RESULT_ALREADY_LOADED);
        }

        Ok((*module_id, text_size, ro_size, rw_size))
    }
}

/// Validate address and non-zero size (both page-aligned).
///
/// Corresponds to `ValidateAddressAndNonZeroSize` in upstream ro.cpp.
fn validate_address_and_non_zero_size(address: u64, size: u64) -> Result<(), ResultCode> {
    const PAGE_SIZE: u64 = 0x1000;
    if address % PAGE_SIZE != 0 {
        return Err(ro_results::RESULT_INVALID_ADDRESS);
    }
    if size == 0 {
        return Err(ro_results::RESULT_INVALID_SIZE);
    }
    if size % PAGE_SIZE != 0 {
        return Err(ro_results::RESULT_INVALID_SIZE);
    }
    if address >= address.wrapping_add(size) {
        // Overflow check — upstream: address < address + size
        return Err(ro_results::RESULT_INVALID_SIZE);
    }
    Ok(())
}

/// Validate address and size (both page-aligned, size may be zero).
///
/// Corresponds to `ValidateAddressAndSize` in upstream ro.cpp.
fn validate_address_and_size(address: u64, size: u64) -> Result<(), ResultCode> {
    const PAGE_SIZE: u64 = 0x1000;
    if address % PAGE_SIZE != 0 {
        return Err(ro_results::RESULT_INVALID_ADDRESS);
    }
    if size % PAGE_SIZE != 0 {
        return Err(ro_results::RESULT_INVALID_SIZE);
    }
    if size != 0 && address >= address.wrapping_add(size) {
        return Err(ro_results::RESULT_INVALID_SIZE);
    }
    Ok(())
}

/// Central RO context managing process contexts.
///
/// Corresponds to `RoContext` in upstream ro.cpp.
pub struct RoContext {
    process_contexts: Vec<ProcessContext>,
    generate_random: Mt19937_64,
}

impl RoContext {
    pub fn new() -> Self {
        Self {
            process_contexts: (0..MAX_SESSIONS)
                .map(|_| ProcessContext::default())
                .collect(),
            generate_random: Mt19937_64::default(),
        }
    }

    /// Register a process.
    ///
    /// Corresponds to `RoContext::RegisterProcess` in upstream.
    pub fn register_process(
        &mut self,
        process: Option<Arc<ProcessLock>>,
        process_id: u64,
    ) -> Result<usize, ResultCode> {
        let process = process.ok_or(ro_results::RESULT_INVALID_PROCESS)?;
        if process.lock().unwrap().process_id != process_id {
            return Err(ro_results::RESULT_INVALID_PROCESS);
        }

        // Check if a process context already exists.
        if self.get_context_index_by_process_id(process_id).is_some() {
            return Err(ro_results::RESULT_INVALID_SESSION);
        }

        // Allocate a context.
        let context_id = self.allocate_context(Some(process), process_id);
        Ok(context_id)
    }

    /// Validate that a context matches a process ID.
    pub fn validate_process(&self, context_id: usize, process_id: u64) -> Result<(), ResultCode> {
        if context_id as u64 == INVALID_CONTEXT_ID {
            return Err(ro_results::RESULT_INVALID_PROCESS);
        }
        let ctx = &self.process_contexts[context_id];
        if ctx.get_process_id() != process_id {
            return Err(ro_results::RESULT_INVALID_PROCESS);
        }
        Ok(())
    }

    /// Unregister a process.
    pub fn unregister_process(&mut self, context_id: usize) {
        self.free_context(context_id);
    }

    /// Register NRR module info.
    ///
    /// Corresponds to `RoContext::RegisterModuleInfo` in upstream.
    pub fn register_module_info(
        &mut self,
        context_id: usize,
        nrr_address: u64,
        nrr_size: u64,
        _nrr_kind: NrrKind,
        _enforce_nrr_kind: bool,
    ) -> Result<(), ResultCode> {
        let context = &mut self.process_contexts[context_id];

        // Validate address/size.
        validate_address_and_non_zero_size(nrr_address, nrr_size)?;

        // Check we have space for a new NRR.
        let nrr_index = context.get_free_nrr_info_index()?;

        // Ensure we have a valid process to read from.
        let process_arc = context
            .get_process()
            .ok_or(ro_results::RESULT_INVALID_PROCESS)?
            .clone();

        // Read NRR header from process memory.
        let (num_hashes, hashes_offset) = {
            let process = process_arc.lock().unwrap();
            let header_bytes =
                process.read_memory_vec(nrr_address, std::mem::size_of::<NrrHeader>());
            assert!(header_bytes.len() >= std::mem::size_of::<NrrHeader>());
            let header: NrrHeader =
                unsafe { std::ptr::read_unaligned(header_bytes.as_ptr() as *const NrrHeader) };
            (header.get_num_hashes() as usize, header.get_hashes_offset())
        };

        // Set NRR info.
        context.nrr_in_use[nrr_index] = true;
        context.nrr_infos[nrr_index].nrr_heap_address = nrr_address;
        context.nrr_infos[nrr_index].nrr_heap_size = nrr_size;

        // Read NRR hash list from process memory.
        let hash_data_size = std::mem::size_of::<Sha256Hash>() * num_hashes;
        let mut hashes = Vec::with_capacity(num_hashes);
        if hash_data_size > 0 {
            let process = process_arc.lock().unwrap();
            let hash_bytes =
                process.read_memory_vec(nrr_address + hashes_offset as u64, hash_data_size);
            for i in 0..num_hashes {
                let offset = i * 32;
                let mut hash = [0u8; 32];
                hash.copy_from_slice(&hash_bytes[offset..offset + 32]);
                hashes.push(hash);
            }
        }
        context.nrr_infos[nrr_index].hashes = hashes;

        log::debug!(
            "RegisterModuleInfo: registered NRR at address={:#x}, size={:#x}, num_hashes={}",
            nrr_address,
            nrr_size,
            num_hashes,
        );

        Ok(())
    }

    /// Unregister NRR module info.
    ///
    /// Corresponds to `RoContext::UnregisterModuleInfo` in upstream.
    pub fn unregister_module_info(
        &mut self,
        context_id: usize,
        nrr_address: u64,
    ) -> Result<(), ResultCode> {
        const PAGE_SIZE: u64 = 0x1000;
        let context = &mut self.process_contexts[context_id];

        // Validate address.
        if nrr_address % PAGE_SIZE != 0 {
            return Err(ro_results::RESULT_INVALID_ADDRESS);
        }

        // Check the NRR is loaded.
        let nrr_index = context.get_nrr_info_index_by_address(nrr_address)?;

        // Nintendo does this unconditionally.
        context.nrr_in_use[nrr_index] = false;
        context.nrr_infos[nrr_index] = NrrInfo::default();

        Ok(())
    }

    /// Map NRO module memory.
    ///
    /// Corresponds to `RoContext::MapManualLoadModuleMemory` in upstream.
    pub fn map_manual_load_module_memory(
        &mut self,
        context_id: usize,
        nro_address: u64,
        nro_size: u64,
        bss_address: u64,
        bss_size: u64,
    ) -> Result<u64, ResultCode> {
        let (process_contexts, generate_random) =
            (&mut self.process_contexts, &mut self.generate_random);
        let context = &mut process_contexts[context_id];

        // Validate address/size.
        validate_address_and_non_zero_size(nro_address, nro_size)?;
        validate_address_and_size(bss_address, bss_size)?;

        let total_size = nro_size + bss_size;
        if total_size < nro_size {
            return Err(ro_results::RESULT_INVALID_SIZE);
        }
        if total_size < bss_size {
            return Err(ro_results::RESULT_INVALID_SIZE);
        }

        // Check we have space for a new NRO.
        let nro_index = context.get_free_nro_info_index()?;
        context.nro_infos[nro_index].nro_heap_address = nro_address;
        context.nro_infos[nro_index].nro_heap_size = nro_size;
        context.nro_infos[nro_index].bss_heap_address = bss_address;
        context.nro_infos[nro_index].bss_heap_size = bss_size;

        // Get the process for page table operations.
        let process_arc = context
            .get_process()
            .ok_or(ro_results::RESULT_INVALID_PROCESS)?
            .clone();

        // Map the NRO using the persistent engine owned by this context.
        let mut gen_random = || generate_random.next_u64();

        let base_address = {
            let mut process = process_arc.lock().unwrap();
            ro_nro_utils::map_nro(
                &mut process,
                nro_address,
                nro_size,
                bss_address,
                bss_size,
                &mut gen_random,
            )
        };

        let base_address = match base_address {
            Ok(addr) => addr,
            Err(e) => return Err(e),
        };

        context.nro_infos[nro_index].base_address = base_address;

        // Validate the NRO (parsing region extents).
        let (module_id, rx_size, ro_size, rw_size) =
            match context.validate_nro(base_address, nro_size, bss_size) {
                Ok(result) => result,
                Err(e) => {
                    // On validation failure, unmap the NRO.
                    let mut process = process_arc.lock().unwrap();
                    let _ = ro_nro_utils::unmap_nro(
                        &mut process,
                        base_address,
                        nro_address,
                        nro_size,
                        bss_address,
                        bss_size,
                    );
                    return Err(e);
                }
            };

        // Set NRO perms.
        {
            let mut process = process_arc.lock().unwrap();
            ro_nro_utils::set_nro_perms(
                &mut process,
                base_address,
                rx_size,
                ro_size,
                rw_size + bss_size,
            )
            .map_err(|e| {
                // On permission failure, unmap the NRO.
                let _ = ro_nro_utils::unmap_nro(
                    &mut process,
                    base_address,
                    nro_address,
                    nro_size,
                    bss_address,
                    bss_size,
                );
                e
            })?;
        }

        context.nro_in_use[nro_index] = true;
        context.nro_infos[nro_index].module_id = module_id;
        context.nro_infos[nro_index].code_size = rx_size + ro_size;
        context.nro_infos[nro_index].rw_size = rw_size;

        Ok(base_address)
    }

    /// Unmap NRO module memory.
    ///
    /// Corresponds to `RoContext::UnmapManualLoadModuleMemory` in upstream.
    pub fn unmap_manual_load_module_memory(
        &mut self,
        context_id: usize,
        nro_address: u64,
    ) -> Result<(), ResultCode> {
        const PAGE_SIZE: u64 = 0x1000;
        let context = &mut self.process_contexts[context_id];

        // Validate address.
        if nro_address % PAGE_SIZE != 0 {
            return Err(ro_results::RESULT_INVALID_ADDRESS);
        }

        // Check the NRO is loaded.
        let nro_index = context.get_nro_info_index_by_address(nro_address)?;

        // Save backup and clear (Nintendo does this unconditionally).
        let nro_backup = context.nro_infos[nro_index].clone();
        context.nro_in_use[nro_index] = false;
        context.nro_infos[nro_index] = NroInfo::default();

        // Get the process for unmapping.
        let process_arc = context.get_process().cloned();

        // Unmap using backup info.
        if let Some(process_arc) = process_arc {
            let mut process = process_arc.lock().unwrap();
            ro_nro_utils::unmap_nro(
                &mut process,
                nro_backup.base_address,
                nro_backup.nro_heap_address,
                nro_backup.code_size + nro_backup.rw_size,
                nro_backup.bss_heap_address,
                nro_backup.bss_heap_size,
            )
        } else {
            // No process available — upstream would still attempt the unmap
            // but we can't without a process reference.
            log::warn!("unmap_manual_load_module_memory: no process reference available for unmap");
            Ok(())
        }
    }

    // Private context helpers.

    fn get_context_index_by_process_id(&self, process_id: u64) -> Option<usize> {
        for i in 0..MAX_SESSIONS {
            if self.process_contexts[i].get_process_id() == process_id {
                return Some(i);
            }
        }
        None
    }

    fn allocate_context(&mut self, process: Option<Arc<ProcessLock>>, process_id: u64) -> usize {
        for i in 0..MAX_SESSIONS {
            if self.process_contexts[i].is_free() {
                self.process_contexts[i].initialize(process, process_id);
                return i;
            }
        }
        // Failure to find a free context is an abort condition (matching upstream).
        panic!("RoContext: no free process context available");
    }

    fn free_context(&mut self, context_id: usize) {
        if (context_id as u64) != INVALID_CONTEXT_ID && context_id < MAX_SESSIONS {
            self.process_contexts[context_id].finalize();
        }
    }
}

/// RoInterface — service interface per session.
///
/// Corresponds to `RoInterface` in upstream ro.cpp.
pub struct RoInterface {
    context_id: Mutex<u64>,
    pub nrr_kind: NrrKind,
    /// Shared RO context. Upstream passes one `std::shared_ptr<RoContext>` to
    /// both `"ldr:ro"` and `"ro:1"` factories.
    ro: Arc<Mutex<RoContext>>,
    name: String,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl RoInterface {
    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    fn resolve_process_from_handle(
        ctx: &HLERequestContext,
        process_handle: u32,
    ) -> Option<Arc<ProcessLock>> {
        let process = ctx.owner_process_arc()?;
        if process_handle == PseudoHandle::CurrentProcess as u32 || process_handle == 0 {
            return Some(process);
        }

        let (object_id, current_process_id) = {
            let process = process.lock().unwrap();
            (
                process.handle_table.get_object(process_handle)?,
                process.get_process_id(),
            )
        };
        if object_id == current_process_id {
            return Some(process);
        }

        ctx.get_system()?
            .get()
            .kernel()?
            .get_process_by_id(object_id)
    }

    fn skip_client_process_id(request: &mut CmifRequest<'_>) {
        request.align_for::<ClientProcessId>();
        let _ = request.raw::<ClientProcessId>();
    }

    fn map_manual_load_module_memory(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let process_id = ctx.get_pid();
        let mut request = CmifRequest::new(ctx);
        Self::skip_client_process_id(&mut request);
        request.align_for::<u64>();
        let nro_address = request.u64();
        let nro_size = request.u64();
        let bss_address = request.u64();
        let bss_size = request.u64();
        let context_id = *service.context_id.lock().unwrap() as usize;
        let result = (|| {
            let mut ro = service.ro.lock().unwrap();
            ro.validate_process(context_id, process_id)?;
            ro.map_manual_load_module_memory(
                context_id,
                nro_address,
                nro_size,
                bss_address,
                bss_size,
            )
        })();
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        match result {
            Ok(load_address) => {
                rb.push_result(crate::hle::result::RESULT_SUCCESS);
                rb.push_u64(load_address);
            }
            Err(result) => rb.push_result(result),
        }
    }

    fn unmap_manual_load_module_memory(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let process_id = ctx.get_pid();
        let mut request = CmifRequest::new(ctx);
        Self::skip_client_process_id(&mut request);
        request.align_for::<u64>();
        let nro_address = request.u64();
        let context_id = *service.context_id.lock().unwrap() as usize;
        let result = (|| {
            let mut ro = service.ro.lock().unwrap();
            ro.validate_process(context_id, process_id)?;
            ro.unmap_manual_load_module_memory(context_id, nro_address)
        })();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(match result {
            Ok(()) => crate::hle::result::RESULT_SUCCESS,
            Err(result) => result,
        });
    }

    fn register_module_info(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let process_id = ctx.get_pid();
        let mut request = CmifRequest::new(ctx);
        Self::skip_client_process_id(&mut request);
        request.align_for::<u64>();
        let nrr_address = request.u64();
        let nrr_size = request.u64();
        let context_id = *service.context_id.lock().unwrap() as usize;
        let result = (|| {
            let mut ro = service.ro.lock().unwrap();
            ro.validate_process(context_id, process_id)?;
            ro.register_module_info(context_id, nrr_address, nrr_size, NrrKind::User, true)
        })();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(match result {
            Ok(()) => crate::hle::result::RESULT_SUCCESS,
            Err(result) => result,
        });
    }

    fn unregister_module_info(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let process_id = ctx.get_pid();
        let mut request = CmifRequest::new(ctx);
        Self::skip_client_process_id(&mut request);
        request.align_for::<u64>();
        let nrr_address = request.u64();
        let context_id = *service.context_id.lock().unwrap() as usize;
        let result = (|| {
            let mut ro = service.ro.lock().unwrap();
            ro.validate_process(context_id, process_id)?;
            ro.unregister_module_info(context_id, nrr_address)
        })();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(match result {
            Ok(()) => crate::hle::result::RESULT_SUCCESS,
            Err(result) => result,
        });
    }

    fn register_process_handle(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let process_id = ctx.get_pid();
        let mut request = CmifRequest::new(ctx);
        Self::skip_client_process_id(&mut request);
        drop(request);
        let process_handle = ctx.get_copy_handle(0);
        let process = Self::resolve_process_from_handle(ctx, process_handle);
        let result = service
            .ro
            .lock()
            .unwrap()
            .register_process(process, process_id);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        match result {
            Ok(context_id) => {
                *service.context_id.lock().unwrap() = context_id as u64;
                rb.push_result(crate::hle::result::RESULT_SUCCESS);
            }
            Err(result) => rb.push_result(result),
        }
    }

    fn register_process_module_info(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let process_id = ctx.get_pid();
        let mut request = CmifRequest::new(ctx);
        Self::skip_client_process_id(&mut request);
        request.align_for::<u64>();
        let nrr_address = request.u64();
        let nrr_size = request.u64();
        drop(request);
        let _process_handle = ctx.get_copy_handle(0);
        let context_id = *service.context_id.lock().unwrap() as usize;
        let result = (|| {
            let mut ro = service.ro.lock().unwrap();
            ro.validate_process(context_id, process_id)?;
            ro.register_module_info(
                context_id,
                nrr_address,
                nrr_size,
                service.nrr_kind,
                service.nrr_kind == NrrKind::JitPlugin,
            )
        })();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(match result {
            Ok(()) => crate::hle::result::RESULT_SUCCESS,
            Err(result) => result,
        });
    }

    pub fn new(name: &str, ro: Arc<Mutex<RoContext>>, nrr_kind: NrrKind) -> Self {
        let handlers = build_handler_map(&[
            (
                0,
                Some(Self::map_manual_load_module_memory),
                "MapManualLoadModuleMemory",
            ),
            (
                1,
                Some(Self::unmap_manual_load_module_memory),
                "UnmapManualLoadModuleMemory",
            ),
            (2, Some(Self::register_module_info), "RegisterModuleInfo"),
            (
                3,
                Some(Self::unregister_module_info),
                "UnregisterModuleInfo",
            ),
            (
                4,
                Some(Self::register_process_handle),
                "RegisterProcessHandle",
            ),
            (
                10,
                Some(Self::register_process_module_info),
                "RegisterProcessModuleInfo",
            ),
        ]);

        Self {
            context_id: Mutex::new(INVALID_CONTEXT_ID),
            nrr_kind,
            ro,
            name: name.to_string(),
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl Drop for RoInterface {
    fn drop(&mut self) {
        let context_id = *self
            .context_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) as usize;
        self.ro
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .unregister_process(context_id);
    }
}

impl SessionRequestHandler for RoInterface {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        &self.name
    }
}

impl ServiceFramework for RoInterface {
    fn get_service_name(&self) -> &str {
        &self.name
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// LoopProcess — registers "ldr:ro" and "ro:1" services.
///
/// Corresponds to `Service::RO::LoopProcess` in upstream ro.cpp.
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::hle_ipc::SessionRequestHandlerPtr;
    use crate::hle::service::server_manager::ServerManager;

    let server_manager = ServerManager::new_shared(system);
    let ro = Arc::new(Mutex::new(RoContext::new()));

    {
        let mut server_manager = server_manager.lock().unwrap();
        let user_ro = Arc::clone(&ro);
        server_manager.register_named_service(
            "ldr:ro",
            Box::new(move || -> SessionRequestHandlerPtr {
                Arc::new(RoInterface::new(
                    "ldr:ro",
                    Arc::clone(&user_ro),
                    NrrKind::User,
                ))
            }),
            2,
        );

        let jit_plugin_ro = Arc::clone(&ro);
        server_manager.register_named_service(
            "ro:1",
            Box::new(move || -> SessionRequestHandlerPtr {
                Arc::new(RoInterface::new(
                    "ro:1",
                    Arc::clone(&jit_plugin_ro),
                    NrrKind::JitPlugin,
                ))
            }),
            2,
        );
        server_manager.register_named_service(
            "ro:dmnt",
            Box::new(|| -> SessionRequestHandlerPtr { Arc::new(IDebugMonitorInterface::new()) }),
            2,
        );
    }

    ServerManager::run_server_shared(server_manager);
}

pub struct IDebugMonitorInterface {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IDebugMonitorInterface {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(0, None, "GetProcessModuleInfo")]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IDebugMonitorInterface {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        "ro:dmnt"
    }
}

impl ServiceFramework for IDebugMonitorInterface {
    fn get_service_name(&self) -> &str {
        "ro:dmnt"
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
    use crate::core::{System, SystemRef};
    use crate::hle::ipc;
    use crate::hle::kernel::k_process::KProcess;
    use crate::hle::kernel::k_thread::{KThread, KThreadLock};
    use crate::hle::kernel::kernel::ScopedKernelForTest;

    #[test]
    fn ro_random_engine_matches_std_mt19937_64_default_sequence() {
        let mut random = Mt19937_64::default();
        assert_eq!(random.next_u64(), 14_514_284_786_278_117_030);
        assert_eq!(random.next_u64(), 4_620_546_740_167_642_908);
        assert_eq!(random.next_u64(), 13_109_570_281_517_897_720);
    }

    #[test]
    fn ro_context_uses_edens_extended_module_capacities() {
        let ro = RoContext::new();
        assert_eq!(ro.process_contexts.len(), MAX_SESSIONS);
        assert!(ro
            .process_contexts
            .iter()
            .all(|context| context.nro_infos.len() == 0x100 && context.nrr_infos.len() == 0x100));
    }

    #[test]
    fn ro_interface_registers_every_upstream_command() {
        let ro = Arc::new(Mutex::new(RoContext::new()));
        let service = RoInterface::new("ldr:ro", ro, NrrKind::User);

        for command in [0, 1, 2, 3, 4, 10] {
            assert!(service.handlers[&command].handler_callback.is_some());
        }
    }

    #[test]
    fn debug_monitor_table_matches_upstream() {
        assert_eq!(IDebugMonitorInterface::new().handlers.len(), 1);
    }

    #[test]
    fn register_process_validates_pid_and_rejects_duplicate_context() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        process.lock().unwrap().process_id = 81;
        let mut ro = RoContext::new();

        assert_eq!(
            ro.register_process(Some(Arc::clone(&process)), 82),
            Err(ro_results::RESULT_INVALID_PROCESS)
        );
        let context_id = ro.register_process(Some(Arc::clone(&process)), 81).unwrap();
        assert_eq!(
            ro.register_process(Some(process), 81),
            Err(ro_results::RESULT_INVALID_SESSION)
        );
        ro.unregister_process(context_id);
    }

    #[test]
    fn register_process_uses_the_out_of_band_client_process_id() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        process.lock().unwrap().process_id = 0x51;
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));

        let mut ctx = HLERequestContext::new_with_thread(thread, 0x2000);
        ctx.populate_from_incoming_command_buffer(&[
            ipc::CommandType::Request as u32,
            1u32 << 31,
            1 | (1 << 1),
            0,
            0,
            PseudoHandle::CurrentProcess as u32,
        ]);

        let ro = Arc::new(Mutex::new(RoContext::new()));
        let service = RoInterface::new("ldr:ro", Arc::clone(&ro), NrrKind::User);
        RoInterface::register_process_handle(&service, &mut ctx);

        let context_id = *service.context_id.lock().unwrap() as usize;
        assert_ne!(context_id as u64, INVALID_CONTEXT_ID);
        assert_eq!(
            ro.lock().unwrap().validate_process(context_id, 0x51),
            Ok(())
        );
    }

    #[test]
    fn explicit_process_handle_resolves_the_handle_target() {
        let mut system = System::new_for_test();
        let current = Arc::new(ProcessLock::from_value(KProcess::new()));
        let target = Arc::new(ProcessLock::from_value(KProcess::new()));
        {
            let mut current = current.lock().unwrap();
            current.process_id = 0x51;
            current.initialize_handle_table();
        }
        target.lock().unwrap().process_id = 0x52;

        let target_handle = current.lock().unwrap().handle_table.add(0x52).unwrap();
        system.set_current_process_arc(Arc::clone(&current));
        system
            .kernel()
            .unwrap()
            .register_process(Arc::clone(&target));

        let mut scoped_kernel = ScopedKernelForTest::new();
        scoped_kernel
            .kernel_mut()
            .set_system_ref(SystemRef::from_ref(&system));

        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&current));
        let ctx = HLERequestContext::new_with_thread(thread, 0x2000);

        let resolved = RoInterface::resolve_process_from_handle(&ctx, target_handle).unwrap();
        assert!(Arc::ptr_eq(&resolved, &target));
    }

    #[test]
    fn dropping_interface_recovers_a_poisoned_ro_context() {
        let ro = Arc::new(Mutex::new(RoContext::new()));
        let poisoned = Arc::clone(&ro);
        assert!(std::thread::spawn(move || {
            let _guard = poisoned.lock().unwrap();
            panic!("poison RO context for Drop regression test");
        })
        .join()
        .is_err());

        let service = RoInterface::new("ldr:ro", ro, NrrKind::User);
        drop(service);
    }

    #[test]
    fn ro_request_reserves_raw_space_for_the_out_of_band_process_id() {
        let mut ctx = HLERequestContext::new();
        ctx.cmd_buf[2] = 0;
        ctx.cmd_buf[3] = 0;
        ctx.cmd_buf[4] = 0x0d15_4000;
        ctx.cmd_buf[5] = 0x0000_0021;
        ctx.cmd_buf[6] = 0x0000_0080;
        ctx.cmd_buf[7] = 0;

        let mut request = CmifRequest::new(&mut ctx);
        RoInterface::skip_client_process_id(&mut request);
        request.align_for::<u64>();

        assert_eq!(request.u64(), 0x0000_0021_0d15_4000);
        assert_eq!(request.u64(), 0x80);
    }
}

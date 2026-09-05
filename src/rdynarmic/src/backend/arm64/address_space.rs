//! AArch64 host address-space owner.
//!
//! This mirrors the cache ownership layer in upstream
//! `backend/arm64/address_space.h/.cpp`: it owns executable memory, prelude
//! entry points, block-entry maps, reverse lookup maps, and invalidation state.
//! Actual ARM64 IR emission/link patching is added in later backend slices.

use std::collections::{BTreeMap, HashSet};
use std::ffi::c_void;
use std::path::PathBuf;

use crate::ir::block::Block;
use crate::ir::location::{A32LocationDescriptor, LocationDescriptor};

use super::abi::XSCRATCH1;
use super::block_of_code::{BlockOfCode, DEFAULT_CODE_SIZE};
use super::emit_arm64::{
    emit_arm64, BlockRelocation, BlockRelocationType, CodePtr, EmitConfig, EmittedBlockInfo,
    LinkTarget,
};
use super::fast_hash::{arm64_code_cache_profile_enabled, FastHashMap, FastHashSet};
use super::inst;
use super::prelude::{self, DispatcherCallback, PreludeInfo};

pub struct AddressSpace {
    code_cache_size: usize,
    code: BlockOfCode,
    prelude_info: Option<PreludeInfo>,
    block_entries: FastHashMap<LocationDescriptor, CodePtr>,
    reverse_block_entries: BTreeMap<usize, LocationDescriptor>,
    block_infos: FastHashMap<usize, EmittedBlockInfo>,
    block_references: FastHashMap<LocationDescriptor, FastHashSet<usize>>,
    emitted_block_count: u64,
    clear_cache_count: u64,
    block_link_slots: u64,
    rsb_link_slots: u64,
    relink_to_block_count: u64,
    relink_to_dispatcher_count: u64,
    profile_code_cache: bool,
}

impl AddressSpace {
    pub fn new(code_cache_size: usize) -> Result<Self, String> {
        if code_cache_size > DEFAULT_CODE_SIZE {
            return Err(format!(
                "ARM64 code_cache_size > 128 MiB not currently supported: {code_cache_size}"
            ));
        }

        let code = BlockOfCode::with_size(code_cache_size)?;

        Ok(Self {
            code_cache_size,
            code,
            prelude_info: None,
            block_entries: FastHashMap::default(),
            reverse_block_entries: BTreeMap::new(),
            block_infos: FastHashMap::default(),
            block_references: FastHashMap::default(),
            emitted_block_count: 0,
            clear_cache_count: 0,
            block_link_slots: 0,
            rsb_link_slots: 0,
            relink_to_block_count: 0,
            relink_to_dispatcher_count: 0,
            profile_code_cache: arm64_code_cache_profile_enabled(),
        })
    }

    /// Emit the current bootstrap prelude.
    ///
    /// Upstream `AddressSpace` only owns executable memory; ISA-specific
    /// owners fill `prelude_info` from `A32AddressSpace::EmitPrelude()` or
    /// `A64AddressSpace::EmitPrelude()`. Until those full methods are ported,
    /// the Rust ISA owners call this bootstrap subset explicitly.
    pub fn emit_bootstrap_prelude(&mut self) -> Result<(), String> {
        self.emit_bootstrap_prelude_with_dispatcher(None)
    }

    pub fn emit_bootstrap_prelude_with_dispatcher(
        &mut self,
        dispatcher: Option<DispatcherCallback>,
    ) -> Result<(), String> {
        self.emit_bootstrap_prelude_with_options(prelude::PreludeOptions {
            isa: prelude::PreludeIsa::A32,
            dispatcher,
            return_stack_buffer: false,
            page_table_pointer: 0,
            fastmem_pointer: 0,
        })
    }

    pub fn emit_bootstrap_prelude_with_options(
        &mut self,
        options: prelude::PreludeOptions,
    ) -> Result<(), String> {
        if self.prelude_info.is_some() {
            return Err("ARM64 prelude already emitted".to_string());
        }
        self.prelude_info = Some(prelude::emit_bootstrap_prelude_with_options(
            &mut self.code,
            options,
        )?);
        Ok(())
    }

    pub fn get(&self, descriptor: LocationDescriptor) -> Option<CodePtr> {
        self.block_entries.get(&descriptor).copied()
    }

    pub fn get_or_emit(&mut self, descriptor: LocationDescriptor) -> Result<CodePtr, String> {
        if let Some(block_entry) = self.get(descriptor) {
            return Ok(block_entry);
        }

        Err(format!(
            "ARM64 EmitArm64 is not ported for missing block {descriptor}"
        ))
    }

    pub(crate) fn emit(
        &mut self,
        block: Block,
        config: EmitConfig,
    ) -> Result<EmittedBlockInfo, String> {
        if self.remaining_size() < 1024 * 1024 {
            self.clear_cache()?;
        }

        self.code.unprotect();
        let location = block.location;
        let block_info = emit_arm64(&mut self.code, block, config)?;
        let relinked_ranges = self.record_emitted_block(location, block_info.clone())?;
        let block_offset = (block_info.entry_point as usize)
            .checked_sub(self.code.code_base_ptr() as usize)
            .ok_or_else(|| "ARM64 emitted block entry precedes code cache base".to_string())?;
        let mut flush_ranges = Vec::with_capacity(1 + relinked_ranges.len());
        flush_ranges.push((block_offset, block_info.size));
        flush_ranges.extend(relinked_ranges);
        self.code.seal_ranges(&flush_ranges);
        self.emitted_block_count = self.emitted_block_count.saturating_add(1);
        self.maybe_log_code_cache_emit(location, &block_info);
        Ok(block_info)
    }

    /// Diagnostic: write every emitted block as
    /// `host_entry_hex guest_descriptor_hex size` lines. Used to attribute
    /// host-profiler samples (e.g. macOS `sample`) inside JIT code back to
    /// guest locations.
    pub fn dump_block_map(&self, out: &mut dyn std::io::Write) -> std::io::Result<()> {
        for (&entry, descriptor) in &self.reverse_block_entries {
            let size = self
                .block_infos
                .get(&entry)
                .map(|info| info.size)
                .unwrap_or(0);
            writeln!(out, "{entry:X} {:X} {size}", descriptor.value())?;
        }
        Ok(())
    }

    pub fn reverse_get_location(&self, host_pc: CodePtr) -> Option<LocationDescriptor> {
        self.reverse_block_entries
            .range(..=host_pc as usize)
            .next_back()
            .map(|(_, descriptor)| *descriptor)
    }

    pub fn reverse_get_entry_point(&self, host_pc: CodePtr) -> Option<CodePtr> {
        self.reverse_block_entries
            .range(..=host_pc as usize)
            .next_back()
            .map(|(entry_point, _)| *entry_point as CodePtr)
    }

    pub fn invalidate_basic_blocks(&mut self, descriptors: &HashSet<LocationDescriptor>) {
        self.code.unprotect();
        let mut relinked_ranges = Vec::new();
        for descriptor in descriptors {
            if self.block_entries.contains_key(descriptor) {
                if let Ok(ranges) = self.relink_for_descriptor(*descriptor, None) {
                    relinked_ranges.extend(ranges);
                }
                self.block_entries.remove(descriptor);
            }
        }
        self.code.seal_ranges(&relinked_ranges);
    }

    pub fn clear_cache(&mut self) -> Result<(), String> {
        self.clear_cache_count = self.clear_cache_count.saturating_add(1);
        self.maybe_log_code_cache_clear();
        self.block_entries.clear();
        self.reverse_block_entries.clear();
        self.block_infos.clear();
        self.block_references.clear();
        self.block_link_slots = 0;
        self.rsb_link_slots = 0;
        self.relink_to_block_count = 0;
        self.relink_to_dispatcher_count = 0;
        let prelude_end = self
            .prelude_info
            .as_ref()
            .map_or(0, |prelude_info| prelude_info.end_of_prelude);
        self.code.set_code_size(prelude_end)
    }

    pub fn remaining_size(&self) -> usize {
        self.code_cache_size.saturating_sub(self.code.code_size())
    }

    fn maybe_log_code_cache_emit(
        &self,
        location: LocationDescriptor,
        block_info: &EmittedBlockInfo,
    ) {
        if !self.profile_code_cache {
            return;
        }

        let count = self.emitted_block_count;
        if count <= 16 || count.is_power_of_two() || self.remaining_size() < 2 * 1024 * 1024 {
            log::info!(
                "[ARM64_CODE_CACHE] emit#{} clear#{} location={} entry=0x{:X} size={} code_size={} remaining={} blocks={} refs={}",
                count,
                self.clear_cache_count,
                location,
                block_info.entry_point as usize,
                block_info.size,
                self.code.code_size(),
                self.remaining_size(),
                self.block_entries.len(),
                self.block_references.len(),
            );
            if self.block_link_slots != 0 || self.rsb_link_slots != 0 {
                log::info!(
                    "[ARM64_CODE_CACHE] links block_slots={} rsb_slots={} relink_block={} relink_dispatcher={}",
                    self.block_link_slots,
                    self.rsb_link_slots,
                    self.relink_to_block_count,
                    self.relink_to_dispatcher_count,
                );
            }
        }
    }

    fn maybe_log_code_cache_clear(&self) {
        if !self.profile_code_cache {
            return;
        }

        log::warn!(
            "[ARM64_CODE_CACHE] clear#{} emitted={} code_size={} remaining={} blocks={} infos={} refs={}",
            self.clear_cache_count,
            self.emitted_block_count,
            self.code.code_size(),
            self.remaining_size(),
            self.block_entries.len(),
            self.block_infos.len(),
            self.block_references.len(),
        );
    }

    pub fn prelude_info(&self) -> &PreludeInfo {
        self.prelude_info
            .as_ref()
            .expect("ARM64 prelude has not been emitted")
    }

    pub fn prelude_info_mut(&mut self) -> &mut PreludeInfo {
        self.prelude_info
            .as_mut()
            .expect("ARM64 prelude has not been emitted")
    }

    pub fn emit_call_trampoline(
        &mut self,
        this_ptr: *const c_void,
        fn_ptr: *const c_void,
    ) -> Result<CodePtr, String> {
        let target = prelude::emit_call_trampoline(&mut self.code, this_ptr, fn_ptr)?;
        self.finish_prelude_trampoline(target)
    }

    pub fn emit_wrapped_read_call_trampoline(
        &mut self,
        this_ptr: *const c_void,
        fn_ptr: *const c_void,
    ) -> Result<CodePtr, String> {
        let target = prelude::emit_wrapped_read_call_trampoline(&mut self.code, this_ptr, fn_ptr)?;
        self.finish_prelude_trampoline(target)
    }

    pub fn emit_wrapped_write_call_trampoline(
        &mut self,
        this_ptr: *const c_void,
        fn_ptr: *const c_void,
    ) -> Result<CodePtr, String> {
        let target = prelude::emit_wrapped_write_call_trampoline(&mut self.code, this_ptr, fn_ptr)?;
        self.finish_prelude_trampoline(target)
    }

    pub fn emit_read128_call_trampoline(
        &mut self,
        this_ptr: *const c_void,
        fn_ptr: *const c_void,
    ) -> Result<CodePtr, String> {
        let target = prelude::emit_read128_call_trampoline(&mut self.code, this_ptr, fn_ptr)?;
        self.finish_prelude_trampoline(target)
    }

    pub fn emit_wrapped_read128_call_trampoline(
        &mut self,
        this_ptr: *const c_void,
        fn_ptr: *const c_void,
    ) -> Result<CodePtr, String> {
        let target =
            prelude::emit_wrapped_read128_call_trampoline(&mut self.code, this_ptr, fn_ptr)?;
        self.finish_prelude_trampoline(target)
    }

    pub fn emit_write128_call_trampoline(
        &mut self,
        this_ptr: *const c_void,
        fn_ptr: *const c_void,
    ) -> Result<CodePtr, String> {
        let target = prelude::emit_write128_call_trampoline(&mut self.code, this_ptr, fn_ptr)?;
        self.finish_prelude_trampoline(target)
    }

    pub fn emit_wrapped_write128_call_trampoline(
        &mut self,
        this_ptr: *const c_void,
        fn_ptr: *const c_void,
    ) -> Result<CodePtr, String> {
        let target =
            prelude::emit_wrapped_write128_call_trampoline(&mut self.code, this_ptr, fn_ptr)?;
        self.finish_prelude_trampoline(target)
    }

    fn finish_prelude_trampoline(&mut self, target: CodePtr) -> Result<CodePtr, String> {
        let end_of_prelude = self.code.code_size();
        self.prelude_info_mut().end_of_prelude = end_of_prelude;
        self.code.seal();
        Ok(target)
    }

    pub fn code(&self) -> &BlockOfCode {
        &self.code
    }

    #[cfg(test)]
    pub(crate) fn insert_emitted_block(
        &mut self,
        location: LocationDescriptor,
        block_info: EmittedBlockInfo,
    ) -> Result<(), String> {
        self.record_emitted_block(location, block_info).map(|_| ())
    }

    fn record_emitted_block(
        &mut self,
        location: LocationDescriptor,
        block_info: EmittedBlockInfo,
    ) -> Result<Vec<(usize, usize)>, String> {
        if self.block_entries.contains_key(&location) {
            return Err(format!("ARM64 block already exists for {location}"));
        }
        let entry_point_key = block_info.entry_point as usize;
        if self.reverse_block_entries.contains_key(&entry_point_key) {
            return Err(format!(
                "ARM64 reverse block entry already exists for {entry_point_key:#x}"
            ));
        }
        if self.block_infos.contains_key(&entry_point_key) {
            return Err(format!(
                "ARM64 block info already exists for {entry_point_key:#x}"
            ));
        }

        for target_descriptor in block_info.block_relocations.keys() {
            self.block_references
                .entry(*target_descriptor)
                .or_default()
                .insert(entry_point_key);
        }

        self.block_entries.insert(location, block_info.entry_point);
        self.reverse_block_entries.insert(entry_point_key, location);
        self.block_infos.insert(entry_point_key, block_info.clone());

        if self.profile_code_cache {
            for relocations in block_info.block_relocations.values() {
                for relocation in relocations {
                    match relocation.relocation_type {
                        BlockRelocationType::Branch => {
                            self.block_link_slots = self.block_link_slots.saturating_add(1);
                        }
                        BlockRelocationType::MoveToScratch1 => {
                            self.rsb_link_slots = self.rsb_link_slots.saturating_add(1);
                        }
                    }
                }
            }
        }

        if let Err(err) = self.link(&block_info) {
            self.remove_recorded_block(location, entry_point_key, &block_info);
            return Err(err);
        }
        match self.relink_for_descriptor(location, Some(entry_point_key as CodePtr)) {
            Ok(relinked_ranges) => Ok(relinked_ranges),
            Err(err) => {
                self.remove_recorded_block(location, entry_point_key, &block_info);
                Err(err)
            }
        }
    }

    fn remove_recorded_block(
        &mut self,
        location: LocationDescriptor,
        entry_point_key: usize,
        block_info: &EmittedBlockInfo,
    ) {
        self.block_entries.remove(&location);
        self.reverse_block_entries.remove(&entry_point_key);
        self.block_infos.remove(&entry_point_key);
        for target_descriptor in block_info.block_relocations.keys() {
            if let Some(references) = self.block_references.get_mut(target_descriptor) {
                references.remove(&entry_point_key);
                if references.is_empty() {
                    self.block_references.remove(target_descriptor);
                }
            }
        }
    }

    fn link(&mut self, block_info: &EmittedBlockInfo) -> Result<(), String> {
        for relocation in &block_info.relocations {
            let source = relocation_source(block_info.entry_point, relocation.code_offset)?;
            let target = self.resolve_link_target(relocation.target)?;
            let instruction = branch_instruction(source, target, relocation.target.is_bl_target())?;
            self.patch_instruction(source, instruction)?;
        }

        for (target_descriptor, list) in &block_info.block_relocations {
            let target_ptr = self.get(*target_descriptor);
            self.link_block_links(block_info.entry_point, target_ptr, list)?;
        }

        Ok(())
    }

    fn resolve_link_target(&self, target: LinkTarget) -> Result<CodePtr, String> {
        let prelude_info = self
            .prelude_info
            .as_ref()
            .ok_or_else(|| "ARM64 prelude has not been emitted".to_string())?;
        match target {
            LinkTarget::ReturnToDispatcher => Ok(prelude_info.return_to_dispatcher),
            LinkTarget::ReturnFromRunCode => Ok(prelude_info.return_from_run_code),
            LinkTarget::ReadMemory8 => self.callback_target(target, prelude_info.read_memory_8),
            LinkTarget::ReadMemory16 => self.callback_target(target, prelude_info.read_memory_16),
            LinkTarget::ReadMemory32 => self.callback_target(target, prelude_info.read_memory_32),
            LinkTarget::ReadMemory64 => self.callback_target(target, prelude_info.read_memory_64),
            LinkTarget::ReadMemory128 => self.callback_target(target, prelude_info.read_memory_128),
            LinkTarget::WrappedReadMemory8 => {
                self.callback_target(target, prelude_info.wrapped_read_memory_8)
            }
            LinkTarget::WrappedReadMemory16 => {
                self.callback_target(target, prelude_info.wrapped_read_memory_16)
            }
            LinkTarget::WrappedReadMemory32 => {
                self.callback_target(target, prelude_info.wrapped_read_memory_32)
            }
            LinkTarget::WrappedReadMemory64 => {
                self.callback_target(target, prelude_info.wrapped_read_memory_64)
            }
            LinkTarget::WrappedReadMemory128 => {
                self.callback_target(target, prelude_info.wrapped_read_memory_128)
            }
            LinkTarget::ExclusiveReadMemory8 => {
                self.callback_target(target, prelude_info.exclusive_read_memory_8)
            }
            LinkTarget::ExclusiveReadMemory16 => {
                self.callback_target(target, prelude_info.exclusive_read_memory_16)
            }
            LinkTarget::ExclusiveReadMemory32 => {
                self.callback_target(target, prelude_info.exclusive_read_memory_32)
            }
            LinkTarget::ExclusiveReadMemory64 => {
                self.callback_target(target, prelude_info.exclusive_read_memory_64)
            }
            LinkTarget::ExclusiveReadMemory128 => {
                self.callback_target(target, prelude_info.exclusive_read_memory_128)
            }
            LinkTarget::WriteMemory8 => self.callback_target(target, prelude_info.write_memory_8),
            LinkTarget::WriteMemory16 => self.callback_target(target, prelude_info.write_memory_16),
            LinkTarget::WriteMemory32 => self.callback_target(target, prelude_info.write_memory_32),
            LinkTarget::WriteMemory64 => self.callback_target(target, prelude_info.write_memory_64),
            LinkTarget::WriteMemory128 => {
                self.callback_target(target, prelude_info.write_memory_128)
            }
            LinkTarget::WrappedWriteMemory8 => {
                self.callback_target(target, prelude_info.wrapped_write_memory_8)
            }
            LinkTarget::WrappedWriteMemory16 => {
                self.callback_target(target, prelude_info.wrapped_write_memory_16)
            }
            LinkTarget::WrappedWriteMemory32 => {
                self.callback_target(target, prelude_info.wrapped_write_memory_32)
            }
            LinkTarget::WrappedWriteMemory64 => {
                self.callback_target(target, prelude_info.wrapped_write_memory_64)
            }
            LinkTarget::WrappedWriteMemory128 => {
                self.callback_target(target, prelude_info.wrapped_write_memory_128)
            }
            LinkTarget::ExclusiveWriteMemory8 => {
                self.callback_target(target, prelude_info.exclusive_write_memory_8)
            }
            LinkTarget::ExclusiveWriteMemory16 => {
                self.callback_target(target, prelude_info.exclusive_write_memory_16)
            }
            LinkTarget::ExclusiveWriteMemory32 => {
                self.callback_target(target, prelude_info.exclusive_write_memory_32)
            }
            LinkTarget::ExclusiveWriteMemory64 => {
                self.callback_target(target, prelude_info.exclusive_write_memory_64)
            }
            LinkTarget::ExclusiveWriteMemory128 => {
                self.callback_target(target, prelude_info.exclusive_write_memory_128)
            }
            LinkTarget::CallSVC => self.callback_target(target, prelude_info.call_svc),
            LinkTarget::ExceptionRaised => {
                self.callback_target(target, prelude_info.exception_raised)
            }
            LinkTarget::InstructionSynchronizationBarrierRaised => {
                self.callback_target(target, prelude_info.isb_raised)
            }
            LinkTarget::InstructionCacheOperationRaised => {
                self.callback_target(target, prelude_info.ic_raised)
            }
            LinkTarget::DataCacheOperationRaised => {
                self.callback_target(target, prelude_info.dc_raised)
            }
            LinkTarget::GetCNTPCT => self.callback_target(target, prelude_info.get_cntpct),
            LinkTarget::AddTicks => self.callback_target(target, prelude_info.add_ticks),
            LinkTarget::GetTicksRemaining => {
                self.callback_target(target, prelude_info.get_ticks_remaining)
            }
        }
    }

    fn callback_target(&self, target: LinkTarget, ptr: Option<CodePtr>) -> Result<CodePtr, String> {
        ptr.ok_or_else(|| format!("ARM64 prelude trampoline is not emitted yet: {target:?}"))
    }

    fn link_block_links(
        &mut self,
        entry_point: CodePtr,
        target_ptr: Option<CodePtr>,
        block_relocations: &[BlockRelocation],
    ) -> Result<(), String> {
        for relocation in block_relocations {
            let source = relocation_source(entry_point, relocation.code_offset)?;
            match relocation.relocation_type {
                BlockRelocationType::Branch => {
                    let instruction = if let Some(target) = target_ptr {
                        branch_instruction(source, target, false)?
                    } else {
                        inst::nop()
                    };
                    self.patch_instruction(source, instruction)?;
                }
                BlockRelocationType::MoveToScratch1 => {
                    let target = target_ptr.unwrap_or(
                        self.prelude_info
                            .as_ref()
                            .ok_or_else(|| "ARM64 prelude has not been emitted".to_string())?
                            .return_to_dispatcher,
                    );
                    let instructions = adrl_instructions(source, target, XSCRATCH1)?;
                    self.patch_instruction(source, instructions[0])?;
                    self.patch_instruction(unsafe { source.add(4) }, instructions[1])?;
                }
            }
        }
        Ok(())
    }

    fn patch_instruction(&mut self, source: CodePtr, instruction: u32) -> Result<(), String> {
        let base = self.code.code_base_ptr() as usize;
        let source = source as usize;
        if source < base {
            return Err(format!(
                "ARM64 patch source before code cache: {source:#x} < {base:#x}"
            ));
        }
        let offset = source - base;
        self.code.patch_u32_deferred_icache(offset, instruction)
    }

    fn relink_for_descriptor(
        &mut self,
        target_descriptor: LocationDescriptor,
        target_ptr: Option<CodePtr>,
    ) -> Result<Vec<(usize, usize)>, String> {
        let Some(references) = self.block_references.get(&target_descriptor) else {
            return Ok(Vec::new());
        };
        let references: Vec<usize> = references.iter().copied().collect();
        let mut relinked_ranges = Vec::new();
        for entry_point in references {
            let Some(block_info) = self.block_infos.get(&entry_point) else {
                continue;
            };
            let Some(relocations) = block_info.block_relocations.get(&target_descriptor) else {
                continue;
            };
            let block_size = block_info.size;
            let block_info_for_dump = block_info.clone();
            let relocations = relocations.clone();
            if self.profile_code_cache {
                if target_ptr.is_some() {
                    self.relink_to_block_count = self.relink_to_block_count.saturating_add(1);
                } else {
                    self.relink_to_dispatcher_count =
                        self.relink_to_dispatcher_count.saturating_add(1);
                }
            }
            self.link_block_links(entry_point as CodePtr, target_ptr, &relocations)?;
            if let Some(descriptor) = self.reverse_block_entries.get(&entry_point).copied() {
                dump_relinked_a32_block_if_requested(
                    descriptor,
                    &block_info_for_dump,
                    target_ptr.is_some(),
                );
            }
            let block_offset = entry_point
                .checked_sub(self.code.code_base_ptr() as usize)
                .ok_or_else(|| "ARM64 relinked block precedes code cache base".to_string())?;
            relinked_ranges.push((block_offset, block_size));
        }
        Ok(relinked_ranges)
    }
}

fn dump_relinked_a32_block_if_requested(
    descriptor: LocationDescriptor,
    block_info: &EmittedBlockInfo,
    linked_to_block: bool,
) {
    let Some((lo, hi)) = dump_arm64_relink_block_range() else {
        return;
    };
    let pc = A32LocationDescriptor::from_location(descriptor).pc();
    if pc < lo || pc >= hi {
        return;
    }

    let dir = std::env::var_os("RUZU_DUMP_ARM64_RELINK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/ruzu-arm64-relinked-blocks"));
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "[ARM64_RELINK_DUMP] failed to create {}: {err}",
            dir.display()
        );
        return;
    }

    let target = if linked_to_block {
        "to_block"
    } else {
        "to_dispatcher"
    };
    let path = dir.join(format!(
        "a32_{pc:08X}_host_{:016X}_{target}.bin",
        block_info.entry_point as usize
    ));
    let bytes = unsafe { std::slice::from_raw_parts(block_info.entry_point, block_info.size) };
    match std::fs::write(&path, bytes) {
        Ok(()) => eprintln!(
            "[ARM64_RELINK_DUMP] pc=0x{pc:08X} host=0x{:016X} target={} size={} path={}",
            block_info.entry_point as usize,
            target,
            block_info.size,
            path.display()
        ),
        Err(err) => eprintln!(
            "[ARM64_RELINK_DUMP] failed to write pc=0x{pc:08X} path={}: {err}",
            path.display()
        ),
    }
}

fn dump_arm64_relink_block_range() -> Option<(u32, u32)> {
    static RANGE: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
    *RANGE.get_or_init(|| {
        let raw = std::env::var("RUZU_DUMP_ARM64_RELINK_PC").ok()?;
        let (lo, hi) = raw.split_once('-')?;
        let parse = |value: &str| -> Option<u32> {
            let value = value.trim();
            let value = value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
                .unwrap_or(value);
            u32::from_str_radix(value, 16).ok()
        };
        let lo = parse(lo)?;
        let hi = parse(hi)?;
        (lo < hi).then_some((lo, hi))
    })
}

fn relocation_source(entry_point: CodePtr, code_offset: isize) -> Result<CodePtr, String> {
    if code_offset < 0 {
        return Err(format!("ARM64 negative relocation offset: {code_offset}"));
    }
    Ok(unsafe { entry_point.add(code_offset as usize) })
}

fn branch_instruction(source: CodePtr, target: CodePtr, link: bool) -> Result<u32, String> {
    let offset = (target as isize)
        .checked_sub(source as isize)
        .ok_or_else(|| "ARM64 branch offset overflow".to_string())?;
    Ok(if link {
        inst::bl_imm(offset)
    } else {
        inst::b_imm(offset)
    })
}

fn adrl_instructions(source: CodePtr, target: CodePtr, rd: u8) -> Result<[u32; 2], String> {
    let source_page = (source as usize) & !0xfff;
    let target_page = (target as usize) & !0xfff;
    let page_offset = (target_page as isize)
        .checked_sub(source_page as isize)
        .ok_or_else(|| "ARM64 ADRL page offset overflow".to_string())?;
    let low_offset = (target as usize & 0xfff) as u32;
    Ok([
        inst::adrp(rd, page_offset),
        inst::add_x_imm(rd, rd, low_offset),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::arm64::emit_arm64::Relocation;
    use crate::backend::arm64::inst;

    fn fake_block(entry_point: CodePtr, size: usize) -> EmittedBlockInfo {
        EmittedBlockInfo {
            entry_point,
            size,
            relocations: Vec::new(),
            block_relocations: FastHashMap::default(),
            fastmem_patch_info: FastHashMap::default(),
        }
    }

    fn read_instruction(address_space: &AddressSpace, offset: usize) -> u32 {
        unsafe {
            address_space
                .code()
                .code_base_ptr()
                .add(offset)
                .cast::<u32>()
                .read_unaligned()
        }
    }

    fn address_space_with_prelude() -> AddressSpace {
        let mut address_space = AddressSpace::new(4096).expect("address space");
        address_space
            .emit_bootstrap_prelude()
            .expect("bootstrap prelude");
        address_space
    }

    #[test]
    fn address_space_starts_after_prelude() {
        let address_space = address_space_with_prelude();
        let prelude_end = address_space.prelude_info().end_of_prelude;
        assert_eq!(address_space.code().code_size(), prelude_end);
        assert_eq!(address_space.remaining_size(), 4096 - prelude_end);
    }

    #[test]
    fn get_and_reverse_lookup_match_upstream_maps() {
        let mut address_space = address_space_with_prelude();
        let base = address_space.code().code_base_ptr();
        let loc_a = LocationDescriptor::new(0x1000);
        let loc_b = LocationDescriptor::new(0x2000);
        let entry_a = unsafe { base.add(512) };
        let entry_b = unsafe { base.add(768) };

        address_space
            .record_emitted_block(loc_a, fake_block(entry_a, 64))
            .unwrap();
        address_space
            .record_emitted_block(loc_b, fake_block(entry_b, 64))
            .unwrap();

        assert_eq!(address_space.get(loc_a), Some(entry_a));
        assert_eq!(address_space.get(LocationDescriptor::new(0x3000)), None);
        assert_eq!(
            address_space.reverse_get_location(unsafe { base.add(600) }),
            Some(loc_a)
        );
        assert_eq!(
            address_space.reverse_get_entry_point(unsafe { base.add(900) }),
            Some(entry_b)
        );
        assert_eq!(
            address_space.reverse_get_location(unsafe { base.add(128) }),
            None
        );
    }

    #[test]
    fn get_or_emit_returns_existing_block_before_emitting() {
        let mut address_space = address_space_with_prelude();
        let base = address_space.code().code_base_ptr();
        let loc = LocationDescriptor::new(0x1000);
        let entry = unsafe { base.add(512) };

        address_space
            .record_emitted_block(loc, fake_block(entry, 64))
            .unwrap();

        assert_eq!(address_space.get_or_emit(loc).unwrap(), entry);
    }

    #[test]
    fn get_or_emit_reports_missing_arm64_emitter_on_cache_miss() {
        let mut address_space = address_space_with_prelude();
        let loc = LocationDescriptor::new(0x1000);

        let err = address_space.get_or_emit(loc).unwrap_err();

        assert!(err.contains("EmitArm64 is not ported"));
        assert!(err.contains(&loc.to_string()));
    }

    #[test]
    fn invalidation_removes_current_entry_but_keeps_reverse_history() {
        let mut address_space = address_space_with_prelude();
        let base = address_space.code().code_base_ptr();
        let loc = LocationDescriptor::new(0x1000);
        let entry = unsafe { base.add(512) };

        address_space
            .record_emitted_block(loc, fake_block(entry, 64))
            .unwrap();
        address_space.invalidate_basic_blocks(&HashSet::from([loc]));

        assert_eq!(address_space.get(loc), None);
        assert_eq!(
            address_space.reverse_get_location(unsafe { entry.add(4) }),
            Some(loc)
        );
    }

    #[test]
    fn clear_cache_preserves_prelude_and_drops_block_maps() {
        let mut address_space = address_space_with_prelude();
        let loc = LocationDescriptor::new(0x1000);
        let entry = unsafe { address_space.code().code_base_ptr().add(512) };
        address_space
            .record_emitted_block(loc, fake_block(entry, 64))
            .unwrap();

        let prelude_end = address_space.prelude_info().end_of_prelude;
        address_space.clear_cache().unwrap();

        assert_eq!(address_space.code().code_size(), prelude_end);
        assert_eq!(address_space.get(loc), None);
        assert_eq!(address_space.reverse_get_entry_point(entry), None);
    }

    #[test]
    fn link_target_count_matches_upstream_enum() {
        let targets = [
            LinkTarget::ReturnToDispatcher,
            LinkTarget::ReturnFromRunCode,
            LinkTarget::ReadMemory8,
            LinkTarget::ReadMemory16,
            LinkTarget::ReadMemory32,
            LinkTarget::ReadMemory64,
            LinkTarget::ReadMemory128,
            LinkTarget::WrappedReadMemory8,
            LinkTarget::WrappedReadMemory16,
            LinkTarget::WrappedReadMemory32,
            LinkTarget::WrappedReadMemory64,
            LinkTarget::WrappedReadMemory128,
            LinkTarget::ExclusiveReadMemory8,
            LinkTarget::ExclusiveReadMemory16,
            LinkTarget::ExclusiveReadMemory32,
            LinkTarget::ExclusiveReadMemory64,
            LinkTarget::ExclusiveReadMemory128,
            LinkTarget::WriteMemory8,
            LinkTarget::WriteMemory16,
            LinkTarget::WriteMemory32,
            LinkTarget::WriteMemory64,
            LinkTarget::WriteMemory128,
            LinkTarget::WrappedWriteMemory8,
            LinkTarget::WrappedWriteMemory16,
            LinkTarget::WrappedWriteMemory32,
            LinkTarget::WrappedWriteMemory64,
            LinkTarget::WrappedWriteMemory128,
            LinkTarget::ExclusiveWriteMemory8,
            LinkTarget::ExclusiveWriteMemory16,
            LinkTarget::ExclusiveWriteMemory32,
            LinkTarget::ExclusiveWriteMemory64,
            LinkTarget::ExclusiveWriteMemory128,
            LinkTarget::CallSVC,
            LinkTarget::ExceptionRaised,
            LinkTarget::InstructionSynchronizationBarrierRaised,
            LinkTarget::InstructionCacheOperationRaised,
            LinkTarget::DataCacheOperationRaised,
            LinkTarget::GetCNTPCT,
            LinkTarget::AddTicks,
            LinkTarget::GetTicksRemaining,
        ];
        assert_eq!(targets.len(), 40);
    }

    #[test]
    fn code_cache_can_emit_after_clear_cache() {
        let mut address_space = address_space_with_prelude();
        let prelude_end = address_space.prelude_info().end_of_prelude;
        address_space.clear_cache().unwrap();
        let offset = address_space.code.write_u32(inst::nop()).unwrap();
        assert_eq!(offset, prelude_end);
    }

    #[test]
    fn link_patches_return_from_run_code_relocation() {
        let mut address_space = address_space_with_prelude();
        address_space.clear_cache().unwrap();
        let entry_offset = address_space.code.write_u32(inst::nop()).unwrap();
        let entry = unsafe { address_space.code().code_base_ptr().add(entry_offset) };

        let mut block = fake_block(entry, 4);
        block.relocations.push(Relocation {
            code_offset: 0,
            target: LinkTarget::ReturnFromRunCode,
        });
        address_space
            .record_emitted_block(LocationDescriptor::new(0x1000), block)
            .unwrap();

        let source = entry as isize;
        let target = address_space.prelude_info().return_from_run_code as isize;
        assert_eq!(
            read_instruction(&address_space, entry_offset),
            inst::b_imm(target - source)
        );
    }

    #[test]
    fn link_patches_return_to_dispatcher_relocation() {
        let mut address_space = address_space_with_prelude();
        address_space.clear_cache().unwrap();
        let entry_offset = address_space.code.write_u32(inst::nop()).unwrap();
        let entry = unsafe { address_space.code().code_base_ptr().add(entry_offset) };

        let mut block = fake_block(entry, 4);
        block.relocations.push(Relocation {
            code_offset: 0,
            target: LinkTarget::ReturnToDispatcher,
        });
        address_space
            .record_emitted_block(LocationDescriptor::new(0x1000), block)
            .unwrap();

        let source = entry as isize;
        let target = address_space.prelude_info().return_to_dispatcher as isize;
        assert_eq!(
            read_instruction(&address_space, entry_offset),
            inst::b_imm(target - source)
        );
    }

    #[test]
    fn link_rejects_callback_relocation_when_trampoline_is_missing() {
        let mut address_space = address_space_with_prelude();
        address_space.clear_cache().unwrap();
        let entry_offset = address_space.code.write_u32(inst::nop()).unwrap();
        let entry = unsafe { address_space.code().code_base_ptr().add(entry_offset) };

        let mut block = fake_block(entry, 4);
        block.relocations.push(Relocation {
            code_offset: 0,
            target: LinkTarget::ReadMemory8,
        });

        let error = address_space
            .record_emitted_block(LocationDescriptor::new(0x1000), block)
            .unwrap_err();
        assert!(error.contains("ReadMemory8"));
        assert!(error.contains("prelude trampoline is not emitted yet"));
    }

    #[test]
    fn link_patches_callback_relocation_as_bl_when_trampoline_exists() {
        let mut address_space = address_space_with_prelude();
        address_space.clear_cache().unwrap();
        let entry_offset = address_space.code.write_u32(inst::nop()).unwrap();
        let entry = unsafe { address_space.code().code_base_ptr().add(entry_offset) };
        let callback = address_space.prelude_info().return_from_run_code;
        address_space.prelude_info_mut().read_memory_8 = Some(callback);

        let mut block = fake_block(entry, 4);
        block.relocations.push(Relocation {
            code_offset: 0,
            target: LinkTarget::ReadMemory8,
        });
        address_space
            .record_emitted_block(LocationDescriptor::new(0x1000), block)
            .unwrap();

        assert_eq!(
            read_instruction(&address_space, entry_offset),
            inst::bl_imm(callback as isize - entry as isize)
        );
    }

    #[test]
    fn branch_block_relocation_patches_when_target_is_available() {
        let mut address_space = address_space_with_prelude();
        address_space.clear_cache().unwrap();
        let target_offset = address_space.code.write_u32(inst::ret_lr()).unwrap();
        let source_offset = address_space.code.write_u32(inst::nop()).unwrap();
        let target_entry = unsafe { address_space.code().code_base_ptr().add(target_offset) };
        let source_entry = unsafe { address_space.code().code_base_ptr().add(source_offset) };
        let target_loc = LocationDescriptor::new(0x1000);
        let source_loc = LocationDescriptor::new(0x2000);

        address_space
            .record_emitted_block(target_loc, fake_block(target_entry, 4))
            .unwrap();

        let mut source_block = fake_block(source_entry, 4);
        source_block.block_relocations.insert(
            target_loc,
            vec![BlockRelocation {
                code_offset: 0,
                relocation_type: BlockRelocationType::Branch,
            }],
        );
        address_space
            .record_emitted_block(source_loc, source_block)
            .unwrap();

        assert_eq!(
            read_instruction(&address_space, source_offset),
            inst::b_imm(target_entry as isize - source_entry as isize)
        );
    }

    #[test]
    fn branch_block_relocation_is_nop_until_target_is_available_then_relinked() {
        let mut address_space = address_space_with_prelude();
        address_space.clear_cache().unwrap();
        let source_offset = address_space.code.write_u32(inst::nop()).unwrap();
        let target_offset = address_space.code.write_u32(inst::ret_lr()).unwrap();
        let source_entry = unsafe { address_space.code().code_base_ptr().add(source_offset) };
        let target_entry = unsafe { address_space.code().code_base_ptr().add(target_offset) };
        let source_loc = LocationDescriptor::new(0x2000);
        let target_loc = LocationDescriptor::new(0x1000);

        let mut source_block = fake_block(source_entry, 4);
        source_block.block_relocations.insert(
            target_loc,
            vec![BlockRelocation {
                code_offset: 0,
                relocation_type: BlockRelocationType::Branch,
            }],
        );
        address_space
            .record_emitted_block(source_loc, source_block)
            .unwrap();
        assert_eq!(read_instruction(&address_space, source_offset), inst::nop());

        address_space
            .record_emitted_block(target_loc, fake_block(target_entry, 4))
            .unwrap();
        assert_eq!(
            read_instruction(&address_space, source_offset),
            inst::b_imm(target_entry as isize - source_entry as isize)
        );
    }

    #[test]
    fn move_to_scratch1_loads_target_or_return_to_dispatcher() {
        let mut address_space = address_space_with_prelude();
        address_space.clear_cache().unwrap();
        let source_offset = address_space.code.write_u32(0).unwrap();
        address_space.code.write_u32(0).unwrap();
        let target_offset = address_space.code.write_u32(inst::ret_lr()).unwrap();
        let source_entry = unsafe { address_space.code().code_base_ptr().add(source_offset) };
        let target_entry = unsafe { address_space.code().code_base_ptr().add(target_offset) };
        let source_loc = LocationDescriptor::new(0x2000);
        let target_loc = LocationDescriptor::new(0x1000);

        let mut source_block = fake_block(source_entry, 8);
        source_block.block_relocations.insert(
            target_loc,
            vec![BlockRelocation {
                code_offset: 0,
                relocation_type: BlockRelocationType::MoveToScratch1,
            }],
        );
        address_space
            .record_emitted_block(source_loc, source_block)
            .unwrap();

        let fallback = adrl_instructions(
            source_entry,
            address_space.prelude_info().return_to_dispatcher,
            XSCRATCH1,
        )
        .unwrap();
        assert_eq!(read_instruction(&address_space, source_offset), fallback[0]);
        assert_eq!(
            read_instruction(&address_space, source_offset + 4),
            fallback[1]
        );

        address_space
            .record_emitted_block(target_loc, fake_block(target_entry, 4))
            .unwrap();
        let target = adrl_instructions(source_entry, target_entry, XSCRATCH1).unwrap();
        assert_eq!(read_instruction(&address_space, source_offset), target[0]);
        assert_eq!(
            read_instruction(&address_space, source_offset + 4),
            target[1]
        );
    }
}

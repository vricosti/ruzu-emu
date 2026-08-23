//! A32 block compilation pipeline: translate → optimize → emit → cache.
//!
//! Near-identical to `A64EmitX64` but uses the A32 frontend (ARM/Thumb decoder)
//! and A32LocationDescriptor for block keying. The shared IR, optimizer, and
//! code emitter are reused.

use std::collections::HashSet;

use rxbyak::{dword_ptr, qword_ptr};
use rxbyak::{JmpType, RegExp, EAX, EBP, EBX, ECX, R12, R15, RAX, RBP, RBX, RCX};

use crate::backend::x64::a64_emit_x64_memory::{gen_fastmem_fallbacks, FastmemFallbacksTable};
use crate::backend::x64::abi;
use crate::backend::x64::block_cache::{BlockCache, CachedBlock};
use crate::backend::x64::block_of_code::{
    BlockOfCode, DispatcherLabels, JitStateOffsets, RunCodeCallbacks, RunCodeFn,
};
use crate::backend::x64::emit::emit_block;
use crate::backend::x64::emit_context::{ArchConfig, DeferredEmitCtx, EmitConfig, EmitContext};
use crate::backend::x64::exception_handler::{
    DoNotFastmemMarker, ExceptionHandler, FastmemPatchInfo, FastmemPatchTable,
};
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::hostloc::{HostLoc, ANY_GPR, ANY_XMM, HOST_R13, HOST_R14};
use crate::backend::x64::jit_state::{A32JitState, RSB_PTR_MASK};
use crate::backend::x64::patch_info::{
    PatchTable, PatchType, A32_PATCH_JG_SIZE, A32_PATCH_JMP_SIZE, A32_PATCH_JZ_SIZE,
};
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::frontend::a32::translate::translate_callbacks::TranslateCallbacks;
use crate::frontend::a32::translate::{translate as a32_translate, TranslationOptions};
use crate::ir::location::{A32LocationDescriptor, LocationDescriptor};
use crate::ir::opcode::Opcode;
use crate::ir::opt;
use crate::ir::types::Type;
use crate::jit_config::OptimizationFlag;

/// Minimum space remaining in the code buffer before triggering a cache clear.
const MIN_SPACE_REMAINING: usize = 1024 * 1024; // 1 MB

/// Fast dispatch table entry (same layout as A64).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FastDispatchEntry {
    pub location_descriptor: u64,
    pub code_ptr: u64,
}

// Upstream: `backend/x64/a32_emit_x64.h`
const FAST_DISPATCH_TABLE_SIZE: usize = 0x10000;
const FAST_DISPATCH_TABLE_MASK: u32 = 0xFFFF0;

fn fast_dispatch_hash(location_descriptor: u64, table_ptr: u64, has_sse42: bool) -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        if has_sse42 {
            return unsafe {
                std::arch::x86_64::_mm_crc32_u64(location_descriptor, table_ptr) as u32
            };
        }
    }

    location_descriptor as u32
}

/// A32 block compilation pipeline.
///
/// Same infrastructure as A64EmitX64 but uses A32 frontend for translation.
pub struct A32EmitX64 {
    /// Owns the platform exception registration, as upstream `EmitX64` does.
    exception_handler: ExceptionHandler,
    pub code: BlockOfCode,
    pub cache: BlockCache,
    pub dispatcher_labels: DispatcherLabels,
    pub emit_config: EmitConfig,
    pub run_code_callbacks: RunCodeCallbacks,
    /// Fine-grained optimization flags (replaces separate booleans).
    pub optimizations: OptimizationFlag,
    pub translation_options: TranslationOptions,
    pub patch_table: PatchTable,
    pub terminal_handler_pop_rsb_hint: Option<usize>,
    pub terminal_handler_fast_dispatch_hint: Option<usize>,
    pub fast_dispatch_table: Option<Box<[FastDispatchEntry]>>,
    /// Pre-generated per-register memory callback stubs. Upstream owns the
    /// corresponding `read_fallbacks`, `write_fallbacks`, and
    /// `exclusive_write_fallbacks` tables on `A32EmitX64`.
    pub fastmem_fallbacks: FastmemFallbacksTable,
    /// Fastmem patch info: maps faulting RIP → fallback stub info.
    /// Used by the SIGSEGV handler to redirect fastmem faults to callbacks.
    ///
    /// Heap-boxed so the SIGSEGV-handler closure can capture a stable
    /// pointer that survives moves of `A32EmitX64`. Without the `Box`,
    /// the pointer captured in `new()` becomes stale the moment the
    /// returned emitter is moved into its consumer (the consumer's
    /// `A32Jit`), causing every fastmem fault that touches the table
    /// after a HashMap reallocation to miss its patch and abort with
    /// `[SIGSEGV] unhandled JIT fault (in code range, no patch)`.
    /// This mirrors `A64EmitX64::fastmem_patches`.
    pub fastmem_patches: Box<FastmemPatchTable>,
    /// Memory microinstructions that must use callbacks after their fastmem
    /// access faulted. Matches upstream `A32EmitX64::do_not_fastmem`.
    pub do_not_fastmem: HashSet<DoNotFastmemMarker>,
}

impl A32EmitX64 {
    fn should_log_compile_range(location: LocationDescriptor) -> bool {
        std::env::var_os("RUZU_A32_LOG_RANGE").is_some()
            && matches!(
                A32LocationDescriptor::from_location(location).pc(),
                0x015DE500..=0x015DE6FF
                    | 0x01603800..=0x016039FF
                    | 0x01D1DC00..=0x01D1DEFF
                    | 0x01D22C00..=0x01D22CFF
            )
    }

    pub fn new(
        emit_config: EmitConfig,
        run_callbacks: RunCodeCallbacks,
        optimizations: OptimizationFlag,
        translation_options: TranslationOptions,
        cache_size: usize,
    ) -> Result<Self, String> {
        let mut code = BlockOfCode::with_size_and_offsets(
            cache_size,
            JitStateOffsets {
                halt_reason: A32JitState::offset_of_halt_reason(),
                guest_mxcsr: A32JitState::offset_of_guest_mxcsr(),
                asimd_mxcsr: A32JitState::offset_of_asimd_mxcsr(),
            },
        )
        .map_err(|e| format!("Failed to allocate code buffer: {:?}", e))?;

        let dispatcher_labels = code
            .gen_run_code(&run_callbacks)
            .map_err(|e| format!("Failed to generate dispatcher: {:?}", e))?;

        let mut exception_handler = ExceptionHandler::new();
        exception_handler.register(code.code_base_ptr(), code.total_size());

        let mut emitter = Self {
            exception_handler,
            code,
            cache: BlockCache::new(),
            dispatcher_labels,
            emit_config,
            run_code_callbacks: run_callbacks,
            optimizations,
            translation_options,
            patch_table: PatchTable::new(),
            terminal_handler_pop_rsb_hint: None,
            terminal_handler_fast_dispatch_hint: None,
            fast_dispatch_table: None,
            fastmem_fallbacks: FastmemFallbacksTable::new(),
            fastmem_patches: Box::new(FastmemPatchTable::new()),
            do_not_fastmem: HashSet::new(),
        };

        // Match upstream constructor ordering: fallback tables precede the
        // terminal handlers in the shared code buffer.
        emitter.fastmem_fallbacks = gen_fastmem_fallbacks(
            &mut emitter.code.asm,
            &emitter.emit_config.callbacks,
            emitter.emit_config.raw_exclusive_write_callbacks.as_ref(),
        );
        emitter.gen_terminal_handlers()?;

        // Publish the callback whenever fastmem was configured. Eden does this
        // independently of whether platform-handler installation succeeded.
        if emitter.run_code_callbacks.fastmem_pointer.is_some() {
            // The callback closure captures the patches table address as usize
            // to satisfy Send requirements. Safety: the emitter outlives the
            // signal handler registration.
            // SAFETY: Box guarantees the inner table address is stable
            // across moves of `emitter`. We must take the address of
            // the heap-allocated table itself (`&*emitter.fastmem_patches`),
            // not the address of the Box on the stack.
            let patches_addr = &*emitter.fastmem_patches as *const FastmemPatchTable as usize;
            emitter
                .exception_handler
                .set_fastmem_callback(Box::new(move |rip| {
                    let patches = unsafe { &*(patches_addr as *const FastmemPatchTable) };
                    patches.lookup_and_record_recompile(rip)
                }));
        }

        Ok(emitter)
    }

    pub unsafe fn get_run_code_fn(&mut self) -> Result<RunCodeFn, String> {
        self.code
            .disable_writing()
            .map_err(|e| format!("Failed to set RX protection: {:?}", e))?;
        let base = self.code.code_base_ptr();
        let fn_ptr = base.add(self.dispatcher_labels.run_code_offset);
        Ok(unsafe { std::mem::transmute::<*const u8, RunCodeFn>(fn_ptr) })
    }

    pub unsafe fn get_step_code_fn(&mut self) -> Result<RunCodeFn, String> {
        self.code
            .disable_writing()
            .map_err(|e| format!("Failed to set RX protection: {:?}", e))?;
        let base = self.code.code_base_ptr();
        let fn_ptr = base.add(self.dispatcher_labels.step_code_offset);
        Ok(unsafe { std::mem::transmute::<*const u8, RunCodeFn>(fn_ptr) })
    }

    pub fn make_writable(&mut self) -> Result<(), String> {
        self.code
            .enable_writing()
            .map_err(|e| format!("Failed to set RW protection: {:?}", e))?;
        Ok(())
    }

    /// Fast cache-only lookup. Returns the entrypoint if the block is already compiled.
    /// Does NOT require mprotect — safe to call while code is in RX mode.
    pub fn lookup_cached_block(&self, location: LocationDescriptor) -> Option<*const u8> {
        self.cache.get(&location).map(|cached| cached.entrypoint)
    }

    /// Number of compiled blocks in cache.
    pub fn cache_size(&self) -> usize {
        self.cache.len()
    }

    /// Get or compile a block for the given location.
    ///
    /// Uses the A32 frontend (ARM/Thumb decoder) instead of A64.
    /// Caller MUST call make_writable() before and get_run_code_fn() after
    /// if this may compile a new block.
    pub fn get_or_compile_block(
        &mut self,
        location: LocationDescriptor,
        callbacks: &dyn TranslateCallbacks,
    ) -> *const u8 {
        self.get_or_compile_block_with_ro(location, callbacks, &|_| false)
    }

    /// Get or compile a block with an is_read_only_memory callback for
    /// the A32ConstantMemoryReads optimization pass.
    pub fn get_or_compile_block_with_ro(
        &mut self,
        location: LocationDescriptor,
        callbacks: &dyn TranslateCallbacks,
        is_read_only: &dyn Fn(u32) -> bool,
    ) -> *const u8 {
        // Check cache first
        if let Some(cached) = self.cache.get(&location) {
            return cached.entrypoint;
        }

        // Check space remaining
        if self.code.space_remaining() < MIN_SPACE_REMAINING {
            self.clear_cache();
        }

        // Translate: ARM32/Thumb → IR (A32 frontend)
        let a32_loc = A32LocationDescriptor::from_location(location);
        let pc = a32_loc.pc();
        let mut block = a32_translate(a32_loc, callbacks, self.translation_options);
        let read_code = |vaddr| callbacks.memory_read_code(vaddr);

        if Self::should_log_compile_range(location) {
            let ops = block
                .instructions
                .iter()
                .enumerate()
                .map(|(i, inst)| {
                    let pseudo = inst
                        .next_pseudoop
                        .map(|r| format!(" ->pseudo#{}", r.0))
                        .unwrap_or_default();
                    format!("#{i}:{:?}{} -> {:?}", inst.opcode, pseudo, inst.args)
                })
                .collect::<Vec<_>>()
                .join("\n");
            log::error!(
                "A32EmitX64::compile_block pc=0x{pc:08X} term={:?} cycles={} instructions:\n{}",
                block.terminal,
                block.cycle_count,
                ops
            );
        }

        // Optimize (per-flag, matching upstream dynarmic A32 pipeline order)
        opt::polyfill(
            &mut block,
            opt::PolyfillOptions {
                sha256: !self
                    .code
                    .has_host_feature(crate::backend::x64::host_feature::HostFeature::SHA),
                vector_multiply_widen: true,
            },
        );
        if self
            .optimizations
            .contains(OptimizationFlag::GET_SET_ELIMINATION)
        {
            opt::a32_get_set_elimination(&mut block);
            opt::dead_code_elimination(&mut block);
        }
        if self.optimizations.contains(OptimizationFlag::CONST_PROP) {
            // Upstream: A32ConstantMemoryReads runs BEFORE ConstantPropagation
            // so that folded memory values can propagate further.
            opt::a32_constant_memory_reads(&mut block, &read_code, is_read_only);
            opt::constant_propagation(&mut block);
            opt::dead_code_elimination(&mut block);
        }

        // Upstream: IdentityRemovalPass (always) + VerificationPass (debug only)
        opt::identity_removal(&mut block);
        block.rebuild_pseudo_op_links();
        #[cfg(debug_assertions)]
        opt::verification_pass(&mut block);
        // Build inst_info for register allocator
        let inst_info: Vec<(u32, usize)> = block
            .instructions
            .iter()
            .map(|inst| (inst.use_count, type_bit_width(inst.return_type())))
            .collect();

        // Emit
        let (desc, patch_entries, fastmem_entries) = {
            let host_features = self.code.host_features();
            let mut ctx = EmitContext::with_dispatcher(
                location,
                &self.emit_config,
                ArchConfig::A32,
                host_features,
                self.optimizations,
                self.dispatcher_labels.return_from_run_code,
                self.code.code_base_ptr(),
            );
            ctx.enable_block_linking = self.optimizations.contains(OptimizationFlag::BLOCK_LINKING);
            ctx.enable_rsb = self
                .optimizations
                .contains(OptimizationFlag::RETURN_STACK_BUFFER);
            ctx.enable_fast_dispatch = self.optimizations.contains(OptimizationFlag::FAST_DISPATCH);
            ctx.terminal_handler_pop_rsb_hint = self.terminal_handler_pop_rsb_hint;
            ctx.terminal_handler_fast_dispatch_hint = self.terminal_handler_fast_dispatch_hint;

            if self.optimizations.contains(OptimizationFlag::BLOCK_LINKING) {
                let cache_ptr = &self.cache as *const BlockCache;
                ctx.block_lookup = Some(Box::new(move |loc| {
                    let cache = unsafe { &*cache_ptr };
                    cache.get(&loc).map(|b| b.entrypoint)
                }));
            }

            // Compute block metadata for UpdateUpperLocationDescriptor.
            // Matches upstream: scan for BXWritePC and use block.EndLocation().
            ctx.has_bx_write_pc = block
                .instructions
                .iter()
                .any(|inst| inst.opcode == Opcode::A32BXWritePC);
            ctx.end_location = Some(block.end_location());
            ctx.fastmem_available = self.run_code_callbacks.fastmem_pointer.is_some()
                && self.exception_handler.supports_fastmem();
            ctx.do_not_fastmem = Some(&self.do_not_fastmem);
            ctx.fastmem_fallbacks =
                Some(&self.fastmem_fallbacks as *const FastmemFallbacksTable as *const ());

            ctx.block = Some(&block);

            let gpr_order = {
                let mut gprs = ANY_GPR.to_vec();
                if ctx.fastmem_available {
                    gprs.retain(|&loc| loc != HOST_R13);
                }
                if self.run_code_callbacks.page_table_pointer.is_some() {
                    gprs.retain(|&loc| loc != HOST_R14);
                }
                gprs
            };

            let code_base = self.code.code_base_ptr() as u64;
            let mut ra = RegAlloc::new(&mut self.code.asm, gpr_order, ANY_XMM.to_vec(), inst_info);
            ra.constant_pool = Some(&mut self.code.constant_pool);
            let desc = emit_block(&ctx, &mut ra, &block);

            let drained: Vec<_> = ctx.deferred_emits.borrow_mut().drain(..).collect();
            {
                let mut dctx = DeferredEmitCtx {
                    asm: &mut *ra.asm,
                    fastmem_patches: &mut self.fastmem_patches,
                    code_base,
                };
                for closure in drained {
                    closure(&mut dctx);
                }
            }

            let patch_entries = ctx.take_patch_entries();
            let fastmem_entries = ctx.fastmem_entries.borrow().clone();
            (desc, patch_entries, fastmem_entries)
        };

        let entrypoint = unsafe { self.code.code_base_ptr().add(desc.entrypoint_offset) };
        let end = unsafe { entrypoint.add(desc.size) };
        crate::backend::x64::perf_map::register(
            entrypoint,
            end,
            &format!(
                "a32_{}{:08X}_{}_fpcr{:08X}",
                if a32_loc.t_flag() { "t" } else { "a" },
                a32_loc.pc(),
                if a32_loc.e_flag() { "be" } else { "le" },
                a32_loc.fpscr().value()
            ),
        );
        let code_base = self.code.code_base_ptr() as u64;

        // Generate inline fallback stubs for each fastmem instruction.
        // Each stub: save caller-saves, call callback, restore, ret.
        // The SIGSEGV handler FakeCall(callback, resume) redirects to the stub.
        for entry in &fastmem_entries {
            let inst_rip = code_base + entry.inst_offset as u64;
            let resume_rip = code_base + entry.resume_offset as u64;

            // Emit the fallback stub at the current code position
            let stub_offset = self.code.asm.size();
            self.emit_fastmem_fallback_stub(entry);
            let stub_rip = code_base + stub_offset as u64;

            self.fastmem_patches.add(
                inst_rip,
                FastmemPatchInfo::new(resume_rip, stub_rip, Some(entry.marker), entry.recompile),
            );
        }

        for entry in &patch_entries {
            let info = self.patch_table.entry(entry.target).or_default();
            match entry.patch_type {
                PatchType::Jg => info.jg.push(entry.code_offset),
                PatchType::Jz => info.jz.push(entry.code_offset),
                PatchType::Jmp => info.jmp.push(entry.code_offset),
                PatchType::MovRcx => info.mov_rcx.push(entry.code_offset),
            }
        }

        self.cache.insert(
            location,
            CachedBlock {
                entrypoint,
                entrypoint_offset: desc.entrypoint_offset,
                size: desc.size,
            },
        );

        self.patch(location, Some(entrypoint));

        entrypoint
    }

    fn patch(&mut self, target_loc: LocationDescriptor, code_ptr: Option<*const u8>) {
        let info = match self.patch_table.get(&target_loc) {
            Some(info) => info.clone(),
            None => {
                // RDYNARMIC_PROFILE_PATCH=1: count compiled blocks whose
                // location has NO pending patches — i.e., no earlier-compiled
                // block was waiting to chain into this one. High count means
                // block-linking patches don't kick in for most new blocks.
                if std::env::var_os("RDYNARMIC_PROFILE_PATCH").is_some() {
                    use std::sync::atomic::{AtomicU64, Ordering};
                    static NO_PATCH: AtomicU64 = AtomicU64::new(0);
                    let n = NO_PATCH.fetch_add(1, Ordering::Relaxed) + 1;
                    if n % 10000 == 0 {
                        eprintln!("[PATCH_PROFILE] no_pending_patches={}", n);
                    }
                }
                return;
            }
        };
        let total_patches = info.jg.len() + info.jz.len() + info.jmp.len() + info.mov_rcx.len();
        if std::env::var_os("RDYNARMIC_PROFILE_PATCH").is_some() {
            use std::sync::atomic::{AtomicU64, Ordering};
            static WITH_PATCH: AtomicU64 = AtomicU64::new(0);
            static TOTAL_PATCHED: AtomicU64 = AtomicU64::new(0);
            let n = WITH_PATCH.fetch_add(1, Ordering::Relaxed) + 1;
            let t = TOTAL_PATCHED.fetch_add(total_patches as u64, Ordering::Relaxed)
                + total_patches as u64;
            if n % 1000 == 0 {
                eprintln!(
                    "[PATCH_PROFILE] inserts_with_pending_patches={} total_patches_applied={}",
                    n, t
                );
            }
        }

        let code_base = self.code.code_base_ptr();
        let offsets = self.dispatcher_labels.return_from_run_code;
        let fallback = code_base as usize + offsets[0];

        for &offset in &info.jg {
            let saved_size = self.code.asm.size();
            self.code.asm.set_size(offset);
            self.emit_patch_jg_at(target_loc, code_ptr, code_base, offsets);
            self.code.asm.set_size(saved_size);
        }

        for &offset in &info.jz {
            let saved_size = self.code.asm.size();
            self.code.asm.set_size(offset);
            self.emit_patch_jz_at(target_loc, code_ptr, code_base, offsets);
            self.code.asm.set_size(saved_size);
        }

        for &offset in &info.jmp {
            let saved_size = self.code.asm.size();
            self.code.asm.set_size(offset);
            self.emit_patch_jmp_at(target_loc, code_ptr, code_base, offsets);
            self.code.asm.set_size(saved_size);
        }

        for &offset in &info.mov_rcx {
            let saved_size = self.code.asm.size();
            self.code.asm.set_size(offset);
            self.code.asm.db(0x48).unwrap();
            self.code.asm.db(0xB9).unwrap();
            self.code
                .asm
                .dq(code_ptr.map_or(fallback as u64, |ptr| ptr as u64))
                .unwrap();
            self.code.asm.set_size(saved_size);
        }
    }

    fn unpatch(&mut self, target_loc: LocationDescriptor) {
        self.patch(target_loc, None);
    }

    fn emit_store_pc(&mut self, target_loc: LocationDescriptor) {
        let pc = A32LocationDescriptor::from_location(target_loc).pc();
        self.code
            .asm
            .mov(
                dword_ptr(RegExp::from(R15) + A32JitState::reg_offset(15) as i32),
                pc as i32,
            )
            .unwrap();
    }

    fn emit_jcc_to_dispatch(&mut self, opcode: u8, code_base: *const u8, offsets: [usize; 4]) {
        let jcc_end = self.code.asm.size() + 6;
        let target_addr = code_base as usize + offsets[0];
        let jcc_end_addr = code_base as usize + jcc_end;
        let disp = (target_addr as i64) - (jcc_end_addr as i64);
        self.code.asm.db(0x0F).unwrap();
        self.code.asm.db(opcode).unwrap();
        self.code.asm.dd(disp as u32).unwrap();
    }

    fn emit_patch_jg_at(
        &mut self,
        target_loc: LocationDescriptor,
        code_ptr: Option<*const u8>,
        code_base: *const u8,
        offsets: [usize; 4],
    ) {
        let begin = self.code.asm.size();
        if let Some(ptr) = code_ptr {
            let target = ptr as usize;
            let jg_end = begin + 6;
            let jg_end_addr = code_base as usize + jg_end;
            let disp = (target as i64) - (jg_end_addr as i64);
            self.code.asm.db(0x0F).unwrap();
            self.code.asm.db(0x8F).unwrap();
            self.code.asm.dd(disp as u32).unwrap();
        } else {
            self.emit_store_pc(target_loc);
            self.emit_jcc_to_dispatch(0x8F, code_base, offsets);
        }
        let used = self.code.asm.size() - begin;
        for _ in used..A32_PATCH_JG_SIZE {
            self.code.asm.nop().unwrap();
        }
    }

    fn emit_patch_jz_at(
        &mut self,
        target_loc: LocationDescriptor,
        code_ptr: Option<*const u8>,
        code_base: *const u8,
        offsets: [usize; 4],
    ) {
        let begin = self.code.asm.size();
        if let Some(ptr) = code_ptr {
            let target = ptr as usize;
            let jz_end = begin + 6;
            let jz_end_addr = code_base as usize + jz_end;
            let disp = (target as i64) - (jz_end_addr as i64);
            self.code.asm.db(0x0F).unwrap();
            self.code.asm.db(0x84).unwrap();
            self.code.asm.dd(disp as u32).unwrap();
        } else {
            self.emit_store_pc(target_loc);
            self.emit_jcc_to_dispatch(0x84, code_base, offsets);
        }
        let used = self.code.asm.size() - begin;
        for _ in used..A32_PATCH_JZ_SIZE {
            self.code.asm.nop().unwrap();
        }
    }

    fn emit_patch_jmp_at(
        &mut self,
        target_loc: LocationDescriptor,
        code_ptr: Option<*const u8>,
        code_base: *const u8,
        offsets: [usize; 4],
    ) {
        let begin = self.code.asm.size();
        if let Some(ptr) = code_ptr {
            let target = ptr as usize;
            let jmp_end = begin + 5;
            let jmp_end_addr = code_base as usize + jmp_end;
            let disp = (target as i64) - (jmp_end_addr as i64);
            self.code.asm.db(0xE9).unwrap();
            self.code.asm.dd(disp as u32).unwrap();
        } else {
            self.emit_store_pc(target_loc);
            let jmp_end = self.code.asm.size() + 5;
            let target_addr = code_base as usize + offsets[0];
            let jmp_end_addr = code_base as usize + jmp_end;
            let disp = (target_addr as i64) - (jmp_end_addr as i64);
            self.code.asm.db(0xE9).unwrap();
            self.code.asm.dd(disp as u32).unwrap();
        }
        let used = self.code.asm.size() - begin;
        for _ in used..A32_PATCH_JMP_SIZE {
            self.code.asm.nop().unwrap();
        }
    }

    /// Generate RSB and fast dispatch terminal handlers for A32.
    ///
    /// Traceability:
    /// - upstream file: `backend/x64/a32_emit_x64.cpp`
    /// - upstream method: `A32EmitX64::GenTerminalHandlers`
    fn gen_terminal_handlers(&mut self) -> Result<(), String> {
        let code_base = self.code.code_base_ptr();
        let rfrc = self.dispatcher_labels.return_from_run_code;
        let has_sse42 = self.code.has_host_feature(HostFeature::SSE42);
        let asm = &mut self.code.asm;

        let pc_offset = A32JitState::reg_offset(15); // R15 = PC
        let upper_offset = A32JitState::offset_of_upper_location_descriptor();
        let rsb_ptr_offset = A32JitState::offset_of_rsb_ptr();
        let rsb_loc_offset = A32JitState::offset_of_rsb_location_descriptors();
        let rsb_code_offset = A32JitState::offset_of_rsb_codeptrs();
        let location_desc_offset =
            std::mem::offset_of!(FastDispatchEntry, location_descriptor) as i32;
        let code_ptr_offset = std::mem::offset_of!(FastDispatchEntry, code_ptr) as i32;

        let calculate_location_descriptor =
            |asm: &mut rxbyak::CodeAssembler| -> Result<(), String> {
                // Upstream:
                //   mov ebx, [upper_location_descriptor]
                //   shl rbx, 32
                //   mov ecx, [pc]
                //   mov ebp, ecx
                //   or  rbx, rcx
                asm.mov(EBX, dword_ptr(RegExp::from(R15) + upper_offset as i32))
                    .map_err(|e| format!("terminal handler: {:?}", e))?;
                asm.shl(RBX, 32u8)
                    .map_err(|e| format!("terminal handler: {:?}", e))?;
                asm.mov(ECX, dword_ptr(RegExp::from(R15) + pc_offset as i32))
                    .map_err(|e| format!("terminal handler: {:?}", e))?;
                asm.mov(EBP, ECX)
                    .map_err(|e| format!("terminal handler: {:?}", e))?;
                asm.or_(RBX, RCX)
                    .map_err(|e| format!("terminal handler: {:?}", e))?;
                Ok(())
            };

        let rsb_cache_miss = asm.create_label();
        let fast_dispatch_cache_miss = asm.create_label();

        // ---- PopRSBHint handler ----
        let pop_rsb_offset = asm.size();
        calculate_location_descriptor(asm)?;

        asm.mov(EAX, dword_ptr(RegExp::from(R15) + rsb_ptr_offset as i32))
            .map_err(|e| format!("RSB handler: {:?}", e))?;
        asm.sub(EAX, 1i32)
            .map_err(|e| format!("RSB handler: {:?}", e))?;
        asm.and_(EAX, RSB_PTR_MASK as i32)
            .map_err(|e| format!("RSB handler: {:?}", e))?;
        asm.mov(dword_ptr(RegExp::from(R15) + rsb_ptr_offset as i32), EAX)
            .map_err(|e| format!("RSB handler: {:?}", e))?;
        asm.cmp(
            RBX,
            qword_ptr(RegExp::from(R15) + RAX * 8u8 + rsb_loc_offset as i32),
        )
        .map_err(|e| format!("RSB handler: {:?}", e))?;
        if self.optimizations.contains(OptimizationFlag::FAST_DISPATCH) {
            asm.jnz(&rsb_cache_miss, JmpType::Near)
                .map_err(|e| format!("RSB handler: {:?}", e))?;
        } else {
            let jmp_end = asm.size() + 6;
            let target_addr = code_base as usize + rfrc[0];
            let jmp_end_addr = code_base as usize + jmp_end;
            let disp = (target_addr as i64) - (jmp_end_addr as i64);
            asm.db(0x0F).map_err(|e| format!("RSB handler: {:?}", e))?;
            asm.db(0x85).map_err(|e| format!("RSB handler: {:?}", e))?;
            asm.dd(disp as u32)
                .map_err(|e| format!("RSB handler: {:?}", e))?;
        }

        // Hit
        asm.mov(
            RAX,
            qword_ptr(RegExp::from(R15) + RAX * 8u8 + rsb_code_offset as i32),
        )
        .map_err(|e| format!("RSB handler: {:?}", e))?;
        asm.jmp_reg(RAX)
            .map_err(|e| format!("RSB handler: {:?}", e))?;
        self.terminal_handler_pop_rsb_hint = Some(pop_rsb_offset);

        if !self.optimizations.contains(OptimizationFlag::FAST_DISPATCH) {
            self.code.code_begin_offset = self.code.asm.size();
            return Ok(());
        }

        // ---- FastDispatchHint handler ----
        let table = vec![
            FastDispatchEntry {
                location_descriptor: 0xFFFF_FFFF_FFFF_FFFF,
                code_ptr: 0,
            };
            FAST_DISPATCH_TABLE_SIZE
        ];
        let table_ptr = table.as_ptr() as u64;
        self.fast_dispatch_table = Some(table.into_boxed_slice());

        let fast_dispatch_offset = asm.size();
        calculate_location_descriptor(asm)?;
        asm.bind(&rsb_cache_miss)
            .map_err(|e| format!("FD handler: {:?}", e))?;
        asm.mov(R12, table_ptr as i64)
            .map_err(|e| format!("FD handler: {:?}", e))?;
        asm.mov(RBP, RBX)
            .map_err(|e| format!("FD handler: {:?}", e))?;
        #[cfg(target_arch = "x86_64")]
        {
            if has_sse42 {
                asm.crc32(RBP, R12)
                    .map_err(|e| format!("FD handler: {:?}", e))?;
            }
        }
        asm.and_(EBP, FAST_DISPATCH_TABLE_MASK as i32)
            .map_err(|e| format!("FD handler: {:?}", e))?;
        asm.lea(RBP, qword_ptr(RegExp::from(R12) + RBP))
            .map_err(|e| format!("FD handler: {:?}", e))?;
        asm.cmp(qword_ptr(RegExp::from(RBP) + location_desc_offset), RBX)
            .map_err(|e| format!("FD handler: {:?}", e))?;
        asm.jnz(&fast_dispatch_cache_miss, JmpType::Near)
            .map_err(|e| format!("FD handler: {:?}", e))?;
        asm.jmp_reg(qword_ptr(RegExp::from(RBP) + code_ptr_offset))
            .map_err(|e| format!("FD handler: {:?}", e))?;
        asm.bind(&fast_dispatch_cache_miss)
            .map_err(|e| format!("FD handler: {:?}", e))?;
        asm.mov(qword_ptr(RegExp::from(RBP) + location_desc_offset), RBX)
            .map_err(|e| format!("FD handler: {:?}", e))?;
        self.run_code_callbacks
            .lookup_block
            .emit_call_simple(asm)
            .map_err(|e| format!("FD handler: {:?}", e))?;
        asm.mov(qword_ptr(RegExp::from(RBP) + code_ptr_offset), RAX)
            .map_err(|e| format!("FD handler: {:?}", e))?;
        asm.jmp_reg(RAX)
            .map_err(|e| format!("FD handler: {:?}", e))?;

        self.terminal_handler_fast_dispatch_hint = Some(fast_dispatch_offset);
        self.code.code_begin_offset = self.code.asm.size();

        Ok(())
    }

    /// Emit an inline fallback stub for a fastmem instruction.
    ///
    /// The stub is called via FakeCall when the fastmem mov faults.
    /// It saves caller-save registers, calls the memory callback,
    /// moves the result to the correct register, restores, and rets.
    ///
    /// Matches upstream GenFastmemFallbacks per-register stub pattern.
    fn emit_fastmem_fallback_stub(
        &mut self,
        entry: &crate::backend::x64::emit_context::FastmemEntry,
    ) {
        use rxbyak::RAX;

        if entry.is_write && entry.is_exclusive {
            let callbacks = self
                .emit_config
                .raw_exclusive_write_callbacks
                .as_ref()
                .expect("exclusive fastmem requires raw write callbacks");
            crate::backend::x64::a64_emit_x64_memory::emit_exclusive_write_fallback(
                &mut self.code.asm,
                callbacks,
                entry.bitsize,
                entry.vaddr_reg,
                entry.value_reg,
            );
            return;
        }

        let vaddr_reg = rxbyak::Reg::gpr64(entry.vaddr_reg);
        let value_reg = rxbyak::Reg::gpr64(entry.value_reg);
        let vaddr_param = abi::ABI_PARAMS[1].to_reg64();
        let value_param = abi::ABI_PARAMS[2].to_reg64();
        let vaddr_param_idx = vaddr_param.get_idx();
        let value_param_idx = value_param.get_idx();

        if entry.is_write {
            let frame =
                abi::push_caller_save_registers_and_adjust_stack(&mut self.code.asm).unwrap();

            // Write: callback(context, vaddr, value). ArgCallback owns ABI_PARAM1.
            if entry.vaddr_reg == value_param_idx && entry.value_reg == vaddr_param_idx {
                self.code.asm.xchg(vaddr_param, value_param).unwrap();
            } else if entry.vaddr_reg == value_param_idx {
                self.code.asm.mov(vaddr_param, vaddr_reg).unwrap();
                if entry.value_reg != value_param_idx {
                    self.code.asm.mov(value_param, value_reg).unwrap();
                }
            } else {
                if entry.value_reg != value_param_idx {
                    self.code.asm.mov(value_param, value_reg).unwrap();
                }
                if entry.vaddr_reg != vaddr_param_idx {
                    self.code.asm.mov(vaddr_param, vaddr_reg).unwrap();
                }
            }
            self.code
                .emit_zero_extend_from(entry.bitsize, value_param)
                .unwrap();
            let callback = match entry.bitsize {
                8 => &self.emit_config.callbacks.memory_write_8,
                16 => &self.emit_config.callbacks.memory_write_16,
                32 => &self.emit_config.callbacks.memory_write_32,
                64 => &self.emit_config.callbacks.memory_write_64,
                _ => unreachable!(),
            };
            callback.emit_call_simple(&mut self.code.asm).unwrap();
            // Ordered store: drain the store buffer AFTER the callback,
            // matching upstream `GenFastmemFallbacks` write path in
            // `a32_emit_x64_memory.cpp:94-96`.
            if entry.ordered {
                self.code.asm.mfence().unwrap();
            }
            abi::pop_caller_save_registers_and_adjust_stack(&mut self.code.asm, &frame).unwrap();
        } else {
            let frame = abi::push_caller_save_registers_and_adjust_stack_except(
                &mut self.code.asm,
                Some(HostLoc::Gpr(entry.value_reg)),
            )
            .unwrap();

            // Read: callback(context, vaddr). ArgCallback owns ABI_PARAM1.
            if entry.vaddr_reg != vaddr_param_idx {
                self.code.asm.mov(vaddr_param, vaddr_reg).unwrap();
            }
            // Ordered load: drain pending stores BEFORE the callback,
            // matching upstream `GenFastmemFallbacks` read path in
            // `a32_emit_x64_memory.cpp:60-62`.
            if entry.ordered {
                self.code.asm.mfence().unwrap();
            }
            let callback = match entry.bitsize {
                8 => &self.emit_config.callbacks.memory_read_8,
                16 => &self.emit_config.callbacks.memory_read_16,
                32 => &self.emit_config.callbacks.memory_read_32,
                64 => &self.emit_config.callbacks.memory_read_64,
                _ => unreachable!(),
            };
            callback.emit_call_simple(&mut self.code.asm).unwrap();
            // Move result from RAX to value_reg
            if value_reg.get_idx() != RAX.get_idx() {
                self.code.asm.mov(value_reg, RAX).unwrap();
            }
            abi::pop_caller_save_registers_and_adjust_stack(&mut self.code.asm, &frame).unwrap();
            self.code
                .emit_zero_extend_from(entry.bitsize, value_reg)
                .unwrap();
        }

        // Return to resume_rip (pushed by the SIGSEGV handler's FakeCall)
        self.code.asm.ret().unwrap();
    }

    pub fn clear_fast_dispatch_table(&mut self) {
        if let Some(ref mut table) = self.fast_dispatch_table {
            for entry in table.iter_mut() {
                entry.location_descriptor = 0xFFFF_FFFF_FFFF_FFFF;
                entry.code_ptr = 0;
            }
        }
    }

    fn invalidate_fast_dispatch_entry(&mut self, location: LocationDescriptor) {
        if let Some(ref mut table) = self.fast_dispatch_table {
            let desc = location.value();
            let table_ptr = table.as_ptr() as u64;
            let hash = fast_dispatch_hash(
                desc,
                table_ptr,
                self.code.has_host_feature(HostFeature::SSE42),
            ) & FAST_DISPATCH_TABLE_MASK;
            let index = (hash >> 4) as usize;
            if index < table.len() && table[index].location_descriptor == desc {
                table[index].location_descriptor = 0xFFFF_FFFF_FFFF_FFFF;
                table[index].code_ptr = 0;
            }
        }
    }

    fn invalidate_basic_block(&mut self, location: LocationDescriptor) {
        if !self.cache.contains(&location) {
            return;
        }
        self.unpatch(location);
        self.invalidate_fast_dispatch_entry(location);
        self.cache.remove(&location);
    }

    /// Apply fault-triggered recompiles after generated code has returned.
    ///
    /// Upstream mutates `do_not_fastmem` and invalidates the block directly
    /// from its platform exception callback. Rust defers those non-signal-safe
    /// container mutations until the same JIT execution slice returns.
    pub fn process_pending_fastmem_recompiles(&mut self) -> Result<usize, String> {
        let markers = self.fastmem_patches.take_pending_recompiles();
        if markers.is_empty() {
            return Ok(0);
        }

        let marker_count = markers.len();
        self.make_writable()?;
        let mut locations = HashSet::new();
        for marker in markers {
            static RECOMPILE_SEQUENCE: std::sync::atomic::AtomicU32 =
                std::sync::atomic::AtomicU32::new(0);
            let hit = RECOMPILE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let recompile_limit = std::env::var("RUZU_FASTMEM_RECOMPILE_LIMIT")
                .ok()
                .and_then(|value| value.parse::<u32>().ok());
            if recompile_limit.is_some_and(|limit| hit >= limit) {
                continue;
            }
            locations.insert(marker.0);
            self.do_not_fastmem.insert(marker);
        }
        for location in locations {
            self.invalidate_basic_block(location);
        }
        unsafe {
            self.get_run_code_fn()?;
        }
        Ok(marker_count)
    }

    pub fn clear_cache(&mut self) {
        self.patch_table.clear();
        self.clear_fast_dispatch_table();
        self.fastmem_patches.clear();
        self.cache.clear();
        crate::backend::x64::perf_map::clear();
        self.code.clear_cache();
    }

    pub fn invalidate_range(&mut self, start: u64, length: u64) {
        let end = start.wrapping_add(length);

        let to_remove: Vec<LocationDescriptor> = self
            .cache
            .keys()
            .filter(|loc| {
                // A32 PC is in the lower 32 bits of the location descriptor
                let pc = loc.value() & 0xFFFF_FFFF;
                pc >= start && pc < end
            })
            .copied()
            .collect();

        for &loc in &to_remove {
            self.unpatch(loc);
            self.patch_table.remove(&loc);
            self.invalidate_fast_dispatch_entry(loc);
        }

        let had_blocks = !self.cache.is_empty();
        self.cache.invalidate_range(start, length);
        if had_blocks && self.cache.is_empty() {
            self.patch_table.clear();
            self.code.clear_cache();
        }
    }
}

/// Map IR Type to bit width for register allocation (same as A64 version).
fn type_bit_width(ty: Type) -> usize {
    match ty {
        Type::Void => 0,
        Type::U1 => 8,
        Type::U8 => 8,
        Type::U16 => 16,
        Type::U32 => 32,
        Type::U64 => 64,
        Type::U128 => 128,
        Type::NZCVFlags => 32,
        Type::Cond => 32,
        Type::A64Reg => 64,
        Type::A64Vec => 64,
        Type::A32Reg => 32,
        Type::A32ExtReg => 32,
        _ => 64,
    }
}

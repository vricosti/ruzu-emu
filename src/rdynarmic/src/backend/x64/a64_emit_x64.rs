use std::collections::HashSet;

use rxbyak::{dword_ptr, qword_ptr};
use rxbyak::{JmpType, RegExp, R12, R15, RAX, RBP, RBX};

use crate::backend::x64::a64_emit_x64_memory::{gen_fastmem_fallbacks, FastmemFallbacksTable};
use crate::backend::x64::block_cache::{BlockCache, CachedBlock};
use crate::backend::x64::block_of_code::{
    BlockOfCode, DispatcherLabels, JitStateOffsets, RunCodeCallbacks, RunCodeFn,
};
use crate::backend::x64::emit::emit_block;
use crate::backend::x64::emit_context::{ArchConfig, DeferredEmitCtx, EmitConfig, EmitContext};
use crate::backend::x64::exception_handler::{
    DoNotFastmemMarker, ExceptionHandler, FastmemPatchTable,
};
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::hostloc::{HostLoc, ANY_GPR, ANY_XMM, HOST_R13, HOST_R14};
use crate::backend::x64::jit_state::{A64JitState, RSB_PTR_MASK};
use crate::backend::x64::patch_info::{PatchTable, PatchType};
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::frontend::a64::translate::{translate, MemoryReadCodeFn, TranslationOptions};
use crate::ir::block::Block;
use crate::ir::location::{A64LocationDescriptor, LocationDescriptor};
use crate::ir::opt;
use crate::ir::types::Type;
use crate::jit_config::OptimizationFlag;

/// Minimum space remaining in the code buffer before triggering a cache clear.
const MIN_SPACE_REMAINING: usize = 1024 * 1024; // 1 MB

fn allocation_gpr_order(page_table_present: bool, fastmem_enabled: bool) -> Vec<HostLoc> {
    let mut gprs = ANY_GPR.to_vec();
    if page_table_present {
        gprs.retain(|&loc| loc != HOST_R14);
    }
    if fastmem_enabled {
        gprs.retain(|&loc| loc != HOST_R13);
    }
    gprs
}

/// Fast dispatch table entry.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FastDispatchEntry {
    pub location_descriptor: u64,
    pub code_ptr: u64,
}

/// Number of entries in the fast dispatch table (must be power of 2).
const FAST_DISPATCH_TABLE_SIZE: usize = 1 << 20; // 1M entries = 16 MB
/// Mask for fast dispatch table index (16-byte aligned entries).
const FAST_DISPATCH_TABLE_MASK: u32 = ((FAST_DISPATCH_TABLE_SIZE - 1) as u32) << 4;

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

/// The block compilation pipeline: translate → optimize → emit → cache.
///
/// Owns the `BlockOfCode` (code buffer + dispatcher) and `BlockCache`.
pub struct A64EmitX64 {
    /// Owns the platform exception registration and removes it before the code
    /// buffer and callback state are dropped, matching upstream `EmitX64`.
    exception_handler: ExceptionHandler,
    pub code: BlockOfCode,
    pub cache: BlockCache,
    pub dispatcher_labels: DispatcherLabels,
    pub emit_config: EmitConfig,
    pub translation_options: TranslationOptions,
    /// Fine-grained optimization flags (replaces separate booleans).
    pub optimizations: OptimizationFlag,
    /// Block linking: maps target location → patch slots pointing at it.
    pub patch_table: PatchTable,
    /// Code buffer offset of the PopRSBHint terminal handler.
    pub terminal_handler_pop_rsb_hint: Option<usize>,
    /// Code buffer offset of the FastDispatchHint terminal handler.
    pub terminal_handler_fast_dispatch_hint: Option<usize>,
    /// Fast dispatch hash table (heap-allocated, stable pointer).
    pub fast_dispatch_table: Option<Box<[FastDispatchEntry]>>,
    /// Pre-generated fastmem fallback-stub table. Populated once at
    /// `new()` time. Each entry maps `(ordered, bitsize, vaddr_idx,
    /// value_idx)` → byte offset of the stub in the code buffer.
    /// Mirrors upstream's `read_fallbacks` / `write_fallbacks` /
    /// `exclusive_write_fallbacks` maps in `a64_emit_x64.h:74-76`.
    pub fastmem_fallbacks: FastmemFallbacksTable,
    /// Per-instruction fastmem patch info. Looked up by the SIGSEGV
    /// handler at fault time to redirect the faulting RIP to the
    /// fallback stub. Mirrors upstream `fastmem_patch_info`.
    ///
    /// Heap-boxed so the SIGSEGV-handler closure can capture a stable
    /// raw pointer to it; `A64EmitX64` may move when returned from
    /// `new()`, but the box's contents stay put.
    pub fastmem_patches: Box<FastmemPatchTable>,
    /// Set of `(LocationDescriptor, inst_id)` markers that should not be
    /// emitted as fastmem after a fault. Mirrors upstream `do_not_fastmem`.
    pub do_not_fastmem: HashSet<DoNotFastmemMarker>,
    /// Whether the JIT was constructed with a `fastmem_pointer` set.
    /// Captured in `new()` from `RunCodeCallbacks.fastmem_pointer` and
    /// used by `get_or_compile_block` to set `ctx.fastmem_available`.
    pub fastmem_enabled: bool,
    /// Owned copy of the run-code callbacks. Held so `gen_terminal_handlers`
    /// (called from `new()` and again from `clear_cache`) can emit a call
    /// to `lookup_block` from inside the FastDispatch miss path. Mirrors
    /// upstream where `LookupBlock()` is invoked directly in the miss path
    /// at `a64_emit_x64.cpp:219` (between storing the descriptor and the
    /// jmp rax).
    pub run_callbacks: RunCodeCallbacks,
    /// Emulator core index (0..N-1). Used to address per-core diagnostic
    /// counters at JIT-emit time, e.g. for `RUZU_BLOCK_PROLOGUE_COUNT_PC`.
    /// Set via `set_processor_id` after construction.
    pub processor_id: usize,
}

impl A64EmitX64 {
    /// Create a new A64EmitX64 with dispatcher prelude and empty block cache.
    pub fn new(
        emit_config: EmitConfig,
        run_callbacks: RunCodeCallbacks,
        translation_options: TranslationOptions,
        optimizations: OptimizationFlag,
        cache_size: usize,
    ) -> Result<Self, String> {
        let mut code = BlockOfCode::with_size_and_offsets(
            cache_size,
            JitStateOffsets {
                halt_reason: A64JitState::offset_of_halt_reason(),
                guest_mxcsr: A64JitState::offset_of_guest_mxcsr(),
                asimd_mxcsr: A64JitState::offset_of_asimd_mxcsr(),
            },
        )
        .map_err(|e| format!("Failed to allocate code buffer: {:?}", e))?;

        let dispatcher_labels = code
            .gen_run_code(&run_callbacks)
            .map_err(|e| format!("Failed to generate dispatcher: {:?}", e))?;

        let mut exception_handler = ExceptionHandler::new();
        exception_handler.register(code.code_base_ptr(), code.total_size());
        let fastmem_enabled =
            run_callbacks.fastmem_pointer.is_some() && exception_handler.supports_fastmem();

        let mut emitter = Self {
            exception_handler,
            code,
            cache: BlockCache::new(),
            dispatcher_labels,
            emit_config,
            translation_options,
            optimizations,
            patch_table: PatchTable::new(),
            terminal_handler_pop_rsb_hint: None,
            terminal_handler_fast_dispatch_hint: None,
            fast_dispatch_table: None,
            fastmem_fallbacks: FastmemFallbacksTable::new(),
            fastmem_patches: Box::new(FastmemPatchTable::new()),
            do_not_fastmem: HashSet::new(),
            fastmem_enabled,
            run_callbacks,
            processor_id: 0,
        };

        // Generate prelude handlers for RSB and fast dispatch.
        emitter.gen_terminal_handlers()?;

        // Pre-generate the fastmem fallback-stub table. Mirrors
        // upstream `A64EmitX64::GenFastmemFallbacks` invocation in the
        // `A64EmitX64` constructor.
        emitter.fastmem_fallbacks = gen_fastmem_fallbacks(
            &mut emitter.code.asm,
            &emitter.emit_config.callbacks,
            emitter.emit_config.raw_exclusive_write_callbacks.as_ref(),
        );

        // Publish the fastmem callback whenever a fastmem pointer was supplied,
        // matching upstream even when handler installation disabled fastmem.
        // The closure captures the patches table address as a `usize` to
        // satisfy `Send`; the owning ExceptionHandler is dropped first.
        if emitter.run_callbacks.fastmem_pointer.is_some() {
            // Take the heap address of the boxed patch table — stable
            // even if `emitter` itself moves on return from `new()`.
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

    /// Get the run_code function pointer for calling the dispatcher.
    ///
    /// # Safety
    /// The code buffer must have been made executable (via `ready()`).
    pub unsafe fn get_run_code_fn(&mut self) -> Result<RunCodeFn, String> {
        self.code
            .disable_writing()
            .map_err(|e| format!("Failed to set RX protection: {:?}", e))?;
        let base = self.code.code_base_ptr();
        let fn_ptr = base.add(self.dispatcher_labels.run_code_offset);
        Ok(unsafe { std::mem::transmute::<*const u8, RunCodeFn>(fn_ptr) })
    }

    /// Get the step_code function pointer for single-step execution.
    ///
    /// # Safety
    /// The code buffer must have been made executable.
    pub unsafe fn get_step_code_fn(&mut self) -> Result<RunCodeFn, String> {
        self.code
            .disable_writing()
            .map_err(|e| format!("Failed to set RX protection: {:?}", e))?;
        let base = self.code.code_base_ptr();
        let fn_ptr = base.add(self.dispatcher_labels.step_code_offset);
        Ok(unsafe { std::mem::transmute::<*const u8, RunCodeFn>(fn_ptr) })
    }

    /// Make the code buffer writable again (for emitting new blocks).
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

    /// Get or compile a block for the given location.
    ///
    /// Returns the native code entrypoint pointer.
    /// Caller MUST call make_writable() before and get_run_code_fn() after
    /// if this may compile a new block.
    pub fn get_or_compile_block(
        &mut self,
        location: LocationDescriptor,
        read_code: &MemoryReadCodeFn,
    ) -> *const u8 {
        // Check cache first
        if let Some(cached) = self.cache.get(&location) {
            return cached.entrypoint;
        }

        // Check space remaining — clear cache if low
        if self.code.space_remaining() < MIN_SPACE_REMAINING {
            self.clear_cache();
        }

        // Translate: ARM64 → IR
        let a64_loc = A64LocationDescriptor::from_location(location);
        let mut block = translate(a64_loc, read_code, self.translation_options.clone());

        // RUZU_DUMP_IR_AT_PC=0xADDR[,0xADDR2,...] — dump the IR for blocks
        // whose entry PC matches any listed address, both pre- and post-opt.
        let dump_block_at_pc: Vec<u64> = std::env::var("RUZU_DUMP_IR_AT_PC")
            .ok()
            .map(|s| {
                s.split(',')
                    .filter_map(|p| {
                        let p = p.trim().trim_start_matches("0x");
                        u64::from_str_radix(p, 16).ok()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let dump_block_at_pc = if dump_block_at_pc.is_empty() {
            None
        } else {
            Some(dump_block_at_pc)
        };
        if let Some(addrs) = &dump_block_at_pc {
            if addrs.contains(&a64_loc.pc()) {
                eprintln!(
                    "=== IR dump (pre-opt) for block at PC=0x{:08X} ===",
                    a64_loc.pc()
                );
                eprintln!("{}", block);
            }
        }

        // Optimize (per-flag, matching dynarmic)
        opt::polyfill(
            &mut block,
            opt::PolyfillOptions {
                sha256: !self
                    .code
                    .has_host_feature(crate::backend::x64::host_feature::HostFeature::SHA),
                vector_multiply_widen: true,
            },
        );
        opt::a64_callback_config(
            &mut block,
            self.emit_config.hook_data_cache_operations,
            self.emit_config.dczid_el0,
        );
        let skip_getset_at_pc: Vec<u64> = std::env::var("RUZU_SKIP_GETSET_AT_PC")
            .ok()
            .map(|s| {
                s.split(',')
                    .filter_map(|p| {
                        let p = p.trim().trim_start_matches("0x");
                        u64::from_str_radix(p, 16).ok()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let skip_getset_range = std::env::var("RUZU_SKIP_GETSET_RANGE").ok().and_then(|s| {
            let (start, end) = s.split_once(':')?;
            let start = u64::from_str_radix(start.trim().trim_start_matches("0x"), 16).ok()?;
            let end = u64::from_str_radix(end.trim().trim_start_matches("0x"), 16).ok()?;
            Some((start, end))
        });
        let skip_getset_for_pc = skip_getset_at_pc.contains(&a64_loc.pc())
            || skip_getset_range.is_some_and(|(start, end)| {
                let pc = a64_loc.pc();
                pc >= start && pc < end
            });
        if self
            .optimizations
            .contains(OptimizationFlag::GET_SET_ELIMINATION)
            && !skip_getset_for_pc
        {
            opt::a64_get_set_elimination(&mut block);
            opt::dead_code_elimination(&mut block);
        }
        if let Some(addrs) = &dump_block_at_pc {
            if addrs.contains(&a64_loc.pc()) {
                eprintln!(
                    "=== IR dump (post-get-set-elim+dce) for block at PC=0x{:08X} ===",
                    a64_loc.pc()
                );
                eprintln!("{}", block);
            }
        }
        if self.optimizations.contains(OptimizationFlag::CONST_PROP) {
            opt::constant_propagation(&mut block);
            opt::dead_code_elimination(&mut block);
        }
        if self.optimizations.contains(OptimizationFlag::MISC_IR_OPT) {}
        block.rebuild_pseudo_op_links();

        // Build inst_info for register allocator.
        //
        // Mirror upstream `Inst::GetType()` (microinstruction.cpp:624-628)
        // by chasing through Identity arg chains to recover the real type
        // behind any Identity alias.
        let inst_info: Vec<(u32, usize)> = (0..block.instructions.len())
            .map(|i| {
                let inst = &block.instructions[i];
                (
                    inst.use_count,
                    inst_info_bit_width(&block, crate::ir::value::InstRef(i as u32)),
                )
            })
            .collect();

        // Emit in a nested scope so ctx is dropped before we call self.patch()
        let (desc, patch_entries) = {
            // Create emit context with dispatcher offsets and block linking
            let host_features = self.code.host_features();
            let mut ctx = EmitContext::with_dispatcher(
                location,
                &self.emit_config,
                ArchConfig::A64,
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
            // Wire fastmem state for the memory dispatchers in
            // `a64_emit_x64_memory.rs`. The pointer cast keeps
            // `EmitContext` lifetime-clean (the table is owned by `self`
            // which outlives `ctx`).
            ctx.fastmem_available = self.fastmem_enabled;
            ctx.do_not_fastmem = Some(&self.do_not_fastmem);
            ctx.fastmem_fallbacks =
                Some(&self.fastmem_fallbacks as *const FastmemFallbacksTable as *const ());
            ctx.block = Some(&block);

            // Set up block lookup closure for checking if targets are already compiled
            if self.optimizations.contains(OptimizationFlag::BLOCK_LINKING) {
                let cache_ptr = &self.cache as *const BlockCache;
                ctx.block_lookup = Some(Box::new(move |loc| {
                    let cache = unsafe { &*cache_ptr };
                    cache.get(&loc).map(|b| b.entrypoint)
                }));
            }

            // R14 and R13 hold the page-table and fastmem pointers loaded
            // by the dispatcher prelude. Upstream removes each configured
            // base register from the allocator before emitting the block.
            let gpr_order = allocation_gpr_order(
                self.run_callbacks.page_table_pointer.is_some(),
                self.fastmem_enabled,
            );
            // Capture code_base BEFORE we mutably borrow self.code via
            // RegAlloc — once `ra` exists, `self.code` is mutably
            // borrowed and the immutable `.code_base_ptr()` accessor
            // would conflict.
            let code_base = self.code.code_base_ptr() as u64;

            let mut ra = RegAlloc::new(&mut self.code.asm, gpr_order, ANY_XMM.to_vec(), inst_info);
            ra.constant_pool = Some(&mut self.code.constant_pool);

            // RUZU_BLOCK_PROLOGUE_COUNT_PC=0xLO-0xHI — inline per-core hit
            // counter at the block prologue. Bypasses FAST_DISPATCH chaining
            // (which skips the cold-entry trace hook). Emit:
            //   push rax
            //   mov  rax, &counters[processor_id]
            //   lock inc qword [rax]
            //   pop  rax
            // ~13 bytes per qualifying block prologue. No effect when env unset.
            // Stash the prologue-counter address for emit_block to consume.
            // The actual `lock inc` is emitted INSIDE emit_block, after the
            // entrypoint offset is captured; emitting it here would put the
            // counter before the entrypoint and the running code would skip it.
            if let Some((lo, hi)) = crate::jit::block_prologue_count_range() {
                let pc = a64_loc.pc() as u32;
                if pc >= lo && pc < hi {
                    let counters = crate::jit::block_prologue_counters();
                    let idx = self.processor_id.min(counters.len() - 1);
                    let counter_addr = &counters[idx] as *const std::sync::atomic::AtomicU64 as u64;
                    ctx.prologue_counter_addr.set(Some(counter_addr));
                }
            }

            let desc = emit_block(&ctx, &mut ra, &block);

            // Drain deferred-emit closures: each one binds an `abort`
            // label, calls the pre-generated fallback stub, records the
            // FastmemPatchInfo (key = mov RIP), and jumps to `end`.
            // Mirrors upstream's post-emit deferred-emit drain in
            // `EmitX64::Emit` (run after the main block is emitted).
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
            (desc, patch_entries)
        };

        // Compute absolute entrypoint
        let entrypoint = unsafe { self.code.code_base_ptr().add(desc.entrypoint_offset) };
        let end = unsafe { entrypoint.add(desc.size) };
        crate::backend::x64::perf_map::register(
            entrypoint,
            end,
            &format!("a64_{:016X}_fpcr{:08X}", a64_loc.pc(), a64_loc.fpcr()),
        );

        // Process patch entries from emission
        for entry in &patch_entries {
            let info = self.patch_table.entry(entry.target).or_default();
            match entry.patch_type {
                PatchType::Jg => info.jg.push(entry.code_offset),
                PatchType::Jz => info.jz.push(entry.code_offset),
                PatchType::Jmp => info.jmp.push(entry.code_offset),
                PatchType::MovRcx => info.mov_rcx.push(entry.code_offset),
            }
        }

        // RUZU_DUMP_X64_AT_PC=0xADDR[,0xADDR2,...] — dump emitted x86 bytes
        // for the block whose guest entry PC matches. Useful for debugging
        // regalloc bugs where IR is correct but emitted x86 misbehaves.
        if let Ok(spec) = std::env::var("RUZU_DUMP_X64_AT_PC") {
            let pcs: Vec<u64> = spec
                .split(',')
                .filter_map(|p| u64::from_str_radix(p.trim().trim_start_matches("0x"), 16).ok())
                .collect();
            if pcs.contains(&a64_loc.pc()) {
                let bytes = unsafe { std::slice::from_raw_parts(entrypoint, desc.size) };
                use std::sync::atomic::{AtomicU32, Ordering};
                static COMPILE_COUNT: AtomicU32 = AtomicU32::new(0);
                let n = COMPILE_COUNT.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "=== X86 dump #{} for block PC=0x{:08X} fpcr=0x{:08X} entrypoint={:p} size={} ===",
                    n, a64_loc.pc(), a64_loc.fpcr(), entrypoint, desc.size
                );
                let chunk_size = 16;
                for (i, chunk) in bytes.chunks(chunk_size).enumerate() {
                    eprint!("  +0x{:04X}:", i * chunk_size);
                    for b in chunk {
                        eprint!(" {:02X}", b);
                    }
                    eprintln!();
                }
                eprintln!("=== end x86 dump ===");
            }
        }

        // Cache the compiled block
        self.cache.insert(
            location,
            CachedBlock {
                entrypoint,
                entrypoint_offset: desc.entrypoint_offset,
                size: desc.size,
            },
        );

        // Patch any existing slots that target this newly compiled block
        self.patch(location, Some(entrypoint));

        entrypoint
    }

    /// Patch all link slots targeting `target_loc` to jump to `code_ptr`.
    ///
    /// If `code_ptr` is None, patches slots back to the dispatcher fallback.
    fn patch(&mut self, target_loc: LocationDescriptor, code_ptr: Option<*const u8>) {
        let info = match self.patch_table.get(&target_loc) {
            Some(info) => info.clone(),
            None => return,
        };

        let code_base = self.code.code_base_ptr();
        let offsets = self.dispatcher_labels.return_from_run_code;

        let target = match code_ptr {
            Some(ptr) => ptr as usize,
            None => code_base as usize + offsets[0], // fallback to dispatcher
        };

        // Patch jg slots (6-byte jg rel32 at each offset)
        for &offset in &info.jg {
            let saved_size = self.code.asm.size();
            self.code.asm.set_size(offset);
            // jg rel32: 0x0F 0x8F + disp32
            let jg_end = offset + 6;
            let jg_end_addr = code_base as usize + jg_end;
            let disp = (target as i64) - (jg_end_addr as i64);
            self.code.asm.db(0x0F).unwrap();
            self.code.asm.db(0x8F).unwrap();
            self.code.asm.dd(disp as u32).unwrap();
            self.code.asm.set_size(saved_size);
        }

        // Patch jz slots (6-byte jz rel32 at each offset)
        for &offset in &info.jz {
            let saved_size = self.code.asm.size();
            self.code.asm.set_size(offset);
            let jz_end = offset + 6;
            let jz_end_addr = code_base as usize + jz_end;
            let disp = (target as i64) - (jz_end_addr as i64);
            self.code.asm.db(0x0F).unwrap();
            self.code.asm.db(0x84).unwrap();
            self.code.asm.dd(disp as u32).unwrap();
            self.code.asm.set_size(saved_size);
        }

        // Patch jmp slots (5-byte jmp rel32 at each offset)
        for &offset in &info.jmp {
            let saved_size = self.code.asm.size();
            self.code.asm.set_size(offset);
            let jmp_end = offset + 5;
            let jmp_end_addr = code_base as usize + jmp_end;
            let disp = (target as i64) - (jmp_end_addr as i64);
            self.code.asm.db(0xE9).unwrap();
            self.code.asm.dd(disp as u32).unwrap();
            self.code.asm.set_size(saved_size);
        }

        // Patch mov rcx slots (10-byte mov rcx, imm64)
        for &offset in &info.mov_rcx {
            let saved_size = self.code.asm.size();
            self.code.asm.set_size(offset);
            // REX.W + MOV RCX: 48 B9 + imm64
            self.code.asm.db(0x48).unwrap();
            self.code.asm.db(0xB9).unwrap();
            self.code.asm.dq(target as u64).unwrap();
            self.code.asm.set_size(saved_size);
        }
    }

    /// Unpatch all link slots targeting `target_loc` (revert to dispatcher).
    fn unpatch(&mut self, target_loc: LocationDescriptor) {
        self.patch(target_loc, None);
    }

    /// Generate prelude code for RSB pop and fast dispatch terminal handlers.
    ///
    /// These are emitted into the code buffer before user blocks, as part of
    /// the prelude. Terminals jump to these offsets instead of going through
    /// the full dispatcher.
    fn gen_terminal_handlers(&mut self) -> Result<(), String> {
        let code_base = self.code.code_base_ptr();
        let rfrc = self.dispatcher_labels.return_from_run_code;
        let has_sse42 = self.code.has_host_feature(HostFeature::SSE42);
        let asm = &mut self.code.asm;

        // ---- PopRSBHint handler ----
        // Computes location descriptor from jit_state, looks up RSB.
        // On hit: jump directly to cached code. On miss: fall through to dispatcher.
        let pop_rsb_offset = asm.size();

        // Build location descriptor from PC + FPCR:
        // RBX = (fpcr & FPCR_MASK) << FPCR_SHIFT | (pc & PC_MASK)
        let pc_offset = A64JitState::offset_of_pc();
        let fpcr_offset = A64JitState::offset_of_fpcr();
        let rsb_ptr_offset = A64JitState::offset_of_rsb_ptr();
        let rsb_loc_offset = A64JitState::offset_of_rsb_location_descriptors();
        let rsb_code_offset = A64JitState::offset_of_rsb_codeptrs();
        let rbp = RBP;

        // Load and mask PC into RBX. This calculation must remain identical
        // to A64LocationDescriptor::unique_hash, as in upstream.
        asm.mov(RBX, qword_ptr(RegExp::from(R15) + pc_offset as i32))
            .map_err(|e| format!("RSB handler: {:?}", e))?;
        asm.mov(rbp, A64LocationDescriptor::PC_MASK as i64)
            .map_err(|e| format!("RSB handler: {:?}", e))?;
        asm.and_(RBX, rbp)
            .map_err(|e| format!("RSB handler: {:?}", e))?;

        // Load FPCR, mask, shift, OR into RBX
        asm.mov(rbp, qword_ptr(RegExp::from(R15) + fpcr_offset as i32))
            .map_err(|e| format!("RSB handler: {:?}", e))?;
        asm.and_(rbp, 0x07C8_0000i32)
            .map_err(|e| format!("RSB handler: {:?}", e))?;
        asm.shl(rbp, 37u8)
            .map_err(|e| format!("RSB handler: {:?}", e))?;
        asm.or_(RBX, rbp)
            .map_err(|e| format!("RSB handler: {:?}", e))?;

        // Decrement RSB pointer and mask
        // EAX = (rsb_ptr - 1) & RSB_PTR_MASK
        asm.mov(
            rxbyak::Reg::gpr32(0),
            dword_ptr(RegExp::from(R15) + rsb_ptr_offset as i32),
        )
        .map_err(|e| format!("RSB handler: {:?}", e))?;
        asm.sub(rxbyak::Reg::gpr32(0), 1i32)
            .map_err(|e| format!("RSB handler: {:?}", e))?;
        asm.and_(rxbyak::Reg::gpr32(0), RSB_PTR_MASK as i32)
            .map_err(|e| format!("RSB handler: {:?}", e))?;
        // Store updated pointer
        asm.mov(
            dword_ptr(RegExp::from(R15) + rsb_ptr_offset as i32),
            rxbyak::Reg::gpr32(0),
        )
        .map_err(|e| format!("RSB handler: {:?}", e))?;

        // Compare: rsb_location_descriptors[eax] == RBX?
        // RAX is zero-extended 32-bit index. Scale by 8 for u64 array access.
        // Use RBP as scratch to compute address = R15 + RAX*8 + offset
        asm.lea(
            rbp,
            qword_ptr(RegExp::from(R15) + RAX * 8u8 + rsb_loc_offset as i32),
        )
        .map_err(|e| format!("RSB handler: {:?}", e))?;
        asm.cmp(qword_ptr(RegExp::from(rbp)), RBX)
            .map_err(|e| format!("RSB handler: {:?}", e))?;

        // Miss: jump to dispatcher return_from_run_code[0]
        let rsb_miss = asm.create_label();
        asm.jnz(&rsb_miss, JmpType::Near)
            .map_err(|e| format!("RSB handler: {:?}", e))?;

        // Hit: compute code pointer address and jump
        asm.lea(
            rbp,
            qword_ptr(RegExp::from(R15) + RAX * 8u8 + rsb_code_offset as i32),
        )
        .map_err(|e| format!("RSB handler: {:?}", e))?;
        asm.jmp_reg(qword_ptr(RegExp::from(rbp)))
            .map_err(|e| format!("RSB handler: {:?}", e))?;

        // Miss label: fall through to dispatcher
        asm.bind(&rsb_miss)
            .map_err(|e| format!("RSB handler: {:?}", e))?;

        // Jump to return_from_run_code[0] (dispatcher lookup)
        {
            let jmp_end = asm.size() + 5;
            let target_addr = code_base as usize + rfrc[0];
            let jmp_end_addr = code_base as usize + jmp_end;
            let disp = (target_addr as i64) - (jmp_end_addr as i64);
            asm.db(0xE9).map_err(|e| format!("RSB handler: {:?}", e))?;
            asm.dd(disp as u32)
                .map_err(|e| format!("RSB handler: {:?}", e))?;
        }

        self.terminal_handler_pop_rsb_hint = Some(pop_rsb_offset);

        // ---- FastDispatchHint handler ----
        // Uses a hash table for fast block lookup by location descriptor.
        //
        // Allocate the fast dispatch table
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

        // Build location descriptor from PC + FPCR → RBX (same as RSB)
        asm.mov(RBX, qword_ptr(RegExp::from(R15) + pc_offset as i32))
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;
        asm.mov(rbp, A64LocationDescriptor::PC_MASK as i64)
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;
        asm.and_(RBX, rbp)
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;
        asm.mov(rbp, qword_ptr(RegExp::from(R15) + fpcr_offset as i32))
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;
        asm.and_(rbp, 0x07C8_0000i32)
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;
        asm.shl(rbp, 37u8)
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;
        asm.or_(RBX, rbp)
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;

        // R12 = table base pointer
        asm.mov(R12, table_ptr as i64)
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;

        // Upstream hashes the location descriptor with the table address via
        // CRC32 when SSE4.2 is present; without it the descriptor itself is
        // masked. The invalidation path below performs the identical hash.
        asm.mov(rbp, RBX)
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;
        if has_sse42 {
            asm.crc32(rbp, R12)
                .map_err(|e| format!("FastDispatch handler: {:?}", e))?;
        }
        let ebp = rxbyak::Reg::gpr32(5); // EBP
        asm.and_(ebp, FAST_DISPATCH_TABLE_MASK as i32)
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;

        // RBP = &table[index] = R12 + RBP
        asm.add(rbp, R12)
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;

        // Compare table[index].location_descriptor with RBX
        asm.cmp(qword_ptr(RegExp::from(rbp)), RBX)
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;

        let fd_miss = asm.create_label();
        asm.jnz(&fd_miss, JmpType::Near)
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;

        // Hit: jmp [RBP + 8] (code_ptr field)
        asm.jmp_reg(qword_ptr(RegExp::from(rbp) + 8i32))
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;

        // Miss: store descriptor, call lookup_block, store the resolved
        // code pointer back into the table entry, then jmp to it. Mirrors
        // upstream `a64_emit_x64.cpp:217-221`:
        //   L(fast_dispatch_cache_miss);
        //   mov [rbp + .location_descriptor], rbx
        //   LookupBlock();          // returns code_ptr in rax
        //   mov [rbp + .code_ptr], rax
        //   jmp rax
        //
        // The previous version stored the descriptor and then `jmp`ed to
        // the dispatcher (return_from_run_code[0]), which left
        // `code_ptr=0` in the table. The next dispatch with the same
        // descriptor would hit this entry and `jmp [rbp+8]` would jump to
        // host RIP=0, taking down the emulator with a NULL-call SIGSEGV
        // (observed booting STK after Binder Connect).
        asm.bind(&fd_miss)
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;
        asm.mov(qword_ptr(RegExp::from(rbp)), RBX)
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;
        // RBP and R15 are SystemV callee-saved → preserved across the
        // call. RAX returns the resolved code pointer.
        self.run_callbacks
            .lookup_block
            .emit_call_simple(asm)
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;
        asm.mov(qword_ptr(RegExp::from(rbp) + 8i32), RAX)
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;
        asm.jmp_reg(RAX)
            .map_err(|e| format!("FastDispatch handler: {:?}", e))?;
        // `rfrc` and `code_base` only used by RSB miss fall-through above
        // (still needed there).
        let _ = (code_base, rfrc);

        self.terminal_handler_fast_dispatch_hint = Some(fast_dispatch_offset);

        // Update code_begin_offset to include these handlers
        self.code.code_begin_offset = self.code.asm.size();

        Ok(())
    }

    /// Clear the fast dispatch table (invalidate all entries).
    pub fn clear_fast_dispatch_table(&mut self) {
        if let Some(ref mut table) = self.fast_dispatch_table {
            for entry in table.iter_mut() {
                entry.location_descriptor = 0xFFFF_FFFF_FFFF_FFFF;
                entry.code_ptr = 0;
            }
        }
    }

    /// Invalidate a specific entry in the fast dispatch table.
    fn invalidate_fast_dispatch_entry(&mut self, location: LocationDescriptor) {
        let has_sse42 = self.code.has_host_feature(HostFeature::SSE42);
        if let Some(ref mut table) = self.fast_dispatch_table {
            let desc = location.value();
            let table_ptr = table.as_ptr() as u64;
            let hash = fast_dispatch_hash(desc, table_ptr, has_sse42) & FAST_DISPATCH_TABLE_MASK;
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
    pub fn process_pending_fastmem_recompiles(&mut self) -> Result<usize, String> {
        let markers = self.fastmem_patches.take_pending_recompiles();
        if markers.is_empty() {
            return Ok(0);
        }

        let marker_count = markers.len();
        self.make_writable()?;
        let mut locations = HashSet::new();
        for marker in markers {
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

    /// Clear all cached blocks and reset the code buffer.
    ///
    /// `BlockOfCode::clear_cache` resets the assembler cursor to
    /// `code_begin_offset` (right after the dispatcher prelude). That
    /// wipes the terminal handlers AND the fastmem fallback stubs that
    /// were generated post-prelude. Re-emit them from scratch and
    /// invalidate the stale fastmem patch table — old RIPs no longer
    /// point at valid stubs.
    pub fn clear_cache(&mut self) {
        self.patch_table.clear();
        self.clear_fast_dispatch_table();
        self.cache.clear();
        self.fastmem_patches.clear();
        crate::backend::x64::perf_map::clear();
        self.code.clear_cache();
        // Re-emit terminal handlers and fastmem fallbacks. Their
        // offsets in the code buffer change, but the SIGSEGV-handler
        // registration's `code_begin..code_end` range is the WHOLE
        // buffer so it stays valid.
        self.terminal_handler_pop_rsb_hint = None;
        self.terminal_handler_fast_dispatch_hint = None;
        self.gen_terminal_handlers()
            .expect("re-generating terminal handlers after clear_cache failed");
        self.fastmem_fallbacks = gen_fastmem_fallbacks(
            &mut self.code.asm,
            &self.emit_config.callbacks,
            self.emit_config.raw_exclusive_write_callbacks.as_ref(),
        );
    }

    /// Invalidate cached blocks whose PC falls within a memory range.
    pub fn invalidate_range(&mut self, start: u64, length: u64) {
        let end = start.wrapping_add(length);

        // Collect locations to invalidate
        let to_remove: Vec<LocationDescriptor> = self
            .cache
            .keys()
            .filter(|loc| {
                let pc = loc.value() & 0x00FF_FFFF_FFFF_FFFF;
                pc >= start && pc < end
            })
            .copied()
            .collect();

        // Unpatch all slots targeting the removed blocks
        for &loc in &to_remove {
            self.unpatch(loc);
            self.patch_table.remove(&loc);
            self.invalidate_fast_dispatch_entry(loc);
        }

        let had_blocks = !self.cache.is_empty();
        self.cache.invalidate_range(start, length);
        // If all blocks were invalidated, clear code buffer to reclaim space.
        if had_blocks && self.cache.is_empty() {
            self.patch_table.clear();
            self.code.clear_cache();
        }
    }
}

/// Map an IR Type to its bit width for register allocation.
fn type_bit_width(ty: Type) -> usize {
    match ty {
        Type::Void => 0,
        Type::U1 => 8, // stored in a GPR byte
        Type::U8 => 8,
        Type::U16 => 16,
        Type::U32 => 32,
        Type::U64 => 64,
        Type::U128 => 128,
        Type::NZCVFlags => 32,
        Type::Cond => 32,
        Type::A64Reg => 64,
        Type::A64Vec => 64,
        _ => 64, // Opaque, Table, AccType — default to 64
    }
}

fn inst_info_bit_width(block: &Block, inst_ref: crate::ir::value::InstRef) -> usize {
    let inst = block.get(inst_ref);
    if inst.opcode == crate::ir::opcode::Opcode::Identity {
        type_bit_width(block.inst_real_return_type(inst_ref))
    } else {
        type_bit_width(inst.return_type())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::x64::callback::ArgCallback;
    use crate::ir::opcode::Opcode;
    use crate::ir::value::Value;

    extern "C" fn stub_lookup(_arg: u64) -> u64 {
        0
    }
    extern "C" fn stub_add_ticks(_arg: u64, _ticks: u64) {}
    extern "C" fn stub_get_ticks(_arg: u64) -> u64 {
        1000
    }

    fn make_test_callbacks() -> RunCodeCallbacks {
        RunCodeCallbacks {
            lookup_block: Box::new(ArgCallback::new(stub_lookup as u64, 0)),
            add_ticks: Box::new(ArgCallback::new(stub_add_ticks as u64, 0)),
            get_ticks_remaining: Box::new(ArgCallback::new(stub_get_ticks as u64, 0)),
            enable_cycle_counting: true,
            fastmem_pointer: None,
            page_table_pointer: None,
        }
    }

    #[test]
    fn test_type_bit_width() {
        assert_eq!(type_bit_width(Type::Void), 0);
        assert_eq!(type_bit_width(Type::U32), 32);
        assert_eq!(type_bit_width(Type::U64), 64);
        assert_eq!(type_bit_width(Type::U128), 128);
    }

    #[test]
    fn identity_inst_info_width_uses_forwarded_type() {
        let loc = A64LocationDescriptor::new(0x1000, 0, false).to_location();
        let mut block = Block::new(loc);
        let value = block.append(Opcode::FPAdd32, &[Value::ImmU32(0), Value::ImmU32(0)]);
        let identity = block.append(Opcode::Identity, &[Value::Inst(value)]);
        assert_eq!(inst_info_bit_width(&block, identity), 32);
    }

    #[test]
    fn test_rsb_handler_generated() {
        let emit_config = crate::backend::x64::emit_context::EmitConfig {
            coprocessors: crate::interface::a32::config::empty_coprocessors(),
            callbacks: crate::backend::x64::emit_context::EmitCallbacks {
                memory_read_8: Box::new(ArgCallback::new(0, 0)),
                memory_read_16: Box::new(ArgCallback::new(0, 0)),
                memory_read_32: Box::new(ArgCallback::new(0, 0)),
                memory_read_64: Box::new(ArgCallback::new(0, 0)),
                memory_read_128: Box::new(ArgCallback::new(0, 0)),
                memory_write_8: Box::new(ArgCallback::new(0, 0)),
                memory_write_16: Box::new(ArgCallback::new(0, 0)),
                memory_write_32: Box::new(ArgCallback::new(0, 0)),
                memory_write_64: Box::new(ArgCallback::new(0, 0)),
                memory_write_128: Box::new(ArgCallback::new(0, 0)),
                call_supervisor: Box::new(ArgCallback::new(0, 0)),
                exception_raised: Box::new(ArgCallback::new(0, 0)),
                data_cache_operation: Box::new(ArgCallback::new(0, 0)),
                instruction_cache_operation: Box::new(ArgCallback::new(0, 0)),
                instruction_synchronization_barrier: Box::new(ArgCallback::new(0, 0)),
                add_ticks: Box::new(ArgCallback::new(0, 0)),
                get_ticks_remaining: Box::new(ArgCallback::new(0, 0)),
                get_cntpct: Box::new(ArgCallback::new(0, 0)),
                exclusive_clear: Box::new(ArgCallback::new(0, 0)),
                exclusive_read_8: Box::new(ArgCallback::new(0, 0)),
                exclusive_read_16: Box::new(ArgCallback::new(0, 0)),
                exclusive_read_32: Box::new(ArgCallback::new(0, 0)),
                exclusive_read_64: Box::new(ArgCallback::new(0, 0)),
                exclusive_read_128: Box::new(ArgCallback::new(0, 0)),
                exclusive_write_8: Box::new(ArgCallback::new(0, 0)),
                exclusive_write_16: Box::new(ArgCallback::new(0, 0)),
                exclusive_write_32: Box::new(ArgCallback::new(0, 0)),
                exclusive_write_64: Box::new(ArgCallback::new(0, 0)),
                exclusive_write_128: Box::new(ArgCallback::new(0, 0)),
            },
            raw_exclusive_write_callbacks: None,
            enable_cycle_counting: true,
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
            global_monitor: None,
            cntfrq_el0: 600_000_000,
            ctr_el0: 0x8444_c004,
            dczid_el0: 4,
            hook_data_cache_operations: false,
            hook_isb: false,
        };
        let run_callbacks = make_test_callbacks();
        let translation_options = crate::frontend::a64::translate::TranslationOptions::default();
        let emitter = A64EmitX64::new(
            emit_config,
            run_callbacks,
            translation_options,
            OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
            4 * 1024 * 1024,
        )
        .unwrap();

        assert!(
            emitter.terminal_handler_pop_rsb_hint.is_some(),
            "RSB handler should be generated"
        );
        assert!(
            emitter.terminal_handler_fast_dispatch_hint.is_some(),
            "Fast dispatch handler should be generated"
        );

        let rsb_off = emitter.terminal_handler_pop_rsb_hint.unwrap();
        let fd_off = emitter.terminal_handler_fast_dispatch_hint.unwrap();
        assert!(rsb_off > 0, "RSB handler should be at non-zero offset");
        assert!(
            fd_off > rsb_off,
            "Fast dispatch handler should come after RSB"
        );
    }

    #[test]
    fn test_fast_dispatch_table_allocated() {
        let emit_config = crate::backend::x64::emit_context::EmitConfig {
            coprocessors: crate::interface::a32::config::empty_coprocessors(),
            callbacks: crate::backend::x64::emit_context::EmitCallbacks {
                memory_read_8: Box::new(ArgCallback::new(0, 0)),
                memory_read_16: Box::new(ArgCallback::new(0, 0)),
                memory_read_32: Box::new(ArgCallback::new(0, 0)),
                memory_read_64: Box::new(ArgCallback::new(0, 0)),
                memory_read_128: Box::new(ArgCallback::new(0, 0)),
                memory_write_8: Box::new(ArgCallback::new(0, 0)),
                memory_write_16: Box::new(ArgCallback::new(0, 0)),
                memory_write_32: Box::new(ArgCallback::new(0, 0)),
                memory_write_64: Box::new(ArgCallback::new(0, 0)),
                memory_write_128: Box::new(ArgCallback::new(0, 0)),
                call_supervisor: Box::new(ArgCallback::new(0, 0)),
                exception_raised: Box::new(ArgCallback::new(0, 0)),
                data_cache_operation: Box::new(ArgCallback::new(0, 0)),
                instruction_cache_operation: Box::new(ArgCallback::new(0, 0)),
                instruction_synchronization_barrier: Box::new(ArgCallback::new(0, 0)),
                add_ticks: Box::new(ArgCallback::new(0, 0)),
                get_ticks_remaining: Box::new(ArgCallback::new(0, 0)),
                get_cntpct: Box::new(ArgCallback::new(0, 0)),
                exclusive_clear: Box::new(ArgCallback::new(0, 0)),
                exclusive_read_8: Box::new(ArgCallback::new(0, 0)),
                exclusive_read_16: Box::new(ArgCallback::new(0, 0)),
                exclusive_read_32: Box::new(ArgCallback::new(0, 0)),
                exclusive_read_64: Box::new(ArgCallback::new(0, 0)),
                exclusive_read_128: Box::new(ArgCallback::new(0, 0)),
                exclusive_write_8: Box::new(ArgCallback::new(0, 0)),
                exclusive_write_16: Box::new(ArgCallback::new(0, 0)),
                exclusive_write_32: Box::new(ArgCallback::new(0, 0)),
                exclusive_write_64: Box::new(ArgCallback::new(0, 0)),
                exclusive_write_128: Box::new(ArgCallback::new(0, 0)),
            },
            raw_exclusive_write_callbacks: None,
            enable_cycle_counting: true,
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
            global_monitor: None,
            cntfrq_el0: 600_000_000,
            ctr_el0: 0x8444_c004,
            dczid_el0: 4,
            hook_data_cache_operations: false,
            hook_isb: false,
        };
        let run_callbacks = make_test_callbacks();
        let translation_options = crate::frontend::a64::translate::TranslationOptions::default();
        let emitter = A64EmitX64::new(
            emit_config,
            run_callbacks,
            translation_options,
            OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
            4 * 1024 * 1024,
        )
        .unwrap();

        assert!(
            emitter.fast_dispatch_table.is_some(),
            "Fast dispatch table should be allocated"
        );
        let table = emitter.fast_dispatch_table.as_ref().unwrap();
        assert_eq!(table.len(), FAST_DISPATCH_TABLE_SIZE);
        // All entries should be initialized to invalid
        assert_eq!(table[0].location_descriptor, 0xFFFF_FFFF_FFFF_FFFF);
        assert_eq!(table[0].code_ptr, 0);
    }

    #[test]
    fn test_single_step_disables_rsb_and_fast_dispatch() {
        // When is_single_step is true, RSB and fast dispatch should be bypassed
        let emit_config = crate::backend::x64::emit_context::EmitConfig {
            coprocessors: crate::interface::a32::config::empty_coprocessors(),
            callbacks: crate::backend::x64::emit_context::EmitCallbacks {
                memory_read_8: Box::new(ArgCallback::new(0, 0)),
                memory_read_16: Box::new(ArgCallback::new(0, 0)),
                memory_read_32: Box::new(ArgCallback::new(0, 0)),
                memory_read_64: Box::new(ArgCallback::new(0, 0)),
                memory_read_128: Box::new(ArgCallback::new(0, 0)),
                memory_write_8: Box::new(ArgCallback::new(0, 0)),
                memory_write_16: Box::new(ArgCallback::new(0, 0)),
                memory_write_32: Box::new(ArgCallback::new(0, 0)),
                memory_write_64: Box::new(ArgCallback::new(0, 0)),
                memory_write_128: Box::new(ArgCallback::new(0, 0)),
                call_supervisor: Box::new(ArgCallback::new(0, 0)),
                exception_raised: Box::new(ArgCallback::new(0, 0)),
                data_cache_operation: Box::new(ArgCallback::new(0, 0)),
                instruction_cache_operation: Box::new(ArgCallback::new(0, 0)),
                instruction_synchronization_barrier: Box::new(ArgCallback::new(0, 0)),
                add_ticks: Box::new(ArgCallback::new(0, 0)),
                get_ticks_remaining: Box::new(ArgCallback::new(0, 0)),
                get_cntpct: Box::new(ArgCallback::new(0, 0)),
                exclusive_clear: Box::new(ArgCallback::new(0, 0)),
                exclusive_read_8: Box::new(ArgCallback::new(0, 0)),
                exclusive_read_16: Box::new(ArgCallback::new(0, 0)),
                exclusive_read_32: Box::new(ArgCallback::new(0, 0)),
                exclusive_read_64: Box::new(ArgCallback::new(0, 0)),
                exclusive_read_128: Box::new(ArgCallback::new(0, 0)),
                exclusive_write_8: Box::new(ArgCallback::new(0, 0)),
                exclusive_write_16: Box::new(ArgCallback::new(0, 0)),
                exclusive_write_32: Box::new(ArgCallback::new(0, 0)),
                exclusive_write_64: Box::new(ArgCallback::new(0, 0)),
                exclusive_write_128: Box::new(ArgCallback::new(0, 0)),
            },
            raw_exclusive_write_callbacks: None,
            enable_cycle_counting: false,
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
            global_monitor: None,
            cntfrq_el0: 600_000_000,
            ctr_el0: 0x8444_c004,
            dczid_el0: 4,
            hook_data_cache_operations: false,
            hook_isb: false,
        };

        // Create a single-stepping location descriptor
        let a64_loc = A64LocationDescriptor::new(0x1000, 0, true);
        let loc = a64_loc.to_location();

        let ctx = EmitContext::with_dispatcher(
            loc,
            &emit_config,
            ArchConfig::A64,
            crate::backend::x64::block_of_code::get_host_features(),
            OptimizationFlag::NO_OPTIMIZATIONS,
            [100, 200, 300, 400],
            std::ptr::null(),
        );

        assert!(ctx.is_single_step, "Context should detect single-stepping");
    }

    #[test]
    fn allocation_gpr_order_reserves_configured_memory_base_registers() {
        let plain = allocation_gpr_order(false, false);
        assert!(plain.contains(&HOST_R13));
        assert!(plain.contains(&HOST_R14));

        let page_table = allocation_gpr_order(true, false);
        assert!(page_table.contains(&HOST_R13));
        assert!(!page_table.contains(&HOST_R14));

        let fastmem = allocation_gpr_order(false, true);
        assert!(!fastmem.contains(&HOST_R13));
        assert!(fastmem.contains(&HOST_R14));

        let both = allocation_gpr_order(true, true);
        assert!(!both.contains(&HOST_R13));
        assert!(!both.contains(&HOST_R14));
    }
}

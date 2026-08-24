# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

rdynarmic is a Rust reimplementation of [dynarmic](https://github.com/MerryMage/dynarmic), an ARM64 dynamic recompiler. It targets the x86-64 backend and is used by [ruzu](https://github.com/vricosti/ruzu-emu) as its JIT CPU engine.

### Reference Projects

- **zuyu/externals/dynarmic** — C++ reference implementation (~8,500 lines of vector emit code across 3 files)
- **rxbyak** — Rust x86-64 runtime assembler (local crate dependency)

## Toolchain

```bash
cargo check --workspace
cargo test -p rdynarmic
cargo clippy -p rdynarmic
```

## Git

Never add `Co-Authored-By` or any reference to Claude in commit messages.

## Architecture

### IR Pipeline

```
ARM64 instruction → Frontend decoder → IR Block (SSA opcodes) → Optimization passes → x64 Backend emit
```

### Key Modules

```
src/halt_reason.rs          → HaltReason bitflags (Step, Svc, Breakpoint, ExternalHalt, etc.)
src/interface/A32/config.rs → A32 UserCallbacks + UserConfig
src/interface/A64/config.rs → A64 UserCallbacks + UserConfig
src/jit.rs                  → Public A32Jit/A64Jit wrappers and callback trampolines

src/ir/              → IR definition: Opcode enum (~650 opcodes), Inst, Block, Value, Type
  opcode.rs          → All IR opcodes (core, vector, FP scalar, FP vector, memory, A64)
  inst.rs            → Instruction struct with args, opcode, type
  block.rs           → Basic block with instruction list + terminal
  opt/               → Optimization passes: constant prop, DCE, identity removal, verification

src/frontend/        → ARM64 → IR translation

src/backend/x64/     → x86-64 JIT code emission
  a64_emit_x64.rs    → Block compilation pipeline, patch/unpatch, RSB/FastDispatch handlers
  block_of_code.rs   → Code buffer + dispatcher assembly (gen_run_code + step_code stubs)
  block_cache.rs     → HashMap<LocationDescriptor, CachedBlock> with invalidate_range
  patch_info.rs      → PatchInformation, PatchEntry, PatchType, patch slot constants
  emit.rs            → Main dispatcher: match on Opcode → emit function (~750 match arms)
  emit_terminal.rs   → Terminal → dispatcher jmp (or fallback ret for unit tests)
  emit_context.rs    → EmitContext with dispatcher offsets + block descriptors
  reg_alloc.rs       → Register allocator (GPR/XMM allocation, spilling, host_call)
  jit_state.rs       → A64JitState: guest registers, NZCV, fpsr_qc (accessed via R15)
  callback.rs        → Callback trait, SimpleCallback, ArgCallback for host calls
  abi.rs             → System V ABI: RDI/RSI/RDX/RCX params, RAX return
```

### Emit File Organization

| File | Opcodes | Pattern |
|------|---------|---------|
| `emit_a64.rs` | A64 context get/set, flags, barriers | Direct JIT state access via R15 |
| `emit_data_processing.rs` | ALU: add/sub/mul/div/shift/logic/extend | Native x86 instructions |
| `emit_memory.rs` | Memory read/write 8-128 bit | Host call to memory callbacks |
| `emit_floating_point.rs` | FP scalar arithmetic, conversions | SSE scalar + fallback |
| `emit_packed.rs` | 32-bit packed SIMD (A32 legacy) | SSE via GPR↔XMM + fallback |
| `emit_vector_*.rs` | 128-bit vector SIMD (10 files) | Native SSE or stack fallback |
| `emit_fp_vector*.rs` | FP vector ops (2 files) | Native SSE or stack fallback |

### Vector Emit Patterns (emit_vector_helpers.rs)

Two main patterns for 128-bit vector operations:

**Native SSE** (~130 opcodes): Direct SSE/SSE4.1/SSSE3 instructions
```
emit_vector_op(ra, inst_ref, inst, CodeAssembler::paddb)
// UseScratchXmm(arg0) + UseXmm(arg1) → op(dst, src) → DefineValue
```

**Stack fallback** (~220 opcodes): `extern "C"` Rust function called via stack
```
emit_two_arg_fallback(ra, inst_ref, inst, fallback_fn as usize)
// alloc_stack(48) → movaps args to stack → lea RDI/RSI/RDX → call → movaps result → release
```

Saturation variants OR the return value (QC flag in RAX) into `fpsr_qc` via R15.

### Execution Flow

```
A64Jit::run()
  → get_or_compile_block(pc)    // translate → optimize → emit → cache
  → run_code_fn(jit_state, code_ptr)
      → dispatcher prelude: push callee-save, alloc StackLayout, R15=jit_state
      → switch MXCSR to guest, jmp to compiled block
      → block executes, terminal jmps to return_from_run_code[N]
      → dispatcher checks halt_reason / cycles, either:
          - LookupBlock callback → jmp to next block (loop)
          - switch MXCSR to host, add_ticks, return HaltReason in EAX
```

### Public API (src/jit.rs)

```rust
use rdynarmic::interface::a64::config::UserConfig;
use rdynarmic::A64Jit;

let mut config = UserConfig::new(Box::new(my_callbacks));
config.enable_cycle_counting = true;
config.code_cache_size = 128 * 1024 * 1024;
let mut jit = A64Jit::new(config)?;

jit.set_pc(entry_point);
jit.set_sp(stack_top);
let reason = jit.run();   // returns HaltReason
```

### Dispatcher Convention

During JIT execution:
- **R15** = `*mut A64JitState` (callee-saved, stable across host calls)
- **RSP** = `*StackLayout` (cycles_remaining, cycles_to_run, spill slots, host MXCSR)
- MXCSR switched between host/guest at JIT entry/exit boundaries
- Terminals jump to `return_from_run_code[index]` (4 variants by MXCSR state × force return)
- LookupBlock callback compiles on cache miss, returns native code pointer in RAX

## Implementation Progress

### Completed Phases

- **Phase 1-9**: IR definition, frontend decoder, optimization passes, core x64 backend scaffolding
- **Phase 10**: ALU, memory, FP scalar, packed, crypto, CRC32, exclusive memory, saturation emit (~155 opcodes)
- **Phase 11**: All vector/SIMD opcodes (~350 opcodes across 10 new files + 2 FP vector files). Native SSE for common ops, stack-based Rust fallback for complex ops.
- **Phase 12**: Public JIT API, block cache, and execution loop. `A64Jit` struct with `run()`/`step()`, architecture-owned callback traits, `BlockCache`, `A64EmitX64` compilation pipeline, dispatcher assembly (`gen_run_code`), terminal-to-dispatcher wiring, and callback trampolines.
- **Phase 13**: Block linking, fast dispatch, atomic halt_reason, step mode. Atomic `xchg` for halt_reason read-and-clear, dedicated `step_code` entry with `lock or` STEP bit, direct block-to-block jump patching via fixed-size NOP-padded slots (`PatchTable`), RSB pop handler with descriptor matching, fast dispatch hash table (1M entries), `patch()`/`unpatch()` for cache invalidation. 175 tests pass, 0 clippy warnings.

### Current State

The JIT is feature-complete with performance optimizations: block linking eliminates dispatcher overhead for block-to-block transitions, RSB predicts return addresses, and the fast dispatch hash table accelerates indirect branches. The `step()` method uses a dedicated entry point with atomic STEP flag. The public `A64Jit` API is ready for integration with ruzu.

### Architecture additions (Phase 13)

```
src/backend/x64/patch_info.rs   → PatchInformation, PatchEntry, PatchType, slot size constants
```

#### Block Linking

LinkBlock/LinkBlockFast terminals emit fixed-size patch slots (23/22 bytes, NOP-padded). When a target block compiles later, `patch()` overwrites the jump displacement in-place via `asm.set_size()`. Cache invalidation calls `unpatch()` to revert slots to dispatcher fallback.

#### RSB / Fast Dispatch

- **PopRSBHint**: prelude handler computes location descriptor from PC+FPCR, decrements `rsb_ptr`, compares `rsb_location_descriptors[ptr]` — hit: `jmp [rsb_codeptrs+ptr*8]`, miss: fall to dispatcher.
- **FastDispatchHint**: prelude handler hashes descriptor, indexes 1M-entry table — hit: `jmp [entry+8]`, miss: store descriptor, fall to dispatcher.
- **PushRSB**: stores descriptor + patchable `mov rcx, imm64` code pointer at `rsb[ptr]`.
- Both bypass to dispatcher when `is_single_step` is true.

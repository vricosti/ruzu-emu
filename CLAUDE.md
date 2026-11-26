# CLAUDE.md — Porting Philosophy And Execution Contract

STRICT MODE ON

## Git Commit Policy (MANDATORY)

ALWAYS ask the user for explicit confirmation BEFORE running `git commit` (or
`git push`, `git merge`), in BOTH the parent `ruzu` repo AND the `rxbyak`
submodule. Never commit on your own initiative — present what would be
committed (files + proposed message) and wait for the user to approve. This
applies even when the work is complete and tests pass.

## Porting contract

The porting philosophy and execution contract live in [AGENTS.md](AGENTS.md) and
apply in full. Read it before porting or fixing anything:

@AGENTS.md

The sections below add what AGENTS.md does not cover.

---

## Integrated JIT Port and Companion Project: rdynarmic and rxbyak

The same porting philosophy applies to the companion projects:

- **rdynarmic** (`~/Dev/emulators/ruzu/src/rdynarmic/`) — Rust port of the Dynarmic ARM JIT compiler, integrated into the ruzu workspace
  - Upstream: `~/Dev/emulators/eden/src/dynarmic/`
  - Must follow the same file structure, method ownership, and behavioral parity rules as ruzu
  - IR opcodes, emitter methods, backend emit handlers, and register allocator must match upstream naming and semantics
  - Pseudo-operation handling (GetCarryFromOp, GetOverflowFromOp, GetNZCVFromOp) must use the same `GetAssociatedPseudoOperation` pattern as upstream — the producing instruction emits flag capture inline, not as a separate standalone handler
  - ARM32 instruction translation must produce identical results to upstream for all valid encodings (verified via differential fuzzing against an oracle built from `~/Dev/emulators/eden/src/dynarmic/`)

- **rxbyak** (`~/Dev/emulators/rxbyak/`) — Rust port of the Xbyak x86-64 assembler library
  - Upstream: the Xbyak 7 source resolved by Eden's `find_package(xbyak 7 CONFIG)` in `~/Dev/emulators/eden/src/dynarmic/CMakeLists.txt` (the local CPM checkout is under `~/Dev/emulators/eden/.cache/cpm/xbyak/`)
  - Instruction encoding must be bit-identical to upstream Xbyak
  - Method names should match upstream where possible (e.g., `mul`, `imul`, `div`, `idiv` for single-operand forms)

When fixing bugs in rdynarmic or rxbyak, always check the upstream C++ implementation first. If upstream handles something a specific way, rdynarmic/rxbyak must do the same — do not invent alternative approaches.

---

## Per-Folder Exceptions

Each section below lists C++ files that have no meaningful Rust counterpart for a specific upstream folder. These omissions are intentional and should not be flagged as missing files during parity audits.

### `audio_core` — `~/Dev/emulators/eden/src/audio_core`

**Rust crate:** `audio_core` at `~/Dev/emulators/ruzu/src/audio_core`

**Status:** Ported.

**Exceptions:**

- `precompiled_headers.h` — precompiled headers are a C++ build optimization; Rust has no equivalent concept.

### `common` — `~/Dev/emulators/eden/src/common`

**Rust crate:** `common` at `~/Dev/emulators/ruzu/src/common`

**Status:** Ported.

**Exceptions:**

- `precompiled_headers.h`, `common_precompiled_headers.h` — C++ build optimization; no Rust equivalent.
- `common_types.h` — defines `u8`, `u16`, `u32`, `u64`, `s8`, `s16`, `s32`, `s64`, `f32`, `f64`; Rust has these as built-in primitive types.
- `common_funcs.h` — C++ utility macros (`DECLARE_ENUM_FLAG_OPERATORS`, `ZUYU_NON_COPYABLE`, etc.); Rust handles these via `bitflags`, `Clone`/`Copy` derives, etc.
- `concepts.h` — C++ concepts; Rust uses trait bounds natively.
- `polyfill_ranges.h`, `polyfill_thread.h` — C++ standard library polyfills; Rust stdlib provides these features natively.
- `expected.h` — C++ `std::expected` polyfill; Rust has `Result` built in.
- `scope_exit.h` — Rust uses `Drop` for deterministic cleanup.
- `bit_cast.h` — Rust uses `bytemuck` or `transmute`.
- `make_unique_for_overwrite.h` — C++ memory allocation detail with no Rust equivalent.
- `parent_of_member.h` — C++ `container_of` macro pattern; Rust does not use this pattern.
- `microprofile.h`, `microprofile.cpp`, `microprofileui.h` — profiling framework integration; a separate concern.
- `stb.h`, `stb.cpp` — stb_image C library bindings; Rust would use an image crate instead.
- `scm_rev.h` — build-time source control revision; handled differently in Rust builds.
- `assert.h` — C++ macro-based assertion system; Rust has built-in `assert!`, `debug_assert!`, `panic!`.
- `atomic_helpers.h` — C++ TSAN annotation macros for atomic fences; Rust `std::sync::atomic` handles this natively.
- `atomic_ops.h` — MSVC/GCC specific compare-and-swap operations; Rust `std::sync::atomic` provides these.
- `demangle.h` — C++ symbol demangling; Rust has different name mangling and its own demangling support.
- `logging/formatter.h` — fmt library formatter specialization for enums; Rust `Display`/`Debug` traits handle this.
- `logging/log.h` — C++ fmt-based logging macros (`LOG_DEBUG`, `LOG_INFO`, etc.); Rust uses the `log` crate macros.
- `unique_function.h` — C++ move-only `std::function`; Rust has `Box<dyn FnOnce()>` and closures natively.
- `literals.h` — C++ user-defined literals (`_KiB`, `_MiB`, etc.); Rust uses `const` values or plain arithmetic.
- `android/android_common.*`, `android/applets/software_keyboard.*`, `android/id_cache.*` — Android JNI integration; platform-specific frontend code with no Rust JNI equivalent in this project.
- `fs/fs_android.*` — Android-specific filesystem via JNI; same as above.
- `linux/gamemode.*` — Linux Feral Interactive gamemode integration; thin platform glue.
- `signal_chain.*` — POSIX signal handler wrapping (Android-specific); platform-specific.
- `nvidia_flags.*` — Environment variable configuration for Nvidia driver quirks; thin platform glue.
- `windows/timer_resolution.*` — Windows-specific timer resolution API; platform-specific.
- `x64/xbyak_abi.h`, `x64/xbyak_util.h` — Xbyak JIT assembly library integration; JIT-specific, handled by `rdynarmic`.
- `reader_writer_queue.h` — third-party lock-free queue (`moodycamel::ReaderWriterQueue`); Rust would use a dedicated crate.
- `algorithm.h` — small generic iterator helpers (`BinaryFind`, `FoldRight`); Rust iterators provide this natively.
- `socket_types.h` — C++ network socket type aliases; Rust `std::net` handles this.

### `core` — `~/Dev/emulators/eden/src/core`

**Rust crate:** `core` at `~/Dev/emulators/ruzu/src/core`

**Status:** Ported.

**Exceptions:**

- `precompiled_headers.h` — C++ build optimization; no Rust equivalent.
- `memory.h/cpp` — ported as `guest_memory.rs` (upstream `Memory::Memory` class maps to `GuestMemory`); the top-level `memory.h` is a thin wrapper around page table access that is integrated into `guest_memory.rs`.
- `file_sys/fssystem_bucket_tree_template_impl.h`, `file_sys/fssystem_bucket_tree_utils.h` — C++ template implementation headers; logic folded into `fssystem/bucket_tree.rs`.
- `arm/nce/arm_nce.s` — ARM64 assembly source file; would need a separate `.s` file or inline assembly in Rust.
- `arm/dynarmic/` — JIT backend files are ported as stubs; full implementation depends on `rdynarmic` integration.
- `arm/nce/` — Native code execution backend is ported structurally; full implementation depends on platform-specific signal handling.

### `video_core` — `~/Dev/emulators/eden/src/video_core`

**Rust crate:** `video_core` at `~/Dev/emulators/ruzu/src/video_core`

**Status:** In progress.

**Exceptions:** *(to be filled as the port progresses)*

## Directories Excluded From The Port

The following upstream directories are **intentionally not ported** and should never be flagged as missing during parity audits.

### `yuzu` — `~/Dev/emulators/eden/src/yuzu`

**Not ported.** This is the Qt-based GUI frontend application. The ruzu project implements its own frontend using a different UI library (not Qt). This directory should not be ported.

### `tests` — `~/Dev/emulators/eden/src/tests`

**Not ported.** Upstream C++ test suite. Rust tests are written natively alongside each crate using `#[cfg(test)]` modules and `cargo test`.

---

## Testing ruzu-cmd

### Manual test with a Nintendo Switch title

```bash
cargo run --bin ruzu-cmd -- -g "/path/to/game.nsp"
```

With debug logging:

```bash
RUST_LOG=info cargo run --bin ruzu-cmd -- -g "/path/to/game.nsp"
```

With isolated cache and config (avoids polluting `~/.cache/ruzu` and `~/.config/ruzu`):

```bash
env XDG_CACHE_HOME=/tmp/ruzu-cache \
    XDG_CONFIG_HOME=/tmp/ruzu-config \
    RUST_LOG=info cargo run --bin ruzu-cmd -- -g "/path/to/game.nsp"
```

Ruzu owns its complete application-data tree. On Unix, overriding `XDG_DATA_HOME`
selects a separate Ruzu NAND beneath that location; on Windows the default root
is `%APPDATA%/ruzu`. Ruzu never borrows yuzu's NAND, keys, SDMC, or caches. The
only yuzu data read by the GTK frontend is a compatible INI selected for the
one-time configuration import, which is copied into Ruzu's own config directory.

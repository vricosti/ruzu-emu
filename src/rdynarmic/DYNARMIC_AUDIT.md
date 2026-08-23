# rdynarmic parity audit

Reference snapshot: Eden `master` at `5b86242313`, using
`src/dynarmic/src/dynarmic` as the read-only source of truth.

This audit distinguishes file presence, IR surface parity, and behavioral
parity. Matching names or passing tests do not establish behavioral parity.

## Structural baseline

- Eden has 385 C++ headers, sources, and opcode/include tables below its
  Dynarmic source root.
- rdynarmic has 236 Rust source files.
- rdynarmic contains x64 and in-progress arm64 backends. Eden additionally
  contains riscv64 and loongarch64 backends that have no Rust counterparts.
- Several upstream `.cpp`/`.h` owners are currently represented by broader
  Rust modules. File and method ownership therefore still require a
  per-directory audit; similar aggregate file counts do not imply parity.

## IR opcode baseline

Run:

```text
python3 tools/audit_dynarmic_opcodes.py /path/to/eden/src/dynarmic/src/dynarmic/ir/opcodes.inc
```

After the opcode naming, broadcast-element, reduction, and multi-result
multiply slices:

- Eden opcodes: 725
- rdynarmic opcodes: 725
- missing in rdynarmic: 0
- extra in rdynarmic: 0

The dead insertion-point and shuffle opcodes have been removed: Eden represents
insertion points as `IREmitter` state, and no Rust frontend produced the three
shuffle opcodes. The sixteen dedicated comparison opcodes have also been
replaced with Eden's exact `Greater`/`Equal`/`Or`/`Not` IR compositions. Ruzu's
environment-gated per-instruction A32 execution hook has also been removed from
the translator, IR, and both host backends. The opcode inventories now match
exactly; this proves IR surface parity only, not structural or behavioral parity.

The first slice restored Eden's exact names for 73 already-equivalent opcodes,
including `BitRotateRight*`, `PackedAbsDiffSumU8`, and the vector `S`/`U`
families. No encoding, metadata type, or dispatch ordering changed.

The second slice replaced the composite extract-plus-broadcast lowering with
Eden's seven dedicated `VectorBroadcastElement*` opcodes, including their exact
IR metadata, index validation, x64 AVX/SSE paths, and arm64 `DUP` paths. The x64
methods now live in the matching `backend/x64/emit_x64_vector.rs` owner.

The third slice replaced ADDV's expanded per-lane loop with Eden's four
dedicated `VectorReduceAdd*` opcodes. It ports the exact x64 SSSE3/SSE2 paths,
arm64 scalar `ADDV`/`ADDP` paths, frontend validation and result write order.

The fourth slice restored Eden's upper/lower pseudo-result ownership. The x64
`GetUpperFromOp` and `GetLowerFromOp` emitters now register results defined by
their producer instead of extracting unrelated 64-bit halves into GPRs.

The fifth slice ports the four `Vector{Signed,Unsigned}Multiply16/32`
multi-result producers and their exact x64 AVX/SSE instruction ordering. It
also removes four dead `*MultiplyLong*` operations that had no Eden equivalent
and had incompatible IR metadata. Eden marks the four producers unreachable
in its arm64 backend; rdynarmic now preserves that behavior explicitly.

The opcode audit now compares exact return/argument signatures in addition to
names, and rejects duplicate or missing Rust metadata entries. It exposed 126
shared-signature mismatches. The vector/CRC, A32 coprocessor, and A64 cache
slices have now removed all of them: all 725 shared names have Eden's exact
return/argument signature. Together with the extra-opcode review, this proves
exact opcode-name and metadata parity for all 725 operations, not behavioral
parity for their producers, optimization passes, or host emitters.

The A64 cache slice also ports the upstream callback-configuration pass. With
hooking disabled, all data-cache callback IR is invalidated and `DC ZVA` is
lowered to exact `DCZVA` writes using the configured block size. With hooking
enabled, x64 and arm64 forward the operation/value pair to the user callback.
`CTR_EL0` and `DCZID_EL0` are now configurable rather than backend constants.

The A32 translation slice now mirrors Eden's `TranslateCallbacks` boundary and
its separate ARM and Thumb loop owners. Both backends preserve the exact
`PreCodeReadHook` → aligned code read → `PreCodeTranslationHook` →
`GetTicksForCode` ordering, including early termination and custom cycle
counts. `TranslationOptions` now carries the configured architecture version,
unpredictable-behavior policy, and hint-hook policy. The architecture version
also reaches `A32IREmitter::ALUWritePC` and `LoadWritePC` at Eden's v7/v5
thresholds. The option audit additionally restored the missing `SEVL`, Thumb32
`PLD/PLDW`, and Thumb32 `PLI` decoder and exception paths.

Focused frontend, x64/arm64 emitter, CP15, IR-emitter, opcode-metadata, and A64
cache runtime tests pass. The A32 callback/options decoder and translation
tests also pass. The cache runtime and emitter tests pass natively on
x64 and under AArch64 QEMU; native Linux, Linux AArch64, and Windows x64 checks
pass. The complete
unit suite has a pre-existing x64 fastmem-test failure (`A32 fastmem path
requires fallback table`) reproduced at the parent commit in an isolated
worktree; differential oracle tests can also fail when the external Eden
oracle does not complete. These are validation blockers, not evidence against
the focused A32 coprocessor slice.

## Known behavioral gaps found during baseline

- `backend/x64/emit_data_processing.rs` still contains dynamic
  `ExtractRegister32` and `ExtractRegister64` `unimplemented!()` paths.
- `common/fp/process_exception.rs` logs floating-point exception raising as
  unimplemented rather than following Eden's exception-state behavior.
- The arm64 backend reports unimplemented vector-saturation opcodes.
- A32 and A64 still share one public `JitConfig` instead of matching Eden's
  separate `interface/A32/config.h` and `interface/A64/config.h` owners.
- Several A32 instruction families remain aggregated in broad Rust modules;
  notably the Thumb32 preload methods are not yet owned by a matching
  `thumb32_load_byte.rs` counterpart.

These are an inventory, not completion claims. Each item must be re-read in
its upstream-owned file and handled as a separate prerequisite-backed slice.

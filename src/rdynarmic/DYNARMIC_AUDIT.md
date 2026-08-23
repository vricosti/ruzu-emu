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

After the first naming-parity slice:

- Eden opcodes: 725
- rdynarmic opcodes: 736
- missing in rdynarmic: 15
- extra in rdynarmic: 26

The 15 missing opcodes are the seven `VectorBroadcastElement*` forms, four
`VectorReduceAdd*` forms, and four `Vector{Signed,Unsigned}Multiply*` forms.
The 26 extra opcodes include internal insertion-point and A32 execution-hook
operations, comparison/shuffle operations that Eden builds from other IR, and
four widening-multiply operations whose ownership differs from Eden's
multi-result multiply opcodes.

The first slice restored Eden's exact names for 73 already-equivalent opcodes,
including `BitRotateRight*`, `PackedAbsDiffSumU8`, and the vector `S`/`U`
families. No encoding, metadata type, or dispatch ordering changed.

## Known behavioral gaps found during baseline

- `backend/x64/emit_data_processing.rs` still contains dynamic
  `ExtractRegister32` and `ExtractRegister64` `unimplemented!()` paths.
- `common/fp/process_exception.rs` logs floating-point exception raising as
  unimplemented rather than following Eden's exception-state behavior.
- A32 non-VFP coprocessor translation/emission contains explicit no-op stubs.
- The arm64 backend reports unimplemented vector-saturation opcodes.

These are an inventory, not completion claims. Each item must be re-read in
its upstream-owned file and handled as a separate prerequisite-backed slice.

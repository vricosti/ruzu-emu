# Interrupted porting slice

## Active slice

Port the Eden IR operations `VectorSignedMultiply16`,
`VectorSignedMultiply32`, `VectorUnsignedMultiply16`, and
`VectorUnsignedMultiply32` from:

- `src/dynarmic/ir/opcodes.inc`
- `src/dynarmic/ir/ir_emitter.h`
- `src/dynarmic/backend/x64/emit_x64_vector.cpp`
- `src/dynarmic/backend/arm64/emit_arm64_vector.cpp`

## Missing prerequisite

Ruzu's `GetUpperFromOp` and `GetLowerFromOp` x64 emitters currently extract
64-bit halves from an ordinary `U128`. Eden instead registers both opcodes as
pseudo-operations whose complete `U128` results are defined by a multi-result
producer such as `VectorSignedMultiply16`.

Before resuming the vector-multiply slice:

1. expose the two pseudo-operation constructors in `ir/emitter.rs`;
2. make the x64 emitters register the pseudo-operations without generating an
   extraction;
3. mirror Eden's ARM64 assertion that the producer already defined the result;
4. add focused pseudo-operation linkage and backend tests;
5. re-read the matching Eden sources and record the verified differences in
   `DIFF.md`.

The interrupted vector-multiply implementation must resume only after this
prerequisite is committed and verified.

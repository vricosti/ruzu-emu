# Interrupted porting slice

## Active slice

Audit and align every shared IR opcode signature in
`src/rdynarmic/src/ir/opcode.rs` with Eden
`src/dynarmic/src/dynarmic/ir/opcodes.inc`.

The name inventory is already at 725/725 shared names, with 22 Ruzu-only
operations. The committed audit-tool work in this slice found 126 signature
mismatches among the 725 shared names. The vector and CRC corrections reduce
that inventory to three:

- `A32CoprocLoadWords` and `A32CoprocStoreWords` include an extra `U1`
  argument;
- `A64DataCacheOperationRaised` is missing Eden's third `U64` argument.

## Missing prerequisites

Changing metadata alone is forbidden because the last seven differences may
reflect divergent emitter, frontend, or backend contracts. Before resuming the
global metadata slice:

1. align A32 coprocessor load/store construction and dispatch;
2. align A64 data-cache-operation construction, lowering and dispatch;
3. verify and commit each prerequisite in its upstream-owned Rust files;
4. rerun the exact-signature audit, update `DIFF.md`, and remove this state
   file only after the shared mismatch count reaches zero.

## Completed prerequisites

- CRC emitters and all A32/A64 frontend/backend call sites were re-read. They
  already pass and consume Eden's raw `U32` operand; the divergent metadata and
  arm64 routing test inputs are corrected.
- `tools/audit_dynarmic_opcodes.py` now compares exact signatures and reports
  duplicate, missing, and unknown metadata entries.
- The binary, immediate-shift, and unary paired-widening vector metadata groups
  now match Eden. Focused Rust tests cover their boundaries, while the audit
  tool exhaustively compares all 725 shared operations.

# Interrupted porting slice

## Active slice

Audit and align every shared IR opcode signature in
`src/rdynarmic/src/ir/opcode.rs` with Eden
`src/dynarmic/src/dynarmic/ir/opcodes.inc`.

The name inventory is at 725/725 shared names, with 22 Ruzu-only operations.
The committed audit-tool work found 126 signature mismatches among the shared
names. The vector, CRC, and A32 coprocessor corrections reduce that inventory
to one:

- `A64DataCacheOperationRaised` is missing Eden's third `U64` argument.

## Missing prerequisites

Changing metadata alone is forbidden because the remaining difference reflects
divergent emitter, lowering, and callback contracts. Before resuming the global
metadata slice:

1. align A64 data-cache-operation construction, lowering, and dispatch;
2. verify and commit the prerequisite in its upstream-owned Rust files;
3. rerun the exact-signature audit, update `DIFF.md`, and remove this state
   file only after the shared mismatch count reaches zero.

## Newly discovered A64 cache-operation prerequisite

Eden's callback-config pass lowers `DC ZVA` to memory writes tagged with
`IR::AccType::DCZVA`. Rust's `ir/acc_type.rs` currently has no `Dczva` variant
and its variant inventory/order does not match Eden `ir/acc_type.h`.

Before the callback-config pass can be ported:

1. restore the exact 16-value `AccType` inventory and discriminant order;
2. rename the two active backend/frontend aliases to their Eden owners
   (`OrderedRw` and `Ifetch`);
3. add focused inventory/discriminant tests and verify all current users;
4. commit this prerequisite independently, then resume the cache-operation
   lowering.

## Completed prerequisites

- The upstream-owned A32 coprocessor interface exists under
  `src/interface/a32/`: `CoprocReg`, callback/action contracts, and the exact
  16-entry optional registry match Eden's interface owners.
- ARM and Thumb decode/translation now cover Eden's seven generic coprocessor
  forms. Metadata construction belongs to `A32IREmitter`, including CDP `CRd`,
  exact field bytes, zeroed reserved bytes, and the exact load/store signature.
- Both x64 and arm64 backends forward the registry and implement all compile-time
  coprocessor actions instead of hard-coded CP15 subsets.
- Core `DynarmicCP15` implements the interface directly; `ArmDynarmic32` owns it,
  installs slot 15 before JIT creation, and uses it for UPRW/URO and CNTPCT.
- CRC emitters and all A32/A64 frontend/backend call sites were re-read. They
  already pass and consume Eden's raw `U32` operand; the divergent metadata and
  arm64 routing test inputs are corrected.
- `tools/audit_dynarmic_opcodes.py` now compares exact signatures and reports
  duplicate, missing, and unknown metadata entries.
- The binary, immediate-shift, and unary paired-widening vector metadata groups
  now match Eden. Focused Rust tests cover their boundaries, while the audit
  tool exhaustively compares all 725 shared operations.

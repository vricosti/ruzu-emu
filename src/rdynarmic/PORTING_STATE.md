# Interrupted porting slice

## Active slice

Audit and align every shared IR opcode signature in
`src/rdynarmic/src/ir/opcode.rs` with Eden
`src/dynarmic/src/dynarmic/ir/opcodes.inc`.

The name inventory is at 725/725 shared names, with 22 Ruzu-only operations.
The committed audit-tool work found 126 signature mismatches among the shared
names. The vector, CRC, A32 coprocessor, and A64 cache-frontend corrections
reduce that inventory to zero. This signature result is not yet a behavioral
completion claim: A64 callback-config lowering and backend dispatch still
have to consume the corrected contract.

## Missing prerequisites

Before closing the global metadata slice:

1. port `A64CallbackConfigPass`, including exact `DC ZVA` writes and invalidation
   of every unhooked cache-operation instruction;
2. forward the hook/DCZID configuration and port x64/arm64 callback argument
   setup for data and instruction cache operations;
3. verify focused behavior and every configured host target;
4. update `DIFF.md` and remove this state file only after those behavioral
   prerequisites are committed.

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
- `ir/acc_type.rs` now has Eden's exact 16-value inventory and discriminant
  order, including `Dczva`; backend ordering checks use the matching aliases.
- A64 `sys_dc.rs` and `sys_ic.rs` now own all cache-maintenance visitors. The
  former hard-coded/NOP implementations are removed from `simd.rs`, and
  `A64IREmitter` emits Eden's exact typed operation IDs and location metadata.
- CRC emitters and all A32/A64 frontend/backend call sites were re-read. They
  already pass and consume Eden's raw `U32` operand; the divergent metadata and
  arm64 routing test inputs are corrected.
- `tools/audit_dynarmic_opcodes.py` now compares exact signatures and reports
  duplicate, missing, and unknown metadata entries.
- The binary, immediate-shift, and unary paired-widening vector metadata groups
  now match Eden. Focused Rust tests cover their boundaries, while the audit
  tool exhaustively compares all 725 shared operations.

# ARM64 code generator migration

## Active slice

Complete typed CodeGenerator use in the ARM64 backend without changing the x64
backend, scheduler or renderer. The original WIP commits are preserved by local
branch backup/rhazel-codegen-before-squash-20260912 and combined in 0ad0aec7.

## Prerequisite validation

Baseline `cargo test --release -p rhazel`: 21 passed, one failed in
`inst::tests::encodes_known_arm64_words` (SHRN). The narrowing immediate is now
verified against Oaknut and six independent Clang encodings. All 22 tests pass.
Associated-type operands enforce Oaknut's widening/narrowing pairs; two
compile-fail doctests pass.

The full rdynarmic suite initially aborted: A32/A64 core test fixtures returned
directly with RET without unwinding the prelude's stack frame. Both fixtures now
branch through the prelude epilogue. The normal-only trampoline fixture also
expected fourteen callbacks instead of the thirteen actually installed. These
tests and production prelude files were unchanged from origin/main. Full suite
verification now passes: 970 library tests, release, serial execution.

ABI migration prerequisite resolved: Q-register LDP/STP offset overloads now
support SP and non-SP bases, checked against Clang including signed boundaries.
ABI save/restore helpers now use typed CodeGenerator methods. rhazel has 24
passing unit tests and two compile-fail doctests. The release rdynarmic suite
passes after the ABI conversion: 971 library tests and three binary/integration
tests. The C++ differential oracle is absent; tests that skip when it is absent
must not be counted as successful differential comparisons.

## Remaining scope

- Complete typed mnemonics needed by scalar/vector/memory/A32 emitters and register
  allocation; keep JIT-specific logic in rdynarmic.
- Remove transitional generator wrappers only after all consumers are migrated.
- Independently validate encodings and run A32/A64 JIT tests, then release game
  smoke tests. No successful game validation has been performed in this pass yet.

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

## Integrated migration

Scalar FP, data processing, vectors, A32/memory, RegAlloc, prelude and AddressSpace
use typed mnemonics. All emitter interfaces take CodeGenerator; one generator is
borrowed at each address-space emission boundary. Per-function wrappers and
production raw instruction encoders are removed. Deferred emissions receive the
generator/context explicitly rather than retaining raw pointers to them.
Link/Relink patch generators preserve the append cursor and owner-managed I-cache
invalidation ranges. Unit tests retain raw encoders as expected-word references.

Final integrated verification: 973 rdynarmic library tests and four additional
binary/integration tests pass in release. rhazel passes 40 unit tests, 13
integration tests and two compile-fail doctests. Neither crate emits warnings.
The final release build produced ruzu, ruzu-cmd and ruzu.app. Freebrick reaches
its correctly displayed menu under Vulkan in a bounded smoke run, with continuing
GPU submissions. The standalone CLI requires LIBVULKAN_PATH pointing to the
bundle's MoltenVK on this machine; no installed library/configuration was changed.
This is a startup/menu smoke test, not exhaustive gameplay or a performance claim.

## Review corrections completed

The final review found and fixed a pre-existing packed-op discrepancy: MOVI V2.8B
broadcasts the immediate byte, whereas upstream MOVI D2, RepImm expands each
immediate bit into a byte mask. Both packed-operation call sites now use the
D-register overload. Independent words, all 256 masks and upstream scratch
sequences are covered by passing native tests. Patch-slot overflow is rejected
before writing; unsupported label/append access on patch cursors is rejected
before mutation. Patch tests cover both publication modes and cursor separation.

## Scope boundaries

This completes the backend's assembler API migration, not the entire Oaknut
instruction catalogue or every pre-existing Dynarmic optimization difference.
The C++ differential oracle is absent locally; optional oracle tests returning
early are not evidence of a successful C++ differential comparison.

The migration slice is complete. The two original WIP commits are squashed into
0ad0aec7; final assembler changes are committed in rhazel as e95f5f5. Commits are
local and have not been pushed.

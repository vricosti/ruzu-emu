STRICT MODE ON

<system_instructions>

<purpose>
This document defines how an LLM agent must port C++ code from `~/Dev/emulators/eden/src/` into `~/Dev/emulators/ruzu/src/`.

It is written as a binding execution contract. Deviation from these instructions is a failure state.

The goal is NOT:
- "make it compile"
- "make tests pass"
- "approximate the behavior"

The goal IS:
- strict file-structure parity where technically possible
- strict method ownership parity where technically possible
- strict constant placement parity where technically possible
- strict behavioral parity with the C++ source
- explicit, reviewable traceability from each Rust file/method/constant back to the upstream C++ file/method/constant

This document exists because earlier progress proved that a crate can be "substantial" while still drifting structurally from upstream in ways that make review and future porting harder.
</purpose>

<source_of_truth>
  <upstream>
    Path: `~/Dev/emulators/eden/src/`
    Access: READ-ONLY. Never edit.
    Authority: absolute for file layout, class/module boundaries, constants, method names and responsibilities, state ownership, control flow, validation behavior, data structure invariants.
  </upstream>

  <target>
    Path: `~/Dev/emulators/ruzu/src/`
    Judgement criterion: the Rust port is judged against the C++ tree, NOT against whether the Rust design looks idiomatic in isolation.
  </target>
</source_of_truth>

<core_philosophy>

  <principle name="fidelity_beats_elegance" priority="1">
    If there is tension between a cleaner Rust architecture and a more faithful C++ port, pick the faithful port.
    Rust idioms are allowed ONLY when they preserve the same ownership boundaries and behavior.

    <acceptable_adaptations>
      - `Result` instead of exceptions
      - `Arc<Mutex<T>>` instead of `shared_ptr` + mutex
      - `enum` instead of tagged unions / `std::variant`
      - `Drop` instead of destructors
    </acceptable_adaptations>

    <unacceptable_adaptations>
      - merging several upstream files into one large Rust file "for convenience"
      - moving constants away from their upstream module
      - centralizing behavior in a generic dispatcher when upstream behavior belongs to per-file modules
      - inventing a new architecture because the upstream one looks awkward in Rust
    </unacceptable_adaptations>
  </principle>

  <principle name="auditability_is_a_feature" priority="2">
    A good port is easy to compare line-by-line against upstream.
    When reviewing a Rust file, a maintainer MUST be able to answer:
    - which C++ file does this map to?
    - which upstream constants live here?
    - which upstream methods live here?
    - what is still missing?
    If the answer is unclear, the port is not structured well enough.
  </principle>

  <principle name="temporary_shortcuts_must_be_unwound" priority="3">
    Temporary shortcuts (stubs, simplified implementations, `todo!()` bodies) MUST NOT be used to continue an implementation when a missing prerequisite is discovered.
    If a required upstream dependency is missing, stop the current implementation slice, save the interrupted state to a project-local state file, implement the missing prerequisite first, then resume the interrupted slice.
    Do not leave known parity debt behind as a TODO in `DIFF.md`; either fix it before continuing or record the stopped state and work on the prerequisite.
  </principle>

  <principle name="tests_are_necessary_but_not_sufficient" priority="4">
    Passing tests only proves current exercised behavior works.
    It does NOT prove: structural parity, ownership parity, exact data-structure layout parity, exact lifecycle parity.
    NEVER conclude "finished" purely from green tests.
  </principle>

  <principle name="default_is_suspicion" priority="5">
    When something is "close enough", assume it is still wrong until verified against upstream.
    "Probably equivalent" is NOT good enough.
  </principle>

  <principle name="fix_small_upstream_divergences_when_found" priority="6">
    If an upstream divergence is discovered while investigating another issue, fix it immediately when the correction is small, local, and does not require a large structural refactor.
    Do NOT dismiss a confirmed divergence merely because it is not proven to be the current bug's root cause.
    If the correction would be large, structural, or risky for the current slice, document it explicitly and continue with the active investigation.
  </principle>

</core_philosophy>

<non_negotiable_rules>

  <rule id="A" name="preserve_file_structure">
    If upstream has a `.cpp` and `.h` pair, Rust MUST have a corresponding `.rs` file at the same relative path within its crate.
    Do NOT hide several upstream files behind one Rust file unless there is a strong technical reason.
    If you MUST diverge, document the reason in code comments AND in the final summary.
    Exceptions are listed in the exceptions section below.
  </rule>

  <rule id="B" name="preserve_method_ownership">
    If an upstream method belongs to a specific `.cpp` file, its Rust equivalent MUST live in the corresponding `.rs` file with Rust naming conventions (snake_case).
    Do NOT leave such logic in `lib.rs`, `mod.rs`, some unrelated shared helper, or a "utils" catch-all — unless upstream itself centralizes it.
  </rule>

  <rule id="C" name="preserve_constant_placement">
    If constants live in a specific upstream file, they belong in the corresponding Rust file.
    Do NOT centralize them into unrelated utility modules unless the upstream does the same.
  </rule>

  <rule id="D" name="preserve_behavior_before_abstraction">
    NEVER extract a helper unless:
    - the upstream has the same helper concept
    - or the extraction is purely mechanical and does not change ownership semantics
    Avoid "helpful" generic abstractions that make the port less traceable.
  </rule>

  <rule id="E" name="preserve_binary_layout">
    Anywhere structs are copied as raw bytes, ALL of these matter: field order, alignment, explicit padding, implicit padding, zero-initialization of reserved bytes.
    `repr(C)` is necessary but not sufficient.
    If a payload is serialized by raw memory copy, initialize the entire payload deterministically before writing fields.
  </rule>

  <rule id="F" name="preserve_signed_unsigned_bit_patterns">
    Upstream often forwards raw values through integer casts.
    Do NOT replace that with "friendlier" Rust logic (clamping negatives to zero, turning invalid values into `None`) unless upstream truly does that.
  </rule>

  <rule id="G" name="preserve_update_and_lifecycle_ordering">
    Many bugs come from doing the right work in the wrong order.
    When in doubt, port the ordering literally.
  </rule>

</non_negotiable_rules>

<post_implementation_verification>
  <mandatory>
    After EVERY implementation (file port, method port, subsystem port), the agent MUST:

    1. READ the upstream C++ source again (both .h and .cpp).
    2. COMPARE the Rust implementation against the upstream line-by-line.
    3. IDENTIFY all differences: structural, behavioral, ordering, naming, constant placement, binary layout.
    4. APPEND findings to `DIFF.md` at the project root except when fixing task from this file.

    DIFF.md format per entry:

    ## [date] — [rust_file_path] vs [cpp_file_path]

    ### Intentional differences
    - [difference]: [reason why this is acceptable in Rust]

    ### Unintentional differences (to fix)
    - [difference]: [what upstream does vs what Rust does]

    ### Missing items
    - [method/constant/struct not yet ported]

    ### Binary layout verification
    - [PASS/FAIL]: [details if relevant]

    This step is NOT optional. Skipping it is a failure state.
    DIFF.md is a living audit trail. It grows with each ported file.
    An implementation without a DIFF.md entry is considered unverified.
  </mandatory>
</post_implementation_verification>

<completion_criteria>
  Do NOT call a folder finished until ALL of the following are true:

  <criterion name="structural_completion">
    - The Rust module/file tree mirrors the upstream C++ folder as closely as reasonably possible.
    - Major missing files are identified and either ported or documented as exceptions.
    - No upstream files are silently merged into unrelated Rust files.
  </criterion>

  <criterion name="ownership_completion">
    - Methods and constants live in the same conceptual modules as upstream.
    - No "catch-all" utility files that absorb logic from multiple upstream sources.
  </criterion>

  <criterion name="behavioral_completion">
    - Critical subsystems follow upstream behavior closely enough that remaining differences are minor and identified.
  </criterion>

  <criterion name="binary_completion">
    - Data structure layouts match upstream where shared across components.
    - Serialized payloads are bit-accurate.
  </criterion>

  <criterion name="validation_completion">
    - Tests exist for nontrivial parity-sensitive behavior.
    - Full `cargo test -p <crate>` passes.
    - No known major "still structurally wrong" caveat remains.
  </criterion>

  <criterion name="diff_completion">
    - DIFF.md contains an entry for every ported file.
    - No unintentional differences remain unfixed or undocumented.
  </criterion>
</completion_criteria>

<workflow>
  For every upstream file, execute this process IN ORDER:

  <step number="1" name="identify_counterpart">
    Before writing code, answer:
    - What is the upstream file path?
    - Where is the Rust counterpart supposed to live?
    - Does that file already exist?
    - If it exists, is it the right owner for this logic?
    If the logic currently lives elsewhere, fix ownership first or during the change.
  </step>

  <step number="2" name="read_upstream">
    For each C++ file, inspect its header for interface and state, inspect its `.cpp` for behavior.
    Capture: constants, structs/classes, public methods, private helpers, static helpers, validation rules.
  </step>

  <step number="3" name="port_names">
    Convert to snake_case and Rust naming conventions ONLY.
    Do NOT rename at the semantic level.
  </step>

  <step number="4" name="preserve_helper_boundaries">
    If upstream uses several helpers inside a file, keep corresponding Rust helpers in the same file rather than flattening them into one large function.
  </step>

  <step number="5" name="implement">
    Write the Rust implementation following all non-negotiable rules.
    If implementation reveals a missing upstream prerequisite, STOP this slice immediately:
    - create/update a project-local state file describing the interrupted slice and exact missing prerequisite
    - implement the prerequisite in its upstream-owned Rust counterpart
    - verify the prerequisite against upstream and update DIFF.md
    - resume the interrupted slice only after the prerequisite is implemented
    Continuing with a known missing parity dependency is forbidden.
  </step>

  <step number="6" name="verify_against_upstream">
    MANDATORY: Re-read the upstream source. Compare line-by-line. Append to DIFF.md.
    This step CANNOT be skipped.
  </step>

  <step number="7" name="add_tests">
    Add focused regression tests for: the behavior being ported, upstream-specific edge cases, any binary-layout or ordering contract involved.
  </step>

  <step number="8" name="run_tests">
    Use focused module tests while iterating, then `cargo test -p <crate>` before concluding the pass.
  </step>
</workflow>

<anti_patterns>
  These are mistakes that already occurred or are easy to repeat. Triggering any of these is a failure.

  <anti_pattern id="1" name="flatten_without_splitting">
    "Flatten now, split later" without actually splitting later. Leads to giant central files, hidden ownership, difficult review.
  </anti_pattern>

  <anti_pattern id="2" name="dispatcher_as_owner">
    Treating dispatcher files as owners. Central dispatch files (`mod.rs`, `lib.rs`, processor files) should coordinate, NOT own every module's real logic.
  </anti_pattern>

  <anti_pattern id="3" name="central_constant_dump">
    If constants belong to a specific upstream file, keep them in the corresponding Rust file. Do NOT make one convenience constants file unless upstream does.
  </anti_pattern>

  <anti_pattern id="4" name="convenience_lifecycle">
    Replacing upstream lifecycle with a Rust convenience lifecycle (eager init where upstream is lazy, preemptive cleanup where upstream defers). If you diverge temporarily, mark it and come back.
  </anti_pattern>

  <anti_pattern id="5" name="tests_justify_divergence">
    Using "tests pass" to justify structural divergence. A structurally wrong port can still pass current tests.
  </anti_pattern>

  <anti_pattern id="6" name="undocumented_platform_replacement">
    Replacing platform-specific code with pure-Rust alternatives WITHOUT documenting the divergence.
  </anti_pattern>

  <anti_pattern id="7" name="skipping_diff_verification">
    Implementing code without verifying against upstream and appending to DIFF.md. Every implementation MUST be verified.
  </anti_pattern>
</anti_patterns>

<rust_idioms>
  Rust idioms are tools, not goals. Use them when they preserve parity.

  <preferred>
    - `enum` for tagged unions and mode flags
    - `Drop` for cleanup matching destructor behavior
    - `Option` for optional ownership
    - `Arc`, `Weak`, `Mutex` for shared lifecycle
    - module-level functions replacing C++ `static` helpers
    - `bitflags` crate for flag enums that use `DECLARE_ENUM_FLAG_OPERATORS` in C++
  </preferred>

  <use_carefully>
    - traits replacing inheritance
    - generic helpers replacing duplicated code
    - borrow-driven refactors that alter access patterns
  </use_carefully>

  <avoid>
    - architecture redesigns
    - replacing C++ data structures with different Rust ones that have different performance characteristics
    - helper layers that erase file ownership
  </avoid>
</rust_idioms>

<testing_philosophy>
  <required>
    Add focused tests for: parity-sensitive edge cases, previous bugs, binary-layout expectations, hash function output matching, data structure invariants.
  </required>

  <granularity>
    - unit tests near the module being ported
    - regression tests for specific upstream contracts
  </granularity>

  <avoid>
    - only broad integration tests
    - only "does not crash" tests
    - tests that assert Rust-specific behavior not derived from upstream
  </avoid>
</testing_philosophy>

<priority_order>
  When choosing the next task, prefer this order:
  1. structural mismatches that make review harder
  2. ownership mismatches that keep logic in the wrong file
  3. missing files that other crates depend on
  4. behavioral mismatches in critical infrastructure
  5. test coverage for newly fixed parity-sensitive behavior
  Do NOT spend time polishing low-value style details while major ownership or behavior mismatches remain.
</priority_order>

<progress_reporting>
  When reporting progress:
  - state exactly what ownership/parity slice was improved
  - name the affected files
  - say what is still missing
  - do NOT over-claim completion
  - confirm DIFF.md was updated

  <good_example>
    "Ported `fs/path_util.rs` from `fs/path_util.cpp`; all path resolution methods match upstream. DIFF.md updated. `cargo test -p common` passes."
  </good_example>

  <bad_example>
    "Improved the crate significantly."
  </bad_example>
</progress_reporting>


<final_standard>
  The agent MUST keep asking:
  - "Where does this logic belong upstream?"
  - "Why is this method not in the matching file?"
  - "Why is this constant not next to its upstream equivalent?"
  - "Is this behavior literally the same, or merely plausible?"
  - "Did I verify this against upstream and update DIFF.md?"
  That is the required mindset.
</final_standard>

</system_instructions>

---

<exceptions>

<folder name="audio_core" upstream="~/Dev/emulators/eden/src/audio_core" crate="audio_core" crate_path="~/Dev/emulators/ruzu/src/audio_core" status="ported">
  - `precompiled_headers.h` — C++ build optimization; no Rust equivalent.
</folder>

<folder name="common" upstream="~/Dev/emulators/eden/src/common" crate="common" crate_path="~/Dev/emulators/ruzu/src/common" status="ported">
  - `precompiled_headers.h`, `common_precompiled_headers.h` — C++ build optimization; no Rust equivalent.
  - `common_types.h` — defines `u8`, `u16`, `u32`, `u64`, `s8`, `s16`, `s32`, `s64`, `f32`, `f64`; Rust has these as built-in primitive types.
  - `common_funcs.h` — C++ utility macros; Rust handles via `bitflags`, derives, etc.
  - `concepts.h` — C++ concepts; Rust uses trait bounds natively.
  - `polyfill_ranges.h`, `polyfill_thread.h` — C++ stdlib polyfills; Rust stdlib provides these natively.
  - `expected.h` — C++ `std::expected` polyfill; Rust has `Result`.
  - `scope_exit.h` — Rust uses `Drop`.
  - `bit_cast.h` — Rust uses `bytemuck` or `transmute`.
  - `make_unique_for_overwrite.h` — C++ memory allocation detail; no Rust equivalent.
  - `parent_of_member.h` — C++ `container_of` macro; Rust does not use this pattern.
  - `microprofile.h`, `microprofile.cpp`, `microprofileui.h` — profiling framework; separate concern.
  - `stb.h`, `stb.cpp` — stb_image C library; Rust uses an image crate.
  - `scm_rev.h` — build-time SCM revision; handled differently in Rust.
  - `assert.h` — C++ assertion macros; Rust has `assert!`, `debug_assert!`, `panic!`.
  - `atomic_helpers.h` — TSAN annotation macros; Rust `std::sync::atomic` handles this.
  - `atomic_ops.h` — MSVC/GCC CAS operations; Rust `std::sync::atomic` provides these.
  - `demangle.h` — C++ symbol demangling; Rust has its own support.
  - `logging/formatter.h` — fmt formatter specialization; Rust `Display`/`Debug` traits handle this.
  - `logging/log.h` — fmt-based logging macros; Rust uses the `log` crate.
  - `unique_function.h` — C++ move-only `std::function`; Rust has `Box<dyn FnOnce()>`.
  - `literals.h` — C++ user-defined literals; Rust uses `const` values.
  - `android/android_common.*`, `android/applets/software_keyboard.*`, `android/id_cache.*` — Android JNI; not applicable.
  - `fs/fs_android.*` — Android-specific filesystem via JNI.
  - `linux/gamemode.*` — Linux Feral Interactive gamemode; thin platform glue.
  - `signal_chain.*` — POSIX signal handler wrapping (Android); platform-specific.
  - `nvidia_flags.*` — Nvidia driver env var config; thin platform glue.
  - `windows/timer_resolution.*` — Windows-specific timer API; platform-specific.
  - `x64/xbyak_abi.h`, `x64/xbyak_util.h` — Xbyak JIT library; handled by `rdynarmic`.
  - `reader_writer_queue.h` — third-party lock-free queue; Rust uses a dedicated crate.
  - `algorithm.h` — small iterator helpers; Rust iterators provide this natively.
  - `socket_types.h` — C++ socket type aliases; Rust `std::net` handles this.
</folder>

<folder name="core" upstream="~/Dev/emulators/eden/src/core" crate="core" crate_path="~/Dev/emulators/ruzu/src/core" status="ported">
  - `precompiled_headers.h` — C++ build optimization; no Rust equivalent.
  - `memory.h/cpp` — ported as `guest_memory.rs`; the top-level `memory.h` is a thin wrapper integrated into `guest_memory.rs`.
  - `file_sys/fssystem_bucket_tree_template_impl.h`, `file_sys/fssystem_bucket_tree_utils.h` — C++ template headers; logic folded into `fssystem/bucket_tree.rs`.
  - `arm/nce/arm_nce.s` — ARM64 assembly; needs separate `.s` file or inline asm.
  - `arm/dynarmic/` — JIT backend; ported as stubs pending `rdynarmic`.
  - `arm/nce/` — NCE backend; ported structurally pending platform signal handling.
</folder>

<folder name="video_core" upstream="~/Dev/emulators/eden/src/video_core" crate="video_core" crate_path="~/Dev/emulators/ruzu/src/video_core" status="in_progress">
  Exceptions to be filled as the port progresses.
</folder>

<excluded_directories>
  <directory name="yuzu" path="~/Dev/emulators/eden/src/yuzu">
    NOT ported. Qt-based GUI frontend. ruzu uses a different UI library.
  </directory>
  <directory name="tests" path="~/Dev/emulators/eden/src/tests">
    NOT ported. C++ test suite. Rust tests are written natively with `#[cfg(test)]` and `cargo test`.
  </directory>
</excluded_directories>

</exceptions>

<testing_commands>

Manual test with a Nintendo Switch title:

```bash
cargo run --bin ruzu-cmd -- -g "/path/to/game.nsp"
```

With debug logging:
```bash
RUST_LOG=info cargo run --bin ruzu-cmd -- -g "/path/to/game.nsp"
```

With isolated cache and config directories:
```bash
env XDG_CACHE_HOME=/tmp/ruzu-cache \
    XDG_CONFIG_HOME=/tmp/ruzu-config \
    RUST_LOG=info cargo run --bin ruzu-cmd -- -g "/path/to/game.nsp"
```

</testing_commands>


### Runtime hardware profile extension (2026-11-08)

Switch 1/NX2 session selection is owned by System/KernelCore and propagated to process/thread/JIT owners; never use a global profile. NUM_CPU_CORES retains Eden's Switch 1 value (4); MAX_NUM_CPU_CORES (6) is host storage capacity only. Active behavior reads the session profile. Preemption priorities remain in global_scheduler_context.rs; affinity-mask derivation remains in k_affinity_mask.rs. Loader metadata preflight is a documented multi-console lifecycle extension: it uses the effective patched ExeFS before kernel construction, then preserves kernel-before-process initialization. NX2 priorities [59,59,59,63,59,59], virtual map and four watchpoints preserve current experimental assumptions, not verified hardware claims.

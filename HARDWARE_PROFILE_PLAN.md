# Plan: Runtime HardwareProfile (Switch 1 / NX2) — v2, for review

Status: IMPLEMENTED on `feat/session-hardware-profile` (2026-11-08).
The v2 design below is retained for traceability; implementation decisions
and validation results are recorded in section 9.

## 1. Problem statement

To support Switch 2 (NX2) titles, ruzu globally changed kernel hardware
constants that upstream Eden defines for Switch 1:

| Constant | Upstream Eden (oracle) | ruzu today | File |
|---|---|---|---|
| `NUM_CPU_CORES` | `4` (`eden/src/core/hardware_properties.h:18`) | `6` | `src/core/src/hardware_properties.rs:14` |
| `PREEMPTION_PRIORITIES` | `{59, 59, 59, 63}` | `[59, 59, 59, 63, 59, 59]` | `src/core/src/hle/kernel/global_scheduler_context.rs:112` |
| `VIRTUAL_TO_PHYSICAL_CORE_MAP` | virtual 4..62 fold to physical 0, 63 -> 3 | virtual 4 -> 4, 5 -> 5, 6..62 -> 0, 63 -> 3 | `src/core/src/hardware_properties.rs:21` |
| `ALLOWED_AFFINITY_MASK` (derived) | `0xF` | `0x3F` | `src/core/src/hle/kernel/k_affinity_mask.rs:11` |

This is a ruzu-local divergence (marked `eden-nx2` in
`src/rdynarmic/src/interface/exclusive_monitor.rs:17`). It does NOT prevent
Switch 1 titles from running — thread placement is confined by the per-process
NPDM `CorePriority` capability mask (`k_capabilities.rs:316`, validated in
`svc_thread.rs:212` and `:699`) — but it changes behavior observable by or
affecting Switch 1 titles, in decreasing order of impact:

1. **Guest timing.** `DynarmicCallbacks::AddTicks` amortizes JIT ticks by
   dividing by `NUM_CPU_CORES` (`arm_dynarmic_64.rs:2210`,
   `arm_dynarmic_32.rs:2351` — both comments still say "(4)"). The cycle-counting path uses ticks/6 instead of upstream's ticks/4.
   Wall-clock callbacks bypass AddTicks, so no universal performance or
   guest-speed impact is claimed.
2. **SVC validation observables** (verified against
   `eden/src/core/hle/kernel/svc/svc_thread.cpp`):
   - `CreateThread(core_id=4)`: NO observable difference. Eden returns
     `ResultInvalidCoreId` from `IsValidVirtualCoreId` (`:41`); ruzu passes
     that check (6 cores) but the process-mask check (`:42`) returns the same
     `ResultInvalidCoreId`. Eden does not fold SVC core-id arguments.
   - `SetThreadCoreMask`: the process mask is checked FIRST (`:260-261`), so
     out-of-process-mask affinities behave identically. The divergence is
     narrow: `core_id=4` with an affinity valid for the process returns
     `ResultInvalidCoreId` on 4 cores (else-branch `:267-269`) but
     `ResultInvalidCombination` on 6 cores (bit test `:266`).
   - `fold_extra_core_mask` / the core-id fold helper
     (`svc_thread.rs:20-36`) do not exist in Eden's svc_thread.cpp at all;
     they are NX2-local additions and must be gated on the profile.
3. **Virtual-core folding** (internal, not SVC arguments): the
   `VIRTUAL_TO_PHYSICAL_CORE_MAP` used by `k_capabilities.rs:179,355` and
   `k_thread.rs:943,3594,3598` folds virtual cores 4-5 to physical 4-5
   instead of hardware's physical 0.
4. **Scheduler shape.** The 10 ms preemption tick rotates 6 queues instead of
   4, and 6 host core threads are spawned. Not guest-visible for cores 0-3,
   but a permanent host cost and a structural divergence from upstream.
5. **Kernel resource sizing.** Slab counts scale with core count
   (`init_slab_setup.rs:25`); slab counts participate in kernel resource
   dimensioning, so this is a parity concern, not just array capacity.

## 2. Goal

One binary that emulates both consoles, with a **runtime `HardwareProfile`**
selected per emulation session:

- **Switch 1 profile**: all values above bit-identical to upstream Eden,
  including tick amortization (/4), validation error codes/ordering, folding,
  and slab counts. This *removes* the current divergence for Switch 1 titles.
- **NX2 profile**: current 6-core behavior, unchanged.
- Sessions of different profiles must work sequentially **in the same
  process** (frontends reload sessions in-process).

## 3. Non-goals

- No change to NX2 GPU paths (`nx2.rs`, Ampere, `nx2_gsv.rs`, video decoder).
  The late GPU-side NX2 flip (`nvhost_ctrl_gpu.rs:281` ->
  `set_nx2_ampere(true)`) stays as-is; it gates GPU behavior only.
- No change to rdynarmic beyond constructing the exclusive monitor with the
  active core count (its API already takes a runtime `processor_count`,
  `exclusive_monitor.rs:34`).
- No invention of new NX2 hardware values. The NX2 preemption priorities
  `[59,59,59,63,59,59]` and `NUM_WATCHPOINTS = 4` stay as currently guessed,
  explicitly documented as assumptions pending a public Switch 2 oracle.

## 4. Design

### 4.1 The profile type and its ownership

A plain immutable struct, owned by `System`/`KernelCore` and shared by
reference (or cheap copy) with their components. **No process-wide global, no
`OnceLock`** — a global would break sequential Switch 1 -> NX2 sessions in
one process and make kernel tests order-dependent.

Constant ownership follows CLAUDE.md Rule C — each per-profile value is
defined in the file that owns its upstream counterpart; the profile selects,
it does not own:

- `src/core/src/hardware_properties.rs`: `HardwareProfile` struct,
  `SWITCH1_PROFILE` / `NX2_PROFILE` consts, the two
  `VIRTUAL_TO_PHYSICAL_CORE_MAP` variants, `num_cpu_cores`,
  the console discriminant. Affinity mask derivation stays in `k_affinity_mask.rs`.
- `src/core/src/hle/kernel/global_scheduler_context.rs`: BOTH preemption
  priority arrays (`[59,59,59,63]` and `[59,59,59,63,59,59]`) stay here;
  the profile carries a discriminant (or `&'static [u32]` pointing at these
  arrays, defined here) — the values never move out of this file.

Capacity is renamed **explicitly**: `MAX_NUM_CPU_CORES: usize = 6` (matching
the existing rdynarmic name) is introduced for static array sizing, instead
of silently redefining `NUM_CPU_CORES`'s semantics. `NUM_CPU_CORES` as a
behavioral constant disappears from call sites in favor of
`profile.num_cpu_cores`; remaining array-size uses switch to
`MAX_NUM_CPU_CORES`. This naming divergence from upstream (which has only
`NUM_CPU_CORES = 4`) is intentional and documented: upstream has no
multi-console concept to mirror.

### 4.2 Profile selection

Detection already exists: `ProgramMetadata` recognizes the Switch 2 ACID
layout (`program_metadata.rs:251-275`) and exposes `is_switch2()` (`:403`),
with tests. The profile is derived from it — no inference from core masks.

The obstacle is boot order: `load_with_parameters` runs
`initialize_kernel()` (Phase 1) before opening the game file
(`core.rs:1684-1699`), and this phase order (InitializeKernel ->
CreateApplicationProcess) mirrors upstream `System::Load`. We therefore do
NOT reorder the phases. Instead, a **metadata pre-pass**: before Phase 1,
open the file and loader just far enough to read `ProgramMetadata` and call
`is_switch2()`, select the profile, then run the upstream-shaped phases
unchanged. Investigation task I1: confirm every loader path (NSP/XCI/NRO/…)
can surface `ProgramMetadata` cheaply before kernel init; fallback is an
explicit user setting, default Switch 1.

### 4.3 Capacity vs. behavior classification

Statically sized per-core arrays stay at capacity `MAX_NUM_CPU_CORES = 6`;
slots `>= profile.num_cpu_cores` are initialized but never touched:

- `GUEST_PC` / `GUEST_LR` / `GUEST_SP` and per-core atomics (`kernel.rs:304-334`)
- rasterizer read/write areas, SMMU scratch buffers (`memory/memory.rs:141-149`)
- `cpu_manager.rs` per-core arrays (`:50`, `:276-278`, `:783-801`, `:865`, `:1887-1889`)

All **behavior** switches to the profile:

| Site | Change |
|---|---|
| `arm_dynarmic_64.rs:2210`, `arm_dynarmic_32.rs:2351` | tick amortization divides by `profile.num_cpu_cores` (fix stale "(4)" comments) |
| `k_affinity_mask.rs:11,33,53` | mask from profile (`set_all()`, assert); see §5 for exact semantics |
| `global_scheduler_context.rs:112,463-464` | preemption loop `0..profile.num_cpu_cores`, priority array selected by profile (arrays stay in this file) |
| `kernel.rs` core-thread spawn, suspend/resume, shutdown loops (`:623`, `:849`, …) | iterate `0..profile.num_cpu_cores` |
| `cpu_manager.rs:1971,1998` core rotation (`% NUM_CPU_CORES`) | modulo `profile.num_cpu_cores` |
| `k_scheduler.rs`, `physical_core.rs`, `core.rs` per-core loops | active count |
| `svc_thread.rs` validation + `fold_extra_core_mask`/core-id fold (`:20-36`) | validate against profile; NX2-only folds gated on profile (absent in Switch 1 mode, as in Eden) |
| `svc_dispatch.rs`, `svc_process_memory.rs` core checks | profile |
| `k_thread.rs:943,3594,3598`, `k_capabilities.rs:179,355` | virtual->physical folding via the profile's map variant |
| `memory/memory.rs:1269,1306,1516` "last core = system core" | `profile.num_cpu_cores - 1` |
| `init_slab_setup.rs:25` slab counts | **profile value** (4 under Switch 1) — slab counts size kernel resources, parity matters |
| exclusive monitor construction (`arm_dynarmic_64.rs` / `_32.rs` call sites) | `profile.num_cpu_cores` |

Full inventory to re-verify during implementation: 22 files reference
`NUM_CPU_CORES` today; each use is classified **capacity** (array sizing ->
`MAX_NUM_CPU_CORES`) or **behavior** (-> profile). Audit task A1: grep
`src/core/` for bare literals (`6`, `5`, `0x3F`) encoding core-count
assumptions that don't name the constant.

### 4.4 Error-code parity

Verified against `eden/src/core/hle/kernel/svc/svc_thread.cpp` (see §1.2 for
the recorded semantics). During implementation, re-read each remaining SVC
touching cores (`GetThreadCoreMask`, `GetCurrentProcessorNumber`, activity
SVCs) line by line against Eden before porting its validation — per the
CLAUDE.md zero-assumption rule, nothing below is assumed from names.

## 5. Test plan

New regression tests (in the owning modules, `#[cfg(test)]`; kernel-level
tests pin an explicit profile on their fixture — no global state):

1. `hardware_properties.rs` — Switch 1 map: virtual 4..=62 fold to physical
   0, virtual 63 -> 3; `convert_virtual_core_mask_to_physical(0x3F) == 0xF`
   (bits 4-5 fold onto already-set bit 0). NX2 map keeps the current
   assertions (`:57-64`), re-expressed as NX2-profile tests.
2. `k_affinity_mask.rs` — `new()` stays zero under both profiles;
   `set_all()` yields `0xF` under Switch 1 and `0x3F` under NX2.
3. `svc_thread.rs` —
   - `CreateThread(core_id=4)`: `InvalidCoreId` under BOTH profiles when the
     process mask excludes core 4 (documents the non-divergence);
   - `SetThreadCoreMask(core_id=4, valid affinity)`: `InvalidCoreId` under
     Switch 1, `InvalidCombination` under NX2 (the narrow real divergence);
   - NX2 fold helpers inert under the Switch 1 profile.
4. `global_scheduler_context.rs` — preemption rotates exactly
   `num_cpu_cores` queues per profile; priority arrays match §1's table.
5. `arm_dynarmic_*` — `add_ticks` divides by 4 under Switch 1, 6 under NX2.
6. `init_slab_setup.rs` — slab counts follow the profile.
7. Session-level: boot a Switch 1 profile session, tear down, boot an NX2
   profile session **in the same process**; both report their expected core
   counts and masks.

Validation gates: `cargo test -p core`; manual boot of one Switch 1 title
(profile must resolve to Switch 1 via `is_switch2() == false`) and one NX2
title. Caveat: a known pre-existing crash in the core test suite is reported;
confirm and record it BEFORE starting so "full suite green" is measured
against the same baseline, not blamed on this change.

## 6. Sequencing

1. **I1** — loader pre-pass investigation: confirm `ProgramMetadata` is
   readable before `initialize_kernel()` on every loader path; record the
   result in this file. Also record the current core-suite baseline
   (pre-existing crash, §5 caveat) and run audit A1 (bare literals).
2. Introduce `MAX_NUM_CPU_CORES` + `HardwareProfile` + both profiles, owned
   by `System`/`KernelCore`, defaulting to NX2 values so behavior is
   unchanged while no site reads the profile. `cargo test -p core`.
3. Convert guest-observable behavior: `add_ticks` (biggest impact), SVC
   validation + NX2 fold gating, virtual-core folding, affinity masks —
   with upstream verification + tests 1-3 and 5.
4. Convert structural behavior: scheduler loops, kernel core-thread spawn,
   cpu_manager rotations, system-core indices, slab counts,
   exclusive-monitor count — tests 4, 6.
5. Wire profile selection from the metadata pre-pass (`is_switch2()`), keep a
   user override setting; test 7 (sequential sessions in one process).
6. Update CLAUDE.md per-folder exceptions: document `MAX_NUM_CPU_CORES`,
   the profile mechanism as the sanctioned NX2 extension point, and the NX2
   values that remain undocumented assumptions (§3).

Each step compiles and passes `cargo test -p core` independently; steps 3-5
are separately revertible.

## 7. Risks

- **Boot-order**: any kernel code running before profile selection would use
  the default. Mitigation: profile chosen in the metadata pre-pass before
  `initialize_kernel()`; `debug_assert!` in `KernelCore` init that a profile
  was explicitly provided.
- **Timing regressions**: fixing `add_ticks` to /4 changes effective guest
  speed for every Switch 1 title relative to current ruzu builds. Expected
  and correct (restores upstream), but flag it in release notes and compare
  against Eden, not against previous ruzu behavior.
- **Loader pre-pass cost/feasibility**: some container formats may require
  nontrivial work (decryption) to reach the NPDM before kernel init. I1
  de-risks this; the user-setting fallback bounds the damage.
- **Hidden 6-core assumptions**: literals not naming the constant (audit A1).
- **Known core-suite crash**: pre-existing, to be confirmed and recorded in
  step 1; it caps the "full suite green" acceptance criterion until fixed or
  quarantined.

## 8. Resolved and open questions

- **Q1 (resolved — review)**: profile owned by `System`/`KernelCore`, not a
  global. Testability and sequential multi-profile sessions decide it.
- **Q2 (resolved — review)**: slab counts take the profile value; they size
  kernel resources and are a parity concern.
- **Q3 — `NUM_WATCHPOINTS`**: stays 4 under both profiles for now; the
  A78C value is unknown publicly. Documented assumption, out of scope.
- **Q4 — NX2 preemption priorities / system core**: `[59,59,59,63,59,59]`
  stays as the documented assumption; no public Switch 2 oracle. Do not
  invent new values.


## 9. Implementation decisions and I1 results (2026-11-08)

- Branch: `feat/session-hardware-profile`, based on NX2 checkpoint `cf6b47b`.
- `HardwareProfile` is a copyable enum, default Switch 1. Constants remain in
  their upstream-owned modules. `NUM_CPU_CORES=4` remains the Switch 1 reference;
  storage reserves `MAX_NUM_CPU_CORES=6`. No global profile was introduced.
- NSP/XCI/NAX delegate metadata preflight to their already-selected child NCA
  loader. NCA retains its existing ExeFS/update fallback. The directory loader
  prepares base NPDM -> PatchExeFS -> effective NPDM, caches that directory and
  metadata, and Load borrows the snapshot, retaining it for failed-load retries. Missing/bad metadata propagates an
  error. Standalone NRO/NSO/KIP formats have no NPDM and default to Switch 1.
- Construction prerequisite discovered before kernel init: System::initialize
  used to create KernelCore and start CoreTiming. Those session-owned operations
  now occur after preflight in load_with_parameters. Capacity-only memory caches
  can still exist earlier. CoreTiming starts before kernel initialization;
  process construction and Load remain after kernel initialization.
- CLI override: `--hardware-profile auto|switch1|nx2`; the core also exposes an
  override setter that rejects changes while an application process exists.
- Found and removed a duplicate placeholder KAffinityMask in k_thread.rs;
  threads now use the upstream-owned k_affinity_mask.rs implementation.
- Added explicit profiles for service-process and kernel-thread constructors;
  user threads inherit their owning process's profile. ARM constructors receive
  the profile explicitly instead of the previously unused Any argument.
- Small upstream divergence corrected: InitializeInterfaces forwards the session
  multicore flag as uses_wall_clock instead of hardcoding true for both backends.
- Metadata Reload now clears a previous NX2 layout flag before detecting the new
  effective layout. This deliberately corrects the sticky flag in Eden-NX2.
- Sequential sessions: CpuManager clears worker/barrier/rotation state on shutdown;
  host thread identity and phantom state are reset; the session CoreTiming worker
  is joined before kernel destruction; GPU attachment resets the existing NX2 flag.
- Tests cover both profile maps, SVC validation ordering, affinity limits, slabs,
  ARM32/ARM64 tick division/minimum/wall-clock bypass, metadata errors/caching and
  real kernel initialization/shutdown Switch 1 -> NX2 -> Switch 1.
- Baseline full core suite reproduced STATUS_ACCESS_VIOLATION at
  wait_cancel_pushes_runnable_thread_through_state_change_handler before changes.
  Final full-suite result and manual title checks will be recorded below.

### Final hardware-profile validation (2026-11-08)

- `cargo test -p core hardware_profile -- --test-threads=1`: 13 passed, including actual NX2 CreateThread on cores 4/5 and SetThreadCoreMask preserving both physical bits. SVC owner/header and Eden IsValidVirtualCoreId/SetThreadCoreMask re-read; no guest ABI changed.
- Separate groups: loader 13 passed; capabilities 7 passed; SVC thread group 6 passed / 3 existing ignored before adding the extra passing NX2 test; memory layout lifecycle and ARM-interface finalization each passed.
- Existing fixtures now construct a session kernel after System initialization, specify profile capabilities and populate only active ARM slots. No production behavior weakened to satisfy tests.
- Full `cargo test -p core -- --test-threads=1` aborts with STATUS_ACCESS_VIOLATION at k_thread::wait_cancel_pushes_runnable_thread_through_state_change_handler, reproduced on the unchanged baseline. Final run has 31 FAILED tests before that crash, all present on baseline; the baseline single-core rotation failure now passes. Skipping the crash reaches another aggregate SVC access violation; the isolated SVC group passes. Full-suite success is NOT claimed.
- Real release Vulkan boots in automatic mode: Mario Kart 8 Deluxe base reaches title screen (Switch1, 4 CPU workers); Mario Kart World reaches title screen (Nx2, 6 CPU workers). This verifies startup/rendering, not gameplay/performance equivalence. Local captures/logs: TEMP/ruzu-profile-switch1-20261108-110240 and TEMP/ruzu-profile-nx2-20261108-110322.
- Tetris 99 v2.6.0.nsp is rejected during container preflight with ErrorNotImplemented; no silent metadata fallback. MK8 base supplied the positive Switch 1 check.
- No temporary runtime instrumentation added. Scripts/logs/captures remain outside tracked sources. Both test runtimes stopped after identity verification.

### Review follow-up

Six findings confirmed; the seventh (obsolete metadata comment) was already fixed in the reviewed working tree. Corrected ServiceContext and lazy dummy profile inheritance, persistent host TLS session invalidation, cached metadata retention on retry, same-file error mapping duplication, and shutdown before kernel replacement. Load/setup error paths now clean up immediately as upstream does. Added service-process, persistent-worker, loader-retry and real System failed-load retry regressions; validation recorded in DIFF.md.

Review validation: 17 hardware-profile tests passed after immediate Load/setup failure cleanup. ServiceContext tests: 7 passed; host_thread: 6; host_identity: 2; persistent_host: 2; phantom_mode: 1; loader group: 14. These filters overlap; counts are not an aggregate unique-test total. Full core rerun repeats the same baseline k_thread access violation with 31 FAILED tests before it, all already present on baseline. git diff --check passes. Release rebuild completion recorded below.
Final review build: cargo build --release --bin ruzu-cmd passed (exit 0). Earlier two-title manual boot evidence predates these review fixes; it is not a post-review gameplay validation.

### Automatic memory extension (2026-11-08)

User requested console-wide memory sizing after the 4 GB GUI failure. Board-owned policy now selects Memory12Gb for all NX2 sessions; Switch1 keeps configured memory. KernelCore snapshots the choice; DeviceMemory/Memory are allocated after metadata preflight and board pool setup receives the same explicit layout. No global settings change or game-ID exception. JIT sign-extension bounds use actual backing size.
Physical RAM source: https://www.ifixit.com/Device/Nintendo_Switch_2 (12 GB LPDDR5X). Existing HLE mode still provisions 8 GiB extra graphics-server backing and the inherited 9809 MiB app pool; this change does not claim retail memory-map fidelity. Tests cover Switch1 -> NX2 -> Switch1 with the global 4 GB setting unchanged. Temporary MKW config override will be removed after successful automatic boot.

GPU follow-up: Host1x already passes the session DeviceMemory. MaxwellDeviceMemoryManager now sizes its reverse table from that backing (same 32/35-bit capacities), not the unchanged global setting. The missed consumer caused first-command GPU stalls with automatic 12 GB selection and global 4 GB. Focused GPU manager suite: 52 passed. Per-owner parity audit appended to DIFF.md.

Automatic-memory runtime validation (2026-11-08): release GUI+CLI build passed. Mario Kart World reached the title screen with configured Memory4Gb unchanged, automatic Nx2/6 cores + Memory12Gb session, and GPU reverse table using session backing. Evidence TEMP/ruzu-profile-4gb-nx2-20261108-122410/automatic-nx2-title.png and ruzu_log.txt. Stopped verified CLI PID 25592 before next Switch1 launch. Removed only the temporary memory override from APPDATA custom/0400C3F00006E000.ini (use_global=true); other per-title settings preserved. Switch1 smoke in progress.

Automatic memory validation completed for both titles: Mario Kart 8 Deluxe also reached title with Switch1/4 cores and Memory4Gb (TEMP/ruzu-profile-4gb-switch1-20261108-122636/automatic-switch1-title.png, ruzu_log.txt). These are independent CLI processes; they verify automatic memory selection and rendering, not a same-GUI-process success cycle or gameplay performance. Failed-load session transitions are covered separately by the 18 passing hardware_profile tests. Both smoke runtimes stopped. Full core suite retains the recorded baseline access violation and 31 baseline failures; no full core pass claimed. Final release GUI+CLI build passed in 2m15s, diff --check passed.

Final GPU validation: full video_core unit-test executable (built by cargo test -p video_core) completed successfully: 1726 passed, 0 failed, 1 ignored, 460.42s; TEMP/nx2-auto-gpu-full.log. Focused GPU manager tests: 52 passed. Automatic memory extension and prerequisite are complete within documented core-suite and retail-layout limitations. No temporary instrumentation introduced, no commit or push performed.

### Console-wide NX2 ASTC policy (2026-11-08)

The GUI crash after automatic RAM selection was Vulkan ERROR_OUT_OF_DEVICE_MEMORY at roughly 6.8 GiB reported device usage. Earlier CLI smoke configs used BC3; the GUI global default remained Uncompressed. User explicitly rejects a Mario Kart World exception. All NX2 sessions now resolve configured Uncompressed to BC3 through a runtime-only Settings accessor; configured BC1/BC3 remain selected, and Switch1 retains configured behavior. System activates policy before GPU creation and clears it after GPU destruction. Shared staging sizes, conversion, surface accounting, Vulkan formats/flags and OpenGL formats/flags all consume the effective value. No title ID branch or persisted ASTC override. Native ASTC capability paths remain unchanged. BC3 is lossy recompression and a VRAM pressure mitigation, not general out-of-memory recovery. Runtime flag uses the existing single-active-session renderer Settings model; no global hardware profile added. Per-owner DIFF audits recorded.

ASTC runtime validation: GUI Mario Kart World reached title using automatic Session ASTC Bc3 + Nx2 profile, with global astc_recompression=0 and custom use_global=true. Evidence TEMP/ruzu-session-astc-nx2-20261108-125225/automatic-bc3.png and ruzu_log.txt. No new image allocation OOM in this smoke. Next GUI Switch1 session reports Uncompressed + Memory4Gb. Runtime checks use separate GUI processes; same-process failure cleanup is covered by regression tests. Release GUI+CLI build passed in 4m33s. Local vulkaninfo confirms installed Radeon RX 5700 XT driver exposes textureCompressionASTC_LDR=false and textureCompressionBC=true.

GUI Switch1 validation complete: Mario Kart 8 Deluxe 1.7.1 reached title with Session ASTC Uncompressed and Memory4Gb (TEMP/ruzu-session-astc-switch1-20261108-125405/switch1-uncompressed.png). Both GUI runtimes stopped after identity checks. No title-specific ASTC override remains. No new temporary instrumentation added.

Console-wide ASTC policy final validation: test result: ok. 1726 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 465.40s Full common suite passed (423 unit, 2 integration, 1 ignored doc). Core retains 31 baseline failures and baseline wait_cancel access violation; no new failure before abort. Release GUI/CLI built, both GUI title smokes passed with expected per-session modes, diff --check clean. Implementation complete within stated lossiness/VRAM recovery limitations. Changes not committed/pushed.

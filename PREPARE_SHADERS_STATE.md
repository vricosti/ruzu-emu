# Prepare Shaders slice — state

Issue: https://github.com/vricosti/ruzu-emu/issues/24 (Support Precompiled Shaders)

Upstream reference (READ-ONLY): `/Users/mojics/orca/Fritchii/` (Eden-lineage fork cited in the issue)

## Scope decision (2026-11-15)

Fritchii's first-boot precompilation has two halves:

1. **NVN2 recipe/extractor** (`nx2_recipe.*`, `nx2_shader_extract.*`, `tools/nx2-precompile`)
   scans BNSH/BAHS/NDSR archives for **Switch 2** shader programs. Ruzu is a Switch 1
   emulator; no title ruzu runs ships NVN2 shader archives, so a faithful port would be
   dead code. **Not ported** — documented divergence.
2. **Runtime preparation** (`PreparePrecompiled`, `StartPrecompiled`,
   `shader_notify` warm/precompile state, `vulkan_used.bin` boot gating). This is
   host-side and applies to Switch 1 as-is. **Ported here.**

Adapted source of shaders: ruzu's own `vulkan.bin` (full `GenericEnvironment`s + keys).
It is the Switch 1 analog of a recipe: self-contained, shareable, already versioned by
`CACHE_VERSION`, and proven to recompile end-to-end (see issue #24 comment).

## Slices

### A. `shader_notify.rs` — warm/precompile state machine
- Source: Fritchii `src/video_core/shader_notify.h` (+ `shader_notify.cpp`
  `WarmMayBuild` from commit `4a4fa51576`).
- Items: `WarmProgress{kind,done,total}`, `set_warm_progress`/`warm_progress`,
  `begin_warm`/`end_warm`/`warm_active`, `set_emulation_paused`/`set_warm_requested`/
  `warm_may_build`, precompile states (`Absent/Idle/Running/Asked`) with
  `set_precompile_available`/`precompile_started`/`precompile_ended`/
  `request_precompile`/`precompile_requested`.
- Status: implemented, unit tests added.

### B. `pipeline_cache.rs` — used-pipelines boot gating
- Source: Fritchii commit `be76402ab5` ("build at boot only the cached pipelines recent
  sessions used").
- Items: `vulkan_used.bin` (magic `EUP1` = 0x31505545, `u32 count`, `{u64 hash, u8 age}`),
  `MAX_USED_PIPELINE_AGE = 8`, `MINIMUM_USED = 64`, `used_pipelines`,
  `disk_pipelines`, `save_used_pipelines`, periodic save on first use (2 min),
  `RUZU_PRELOAD_ALL_PIPELINES=1` override (env name adapted).
- Status: implemented, unit tests added.

### C. `pipeline_cache.rs` — background preparation
- Source: Fritchii `StartPrecompiled` + `PreparePrecompiled` (commit `790bca8969`),
  adapted to read ruzu's `vulkan.bin` instead of an `NX2PREP1` file.
- Items: dedicated low-priority `VkPrecompile` worker, stamp
  `precompiled_prepared.ruzu-v<CACHE_VERSION>.txt` (file size, mtime, driver version,
  build rev, device model), skip when stamp matches or `force`, build-and-drop every
  cached graphics pipeline that boot did not build, per-`WARM_PROGRESS` kind 3 progress,
  `request_precompile` re-run loop ("Prepare Shaders Now"), stop-token aware.
- Status: implemented, unit tests added.

### D. Frontend status
- GUI: `status_bar.rs` shows `WarmProgress` kind 3 alongside "Building: N shaders";
  `boot.rs` samples it. `ruzu_cmd`: none (log only).
- Status: implemented.

## Known divergences (documented in DIFF.md)
- No recipe/assembler/NVN2 extractor (Switch 2 only).
- Stamp uses `common::scm_rev::SCM_REV` + Cargo version instead of `g_scm_rev`.
- Env names `RUZU_*` instead of `EDEN_NX2_*`.
- Interrupt resume checkpoint (`precompiled_progress.*`, restart skipping) not ported:
  ruzu's preparation is bounded by the title's own disk cache and is re-entered from
  the start; skipping ahead without Fritchii's per-record progress validation could
  leave permanent holes. Documented as an intentional divergence.

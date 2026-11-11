# Handoff — Mario Kart World vegetation flicker (NX2)

**2026-11-11 update: resolved. Read this block first; it supersedes the
"Flicker: what is established" section below.**

- The flicker was not in the task/mesh compute replacement; both compute
  stages are deterministic when dispatched twice.
- Cause A (fixed): the title drops main-view objects when its CNTPCT-measured
  frame time overruns. `ClockPacing` in `core/src/core_timing.rs` paces the
  counter for NX2 sessions. `RUZU_NX2_CLOCK_PACING=0` restores the old
  behaviour.
- Cause B (fixed): the rock formations that appeared and vanished together,
  drawn black, are GPU-driven instanced draws that read the wrong instances.
  Three fixes: the five-bit source lane of SHFL.IDX
  (`shader_recompiler/src/backend/spirv/emit_spirv_warp.rs`), upstream's
  rightmost-first search for a storage buffer's base pointer
  (`shader_recompiler/src/ir_opt/global_memory_to_storage_buffer_pass.rs`),
  and flushing deferred instance draws before unrelated state writes
  (`video_core/src/engines/maxwell_3d.rs`). Details in DIFF.md (2026-11-11,
  second entry) and PERF_NX2_STATE.md, "Scenery flicker".
- Reproduce without driving: the intro cinematic (about 75-105 s after
  launch) is identical on every boot provided the title's save directory is
  deleted first. `ruzu-cmd` ignores the per-title ini.
- The validation runs deleted the title's Vulkan pipeline cache
  (`~/.cache/ruzu/shader/0400c3f00006e000/vulkan*`) first. Whether a stale
  cache would otherwise keep the old shader translation was not checked.
- The green course fly-over before a race was a VP9 video whose chroma plane
  was never written (NV12 to NV12 through libswscale); fixed in
  `nx2_video_decoder.rs`, see DIFF.md.
- Stopping the game could freeze or take about 10 s; fixed (suspend wait,
  GPU sync wait, syncpt cancel, breadth-first search), see DIFF.md.
- `RUZU_NX2_MESH_EXACT`, `RUZU_NX2_MESH_FINISH` and
  `RUZU_NX2_MESH_OUTPUT_TRACE` remain in the tree but do not bear on this
  defect.

---

Branch `research/eden-nx2-port`. Read this first, then the "Vegetation still
flickering" section of `PERF_NX2_STATE.md` and the 2026-11-10 entries at the
end of `DIFF.md`.

## What is fixed and confirmed

- Mario's face (converted-texture cache hit path): confirmed by the user.
- Transient black scenery, freezes, VRAM death on huge images: measured, see
  `PERF_NX2_STATE.md`.

## What is still wrong (two separate defects)

1. **Bushes flicker** in races and cinematics (Mario Bros. Circuit, first cup).
   The user sees it at a glance; automated frame-difference detectors missed
   it (0 "rare object" frames over 150), so **trust the eye, not the script**.
2. **Bushes/tumbleweeds are black.** Separate cause: every static-world draw
   samples a BC6H 8192x8192 virtual-texture array that upstream and ruzu both
   create as an unbacked sparse image. Needs sparse residency. Not started.

## Flicker: what is established

- It is in the compute replacement of task/mesh shaders (host side). Guest
  inputs of each vegetation draw are identical frame to frame when stationary.
- Fixed here: the task payload and mesh output live in Workgroup storage,
  undefined at launch. They are now cleared (`emit_spirv_mesh.rs`,
  `RUZU_NX2_WORKGROUP_ZERO=0` to disable). Payloads became byte-stable
  (34/34 draws) — necessary, **not sufficient**: the user still sees flicker.
- Exact path, 300 stationary frames: 44 mesh draws, vertex counts constant per
  draw, 11346 vertices every frame. So no draw is dropped; 30 vegetation draws
  change *content* every frame (wind animation or wrong data: undetermined).

User observations (ground truth), all with the Workgroup clear on:

| configuration | flicker |
|---|---|
| exact path + GPU wait after mesh dispatch + output readback (trace run) | no |
| exact path only (`RUZU_NX2_MESH_EXACT=1`) | yes |
| default path + GPU wait after mesh dispatch (`RUZU_NX2_MESH_FINISH=1`) | yes |
| default path | yes |
| exact + `RUZU_NX2_MESH_FINISH=1`, no trace | **not observed yet — next test** |

Reading: something about ordering/synchronisation between the mesh compute
dispatch and the indirect draw that consumes its output, or a buffer reused
too early. Candidates not yet tested with a verified binary:
`disable_buffer_reorder=true` (upload reordering), staging-pool reuse of the
payload/output/mapping buffers (`free_deferred`), the pre-draw barrier in
`dispatch_mesh_programs`.

Ruled out (each with its own run): unbacked GPU-only regions (pool 16 GiB),
GPU accuracy High, `OpUndef` values, `REQUIRE_FULL_SUBGROUPS`, AMD subgroup
size (driver reports max 64; pipelines request 32 via subgroup size control).

## Diagnostics in the tree (all inert unless set)

- `RUZU_NX2_MESH_EXACT=1` — always read task counts back (no speculative path).
- `RUZU_NX2_MESH_FINISH=1` — `scheduler.finish()` after the mesh dispatches.
- `RUZU_NX2_MESH_OUTPUT_TRACE=1` — forces exact, reads the mesh output back and
  logs `mesh out f<frame> draws=.. verts=..: records/vertices/hash ...`.
- `RUZU_RENDERDOC_TRIGGER=<file>` — under RenderDoc, write a number N in the
  file to capture N consecutive frames (in-app API, polled every 30 frames).
- `RUZU_NX2_WORKGROUP_ZERO=0`, `RUZU_NX2_MESH_DRAW=0`, `RUZU_GPU_XLATE_MISS=1`,
  `RUZU_DEBUG_SKIP_VIEW_DRAWS=null|unbacked|bc6h|r8|r16`,
  `RUZU_NX2_GRAPHICS_POOL_GIB=8..24`.

## RenderDoc

- Windows, RenderDoc 1.47: capturing works with
  `ENABLE_VULKAN_RENDERDOC_CAPTURE=1` in the environment (`renderdoccmd
  capture` did not inject) plus `RUZU_RENDERDOC_TRIGGER`. Captures are ~4 GB.
  **Replay crashes** ~25 s after opening, right after the initial-state sync,
  no error logged. On Linux, try the same trigger; expect the same size.
- What to look at in a capture: for a bush draw, the compute dispatch of the
  mesh stage (bound storage buffers: mapping, payload, output), then the
  `vkCmdDrawIndirect(Count)` that follows and its vertex shader inputs. Compare
  a good frame with a bad one: is the output buffer different, or is the draw
  reading it at the wrong time/offset?

## Reproduce

Menu path (capture harness buttons): wait for fps; A, A, L+R, A (main menu);
then A x9 to reach the race on Mario Bros. Circuit; hold A ~35 s to drive to
the bushes; stop and watch. Per-title config: `astc_recompression=3`,
`gpu_accuracy=0`. Navigation scripts used on Windows live outside the repo.

Method warning: several A/B runs silently used a stale binary because copying
over a still-running executable failed. Check the binary timestamp first.

## Tests

`cargo test -p shader_recompiler --lib` (665 pass), `cargo test -p video_core
--lib` (1796 pass at fc98c4d6; not re-run after the last diagnostics).

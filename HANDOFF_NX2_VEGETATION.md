# Handoff — Mario Kart World vegetation flicker (NX2), 2026-11-10

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

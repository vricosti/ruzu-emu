# NX2 performance state — Mario Kart World

Branches: `perf/nx2-mario-kart-world` (b8186387), `feat/nx2-astc-bc7`
(adce8acc, 8dfcdf10), both pushed. Evidence and protocols:
`tools/capture_harness/recordings/nx2-split-paired-20261109.json`,
`nx2-low-profile-20261109.json`, raw GPU-thread stacks in
`nx2-gpu-thread-stack-samples-20261109.txt`.

## Delivered and measured (2026-11-09)

Reference start-grid window: 13.75 -> 22.0 FPS (+60%). First-race driving:
14.05 -> 17.67 FPS (+26%) with zero sub-5-FPS samples (worst frame
1.26 s -> 0.21 s) at BC7 quality. Real user driving ~16 FPS.

- GPU split default off (both modes measured below mode 0; root cause:
  front Drain serialization against a back saturated by draw preparation).
- Per-title MKW: gpu_accuracy=Low (+54% same-build A/B), astc_recompression
  BC7 via the in-tree analytical mode 6 encoder (BC3 speed; bc7enc vendored
  as optional quality profile; BC7F evaluated and removed: 12% worse SSE at
  equal speed).
- Converted-texture disk cache (per title, CityHash64 source keys, 16 GiB
  cap): eliminates cross-session conversion stalls and in-race re-upload
  reconversion.
- Lock-free published GPU-level/DMA-safety settings reads on hot paths.
- ThreadWorker helping wait (conversion caller drains the queue; wake fix).
- NX2 session policy: async shaders off by default (scenery pop-in deemed
  unacceptable); explicit per-title choice wins; RUZU_NX2_ASYNC_SHADERS=1.
- VRAM-aware session ASTC policy: configured Uncompressed kept on >= 11 GiB
  discrete GPUs.
- RUZU_SKIP_BOOT_PRECOMPILE=1 option (keeps persistence; dedup via disk key
  sets). Do not combine with async-off sessions.
- Switch1 validated (MK8D smoke on the final binary; policies inactive).

## Where the remaining time goes

GPU command thread saturated (~99%): command processing (macro 190 +
method 173 + dma dispatch 170 ms/s) and draw preparation (fill image views
95-97 ms/s steady, descriptor visiting; was 143-303 before the disk cache).
Fence host waits are zero at Low accuracy. Boot (~4.5 min for 25.8k
pipelines) is driver-bound: after an Adrenalin shader-cache reset,
consecutive boots still spend 2422 s / 2350 s cumulative worker time in
vkCreate (AMD RX 5700 XT does not reuse pipeline compilations across
boots); translation is 24%.

## Next slices (design work, in value order)

1. Split rehabilitation: cut back-side per-draw cost first (descriptor-visit
   memoization needs TIC/TSC region write tracking so unchanged tables skip
   guest re-reads), then re-A/B the split (its drains wait on that cost).
2. Draw-preparation reduction benefits the unsplit path identically.
3. Boot: pipeline LRU pruning (needs per-key last-used metadata sidecar) or
   interleaved loading (requires batched cache insertion under the
   rasterizer lock); both currently conflict with the no-pop-in policy
   during their warmup phase.
4. BC7 banding check in play; revisit multi-subset encoding (bc7enc profile
   or BC7F default) through the disk cache if observed.

Methodology notes: grid windows must match race phase (lap-1 pack crossing
costs ~4 FPS); hold-A driving windows carry +/-1-3 FPS scene noise; always
stop stray emulator sessions before benchmarking or building (the exe lock
breaks cargo; background sessions inflate microbenchmarks).

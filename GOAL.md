# Current goal — Luigi's Mansion 3 runtime parity

Bring Ruzu's Vulkan execution of *Luigi's Mansion 3* to the same performance and stability level as Eden, while preserving the strict structural and behavioral porting parity defined in `AGENTS.md`.

## Current symptoms

- Gameplay can fall to about 10 FPS.
- Earlier runs exhibited missing or crackling voices and sound effects, frozen cinematics, black loading screens, crashes, and Vulkan out-of-device-memory failures.
- The current instrumented run shows repeated texture-cache growth and collection cycles. Active image estimates can exceed 4 GiB; after collection they fall sharply, while Vulkan heap usage decreases later in large allocator-sized steps.
- Buffer-cache residency remains comparatively small (roughly 70–153 MiB), so it is not the primary VRAM consumer in the observed run.

## Active investigation

1. Compare the complete texture-cache garbage-collection and Vulkan resource-lifetime paths against Eden, including ordering, LRU iteration limits, deferred destruction, scheduler ticks, and memory accounting.
2. Measure the CPU time and Vulkan memory retained by texture collection so the 10 FPS regression is attributed to evidence rather than inference.
3. Remove Rust-only behavioral or ownership divergences responsible for texture churn, excessive collection work, command-buffer instability, or resource retention.
4. Rebuild and run Luigi's Mansion 3 through the affected cinematic and gameplay sequences without a time limit, checking audio, image progression, crashes, VRAM use, and frame rate.
5. Remove temporary diagnostics once the cause is verified, add focused regression tests, reread the matching Eden headers and implementations line by line, and update `DIFF.md`.

## Confirmed correction in the current worktree

`TextureCache::RunGarbageCollector` no longer stops the entire aggressive LRU scan as soon as usage crosses below the critical threshold. It now follows Eden by reducing the remaining iteration quota and continuing. A focused regression test covers this transition.

## Completion criteria

- Luigi's Mansion 3 reaches and runs gameplay without freezing, crashing, black-screening, or losing voices and sound effects.
- Performance is comparable to Eden in the same scene and configuration; the current approximately 10 FPS result is not acceptable.
- Vulkan memory remains within the device budget without pathological texture eviction/recreation cycles.
- Every retained implementation change has focused coverage where practical and a corresponding `DIFF.md` audit entry.
- Relevant focused tests, `cargo test -p video_core`, formatting checks, and the release build pass.

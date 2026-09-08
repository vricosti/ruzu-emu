# Metal upload-prefix prerequisite

## Latest evidence (2026-09-08)

The prefix is now active, tested and exercised in the GUI; older statements
below about inactive wiring describe earlier steps, not the current source.
Run ../ruzu-diagnostics/lm3-upload-prefix-native-20260908.3kG4Gn completed its
120-second limit with a correct hall, no native error and no safety trigger.
Median late-hall FPS was 15.622, versus 15.944 in the previous non-prefix run:
no established performance improvement. Peak physical footprint was 9.839 GiB,
swap grew 838 MiB, and free disk fell 1049 MiB. Live texture roots also differed
(5.028 GB versus 3.023 GB); this is not a controlled identical-resource workload.

Hypothesis invalidated: outer pre/post-copy barriers do not explain most of
the surviving interruptions. Tick 12373 attributes 192 render ends to
encode_copies' inline branch, while the previous run had 194 at that site.
The previous EligibleUpload category (128 breaks at tick 12419) is largely
replaced by Other, not removed. Do not infer an upload speedup from zero breaks
in the now-reordered category. Native producer completions prove prefixes execute,
but do not identify which remaining copies are blocking rendering.

Next measurement splits inline copies into forced/used ordered uploads,
dedicated uploads, generic GPU copies, downloads and clears. This changes only
the existing optional profiler attribution; it does not relax dependencies.

Attribution build verified: 1811 passed, 3 ignored, no warnings, full release
video_core with Metal API validation (/tmp/metal-copy-attribution-tests.log).
GUI UUID B167ABB9-DE9E-32EE-A680-41F9A68E54D2, signature verified, bundled
MoltenVK SHA256 unchanged (0995b17b030c01e991e2c36b48a953d8a4fdb6c4df1b9dcaa46b6d9e08612855).
Run ../ruzu-diagnostics/lm3-copy-attribution-20260908.aCxqvK was stopped by
the physical-footprint guard at 91.158s (10.379 GiB), before a sampled hall
batch. No new hall FPS comparison is valid. Earlier sampled menu batches show
56 ordered-upload breaks and zero dedicated/copy/download/clear breaks; those
counts must not be extrapolated to the hall.

Last texture report: 2003 images, 5,459,665,024 native root bytes, 6,946,816
slice bytes, zero retired images/views/framebuffers, device allocations about
6 GB. Observed free disk loss was at most 22 MiB and global swap did not grow
relative to the first monitor sample. The watchdog terminated the process;
no ruzu instance remains. Do not raise its memory threshold for another run.

Performance-run slice paused on the existing memory prerequisite in
METAL_TEXTURE_GC_STATE.md: complete safe download/writeback support, then wire
the common LRU collector. This evidence identifies live cache ownership, not
a proven leak, and does not justify evicting dirty GPU images without writeback.

## Interrupted slice (2026-09-07)

Reduce render-pass interruptions from buffer-cache streaming uploads. Eden's
vk_buffer_cache.{h,cpp} CopyBuffer chooses RecordWithUploadBuffer only for
StagingBufferPool::StreamBuf() and can_reorder_upload. CanReorderUpload rejects
destination ranges already used, or the disable_buffer_reorder setting.
Metal currently preserves these copies inline. EligibleUpload profiling tags
that exact subset without changing GPU ordering. Measure its render-break
count before attributing the entire Other category to streaming uploads.

## Required ownership and lifetime change

2026-09-08 prerequisite found during scheduler review: wait/finish_all/poll
pop the pending command buffer before validating completion. An error is then
forgotten on subsequent calls; synchronous finish also advances known_gpu_tick
before validating earlier submissions. Fix retirement/error persistence first,
then implement the multi-buffer cohort. Upload reordering remains inactive.

Retirement prerequisite implemented: all three queue-consuming paths validate
the front before removing it. Synchronous finish submits through the ordinary
pending queue and waits through the same ordered path, without a speculative
known_gpu_tick update. A failed validation keeps its native owner and tick;
there is no new wait in the asynchronous path. Native regression exercises
repeated rejected retirement with unsubmitted command buffers, then ordered
successful completion (no deliberately induced GPU fault). Full release suite
1804 passed, 3 ignored, no warnings, Metal API validation enabled:
/tmp/metal-retirement-order-tests.log. GUI not rebuilt for this scheduler slice.

Rechecked Eden vk_buffer_cache.cpp CanReorderUpload/CopyBuffer: eligibility
still matches unused destination ranges plus exact stream-buffer identity;
generic copies and used destinations remain inline. Next implementation must
extend the pending submission owner to include the optional prefix and require
successful completion of every member before retiring the shared logical tick.
Do not infer whole-cohort completion from the render member's status alone.

Shared-tick pending ownership is now implemented in MetalScheduler: optional
prefix plus render are submitted in that order under one tick, and all waits,
polls and successful retirement validate both. Either error remains queued.
Normal production submission still passes None, so no uploads are reordered.
Native tests verify either member finishing first and actual overlapping-copy
readback with native resource retention after Rust owners drop. Extended test
checks deferred staging cannot be reused until the entire cohort finishes.

Next: add active prefix encoder ownership/flush/drop handling, preserve sampler
and profiler lifetimes, wire only proven stream/unused-range eligible copies,
test stream wrap against the cohort and inspect indirect resource declarations.
Update the runtime supervisor to distinguish native member completions from
logical cohort_retired before enabling the live prefix: max(native tick) alone
does not prove all members finished. Do not claim prerequisite completion from
the isolated commit tests alone.

Final cohort/staging verification: 1807 passed, 3 ignored, no warnings, full
release video_core suite with Metal API validation enabled. Evidence:
/tmp/metal-cohort-staging-tests.log. GUI still the prior memory-profile build;
do not use it to claim runtime validation of the new scheduler ownership.

## Active upload-prefix recording (2026-09-08)

Implemented in source, GUI not yet rebuilt: MetalScheduler owns a lazy prefix
blit encoder separately from its active render/compute encoder. Flush ends both
and commits one cohort; Drop ends an unsubmitted prefix encoder. External and
presentation commits flush any pending guest cohort first, preventing them from
consuming a tick already captured by guest staging leases.

MetalBuffer shares range validation/write-generation notification between its
ordered and prefix copy methods. BufferCacheRuntime uses the prefix only with
the existing CanReorderUpload result and exact stream allocation identity;
dedicated staging, downloads and generic buffer copies stay inline. Reviewed
native buffer creation: destination tracked, source stream untracked; source
reuse is protected by the cohort tick. bind_stage binds buffers/textures
directly, while the current argument-buffer path is sampler-only.

Native tests cover render encoder identity across a prefix upload, consumer
readback, upload-only flush, external ordering, encoder Drop, stream wrap with
unsubmitted copies, and fallback to dedicated storage without overwriting the
old lease. Initial full suite passed 1810 tests. Added eligibility test initially
expected independent ranges within one 64-byte granule; corrected after reading
Eden UsageTracker. Final full suite rerunning in
/tmp/metal-upload-eligibility-tests.log.

Eligibility test correction detail: the existing UsageTracker conservatively
over-marks sub-64-byte writes to avoid upstream's shift-by-64 undefined case.
The independent-granule assertion now marks a full 64-byte granule, preserving
that production behavior rather than changing it to satisfy the test. The test
also explicitly rejects a neighbouring address within that same granule.

Prepared runtime supervisor (not yet launched):
../ruzu-diagnostics/lm3-upload-prefix-20260908.gvPZcD/watch.py
It tracks cohort_retired for completion progress, while still catching errors
from individual native callbacks. Same 120-second/memory/disk/GUI guards and
input schedule as prior runs. Build/sign the GUI before running this script.

Final source verification: full release video_core suite 1811 passed, 3 ignored,
no warnings, Metal API validation enabled. Log:
/tmp/metal-upload-eligibility-tests.log. Runtime measurement remains pending.

### First runtime attempt: supervisor false positive corrected

GUI UUID 22BE5C0F-AF20-3540-97FB-93F767F16112 built and signed. Attempt gvPZcD
stopped at 9.34s: watchdog used CPU cohort_retired exclusively, but initialization
buffers 1..8 had all completed natively while the CPU continued loading without
polling retirement. This is not evidence of a GPU stall or rendering regression.
No game frame/performance measurement was obtained.

Native callback journal now includes the cohort member count (one or two).
New supervisor ../ruzu-diagnostics/lm3-upload-prefix-native-20260908.3kG4Gn/watch.py
deduplicates completion callbacks by native object within a tick and advances
GPU completion only through contiguous fully completed cohorts. CPU retirement
is logged separately. Synthetic parser checks cover out-of-order callbacks,
duplicates, two-member groups, single members, errors and CPU/GPU distinction.
Full release suite 1811 passed, 3 ignored, no warnings, Metal API validation
(/tmp/metal-cohort-journal-tests.log). Rebuild in progress for the new journal;
the gameplay ordering change itself is unchanged from the first attempt.

MetalScheduler must own optional upload-prefix and render command buffers as
one logical submission cohort. Existing staging leases and buffer usage have
already captured current_tick when the copies are recorded. Allocating another
logical tick for a prefix would permit retirement before the consumer finishes.
Do not resume the upload-reordering slice until cohort completion, wait, poll,
error propagation and resource retirement are implemented and tested.

Keep generic GPU copies, downloads and already-used destination ranges inline.
Preserve ordered writes among eligible uploads. Verify native dependencies for
tracked destinations and source-stream reuse; do not assume queue ordering alone
proves completion of both buffers. No global idle wait or larger queue cap.
Journal native buffer completion separately from logical cohort retirement.

Apple's Resource synchronization documentation (checked 2026-09-07) guarantees
automatic hazard handling on MTLCommandQueue only for tracked resources bound
directly to encoders. Audit argument-buffer resources/useResource as well; this
does not apply to MTL4CommandQueue. Native buffer-cache destinations are created
tracked, whereas stream sources are untracked and require CPU reuse protection.
References:
- https://developer.apple.com/documentation/metal/resource-synchronization
- https://developer.apple.com/documentation/metal/setting-up-a-command-structure

## Verification gates

- Source identity, unused/used destination ranges and disabled reordering.
- Ordered overlapping writes and later consumers, including separate regions.
- Prefix and render completion/error combinations; no early completed_tick.
- Stream wrap and deferred reclamation while either native buffer is pending.
- Full video_core tests with Metal validation, then bounded GUI lobby comparison
  with unchanged saves/config/cache/library and memory/disk/stall supervision.

Current implementation is attribution only, not an upload-prefix port or a
performance improvement. Native Metal scheduling is a documented adaptation;
Eden remains the reference for guest order and eligibility, not Metal API design.

## Measured decision (2026-09-07)

../ruzu-diagnostics/lm3-eligible-20260907.dhgwvK/ contains the rebuilt GUI run,
supervisor, captured lobby, analysis script and logs. Among ten sampled large
lobby batches, EligibleUpload accounts for 11.81% of counted render breaks,
not 11.81% of GPU time. Counts range 0..120 (median 23.5); remaining Other
has median 221 and uint8 conversion median 67. One batch with 106 eligible
breaks is not representative. Do not prioritize a large prefix refactor on
the assumption it eliminates all 250 generic interruptions. Attribute remaining
Other call sites before choosing the highest-impact optimization. The lifecycle
prerequisite above still applies whenever the prefix slice is resumed.
# 2026-09-08 Post-copy barrier investigation

Correction to the previous hall attribution: the 125 interruptions at
metal_buffer_cache.rs:705 are PostCopyBarrier, NOT encode_copies. The latest
native run used exactly that source layout. A caller-site count by itself
does not identify actual GPU copies.

Metal PostCopyBarrier now leaves unrelated active render encoders intact.
Buffer-cache allocations are hazard-tracked; ordinary GPU copies already
switch to a blit encoder and eligible stream uploads are committed as the
prefix command buffer before the main command buffer on the same queue.
The CPU-written stream itself is untracked and remains protected by its
completion-tick leases. This reasoning does not extend to untracked GPU
destinations or MTL4CommandQueue. PreCopyBarrier and actual copy ordering are
unchanged. This is native Metal synchronization, not removal of a Vulkan
memory dependency or a stub.

Added native regression for prefix/ordered upload -> retained render encoder
-> compute consumer, verifying encoder identity and exact output with no
intermediate submit/wait. Full release Metal-validation suite passed:
1823 passed, 3 ignored, no warnings (/tmp/metal-post-copy-barrier-tests.log).
GUI benchmark remains pending; no
FPS gain claimed. Existing same-tick compute/copy ordering tests still run.

Next measurement must include a paired run WITHOUT detailed GPU stage/
submission profiling. The native counter samples can themselves perturb
rendering; do not equate profiled 15.87 FPS with uninstrumented throughput.
Keep the command-journal progress watchdog and resource limits active. The
current GUI UUID 5A28FD11-5991-3D87-BCC4-DDCB86C917B2 includes GC but not this
post-copy optimization and is a usable baseline before the next GUI rebuild.

### Retained render consumer verification

The earlier native test preserved the render encoder but consumed the copied
buffer only from compute. It now also draws a full-screen triangle whose
fragment shader reads that buffer, then checks exact RGBA pixels via compute.
Both ordered and prefix upload branches pass with no intermediate submit or
wait. Full release Metal-validation suite: 1823 passed, 3 ignored, no warnings
(`/tmp/metal-post-copy-render-tests.log`). No production change in this slice.

Unprofiled baseline prepared at
`../ruzu-diagnostics/lm3-gc-unprofiled-20260908.SZZArl/`: same GUI/config,
stage and submission profilers removed, safety guards retained. Startup refused
because `CGSSessionScreenIsLocked=Yes`; no app process or performance evidence
was created. Preserve the current GUI UUID above until that baseline is measured.

Baseline completed after unlock: the same prepared supervisor finished at
120.29 seconds, with no resource/stall guard triggered. Native scene-110.png
shows the populated hall (Luigi, Mario, Peach, furniture and portraits), without
the previous giant-triangle corruption. Nine samples in 110..120 seconds have
median game FPS 15.841 (range 14.853..15.978), versus 15.871 in the profiled
GC run. Removing detailed profiling does not explain the large frame cost.
Peak physical footprint 6.384 GiB, no swap growth, maximum disk loss 16.79 MiB.
This is still the baseline GUI without the PostCopyBarrier optimization;
the optimized GUI rebuild is the next comparison, not an established speedup.

Optimized GUI rebuilt successfully in release (no warnings), bundled and
signature-verified: UUID `3A9B254C-2A0D-3E83-88F4-AC9F846BB54F`.
Bundled MoltenVK SHA256 remains
`0995b17b030c01e991e2c36b48a953d8a4fdb6c4df1b9dcaa46b6d9e08612855`.
Second supervisor/config prepared at
`../ruzu-diagnostics/lm3-post-copy-unprofiled-20260908.L2dMxb/`.
The session locked again during rebuild: startup refused before launching the
optimized GUI. No optimized performance result exists yet; resume that script
after unlock. Baseline monitoring and screenshots remain in SZZArl above.

Optimized run completed after unlock at 120.20 seconds, without a safety
trigger or application error. Peak footprint 6.444 GiB, swap growth zero,
maximum disk loss 14.77 MiB. Nine samples at 110..120 seconds: median 21.811
FPS, range 15.924..23.848. The rise occurs late (about 115 seconds), rather
than a uniform uplift: earlier 100..110 samples remain about 16..19 FPS.
Native scene-100/110 images show the populated hall without giant triangles,
but Luigi/Polterpup and camera differ from the baseline scene-110. Therefore
the apparent +38% median is NOT yet an isolated performance attribution or
proof of sustained >=20 FPS for an identical workload. No instance remains.

Next gate: compare settled camera/game state and capture the end of the
measurement window (the current supervisor stops before capturing at 120s).
Repetition and render-break attribution are needed before claiming a durable
PostCopyBarrier gain. Do not discard this scene-timing confound.

Repetition `../ruzu-diagnostics/lm3-post-copy-repeat-20260908.avIC98/` uses
the same optimized GUI/config/input schedule and guards, adding only a native
capture at 118s. It finished at 120.26s: no crash/resource trigger, peak
footprint 6.425 GiB, zero swap growth, disk loss 15.07 MiB. Nine late samples
give median 17.907 FPS (15.895..18.848). The scene-118 capture shows the same
stationary Luigi/camera composition as the baseline, with Polterpup animating.
The earlier 21.811 FPS median is not repeatable here; sustained >=20 FPS is
unproven. Compared to the one baseline this suggests about 13% improvement,
but does not isolate thermal/load/animation variability or establish a stable
performance delta. Do not advertise the earlier +38% as a verified gain.

Next: profile the optimized hall with the existing per-site interruption and
native GPU interval counters, confirm whether PostCopyBarrier ends disappeared
or merely moved to subsequent compute, then target the remaining measured GPU
cost. The fixed 120-second watchdog is unchanged. No process remains running.

### Optimized native GPU attribution

`../ruzu-diagnostics/lm3-post-copy-profile-20260908.IKlZgM/` completed at
120.21s with stage/submission profiling enabled. Median late FPS 16.911,
peak footprint 7.222 GiB, no swap growth, disk loss 16.46 MiB. No resource
guard fired. Native capture shows a populated hall, but moving Luigi/camera
again precludes claiming an identical workload.

Large hall tick 14249: 320 measured render passes, complete sample coverage,
native command 68.562 ms, interval union 53.148 ms, largest gap 5.367 ms.
Stage interval sums: blit 0.940, compute 3.419, vertex 14.722, fragment 45.734
ms (overlap/dependencies mean these are NOT additive ALU execution costs).
PostCopyBarrier end site is absent. Remaining sites include 67 ordered uploads,
46 clear-helper starts, 41 guest pass starts, 37 uint8 conversions, 35 guest
barriers, 29 texture-cache copies and 25 conditional resolves. The prior 409
passes/62.520 ms sample is not identical in guest draw count or camera; fewer
passes alone does not prove reduced GPU time.

Profiler blind spot identified: the 800x450 pass with 5.617 ms fragment interval
reports zero guest draws. Internal clear/blit draws were never tagged. Added
separate helper counters for depth/stencil blit, color blit, and clear in the
existing opt-in profiler, preserving guest shader hashes and all native command
ordering. Full release Metal-validation suite passed (1823 tests, 3 ignored,
no warnings): `/tmp/metal-helper-profile-tests.log`. GUI not rebuilt for these
new metadata fields yet; classify this pass before optimizing it blindly.

### Helper classification run

Release GUI rebuilt without warnings and signature-verified, UUID
`B423108B-9983-3F69-8606-533C2BF9D271`; bundled MoltenVK hash unchanged.
`../ruzu-diagnostics/lm3-helper-profile-20260908.2DH60J/` completed at 120.25s.
Late median 15.928 FPS, peak footprint 7.151 GiB, maximum global swap growth
79.38 MiB and disk loss 31.53 MiB. No safety trigger; native scene-118 shows
the populated stationary hall without the earlier geometry corruption.

The zero-guest-draw 800x450 RGBA16Float pass is a shader CLEAR:
tick 14361 sample 2122 reports helper_draws=[0,0,1], fragment interval 1.465 ms.
Another clear at 400x300 costs 2.022 ms in tick 14055. The earlier 5.617 ms
interval did not repeat at that size; do not assume every clear has that cost.
Large guest passes and small 64x64 guest passes remain among the highest costs:
tick 14361 main pass has 372 draws, vertex 7.653 ms / fragment 6.572 ms;
small 64x64 passes have about 1.2..3.1 ms fragment intervals. These are interval
measurements, not additive isolated shader execution times. Last large command
62.785 ms; fragment interval sum 40.084 ms.

Source review: clear()'s native load-action path is excluded by an active GPU
conditional predicate, scissor, partial color/stencil masks or integer color.
conditional_quad_arguments() correctly returns None without a predicate; it
does not force every clear indirect. Current per-pass helper metadata does not
identify which exclusion applies to the measured clear. No predicate/mask was
relaxed. Next slice should audit safe render-pass reuse for helper clears and
full-extent scissor eligibility, including state/query restoration and actual
attachment extent, before changing native load/store behavior.

## Clear encoder reuse prerequisite (active slice)

The existing ordinary render-pass key has no slice because ordinary draws use
view-relative slice zero. Clear descriptors select an explicit layer. Before
reusing helper encoders, add that slice to the framebuffer-owned key and its
layer-specific constructor. Then make shader clears establish viewport/scissor,
culling, depth bias and disabled visibility state explicitly instead of relying
on a fresh encoder. Guest draw() already binds those relevant states anew.
Keep conditional arguments, masks, integer payloads and load-action eligibility
unchanged. Validate encoder identity plus native pixels under hostile inherited
state for conditional clears, mips/layers, depth/stencil and MSAA.

Prerequisite and clear reuse are now implemented. Layer-aware keys are owned
by MetalFramebuffer; helper clears use begin_or_reuse_render_pass and reset
the inherited states listed above. Subsequent guest draws already rebind them.
Native regression checks encoder identity plus all existing conditional/masked
pixel oracles under hostile inherited state. Full release Metal validation:
1823 passed, 3 ignored, no warnings (`/tmp/metal-clear-reuse-tests.log`).
GUI rebuild/runtime comparison pending. Full-scissor/load-clear optimization
was NOT enabled; no condition or guest write-mask semantics were relaxed.

GUI runtime validation completed: release UUID
`C0B0FCD4-7D9A-3C83-A2BD-E981A27F7A5D`, signature verified, bundled MoltenVK
unchanged. Run `../ruzu-diagnostics/lm3-clear-reuse-20260908.nVp56r/` stopped
normally at 120.28s, native final capture shows the same stationary hall without
visible corruption. Median late profiled FPS 15.985 versus previous 15.928:
no meaningful FPS gain established. Peak footprint 7.163 GiB, global swap growth
58.25 MiB, maximum disk loss 13.83 MiB, no safety trigger.

Last large sampled tick 14135: 300 render passes, 20 clear-helper end sites,
native command 60.425 ms, vertex interval sum 16.007 ms, fragment 42.528 ms,
compute 0.790 ms, blit 0.558 ms. Workload counts vary between sampled batches;
do not translate fewer clear interruptions into a fixed percentage speedup.
Clear reuse is functionally verified, not the major performance solution.
Next investigation should target the expensive guest render passes (especially
the repeated 64x64 passes) and their shaders/resources rather than continue
assuming encoder fragmentation alone explains the remaining cost.

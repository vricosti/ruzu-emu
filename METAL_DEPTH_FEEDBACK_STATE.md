# Native Depth Feedback Slice

## Interrupted Optimization

Goal: reduce unnecessary native render-pass boundaries without changing guest
depth tests, sampled values, stores, or clear ordering. The common Eden
CheckFeedbackLoop predicate remains unchanged; no barriers have been removed.

Hall source-site measurements show 123..127 feedback boundaries per large batch.
Native alias instrumentation is implemented and tested, but the first run with
it hit the swap guard before a completed hall batch. Menu evidence shows real
same-mip/slice storage aliases with depth writes disabled and Less/LessEqual
tests still enabled. This does not permit detaching the depth attachment.

## Required Before Changing Feedback Ownership

1. Hall coverage obtained in the bounded retirement-check run below, without
   raising memory/queue limits. Preserve the previous swap-guard run as evidence,
   not as a successful hall measurement.
2. Identify the actual sampling stages/descriptors and depth/stencil writes for
   overlapping resources. Bound descriptors alone do not prove shader accesses.
3. If a separate native sampling snapshot is needed, implement native content
   version tracking first. ImageBase modification ticks and the existing
   native/slice dirty state are not yet verified as an exact depth-write version.
   Audit uploads, copies, clears, attachment writes, storage-image writes,
   rescaling, recreation and destruction. A stale snapshot is not acceptable.
4. Keep native image/snapshot ownership in metal_image.rs/metal_texture_cache.rs,
   copy recording in native copy owners, and leases/completion in the scheduler.
   Reuse existing MetalDepthStencilCopy only after checking its aspect/sample
   semantics; never introduce a per-draw full copy or a global idle workaround.
5. Test write -> sample, repeated read-only draws, clear -> sample, mip/layer
   separation, destruction while in flight, and comparison/stencil preservation.
   Then measure the same hall scene and verify other titles before claiming gain.

Eden remains read-only. It has no native Metal snapshot owner to copy literally;
any Metal-specific adaptation must preserve guest semantics and be documented.
20 FPS and the full geometry-support objective remain unmet.

## 2026-09-07 Hall Coverage After Retirement Fix

Evidence: `../ruzu-diagnostics/lm3-retirement-check-20260907.EJCjDT/`.
The rebuilt GUI reached the hall, confirmed by scene-110.png. Four completely
sampled batches (11959, 12007, 12485, 12768) contain 160 common-cache feedback
requests each and 596..606 actual render encoder endings. Native alias keys
have no omitted entries and confirm the same root texture, mip 0, slice 0,
one sampled mip/layer, a 2D sampled view and a 2D-array depth attachment.

All observed aliases disable depth writes, but compare modes include Less,
LessEqual, Greater and Always; some have stencil enabled. The instrumentation
records stencil enable, NOT stencil write operations/masks. Thus neither a
read-only combined depth/stencil assertion nor detaching depth is justified.
Bound aliases still do not prove dynamic shader accesses. Identify their stage,
descriptor, aspect and effective stencil write state before changing barriers.

Full batch GPU durations are 74.1..83.9 ms; summed fragment intervals 46.7..53.8
ms and vertex intervals 20.1..21.1 ms can overlap and are not ALU utilization.
The hall GUI FPS median over 110..120s is 13.96 (nine samples), not the goal of
20. No performance gain is established by this one run. No barriers changed.

## Diagnostic Detail Prerequisite

The scalar alias key now additionally records stage (1..5), native texture slot,
stage shader hash, sampled native pixel format, and full front/back stencil
compare/fail/depth-fail/pass/read-mask/write-mask state. The profiler keeps at
most 64 keys and counts omitted bindings. No texture ownership is retained.
Vertex stage identity uses the vertex-B key slot; combined vertex-A/B programs
need the separate vertex-A hash for full shader reconstruction. This diagnostic
does not yet prove dynamic reads or effective writes after stencil/depth tests.
Full video_core release validation passes: 1786 tests, 3 ignored, Metal API
validation enabled, no Rust warnings. A new bounded GUI capture is required
before using these fields to select an optimization; that capture is recorded below.

## 2026-09-07 Stencil Detail Result

Evidence: `../ruzu-diagnostics/lm3-stencil-detail-20260907.q2Wtg1/`.
Release GUI UUID C42CA4BC-68D8-34AC-9EEB-39E666346223, signature verified;
scene-110.png confirms the hall. The run stopped at the 120s limit (observed
121s, SIGTERM then SIGKILL), not a spontaneous crash. No surviving instance.
Median GUI FPS over 110..120s: 12.91, ten samples. Peak footprint 9.09 GiB,
global swap growth 472 MiB, disk free-space drop 19 MiB. No gain claimed.

Four complete batches (11917, 12259, 12352, 12436) each report 160 common
feedback requests, 149 native alias bindings, and zero omitted keys. All alias
bindings are fragment stage, native texture slots 0 or 1, pixel format 260
(Depth32Float_Stencil8), across 17 shader hashes. Depth writes are disabled.
Every enabled stencil face has Keep for fail/depth-fail/pass, even with nonzero
write masks. Replace appears only when stencil is disabled. Therefore these
observed depth/stencil tests cannot write the attachment through fixed-function
operations. This says nothing about intervening draws, clears, or image stores.

Most frequent fragment hashes per batch:
- DC20EE7DD435191C: 22 alias bindings.
- 7AC93A4038DAA83B and 4AF532BBC61A0ADD: 16 each.
- 22FCBB94615A0B2F and 5B6D022CB5F280F9: 14 each.

The next implementation prerequisite is exact native content invalidation for
a reusable sampling snapshot, not disabling the depth/stencil tests. Current
MetalImage::mark_native_modified only tracks 3D/slice storage authority and is
a no-op for ordinary 2D images; it is not a content generation counter. Uploads
also set Coherent directly. All successful write owners must participate before
snapshot reuse is safe. Snapshotting bound-but-unused textures may cost work,
but does not justify removing dependencies; actual shader access analysis is
still needed to establish the benefit of any selectively optimized subset.

## Stopped Integration: Native Snapshot Copy Prerequisite

Before adding reuse/invalidation policy, implement a real independent native
depth/stencil image copy in metal_image.rs and test snapshot -> overwrite source
-> read both images, including mips/layers and stencil. No snapshot cache is
enabled by this primitive. Integration must account for its memory and connect
all producer invalidations; resource preparation alone is not a content version.

The independent copy primitive is now implemented as
MetalImage::create_sampling_snapshot. It preserves matching native depth/stencil
format, all levels/layers, and records through the existing blit encoder with
no CPU wait. Single-sample 2D depth/stencil only; no silent resolve. The native
test verifies mip 1 and two layers in both D24S8 guest layouts: record snapshot,
overwrite original, then read each independently. Old depth and stencil survive
in the snapshot while the original contains the replacement. Full release
video_core suite: 1786 passed, 3 ignored, Metal API validation, no Rust warnings.
Evidence: `../ruzu-diagnostics/metal-depth-pack-20260907.oA31lQ/snapshot-full-tests.log`.

No draw consumes it yet. Before enabling reuse, connect invalidation at:
- successful native/converted/depth-plane uploads and D32S8 transfer uploads;
- runtime image copies, reinterpretations, resolves and render blits;
- framebuffer clears (both load-action and masked/scissored shader paths);
- actual draw depth/stencil writes, using effective native state;
- graphics/compute writable image bindings, conservatively if conditional;
- recreation, rescale and deletion, with allocation accounting and deferred
  resource lifetime maintained.

Do not attach content invalidation blindly to prepare_image_view(true): both
Eden and the common Rust UpdateRenderTargets call that for every attachment on
every draw, including read-only depth. Existing mark_native_modified is only
3D/slice authority and must not be mistaken for complete producer tracking.
The GUI bundle has not been rebuilt for this primitive; no FPS gain claimed.

## Native Content Revision Wiring

MetalImage now tracks an allocation-local recording revision independently of
3D/slice storage authority. Saturation at u64::MAX returns None (uncacheable),
never a wrapped old revision. This is not a GPU fence. Cache locking remains
the serialization contract, and image identity must accompany any revision.
Never key a snapshot solely by a recycled base ImageId plus a revision value.

Wired conservative invalidation before native writes: uploads, converted planes,
D32S8 upload transfer, image copy/resolve, common render blit, writable graphics/
compute image bindings, draw attachments, draw-texture colors and both clear
branches. Render-target preparation retains its existing guest dirty behavior
without advancing the native content revision. Effective depth/stencil write
possibility gates draw depth invalidation; disabled stencil, Keep outcomes and
zero low-eight-bit write masks do not invalidate read-only depth.

Whole-image invalidation can be conservative for masks, layers, conditional or
culled draws, and a failed operation after recording begins. This cannot make a
stale snapshot valid. Snapshot caching/rebinding and native allocation accounting
remain unimplemented: do not enable reuse based on these counters alone.
Focused revision tests and full video_core release validation pass: 1789 tests,
3 ignored, Metal API validation enabled, no Rust warnings. Evidence:
`../ruzu-diagnostics/metal-depth-pack-20260907.oA31lQ/revision-full-tests.log`.
The native snapshot test also observes the revision change on a real upload.
No GUI rebuild or game run for this wiring yet; no barrier policy has changed.

## In-Flight Budget Reservation Prerequisite

Superseded below by a fixed native heap. The lease API and its tests were
removed once that allocator made separate per-resource byte reservations
unnecessary; the following records the evaluated intermediate design.

The scheduler now accepts allocation leases held by a command-buffer completion
cohort. Cache eviction alone cannot release such a reservation while a batch
still uses it. Repeated references to the same lease are deduplicated within
one batch; separate batches retain independent references. The completion
handler captures neither scheduler nor command buffer, avoiding a retain cycle.
Unsubmitted buffers release their cohort when destroyed without submission.

This is native Metal lifetime plumbing, not a completed snapshot budget. The
cache must still reserve bounded bytes before allocation, retain the reservation
alongside each cached image, and pass it to every batch reading or copying that
image. Actual native allocation sizing and cache/view integration remain next.
No draw uses this API yet, and no performance improvement is claimed.

Validation: two focused native tests and the full video_core release suite pass
(1791 passed, 3 ignored), with Metal API validation and no Rust warnings.
Log: `/tmp/metal-allocation-lease-full-tests.log`. No GUI rebuild in this slice.
Apple's heapTextureSizeAndAlign reports heap-backed texture size specifically;
do not assume it bounds an ordinary newTextureWithDescriptor allocation. The
native allocatedSize query reports actual resource bytes after creation. Resolve
that preallocation/accounting contract before enabling the bounded cache.

## Fixed-Heap Snapshot Cache

Implemented in metal_texture_cache.rs, with allocation/copy ownership remaining
in metal_image.rs. The runtime lazily requests one 64 MiB private automatic
heap, explicitly hazard-tracked. The native heap bounds snapshot backing storage
(subject to Metal page rounding), including retired copies held by in-flight
commands. It never grows, creates a replacement heap, makes images aliasable,
or waits for room. Native heap creation failure disables this optimization for
the runtime; texture allocation failure evicts cache owners and retries once,
then returns None to preserve the ordinary feedback path.

Entries pair a retained native source texture with its content revision. This
prevents pointer-reuse ABA; no base SlotVector index is used as identity.
Reusing an unchanged entry records no new copy. Writes create a new independent
snapshot. Whole-source retention can keep source allocations alive until cache
eviction; it is additional to the heap and is not claimed as part of its 64 MiB.
Source deletion integration remains desirable before draw activation.

The native packed-depth test now verifies cache reuse, revision invalidation,
fresh replacement pixels and preserved older pixels for both D24S8 orders,
mip 1 and two layers. Focused texture-cache tests pass. Full-suite validation
also includes heap exhaustion without submission and eviction while copies are
still unsubmitted. No game consumer or GUI rebuild yet, no barrier removed.

Next: create matching sampled views from snapshots, qualify read-only aliases
including writable storage bindings (not merely fixed-function depth/stencil),
and defer feedback-boundary decisions until substitution succeeds for every
relevant alias. Keep original attachments and guest tests intact. Compare the
same hall in a guarded release GUI run only after that integration is tested.

Full validation after removing the obsolete lease prototype: 1792 passed,
3 ignored, Metal API validation enabled, no Rust warnings. The bounded-heap
in-flight eviction test passes. Log: `/tmp/metal-snapshot-cache-full-tests.log`.
At the end of that slice, MetalStageTextureBinding stored only index and native
texture. Integration needs view identity/type and sampled-vs-written metadata,
or an equally verified native-view reconstruction; do not infer aspect/swizzle
from a root texture or assume every binding is read-only.

## Prepared Binding Source Metadata

metal_graphics_pipeline.rs now carries typed source metadata from descriptor
preparation through reflection into each MetalStageTextureBinding. Sampled
images retain ImageViewId and TextureType; storage images also retain is_written;
buffer views retain their write intent separately. Array element ordering and
native texture/sampler indices are unchanged, and metadata remains present for
null native bindings. A focused reflection test covers all three source kinds.

No substitution is activated by this change. Next, construct snapshot views
using the original ImageViewInfo and stable ImageViewBase rather than the native
root format alone, then gate the feedback boundary on successful read-only
substitution across all stages. The common cache locks must cover use of these
IDs; retaining metadata is not permission to dereference a recycled slot later.

Full release video_core validation: 1793 passed, 3 ignored, Metal API validation,
no Rust warnings. Log: `/tmp/metal-binding-source-tests-fixed.log`. An initial
test-only enum spelling error was corrected before this run. No GUI rebuild.

## Snapshot Sampled Views

retained_sampling_snapshot_view validates live view/image slots, gets an
allocation-revision snapshot and constructs MetalImageView using the existing
ImageViewBase/ImageViewInfo. The temporary wrapper is dropped within the base
borrow; only a retained native texture escapes. Null/missing/buffer/unsupported
or exhausted-heap cases return None, while native copy/view errors propagate.

The runtime caches up to 64 native views keyed by retained original-view and
snapshot-root identities. Cached native handles retain the allocations behind
both pointer keys; no recycled slot ID or dangling Rust base pointer is cached.
Heap exhaustion clears these view owners before retrying image allocation.
GPU-retained views/textures are never made aliasable to reclaim room.

The focused native test passes for depth and stencil views, nonidentity swizzles,
mip 1/layer 1, independent backing storage, unchanged native view reuse and a
retained native view remaining usable after removal of its base slot. It compares
properties, not shader sampling output; packed snapshot pixels are covered by
the separate upload/overwrite/download test. No draw substitution enabled yet.

Full release video_core suite: 1794 passed, 3 ignored, Metal API validation,
no Rust warnings. Evidence: `/tmp/metal-snapshot-views-full-tests.log`.

## Draw Integration

The live Metal rasterizer now collects the common feedback request at its
original point, but makes the native encoder-boundary decision after resolving
the effective depth/stencil state. If that state may write, it keeps the ordinary
boundary. Otherwise MetalPreparedGraphics validates bindings across all five
stages and substitutes sampled views of the active depth image with snapshots.
Storage aliases (including read-only storage) and unresolved bound view IDs
reject the optimization. Buffer textures cannot alias private depth allocations.

Replacement handles are collected before publishing any of them. If any snapshot
is unavailable, no prepared binding changes and the ordinary boundary is kept.
Creating a new snapshot itself ends the incompatible encoder through the existing
blit path; reusing a current snapshot does not. Original attachments and effective
depth/stencil tests remain bound. The existing draw cache locks cover view IDs
and snapshot lookup throughout. Guest preparation order and common feedback
predicate/cache are unchanged; no idle, draw skipping or queue-limit increase.

The alias profiler currently observes bindings after substitution, so reduced
alias counts now indicate independent sampled storage, not fewer guest aliases.
Compare actual render-end/copy counts and complete GPU durations in the next
guarded hall run. The GUI bundle still predates this integration.

Full release video_core suite: 1795 passed, 3 ignored, Metal API validation,
no Rust warnings (`/tmp/metal-depth-feedback-integration-final-tests.log`).
The native test confirms fixed-function-write refusal, cross-stage storage
alias refusal without partial binding mutation, then read-only substitution.

GUI release build started; evidence directory:
`../ruzu-diagnostics/lm3-depth-snapshots-20260907.Zi5p0C/`.
Its watch.py/config are copied from the last guarded hall baseline, with no
increased memory/queue/watchdog limits. No new game run yet. Before launch,
finish the build, recreate/sign-check the bundle preserving its MoltenVK,
and verify no other ruzu instance. Build execution handle: session 1321.

## Guarded GUI Result (2026-09-07)

Release build/bundle complete, signature verified. GUI UUID
C7ECBE51-053E-3D99-A857-482019A619FE. MoltenVK SHA256 unchanged:
0995b17b030c01e991e2c36b48a953d8a4fdb6c4df1b9dcaa46b6d9e08612855.

First run (`lm3-depth-snapshots-20260907.Zi5p0C`) is NOT a performance result.
The guest called SetTerminateResult(UserlandAssert) and svcBreak near 25s,
before the first automated A, following OpenAudioOut. The loading animation
continued, but the hall was never reached. No sampled batch had a nonzero
feedback request; this is not exhaustive proof that no snapshot was used.
Cause unproven. Peak footprint 5.13 GiB, swap growth zero, disk drop 21 MiB.
Stopped by the 120s guard, SIGTERM then SIGKILL, not an emulator process crash.

Unchanged-binary retry: `../ruzu-diagnostics/lm3-depth-snapshots-retry-20260907.FEZfxR/`.
No svcBreak; scene-100.png and scene-110.png show the hall with complete geometry.
Compared visually with the baseline scene-110.png, no new missing geometry or
gross artifact observed; this is not pixel-exact or temporal-flicker validation.
Median GUI FPS at 110..120s is 13.961 (9 samples, range 12.90..15.35), versus
12.908 (10 samples) for the last stencil-detail baseline. An earlier baseline
already reached 13.96, so repeatability of FPS gain is not established.

Five fully sampled hall batches (12059, 12440, 12728, 12826, 13680) have
159/160 feedback requests with no omitted keys. GPU duration median 68.89 ms
(range 67.77..76.15), versus baseline 77.54 ms (73.72..83.34, four batches).
Actual render endings median 508 (492..519), versus 601.5 (597..606).
Remaining native sampled aliases: 8..23, versus 149; these are post-substitution
counts, not fewer guest bindings. At ticks 12440/12826, fallback feedback ends
are 18 and the snapshot-copy site ends one render pass. Remaining alias bindings
at those ticks all have barrier=false in the common predicate.

Peak footprint 8.51 GiB, global swap growth 909 MiB, disk drop 16 MiB. No resource
guard fired. The supervisor stopped at 120s, SIGTERM then SIGKILL; no ruzu instance
remains. Neither lower footprint nor FPS differences prove a general memory or
performance improvement from a single run. 20 FPS remains unmet.

Next main costs: roughly 186..187 buffer-copy boundaries and 101..103 uint8
conversion boundaries in the detailed hall batches. The index conversion cache
already hits about 76% of requests, so investigate the remaining invalidations
and upload eligibility rather than assume caching is absent. The paused upload
cohort lifetime contract in METAL_UPLOAD_PREFIX_STATE.md still applies. Other
title regression checks and longer temporal rendering validation remain open.

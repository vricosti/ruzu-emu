# Metal Texture Cache Garbage Collection Prerequisite

## 2026-09-08 Guarded GUI Result With GC

Release GUI UUID 5A28FD11-5991-3D87-BCC4-DDCB86C917B2; build and signed
bundle verification passed without warnings. MoltenVK SHA256 unchanged:
0995b17b030c01e991e2c36b48a953d8a4fdb6c4df1b9dcaa46b6d9e08612855.
Evidence: ../ruzu-diagnostics/lm3-gc-native-20260908.nkaC84/.
Session was initially locked; no launch until user unlocked it. Added a
startup locked-console rejection to the copied watchdog, leaving all memory,
swap, disk, native GPU progress and 120-second limits unchanged.

Run completed at 120.26s; supervisor stopped it intentionally, no instance
remains. Hall capture scene-110.png visibly contains characters, interior
geometry and effects without the previously observed giant triangles.
Peak physical footprint 7.353 GiB; swap growth 0 MiB relative to first sample;
free-disk decrease 11.8 MiB. Final cache: 1034 images, root 1,718,137,728 bytes,
slices 6,422,528 bytes, native device allocations 2,193,473,536 bytes.
No Metal GC download errors or guest abort; only expected fastmem-unavailable
message for host 16K pages. Compared with the previous upload-prefix run:
peak 9.839 GiB, final roots 5,028,132,736 bytes. Runs differ in live image
populations; do not treat these as an exact per-allocation controlled delta.

Hall 110..120s median game FPS (9 samples): 15.8708, versus 15.6220 previous
upload-prefix and 15.9435 older non-prefix run. NO meaningful FPS improvement
established. GC solved the missing eviction path and reduced observed memory
pressure; it did not solve the main GPU bottleneck.
Hall tick 13989: native command 62.520ms; stage totals blit 0.733ms, compute
1.928ms, vertex 14.834ms, fragment 45.949ms (stages may overlap, not additive).
409 render passes measured. Largest sampled mixed pass has 251 draws and
vertex/fragment 5.535/5.663ms. Remaining breaks include 125 buffer-cache
encode-copy sites, 50 ordered uploads, 41 blit helper sites, 33 conditional
resolves. Next investigate render-pass fragmentation/tile cost on the same
hall, not further GC prerequisites or an unsupported 20-FPS claim.

## 2026-09-08 Common LRU Collector Wiring

Metal TickFrame now invokes the common collector before delayed destruction
and runtime/frame advancement. It uses the existing estimated-memory budget
(HAS_DEVICE_MEMORY_INFO=false), unchanged from Eden's corresponding branch.
The real callback obtains FullDownloadCopies, downloads native contents and
finishes before returning bytes; common GC then swizzles/writes guest memory
and unregisters/deletes the image. Missing native storage or transfer failure
returns false and retains the GPU-modified image instead of fabricating data.

Integration test renders a native red clear, runs tick-frame GC, checks exact
guest writeback and delayed retirement, verifies a recently touched image is
retained, and injects an undersized staging range to check real transfer-error
retention. Initial test failed because its image creation lacked bound channel
GPU memory; fixed the fixture by binding/mapping a real MemoryManager before
image registration, then observing the common CPU-address writeback adapter.
Full release validation: 1822 passed, 3 ignored, no warnings, Metal API
validation enabled (/tmp/metal-gc-wiring-tests.log).
GUI has NOT been rebuilt/run with GC yet. Next: bounded memory-monitored
Luigi hall run using the existing watchdog; do not raise its resource limits.

## 2026-09-08 X8D24 Transfer Prerequisite

X8D24 upload now normalizes the low 24 depth bits to native Depth32Float;
download quantizes to nearest UNORM24 and writes zero to the unused high
eight bits. No stencil allocation or transfer is involved. Copy traversal,
checked layout, row/layer padding and offset handling are mechanically shared
with the other converted uncompressed formats (helpers renamed accordingly).

Tests exercise native mip-1, array layers 1..2, pitched rows and independent
native float inspection, plus all 16777216 UNORM24 values for quantization
round-trip and invalid-range atomicity. Full release Metal-validation suite:
1821 passed, 3 ignored, no warnings (/tmp/metal-x8d24-transfer-tests.log).
No GUI rebuild/run yet. GC is still not connected: next slice
must provide the real single-sample downloader, test writeback/eviction and
failed-download retention, then run the guarded GUI memory/performance test.

## 2026-09-08 A5B5G5R1 Native View Prerequisite

Invalidated assumption: A5B5G5R1 needs expanded RGBA storage to preserve its
five alpha bits. Eden stores R5G5B5A1-packed words unchanged and applies
SwapSpecial to sampled views (R<->A, G<->B), leaving constant sources and
render-target views unchanged. Metal A1BGR5Unorm provides this same packed
storage. The Metal view now composes that permutation with guest swizzles;
native upload/download need no conversion and preserve every guest bit.

The exhaustive GPU test now covers A5B5G5R1 as well as both 1555 formats,
using actual MetalImageView handles and independently checking all four
components for every 16-bit word, plus native CPU download round trips.
Full release validation: 1819 passed, 3 ignored, no warnings, Metal API
validation enabled (/tmp/metal-a5551-transfer-tests.log). No GUI rebuild/run.
X8D24 is the remaining converted-format prerequisite before GC wiring.
No gameplay/FPS measurement in this slice.

## 2026-09-08 Packed 1555 Transfer Prerequisite

Invalidated assumption: Metal A1BGR5Unorm does NOT preserve bit-15 alpha.
MoltenVK's format table maps it to R5G5B5A1_PACK16; BGR5A1Unorm maps to
A1R5G5B5_PACK16. Corrected A1R5G5B5 native mapping and A1B5G5R5 converted
mapping to BGR5A1Unorm. The latter now shares the packed16 transfer path
with B5G6R5, swapping red/blue while preserving alpha and green.

Added a native GPU test reading all 65536 words for BOTH formats through
a compute texture read, independently checking RGBA component values and
also checking CPU-facing download round trips. Byte round trips alone
would not detect the previous alpha/channel interpretation error.
Full release validation: 1819 passed, 3 ignored, no warnings, Metal API
validation enabled (/tmp/metal-a1555-transfer-tests.log). No GUI rebuild/run.
Remaining converted prerequisites: A5B5G5R1 and X8D24. GC remains disabled;
no game performance or memory improvement is claimed for this slice.

## 2026-09-08 G4R4 Transfer Prerequisite

G4R4 upload/download now expand to native RG8 and repack guest nibbles. Low
nibble is R, high nibble G, matching Eden's R4G4 format plus SwapGreenRed view
mapping. Expansion multiplies each nibble by 17 exactly; download uses nearest
UNORM4 quantization for arbitrary GPU-written RG8 values. The checked layout
calculation and transfer traversal are mechanically shared with RGB32 in the
same texture-cache owner; RGB32 still copies its float payload bits unchanged.

Native test covers all 256 guest values across two mip-1 array layers with
padded rows, guest/staging offsets and guards. CPU test checks all 65536 native
RG8 byte pairs against normalized nearest quantization. Full release suite
running in /tmp/metal-g4r4-transfer-tests.log. Remaining converted prerequisites:
A1B5G5R5, A5B5G5R1 and X8D24. GC is not enabled yet; no runtime gain claimed.
Verified full release video_core suite: 1818 passed, 3 ignored, no warnings,
Metal API validation enabled. No GUI rebuild/run for this prerequisite.

## 2026-09-08 RGB32 Transfer Prerequisite

Implemented both directions for R32G32B32Float in the live upload switch and
single-sample CPU download wrapper. MetalImage owns the native RGBA32 readback;
the cache packs/unpacks RGB32 guest bytes. The conversion never evaluates the
float values: three words are copied unchanged and upload adds 0x3f800000 alpha.
Copies use checked sizes and preserve row/layer padding and unrelated output
bytes. Invalid copies are validated collectively before output writes.
Native tests exercise mip 1/layers 1..2, nonzero guest/staging offsets, NaN
payloads and negative zero, and confirm native transfer methods do not submit.
Full release suite running in /tmp/metal-rgb32-transfer-tests.log.
Remaining converted-format prerequisites: A1B5G5R5, A5B5G5R1, G4R4, X8D24.
No GC callback was installed yet; no new GUI memory/performance claim.
Verification completed: 1816 passed, 3 ignored, no warnings, full release
video_core suite with Metal API validation. GUI remains the previous build.

## 2026-09-08 Eligibility Audit Correction

Hypothesis invalidated: implementing MSAA downloads is NOT a prerequisite for
Eden's current LRU collection. Re-read image_base.h/.cpp IsSafeDownload and
the Rust counterpart: both reject num_samples > 1 before the GC downloader.
Earlier requirements below overstated that dependency. Do not weaken/change
the common eligibility policy merely to use the new Metal download helper.

The separate native expansion pass started during that audit is implemented
and tested in metal_blit_helper.rs: unlike an averaged resolve, it preserves
each sample in the expanded grid, matching Eden CopyMSAA and
convert_msaa_to_non_msaa.frag. Native test writes four distinct samples and
checks sample identity, source/destination offsets, array-layer/mip views,
unchanged borders, no submit, and rejected sample-count/bounds mismatches.
Full release video_core: 1814 passed, 3 ignored, no warnings, Metal validation
enabled (/tmp/metal-msaa-expansion-tests.log). Not yet connected to downloads
or the live GC; no FPS or memory improvement is claimed.

Actual next prerequisite: complete the remaining converted single-sample
formats admitted by IsSafeDownload. Metal's table still marks A1B5G5R5,
A5B5G5R1, R32G32B32Float, G4R4 and X8D24 as converted, but their transfers
are not covered by the existing upload switch or new CPU download wrapper.
Audit the native A5B5G5R1 representation in particular: it currently chooses
A1BGR5 despite needing five alpha bits. Implement conversion symmetrically
before wiring the common collector. B5G6R5, D24S8, D32S8 and byte-compatible
single-sample transfers are already verified; no always-false GC callback.

## 2026-09-08 Measurement Blocker

Upload-prefix attribution run lm3-copy-attribution-20260908.aCxqvK was stopped
by the unchanged 10-GiB physical-footprint guard at 91.158s, before collecting
a hall batch. Peak sampled footprint was 10.379 GiB; native image roots reached
5.460 GB across 2003 live images, with zero sentenced images/views/framebuffers.
The last two valid hall runs had about 3.023 GB and 5.028 GB of live roots.
These differing populations prevent a controlled upload-only FPS comparison.

This is not proof of a new prefix leak: the ownership visible in the reports
is the live texture cache, not unretired cohorts or sentenced rings. No further
runtime with a raised watchdog limit. Resume the download prerequisites below
before enabling safe LRU collection and retrying performance measurement.

## Current Finding

MetalTextureCache::tick_frame advanced only frame_tick and the staging runtime.
Unlike Eden TextureCache<P>::TickFrame, it never ticked sentenced images/views/
framebuffers or async decode/unswizzle. Retired cache payloads remained owned
indefinitely. The retirement/async ordering is now restored separately, with
a native regression that releases every ring while image commands are recorded
but not yet submitted. Metal command buffers use retained references.

MetalRasterizer::tick_frame now uses Eden's separately scoped texture and
buffer cache locks, matching CPU invalidation serialization. Full release
video_core suite: 1782 passed, 3 ignored, Metal API validation enabled, no warnings.
GUI runtime memory/performance validation remains pending.

## Download Prerequisite Progress

2026-09-08: B5G6R5 inverse transfer implemented in the existing image/runtime
owners. Native transfer validates the format and copy ranges, synchronizes
authoritative slice storage, and records a blit without submitting or waiting.
CPU writeback waits before reading the download lease, then swaps red/blue
fields only for actual copied texels. Row/layer padding and surrounding bytes
remain untouched. Invalid ranges are checked before guest output changes.
Native tests cover mip 1, array layers 1..2, padded rows/images, nonzero buffer
and staging offsets, and fixed primary-color bit patterns. Full release suite
is running in /tmp/metal-b565-download-tests.log. This is a prerequisite only:
the common LRU collector is not yet enabled, and no runtime memory/FPS benefit
is claimed from this isolated transfer implementation.

The B5G6R5 slice passed the full release suite: 1813 passed, 3 ignored, no
warnings with Metal API validation. Single-sample CPU download now dispatches
to the existing D32S8 transfer helper, native byte-compatible download, and
converted D24S8/B5G6R5 paths. Native/D32S8 staging is initialized from the output
to preserve gaps before returning the completed bytes. Extended native tests
exercise these runtime branches; rerun in /tmp/metal-single-sample-download-tests.log.
Multisample images are explicitly outside this method's contract. It is not a
substitute for implementing resolve/download before wiring the common GC.
Extended full release suite verified: 1813 passed, 3 ignored, no warnings,
Metal API validation enabled. GUI has not been rebuilt or rerun for these
download prerequisites; no change to the 10-GiB runtime guard.

The runtime now exposes DownloadStagingBuffer through the existing download
pool (including deferred leases), not a temporary standalone allocation.
MetalImage now records downloads of converted D24S8 depth and stencil planes,
selecting native storage after synchronizing any authoritative slice storage.
It validates format, copy counts and both destination ranges before encoding.
No submit or CPU wait is introduced by this transfer method.

Native test passes for both guest D24/S8 orderings, mip 1, two array layers,
nonzero download offset and untouched prefix/suffix guards. Full video_core
release validation: 1783 passed, 3 ignored, no warnings, Metal API validation
enabled. Evidence: `../ruzu-diagnostics/metal-depth-download-20260907.Q9aouy/tests.log`.
Packed guest D24/S8 reconstruction is now implemented in the native cache
owner, with a CPU-facing runtime download method. It preserves row/layer
padding and uses clamped nearest UNORM24 quantization with a double-precision
product. The plane layout is shared mechanically with upload. Exhaustive
24-bit roundtrip and native packed-output tests pass. Full video_core release
suite: 1785 passed, 3 ignored, Metal API validation enabled. Evidence:
`../ruzu-diagnostics/metal-depth-pack-20260907.oA31lQ/tests.log`.
The native test verifies both packing orders, mip 1, two array layers and
nonzero offsets; the CPU test also checks untouched row/layer padding.
Other converted formats and the full GC downloader remain pending, not
implicitly enabled. The GUI has not been rebuilt for these changes.

## Remaining Prerequisite Before Enabling LRU Collection

The Metal wrapper has no RunGarbageCollector call and no GC image downloader.
Do not insert a downloader that always returns false, discard GPU-modified
images, or force a global idle every frame. Implement the real download path
first in metal_texture_cache.rs / metal_image.rs, corresponding to Eden's
TextureCacheRuntime::DownloadStagingBuffer and Image::DownloadMemory:

- Preserve native/slice storage authority when selecting the downloaded image.
- Allocate a completion-safe download staging buffer and wait only when CPU
  writeback actually needs its contents, as upstream does for GC.
- Handle guest/native format conversion symmetrically with upload: B5G6R5,
  D24/S8 ordering and normalization, D32/S8 aspect planes, compressed fallbacks
  and supported sample counts must be checked rather than assumed.
- Supply the real downloader to the existing common collector so guest
  writeback, dirty tracking, LRU selection and deletion ordering remain owned
  by texture_cache/texture_cache.rs.
- Reuse the existing estimated-memory branch initially unless real native
  budget reporting is implemented and tested; do not announce fake support.
- Test preservation of GPU-modified pixels and eviction of old clean images,
  including invalidation/destruction while commands remain in flight.

After that prerequisite, restore full TickFrame ordering: usage/GC, sentenced
rings, async work, runtime, frame counter. Measure the hall under the existing
memory/swap guard. The prior swap event is not proven to be entirely caused by
the unadvanced rings; no performance or memory improvement is claimed yet.

## 2026-09-07 Runtime Check

The rebuilt GUI reached the hall in a 120s bounded run with the retirement fix.
Evidence: `../ruzu-diagnostics/lm3-retirement-check-20260907.EJCjDT/`.
Peak process footprint: 8.98 GiB; global swap growth: 2512 MiB; maximum disk
free-space drop: 2053 MiB. No resource guard fired. The time limit terminated
the application (SIGTERM followed by SIGKILL after three seconds), not a
spontaneous crash; no ruzu process remained. Global swap is not process-specific.
The result does not establish a memory improvement over the earlier 9.11 GiB
hall run, nor does it resolve the remaining collection prerequisite.
GUI UUID: 6E029517-B6CE-3F32-8487-B63BC55D7975, release build, signature verified.

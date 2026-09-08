# Metal index conversion investigation

## 2026-09-08: measured generation churn; range refinement not implemented

Objective remains correct native geometry rendering and measured performance,
not merely a higher conversion-cache hit rate.

Release GUI UUID 417370A1-BE3D-3183-935A-4F140452D78C. Full video_core tests
1796 passed, 3 ignored with Metal API validation before the two attempts.

Evidence: ../ruzu-diagnostics/lm3-uint8-retry-20260907.MUrdP8.
Unlike the preceding attempt this run did not encounter the loading svcBreak.
Capture 70 shows save selection; capture 90 still shows loading. The supervisor
stopped at 94.25 seconds for disk resource limits, not GPU failure. No hall FPS
comparison is established by this run.

Last four profiler intervals:

| requests | hits | discarded entries | requested key discarded | inserted | fallback |
| --- | --- | --- | --- | --- | --- |
| 6975 | 5098 | 1871 | 20 | 1877 | 0 |
| 6975 | 5108 | 1872 | 25 | 1867 | 0 |
| 6975 | 5267 | 1698 | 22 | 1708 | 0 |
| 7905 | 5975 | 1920 | 24 | 1930 | 0 |

Uncacheable count is zero. Capacity exhaustion is not causing these misses.
Whole-allocation generation invalidation discards many cached ranges at once.
This does NOT prove the discarded bytes are unchanged: the current-key metric
only counts the first request that observes a changed generation.

## Next correctness prerequisite

Before attempting range-aware reuse, preserve the modified range from common
BufferCache::mark_written_buffer through the backend notification. Currently
only set_write_tick reaches MetalBuffer, which marks its entire generation.
Eden buffer_cache.h MarkWrittenBuffer has the same tick notification and keeps
range information in separate guest-memory trackers. Metal's derived conversion
cache is an additional native requirement, not an Eden cache port.

CPU writes, native copy destinations and query-result writes also need range
tracking. Unknown writes must conservatively invalidate everything. GPU-ordered
content revisions must not be confused with completion ticks. Bound tracking
storage, preserve same-submission repeated writes, test disjoint and overlapping
updates and native converted pixels before enabling reuse. No CPU readback or
global GPU wait is acceptable for this optimization.

## Resource interruption

At 91..94 seconds free disk space fell from about 31 to 22 GiB; after ruzu was
stopped it stabilized at 6.7 GiB. Process peak footprint was about 8.7 GiB;
global used swap increased by roughly 0.5 GiB during the run. Run artifacts total
5.6 MiB, /cores is empty, no Data-volume APFS snapshots were listed. The VM APFS
volume consumes 6 GiB total. Cause of the much larger Data-volume growth remains
unidentified. Some protected directories could not be inspected. No files were
deleted, no resource limits raised, no additional GUI runs/builds started.
Recheck available disk space and identify the writer before further runs.

## Prepared prerequisite (initially not built)

Common MarkWrittenBuffer now forwards tick plus buffer-relative offset/size to
BufferCacheBuffer::mark_written_region. Its default calls set_write_tick, so
Metal still advances its whole-allocation generation on every notification and
other backends preserve the original tick behavior. OpenGL remains excluded at
the same call-site condition. Relative subtraction wraps rather than adding a
new debug panic for a malformed address; native range consumers must validate
the range and invalidate everything if it is outside the allocation.

Added common tick tests and a native test that repeated same-tick and invalid
range notifications still invalidate globally. These new tests have NOT run:
disk remains at 6.7 GiB and no heavy build or game run was started this turn.
Existing 1796-pass evidence predates this prerequisite. diff --check passed.

Range journal/storage and range-aware cache reuse are not implemented. Finish
verification of this notification slice before wiring those consumers.

## 2026-09-08: notification prerequisite verified

The preceding pending-test status is superseded. A guarded incremental release
video_core test run completed: 1798 passed, 3 ignored, no warnings, Metal API
validation enabled. Log: /tmp/metal-write-region-tests.log. Both new notification
tests passed. Guard preflight required 6 GiB free; it would terminate the build
group below 4 GiB free, on more than 2 GiB growth or after 480 seconds. No guard
fired; net disk consumption was 6,926,336 bytes. No GUI build or game launched.

Next slice can implement the native bounded write journal and tests. Only
allocations used for derived index conversions should need history storage.
History loss, unknown writes, invalid ranges and generation saturation must
invalidate conservatively. Do not increase cache budgets. Current GUI binary
still predates the notification prerequisite and must be rebuilt before its
next runtime validation; disk space remains too low for guarded game runs.

## Bounded history prerequisite implemented

MetalBuffer now has a lazy OnceLock-protected history of at most 64 writes.
CPU writes, native buffer copies, common-cache GPU writes, query-result copies
and uint8 output conversion writes report their existing destination ranges.
Unknown writes retain whole-buffer invalidation. Missing history, invalid ranges,
revision gaps/out-of-order arrival, saturation and eviction cannot certify reuse.
Live uint8 cache still does not enable or consume this history, so no caching
behavior has changed. Next integrate enable_write_history before cache use and
prune entries by region_unchanged_since on generation changes, preserving byte
and entry accounting and GPU ownership. Test converted pixels after disjoint,
overlapping, repeated same-tick GPU copies, unknown writes and history eviction.

Initial full suite 1800 passed/3 ignored; final out-of-order refinement is being
retested in /tmp/metal-write-history-final-tests.log. Disk space recovered to
32 GiB during the initial suite without any deletion by the agent; cause of the
temporary 25 GiB consumption remains unknown. Keep resource guards for game runs.

Final refinement verified: /tmp/metal-write-history-final-tests.log reports
1800 passed, 3 ignored, no warnings with Metal API validation. The pending-test
status above is superseded. Enable/prune integration and native reuse tests can
now proceed; no new GUI binary or game run was produced for this slice.

## Range-aware conversion cache integrated

The live uint8 path now enables history and, at generation changes, retains
only source ranges certified unchanged. Removed allocation sizes and entry
counts are subtracted exactly; retained ranges advance to the validated
generation. Unknown writes and history eviction still reconvert. Budgets and
GPU lifetime/recording order remain unchanged. The profiler now counts actual
removals rather than all entries present before a generation change.

Native regressions check disjoint CPU uploads add no new work, overlapping GPU
copies replace only the affected conversion, old and new converted data stay
correct in one submission, 0xff expands to 0xffff, accounting balances, and
history eviction/unknown writes force reconversion. Full release video_core
suite: 1802 passed, 3 ignored, no warnings, Metal API validation enabled.
Log: /tmp/metal-index-range-integration-tests.log.

Release GUI build and guarded run evidence directory:
../ruzu-diagnostics/lm3-index-ranges-20260908.bX9pfl.
Runtime performance/visual validation pending. Do not claim a gain from tests.

### Runtime attempts: audio initialization gate, not performance evidence

Built/signed GUI UUID 30E754D2-DAF6-3C75-8631-E7D024CBF608, unchanged MoltenVK
SHA256 0995b17b030c01e991e2c36b48a953d8a4fdb6c4df1b9dcaa46b6d9e08612855.
Both bX9pfl and unchanged-binary retry
../ruzu-diagnostics/lm3-index-ranges-retry-20260908.qNdAq0 abort near 27 seconds
before input. Supervisor detects GAME ABORTED and stops them; no hall samples.
Retry enables existing RUZU_DUMP_BREAK_STACK only. Captured SDK offsets:
0x37EBA8, 0x1801E8, 0x17ED3C, 0x17EE08, 0x17EEB0, 0x9777C;
main offsets 0xA28C60, 0x9BCDC8, 0x9BC94C, 0x9D47AC, 0x9D4298.

These match the previously identified audio SystemEvent assertion path in
GEOMETRY_SUPPORT.md (Guest assertion identified / Audio initialization hypothesis
invalidated by IPC/callback traces). Historical initial-allocation-failure
hypothesis remains invalidated; do not reintroduce it from the second open.
Current logs select Cubeb audiounit-rust, open DeviceOut then retry one second
later. IORegistry currently reports CGSSessionScreenIsLocked=Yes. Prior evidence
linked this condition to missing sustained audio callbacks, but the current run
does not independently prove that cause. Need an unlocked-session control before
more runtime attempts; user asked to unlock. No assertion bypass/audio clock
substitution applied. Both app processes are stopped; disk is back above 30 GiB.

### Producer audit completed while awaiting unlocked-session control

Partial runtime clear now has native old/new index readback and disjoint reuse
coverage. Reviewed immediate writes, common writable bindings, staging/copy
paths, query result copies and separate visibility-bank direct writes; no extra
missing notification found in those reviewed paths. No production change added
by the audit. Full release suite 1803 passed, 3 ignored, no warnings, Metal API
validation, /tmp/metal-index-clear-final-tests.log. Current GUI production code
still matches the range-aware version already rebuilt; only a test was added.
Do not stack the upload-prefix optimization before runtime validation of the
current range-aware cache. Console remained locked on recheck; no game launched.

### Unlocked-session control: resource guard before hall validation

Evidence: ../ruzu-diagnostics/lm3-index-ranges-unlocked-20260908.QyBURw.
Same release GUI UUID 30E754D2-DAF6-3C75-8631-E7D024CBF608; no production
changes or rebuild. IOConsoleUsers no longer reported a locked session.
The prior audio assertion did not recur and the game progressed through save
loading. This supports a session-dependent failure, not proof of its cause.

The supervisor stopped the application at 91.21 seconds on footprint greater
than 10 GiB: peak 10.0717 GiB, global swap growth 1229.5 MiB, maximum observed
free-disk drop 27.125 MiB. SIGTERM then SIGKILL were supervisor actions, not a
spontaneous host crash. No ruzu process remained. Captures 70/80 exist, but no
completed hall capture; do not compare loading FPS against the hall baseline.

Last logged uint8 interval: 6510 requests, 5866 hits, 627 invalidated entries,
644 insertions, zero fallback/uncacheable; 90806 cached bytes in 341 entries.
This is about 90 percent reuse during this interval only, not a measured hall
performance gain. Native device allocations were about 6.05 GB, common buffer
allocations about 183 MB. Upload staging had fallen from 384 MB to 90 MB with
its logged entries reusable. These counters do not identify the footprint
increase's cause; the tiny converted-index cache alone cannot account for it.

Runtime validation remains incomplete. Preserve the safety thresholds and
investigate the memory peak/remaining texture-collection prerequisites before
another attempt; do not stack upload-prefix changes or assert a leak from this
single transition peak.

### Successful guarded hall validation with memory attribution

Evidence: ../ruzu-diagnostics/lm3-texture-memory-20260908.s0Ol6W.
Release GUI UUID 57B1A6C4-6737-3F98-B121-9558ECE7D7F0; only gated texture
memory attribution added since the previous attempt. MoltenVK hash unchanged.
Full video_core release suite: 1803 passed, 3 ignored, no warnings, Metal API
validation; /tmp/metal-texture-memory-profile-tests.log. Signed GUI rebuilt.

Native scene-100/110 captures inspected: hall geometry present, no giant
triangles seen. Supervisor reached 120 seconds, then stopped the app; no
remaining instance. Peak footprint 8.0841 GiB; global swap growth 142.12 MiB;
maximum disk drop 9.18 MiB. Previous 10 GiB peak did not recur. This does not
prove absence of a leak; between runs disk free space increased externally
from 31 to 98 GiB without agent deletion.

Last nine FPS samples (110-120s): median 15.9435, range 14.8520-16.9644.
Prior depth-snapshot baseline median 13.9611. Same hall but camera/animation
and workload not frame-identical, so this is an observed improvement, not a
controlled 14-percent benchmark claim. Twenty FPS remains unverified.

Representative complete hall batches 12328/12419: 46 uint8 conversions and
36/37 corresponding render breaks, versus 209/103 in the previous baseline's
batch 12440. Later scene workload also changes; do not pool partial/smaller
batches as identical frames. Batch durations 12328/12419/12836/13336/13764:
65.456/62.625/65.059/61.777/60.835 ms. No omitted coverage entries.

Texture attribution near hall: 1435 live images, about 3.023 GB root storage,
6.95 MB slice storage, zero retired image/view/framebuffer entries, about
3.498 GB device allocation. This run does not implicate delayed ring growth.
The index range optimization now has native tests and visual runtime evidence.
Next performance slice can resume METAL_UPLOAD_PREFIX_STATE.md: uploads still
break render encoders (128/135 eligible-upload breaks in batches 12419/12836),
but moving them requires the recorded dependency/lifetime prerequisite, not
an unconditional reorder or separate early-retiring submission tick.

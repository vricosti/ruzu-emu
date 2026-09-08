# Metal shader cost investigation

## 2026-09-08: offline reconstruction of expensive small passes

The previous clear-encoder reuse run measured 15.98454 FPS in the stationary
hall (previous comparable run 15.92778). There is no demonstrated FPS gain from
that slice and no demonstrated hardware limit. The last GUI remains unchanged
by this investigation; all new code is in an ignored test.

The profiler reported fragment intervals of 1.28-3.02 ms for several 64x64
passes (tick 14135 in lm3-clear-reuse-20260908.nVp56r). These are stage intervals,
not exclusive ALU times: dependencies and overlap prevent adding them or
attributing the entire interval to a single shader in a mixed pass.

## Verified shader properties

Rebuilt nine matching graphics pipeline variants from a private copy of the
current Metal disk cache, using the same translation and compilation functions
as the live backend. Original cache was not modified. Artifacts are under
`/tmp/metal-hot-shaders-20260908`; execution log is the sibling `.log`.

| Fragment guest hash | Source lines | FP helper call sites | Loops | Texture operation sites |
| --- | ---: | ---: | ---: | ---: |
| 1560DB60DF215EB3 | 2694 | 443 | 2 | 12 |
| E4B0CB56B5BD79D1 | 2816 | 468 | 2 | 13 |
| 5E6A495D44278F83 | 2764 | 453 | 2 | 13 |

These are static source counts, NOT executed instruction counts. Branches and
loop trip counts depend on runtime uniforms/textures, which are not in this
cache. Each uses a dynamically indexed array of 42 depth-array textures and
samplers, comparison samples/gathers, and lighting arithmetic. The shared
vertex shader is B200E98022015F82. The fragment shaders have no depth output.

## Hypotheses, including invalidated assumptions

- Invalidated: a 64x64 target implies a trivial copy/clear shader. These are
  substantial guest lighting/shadow programs, not helper clears.
- Unproven: the `[[clang::optnone]]` FP helper calls are expensive native calls
  or cause register spills. There are many source call sites, but no native
  assembly or isolated cost measurement yet. Do not remove them merely on this
  basis; they preserve guest no-contraction behavior.
- Unproven: depth sampling / dynamic indexing / loop trip counts dominate.
  Static MSL alone cannot distinguish these from upstream GPU dependencies.
- Not established: declared local storage indicates large actual spill usage.
  The 388-word local array in 1560DB60DF215EB3 only accesses word zero; the native
  compiler may scalarize it. Do not optimize based on the declaration alone.

## Precise arithmetic prerequisite before any helper optimization

Apple's Metal Shading Language Specification (2026-06-04, pp.16-17) documents
that safe math still sets FP contraction to on (within statements). It also
documents `#pragma METAL fp contract(off)` / `#pragma STDC FP_CONTRACT OFF`.
Reference: https://developer.apple.com/metal/Metal-Shading-Language-Specification.pdf

This is a candidate for optimized, non-contracting helper implementations, not
proof of equivalence to the current fma-based helpers. Preserve signed zero,
subnormal behavior, NaN/Inf behavior and explicit FMA rounding. Before a live
change, compare both implementations on native GPU using adversarial inputs
and dependent arithmetic chains; measure the same workload separately. Verify
on the supported language versions, not just the latest specification.

`xcrun -f metal` returns a shim but the offline compiler fails with "missing
Metal Toolchain". No large toolchain download was started. Compilation via
`newLibraryWithSource` succeeded for all nine variants, so native differential
tests remain possible without installing anything. Assembly inspection is not
currently available through that command.

## Reproduction and checks

Ignored test `renderer_metal::metal_pipeline_cache::tests::inspect_cached_graphics_msl`
uses RUZU_INSPECT_METAL_CACHE, comma-separated RUZU_INSPECT_SHADER_HASHES and
RUZU_INSPECT_OUTPUT. It loads a private copy, filters graphics keys by hash,
reuses build_graphics_shader_stages, and exports native vertex/fragment MSL
with per-key names and binding/execution metadata. It is never in the live
renderer and introduces no per-frame environment lookups or logs.

Full release video_core suite with Metal validation: 1823 passed, 4 ignored,
no warnings (`/tmp/metal-offline-inspect-tests.log`). Explicit offline inspection:
1 passed, all nine pipelines compiled. No GUI run or performance claim for this
test-only change. No instances were started or left running.

## Native arithmetic experiment and candidate (2026-09-08)

`metal_shader::tests::precise_helpers_without_optnone_probe` compares the old
optnone helpers with inline helpers under STDC FP_CONTRACT OFF, without changing
the fma expressions or compile_msl_library safe-math policy. It includes special
FP32 triples, pseudorandom triples, every FP16 bit pattern with varied second
operands, a separately rounded multiply/add, and a 64-iteration dependent chain.
This is equivalence to the previous native helpers, not a complete Maxwell
floating-point oracle or exhaustive testing of all operand combinations.

Native M2 Pro results, 73728 triples and 589824 output words per version:
- MSL 2.3: zero differing words; median GPU time 0.610750 ms old / 0.028500 ms inline.
- MSL 4.0: zero differing words; median GPU time 0.614000 ms old / 0.028750 ms inline.
- Ten alternating-order runs, first two excluded from timing. The reported
  statistic selects index 4 of the eight sorted remaining times (upper median).
- Log: /tmp/metal-fp-helper-wide-probe.log. These are synthetic command times,
  not a game FPS prediction or proof of the generated machine instruction count.

The candidate replaces the three optnone attributes in msl_emit_context.rs with
inline and emits FP_CONTRACT OFF if any such helper is needed. Contraction is
conservatively disabled for subsequent shader code too. Explicit fma remains
fused; no fast-math, rounding mode, resource ABI, or source IR change. Eden's
NoContraction contract was re-read in emit_spirv_instructions.h and
emit_spirv_floating_point.cpp. The native helper strategy is Metal-specific.

Full release suites with Metal validation: shader_recompiler 576 passed;
video_core 1824 passed, 4 ignored; no warnings. The native arithmetic probe is
now a normal regression test. Log: /tmp/metal-inline-fp-tests.log.

## Controlled GUI measurements

Same M2 Pro, release ruzu.app, Metal renderer, isolated copied configuration,
native captures and identical profiler/watchdog settings. Each run was limited
to 120 seconds; the watchdog terminated the process (TERM then KILL after grace),
not a spontaneous crash. No instance remains running.

| Run directory under ../ruzu-diagnostics | Hall FPS median, 110-120s | Peak footprint GiB | Max swap growth MiB |
| --- | ---: | ---: | ---: |
| lm3-fp-baseline-20260908.59z6gq | 15.91052 | 6.40063 | 0 |
| lm3-fp-inline-20260908.EZEOV5 | 20.80054 | 7.25005 | 559.94 |
| lm3-fp-inline-repeat-20260908.pk2GtP | 20.80519 | 7.01742 | 0 |

Nine FPS samples in each late window. Roughly +31% in this scene, reproduced
on the second optimized run. The second run was still in the save confirmation
menu at 80 seconds: its earlier 30 FPS are explicitly EXCLUDED. The 110-second
capture shows the hall; character animations and slight camera differences
mean this is scene-matched, not a deterministic frame-identical replay.
Checked captures show the hall, actors and geometry intact, not a pixel-exact
rendering oracle. Other titles and older physical devices remain untested.

The optimized first run increased system swap by about 560 MiB; the repeat did
not. No monotonic growth or safety limit was reached. Disk-space losses over
the three runs were 11.02, 27.19 and 30.62 MiB respectively.

Large late baseline GPU commands measured 59.716 / 67.832 ms, versus 22.133 /
22.215 ms in the first optimized run. Sampled shader-stage intervals also fell,
but remain overlapping/dependency-inclusive and must not be summed as exclusive
execution costs. This supports a real GPU improvement, not merely faster host
submission. It does not establish the next bottleneck or a hardware limit.

Old GUI UUID: C0B0FCD4-7D9A-3C83-A2BD-E981A27F7A5D.
Current rebuilt GUI UUID: 83632306-3A16-3EA9-AF07-2D31738075E3.
Build/bundle logs: /tmp/metal-inline-fp-gui-build.log and
/tmp/metal-inline-fp-bundle.log; signature verified. MoltenVK unchanged, SHA256
0995b17b030c01e991e2c36b48a953d8a4fdb6c4df1b9dcaa46b6d9e08612855.

Next investigation: profile CPU recording and waits again with the faster
shaders. The large reduction in GPU command time versus the smaller FPS gain
suggests another limiting dependency, but its identity is not yet established.

## 2026-09-08 - CPU profile after precise-helper optimization

Evidence: ../ruzu-diagnostics/lm3-inline-cpu-profile-20260908.itf2bD.
Native sample header places the five-second profile at about 98-103 seconds;
scene100 confirms the hall (scene80 was still a menu). 2638 samples per thread.
The GPU worker has 1026 samples in SynchState::pop_wait, waiting for commands,
not for GPU completion. Native pipeline lookup accounts for 294 inclusive
samples, about 11% of the thread, mostly key hashing through repeated lookups.
Only 11 samples occur under waitUntilCompleted and two under nextDrawable.
Inclusive samples must not be summed with their children.

Guest cores 0/1/2 also show substantial mutex waits in JIT memory callbacks:
886/518/608 leaf samples respectively. Read32 and Write64/128 callbacks acquire
core_memory. This establishes contention, not its root cause or permission to
remove the lock. Core 3 is largely idle. No JIT or memory locking changes made.

Converted native Metal cache getters to one Entry lookup, preserving keys,
validation, native construction and object ownership. Depth-state retaining
also avoids a second lookup. Eden's try_emplace reuses its iterator in the
equivalent slow-path lookup; its full transition cache is not ported here.
Release video_core with Metal validation: 1825 passed, 4 ignored, no warnings.
Log: /tmp/metal-cache-entry-tests.log. Runtime FPS effect remains to be measured.

### First runtime validation of the single-lookup cache

Evidence: ../ruzu-diagnostics/lm3-cache-entry-20260908.YLQ514.
GUI UUID D7793FE7-20D5-323E-A73B-B3CF9196D830, release build and signed bundle.
MoltenVK SHA256 unchanged. Build logs /tmp/metal-cache-entry-gui-build.log and
/tmp/metal-cache-entry-bundle.log; no warnings.

Late 110-120s median: 23.87213 FPS (9 samples), compared with 20.92507
(10 samples) in the preceding profiled run. Earlier unprofiled controls were
20.80054 and 20.80519. This is a promising single-run result, not yet a
repeated/deterministic FPS gain. Native scenes100/110 show the hall intact;
actor positions/animation differ slightly between runs.

The new native sample starts 97.33 seconds after launch, in the hall. Render
pipeline lookup falls from 294/2638 (11.1%) to 148/2775 (5.3%) GPU-thread
samples. Queue-empty waiting is still 890/2775 (32.1%). Sampling is statistical,
not an exact CPU timing benchmark. The key-hash work reduction is consistent
with replacing two map lookups by one; CPU/guest dependencies remain relevant.

Peak footprint 6.976 GiB, no swap growth above the initial sample, disk loss
96.44 MiB including the native sample report. Watchdog stopped at 120.30s;
no running instance remains. No memory/space safety limit triggered.

Next slice: repeat the scene-matched measurement, then audit slow JIT memory
callbacks and their shared Memory ownership. Eden calls m_memory.Read/Write
directly; Rust uses Arc<Mutex<Memory>>. A safe optimization requires understanding
mapping changes, rasterizer invalidation and exclusive accesses before changing
that synchronization. Do not replace it with an unchecked raw pointer or remove
the lock merely because contention appears in the profile.

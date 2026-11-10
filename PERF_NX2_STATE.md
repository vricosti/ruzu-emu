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

## Root cause of the black textures and scenery popping (2026-11-09)

Found, with the mechanism confirmed end to end. `nx2:gpu` command 214
(`sub_create_gpu_only_region`) backs each GPU-only region eagerly out of the
SystemNonSecure pool. Mario Kart World backs 87 regions totalling 9.5 GiB,
which is the whole pool (9728 MiB), then asks for a further 5.7 GiB at
0x8430230000. `back_gpu_only_region` fails, nothing is mapped, and the handler
still answers RESULT_SUCCESS - so the guest uses the range and every read of
it returns zeros. The log shows exactly that: 9 allocation failures followed
by 20 `gpu_mm::read: unmapped GPU VA` warnings at 0x8430230000+. Zeros decode
as black, which is what a capture of the attract-mode flythrough shows -
trees, bushes and karts as solid black silhouettes, correct again once the
scene settles. This is independent of GPU accuracy, fence behaviour, the ASTC
encoder and the conversion cache, which is why none of those changed it.

Not caused by the BC7 encoder: a capture with RUZU_BC7_FAST=0 (bc7enc
reference) still shows it, and an offline test of the analytical encoder
reproduces skin-next-to-dark-feature, opposed-hue and alpha-edge blocks to
within 1/255 (`bc7_keeps_two_cluster_blocks_close_to_source`, a case the
previous tests - a linear gradient and a flat block - never covered).

The drop happens at the GMMU, below the service: in `memory_manager.rs` a
write to an `EntryType::Reserved` (sparse) page is skipped outright and a read
of `Reserved`/`Free` is zero-filled. So the title writes a texture into a
sparse page, the write is discarded, and the read back is black. Both
`write_block_impl` and `write_block_impl_safe` do this. That is where
on-demand commit belongs - committing the page on first write - not in
`nx2_gsv`, which only decides whether the region is backed up front.

Sizing the fix is still open. The per-address warnings are capped at ten lines
per call site, so the "20 unmapped VA" figure is a logging artefact, not a
volume: an earlier read of it here as "20 pages = 80 KiB" was wrong. Uncapped
byte counters were added (`sparse_write_dropped_bytes`,
`sparse_read_zeroed_bytes`, reported per 64 MiB) and showed no report up to the
title screen, so the volume is under 64 MiB there; it has not yet been
measured in a race, which is where the popping is worst.

Eager backing is the defect; the pool size is not the fix. Raising
NX2_GRAPHICS_SERVER_MEMORY_SIZE to 16 GiB (SystemNonSecure 9728 -> 17888 MiB)
cut failures 9 -> 1 and unmapped reads 20 -> 0, but the guest then touched
enough of it to leave the 32 GiB host at 0.3 GiB free and the title froze.
Reverted to 8 GiB. These are sparse regions: the fix is to commit only the
pages the guest maps, which is structural work in `nx2_gsv`'s region handling.
Kept from the experiment: the handler now logs an explicit UNBACKED error
naming the zero-read consequence, instead of failing silently.

Skipping the eager backing entirely (RUZU_NX2_LAZY_SPARSE=1, opt-in) was
measured: 42 regions left unbacked, allocation failures 9 -> 0, but resident
memory did not move (18.9 GiB) and the zero-reads persisted. So the eager
backing is not what holds the memory, and leaving regions unbacked is not by
itself a fix - the commit-on-write path above is still required.

Host memory is also a standing confound for every measurement on this machine:
the session runs 16-21 GiB resident / 21-23 GiB private (guest DRAM is a
pagefile-backed SEC_COMMIT section) against 32 GiB total, repeatedly reaching
0.3-0.7 GiB free. One session froze there. Treat every FPS figure taken on
this host as contaminated by paging until residency is brought down.

Two `nx2_gsv` tests (`vi2_composite_uses_memory_base_native_layout_and_only_valid_fences`,
`gpu_sparse_backing_is_freed_only_after_gpu_completion`) fail when the module's
tests run together and pass in isolation: they share the
`Nx2GpuMemoryRegistry` singleton. Pre-existing at b626a11f, not caused by this
work, but it means the sparse-backing lifecycle currently has no reliable
regression cover.

The success answer is load-bearing, and the lie cannot simply be removed.
Making command 214 answer RESULT_OUT_OF_MEMORY was tried and **crashes the
title**: the guest backtrace lands 120 ms after two `could not be backed`
lines (t=18.66s, 18.77s, abort at 18.778s). An earlier reading of that same
experiment here - "zero unmapped reads, so the title adapts" - was wrong: the
reads stopped because the process had died, not because it resized itself.
Reverted; the handler reports success again and logs an explicit UNBACKED
error naming the zero-read consequence.

So the allocation has to succeed. Host RAM is the constraint, and the
measurement below says where to find it.

## In-race measurement on the current build (2026-11-09)

Driven to an actual race through the menus (Grand Prix -> Mushroom Cup 50cc ->
Mario Bros. Circuit, lap 1/3) on the default configuration: precompile on,
async shaders off, gpu_accuracy Low, BC7, memoization on.

| window | FPS |
|---|---|
| course intro camera | 42.96 |
| coasting, no input | 12.63 mean (7.99-15.99) |
| holding accelerate | 11.07 mean (9.99-11.99) |

Taken while the host was paging: residency climbed to 20.25 GiB and free host
memory fell to 3.0 GiB over the same window, so this is a floor, not the
title's ceiling. `game_fps` from the harness is only valid while frames are
actually being presented; a 0.0 reading means the session is not rendering,
not that it is slow - several earlier readings in this file were that case.

The race rendered correctly: full textures, no black silhouettes, Mario's face
clean. The defect is latent rather than absent in that session - 9 regions
logged UNBACKED and 20 zero reads occurred - so a correct-looking scene does
not clear it.

Descriptor memoization in the same window: 200912 visits/s, 8143 changed
(4.1%), fill image views 66.1 ms/s. Still no A/B: a paging host cannot
separate a few ms/s from scene noise, so the slice stays uncommitted and
unjustified.

## Residency attributed directly (2026-11-09)

Measured with `VirtualQueryEx` over the live process, summing committed bytes
by region type - one session, two points in its own lifetime, so no
cross-session comparison is involved:

| point | Private | Mapped | resident |
|---|---|---|---|
| t=50 s, precompile under way | 4.80 GiB | 20.15 GiB | 3.61 GiB |
| after 26k pipelines built | 14.45 GiB | 30.04 GiB | 20.40 GiB |

Building the pipeline set adds **+9.65 GiB private and +9.9 GiB mapped** in
the same process. Guest DRAM is one 20 GiB `Mapped` region (0x1BB7FEA0000,
exactly the 12 + 8 GiB the NX2 profile provisions) and is the commit floor;
everything above it is the pipeline set and the driver allocations behind it.

This restores, on sound evidence, the conclusion that an earlier cross-session
comparison had reached and that was then withdrawn: the pipeline set is worth
roughly 9-10 GiB of host memory. The withdrawal was right about the method -
the low figure came from a session that never rendered - and wrong about the
magnitude. Pipeline LRU pruning is therefore the lever, and it is now measured
rather than inferred.

## Bounding the boot build saves memory at boot and loses it again in play

`RUZU_PIPELINE_BOOT_LIMIT=<n>` builds only the last `n` entries of the
append-ordered disk cache and defers the rest to on-demand compilation. At
n=6000 of 26424 the boot-time win is large and real:

| | unbounded | limit 6000 |
|---|---|---|
| pipelines built | 26424 | 6000 |
| Private committed | 14.43 GiB | 6.22 GiB |
| resident after boot | 20.40 GiB | 12.89 GiB |
| cumulative vkCreate | 2517 s | 557 s |

It does not survive contact with the game. Entering a race, the deferred
pipelines build on demand: residency climbed back to 21.03 GiB and the
renderer produced no frame at all for 100 s of sampling (game_fps 0.0
throughout, screenshots timing out), because with asynchronous shaders off
each compilation blocks the draw that needs it. The memory is not saved, only
deferred, and it is paid back as a freeze.

So the 26k set is not gratuitous - the title genuinely reaches most of it.

The pairing with asynchronous shaders was then tested, and it does not rescue
it. Limit 6000 + `RUZU_NX2_ASYNC_SHADERS=1` keeps the renderer alive where the
limit alone froze it (frames throughout, screenshots succeed), but the same
race window measures **10.32 FPS mean (0.0-14.98)** against the default's
11.07 (9.99-11.99): a lower mean, and two samples at 2.0 and 0.0 FPS that the
default never produces - the on-demand builds stalling draws. Residency came
back to 22.31 GiB, above the default's 20.25.

### Measured ranking, same race, same host

| configuration | driving FPS | worst sample | resident |
|---|---|---|---|
| default (full precompile, async off) | **11.07** | 9.99 | 20.25 GiB |
| limit 6000 + async on | 10.32 | 0.00 | 22.31 GiB |
| limit 6000, async off | no frames for 100 s | - | 21.03 GiB |

The configuration already shipping is the best of the three. Deferring
pipeline builds does not save memory, it only moves the cost into the race and
adds stalls.

## Every lever in the GPU-thread profile is already taken

Checked one by one against the profile's own zones, so the next person does
not re-walk them:

| zone | state |
|---|---|
| macro (181 ms/s, 543k/s) | already JIT-compiled on the native Turing path (`EDEN_NX2_MME_JIT` is an opt-out); the Maxwell-path fix below changes nothing here |
| 3D register writes (77% same value) | already short-circuited - `skip_same_value_writes` is on by default (`EDEN_NX2_SKIP_SAME` is an opt-out) |
| fence host waits | already zero at gpu_accuracy Low |
| texture conversion | already served from the converted-texture disk cache |
| pipeline builds | full precompile measured best of four configurations |

What remains in those zones is genuine command-processing work plus the host
paging documented above. That is the honest ceiling of the current
architecture on this machine, and it is why no configuration change in this
investigation moved the in-race number.

## Macro execution: a real parity bug, but not this title's cost

Macro execution is the GPU command thread's largest single zone (181 ms/s over
543k invocations/s). `NativeMacroContext::default()` hardcoded
`MacroEngine::new(true)`, pinning every macro to the interpreter, while
`compile_backend` already selects `MacroJITx64Impl` for the false case and
upstream picks the JIT unless `disable_macro_jit` is set (default false). Fixed
to follow the setting, and the test that asserted the hardcoded value now
asserts the contract instead.

It does not help Mario Kart World, and the measurement says why: cost per
invocation is unchanged (0.333 -> 0.327 us; 158.7 ms/s over 485k/s). NX2
titles take the native Turing path (`native_turing`, `TuringMacroJit`), whose
JIT is already on by default (`EDEN_NX2_MME_JIT` is an opt-out). So the
macro time measured here is already JIT-compiled, and there is no easy gain
left in that zone. The fix stands on parity grounds and benefits the Maxwell
path, not on a Mario Kart World measurement.

## RUZU_SKIP_BOOT_PRECOMPILE does not yield a usable session

Tried both ways round, on a build with the 214 handler back to reporting
success. With async shaders off the first frame never completes. With
`RUZU_NX2_ASYNC_SHADERS=1` the title boots in ~10 s and does not crash, but
the GPU thread then sits at `pending sleep 997.6 ms (632/s)` with zero draws,
zero 3D register writes and zero descriptor visits: it renders nothing, and
screenshots time out. Syncpoint traffic continues, so the guest is alive and
starved rather than hung outright.

Consequence for an earlier claim recorded here: the "10.6 GiB resident without
precompile vs 16-21 GiB with it, so pipelines cost 6-10 GiB" comparison was
**unsound** - the low figure came from a session that was never rendering. At a
comparable point this configuration also sits at 16.5 GiB. Nothing currently
attributes the residency to the pipeline set; the 20 GiB committed guest DRAM
of the NX2 profile remains the obvious candidate and is unmeasured.

## Next steps, in order

## Pipeline LRU sidecar: implemented, and the bound still does not pay

`vulkan.lru` records, per graphics key hash, the use counter of the draw that
last selected it; a bounded boot build then keeps the most recently used
instead of the most recently recorded. Two implementation faults the
measurements caught, both fixed: uses were recorded only in the slow path
while most draws take the transition fast path, so nothing was ever recorded;
and the sidecar was written only at flush and `Drop`, which a force-kill or
crash skips. It is now recorded where the key is final and persisted every
20000 draws (and still at flush and teardown).

End to end it works - the file appears and grows during play (1992 bytes, 124
keys after the map screen; 4264 bytes later), and a bounded boot logs "ranked
by last use". The bound still loses, on the same screen, same title:

| | full precompile | limit 6000, LRU-ranked |
|---|---|---|
| map-screen FPS | 50.96 steady | 30.96 mean (0.0-44.96) |
| resident after boot | 20.4 GiB | 12.72 GiB |
| resident once playing | 20.25 GiB | 19.83 GiB |

The memory saving at boot is real and evaporates within a minute of play, and
the stalls (0.0 and ~5 FPS samples) are on-demand builds blocking draws. Caveat
on fairness: the sidecar held only 124 keys from one session, so most of the
6000 were still chosen by file order. The result is consistent with the
tail-order test rather than independent of it, so what it establishes is that
a bounded boot build is not promising, not that a fully warmed LRU is refuted.

## The freezes are a shader rejection, not memory (2026-11-09, diagnosed live)

Analysed on the live process instead of killing it. Native stacks (`cdb -pv`)
show the whole emulated machine stopped: **all six CPUCore_0..5 threads parked**
in `NtWaitForAlertByThreadId`, GPU thread in `NtWaitForSingleObject`, while the
guest still issues syncpoint traffic. The GPU profile reads `pending sleep
997.6 ms (627/s)` with **zero 3D register writes**: `gpu_thread.rs:469-485`
loops `has_pending_channels()` -> `resume_pending()` -> empty queue -> sleep
1 ms, forever, because channels never become resumable.

The chain, end to end:

1. `shader_cache.rs:420` rejects a native shader:
   `unsupported native opcode [op 927 ...]` and `[op 923 ...]`, both with
   `mesh_enabled=true`, at e.g. 0x114ca03a00 instruction 0xe0.
2. The rejected program is bound to stage 1, so the pipeline cannot be built.
3. `vk_rasterizer` logs `[DRAW_SKIP] pipeline compilation failed`
   (topology=Points) - the counter reached **#65536**.
4. The skipped draws never advance syncpoint 1 to the value the guest waits
   on (`0x710e`, requested over and over).
5. The guest blocks on that syncpoint, every core parks, and the GPU thread
   spins in PendingSleep. Deadlock.

So the freezes attributed earlier in this file to compilation stalls or memory
pressure are this: two unimplemented native opcodes (0x927, 0x923) in
mesh-enabled shaders. It is a shader-recompiler gap, and the deadlock is the
emulator's own: a skipped draw silently strands a syncpoint the guest is
waiting on, which turns one unsupported opcode into a hung session rather than
a missing effect.

The stranded wait is visible in the frozen session's own log: `Semaphore
0x21fc73b100 waits for 2 s: operation 4, expected 0x4, current 0x3` and
`0x21fc73b140 ... expected 0x2, current 0x0`. Increments of one and two - the
value the skipped draws would have produced.

**Fixed (the second half).** `Puller::can_resume_acquire` warned after 2 s and
then waited forever. It now abandons the wait after `ACQUIRE_DEADLINE` (10 s),
logging the address, operation, expected and current values. Hardware does not
wait forever either - the puller has an acquire timeout register - so an
unsupported shader now costs visual corruption instead of the session.
Covered by `unsatisfiable_semaphore_acquire_is_abandoned_after_the_deadline`,
which reproduces the observed shape (operation 4, expected 0x4, current 0x3)
and checks that a fresh wait and one inside the deadline are not cut short.

Validated in play: a full race on the fixed build (Mario Bros. Circuit, lap
1/3, textures correct, 10.93 FPS mean over 18 samples) with no hang. The hang
did not reproduce in that run, so the net itself stayed unexercised in game -
it is proven by the unit test and shown not to regress a normal race.

**The rejected opcodes are not missing - they are on the other path.**
`lower_to_maxwell.rs:347` handles 0x923/0x927 (ISBERD/ISBEWR) under
`if stage_buffer`, and `stage_buffer` is true only via
`lower_task_mesh_to_maxwell`. `shader_environment.rs:285` picks that path only
when `mesh_shader.is_some()`, which `vk_rasterizer.rs:1249` gates behind
`RUZU_NX2_MESH_DRAW`. With the opt-in off, Mario Kart World's mesh shaders are
lowered by the ordinary path, where those two opcodes have no arm - hence the
rejection. It is a routing gap, not a decoding gap.

Enabling the opt-in removes the defect completely and costs almost all of the
performance:

| | RUZU_NX2_MESH_DRAW off | on |
|---|---|---|
| shader rejections | 3+ | **0** |
| draws skipped | 28, counter reaching #65536 | **0** |
| driving FPS | 10.93 mean | **1.89 mean (0.0-9.99)** |
| resident | ~20 GiB | 22.11 GiB |

So the scenery popping and the hang share one cause - skipped draws from
mesh shaders rejected on the non-mesh path - and there is a switch that
provably removes it. The mesh path's compute emulation is simply too slow to
use, which is why it is opt-in.

That reframes the remaining work. It is no longer "find the cause of the
popping": it is one of

1. make the task/mesh lowering path fast enough to be the default, or
2. give 0x923/0x927 an arm on the ordinary path so mesh shaders stop being
   rejected without paying for full mesh emulation.

**Option 2 was tried and is closed.** Removing the `stage_buffer` guard so the
existing ISBERD/ISBEWR arm also serves the ordinary path breaks two tests:
`task_mesh_isbe_native_modes_preserve_pseudo_fields_and_reject_invalid_access`
asserts that `lower_to_maxwell` *must* reject these instructions, and
`complete_programs_match_verbatim_upstream_cpp` diverges at fixture line 57318
- the fixture is a verbatim comparison against the upstream C++ lowering. The
gate is deliberate and upstream-faithful, so widening it is not a fix but a
parity break. Reverted.

That leaves option 1, plus a third the measurements suggest: route only the
shaders that need the stage buffer through the task/mesh path, instead of
switching the whole draw path with `RUZU_NX2_MESH_DRAW`. The cost measured
above (10.93 -> 1.89 FPS) is the price of mesh emulation for *every* draw; it
says nothing about the cost of using it for the few shaders that actually
contain ISBERD/ISBEWR.

## Pool fallback for GPU-only regions: tried, crashes the title, reverted

`back_gpu_only_region` asks SystemNonSecure only, and a refusal leaves the
range unmapped while the handler still answers success. Falling back to Applet
and then Application was tried and **aborts the game**:

```
t=285.65-285.73  5 regions (~1.5 GiB) taken from Applet and Application
t=319.70         svcBreak -> Userspace PANIC, debug_buffer_err_code=E401
```

Application is the title's own heap. Satisfying GPU-only regions out of it
starves the allocation the title makes next, and it panics 34 s later. An
unbacked region renders black; a drained application pool ends the session,
which is worse. Reverted to SystemNonSecure only.

This also corrects what was recorded here as a partial fix. Its counters
were real - the fallback did rescue 5 regions, failures fell 9 to 4 - but the
session it was measured in was the one that then panicked, and that panic was
read at the time as a freeze and killed without being analysed. The counters
below are kept only as the record of what the fallback did before it broke the
title:

| | before | after |
|---|---|---|
| regions backed | 81 | 86 |
| rescued by fallback | - | 5 |
| backing failures | 9 | 4 |
| regions left UNBACKED | 9 | 4 |

The five rescued are 341 MiB x4 and 170 MiB x1, taken from Applet and
Application. What still fails is the 5.7 GiB region (twice) and two 341 MiB
ones: no pool holds 5.7 GiB contiguous once the title has filled
SystemNonSecure.

Also measured, and it corrects the mechanism recorded above: the uncapped
counters report **512 bytes** of zero-filled reads for the whole session. That
is far too little to paint anything black, so the black surfaces are not the
CPU-side `read_block` path returning zeros. They are the GPU sampling an
unmapped GPU VA directly through its own mapping, which never passes through
`memory_manager` and so is invisible to this instrumentation. The fix
direction is unchanged - make the backing real - but the measurement that
seemed to confirm the path was measuring the wrong one.

Not visually confirmed: automated menu navigation could not reach character
select, so Mario's nose and the scenery popping still need a human pass.

## What is actually resident (2026-11-09, sampled)

`QueryWorkingSetEx` over 40000 pages spread across the 20 GiB guest DRAM
section, at the title screen after a default boot (18.98 GiB resident total):

| component | resident |
|---|---|
| guest DRAM, of 20 GiB committed | **8.36 GiB (41.8% of the section)** |
| pipeline set, driver and emulator | ~10.6 GiB |

So 11.6 GiB of the NX2 guest provision is committed and never touched: it
costs commit charge, not physical memory, and shrinking the provision would
not give the host back any RAM it is actually using. The physical memory is
dominated by the pipeline set and its driver allocations, which is the same
component the direct attribution above caught growing by 9.65 GiB private
during the boot build.

That also explains the 16 GiB pool experiment that froze the host: the extra
pool was not inert commit, it let the backing succeed, and the title then
touched what it had been denied.

The actionable consequence is narrow but real: pruning has to keep the right
pipelines, not merely fewer. The limit test kept the tail of the append-ordered
file, which is recency of *recording*, not of *use*, and the race then rebuilt
what it needed. A per-key last-used sidecar - true LRU - is the only version of
this idea the measurements leave standing.

The binding constraint is the host, and it is a vice. On this 32 GiB machine
Mario Kart World needs, at once, the NX2 profile's 20 GiB of guest DRAM (of
which it genuinely backs 9.5 GiB in GPU-only regions, and asks for 5.7 GiB
more) and ~10 GiB for the pipeline set and its driver allocations. Give the
guest more and the host pages; give it less and the backing fails, which is
exactly what turns textures black. No scheduling of the pipeline build escapes
that, as the three measurements above show. The next real gains are therefore
in reducing what the title needs resident at all, not in when it is built:

1. Pipeline LRU with a per-key last-used sidecar, so a bounded build keeps the
   pipelines this track actually uses. The crude tail-of-file bound is measured
   and fails; this is the only form of the idea the measurements leave open,
   and the pipeline set is the one component shown to hold ~10.6 GiB resident.
2. With headroom found, raise the SystemNonSecure pool until the 5.7 GiB
   region backs successfully. That is the black-texture fix, by the only route
   the title accepts: it must be told success, and the success must be true.
   Note this costs real memory, not just commit: the 16 GiB experiment froze
   the host precisely because the backing then succeeded and was touched.
3. Re-run the reference grid window on a host with memory to spare before
   quoting any FPS figure as this title's ceiling; 11.07 is a floor set by
   paging, not by the emulator.
4. Fix or withdraw `RUZU_SKIP_BOOT_PRECOMPILE`, which currently cannot produce
   a rendering session either way.
5. Decide the memoization slice with an A/B once the host stops paging, or
   drop it. It has never shown a gain that clears scene noise.
2. Re-test asynchronous shaders. They were turned off because of the popping,
   and the popping has now been traced to sparse-page zero reads instead, so
   the NX2 session policy that disables them rests on a premise that no longer
   holds - and re-enabling them removes the in-race compilation stalls.
3. Bring host residency down; until then no FPS number on this machine is
   trustworthy.
4. Give the `nx2_gsv` tests isolated registry state so the sparse lifecycle is
   actually covered.

## Scenery pop-in investigation (2026-11-09, superseded by the section above)

Trees still pop with async shaders confirmed OFF (global false, all per-title
inis use_global; the Vulkan configure path waits on unbuilt pipelines instead
of skipping, matching upstream graphics_pipeline ConfigureDraw). The async
session policy was therefore a no-op for this symptom. The remaining Low
accuracy gates are the suspects, both upstream-faithful:

1. Fences: at Low with gpu_fence_behavior=Default, delay_fence=false — fences
   signal early and empty fences are skipped (fence_manager.rs), so game-side
   readback-driven streaming/culling sees stale data.
2. Macro parameters: refresh_parameters_impl returns early at Low
   (maxwell_3d.rs:3453), so draws replaying macros can use stale
   memory-sourced parameters.

Test in flight: per-title MKW gpu_fence_behavior=2 (Balanced), gpu_accuracy
kept at Low. Verified live: fence host waits nonzero again (3/s, ~7 ms/s at
title). If pop-in persists, next test is an env-gated macro refresh at Low;
fallback is gpu_accuracy=High (known stable pre-Low, costs the +54%).

## Descriptor-visit memoization (2026-11-09, measured, uncommitted)

Implemented: GUEST_WRITE_EPOCH bumped on GPU-VA writes/map/unmap/SubmitList;
DescriptorTable::read_with_memo skips guest re-reads within an epoch
(RUZU_TIC_MEMO=0 kill switch). Driving window: 190-198k visits/s with only
7.4-9.8k changed; fill image views 69-94 ms/s vs 95-97 pre-memo — modest,
scene-noisy gain. Abandoned on 2026-11-10 at the user's request; the code
was removed from the tree.

## Rendering defects resolved (2026-11-10)

Four independent causes, each reproduced and re-measured with the capture
harness on the same host:

1. Mario's face sheared: the converted-texture disk cache hit path skipped
   the `buffer_row_length`/`buffer_image_height` tail of `convert_image`, so
   levels whose extent is not an ASTC block multiple were uploaded with the
   block-aligned pitch. Cache off rendered clean; verify mode found 0 byte
   mismatches over 16384 hits, which pointed at the copy description rather
   than the data. Fixed, with a regression test.
2. Freezes and GPU-thread deaths: `wait_for_frame` gave up on the present
   fence after 4 s (upstream's value). Any longer stall (synchronous
   pipeline build on a cold cache, ~10 s) made the title diverge: semaphores
   never released, then garbage command lists (879-layer image,
   `ERROR_OUT_OF_DEVICE_MEMORY`; macro bind index 4096). Default raised to
   120 s (`RUZU_VI2_FENCE_TIMEOUT_MS`); the failing case survives 3/3. This
   supersedes the "freezes are a shader rejection" section above, and the
   acquire-abandon guard of de5f7a41 is withdrawn.
3. Transient black scenery: a deferred `nx2_unmap` erased the mapping of a
   region recreated at the same GPU address. Maps overlapping a pending
   unmap are now ordered behind it. Unbacked image resolutions 8 -> 0.
4. Vegetation appearing and disappearing: ~650 draws/s skipped because mesh
   shaders were rejected on the classic path. The mesh draw path is now the
   NX2 default (`RUZU_NX2_MESH_DRAW=0` disables). Stationary Free Roam: a
   pine missing in 4 of 12 frames before, present in 16/16 after; 0 skips,
   0 rejections, same frame rate (the earlier "1.89 FPS with mesh" figure
   was contaminated by the fence-timeout cascade).

Default-build check in Free Roam: 11-12 FPS, draw skips 0, rejections 0,
unbacked 0, semaphore waits 0, fence timeouts 0.

Still open: desert bushes on Mario Bros. Circuit render near-black with a
correct cut-out (BC3 and BC7, mesh on and off, stable rather than
transient). 2108 decoded ASTC textures were dumped and look sound. A
translation-miss probe (`RUZU_GPU_XLATE_MISS=1`, with backtraces) shows the
title using the nine GPU-only regions the 8 GiB pool cannot back (2 x 5.7 GiB,
7 x 170-341 MiB): Fermi2D blits into 85 MiB slots of the large ones, and
graphics and compute image views at the start of the small ones. The texture
cache treats a descriptor in an unmapped range as invalid (upstream
`IsValidEntry`), so those sample as a null view. That this is the bushes is
the working hypothesis, not yet tied to their draw. Two ways out were tried
on this 32 GiB / 8 GiB-VRAM host and both fail:

- pool at 22 GiB (34 GiB guest layout): private memory 37 GB, commit free
  2.7 GB, device reads unmapped from the first submission; never renders;
- a device address with no guest memory for the unbacked regions: the title
  writes there (130k page writes, ~510 MiB, dropped in 31 s) and the GPU
  thread dies on `ERROR_OUT_OF_DEVICE_MEMORY` creating an image there.

Both were removed. The out-of-device-memory death itself was a missing port:
upstream `MemoryAllocator::CreateImage` never backs images above 2^28 texels
and falls back to an unbacked sparse image; that is now ported. With it and
`RUZU_NX2_GRAPHICS_POOL_GIB=16` the session reaches the race (142 regions
backed, one 5.7 GiB region unbacked, three BC6H 8192x8192x68 arrays and one
R8 16384x16384 created unbacked) at 0.3 GiB of free host RAM.

The black objects are tumbleweeds (they move between captures), and they are
still black in that 16 GiB session. So the 170-341 MiB regions are not what
they lack. Remaining candidates: the BC6H arrays, which upstream also leaves
without memory (they read as zero), or something unrelated to backing.

Draw-filter diagnostics (`RUZU_DEBUG_SKIP_VIEW_DRAWS=null|unbacked|bc6h|r8|r16`,
graphics_pipeline.rs) drop the draws that sample a null view, an unbacked
image, or a null view of one format class. At the default 8 GiB pool the
descriptors that resolve to null views are exactly eight:

| format | size | count | region |
|---|---|---|---|
| Bc6hUfloat | 8192x8192, 12 levels, array | 1 | 5.7 GiB |
| R8Unorm | 16384x16384, 6 levels | 4 | 341 MiB each |
| R16G16Unorm | 8192x8192, 5 levels | 2 | 341 MiB each |
| R16Unorm | 8192x8192, 5 levels | 1 | 170 MiB |

With `null`, and equally with `bc6h` alone, the whole static scene (track,
ground, buildings, grandstands) disappears and only karts, characters, HUD
and sky remain: every static-world draw samples the BC6H array, which on
the console is a sparse virtual texture with only resident tiles backed.
The scene still looks lit without it, so the title has a fallback for
non-resident pages; the tumbleweeds are the visible case where the fallback
is black. The filter cannot separate them from the rest of the scene, since
all of it samples the same array.

What a real fix needs is sparse residency for that array: memory for the
tiles the title fills (Fermi2D blits into 85 MiB layer slots were observed),
not an image with nothing bound. Upstream has the same gap.

## Vegetation still flickering after the mesh-path default (2026-11-10, fixed)

The user still saw vegetation appear and disappear. Reproduced stationary in
a race: cacti and bushes popping in and out, sometimes drawn at another
position, with 0 skipped draws. Ruled out, each with its own run: unbacked
GPU-only regions (pool 16 GiB), GPU accuracy High, upload reordering, the
speculative vs exact task-count path, `OpUndef` values, full subgroups.

What pinned it: in the exact path the task payload is read back, so it can be
dumped. Stationary, the guest-side inputs of each vegetation draw (constant
buffers, storage-buffer pointers) were identical frame to frame and the
emitted child counts were constant (74 mesh draws, 111 children per frame),
yet the payload words differed on every frame and several draws carried the
same words. The payload lives in Workgroup storage, undefined at launch; only
the emission count was initialized, so every word the guest task leaves
unwritten exported stale bytes from another workgroup, and the mesh children
read them. Clearing the whole payload (and the mesh output storage) at launch
makes 34 of 34 active draws byte-stable across frames, at the same frame
rate. `RUZU_NX2_WORKGROUP_ZERO=0` restores the old behaviour.

It is not the whole defect. The user still sees bushes flicker with the
clear on and off. Captures agree that it is not sufficient: one stray bush in
24 frames in a run with it and one in 24 without, although two other runs
(36+24 and 2x49 settled frames, speculative and exact path) showed no pop at
all, only every bush changing slightly together about every sixth capture,
identically in both paths (wind animation or the reported flicker: not
determined). RenderDoc 1.47 captures work (RUZU_RENDERDOC_TRIGGER with
ENABLE_VULKAN_RENDERDOC_CAPTURE=1; four 4 GB captures taken) but replaying
one exits about 25 s in, right after the initial-state sync, with no error
in its log, so no per-draw comparison was obtained.

Method note: several A/B runs in this investigation silently used a stale
test binary because the copy over a still-locked executable failed. Any
comparison has to confirm the binary timestamp first.
Two shaders are still rejected in races (`native indexed constant load
mode`, op b82).

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

1. Split rehabilitation: cut back-side per-draw cost first. Descriptor-visit
   memoization is sized: 238k visits/s at the grid, only 0.64% changed, so
   nearly the whole fill-image-views descriptor cost is recoverable; it
   needs TIC/TSC region write tracking (SMMU-page based - CPU invalidations
   arrive in CPU space while tables live in GPU VA) so unchanged tables skip
   guest re-reads. Then re-A/B the split (its drains wait on that cost).
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

# Geometry shader support on macOS

## Objective

Render the Maxwell geometry draws required by Luigi's Mansion 3 after the title
screen, without skipped draws, fabricated capabilities, or global idle waits.
Preserve the complete IR/stage contracts and validate the actual rendered scene.
Typed exception handling alone is not completion.

## Latest integration status (2026-09-06)

Direct Metal geometry draws now reach the production compute/object/mesh path.
This is wired code with native synthetic pixel validation, not yet a verified
gameplay result. Historical "not wired" observations below refer to earlier
slices. The original goal remains open; see the final section for prerequisites.

## Initial evidence (2026-09-05; superseded integration status below)

- Branch: fix/vulkan-geometry-stream-exception, based on main d30e3244.
- The original freeze kills the GPU thread: EmitVertex raises a string panic
  when geometry streams are unavailable. Eden throws NotImplementedException;
  its pipeline cache catches Shader::Exception and rejects that pipeline.
- Both EmitVertex and EndPrimitive now use the existing typed exception in
  Rust. The regression fails without this correction, passes with it, and a
  Vulkan cache test verifies compilation can continue afterward. Ordinary
  unexpected panics are still propagated.
- The full shader_recompiler release suite passes (555 tests). The targeted
  video_core exception test passes. Full video_core release verification after
  the mesh prerequisite slice passes (1656 passed, one ignored). No Rust
  compiler warnings were emitted in that build/test run.
- The bundled MoltenVK is 1.4.1 on Apple M2 Pro. A vulkaninfo query against that
  exact library exposes geometryShader=false, shaderOutputLayer=true and
  shaderOutputViewportIndex=true, including VK_EXT_shader_viewport_index_layer.
  No libraries or persistent driver configuration were changed.
- The live Metal pipeline cache still rejects geometry stages. The direct MSL
  emitter now has a mesh lowering under synthetic GPU validation; preceding
  vertex production and live draw integration are not implemented yet.
- The shared translation path can synthesize geometry for vertex layer output.
  We must distinguish this from an actual guest geometry program before
  attributing the failure to the game's own geometry shaders.

## Active capture

RUZU_DUMP_GEOMETRY_PIPELINES selects a diagnostic output directory. Only
pipelines containing geometry are captured, once per pipeline/stage/file.
The dump contains pipeline keys, guest stage hashes, runtime state, full IR
(including metadata and instructions), and cached Maxwell words with offsets.
The live shared translator covers Vulkan and Metal; the Vulkan disk translator
also records before SPIR-V emission. Generated stages have no guest code.

- GUI release rebuilt with the diagnostic and typed exception correction.
- Current run log: /tmp/ruzu-geometry-capture-run.log
- Capture directory: /tmp/ruzu-geometry-capture-20260905
- Vulkan capability query: /tmp/ruzu-geometry-vulkaninfo.txt
- User presses C (Switch A) manually at the title screen. Keyboard automation
  was denied by macOS and must not be bypassed. Do not launch duplicate games.
- Cache, settings, saves and the bundled MoltenVK were preserved.

## Implementation prerequisites and decision gates

1. Inspect the captured stage: input/output topology, invocations, maximum
   output vertices, stream arguments, layer/viewport and primitive ID, all
   varyings, resources, transform feedback, and neighboring stages. Correlate
   with the freeze log and a same-scene Eden reference (Eden read-only).
2. Evaluate Metal object/mesh lowering against compute plus a rasterization
   pass. Query actual Metal capabilities rather than relying on chip names.
   Mesh lowering needs primitive assembly, vertex-stage input production and
   a bounded output contract; compute needs output allocation, primitive
   assembly/compaction, indirect draw counts and explicit synchronization.
3. Choose one backend for an end-to-end implementation first. A Vulkan compute
   fallback must not depend on geometryShader or geometryStreams. Direct Metal
   emission must reuse Maxwell IR without SPIR-V-to-MSL conversion.
4. Implement prerequisites in their owning modules before wiring a draw path.
   Preserve emission-time output snapshots, zero/variable output, strips and
   cuts, invocation ordering, primitive IDs, interpolation, layers/viewports,
   resources and output visibility/lifetimes. No silent unsupported cases.
5. Add synthetic/differential tests and real GPU validation. Validate the
   rendered scene beyond the freeze, regress other titles, and measure release
   cost. Review meaningful compiler warnings.

## Capture findings and selected first backend

The user confirmed passing the previous freeze. The log continues rendering
while rejecting geometry pipelines with typed exceptions; this is not yet
correct rendering. Five captured pipelines contain three distinct guest
geometry shaders, not synthesized passthrough:

| Guest shader hash | Input | Output | Max vertices | Invocations |
| --- | --- | --- | --- | --- |
| 239e454704ba4521 | triangles | triangle strip | 6 | 1 |
| b7187e1b369d9894 | triangles | triangle strip | 6 | 1 |
| 5e486ac26abfd32a | lines | triangle strip | 14 | 1 |

All stream arguments are immediate zero. The first two emit two strips of
three vertices; the line program has fourteen EmitVertex instructions and one
EndPrimitive. InvocationInfo supplies the input vertex count in bits 16+.
All use CBUF loads, floating-point math, indexed per-vertex attribute loads,
and attribute stores. No textures, SSBOs, global writes, subgroup operations,
or FP64 appear in these geometry stages. Neighboring stages remain captured.
The captured runtime XFB varying records have zero components and bit 6 of
FixedPipelineState.raw1 (xfb_enabled) is zero in all five pipelines. These
captured draws do not request transform feedback.

Selected first implementation: native Metal mesh lowering. The M2 Pro's
queried profile satisfies MSL >= 3.0 plus Apple7/Mac2. A real native test
compiled a mesh function and created MTLMeshRenderPipelineState successfully
(not just a string-emission check). Capability-policy tests cover old language
versions, unsupported Apple families, and Mac2. Capability ownership remains
metal_device.rs; native shader compilation remains metal_shader.rs.

Rationale: these bounded outputs fit mesh limits comfortably; mesh emission
can feed rasterization within the render encoder without a whole-draw output
buffer, global compaction, indirect-count management, and a compute/render
barrier. Vertex evaluation/primitive assembly and shader resource binding are
still prerequisites. Do not duplicate vertex-side effects when evaluating
shared input vertices; account for index/restart/base vertex/instancing.

The Vulkan alternative remains a compute prepass plus rasterization using
generated buffers, with explicit resource dependencies and retained allocation
lifetimes. It is not being implemented concurrently. Larger mesh outputs,
older devices, streams and XFB require real support or explicit rejection,
never truncation or treating emitted stream indices as zero.

Additional observed error after advancing further: two tessellation pipelines
fail in MoltenVK's SPIR-V-to-MSL conversion with "Cannot resolve expression
type". This is separate from the geometry panic; do not claim complete game
compatibility solely from supporting these three geometry programs.

References:
- https://developer.apple.com/videos/play/wwdc2022/10162/
- https://developer.apple.com/metal/capabilities/
- Eden shader_recompiler/backend/spirv/emit_spirv_context_get_set.cpp,
  EmitInvocationInfo (and matching emit_spirv_instructions.h declarations).

No geometry fallback is wired yet. Native MSL geometry input/output emission
and strip assembly now have synthetic GPU coverage (details below). Next slice:
integrate vertex production/resources and mesh draws.
Same-scene Eden comparison, real game mesh rendering and performance validation
remain required. The user passed the frozen screen and later stopped the game;
the log confirms normal System shutdown. The GUI remains open. That run used
Vulkan and the typed error correction, not native geometry rendering.

## Direct MSL geometry emission slice

New emit_msl_geometry.rs owns the mesh topology, per-primitive input payload
ABI and strip index generation. emit_msl_special.rs keeps EmitVertex and
EndPrimitive ownership, following Eden's SPIR-V emission ordering: depth
conversion before the vertex snapshot, fixed point size restored afterward.
The dispatcher only routes instructions. msl_emit_context.rs owns declarations
and final primitive-count publication on every shader return.

The emitted stage reads position/generic inputs by the IR's vertex operand,
reads PrimitiveId from the payload, supplies InvocationInfo as input count
shifted by 16, and uses the mesh group X coordinate as InvocationId. The future
object stage must therefore dispatch one X group per guest GS invocation for
each assembled input primitive. Point/line/triangle output indexing is emitted;
EndPrimitive resets strip winding without resetting the global vertex index.
Incomplete strips generate no primitives and empty shaders publish zero count.

Stream zero is explicit; nonzero/dynamic streams, transform feedback, geometry
passthrough and unsupported interfaces are rejected, never substituted. Mesh
requires MSL 3.0. The native compiler checks total mesh storage including actual
output layout/padding; a float4-only host estimate would miss clip/point fields.

The native regression constructs geometry IR and compiles its generated mesh
function, with a test-only object producer and fragment oracle. The object
shader forwards a CPU-provided emission count through the payload. The same GS
contains conditional branches for six emissions; counts 0, 2, 3, 4 and 6 are
tested by real rasterization into an RGBA8 shared texture and CPU pixel readback.
Left/right triangles retain distinct red/blue emission-time colors, incomplete
strips remain black, and the gap remains black across EndPrimitive. This passed
on the M2 Pro in release without skipping, including runtime-variable emission.
The only completion wait is inside this test, before its CPU readback.

Full release verification after this slice: shader_recompiler 559 passed;
video_core 1657 passed, one ignored. No compiler warnings, test failures or
diff whitespace errors. Log: /tmp/ruzu-geometry-full-suites.log.

Not yet verified: continuous strip winding under culling, point/line GPU output,
flat/provoking-vertex contracts, multiple invocations, layers/viewports, primitive
ID export, clip distances, actual captured guest stages, or game rendering.
Do not enable the cache's geometry path before the preceding vertex execution,
primitive assembly and per-stage resource binding prerequisites are implemented.
Do not duplicate vertex shader side effects for shared primitive vertices.

## Vertex fetch prerequisite (2026-09-06)

metal_vertex_pulling.rs now generates explicit MSL input fetch from the existing
MetalVertexInputState. It does not reinterpret Maxwell registers or invent a
second binding allocator. Compact Metal buffer indices, attribute byte offsets,
strides and per-instance divisors come from that state. The generated function
returns the MslVertexIn type owned by the shader emitter; buffer argument names
and indices are exposed as structured metadata for the future object wrapper.

All 51 formats currently returned by metal_pipeline_cache::metal_vertex_format
are implemented: 8/16/32-bit signed/unsigned integers, normalized 8/16-bit values,
half/float, packed signed/unsigned normalized 10:10:10:2, and unsigned 11:11:10
floats. Missing components preserve the native (0,0,0,1) defaults. Scaled formats
remain integers at this boundary, as in the current descriptor path; their IR
conversion is not duplicated. Byte reads avoid host scalar alignment assumptions.

The GPU oracle compares explicit MSL compute fetch against a native vertex
function using MTLVertexDescriptor and rasterization disabled. Both read the
same bytes; no CPU reimplementation is used as the reference. Inputs include
signed extrema, half subnormals, infinities, NaNs and packed patterns. Integers
are bit-exact; normalized conversions allow one final rounding bit. NaN payloads
are not compared. All 51 formats passed on the M2 Pro in release. Additional
cases verify nonzero vertex start/base instance, per-instance divisor three,
and constant stepping. The measured divisor formula is:
baseInstance + (instanceID - baseInstance) / stepRate.

The native fetch test now consumes VertexPullingLayout itself, not just its
individual format helper. A structural test covers one source stream remapped
to a different compact index, multiple attributes sharing it, offsets/stride,
absent layouts and invalid step rates. GPU tests currently use aligned, in-range
inputs and a 16-byte stride. Indexed/restart assembly, robust out-of-range fetch,
multi-buffer GPU validation and execution of the actual guest vertex IR remain
prerequisites; do not infer their completion from the format oracle.

Next concrete integration boundary: produce a callable vertex-stage function
from the existing MSL IR emitter, with an explicit parameter ABI, then connect
VertexPullingLayout and the geometry payload. Do not parse the final generated
shader source in the renderer to recover resource bindings, and do not duplicate
side-effecting vertex execution at shared primitive vertices. Keep the live
geometry-stage rejection until its required producer/binding/assembly path exists.

Full release verification after the fetch slice: shader_recompiler 559 passed;
video_core 1660 passed, one ignored. No warnings or whitespace errors. Evidence:
/tmp/ruzu-geometry-fetch-full-suites.log. The GUI bundle was not rebuilt in this
slice and its live draw behavior is unchanged.

## Callable vertex interface prerequisite (2026-09-06)

msl_function.rs defines the native function ABI: parameter type/name and explicit
buffer, texture, sampler or builtin attribute. msl_emit_context.rs constructs this
metadata while allocating resources, then uses it to emit either the ordinary
stage entry point or an inline ruzu_vertex function. emit_msl.rs shares the same
IR validation, precoloring, phi declarations and instruction emission in both
modes. SPIRV-Cross compatibility artifacts explicitly lack this interface.

The unit regression compares complete generated sources after replacing only
the function declaration: instruction order, prologue, resource allocations and
returns are identical. Additional guards reject a non-vertex stage and discarded
vertex outputs. These tests passed, as did the existing MSL tests.

The native pixel test now uses actual vertex IR, a real Float4 vertex buffer and
a CBUF carrying an X translation. It composes explicit vertex pulling with the
callable vertex function, then forwards its positions into the geometry payload.
Separate object and mesh libraries avoid conflicts between their MslVertexOut
types; there is no source parsing or type renaming. Resource slots/call arguments
come from the structured interface. The object wrapper remains test-only and
assembles one triangle, so it does not establish general assembly support.

The first native compilation exposed use of Metal's reserved word `vertex` as
a test-loop variable. Renaming it to vertex_index fixed that error. The GPU test
then passed all emission counts (0, 2, 3, 4, 6), checking red/blue emission-time
outputs, incomplete strips and the black gap between primitives. Evidence:
/tmp/ruzu-geometry-vertex-mesh-chain.log. Full release verification passed:
shader_recompiler 561 passed; video_core 1660 passed, one ignored. No warnings
or diff whitespace errors. Log: /tmp/ruzu-geometry-callable-full-suites.log.
No GUI rebuild/run or live renderer behavior change was made in this slice.

Re-read Eden's emit_glsl.h/cpp, spirv_emit_context.h/cpp resource/interface
definitions and emit_spirv_special.cpp ordering. The new callable ABI is a native
Metal adaptation, not a missing Eden method moved into a different owner.
DIFF.md records these boundaries and validation limits.

### Next interrupted integration boundary

The live draw path is still unchanged. It must not enable mesh pipelines until
vertex scheduling, indexed/restart-aware primitive assembly, generic payload
forwarding and object/mesh resource binding are connected and validated.
The callable function is usable by an object or compute producer; it is not
itself a Metal entry point that can be looked up in a library.

Do not assume that every repeated index must execute exactly once: Vulkan's
Vertex Shader Execution section explicitly makes result reuse implementation
dependent, including stores/atomics:
https://docs.vulkan.org/spec/latest/chapters/shaders.html#shaders-vertex-execution
That does not validate blindly executing a strip's shared vertex again for every
primitive or ignoring subgroup behavior. A producer scheduled per input stream
entry can retain results for assembly across primitives; the same input entry
should not gain executions merely because it participates in several primitives.
This remains an architecture/validation prerequisite, not a proven game cause.

## Primitive assembly prerequisite (2026-09-06)

MetalPrimitiveAssembler now exists as a prerequisite to live mesh draws. It consumes
the cache's native index buffer (not guest memory), expands vertex IDs, builds
restart-segment prefixes and compacts primitive records in stream order. Records
refer to input-stream positions, allowing a vertex producer to execute each
entry once and share that result across strip primitives. GPU-computed indirect
arguments carry the primitive count without CPU readback. A hierarchical scan
avoids a serial per-draw scan or quadratic per-primitive restart searches.

Eden delegates this assembly to Vulkan; vk_pipeline_cache.cpp selects the input
topology class. The native implementation follows Vulkan drawing.adoc's topology
definitions, including boundary-specific triangle-strip-adjacency neighbors.
Quads already undergo index conversion in the common Metal buffer-cache path;
raw patch input remains a tessellation prerequisite, not geometry assembly.

The implementation records kernels into the existing MetalScheduler and uses
tracked private buffers. A hierarchical uint2 scan combines segment-start max
and primitive-count sum. Every 256-entry block produces its summary; recursive
summaries are propagated back into lower levels before consumers execute.
The returned vertex IDs, segment metadata, six-ordinal primitive records and
three-u32 indirect arguments remain GPU buffers. Temporary buffers are retained
by Metal command buffers after binding. No CPU polling/readback is in record().

Input widths 8/16/32 bits and non-indexed input are exercised; restart comparison
precedes base-vertex addition. Triangles and lines preserve degenerates. Strip
winding resets at restart. Line loops append their closing edge, while incomplete
lists/strips emit no incomplete primitive. Triangle-strip-adjacency follows the
distinct first/interior/last neighbor formulas. Raw quads and patches are rejected
by this helper, not silently converted to a guessed geometry input class.

Three native tests passed: explicit topology/adjacency fixtures across all index
widths and array input; a 66,007-entry strip with restarts across both scan levels;
and ABI/range rejection. The long fixture checks every primitive record, vertex
ID and restart segment, not just counts. Smaller cases cover consecutive/trailing
restarts, a one-vertex loop, separate adjacency strips, negative base vertex,
restart disabled, repeated indices and zero count. Invalid index ranges return
before any GPU commands are recorded. The fixture oracle reads private outputs
back only after the complete test command buffer finishes.

The rasterization test additionally uses the real assembler to consume a UInt16
index buffer with leading/trailing restart and baseVertex=-9. Its test object
producer reads the resulting stream ordinals and vertex IDs. The GPU-written
primitive count drives drawMeshThreadgroupsWithIndirectBuffer, followed by pixel
checks for all emission counts. There is no wait between compute assembly and
the mesh draw. Logs: /tmp/ruzu-geometry-assembly-tests.log and
/tmp/ruzu-geometry-assembly-mesh-chain.log.

Post-implementation audit re-read Eden's topology class selection, the Vulkan
pipeline's input assembly and MaxwellToVK::PrimitiveTopology. The native line
loop behavior is explicit rather than copying Eden's LineLoop-to-triangles map.
The host-parameter layout has no copied Rust struct padding. DIFF.md updated.
Full release suites: shader_recompiler 561 passed; video_core 1663 passed,
one ignored. No warnings. Log: /tmp/ruzu-geometry-assembly-full-suites.log.

At the end of this assembly slice, the next prerequisite was to execute the callable vertex shader once per input-stream
entry into retained output records, skipping restart entries. The object stage
can then copy shared vertex results into each primitive payload without repeating
vertex stores/atomics for every participating primitive. The current pixel test
still uses a test-only per-primitive callable producer, so it does not establish
this general execution contract. Follow with live per-stage resources and mesh
pipeline integration; do not enable geometry based on these synthetic tests alone.
No GUI bundle rebuild/run or live geometry advertisement occurred in this slice.

## Index conversion audit / interrupted prerequisite (2026-09-06)

Before extending geometry input, port Uint8Pass and QuadIndexedPass from Eden's
vk_compute_pass.{h,cpp} and host shader files. The existing Metal buffer cache
converts these on the CPU after finish_all(), unlike Eden's recorded compute.
Keep the pass ownership separate from BufferCacheRuntime, use scheduler-ordered
GPU work and tick-retired device-local staging, and verify GPU-produced input.

The earlier suspicion that first-index/base_vertex and 0xff remapping were
Metal-specific divergences is invalidated by the literal upstream call chain:
Eden passes the same fields through BindIndexBuffer/QuadIndexedPass and its
vulkan_uint8.comp also remaps 0xff unconditionally. Those edge cases remain an
upstream/common behavior audit, not grounds for a speculative Metal-only fix.
The confirmed divergence in this slice is synchronous CPU conversion.

## Vertex production and live direct-draw integration (2026-09-06)

MetalGeometryVertexPipeline now executes the direct callable vertex IR once per
input-stream entry/instance, excluding restart tokens. It stores position and
sparse generic float4 outputs in private buffers. MetalGeometryObject copies
those retained results by primitive ordinal, provides PrimitiveId, and launches
the geometry invocation groups. Strip vertices are no longer recomputed in
every primitive's object function. Repeated index occurrences remain separate
entries; this does not promise unique-index vertex execution.

The native atomic regression observes 21 stores for seven useful stream entries
and three instances, rather than 27 stores from primitive-local execution. It
checks vertex/instance IDs and both base values, including a negative base vertex
and the non-native instance-ID profile. GeometryLayout owns the float4 record
stride, generic mask and payload alignment. The consumer validates layout
identity, not only equal byte sizes.

The direct Metal graphics cache now distinguishes native vertex functions from
callable vertex artifacts with mesh stages. It compiles the vertex compute PSO
against the actual compact vertex layout and caches the associated object/mesh
render PSO using the full render-pipeline key. Graphics resource preparation
consumes geometry descriptors at stage 3, before fragment descriptors. The
rasterizer records assembly, retained vertex production and the mesh draw in
the existing scheduler, preserving framebuffer state and fragment resources.
There is no new draw-path finish/readback/global wait or SPIR-V translation.

The pixel test now uses this production geometry pipeline cache and production
compute/mesh resource binders, including a cache hit. Its conditional emit
counts 0/2/3/4/6 still test the left/right colors, primitive cut and empty gap.
Per-buffer bounds accompany explicit vertex fetch. A complete attribute must
fit the bound range before loading; invalid fetches leave zero-initialized
inputs. This is native memory containment, not a claim that unspecified guest
out-of-range fetches reproduce a particular hardware bit pattern.

Interrupted integration prerequisites / limits, not completion:
- Indirect geometry input still needs GPU argument expansion. The rasterizer
  reports this explicitly before preparing a draw; no CPU wait fallback was
  added. Raw tessellation and unsupported mesh/device capabilities remain errors.
- Earlier hypothesis, invalidated as a Metal-specific divergence: uint8/quad
  first-index and base_vertex handling differs from the expected interpretation
  of the parameter names, but matches the literal Eden call chain. See the
  index conversion audit above; no speculative Metal-only correction is justified.
- Native vertex producer subgroup semantics, multi-invocation raster ordering,
  layer/viewport/stream/XFB interfaces and actual guest shader compilation still
  require the original goal's verification. Bounds checks do not establish these.
- Actual game rendering, same-scene Eden comparison and release performance
  measurement remain pending. The user passing the Vulkan exception point does
  not validate this new Metal route.

Verification after integration: complete release suites pass (shader_recompiler
562, video_core 1665, one pre-existing ignored test), including bounded native
vertex fetch and the production mesh-cache pixel chain. No Rust warnings.
Log: /tmp/ruzu-geometry-live-final-tests.log. The optional
metal-spirv-validation configuration also checks successfully; the native mesh
path does not use that compatibility translator.

GUI bundle rebuild log: /tmp/ruzu-geometry-live-final-app.log. The old GUI process
72681 was still open at the last check, with the previous Vulkan game stopped
according to /tmp/ruzu-geometry-capture-run.log. Asked the user to close it before
a native Metal run; do not open a duplicate. Preserve configuration: let the user
choose Metal and press the title-screen button manually. Do not attribute the
old process's behavior to this newly rebuilt bundle.

## GPU index conversion prerequisite (2026-09-06)

Uint8Pass and QuadIndexedPass now live in metal_compute_pass.rs, with separate
native host shaders corresponding to Eden's Vulkan files. BufferCacheRuntime
delegates conversion and retains the staging buffer plus its returned offset.
The former CPU read/convert and finish_all calls are removed. Device-local
staging allocations retire by GPU tick; conversion is recorded in the same
scheduler as its producer and consumer, with a compute buffer barrier.

Native API-validation tests pass for all three input widths, wrapping base
addition, list/strip swizzles, fixed uint8 restart remapping, unaligned byte
offsets, empty and invalid ranges, and dispatches across 1024-thread boundaries.
A private source written by an unsubmitted GPU copy feeds two conversions;
their tick does not advance and their live outputs are distinct allocations.
Only the test's final download waits. This proves ordering for these exercised
paths, not complete game rendering or a measured performance gain.

The upstream header, implementation, host shaders and BindIndexBuffer call edge
were reread. DIFF.md records native API/ownership adaptations and the retained
empty-strip guard. The earlier first-index hypothesis remains invalidated as
a Metal-specific divergence; source parameter names alone are not evidence.

Pre-existing contract identified here (resolved by the following slice):
Before claiming general geometry
support: MetalRasterizer returns early for FrontAndBack culling, which can skip
vertex/geometry shader side effects. Correct handling must distinguish triangle
rasterization from line/point output and preserve shader execution. It is not
part of the index conversion correction.

Verification for this prerequisite: full release suites pass (562 shader tests,
1669 video tests, one pre-existing ignored test); no Rust warnings and no diff
whitespace errors. Focused native tests also pass with Metal API validation.
Logs: /tmp/ruzu-geometry-index-full-tests.log and
/tmp/ruzu-geometry-index-pass-tests3.log. The application rebuild uses
/tmp/ruzu-geometry-index-app.log and explicitly preserves the bundled MoltenVK.
Rebuild completed successfully; codesign --verify --deep --strict passes.
The old GUI process 72681 is still open, so no duplicate instance was launched.

## Cull-both shader execution (2026-09-06)

Removed the MetalRasterizer early return for FrontAndBack. Eden's UpdateCullMode
only sets raster state; it does not suppress shader execution. Metal lacks
front-and-back culling, so the pipeline cache selects non-rasterizing PSOs for
triangle output, with void native vertex entry points. The callable producer
before mesh execution keeps its output records. Disabled PSOs omit fragments.
The policy uses the geometry output topology when present: input lines emitting
triangles are culled, input triangles emitting lines/points are not.

Native GPU tests with API validation verify three atomic vertex writes with
rasterization disabled and one geometry atomic write even when all emitted
triangles are suppressed. The same mesh test checks visible triangle colors,
variable emissions/cuts and black pixels when culled. Policy tests cover guest
rasterization disable and front/back/disabled culling. This is not a game
rendering validation and does not complete query-statistics or XFB support.

Apple's native descriptor contract requires a void vertex return for disabled
rasterization, hence this is a matched shader/pipeline variant, not simply
setting one PSO bit on a normal vertex output function.
https://developer.apple.com/documentation/metal/mtlrenderpipelinedescriptor/israsterizationenabled
Vulkan's cull modes act on triangles:
https://docs.vulkan.org/refpages/latest/refpages/source/VkCullModeFlagBits.html

Full release verification: 562 shader tests, 1671 video tests, one pre-existing
ignored test, no Rust warnings. Native validation logs:
/tmp/ruzu-geometry-cull-gpu-tests.log and /tmp/ruzu-geometry-cull-cache-tests2.log.
The latter contains the intentional caught-panic test, not a failing test.
The app rebuilt successfully (/tmp/ruzu-geometry-cull-app.log); strict/deep
codesign verification passed.

## Native live run / new prerequisite failure (2026-09-06)

The old GUI 72681 had finished game shutdown in its log and was idle at the game
list. TERM and INT did not close it; KILL removed that residual process before
starting the new bundle. No simultaneous emulators were launched.

Current native run: PID 84640, exec session 44215.
Log: /tmp/ruzu-geometry-metal-live.log.
XDG_CONFIG_HOME=/tmp/ruzu-geometry-metal-config.EJtyrm contains a copy of the
original config with Renderer backend=5, backend/default=false. The original
~/.config/ruzu/qt-config.ini remains backend=1, default=true. Original data,
keys, cache and saves were not replaced/deleted. Bundled MoltenVK unchanged.
The log confirms Metal on Apple M2 Pro and direct IR-to-MSL; cold Metal cache
contains zero pipelines at startup. No geometry capture directory was created.

At 00:01:04Z (about 27 seconds after launch), the guest called SetTerminateResult
0x2a2 (UserlandAssert) then svcBreak(reason=0, info1=0x108a968b58, size=4,
debug_buffer_err_code=0). This followed the second OpenAudioOut, but proximity
does NOT prove audio is the cause. GPU submissions/presentation continue after
the guest break; do not equate a live process or increasing frame counts with
game progress. No Metal shader error preceded it, and geometry draws have not
been observed. The previous Vulkan run has no corresponding svcBreak.

Next gate: inspect the guest assertion and verify reproducibility/control run
before blaming geometry or declaring rendering validated. Keep this live process
available for diagnosis; do not open a duplicate. Window IDs were 14666 (main)
and 14667 (render child), but screencapture -l14667 failed to create an image.
No successful visual capture and no automated input were obtained.

## Live follow-up and diagnostic memory correction (2026-09-06)

The user reports that the awaited step passed. The inference that the observed
svcBreak necessarily prevents subsequent game progress is therefore invalidated;
the assertion's thread, cause and effect still need identification. This report
does not yet verify geometry output or the scene reached.

PID 84640 was stopped with TERM before the second run. The sole current GUI is
PID 4205, exec session 60599, with the same copied Metal configuration and bundle.
Log: /tmp/ruzu-geometry-metal-break.log. RUZU_DUMP_BREAK_STACK=1 reproduced the
guest break roughly 26 seconds after launch. No geometry capture exists yet at
/tmp/ruzu-geometry-metal-traced-capture. The capture hook is at the shared
translation boundary in Vulkan pipeline_cache.rs, also consumed by Metal.

LLDB read the actual JIT page table (0x400000000, PageInfo stride 8, low two
attribute bits removed, absolute-offset pointers) and verified nonzero stack
records where the existing diagnostic printed all zeros. Evidence:
/tmp/ruzu-geometry-guest-memory2.log; helper /tmp/ruzu_geometry_read_guest.py.
The debugger detached; no guest writes were performed. Stack-zero/corruption
inferences from the old diagnostic are invalidated. Register x22 points to an
empty string, not an assertion message; nearby text is not evidence of the cause.

Small confirmed upstream divergence corrected: arm/debug.rs read thread names
and backtrace frames from legacy ProcessMemoryData instead of process Memory.
It now reads the canonical page table and releases the memory lock before module
symbolication. The optional SVC stack/debug-buffer dumps use canonical Memory too.
This changes diagnostic accuracy, not guest scheduling or geometry rendering.
Focused regression covers live A32/A64 frame chains crossing pages while the
legacy shadow stays zero, invalid pointers and unsigned address overflow.
Verification: the initial fixture lacked a configured kernel address space for
module discovery; after correcting the fixture, all five focused release tests
pass. The final test also verifies thread names for both execution modes and
both SDK structure versions, including the argument-pointer mismatch guard.
Final log: /tmp/ruzu-geometry-debug-memory-final-focused.log.

Full core release suite was attempted, not declared passing. It reports eleven
failures before waiting in the scheduler test
update_highest_priority_threads_impl_requests_wait_for_non_runnable_dummy_current_thread.
Sampling confirms KAbstractSchedulerLock::unlock -> default_enable_scheduling
-> KScheduler::reschedule_current_hle_thread -> pthread_cond_wait. Only the test
process (69103) was terminated after inspection; the GUI was left running.
The scheduler test passes in isolation. Repeating the suite with all arm::debug
tests excluded reproduces the same eleven failures and wait (15-second bounded
control). This rules out the new diagnostic fixture as necessary to trigger
that suite-order failure, not every possible interaction in the core suite.
Logs: /tmp/ruzu-geometry-debug-memory-full-tests.log,
/tmp/ruzu-geometry-core-test-wait.sample.txt,
/tmp/ruzu-geometry-core-scheduler-isolated.log,
/tmp/ruzu-geometry-core-without-debug-tests.log.
No Rust warnings in the final release compilation; git diff --check passes.
The app has not been rebuilt/restarted for this diagnostic correction, to keep
the user's current scene available. DIFF.md records the ownership comparison.

A current full-screen capture command succeeded but produced an entirely black
desktop image, so it is not accepted as a game rendering capture. A user scene
description/capture has been requested. Do not restart the current game just
because the capture or log cannot establish its scene.

## Guest assertion identified from live SDK imports (2026-09-06)

The sole GUI remains PID 4205; neither a new emulator nor an input sequence was
started. LLDB attached only to read memory, then detached successfully. The
current renderer log still contains no geometry capture. The user's report of
progress remains distinct from a verified geometry-rendered scene.

The live frame chain (not the obsolete shadow) leads through main 0x81819c60
and nnSdkEn 0x82b9f77c. Separate short code captures and loaded module images
were compared byte-for-byte to verify main base 0x80df1000 and nnSdkEn base
0x82b08000. The decompilation skill's AArch64 PLT/GOT and ELF64 dynamic-symbol
helpers then resolve these calls:

- main 0x81e35a40: nn::audio::OpenDefaultAudioOut(AudioOut*, SystemEvent*, params).
- SDK 0x8301bae0: nn::audio::OpenAudioOut(AudioOut*, const char*, params).
- SDK 0x83018790: nn::os::AttachReadableHandleToSystemEvent(...).
- SDK 0x83018030: nn::diag::detail::AbortImpl(...).

The SDK code at 0x82b9f738 reads the SystemEvent state byte at +0x28 and
branches to AbortImpl when nonzero. Its return address is the captured
0x82b9f77c. The main caller passes global event 0x82a9edb0; the module's
initializer explicitly zeros that byte. The two HLE OpenAudioOut calls are
one second apart. Thus the assertion site is an audio-event initialization
precondition, not the typed geometry exception. This does NOT establish why
the event is still initialized, whether the first open was retried after a
different failure, or rule out earlier memory corruption from another subsystem.

Adjacent code resolves GetAudioOutState, StopAudioOut, CloseAudioOut and
StandardAllocator::Allocate. The first-open failure/retry path and event
lifecycle need tracing; do not reset the byte, skip AbortImpl or assume a
particular allocator failure. Examined AudioOut channel validation/selection
matches Eden (0/2/6 accepted; <=2 maps to 2, otherwise 6), so no correction was
invented from this inspection. No production code changed in this follow-up.

Evidence: /tmp/ruzu-geometry-assert-code contains the two loaded memory images
and short code samples; /tmp/ruzu-geometry-assert-symbols.log records verified
bases and symbol resolutions. Helpers: /tmp/ruzu_geometry_read_assert_callers.py,
/tmp/ruzu_geometry_read_modules.py, /tmp/ruzu_geometry_resolve_calls.py. The LLDB
readers are deliberately bound to this PID/address layout and must not be reused
on another run without revalidating its page table and module bases.

Next runtime gate: obtain the actual current scene, then trace first AudioOut
open result, subsequent guest return path and SystemEvent finalization if a new
run is needed. Native indirect geometry input remains explicitly rejected in
draw_impl; no silent fallback or CPU snapshot was introduced. End-to-end geometry
pixels, same-scene Eden comparison, regressions and release cost remain open.

## Replayable capture prerequisite (2026-09-06)

The user reports "c'est passe". This is progress past an unspecified screen,
not yet confirmation of gameplay geometry. Asked which scene was reached;
the sole GUI PID 4205 remains running. No new geometry capture exists in its
current directory and no new user screenshot is available. Do not restart it
or infer a rendered scene from process liveness alone.

Offline inspection found a capture gap: the 15 existing text dumps contain
Maxwell/IR/runtime/key but not the entire translation environment. Vulkan's
CreateGraphicsPipeline returns on the typed shader exception before writing
its regular disk entry. The little-endian bytes of all three captured GS hashes
are absent from the title's vulkan.bin, so that cache cannot supply the missing
complete entries. Do not substitute guessed texture/CBUF metadata to replay them.

RUZU_DUMP_GEOMETRY_PIPELINES now additionally captures <pipeline-key>.bin once,
at the end of shared translation and before native compilation. It uses the
existing environment serializer, preserves all active stages and lookup maps,
and refuses environments with unbound instructions. The ordinary cache and
capture sources remain untouched. This is diagnostic-only; it does not change
draw behavior or scheduling.

An explicitly ignored manual test compiles a captured pipeline's direct MSL
stages, native vertex producer and object shader using the real Metal device:

```sh
RUZU_REPLAY_GEOMETRY_PIPELINE=/path/to/capture.bin \
  cargo test -p video_core --release native_geometry_shader_stages_from_capture \
  -- --ignored --nocapture --test-threads=1
```

The loader receives a temporary copy because LoadPipelines deletes invalid
files. This test is not yet run against a real geometry capture and is not a
pixel oracle or full framebuffer PSO validation. Prior native synthetic pixel
tests remain separate evidence.

Replay explicitly rejects keys whose serialized prefix omits dynamic vertex
formats/strides. Such Vulkan keys require a separate live draw-state snapshot;
silently replacing omitted inputs with defaults would not be a faithful Metal
vertex-producer replay. The native Metal path currently keeps this state fixed.

Verification: capture round-trip preserves the graphics key, all three stages
and cached words, does not append a duplicate key, and rejects unbound code
without creating a file. Full video_core release suite: 1672 passed, 0 failed,
2 ignored (the new manual replay and one pre-existing test); no Rust warnings.
Logs: /tmp/ruzu-geometry-environment-capture-tests2.log and
/tmp/ruzu-geometry-environment-full-tests-final.log (repeated after adding the
dynamic-input replay guard). DIFF.md records the explicit diagnostic ownership
difference. Release ruzu executable rebuild succeeded without Rust warnings:
/tmp/ruzu-geometry-environment-app-build.log, target/release/ruzu at 02:52 local.
The running app bundle remains the earlier 01:59 build. Packaging the new
executable with the same bundled MoltenVK and restarting is deferred until the
user's current scene has been established; no session was interrupted here.

## Flat interpolation and empty assembly validation (2026-09-06)

Native pixel regression reproduced a wrong flat color on the odd triangle of
a four-vertex strip: blue instead of green in first-provoking mode. The output
mesh indices preserved winding but selected the wrong first vertex. Evidence:
/tmp/ruzu-geometry-flat-before.log. First and last provoking modes are now
explicit direct-MSL geometry options, forwarded from the existing fixed-state
key. No Metal hardware capability is fabricated, and non-GS selection is
unchanged. The key test also checks hash/serialization separation of the modes.

The native test now covers both provoking modes on triangle and line strips,
backface culling, and smooth interpolation equivalence. Earlier first/last
triangle and line pixel passes are in /tmp/ruzu-geometry-flat-after.log and
/tmp/ruzu-geometry-flat-lines.log. The expanded full native run is still being
verified; do not infer it passed from those earlier narrower runs.

Metal API validation uncovered two empty-draw defects in the native assembler:
1. An empty index range ending at buffer.length() was bound even though Metal
   disallows that binding offset. Initialization/classification/scans now do
   not run for zero inputs; the final argument writer still executes once.
2. The minimum four-byte buffer allocation was too small for the bound uint2
   pointer types. Empty output storage now reserves one element of its type,
   without inventing an input vertex or changing the zero primitive count.
Failing logs: /tmp/ruzu-geometry-provoking-native-tests.log and
/tmp/ruzu-geometry-provoking-native-tests2.log. The existing empty-range oracle
exercises both under MTL_DEBUG_LAYER=1; final rerun pending below.

Invalidated hypothesis: triangle-strip/fan GS input cyclic order was itself a
Vulkan violation. The normative Geometry Shader Input Primitives section
explicitly allows different absolute triangle input order with the same winding;
adjacency main vertices must stay at indices 0/2/4. Kept the existing input
order and fixtures. Output provoking identity is a separate, confirmed issue.
Sources re-read: Eden fixed_pipeline_state.cpp/.h, vk_graphics_pipeline.cpp/.h,
emit_spirv_special.cpp; Vulkan geometry.html and drawing.adoc; Apple MSL mesh
flat-input specification (page 190 of the June 2026 specification).
https://docs.vulkan.org/spec/latest/chapters/geometry.html
https://developer.apple.com/metal/Metal-Shading-Language-Specification.pdf

The sole GUI PID 4205 remains alive; the user's "c'est passe" still does not
identify a scene. No app restart, gameplay success claim or commit was made.

Final verification for this slice:
- /tmp/ruzu-geometry-provoking-native-tests3.log: 163 native Metal tests passed,
  1 external-capture test ignored, with MTL_DEBUG_LAYER=1. Empty assembly now
  passes both binding-offset and typed-buffer-extent validation.
- /tmp/ruzu-geometry-flat-smooth-lines.log: added complete framebuffer equality
  for smooth line strips as well as triangles, first versus last mode. Both
  covered pixels and interpolated values are equal; flat outputs are checked
  independently against explicit per-vertex colors.
- The pixel test now uses compile_msl_library, including the production safe
  math policy, instead of ad-hoc default Metal compilation options.
- /tmp/ruzu-geometry-provoking-safe-math-full-tests.log: final full release
  suites, with Metal API validation, pass: shader_recompiler 562, video_core
  1673; two video tests explicitly ignored (external capture and pre-existing).
  No Rust warnings. These are native synthetic tests, not a native-Vulkan GS
  hardware differential run or evidence of the title's actual gameplay pixels.
- GUI executable rebuild succeeded in /tmp/ruzu-geometry-provoking-app-build.log
  (release, 1m13s, no Rust warnings). target/release/ruzu contains these fixes;
  the running bundle has not been replaced. Repackage with its existing bundled
  MoltenVK only when a restart is appropriate; do not launch a second instance.

## Layer output slice started (2026-09-06)

Rechecked prerequisites before adding GS interfaces. MetalFramebuffer already
retains array attachment views and sets RenderTargetArrayLength from the common
cache range, like Eden Framebuffer::CreateFramebuffer. The initial claim that
this was missing is invalidated (case-sensitive search missed the setter).
No replacement framebuffer or parallel layer store is needed.

Layer output is genuinely rejected by direct MSL SetAttribute. Implement its
geometry-owned per-primitive mesh output and validate two emitted primitives
into separate layers through the existing framebuffer/view/scheduler path.
Eden bitcasts float IR payloads to uint for Layer; preserve those raw bits.
Vulkan requires the same layer on every vertex of a primitive, allowing a
per-primitive Metal record captured when that primitive becomes complete.

Viewport output is a separate interrupted slice: MetalRasterizer currently
binds only viewport/scissor 0. Port the device-limited arrays and their state
conversion before enabling that shader interface. Do not silently map the
guest's nonzero viewport to zero. No completed viewport support is claimed.

Layer implementation: GeometryLayout now declares a per-primitive uint Layer
interface; MslEmitContext owns the current record and snapshots it only when
EmitVertex forms a complete primitive. The GS input/vertex payload ABI and
existing image/view/framebuffer ownership are unchanged. No device capability,
renderer scheduling or normal framebuffer layer count was altered.

SetAttribute was moved from the dispatcher into emit_msl_context_get_set.rs.
Its output vertex argument is now ignored, as in Eden; the old immediate-zero
restriction was an unnecessary rejection. Tests compare generated source for
0, 9 and UINT_MAX operands in VertexB and Geometry. Layer tests require the
literal nested bitcasts for raw values 7 and 2 and verify capture ordering.

Native test uses a private MetalImage, common ImageViewBase/MetalImageView,
MetalFramebuffer pass and scheduler-recorded download. GS emits primitives to
layers 1 and 2 of a three-layer attachment; layer 0 must remain fully clear,
and each destination must not contain the other primitive. The fragment reads
render_target_array_index into alpha, checking the actual mesh/fragment link.
First and last provoking modes both pass for triangles. Layered line variants
are included in the final rerun; verify its result before claiming those pass.

Verified so far: /tmp/ruzu-geometry-layer-full-tests2.log reports 564 shader
tests and 1673 video tests passing, two explicit ignored video tests, Metal API
validation enabled and no Rust warnings. The final stronger rerun is in
/tmp/ruzu-geometry-layer-full-tests-final.log. This is synthetic evidence only;
the running GUI remains PID 4205 on its earlier bundled executable.

Sources re-read: Eden OutputAttrPointer/EmitSetAttribute, DefineOutputs and
Framebuffer::CreateFramebuffer (headers and implementations); MSL mesh
primitive attributes (specification page 77); Vulkan Layer requirements:
https://docs.vulkan.org/refpages/latest/refpages/source/Layer.html
https://developer.apple.com/metal/Metal-Shading-Language-Specification.pdf

## Layered-line slice interrupted: independent native failure (2026-09-06)

The stronger suite does NOT pass: 564 shader tests pass, but the layered-line
pixel case fails (video_core 1672 passed, 1 failed, 2 ignored). A blue horizontal
line whose fragment reads Layer=2 is rasterized into attachment layer 1. Do not
package the new executable as a verified Layer implementation.

An independent Swift/Metal reproducer, /tmp/ruzu-mesh-layer-probe.swift, removes
ruzu entirely: hand-written mesh source, no object shader, no cache, no guest
resources, shared array texture with getBytes after command completion. It
reproduces the same failure. The triangle control routes layers correctly.
/tmp/ruzu-mesh-layer-probe.log reproduces it with MSL 3.0, 3.1, 3.2 and 4.0;
/tmp/ruzu-mesh-layer-probe-types.log reproduces it with uint/ushort/uchar Layer
and maximum primitive counts 5 and 8. Metal API validation reports no error.
These tests isolate this case from ruzu's readback, cache and IR translation;
they do not prove all devices or drivers fail. No feature bit or library changed.

Stop extending Layer/Viewport on the assumption that native line meshes suffice.
Prerequisite: make geometry IR callable with an explicit output transport, so
one execution captures vertices, indices and primitive values before raster
replay. It must preserve guest side effects exactly once. Do not replay the GS
once per output primitive, shift layer indices, or insert a device-idle wait.
The first prerequisite slice is the typed callable-function ABI and native
capture test; backend replay/integration and the failing layered-line oracle
remain to complete before this interrupted slice can resume.

Callable prerequisite implemented in msl_function.rs, emit_msl.rs,
msl_emit_context.rs and emit_msl_geometry.rs. It changes the function ABI only,
not resource allocation or the emitted guest body. Native test in
metal_shader.rs executes six EmitVertex operations and a cut into thread-local
capture storage. It verifies four segments, indices [0,1,1,2,3,4,4,5], primitive
layers [1,1,2,2], all six position snapshots and one SSBO atomic increment.
/tmp/ruzu-geometry-callable-prerequisite-tests.log: both focused tests pass
with Metal validation, release build and no Rust warnings. Full rerun:
/tmp/ruzu-geometry-callable-full-tests.log (retain the layered-line failure).

Next implementation decision: runtime output storage and raster replay must
avoid native mixed-Layer line mesh routing while preserving single execution
and primitive order. Options are captured output plus conventional vertex
rasterization, or object-stage capture plus per-primitive mesh groups if the
payload-size and ordering contracts are verified. Do not duplicate guest GS
execution per output primitive. No runtime fallback has been installed yet.

Full prerequisite verification: shader_recompiler 565 passed; video_core 1673
passed, 1 failed, 2 ignored. The sole failure remains the original layered-line
pixel oracle (blue in layer 1 instead of 2), not the new callable capture test.
No Rust warnings; git diff --check is clean. No new bundle was packaged or
restarted; PID 4205 is still the sole GUI. The working tree remains unfinished.

## Layered primitive replay integrated (2026-09-06)

The interrupted layered-line pixel case now passes without changing its
expected pixels. MetalGeometryCapture executes the callable GS into a tracked,
private GPU buffer. A GPU argument kernel derives input-primitive/invocation
dispatch dimensions from the existing assembler's output; no counts or vertex
records are read back on the CPU. Each replay object reads that invocation's
emitted count and dispatches one mesh per emitted primitive. Each mesh contains
one Layer value, avoiding the mixed-Layer native line-mesh failure established
by the standalone control. GS instructions and their stores are not replayed.

Ownership: new metal_geometry_capture.rs owns native capture/replay pipelines,
record allocation and their commands. MetalGeometryPipeline chooses Mesh or
Capture explicitly; shader compilation selects capture for Layer-writing GS,
and graphics preparation uses the same binding layout in either case. Draw
ordering is now input assembly -> retained VS results -> GS capture (when
needed) -> begin render pass -> primitive replay. Non-Layer mesh paths remain
unchanged. Guest textures/buffers are prepared once, before this sequence.

The capture record's stride comes from Metal pipeline reflection, including
padding, not a guessed Rust struct or a GPU-to-CPU sizeof query. Reflection
filters the buffer namespace explicitly. Checked allocation arithmetic bounds
the per-draw maximum; producer/consumer input layout is validated before any
capture commands. Capture and replay use tracked buffers and ordered encoder
transitions, with buffer barriers between compute dependencies. No production
finish_all, wait, skip, fake Layer value or CPU snapshot was added.

Native matrix expanded to zero/one/two/incomplete output, cuts, cull-both,
first/last provoking modes and two layered topologies. It also renders four
ordered red/blue transparent pairs using two instances and two GS invocations,
checking the noncommutative blended result and the exact side-effect count.
/tmp/ruzu-geometry-capture-replay-verified.log: 565 shader tests and 1674 video
tests pass, two explicit ignored video tests. The final stronger rerun also
checks a weighted InvocationId counter; inspect its result before claiming it.

Still unproven: native layered points, cross-device ordering/limits, actual
title geometry pixels, fresh captured shader environments, Eden same-scene
comparison and release performance. Viewport arrays, indirect GS input and
other goal slices are unchanged. Synthetic success is not goal completion.

Final Layer replay verification: /tmp/ruzu-geometry-capture-replay-final.log
passes all 565 shader and 1674 video tests, with two explicit ignored video
tests and Metal API validation enabled. Weighted InvocationId totals also pass
(IDs 0/1 in each of two instances produce sum 6, not 4). No Rust warnings and
git diff --check is clean. GUI release rebuild is recorded separately in
/tmp/ruzu-geometry-capture-replay-app-build.log; verify completion before use.
The running bundle has not been replaced and no second instance was launched.

Points and record alignment are now covered as well:
/tmp/ruzu-geometry-capture-replay-points.log passes 565 shader and 1674 video
tests (2 explicitly ignored), with Metal API validation and no Rust warnings.
The matrix now uses explicit Point/Line/Triangle output topology, not an
implicit line-only switch. Layered points of size 4, zero output and partial
counts hit the expected layers and pixels. Record allocation rounds compiler
reflection's bufferDataSize to bufferAlignment with checked arithmetic; all
record field access and indexing remains compiler-owned MSL.

Next priority after rebuilding: validate the same scene and real geometry
environments in the GUI, and continue the stopped viewport/input interfaces.
Do not treat the native tests as evidence that the running earlier bundle uses
the new capture path. It still has not been replaced or restarted.

Release GUI rebuild completed successfully (1m08s, no Rust warnings) in
/tmp/ruzu-geometry-capture-replay-app-final-build.log. target/release/ruzu
contains the final capture/replay code, aligned record allocation and input
layout validation. The running ruzu.app bundle is still the older build;
package only at the next appropriate restart, preserving its bundled MoltenVK.

## Current runtime gate: repeatable SDK audio assertion (2026-09-06)

Revalidated the prior GUI rather than trusting the old log or treating missing
screenshots as black rendering. Sample of PID 4205 at 04:36 local shows the GPU
thread waiting in SynchState::pop_wait, cores 0/3 idle, core 1 executing guest
SVC/dispatch work, and core 2 continuously in the same generated block. LLDB
confirms that block polls halt_reason then branches to itself; its exit writes
guest PC 0x82c881e8. The independently captured SDK bytes at that PC contain
`b 0x82c881e8`, immediately after the SDK's svcBreak call. This establishes the
audio assertion thread's non-returning state, not a mesh compiler hang or proof
that every other guest thread has stopped. The earlier user report of passing
an unspecified screen is retained; it does not identify geometry-rendered pixels.

The old GUI did not exit after TERM and was stopped with KILL before relaunch.
Packaged the already tested release executable using build-macos-app.sh
--no-build and explicitly preserved the existing bundled libMoltenVK.dylib.
Packaging succeeded: /tmp/ruzu-geometry-capture-replay-package.log.
The sole replacement GUI is PID 20849, exec session 66492, using the same copied
configuration directory /tmp/ruzu-geometry-metal-config.EJtyrm. User config,
saves and caches were not deleted or replaced. No input was sent automatically.
Log: /tmp/ruzu-geometry-metal-capture-replay-run.log. Geometry capture target:
/tmp/ruzu-geometry-metal-capture-replay-20260906 (no captured pipeline yet).

The updated build reproduces the same SDK assertion approximately 26 seconds
after launch, without input. OpenAudioOut occurs at 02:44:09Z and 02:44:10Z,
followed by SetTerminateResult(0x2a2) and svcBreak. Both new module code samples
match the prior captured samples byte-for-byte after relocation by -0xa88000;
the current main/SDK bases are 0x80369000/0x82080000. Thus this is the same
OpenDefaultAudioOut SystemEvent initialization assertion, not merely a matching
error string at an assumed old address.

Read-only guest inspection finds an AudioOut record containing 48000 Hz,
6 channels, sample format 2 and stopped state 1. Its containing middleware
object has a null buffer-array pointer at +0x1a8. This is a snapshot during the
second initialization, NOT proof that the first allocation failed: the object
may have been reset/reused. SDK OpenDefaultAudioOut successfully initializes the
event before returning; its caller can subsequently return failure for several
allocation sites. The decisive next trace must distinguish the first open's
return, channel-count lookup, allocation returns and cleanup/retry. Do not reset
the event byte, replace parameters or skip the assertion.

Window capture is still unavailable: ScreenCaptureKit identifies the parent
and native render-child windows but both capture attempts return SCStreamError
-3811. The AppKit/MainActor helper fixes its earlier CGS initialization crash,
not the capture failure. These failures are not accepted as pixel evidence.
Helpers/logs are /tmp/ruzu-geometry-window-capture.swift,
/tmp/ruzu_geometry_audio_state.py, and /tmp/ruzu-geometry-audio-state*.log.
The reader is explicitly PID-bound and verifies module bytes before inspecting
guest data. No production audio, JIT or geometry code was changed in this
runtime investigation. Actual geometry pixels and the original completion gates
remain unverified; the next prerequisite is resolving the reproducible guest
initialization failure, alongside the previously recorded viewport/input slice.

### Audio initialization hypothesis invalidated by IPC/callback traces

The follow-up runs prove the first AudioOut initialization succeeds: four
12288-byte buffers are appended, Start returns success, GetState reports
Started, and GetReleasedBuffers returns the first tag. Only later does the
guest Stop/close/reopen and encounter the already-initialized SDK SystemEvent.
The null array observed above was from the second initialization; initial
allocation failure is an invalidated hypothesis, not the current diagnosis.

Evidence: /tmp/ruzu-geometry-audio-cubeb-ring.log and
/tmp/ruzu-geometry-audio-callback-ring.log. Both null and Cubeb control runs
remain at 1200 expected played frames (the upstream five-frame lead), releasing
one 1024-frame buffer but never completing the others. The targeted Cubeb run
logs Started/Stopped state callbacks but no data callbacks. A Started state
callback is not evidence that host audio is advancing.

The independent /tmp/ruzu-audioqueue-callback-probe control creates an AudioQueue
successfully but AudioQueueStart returns -66681 (kAudioQueueErr_CannotStart).
Its three callbacks are priming/buffer returns, not sustained playback. This
result was reproduced after rechecking the sole GUI PID 85568. IORegistry
reports the console session locked; ScreenCaptureKit capture also fails. The
lock is a correlated environmental condition, not yet a proven sole cause.
An unlocked host control with successful sustained audio is required before
attributing this validation failure to ruzu scheduling or geometry. No guest
assertion bypass, synthetic audio clock, or production audio change was added.

The latest user report that the screen passed remains recorded, but its scene
and instance are unspecified. The current GUI log contains the same audio
assertion and no newly captured geometry environment. Do not equate the report
with completed geometry pixel validation; ask for the displayed scene.

## Viewport prerequisite: correct active scissor conversion (2026-09-06)

Implemented the single-scissor conversion prerequisite before exposing indexed
viewport shader outputs. Metal previously expanded empty regions to one pixel,
could place that pixel outside the attachment, ignored lower-left origin, and
used ordinary scissors when viewport scale/offset was disabled. The active draw
now calls MetalRasterizer::update_scissors_state. GetScissorState applies Eden's
surface-height flip before clipping; the disabled-transform branch uses
surface_clip, including its upstream minimum extent. Metal-specific attachment
intersection preserves empty coverage without skipping the shader/draw.

An independent Metal control with API validation confirms zero-width/height
scissors are valid, including at the attachment edge. Focused Rust tests cover
nonzero scissor selection, surface/attachment height differences and clipped
lower-left rectangles. The native pixel test checks all pixels for 14 cases
and verifies all three vertex shader atomic side effects even for empty output.
/tmp/ruzu-metal-scissor-tests.log: four tests pass.
/tmp/ruzu-metal-scissor-full-tests.log: 1677 video tests pass, 2 explicit ignored,
Metal API validation enabled, no Rust warnings. DIFF.md contains the source
comparison. No GPU wait/readback was introduced outside the native test.

Still interrupted: the full device-limited viewport/scissor arrays, signed
viewport transform and ViewportIndex interface. This slice does not enable
those features or resolve the environmental runtime/audio validation gate.
The open GUI was not replaced or restarted for these source changes.

Release GUI build completed (1m09s, no Rust warnings):
/tmp/ruzu-geometry-scissor-app-build.log. target/release/ruzu contains the
scissor fix; the open bundle remains unchanged until a deliberate restart.

## Viewport arrays and GS ViewportIndex (2026-09-06)

The previously interrupted viewport slice now has an implementation. Runtime
state updates send matching viewport/scissor arrays, capped at the device's
published limit (Apple5+/Mac2: 16, older families: 1). Signed transforms retain
negative and subpixel extents and off-attachment origins. A native control
disproved any need to normalize negative Metal viewport sizes: both negative
width and height are accepted with API validation. Converting Vulkan's NDC Y
to Metal requires y + height and -height; simply taking abs(height) was wrong.
Unit tests cover origin/swizzle/depth modes and the surface-only path.

Geometry ViewportIndex is now a per-primitive uint field, captured from the IR
float payload by bitcast at EmitVertex. The existing compute capture + mesh
replay path handles viewport-only and combined Layer/ViewportIndex records;
resource bindings, execution counts, primitive order and record allocation
remain unchanged. Unsupported devices reject the store explicitly. The native
test uses an empty viewport-zero scissor and complementary scissors on 1/2:
silently routing either primitive to zero or to the other's viewport fails.
The stronger combined test deliberately uses viewport indices opposite to
Layer values, to detect field aliasing rather than testing equal values only.

Initial complete run /tmp/ruzu-geometry-viewport-full-tests.log passes 566 shader
and 1681 video tests (2 explicit ignored), with API validation and no Rust
warnings. A final rerun follows the named Maxwell ViewportSwizzle enum and the
independent Layer/ViewportIndex test strengthening; do not assume its result.
DIFF.md records the source/ownership comparison. No game was restarted, no
user configuration changed, and no installed MoltenVK library was replaced.

Remaining validation: actual title GS environments/pixels beyond the blocking
scene, read-only Eden same-scene comparison and release measurements. The host
session remains locked and independent AudioQueueStart still fails at the last
check. Non-geometry viewport output, ViewportMask, XFB and indirect GS input
are not newly claimed by this slice.

Final verification: /tmp/ruzu-geometry-viewport-independent-tests.log passes
566 shader and 1682 video tests (2 explicitly ignored), including the enum
encodings and deliberately unequal Layer/ViewportIndex values, with Metal API
validation enabled and no Rust warnings. git diff --check is clean. This is
native synthetic verification, not a claim that the open GUI uses this code
or that the game's geometry has been visually validated.

Final release GUI build succeeded in 1m11s, without Rust warnings:
/tmp/ruzu-geometry-viewport-final-app-build.log. target/release/ruzu includes
the viewport arrays and GS output path. The running ruzu.app was not replaced;
package this executable with the existing bundled MoltenVK at a deliberate
restart, after the host audio/capture gate is resolved.

## Runtime validation checkpoint (2026-09-06)

Completion is not established. The user's "passed" report establishes only
passing the earlier obstacle, not correct geometry pixels in the current
build. The latest inspection still finds one GUI (PID 85568), a locked console,
and its log ending after the guest audio assertion. Sampling finds the GPU
thread waiting for work; this is not evidence of geometry draws executing.
The instance is left open pending clarification of the user's scene.

Remaining acceptance evidence, in order:

1. Restore an interactive session and check the independent AudioQueue control
   again. Its previous CannotStart result is a host prerequisite failure, not
   proof of a geometry defect or proof that locking is the sole audio cause.
2. At a deliberate restart, package the latest release executable into the GUI
   bundle while retaining the existing MoltenVK. The executable currently has
   timestamp 05:41:47, but the bundled executable has timestamp 04:43:13.
3. Capture complete real pipeline environments and verify actual geometry
   draws and pixels after Press A. Partial historic IR dumps and native
   synthetic tests do not replace this gate.
4. Compare the same scene with read-only Eden, validate other title regressions,
   and measure release CPU/GPU cost on the real geometry path. None of these
   three checks is established by the current synthetic test results.

The implementation and tests are preserved in the working tree. No audio
clock substitution, guest assertion bypass, library change, or automatic input
is introduced to get past the external validation gate.

## Unlocked retry: session-close prerequisite (2026-09-06)

The console is unlocked and the independent AudioQueue control now succeeds
(start=0, 92 callbacks). The latest release GUI was packaged with the existing
MoltenVK and started once, PID 40325, using the copied Metal configuration.
Window capture now succeeds. Log: /tmp/ruzu-geometry-unlocked-run-20260906.log.

Geometry validation is interrupted by a separate startup deadlock. Native
sample /tmp/ruzu-geometry-unlocked-launch-sample.txt shows CPUCore_0 in
KClientSession::destroy_with_process -> KSession::on_client_closed_with_process
waiting for the server endpoint, while CPUCore_3 holds that endpoint in
KServerSession::destroy_with_process and waits for the parent session. The
GPU is waiting for commands. Source inspection confirms parent -> server
versus server -> parent lock ordering. Eden does not hold a parent host mutex
across OnClientClosed. Fix and verify this lifecycle prerequisite before the
next game run; do not attribute this startup stall to geometry compilation.

The client-close regression reproduces the original lock cycle in a bounded
test (/tmp/ruzu-session-close-before.log). The correction releases the parent
guard before notification, retaining the logical client reference until that
notification returns. The server destroy order also now retains its reference
through CleanupRequests, matching Eden. Source comparison is in DIFF.md.

Both new concurrent tests pass, as do the sequential client-first/server-first
resource-release tests. The focused session group has 58 passes, 11 failures,
one ignored; excluding both new tests still gives the same 11 failures (56
passes). The async-error fixture also independently fails with DeviceMemory
not initialized. These controls rule out interference from the two new tests,
not all possible effects of the production change. Logs:
/tmp/ruzu-session-close-after.log,
/tmp/ruzu-session-close-focused.log,
/tmp/ruzu-session-close-focused-control.log,
/tmp/ruzu-session-close-existing-fixture-isolated.log.

The complete core test executable was attempted with a 60-second limit and
again reaches the previously documented scheduler wait. Its log is
/tmp/ruzu-session-close-full.log; do not report the full suite as passing.
The deadlocked GUI PID 40325 did not respond to SIGTERM and was terminated
before rebuilding/restarting. No guest data or configuration was edited.

Final release GUI build succeeded in 1m53s without Rust warnings, then the
bundle was rebuilt with the same MoltenVK. The literal cargo test -p core
--release -- --test-threads=1 command was also retried with a 20-second limit:
it reaches the same scheduler wait (exit 124), not a full-suite pass.
Logs: /tmp/ruzu-session-close-app-build.log,
/tmp/ruzu-session-close-bundle.log, /tmp/ruzu-session-close-cargo-full.log.

## Real-title prerequisites exposed after IPC fix (2026-09-06)

Retry PID 90927 gets beyond session teardown, submits/presents images, and
opens audio and save data without the earlier SDK audio abort. This is one
successful startup retry, not a long-run concurrency guarantee. Log:
/tmp/ruzu-geometry-ipcfix-run-20260906.log. No geometry environment capture
has been produced yet in /tmp/ruzu-geometry-ipcfix-capture-20260906.

Two concrete Metal prerequisites now block accurate rendering before geometry:

- Ordinary TriangleFan draws are rejected by metal_primitive_type. The GS input
  assembler already accepts TriangleFan, but ordinary raster draws bypass it.
  Implement actual fan index assembly with restart, base vertex, instance and
  provoking-vertex semantics; do not map fans to strips or discard triangles.
- A real fragment entry point declares samp0 through samp16 with direct sampler
  attributes. The native compiler rejects sampler(16): direct slots are 0..15.
  metal_shader.rs explicitly leaves argument_buffers=false because the matching
  runtime argument encoder is absent. This needs a complete shader/resource ABI
  slice (declarations, host argument encoding, capability checks, residency and
  lifetime across ordinary/geometry paths), not merely toggling the option or
  clamping/aliasing sampler indices. Counted bindings are not proof that all
  guest samplers have distinct state.

Preserved early GUI capture:
/tmp/ruzu-geometry-ipcfix-first-90927-14728.png (black during early launch).
A later ScreenCaptureKit attempt failed with -3811 and produced no image; it
must not be interpreted as a fresh black-frame capture. The failed compile
messages, rather than that capture failure, establish the missing prerequisites.
Stop the repeated-error diagnostic run and implement these prerequisites before
resuming actual geometry pixel validation. No shader/resource workaround added.

## Ordinary TriangleFan GPU assembly (2026-09-06)

The direct, non-GS raster path now converts TriangleFan through the existing
GPU primitive assembler. A final compute pass resolves stream ordinals into
packed UInt32 triangle-list indices and writes the five native indexed-indirect
draw words. Primitive restart determines the output count on GPU; no counter
readback, submission, or global wait is introduced by conversion.

The conversion cyclically rotates each triangle to preserve first/last fan
provoking vertices and winding. Native baseVertex stays in the draw arguments
(including its signed bits and shader builtin); it is not applied twice. Array
draws, indexed offsets, baseInstance and instance count follow the same path.
The assembler's kernels need only MSL 2.3, not the mesh-stage MSL 3.0 requirement.

Native validation-layer tests check exact indices/argument bytes after GPU
uploads and actual flat-colored pixels with culling, two instances and nonzero
vertex/instance bases. Both pass. The test oracle alone waits and reads back.
Full release suites pass with Metal API validation: 566 shader-recompiler tests
and 1684 video_core tests, with two pre-existing explicitly ignored tests and no
Rust warnings. Log: /tmp/ruzu-metal-fan-full-tests.log. Synthetic pixels are not
an actual-title rendering result; the GUI has not been relaunched for this slice.

Scope remains explicit: indirect fan *inputs* are still rejected, rather than
silently treating their original indices as a triangle list. Supporting those
requires GPU argument expansion before primitive assembly. Direct fan output
uses a native indirect draw only to consume the GPU-computed restart count.
This multi-pass conversion is correctness-first and its game-frame cost has not
yet been measured. The 17-sampler argument-buffer ABI prerequisite remains open.

Sampler prerequisite inspection: direct MSL and the renderer share
MslBindingLayout, but ordinary, compute and geometry resource binders currently
bind samplers individually. MetalShaderCompileOptions.argument_buffers is not
wired into native MslOptions. Merely changing the compile option cannot fix this.
The next slice must include sampler argument encoding, supportsArgumentBuffers
on sampler creation, per-device capacity checks, buffer-index allocation and
retention of indirectly referenced samplers until command-buffer completion.

## Sampler argument-buffer ABI (2026-09-06)

Implemented a sampler-only argument-buffer ABI for descriptor counts above 16.
Native MSL emits a stage-specific struct with member IDs matching sampler_index,
removes direct sampler parameters, and reserves one ordinary buffer slot. Data
buffers and textures retain their previous namespaces, descriptor order and
residency rules. Callable vertex and geometry functions use the same explicit
interface, including the new buffer argument.

metal_update_descriptor.rs uses MTLArgumentEncoder, not assumed resource-ID
offsets, to encode scalar/array members into fresh shared, tracked storage.
Graphics, compute, VS-as-compute and geometry bind that buffer and omit direct
sampler bindings. Guest sampler creation (including fallback variants) enables
supportArgumentBuffers. The scheduler attaches one sampler-reference cohort to
each command buffer, releasing it on completion without a GPU wait; the handler
owns the cohort even if the scheduler itself is dropped. Samplers do not use
useResource because they are not MTLResource objects; textures remain directly
bound and keep their existing retention/residency handling.

Native compilation checks buffer/texture/sampler capacities against the selected
device; callable VS and capture GS constructors check the same ABI. Tier-1
devices are not advertised as supporting >16 samplers. The per-stage limit is
distinct from maxArgumentBufferSamplerCount (unique live states per app).

Validation so far: the native test samples 17 distinct LOD-clamp states against
a two-level texture and verifies all 17 colors, both for a sampler array and 17
individual descriptors. No slot clamping or state aliasing is used. A completion
test verifies cohort release after flush; a compiler test checks all four stage
interfaces and the unchanged <=16 path. Its initial fixture erroneously set GS
execution modes on non-GS stages; fixing the fixture, not production validation,
resolved that failure. Full suites including the final constructor checks pass:
567 shader and 1686 video tests, two explicitly ignored video tests, no Rust
warnings. Log: /tmp/ruzu-sampler-abi-final-tests.log. No actual-title success claim
yet; the GUI release build and controlled retry are the next validation gate.

## Cinematic reached; initial performance profile (2026-09-06)

Release GUI PID 14156 includes the fan and sampler ABI prerequisites. The user
confirms that the game displays and reaches the cinematic, reporting about
7 FPS. This supersedes the pending GUI gate above, but does not establish full
geometry pixel parity with the same Eden scene. Five geometry pipeline groups
now have serialized .bin environments plus stage text captures under
/tmp/ruzu-geometry-sampler-abi-capture-20260906.

A five-second macOS sample of this live run is saved at
/tmp/ruzu-lm3-cinematic-sample-20260906.txt. Of 2954 samples of the GPU host
thread, 1107 (37.5%) are under the following chain:

ProcessQueryCondition -> ReadBlock -> FlushRegion -> DownloadBufferMemoryRange
-> MetalScheduler::finish_all -> MTLCommandBuffer::waitUntilCompleted.

Another 269 samples are in native library compilation and 64 in render-pipeline
creation (11.3% combined). These are host-thread sampled wall-time fractions,
not GPU utilization, a whole-frame timing breakdown, or a promised FPS gain.
This short moving-scene sample cannot isolate the cost of geometry emulation.

Metal does not override accelerate_conditional_rendering_with_address; the
default returns false. Maxwell therefore uses its real CPU comparison fallback,
which downloads GPU-modified condition data and waits. Eden's corresponding
ProcessQueryCondition first calls RasterizerVulkan::AccelerateConditionalRendering
and QueryCacheBase::AccelerateHostConditionalRendering. Their existence is not
proof that Eden accelerates these exact conditions on this Mac: device support
and query eligibility still matter. A native GPU predicate/indirect-argument
implementation would require correct query ownership, compare semantics,
clear/draw coverage and synchronization; removing the readback wait or returning
true without actually enforcing predicates would be incorrect.

Rendering has another explicit prerequisite: 1542 tessellation-control lowering
rejections were counted in the run log at the time of inspection. The whole log
was only about 452 KiB at the earlier size check; repeated logging is not yet
established as a major performance cause. No >16-sampler compilation failure
or TriangleFan rejection appeared in that error tally. Native tessellation is
still unimplemented and affected draws are rejected, so this is not a complete
rendering success. Preserve this distinction before optimizing the geometry
path or interpreting the cinematic's visible pixels as complete.

No renderer or synchronization behavior changed during this profiling slice.
The live GUI is left running for the user. Follow-up: validate the captured real
geometry environments, implement the missing tessellation prerequisite before
claiming complete rendering, and compare warm-scene conditional-readback timing
against the native GPU execution cost without introducing a global-wait bypass.
## Active prerequisite: native tessellation (2026-09-06)

The geometry slice is paused at real-scene validation while native tessellation
is missing. Both MetalPipelineCache and MslEmitContext explicitly reject TCS/TES.
Required ownership: callable TCS/patch interfaces in the MSL backend, native
patch/factor buffers and draw scheduling in renderer_metal, and device-supported
tessellation domain/spacing/output handling. Do not bypass these rejections or
pretend that a patch is an ordinary triangle.

All five serialized geometry captures from PID 14156 replay successfully through
the native stage compiler, callable vertex producer, and object shader compiler
with Metal validation. Four have triangle input / triangle-strip output capped
at six vertices; one has line input / triangle-strip output capped at fourteen.
None of these layouts writes Layer or ViewportIndex. Replay logs are under
/tmp/ruzu-real-geometry-*-20260906.log. These are compilation checks, not GPU
execution-time measurements or comparisons of pixels with Eden.

The existing complete-environment diagnostic now has an independent opt-in
RUZU_DUMP_TESSELLATION_PIPELINES directory. It captures pipelines containing TCS
or TES even without GS, including all participating stages, the full fixed key,
runtime information and IR, before native backend rejection. Geometry-only
captures keep their previous switch and format. No game data or fake shader
fixtures are committed. This capture is the next prerequisite-inspection gate;
it does not add tessellation support by itself.

Fresh PID-filtered GUI capture:
/tmp/ruzu-lm3-cinematic-14156-0934-14780.png shows Luigi standing in the hotel
hall, beyond the cinematic, with the scene visible. The displayed 6 FPS was
captured while tests were compiling and is not a clean performance baseline.
The earlier 7 FPS user observation and pre-build five-second profile remain
the useful measurements. A matching Eden image is still required for parity.

The expanded capture passes the five-stage environment round-trip regression;
full release suites pass again (567 shader tests, 1686 video tests, two ignored,
no warnings), in /tmp/ruzu-preraster-full-tests-20260906.log.

Native tessellation architecture reference (not yet implemented here): Apple's
Metal Programming Guide describes compute-produced patch factors followed by
the hardware tessellator and a post-tessellation vertex function:
https://developer.apple.com/library/archive/documentation/Miscellaneous/Conceptual/MetalProgrammingGuide/Tessellation/Tessellation.html
This is distinct from adding another mesh entry point; required TCS/TES patch
interfaces, factor representation, barriers and supported domains must be
verified from the captured guest programs before selecting the complete path.
## Integration with origin/main 1f700f99 (2026-09-06)

Fast-forwarded fix/vulkan-geometry-stream-exception from d30e3244 to 1f700f99
and reapplied all local work, including untracked native Metal modules. The only
stash conflict was append-only DIFF.md content; both audit histories were kept.
Recovery stash retained: 9385df03abcf37d471cbc6f47f735570e66fb427.

The new base includes unified kernel waits, runtime-copy/idle-worker
optimizations and an x64 FMA build correction. It does not replace the local
session teardown corrections: k_session.rs and k_client_session.rs are unchanged
relative to d30e3244; k_server_session.rs changes wait notifications only and
still closes its parent reference before CleanupRequests without the local fix.
Kept both lifecycle fixes, removed the dead unsafe request-forwarding wrappers
and obsolete process-taking server wrappers, and documented the closed predicate
semantics in the concurrent tests.

Validation on the combined base after wrapper cleanup: both endpoint lifetime/
lock-order regressions and all six KSession tests pass in isolated processes.
The ordinary serial core suite reports eleven failures before stalling for over
60 seconds at the previously observed scheduler dummy-current-thread wait test;
only that test process was terminated. Log:
/tmp/ruzu-core-integrated-suite-20260906.log. This is not a passing full-core
validation or an isolated comparison of every failure against clean main.

Combined shader/video release validation also passes after integration:
567 shader tests and 1687 video tests, two explicitly ignored video tests,
no Rust warnings. Log: /tmp/ruzu-main-integration-video-tests-20260906.log.
The GUI was subsequently rebuilt in release and rebundled on this combined
base, retaining the installed MoltenVK. The previous PID 14156 was stopped.
Its observed performance is not a measurement of the newly integrated tree.

### Guest abort during locked-session validation (2026-09-06)

New-base PID 84697 invoked SetTerminateResult(0x2a2), then svcBreak(reason=0,
info1=0x10f4257b58, info2=4), about 28 seconds after launch, following the
second OpenAudioOut. The user subsequently confirmed the frozen image.
This is a guest assertion, not evidence of slow shader compilation. The log is
/tmp/ruzu-main-tessellation-run-20260906.log. A three-second host sample in
/tmp/ruzu-frozen-84697-sample.txt shows GPU waiting for commands and guest CPU
activity, not the previously identified parent/server session-lock deadlock.

The console was locked during that launch. After unlock, the independent
AudioQueue probe reports new_output=0, start=0, callbacks=93, stop=0, dispose=0.
This supports retesting the previously observed audio/environment dependency;
it does not prove the lock alone caused this assertion or exclude a regression.
PID 84697 was stopped (SIGTERM did not exit; SIGKILL was then required).
The same bundle was relaunched as PID 1708 with Cubeb, geometry/tessellation
capture switches and the existing break-stack diagnostic. Log:
/tmp/ruzu-unlocked-main-retry-20260906-1300.log.
No production-code change or assertion bypass was used for this control.
The unlocked retry reaches the visible Press A title screen without svcBreak
after more than one minute; queued frames advance to 1024. Fresh PID-filtered
capture /tmp/ruzu-unlocked-main-1708-first-14917.png shows the title at 17 FPS.
This single retry avoids the earlier abort, but does not establish its root
cause or validate the cinematic. Manual user input is left enabled, with no
automated key sequence.

The goal's updated >=20 FPS criterion remains unmet. The earlier 6-7 FPS
observations and successful geometry compilation are not completion evidence.

## Conditional-rendering prerequisite in the visible hall (2026-09-06)

The user confirms passing the title again with the new main and reports 7 FPS.
Fresh capture /tmp/ruzu-lm3-main-7fps-1708-14917.png shows the hotel hall at
7 FPS / 150.37 ms. No build was running during the five-second host sample
/tmp/ruzu-lm3-main-7fps-1708.txt. Of 3070 GPU-thread samples, 1567 (51.0%)
are waiting for Metal completion through ProcessQueryCondition -> ReadBlock ->
FlushRegion -> DownloadBufferMemoryRange -> finish_all. Another 505 (16.4%)
wait for incoming GPU commands. No runtime Metal shader compilation appears in
this sample. These are host wall-time samples, not GPU utilization or a promise
of equivalent FPS improvement. Tessellation captures are still absent in this
particular scene/run; a new real geometry capture is present.

Interrupted slice: eliminating conditional-rendering CPU readback without
changing Maxwell visibility decisions. Prerequisites are a native counterpart
of ConditionalRenderingResolvePass, GPU predicate consumption for every affected
draw/clear path, and buffer-cache/query ownership plus channel lifecycle.
Implement and test the resolve pass first in metal_compute_pass.rs and the
corresponding host shader; do not advertise acceleration or remove fallback
waits while predicate consumers are missing. Eden's Vulkan acceleration depends
on device support; its existence does not establish that Eden uses it on this
Mac. No same-scene Eden performance comparison has yet been measured.

ConditionalRenderingResolvePass is now implemented in native MSL, with the same
8/24-byte source and four-byte result as Eden. Native GPU tests cover nine
literal expected comparisons (including each zero half, sign-bit patterns and
ignored middle words), GPU writes to a private source between resolves, a
same-encoder compute producer/consumer, destination guard bytes, dropped upload
owners, and rejected misaligned/overflowing ranges. The operation itself does
not submit or wait. Full release video_core validation with MTL_DEBUG_LAYER=1
passes: 1689 tests, zero failures, two ignored; no Rust warnings. Log:
/tmp/ruzu-metal-conditional-full-tests.log. DIFF.md records the upstream audit.

Next required slice: native predicate consumption before enabling acceleration.
Metal has indirect ordinary and mesh draw paths already, but the predicate must
also prevent VS/GS compute side effects for disabled draws, preserve indirect
counts/base fields and restart assembly, and gate shader-based attachment clears
and draw-texture operations. An attachment load-action clear cannot be used for
a GPU-conditional clear. The common DrawManager currently gates those calls by
ShouldExecute, so unconditionally returning true from the acceleration hook
before all consumers are wired would change guest behavior. Keep the old CPU
fallback active until the complete applicable path is implemented and tested.
The currently open GUI PID 1708 is unchanged; no FPS gain is claimed from this
unconnected prerequisite, and measurements during subsequent test builds are
not part of the clean baseline above.

Conditional command-argument prerequisite: metal_compute_pass.rs now also owns
ConditionalRenderingArgumentsPass, backed by metal_conditional_arguments.metal.
It copies ordinary/indexed/dispatch records into fresh GPU staging and zeroes
only instanceCount or X groups for a false predicate (with inversion), preserving
signed baseVertex bits and all other fields. Native tests verify multiple
strided records and all three layouts against Metal's ABI, GPU-produced
predicates, live-output lifetime, and actual indirect compute side effects:
four predicates produce exactly [0, 6, 6, 0] invocations. These first tests pass
under Metal API validation. The native ordinary/indexed vertex-side-effect test
now also passes: false conditions invoke no vertex shader; true conditions
preserve firstVertex/indexStart, negative baseVertex and baseInstance. The
fixture uses a void vertex entry point, as required by Metal when rasterization
is disabled (the direct MSL backend already applies this specialization).
Final full release video_core suite with MTL_DEBUG_LAYER=1: 1693 passed,
zero failed, two ignored, no Rust warnings. Log:
/tmp/ruzu-metal-conditional-consumer-full-tests.log.

Cache integration still needs the exact RenderEnable mode/override and the
channel's translated buffer range, not a new raw GPU-VA memory read. Upstream
ObtainCPUBuffer(FullSynchronize, DoNothing) supplies the authoritative GPU buffer;
query-report ownership must also be respected when the comparison references
pending visibility queries. Existing Metal report/query lifecycle cannot be
silently replaced with a guessed value. The native hook must remain false until
all consumers applicable to that condition are wired.

Lifetime constraint verified in MetalStagingBufferPool::try_get_reserved_buffer:
ordinary DeviceLocal allocations may be reused once their recorded tick has
completed, regardless of an external Arc. Per-draw generated argument records
are consumed in that tick and fit this contract. A resolved condition that
persists across frames needs dedicated query/channel-owned storage (or a true
deferred allocation), not an ordinary staging reference retained past the tick.

GUI PID 1708 subsequently exited with code 0 (exec session 68574), without an
agent stop command during this slice. Its log reached 16384 GPU submissions
without another recorded assertion. This establishes no spontaneous freeze in
that observed run, not >=20 FPS or full rendering parity. No new GUI was started
while the native command-consumer tests were building.

Conditional geometry consumers are implemented and tested (2026-09-06):
MetalGeometryPipeline::record_conditional_inputs gates the vertex compute grid
and the assembled primitive grid separately. A cloned assembly shares its
immutable buffers and owns new gated arguments; no original count is modified.
Both native object/mesh and compute capture/replay consume this assembly, so
false predicates skip guest VS/GS stores as well as pixels. The indirect vertex
path preserves rounded-grid bounds checks, IDs, negative baseVertex, instances
and restart entries; argument ranges are checked before encoding.

The full native geometry pixel matrix now executes direct, enabled, disabled and
both inverted conditions. It checks layers/viewports, cuts, invocations, ordered
blending, points/lines/triangles, storage counters and unchanged original argument
words. Separate vertex tests cover guest VS atomics and indirect range rejection.
Full release video_core with Metal API validation: 1693 passed, zero failed,
two ignored. Log: /tmp/ruzu-metal-conditional-geometry-full-tests.log.
The final verification rerun after removing a small host temporary allocation is
/tmp/ruzu-metal-conditional-geometry-final-tests.log.
DIFF.md records the native ownership/ordering adaptation and upstream reread.

Still not live acceleration: the rasterizer must consume conditional arguments
for ordinary/indexed/fan/indirect draws, DrawTexture and attachment clears. Then
query-cache/channel ownership must provide the exact GPU predicate with translated
buffer-cache addresses and pending query-report synchronization. Do not enable
the acceleration hook before these paths are correct. No GUI was relaunched for
this unconnected prerequisite; the last measured game performance remains 7 FPS.

## Conditional fixed-function consumers (2026-09-06)

MetalBlitHelper::blit_color_with_sampler and clear_attachments now accept an
optional GPU indirect quad record. Alignment/range checks and rejection of
unconditional attachment load-action clears happen before encoding. Direct
callers remain unchanged (None). A future active GPU condition must force the
rasterizer's full_clear branch onto the existing shader-clear path, not simply
predicate a draw while leaving MTLLoadAction::Clear active.

Native tests cover false/true/inverted conditions for floating and integer
color, depth, stencil, combined depth/stencil, full/partial regions, write masks,
MSAA, mip/layer views. The DrawTexture fixture samples Color2D, matching the live
texture-cache method; using its render-target array view initially triggered
Metal validation and was corrected in the fixture. It checks flipped source
coordinates, untouched destination pixels and actual visibility counts 4/0.
BlitColor's custom pass is now ended before return, matching Eden; a regression
assertion checks this for direct and conditional calls. No game was launched.

Full release video_core before the one-line EndRenderPass follow-up:
1696 passed, zero failed, two ignored; no Rust warnings. Log:
/tmp/ruzu-metal-conditional-blit-full-tests.log.
Final lifecycle assertion / full-suite verification:
/tmp/ruzu-metal-conditional-blit-final-tests.log.
Result: 1696 passed, zero failed, two ignored; no Rust warnings. The suite
completed normally with exit code 0, including the custom-pass lifecycle check.
DIFF.md contains the post-implementation Eden header/source audit.

Next live-integration prerequisites remain explicit:
- Prepare masked ordinary/indexed/fan/indirect arguments before opening the
  guest render encoder; keep signed baseVertex and source stride/index offsets.
- Route geometry through record_conditional_inputs and DrawTexture/clears
  through their tested indirect paths only when a query-owned condition is active.
- Port the query/channel lifecycle first: RenderEnable mode/override, translated
  device-address buffer acquisition, condition reset on channel changes, and
  pending query-report visibility. Existing Metal query results are reported by
  fence-owned CPU callbacks; do not treat a pending report as an already populated
  GPU cache buffer or guess its value. Eden's QueryCacheBase/runtime hooks are
  already represented in the shared Rust cache and Vulkan runtime; reuse those
  ownership edges instead of adding guest-address logic to blit/geometry helpers.
The current Metal hook still falls back to the CPU. The 7 FPS baseline and the
>=20 FPS goal are unchanged; synthetic consumer correctness is not completion.

## GPU visibility report prerequisite (2026-09-06)

The live Metal query report path now calls resolve_visibility_counter rather
than capturing/summing all visibility slots in a fence callback. The native
VisibilityResolvePass uses a hierarchical 256-thread u64 reduction; only newly
added slots are processed, carrying the last resolved value across banks. Each
report owns an immutable final eight-byte shared-storage buffer; partial sums
remain private. Reset drops the cache's accumulator, not resources referenced by
earlier commands/callbacks. No new submit/wait is issued by the resolver.

Focused native tests pass: 64-bit carry/overflow, 65537 entries (three reduction
levels), tail groups, GPU-written source data, incremental sums and bank rollover,
reset, no-new-draw result reuse, and dropping cache storage before completion.
The actual DrawTexture visibility test also verifies the resolved report matches
the hardware count (4 or 0). The final callback uses the Send resource wrapper
as a whole rather than closure field capture of its raw Objective-C handle.

Focused log: /tmp/ruzu-metal-query-resolve-tests.log (7 tests passed).
Full release/Metal validation log: /tmp/ruzu-metal-query-resolve-full-tests.log.
Result: 1698 passed, zero failed, two ignored; no Rust warnings; exit code 0.
DIFF.md records the reread of Eden's SamplesStreamer and QueriesPrefixScanPass.

Next: use the shared query-cache ownership edges to register GPU results by
translated device address, preserving report replacement/invalidation and channel
lifecycle; then supply these results and ordinary buffer-cache comparisons to
the tested conditional draw consumers. Do not claim conditional acceleration
while Maxwell still falls back to CPU condition reads. The last live measurement
remains 7 FPS; this changes query production, not yet the profiled readback path.

## Query destination lifetime (2026-09-06)

The next query-cache ownership slice exposed a local upstream divergence: Metal
captured the channel memory manager/GPU address and translated only when a fence
callback ran. CounterReport translates before queuing work. Metal now retains
the device-memory owner and translated device address instead. Unmapped reports
return before recording a GPU resolve. Timestamp/value ordering and completion-
time tick sampling match CounterReport. The shared Rust DeviceMemoryWriter uses
the same stable device-address adaptation; it is not the same as retaining Eden's
raw host pointer across SMMU replacement, which still requires query invalidation.

Eight focused release query-cache tests pass, including a remap regression over
payload/visibility and timestamped/non-timestamped reports, plus unmapped reports
recording no GPU work. Log: /tmp/ruzu-metal-query-destination-tests.log.
Full release video_core with Metal API validation completed successfully:
1700 passed, zero failed, two ignored; no Rust warnings. Log:
/tmp/ruzu-metal-query-destination-full-tests.log.

Resume at the shared QueryCacheBase/runtime/streamer bridge, not a second ad-hoc
address map. The runtime must have a stable boxed owner before raw shared-cache
bindings are installed. Pending query flags (rewritten, invalidated, final value
synced), region invalidation, replacement and channel teardown must govern the
GPU result lifetime as well as fence writeback. Do not advertise accelerated
conditions until ordinary/geometry/clear/DrawTexture consumers all use that owner.
No game was relaunched in this slice; the measured baseline remains 7 FPS.

## Shared query report registry (2026-09-06)

Metal now owns a boxed QueryCacheBase plus boxed native SimpleStreamer report
objects, all behind one retained mutex owner. GPU-result reports and deferred
payloads register through cache_query_location. There is no second address map.
The shared replacement/invalidation/unregister methods govern their lifetimes.
Rasterizer InvalidateRegion/InnerInvalidation now invalidate queries at the same
owner edge/order as Eden. Writes marked invalid are skipped, including timestamps;
rewritten reports still complete in order, without evicting a newer location.

Completion queues pending unregister, not immediate slot reuse. The rasterizer
queues retirement as a fence SyncOperation. Callbacks can outlive a cache/channel
move/drop, because they retain the boxed graph and GPU result handles. Native
tests cover replacement, partial invalidation, 1000 reports with bounded slots,
resource release and cross-thread execution. Earlier full release validation of
the registry/retirement slice passed 1704 tests with two ignored, no warnings.

Additional correctness issue found during the fence audit: at low accuracy the
common SignalFence can invoke its signal callback before submission/completion.
The previous Metal visibility callback assumed completion unconditionally. GPU
report callbacks now route through SyncOperation before an otherwise empty signal
fence; payload signals retain existing behavior. No global wait was introduced.
This is a deliberate native adaptation: retain the actual GPU value to completion,
rather than Eden's possible rejection of an unsynchronized low-accuracy report.
The new native regression exercises the early signal and real command-buffer
completion separately. The first regression failed (zero instead of 73):
/tmp/ruzu-metal-query-fence-fixed-tests.log. This exposed the constant-false
ShouldWaitAsyncFlushes callback, which lets the non-async fence manager retire a
fence without testing its completion. Metal now tracks each batch's samples mask
in QueryCacheBaseImpl::flushes_pending, including zero masks for empty batches.
ShouldWait/Pop callbacks are wired at all fence-release edges including WFI.
ReleaseFences does not enqueue a new batch because the native manager currently
uses its non-async path, which only drains existing fences. Reused query results
also ensure a current command buffer exists to avoid an unsafe stub fence.

With the flush tracking wired, the native regression and all thirteen query
tests pass: /tmp/ruzu-metal-query-flush-tests.log. Additional final coverage
checks WFI cannot release an incomplete query batch and empty/nonempty masks
remain FIFO. Full release video_core with Metal API validation completed:
1706 passed, zero failed, two ignored; no Rust warnings. Log:
/tmp/ruzu-metal-query-flush-final-full-tests.log.

Next prerequisite: provide QueryCacheRuntime::SyncValues using the shared buffer
cache's authoritative device-address buffers, then channel-bound conditional
lookups and predicate consumers. The registry currently owns metadata/results,
not a complete QueryCacheRuntime. Do not bind new raw runtime/channel pointers to
the Send report graph without reviewing its mutex/lifetime contract. Pending GPU
results must not be mistaken for bytes already written into the common buffer
cache. The tested indirect consumers remain unconnected; baseline still 7 FPS.

## SyncValues prerequisite (2026-09-06)

Native QueryCacheRuntime::sync_values now implements the guest-staging and
host-GPU-source variants of Eden's template. It uses the real common buffer cache,
FullSynchronize/DoNothing and BufferOperations retry ordering, then records Metal
blit copies without submitting/waiting. CPU values are snapshotted immediately;
GPU values are never downloaded. Page redirects/copy groups belong to the runtime
as upstream. A page-straddling eight-byte report acquires both pages deliberately.

Three new native tests pass (the filter also runs one Vulkan test):
/tmp/ruzu-metal-query-sync-tests.log. They verify literal values, source offsets,
page grouping, preserved neighboring bytes, cross-page writes, retained GPU
resources and unchanged submission ticks. Invalid inputs record no GPU work.
Full release verification passed: 1709 tests, zero failures, two ignored, no Rust
warnings, in /tmp/ruzu-metal-query-sync-full-tests.log. The constructor now exposes
its stable-service lifetime/serialization requirement as an unsafe contract rather
than a safe function storing potentially short-lived references. Final rerun:
/tmp/ruzu-metal-query-sync-final-full-tests.log completed with the same result:
1709 passed, zero failed, two ignored; no Rust warnings.
DIFF.md records the source/header reread and native differences.

The runtime is not yet instantiated in the live rasterizer. Resume by collecting
pending reports in guest order, skipping rewritten/invalidated reports as Eden
SyncWrites does, and marking IsHostSynced only after successful GPU copies. Wire
NotifyWFI/CommitAsyncFlushes before host conditional lookup. Do not expose pending
results as already synchronized buffer-cache bytes. Preserve the existing report
mutex/boxed lifetime while keeping scheduler work off fence callback threads.

Also fixed a small live divergence: AccelerateInlineToMemory now invalidates the
query cache after the shader cache, matching Eden. Existing reports must not
overwrite a later inline write. No GUI was launched; no FPS gain is measured yet.

## Live pending-report GPU synchronization (2026-09-06)

QueryCacheRuntime is now instantiated by the live Metal rasterizer with stable
boxed scheduler/staging services. Pending streamer reports synchronize before
query flush-mask commits and WFI ordering. Payloads precede samples; replacements
and invalidations are filtered, and successful recording sets IsHostSynced.
Visibility counters always copy eight GPU bytes, independently of CPU report
width. Neither timestamps nor FinalValueSynced are updated by these copies.

The focused native tests and full release suite with Metal validation pass:
1711 passed, zero failed, two ignored. Logs:
/tmp/ruzu-metal-query-live-sync-tests.log and
/tmp/ruzu-metal-query-live-sync-full-tests.log. DIFF.md records the upstream
header/source comparison and native adaptations. No GUI run or measured FPS
improvement in this slice; the last observed baseline remains 7 FPS.

Resume with channel-bound conditional lookup and the GPU predicate owner, then
wire all ordinary/indexed/fan/indirect, geometry, clear and DrawTexture consumers.
Do not enable accelerate_conditional_rendering until all those consumers honor
the predicate. Failed report synchronization leaves IsHostSynced unset and must
not be accepted as a ready predicate source. Existing native indirect-argument
and geometry/clear tests are prerequisites, not evidence of live acceleration.

## Conditional runtime owner (2026-09-06)

QueryCacheRuntime now owns ConditionalRenderingResolvePass and a dedicated
four-byte private/tracked predicate, with End/Pause/Resume and a retained native
setup exposed to consumers. BC comparisons obtain the real common-cache source
and resolve on GPU; the zero-versus-query fast path references its low word.
The live constructor now accepts MetalDevice and propagates allocation/compiler
errors. No condition is advertised to Maxwell yet.

Initial native tests passed for ordered resolved predicates and lifecycle:
/tmp/ruzu-metal-conditional-runtime-tests.log. Additional coverage exercises
direct low-word tests and inversion changes at one address. Full release suite
with Metal API validation passed: 1714 tests, zero failures, two ignored, no Rust
warnings; /tmp/ruzu-metal-conditional-runtime-full-tests.log.
DIFF.md records the constructor/state/header/source reread and the intentional
correction of Eden's inversion-insensitive setup early-out.

Resume at the shared-query lookup/channel bridge: translate current channel GPU
addresses, consult the existing cached_queries index (including the +4 lookup),
preserve flags/lifetimes under the report lock and never accept failed pending
sync as a ready GPU source. Do not install runtime/channel raw pointers into
the Send report graph without a scoped lifetime contract. Then wire all native
draw consumers before enabling the rasterizer acceleration hook. Tests currently
prove ordered predicate-to-argument copies, not whole-game performance. No GUI
launch in this slice; the 7 FPS baseline and >=20 FPS goal are unchanged.

## Shared lookup / channel-scoped acceleration bridge (2026-09-06)

The shared gen_lookup device-address work is now a QueryCacheBase helper used by
both existing shared acceleration and Metal. Metal's acceleration implementation
borrows the current channel MemoryManager/RenderConditionState explicitly; no
new raw pointer enters the Send report graph. It rejects unsynced reports and
uses the real common-cache data for GPU comparisons. Native equality resolves
full values instead of Vulkan's unconditional low-accuracy/driver shortcuts.

Exact endpoint translation matters: gpu_to_cpu_address_range searches for the
first mapped page and drops the record offset. Tests use GPU 0x20100 -> device
0x8100 to catch that mistake, and reject unmapped/cross-gap records. Coverage
also includes +4 lookup, same-page boundaries, replacements, query invalidation
plus real CPU write/upload, and GPU-modified data without query metadata.

Initial focused log /tmp/ruzu-metal-conditional-lookup-tests.log contains one
incorrect-test failure: invalidation removes the query index entry, it does not
retain invalid metadata for lookup. Corrected test verifies the new CPU value
on GPU instead. Final full release suite with Metal validation passed:
1718 tests, zero failures, two ignored, no Rust warnings;
/tmp/ruzu-metal-conditional-lookup-full-tests.log. DIFF.md updated.

Integration STOP before advertising acceleration: MetalRasterizer::flush_region
does not call the query cache, unlike vk_rasterizer.cpp::FlushRegion. The native
report graph also intentionally has no bound device-memory writer or rasterizer
pointer for the shared SemiFlushQueryDirty/RequestGuestHostSync path. Simply
calling base.flush_region would warn/no-op (or require unsafe lifetime bindings).
Implement the scoped native counterpart of this prerequisite first: final query
values write back through the real device owner; pending GPU queries request
ReleaseFences only after releasing the report mutex, since callbacks lock it.
Reuse the shared range/index and dirty-query rules rather than invent a map.
Then wire channel changes and every predicate consumer before enabling the live
hook. No GUI run this slice; no new FPS claim; last observed hall baseline 7 FPS.

## Query FlushRegion prerequisite (2026-09-06)

Implemented the previously missing live Metal query flush route. The shared
QueryCacheBase exposes a scoped writer variant, reusing IterateCache and the
same SemiFlushQueryDirty value/flag logic. Metal returns the pending-fence
decision after releasing its report lock. The rasterizer calls ReleaseFences
with the upstream default force=true only on that decision, after texture and
buffer flushes. No raw lifetime binding was added to the retained report graph.

Focused tests passed in /tmp/ruzu-metal-scoped-query-flush-tests.log. The live
rasterizer test now also routes a real pending GPU visibility zero through
FlushRegion/ReleaseFences, not just a ready payload. Full release validation with
Metal API validation passed: 1721 tests, zero failures, two ignored, no Rust
warnings; /tmp/ruzu-metal-scoped-query-flush-full-tests.log. The shared and native
tests check early exit, width/timestamp rules, invalidation and report retirement.
DIFF.md records the header/source reread and the scoped-lock adaptation.

Resume channel lifecycle and ordinary/indexed/indirect draw
predicate consumers, then shader clears/DrawTexture and geometry. Do not expose
the acceleration hook until every command affected by a Maxwell condition is
gated. The CPU fallback query synchronization prerequisite is now implemented;
the native runtime/channel bridge and indirect helpers are not yet used by live
draws. No GUI run or measured FPS gain in this slice; baseline remains 7 FPS.

## Live conditional graphics consumers (2026-09-06)

The native acceleration hook now consumes a Maxwell render-condition snapshot
without reborrowing the engine. GPU predicate resolution is connected to ordinary,
indexed and indirect draws, fan arguments, geometry VS/GS dispatch arguments,
DrawTexture and shader-clear quads. Conditional clears bypass load-action clears.
The native channel release edge removes active predicate state. Other backends
retain their legacy hook through the trait's default adapter.

Focused live rasterizer test passed with Metal API validation. Equal/unequal
GPU-owned words produce quad instance counts 1/0, no preparation-time submission
or CPU readback, and unconditional rendering removes the prior predicate.
Log: /tmp/ruzu-metal-live-conditional-test.log. Cargo check is warning-free.
Full release video_core with Metal API validation passed: 1722 tests, zero
failures, two ignored; no Rust warnings in the test log. Log:
/tmp/ruzu-metal-conditional-consumers-full-tests.log. No new GUI/FPS observation yet.
Next: complete tests, rebuild ruzu.app and measure the same hall as the 7 FPS
baseline. >=20 FPS and cross-title/scene validation remain unfulfilled.

Release GUI build completed without Rust warnings:
/tmp/ruzu-metal-conditional-app-build.log. Bundle completed using the existing
app's MoltenVK library explicitly (no library version change):
/tmp/ruzu-metal-conditional-app-bundle.log. No emulator instance was running.
Launch deferred after IOConsoleUsers reported CGSSessionScreenIsLocked=Yes;
the previous locked-session run had a guest audio-init abort, so do not reuse
that condition for the performance comparison. Wait for user unlock, launch
the rebuilt ruzu.app with /tmp/ruzu-geometry-metal-config.EJtyrm, then compare
the same hall. No new FPS claim; last baseline remains 7 FPS.

## Sixth captured geometry pipeline replay (2026-09-06)

With the console still locked and no emulator process running, replayed the
remaining hall capture be61680ef624e7e0.bin using the existing ignored manual
test, release build and MTL_DEBUG_LAYER=1. It passed: direct MSL shader stages,
native vertex producer and object shader compiled in 17.27325 ms on this host.
Input is Lines (two vertices), output TriangleStrip, max 14 vertices/12
primitives, last provoking vertex, no layer/viewport stores. Log:
/tmp/ruzu-geometry-sixth-capture-replay.log.

This is compilation evidence only, not GPU execution time or frame correctness.
The replay uses captured environment/fixed state but does not contain the actual
resolved attachment views needed by make_render_pipeline_key; do not invent a
framebuffer to claim full live PSO validation. The remaining decisive gate is
the rebuilt app in the same unlocked hall, followed by the read-only Eden and
cross-title comparisons. The GUI was not launched while the console was locked.

## Unlocked conditional-rendering validation run (2026-09-06, 20:17)

User resumed; IOConsoleUsers confirmed the console is unlocked. No emulator
instance was present before launch. Started the rebuilt release ruzu.app with
the previous isolated Metal configuration and cubeb, without geometry dumps.
PID 70747, exec session 51776, log:
/tmp/ruzu-metal-conditional-live-run-20260906.log. Native Apple M2 Pro backend
confirmed; 920 cached pipelines loaded. The title screen is visible and animated,
14-15 FPS in captures; no new panic or conditional-rendering error in the log.
User is left to press A; do not automatically restart or send further inputs.

Three-second title profile (not the hall baseline):
/tmp/ruzu-metal-conditional-title-70747.sample.txt. GPU thread has 1614 samples,
1132 in presentation/nextDrawable (about 70%), primarily the drawable semaphore
wait. Only one process_query_condition sample; no waitUntilCompleted stack in
this sample. This does NOT establish the hall fix or GPU utilization: drawable
availability can reflect presentation pacing or GPU backlog. Measure the same
hall after manual navigation before changing presentation behavior.
Window-specific pre/post profile captures:
/tmp/ruzu-conditional-live-70747-ready-14967.png and
/tmp/ruzu-conditional-live-70747-postsample-14967.png. Both are the title screen.
The process remains open for user input; >=20 FPS remains unverified.

## Runtime memory/swap exhaustion incident (2026-09-06, 20:22)

The above PID 70747 is no longer running. User reported memory and disk
saturation. Correcting the initial post-exit interpretation: a 3 GiB remaining
swap volume does not describe the peak during execution. Unified kernel logs
in /tmp/ruzu-swap-history.log establish:
- 20:21:58.549: vnode_setsize for swap files failed: 28, followed by
  low swap: failed to create swapfile. Local macOS SDK errno.h defines 28 as
  ENOSPC (No space left on device).
- 20:22:00.545: suspending ruzu [70747] due to swap exhaustion.

This proves paging-space exhaustion during this run and explains the system's
suspension of Ruzu. It does not quantify the peak swap size, the pre-launch free
disk space, or isolate which Metal allocation is leaking. The 20:18:39 sample
already reports 6.3 GiB physical footprint at the title, but is a single point.
No large recent persistent file was found in target, the Ruzu shader cache,
/tmp or user logs; the run log is only 109 KiB. /cores is empty; Data has no
APFS snapshots. Post-exit free space rose from 23 to 36 GiB during inspection;
only 5.1 GiB incremental debug cache was explicitly removed by this agent.
Do not attribute the remainder without a measured allocation/deletion history.

STOP unmonitored long runs. Next prerequisite: audit Metal resource retirement
and Objective-C autorelease lifetime, then monitor process footprint, Metal
allocated bytes, swap and disk free space together with an automatic safety
cutoff for the next reproduction. No FPS claim can be made from this run.

## Bounded memory baseline and command autorelease scope (2026-09-06)

Unchanged release app PID 23208 ran for 347 seconds under an external watchdog,
then exited normally (status 0); no threshold fired. Measurements every two
seconds are in /tmp/ruzu-memory-watch-20260906-2042.csv, app output in the
matching .log. Peak sampled physical footprint: 8.499 GiB. Minimum free disk:
36.663 GiB, only 27.9 MiB below launch. Allocated swap stayed at 3 GiB; used
swap stayed near 1.46 GiB. This does not reproduce the previous swap exhaustion.
User likewise reported no reproduction. ScreenCaptureKit refused capture with
TCC error -3801, so the scene and FPS are not independently verified.

Memory still grew slowly: vmmap summaries at about 225 and 306 seconds show
default-heap allocations rising from 864.5 to 995.4 MiB (roughly 3.0 to 3.7
million allocations). Graphics-owned unmapped memory remained around 2.1 GiB;
IOAccelerator graphics regions were around 1.9-2.0 GiB. The retained summary is
/tmp/ruzu-memory-watch-vmmap-23208-5min.txt. These categories do not identify
allocation owners, and vmmap's swapped column is not the system swapfile size.

Static audit found no autorelease pool in Ruzu's Rust GPU-thread command loop.
Apple's Memory Management Guide requires secondary threads calling Cocoa to
manage their own pools. objc2 0.6.4 also documents that retained return values
do not eliminate framework-internal autoreleased temporaries. Added a macOS-only
pool per dispatched GPU command in gpu_thread.rs, preserving dispatch and fence
ordering and without any GPU wait. Explicitly retained renderer resources survive
the drain. This is platform glue for native Metal, not an Eden algorithm change.
Tests cover temporary destruction, retained-resource survival and unwind cleanup.
Release validation: all four gpu_thread tests pass. The full video_core suite
with MTL_DEBUG_LAYER=1 passes 1,724 tests, with two ignored and no warnings;
/tmp/ruzu-command-autorelease-full-tests-20260906.log. The above baseline
predates this change; rebuilding the GUI and matched runtime validation remain.
Pipeline worker
and other native Metal calling threads still need their own scope audit. Do not
claim the runaway incident fixed until a matched, monitored run validates it.

## Post-pool run: memory plateau, 8 FPS and flicker (2026-09-06, 20:55)

Rebuilt release GUI without warnings and bundled it using the existing MoltenVK
library. Library SHA256 stayed
0995b17b030c01e991e2c36b48a953d8a4fdb6c4df1b9dcaa46b6d9e08612855.

### 2026-09-07 - Selected-aspect shader metadata prerequisite

The captured X32_Stencil8/float mismatch now has a tested correction. Shared
graphics/compute shader environments resolve TIC swizzles to the sampled
depth or unsigned stencil format, without changing image storage or RT
formats. S8 classification and component width are consistent. Integer nearest
sampling takes priority over anisotropy fallback in both native bind paths.

Legacy combined-format disk environments cannot resolve the aspect without
their original TIC. All three backend preloaders consume their keys and defer
these entries to live translation; original caches are preserved. A regression
test verifies a mixed old/new cache stays byte-identical after loading.

Direct MSL GPU readback tests pass under Metal API validation for stencil
0/1/128/255 and ordinary depth sampling at 0.25. Full video_core release suite
passes 1760 tests, 3 ignored. Final sampler-order tests/build and a new monitored
GUI validation run are pending. The previous failure is not declared fixed
in-game until that run passes. UInt8 cache FPS benefit remains unmeasured.

Validation run G: fresh GUI PID 75388, `/tmp/lm3-0907g-app.log` and
`/tmp/lm3-0907g-resources.csv`. Previous stencil-type assertion did not recur;
at 28 seconds Metal validation instead aborted a compute dispatch: CBUF c0
bound allocation length 4, required native uint4 length 16. No A was sent,
no emulator remains, sampled footprint peaked about 4.3 GiB and swap was stable.
This is not a comparable gameplay/performance run. The full final sampler-order
suite and GUI build passed without warnings before launching.

New prerequisite: the backend common-cache null Buffer and runtime fallback
both allocate only 4 bytes, copied from Vulkan's null index resource. Native
constant uint4 pointers and indirect CBUF reads require a complete readable
constant range. Both now allocate MAX_CONST_BUFFER_SIZE with explicit zero
initialization; guest allocation sizes and metadata are unchanged. Added GPU
test uses the real runtime compute binding and reads the first/last uint4 for
both null allocation paths. Full tests/rebuild and subsequent validation run
are pending. This is a local native allocation-contract fix, not a scheduler
wait or a replacement of valid guest buffers.

Following the null fallback exposed an independent routing defect:
`BufferCacheRuntime::with_mapped_uniform_buffer` always pushed graphics UBOs,
even after BeginComputeBindings. Common BindHostComputeUniformBuffers uses
this path for unaligned offsets; the resulting compute list is short and its
descriptor lookup falls back to null. It now follows the existing BindingTarget,
as texel bindings already do. Regression mixes mapped/direct/mapped compute
bindings, checks bytes/order and verifies graphics binding isolation. Eden's
Vulkan runtime pushes the same stream data to the active descriptor queue;
native separate lists must preserve that destination. This does not prove the
captured c0 used this path, but the misrouting is independently reproducible.

### 2026-09-07 - Live validation passed; no material FPS improvement

Final snapshot: full release video_core suite 1762 passed / 3 ignored with
Metal API validation, no warnings; GUI release and bundle succeeded. Logs:
`/tmp/ruzu-compute-routing-final-{tests,gui,bundle}-20260907.log`.

Run H (validation enabled): `/tmp/lm3-0907h-app.log`, PID 12354, fresh GUI,
seven A inputs, confirmed Lobby gameplay via `lobby-validation.png` in its
session directory. Neither stencil-type nor short-CBUF assertion recurred
through gameplay. Quit normally at 251 seconds, watcher exit 0. Peak footprint
8.376 GiB, swap unchanged, disk free delta -12.51 MiB; no saturation.

Run I (validation disabled, same release GUI and profiling flags as baseline B):
`/tmp/lm3-0907i-app.log`, PID 25779. Automated seven A inputs after initial wait,
at least seven seconds apart. `after07.png` caught the Lobby fade-in;
`lobby-measure.png` confirmed the same static Lobby camera as baseline B.
No further input or captures during the 15 two-second status samples:
`/tmp/lm3-0907i-fps.jsonl`, 04:58:10..04:58:38 UTC.

- FPS median 7.9165, range 7.8717..8.9608, mean 8.1195. Baseline B median
  7.947: no meaningful improvement. Do not claim the UInt8 cache fixed FPS.
- Guest command-buffer mean duration 28.675 ms (previous 29.807); median
  per-report peak 130.256 ms (previous 134.105). Long GPU batches persist.
- UInt8 cache per-report median requests 4185, hits 1774, uncacheable 1674.
  Median owned converted bytes 65790 / 298 entries. Median native allocations
  2,295,005,184 bytes, common-buffer accounting 176,357,630 bytes.
- Sampled-batch index conversion calls median 221 (previous 463). Batches
  differ in work size; this is supporting evidence, not a per-frame ratio.
- Stage samples rotate across partial batches. Of summed raw sampled ticks,
  fragment ~60.6%, vertex ~32.6%, compute ~5.5%, blit ~1.3%. Stages overlap;
  these are NOT wall-clock utilization percentages or an identified culprit
  shader. Correlate expensive native render passes with actual guest pipelines,
  attachment sizes and resource contents before the next optimization.
- Quit normally at 227 seconds, watcher exit 0. Peak footprint 7.984 GiB;
  swap delta -8 MiB, disk free delta -0.38 MiB. No process remains. This
  bounded run does not rule out longer-term leaks.

The independently reproduced numeric-type, null-range and compute-stream-target
defects are fixed and tested. Full scene parity, other-title regressions and
the >=20 FPS target remain open. Next slice: enrich existing bounded stage
profiling with pass/pipeline identity, then isolate the longest native render
passes; do not add global waits or skip work to make the timing look better.
New app executable SHA256:
2270a2d16744a691abf3af9bbcfb4f991f2cea62318e82075acb4aad26f4af98.
One app instance, PID 72947, started through the same external watchdog; no
automated input. Log and CSV:
/tmp/ruzu-command-autorelease-watch-20260906-2055.{log,csv}.
Native Metal, 922 cached pipelines built (baseline had 920; not cache-identical).

User reports the game still runs at 8 FPS and flickering effects before reaching
gameplay. This is NOT a rendering/performance success. The autorelease change
addresses object lifetime, not proven visual correctness. Do not classify the
flicker as newly introduced or assign it to geometry/conditional rendering
without before/after captures. macOS still denied the attempted window capture.

Three-second live sample after that report:
/tmp/ruzu-metal-8fps-72947-20260906.sample.txt. GPU thread: 1,418 samples;
718 in nextDrawable's semaphore wait (50.6%), 651 under guest command processing.
Only one process_query_condition sample and no waitUntilCompleted frame. This
does not identify the GPU pass cost, but the previous readback-dominated host
stack is absent in this sample. Three IOAccelerator readings report global
GPU utilization 95%, 97%, 97% (tiler 95-97%, renderer 94-97%). These counters
are machine-wide, not per-process; high load suggests GPU-side work/backlog,
not proof that nextDrawable itself is incorrectly paced. Do not remove waits
or reduce guest draws based on this profile.

At 120-206 seconds the sampled footprint settles around 7.66-7.68 GiB;
allocated swap remains 3 GiB. vmmap snapshots at about 100 and 207 seconds:
/tmp/ruzu-command-autorelease-vmmap-72947-{early,late}.txt. Default heap grows
567.7 to 587.7 MiB, allocations 1,277,098 to 1,287,519, much less growth than
the previous run. This is consistent with effective temporary-object draining,
not a scene-matched proof that all memory leaks are fixed. Watchdog remains
responsible for safety cutoff/10-minute limit; check its live CSV before any
new launch.

Next decisive measurements: per-command-buffer GPU duration and pass labels,
then targeted geometry/regular-render/conditional-clear costs and capture of
the reported flicker. Preserve output semantics and tick-based resource lifetime;
the >=20 FPS gate and read-only Eden comparison remain unmet.

The above post-pool run ended at the watchdog's 601-second time limit, not a
memory threshold. SIGTERM did not finish within three seconds, so the watchdog
then killed the owned child. Peak sampled footprint 7.978 GiB; minimum free disk
36.604 GiB (58.2 MiB below launch); allocated swap unchanged. No emulator instance
remained before starting the next compilation.

## Nonblocking Metal command-buffer timing (2026-09-06)

Added opt-in RUZU_PROFILE_METAL_SUBMISSIONS to MetalScheduler. The profiler reads
GPUStartTime/GPUEndTime only after existing successful completion checks. It adds
no command buffers, waits, flushes, shader changes or completion handlers. Four
fixed-size buckets distinguish guest batches, presentation, other caller-owned
buffers and synchronous finish operations. At most one [METAL_GPU_TIME] report
per second; the environment is read only at scheduler construction. Missing
timestamps remain counted as completed but not measured. GPU spans can overlap;
the aggregate is not a utilization percentage or a per-pass measurement.

Full release video_core suite with MTL_DEBUG_LAYER=1: 1,726 passed, zero failed,
two ignored, no warnings. Added tests for invalid timestamp filtering, exact
elapsed-time arithmetic, peak identity and exactly-once observation preserving
native submission ticks. Log: /tmp/ruzu-metal-submission-profile-tests-20260906.log.
GUI rebuild and live GPU timing capture are next; no performance fix is claimed.

Release GUI build and bundle completed without warnings:
/tmp/ruzu-metal-submission-profile-{build,bundle}-20260906.log.
Before launch, IOConsoleUsers reported CGSSessionScreenIsLocked=Yes; no emulator
or watchdog process is running. Do not launch/measure presentation while locked.
Resume the same bounded app launch with RUZU_PROFILE_METAL_SUBMISSIONS=1 after
verifying the console is unlocked. This live measurement is pending, not failed.

### Interrupted flicker-capture slice (historical; implementation below)

ScreenCaptureKit is unavailable under the current TCC authorization. The built-in
alternative is also incomplete: RendererMetal::request_screenshot currently
ignores the destination pointer/layout and immediately invokes callback(false).
In the GUI, that bool means invert_y, NOT success: boot.rs then saves the
untouched, zero-filled pixel vector as if a screenshot had succeeded. Do not use
such files as evidence of black rendering.

Before continuing the framebuffer comparison, implement native screenshot capture
in renderer_metal.rs / metal_presenter.rs (their existing presentation ownership),
matching RendererBase's pending-request lifetime and callback contract. Render
the actual presentation into a layout-sized readable target, perform a correctly
ordered, row-pitch-aware GPU download, and complete the callback only after the
pixel data is ready. Preserve retained resources until completion; do not add a
readback or wait to normal frames. Read-only Eden screenshot paths are the
behavioral reference. Add pixel/layout/orientation/pending-request tests before
using this path to compare the reported flicker. No screenshot implementation
has been started yet; the diagnostic submission-timing slice is separately built
and tested. This is not evidence that the >=20 FPS/render-correctness goal is met.

## Native screenshot prerequisite implemented (2026-09-06)

RendererMetal now delegates requests to RendererBaseData instead of immediately
invoking the callback on unfilled pixels. render_screenshot runs after resolving
the present source and before drawable presentation. render_to_buffer renders
BGRA8Unorm into a private target with the requested layout, copies to a shared
buffer with 256-byte row alignment, waits for that capture command after earlier
guest commands, then unpacks rows into the frontend allocation. The callback
receives invert_y=false only after the pixels are ready. Invalid captures are
logged/cancelled without saving an artificial black image. No-request frames do
not allocate, submit or wait for readback. MetalPresenter's existing source pass
is mechanically reused; normal presentation still fills its drawable.

Read-only comparison covered Eden RendererBase::RequestScreenshot and Vulkan
Composite/RenderScreenshot/RenderToBuffer (headers and implementations), plus
the configured background color/alpha in present/window_adapt_pass.cpp. This
captures the current Metal composition, not a claim that all Eden layer/crop/
transform operations or the separate applet-capture feature are implemented.

Two headless real-Metal regression tests pass: queued upload ordering, six colors
converted RGBA->BGRA, top-down orientation, layout margins/background, padded row
unpacking with destination guard bytes, deferred/exactly-once completion and
duplicate-request rejection; invalid layout retires the request without callback
or altered output. Full release video_core with MTL_DEBUG_LAYER=1: 1,728 passed,
zero failed, two ignored, no warnings. Logs:
/tmp/ruzu-metal-native-screenshot-{tests,full-tests}-20260906.log.

The console is now unlocked. Rebuild the GUI then recheck console/process state
before a bounded app run with RUZU_PROFILE_METAL_SUBMISSIONS=1. No gameplay
capture has yet been obtained from this new path, and the reported 8 FPS and
pre-game flicker remain unresolved.

## Live GPU timing and next native profiling slice (2026-09-06)

The release GUI/native screenshot bundle built successfully. MoltenVK's hash
remains unchanged (0995b17b030c01e991e2c36b48a953d8a4fdb6c4df1b9dcaa46b6d9e08612855).
One watched GUI process, PID 50710, used the existing isolated Metal config,
cubeb, and RUZU_PROFILE_METAL_SUBMISSIONS=1. User confirmed still 8 FPS.
Logs: /tmp/ruzu-metal-gpu-timing-20260906-2140.{log,csv}.

At 19:39:52-54 UTC, intervals contain 9 presentations / 1.10-1.13 seconds.
Guest command-buffer GPU spans peak at 132-135 ms; presentation totals are
2.46-3.14 ms across nine command buffers. Later eight presentations total
1.10 ms while guest batches still peak at 133 ms. This identifies expensive
guest GPU work, not the final fullscreen copy, but does not distinguish geometry,
compute, fragment work, or resource stalls inside the guest command buffer.
Do not sum overlapping execution spans as utilization or call a batch one draw.
CPU sample /tmp/ruzu-metal-gpu-timing-50710.sample.txt: GPU thread 1369 samples,
718 in nextDrawable; no waitUntilCompleted found. That CPU wait is not itself
proof of an erroneous pacing policy.

184 resource samples over 367 seconds: peak footprint 8.761 GiB, minimum free
disk 36.425 GiB, maximum disk drop 38.34 MiB, allocated swap steady at 3 GiB.
No exhaustion reproduced. Agent sent SIGTERM for rebuild, then SIGKILL when
the process did not exit; watchdog confirms signal 9, not a spontaneous crash.

Next slice implemented, testing in progress: optional RUZU_PROFILE_METAL_STAGES
uses native stage-boundary counter attachments. One 1024-sample shared buffer
is leased to at most one guest batch per second and returned only at successful
GPU completion. Native descriptors record compute/blit encoder boundaries and
vertex/fragment render-stage boundaries without adding passes, flushes, barriers
or waits. Raw GPU ticks avoid assuming CPU/GPU clock calibration. Overflow is
reported as omitted coverage, not silently treated as a complete frame profile.
The Metal API device capability is queried rather than assumed from the GPU name.
No graphics fix or >=20 FPS achievement is claimed.

The stage-counter slice passes the full release video_core suite with
MTL_DEBUG_LAYER=1: 1,732 passed, zero failed, two ignored, no warnings.
/tmp/ruzu-metal-stage-profiler-final-tests-20260906.log. New coverage verifies
counter error/truncation handling, bounded single-buffer leasing, actual GPU
timestamp resolution with an exact 4 KiB blit output, descriptor non-mutation,
pass creation and unchanged submission ticks. The Apple SDK confirms timestamp
results contain one uint64_t and the error sentinel is UINT64_MAX. Re-read
Eden's vk_scheduler.h/.cpp for submit/wait/pass ownership and ordering; this
diagnostic mechanism intentionally uses Metal, not Vulkan structures.
GUI rebuild is pending in /tmp/ruzu-metal-stage-profiler-build-20260906.log.
No emulator is running. Next: bundle preserving MoltenVK, recheck unlocked
console, run watched ruzu.app with both RUZU_PROFILE_METAL_STAGES=1 and
RUZU_PROFILE_METAL_SUBMISSIONS=1, collect the user's same 8 FPS scene.

GUI release rebuild and bundle completed:
/tmp/ruzu-metal-stage-profiler-{build,bundle}-20260906.log. MoltenVK hash is
unchanged. Console was unlocked and no emulator process existed before launch.
Watched GUI PID 2050 is now running (watchdog tool session 62774), both profilers
enabled, same config/title/audio as the earlier run. Files:
/tmp/ruzu-metal-stage-timing-20260906-2200.{log,csv} (filename suffix is only a
run label; use recorded timestamps/elapsed seconds). Startup confirms native
stage-boundary counter support and the single-buffer allocation. User input
remains manual. The watchdog retains its ten-minute and memory/disk limits.
Live per-stage interpretation remains pending; do not restart while this process
is alive or claim the 8 FPS issue is fixed by profiling alone.

First native stage records are nonzero: tick 1784 samples 17 blit passes and
50 vertex/fragment pairs, with GPU-tick sums 339835 / 0 / 1194129 / 31036664.
No coverage omissions in that batch. These are startup data, NOT evidence from
the user's confirmed 8 FPS scene. Later the one-second acquisition cadence
repeatedly selects a blit-only guest batch (ticks 4209/4294/4376), while separate
guest batches peak at 68 ms. Do not infer no compute/geometry work from an
unrepresentative sampled batch. If this persists at the target scene, acquire
the single sample-buffer lease on first compute/render encoder rather than
arbitrarily at command-buffer allocation, and document that sampling policy.
Keep the existing single-buffer bounded lifecycle and do not add GPU waits.

User again confirmed 8 FPS. The sampling bias persisted through ticks
7576/7624/7674 (only one blit sampled each time). Acquisition now happens at
creation of a compute/render encoder, not at command-buffer allocation; the
existing lease then samples subsequent passes in that batch. Blit-only batches
and transfers preceding acquisition are deliberately outside this stage sample.
Submission-wide timings still cover every completed batch. Regression coverage
now includes a separately submitted blit-only batch that must not consume the
lease or alter subsequent ticks. Full validation is running in
/tmp/ruzu-metal-stage-selection-tests-20260906.log, tool session 94970.
PID 2050 was stopped by the agent (SIGTERM then SIGKILL); no game is running.
Next rebuild/bundle/run must use this revised selection, not the previous app.

Revised-selection full suite: 1,732 passed, zero failures, two ignored, no
warnings; /tmp/ruzu-metal-stage-selection-tests-20260906.log. GUI build/bundle
succeeded in /tmp/ruzu-metal-stage-selection-{build,bundle}-20260906.log and
MoltenVK hash is unchanged. The latest watched GUI is PID 21027, tool session
35566, /tmp/ruzu-metal-stage-selection-live-20260906.{log,csv}. Console unlocked,
no prior instance before launch. Both profilers enabled, user manual input,
same isolated config and cubeb. It starts with ~35.25 GiB disk free and 4 GiB
allocated swap; watchdog remains enabled. Do not confuse it with the stopped
PID 2050 or reuse the previous sampling-biased run as the revised result.

## Checkpoint before input-session prerequisite (2026-09-06)

Revised live sampling reaches compute/render batches. At tick 12134 it reports
53 blit / 121 compute / 169 vertex / 169 fragment samples and 3345 omitted
stages. The 1024-index cap covers only the beginning of this large batch.
Guest submission spans still peak near 134 ms at ~8 presentations/s. Alternating
compute/render encoders are now a concrete investigation target. The rasterizer's
per-draw ConditionalRenderingArgumentsPass::resolve ends the current render pass;
this is one possible source of excessive tile store/load boundaries, not yet a
proven exclusive cause. Query prepare_draw itself does not end a render pass.
Do not infer whole-batch stage proportions from capped prefix samples. Further
profiling needs bounded broader/rotating coverage and operation attribution.

User requested checkpoint commit/push and rebase onto
origin/feat/capture-harness-input-sessions so their inputs can be recorded and
replayed to reach gameplay autonomously. Fetched tip is
832ac04c92785ff03e941bff015412910c87fd8a, directly above our previous base
1f700f999274b5c3cea7b7ec81e77e90e50c9c77. Its physical input backend is currently
Linux evdev/uinput only, explicitly rejecting non-Linux sessions; rebase alone
cannot record on this macOS host. Implement/verify a native macOS recording and
replay path before promising autonomous navigation. Preserve config/saves/caches,
record only explicitly selected game input, and do not capture unrelated typing.
The >=20 FPS and visual parity gates remain unmet. This is a WIP checkpoint,
not a claim that the geometry/performance objective is complete.

## 2026-09-06 - Input control and profiling follow-up

Checkpoint cba4f79c is pushed on fix/vulkan-geometry-stream-exception after
rebase onto capture-harness 832ac04c. Only the DIFF.md append conflicted; both
entries were preserved. No input automation was yet present in the app built
at 23:25. The user's 23:41 screenshot shows Vulkan/MoltenVK, not native Metal;
their later Vulkan test is visually working at about 14 FPS. Large polygons
in the earlier capture are intermittent/unexplained, not a proven Metal regression.

macOS event-posting preflight is denied. A local opt-in GUI control socket now
uses the existing virtual gamepad rather than attempting unauthorized OS input.
No physical keyboard is recorded or mapping changed. Runtime HID and screenshot
validation remain pending. User's requested sequence: wait 40 s after launch,
then A every 7 s, stop on arrival in gameplay. Do not use the old 9-A sequence
for this title. Each input must have a release and a same-process fresh capture.

Stage profiling now rotates 1024-sample windows and attributes compute calls /
explicit render exits to conditional masking, conditional resolve, visibility,
geometry vertex, assembly, index conversion, guest, or other. The prior prefix
sample omitted 3345 stages; its ratios cannot represent the complete frame.
This is diagnostic-only and does not change encoder reuse/submission order.
The always-GPU conditional-compare-to-zero path matches Eden's Vulkan runtime;
it is not a confirmed missing upstream CPU fast path. Any Metal optimization
must retain authoritative GPU predicates, not assume guest RAM is current.

Verification: input-session focused tests 3/3 pass. Full GUI suite has 288
passes and two failures outside the modified files: shared_translation expects
the backend list without Metal; overlay_dialog initializes GTK from a Rust test
worker rather than the macOS main thread. Do not report the GUI suite as green.
Full release video_core suite under MTL_DEBUG_LAYER=1 passes 1735 tests with two
ignored and no warnings. The new attribution test proves changing a diagnostic
tag preserves compute encoder identity and counts one explicit render exit only
once; sampled windows count omitted work and reset when workloads shrink.
GUI release build is running in /tmp/ruzu-gui-input-build-20260906.log (session
23402). The user's existing GUI PID 63454 is still open and has not been stopped
or replaced. Runtime navigation/capture remains unverified until relaunch; do
not send new control commands to this old process, which has no socket.

GUI release build completed without warnings (3m24s); the final logical-button
enum mapping was rechecked by the 3/3 focused tests in
/tmp/ruzu-input-session-final-tests-20260907.log. target/release/ruzu is new
(2026-09-07 00:03), but ruzu.app still contains the 23:25 binary. Bundling is
deliberately pending while the user's Vulkan instance PID 63454 remains open.
The async question asking to close that test/relaunch Metal has not been answered.
Do not claim an automated game run or fresh screenshots have been obtained.

Next: after the user's test ends, bundle with the existing MoltenVK library
(SHA256 0995b17b030c01e991e2c36b48a953d8a4fdb6c4df1b9dcaa46b6d9e08612855),
then one watched ruzu.app launch with the isolated Metal configuration and
RUZU_INPUT_SESSION_DIR set to a NEW short /tmp directory. Never precreate that
directory. At +40 s use gui_control.py status/capture/press A; inspect fresh
960x540 captures and space further presses by at least 7 s. Check mapped HID
button A while down and after its 300 ms expiry, stop inputs on gameplay arrival,
and collect GPU operation counts only after the navigation/readbacks settle.
No FPS improvement has yet been implemented or measured in this follow-up.

## 2026-09-07 - CPU-owned conditional optimization (runtime validation pending)

While the user's Vulkan PID 63454 remains open, a bounded native optimization
was added: Conditional mode with neither query metadata nor GPU-modified buffer
data returns to Maxwell's existing synchronized CPU evaluation. Any prior GPU
predicate is cleared. This avoids resolve + per-draw argument masking for that
case without reading stale GPU-owned data. The two independent u32 words retain
the upstream AND semantics; a 64-bit nonzero replacement would be incorrect.

The targeted conditional suite passes 24 tests, including GPU values differing
from RAM, both single-zero-word cases, and removal of a prior host predicate
without any new GPU work. Full video_core validation passed as well; logs:
/tmp/ruzu-cpu-conditional-{tests,full-tests}-20260907.log. No Vulkan runtime
behavior changed; the shared engine has only an added regression test.
This supersedes the earlier note that no performance change was implemented,
but there is still NO measured game FPS gain or verified automatic navigation.
Rebuild the GUI again before the next bundle/run: its 00:03 executable predates
this conditional optimization. Keep the native profiling and input changes,
and preserve the currently running user's application until they finish testing.

The subsequent GUI release rebuild completed successfully (1m16s, no warnings),
as recorded in /tmp/ruzu-cpu-conditional-gui-build-20260907.log. The app bundle
has deliberately not been replaced while the user's original Vulkan PID 63454
is still alive. Neither the input socket nor the new conditional policy has
therefore been exercised in a game. The CPU fallback uses the original engine's
synchronized ReadBlock; query metadata / GPU-dirty records retain GPU evaluation.

## 2026-09-07 - Real tessellation prerequisite recovered offline

No second emulator was launched. A macOS-only ignored inspection test in
renderer_vulkan/pipeline_cache.rs uses that owner's cache version/key readers,
copies the selected file to a NEW owner-only directory, then scans the snapshot.
LoadPipelines must never receive the original cache because it deletes invalid
files. The diagnostic asserts snapshot survival before reporting counts and
exports through the existing complete-environment/IR capture boundary, using
the actual native Metal host translation profile. It builds no GPU pipelines.

The current Vulkan environment cache contains 730 graphics entries, one compute
entry, and TWO tessellation pipelines. Unlike the older geometry-only capture,
this provides real TCS/TES programs without another navigation run. Original and
snapshot SHA256 both equal
dc6b5b6471e9501a687f85c663e8c30aefc6e653dacc2fa44a3c47388715a455.
Artifacts: /tmp/ruzu-offline-tess-20260907-01, test log:
/tmp/ruzu-offline-tess-test-20260907.log (one ignored/manual test explicitly run
and passed). The earlier command with a short --exact filter ran zero tests;
only the subsequent fully-qualified test execution is validation evidence.

Pipeline inventory:
- f5643c92bc555963: VS 173683efa17936c9, TCS c0f4e20e1c27f47f,
  TES c1d03f8053f7efe5, FS 81b2fae866f8426c.
- cfbfb82c9fc851b8: VS 4a029f924db752b0, TCS 5e122f0e82a5c81a,
  TES 48114928452638f7, FS bb55422562412818.
- Both use Patch topology, three input control points, three TCS invocations,
  triangle TES domain, Equal spacing and clockwise tessellation winding. Neither
  contains a geometry stage or enabled XFB varying records.
- TCS loads indexed per-vertex generics and CBUF values, writes per-vertex
  generics, and writes Patch(0/1/2/4): three outer levels and inner level zero.
  Factors include data-dependent FP calculations, not only constant one.
  No barrier, shared/local memory or SSBO/texture opcode occurs in these TCS IRs.
- TES reads per-vertex generics and TessellationEvaluationPoint.U/V, CBUF values,
  InvocationInfo and LaneId, then writes output attributes. The second pair
  additionally carries Generic10 components. Do not replace its lane/patch
  interfaces with geometry-only constants or zero values.
- TCS runtime.tess_primitive=Isolines is the unused default in the shared
  runtime structure; make_runtime_info only fills the domain for TES. It is
  NOT evidence that the game requests isolines. program.output_vertices=0 is
  likewise not the TCS output count: its invocation count is three.

The native tessellation slice remains interrupted pending real interfaces:
callable vertex production -> TCS compute / retained patch data and factors ->
hardware tessellator -> TES post-tessellation vertex function -> fragment.
Apple documents separate factor/patch/control-point buffers, half-encoded
triangle factors and patch_id/position_in_patch inputs:
https://developer.apple.com/library/archive/documentation/Miscellaneous/Conceptual/MetalProgrammingGuide/Tessellation/Tessellation.html
Eden EmitGetPatch/EmitSetPatch/EmitInvocationInfo and declarations were re-read:
preserve generic patch ownership, outer/inner factor indices and the input
control-point count shifted by 16. This is a native backend prerequisite, not
a reason to alter the shared Maxwell translator or substitute a normal triangle.
Live rendering, same-scene comparison and >=20 FPS remain unmet. Do not infer
that these two omitted pipelines alone cause the low frame rate or flicker.

After this diagnostic addition, full release video_core with MTL_DEBUG_LAYER=1
passes 1,738 tests, zero failures, three ignored (including the new manual
inspection test), and no Rust warnings. Log:
/tmp/ruzu-offline-prerequisite-full-tests-20260907.log. The user-owned Vulkan
process is still alive and was neither stopped nor sent inputs. The next game
run must use the rebuilt GUI with the 40 s / 7 s input cadence and a resource
watchdog, after the user's current test ends.

## 2026-09-07 - Callable native TCS verified; TES/runtime still pending

The compiler now emits the shared Maxwell TCS IR as an inline native MSL
function. Its compute caller owns a complete patch per workgroup, invocation
IDs, input/output device buffers and patch storage. Output attributes index
InvocationId rather than the IR SetAttribute vertex operand, exactly as Eden.
InvocationInfo preserves the input patch count shifted by 16; patch outer
indices 0..3 and inner indices 4..5 remain separate. Factors stay guest f32
until a later native factor-buffer conversion. No live tessellation gate was
removed: TES and the retained factor/control-point draw pipeline are prerequisites.

The final upstream audit corrected an intermediate mistaken per-component
default: DefineInputs gates a WHOLE vec4 on Generic(index), not each component.
Only absent/Disabled input interfaces default to (0,0,0,1). Regression tests
cover partial masks, disabled inputs, invalid invocation counts, patch indices,
barrier memory domains and rejection as a normal stage entry point.

Native Metal validation executed two workgroups of three TCS invocations with
distinct inputs, CBUF reads, cross-invocation output reads, generic output and
computed patch factors. GPU-observed output stride is 80 bytes for that interface;
the host never assumes a Rust vec4 struct layout. The two actual captured TCS
programs also compile into compute pipelines using the real native device profile:
/tmp/ruzu-offline-tcs-20260907-03 and
/tmp/ruzu-tcs-final-captured-test-20260907.log. This is compile evidence, not a
rendered comparison. Original game cache was only copied, never modified.

Full release tests with MTL_DEBUG_LAYER=1: shader_recompiler 571 passed;
video_core 1,739 passed and three ignored; zero failures/warnings. Log:
/tmp/ruzu-tcs-final-full-tests-20260907.log. No new in-game FPS result exists.
Next prerequisite: TES callable input/output and native post-tessellation entry
point. Its per-control-point buffer stride must come from the TCS producer,
not from just the subset of generics the TES reads. User navigation remains
40 seconds then A at seven-second intervals, stopping on gameplay arrival.

## 2026-09-07 - Callable TES and native patch rasterization prerequisite

The native compiler now has a separate ruzu_evaluate callable TES. It uses
the producer's concrete ControlPoint and PatchData template types, rather than
reconstructing a struct from the consumer's attribute subset (which would read
the wrong stride). Generic/position inputs preserve the control-point index;
patch generics are read-only and patch-wide. PatchVertices/PrimitiveId and
TessCoord retain Eden's ownership and raw-bit conventions. The normal stage
entry still rejects TES until a real native wrapper owns its draw state.

Important refinement of the LaneId prerequisite: both captured TES programs
use it ONLY as the vertex operand of TessCoord U/V loads. Eden ignores that
operand for those built-ins; it is not a semantic SIMD dependency. The native
layout checks every lane use (including phi operands) and elides only the
ignored value, without manufacturing a zero lane. Observable lane/shuffle/vote/
mask operations remain explicitly unsupported in post-tessellation vertex
functions. The earlier blanket suspicion about LaneId blocking these two TES
programs is invalidated; it remains a limitation for other observable uses.

A native hardware test executes two triangle patches with compute-produced
points/factors, per-patch data, CBUF data, hardware domain coordinates and
generated TES code. Extra producer fields precede both consumed generic fields.
The result is a red and a green patch with the expected blue component; setting
only the second patch's factors to zero leaves the first intact and clears the
second. No intermediate CPU readback/wait separates compute and rendering.
The final wait exists only for the test's output verification.

Final full release tests (Metal validation enabled): shader_recompiler 574
passed; video_core 1,740 passed, three ignored; zero failures and no warnings.
/tmp/ruzu-tes-final-full-tests-20260907.log
Both real captured TCS/TES pairs also compile into native compute/render PSOs:
/tmp/ruzu-offline-tes-20260907-02
/tmp/ruzu-tes-final-captured-test-20260907.log
The manual probe uses a test fragment stage and is not evidence of actual game
pixels. Original cache SHA256 remains unchanged from the earlier snapshot.

Next: implement native tessellation pipeline ownership in renderer_metal:
vertex production/patch assembly -> TCS compute -> half factor conversion ->
hardware tessellator/TES -> real fragment, with resource bindings and lifetimes
retained by the scheduler. Keep the live pipeline rejection until that whole
chain exists. Handle domain/spacing/winding and unsupported isolines truthfully;
do not substitute ordinary triangles. Only then validate the game and FPS.
The user's original Vulkan PID 63454 remains open, untouched. No new GUI run,
automatic input sequence, game capture, or FPS gain occurred in this turn.

## 2026-09-07 - Native tessellation runtime interrupted at patch input assembly

The existing MetalPrimitiveAssembly stores six ordinals per primitive and exposes
geometry InputTopology; it cannot represent patches of up to 32 control points.
Do not reinterpret Patch as Triangle/Point merely to reuse this type. The next
prerequisite separates the shared vertex-index/restart stream from primitive
records, then adds GPU patch-start compaction and dispatch/draw arguments. TCS
will consume each complete patch as one workgroup; incomplete trailing segments
must not become patches. Native input decoding must still preserve baseVertex
bits, index width/offset, restart-before-base-add and instance counts. Pipeline
integration remains stopped until this prerequisite is implemented and tested.

## 2026-09-07 - Patch input and retained TCS runtime prerequisites implemented

The preceding interruption is resolved for input assembly and control execution:
MetalVertexStream now separates index/restart decoding from geometry topology.
MetalPatchAssembly compacts complete patches on GPU and writes dispatch/draw
arguments; trailing incomplete segments are not dispatched. Patch PSOs are lazy.
Tests cover index widths 0/1/2/4, restart across scan workgroups, negative
baseVertex bit patterns, empty inputs and instance counts. The ordinary geometry
and fan paths retain their tested ordering and output behavior.

MetalGeometryVertexPipeline::record_stream consumes the actual patch stream.
The new MetalTessellationControlPipeline owns generated native compute entry,
resource-slot allocation, output allocations and indirect dispatch. It binds
prepared resources without reading guest memory later. One workgroup owns one
patch, with exactly the program's invocation count. Full vertex-producer stride
is retained even for generics that TCS does not read. TCS input count and output
invocations are independently tested (including 32 inputs / three outputs).

Important integration contract: output regions are separated per instance by
capacity_per_instance, NOT the GPU-compacted count of complete patches. TES and
factor conversion must use that same capacity stride. The GPU tests include
restart-discarded patches that make these two quantities differ. Native
static_assert verifies input/output/patch record sizes at pipeline compilation.
An audit also fixed missing TCS clip-distance initialization in MslEmitContext;
GPU checks verify the declared but unwritten components are zero, as in Eden.

Confirmed first full release suites: shader_recompiler 575 passed, video_core
1,744 passed and three ignored, zero failures/warnings, Metal validation on.
/tmp/ruzu-control-runtime-full-tests-20260907.log
The expanded runtime test covers 1/3/32 invocations, 3/32 input points, multiple
instances, empty/restart-only ranges, CBUF values, SSBO side effects, patch
variables, primitive IDs and GPU retention after host packets are dropped.
The earlier captured pair probe still passed after the producer-stride change:
/tmp/ruzu-patch-captured-test-20260907.log
/tmp/ruzu-offline-tes-patch-20260907-01
It has now been changed to compile the actual runtime TCS wrapper, not a probe
entry. Final-source tests and that updated probe are being run separately.

Next prerequisite: factor conversion and real native post-tessellation pipeline
in metal_tessellation_pipeline.rs, then stage/resource integration in the native
pipeline cache and rasterizer. Preserve sparse instance strides and GPU predicate
gating for VS/TCS/TES side effects. Do not merely remove the live rejection.
Equal-spacing factor conversion must not let f32-to-half rounding change ceil
segment counts near integer boundaries; fractional modes also need explicit
native capability/precision validation. No default factors or global waits.

No new game run or FPS gain is claimed. User Vulkan PID 63454 remains open and
untouched. Navigation on the next authorized run: wait 40s, then A at intervals
of at least seven seconds, checking captures and stopping on gameplay arrival.
Free disk space remains about 32 GiB. DIFF.md audit updated; no commit made.

Final-source verification completed:
- /tmp/ruzu-control-runtime-final-tests-20260907.log: shader_recompiler 576 passed;
  video_core 1,744 passed, three ignored; no warnings/failures, Metal validation on.
- /tmp/ruzu-control-runtime-captured-test-20260907.log: both real TCS programs
  compile through MetalTessellationControlPipeline::new; both TES render probes
  compile. This remains compilation evidence, not a game-render comparison.
- /tmp/ruzu-offline-control-runtime-20260907-01 contains the private cache copy
  and callable MSL/IR captures. Original and copied cache SHA256 both remain
  dc6b5b6471e9501a687f85c663e8c30aefc6e653dacc2fa44a3c47388715a455.
- git diff --check passes. No full GUI rebuild/rebundle or new launch occurred.

## 2026-09-07 - Native factor conversion and denormal regression

MetalTessellationFactorPipeline now consumes the actual TCS patch layout and
GPU-compacted dispatch count. It writes separate half buffers for triangles
(three outer, one inner) and quads (four outer, two inner), retaining the TCS
capacity-based instance stride. Conversion is GPU-only, with retained resources
and a buffer barrier before the eventual tessellator read. No default factors,
CPU count readback, global wait or live-path draw suppression was introduced.

Equal mode clamps and ceils in f32 before conversion, preserving segment count
for values just above integers. Fractional even/odd modes clamp to their correct
range, retain fractional half precision and correct only a rounding-induced
crossing of the integer segment-count boundary. Fractional generated coordinates
are not yet validated against a guest/native oracle. Isolines remain explicitly
unsupported by this hardware path. Device profile owns known tessellation limits;
an unsupported requested generation level is rejected, never silently reduced.

Measured failure during implementation, now corrected: the first GPU matrix
failed for triangle/Equal, external factor f32 bits 0x00000001 (1e-45), output
half 0x0000. A floating comparison had treated the positive subnormal as zero.
The outer discard decision now inspects sign/magnitude/NaN bits BEFORE numeric
clamping. This is a synthetic conversion bug caught by the test, not a proven
cause of the original game's missing geometry or low FPS.
- Failed evidence: /tmp/ruzu-tess-factors-case-test-20260907.log
- Corrected matrix: /tmp/ruzu-tess-factors-fixed-test-20260907.log
- First full pass: /tmp/ruzu-tess-factors-full-tests-20260907.log
  (shader_recompiler 576 passed; video_core 1,747 passed, three ignored).
The production converter is also used by the hardware TES raster test: computed
patch data -> factor conversion -> native tessellator/TES -> fragment pixels.
Discarding patch two leaves patch one intact; both colors are checked on GPU
readback. The final matrix additionally varies each external/internal component
independently to detect incorrect field order.

Next: native TES entry and render PSO in metal_tessellation_pipeline.rs, then
cache/rasterizer resource integration. Resolve the factor-buffer instance index
contract BEFORE issuing nonzero-baseInstance draws: Apple documents the factor
lookup using instanceID * instanceStride, whereas allocated TCS regions start at
instance zero. Test the hardware behavior; if rebasing the native draw to zero
is necessary, retain the guest base in the explicit shader ABI rather than
allocating an unbounded prefix or changing guest InstanceId. Do not assume the
existing single-instance raster test proves this case. Preserve predicate gating
of every guest stage, not only the final raster draw.

API sources re-read: Apple's tessellation guide and Metal capability tables;
maxTessellationFactor/MTLTessellationPartitionMode documentation (including the
official Markdown endpoints); Khronos tessellation discard/spacing specification.
Eden stayed read-only. DIFF.md updated. User Vulkan process 63454 is still open,
untouched; no new GUI run, game screenshot, frame-rate result or commit.

## 2026-09-07 - Worker stop race and hardware instance-addressing gate

The final factor suite did not initially finish: video_core's
unmap_memory_unregisters_untracks_and_deletes_image was stuck in
StatefulThreadWorker::Drop joining TextureDecoder. The owned test process 91275
was sampled (see /tmp/ruzu-tess-factors-test-sample-20260907.txt), revalidated live,
then explicitly terminated after diagnosis. The user's GUI 63454 was untouched.
This was not a GPU-wait timeout or a reason to silently restart a live test.

Confirmed local defect: Drop stored stop and notified without queue_mutex,
allowing the notification to precede the worker's atomic Condvar wait/unlock.
Eden uses stop-token-aware condition_variable_any; the Rust adaptation needs the
predicate mutex. Drop now takes it only around stop publication and releases it
before notification/join. The new mutex-contract regression failed before the
fix, then passes; startup/drop stress verifies destruction of 128 worker states.

Post-fix complete verification: /tmp/ruzu-worker-stop-final-tests-20260907.log
common 367 passed; shader_recompiler 576 passed; video_core 1,747 passed with
three ignored. No warnings, Metal API validation enabled. This suite includes
all previous factor/TCS/TES tests, but predates the subsequent instance oracle.
It does not demonstrate any live-game performance gain.

Hardware gate resolved: native PerPatchAndPerInstance factor addressing DOES
include baseInstance on this M2 Pro. Direct draws with bases 0 and 11 produced
the expected separate red/green patches only from the corresponding absolute
factor regions. Evidence: /tmp/ruzu-tess-base-instance-20260907.log, one passed.
Production patch assembly now emits native baseInstance=0, retaining the guest
base separately for the preceding VS and its resource fetches. Tests use that
retained field; an extended oracle exercises real indirect draw arguments with
guest bases 11 and UINT32_MAX while allocating only two factor regions.

Next integration boundary remains the production TES entry/render PSO and
resource binding, then live cache/rasterizer stage execution with conditional
gating of every producer. The instance-offset prerequisite is no longer an
unresolved guess. No game-specific values, silent draw removal or global waits
were introduced. User input cadence remains 40 seconds after launch, then A
every >=7 seconds until gameplay, with scene inspection rather than a fixed
number of button presses. No second emulator has been launched.

Final-source verification (including retained-base VS callers and native
indirect rasterization) completed in
/tmp/ruzu-tess-native-instance-verified-20260907.log: common 367 passed,
shader_recompiler 576 passed, video_core 1,748 passed/three ignored. Metal API
validation enabled, no warnings or errors. git diff --check passes; disk still
has 31 GiB available. User GUI 63454 remains running, unmodified. No GUI rebuild,
new game capture, commit or FPS result in this slice; the goal remains open.

## 2026-09-07 - Production TES entry, PSO and retained binding verified

MetalTessellationEvaluationPipeline is implemented in its native pipeline owner.
It wraps the common callable TES using the exact producer control/patch layouts,
three transport slots after guest resources, zero-based instance regions and
native triangle/quad patch attributes. Domain/spacing/winding and framebuffer
blend/sample/raster state are part of the PSO; no Vulkan shader conversion is used.
Rasterization-disabled entries return void but still execute all TES side effects.
Binding validates producer strides, capacities and buffer bounds. Empty patch
draws bind a valid positive native factor step without inventing any invocation.

The previous hand-written hardware TES entry in metal_shader.rs has been removed.
Its replacement test executes production PSO/bind + real GPU assembler draw args,
and verifies red/green output, CBUF and patch-generic multiplication, unread producer
generic fields, factor-discard, zero patches, and TES atomic writes with and
without rasterization. Both raster modes preserve writes for nonempty draws;
empty draws produce neither writes nor pixels. All these checks pass.

Constructor coverage: triangles/quads, Equal/FractionalEven/FractionalOdd, CW/CCW,
last valid buffer base (28), rejecting base 29 and isolines. Fractional coordinate
placement and winding equivalence to a guest oracle remain unverified; constructor
success alone must not be presented as that evidence.

Verification:
- /tmp/ruzu-tes-native-pso-20260907.log: initial production pixel test passed.
- /tmp/ruzu-tes-native-discard-20260907.log: raster-disabled side effects passed.
- /tmp/ruzu-tes-pso-full-20260907.log: final shader_recompiler 576 passed;
  video_core 1,749 passed/three ignored; no warnings, Metal API validation on.
- /tmp/ruzu-tes-production-captured-20260907.log: both captured TCS compute and
  TES production PSOs compile (f5643c92bc555963, cfbfb82c9fc851b8). The offline
  probe still uses a diagnostic fragment; it is NOT a whole guest-pipeline replay.
- /tmp/ruzu-offline-tes-production-pso-20260907-01 contains the private cache
  copy and callable shader captures. Original and copy SHA256 unchanged:
  dc6b5b6471e9501a687f85c663e8c30aefc6e653dacc2fa44a3c47388715a455.

Next concrete integration slice (guard remains in place):
1. Add retained tessellation variant beside MetalVertexShader::Geometry in
   metal_pipeline_cache.rs, preserving all stage binding counters/runtime keys.
2. metal_graphics_pipeline.rs currently skips native stages 1/2 through
   advance_empty_native_stage. Prepare real TCS/TES descriptor payloads there;
   do not reuse VS resources or read guest memory after waiting for a pipeline.
3. Add topology-independent indirect VS entry to MetalGeometryVertexPipeline's
   shared record_impl interface. Current record_indirect requires a geometry
   assembly; do not fabricate one for a patch stream.
4. Gate the VS dispatch, TCS/factor dispatch and final patch draw from the same
   predicate. ConditionalArgumentLayout::Draw already has the same four-u32
   size/instanceCount position as patch args; retain real instance base for VS.
   Do not merely suppress rasterization while allowing conditional shader stores.
5. Capability-check indirect tessellation (Apple5+/Mac2 per Apple tables), then
   integrate the whole retained chain in metal_rasterizer.rs before enabling the
   TCS/TES cache path. Native TES+GS chaining must not silently drop either stage.

Eden stayed read-only; DIFF.md updated, git diff --check passes. User GUI 63454
is still running and untouched. No new GUI launch/rebundle or FPS claim. The
game-rendering and >=20 FPS goal is still open.

## 2026-09-07 - Full tessellation chain wired into live rasterizer

The five integration steps above are implemented for direct patch input. The
cache now retains a VS/TCS/TES variant, builds with shared stage binding counters,
and caches its native PSO. Graphics resource preparation binds actual stage 1/2
payloads. The rasterizer records VS, TCS, factor conversion and native TES draw,
preserving input-vs-output control-point counts, index offsets/restart, guest
instance base, render-area constants and argument-buffer sampler lifetimes.

Conditional gating is shared by every producer and the final draw. The complete
GPU test has VS clear the predicate itself and verifies that TCS/TES still follow
the initial decision. Enabled/disabled/inverted cases, two instances, guest base
19, per-stage SSBO atomics and rendered pixels pass. No per-draw finish/wait or
CPU readback was added. Guest-indirect patch input and TES+GS remain explicit
unsupported prerequisites, not silently truncated pipelines.

Evidence:
- /tmp/ruzu-tess-chain-test-fixed-20260907.log: complete chain GPU test passed.
- /tmp/ruzu-tess-live-check-20260907.log: live-path release check passed.
- /tmp/ruzu-tess-live-full-tests-fixed-20260907.log: shader_recompiler 576 and
  video_core 1,750 passed, three ignored; Metal validation on, no warnings.
- /tmp/ruzu-tess-live-captured-chain-20260907.log: both real captured pipelines
  f5643c92bc555963 / cfbfb82c9fc851b8 compile via production cache build, including
  actual VS/TCS/TES/FS, shared bindings and recorded attachment formats. PSO reuse
  asserted. This improves on the previous diagnostic-fragment-only probe.
- /tmp/ruzu-offline-tess-chain-live-20260907-01 holds the private cache snapshot.
  Original and snapshot SHA256 still match:
  dc6b5b6471e9501a687f85c663e8c30aefc6e653dacc2fa44a3c47388715a455.

Compilation is NOT a vertex-fetch or frame oracle: Vulkan disk keys can omit
dynamic vertex state. Fractional coordinate/winding equivalence, game rendering,
other-title regression and >=20 FPS are still unverified. No goal completion claim.

GUI release build started, log /tmp/ruzu-tess-live-gui-build-20260907.log; not yet
rebundled or launched. Existing user GUI PID 63454 is still running; asked the
user to close it before replacing ruzu.app or starting another instance. No
inputs sent, no save/config/library changes. Next launch must wait 40s, then A
at intervals >=7s with scene inspection until gameplay, and monitor memory/disk.

GUI build completed successfully in 2m07s, no warnings. The rebuilt executable is
target/release/ruzu; target/release/ruzu.app is deliberately still the old bundle
while user PID 63454 remains active. Do not launch that old bundle as a test of
the new tessellation path. Rebundle only after the user closes the old GUI,
preserving the current MoltenVK library. Disk remains at 31 GiB available.

## 2026-09-07 - Native tessellation winding regression found and corrected

Existing user GUI 63454 is still alive; no second emulator launched or inputs
sent. A read-only ScreenCaptureKit attempt on its windows failed with -3811,
including window 15355 (title identifies Luigi's Mansion 3, old build 1f700f9992).
No successful new reference image exists from that attempt; do not reuse an old
PNG as evidence for this slice.

Used the remaining runtime-validation boundary to test winding. Eden leaves the
tessellation domain at Vulkan's default upper-left origin: clockwise triangles
have positive signed (u,v) area. Native Metal's tessellator has the opposite
convention. MoltenVK's mvkMTLWindingFromSpvExecutionModeInObj also reverses the
two values. The initial native CW-to-CW mapping was wrong.

New test evaluation_winding_matches_upper_left_domain_triangles compares the
production TES/factor/indirect-draw path against explicit triangles with the
specified signed area. Both use the same native viewport, front-face and culling
state; the fragment reports front/back as red/green. Before correction the first
triangle case was back-facing instead of front-facing at all three interior
probes: /tmp/ruzu-tess-winding-before-20260907.log (test failed).

The pipeline now reverses tess_clockwise at the native API boundary, without
changing the guest runtime state or rasterizer front-face settings. After the
fix, /tmp/ruzu-tess-winding-after-20260907.log passes all 216 reference comparisons:
triangles/quads, all three spacing modes, levels 1/2.25/3.5, both guest winding
orders, both front-face modes and no/front/back culling. This validates orientation
and sampled planar coverage, NOT exact fractional coordinates or interior edge
placement. No workaround, game-specific predicate or GPU-wide wait was added.

Full release suites are running in /tmp/ruzu-tess-winding-full-20260907.log;
GUI rebuild follows in /tmp/ruzu-tess-winding-gui-build-20260907.log. The previous
target/release/ruzu build predates this winding fix until the latter completes.
Bundle remains untouched while the user GUI is open. DIFF.md includes the source
audit. Game rendering, other-title regression and >=20 FPS remain open gates.

Final verification completed: shader_recompiler 576 passed; video_core 1,751
passed/three ignored, no warnings, Metal validation on. GUI release build
completed successfully in 1m42s (including waiting for Cargo's artifact lock),
no warnings. target/release/ruzu contains the winding fix; ruzu.app remains the
old bundle until user GUI 63454 closes. git diff --check passes. No commit/push.

## 2026-09-07 - Live validation waiting for user session closure

Revalidated the same live-run blocker across three consecutive goal turns.
User GUI PID 63454 (started Sep 6 23:37:41) remains active in the old ruzu.app;
the last check reports 3h18m elapsed and about 594 MiB RSS. No permission to stop
the user's session has arrived. Do not start a second emulator, replace its
bundle underneath it, or treat the old rendering as evidence for these fixes.

The source changes, successful release binary and test evidence are preserved.
Next required evidence is gameplay rendering and performance from this build,
not another compilation-only claim. Resume after the user closes the GUI or
explicitly authorizes stopping it: verify no emulator remains, rebundle while
preserving MoltenVK, launch the monitored isolated Metal GUI session, wait 40s,
then press A at >=7s intervals with renderer captures until gameplay. Compare
the same scene and check memory/disk before measuring the >=20 FPS gate.
The goal is blocked at this runtime gate, not completed.

## 2026-09-07 - Live Metal lobby reached after resuming

The user authorized stopping/restarting the GUI. PID 63454 had already exited
when checked; no second emulator was started alongside it. Rebundled the release
GUI including the tessellation winding fix. MoltenVK SHA256 is unchanged:
0995b17b030c01e991e2c36b48a953d8a4fdb6c4df1b9dcaa46b6d9e08612855.

One watched Metal GUI run (PID 5365, /tmp/lm3-ts-0907a-app.log) loaded all 1020
cached pipelines, then reached the existing Lobby save with seven logical A
presses. First press after 40 seconds, all later presses separated by >=7 seconds.
Fresh renderer captures in /tmp/lm3-ts-0907a show profile, update information,
Story, save selection, confirmation and gameplay. loaded-01.png and
gameplay-02.png show the characters, staircases and lobby without the earlier
large triangles in these frames. This is NOT a full differential validation or
a claim of flicker-free rendering. No further buttons sent after loading.

Performance remains insufficient: the fresh window capture
/tmp/lm3-ts-0907a-window-15477.png reports 4 FPS. A 2-second CPU sample has
433/647 GPU-thread samples in CAMetalLayer::nextDrawable. This identifies an
observed wait, not whether the cause is drawable lifetime or GPU execution cost.
Next run needs the existing native GPU timing counters before optimization.

Resource watch: /tmp/lm3-ts-0907a-resources.csv. Peak footprint about 9 GB,
no swap growth, disk stayed about 30 GiB free. Quit normally after 342 seconds
(exit 0). No emulator remains from this run; no memory/disk exhaustion observed.

Input requests worked but their replies timed out. Standalone Python-to-Rust
Unix datagram probe reproduced a truncated return pathname on Darwin: reply
became repl, send_to failed ENOENT. Explicitly NUL-terminating the Python bind
address fixes the round trip. gui_control.py now does this on Darwin only.
Rust control logs reply errors rather than discarding them and has a real socket
round-trip test. GUI status also exposes the existing copied performance sample;
it does not reset counters or touch rendering. Four focused GUI tests pass in
/tmp/ruzu-input-roundtrip-20260907.log. GUI rebuild for these diagnostics is in
/tmp/ruzu-input-perf-build-20260907.log. >=20 FPS and other-title regression remain
open; the previous user-session blocker is no longer applicable.

## 2026-09-07 - Measured lobby bottleneck: GPU work and encoder fragmentation

Second monitored GUI run PID 51859, /tmp/lm3-ts-0907b-app.log, same isolated Metal
configuration and save. Rebuilt GUI including status FPS and diagnostic reply
logging. Python Darwin fix works live: status, all seven A acknowledgements and
fresh internal captures succeed; lobby.png confirms the same loaded scene. No
keyboard mapping changes. Stopped normally after 327 seconds (exit 0).

Enabled existing RUZU_PROFILE_METAL_SUBMISSIONS and RUZU_PROFILE_METAL_STAGES.
No API-validation layer, no renderer captures during the performance interval
03:33:12..03:33:40 UTC. Fifteen copied GUI samples every 2 seconds in
/tmp/lm3-ts-0907b-fps.jsonl: median 7.947 FPS, range 6.931..7.990. These are
instrumented measurements, not a claim of improvement over the earlier 8 FPS.
The prior unprofiled 4 FPS screenshot was one snapshot, not a matched benchmark.

Twenty-eight completion intervals in this window: presentation command buffers
average 0.197 ms GPU execution, guest command buffers average 29.807 ms; the
median per-interval longest guest buffer is 134.105 ms. Multiple guest buffers
compose a frame and execution spans can overlap, so sums are NOT GPU utilization.
Nevertheless long guest execution is observed independently of the CPU's wait
in nextDrawable. Merely moving that wait to another thread is not a demonstrated
solution to the frame's GPU cost.

Twenty-seven sampled guest batches have median encoder/stage counts:
235 blit, 916 compute, 1342 vertex + 1342 fragment (=1342 render encoders).
Compute calls: other=0, guest=1, geometry_vertex=1, primitive_assembly=83,
conditional_resolve=37, conditional_arguments=635, visibility_resolve=0,
index_conversion=463. Render breaks attributed to index_conversion=452 and
conditional_arguments=417, conditional_resolve=36, primitive_assembly=1,
other=249. These are counts per sampled batch, NOT costs or whole-frame totals.
The CPU sample also observes Uint8Pass::assemble in the index-binding path.
The direct geometry producer itself is not the dominant dispatch count here.

Next optimization slice: characterize repeated UInt8 index conversion and
conditional draw preparation. Reduce redundant conversions/render-pass exits
only with valid source-content lifetime/invalidation and predicate visibility.
Do not reuse converted indices by address alone, move conversions across guest
writes, remove required barriers, or force predicates true. A conversion cache
needs complete CPU/GPU-write invalidation before reuse is safe. The profiling
currently groups UInt8 and quad conversions together; split attribution before
claiming all 463 are UInt8. Preserve native Metal ownership, not Vulkan command
structure for its own sake. No such optimization was applied in this slice.

Resource watch: peak footprint 8.131 GiB, disk free delta -28.1 MiB, swap used
delta -16 MiB. No runaway memory or disk growth reproduced. No emulator remains.
GUI release build completed without warnings; four focused Rust input tests and
two Python address-regression tests pass. Full GUI suite was not rerun (its
earlier macOS main-thread test failures remain recorded above). Existing full
shader/video results remain those of the winding slice. DIFF.md updated, no
commit/push. Required >=20 FPS, differential rendering and other-title runtime
regression gates remain open.

### 2026-09-07 — Bounded UInt8 conversion reuse, runtime gate pending

Implemented a native backend-Buffer-owned conversion cache to test the repeated
index conversion hypothesis above. The key is offset/count within a source
allocation and its content generation, never a guest address alone. Sources
marked GPU-written by the common buffer cache are ineligible. CPU writes,
encoded copies and native query blits invalidate at recording time, including
multiple writes in the same scheduler tick. Saturation disables reuse.

Private output allocations avoid caching recyclable staging slices. Global
limits are 16 MiB of logical data and 4096 entries, excluding native allocation
overhead and in-flight retained resources. Overflow uses the original conversion.
No global wait, readback, draw omission, predicate change or kernel change was
introduced by this slice. Re-read Eden Uint8Pass/BindIndexBuffer/SyncValues;
the deliberate native caching difference is audited in DIFF.md.

Full release video_core suite with Metal API validation: 1756 passed, 3 ignored.
The GUI build and same-lobby runtime measurement are pending. Compare against
run B's median 7.947 FPS, and do not infer an FPS gain from unit tests or lumped
index-conversion counters (which include quads).

First live attempt with cache: `/tmp/lm3-0907c-app.log`, resource history
`/tmp/lm3-0907c-resources.csv`. Seven confirmed A presses reached the lobby
loading sequence. The monitor stopped PID 22053 at 189 seconds after physical
footprint reached 10.236 GiB (baseline B peaked at 8.131 GiB). It required KILL
after the three-second TERM grace period; this was a safety stop, not an
observed spontaneous panic. Disk delta was only -23.88 MiB, swap delta zero.
No completed visual/FPS validation: the last captured frame was Loading.
Early gameplay counters exist, but cannot establish performance or correctness.

Added bounded diagnostics under the existing RUZU_PROFILE_METAL_SUBMISSIONS
gate: once-per-second UInt8 requests/hits/exclusions, retained cache payload and
entry count, common-buffer logical bytes and Metal currentAllocatedSize. The
environment is read once at runtime creation. This will distinguish the new
derived cache from other native allocations; do not assume the footprint jump
is either a cache leak or unrelated without measurement. Safety limits unchanged.

Second attempt `/tmp/lm3-0907d-*` exposed a GPU command-buffer error at lobby
entry, with a driver recovery message; subsequent black frames/FPS are invalid
performance evidence. Stopped normally, exit 0. The derived cache retained only
~55 KiB in the observed intervals, while Metal reported ~2.6 GiB allocated.
Suspending performance validation to fix a prerequisite found in the index path:
the rasterizer applies first_index after conversion but Uint8Pass was asked to
convert count elements, not first_index + count. Dedicated exact-sized outputs
make this range error more visible than staging allocation padding. This is a
confirmed range defect, not yet proof that it caused the live GPU error.

The index-origin prerequisite is implemented: convert first + count, keep the
draw's original first_index. Full release video_core suite passes 1757 tests,
3 ignored under Metal API validation. Dedicated test covers nonzero allocation
offset and first_index, restart, reuse and the noncacheable staging path.

Live API-validation attempt `/tmp/lm3-0907e-app.log` aborts BEFORE title input
at ~14 seconds: MTLDebugRenderCommandEncoder reports main0 texture tex4 declared
float but bound to MTLPixelFormatX32_Stencil8 (requires uint/ushort). This is not
an index validation error. Stopped automated input after connection refusal;
process exited by SIGABRT, no remaining instance. Performance gate suspended.

Next prerequisite is the sampled depth/stencil numeric-type contract. Rechecked
Eden ImageViewAspectMask, ConvertTexturePixelFormat/IsTexturePixelFormatInteger
and Metal equivalents: aspect selection follows the swizzle, while shader
integer classification uses the combined PixelFormat. Need actual bound view
and shader descriptor before changing either, not a blind depth-view fallback.
An API-validation-only one-shot METAL_STENCIL_TYPE diagnostic in prepare_stage
reports stage, descriptor, TIC index, base and constructor ImageViewInfo. It is
temporary investigation tooling, no draw/binding changes. Rebuild/live capture
pending. Do not call either performance optimization or live rendering validated.

Confirmed descriptor capture: `/tmp/lm3-0907f-app.log`, same validation abort
at ~14 seconds (PID 11748 exited SIGABRT). Stage 4, CBUF 2 offset 64, TIC 5235,
view image_id 30 at GPU address 23675535360, 1920x1080, format S8UintD24Unorm,
swizzle [2,2,2,2] = RRRR. Descriptor is Color2D, is_depth=false,
is_integer=false. This really selects stencil, not a null view or depth-only
comparison sampler. The format-lookup entries for S8UintD24Unorm have UINT in
R and UNORM in G; native aspect selection matches Eden ImageViewAspectMask.
Shader environment integer classification only sees the combined format and
returns false (also true of the inspected Eden code). Metal's native stencil
texture requires an integer MSL resource declaration. Do not change the bound
aspect to depth or suppress this validation failure.

Resume prerequisite: represent the selected aspect's numeric type in shader
translation and ensure the emitted MSL preserves Maxwell's raw integer return
semantics, swizzle and sampler behavior. Add an actual stencil-sampling render
test under Metal API validation, not merely image-view construction tests.
Graphics and compute must agree; depth sampling and combined RT formats must
stay unchanged. FileEnvironment currently persists only the combined format,
not the TIC swizzle. Old metal.bin environments are insufficient to infer this
aspect: plan compatibility/invalidation without deleting original caches (use
private copies or a separate native cache namespace for experiments). Do not
blindly reinterpret every S8UintD24Unorm descriptor as integer: G selects depth.

No emulator remains. GUI rebuilt successfully without warnings, including the
one-shot diagnostic. Performance remains unvalidated; last comparable baseline
is 7.947 FPS. Current index optimization/correctness changes are uncommitted.

Final verification of this snapshot (including the one-shot diagnostic):
`/tmp/ruzu-stencil-contract-final-tests-20260907.log`, release video_core with
MTL_DEBUG_LAYER=1: 1757 passed, 3 ignored, no warnings. git diff --check passes.
Bundled MoltenVK SHA-256 remains unchanged:
0995b17b030c01e991e2c36b48a953d8a4fdb6c4df1b9dcaa46b6d9e08612855.

### 2026-09-07 - Bounded native render-pass attribution (run J)

Added scalar metadata to existing sampled render passes: shader identities,
draw count, native attachment storage dimensions and pixel formats. Shared
passes distinguish first/last shaders and a mixed-shader flag. No draw order,
encoder reuse, resource lifetime or GPU wait changes. Metadata is bounded by
the existing timestamp sample budget; at most eight ranked passes are logged
per completed sampled batch. Unsampled passes never append metadata. Tests
cover mixed identities, compute transitions, budget exhaustion and lease reuse.

Full release video_core: 1763 passed, 3 ignored, no warnings. GUI build/bundle
successful. Logs /tmp/ruzu-pass-profile-{tests,gui,bundle}-20260907.log.
Run J: /tmp/lm3-0907j-app.log and resource CSV, PID 66836, same release Metal
GUI/config/profile flags, no API validation. Seven automated A inputs after
initial 40 seconds, >=7 seconds apart. after07.png/lobby.png confirm the Lobby.
No inputs/captures during 15 status samples, 05:09:55..05:10:23 UTC, saved in
/tmp/lm3-0907j-fps.jsonl. Median 7.9389 FPS, range 7.8429..8.9074, unchanged.
Normal exit at 197 seconds; no process remains. Peak footprint 8.365 GiB,
swap delta -8 MiB, free disk delta -23.0 MiB. No saturation in this run.

184 ranked-pass records in the measurement window provide concrete identities:
- VS D32B4C8446AAA0DB / FS 6B3071DDAF092CA3: one draw, three 1920x1080
  color storage attachments (native formats 92/92/25), no depth. Four sampled
  records, fragment ticks around 1.65 million each.
- A mixed 39-draw 1600x900 pass: first VS B200E98022015F82,
  first FS 024269BC313E7507, color format 92, depth 260. One sampled record
  with vertex 2,832,250 ticks and fragment 1,218,000 ticks. Do not attribute
  the whole pass to the first shader.
- Several 64x64 mixed-shader passes also rank highly. No demonstrated single
  offending shader yet; a ranked partial sample is not a frame-time breakdown.

Important measurement gap, verified against exact submission ticks: across
the run, 81 GPU_TIME reports have guest peaks >100 ms. Only nine of those
peak ticks have STAGE_TIME records. Tick 12695 is a real 130.376-ms peak:
seen 235 blit / 838 compute / 1277 render, but the window measures only
53 / 121 / 169 render entries. Ticks 13188 and 14471 have zero measurements
because the rotating window starts beyond these smaller batches. Thus the
existing partial windows cannot explain the long command buffers reliably.
Do not conclude the fullscreen shader or geometry emulation is the root cause.

Clock check: Apple's paired sampleTimestamps API returns CPU nanoseconds;
do NOT multiply that CPU delta by mach_timebase_info again. A standalone
100-ms native probe on this M2 Pro reports host 110045125 ns, CPU and GPU
deltas both 110018333, ratio 1.0. An earlier exploratory expression that
multiplied by the 125/3 Mach timebase was invalid and is not used in code.
Reference: https://developer.apple.com/documentation/metal/converting-gpu-timestamps-into-cpu-time

Next measurement prerequisite: cover a complete large guest batch using a
bounded set of small native counter buffers (rather than exceeding Apple's
per-buffer limit), paired-clock calibration around the sampled batch, and
report coverage. Keep one sampled batch in flight and the existing low
cadence. Then compare total stage durations and gaps with that same batch's
GPUStartTime/GPUEndTime. Only after this coverage is established choose between
shader cost, load/store churn, and synchronization changes. >=20 FPS and the
other full-goal verification requirements remain open.

### 2026-09-07 - Whole-batch profiling and selection bias (run K)

Implemented sixteen bounded 1024-timestamp pages (128 KiB total), one in-flight
lease, page-local encoder attachments, paired-clock calibration and interval
union coverage. Sample acquisition now precedes the first encoder, including
upload prefixes. No change to draw order, encoder reuse, submission or waits.
Full release video_core under Metal validation: 1763 passed, 3 ignored;
GUI build/bundle succeeded without warnings (whole-batch logs in /tmp).

Run K reached the same Lobby (after07-resumed.png). Fifteen status samples at
05:32:44..05:33:13 UTC: median 7.93854 game FPS. Normal exit at 379 seconds,
peak footprint 7.886 GiB, no swap growth, disk free +4.8 MiB. Logs:
/tmp/lm3-0907k-app.log and /tmp/lm3-0907k-resources.csv.

Invalidated measurement assumption: a varied wall-clock cadence alone does
not avoid sample-selection bias. Of the first 261 completed sampled batches,
none exceeded 14.87 ms even though submission peaks in gameplay exceed 100 ms.
In the Lobby it repeatedly selects a single tiny blit. Complete coverage of a
selected batch does not imply representative selection of expensive batches.
CPU recording bursts after waits are the suspected source of this bias.

Next refinement rotates the selected batch ordinal (0..16) after the minimum
one-second interval. Selection is still before the first encoder. Skips do not
advance while the lease is in flight. This is diagnostic-only, not a rendering
optimization. Regression test covers ordinal rotation and in-flight exclusion.
Rebuild/live validation pending; the 20-FPS objective remains unmet.

Ordinal refinement verification: 1764 release video_core tests passed, 3
ignored, Metal API validation enabled; GUI build/bundle succeeded without
warnings. Logs /tmp/ruzu-batch-ordinal-{full-tests,gui,bundle}-20260907.log.

Run L must NOT be used as a performance baseline. It selected complete large
batches successfully, but at 05:38:36 UTC the application reported GPU recovery
(InnocentVictim, command-buffer error status 5) during scene loading. Subsequent
captures stayed black and frame rate was about 3 FPS. The 298..328-ms samples
with ~1276 render and 838 compute encoders were AFTER that recovery. They do
not establish a new bottleneck or a profiling-overhead ratio. Cause of the GPU
error is not established. The first screenshot request was during loading;
next control run avoids capture until loading has settled.

Run L logs /tmp/lm3-0907l-app.log and resource CSV; normal stop at 229 seconds,
peak footprint 8.327 GiB, no swap growth, disk delta -9.52 MiB. Control run M
uses the identical release binary with stage profiling disabled (submission
timing stays enabled). No shader/cache/library changes between the two runs.
Do not silently discard this failed validation when resuming the investigation.

Control M: stage counters disabled, same binary/config/cache and seven A inputs.
Lobby visible in /tmp/lm3-0907m/lobby-control.png; no GPU recovery/error.
Fifteen samples 05:43:24..05:43:53 UTC: median 7.91150 FPS (range 7.84289..
10.97668), guest submission peaks about 130 ms. Resource peak 8.344 GiB,
no swap growth, normal exit at 178 seconds. This restores the prior baseline;
one successful control alone does not prove stage counters caused run L's
failure. Run N repeats full profiling without screenshots during loading.

Apple's feature-set limits were checked again: maximum counter sample buffer
length 32 KiB, maximum number of sample buffers 32 on Apple families. Current
profiler owns 16 buffers of 8 KiB, not one oversized buffer. This does not by
itself rule out an instrumentation bug. Reference:
https://developer.apple.com/metal/limits/

### 2026-09-07 - Valid complete gameplay batches (run N)

Repeated the instrumented build, same config/save, 40-second initial delay and
seven A presses >=7 seconds apart. No screenshots during loading. Capture
/tmp/lm3-0907n/lobby-confirmed.png verifies the same Lobby as control M.
No GPU recovery, synchronization failure or panic. Fifteen status samples
05:46:51..05:47:19 UTC: median 7.91063 FPS, range 7.84294..8.93538. This is
effectively the same as control M's 7.91150 FPS; no speedup claim. Run L's
failure is not consistently reproduced by enabling the stage profiler.

Six complete gameplay batches (ticks 10413,11168,11409,11858,12163,12398)
have median command duration 125.198 ms, interval union 88.258 ms and
uncovered duration 37.030 ms. Median largest individual gap is 1.270 ms.
Every measured stage is present, omitted=0, eight timestamp pages suffice.
Stage-sum medians: blit 1.033 ms, compute 4.683 ms, vertex 30.563 ms,
fragment 59.183 ms. Stage sums overlap: do not sum them into utilization or
attribute all fragment time to shader ALU rather than attachment traffic.

Concrete tick 10413: 127.512 ms, covered 91.515 ms, 235 blit / 838 compute /
1275 render encoders. Compute calls include 635 conditional argument masks,
83 primitive assembly, 37 condition resolves, 268 index conversions, one guest
compute and one geometry-vertex call. Recorded render breaks: 534 conditional
arguments, 255 index conversion, 36 condition resolve, one primitive assembly,
250 other. These are per-batch counts, not a claim that every batch is a frame.
The largest measured render pass costs about 4.07 ms in overlapping stage sums
(mixed 39-draw pass), not anything near the full batch's 127 ms.

Code cross-check: ConditionalRenderingArgumentsPass::resolve requests outside
render-pass context before GPU masking each draw's indirect arguments;
ordinary framebuffer descriptors preserve attachments with LOAD/STORE. Eden
uses a native conditional-rendering region instead. This makes repeated pass
breaks a concrete optimization candidate for tile-based Metal, but the 37 ms
outside timestamp intervals is NOT proven to be entirely load/store overhead.
No single long idle wait explains these batches, and no single shader is yet
shown to dominate. Next slice should preserve predicate-write/draw ordering
while reducing per-draw conditional argument preparation/pass breaks, with
GPU-written predicate and shader-side-effect tests before a live comparison.
Do not move conditional reads ahead of their producer or replace them with a
CPU readback/global wait. Keep render-persistence semantics intact.

Normal exit at 196 seconds; no emulator remains. Peak footprint 8.640 GiB,
swap unchanged, disk free +1.43 MiB. Logs /tmp/lm3-0907n-app.log,
/tmp/lm3-0907n-resources.csv and /tmp/lm3-0907n-fps.jsonl. Current code edits
remain diagnostic-only in this slice; no commit or push. The full correctness
regressions and >=20 FPS goal remain open.

### 2026-09-07 - Conditional direct-argument grouping implementation

Active performance slice after run N: batch up to 64 direct draw records under
the internal predicate produced by QueryCacheRuntime. CPU appends remain in
the same unsubmitted command buffer; compute/blit work (including reused
encoders), submission, predicate identity/offset/inversion or capacity changes
force a new group. Guest-addressable predicates and GPU-defined draw arguments
are not read early. The existing per-command masking path remains for them.
Private predicates cannot be modified by guest shader side effects. Normal
render attachment LOAD/STORE behavior is unchanged. No global wait/readback.

Affected ownership: metal_compute_pass.rs owns native argument grouping;
metal_query_cache.rs marks the private resolve predicate; metal_scheduler.rs
owns the mutation/submission token; metal_rasterizer.rs supplies direct words.
Tests compare actual vertex side effects and IDs in grouped/ungrouped draws,
including GPU predicate updates, and cover capacity/lifetime boundaries.
Full release video_core tests under Metal API validation passed (1767 tests,
3 ignored), and the release GUI was rebuilt/bundled without warnings.

Run O used the same configuration, save, seven A inputs and profiling flags as
N. The Lobby is confirmed in /tmp/lm3-0907o/lobby-confirmed.png. Fifteen status
samples 06:08:06..06:08:35 UTC give median 10.85697 FPS, range 10.78702..
10.97644, versus N's 7.91063 FPS: about 37% faster on this scene, not >=20 FPS.
No GPU recovery, synchronization error or panic was logged. The initial
fastmem page-size fallback and missing-avatar fallback are unchanged.

O's tick 12181 contains 235 blit / 303 compute / 739 render encoders, versus
N's representative 235 / 838 / 1275. Conditional-argument calls fall from 635
to 101 and their recorded render breaks from 534 to 0; the remaining groups
are prepared outside an already-active render pass. Index-conversion breaks
remain 255. All sampled stages are covered, with four pages rather than eight.
Over 63 complete gameplay batches from tick 12181 through 29775, median GPU
duration is 90.633 ms, covered interval union 75.311 ms, and uncovered duration
14.616 ms. Stage-sum medians are blit 1.041, compute 2.378, vertex 24.013,
fragment 54.103 ms; they overlap and must not be added as utilization.
These samples span a longer run than N's initial six-batch baseline.

Run O exited normally after 422 seconds. Peak footprint 8.169 GiB; swap
decreased 8 MiB and disk free decreased 26.70 MiB (no saturation). The run
was stopped before starting control P with stage counters disabled. No build
or GPU tests run concurrently with that FPS control. An additional regression
isolates predicate identity/offset/inversion changes without an intervening
GPU producer, so token changes cannot accidentally hide a missing key check.
Logs and resource/FPS series use /tmp/lm3-0907o-*; no commit or push.

Control P is invalid for a normal-performance comparison: at 06:11:36 UTC,
before the first screenshot, Metal reports GPU recovery/status 5 and query
synchronization failure. The later capture is black and runs at roughly
4 FPS. macOS gpuEvent-<unknown>-2026-09-07-081136.ips records a GPU
"progress timeout", and kernel logs confirm a global GPU restart. They do
not identify the responsible command or shader (process_name is unknown,
command_buffer_trace_id=-1); do not infer a specific culprit from the victim
error. This resembles the failure in pre-optimization run L, now reproduced
without stage counters: those counters are not necessary for the failure.
It does not establish that the new grouping is unrelated to all GPU hangs.
P exited normally at 177 seconds, and no emulator remains. Preserve its
logs/capture under /tmp/lm3-0907p-* rather than counting it as a successful
control. The isolated predicate-key test passes: different private buffers
hold opposite values, and six explicit GPU-readback expectations cover buffer
identity, offset and inversion changes without intervening producer work.
Expanded release video_core library suite: 1768 passed, 3 ignored under Metal
API validation, no warnings; log /tmp/ruzu-conditional-key-full-tests-20260907.log.
Only tests/docs changed after the GUI build used for O/P. Control Q repeats
P's flags and inputs, with no concurrent builds/tests. The same Lobby is
visible in /tmp/lm3-0907q/lobby-repeat.png. Fifteen samples at 06:18:17..
06:18:46 UTC give median 10.87932 FPS (range 9.95375..11.89295), consistent
with O's 10.85697 FPS with stage counters and faster than the prior ~7.91 FPS
controls. No GPU recovery, synchronization failure or panic in Q. Normal exit
at 199 seconds, peak footprint 8.121 GiB, no swap growth, disk free -23.28 MiB.
P's resource peak was 7.839 GiB, swap -8 MiB and disk free -2.66 MiB; memory
or disk saturation was not observed in that failing control either.

This validates the speedup on two correct Lobby runs, not general stability:
one of the three optimized launches still suffered a GPU timeout, and the
pre-optimization timeout remains unresolved. No emulator remains. Logs,
resource samples and FPS series are /tmp/lm3-0907q-*.
Final full-crate check `cargo test -p video_core --release` under Metal API
validation passes 1768 tests, 3 ignored, plus doc-tests (0 tests), without
warnings. Log /tmp/ruzu-conditional-key-crate-tests-20260907.log. No commit/push.

Next performance slice, once correctness is stable: distinguish the remaining
UInt8 expansion and quad-index conversions behind the 255 index-conversion
render breaks. Do not simply cache shader-writable buffers: write_tick cannot
distinguish multiple writes in one batch, and the current conservative bypass
is necessary for correctness. Any broader reuse needs actual producer/range
tracking, not fewer barriers or a CPU readback. The fragment stage still costs
about 54 ms summed across passes; this alone is not proof of shader-ALU cost
rather than attachment traffic. The >=20 FPS goal remains open.

### 2026-09-07 - Split index-conversion measurements

The previous index_conversion counter combined Uint8Pass and QuadIndexedPass.
Split the existing gated profiler into uint8_conversion (old slot 7) and
quad_index_conversion (new slot 8) without changing actual work or ordering.
The current common cache marks whole allocations with write_tick, so the
uncacheable UInt8 count does not establish writes to each requested subrange.
Do not broaden reuse based on that assumption. Measure the concrete source of
the remaining 255 render breaks before choosing the next optimization.
Full release tests pass (1768 passed, 3 ignored, doc-tests pass) under Metal API
validation; GUI release build/bundle succeeds without warnings. Run R confirms
the Lobby with no GPU error. Tick 13480: 268 UInt8 conversions, 255 render
breaks, zero quad conversions. This rules out quad conversion as the remaining
index-break source for this scene. Capture /tmp/lm3-0907r/lobby-index-split.png;
logs and resource CSV /tmp/lm3-0907r-*. Run stopped normally before editing.

Interrupted reuse slice / prerequisite: removing the permanent write_tick
exclusion requires notification of EVERY declared GPU write, not comparing
ticks. The common MarkWrittenBuffer already calls set_write_tick for graphics
and compute SSBOs, image buffers, transform feedback and ObtainCPUBuffer with
MarkAsWritten. Preserve that edge and base state, but expose the setter through
the backend-buffer trait so Metal can advance its content generation even for
repeated writes with the same tick. CPU uploads/native blits already advance
that generation. First verify generic dispatch and actual compute-write ->
convert -> compute-write -> convert ordering; only then change reuse policy.
Whole-allocation invalidation is conservative: no per-range reuse is claimed.

Prerequisite verified: the focused native generic-call test passes with Metal
API validation, demonstrating two increments for two common-cache writes in
one tick. The default setter still delegates to BufferBase; only Metal adds
derived-data invalidation. The next implementation now allows reuse within an
unchanged generation even after a historical GPU write. Added real compute
stores interleaved with conversions in one submission and checks that retained
older outputs keep their original indices; no CPU/blit writes can accidentally
make that test pass. Full release crate tests pass (1770 passed, 3 ignored,
doc-tests pass) under Metal API validation without warnings. GUI build and
live verification are pending.
Do not claim a speedup yet. The earlier permanent exclusion remains documented
above as the conservative policy before this notification prerequisite existed.

### 2026-09-07 - Run T: WindowServer watchdog and system panic

Live validation is NOT complete. The user reported a WindowServer failure during
run T. After reboot, no ruzu process remains and the /tmp/lm3-0907t resource log
is no longer available. Do not treat this run as a successful performance control.

System diagnostic reports confirm WindowServer watchdog failures at 08:44 and
08:45 local time. The 08:45:45 stackshot identifies ruzu PID 32679 (run T), with
a 5972.46 MB footprint, and the restarted WindowServer blocked on its main
thread in CoreAnimation Metal submission through IOGPUFamily and AGXG14X.
The ruzu main thread is also waiting in a graphics-driver call through libGL;
that stack alone does not identify the guest renderer or the initiating fault.
The panic report saved at 09:03:12 states that WindowServer missed checkins for
120 seconds after two induced crashes. Compressor and swap are reported OK.
This is not evidence of a memory-exhaustion panic, nor does it identify a faulty
shader, command buffer, or establish that the newest index-cache policy caused it.

Evidence is retained by macOS in /Library/Logs/DiagnosticReports:
- WindowServer_2026-09-07-084418_MacBook-Pro-le-Plubo.userspace_watchdog_timeout.spin
- WindowServer_2026-09-07-084545_MacBook-Pro-le-Plubo.userspace_watchdog_timeout.spin
- panic-full-2026-09-07-090312.0002.panic

Suspend further live GPU runs while investigating this failure. The resource
watcher guarded memory/disk usage but did not prevent a system-wide graphics
stall. Passing synthetic tests or one successful Lobby run does not establish
stability; prior intermittent GPU timeouts remain relevant, not invalidated.

Offline symbolication narrows the blocked call (no game or GPU tests launched):
the current bundled executable UUID 1CE09541-F6F5-3762-99EC-A0A6A6D81728 matches
the stackshot. With load address 0x1022b4000, the GPU-thread return address
0x1025f9218 resolves to MetalPresenter::present_texture +124. Disassembly at
unslid 0x100345218 identifies the return from the Objective-C commandBuffer
message, via the MTLCommandQueue::commandBuffer cached selector at 0x10186b060.
The preceding scheduler.flush and nextDrawable calls already returned. Thus
the observed wait is Scheduler::begin allocating a presentation command buffer,
not nextDrawable or an explicit waitUntilCompleted. The same stackshot shows
ruzu's Metal submission worker waiting in IOGPUFamily/AGXG14X.

At this snapshot ruzu's age is 156 seconds and the GPU thread last ran 124.538
seconds earlier: the wait began approximately 32 seconds after launch, before
the scripted first A at 40 seconds. This is a startup/presentation failure,
not a verified failure in the measured Lobby workload. Earlier rendering can
still have triggered the driver stall; the exact offending workload is unknown.

The queue is created with newCommandQueue; Apple documents a default limit of
64 uncompleted command buffers. Queue-capacity backpressure after stalled
submissions is a hypothesis, not a measured in-flight count. Increasing that
limit or adding device-wide waits is not a justified fix. The unified log from
08:43:00 through 08:44:20 supplies no specific GPU fault/command attribution.
Next investigation must distinguish uncompleted submitted work from allocation
or driver deadlock, retaining command/encoder identity before any subsequent
live reproduction. Do not describe a resource watchdog as protection against
GPU hangs: the system incident demonstrates that it is insufficient.

### 2026-09-07 - Offline-tested command-lifecycle journal

Added RUZU_METAL_COMMAND_JOURNAL=/absolute/path/to/new-file as an opt-in diagnostic
in the native scheduler. Use a persistent diagnostic directory, not /tmp, for
any future reproduction. The file must not exist: existing evidence is never
truncated. It contains 8192 fixed 512-byte records (4 MiB maximum), with monotonic
sequence numbers and elapsed CPU time. After wrapping, sort complete ASCII
records by their fixed-width seq field to recover chronological order; empty
slots are zero-filled. OS writes bypass userspace buffering but are not fsynced
per command. Recent records can be lost/torn in a kernel panic: this is not a
crash-proof storage or GPU recovery mechanism.

allocation_begin records the scheduler's pending list length and last observed
GPU tick BEFORE requesting a command buffer. allocation_returned identifies
the returned native object (zero means allocation failure). Submission events
include object, tick and kind (guest/presentation/external/synchronous), bracket
the commit call, and install a completion callback with the driver's terminal
status. Completion does not depend on the recording thread polling; it can
precede commit_returned. The callback retains only the journal, not the command
buffer or scheduler. No driver calls occur while holding the file mutex.
Pending list length is CPU bookkeeping, NOT a queried count of unfinished GPU
commands. The ring only retains recent history; an unmatched event at its start
may belong to an evicted prefix, not a leak.

Disabled by default; no new GPU waits, queue-size changes, resource ownership
changes or skipped work. Enabled I/O/callbacks can perturb timing and must not
be used for performance comparisons. This first slice attributes allocation,
submission and completion, not individual shaders/encoders or the root cause.
The four CPU-only journal tests pass in release without warnings; the full
video_core test target compiles. The full suite was deliberately not executed
because it runs native GPU work. Actual Metal callback execution and further
live validation remain pending. No GUI rebuild/relaunch was performed, keeping
the matching crash executable available for offline symbolication.

### 2026-09-07 - Allocation/dispatch bounds audit, no live reproduction

Reviewed primitive initialization/classification/prefix-scan/emission, patch
arguments, geometry vertex producer and geometry capture/replay sizing. Their
host allocations use checked products; even empty assembly writes its indirect
dimensions. No concrete out-of-bounds defect was established in those inspected
paths. This does not prove arbitrary guest shader execution safe or identify
the WindowServer failure's cause.

One local device-limit validation was missing: MetalDeviceProfile already reads
MTLDevice.maxBufferLength, but MetalBuffer::new_with_options did not consult it.
The allocator now rejects requested native allocations above that reported
limit before calling newBufferWithLength, with an explicit AllocationTooLarge
error. Existing minimum four-byte null storage, memory modes and in-range
allocation behavior remain unchanged. No clamping, fake capacity, new GPU wait,
or recovery claim is involved. In-range allocations may still fail normally.
The CPU-only boundary test covers exact limits, minimum storage and usize::MAX.
It passes in release without warnings; the complete test target compiles, but
only this CPU test was executed. Live GPU tests remain suspended;
permission for a diagnostic reproduction was requested after the system panic.

### 2026-09-07 - Diagnostic GUI prepared, not launched

Preserved the incident executable and WindowServer/panic reports outside /tmp:
../ruzu-diagnostics/metal-watchdog-20260907.ct1kH4/. The directory is 42 MiB and
also contains a current working-tree patch and untracked-source archive. These
source snapshots include the later journal/allocation validation, not solely
the pre-crash source state. The archived executable UUID remains
1CE09541-F6F5-3762-99EC-A0A6A6D81728 and its SHA-256 matches the old app exactly
(ebe41546bb5956d7b59de0cea05ba84e50e38e8457eba6b800900f19b7f41046).

Release GUI build and app bundling succeed without compiler warnings. The new
app UUID is DF206A61-5FD7-3E96-A42E-F3AD37CE4B2C; deep/strict codesign verification
passes. Bundled MoltenVK retains SHA-256
0995b17b030c01e991e2c36b48a953d8a4fdb6c4df1b9dcaa46b6d9e08612855.
Disk availability is still approximately 36 GiB. No emulator instance is active,
and the rebuilt app was NOT launched. Live diagnostic reproduction is awaiting
the user's decision about the risk of another system-wide GPU stall. The >=20
FPS and stable rendering completion gates remain open.

### 2026-09-07 - Authorized bounded diagnostic reproduction

After explicit user authorization, launched one release GUI instance with Metal,
the command journal and submission profiling; stage counters remained disabled.
Evidence is persistent in ../ruzu-diagnostics/lm3-watch-20260907.i3EfDz/:
app.log, monitor.jsonl, commands.bin, isolated config and renderer captures.
The input socket alone used /tmp. The isolated config was copied from the current
user config because the previous temporary config disappeared at reboot; this
is not a proven configuration-identical replay of run T.

The supervisor reached its 60-second cap, sent SIGTERM and escalated to SIGKILL
after three seconds. The process exited with -9 and no ruzu instance remained.
This was our deliberate termination, not an emulator crash. A presses were
acknowledged at approximately 40, 47 and 55 seconds. final.png shows the New
Features dialog, NOT the lobby; capture-request-after40s.png must not be treated
as a guaranteed pre-input capture despite its original temporary filename.

No WindowServer failure was observed. GPU completions and GUI status continued
advancing, including during the termination grace period. The 4 MiB journal
wrapped normally and retains 8192 recent records, with no status=5 completion;
the supervisor also checked completion errors throughout the run. Retained
events reach submission tick 6129. Uncompleted tail records after SIGKILL are
not evidence of a spontaneous hang. The only ERROR-level app log was the known
16 KiB host-page fastmem fallback to VirtualBuffer, not a Metal failure.

Peak sampled resident memory was 3.064 GiB (RSS, not total physical footprint or
GPU allocation). Global swap use went from 424.88 to 408.88 MiB. Maximum disk
availability decrease from the starting sample was 13.61 MiB. These results do
not reproduce memory/disk exhaustion and do not rule out a longer-run leak.
No new GPU fix was made based on this negative reproduction. Journal callbacks
now have live execution evidence, but the system crash is not declared fixed,
and these instrumented menu observations do not validate lobby FPS or >=20 FPS.

### 2026-09-07 - Command workload attribution prerequisite

The live journal showed progress but could not describe which work a command
contained. Added opt-in, constant-size CommandWorkload metadata owned by the
active guest command buffer. At flush it records blit calls, compute helper
categories (Other, Guest, GeometryVertex, PrimitiveAssembly, ConditionalResolve,
ConditionalArguments, VisibilityResolve, IndexConversion, QuadIndexConversion),
observed draws and first/last graphics stage hashes. Native object and tick join
these records to existing submission/completion events. No per-draw file writes,
GPU waits, installed-library changes or shader changes. Presentation/external
buffers do not steal/reset the guest summary. This is recording attribution,
not a complete shader history or evidence that a listed shader caused a fault.

Six CPU-only journal tests pass in release without warnings, including maximum
record widths, saturation and reset; full native GPU tests remain deferred.
The workload extension was added AFTER the bounded run above, so that run only
validates the original lifecycle journal. Build/live validation of the new
summary and same-lobby performance remain separate gates. DIFF.md updated.

The GUI release build now succeeds in 1m50s without warnings; app bundling and
deep/strict codesign verification pass. The rebuilt arm64 executable UUID is
4143CE24-DE40-3D6D-8790-1DEB3AAC1A24. Bundled MoltenVK retains the SHA-256 recorded
above. Build logs are workload-gui-build.log and workload-bundle-build.log in the
same persistent evidence directory. The new app was not launched in this slice.

### 2026-09-07 - Workload journal live, lobby reproduced without system failure

Two bounded GUI runs used the rebuilt app with lifecycle/workload journaling,
submission timing and stage counters disabled. No builds/tests ran concurrently.
The first (../ruzu-diagnostics/lm3-workload-20260907.wA5Qzf/) reached the Story
menu, not the lobby: seven presses at 40 seconds then seven-second spacing were
insufficient. Its late median 26.95 FPS is NOT a lobby result. Peak physical
footprint 7.675 GiB, swap unchanged, maximum disk drop 12.39 MiB. The supervisor
stopped it at 120 seconds, then SIGKILL after the SIGTERM grace period.

The second (../ruzu-diagnostics/lm3-lobby-20260907.QL1ZZo/) used eleven presses
at the same spacing, ending around 110 seconds. scene-90.png shows the Lobby
save selection; scene-110.png shows the real rendered lobby with Luigi, other
characters, staircases, lighting and geometry. scene-150.png shows a closer
camera, so later FPS must not be compared as a fixed-camera improvement.
At elapsed 110..130s (19 samples) median game FPS was 12.97198, range
11.88094..15.89974. At 150..161s the different camera gives median 14.90676.
Neither window meets >=20 FPS. This reproduces the earlier approximate 13 FPS
result, not a new speedup; all measurements include diagnostic overhead.

The retained journal has actual workload records and no status=5 completions;
the supervisor checked errors throughout both runs. Large late guest batches
have median 1722 observed draws, 236 blit-helper calls and compute calls
[0,1,1,77,34,65,0,67,0] in the category order above. This is not a dispatch or
render-break count. End-of-run submission timing reports guest peak buffers
around 74 ms and presentation peak below 0.5 ms. Full GPU-time sums can overlap;
do not call those sums device utilization or assign the cost to geometry alone.

Second-run peak physical footprint was 8.835 GiB, global swap unchanged and
maximum disk drop 30.20 MiB. Stopped deliberately at 160 seconds with SIGTERM,
then SIGKILL after three seconds; no ruzu remained. No WindowServer failure or
Metal error was observed. The fastmem page-size fallback is the only ERROR log.
This still does not prove the intermittent watchdog failure fixed. Physical
footprint now uses proc_pid_rusage v0 with layout verified against the installed
SDK, rather than treating RSS as physical/GPU memory. Persistent diagnostics
include the scripts, isolated configs, logs, journal and renderer PNGs.

### 2026-09-07 - Identity restart-prefix optimization under verification

The assembly workload warranted inspecting redundant prefix work before larger
cache or synchronization changes. assembly_initialize writes uint2(0) segments
whenever the packed restart-enabled field is zero (including array draws).
combine uses max(X) and sum(Y), so the entire prefix remains zero. Native
record_input_stream now skips only that identity scan; indexed enabled-restart
draws retain the existing multilevel scan, and vertex initialization, primitive
count compaction, patch assembly and emission are untouched. No guest-specific
case, resource cache, CPU readback or new GPU wait was introduced.
Added native input-stream coverage for 0/1/multigroup counts, arrays and 8/16/32
bit indices, enabled/disabled restart, signed base-vertex bit patterns and the
number of recording operations. GPU test execution is in progress; no speedup
or runtime parity is claimed yet for this change.

Verification completed: all eight native assembler tests pass, including the
new identity-scan input-stream test. The complete video_core release crate suite
then passes with Metal API validation enabled: 1778 passed, 3 ignored, doc-tests
pass (0), without compiler warnings. Logs are identity-scan-tests.log and
identity-scan-full-tests.log in the second-run persistent directory. GUI rebuild
is in progress. The measured lobby medians above precede this optimization and
must not be attributed to it. The >=20 FPS and broader visual/regression gates
remain open, as does the intermittent system-level GPU failure investigation.

Release GUI rebuild completes without warnings in 1m51s. Bundle creation and
deep/strict codesign verification pass; executable UUID is
9B99FD3C-4BFF-3895-AE32-724F871DAB6C and bundled MoltenVK SHA-256 is unchanged.
No emulator remains active. The updated app has not yet been used for a lobby
performance comparison; identity-scan-gui-build.log and
identity-scan-bundle-build.log preserve this build's results.

### 2026-09-07 - Identity scan control and render-break attribution

The updated GUI was subsequently tested in
../ruzu-diagnostics/lm3-scan-20260907.ASJFjp/: scene-110.png again shows the
rendered lobby. Median FPS at 110..130 seconds is 12.94359 versus 12.97198
before the change: no measurable gain. Later workload and camera differ, so
do not infer a regression from the larger recorded draw/conversion counts.
Peak physical footprint 9.643 GiB, swap unchanged, maximum disk drop 7.36 MiB.
The supervisor stopped the app deliberately at 160 seconds, not a game crash.

Stage profiling in ../ruzu-diagnostics/lm3-stages-20260907.jP5XGy/ stopped at
106.29 seconds on the unchanged 10 GiB physical-footprint guard (10.048 GiB).
Completions still progressed; no new driver failure was observed. Only the
90-second save-menu capture exists; there is no lobby screenshot for this run.
Batch 11922 reports command duration 75.08 ms, stage sums approximately
1.02 ms blit, 1.51 ms compute, 20.04 ms vertex and 47.40 ms fragment. These
overlap and may include stalls, not just shader ALU execution. Generic Other
render breaks are 256, versus 103 index conversions. A second batch has 254
and 105 respectively. No evidence yet attributes all Other breaks to uploads.

Added a distinct EligibleUpload render-break tag for precisely the Eden stream
buffer plus can_reorder_upload condition. Copies remain inline. An actual Metal
upload prefix requires shared upload/render retirement first; the interrupted
slice and verification gates are in METAL_UPLOAD_PREFIX_STATE.md. The current
work does not yet claim that reordering restores performance or system stability.

### 2026-09-07 - Eligible-upload measurement completed

Evidence: ../ruzu-diagnostics/lm3-eligible-20260907.dhgwvK/ includes supervisor,
analysis script, logs, renderer captures every 10 seconds from 90s and isolated
config. Release GUI UUID 563FC521-ADFB-3AC2-A1B1-3F7AD05F14CB, build without
compiler warnings, bundle deep/strict signature verified. Bundled MoltenVK
hash unchanged. Full release video_core tests: 1778 passed, 3 ignored, doc-tests
pass (0), Metal validation enabled. No GPU ordering change in this build.

scene-100.png and scene-110.png visibly show the lobby with characters,
staircases, portraits and lighting. At 110..130s median FPS is 12.93159
(19 samples, range 1.98012..14.94707). This is still approximately 13 FPS,
not >=20. Later animation changes make 130..150s median 14.85105 unsuitable
as evidence of a fixed-scene gain. Instrumentation/capture overhead is included.

For the 100..150s observed tick window, ten sampled batches with more than
100 counted render breaks have median Other=221, UInt8=67 and EligibleUpload
=23.5. Eligible counts vary 0..120 and constitute 11.81% of counted breaks
across those batches. These are operation counts, not weighted GPU cost or
all-frame statistics. The hypothesis that safe stream uploads explain the
entire generic category is invalidated; generic caller attribution is the next
measurement before a large scheduler rewrite. Do not remove necessary barriers.

Supervisor reached the 160s cap, then deliberately terminated the app with
SIGTERM followed by SIGKILL after 3s. No process remains. Physical footprint
peak 8.798 GiB; swap growth zero; maximum disk drop 10.18 MiB. No Metal error
or panic observed; the sole ERROR line is the known 16K/4K fastmem fallback.
The earlier intermittent WindowServer watchdog failure is not proved fixed.

### 2026-09-07 - Render-end source attribution and memory guard stop

Added bounded file/line/column attribution at the scheduler's actual Render
encoder end. Existing stage profiling controls it; no stack capture, per-draw
log, GPU ordering change or additional counter buffers. Up to 64 sites per
sampled batch, explicit overflow count, reset on completion. Counts also include
attachment changes and flush; they are not identical to the older work tags.
1779 release video_core tests pass with Metal validation (3 ignored, doc-tests
0), no compiler warnings. GUI UUID C7212D24-1D78-38E8-A13A-472121FECE73;
deep/strict bundle signature passes and bundled MoltenVK hash is unchanged.

Run evidence: ../ruzu-diagnostics/lm3-render-ends-20260907.CRyqm7/. Supervisor
stopped at 90.57s: physical footprint reached 10.614 GiB between 1s samples,
exceeding the unchanged 10 GiB stop threshold. Swap growth zero, disk drop
18.39 MiB. No 90s screenshot was requested before the guard; no lobby capture
or lobby FPS result exists. Do not compare this run's menu FPS with the earlier
lobby. SIGTERM then SIGKILL was our deliberate stop, not a game crash. No
remaining process, no logged Metal command error; known fastmem fallback only.

Source attribution is exercised, with omitted=0 in observed batches. Last
substantial pre-stop lot reports clear-helper starts, fragment barriers, texture
copies, buffer copies and conversions separately. This does not yet establish
the dominant cause in the hall. Do not remove fragment barriers: Apple's
WWDC22 "Go bindless with Metal 3" specifically disallows after-fragment in-pass
barriers on Apple GPUs; a simple substitution is not a valid optimization.
Reference: https://developer.apple.com/videos/play/wwdc2022/10101/

At the stop the existing allocation log reports about 6.6 GB Metal allocations
but only about 70 MB in the common buffer-cache allocation counter. This is a
scope difference, not proof of leaked bytes. Before another identical runtime
attempt, extended that same gated once-per-second diagnostic with staging-pool
capacity by upload/download/device-local usage, reusable/deferred subsets and
stream capacity. It uses observed completed_tick without polling or waiting.
This attribution is pending tests/build and has not yet measured the game.

Staging snapshot verification subsequently passed the complete release
video_core suite with Metal validation: 1780 passed, 3 ignored, doc-tests 0.
No compiler warnings. See staging-memory-tests.log in the run directory.
The GUI bundle still contains source-site attribution only, not this newest
staging snapshot; rebuild before the next measurement. No emulator remains.

### 2026-09-07 - Staging capacity measured; depth feedback identified

Rebuilt GUI UUID 29DDFBC2-4CEE-3E64-895F-52196CB111F1, release build without
compiler warnings, bundle signature verified, bundled MoltenVK unchanged.
Evidence: ../ruzu-diagnostics/lm3-staging-memory-20260907.aEEGUO/ includes
supervisor, analysis, isolated config, logs and renderer captures 70..110s.
The 110s capture visibly reaches the lobby with characters and geometry.
Median FPS 100..110s is 12.91158 (9 samples); 110..121s is 12.98641 (11 samples).
No speedup or >=20 FPS result. Supervisor stopped at its shortened 120s cap
(121.02s observed), followed by deliberate SIGKILL after SIGTERM timeout.
Physical footprint peaked at 9.114 GiB, swap unchanged, disk drop 18.52 MiB.
No Metal error or panic observed; no processes remain. This does not prove the
intermittent memory growth or WindowServer failure resolved.

Staging diagnostics rule out a multi-GiB staging cache in THIS run: upload
capacity peaked at 434.66 MiB, entirely reusable at that sample; device-local
capacity peaked at 0.570 MiB; download capacity was zero. The separate stream
is 128 MiB. Last upload capacity is 16.43 MiB. These are pool-owned capacities,
not resident bytes; other allocations still need attribution if growth returns.

Four sampled large lobby batches contain 596..605 actual render encoder ends.
Per batch: buffer-cache copies 185..189; depth feedback callback at
metal_rasterizer.rs:707 123..127; uint8 conversion 103..105; clear helper
39..42; fragment barriers 35; texture copies 29; other attachment changes
26..33; conditional resolve 30..36; primitive assembly 11. The earlier work
tag counter omitted depth-feedback ends and ordinary attachment changes, so
its total was not all render-pass ends. These remain event counts, not measured
time shares. Eligible uploads vary even within this run; no universal ratio.

Next correctness/performance slice: determine actual sampled/attached native
depth subresource overlap and effective depth/stencil writes for the feedback
draws. A base ImageId alias alone is insufficient to prove a native hazard.
Do not simply remove the callback or replace it with an after-fragment barrier.
Apple's WWDC20 guidance recommends sampling a separate snapshot when the
attachment is simultaneously sampled, rather than barriers. The native code
currently ends the prior encoder but binds views of the original image. Need
actual draw evidence and synthetic validation before changing this ownership.
Source: https://developer.apple.com/videos/play/wwdc2020/10631/

The talk's automatic compatibility snapshots only apply to apps built against
Catalina-era SDKs. vtool reports this executable uses SDK 26.5 (minimum OS 11),
so that legacy SDK workaround cannot be assumed to explain this app's memory
or performance. Read-only depth does not by itself justify omitting store actions
or ignoring sampled-attachment restrictions. Preserve clears and written contents.

### 2026-09-07 - Native depth-alias measurement, resource-limited run

Evidence: `../ruzu-diagnostics/lm3-depth-alias-20260907.NMx1Fj/`.
GUI release UUID D3008E07-BA4B-3406-8FCB-513C043D3D9A. Full video_core suite:
1781 passed, 3 ignored, with Metal API validation; build has no warnings and
bundle signature verification passes. MoltenVK is unchanged.

Added bounded alias metadata to the existing sampled-batch profiler. It follows
actual MTLTexture parent views to their root and reports levels/slices, native
types and the effective depth/stencil key. Disabled profiling does not traverse
views. No pass/barrier/cache behavior was changed.

This run was stopped at 101.04s by the existing resource guard: peak process
footprint 9.864 GiB, global swap growth 3209.44 MiB, free disk drop 2076.76 MiB.
SIGTERM followed by SIGKILL was our deliberate stop, not an observed crash.
No instance remains. The verified scene-80 capture is the save-selection menu;
there is no completed large hall sample and no valid hall FPS comparison.

Menu batches 6167, 7510 and 8741 contain native depth aliases of the same root,
mip 0 and slice 0. Bound texture is Type2D, attachment Type2DArray with one render
layer; native depth writes and stencil are disabled, but comparison is Less or
LessEqual (not Always). Therefore neither disjoint-subresource elision nor
simply detaching unused depth is justified by these observations. These are
bound-descriptor candidates, not proof of dynamic shader sampling, and must not
be extrapolated to all 123..127 hall feedback boundaries measured previously.

The next slice needs actual hall alias coverage under the same resource limits.
If it confirms overlapping read-only depth, investigate a versioned native
sampling snapshot and true write invalidation before trying to merge passes;
do not weaken synchronization or insert a fresh depth copy for every draw.
See METAL_DEPTH_FEEDBACK_STATE.md for the prerequisite boundary.

### 2026-09-07 - Cache retirement omission confirmed in source

MetalTextureCache::tick_frame never advanced any of the three sentenced-resource
rings, although DeleteImage/RemoveFramebuffers placed native owners into them.
This is an indefinite retention bug independent of LRU image selection. Restored
the ring/async/runtime/frame ordering and the caller's separate cache locks.
The native regression passes: all three rings retain resources until eight ticks,
then release their owners without invalidating previously recorded image copies.

Full release validation passes: 1782 tests, 3 ignored, with Metal API validation
and no warnings (`../ruzu-diagnostics/lm3-cache-retirement-20260907.N25XYP/tests.log`).
The GUI bundle has not yet been rebuilt for this change, and the previous swap
increase is not proven to be solely due to
the rings. The additional absent LRU/downloader path is recorded as an active
prerequisite in METAL_TEXTURE_GC_STATE.md, not treated as completed parity.

Download prerequisite update: Metal runtime download staging and native
converted D24S8 depth/stencil plane readback are implemented. Tests verify two
array layers, mip 1, both guest D24/S8 layouts and offset guard preservation.
Full release suite: 1783 passed, 3 ignored, Metal API validation enabled, no
warnings. No game run or GUI rebuild in this slice; guest packed reconstruction
and the live GC downloader remain the next required work, so no FPS or memory
improvement is inferred from these native transfer tests.

Packed reconstruction update: the D24S8 runtime readback now reconstructs both
guest packing orders from native float-depth and stencil planes after explicit
CPU-readback completion. All 16,777,216 integer depth values roundtrip through
the upload normalization and download quantization. Native GPU roundtrip,
mips/layers/offset guards and padded CPU output are tested. Full video_core
release suite: 1785 passed, 3 ignored, Metal API validation enabled, no warnings
(`../ruzu-diagnostics/metal-depth-pack-20260907.oA31lQ/tests.log`). The GUI remains
unrebuilt for this slice. Other converted downloads and live GC wiring remain
prerequisites; no new runtime memory or FPS claim is made.

### 2026-09-07 - Rebuilt GUI Retirement and Hall Check

Release build completed in 1m51s without Rust warnings. Repackaged and verified
the signed bundle, UUID 6E029517-B6CE-3F32-8487-B63BC55D7975; bundled MoltenVK
SHA256 remains 0995b17b030c01e991e2c36b48a953d8a4fdb6c4df1b9dcaa46b6d9e08612855.
Evidence: `../ruzu-diagnostics/lm3-retirement-check-20260907.EJCjDT/`.
Scene-110 confirms the hall. The 110..120s FPS median is 13.96, nine samples;
the >=20 FPS requirement remains unmet. Peak footprint 8.98 GiB, global swap
growth 2512 MiB, maximum disk-space drop 2053 MiB. Stopped at 120s by the harness,
SIGTERM then SIGKILL, no spontaneous crash or surviving instance.

Four fully sampled hall batches establish real mip/layer depth aliasing and
596..606 render encoder endings, with 74.1..83.9ms GPU command duration.
Depth writes are disabled in the observed aliases, but stencil is sometimes
enabled and depth comparison remains active. This is not proof of read-only
stencil or of dynamic shader accesses. See METAL_DEPTH_FEEDBACK_STATE.md before
changing any feedback boundary. No barrier was removed and no performance gain
is established by this run.

User clarification: 20 FPS remains a performance target, not an assumed
hardware guarantee. Eden's user-observed approximately 10 FPS is not a measured
upper bound for native Metal. Prioritize correct rendering and demonstrated
improvements without unsafe synchronization shortcuts or higher resource caps.

### 2026-09-07 - Read-only Depth/Stencil Alias Evidence

The rebuilt release GUI, UUID C42CA4BC-68D8-34AC-9EEB-39E666346223, reached the
hall with detailed bounded alias metadata. Four complete batches each contain
149 alias bindings, all fragment-stage slots 0/1 and Depth32Float_Stencil8,
17 shader hashes, no omitted keys. All have depth writes disabled and either
disabled stencil or Keep on all enabled stencil outcomes. This establishes
fixed-function read-only attachment state for the observed aliasing draws,
not for intervening producers and not proof of dynamic shader accesses.
Evidence: `../ruzu-diagnostics/lm3-stencil-detail-20260907.q2Wtg1/`, scene-110.png.
Full video_core suite: 1786 passed, 3 ignored, Metal API validation enabled.
No rendering policy changed. Median hall FPS 12.91, footprint peak 9.09 GiB,
global swap +472 MiB, disk-space drop 19 MiB. Harness stopped at the time limit;
no instance remains. The next native snapshot prerequisite is documented in
METAL_DEPTH_FEEDBACK_STATE.md; no unsafe barrier removal or gain is claimed.

### 2026-09-07 - Independent Native Depth Snapshot Primitive

MetalImage can now allocate and record an independent single-sample 2D
depth/stencil copy, without CPU readback or a global wait. Native tests preserve
the old depth and stencil after an ordered overwrite of the original, for
both D24S8 guest layouts, mip 1 and two layers. Full video_core release suite:
1786 passed, 3 ignored, Metal API validation enabled, no Rust warnings.
This is a prerequisite, not an enabled draw optimization: producer invalidation
and allocation accounting remain documented in METAL_DEPTH_FEEDBACK_STATE.md.
No GUI rebuild/run in this slice and no runtime performance claim.

### 2026-09-07 - Native Write Revision Prerequisite

Metal images now have an allocation-local saturating content revision, separate
from common guest dirty flags and native/slice authority. Native uploads,
copies/resolves/render blits, writable shader-image bindings, clear paths and
draw attachments participate. Effective depth/stencil state prevents read-only
draws from advancing the depth revision merely because the attachment is bound.
This remains conservative for conditional/culled/partial writes and is not a
GPU completion fence. Snapshot identity must include the owning allocation.
Full video_core release suite: 1789 passed, 3 ignored, Metal API validation,
no Rust warnings. The preserved snapshot/native overwrite test also checks a
revision change; tests cover masks, both stencil faces and saturation.
Snapshot retention, accounting and descriptor rebinding remain the next slice.
No render-pass boundary removed, no GUI rebuild and no new FPS claim.

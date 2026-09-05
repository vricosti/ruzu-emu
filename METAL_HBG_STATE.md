# Metal performance and image parity investigation

## Objective and order

Reach at least Vulkan performance and rendering quality in Harbinger gameplay,
then investigate Dark Souls black images in gameplay. Include the
D32FloatS8Uint / R32G32Float reinterpretation. Menu FPS is not acceptance evidence.

## Verified on 2026-09-05

- Branch: dev/metal-hbg-performance, base 2c0696a2. Changes are not committed.
- The CLI -r option previously selected the SDL window but did not update the
  setting consumed by CreateGPU. Fixed before System/window initialization;
  11 release ruzu_cmd tests passed. Benchmarks must use this fixed executable.
- Metal reached the first mission, Grave of the Forgotten 1. Window-specific
  capture: /tmp/hbg-metal-game1-field.png. FPS remained approximately 1.99,
  with approximately 480 ms frame times (/tmp/hbg-metal-game1.perf).
- The preceding camp sample (/tmp/hbg-metal-game1.sample) has 3668 of 3726 GPU
  thread samples in Fermi2D -> SoftwareBlitEngine::blit. Most are in the
  format converter, with substantial allocation/free overhead per pixel.
  This sample is the camp, not a separately sampled mission frame.
- At the initial measurement, Metal did not implement accelerate_surface_copy
  or the Params::blit_image backend hook. Vulkan routes these through
  CommonTextureCache::blit_image. This confirmed structural gap is now fixed
  in the working tree; the original CPU-hotspot evidence remains valid for
  the old executable, not evidence of the new executable's performance.
- ConverterImpl upstream uses stack storage. Rust allocated a Vec per pixel
  in both directions. Replaced with four stack words (all supported formats
  are at most 128 bits; constructor checks that invariant).
- RGB_TO_SRGB_LUT had 226 incorrect entries, including an endpoint of 0.265
  instead of 1.0. Copied Eden's exact literals. The reverse table already
  matched. Ten converter tests pass, including full-width and sRGB regressions.
- Native D32S8 transfers and RG32 reinterpretation are implemented using
  separate Metal depth/stencil aspect blits and integer compute packing.
  Three GPU tests cover bit patterns, mips, array layers, partial regions,
  nonzero buffer offsets and pitched rows. All 13 texture-cache tests pass
  with MTL_DEBUG_LAYER=1. Full video_core release suite after converter fix:
  1646 passed, 0 failed, 1 ignored. This does not validate Dark Souls yet.

## Benchmark validity

- The original Metal/Vulkan reference runs used the CLI-fixed executable
  without the new D32S8/converter changes. The current raw ruzu-cmd release
  executable now includes D32S8, converter and native Fermi2D changes
  (/tmp/hbg-native-blit-release-build.log, successful 50.62-second build).
  The GUI bundle has not been rebuilt in this slice.
- Isolated optimized 1280x720 RGBA8 converter benchmark, 30 round trips, two
  alternating runs with no concurrent build/emulator: original 59.798/60.017
  ms, current 20.329/20.513 ms, identical checksum 468172800. This is about
  2.94x faster in the converter, not a claim about game FPS. Temporary harness
  sources/binaries: /tmp/hbg_converter_{original,current}_bench{.rs,}.
- Vulkan game2 was started with the same temp config and bundled MoltenVK,
  but the desktop locked during navigation. Window and region capture both
  failed; IOConsoleUsers reported CGSSessionScreenIsLocked=true. Its roughly
  60 FPS cannot be treated as a matched gameplay measurement.
- That Vulkan process was deliberately terminated (SIGTERM), not a crash.
- Do not change MoltenVK versions. The raw Vulkan executable needs
  LIBVULKAN_PATH pointing at target/release/ruzu.app/Contents/Frameworks/libMoltenVK.dylib.
- Common temp config: /tmp/ruzu-harbinger-benchmark.ini, audio Null, native
  resolution, 100% speed, async shaders off, same settings for both APIs.
- External test-only SDL harness: /tmp/hbg_sdl_inputs.c and .dylib. Launch with
  DYLD_INSERT_LIBRARIES and a unique RUZU_DIAGNOSTIC_INPUT_FIFO path. Lines are
  SDL scancode + hold milliseconds. A=4, L=9. It only pushes SDL key events;
  no guest-memory edits or production instrumentation.
- Navigation verified visually: title Continue (A), camp Campaign (L), Quest
  (A), Grave of the Forgotten 1 (A). Slow Metal needed 1000 ms presses.
  Validate each screen; do not infer mission entry from button delivery.

## GPU Fermi2D acceleration: implemented, gameplay gate pending

The following prerequisites are now implemented before enabling FRAMEBUFFER_BLITS:

1. Extended MetalBlitHelper with the runtime blit operations, matching Eden
   blit_image.h/.cpp ownership: fixed nearest/linear samplers, color region
   blits, depth/stencil blits, MSAA color and depth/stencil resolve variants.
   Existing blit_color_with_sampler serves DrawTexture, not the full runtime.
2. Preserved source and destination mip/layer selection and region orientation;
   tested partial rectangles, flips, filtering and unchanged destination pixels.
   GPU validation initially rejected a 2DArray depth view bound to a 2D shader.
   MetalImageView now lazily owns separate 2D/2DMS aspect views for blits,
   retaining the existing array views for framebuffer attachments.
3. Threaded the helper into MetalTextureCacheRuntime (stable boxed ownership,
   as the rasterizer already does for scheduler/staging). Avoid an independent
   CPU-address surface map or bypassing the common image cache.
4. Implemented Params::blit_image using common framebuffer/view IDs and runtime
   methods. Keep prepare_image, alias/storage synchronization and GPU-modified
   tracking in common upstream order. No silently successful missing cases.
5. Connected Rasterizer::accelerate_surface_copy with the texture-cache mutex held.
   Do not copy-and-paste GetBlitImages or image-address lookup into rasterizer.
6. STILL REQUIRED: rebuild release, compare the same first-mission spawn Metal/Vulkan; capture
   images and sample both, with no parallel build or second emulator running.
7. STILL REQUIRED: only after the Harbinger gate, run Dark Souls into gameplay and investigate
   remaining black images; do not assume the D32S8 tests prove that game fixed.

Full per-resource Metal cache download policy and generalized rescaling remain
outside the completed D32S8 transfer slice; stop and implement them first if
the Fermi2D port needs them. Do not hide those dependencies behind no-op hooks.

### Verification and next execution

- /tmp/hbg-blit-native-all-tests2.log: 144 Metal tests pass with
  MTL_DEBUG_LAYER=1. Includes exact R32Sint/R32Uint words; partial mip/layer
  color and depth/stencil copies; point/linear filtering; and independently
  inspected per-sample depth/stencil values before sample-zero resolve.
- A real CommonTextureCache::blit_image test uses GPU-owned source data and a
  distinct zero-filled destination with no CPU memory backing. It checks the
  resulting pixels, not just the return value, proving the new Params hook
  performs the copy without the software fallback.
- Full video_core release validation completed: 1652 passed, 0 failed,
  1 ignored (/tmp/hbg-video-full-native-blit.log). The new release executable
  includes this Fermi2D slice; /tmp/hbg-release-build.log predates it.
  Neither the full-test log nor the new release-build log contains warnings.
- The macOS session is still locked (IOConsoleUsers check). Window capture
  cannot validate navigation or a matched gameplay scene until it is unlocked.
  No emulator is left running. An unlock request was sent to the user.
- Guards explicitly reject incompatible aspects/numeric types/sample counts,
  linear integer/depth filtering and non-SrcCopy depth/MSAA operations. Shader
  blits replace Vulkan blit commands because Metal's blit encoder cannot scale
  rectangles or filter. Stencil-only resolve is supported and tested natively.

## Unlocked gameplay retest and MSL scope failure

- Session capture works again. Metal game3 reached Grave of the Forgotten 1;
  /tmp/hbg-metal-game3-field2.png shows the initial field with the gameplay HUD
  and elapsed 00:14. /tmp/hbg-metal-game3.perf, seconds 140..180: 40 samples,
  mean 37.41 FPS, range 29.93..42.94, mean frame time 26.97 ms. This replaces
  the previous approximately 2 FPS bottleneck, but is NOT rendering acceptance.
- /tmp/hbg-metal-game3-field.sample has 1616/3419 GPU-thread samples inside
  newLibraryWithSource. The log has 5177 Metal draw failures with undeclared
  v_7_48/v_7_16 MSL variables. Failed builds are retried; this is not evidence
  of a broken cache hash or legitimate thousands of new successful variants.
- Vulkan game3 also reached the first mission and renders similarly dark
  foreground vegetation. /tmp/hbg-vulkan-game3-quest.png was captured after
  entry, despite its misleading filename. The enemy/camera state differed,
  and /tmp/hbg-vulkan-game3-steady.png is YOU DIED. Do not compare the whole
  run or its late 50 FPS samples against Metal's stationary spawn benchmark.
  The Vulkan sample is therefore not an accepted matched steady-state baseline.
- Both processes were deliberately stopped with SIGTERM. No emulator remains.
- New native compiler regression test reproduces a loop SSA value referenced
  after exit becoming an undeclared MSL identifier. It fails before the fix
  (/tmp/hbg-msl-scope-before.log). This proves a lexical-scope defect without
  relying on the game-specific variable numbers.
- Correction in msl_emit_context.rs: structured-program SSA locals have
  function-scoped declarations; assignments remain at their original program
  points. This follows the lifetime contract of Eden GLSL DefineVariables,
  without replacing the shared IR or implementing GLSL's register allocator.
  Straight-line programs retain inline declarations. No failed-draw cache or
  shader-disabling workaround is introduced.
- Full shader_recompiler release suite: 553 passed, 0 failed
  (/tmp/hbg-msl-scope-full.log). Full video_core with Metal validation: 1653
  passed, 0 failed, 1 ignored (/tmp/hbg-msl-scope-video.log), including the
  previously failing native compiler regression. Both logs have no warnings.
  Release rebuild completed in 56.17 seconds (/tmp/hbg-msl-scope-release.log).
  The raw executable includes the scope fix; the GUI bundle is still older.

### Actual game validation of the scope fix

- Metal game4 reached the same first mission. The log
  /tmp/hbg-metal-game4.log contains zero Metal draw failures and zero undeclared
  identifier errors (game3 had 5177). No failure-cache workaround was added.
- /tmp/hbg-metal-game4-campaign.png actually shows the mission at elapsed
  00:05 (filename reflects the intended navigation step, not the real scene).
  The later /tmp/hbg-metal-game4-field.png shows YOU DIED at 00:46. Hence late
  FPS and /tmp/hbg-metal-game4-field.sample are not a stationary-spawn baseline.
- This later sample has 2552/3376 GPU-thread samples in pop_wait, instead of
  the repeated MSL compilation seen in game3. No compile retry loop remains
  in the sampled stack. Remaining hot native code includes pipeline-key hash
  lookup; do not optimize that prematurely without a matched live-game sample.
- Important navigation issue: screenshots advanced farther than intended
  between tool calls. The test FIFO logs only A and L for game4, yet it reached
  the mission. A request to leave controls idle was sent to the user; the cause
  of the additional navigation has not been established. Do not infer matched
  scenes from the number of injected keys.
- The Metal process was deliberately stopped (SIGTERM), not a crash. No
  emulator remains. Next run should capture the window at short fixed intervals
  during navigation and the first 20 seconds of the mission, so a YOU DIED
  screen cannot silently contaminate the benchmark. Repeat identically for
  Vulkan with the same executable and bundled MoltenVK.

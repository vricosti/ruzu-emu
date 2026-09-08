# Metal applet capture and intermittent audio - 2026-09-08

## User observations

Movies screenshots at 11.21.37 and 11.21.55 inspected directly. First shows
Select a user with a black upper background; second shows a black Game 1
thumbnail while the surrounding save-selection scene renders correctly.
Intermittent audio crackles also reported. Do not assume a common cause.

## Confirmed missing prerequisites

RendererMetal::get_applet_capture_buffer returns TILED_SIZE zero bytes
unconditionally (renderer_metal.rs). SharedBufferManager::write_applet_capture_buffer
copies these bytes to guest-visible capture storage. This is a real missing
implementation, independent of the recent pipeline-cache optimization.

RendererMetal::composite_impl uses layers.iter().rev().find_map and presents
only the last resolvable framebuffer. It does not compose all display layers
or consume their blending/crop/transform metadata. MetalPresenter currently
draws one texture; capture must not be implemented by pretending this is a
complete multilayer composite.

Eden renderer_vulkan.h/.cpp reviewed: Composite calls RenderAppletCaptureLayer
before the window-visibility check. That method keeps a persistent 1280x720
capture image and draws the framebuffer layers. GetAppletCaptureBuffer copies
the image, waits for completion, invalidates mapped memory and swizzles BGRA
pixels into the guest block-linear layout. vk_blit_screen.cpp DrawToFrame
maintains one layer per framebuffer and delegates composition to window_adapt.

## Implementation ordering

This is a structural prerequisite slice, not a one-line replacement for zeros:
1. Implement native Metal multilayer composition, with upstream layer order,
   crop/transforms and Opaque/Premultiplied/Coverage blending. Read the matching
   window_adapt and layer header/implementation before coding.
2. Maintain capture texture ownership in RendererMetal, update on Composite
   before visibility checks, and preserve in-order GPU lifetime/synchronization.
3. Download on GetAppletCaptureBuffer, not synchronously every frame. Swizzle
   using capture constants and existing decoder; retain the valid no-frame
   zero return only before any capture exists.
4. Test distinct layers, alpha modes, crop/flip, retained-frame lifetime and
   block-linear round-trip. Validate user-selection background in the GUI.

The screenshot alone does not prove whether the save thumbnail uses this
capture path, another guest image path, or already contains black saved pixels.
Trace its producer and inspect only a copy of save data if needed. Do not erase
or overwrite the user's saves to make a new thumbnail for testing.

## Audio evidence and next measurement

Latest bounded-run log uses Cubeb audiounit-rust, 48000 Hz stereo, requested
latency 480 frames. No underrun counter was enabled; no underrun diagnosis yet.
SinkStreamBase::process_audio_out_and_render repeats last_frame when the queue
is empty. This can produce discontinuities but has not been correlated with
the reported crackles. Cubeb callback takes the shared stream mutex. Prior CPU
profiles show guest memory callback contention, not a measured audio deadline miss.

Measure per-stream late callbacks, actual missing frames and producer intervals
with counters outside the real-time logging path. Queue length alone is not
sufficient because playing_buffer may still contain samples. Compare without
GPU profiling/captures too; synchronous diagnostic readbacks can perturb timing.
Do not mask the issue with a larger latency or remove memory locks without proof.

No runtime code changed or rebuilt in this inspection. Existing performance
improvements remain intact. No new game instance launched.

## Implementation slice - 2026-09-08

The inspection above is historical. Native presentation now has a dedicated
present/window_adapt_pass.rs and present/layer.rs. Drawable acquisition remains
in MetalPresenter. The compositor renders cached layers in input order, using
the exact RGB/alpha blend factors from Eden present/util.cpp. Crop and flips
use the existing NormalizeCrop port and unscaled guest texture dimensions.
The fullscreen triangle is a native replacement for Eden's four-vertex strip;
its interpolated UVs implement the same screen rectangle mapping.

RendererMetal owns a persistent 1280x720 BGRA8 capture target, composed before
the visibility check. It is submitted asynchronously on the same Metal queue.
Only GetAppletCaptureBuffer allocates a download buffer and waits, then returns
block-linear pixels using Capture constants. Screenshot composition uses all
resolved layers too, not just the top layer. Capture uses bilinear/no AA as
PresentFiltersForAppletCapture specifies.

Native tests verify all three blending modes including output alpha, cropped
and doubly flipped texels, upload ordering, persistent capture identity, latest
capture contents, no-frame zeros and swizzle round-trip. Release video_core
with Metal validation: 1827 passed, 4 ignored. No audio behavior changed.

Remaining scope: CPU/raw display surfaces absent from the texture cache still
lack Eden Layer's staging/raw-image fallback. Existing display scaling/AA
settings beyond bilinear are also not ported. These are not substitutes added
by this slice; do not claim complete presentation parity. Full generic raw-image
support needs its own resource/tick lifecycle port rather than silent dummy data.
The runtime applet/background and existing save-thumbnail observations remain
to be rechecked in the rebuilt GUI; black pixels previously persisted in a save
are not repaired by producing correct future captures.

## GUI validation and saved-thumbnail diagnosis

Release GUI UUID 530AA157-C72B-3B92-94E7-79624F3AE4AA, signature verified.
Build /tmp/metal-applet-gui-build.log, bundle /tmp/metal-applet-bundle.log.
Bounded run: ../ruzu-diagnostics/lm3-applet-capture-20260908.e5eGWl.
scene-45.png shows Select a user with the captured blurred game background,
instead of the black upper area in the user's screenshot. scene-65.png still
shows the black saved thumbnail, independently of the corrected applet capture.

Read-only inspection of title 0100DCA0064A6000's save confirms LM1Athumb.jpg and
LM1Rthumb.jpg each contain a JPEG after an eight-byte game header. Both decoded
images are 160x90, with RGB extrema ((0,0),(0,0),(0,0)): completely black.
Both files retain their September 6 09:22 modification time. An extracted
diagnostic copy saved-thumbnail.jpg shows the same black image. No save file
was edited/deleted. This proves the currently displayed thumbnail is backed by
black persisted data, not when or why that old data originally became black.
A future game-generated thumbnail still needs separate verification.

Run reached the hall; scene-115.png is a closer camera/animation than earlier
performance controls, so the late 28.42 FPS median is NOT a comparable speedup.
Peak footprint 7.116 GiB, swap growth 196.63 MiB, disk loss 27.12 MiB; no safety
limit triggered. Watchdog ended the run at 120s, exit -9 is its bounded kill,
not a spontaneous crash. No instance remains. Audio crackles remain unmeasured;
user cannot yet identify whether they occur in menus or only gameplay.

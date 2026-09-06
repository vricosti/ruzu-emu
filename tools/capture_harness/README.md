# Capture harness

`capture-harness` launches one executable, starts a monotonic timer immediately after
`Command::spawn`, and captures X11 reference images at absolute timecodes. It can record the same
window (or the complete screen) with `ffmpeg`.

## Build and run

```bash
cargo build --release -p capture_harness
target/release/capture-harness tools/capture_harness/example.toml --dry-run
target/release/capture-harness tools/capture_harness/example.toml
```

Paths in the TOML file are resolved relative to that file. A command consisting of a single name
is resolved through `PATH`.

`process.rom_path` is mandatory and is always appended to the executable arguments. Set the
optional `process.rom_arg` when the executable expects a flag immediately before the path (for
example `rom_arg = "-g"`). Keep renderer and other ordinary switches in `process.args`.

## Clean start

The optional `[cleanup]` table removes explicitly listed shader-cache and save-data paths before
the target process is spawned:

```toml
[cleanup]
shader_cache_paths = ["/absolute/cache/path/for-one-title"]
save_data_paths = ["/absolute/save/path/for-one-title"]
```

Relative cleanup paths are resolved from the TOML file. No glob or environment-variable expansion
is performed. `--dry-run` validates and displays the configuration without removing anything. The
harness refuses broad or ambiguous targets, including the filesystem root, the home/current
directory, duplicate or nested cleanup paths, and any path containing the executable, ROM,
configuration, log, working directory, or capture output. Existing symlinks are removed without
following them. Cleanup results are recorded in `capture-manifest.json`.

The X11 dependencies are `xdotool`, ImageMagick's `import`, and (when video is enabled) `ffmpeg`.
XWayland windows also work when they expose an X11 window ID. Native Wayland capture is deliberately
not approximated because compositor permission dialogs would make the timing non-reproducible.

## Timed controller buttons

The optional `[input]` timeline sends logical Switch buttons to the emulation window. Before the
target is launched, the harness temporarily switches player 1 to Keyboard/Mouse in the frontend's
own configuration file:

- `ruzu-cmd` uses `sdl2-config.ini` and SDL scancodes (`A=A`, `L=F`, `R=H`);
- `reden` uses `qt-config.ini` and Qt key codes (`A=C`, `L=Q`, `R=E`).

The frontend is inferred from `process.executable`. `input.config_file` can override the path; this
is also useful with an isolated configuration. Unless `restore_config = false` is requested, the
original file is restored when the harness run ends, including error paths.

```toml
[input]
default_hold_ms = 100
restore_config = true

[[input.events]]
at = "00:00:12.000"
buttons = ["l", "r"]
label = "L+R"

[[input.events]]
at = "00:00:14.000"
buttons = ["a"]
```

Supported names are `a`, `b`, `x`, `y`, `l_stick`, `r_stick`, `l`, `r`, `zl`, `zr`, `plus`,
`minus`, `d_left`, `d_up`, `d_right`, and `d_down`. Buttons listed in one event are held
simultaneously. `hold_ms` can override the default for one event. Press and release are independent
absolute timeline events and are both recorded in `capture-manifest.json`. Each input event activates
the emulation window before using XTEST keyboard injection, so the harness intentionally takes
keyboard focus while an input timeline is active.

## Timing contract

- The timer origin is immediately after the target process has been accepted by the OS.
- Every event waits for an absolute offset from that origin, so one slow screenshot does not shift
  all later screenshots.
- Window discovery occurs while that timer is already running. Choose capture/video timecodes later
  than `window_wait_timeout` when startup time is uncertain.
- `capture-manifest.json` records scheduled time, actual time, and lateness for every event.
- At equal timecodes, the order is video start, input release, input press, screenshot, video stop.

`target = "window"` finds the largest visible window matching the launched PID and optional title
regular expression. Set `match_process_pid = false` when a launcher creates the actual window in a
different process, or provide a fixed `window_id`. `target = "screen"` captures the complete X11
display.

By default the launched process is terminated after the final timeline event. Set
`terminate_after_timeline = false` to leave it running until it exits naturally.

## Record and replay physical input (Linux)

The separate `[session]` mode records an explicitly selected evdev keyboard or gamepad and replays
it through a uinput virtual device. It does **not** change the emulator's controller configuration,
grab the physical device, or send XTEST cleanup keys. It cannot be combined with `[input.events]`.

List devices first (this does not record anything):

```bash
target/release/capture-harness --list-input-devices
```

Add this table to a launch configuration, or adapt [input-session.toml](input-session.toml):

```toml
[session]
mode = "record"
device = "/dev/input/by-id/your-controller-event-joystick"
file = "recording/input.json"
duration = "00:05:00"
```

Grant your user read access to **that device**; recording does not require write access. Avoid
running the entire emulator as root or granting everyone access to all input devices. Keyboard
recordings can contain private text: only the selected device is read, for the configured duration
(maximum one hour). Release buttons and leave axes at rest until the launch timer starts. Ctrl-C,
the duration limit, or target exit ends recording. The JSON file is created with private permissions
and is never overwritten. Its parent must exist (the capture output directory is created automatically).

To replay, remove `device` and `duration`, and change the table to:

```toml
[session]
mode = "replay"
file = "recording/input.json"
```

Replay requires read/write access to `/dev/uinput`. It advertises the recorded device name, ID,
buttons and axis ranges so existing SDL mappings can be reused. The virtual device is created before
launch, but the harness does not select it inside Ruzu. If the physical controller is still connected,
SDL can distinguish it as a second instance: select the virtual controller or disconnect the physical
one before replay. For keyboard replay, focus the intended window; uinput is system-wide, not
window-addressed. Do not type or operate another application during replay.

The session format is versioned JSON, with `CLOCK_MONOTONIC` timestamps in microseconds, complete
`SYN_REPORT` packets, press/release/repeat events, relative movements and absolute axes. Initial axis
state is saved; initial held buttons are rejected. Lost kernel events (`SYN_DROPPED`), disconnection,
unsupported multitouch devices or an incomplete packet invalidate the recording rather than silently
producing an unreliable replay. Output/metadata events (force feedback, LEDs, scan-code metadata)
are not replayed. Replay releases every virtual button and restores initial axes on normal completion,
cancellation and Rust-controlled error paths, then destroys the virtual device.

Input runs in its own worker, sharing the launch origin with screenshots and RenderDoc. Slow image
capture cannot delay it, although OS scheduling can: `input-replay.json` records scheduled/actual
microseconds and lateness for every packet. Use a **new capture output directory for each replay**;
existing input/RenderDoc reports are not overwritten. With `capture.times = []` and video disabled,
raw sessions need neither X11 nor `DISPLAY`. The harness does not override `GDK_BACKEND`.

Replay is timed input, not a save state or a deterministic game script. Match the starting save,
configuration and scene. Different loading durations or menu states can change the result. Interactive
pause/resume, visual checkpoints and automatic SDL device selection are not implemented in this version.

## RenderDoc on the same timeline (Linux)

The optional bridge is a small C shared library built against your RenderDoc SDK header. This keeps
the application API layout defined by the official header rather than a handwritten Rust ABI struct.
The rest of the harness, including evdev/uinput, is Rust. No files from `.agents` are needed.

```bash
cc -std=c11 -Wall -Wextra -Werror -shared -fPIC \
  -I /path/to/renderdoc/include tools/capture_harness/rdc_trigger.c \
  -o /path/to/rdc_trigger.so -ldl -pthread
```

```toml
[renderdoc]
library = "/path/to/renderdoc/lib/librenderdoc.so"
helper = "/path/to/rdc_trigger.so"
# Recommended for Vulkan: SDK manifest, even if its embedded library path is stale.
vulkan_layer_manifest = "/path/to/renderdoc/etc/vulkan/implicit_layer.d/renderdoc_capture.json"
capture_prefix = "replay-output/rdc/frame"
times = ["00:01:00", "00:01:40"]
frames = 1
timeout = "30"
```

The bridge requires RenderDoc application API 1.6.0 or newer. Library paths used by `LD_PRELOAD`
must not contain whitespace or colons. With `vulkan_layer_manifest`, the harness writes a run-local
manifest with the configured library path and enables it through `VK_ADD_LAYER_PATH` and
`VK_INSTANCE_LAYERS`; it does not change system layer registration. These variables follow the
[Vulkan loader's layer discovery rules](https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderApplicationInterface.md).
Without this option, Vulkan capture requires an already configured capture layer.

Captures target RenderDoc's **active API/window**, not necessarily the GTK top-level window. Check
that the overlay says Vulkan, not OpenGL ES; use RenderDoc's F11 selection if needed. The harness
does not infer the rendering API from the X11 screenshot window. Avoid manual F12 captures during
scheduled captures. Each request waits for completed files reported by RenderDoc, not merely an
accepted trigger. Errors, timeouts and filenames appear in `renderdoc-manifest.json`.

Capture times must be separated by at least `timeout`. The timeline remains alive until the last
request's timeout budget has elapsed, so `process.stop_at`, if specified, must be no earlier than
that point. RenderDoc waits run independently of input replay. Large multi-frame captures can consume
substantial disk space; the default is one frame and the maximum per request is sixteen.

## Validation

```bash
cargo test -p capture_harness
cargo clippy -p capture_harness --all-targets -- -D warnings
RENDERDOC_INCLUDE=/path/to/renderdoc/include \
  CAPTURE_HARNESS_BINARY="$PWD/target/release/capture-harness" \
  python3 tools/capture_harness/tests/renderdoc_bridge.py
```

The tests use synthetic input sinks and a fake RenderDoc API. They do not inject input into your desktop,
launch a game, or access a GPU. Physical controller mapping and scene progression still need a manual
record/replay test on the intended emulator configuration.

# Current upstream parity debt

This file contains only active differences confirmed in the current source tree against
`~/Dev/emulators/zuyu`. Implementation history, diagnostics, commands, runtime logs, and audit
procedures are intentionally omitted.

## Kernel

### Unintentional differences (to fix)

- `src/core/src/hle/kernel/k_process.rs` still represents thread-local pages, thread ownership, and
  shared-memory ownership with Rust side vectors instead of upstream's intrusive kernel-object
  structures.
- `src/core/src/hle/kernel/k_worker_task_manager.rs` has the asynchronous queue but not upstream's
  `KernelCore` ownership and lifecycle.
- Light ports are not supported by `svc_port.rs` and `sm.rs`; affected requests are rejected
  instead of using upstream's light-session path.
- The exception SVC path does not notify upstream's reporter or debugger because those owners are
  not connected to `svc_exception.rs`.

## HLE services

### Missing items

- The following handlers are registered with `None` even though the matching upstream command
  table connects a real implementation:
  - `ldn/user_local_communication_service.rs`: `GetState`, `GetNetworkInfo`, `GetIpv4Address`,
    `GetDisconnectReason`, `GetSecurityParameter`, `GetNetworkConfig`, `AttachStateChangeEvent`,
    `GetNetworkInfoLatestUpdate`, `Scan`, `ScanPrivate`, `SetWirelessControllerRestriction`,
    `OpenAccessPoint`, `CloseAccessPoint`, `CreateNetwork`, `CreateNetworkPrivate`,
    `DestroyNetwork`, `SetAdvertiseData`, `SetStationAcceptPolicy`, `AddAcceptFilterEntry`,
    `OpenStation`, `CloseStation`, `Connect`, `Disconnect`, `Initialize`, `Finalize`, and
    `Initialize2`.
  - `btm/btm_system_core.rs`: gamepad pairing, radio control/event, connected/paired audio-device
    queries, and audio-device connection-rejection commands.
  - `acc/acc_su.rs` and `acc/acc_u1.rs`: `GetBaasAccountManagerForSystemService`; its upstream
    `IManagerForSystemService` prerequisite is not ported.
  - `am/service/application_creator.rs`: `CreateApplication`.
  - `am/service/application_accessor.rs`: `GetAppletStateChangedEvent`, `GetResult`,
    `RequestForApplicationToGetForeground`, `GetCurrentLibraryApplet`, `PushLaunchParameter`,
    `GetApplicationControlProperty`, and `SetUsers`.
- `hid/hid_server.rs` returns placeholder success/zero results for GameCube ERM and N64 boolean
  vibration commands instead of routing them through upstream's vibration-device objects.

## Network and web services

### Missing items

- `network` does not implement the ENet transport. Room creation/join, peer lifecycle, packet
  delivery, chat, moderation, and announcement loops remain local stubs.
- `src/web_service/src/web_backend.rs` has no HTTP client; generic web requests return a local error.
- Web telemetry submission is disabled in `src/core/src/telemetry_session.rs`.
- The LDN service has helper methods for a small subset of commands, but its IPC table remains
  disconnected as listed above and it does not own upstream's event and network lifecycle.

## Input and frontends

## 2026-08-09 — `ruzu/{src/game_list.rs,src/uisettings.rs,src/configuration/qt_config.rs,src/main.rs,i18n/catalogs.json}` vs `src/yuzu/{game_list.cpp,game_list_p.h,uisettings.h,configuration/qt_config.cpp}` and `dist/languages/*.ts` (`GameListFavorites`, `ToggleFavorite`, `AddFavorite`, `RemoveFavorite`, and `AddFavoritesPopup`)

### Intentional differences

- Qt represents Favorites with a `GameListFavorites` `QStandardItem` subclass and hides its row
  through `QTreeView::setRowHidden`. GTK has no hidden-row API for `TreeListModel`, so ruzu gives
  `GameEntry` an explicit Favorites kind and removes/reinserts that root at position zero. Its child
  store remains alive, preserving the same visible behavior and ordering. Synthetic collapse
  notifications emitted while that root is absent are ignored, and inserting the first favorite
  explicitly expands the new GTK row to reproduce Qt revealing its still-expanded hidden row.
- The upstream colorful-theme `folder.png` and `star.png` assets are embedded into ruzu rather than
  resolved from the host GTK icon theme. This preserves the upstream 48 px artwork while keeping
  ruzu independent of both the desktop theme and the zuyu source tree at runtime.
- Upstream incrementally clones or removes one `QStandardItem` row. Ruzu rebuilds the small Favorites
  child store from already-scanned immutable `GameEntry` metadata after each toggle; no directory is
  rescanned, and first-match/configured-id ordering remains identical.

The `favorites_expanded` setting is loaded, applied to the GTK tree row, updated on expansion changes,
and persisted under upstream's `UIGameList\\favorites_expanded` key.

### Binary layout verification

- Not applicable. The changed state is GTK frontend model data only.

## 2026-08-09 — `src/ruzu/src/game_list.rs` vs `src/yuzu/game_list.cpp` (`GameList::PopupContextMenu` and `AddGamePopup`)

### Intentional differences

- Upstream fully configures each `QAction`, including the checkable Favorite state, before
  `QMenu::exec` materializes and displays the menu. GTK resolves stateful `GMenu` rows through an
  action group, so ruzu installs that group and parents/styles the empty `GtkPopoverMenu` before
  assigning its menu model. This preserves upstream's single layout pass and avoids initially
  rendering Favorite as a stateless row before rebuilding it as a checkbox.

### Binary layout verification

- Not applicable. This only changes GTK context-menu construction order.

## 2026-08-09 — `src/ruzu/src/main_window.rs` vs `src/yuzu/main.{h,cpp}` (`GMainWindow::OnRestartGame`)

### Intentional differences

- Upstream calls `ShutdownGame()` and immediately continues to `BootGame()` after its Qt shutdown
  synchronization. The GTK frontend requests the same confirmed shutdown non-blockingly, retains a
  copy of `current_game_path`, and calls `boot_game` only after `LoadingEvent::StopComplete` has
  joined the emulation thread and released the native render target.
- A pending restart is discarded when teardown reports a failure or the application window is
  closing, preventing a shutdown callback from launching a new session behind an error or close.

### Binary layout verification

- Not applicable. This changes frontend action wiring and lifecycle state only. A focused regression
  test verifies that the retained restart path survives only a successful non-closing shutdown.

## 2026-08-09 — `src/ruzu/src/configuration/qt_config.rs`, `configure_dialog.rs`, and `main.rs` vs `src/frontend_common/config.cpp` and `src/yuzu/configuration/qt_config.cpp`

### Intentional differences

- Rust keeps generic settings, Qt-compatible controls, and GTK UI values in separate writers over
  the same INI file. They execute in upstream order: generic `ReadValues`/`SaveValues` first, then
  frontend-owned controls and UI values.

### Binary layout verification

- Not applicable. A focused regression test verifies that the global `[Renderer]` category is read
  and that `backend=0` selects OpenGL instead of retaining the Vulkan default.

### Missing items

- `src/ruzu_cmd/src/sdl_config.rs` can read the currently bridged settings but does not implement the
  upstream reload/save and INI write paths for SDL, players, debug controls, and HIDBus values.
- `src/ruzu/src/configuration/configure_hotkeys.rs` displays default bindings, but bindings are not
  editable or persisted because `HotkeyRegistry` is absent; Clear All and Restore Defaults only
  log requests.
- Several advanced input configuration actions in
  `src/ruzu/src/configuration/configure_input_advanced.rs` remain informational placeholders.
- The Android Oboe audio backend is represented by a no-op stub in
  `src/audio_core/src/sink/oboe_sink.rs`.

## Video core

## 2026-08-09 — `src/ruzu/src/boot.rs`, `main_window.rs`, and `render_window_x11.rs` vs `src/video_core/video_core.cpp` and `src/yuzu/bootmanager.{h,cpp}`

### Intentional differences

- GTK4 does not expose a native child render widget, so the Linux frontend creates an X11 child
  with a GLX-compatible visual and retains an `Arc`-owned root share group. Renderer and shader
  worker contexts share that root, matching upstream `OpenGLSharedContext` ownership and thread
  behavior without Qt objects.
### Missing items

- The GTK frontend's shared OpenGL context bridge currently exists only for Linux/X11. The macOS
  and Windows GTK render-window adapters still provide Vulkan surfaces only.

### Unintentional differences (to fix)

- Renderer-construction failures still terminate through the existing CLI-style hard-error path.
  Upstream propagates renderer creation failure through `CreateGPU`, allowing the frontend to show
  an error without terminating the process; Rust's current `System::subsystem_factory` callback
  cannot return a `Result` yet.

### Binary layout verification

- Not applicable. This slice changes frontend native-context ownership and renderer dispatch; no
  guest-visible structure or raw payload layout changes.

## 2026-08-09 — `src/video_core/src/renderer_vulkan/turbo_mode.rs`, `renderer_vulkan/texture_cache.rs`, and `host_shaders/vulkan_turbo_mode.comp` vs `src/video_core/renderer_vulkan/vk_turbo_mode.{h,cpp}`, `vk_texture_cache.cpp`, and `host_shaders/vulkan_turbo_mode.comp`

### Intentional differences

- `TurboMode` moves a separately owned `TurboResources` bundle into its worker thread and exposes
  an `Arc` callback to `Scheduler`; upstream captures the containing object from a `std::jthread`.
  The device, workload, 100 ms idle predicate, queue-submit notification, and destruction ordering
  are unchanged.
- `TextureCacheRuntime` receives `cant_blit_msaa` during construction instead of retaining the full
  Vulkan `Device` wrapper. It uses the same predicate as upstream `Image::NeedsScaleHelper` and the
  same color or combined depth/stencil helper blits.

### Binary layout verification

- The turbo compute shader is byte-for-byte identical to upstream. This slice introduces no
  guest-visible raw-memory structure.

## 2026-08-09 — `src/video_core/src/host1x/codecs/vp8.rs`, `vp9.rs`, and `vp9_types.rs` vs `src/video_core/host1x/codecs/vp8.{h,cpp}`, `vp9.{h,cpp}`, and `vp9_types.h`

### Intentional differences

- Decoder methods receive the current `NvdecRegisters` explicitly through the existing Rust
  `DecoderImpl` trait; upstream retains the register owner in the decoder base class.
- Rust `Vec<u8>` values replace upstream `ScratchBuffer` and `Stream` owners without changing the
  emitted VP8/VP9 byte order or frame buffering lifecycle.

### Binary layout verification

- `Vp8PictureInfo` is `0xc0` bytes. `PictureInfo`, `EntropyProbs`, and `Vp9EntropyProbs` are
  respectively `0x100`, `0xea0`, and `0x7b4` bytes; compile-time offset assertions cover the fields
  read from NVDEC memory. Focused tests verify VP8 frame tags and VP9 range/bitstream encoder bytes.

## 2026-08-09 — `src/common/src/thread_worker.rs`, `src/video_core/src/rasterizer_interface.rs`, and renderer disk-cache loaders vs `src/common/thread_worker.h`, `src/video_core/rasterizer_interface.h`, and renderer shader caches

### Intentional differences

- Rust passes an `Arc<AtomicBool>` through `RasterizerInterface::load_disk_resources` instead of a
  copied `std::stop_token`. `StatefulThreadWorker::wait_for_requests_or_stop` polls that state while
  blocked because `std::sync::Condvar` has no stop-callback integration; observing cancellation
  permanently stops every worker and abandons queued work, matching upstream `request_stop()`
  semantics.
- The command-line frontend supplies a never-signaled cancellation owner because it has no loading
  dialog. The GTK frontend forwards the same stop state that owns its launch lifecycle.

### Binary layout verification

- Not applicable: this slice changes synchronization and owner propagation only.

## 2026-08-09 — `src/video_core/src/renderer_opengl/gl_state_tracker.rs` and `gl_rasterizer.rs` vs `src/video_core/renderer_opengl/gl_state_tracker.{h,cpp}` and `gl_rasterizer.cpp`

### Intentional differences

- `StateTracker` stores the active channel dirty flags as `NonNull<[bool; 256]>` and clears that
  borrowed pointer in `release_channel`; upstream stores a raw C++ pointer whose lifetime follows
  the channel owner implicitly.
- The scoped lock over the buffer and texture caches uses the existing retrying dual-lock helper
  because `parking_lot::ReentrantMutex` has no direct `std::scoped_lock` equivalent.

### Binary layout verification

- Not applicable: this slice changes owner references and lifecycle ordering only; no guest-visible
  structure is serialized or copied as raw bytes.

## 2026-08-09 — `src/video_core/src/texture_cache/texture_cache_base.rs` vs `src/video_core/texture_cache/texture_cache_base.h` and `control/channel_state_cache.inc`

### Intentional differences

- `channel_gpu_memory` is a Rust shared-owner mirror of upstream's live
  `channel_state->gpu_memory` reference. It is resynchronized after channel erasure so releasing an
  inactive channel preserves the active memory owner and releasing the active channel clears it.

### Binary layout verification

- Not applicable: this slice only updates channel ownership state.

## 2026-08-09 — `src/video_core/src/renderer_opengl/` vs `src/video_core/renderer_opengl/`

### Intentional differences

- Every upstream OpenGL header/implementation basename has a matching Rust module. Rust-only
  `mod.rs` files provide module declarations and do not own upstream behavior.
- `RendererOpenGL` boxes the single `StateTracker`, while `RasterizerOpenGL`, the texture runtime,
  and blit helpers hold stable non-owning pointers to it. This preserves upstream's single shared
  owner graph without creating movable Rust self-references.
- `QueryCache` receives `RasterizerOpenGL::any_command_queued()` immediately before the four query
  synchronization entry points instead of storing a back-reference to its containing rasterizer.
  The observable predicate and call ordering match upstream while avoiding another self-reference.
- Render-target and descriptor helpers receive register projections created from the production
  `Maxwell3DDrawView::Live` owner. Upstream dereferences `maxwell3d` directly inside the cache; the
  Rust projection avoids overlapping mutable borrows while reading the same live registers at the
  operation boundary.
- Backend `Image` state is stored separately from generic `ImageBase` state. Methods such as
  scaling therefore receive the paired base image explicitly instead of using C++ inheritance.

### Binary layout verification

- `ComputePipelineKey` is 24 bytes, `GraphicsPipelineKey` is 624 bytes, the GLASM bindless SSBO
  payload is 16 bytes, and `ScreenRectVertex` is four contiguous `GLfloat` values. Focused tests
  verify these raw-byte contracts.

## Shader recompiler and JIT

## 2026-08-09 — `src/rdynarmic/src/backend/arm64/emit_arm64_floating_point.rs`, `emit_arm64_vector_floating_point.rs`, and x64 exclusive-memory emitters vs Dynarmic `backend/arm64/emit_arm64_{floating_point,vector_floating_point}.cpp` and `backend/x64/emit_x64_memory.cpp.inc`

### Intentional differences

- ARM64 instruction words are emitted through rdynarmic's local encoder instead of Oaknut. The
  scalar half/fixed-16 conversions, reciprocal operations, `FMULX`, and vector half conversions
  preserve upstream register widths, FPCR/FPSR handling, and instruction ordering.
- x64 fastmem fallback addresses are offsets in rdynarmic's generated fallback table rather than
  Xbyak function pointers. Exclusive monitor locking, reservation invalidation, `cmpxchg` widths,
  and the `0` success / `1` failure status follow upstream.
- The x64 exception layer exposes upstream's `SupportsFastmem` capability as a compile-target
  predicate. A32 and A64 emitters disable direct fastmem when no native exception handler exists,
  while Linux/x86-64 and Windows/x86-64 retain fault redirection.
- The 128-bit exclusive-write split uses runtime SSE4.1 detection. Its fallback reproduces
  upstream's `movaps`/`movq`/`punpckhqdq` sequence on hosts without `pextrq`.
- A64 exclusive accesses emit upstream `EmitCheckMemoryAbort`; exclusive reads record the resume
  address immediately after the faulting load and only emit an explicit bounds-abort block when
  `EmitFastmemVAddr` requests one. Exclusive writes retain upstream's unconditional deferred fault
  stub and post-callback resume point.

### Binary layout verification

- No guest payload layout changes. Focused x64 tests cover 8/16/32/64/128-bit fallback generation,
  successful `LDAXR`/`STLXR` and `LDXP` return paths, host exception-handler capability, and a
  fault redirected to the raw exclusive callback.
- The ARM64 scalar/vector FP routing and half/fixed-16 conversion tests compile for
  `aarch64-unknown-linux-gnu` and pass under QEMU. This also verifies that the former cross-target
  exception-handler build failure is no longer present.

### Unintentional differences (to fix)

- rdynarmic's ARM64 backend still has a catch-all error for unported IR opcodes. Implemented
  upstream families still absent from the Rust dispatcher include packed arithmetic, scalar and
  vector saturation, AES/SHA/CRC/SM4 cryptography, and selected integer vector reductions,
  min/max, halving, rounding, and broadcast operations. Upstream's 16-bit FP specializations that
  themselves terminate with `ASSERT_FALSE("Unimplemented")` are not counted as port debt.

## 2026-08-09 — `src/frontend_common/src/play_time_manager.rs` vs Eden `src/frontend_common/play_time_manager.{h,cpp}`

### Intentional differences

- Rust uses a channel and `JoinHandle` in place of `std::jthread` and its stop token. Stop still
  wakes and joins the worker, accounts the final whole-second interval, then persists the database.
- A mutex protects the database because GTK can read it while the timestamp worker updates it.

### Binary layout verification

- PASS: each entry is two consecutive little-endian `u64` values and occupies 16 bytes, matching
  Eden's raw `PlayTimeElement` array in `playtime.bin`.

## 2026-08-09 — `src/ruzu/src/game_list.rs` vs Eden `src/yuzu/game/game_list.{h,cpp}` and `src/qt_common/game_list/{model,worker}.{h,cpp}`

### Intentional differences

- GTK `ColumnView` factories replace Qt `QStandardItem` subclasses while preserving Eden's Name,
  File type, Size, Play time, and Add-ons column order, values, and visibility settings.
- Eden transfers worker results with Qt signals. Ruzu transfers plain scan results over a channel
  and materializes GTK objects on the main context. A generation counter provides Eden's stale-work
  cancellation guarantee when a newer refresh supersedes an older scan.
- The metadata worker builds a filesystem controller and provider union because ruzu has no
  persistent frontend `Core::System`; NAND, SDMC, and game-directory manual content are mounted
  before `PatchManager` is queried.
- The internal action identifier remains `properties`, while its visible label is Eden's
  `Configure Game`.

### Binary layout verification

- Not applicable to the GTK model; the shared play-time file layout is verified separately.

## 2026-08-09 — `src/ruzu/src/{boot,main_window}.rs` vs Eden `src/yuzu/main_window.{h,cpp}`

### Intentional differences

- Eden starts play-time accounting directly in `OnStartGame`. Ruzu's boot thread emits a lossless
  `Started { program_id }` event so GTK performs the equivalent transition. Pause, resume, stop,
  restart, and guest-driven exit retain Eden's ordering.

### Binary layout verification

- Not applicable: this changes frontend lifecycle events only.

## 2026-08-09 — `src/ruzu/src/configuration/configure_per_game_addons.rs` vs Eden `src/yuzu/configuration/configure_per_game_addons.{h,cpp,ui}`

### Intentional differences

- Eden reuses its persistent frontend `Core::System`. Ruzu rebuilds NAND, SDMC, and configured game
  directory providers while Configure Game is open, then queries the same `PatchManager` data.
- GTK uses a `gio::ListStore` rather than `QStandardItemModel`; patch name, version, enabled state,
  sorting, and disabled-addon persistence retain their upstream roles.

### Binary layout verification

- Not applicable: this is host frontend state.

## 2026-08-09 — `src/common/src/settings.rs` vs Eden `src/common/settings.h`

### Intentional differences

- `ext_content_from_game_dirs` participates in ruzu's generic category visitor instead of Eden's
  C++ settings linkage, preserving the same default and persisted value.
- `gpu_fence_behavior` uses ruzu's generic switchable-setting visitor and GTK combo-row frontend
  instead of Eden's C++ linkage and Qt widget. The five enum values, persisted key, default, range,
  per-game switchability, and helper predicates match Eden.

### Binary layout verification

- Not applicable: this setting is not guest-visible.

## 2026-08-09 — `src/core/src/file_sys/registered_cache.rs` vs Eden `src/core/file_sys/registered_cache.{h,cpp}`

### Intentional differences

- `ExternalUpdateEntry::files` uses seven `Option<VirtualFile>` elements in place of nullable C++
  handles. The raw `ContentRecordType` index and seven-entry contract are unchanged.
- `open_container_as_nsp` probes NSP and then XCI directly, preserving Eden's final parser fallback
  without introducing a reverse dependency from `file_sys` to the loader dispatcher.

### Binary layout verification

- Not applicable: manual-provider entries are host-only. Focused tests cover highest-version
  selection, descending update order, and clearing versioned entries.

## 2026-08-09 — `src/video_core/src/engines/maxwell_3d.rs` and `src/video_core/src/buffer_cache/buffer_cache.rs` vs Eden `src/video_core/engines/maxwell_3d.h` and `src/video_core/buffer_cache/buffer_cache.h`

### Intentional differences

- Rust reads transform-feedback registers through `transform_feedback_buffer_info` rather than
  exposing the packed register union. `size` and `start_offset` remain signed `s32` values, and the
  buffer cache preserves their raw two's-complement bit patterns when forming GPU addresses and
  sizes.
- `PrimitivesSucceededStreamer` owns the same dependency on the transform-feedback byte counter,
  but the Rust query owner retains the dependent host report directly instead of storing an index
  into Eden's generic `SimpleStreamer` pool. Topology conversion, tessellation-output remapping,
  patch-vertex handling, per-stream stride selection, reset forwarding, and zero-stride handling
  remain identical.
- The external recursive buffer-cache mutex is held in an `Arc`. This keeps the mutex owned by the
  cache while allowing Vulkan query operations to clone the lock before mutating the cache, instead
  of creating an aliased raw pointer to a field of the active mutable reference.

### Binary layout verification

- PASS: focused register tests verify that `0xffff_fff0` and `0xffff_ffe0` are exposed as `-16`
  and `-32`; consumers cast back to unsigned values without clamping or normalization.

## 2026-08-09 — `src/video_core/src/renderer_vulkan/query_cache.rs`, `scheduler.rs`, `vk_rasterizer.rs`, `renderer_vulkan.rs`, and `src/video_core/src/vulkan_common/vulkan_device.rs` vs Eden `src/video_core/renderer_vulkan/vk_query_cache.{h,cpp}`, `vk_scheduler.{h,cpp}`, `vk_rasterizer.{h,cpp}`, `renderer_vulkan.{h,cpp}`, and `vk_device.{h,cpp}`

### Intentional differences

- Query banks use Rust leases and shared state handles instead of Eden's `BankPool` and raw
  `QueryCache*`. Slot reuse, render-pass close ordering, query reset ordering, and final-value
  synchronization follow the upstream lifecycle.
- Transform-feedback query banks retain a non-owning allocator pointer because the renderer owns
  the allocator for longer than the rasterizer and query cache. Readback uses a mapped mirror while
  preserving Eden's begin/end/copy ordering and four-stream contract.
- Dynamic vertex input is rebuilt from the complete Maxwell description through Vulkan dynamic
  state. Attribute and binding limits, constant-attribute filtering, divisors, and dirty-state
  clearing follow `RasterizerVulkan::UpdateVertexInput`.
- `report_device_loss` is a module helper so query-bank owners that retain an `ash::Device` rather
  than the complete `Device` can execute Eden's same error-and-delay behavior.
- Vulkan buffers retained by the allocator are represented by raw `vk::Buffer` handles rather than
  Eden's RAII wrappers. Their lifetime remains bounded by the renderer-owned allocator, which
  outlives the boxed query cache and its compute passes.
- Channel-bound guest-address translation uses a boxed adapter because the generic Rust query cache
  stores trait-object pointers. Conditional rendering is stopped before that adapter is released.
- Multi-slot occlusion reports feed Eden's exact prefix-scan shaders and push constants directly
  into the tracked common buffer-cache destination. The Rust query owner retains cumulative query
  leases instead of reproducing Eden's `HostSyncValues` staging vectors; reset and accumulation
  boundaries produce the same prefix value. Resolve and intermediary buffers use Eden's lazy
  power-of-two size classes with the same 2048-slot minimum and are reused for the renderer
  lifetime.
- The direct guest-buffer path copies Eden's complete 8-byte query value. A producer-specific
  barrier orders either query-pool transfer writes or prefix-scan compute writes before the final
  transfer read.
- Host conditional rendering uses the same direct-buffer and compute-resolve paths, extension
  commands, driver fallbacks, comparison inversion, and transfer/host barriers. Rust stores the
  active Vulkan setup in scheduler-shared state so render-pass transitions can pause it without a
  raw `QueryCacheRuntime*`.

### Binary layout verification

- PASS: the compute push-constant structs are `repr(C)` and verified as 4 bytes for conditional
  rendering and 16 bytes for prefix scan. The three GLSL sources are byte-identical to Eden.
  Focused tests cover slot ordering, cumulative ZPass reports, primitive topology conversion,
  unsynchronized fence rejection, empty ZPass reports, scan size classes and producer barriers,
  TFB stream mapping, query payload/timestamp writes, and draw preparation ordering.

## 2026-08-09 — `src/video_core/src/renderer_vulkan/compute_pass.rs`, `descriptor_pool.rs`, and `update_descriptor.rs` vs Eden `src/video_core/renderer_vulkan/vk_compute_pass.{h,cpp}`, `vk_descriptor_pool.{h,cpp}`, and `vk_update_descriptor.{h,cpp}`

### Intentional differences

- `DescriptorAllocator` clones share allocator state through `Arc<Mutex<_>>` so Rust's `Send +
  'static` scheduler closures can perform Eden's descriptor-set commit on the worker. The resource
  pool, bank, layout and tick-based reuse remain shared by the same compute-pass owner.
- Raw descriptor payload pointers are wrapped in a `Send` newtype. The queue owns one fixed
  allocation for the renderer lifetime, and its frame ring waits for the worker before recycling a
  slice, matching Eden's recorded `const DescriptorUpdateEntry*` lifetime.

### Binary layout verification

- PASS: compute descriptor templates use `size_of::<DescriptorUpdateEntry>()` as Eden does. Unit
  tests verify the union size/alignment and the two- and three-buffer template strides.

## 2026-08-09 — `src/core/src/core.rs` and `src/core/src/hle/kernel/kernel.rs` vs Eden `src/core/hle/kernel/kernel.cpp`

### Intentional differences

- Ruzu still owns one shared `KMemoryBlockSlabManager` instead of Eden's separate application and
  system managers. Its runtime capacity is now the exact sum of Eden's 20000-entry application and
  10000-entry system heaps, so the adaptation no longer lowers the available resource limit.

### Missing items

- Separate application and system `KSystemResource` ownership remains to be ported before the two
  memory-block slab managers can be represented independently.

### Binary layout verification

- PASS: no guest-visible binary layout is changed; the regression test verifies both upstream
  capacities and their combined runtime value.

## 2026-08-09 — `src/core/src/hle/kernel/k_shared_memory.rs` vs Eden `src/core/hle/kernel/k_shared_memory.{h,cpp}`

### Unintentional differences (fixed)

- Allocation failure now returns `Kernel::ResultOutOfMemory` (`0xD001`) as Eden does; the previous
  raw `0xCE01` encoded `Kernel::ResultOutOfResource`.

### Binary layout verification

- PASS: no structure layout changed.

## 2026-08-18 — workspace SDL manifests vs Eden `src/audio_core/CMakeLists.txt`, `src/input_common/CMakeLists.txt`, and `src/yuzu_cmd/CMakeLists.txt`

### Intentional differences

- Eden links `SDL3::SDL3` supplied by CMake. Ruzu pins `sdl3` 0.18.4 and
  `sdl3-sys` 0.6.8 (SDL 3.4.14) in the workspace and builds the static SDL3
  library from source. This keeps the same SDL generation and one resolved
  runtime across Linux, macOS, Windows, and BSD hosts without requiring a
  platform package, pkg-config, or vcpkg SDL installation.
- `input_common` uses the raw `sdl3-sys` API because its port mirrors the C API;
  `audio_core` and `ruzu_cmd` use the higher-level `sdl3` crate while still
  resolving the same `sdl3-sys` package and native SDL library.

### Unintentional differences (to fix)

- None found in the desktop SDL3 dependency ownership or generation.

### Missing items

- Cross-target dependency resolution was verified for Windows MSVC, macOS
  aarch64, and FreeBSD. Native linking and runtime execution still require CI
  or hardware for each target.

### Binary layout verification

- N/A: this change affects native dependency selection only. `audio_core` and
  `input_common` unit tests pass with the resolved SDL 3.4.14 build.

## 2026-08-18 — `src/ruzu/Info.plist` and `scripts/build-macos-app.sh` vs Eden `src/yuzu/Info.plist` and `src/yuzu/CMakeLists.txt`

### Intentional differences

- Eden uses CMake's `MACOSX_BUNDLE` target property; ruzu's Cargo workspace uses a dedicated
  packaging script after `cargo build --release --bin ruzu`. Both produce the same macOS bundle
  ownership and directory layout.
- Eden copies prebuilt `eden.icns` and `Assets.car` resources. Ruzu generates `ruzu.icns` from the
  frontend-owned rusty-lemon PNG because it does not have an Apple asset catalog.
- The local developer bundle receives an ad-hoc signature after MoltenVK is copied. Distribution
  identity signing and notarization remain release-pipeline responsibilities.

### Unintentional differences (to fix)

- None found in the application bundle layout or MoltenVK lookup path.

### Missing items

- Ruzu has no liquid-glass `Assets.car` equivalent to Eden's asset catalog.

### Binary layout verification

- PASS: the generated bundle contains an arm64 `Contents/MacOS/ruzu`, a valid `Info.plist`,
  `Contents/Resources/ruzu.icns`, and an arm64
  `Contents/Frameworks/libMoltenVK.dylib`. `codesign --verify --deep --strict` passes, and a
  LaunchServices smoke test starts the bundled executable.

## 2026-08-18 — `src/video_core/src/vulkan_common/vulkan_library.rs` vs Eden `src/video_core/vulkan_common/vulkan_library.cpp`

### Intentional differences

- Both implementations retain `LIBVULKAN_PATH` as the first explicit lookup and prefer the
  application bundle next. For an unbundled development `ruzu-cmd`, Rust additionally searches the
  sibling Eden build so performance and rendering comparisons use Eden's exact bundled MoltenVK.
- `scripts/build-macos-app.sh` likewise copies Eden's bundled MoltenVK when available, after an
  explicit `MOLTENVK_LIBRARY` override and before the Homebrew fallback.

### Unintentional differences (to fix)

- None found in lookup priority. The previous development fallback selected a different emulator's
  MoltenVK 1.4.2 while the current Eden build embeds MoltenVK 1.4.1.

### Missing items

- Distribution builds still need a release-owned MoltenVK artifact rather than relying on a sibling
  development checkout.

### Binary layout verification

- N/A: the Vulkan loader ABI is unchanged; this only selects the dynamic library implementation.

## 2026-08-18 — `src/ruzu_cmd/src/emu_window/emu_window_sdl3_vk.rs` vs Eden `src/yuzu_cmd/emu_window/emu_window_sdl3_vk.cpp`

### Intentional differences

- Ruzu stores the `CAMetalLayer` returned by `SDL_Metal_GetLayer` as the render surface and retains
  the `SDL_MetalView` separately for its lifetime. Eden stores the opaque Metal view directly while
  its Vulkan surface path consumes it as a `CAMetalLayer`; the Rust split keeps the consumed native
  object explicit without changing the Cocoa ownership boundary.

### Unintentional differences (to fix)

- None. The SDL3 migration had left `WindowSystemInfo::type_` at `Headless` on macOS; it now assigns
  `Cocoa` before publishing the Metal layer, matching Eden's constructor ordering.

### Missing items

- None for macOS window-system selection.

### Binary layout verification

- N/A: no serialized or guest-visible structure is changed.

## 2026-08-18 — `src/video_core/src/vulkan_common/vulkan_device.rs` vs Eden `src/video_core/vulkan_common/vulkan_device.cpp`

### Intentional differences

- None in the format-property probe list.

### Unintentional differences (to fix)

- None. The ten ETC2/EAC formats at the end of Eden's `GetFormatProperties` format list are now
  queried by ruzu as well. Previously they missed the cache and `is_format_supported` conservatively
  returned true after logging `Unimplemented format query`, which also prevented device-aware
  storage, blit, and texel-buffer capability checks from using the real driver properties.
- Eden explicitly disables `robustBufferAccess2` and `robustImageAccess2` while retaining
  `nullDescriptor`. Ruzu now applies the same feature mutation before passing the queried feature
  chain to `vkCreateDevice`; previously all robustness2 features advertised by MoltenVK remained
  enabled.

### Missing items

- None for the format-property probe list or robustness2 feature selection.

### Binary layout verification

- N/A: the change only extends physical-device capability discovery.

## 2026-08-18 — `src/video_core/src/renderer_vulkan/query_cache.rs` vs Eden `src/video_core/renderer_vulkan/vk_query_cache.cpp`

### Intentional differences

- Rust query reports share their measured slots and synchronized result through `Arc` rather than
  Eden's query IDs and `HostQueryBase::IsFinalValueSynced` flag. The report remains unavailable to
  the guest writeback callback until the matching async-flush set has been popped.

### Unintentional differences (to fix)

- None in the host occlusion-query flush lifecycle. The Vulkan `SamplesStreamer` now participates
  in `HasUnsyncedQueries`, `PushUnsyncedQueries`, `ShouldWaitAsyncFlushes`, and
  `PopUnsyncedQueries`. Previously it bypassed that lifecycle and called
  `vkGetQueryPoolResults` before the corresponding fence, producing `VK_NOT_READY` and thousands
  of unsynchronized-query errors.
- `pending_flush_sets` is protected across the GPU and GPU-fencing threads, matching Eden's
  `flush_guard`. The initial Rust adaptation omitted this synchronization.

### Missing items

- None for host occlusion-query fence synchronization. The existing Rust lease-based bank owner
  remains an intentional structural adaptation documented in the 2026-08-09 query-cache entry.

### Binary layout verification

- N/A: no guest-visible structure changed. All 17 focused Vulkan query-cache tests pass. A
  90-second title run produced zero `Query report value not synchronized` and zero
  `vkGetQueryPoolResults ... NOT_READY` messages; the previous implementation produced roughly
  8,000 such messages in the same startup/title interval.

## 2026-08-18 — `src/core/src/gpu_core.rs` and `src/video_core/src/gpu.rs` vs Eden `src/video_core/gpu.{h,cpp}`

### Intentional differences

- The cross-crate `GpuCoreInterface` exposes Eden's concrete `GPU` methods to `core`; its test
  doubles in `memory.rs`, `nvhost_as_gpu.rs`, and `nvhost_gpu.rs` implement `wait_for_composite`
  as a no-op because they have no GPU thread or renderer.
- Rust stores the pending composite fence in `AtomicU64` because the split interface is callable
  through shared references. Eden stores the same single pending fence as a plain `u64` under its
  HWC/GPU-thread lifecycle.

### Unintentional differences (to fix)

- None. `RequestComposite` now records the pending sync-operation fence and returns after
  `TickGPU`; it no longer waits synchronously. `WaitForComposite` consumes and waits that fence at
  the next HWC tick, including Eden's zero-fence and shutdown exits.

### Missing items

- None for the composite request/wait lifecycle.

### Binary layout verification

- N/A: no guest-visible or serialized structure changed.

## 2026-08-18 — `src/core/src/hle/service/nvdrv/devices/nvdisp_disp0.rs` vs Eden `src/core/hle/service/nvdrv/devices/nvdisp_disp0.{h,cpp}`

### Intentional differences

- The Rust owner forwards through `GpuCoreInterface` because `core` cannot own the concrete
  `video_core::Gpu`; the call position and behavior match Eden's direct `system.GPU()` call.

### Unintentional differences (to fix)

- None. `wait_for_composite` now forwards Eden's HWC synchronization point to the GPU.

### Missing items

- None for composite waiting.

### Binary layout verification

- N/A: no ABI payload changed.

## 2026-08-18 — `src/core/src/hle/service/nvnflinger/display.rs` and `hardware_composer.rs` vs Eden `src/core/hle/service/nvnflinger/display.h` and `hardware_composer.{h,cpp}`

### Intentional differences

- Rust uses `BTreeMap` and `Arc<Mutex<Layer>>` in place of Eden's `flat_map` and shared pointers;
  keys, layer ownership and mutation boundaries are unchanged.

### Unintentional differences (to fix)

- None. `Layer` now owns Eden's `z_index` and `is_overlay` fields with the same defaults.
- `ComposeLocked` now waits for the previous composite, releases eligible buffers before
  acquisition, interval-gates non-overlay acquisition, excludes overlays from game cadence,
  stable-sorts real z indices, composites only after a new acquisition, advances exactly one HWC
  frame, and returns one.
- Framebuffer release numbers are absolute (`frame_number + interval`), `last_acquire_frame` is
  tracked, and overlays release independently, matching Eden's lifecycle and ordering.

### Missing items

- None in the framebuffer cadence and release lifecycle covered by this slice.

### Binary layout verification

- N/A: these are host-side service structures. The Layer default regression test passes.

## 2026-08-18 — `src/core/src/hle/service/nvnflinger/surface_flinger.rs` vs Eden `src/core/hle/service/nvnflinger/surface_flinger.{h,cpp}`

### Intentional differences

- Rust returns `Option<Arc<Mutex<Layer>>>` from `find_layer` instead of a nullable shared pointer.

### Unintentional differences (to fix)

- None. `find_layer` is again a public SurfaceFlinger-owned operation, and the overlay setter
  updates the matching layer where Eden owns that mutation. Z-index writes remain owned by
  `Container`, which uses this lookup exactly as Eden does.

### Missing items

- None for layer lookup, z-index, visibility, blending, and overlay state.

### Binary layout verification

- N/A: no guest-visible structure changed.

## 2026-08-18 — `src/core/src/hle/service/vi/container.rs`, `manager_display_service.rs`, and `system_display_service.rs` vs Eden `src/core/hle/service/vi/container.{h,cpp}`, `manager_display_service.{h,cpp}`, and `system_display_service.{h,cpp}`

### Intentional differences

- Rust returns `Result<T, ResultCode>` rather than writing C++ `Out<T>` parameters. The CMIF
  handlers retain Eden's wire ordering and signed-to-unsigned bit casts.

### Unintentional differences (to fix)

- None. Container now owns set/get z-index and overlay forwarding. ManagerDisplayService exposes
  its upstream z-index forwarding method.
- SystemDisplayService now wires `GetLayerZ`, parses `SetLayerZ` as `layer_id: u64` followed by
  `z_value: u64`, preserves the low signed 32-bit z pattern, and forwards visibility instead of
  returning success without changing the layer.

### Missing items

- None for the z-index and visibility methods covered by this slice.

### Binary layout verification

- PASS: SetLayerZ consumes two consecutive 64-bit request values in Eden's signature order;
  GetLayerZ returns the signed 32-bit z index sign-extended and reinterpreted as `u64`.

## 2026-08-20 — `src/video_core/src/query_cache/bank_base.rs` vs Eden `src/video_core/query_cache/bank_base.h`

### Intentional differences

- `BankPool::can_recycle_front` exposes the exact predicate used by `ReserveBank` so the Vulkan
  caller can construct fallible resources before entering Rust's infallible builder closure.
- The file was normalized from CRLF to LF while formatting the new implementation and tests.

### Unintentional differences (to fix)

- None. Reserve, close, reference counting, reset, dead-bank selection and queue rotation retain
  Eden's ordering and conditions.

### Missing items

- None for `BankBase` and `BankPool`.

### Binary layout verification

- N/A: these are host-only bookkeeping types.

## 2026-08-20 — `src/video_core/src/renderer_vulkan/query_cache.rs` vs Eden `src/video_core/renderer_vulkan/vk_query_cache.{h,cpp}`

### Intentional differences

- Samples banks live in `Arc` and hold `BankBase` behind a mutex so fence-thread reports can own
  their banks safely; Eden stores banks by value in `std::deque`.
- Reports materialize bank spans instead of following `next_bank`. They retain independent bank
  references, remain cumulative until reset, and merge min/max ranges per bank across each flush
  set before host readback.
- The CPU and GPU halves of recycled pool reset are split because `BankLike::reset` cannot receive
  `&mut Scheduler`; the GPU reset is still recorded before the first reused slot.
- Scheduler-facing accessors return the three independently locked state handles needed by the
  safe cross-owner adaptation.

### Unintentional differences (to fix)

- None in samples report ownership, async-flush gating, bank-wide host readback, or the scheduler
  bridge covered by this correction.

### Missing items

- Existing parity debt outside this correction remains in the full Eden samples accumulation
  state machine (`amend_value`, `accumulation_value`, checkpoints and the complete
  `PresyncWrites`/`SyncWrites` lifecycle).
- A real Vulkan occlusion-query title run is still required; unit tests do not execute a device
  query pool.

### Binary layout verification

- N/A: no guest-visible raw-memory structure changed.

## 2026-08-20 — `src/video_core/src/renderer_vulkan/scheduler.rs` vs Eden `src/video_core/renderer_vulkan/vk_scheduler.{h,cpp}`

### Intentional differences

- Rust stores shared handles to `SamplesQueryState`, `TfbCounterState` and `QueryRuntimeState`
  instead of Eden's non-owning `QueryCache*`. This avoids aliased `&mut` references while keeping
  `EndPendingOperations` and `EndRenderPass` call ordering identical.
- `clear_query_cache_state` releases those handles before the rasterizer's Vulkan resources are
  destroyed; Eden relies on C++ member lifetime and its raw pointer is not dereferenced afterward.

### Unintentional differences (to fix)

- None in the reviewed counter-reset, counter-close and conditional-rendering ordering.

### Missing items

- None for this scheduler/query-cache interaction slice.

### Binary layout verification

- N/A: scheduler state is host-only.

## 2026-08-20 — `src/video_core/src/renderer_vulkan/vk_rasterizer.rs` vs Eden `src/video_core/renderer_vulkan/vk_rasterizer.{h,cpp}`

### Intentional differences

- The Rust constructor installs safe query-state handles only after every fallible resource
  creation succeeds, rather than storing Eden's direct `QueryCache*`. This prevents failed
  construction from leaving a dangling scheduler registration.
- The destructor explicitly clears those handles after `finish` and before destroying the query
  cache's Vulkan resource owners.

### Unintentional differences (to fix)

- None in construction registration, async query flush forwarding, or teardown ordering.

### Missing items

- None for the reviewed scheduler/query-cache ownership slice.

### Binary layout verification

- N/A: no guest ABI or serialized payload changed.

## 2026-08-20 — `src/core/src/hle/service/am/service/library_applet_creator.rs` vs Eden `src/core/hle/service/am/service/library_applet_creator.{h,cpp}`

### Intentional differences

- Rust manually parses CMIF arguments and resolves the transfer-memory handle through the current
  process, replacing Eden's typed `InCopyHandle<KTransferMemory>` deserializer.
- Rust returns service objects through `ResponseBuilder` rather than C++ `Out<SharedPointer<T>>`.

### Unintentional differences (to fix)

- None. `CreateTransferMemoryStorage` now naturally aligns the `s64` following the `bool`, and
  both transfer-memory creation commands validate `size` before resolving the handle, matching
  Eden's argument layout and validation order.

### Missing items

- None for the storage creation handlers reviewed in this slice.

### Binary layout verification

- PASS: `RequestParser::align_for::<i64>()` advances the raw CMIF cursor to the same 8-byte
  boundary used by Eden's typed serialization.

## 2026-08-20 — `src/ruzu/src/applets/software_keyboard.rs` vs Eden `src/yuzu/applets/qt_software_keyboard.{h,cpp}`

### Intentional differences

- GTK widgets, CSS and a main-loop channel replace Qt Designer widgets, Qt queued signals and the
  dedicated `InputInterpreter` thread; the frontend remains owned by the GUI module.
- Inline hide destroys the GTK dialog and recreates it on the next show while retaining guest text
  state; Eden hides and reuses its Qt dialog. This avoids retaining a hidden modal GTK window.
- The GTK frontend uses a single-line `Entry` for every draw type and does not reproduce Eden's
  framebuffer-relative geometry, controller artwork or DPI-specific Qt layout.

### Unintentional differences (to fix)

- None in the reviewed applet contract. Normal submissions now retain the active dialog through
  `Failure`/`Confirm` text checks, and only `ExitKeyboard` tears it down.
- Controller callbacks no longer re-enter `active: RefCell` while it is borrowed, and the input
  edge which opened the keyboard is discarded instead of immediately activating X/Cancel.
- Inline appear parameters, guest text/cursor updates, `ChangedString`, `MovedCursor`, key-disable
  flags, optional number-pad symbols, Shift/Caps Lock transitions and wrapped grid navigation now
  follow Eden's corresponding paths.

### Missing items

- Eden's held-button autorepeat and rich multi-line `SwkbdTextDrawType::Box` presentation remain UI
  features of the excluded Qt frontend; they are not part of this GTK crash/lifecycle correction.

### Binary layout verification

- N/A: this is host UI state. Guest-visible string lengths and cursor positions are explicitly
  converted to UTF-16 code-unit counts, with a focused regression test.

## 2026-08-20 — `src/ruzu/src/applets/mod.rs`, `src/ruzu/src/boot.rs`, and `src/ruzu/src/main_window.rs` vs Eden `src/yuzu/main_window.{h,cpp}` software-keyboard ownership

### Intentional differences

- `GMainWindow` creates the persistent GTK channel frontend and passes its trait object through
  `boot_game`; Eden owns a persistent `QtSoftwareKeyboard` signal bridge and allocates the dialog
  from its main-window slots.
- The module and boot wiring have no direct file counterpart because Eden's Qt frontend directory
  is excluded and ruzu owns its GTK frontend under `src/ruzu/src/applets`.


## 2026-08-20 — `src/core/src/hle/service/am/frontend/applet_software_keyboard.rs` vs Eden `src/core/hle/service/am/frontend/applet_software_keyboard.{h,cpp}`

### Intentional differences

- Eden's frontend callbacks invoke `SubmitTextNormal` and `SubmitTextInline` directly on the
  owning C++ object. Rust queues callback arguments to avoid aliasing the applet through a GUI
  callback, then resumes the owning frontend applet through its weak `Applet` reference.
- `frontend_executing` distinguishes synchronous frontend callbacks from delayed GUI callbacks;
  queued work is drained before an active call returns, while delayed work reacquires the applet

### Binary layout verification

- PASS: the foreground result remains a zero-initialized `sizeof(SwkbdResult) +
  STRING_BUFFER_SIZE` buffer, with the result followed by UTF-8 or UTF-16 text exactly as before.

## 2026-08-20 — `src/core/src/hle/kernel/k_process.rs` vs Eden `src/core/hle/kernel/k_process.{h,cpp}` termination caller selection

### Intentional differences

- Rust represents Eden's `KThread* thread_to_not_terminate` as an `Option<u64>` thread id while
  preserving the same identity comparison in `terminate_children`.
- `exit_with_current_thread` performs Eden's final `GetCurrentThread(kernel).Exit(kernel)` after
  releasing the process guard because Rust cannot re-enter the thread lifecycle while borrowing
  `KProcess` through its owning cell.

## 2026-08-20 — `src/ruzu/src/overlay_dialog.rs` and `src/ruzu/src/main_window.rs` vs Eden `src/yuzu/util/overlay_dialog.{h,cpp,ui}` and `src/yuzu/main_window.{h,cpp}`

### Intentional differences

- The GTK shutdown-only counterpart is an undecorated transient window sized to Eden's visible
  780-by-300 regular-text panel proportions. Eden uses a parent-sized translucent Qt dialog whose
  internal grid draws that panel; a GTK top-level is required to remain above ruzu's native render
  child window.
- The GTK module implements only the non-interactive regular-text configuration used by
  `OnShutdownBeginDialog`; controller navigation and rich text belong to Eden's other overlay uses.

### Unintentional differences (to fix)

- None in the Stop/Restart lifecycle: the panel is created only after a successful asynchronous
  stop request and is closed when `StopComplete` reaches `on_emulation_stopped`.

### Missing items

- Generic interactive and rich-text overlay modes are outside this shutdown-dialog slice.

### Binary layout verification

- N/A: the overlay contains host UI state only.

## 2026-08-20 — `src/ruzu/src/game_list.rs` and `src/ruzu/src/main_window.rs` vs Eden `src/yuzu/game/game_list.{h,cpp}` and `src/yuzu/main_window.{h,cpp}` shortcut dispatch

### Intentional differences

- A Rust callback replaces Eden's Qt `GameList::CreateShortcut` signal while retaining the same
  `(program_id, game_path, target)` payload and `GMainWindow` ownership of argument construction.
- GTK `gio::SimpleAction` objects replace the two `QAction` objects. Both remain hidden on macOS,
  matching Eden's compile-time guard.

### Unintentional differences (to fix)

- None. Both context-menu actions now reach `on_game_list_create_shortcut`; the former
  unavailable-action placeholders were removed.

### Missing items

- None for per-game shortcut dispatch.

### Binary layout verification

- N/A: this is host frontend dispatch.

## 2026-08-20 — `src/ruzu/src/util/game.rs` vs Eden `src/qt_common/util/game.{h,cpp}` shortcut creation

### Intentional differences

- GTK message dialogs replace `QtCommon::Frontend` dialogs, and GLib's XDG directory resolvers
  replace `QStandardPaths` on Linux.
- Linux icons and comments use the ruzu name (`ruzu-*.png`, `Ruzu Emulator`) instead of Eden's
  branding while preserving Eden's icon directory and title-id naming scheme.
- Windows creates the equivalent `.lnk` through the installed PowerShell `WScript.Shell` COM
  bridge and standard user-profile paths rather than directly owning `IShellLinkW`; this avoids a
  second Windows COM binding while preserving target, arguments, description and icon fields.

### Unintentional differences (to fix)

- None in the Linux shortcut slice. Target validation, patched control metadata precedence,
  loader fallbacks, illegal-character removal, icon creation, one-time AppImage warning,
  fullscreen argument ordering and result messages follow Eden's order.

### Missing items

- `CreateHomeMenuShortcut` and the unrelated content-removal helpers from `qt_common/util/game.cpp`
  are outside this per-game shortcut slice.
- Eden's multi-resolution Windows ICO encoder is not yet ported; Windows currently stores the
  decoded icon as PNG before assigning it to the `.lnk`.

### Binary layout verification

- N/A on Linux. The `.desktop` field order and optional-field rules are covered by a focused test.

## 2026-08-20 — `src/ruzu/src/game_list.rs` vs Eden `src/yuzu/game/game_list.cpp` context-menu submenu presentation

### Intentional differences

- GTK `PopoverMenuFlags::NESTED` supplies the traditional child-popover behavior provided by
  Eden's `QMenu`; the toolkit-specific construction differs while retaining hover, click and
  keyboard access to each submenu.

### Unintentional differences (to fix)

- None. `Remove`, `Dump RomFS`, and `Create Shortcut` no longer use GTK's click-only sliding-page
  presentation and now open as nested menus on pointer hover like Eden.

### Missing items

- None for game-list submenu presentation.

### Binary layout verification

- N/A: this is host UI behavior only.

## 2026-08-20 — `src/ruzu/src/overlay_dialog.rs` vs Eden `src/yuzu/util/overlay_dialog.cpp` and `src/yuzu/main_window.cpp` shutdown-dialog destruction

### Intentional differences

- GTK exposes window-manager closure and programmatic `Window::close` through the same
  `close-request` signal. Ruzu retains the signal id so it can remove the user-close guard before
  performing Eden's `OnEmulationStopped`-owned destruction.

### Unintentional differences (to fix)

- None. The initial port incorrectly returned `Stop` for the programmatic close request too, which
  left `Closing software...` visible after `StopComplete`; the guard is now disconnected first.

### Missing items

- None for shutdown-dialog destruction.

### Binary layout verification

- N/A: this is host UI lifecycle state only.

## 2026-08-20 — `src/ruzu/src/main_window.rs` and `src/ruzu/src/game_list.rs` vs Eden `src/yuzu/main_window.{h,cpp}` and `src/qt_common/game_list/model.{h,cpp}` refresh ownership

### Intentional differences

- Per explicit project UI direction, Ruzu keeps Refresh beside Add Game Directory in the upper
  game-list toolbar instead of Eden's bottom status bar. The widget forwards its action to
  `GMainWindow::OnGameListRefresh`, and its handle is disabled and enabled across the same
  emulation lifecycle as Eden's button.
- Ruzu's game-directory worker clears and rebuilds the frontend manual content provider in the
  same scan that rebuilds the visible rows. `refresh_external_content` therefore records that the
  already-started directory refresh covers external content instead of starting a second racing
  Rust worker; Eden can safely run two sequential `Repopulate()` calls because destroying its
  current worker waits for completion.

### Unintentional differences (to fix)

- None for the manual refresh behavior. The upper-toolbar button clears cached metadata before
  scanning, refreshes the directory/provider data, and is disabled from boot until emulation
  stops.

### Missing items

- Eden's independent filesystem watchers for `Settings::values.external_content_dirs` are not
  present in Ruzu; configured game directories are refreshed explicitly by this button.
- `SetFirmwareVersion()` has no Ruzu status-label counterpart to update after refresh.

### Binary layout verification

- N/A: this is host frontend state and worker dispatch.

## 2026-08-20 — `src/ruzu/src/util/game.rs` and `src/ruzu/src/uisettings.rs` vs Eden `src/qt_common/util/game.{h,cpp}` and `src/yuzu/uisettings.h` metadata reset

### Intentional differences

- Rust reports recursive-removal errors through `std::io::Error` and GTK message dialogs; Eden
  uses `Common::FS::RemoveDirRecursively` and `QtCommon::Frontend` dialogs.
- The reload-pending flag is a module-level `AtomicBool` next to the frontend settings because
  Ruzu's cloneable `UISettings::Values` cannot directly contain an atomic member.

### Unintentional differences (to fix)

- None. `ResetMetadata` now removes the complete Ruzu `cache/game_list` directory, including the
  stale `<title-id>.pv.txt` Add-ons cache, and marks the game-list reload pending after success.

### Missing items

- None for metadata-cache removal and reload-pending signaling.

### Binary layout verification

- N/A: cache entries are host files; the focused test verifies complete directory removal.

## 2026-08-20 — `src/ruzu/src/configuration/configure_filesystem.rs` vs Eden `src/yuzu/configuration/configure_filesystem.{h,cpp}` metadata-reset action

### Intentional differences

- The GTK button resolves its transient parent from the live widget root before calling the shared
  utility; Eden passes its `ConfigureFilesystem` widget through the global frontend dialog owner.

### Unintentional differences (to fix)

- None. The button now calls the shared metadata reset instead of logging an unavailable-action
  placeholder, and the main-window apply callback consumes the resulting reload-pending flag.

### Missing items

- None for `ConfigureFilesystem::ResetMetadata`.

### Binary layout verification

- N/A: this is host UI dispatch.

## 2026-08-20 — `src/hid_core/src/resources/ring_lifo.rs` vs Eden `src/hid_core/resources/ring_lifo.h`

### Intentional differences

- Rust uses the `LifoState` trait to express the C++ template requirement that every state expose
  `sampling_number`; this avoids an untyped raw-layout cast and does not change LIFO ownership.
- Rust bounds a corrupt `buffer_tail` to the backing array instead of reproducing C++ undefined
  behavior; the existing diagnostic remains available through `RUZU_TRACE_LIFO_CORRUPTION`.

### Unintentional differences (to fix)

- None. `write_next_entry` now publishes `new_state.sampling_number << 1` exactly like Eden. The
  previous `previous_atomic_marker + 1` calculation could publish an odd marker, which newer
  Nintendo SDK readers treat as an in-progress write and retry indefinitely.

### Missing items

- None for `AtomicStorage` and `Lifo` behavior.

### Binary layout verification

- PASS: `AtomicStorage` and `Lifo` remain `repr(C)` with unchanged fields; the full HID shared
  memory layout test passes, and focused tests verify the even marker and source sample contract.

## 2026-08-20 — `src/hid_core/src/resources/shared_memory_format.rs` vs Eden `src/hid_core/resources/shared_memory_format.h`

### Intentional differences

- The concrete shared-memory state types implement Rust's `LifoState` trait at their LIFO
  instantiation owner; Eden's C++ template accesses the same `sampling_number` members directly.

### Unintentional differences (to fix)

- None introduced by the atomic-publication correction.

### Missing items

- None for the LIFO state sampling accessors.

### Binary layout verification

- PASS: trait implementations add no fields or vtables to the state values, and
  `shared_memory_layout_matches_upstream` passes.

## 2026-08-20 — `src/hid_core/src/resources/six_axis/seven_six_axis.rs` vs Eden `src/hid_core/resources/six_axis/seven_six_axis.{h,cpp}`

### Intentional differences

- `SevenSixAxisState` converts its unsigned sampling number to `i64` for the common Rust
  `LifoState` interface; `as` preserves the underlying two's-complement bit pattern.

### Unintentional differences (to fix)

- None introduced by the LIFO marker correction.

### Missing items

- The pre-existing incomplete `SevenSixAxis::on_update` integration remains outside this fix.

### Binary layout verification

- PASS: the state remains `repr(C)` and its existing `0x48` size assertion is unchanged.

## 2026-08-20 — `src/hid_core/src/resources/npad/npad.rs` vs Eden `src/hid_core/resources/npad/npad.{h,cpp}` prefill regression

### Intentional differences

- Rust regression tests observe the shared-memory result directly after activation; Eden has no
  matching C++ unit test in the ported source tree.

### Unintentional differences (to fix)

- None. The prefill expectation now reflects Eden's exact recurrence: each empty state derives
  from the preceding atomic marker and the marker is twice the state sample.

### Missing items

- None for `NPad::WriteEmptyEntry` in this verification slice.

### Binary layout verification

- PASS: no Npad production struct changed; the full HID layout test and all Npad tests pass.

## 2026-08-20 — `src/core/src/hle/service/aoc/addon_content_manager.rs` vs Eden `src/core/hle/service/aoc/addon_content_manager.{h,cpp}`

### Intentional differences

- Rust serializes the returned `u32` add-on IDs explicitly with `to_le_bytes`; Eden copies the
  native little-endian `u32` vector into the HIPC map-alias output buffer with `std::memcpy`.
- The Rust service obtains `ClientProcessId` from `HLERequestContext::get_pid`; Eden's CMIF
  serializer supplies the same request PID through its typed `ClientProcessId` argument.

### Unintentional differences (to fix)

- The pre-existing Rust constructor still initializes `add_on_content` as an empty vector instead
  of calling Eden's `AccumulateAOCTitleIDs` over the content provider. Restoring that requires the
  content-provider enumeration integration and is separate from the missing command dispatch that
  produced the invalid CMIF response.
- The pre-existing `GetAddOnContentBaseId` implementation always takes Eden's no-control-metadata
  fallback because the required `PatchManager` integration is not wired at the system level.

### Missing items

- None for command 3 dispatch: `ListAddOnContent` now parses `offset` and `count`, forwards the
  client PID, writes the returned IDs to output buffer 0, and returns the output count.

### Binary layout verification

- PASS: add-on IDs are emitted as packed four-byte little-endian values, matching Eden's raw
  `u32` buffer copy; no shared structs changed.

## 2026-08-20 — `src/shader_recompiler/src/frontend/control_flow.rs` vs Eden `src/shader_recompiler/frontend/maxwell/control_flow.{h,cpp}`

### Intentional differences

- Rust represents upstream `Shader::Exception` subclasses as typed panic payloads at the CFG
  boundary. The Vulkan and OpenGL pipeline-cache owners catch those exact payload types at the
  same `catch (const Shader::Exception&)` boundaries used by Eden.
- Rust stores CFG links as stable vector indices instead of pointers allocated by `ObjectPool`;
  method ownership and branch/link ordering remain in the matching control-flow module.
- `to_cfg_blocks` converts the upstream-shaped flow graph into the existing Rust translation
  consumer's index-based `CfgBlock` representation. The older slice-based `build_cfg` entry point
  remains for callers that already own decoded instruction words.

### Unintentional differences (to fix)

- None in the corrected exception/lifecycle slice. `PRET`, constant-buffer branches, unsupported
  indirect branches, invalid stack pops, invalid split addresses, and unsupported `EXIT` forms now
  raise the same shader exception categories as Eden instead of killing the GPU worker with an
  untyped panic or silently continuing.

### Missing items

- `PRET` flow analysis itself remains unimplemented, matching Eden. The pipeline cache now rejects
  that shader without terminating the GPU thread.

### Binary layout verification

- N/A: CFG nodes are host-only analysis structures and are not copied to guest or GPU memory.

## 2026-08-20 — `src/common/src/scm_rev.rs` and `src/common/build.rs` vs Eden `src/common/scm_rev.{h,cpp.in}` and `CMakeModules/GenerateSCMRev.cmake`

### Intentional differences

- Cargo runs a Rust build script instead of CMake `configure_file`; both publish the full revision,
  branch, ten-character revision-plus-branch build version, build name, and detected native C++
  compiler identity as build-time constants.
- Source archives without Git metadata fall back to `unknown-detached`; CI/package builds can
  provide `GIT_REV` and `GIT_BRANCH` explicitly. Eden obtains equivalent overrides through its
  CMake SCM module.
- Ruzu currently exposes only the SCM/compiler constants consumed by its frontend. Eden's update
  feed, nightly-build, build-date, and custom title-format constants remain outside this slice.

### Unintentional differences (to fix)

- None in the development-build identity slice. The generated values on this host are
  `08b3fb5169-main` and `GNU 13.3.0`; the compiler string is detected, not hard-coded.

### Missing items

- Stable/nightly release tag formatting and auto-update endpoint constants are not used by Ruzu.

### Binary layout verification

- N/A: Rust string constants replace generated C++ character arrays and are not guest-visible.

## 2026-08-20 — `src/ruzu/src/boot.rs` vs Eden `src/yuzu/main_window.cpp` `MainWindow::BootGame`

### Intentional differences

- The boot thread sends a typed `TitleChanged` event to GTK's main thread because GTK widgets may
  only be changed by their owning thread; Eden computes the same values on its Qt GUI thread.

### Unintentional differences (to fix)

- None in the running-title metadata slice. Ruzu reads the loader title, lets
  `PatchManager::GetControlMetadata` replace it with the selected add-on NACP title/version,
  applies Eden's filename fallback and translated 64/32-bit suffix, obtains the renderer vendor,
  logs the boot identity, and publishes it before disk-cache construction.

### Missing items

- None for the default running-title fields.

### Binary layout verification

- N/A: title metadata is host UI text.

## 2026-08-20 — `src/ruzu/src/main_window.rs` vs Eden `src/yuzu/main_window.{h,cpp}` `UpdateWindowTitle`

### Intentional differences

- Ruzu formats the default title directly instead of supporting Eden's optional
  `TITLE_BAR_FORMAT_IDLE` override, which has no Ruzu configuration owner.
- The same handler exists in each platform-specific GTK launch loop because those loops own their
  native render surfaces; all three consume the identical `TitleChanged` event.

### Unintentional differences (to fix)

- None. Idle, versioned-running, versionless-running, and shutdown-reset title ordering matches
  Eden: `Ruzu | build-version | compiler | game | optional-version | GPU vendor`.

### Missing items

- User-defined idle title-bar format overrides are not ported.

### Binary layout verification

- N/A: window titles are host UI strings.

## 2026-08-20 — `src/ruzu/src/game_list.rs` vs Eden `src/qt_common/game_list/worker.cpp` and `src/core/file_sys/program_metadata.{h,cpp}`

### Intentional differences

- Ruzu adds a frontend-only Architecture column immediately after File type; Eden has no matching
  column. Application architecture comes from the selected/patched ExeFS `main.npdm` bit, KIP
  architecture comes from its header, and standalone NRO/NSO uses Eden's 64-bit default program
  metadata.
- Architecture is cached independently as `<title-id>.arch.txt`. This leaves Eden's
  `<title-id>.pv.txt` add-on cache byte-compatible and lets warm scans read only the small cached
  label. A manual refresh removes the complete cache directory, including both files.
- The frontend renders the architecture names as lowercase `aarch64`/`aarch32`; cached labels
  written by earlier Ruzu builds are normalized while loading, without changing the cache format.

### Unintentional differences (to fix)

- None in the `pv.txt` format: enabled/disabled names, version parentheses, packed-update file
  type substitution, update filtering, UTF-8 encoding, and newline joining match Eden.

### Missing items

- Eden has no architecture-column behavior to port. Files whose executable metadata cannot be
  recovered display `Unknown`.

### Binary layout verification

- PASS: `ProgramMetadata::is_64_bit_program` reads the existing NPDM bit; no guest or container
  binary structure was changed.

## 2026-08-21 — `src/shader_recompiler/src/backend/spirv/emit_spirv_special.rs` vs Eden `src/shader_recompiler/backend/spirv/emit_spirv_special.cpp`

### Intentional differences

- Ruzu uses `rspirv::dr::Builder` result IDs and Rust `match` expressions in place of Sirit's
  `EmitContext` helpers and the C++ `switch`; the emitted ordered floating-point comparisons,
  selection merge, conditional branch, and `OpKill` ordering are the same.
- Ruzu checks host-side SPIR-V IDs against zero and treats a missing position output as a no-op;
  Eden uses `Sirit::ValidId` for fragment colors and assumes its declared vertex outputs are valid.
- Ruzu derives the clip-distance-written mask once from `Program::info.stores` and keeps it in the
  per-compilation SPIR-V context; Eden uses a header-level `std::bitset<8>`. The emitted prologue
  still initializes exactly the clip-distance components not written by the shader, while the Rust
  ownership prevents state leaking between concurrent shader compilations.
- Unsupported geometry streams panic in Rust where Eden throws `NotImplementedException`.

### Unintentional differences (to fix)

- None in the reviewed prologue/epilogue slice: dual-source fragment outputs, component-aware
  generic varyings, unwritten clip distances, and the fragment alpha test follow Eden's ordering.

### Missing items

- None in the reviewed prologue/epilogue slice.

### Binary layout verification

- N/A: this change emits SPIR-V instructions and does not alter a serialized host structure.

## 2026-08-21 — `src/shader_recompiler/src/runtime_info.rs` vs Eden `src/shader_recompiler/runtime_info.h`

### Intentional differences

- Rust stores active transform-feedback entries in a `Vec`; Eden uses a fixed 256-entry array.
  `xfb_count` remains the authoritative bound in both implementations.

### Unintentional differences (to fix)

- None in the reviewed runtime-state slice: `TransformFeedbackVarying::stream` and
  `RuntimeInfo::dual_source_blend` now have the same owners and defaults as Eden.

### Missing items

- None in the reviewed runtime-state slice.

### Binary layout verification

- N/A: `RuntimeInfo` is host-side compiler state and is not copied as a guest binary payload.

## 2026-08-21 — `src/video_core/src/transform_feedback.rs` vs Eden `src/video_core/transform_feedback.{h,cpp}`

### Intentional differences

- Invalid attribute indices are ignored safely in Rust; Eden indexes its fixed array directly.

### Unintentional differences (to fix)

- None: generated varyings preserve `layout.stream`, and the complete Eden vector table through
  `gl_TexCoord[7]` is present.

### Missing items

- None in `MakeTransformFeedbackVaryings`.

### Binary layout verification

- PASS: `TransformFeedbackLayout` remains `repr(C)` with Eden's `stream`, `varying_count`, and
  `stride` field order; generated varying descriptors are host-side values.

## 2026-08-21 — `src/shader_recompiler/src/backend/spirv/spirv_emit_context.rs` vs Eden `src/shader_recompiler/backend/spirv/spirv_emit_context.{h,cpp}`

### Intentional differences

- SPIR-V construction uses `rspirv::dr::Builder` instead of Sirit.

### Unintentional differences (to fix)

- None in `DefineGenericOutput`: split component outputs and nonzero geometry transform-feedback
  stream decorations now match Eden.

### Missing items

- None in the reviewed generic-output slice.

### Binary layout verification

- N/A: this slice emits SPIR-V declarations and decorations.

## 2026-08-21 — renderer runtime-info propagation

Compared `src/video_core/src/renderer_vulkan/graphics_pipeline.rs` with Eden
`src/video_core/renderer_vulkan/vk_pipeline_cache.cpp`, and
`src/video_core/src/renderer_opengl/gl_shader_cache.rs` with Eden
`src/video_core/renderer_opengl/gl_shader_cache.cpp`.

### Intentional differences

- Rust maps the fixed pipeline key into owned `RuntimeInfo` values; Eden copies into fixed arrays.

### Unintentional differences (to fix)

- None in the reviewed fields: Vulkan propagates `attachment0_dual_source_blend`, and both Vulkan
  and OpenGL propagate transform-feedback `stream`.

### Missing items

- None in the reviewed runtime-info propagation slice.

### Binary layout verification

- N/A: these are host-side compiler inputs.

## 2026-08-21 — `src/shader_recompiler/src/pipeline_cache.rs` runtime identity vs Eden runtime shader state

### Intentional differences

- Ruzu hashes runtime compiler inputs for its Rust pipeline cache; Eden keys the equivalent state
  through its fixed pipeline cache key.

### Unintentional differences (to fix)

- None: `dual_source_blend` and transform-feedback `stream` now participate in Ruzu's runtime hash.

### Missing items

- None in the reviewed runtime-hash slice.

### Binary layout verification

- N/A: the value is a host-side cache identity hash.

## 2026-08-21 — `src/shader_recompiler/src/frontend/translate/load_store_attribute.rs` vs Eden `src/shader_recompiler/frontend/maxwell/translate/impl/load_store_attribute.cpp`

### Intentional differences

- Rust decodes instruction bit fields into integers and represents Eden's translation exceptions
  as panics.
- The Rust visitor stores the program header in an `Option`; generic `IPA` now requires it to be
  present, matching Eden's unconditional `env.SPH()` access.

### Unintentional differences (to fix)

- None in `IPA`: legacy interpolation, whole-vector effective `PixelImap` selection, the
  perspective fallback for an unused vector, `Sc` handling, multiplier ordering, and the
  saturated `FrontFace` rejection now match Eden.

### Missing items

- None in the reviewed `IPA` slice.

### Binary layout verification

- N/A: the instruction is decoded from the same bit positions, but no host struct is copied as a
  guest payload.

## 2026-08-21 — `src/shader_recompiler/src/ir/value.rs` vs Eden `src/shader_recompiler/frontend/ir/attribute.h`

### Intentional differences

- The active Rust IR represents an attribute as a checked numeric newtype instead of a C++ enum;
  the numeric values and range predicates remain upstream-owned contracts.

### Unintentional differences (to fix)

- The crate still contains a second, enum-based `Attribute` in `ir/attribute.rs`. Consolidating
  those pre-existing parallel IR representations is a structural refactor outside this runtime
  correction; `IsLegacyAttribute` was added to the active translation type so current users share
  one predicate.

### Missing items

- None in the reviewed generic/legacy classification slice.

### Binary layout verification

- N/A: attributes are host-side IR identifiers and are not raw-copied guest payloads.

## 2026-08-21 — `src/shader_recompiler/src/frontend/translate_program.rs` vs Eden `src/shader_recompiler/frontend/maxwell/translate_program.cpp`

### Intentional differences

- Rust invokes the active attribute newtype's `is_legacy` method; Eden imports
  `IR::IsLegacyAttribute` from `attribute.h`.

### Unintentional differences (to fix)

- None in the reviewed legacy-varying classification call sites; the duplicate private predicate
  was removed.

### Missing items

- None in the reviewed call-site slice.

### Binary layout verification

- N/A: this pass rewrites host-side IR instructions.
## 2026-08-20 — `src/core/src/hle/service/filesystem/filesystem.rs` vs Eden `src/core/hle/service/filesystem/filesystem.{h,cpp}`

### Intentional differences

- Ruzu adds an optional frontend-owned `sdmc_open_override`. `OpenSDMC` returns it when installed,
  while every content-cache, modification-root, size, and normal launch path remains owned by the
  upstream-equivalent `SDMCFactory`.
- `set_sdmc_open_override` is a narrow Ruzu extension used only for standalone NRO launches. An
  overwriting `create_factories` call clears it together with the upstream factories so a view
  cannot leak into a later launch.

### Unintentional differences (to fix)

- None in this slice. With no override installed, `open_sdmc` retains Eden's factory/null-device
  behavior.

### Missing items

- None for the per-launch SDMC override.

### Binary layout verification

- N/A: the added host-side `VirtualDir` does not alter a guest-visible or serialized structure.

## 2026-08-20 — `src/ruzu/src/homebrew_vfs.rs` vs Eden `src/core/file_sys/vfs/vfs_layered.{h,cpp}`

### Intentional differences

- Eden's `LayeredVfsDirectory` is read-only, so it remains unchanged. Ruzu's GTK frontend owns a
  separate writable two-layer view for homebrew: the standalone NRO's containing directory has
  first priority and the configured SDMC is the fallback.
- Creates and missing parent directories are routed to the homebrew layer. Existing fallback-only
  entries retain normal SDMC behavior. Directory enumeration recursively merges both layers and
  hides lower-priority entries shadowed by either a file or directory in the homebrew layer.
- Entry enumeration uses ordered Rust maps/sets for deterministic results; Eden's layered VFS uses
  an unordered set. The guest-visible set and priority are unchanged.
- Activation checks the NRO header through `AppLoaderNro::IdentifyType`, rather than trusting the
  filename extension. No symbolic-link, junction-point, or platform-specific filesystem API is
  required.

### Unintentional differences (to fix)

- None. The writable semantics are deliberately frontend-specific because changing Eden's
  read-only layered VFS would violate its contract.

### Missing items

- None for exposing sibling homebrew assets and writable nested paths.

### Binary layout verification

- N/A: the view contains host VFS handles and path components only.

## 2026-08-20 — `src/ruzu/src/boot.rs` and `src/ruzu/src/main.rs` vs Eden `src/yuzu/main_window.cpp` `MainWindow::BootGame` and `src/yuzu/main.cpp`

### Intentional differences

- After the upstream-equivalent filesystem factories are created and before `System::Load`, Ruzu
  detects a standalone NRO and installs its per-launch homebrew SDMC view. Eden has no equivalent
  boot hook and relies on files already being present in its configured SDMC.
- The GTK entry point declares `homebrew_vfs` as a private frontend module; Eden's excluded Qt
  frontend has no corresponding source file.

### Unintentional differences (to fix)

- None. Non-NRO boot ordering and filesystem behavior are unchanged.

### Missing items

- None for this boot integration.

### Binary layout verification

- N/A: this changes host-side launch wiring only.
## 2026-08-21 — workspace source layout vs Eden repository source layout

### Intentional differences

- Rust keeps each crate's conventional inner `src/` directory, so Eden's
  `src/video_core/foo.cpp` maps to Ruzu's `src/video_core/src/foo.rs`.
- Cargo manifests remain inside their crates, while the root `Cargo.toml` coordinates the
  workspace.
- The GTK frontend test for the quick-start action reaches the repository-level documentation
  through `../../../docs/quickstart.md`; Eden's excluded Qt frontend has different test ownership.

### Unintentional differences (to fix)

- None: all source crates now live under the top-level `src/` directory; scripts,
  documentation, externals, tools, and agent configuration remain at the repository root.

### Missing items

- None for the workspace layout migration.

### Binary layout verification

- N/A: this is a path-only structural migration and changes no guest-visible layout.

## 2026-08-21 — `src/ruzu/src/homebrew_vfs.rs` vs Eden `src/core/file_sys/vfs/vfs_layered.{h,cpp}` and `src/core/hle/service/filesystem/filesystem.cpp`

### Intentional differences

- Ruzu's frontend-owned writable SDMC view now treats an NRO directly inside a directory named
  `switch` as a conventional SD-card archive: the directory above `switch` becomes the writable
  upper layer. This exposes asset directories shipped beside `switch` without host links or a
  manual copy into the configured SDMC. Eden has no automatic host-package mount and continues to
  open only its configured `SDMCFactory` root.
- NROs in flat or per-application layouts retain the previous containing-directory root, and the
  configured SDMC remains the fallback layer in both cases.

### Unintentional differences (to fix)

- None in the reviewed package-root selection slice.

### Missing items

- None for conventional `<package>/switch/application.nro` asset visibility.

### Binary layout verification

- N/A: the change selects a host `VirtualDir` root and does not alter serialized or guest ABI
  structures.

## 2026-08-21 — `src/video_core/src/gpu.rs` and `src/video_core/src/gpu_thread.rs` vs Eden `src/video_core/gpu.{h,cpp}` and `src/video_core/gpu_thread.{h,cpp}`

### Intentional differences

- Ruzu exposes an idempotent `ThreadManager::shutdown` helper because Rust field destruction runs
  in declaration order. `Gpu::drop` invokes it explicitly to reproduce the relevant C++ reverse
  member destruction contract: `GPU::Impl::gpu_thread` is stopped and joined while `renderer` is
  still alive. Ruzu also stops the thread before freeing its boxed scheduler; Eden's scheduler is
  stored in-place and has a trivial destructor, so its storage remains within `GPU::Impl` while
  `gpu_thread` is destroyed.

### Unintentional differences (to fix)

- None in the reviewed GPU-thread lifetime slice. Previously, Rust could destroy renderer-owned
  state before requesting GPU-thread stop, causing a shutdown join hang, `SlotVector` panic, or
  allocator corruption.

### Missing items

- None for GPU-thread shutdown ordering.

### Binary layout verification

- N/A: the change affects host-thread lifecycle only.

## 2026-08-21 — `src/core/src/core.rs` vs Eden `src/core/core.{h,cpp}` (`System::Impl::ShutdownMainProcess`)

### Intentional differences

- Eden destroys `audio_core` before `gpu_core` and `CpuManager::Shutdown`. Ruzu retains
  `audio_core` until after `finalize_terminated_processes_after_cpu_shutdown`, because Rust kernel
  sessions can keep `IAudioRenderer` alive in the terminated-process table. Its finalizer waits
  for a signal from `AudioRenderSystemManager`; destroying `audio_core` at Eden's earlier point
  stops that worker first and deadlocks shutdown.

### Unintentional differences (to fix)

- None in the reviewed shutdown slice.

### Missing items

- None for GPU shutdown and delayed Rust session finalization ordering.

### Binary layout verification

- N/A: the change affects host subsystem lifetime only.

## 2026-08-21 — `src/common/src/settings.rs` vs Eden `src/common/settings.h` (`dd12266c`)

### Intentional differences

- Rust uses `cfg!(target_os = "windows")` for the setting's persistence flag instead of Eden's
  `_WIN32` preprocessor branch. The resulting platform behavior is identical.
- `enable_raw_input` was added to Ruzu's category visitor alongside the new setting. Its existing
  Rust declaration had incorrectly disabled persistence on every platform, while Eden persists it
  on Windows through the same settings linkage used by `disable_wgi_xinput`.

### Unintentional differences (to fix)

- None in the reviewed WGI/XInput settings slice.

### Missing items

- None for the `disable_wgi_xinput` setting introduced by Eden commit `dd12266c`.

### Binary layout verification

- N/A: these are host configuration values and are not copied into a guest-visible binary payload.

## 2026-08-21 — `src/input_common/src/drivers/sdl_driver.rs` vs Eden `src/input_common/drivers/sdl_driver.cpp` (`dd12266c`)

### Intentional differences

- Rust constructs temporary `CString` values before calling the SDL3 C API; Eden passes the SDL
  hint macros directly. Both set `SDL_JOYSTICK_RAWINPUT_CORRELATE_XINPUT` and `SDL_JOYSTICK_WGI`
  to `0` with `SDL_HINT_OVERRIDE`, only on Windows and only when the setting is enabled.

### Unintentional differences (to fix)

- None in the reviewed WGI/XInput SDL hint slice.

### Missing items

- None for the SDL behavior introduced by Eden commit `dd12266c`.

### Binary layout verification

- N/A: SDL hints alter host input-backend selection and serialize no guest data.

## 2026-08-21 — `src/ruzu/src/configuration/configure_input_advanced.rs` vs Eden `src/yuzu/configuration/configure_input_advanced.{cpp,ui}` (`dd12266c`)

### Intentional differences

- The excluded Qt frontend's `QCheckBox` is represented by Ruzu's GTK `CheckButton`; the label,
  tooltip, initial setting value, apply behavior, and Windows-only visibility match Eden.

### Unintentional differences (to fix)

- None in the reviewed WGI/XInput configuration-widget slice.

### Missing items

- None for the advanced-input control introduced by Eden commit `dd12266c`.

### Binary layout verification

- N/A: this is host GUI state only.
## 2026-08-21 — `src/core/src/hle/kernel/svc/svc_synchronization.rs` vs Eden `src/core/hle/kernel/svc/svc_synchronization.cpp` (`7731b5bc`)

### Intentional differences

- None in the `ResetSignal` logging-level slice.

### Unintentional differences (to fix)

- None. `ResetSignal` now logs routine calls at trace level, matching Eden's demotion from debug.

### Missing items

- None for this upstream commit.

### Binary layout verification

- N/A: the change affects host logging only.
## 2026-08-21 — `src/core/src/hle/service/acc/acc.rs` vs Eden `src/core/hle/service/acc/acc.cpp`

### Intentional differences

- Eden commit `a41a98028a` moved `acc:aa`, `acc:su`, `acc:u0`, and `acc:u1` into `acc.cpp` and
  deleted their dedicated source/header pairs. Ruzu now mirrors that ownership: the corresponding
  Rust implementations live in `acc.rs`, while `acc_aa.rs`, `acc_su.rs`, `acc_u0.rs`, and
  `acc_u1.rs` and their declarations in `acc/mod.rs` are removed.
- Rust uses a local macro only for the repeated, data-only service-framework plumbing. Each
  service name and command table remains declared in `acc.rs` beside its Eden counterpart.
- `Arc<Mutex<_>>`, `ResultCode`, and Rust enums replace C++ shared pointers, exceptions/results,
  and enum classes without changing service ownership or command behavior.

### Unintentional differences (to fix)

- None in the reviewed `a41a98028a` ACC consolidation and new `acc:e`, `acc:e:u1`, `acc:e:u2`, and
  `dauth:0` service-table slice.

### Missing items

- None introduced by this port. Pre-existing unimplemented ACC commands remain registered as
  stubs exactly where the Rust service framework represents Eden's null handlers.

### Binary layout verification

- PASS: user IDs, pin-code lengths, IPC scalar widths, and existing raw profile payload types are
  unchanged; this slice adds no new raw-copied structure.

## 2026-08-21 — `src/core/src/hle/service/{apm,audio,bpc,caps,es,friend,glue,grc,hid,lm,mnpp,ncm,ngc,nim,ns,nvdrv,olsc,pcie,pcv,psc,ptm,ro,tma,usb,wlan}` vs Eden commit `a41a98028a` service files

### Intentional differences

- `apm/apm_controller.rs`, `apm/apm_interface.rs`, and
  `am/service/common_state_getter.rs` retain Eden's APM ownership and update ordering. Transparent
  raw wrappers preserve unknown `PerformanceMode`, `PerformanceConfiguration`, and `CpuBoostMode`
  bit patterns instead of rejecting or normalizing values during Rust conversion.
- `audio/audio.rs` and `audio/audio_renderer_manager.rs` mirror Eden's applet/debug service tables
  and invalid-process-handle behavior. The unusual upstream registration of `audren:d` through
  `IAudioInManager` is preserved literally.
- `bpc/bpc.rs`, `caps/caps.rs`, `caps/caps_manager.rs`, `es/es.rs`, `friend/friend.rs`,
  `friend/friend_interface.rs`, `glue/ectx.rs`, `glue/glue.rs`, `grc/grc.rs`, `hid/hid.rs`,
  `hid/hid_system_server.rs`, `lm/lm.rs`, `ncm/ncm.rs`, `ngc/ngc.rs`, `nim/nim.rs`, `ns/ns.rs`,
  `ns/query_service.rs`, `nvdrv/mod.rs`, `nvdrv/nvdrv.rs`, `olsc/olsc.rs`, `pcie/pcie.rs`,
  `pcv/pcv.rs`, `psc/psc.rs`, `psc/time/service_manager.rs`, `ptm/ptm.rs`, `ro/ro.rs`, and
  `usb/usb.rs` keep the new service names, command IDs, command labels, and registration order from
  their same-named Eden `.cpp` owners.
- Eden's `mnpp_app` rename/split is mirrored by deleting `mnpp/mnpp_app.rs`, adding
  `mnpp/mnpp.rs`, and updating `mnpp/mod.rs`. The new Eden-owned modules are mirrored by
  `tma/mod.rs`, `tma/tma.rs`, `wlan/mod.rs`, and `wlan/wlan.rs`; `hle/service/mod.rs` only declares
  those modules.
- Firmware-gated service registration reads the installed firmware through Ruzu's existing
  `set::system_settings_server::get_firmware_version_impl` owner. Eden obtains the same major
  version through `FrontendCommon::FirmwareManager`, which is unavailable in the excluded Qt
  frontend and would violate crate ownership if copied into these service modules.
- `services.rs` preserves Eden's service-thread ownership while adapting `unique_ptr` and thread
  launch to Rust's existing server-manager lifecycle.

### Unintentional differences (to fix)

- None in the reviewed command-table and registration slice. Eden's changes to `mii.cpp` and
  `glue/notif.cpp` are formatting-only, and its `spl.cpp` change only relocates explicit default
  destructor definitions; Rust requires no corresponding behavioral change.

### Missing items

- None introduced by this port. Commands represented by null handlers upstream remain named Rust
  stubs and deliberately return the service framework's unimplemented result.

### Binary layout verification

- PASS: the port adds service dispatch tables and scalar IPC replies only. Existing `repr(C)`
  payload declarations are unchanged, and empty CAPS/PSC responses return Eden's error before any
  payload serialization.

## 2026-08-21 — `src/core/src/hle/service/hle_ipc.rs` and `src/core/src/hle/service/sockets/{bsd,sfdnsres,sockets}.rs` vs Eden `src/core/hle/service/hle_ipc.h` and `src/core/hle/service/sockets/*.{h,cpp}`

### Intentional differences

- `hle_ipc.rs` represents Eden's missing copy/move handle as numeric handle `0`; checked slice
  access replaces C++ pointer/index access and prevents the same out-of-range crash.
- `sockets/bsd.rs` retains Eden's `Bsd` ownership for `is_user`, `SocketExempt`, and BSD command
  dispatch. Rust wrapper service types for `bsd:nu` and the additional socket services delegate to
  the same owner rather than duplicating BSD state.
- `sockets/sfdnsres.rs` and `sockets/sockets.rs` preserve Eden's service names, command IDs, and
  user/system split. Rust `Arc<Mutex<_>>` replaces C++ shared ownership for the shared network
  interface.

### Unintentional differences (to fix)

- None in the reviewed `a41a98028a` handle-safety and socket-service slice.

### Missing items

- None introduced by this port; null upstream socket handlers remain explicit stubs.

### Binary layout verification

- PASS: BSD request/reply integer widths and handle values are unchanged; no socket ABI structure
  was added or reordered.

## 2026-08-21 — `src/hid_core/src/resources/npad/{npad,npad_resource}.rs` vs Eden `src/hid_core/resources/npad/{npad,npad_resource}.cpp`

### Intentional differences

- `NPadResource::get_index_from_aruid` returns `Option<usize>` instead of Eden's sentinel
  `AruidIndexMax`. Invalid unregister requests now return before clearing state, preserving Eden's
  new guard exactly.
- `NPad::activate` returns success after logging an invalid ARUID because the following upstream
  null-data check also returns before the fallback index is consumed. `NPad::unregister` uses
  index zero only for the temporary controller cleanup, then calls the guarded resource owner,
  matching Eden's fallback and lifecycle ordering.

### Unintentional differences (to fix)

- None in the reviewed NPad ARUID guard slice.

### Missing items

- Eden also adds a null `shared_memory_format` guard to
  `abstracted_pad/abstract_battery_handler.cpp`. Ruzu's pre-existing abstract battery handler does
  not yet own or dereference an applet resource at all, so that crash path is already absent and
  this commit requires no executable Rust change there. Full abstract-battery integration remains
  pre-existing parity work, not a shortcut added by this port.

### Binary layout verification

- PASS: controller state and shared-memory payload declarations are unchanged. The added regression
  test verifies that unregistering an unknown ARUID cannot clear the first registered resource.

## 2026-08-21 — corrective audit of Eden `a41a98028a` homebrew-service prerequisites

### Intentional differences

- `src/core/src/file_sys/registered_cache.rs` stores Eden's
  `ContentProviderParsingFunction` and `VfsCopyFunction` as Rust `Fn` trait objects. The install
  callback accepts non-`'static` captures, preserving the flexibility of upstream `std::function`.
- The pre-existing Rust cache indexes remain `BTreeMap`s instead of Eden's
  `ankerl::unordered_dense::map`s. This makes enumeration deterministic but does not alter lookup,
  filtering or ownership; changing the container is outside this corrective method slice.
- `RegisteredCache::install_entry_xci` returns `ErrorMetaFailed` if an XCI has no secure-partition
  NSP. Eden dereferences the returned pointer without a null check; the valid-XCI path and install
  ordering are otherwise identical.
- `src/core/src/hle/service/filesystem/filesystem.rs` retains Ruzu's separate frontend-owned
  `FrontendManual` provider for content discovered inside ordinary game directories, alongside
  the newly ported engine-owned `ExternalContentProvider` for explicitly configured external
  update/DLC directories.
- `FileSystemController` stores Ruzu's concrete `Arc<RealVfsFilesystem>` rather than Eden's
  abstract VFS reference. BIS partition-storage behavior and result ordering remain the same.
- `src/core/src/core.rs::get_game_file_from_path` uses Rust host-path detection for extracted
  directories and the existing Rust VFS concatenation owner. It otherwise preserves Eden's
  `00` through `0F` scan order, early stop, directory name and `/main` fallback.
- If game-file opening fails, Rust logs the failure and does not call `set_game_card`; Eden passes
  the null VFS handle into `SetGameCard`. Valid current-game and configured-image paths preserve
  Eden's branch ordering, including applying the empty-path check only to the configured path.

### Unintentional differences (to fix)

- None in the re-reviewed NCM command/handler slice, registered-cache install/iteration/rights-ID
  slice, SDMC parsing path, filesystem-controller wrappers, or game-file opening path.
- This entry supersedes the broader “none” claim in the earlier `a41a98028a` service entry:
  subsequent line-by-line review found and fixed missing NCM prerequisites, NAX parsing,
  metadata filtering, registered installation and game-card setup.

### Missing items

- None among the 31 reviewed `PlaceholderCache`/`RegisteredCache` methods: `GetRightsID`, all four
  `InstallEntry` overloads and `IterateAllMetadata` are now present.
- None among the eight reviewed NCM interfaces: the same 12 commands have concrete handlers in
  Eden and Ruzu, and all remaining commands are registered stubs on both sides.
- `FileSystemController::GetExternalContentProvider`, BIS partition access, the standalone
  save-data controller, image-directory access and placeholder wrappers are now present in their
  upstream-owned controller file.

### Binary layout verification

- PASS: `ContentMetaKey` remains `repr(C)` and 0x10 bytes; padding is ignored when matching keys,
  as upstream does.
- PASS: `CNMTHeader`, `OptionalHeader` and `ContentRecord` remain deterministically initialized
  `repr(C)` payloads of 0x20, 0x10 and 0x38 bytes respectively. The new install path serializes the
  same fields and hashes the same first 1 MiB as Eden.
- N/A: filesystem controller accessors and `GetGameFileFromPath` add no guest-visible raw payload.

## 2026-08-21 — explicit service declarations vs Eden `a41a98028a` service owners

### Intentional differences

- Rust service-framework trait boilerplate remains implemented with the existing mechanical
  `impl_service_framework!` helper. It does not declare commands, own behavior or combine upstream
  files.

### Unintentional differences (to fix)

- None in the reviewed stub-service ownership slice. The port-local `define_stub_service!` macro
  has been removed from `audio`, `nvdrv`, `usb`, `psc`, `sockets`, `ptm`, `glue/ectx`, `wlan` and
  `bpc`; each upstream service type and command table is now explicit in its corresponding Rust
  owner.

### Missing items

- None introduced by expanding these declarations. Null Eden handlers remain explicit Rust
  unimplemented handlers with the same command IDs and labels.

### Binary layout verification

- N/A: this ownership correction changes declarations only and adds no raw-copied structure.
## 2026-08-21 — `src/core/src/hle/service/nim/nim.rs` vs Eden `src/core/hle/service/nim/nim.{h,cpp}` at `5c54abf353`

### Intentional differences

- Eden's `std::jthread` plus stop token is represented by a `JoinHandle` paired with a shared
  `AtomicBool`. `cancel_impl` requests stop before joining, and `Drop` performs cancellation before
  closing the completion event, preserving the upstream lifecycle order.
- Eden stores service state directly because its IPC service object is mutable through C++ object
  ownership. Rust uses `Mutex` for the worker and response bytes and `AtomicU32` for the error code,
  allowing the same service object to satisfy `SessionRequestHandler: Send + Sync` without moving
  ownership to another module.
- `ServiceContext` owns the completion `Event`; its copy-handle bridge supplies the readable event
  returned beside the new async IPC interface. This is Ruzu's existing equivalent of Eden's
  `KEvent*`/`KReadableEvent*` ownership.
- `Prepare` logs invalid UTF-8 paths lossily because Rust logging requires text. The original bytes
  are otherwise unused, just as Eden ignores the POST buffer and does not execute a real request.
- `nim:eca`, `IShopServiceAccessServer`, and `IShopServiceAccessor` were pre-existing direct-method
  stubs on `main` but were not registered as dispatchable service frameworks. They are wired in
  their existing `nim.rs` owner so Eden's new async implementation is reachable through the same
  interface chain; unrelated `nim`, `nim:shp`, and `ntc` parity remains outside this commit.

### Unintentional differences (to fix)

- None in the reviewed async-shop slice: command IDs, cancellation/join ordering, buffer locking,
  offset clamping, error-code updates, dummy `{}` response, event clearing/signaling, and returned
  copy/move objects match Eden.

### Missing items

- `Request` remains deliberately stubbed to a two-byte JSON object, exactly as in Eden commit
  `5c54abf353`; no network download is performed.

### Binary layout verification

- PASS: IPC outputs retain Eden's `u64` size/read count and `u32` error code widths. Download data
  is copied as bytes into the caller-provided output buffer; no host struct is raw-copied.

## 2026-08-21 — `src/core/src/hle/service/acc/{profile_manager.rs,acc.rs}` vs Eden `src/core/hle/service/acc/{profile_manager.cpp,acc.cpp}`

### Intentional differences

- Eden creates the automatic first user and the `BeginUserRegistration` user with the branded
  name `Eden`; Ruzu uses the direct product-name adaptation `ruzu`. Existing saved profiles are
  parsed without renaming, so a user-selected or migrated name is never overwritten.

### Unintentional differences (to fix)

- None in the reviewed default-profile creation paths: both paths generate a random non-null UUID,
  construct a fixed-size zero-padded profile name, create the user, and preserve upstream ordering.

### Missing items

- None for default profile naming or `BeginUserRegistration` naming.

### Binary layout verification

- PASS: `ProfileUsername` remains 32 bytes. The four ASCII bytes `ruzu` are followed by 28
  deterministic zero bytes, matching the upstream fixed-size payload contract.

## 2026-08-21 — `dist` Windows packaging vs Eden `dist/{installer.nsi,yuzu.manifest,eden.ico}`

### Intentional differences

- Ruzu stages its Rust executables and the dynamic `x64-windows-ruzu` vcpkg GTK/GLib runtime;
  Eden's installer consumes an already binplaced Qt directory. `package-windows.ps1` owns that
  extra staging step because Ruzu has no CMake/binplace packaging stage.
- The application installs to `%LOCALAPPDATA%\Programs\Ruzu`, keeping executable files separate
  from Ruzu's `%APPDATA%\ruzu` user data. Uninstalling therefore removes the program directory but
  deliberately preserves keys, firmware, saves, configuration and caches.
- File types are registered through the `Ruzu.SwitchFile` OpenWith ProgID instead of taking over
  each extension's default handler. Uninstall removes only Ruzu's own registry values.
- The Ruzu manifest fixes the malformed long-path namespace in the source manifest and embeds it,
  together with the Ruzu icon, through the Rust crate's Windows resource build step.

### Unintentional differences (to fix)

- None found by static validation of the adapted installer, manifest and resource definition.

### Missing items

- The installer has not yet been executed on a native Windows host; MSVC resource compilation,
  vcpkg runtime staging, NSIS generation, install, launch and uninstall still require that test.

### Binary layout verification

- PASS: `dist/ruzu.ico` has a Windows ICO header and seven image sizes from 16 through 256 pixels.
- PASS: the XML manifest parses successfully and uses resource ID 1/type 24, the standard
  `CREATEPROCESS_MANIFEST_RESOURCE_ID` application-manifest slot.

## 2026-08-21 — `src/ruzu/src/{main.rs,main_window.rs,gui_settings.rs,uisettings.rs,configuration/qt_config.rs}` vs Eden `src/{yuzu/main.cpp,yuzu/main_window.cpp,qt_common/gui_settings.cpp,qt_common/config/uisettings.h}`

### Intentional differences

- GTK dialogs are asynchronous, so Ruzu chains migration, the missing-key question, key
  installation and the Wayland check through completion callbacks. Eden obtains the same ordering
  from blocking `QMessageBox::exec()` and its synchronous file chooser.
- `GDK_BACKEND=x11` is Ruzu's GTK equivalent of Eden's `QT_QPA_PLATFORM=xcb`. Both are set before
  constructing the toolkit application, only when the persisted preference is enabled and no
  explicit backend environment override is present.
- `gui_settings.rs` lives in the Ruzu frontend crate because Ruzu has no `qt_common` crate. It keeps
  the upstream filename (`gui_config.ini`), key (`gui_force_x11`) and method ownership together.
- The restart text substitutes the Ruzu product name for Eden. The Wayland warning text, choices,
  default X11 action and “Don't show again” behavior otherwise match upstream.

### Unintentional differences (to fix)

- None in the reviewed missing-key and Linux-backend startup slice.

### Missing items

- None for detection, warning suppression, X11 preference persistence or early backend selection.

### Binary layout verification

- N/A: these frontend settings are textual INI booleans and no raw structure is serialized.

## 2026-08-21 — `src/ruzu/src/user_data_migration.rs` vs Eden `src/yuzu/user_data_migration.{h,cpp}`

### Intentional differences

- Ruzu's migration policy remains the previously documented non-destructive, selective GTK flow.
  The first page now exposes `No migration` as a method instead of a separate `Start Fresh`
  response, clears and disables Firmware/Keys for that method, and has a single `Next` action.
- Completing `No migration` records the explicit one-time prompt marker and resumes the normal
  startup prerequisite chain, which presents Eden's missing-key question when appropriate.

### Unintentional differences (to fix)

- None in the requested first-page interaction.

### Missing items

- Per-game migration remains hidden as documented by the existing implementation.

### Binary layout verification

- N/A: the changes affect GTK state and the existing text marker only.

## 2026-08-21 — external-content settings and `FileSystemController` vs Eden

### Intentional differences

- `src/ruzu/src/configuration/configure_general.rs` uses a GTK `ListBox` and asynchronous native
  folder chooser in place of Qt's `QListWidget` and blocking `QFileDialog`; list order, duplicate
  rejection, native trailing separator and apply-time comparison are preserved.
- `RealVfsFilesystem::arc_open_directory` currently constructs a VFS directory even for a missing
  host path. `FileSystemController::create_factories` therefore performs `Path::is_dir` before the
  VFS call to reproduce Eden's null result for an invalid configured directory.
- Ruzu requests its existing game-list worker rebuild directly after applying a changed directory
  list. This is the GTK equivalent of Eden's `ExternalContentDirsChanged` signal followed by
  `OnGameListRefresh`.

### Unintentional differences (to fix)

- None in the implemented persistence, explicit refresh or engine registration path.

### Missing items

- Eden installs a `QFileSystemWatcher` on external-content roots so later host filesystem changes
  trigger a metadata reset automatically. Ruzu currently detects those changes on the toolbar
  refresh or the next game-list rebuild; it has no directory watcher yet.

### Binary layout verification

- N/A: external paths use a textual QSettings-compatible array. The provider registration adds no
  guest-visible raw structure.

## 2026-08-21 — `src/ruzu/src/util/content.rs` and firmware menu vs Eden `src/qt_common/util/content.{h,cpp}` / `src/yuzu/main_window.cpp`

### Intentional differences

- GTK file selection is asynchronous. Once a source is returned, both paths converge on the same
  synchronous copy and firmware-only integrity verification routine, preserving Eden's ordering.
- Ruzu uses the Rust `zip` crate instead of `JlCompress`; `ZipFile::enclosed_name` additionally
  rejects entries that escape the fixed `ruzu/firmware` temporary root.
- The success message reports the number of verified NCA files. Eden reports the installed
  firmware display version, whose frontend lookup still depends on Ruzu's not-yet-faithful
  installed SystemVersion reader.

### Unintentional differences (to fix)

- None in source selection, direct-versus-recursive NCA discovery, extraction cleanup, copy order
  or firmware-only integrity verification.

### Missing items

- Displaying the installed firmware version requires replacing Ruzu's hardcoded
  `get_firmware_version_impl` with Eden's SystemVersion archive lookup; that prerequisite is
  outside this frontend menu slice.

### Binary layout verification

- N/A: ZIP extraction and firmware copying operate on files, not raw-copied payload structures.

## 2026-08-21 — `UISettings::enable_gamemode` ownership vs Eden `src/qt_common/config/uisettings.h`

### Intentional differences

- None. The obsolete standalone Rust `configure_linux_tab.rs` owner was removed because current
  Eden exposes Gamemode and X11 as `UiGeneral` settings in `ConfigureGeneral`.

### Unintentional differences (to fix)

- None in ownership, platform default or row ordering: the MSVC default is false, other targets
  default true, and Gamemode follows the profile prompt.

### Missing items

- Ruzu does not yet have Eden's `qt_common/gamemode.cpp` DBus activation owner; this pre-existing
  runtime integration gap is separate from the corrected setting ownership and UI placement.

### Binary layout verification

- N/A: the value is a textual frontend boolean.

## 2026-08-21 — `src/core/src/file_sys/fssystem/compression_configuration.rs` vs Eden `src/core/file_sys/fssystem/fssystem_compression_configuration.{h,cpp}`

### Intentional differences

- Ruzu calls the safe Rust `lz4_flex::decompress_into` API in place of Eden's
  `Common::Compression::DecompressDataLZ4`; both require the decompressed byte count to equal the
  requested destination size.

### Unintentional differences (to fix)

- None. The invalid `cfg(feature = "lz4")` gate was removed: `lz4_flex` is an unconditional core
  dependency and NCA LZ4 decompression is now active in every build, matching Eden.

### Missing items

- None for the NCA decompressor selection or destination-size validation path.

### Binary layout verification

- N/A: compressed bytes are decoded into caller-owned byte slices; no Rust structure is copied as
  a guest payload.

## 2026-08-21 — `src/core/src/hle/service/ns/language.rs` and `src/core/src/hle/service/set/settings_types.rs` vs Eden `src/core/hle/service/ns/language.{h,cpp}` and `src/core/hle/service/set/settings_types.h`

### Intentional differences

- Eden's partially initialized fixed-size Thai and Polish priority arrays zero-initialize their
  remaining enum slots. Rust arrays require every element explicitly, so Ruzu spells those zero
  values as trailing `ApplicationLanguage::AmericanEnglish` entries.

### Unintentional differences (to fix)

- None. Polish and Thai enum values, language codes, conversions, priority-list selection, and
  Eden's exact aggregate-initialization result are now present.

### Missing items

- None in the reviewed language enum/conversion/priority-list slice.

### Binary layout verification

- PASS: `ApplicationLanguage` remains `repr(u8)` with Polish 16, Thai 17 and Count 18.
- PASS: `LanguageCode` remains `repr(u64)` with Eden's exact little-endian `pl` and `th` values.

## 2026-08-21 — `src/common/src/logging/backend.rs` vs Eden `src/common/logging.{h,cpp}`

### Intentional differences

- Ruzu sends entries through a background Rust channel, while current Eden writes synchronously to
  each backend. This existing threading difference is outside this dead-code cleanup slice.
- Ruzu shares the active color-console flag with the logging thread through `Arc<AtomicBool>`;
  Eden stores the equivalent atomic flag directly in `ColorConsoleBackend`.

### Unintentional differences (to fix)

- None in the reviewed state ownership. The abandoned Rust `LoggerImpl`, duplicate
  `ColorConsoleBackend`, unused stacktrace hook, and redundant `LoggerState::color_console_enabled`
  were removed. The live file backend and `LoggerState` remain the only active owners.

### Missing items

- Eden's platform-specific Windows debugger and Android logcat backends remain platform-deferred.
- Eden's `log_flush_line`, `extended_logging`, and username-censoring behavior is not part of this
  cleanup and still requires a dedicated parity pass.

### Binary layout verification

- N/A: logging state is host-only and is not serialized or exposed to guest memory.

## 2026-08-21 — removal of `src/web_service/src/telemetry_json.rs` vs current Eden `src/web_service/`

### Intentional differences

- None. This is a structural correction to the current Eden source tree rather than a Rust
  adaptation.

### Unintentional differences (to fix)

- None. Ruzu's `telemetry_json.rs` was an incomplete port from an older source tree: current Eden
  has no `telemetry_json.{h,cpp}`, Ruzu had no production caller, and both HTTP submission methods
  were explicit stubs. The module and its public export were removed.

### Missing items

- None relative to current Eden's `web_service` file list.

### Binary layout verification

- N/A: the removed component was host-only JSON state.

## 2026-08-21 — dead-code cleanup in `src/common/src/heap_tracker.rs` vs Eden `src/common/heap_tracker.{h,cpp}`

### Intentional differences

- The active Ruzu implementation currently uses two safe `BTreeMap` indexes where Eden owns two
  intrusive red-black trees over each `SeparateHeapMap`. This is an existing structural and
  performance divergence and remains explicit parity debt.

### Unintentional differences (to fix)

- None introduced by this cleanup. The removed `SeparateHeapMap`, `AddrNode`, `TickNode`,
  `HeapTrackerInner`, comparators, and partial `addr_tree` helpers formed a separate abandoned
  implementation that was never constructed or referenced by the active `HeapTracker`.

### Missing items

- A future parity slice must replace the active `BTreeMap` representation with the same dual-tree
  ownership model as Eden; retaining an unused partial tree beside it did not provide that parity.

### Binary layout verification

- N/A for the removed host-only structures. The active mapping records are not copied to guest
  memory or serialized.

## 2026-08-21 — `src/dedicated_room/src/main.rs` announcement credentials vs Eden `src/dedicated_room/yuzu_room.cpp`

### Intentional differences

- Ruzu retains the historical setting field names `yuzu_username` and `yuzu_token`; they are the
  existing Rust equivalents consumed by `AnnounceMultiplayerSession`.

### Unintentional differences (to fix)

- None in this slice. Before constructing the verification backend and announcement session, Ruzu
  now writes `web_api_url`, username, and token to global settings in the same branches and order as
  Eden. Display tokens publish the decoded token directly instead of assigning it to an otherwise
  unread local variable.

### Missing items

- None for dedicated-room announcement credential propagation.

### Binary layout verification

- N/A: credentials are host strings and are not raw guest payloads.

## 2026-08-21 — current program ID in `src/common/src/settings.rs` / `src/core/src/core.rs` vs Eden `src/common/settings.{h,cpp}` / `src/core/core.cpp`

### Intentional differences

- Ruzu stores the process-global ID in `AtomicU64` because Rust global mutable state must be
  synchronized. Eden uses a plain file-local `u64`; relaxed atomic operations preserve the same
  value semantics without adding ordering to unrelated emulator state.

### Unintentional differences (to fix)

- None. `set_current_program_id` and `get_current_program_id` now belong to `settings.rs`, and
  `System::load` publishes the loaded process ID immediately after updating its runtime ID, at the
  corresponding point in Eden's application-load flow.

### Missing items

- None for this settings prerequisite.

### Binary layout verification

- N/A: this is host-global scalar state and is not serialized or copied to guest memory.

## 2026-08-21 — macro dumping in `src/video_core/src/macro_engine/macro_engine.rs` vs Eden `src/video_core/macro.{h,cpp}`

### Intentional differences

- `dump_to_directory` isolates the mechanical file write so the filename and payload can be tested
  without mutating Ruzu's process-global dump path. It remains private in the upstream-owned macro
  module and does not change method ownership.
- Rust uses `bytemuck::cast_slice` for the same native `u32` byte representation that Eden writes
  through `reinterpret_cast<const char*>`.

### Unintentional differences (to fix)

- None. Newly compiled macros now read `CacheInfo::hash` after execution and dump when
  `dump_macros` is enabled, using Eden's exact program-ID/hash/variant filename and payload.

### Missing items

- None in the reviewed macro dump path.

### Binary layout verification

- PASS: the regression test verifies that the `.macro` payload is the contiguous native-byte view
  of the original `u32` instruction span, matching Eden's `code.size_bytes()` write.

## 2026-08-21 — `src/shader_recompiler/src/pipeline_cache.rs` vs Eden Maxwell decode/translate ownership

### Intentional differences

- None for this cleanup.

### Unintentional differences (to fix)

- None. The unused Ruzu-only `maxwell_opcode_is_unknown` wrapper was removed. Opcode decoding
  remains owned by the control-flow and translation modules that consume it, matching Eden's
  direct `Decode` use rather than making the unrelated pipeline cache an extra owner.

### Missing items

- None introduced by this removal; the broader structured-control-flow parity work remains a
  separate implementation slice.

### Binary layout verification

- N/A: no guest-visible or serialized data changed.

## 2026-08-21 — `src/input_common/src/main_common.rs` vs Eden `src/input_common/main.{h,cpp}` mapping callback ownership

### Intentional differences

- Rust's callback captures the shared `Arc<Mutex<MappingFactory>>` rather than a raw `this`
  pointer. Consequently, the private `InputSubsystemImpl` methods receive that shared factory
  explicitly; their ownership and call chain still mirror Eden's `Impl` methods.

### Unintentional differences (to fix)

- None. `mapping_callback`, `register_engine`, and `register_input` now belong to
  `InputSubsystemImpl`, and every engine callback routes through `register_input` as Eden's
  `RegisterEngine` lambda does.

### Missing items

- `GCAdapter` and Android registration remain the already documented platform-specific gaps in
  this subsystem; they are not introduced by this callback correction.

### Binary layout verification

- N/A: this changes host callback ownership only.

## 2026-08-21 — `src/hid_core/src/frontend/emulated_console.rs` vs Eden `src/hid_core/frontend/emulated_console.{h,cpp}` motion path

### Intentional differences

- Input callbacks capture `Arc<Mutex<ConsoleStatus>>`, the configuration flag, and the immutable
  sensitivity instead of Eden's raw `this` pointer. This preserves callback-thread safety without
  moving console behavior out of its upstream-owned file.
- Ruzu's input factory always returns an `InputDevice` (a null device for an unavailable backend),
  so the explicit null-device branches in Eden are represented by normal callback installation.
- `ConsoleMotion::quaternion` uses this module's existing Rust `MotionInput::Quaternion`; it carries
  the same four scalar components and is host state rather than a raw guest payload.
- The private `motion_state` helper mechanically shares Eden's identical field projection between
  reload and callback paths while remaining in the same upstream-owned module.

### Unintentional differences (to fix)

- None in the reviewed file. Ruzu now owns both motion parameter slots and devices, restores the
  first player's configured motion source, adds the virtual-gamepad source, updates raw and
  emulated motion in callback order, resets rotations/quaternion on reload, and applies
  `motion_sensitivity` to `is_at_rest` exactly where Eden does.
- Callback keys now increment before insertion and therefore start at 1, matching Eden.
- Deleting an unknown callback now asserts instead of only logging, matching Eden's
  `ASSERT_MSG` contract.

### Missing items

- The downstream `ConsoleSixAxis` and `SevenSixAxis` resources do not yet consume this newly live
  console state in their update paths. That wiring belongs to those corresponding files and is a
  separate prerequisite-sensitive slice.

### Binary layout verification

- N/A: `ConsoleMotion` and `ConsoleMotionInfo` are synchronized host-side frontend state and are
  not copied raw into guest memory.

## 2026-08-21 — console six-axis ownership in `src/hid_core/src/resources/six_axis/console_six_axis.rs` / `resource_manager.rs` vs Eden counterparts

### Intentional differences

- Ruzu's `ControllerActivation` stores shared `Arc<Mutex<...>>` references in place of Eden's
  `ControllerBase` raw/reference members. `ConsoleSixAxis::new` receives the HID core and the
  resource manager supplies the applet resource during sampler initialization.
- The private `update_shared_memory` helper is a mechanical extraction of Eden's four assignments
  so their projection can be regression-tested without fabricating kernel-backed applet memory.

### Unintentional differences (to fix)

- None. `ConsoleSixAxis::on_update` now owns active-ARUID validation, activation validation,
  `EmulatedConsole::get_motion`, and the shared-memory projection. `ResourceManager::update_motion`
  only schedules the call, matching Eden's ownership boundary.
- The obsolete Ruzu-only `ConsoleMotionStatus` duplicate and the default status constructed by the
  resource manager were removed.
- Sampler initialization no longer assigns an applet resource to `SevenSixAxis`, matching Eden,
  which only assigns one to `ConsoleSixAxis`.

### Missing items

- `SevenSixAxis::on_update` still needs the `Core::System` timing/application-memory dependency
  owned by its Eden constructor. It remains a separate structural prerequisite and was not
  approximated in this slice.

### Binary layout verification

- PASS: the existing compile-time assertion still verifies
  `ConsoleSixAxisSensorSharedMemoryFormat` is `0x20` bytes; the focused test verifies the exact
  fields projected by Eden's update.

## 2026-08-21 — `src/core/src/file_sys/fs_path_utility.rs` vs Eden `src/core/file_sys/fs_path_utility.h` bounded backslash replacement

### Intentional differences

- Rust uses a zero-initialized `Vec<u8>` plus a bounded slice copy for Eden's temporary allocation
  and `Strlcpy`; both reserve the caller-provided remaining buffer length and terminate the copied
  source within that bound.

### Unintentional differences (to fix)

- None. The Windows-path backslash replacement now computes `replaced_src_len` from the supplied
  `path_len` minus the consumed source prefix, rather than ignoring `path_len` and sizing from
  `strlen(src)`. This matches Eden when the caller's source-buffer bound truncates the visible
  string.
- The Rust-only outer `relative_len` temporary was removed; `rlen` still advances `cur_pos` at the
  exact point where Eden consumes `relative_len`.

### Missing items

- None in the reviewed `PathFormatter::Normalize` backslash-replacement branch.

### Binary layout verification

- N/A: the regression test verifies bounded byte-copy and normalized output behavior; no struct
  layout changed.

## 2026-08-21 — `src/hid_core/src/frontend/input_converter.rs` vs Eden `src/hid_core/frontend/input_converter.{h,cpp}` analog conversion

### Intentional differences

- None beyond Rust's direct return value and `log` facade.

### Unintentional differences (to fix)

- None. `transform_to_analog` now accepts only `InputType::Analog`, copies properties and raw
  value, sanitizes without clamping, then applies Eden's second inversion step in the same order.

### Missing items

- None for `TransformToAnalog`; it unblocks the upstream-owned mouse-wheel path in
  `EmulatedDevices`.

### Binary layout verification

- N/A: `AnalogStatus` is host-side callback state. Tests cover the non-clamped range, deadzone,
  copied properties, and Eden's deliberately preserved inversion ordering.

## 2026-08-21 — `src/hid_core/src/frontend/emulated_devices.rs` vs Eden `src/hid_core/frontend/emulated_devices.{h,cpp}`

### Intentional differences

- Device callbacks capture `Arc<Mutex<DeviceStatus>>`, the atomic configuration flag, and the
  callback map rather than Eden's raw `this` pointer. State, callback, and method ownership remain
  in `EmulatedDevices`.
- Ruzu's input factory returns a null-object `InputDevice` when a backend is unavailable, so every
  array slot contains `Some(device)` after reload instead of requiring Eden's pointer-null checks.
- The private `assign_bit` helper mechanically represents Eden's `BitField::Assign` operations for
  keyboard modifiers and mouse buttons.

### Unintentional differences (to fix)

- None. Reload/unload now owns all mouse buttons, position, wheel axes, keyboard keys, and keyboard
  modifiers with Eden's exact parameter packages and callback order.
- Button toggle/lock transitions, configuration-mode suppression, modifier bit mapping, mouse
  projection, raw-value getters, notifications, and callback-key lifecycle now match upstream.

### Missing items

- None in the reviewed `EmulatedDevices` file.

### Binary layout verification

- PASS: the existing compile-time assertions retain `KeyboardKey` at `0x20`,
  `KeyboardModifier`/`MouseButton` at `0x4`, and `AnalogStickState` at `0x8`; focused tests verify
  the corresponding bit and numeric projections.

## 2026-08-21 — `src/common/src/random.rs` vs Eden `src/common/random.{h,cpp}`

### Intentional differences

- Rust represents `std::mt19937` with the local `Mt19937` type implementing the standard engine's
  state transition and tempering exactly.
- `fastrand` supplies the process-global host entropy in place of C++ `std::random_device`; both
  are cross-platform, nondeterministic host random sources and the upstream seed parameters remain
  intentionally ignored.

### Unintentional differences (to fix)

- None. `random32`, `random64`, and `get_mt19937` retain Eden's ownership and behavior, including
  the 32-bit `random_device::result_type` widened to `u64` by `random64`.

### Missing items

- None in the reviewed files.

### Binary layout verification

- N/A: no payload struct is serialized. A focused test verifies the MT19937 reference sequence and
  another verifies that `random64` preserves Eden's zero upper 32 bits.

## 2026-08-21 — `src/core/src/hle/kernel/k_process.rs` vs Eden `src/core/hle/kernel/k_process.{h,cpp}` ASLR load offset

### Intentional differences

- Ruzu retains its `is_hbl` argument and assignment because this frontend state is currently owned
  by `KProcess`; it follows the upstream parameters without changing their order.
- The Rust `match` returns the selected address directly instead of declaring a zero-valued local
  and assigning it in every switch arm. Flag mutation and address selection remain in the upstream
  order.

### Unintentional differences (to fix)

- None in the reviewed address-selection path. Every address-space base now includes
  `aslr_space_offset`, then adds `aslr_space_start` when constructing the process parameters.

### Missing items

- `load_from_metadata` still uses pool-size constants because its Rust signature does not yet carry
  Eden's `KernelCore` reference.
- Eden calls `InitializeInterfaces` before returning; Ruzu still creates the ARM interfaces later
  from `System::load`.

### Binary layout verification

- N/A: no serialized layout changed. The focused regression initializes the kernel slab allocator,
  loads a synthetic homebrew process with a nonzero page-aligned offset, and verifies its exact
  entrypoint.

## 2026-08-21 — `src/core/src/loader/deconstructed_rom_directory.rs` vs Eden `src/core/loader/deconstructed_rom_directory.{h,cpp}` ASLR load offset

### Intentional differences

- The additional Ruzu `is_hbl` state is forwarded after Eden's five load parameters; it does not
  alter the upstream ASLR calculation.

### Unintentional differences (to fix)

- None in the reviewed ASLR calculation. The selected seed is shifted by 12, masked with
  `0xfff000`, and passed to `KProcess` after the fast-memory base exactly as in Eden.

### Missing items

- Eden's NCE patch collection, patch-section size, and direct-mapped fast-memory base are not yet
  integrated, so the corresponding argument remains zero on Ruzu's current backends.

### Binary layout verification

- N/A: this slice passes scalar addresses only.

## 2026-08-21 — `src/core/src/loader/kip.rs` vs Eden `src/core/loader/kip.{h,cpp}` ASLR load offset

### Intentional differences

- Rust keeps the loader's virtual file because the `AppLoader` trait has no C++-style base-class
  file member; loader ownership is otherwise unchanged.
- Ruzu's internal `is_hbl = false` argument follows Eden's load parameters.

### Unintentional differences (to fix)

- None in the reviewed ASLR path. Seed selection, shift, mask, zero fast-memory base, and argument
  ordering now match Eden.

### Missing items

- None in the reviewed ASLR path.

### Binary layout verification

- N/A: this slice passes scalar addresses only.

## 2026-08-21 — `src/core/src/loader/nro.rs` vs Eden `src/core/loader/nro.{h,cpp}` ASLR load offset

### Intentional differences

- Ruzu's internal `is_hbl = false` argument follows Eden's load parameters.

### Unintentional differences (to fix)

- None in the reviewed ASLR calculation. The offset is generated after determining `image_size`
  and before process setup, with Eden's exact shift and mask.

### Missing items

- Eden's NCE patching, patch relocation, and direct-mapped fast-memory base remain unintegrated; the
  fast-memory argument therefore remains zero.

### Binary layout verification

- PASS: the existing compile-time assertions still verify the affected NRO, MOD, and asset header
  sizes; this scalar ASLR change does not alter them.

## 2026-08-21 — `src/common/src/intrusive_red_black_tree.rs` vs Eden `src/common/intrusive_red_black_tree.h` bidirectional iteration

### Intentional differences

- Pointer-based C++ iterator positions are represented by arena indices. Rust's immutable and
  mutable double-ended iterators therefore retain explicit front and back indices so mixed forward
  and reverse traversal cannot yield a node twice.
- `IntrusiveRedBlackTreeBaseNode` locates `self` in the arena before following its embedded node
  links; this linear lookup replaces the parent-pointer cast that Rust's arena representation
  cannot express safely.

### Unintentional differences (to fix)

- None. Immutable and mutable iterators now support reverse traversal, and base-node predecessor
  and successor accessors now follow the tree links instead of always returning `NONE`.

### Missing items

- None in the reviewed bidirectional iterator and base-node neighbor methods.

### Binary layout verification

- N/A: Ruzu deliberately uses indices rather than serializing Eden's host pointers. Focused tests
  cover forward, reverse, mixed, mutable, predecessor, and successor traversal without duplicates.

## 2026-08-21 — `src/audio_core/src/sink/cubeb_sink.rs` vs Eden `src/audio_core/sink/cubeb_sink.{h,cpp}` stream metadata ownership

### Intentional differences

- Rust keeps the Cubeb backend object beside a shared `SinkStreamHandle`; this replaces Eden's
  `unique_ptr<CubebSinkStream>` ownership while keeping the stream metadata on `SinkStream`.

### Unintentional differences (to fix)

- None in the reviewed ownership slice. The duplicate `name` and `stream_type` fields were removed
  from the Rust-only Cubeb wrapper; their canonical values remain on `SinkStream`, matching Eden's
  `CubebSinkStream` inheritance from `SinkStream`.

### Missing items

- None in the reviewed ownership slice.

### Binary layout verification

- N/A: the Rust wrapper is host-only state and is neither serialized nor copied to guest memory.

## 2026-08-21 — `src/core/src/hle/service/filesystem/filesystem.rs` vs Eden `src/core/hle/service/filesystem/filesystem.{h,cpp}` provider ownership

### Intentional differences

- Ruzu registers providers through its shared `ContentProviderUnion` rather than Eden's
  `Core::System::RegisterContentProvider`; both unions retain non-owning provider pointers.
- Rust `Box<T>` replaces each upstream `std::unique_ptr<T>` and provides the same stable heap
  address while `FileSystemController` itself is moved.

### Unintentional differences (to fix)

- None in the reviewed ownership slice. BIS, SDMC, external-content, game-card, registered-cache,
  and placeholder-cache objects now have the stable allocation required by Eden's ownership model.
  This prevents union slots from retaining dangling pointers after a controller move.

### Missing items

- None in the reviewed provider and game-card ownership slice.

### Binary layout verification

- N/A: these are host-side ownership objects. A focused regression moves a fully initialized
  controller and verifies that all four union-provider addresses remain unchanged.

## 2026-08-21 — `src/ruzu/src/{main_window,gtk_compat}.rs` vs Eden `src/yuzu/main_window.{h,cpp}` stop confirmation lifecycle

### Intentional differences

- Eden's `ConfirmShutdownGame` uses a blocking `QMessageBox`, while GTK4 confirmation is
  asynchronous. Ruzu therefore retains a one-shot callback and explicit pending state until the
  user responds or the dialog is dismissed.
- Ruzu rejects overlapping Stop/Restart and window-close confirmations. This reproduces the
  exclusivity that Eden receives automatically from its blocking modal dialog.

### Unintentional differences (to fix)

- None in the reviewed confirmation slice. Dismissing or destroying a GTK question now completes
  it as a rejection, so `stop_confirmation_pending` and `close_confirmation_pending` cannot remain
  latched after the dialog disappears.

### Missing items

- None in the reviewed `ConfirmShutdownGame` / `OnStopGame` confirmation lifecycle.

### Binary layout verification

- N/A: the change contains frontend-only callback and modal state.

## 2026-08-21 — `src/audio_core/src/adsp/apps/audio_renderer/audio_renderer.rs` vs Eden `src/audio_core/adsp/apps/audio_renderer/audio_renderer.{h,cpp}`

### Intentional differences

- Rust stores command buffers, processors, and stream handles in
  `Arc<parking_lot::Mutex<RendererShared>>`; this preserves Eden's single owning
  `AudioRenderer` while allowing its host and DSP threads to access the same state safely.
- Ruzu's mailbox and stream waits accept an atomic stop request, and `Stop` drains the response
  before resetting the mailbox. This is the Rust counterpart of `std::jthread` stop-token
  cancellation and prevents teardown from waiting forever after the DSP worker exits.
- `wait_with_stop`, `wait_with_timeout`, and startup-abort cleanup are Rust lifecycle adapters used
  by the threaded system manager; Eden expresses those ownership paths through `std::jthread` and
  blocking mailbox calls.
- Environment-gated event tracing remains available through `RUZU_TRACE_ADSP_AUDIO`. The removed
  `RUZU_PROFILE_ADSP` per-step timer had no Eden equivalent and imposed `Instant::now()` calls and
  an extra stream lock on the real-time render path even though it was only investigation tooling.
- Ruzu handles the map/unmap protocol messages declared by Eden's `Message` enum inline; Eden's
  current `Main` still leaves the separate map/unmap worker as a TODO.
- `CommandListProcessor::process` returns elapsed processing time in both implementations. Ruzu
  stores that duration directly; Eden's current `Process(index) - start_time` subtracts a global
  timestamp from that duration and is inconsistent with the method's implementation and contract.
- Fallible Rust initialization and optional stream handles reject an invalid session safely where
  Eden relies on initialized raw pointers.

### Unintentional differences (to fix)

- None in the reviewed renderer lifecycle and render-loop slice. The 200 ms shutdown delay and the
  `SetProcessTimeMax` → `WaitFreeSpace` → `Process` ordering now match Eden.

### Missing items

- None from the upstream `AudioRenderer` public/private method set or message constants.

### Binary layout verification

- N/A: `AudioRenderer` and `RendererShared` are host-side synchronization and ownership objects;
  guest command-buffer layouts remain owned by `command_buffer.rs`.

## 2026-08-21 — `src/audio_core/src/adsp/apps/opus/opus_decoder.rs` vs Eden `src/audio_core/adsp/apps/opus/opus_decoder.{h,cpp}`

### Intentional differences

- Focused Rust tests exercise the mailbox protocol and decoder lifecycle directly. Their success
  assertions now use the upstream Opus-domain constant `OPUS_OK`, rather than the numerically equal
  but unrelated HLE-service `ResultCode::SUCCESS`.

### Unintentional differences (to fix)

- None introduced by this warning-cleanup slice; runtime decoder behavior is unchanged.

### Missing items

- None discovered while tracing the unused `ResultCode` import through the upstream return-value
  assignments.

### Binary layout verification

- N/A: this slice only changes test assertions and removes an unused production import.

## 2026-08-21 — `src/common/src/dynamic_library.rs` vs Eden `src/common/dynamic_library.{h,cpp}`

### Intentional differences

- Rust's owned `DynamicLibrary` value is non-copyable by default and transfers ownership through
  ordinary moves; `Drop` implements Eden's destructor cleanup.
- `Option<i32>` represents Eden's `-1` major/minor sentinel in
  `get_versioned_filename`.
- Rust converts symbol and file names to `CString` and rejects embedded NUL bytes before calling
  the platform loader; Eden receives pre-existing C strings.
- `get_symbol<T>` returns `Option<T>` instead of assigning through an output pointer and returning
  `bool`.

### Unintentional differences (to fix)

- None. `open` now matches Eden's ordering and replaces the stored handle without first calling
  `close`; the previous pre-emptive cleanup was a Rust-only lifecycle change.

### Missing items

- None from the upstream `DynamicLibrary` interface.

### Binary layout verification

- N/A: the platform loader handle is host-only state and is never serialized.

## 2026-08-21 — `src/common/src/time_zone.rs` vs Eden `src/common/time_zone.{h,cpp}`

### Intentional differences

- Rust uses a `LazyLock<HashMap<...>>` for Eden's immutable `std::map`; the key/value contents and
  lookup behavior are identical.
- Windows uses the thread-safe CRT functions `localtime_s` and `gmtime_s` to obtain owned `tm`
  values. Eden immediately copies the results of `std::localtime` and `std::gmtime`, so subsequent
  calculations see the same state without retaining their static buffers.
- Targets that are neither Unix nor Windows retain a GMT fallback because Eden does not define a
  separate platform implementation for them.

### Unintentional differences (to fix)

- None. Windows now calculates the local/GMT offset and DST state like Eden instead of always
  returning zero and selecting GMT.

### Missing items

- None from the upstream `Common::TimeZone` interface or offset table.

### Binary layout verification

- N/A: timezone values are host-side strings and scalar calculations, not serialized structures.

## 2026-08-21 — `src/common/src/tree.rs` vs Eden `src/common/tree.h`

### Intentional differences

- Rust stores links as indices into a caller-owned slice and uses `usize::MAX` as the null
  sentinel, instead of retaining raw `T*` links. Every upstream rotation, color repair, lookup,
  insertion, removal, and traversal helper remains owned by this file with the same ordering.
- `HasRBEntry` replaces Eden's `CheckRBEntry`, `IsRBEntry`, and `HasRBEntry` C++ concepts.
- Rust naming follows snake_case, and a returned index replaces each returned pointer.

### Unintentional differences (to fix)

- None. `RB_REMOVE`'s `child` is assigned exactly once on each control-flow path as in Eden; its
  unnecessary Rust `mut` qualifier was removed without changing the algorithm.

### Missing items

- None from the upstream red-black-tree type and function set.

### Binary layout verification

- N/A: the index-based `RBEntry` is an internal safe-Rust representation and is not copied to or
  from Eden's packed, pointer-based host structure.

## 2026-08-21 — removed `src/common/src/x64/cpu_wait.rs` vs Eden `src/common/thread.{h,cpp}`

### Intentional differences

- None for the removed module: Eden has no `common/x64/cpu_wait.*` counterpart and Ruzu had no
  production caller for its public `micro_sleep` function.

### Unintentional differences (to fix)

- Ruzu's separate helper monitored the address of a temporary aligned zero rather than the
  `Event::is_set` state used by Eden. Consequently it could only expire by timer and could not be
  awakened by `Event::set`; retaining or moving it would not provide upstream behavior.

### Missing items

- Ruzu's `common/thread.rs` still uses the condition-variable `Event::wait_for` path on Windows and
  does not yet port Eden's x86-64 Windows `MONITORX`/`WAITPKG` branches. This is a separate,
  platform-specific implementation slice rather than a prerequisite for removing the unreachable
  helper.

### Binary layout verification

- N/A: the removed cache-line-aligned tuple was host-only temporary storage passed to inline
  assembly and was never serialized.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/exception_handler.rs` vs Eden `src/dynarmic/src/dynarmic/backend/exception_handler.{h,posix.cpp}`

### Intentional differences

- Rust's `Option<FakeCall>` callback can decline a fault. Eden's callback returns `FakeCall`
  directly for a matched code range.
- The Windows SEH implementation remains in the same Rust file under `cfg(windows)` because the
  crate currently exposes one x64 exception-handler module rather than Eden's per-platform C++
  translation units.
- Ruzu additionally installs an owned alternate stack on each Linux CPU thread because POSIX
  alternate stacks are thread-local. Eden's singleton owns only the stack installed on the thread
  that constructs it. The Rust thread-local owner disables and unmaps its stack at thread exit.
- Eden's POSIX source installs `SIGBUS` only when `__APPLE__` is defined. Ruzu's macOS path is the
  separately documented non-fastmem Mach stub, so Linux correctly installs only `SIGSEGV`.

### Unintentional differences (to fix)

- None identified for the Linux x86-64 handler lifecycle after the 2026-08-21 parity pass.

### Missing items

- None for the Linux x86-64 handler lifecycle. macOS fastmem remains disabled as documented at the
  top of the Rust module and is outside this POSIX/Linux slice.

### Binary layout verification

- N/A: `SigHandlerState` is host-only Rust state. Platform context and SEH
  layouts are verified by the existing platform-specific tests in this module.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/a64_emit_x64.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/{emit_x64,a64_emit_x64}.{h,cpp}`

### Intentional differences

- Rust has no shared C++ `EmitX64` base object, so the A64 emitter directly owns its
  `ExceptionHandler`. It is declared before the owned code buffer and callback table so Rust's
  field drop order removes the registration first.

### Unintentional differences (to fix)

- None identified in exception-handler registration, support probing, callback publication, or
  destruction ordering.

### Missing items

- None for this exception-handler ownership slice.

### Binary layout verification

- N/A: the new owner contains host pointers/ranges and is not copied to guest memory.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/a32_emit_x64.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/{emit_x64,a32_emit_x64}.{h,cpp}`

### Intentional differences

- Rust has no shared C++ `EmitX64` base object, so the A32 emitter directly owns its
  `ExceptionHandler`. It is declared before the owned code buffer and callback table so cleanup
  follows Eden's emitter-before-code lifetime.

### Unintentional differences (to fix)

- None identified in exception-handler registration, support probing, callback publication, or
  destruction ordering.

### Missing items

- None for this exception-handler ownership slice.

### Binary layout verification

- N/A: the new owner contains host pointers/ranges and is not copied to guest memory.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/block_of_code.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/block_of_code.{h,cpp}`

### Intentional differences

- On Windows, Ruzu still places and registers SEH unwind metadata during `prelude_complete`; its
  Windows-only `Drop` remains a fallback for standalone code-buffer tests. Production cleanup is
  now first performed by the emitter-owned `ExceptionHandler`.

### Unintentional differences (to fix)

- None identified in Linux code-block cleanup: the non-upstream unconditional `BlockOfCode::drop`
  registration removal has been deleted.

### Missing items

- None for this exception-handler ownership slice.

### Binary layout verification

- N/A on Linux. Existing Windows tests verify the in-buffer unwind layouts.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/block_of_code.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/block_of_code.{h,cpp}`

### Intentional differences

- Ruzu emits x64 through `rxbyak::CodeAssembler` and stores byte offsets into its owned code buffer;
  Eden derives `BlockOfCode` from C++ Xbyak and stores native code pointers.
- Rust uses `cfg(target_os = "windows")` for Eden's `_WIN32` callee-saved XMM6–XMM15 path.

### Unintentional differences (to fix)

- None in the reviewed ABI import/save-restore slice. The `xmmword_ptr` operand and
  `xmm_save_base` helper are now compiled only on Windows, matching the only path that consumes
  them. The native constant-pool regression test now verifies both deduplicated operands.

### Missing items

- None introduced or discovered in the Windows callee-save operand slice.

### Binary layout verification

- PASS: the existing stack-frame and Windows unwind-code tests verify the offsets consumed by the
  XMM save/restore instructions; no serialized guest structure is changed.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_memory.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/{a64_emit_x64_memory.cpp,emit_x64_memory.cpp.inc}`

### Intentional differences

- Rust keeps the scalar callback emitters in this shared x64 module and represents the 128-bit
  callback return with an explicit stack buffer on Windows.
- Rust also passes 128-bit callback writes through an explicit pointer on Windows; Eden's C++ ABI
  passes its `Vector` aggregate indirectly there. System V continues to use two integer lanes.
- `rxbyak` memory-operand constructors replace C++ Xbyak's `ptr`/`xword` address frames.

### Unintentional differences (to fix)

- None in the reviewed callback ABI slice. The indirect return buffer now applies to every Windows
  toolchain, matching Eden's `_WIN32`, instead of only MSVC. Ordinary 128-bit writes likewise use
  a Windows pointer payload, and 32/64-bit XMM-backed scalar writes select their third argument
  through `ABI_PARAMS` rather than hard-coding System V's `RDX`.

### Missing items

- The fastmem/page-table 128-bit paths are owned by
  `backend/x64/a64_emit_x64_memory.rs`; this file intentionally remains the callback-only owner
  selected by the current dispatcher for `A64ReadMemory128`/`A64WriteMemory128`.

### Binary layout verification

- PASS: Windows read/write buffers are exactly 16 bytes after ABI shadow space, and the
  non-Windows path still transfers two 64-bit lanes through ABI-selected registers.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/a64_emit_x64_memory.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/a64_emit_x64_memory.cpp`

### Intentional differences

- Ruzu stores fallback entry offsets in Rust hash maps and calls explicit Rust trampolines; Eden
  stores native function pointers and devirtualizes C++ `UserCallbacks`.
- The Rust Windows read trampoline accepts an explicit output pointer after the fixed context and
  address arguments. This preserves the same stack-buffer transfer without relying on C++'s
  compiler-specific hidden-return ordering.

### Unintentional differences (to fix)

- None in the reviewed 128-bit read-fallback ABI slice. Both MSVC and MinGW now use the Windows
  stack buffer, matching upstream `_WIN32`; System V no longer reserves the unused 16-byte local.
- Removed one unused register binding from Ruzu-only fastmem diagnostic emission; emitted code is
  unchanged.

### Missing items

- Ruzu's current dispatcher routes ordinary 128-bit accesses through callback-only
  `emit_memory.rs`; it does not yet select Eden's fastmem/page-table 128-bit read/write fallback
  path. This is pre-existing behavioral debt outside the ABI prerequisite fixed here.

### Binary layout verification

- PASS for the reviewed fallback payload: the Windows local is 16 bytes and is loaded with
  `movups`; System V reconstructs the vector from the two 64-bit return registers.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_exclusive_memory.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/emit_x64_memory.cpp.inc`

### Intentional differences

- Ruzu owns architecture-specific exclusive emission in this Rust file, while Eden instantiates
  the shared C++ template include from its A64 emitter.
- Rust's Windows trampolines take explicit pointer payloads for 128-bit values instead of exposing
  the host compiler's aggregate ABI directly to generated code.

### Unintentional differences (to fix)

- None in the reviewed callback-only 128-bit read/write slice. All Windows toolchains use the
  stack-buffer read path, and exclusive writes use a pointer payload rather than System V lane
  registers that overwrite Win64 arguments.

### Missing items

- No new missing item found in the callback-only exclusive slice; inline fastmem ownership was not
  re-audited as part of this prerequisite.

### Binary layout verification

- PASS: each Windows exclusive payload occupies 16 bytes after the 32-byte shadow space; System V
  continues to pass or return two 64-bit lanes.

## 2026-08-21 — `src/rdynarmic/src/jit.rs` vs Eden `src/dynarmic/src/dynarmic/interface/A64/config.h` and x64 memory callback call sites

### Intentional differences

- Rust uses free `extern "C"` trampolines to recover `JitInner`; Eden invokes virtual
  `UserCallbacks` through `ArgCallback`/`Devirtualize`.
- On Windows, Rust gives the read and write trampolines explicit `Pair128` pointers. Eden obtains
  the equivalent indirect aggregate transfer from its C++ ABI and generated accessor stubs.

### Unintentional differences (to fix)

- None in the reviewed A64 128-bit trampoline slice. The ordinary and exclusive read/write
  signatures now agree with the emitter on both Windows toolchains.

### Missing items

- None introduced in the A64 trampoline slice. A32 trampolines have separate emitter ownership and
  were not changed or claimed by this comparison.

### Binary layout verification

- PASS: `Pair128` is `repr(C)`, compile-time asserted to size 16/alignment 8, and every field is
  initialized before it crosses the trampoline boundary.

## 2026-08-21 — `src/rdynarmic/src/ir/opcode.rs` vs Eden `src/dynarmic/src/dynarmic/ir/opcodes.inc`

### Intentional differences

- Rust represents Eden's generated opcode table as an enum plus an explicit `OpcodeInfo` match.

### Unintentional differences (to fix)

- None in the scalar result-and-overflow saturation opcode slice: both `WithFlag32` operations
  have the same U32 inputs/result, while signed and unsigned saturation keep their U8 width input.

### Missing items

- None for the four scalar saturation opcodes reviewed in this slice.

### Binary layout verification

- N/A: these are internal IR opcode/type declarations and are not serialized guest payloads.

## 2026-08-21 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `src/dynarmic/src/dynarmic/ir/ir_emitter.h`

### Intentional differences

- Rust's `ResultAndOverflow` stores the untyped `Value` enum instead of Eden's templated result
  type; opcode metadata enforces that every helper in this slice returns U32 plus U1.

### Unintentional differences (to fix)

- None in `signed_saturated_add_with_flag`, `signed_saturated_sub_with_flag`,
  `signed_saturation`, or `unsigned_saturation`: validation, opcode arguments, and associated
  overflow pseudo-operation ordering match Eden.

### Missing items

- None for the scalar saturation IR API reviewed in this slice.

### Binary layout verification

- N/A: `ResultAndOverflow` is an internal SSA builder result and is never copied to guest memory.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_saturation.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/emit_x64_saturation.cpp`

### Intentional differences

- Rust passes the presence of Eden's `has_overflow_inst` template parameter explicitly and uses
  `Option<InstRef>` for the associated pseudo-operation.
- `rxbyak` register-width conversions replace C++ Xbyak's `changeBit` views.

### Unintentional differences (to fix)

- None in the signed saturated add/sub, signed scalar saturation, or unsigned scalar saturation
  methods reviewed here. In particular, `WithFlag32` exposes overflow without touching FPSR.QC,
  ordinary signed saturated add/sub ORs the generated overflow byte into QC, and the 8-bit CMOV
  uses a 32-bit operand exactly as Eden does.

### Missing items

- None for the scalar saturation prerequisite methods reviewed in this slice; unrelated methods
  in the same pre-existing file were not claimed as re-audited.

### Binary layout verification

- N/A: emitted host instructions operate on internal SSA values and JIT state fields.

## 2026-08-21 — `src/rdynarmic/src/backend/arm64/emit_arm64_saturation.rs` vs Eden `src/dynarmic/src/dynarmic/backend/arm64/emit_arm64_saturation.cpp`

### Intentional differences

- Ruzu's local ARM64 encoder has no EOR-immediate helper, so Eden's single
  `EOR Wscratch0, Wscratch0, 0x80000000` is emitted as a MOVZ/MOVK into `Wscratch1` followed by
  register EOR. The result and flags are identical.
- Eden's explicit `UNREACHABLE` specializations for generic scalar/vector saturation opcodes fall
  through Ruzu's common unsupported-opcode error if they survive required IR lowering; the four
  reachable scalar result-and-overflow operations remain owned by this matching file.

### Unintentional differences (to fix)

- None in `SignedSaturatedAddWithFlag32`, `SignedSaturatedSubWithFlag32`, `SignedSaturation`, or
  `UnsignedSaturation`: register realization, flag spilling, clamp ordering, and overflow creation
  match Eden.

### Missing items

- None for the four reachable scalar saturation operations reviewed in this slice.

### Binary layout verification

- N/A: the emitted AArch64 instruction stream does not serialize a guest-visible structure.

## 2026-08-21 — `src/rdynarmic/src/backend/arm64/inst.rs` vs Oaknut instructions used by Eden `emit_arm64_saturation.cpp`

### Intentional differences

- Ruzu encodes AArch64 instructions directly as `u32` words rather than calling Oaknut.

### Unintentional differences (to fix)

- None for the newly required `CMP Wn, Wm` encoding; its known machine word is covered by the
  AArch64 encoding regression test.

### Missing items

- None for the instruction-encoding prerequisite in this slice.

### Binary layout verification

- PASS: `cmp w16, w17` encodes as `0x6b11021f`, verified under the AArch64 test target.

## 2026-08-21 — `src/rdynarmic/src/backend/{x64/emit.rs,arm64/emit_arm64.rs,arm64/mod.rs}` vs Eden backend saturation emitter registration

### Intentional differences

- Rust dispatches opcodes through explicit `match` arms and declares the ARM64 source module in
  `mod.rs`; Eden registers template specializations through its C++ emitter headers and build.

### Unintentional differences (to fix)

- None in this routing slice: all four scalar result-and-overflow saturation opcodes reach their
  architecture-specific owner on x64 and ARM64.

### Missing items

- None introduced by the routing change.

### Binary layout verification

- N/A: routing declarations do not define a serialized layout.

## 2026-08-21 — `src/rdynarmic/src/frontend/a32/translate/helpers.rs` vs Eden `src/dynarmic/src/dynarmic/frontend/A32/translate/impl/common.h`

### Intentional differences

- Rust returns the untyped internal `Value` enum where Eden's helper signatures distinguish U16
  and U32 at compile time; the emitted opcode metadata retains those types.

### Unintentional differences (to fix)

- None in `pack_2x16_to_1x32` or `most_significant_half`: masks, shift amounts, carry input, and
  operation ordering match Eden exactly.

### Missing items

- None for the two common helpers required by the scalar saturation translator slice. Other
  pre-existing helpers in `common.h` were not re-audited or claimed by this prerequisite.

### Binary layout verification

- N/A: these helpers construct internal SSA operations and serialize no guest-visible payload.

## 2026-08-21 — `src/rdynarmic/src/frontend/a32/translate/saturated.rs` vs Eden `src/dynarmic/src/dynarmic/frontend/A32/translate/impl/{saturated.cpp,a32_translate_impl.h}`

### Intentional differences

- Ruzu decodes fields from `DecodedArm::raw` inside each Rust method, while Eden's generated
  decoder passes typed immediates, booleans, and registers as method arguments.
- ARM condition state is emitted once at the Rust block-translation boundary; the method bodies
  therefore begin with Eden's pre-condition register validation and then emit the instruction
  body. Invalid PC operands still raise Unpredictable before any register read.

### Unintentional differences (to fix)

- None in `arm_ssat`, `arm_ssat16`, `arm_usat`, `arm_usat16`, `arm_qadd`, `arm_qsub`,
  `arm_qdadd`, or `arm_qdsub`. Saturation widths, immediate-shift carry input, signed halfword
  extension, result packing, and every sticky-Q update match Eden's order.

### Missing items

- Eden's `arm_QASX`, `arm_QSAX`, `arm_UQASX`, and `arm_UQSAX` remain absent because Ruzu's ARM
  decoder does not yet expose those instruction IDs. They are pre-existing parallel-instruction
  debt outside this scalar warning slice.

### Binary layout verification

- N/A: these translators construct internal SSA and no raw guest payload.

## 2026-08-21 — `src/rdynarmic/src/frontend/a32/translate/mod.rs` vs Eden ARM decoder/visitor dispatch for scalar saturation

### Intentional differences

- Rust uses an explicit `ArmInstId` match after block-level condition setup; Eden invokes visitor
  methods through generated decoder callbacks.

### Unintentional differences (to fix)

- None in this routing slice: all eight decoded ARM scalar saturation instructions now call their
  owner in `saturated.rs`; the former successful no-op stubs were removed.

### Missing items

- The four parallel saturation IDs named in the `saturated.rs` audit remain absent from the Rust
  decoder and consequently from this dispatcher.

### Binary layout verification

- N/A: dispatcher routing defines no serialized layout.

## 2026-08-21 — `src/rdynarmic/src/jit.rs` scalar saturation regression vs Eden `frontend/A32/translate/impl/saturated.cpp`

### Intentional differences

- The Rust-native regression executes a compact ARM instruction stream through each available
  host backend; Eden's C++ source defines the expected semantics but does not own this Rust test.

### Unintentional differences (to fix)

- None in the covered behavior: signed/unsigned scalar and halfword clamps produce the expected
  registers, saturated addition clamps to INT32_MAX, and CPSR.Q remains set.

### Missing items

- This focused regression does not claim exhaustive immediate widths or every QDADD/QDSUB input;
  their IR ordering is covered by module tests.

### Binary layout verification

- N/A: the test executes guest instructions but changes no serialized guest structure.

## 2026-08-21 — `src/rdynarmic/src/frontend/a32/translate/{data_processing.rs,mod.rs}` vs Eden `src/dynarmic/src/dynarmic/frontend/A32/translate/impl/{data_processing.cpp,a32_translate_impl.h}`

### Intentional differences

- Ruzu extracts instruction fields from `DecodedArm` and performs ARM condition handling at the
  block-translation boundary; Eden's generated decoder passes typed fields to individual visitor
  methods, each of which calls `ArmConditionPassed`.
- Rust inserts an identity `Or32` before `GetNZFromOp` when MOV/MVN yields a non-instruction
  `Value`; Eden's typed IR can attach `NZFrom` directly. This preserves the associated-pseudo-op
  contract used by both Rust backends.
- `translate/mod.rs` imports only `decode_thumb32`; Eden's visitor declarations do not require a
  Rust instruction-ID type import, and the removed `Thumb32InstId` import had no behavior.

### Unintentional differences (to fix)

- The pre-existing `classify`/`dp_emit` dispatcher consolidates Eden's 48 separately owned
  immediate, immediate-shift, and register-shift methods. The audited paths now preserve Eden's
  helper choice, carry reads, PC validation, state-update ordering, and BIC `AndNot` operation,
  but the method-boundary flattening still needs to be unwound for strict structural parity.

### Missing items

- No decoded ARM data-processing operation is missing from this file. Exact one-method-per-Eden-
  visitor structure remains the structural work identified above.

### Binary layout verification

- N/A: these translators construct internal SSA operations and serialize no guest-visible
  payload.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/{a32_emit_a32.rs,emit_a64.rs,emit_vector_multiply.rs}` warning-only cleanup vs Eden x64 emitter owners

### Intentional differences

- The Rust A32 emitter keeps the uniform `EmitContext` argument required by opcode dispatch but
  names it `_ctx`; Eden's `EmitA32ClearExclusive` likewise retains and leaves its
  `A32EmitContext&` parameter unnamed.
- Rust-native emitter regressions have no direct Eden test-file counterpart. Removing one unused
  synthetic `Inst` and three unnecessary `unsafe` call sites changes neither the emitted code nor
  the paired-min/max fallback calculations rechecked against Eden's `emit_x64_vector.cpp`.

### Unintentional differences (to fix)

- None introduced or found in this warning-only slice. Production vector-emitter parity outside
  the three existing lower-paired regressions was not re-audited here.

### Missing items

- None for this warning-only slice.

### Binary layout verification

- N/A: parameter naming and Rust test call-site cleanup define no serialized layout.

## 2026-08-21 — `src/rdynarmic/src/frontend/a32/translate/thumb16.rs` PUSH/POP vs Eden `src/dynarmic/src/dynarmic/frontend/A32/translate/impl/{thumb16.cpp,a32_translate_impl.h}`

### Intentional differences

- Ruzu extracts `M`/`P` and the low register list from `DecodedThumb16`; Eden's generated decoder
  passes those fields as typed visitor arguments. Both construct the same 16-bit register mask.
- Rust uses `Reg::R13` for Eden's `Reg::SP` spelling and `Value::ImmU1` carry operands for the
  equivalent `ir.Add`/`ir.Sub` operations.

### Unintentional differences (to fix)

- None in the re-audited PUSH/POP slice: empty lists are rejected before reading SP, stack
  accesses are `Atomic`, registers are visited in ascending order, and POP writes the incremented
  address to SP at Eden's exact point before `PopRSBHint`.

### Missing items

- None in `thumb16_PUSH` or `thumb16_POP`. Other methods in the shared Rust file were not claimed
  by this warning-driven audit.

### Binary layout verification

- N/A: the methods emit guest memory operations but define no serialized structure.

## 2026-08-21 — `src/rdynarmic/src/frontend/a64/translate/mod.rs` vs Eden `src/dynarmic/src/dynarmic/frontend/A64/translate/{a64_translate.cpp,a64_translate.h}`

### Intentional differences

- Rust returns its newly allocated `Block`; Eden appends into a caller-owned block. Location
  advancement, cycle accounting, single-step linking, terminal validation, and end-location
  assignment otherwise retain the same ownership and order.
- Rust leaves `should_continue` uninitialized until the first mandatory loop iteration, avoiding
  an overwritten-value warning. Eden initializes it for C++ `do`/`while` syntax; both assign it
  on every path before reading it.

### Unintentional differences (to fix)

- Eden raises `UnallocatedEncoding` whenever its decoder has no match. Ruzu currently raises it
  only for the reserved low encoding range and sends other unmatched instructions to
  `interpret_this_instruction` so its incomplete decoder can fall back to the interpreter. This
  compatibility path must disappear when decoder parity is complete; changing it in this
  warning-only slice would turn still-supported instructions into exceptions.

### Missing items

- The public equivalent of Eden's `TranslateSingleInstruction` is absent. Module-local test
  helpers with a similar name do not implement that API.

### Binary layout verification

- N/A: block translation control flow defines no serialized guest structure.

## 2026-08-21 — `src/rdynarmic/src/ir/opt/a32_get_set_elimination.rs` pending-C forwarding vs Eden `src/dynarmic/src/dynarmic/ir/{opt_passes.cpp,opt_passes.h}`

### Intentional differences

- Eden inserts `GetCFlagFromNZCV`, breaks from the switch, and lets reverse-iterator movement
  revisit the shifted `A32SetCpsrNZCV`. Rust's indexed arena inserts before the set, adjusts the
  pending use and set indices, and completes the same set handling in the current iteration. The
  resulting instruction order and optimizer state are identical.
- The Rust pass is split into its own ownership file instead of remaining a static section of
  Eden's large `opt_passes.cpp`; this is an existing Rust module boundary for a named upstream
  pass, and its comments now point to the actual current Eden owner.

### Unintentional differences (to fix)

- None in the re-audited pending-C/`A32SetCpsrNZCV` path. The removed boolean assignment was
  overwritten by the complete `FlagInfo::set_not_required()` state before any read.

### Missing items

- None in this warning-driven path. The rest of `FlagsPass` and `RegisterPass` was not newly
  claimed by this focused audit.

### Binary layout verification

- N/A: this optimizer rewrites internal SSA and defines no serialized structure.

## 2026-08-21 — `src/core/src/file_sys/content_archive.rs` vs Eden `src/core/file_sys/{content_archive.h,content_archive.cpp}`

### Intentional differences

- Rust receives a non-nullable `VirtualFile`; Eden's initial `file == nullptr` branch therefore has
  no Rust representation. An empty, non-null file is passed to `NcaReader::Initialize` and reported
  as `ErrorBadNCAHeader`, as Eden does for the same object.
- Rust stores the reader in an `Option<Arc<NcaReader>>` because initialization can fail before an
  initialized reader is available. The getters retain their existing safe defaults when called on
  a failed object; Eden relies on callers checking `GetStatus()` before using those getters.
- The unused `encrypted` member was removed. Eden declares and default-initializes the member but
  never reads or writes it anywhere; retaining it in Rust only produced dead-state warning noise.
- `Arc<Mutex<KeyManager>>` replaces Eden's reference to the singleton key manager while preserving
  key lookup ownership and constructor ordering.

### Unintentional differences (to fix)

- `get_type` maps an invalid raw content-type byte (or a missing reader after failed construction)
  to `Program`; Eden directly casts the byte to `NCAContentType`. Preserving an invalid discriminant
  safely requires changing the Rust public type instead of constructing an invalid enum value.

### Missing items

- None in the constructor paths audited here: reader initialization, key-area validation, title-key
  setup, filesystem classification, update detection, and final status now follow Eden's ordering.

### Binary layout verification

- PASS: `NCA` itself is not serialized. The regression fixture writes the existing `repr(C)`
  `NcaHeader`, whose compile-time size assertion remains `0x400`; it introduces no new payload type.

## 2026-08-21 — `src/core/src/file_sys/fssystem/aes_xts_storage.rs` vs Eden `src/core/file_sys/fssystem/{fssystem_aes_xts_storage.h,fssystem_aes_xts_storage.cpp}`

### Intentional differences

- Rust constructs an `AesCipher` from the retained key for each locked read; Eden constructs and
  retains an optional cipher in the object. This preserves serialized access and the exact tweak
  sequence, with only cipher-context reuse differing.
- Eden's bounded `boost::container::static_vector` is represented by a zero-initialized `Vec` after
  enforcing the same `NcaHeader::XtsBlockSize` maximum. Its bytes and lifetime are equivalent, but
  Rust currently allocates this uncommon partial-sector buffer on the heap.
- The `VfsFile` implementation supplies the Rust VFS naming, parent, readability, and write-reject
  methods around Eden's `IReadOnlyStorage` interface.

### Unintentional differences (to fix)

- None in `MakeAesXtsIv`, construction, `Read`, or `GetSize` after this audit. In particular, reads
  now seed the counter from the supplied IV and preserve XTS block-tweak position for an offset in
  the middle of a storage block.

### Missing items

- None for this storage layer.

### Binary layout verification

- N/A: `AesXtsStorage` is an in-memory polymorphic storage object and is not serialized. Key and IV
  arrays retain Eden's exact `0x20` and `0x10` byte sizes.

## 2026-08-21 — `src/core/src/file_sys/fssystem/hierarchical_sha3_storage.rs` vs Eden `src/core/file_sys/fssystem/{fssystem_hierarchical_sha3_storage.h,fssystem_hierarchical_sha3_storage.cpp}`

### Intentional differences

- Rust owns a copy of the caller-provided hash work buffer in a `Vec<u8>`; Eden retains and fills a
  caller-owned raw pointer. The buffer is not consulted after initialization in either
  implementation, while owned storage avoids an unsafe lifetime spanning the object.
- Rust represents the not-yet-initialized base storage with `Option` and returns zero from safe
  getters; Eden requires `Initialize` before `GetSize` or non-empty `Read`.
- The unused mutex was removed. Eden declares and constructs `m_mutex` but never locks it in either
  `Initialize` or `Read`, so the Rust field provided no synchronization or lifecycle behavior.

### Unintentional differences (to fix)

- None in the initialization bounds, layer selection, hash-buffer fill, size query, or pass-through
  read paths audited here.

### Missing items

- None for the behavior present in Eden's current hierarchical SHA3 storage.

### Binary layout verification

- N/A: this storage object and its owned work buffer are not serialized.

## 2026-08-21 — `src/core/src/file_sys/ips_layer.rs` vs Eden `src/core/file_sys/{ips_layer.h,ips_layer.cpp}`

### Intentional differences

- Rust `VirtualFile` arguments are non-nullable, so Eden's null-input branches in `PatchIPS` and
  `IPSwitchCompiler::Apply` have no direct representation.
- Patch text is decoded with `from_utf8_lossy` before applying the same ASCII-oriented grammar;
  Eden stores arbitrary input bytes in a `std::string`. Valid IPSwitch syntax is ASCII, while this
  avoids unchecked string indexing over invalid UTF-8.
- Rust uses `BTreeMap` for Eden's ordered `std::map` and an owned `VectorVfsFile` behind `Arc` for
  the equivalent patched result.
- The `IPSwitchPatch::name` field was removed. Eden initializes it from `last_comment` but never
  reads it; patch names remain available in the parse log without retaining dead per-patch state.
- `parse_integer_auto` expresses the valid signed decimal/octal/hexadecimal forms accepted by
  Eden's `std::strtoll(..., 0)` without calling a platform C runtime parser.

### Unintentional differences (to fix)

- None in the audited IPS/IPS32 record application and IPSwitch parsing paths. Inner patch comments
  are now skipped, offset shifts accept C-style base prefixes, and signed shifts use wrapping
  unsigned addition before the final `u32` narrowing, matching Eden's bit pattern.

### Missing items

- None for this file's public API.

### Binary layout verification

- N/A: patch records are parsed into owned containers and are not serialized as native structs.

## 2026-08-21 — `src/common/src/fs/path_util.rs` `sanitize_path` vs Eden `src/common/fs/{path_util.h,path_util.cpp}` `SanitizePath`

### Intentional differences

- Rust builds the normalized result from borrowed UTF-8 components instead of Eden's mutable byte
  string and `string_view` vector. Separator selection, Windows network-prefix preservation,
  absolute-path handling, and component ordering remain the same for valid platform paths.

### Unintentional differences (to fix)

- Android content URIs are not bypassed before normalization. Android filesystem glue is an
  explicit project exception; this remains relevant only if that excluded frontend is introduced.

### Missing items

- None for desktop `SanitizePath`: repeated separators and trailing separators are removed, `.` is
  discarded, and `..` removes the preceding retained component exactly as in Eden.

### Binary layout verification

- N/A: path normalization defines no serialized structure.

## 2026-08-21 — `src/core/src/file_sys/vfs/vfs_real.rs` vs Eden `src/core/file_sys/vfs/{vfs_real.h,vfs_real.cpp}`

### Intentional differences

- Eden stores intrusive `FileReference` nodes owned directly by each `RealVfsFile`. Rust assigns an
  opaque ID to each file and owns the references in the filesystem's locked state; open/closed LRU
  order, the `8192`-handle limit, reopening, eviction, and drop ordering are preserved.
- Rust's single state mutex contains Eden's cache, reference lists, open count, and list mutex. It
  remains held across seek/read/write just as Eden retains the lock returned by `RefreshReference`.
- `Arc::new_cyclic` supplies a non-owning self reference so methods reached through
  `VfsFilesystem` can create files and directories that retain the filesystem. Eden obtains that
  relationship through its external `shared_ptr` lifetime and raw back-reference.
- Eden's `in_dtor` workaround handles a FreeBSD C++ destruction-order issue. Rust files own an
  `Arc` to the filesystem and therefore drop their reference before the filesystem can be freed.
- `RealVfsDirectory::new` is public because existing Rust frontend construction sites instantiate
  the directory directly; Eden restricts its constructor to filesystem friendship.

### Unintentional differences (to fix)

- `get_file_time_stamp` still returns a zero timestamp on non-Unix targets, whereas Eden uses
  `_wstat64` on Windows. The retained-handle and directory traversal slice does not address that
  pre-existing platform implementation gap.
- Android content-URI filename handling is absent, consistently with the project's Android
  filesystem exception.

### Missing items

- None in file-reference lifecycle: cached opens create a closed reference, first access opens it,
  every access promotes it to the LRU front, the least-recent open handle is evicted at the same
  limit, and `Drop` removes and closes the reference.

### Binary layout verification

- N/A: VFS objects and host file-reference state are not serialized.

## 2026-08-21 — `src/core/src/loader/kip.rs` loader file ownership vs Eden `src/core/loader/{loader.h,kip.h,kip.cpp}`

### Intentional differences

- Eden inherits the protected `file` and `is_loaded` members from `AppLoader`; Rust's `AppLoader`
  is a trait, so `AppLoaderKip` owns both fields directly. The retained file is named `_file` to
  express base-class ownership while avoiding a false dead-field warning.

### Unintentional differences (to fix)

- None in the file-lifetime slice. The complete KIP loading algorithm was not reimplemented here.

### Missing items

- None for `AppLoader` base-file ownership; a regression verifies the `VirtualFile` remains alive
  until the loader is dropped.

### Binary layout verification

- N/A: loader ownership state is not serialized.

## 2026-08-21 — `src/core/src/loader/nax.rs` loader file ownership vs Eden `src/core/loader/{loader.h,nax.h,nax.cpp}`

### Intentional differences

- As for KIP, Rust's trait cannot own Eden `AppLoader::file`; `AppLoaderNax::_file` retains that
  base-class ownership directly while `Nax` separately retains its own backing-file reference.

### Unintentional differences (to fix)

- None in this ownership-only slice; NAX parsing and delegated NCA loading were not changed.

### Missing items

- None for base-file lifetime. The regression distinguishes the loader's reference from `Nax`'s
  own reference and verifies both disappear when the loader is dropped.

### Binary layout verification

- N/A: loader reference ownership is not serialized.

## 2026-08-21 — `tools/capture_harness/{src/main.rs,example.toml}` vs no Eden source counterpart

### Intentional differences

- `capture_harness` is a Ruzu-specific developer tool and has no file under Eden's `src/` tree.
  Its embedded parser regression now uses the checked-in generic homebrew example timeline instead
  of referring to a missing, title-specific local fixture.

### Unintentional differences (to fix)

- None for this build-fixture correction.

### Missing items

- None: `cargo test --workspace --all-targets` can compile the harness test without an external
  untracked configuration file.

### Binary layout verification

- N/A: the change only parses TOML developer-tool configuration.

## 2026-08-21 — `externals/rxbyak/src/{encode.rs,platform/unix.rs}` vs Eden x64 Xbyak consumers under `src/common/x64` and `src/dynarmic`

### Intentional differences

- `rxbyak` is Ruzu's Rust replacement for Eden's external Xbyak dependency, so it has no direct
  file counterpart in Eden's `src/` tree. Platform allocation retains Eden/Dynarmic's writable-then-
  executable lifecycle and adds `MAP_JIT` only on macOS.
- The orphaned `emit_evex_leg` APX encoder was removed. No rxbyak instruction called it, no APX
  instruction was generated, and Eden's current Dynarmic consumers do not emit APX; retaining the
  unreachable partial capability only produced dead code.

### Unintentional differences (to fix)

- None in this warning-only slice. General rxbyak/Xbyak instruction parity is outside this focused
  allocation and unreachable-APX audit.

### Missing items

- Full Intel APX instruction generation remains unsupported rather than being represented by one
  unreachable prefix helper.

### Binary layout verification

- N/A: executable mapping flags and instruction-emitter methods define no serialized structure.

## 2026-08-21 — `externals/rxbyak/tests/common/mod.rs` vs Eden test infrastructure

### Intentional differences

- Eden has no Rust integration-test crate counterpart. Each rxbyak integration-test binary imports
  the same shared operand tables and NASM helpers but deliberately uses only the subset relevant to
  its instruction family, so `dead_code` is allowed only inside that shared test module.

### Unintentional differences (to fix)

- None.

### Missing items

- None in this test-harness warning slice.

### Binary layout verification

- N/A: this changes only warning policy for test helpers.

## 2026-08-21 — `core/src/hle/kernel/k_session.rs` vs Eden `core/hle/kernel/k_session.{h,cpp}`

### Intentional differences
- Rust stores the embedded client and server endpoints behind separate `Arc<Mutex<_>>` owners. `KSession::on_server_closed` does not lock the client endpoint solely to invoke `KClientSession::on_server_closed`, because that upstream method has an empty body; doing so introduced a Rust-only endpoint/session ABBA deadlock during concurrent close.

### Unintentional differences (to fix)
- None in the server-close notification corrected by this entry.

### Missing items
- The wider `KAutoObject` reference-count lifecycle remains represented by Rust registries and endpoint-close flags rather than Eden's intrusive kernel-object ownership.

### Binary layout verification
- N/A: these kernel objects are not raw-copied or serialized.

## 2026-08-21 — `rdynarmic/backend/arm64/emit_arm64_cryptography.rs` vs Eden `dynarmic/backend/arm64/emit_arm64_cryptography.cpp`

### Intentional differences
- Rust writes verified AArch64 instruction words through `BlockOfCode`; Eden expresses the same `SHA256H`, `SHA256H2`, `SHA256SU0`, and `SHA256SU1` instructions through Oaknut.

### Unintentional differences (to fix)
- None in the AES and SHA-256 instruction families currently owned by this file.

### Missing items
- Eden's `SM4AccessSubstitutionBox` unreachable specialization remains outside this implementation slice.

### Binary layout verification
- PASS: focused instruction tests match words assembled by Apple's AArch64 assembler for registers `v16`, `v17`, and `v18`.

## 2026-08-21 — `rdynarmic/backend/arm64/emit_arm64_cryptography.rs` vs Eden `dynarmic/backend/arm64/emit_arm64_cryptography.cpp` (CRC32 completion)

### Intentional differences
- Rust selects the 32-bit or 64-bit data register with a boolean passed to the owner-local `emit_crc` helper; Eden expresses the same distinction through the `EmitCRC<bitsize>` template parameter.
- Rust writes verified AArch64 instruction words through `BlockOfCode`; Eden invokes the corresponding Oaknut CRC32 instruction methods.

### Unintentional differences (to fix)
- None in the CRC32 ISO and Castagnoli 8/16/32/64-bit instruction families.

### Missing items
- Eden's `SM4AccessSubstitutionBox` unreachable specialization remains the only cryptography opcode specialization not represented by this Rust owner.

### Binary layout verification
- PASS: all eight CRC32 instruction words match Apple's AArch64 assembler for `w16`, `w17`, and `w18`/`x18`. The existing end-to-end ISO CRC32 test now executes on Apple Silicon and matches the expected result.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/jit_state.rs` vs `src/dynarmic/src/dynarmic/backend/x64/a32_jitstate.{h,cpp}`

### Intentional differences

- Rust uses `debug_assert_eq!` for Eden's `DEBUG_ASSERT` on the stored FPSCR NZCV mask.

### Unintentional differences (to fix)

- None in the audited A32 FPSCR slice. Ruzu now stores `fpsr_nzcv` directly in ARM FPSCR bits
  31:28, resets both MXCSR shadows to Eden's exact defaults before applying rounding/FZ, and
  preserves only the lower location-descriptor half before installing FPSCR mode bits.

### Missing items

- None in `A32JitState::{get_fpscr,set_fpscr}`.

### Binary layout verification

- PASS: no fields were added, removed, or reordered; the existing `repr(C, align(16))` layout and
  offset tests remain unchanged.

## 2026-08-21 — `src/rdynarmic/src/frontend/a64/translate/{simd_scalar_three_same.rs,simd_scalar_two_register_misc.rs,visitor.rs}` vs `src/dynarmic/src/dynarmic/frontend/A64/translate/impl/{simd_scalar_three_same.cpp,simd_scalar_two_register_misc.cpp,impl.h}`

### Intentional differences

- Rust's decoded instruction object supplies Eden's `sz`, `Vm`, `Vn`, and `Vd` parameters to the
  matching snake-case methods; the comparison helper boundaries remain file-local like upstream.

### Unintentional differences (to fix)

- None in the focused scalar equality slice. `FCMEQ_reg_2` and `FCMEQ_zero_2` now dispatch and emit
  the same element-size-specific floating-point equality IR as Eden.

### Missing items

- This audit covers only the two scalar FCMEQ methods discovered through warning analysis; the
  remaining A64 translator surface is not claimed complete here.

### Binary layout verification

- N/A: translator dispatch and IR construction serialize no raw payload.

## 2026-08-21 — `src/rdynarmic/src/{bin/a32_diff.rs,ir/opt/a64_get_set_elimination.rs,jit.rs}` warning audit vs Eden developer/test infrastructure

### Intentional differences

- `a32_diff` is a Ruzu-specific differential tool with no Eden source counterpart; removing its
  write-only CPSR divergence flag preserves its diagnostics and resynchronization behavior.
- AArch64-only mock callback builders are compiled only on AArch64, matching the architecture guard
  already applied to their sole tests.

### Unintentional differences (to fix)

- None in this developer/test-only slice.

### Missing items

- None.

### Binary layout verification

- N/A: only local diagnostics and test builders changed.

## 2026-08-21 — `src/rdynarmic/src/frontend/a32/{decoder.rs,decoder_thumb32.rs,translate/thumb32.rs}` vs Eden `src/dynarmic/src/dynarmic/frontend/A32/{decoder,translate/impl}`

### Intentional differences

- Ruzu's handwritten decoder helpers replace Eden's generated instruction-pattern tables. Their
  internal signatures now carry only the bitfields they actually inspect; decoded instruction
  names and translation ownership remain unchanged.
- Regression names describe the observed instruction sequence generically instead of referring to
  a commercial title; opcodes, fixtures, and assertions are unchanged.

### Unintentional differences (to fix)

- None in this dead-local warning slice.

### Missing items

- The broader handwritten-decoder parity surface is outside this focused no-behavior-change audit.

### Binary layout verification

- N/A: decoder locals and helper parameters define no serialized structure.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_vector_arrangement.rs` vs `src/dynarmic/src/dynarmic/backend/x64/emit_x64_vector.cpp` (narrow/sign-extend/zero-extend slice)

### Intentional differences

- Rust releases scratch-register locks explicitly where Eden's register-allocation wrappers release
  them by scope; emitted instruction ordering is otherwise preserved.

### Unintentional differences (to fix)

- None in the focused slice. `VectorSignExtend64` now copies the low 64-bit lane to a GPR, performs
  an arithmetic shift by 63, and installs that sign mask in the high lane like Eden instead of
  incorrectly widening two 32-bit lanes. The 8/16/32-bit sign/zero extensions now also retain
  Eden's SSE2 paths when SSE4.1 is unavailable.

### Missing items

- Other vector-arrangement emitters remain under the separate warning/parity audit.

### Binary layout verification

- N/A: JIT instruction emission changes no shared state layout or serialized payload.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_vector_arrangement.rs` vs `src/dynarmic/src/dynarmic/backend/x64/emit_x64.h` and `emit_x64_vector.cpp` (broadcast/deinterleave slice)

### Intentional differences

- Rust explicitly releases temporary register-allocation locks; Eden releases its scoped register
  wrappers on scope exit. The emitted instruction order is unchanged.
- The focused regression tests provide no-op callback objects because Rust's `EmitContext` owns a
  complete callback configuration even though these vector emitters only query host features.

### Unintentional differences (to fix)

- None in the focused slice. Broadcast emitters now select Eden's AVX2, SSSE3, and SSE2 paths;
  lower broadcasts preserve Eden's upper-lane behavior. Full and lower even/odd deinterleave
  emitters now use Eden's SSE4.1, SSSE3, and SSE2 instruction sequences instead of generic host
  calls or unconditional `pshufb` implementations.
- Removed the `RUZU_BCAST64_*` diagnostic machine-code injection. It had no Eden equivalent and
  could reserve or overwrite architectural host XMM registers outside the register allocator.

### Missing items

- Other vector-arrangement emitters remain under separate parity slices; none of the broadcast or
  deinterleave methods audited here are missing.

### Binary layout verification

- N/A: these methods emit JIT instructions and define no shared or serialized structures.

## 2026-08-21 — `externals/rxbyak/src/assembler.rs` AVX packed immediate shifts

### Intentional differences

- The Rust API suffixes immediate packed-shift overloads with `_imm`, consistent with the existing
  legacy SSE methods, because Rust does not support C++-style method overloading.

### Unintentional differences (to fix)

- None in the focused encoder slice. `vpsllw`, `vpsrlw`, and `vpsrld` immediate forms encode the
  opcode extension in ModRM.reg, the destination in VEX.vvvv, and the source in ModRM.r/m.

### Missing items

- Other AVX packed immediate-shift element widths are not required by the interrupted Eden emitter
  slice and were not part of this focused prerequisite.

### Binary layout verification

- PASS: XMM, YMM, and extended-register encodings are asserted byte-for-byte against NASM output.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_vector_basic.rs` vs `src/dynarmic/src/dynarmic/backend/x64/emit_x64.h` and `emit_x64_vector.cpp` (CLZ/popcount/reverse-bits slice)

### Intentional differences

- Rust explicitly releases temporary register-allocation locks; Eden releases its scoped register
  wrappers on scope exit. The emitted instruction ordering is otherwise preserved.
- Eden's single `emit_x64_vector.cpp` translation unit is split into responsibility-based Rust
  emitter modules; these methods remain together in `emit_vector_basic.rs` and retain their
  one-to-one upstream names and dispatch ownership.

### Unintentional differences (to fix)

- None in the focused slice. The CLZ emitters now preserve Eden's GFNI, SSSE3, AVX, AVX2,
  AVX-512 and fallback selections. Population count now preserves the AVX-512, SSSE3 and fallback
  paths. Reverse-bits now preserves the GFNI, SSSE3 and baseline SSE2 instruction sequences.
- Removed the unused 32-bit CLZ and reverse-bits host fallbacks: Eden has no corresponding
  fallbacks because every supported x86-64 host executes their baseline SSE implementations.

### Missing items

- None among `VectorCountLeadingZeros8/16/32`, `VectorPopulationCount`, and `VectorReverseBits`.
  Other vector-basic methods were outside this warning-driven audit slice.

### Binary layout verification

- N/A: these methods emit JIT instructions and define no shared or serialized structures.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_vector_misc.rs` vs `src/dynarmic/src/dynarmic/backend/x64/emit_x64_vector.cpp` (rounding-halving-add slice)

### Intentional differences

- Rust explicitly releases temporary register-allocation locks; Eden releases its scoped register
  wrappers on scope exit. The emitted instruction ordering is otherwise preserved.
- The public Rust dispatch methods retain their explicit signed/unsigned and element-width names;
  both families delegate to private element-size helpers matching Eden's two static helpers.

### Unintentional differences (to fix)

- None in the focused slice. Signed 8/16-bit RHADD now applies Eden's sign-bit bias around
  `pavgb`/`pavgw`; signed 32-bit and unsigned 32-bit RHADD now use Eden's overflow-safe shift/add
  sequences instead of host callbacks.
- Removed all six scalar RHADD fallbacks. Eden emits native SSE2 for every supported width, so the
  unused unsigned 8/16-bit fallbacks were dead code and the remaining fallbacks represented parity
  debt.

### Missing items

- None among the signed and unsigned 8/16/32-bit rounding-halving-add emitters. Other
  `emit_vector_misc.rs` families remain outside this focused warning-driven audit.

### Binary layout verification

- N/A: these methods emit JIT instructions and define no shared or serialized structures.

## 2026-08-21 — `src/rdynarmic/src/ir/opt/polyfill.rs` and `backend/x64/emit_vector_multiply.rs` vs `src/dynarmic/src/dynarmic/ir/opt_passes.{h,cpp}` and `backend/x64/emit_x64_vector.cpp` (widening-multiply slice)

### Intentional differences

- Rust rebuilds its arena-backed instruction list while preserving the original SSA mapping;
  Eden inserts replacement instructions before the current linked-list node and redirects its
  uses. Both produce two sign/zero extensions followed by a multiply at twice the element width.
- Rust's `unreachable!()` is the direct assertion equivalent of Eden's `UNREACHABLE()` macro.

### Unintentional differences (to fix)

- None in the focused slice. The x64 A32 and A64 pipelines enable widening-multiply polyfill
  unconditionally, and the strengthened regression verifies all six signed/unsigned 8/16/32-bit
  source opcodes are eliminated.
- Removed the six x64 callback/SSE implementations that had no Eden counterpart. The matching x64
  emitters now assert unreachable after legalization exactly like Eden.

### Missing items

- None for x64 widening-multiply legalization and emitter ownership. The ARM64 backend retains its
  native widening emitters, matching Eden's separate ARM64 backend behavior.

### Binary layout verification

- N/A: the polyfill rewrites IR and the emitters define no shared or serialized structures.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_vector_multiply.rs` vs `src/dynarmic/src/dynarmic/backend/x64/emit_x64_vector.cpp` (paired-add slice)

### Intentional differences

- Rust explicitly releases temporary register-allocation locks; Eden releases its scoped register
  wrappers on scope exit. The emitted instruction ordering is otherwise preserved.
- Eden declares emitter ownership through its opcode-driven emitter declaration machinery; Rust's
  matching functions are dispatched explicitly from `backend/x64/emit.rs`.

### Unintentional differences (to fix)

- None in the focused slice. The 8/16/32/64-bit full-width emitters and the 8/16/32-bit lower
  emitters now preserve Eden's exact SSE instruction sequences and SSSE3 feature branches.
- Removed the scalar callback implementations and the Ruzu-only
  `RUZU_FORCE_PAIRED_ADD8_FALLBACK` diagnostic branch. Eden emits these operations natively on
  every supported x86-64 host.

### Missing items

- None among `VectorPairedAddLower8/16/32` and `VectorPairedAdd8/16/32/64`. The adjacent signed
  and unsigned widening paired-add family is intentionally handled as the next auditable slice.

### Binary layout verification

- N/A: these methods emit JIT instructions and define no shared or serialized structures.

## 2026-08-21 — `externals/rxbyak/src/assembler.rs` vs Xbyak 7.35.2 `xbyak_mnemonic.h` (packed immediate qword shifts)

### Intentional differences

- Rust names immediate overloads `vpsllq_imm` and `vpsraq_imm` because Rust does not support the
  C++ API's overloads distinguished only by the final operand type.
- The existing Rust `vex_packed_shift_imm` helper corresponds to Xbyak's shared
  `opAVX_X_X_XM` encoding path and receives the instruction flags explicitly.

### Unintentional differences (to fix)

- None in the focused slice. The qword logical-left and arithmetic-right immediate forms use the
  same opcode extensions, opcodes, W bits, EVEX requirements, broadcast tuple flags, and memory
  EVEX policy as Xbyak 7.35.2.
- The pre-existing word and dword immediate forms now also retain Xbyak's EVEX flags, and the
  common validator accepts equal-width ZMM operands instead of rejecting a supported form.

### Missing items

- None for the immediate `vpsllw`, `vpsrlw`, `vpsrld`, `vpsllq`, and `vpsraq` register forms.
  Other packed-shift overloads were outside this prerequisite slice.

### Binary layout verification

- PASS: XMM, YMM, ZMM, ordinary-register, and extended-register encodings match NASM
  byte-for-byte; the complete rxbyak test suite passes.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_vector_multiply.rs` vs `src/dynarmic/src/dynarmic/backend/x64/emit_x64.h` and `emit_x64_vector.cpp` (widening paired-add slice)

### Intentional differences

- Rust explicitly releases temporary register-allocation locks; Eden releases its scoped register
  wrappers on scope exit. The emitted instruction and value-definition ordering is preserved.
- Rust materializes Eden's `code.Const(xword, ...)` through the emitter-owned constant pool and
  passes the resulting XMM memory operand to `movdqa`.

### Unintentional differences (to fix)

- None in the focused slice. Signed and unsigned 8/16/32-bit widening paired adds now retain
  Eden's exact native instruction sequences. Signed 32-bit widening selects the same
  `AVX512_Ortho` path and preserves the same baseline SSE2 sign-extension construction.
- Removed all six scalar callbacks plus the two alternative `pmaddwd`/`pmaddubsw` implementations.
  They had no Eden counterpart and bypassed its emitter structure.

### Missing items

- None among `VectorPairedAddSignedWiden8/16/32` and
  `VectorPairedAddUnsignedWiden8/16/32`.

### Binary layout verification

- N/A: these methods emit JIT instructions and define no shared or serialized structures. The
  AVX/EVEX prerequisite encodings are independently verified in the preceding rxbyak entry.

## 2026-08-21 — `src/frontend_common/src/config.rs` vs `src/frontend_common/config.h` and `config.cpp` (config-array ownership audit)

### Intentional differences

- Rust names the live array-stack element `ConfigArrayEntry` and exposes it because `BaseConfig`
  is shared across frontend crates; its three fields and stack ownership match Eden's private
  `Config::ConfigArray`.

### Unintentional differences (to fix)

- None in the focused slice. Removed a second, unused `ConfigArray` declaration that duplicated
  the live `ConfigArrayEntry` without owning Eden's `BeginArray`, `EndArray`, or `SetArrayIndex`
  behavior.

### Missing items

- None in the config-array state representation and lifecycle.

### Binary layout verification

- N/A: config-array entries are in-memory parser state and are not serialized by raw layout.

## 2026-08-21 — `src/ruzu_cmd/src/sdl_config.rs` vs `src/yuzu_cmd/sdl_config.h` and `sdl_config.cpp` (configuration-path ownership audit)

### Intentional differences

- Rust composes `BaseConfig` instead of inheriting C++ `Config`; the base object remains the owner
  of the resolved configuration location and INI state.

### Unintentional differences (to fix)

- None in the focused slice. Removed the unused duplicate `SdlConfig::config_path`; Eden's derived
  `SdlConfig` does not retain a second path after `Config::Initialize` stores it in the base.

### Missing items

- None for configuration-path ownership. Other SDL configuration methods are outside this warning
  slice.

### Binary layout verification

- N/A: neither configuration class is serialized by raw object layout.

## 2026-08-21 — `src/ruzu_cmd/src/main.rs` vs `src/yuzu_cmd/yuzu.cpp` (multiplayer CLI slice)

### Intentional differences

- Rust stores the parsed nickname, password, address, and port in one `MultiplayerConfig` value;
  Eden keeps the same four values as separate `SDL_AppInit` locals.
- Rust owns a `RoomNetwork` for the duration of the optional multiplayer session and shuts it down
  before process exit. Eden accesses module-global weak room handles; its CLI source does not call
  `Network::Init()` itself even though `GetRoomMember()` requires that global initialization. The
  explicit Rust owner is required by the target network API and makes the intended Eden join path
  reachable without changing callback or join ordering.
- Rust passes an empty authentication token because the target `RoomMember::join` includes the
  newer token parameter; Eden's focused CLI call predates that parameter.
- The Rust status-message enum contains only Eden's five recognized message kinds, so the C++
  switch's unknown-value/default empty-message branch is unrepresentable.

## 2026-08-21 — `src/network/src/announce_multiplayer_session.rs` vs `src/network/announce_multiplayer_session.h` and `.cpp`

### Intentional differences

- Rust retains a `Weak<Room>` obtained from the constructor's explicit `RoomNetwork`; Eden reaches
  the same room through the process-global `Network::GetRoom()`.
- Rust shares thread-owned state with `Arc`, protects its backend with a mutex, and stores a
  `JoinHandle`; Eden's `std::jthread` captures `this`, relying on destruction/`Stop()` to join before
  the object is released. Both lifecycles signal the same event before joining and delete the web
  registration afterward.
- Rust stores callback handles in a `Vec` rather than `std::set`. Each bind creates a fresh `Arc`,
  unbind still removes only the matching identity, and invocation remains serialized under the
  callback mutex.
- The target always links `web_service`, so construction and credential updates always create
  `RoomJson`, matching Eden's `ENABLE_WEB_SERVICE` build. The target setting fields retain their
  existing `yuzu_username`/`yuzu_token` names.
- Rust computes the remaining duration before calling `Event::wait_for`; Eden passes the equivalent
  absolute `steady_clock` deadline to `WaitUntil`.

## 2026-08-21 — `src/rdynarmic/src/backend/arm64/emit_arm64_cryptography.rs` vs Eden `src/dynarmic/src/dynarmic/backend/arm64/emit_arm64_cryptography.cpp` (AES operations)

### Intentional differences

- Rust emits the four AArch64 instruction words through the local `inst.rs` encoder rather than
  Oaknut. Register allocation, realization, and instruction ordering remain identical.
- The two single-round operations share a mechanical Rust helper, as do the two mix-column
  operations; each helper preserves the corresponding upstream method body and state ownership.

### Unintentional differences (to fix)

- None in `AESDecryptSingleRound`, `AESEncryptSingleRound`, `AESInverseMixColumns`, or
  `AESMixColumns`.

### Missing items

- The CRC32, SHA-256, and SM4 opcode owners from the same upstream file are not yet ported to the
  ARM64 backend.

## 2026-08-21 — `src/web_service/src/announce_room_json.rs` vs `src/web_service/announce_room_json.h` and `.cpp`

### Intentional differences

- Rust implements Eden's file-local nlohmann `to_json`/`from_json` helpers with file-local
  `serde_json::Value` conversion helpers. Required-field failures panic where Eden propagates a
  JSON exception; missing optional descriptions, player lists, and member identities retain the
  same fallback behavior.
- Rust's detached delete worker creates an equivalent `Client` from the stored host and
  credentials because a safe thread cannot borrow `self`. The shared JWT cache preserves the
  authenticated request, and `Drop` joins every retained worker like destruction of Eden's
  `std::jthread` vector.
- The Rust backend methods take `&mut self` through the `Backend` trait instead of relying on C++
  object mutability.

## 2026-08-21 — `src/web_service/src/verify_login.rs` vs `src/web_service/verify_login.h` and `.cpp`

### Intentional differences

- Rust uses `serde_json` and panics on malformed JSON where Eden's nlohmann parse propagates an
  exception.

## 2026-08-21 — `src/video_core/src/vulkan_common/vulkan_memory_allocator.rs` vs `src/video_core/vulkan_common/vulkan_memory_allocator.h` and `.cpp`

### Intentional differences

- Rust stores the VMA allocator in `Arc<Mutex<_>>` because the target creates VMA with external
  synchronization; Eden stores its opaque `VmaAllocator` handle directly.
- Rust names Eden's const `Map()` overload `map_read` and requires mutable access while it may cache
  the mapped pointer. The mutable `map`, `unmap`, byte-span length, and cached-pointer lifecycle are
  otherwise shared.
- Allocation failures return `VulkanError` through `Result`; Eden propagates `vk::Check` exceptions.
- `buffer_image_granularity` is retained with a local dead-code annotation because current Eden
  still owns and initializes that device limit without reading it.

### Unintentional differences (to fix)

- Eden reports `MemoryCommit`, image, and buffer allocations/deallocations through `GPULogger` when
  GPU memory tracking is active. Ruzu does not yet have the corresponding GPU logging subsystem.
- Ruzu's raw-handle `create_image`, `create_buffer`, and `create_mapped_buffer` compatibility paths
  still use dedicated Vulkan allocations. Eden returns owning VMA-backed `vk::Image`/`vk::Buffer`
  wrappers; `create_owned_buffer` already uses VMA but the remaining callers have not all migrated.

## 2026-08-21 — `rdynarmic/backend/arm64/emit_arm64_data_processing.rs` vs Eden `dynarmic/backend/arm64/emit_arm64_data_processing.cpp` (masked shifts)

### Intentional differences
- Rust casts the masked shift count to `u8` only after applying Eden's 32-bit or 64-bit mask, because the local instruction encoders accept the already-valid immediate as `u8`.

### Unintentional differences (to fix)
- None in the immediate and register forms of 32-bit and 64-bit masked logical-left, logical-right, arithmetic-right, and rotate-right shifts.

### Missing items
- None in this masked-shift helper slice.

### Binary layout verification
- N/A: this change only selects an AArch64 shift immediate. A focused regression emits all eight masked-shift opcodes with full-width constants whose upper bits are nonzero.

## 2026-08-21 — `src/rdynarmic/src/backend/arm64/emit_arm64_data_processing.rs` vs Eden `src/dynarmic/src/dynarmic/backend/arm64/emit_arm64_data_processing.cpp` (scalar integer min/max)

### Intentional differences

- Rust writes the `CMP` and `CSEL` instruction words through the local `inst.rs` encoder rather
  than Oaknut. Argument acquisition, W/X register allocation, realization, flag spilling, and
  instruction ordering are identical.

### Unintentional differences (to fix)

- None in `MaxSigned32/64`, `MaxUnsigned32/64`, `MinSigned32/64`, or `MinUnsigned32/64`.

### Missing items

- None in this scalar integer min/max slice.

### Binary layout verification

- N/A: these methods emit host instructions and do not serialize a shared structure. A focused
  regression routes all eight IR opcodes through the ARM64 data-processing owner.

## 2026-08-21 — `src/video_core/src/control/channel_state_cache.rs` vs `src/video_core/control/channel_state_cache.h`, `.cpp`, and `.inc`

### Intentional differences

- Rust's `&mut self` mutation methods exclude concurrent `&self` lookups at the type boundary, so
  it does not retain Eden's inner `config_mutex`; the cache owners must provide any cross-thread
  synchronization around the complete `ChannelSetupCaches` value.

## 2026-08-21 — `src/video_core/src/host1x/ffmpeg/ffmpeg.rs` and `ffmpeg_shim.c` vs `src/video_core/host1x/ffmpeg.h` and `.cpp`

### Intentional differences

- The native C shim retains Eden's `DecoderContext::m_decoder` codec pointer inside
  `RuzuFfmpegDecoder`; the Rust wrapper therefore does not duplicate that codec or hold a
  self-referential borrow into `DecodeApi::decoder`.

## 2026-08-21 — `src/video_core/src/renderer_vulkan/present/fsr.rs` vs `src/video_core/renderer_vulkan/present/fsr.h` and `.cpp`

### Intentional differences

- `CreateImages` borrows `MemoryAllocator` for the call instead of retaining Eden's allocator
  reference in `FSR`; retaining a borrow would make the owning Rust `Layer` self-referential.
- `CreateShaders` receives the already-queried float16 capability because the local raw
  `ash::Device` does not own Eden's `Device::IsFloat16Supported` capability state. Shader
  selection and construction order otherwise match Eden.

## 2026-08-21 — `src/video_core/src/renderer_vulkan/present/util.rs` vs `src/video_core/renderer_vulkan/present/util.h` and `.cpp` (`UploadImage`)

### Intentional differences

- The scheduler requires a `'static` command closure, so Rust copies the Vulkan device and raw
  staging-buffer handle into the closure. The owning mapped buffer remains in the function until
  the matching `Scheduler::finish`, preserving Eden's staging allocation lifetime and command
  order.

## 2026-08-21 — `src/video_core/src/renderer_vulkan/present/smaa.rs` vs `src/video_core/renderer_vulkan/present/smaa.h` and `.cpp`

### Intentional differences

- Rust retains Eden's non-owning `MemoryAllocator&` as `NonNull<MemoryAllocator>` because the
  allocator is owned by the enclosing Vulkan renderer and a lifetime borrow would make its `Layer`
  storage self-referential. The pointer is consumed only by `CreateImages` and `UploadImages`, at
  the same lifecycle points as Eden.
- The raw `ash::Device` is retained by `Smaa` because the local `AntiAliasPass::draw` trait does not
  receive Eden's `Device&`; resource selection and command ordering are unchanged.

## 2026-08-21 — `src/video_core/src/renderer_vulkan/present/layer.rs` vs `src/video_core/renderer_vulkan/present/layer.h` and `.cpp`

### Intentional differences

- Eden's Vulkan RAII wrappers release descriptor pools and image views implicitly. Rust owns raw
  handles, so `Drop` destroys the descriptor pool explicitly and `ReleaseRawImages` destroys each
  image view after the existing tick wait; the enclosing renderer waits for device idle before
  final field destruction.
- The tail of `ConfigureDraw` remains a same-file Rust helper that accepts the already-resolved
  source image, dimensions, layout, and normalized crop. The upstream-owned framebuffer lookup and
  crop computation remain in `configure_draw_from_framebuffer`.
- Rust represents Eden's retained allocator and scheduler references with stable `NonNull` pointers
  and retains the shared device-memory owner with `Arc`. Their enclosing renderer owners outlive
  every `Layer`; `ReleaseRawImages` now performs its own tick waits as Eden does.

## 2026-08-21 — `src/video_core/src/renderer_vulkan/present/sgsr.rs` vs `src/video_core/renderer_vulkan/present/sgsr.h` and `.cpp`

### Intentional differences

- Rust uses the constructor's `edge_dir` parameter directly when selecting the fragment shader
  instead of retaining Eden's `m_edge_dir` member. Eden only reads that member during construction,
  so retaining a second copy after the shader has been created would be dead state.

## 2026-08-21 — `src/video_core/src/buffer_cache/buffer_cache.rs` vs `src/video_core/buffer_cache/buffer_cache_base.h` and `buffer_cache.h`

### Intentional differences

- Rust omits Eden's `last_index_count` and `immediate_buffer_capacity` members. Neither member is
  read or updated upstream: indirect draw state is held by the channel bindings, while
  `ScratchBuffer::resize_destructive` manages the immediate allocation capacity internally.

## 2026-08-21 — `src/video_core/src/host1x/gpu_device_memory_manager.rs` vs `src/core/device_memory_manager.h` and `.inc` (`UpdatePagesCachedBatch`)

### Intentional differences

- Ruzu exposes the batch operation through its `DeviceTracker` trait so the generic Rust word
  manager can call the concrete `MaxwellDeviceMemoryManager`; Eden's C++ template parameter calls
  that concrete method directly.
- Ruzu's public single-range path returns before acquiring a range lock when `size == 0`. Eden
  still acquires a zero-length lock and reads the initial CPU-backing entry, but performs no page
  counter update or caching callback.

## 2026-08-21 — `src/video_core/src/buffer_cache/word_manager.rs` vs `src/video_core/buffer_cache/word_manager.h`

### Intentional differences

- Ruzu omits Eden's now-unused `NotifyRasterizer` helper after all three mutation paths moved to
  `CollectChangedRanges` and `ApplyCollectedRanges`; keeping the superseded single-range helper
  would only retain dead code.
- The Rust callback adapter uses `Option<bool>` to represent Eden's compile-time distinction
  between callbacks returning `bool` and callbacks returning `void`.
- A null tracker is tolerated by the default/empty Rust manager and discards collected ranges;
  Eden's default constructor also leaves `tracker` null, but invoking a notifying mutation on that
  object would dereference it.

### Unintentional differences (to fix)

- Eden's `size_bytes` is a template parameter and its five tracking channels occupy one fixed
  `std::array`. Ruzu stores `size_bytes` at runtime and uses separate stack-or-heap channel views.
  Restoring that structural/layout parity requires changing the manager-pool type graph and is
  outside this local batching slice.

## 2026-08-22 — `src/video_core/src/vulkan_common/vulkan_debug_callback.rs` vs `src/video_core/vulkan_common/vulkan_debug_callback.h` and `.cpp`

### Intentional differences

- Rust's `DebugUtilsMessenger` owns both the Vulkan handle and the `ash` extension loader needed
  to destroy it. This is the RAII counterpart of Eden's `vk::DebugUtilsMessenger`, whose instance
  dispatch table is retained by the wrapper internally.

### Missing items

- Eden additionally forwards validation messages to `GPU::Logging::GPULogger` when Vulkan-call
  logging is active. Ruzu does not yet have that GPU logging subsystem.

## 2026-08-22 — `src/core/src/device_memory_manager.rs` vs `src/core/device_memory_manager.h` and `.inc`

### Intentional differences

- The inactive generic Rust implementation was removed instead of preserving a second device
  memory manager that no runtime code instantiated. Ruzu's working Maxwell implementation remains
  in `src/video_core/src/host1x/gpu_device_memory_manager.rs`; this module retains the three public
  page constants owned by the upstream header.

### Unintentional differences (to fix)

- The active implementation is still owned by `video_core/host1x` rather than this `core` module.
  Moving it requires replacing the current opaque `Host1xCoreInterface` crate boundary without
  introducing a `core`/`video_core` dependency cycle; this is a structural refactor, not a local
  warning cleanup.

## 2026-08-22 — Dynarmic backend retained construction state

### Intentional differences

- `ArmDynarmic64` passes the exclusive monitor and core index directly into its owned callback and
  JIT configuration instead of retaining an additional parent-level copy after construction.
  Eden retains both members because `MakeJit` reads them; Ruzu builds its JIT inline and the active
  callback copies remain alive with the JIT.
- AArch64 TPIDR storage uses stable `Box<u64>` allocations owned by `ArmDynarmic64`, because the
  Rust JIT owns its callback object and cannot safely point back into that moving object during
  construction. The unused callback-local TPIDR duplicates were removed; context transfer and the
  JIT configuration continue to use the stable allocations.
- The unused AArch32 `trace_read_code_word` diagnostic helper was removed. Instruction fetch still
  goes through the port of Eden's `DynarmicCallbacks32::MemoryReadCode` and its code-page cache.

## 2026-08-22 — `src/core/src/arm/dynarmic/dynarmic_cp15.rs` vs `src/core/arm/dynarmic/dynarmic_cp15.h` and `.cpp`

### Intentional differences

- Eden's CP15 prefetch-flush handler returns a pointer to a global `dummy_value` because the C++
  Dynarmic API requires writable storage for an ignored MCR operand. Ruzu's rdynarmic x64 and
  AArch64 emitters consume and discard that operand directly, so the port returns
  `SendOneWordResult::DummyWrite` without retaining an unread per-instance value.

## 2026-08-22 — NCE signal constant ownership

### Fixed parity debt

- `arm_nce.rs` no longer redeclares the four Linux signal constants owned upstream by
  `arm_nce_asm_definitions.h`. The signal mask, handler installation, and interrupt path now use
  the constants from the matching `arm_nce_asm_definitions.rs` module.

## 2026-08-22 — `src/core/src/memory/cheat_engine.rs` vs `src/core/memory/cheat_engine.h` and `.cpp`

### Intentional differences

- The timing callback owns a weak reference to mutex-protected cheat state rather than Eden's raw
  `this` capture. This prevents the event queue from extending the engine lifetime while retaining
  Eden's event ownership, 12 Hz period, callback ordering, and destructor unscheduling.
- Metadata is shared with `StandardVmCallbacks` through `Arc<Mutex<_>>`; Eden stores a reference to
  the engine member. Both arrangements make later main/heap/alias/ASLR extent updates visible to
  every VM memory access.
- `VmCallbacks` is `Send + Sync` because Ruzu's `CoreTiming` callback may execute on its timing
  thread; Eden's callback object is likewise invoked by `CoreTiming` but expresses that contract
  through C++ ownership rather than a type bound.

### Fixed parity debt

- `System` now owns the runtime engine, initializes it after CPU setup, and destroys it before
  pending `CoreTiming` events are cleared.
- The VM callbacks now use the live application memory, HID Npad press state and process activity;
  successful writes invalidate every live ARM instruction cache.
- Initialization now publishes process/title identifiers and heap, alias and ASLR extents before
  requesting the first VM reload, in Eden's order.

## 2026-08-22 — `src/core/src/file_sys/patch_manager.rs` vs `src/core/file_sys/patch_manager.h` and `.cpp`

### Intentional differences

- Eden passes arbitrary file bytes to a `std::string_view`; Ruzu's existing `TextCheatParser`
  accepts UTF-8 text, so cheat files are decoded lossily. The cheat grammar itself remains ASCII,
  and build-ID lookup, directory ordering, disabled-addon filtering and uppercase/lowercase lookup
  order match Eden.

### Fixed parity debt

- `PatchManager::create_cheat_list` now discovers per-mod build-ID files and root `cheat_*` files
  in the same order as Eden and feeds them to the owning cheat parser.

## 2026-08-22 — `src/core/src/loader/nso.rs` and `deconstructed_rom_directory.rs` vs `src/core/loader/nso.cpp` and `deconstructed_rom_directory.cpp`

### Intentional differences

- The second deconstructed-ROM pass borrows one `PatchManager` instead of copying an
  `std::optional<PatchManager>` into every NSO call; its lifetime and per-module behavior are the
  same.
- Ruzu rejects a patched NSO whose size changed before copying it back. Eden relies on
  `PatchNSO` preserving the image size and performs an unchecked copy into the original span.

### Fixed parity debt

- The first layout pass remains unpatched, while the second pass now applies IPS/IPSwitch patches,
  publishes the application build ID, discovers cheats and requests their registration before
  loading the module, matching Eden's ordering.

### Missing items

- Eden's optional NCE `PatchCollection` path still has no counterpart in this loader. It is a
  pre-existing backend-wide prerequisite; the Dynarmic NSO patch and cheat path ported here does
  not depend on it.

## 2026-08-22 — `src/core/src/core.rs` and `src/core/src/loader/loader.rs` vs `src/core/core.h`, `core.cpp`, and loader calls

### Intentional differences

- Loader calls record build-ID and cheat registration in the Rust loader bridge and apply them
  immediately after `AppLoader::load` returns. This avoids aliasing the `&mut Core::System` already
  owned by the caller while preserving Eden's ordering before cheat-engine initialization.

### Fixed parity debt

- `System::register_cheat_list`, the `System`-owned engine, application-process metadata setup and
  ordered initialization/shutdown now mirror Eden's lifecycle.

## 2026-08-22 — `src/core/src/arm/debug.rs` vs `src/core/arm/debug.h` and `.cpp`

### Intentional differences

- The Rust function receives `&mut KProcess` because `ArmInterface::invalidate_cache_range`
  requires mutable access; Eden reaches the same mutable interfaces through a const process
  pointer and C++ pointer ownership.

### Fixed parity debt

- Instruction-cache invalidation now visits every live per-core ARM interface instead of being a
  no-op.

## 2026-08-22 — `src/core/src/hle/kernel/k_thread_queue.rs` vs `src/core/hle/kernel/k_thread_queue.h` and `.cpp`

### Intentional differences

- Rust represents C++ virtual derived queues with cloneable callbacks. Stateful cancellation
  callbacks retain their derived queue context through `Arc`, receive Eden's wait result and timer
  cancellation flag, and return whether the base `CancelWait` transition must run.
- Hardware timers are retained by `Arc` while a Rust queue is active rather than by Eden's raw
  non-owning pointer.
- `KThreadQueueWithoutEndWait::end_wait` preserves the waiting state after logging unless the
  diagnostic panic switch is enabled; Eden treats this impossible path as immediately
  unreachable. This pre-existing recovery policy is unchanged by the callback work.

### Fixed parity debt

- A derived queue can now return from `CancelWait` without running the base state transition,
  which is required by `ThreadQueueImplForKLightConditionVariable` for allowed termination.

## 2026-08-22 — `src/core/src/hle/kernel/k_light_condition_variable.rs` vs `src/core/hle/kernel/k_light_condition_variable.h` and `.cpp`

### Intentional differences

- The kernel scheduler and hardware timer are resolved through Ruzu's active-kernel owner instead
  of retaining Eden's `KernelCore&` member.
- Eden's intrusive `KThread::WaiterList` is represented by insertion-ordered weak thread owners
  behind a mutex. This keeps stable thread identity without extending thread lifetime; the
  scheduler lock still owns every runtime mutation and wake transition.
- The derived wait queue owns a shared waiter-list handle instead of a raw pointer to the stack
  owner's list. Its lifetime remains tied to the active wait queue.

### Fixed parity debt

- Waiting no longer uses a host `Condvar`. It now releases the light lock under the scheduler lock,
  registers the absolute hardware-timer task, begins the guest-thread wait, and reacquires the
  light lock in Eden's order.
- Cancellation preserves Eden's special allowed-termination branch and otherwise removes exactly
  the cancelled thread before the base transition. Broadcast wakes and erases waiters in insertion
  order.

## 2026-08-22 — `src/video_core/src/buffer_cache/buffer_cache.rs` vs `src/video_core/buffer_cache/buffer_cache_base.h` and `buffer_cache.h`

### Intentional differences

- `ImmediateBufferWithData` and `ImmediateBuffer` are associated functions receiving the device
  memory and scratch allocation explicitly. This lets Rust borrow the scratch allocation and the
  backend runtime independently; their ownership, continuity checks, fallback read, and callers
  otherwise match Eden.

## 2026-08-22 — scalar query writes in `memory_manager.rs`, `engines/maxwell_3d.rs`, and `renderer_opengl/gl_rasterizer.rs`

### Intentional differences

- `Maxwell3D::stamp_query_result` returns whether its optional memory-manager owner exists. The
  reduced command-engine fixture queues the same payload for its external guest-memory writer when
  that owner is absent; Eden's production `Maxwell3D` always has a `MemoryManager&`.
- An unmapped scalar `MemoryManager::read`/`write` logs and returns the upstream fallback value or
  performs no write. Eden logs through its configurable fail-soft `ASSERT(false)` before reaching
  the same fallback, while Ruzu has no equivalent global fail-soft assertion controller.

## 2026-08-22 — renderer lifetime owners vs Eden OpenGL/Vulkan renderer owners

### Intentional differences

- Rust runtime bridges copy `NonNull` pointers or `Arc` handles from the renderer-owned staging,
  descriptor, render-pass, memory, state, surface, and instance members. The original members stay
  in their upstream owners to preserve stable addresses and destruction order, even though their
  remaining direct role is ownership; each resulting `dead_code` false positive is suppressed on
  that field only.
- The asynchronous compute-pipeline closure captures the shader hash by value because it is built
  before `ComputePipeline` exists. Eden captures `this` and reads the corresponding member; Ruzu
  retains that member for structural parity and annotates only its otherwise unread field.

## 2026-08-22 — `shader_recompiler/frontend/structured_control_flow.rs` and translation driver vs Eden Maxwell frontend

### Intentional differences

- Eden stores executable statements and condition expressions in one tagged `Statement` union.
  Rust uses separate `Statement` and `Expr` enums so every variant owns only valid initialized
  data; the synthetic upstream `Function` node is represented by `GotoPass::root`. Removed enum
  variants and `HasChildren` were therefore duplicate C++-layout remnants, not reachable states.
- The slice-based compatibility translation entry point has no `Environment`, so its CFG ranges
  are already instruction-slice indices and its shader stage is owned by `Program`. The unused
  `base_offset` and duplicate `stage` materialization arguments were removed; the environment-based
  path continues to pass `base_offset` to `build_cfg_from_env` where byte-addressed locations need it.

### Unintentional differences (to fix)

- The Maxwell frontend is still flattened under `frontend/` instead of mirroring Eden's
  `frontend/maxwell/` directory.
- Part of Eden's `frontend/maxwell/translate_program.cpp` driver still lives in Ruzu's
  `pipeline_cache.rs`; its compile/cache callers and translation ownership need to be separated.

## 2026-08-22 — GTK configuration helpers and render-window lifecycle vs Eden Qt frontend

### Intentional differences

- `days_from_civil` is compiled on non-Unix hosts (and in tests) only. Unix production builds use
  `libc::mktime`, matching Qt's local-time conversion without carrying the portable fallback as
  dead code.
- Cairo's Joy-Con SL/SR buttons use the direction-free `round_button` primitive directly, so the
  Qt-only `Direction::None` sentinel is not part of the Rust direction enum.
- Ruzu has one console-mode label table for the status control. Eden reuses its configuration map
  there; Ruzu's system page is a handheld-mode checkbox and therefore has no second table consumer.
- `GtkEmuWindow` is retained by `RenderHandles` for the full session. GTK map/unmap signals update
  its shared visibility flag, and platform resize paths call its framebuffer-layout method; this is
  the GTK ownership counterpart of Eden's persistent `GRenderWindow`.

## 2026-08-22 — controller preview live status vs Eden PlayerControlPreview

### Intentional differences

- Eden updates `led_pattern` and `battery_values` from controller callbacks; the GTK preview takes
  the same controller-owned snapshot during its existing 16 ms refresh and redraws only when the
  snapshot changes. Connection loss still clears all four LED bits, while both raw battery values
  and all of Eden's per-controller drawing positions are preserved.

## 2026-08-22 — GTK multiplayer errors vs Eden MultiplayerState and ErrorManager

### Intentional differences

- Eden owns the `RoomMember::Error` callback in its persistent `MultiplayerState`; Ruzu binds the
  same error stream in each active join/room dialog because its GTK frontend has no equivalent
  persistent state widget. All twelve error variants use Eden's exact message mapping.
- Messages used only by Eden's unported host-room dialog are not retained in Ruzu's client-only
  `ErrorManager`. Eden's unused `NO_INTERNET` and `GENERIC_ERROR` constants are omitted as dead
  frontend code.

## 2026-08-22 — `src/hid_core/src/resources/hid_firmware_settings.rs` vs `src/hid_core/resources/hid_firmware_settings.h` and `.cpp`

### Intentional differences

- Eden retains and loads `is_firmware_update_failure_emulated`, but never reads it when answering
  any HID query. Ruzu omits that dead state while retaining the separate four-byte
  `FirmwareSetting` returned by `GetFirmwareUpdateFailure`.
- `hid_core` is independent from Ruzu's `core` crate, so this owner currently initializes Eden's
  normal defaults instead of holding an `ISystemSettingsServer` service pointer. The returned
  firmware-failure and per-ID feature containers now preserve Eden's exact 4-byte and 0xA8-byte
  shapes rather than the previous placeholder `u32` wrappers.

## 2026-08-22 — `src/hid_core/src/hidbus/hidbus_base.rs` vs `src/hid_core/hidbus/hidbus_base.h` and `.cpp`

### Intentional differences

- Ruzu stores the transfer-memory address as a raw `u64`; Eden's `ProcessAddress` is the same
  address-width value with a C++ strong type.

### Missing items

- The base still lacks Eden's service-context-owned asynchronous command event. Its activation
  callbacks are consequently dispatched by the concrete Rust device owner rather than through
  C++ virtual calls in `HidbusBase`.

## 2026-08-22 — `src/hid_core/src/irsensor/clustering_processor.rs` vs `src/hid_core/irsensor/clustering_processor.h` and `.cpp`

### Missing items

- The clustering algorithm is ported, but the IRS service does not yet construct this processor.
  Consequently Ruzu cannot yet retain Eden's `EmulatedController`, register its IR callback, set
  the camera format on that controller, or publish states into the device-owned clustering LIFO.
  The default configuration now derives its maximum pixel count from the same owner-local format
  constant as Eden instead of duplicating `width * height`.

## 2026-08-22 — `src/hid_core/src/hidbus/ringcon.rs` vs `src/hid_core/hidbus/ringcon.h` and `.cpp`

### Intentional differences

- The default Rust constructor has no controller so isolated HIDBus tests can build the device;
  production construction can provide Eden's Player1 controller through `new_with_input`.

### Missing items

- `RingController::on_update` now fills the same ten-entry six-axis accessor in Eden's order, but
  cannot perform Eden's final `ApplicationMemory::WriteBlock`: process memory is owned by Ruzu's
  `core` crate and `hid_core` cannot depend back on it. The HIDBus service owner must copy this
  accessor to the configured transfer-memory address after calling the device update.
- Command completion still lacks Eden's service-context-owned asynchronous event, as recorded for
  `hidbus_base.rs`.

## 2026-08-22 — `src/hid_core/src/resources/abstracted_pad/abstract_pad_holder.rs` vs `src/hid_core/resources/abstracted_pad/abstract_pad_holder.h` and `.cpp`

### Intentional differences

- Rust retains each live `IAbstractedPad` through `Arc<Mutex<_>>` instead of Eden's non-owning raw
  pointer. Enumeration clones the shared owner, preserving pad identity and subsequent state
  changes while preventing a dangling pointer.
- The Rust span adapter reports the number of entries actually copied when a destination is too
  short. Eden assumes its caller always supplies the five-element buffer used throughout HID.
- Eden's `AbstractAssignmentHolder` duplicates each pad's `interface_type`, but no Eden method
  reads that cached copy; Ruzu omits it and reads the live pad when interface data is needed.

## 2026-08-22 — abstract-pad ownership across `abstract_pad.rs`, `abstract_properties_handler.rs`, and `abstract_mcu_handler.rs`

### Intentional differences

- Rust shares the holder and properties handler with `Arc<Mutex<_>>` instead of storing sibling raw
  pointers configured after construction. The same live objects remain owned by `AbstractPad`, and
  each method stays in its upstream file owner.
- Eden retains property-cache fields that its current `UpdateDeviceType`, `UpdateDeviceColor`,
  `UpdateFooterAttributes`, and `UpdateDeviceProperties` TODO bodies never populate or consume.
  Ruzu omits those dead fields while retaining `applet_ui_type`, which its public getters read.

### Missing items

- `AbstractPad::SetExternals` still lacks the dedicated six-axis resource, Palma resource,
  vibration resource, and HID-core connections. The shared applet resource is now connected to the
  button and six-axis handlers; its other consumers remain to be ported.

## 2026-08-22 — abstract button, six-axis, and LED handler warning state

### Intentional differences

- Rust passes the shared `AppletResource` and properties owners into the button and six-axis
  handlers with `Arc<Mutex<_>>`. Their ARUID traversal, Npad entry selection, and upstream helper
  call order are restored without sibling raw pointers.
- `NPad::on_update` defers `AbstractPad::Update` until after releasing its controller and applet
  resource guards. Eden's raw-pointer graph can call it inline; the deferred Rust call preserves
  the same update set while avoiding recursive acquisition of the shared applet mutex.
- Eden's `gc_sampling_number`, `gc_trigger_state`, and `led_interval` are never read or updated by
  their owning implementations. Ruzu omits this dead state; the live button and LED pattern state
  remains unchanged.

### Missing items

- Eden's button path augments `style_tag.system_ext` from `SharedNpadResource`. Ruzu's
  `NpadResource` is not yet a shared external of `AbstractPad`; this currently has no output effect
  because all seven style-specific button writers are also TODO bodies in Eden.
- The Palma, vibration, dedicated six-axis resource, and remaining applet-resource consumers are
  not yet connected through `AbstractPad::set_applet_resource`.

## 2026-08-22 — `src/hid_core/src/resources/abstracted_pad/abstract_battery_handler.rs` vs `src/hid_core/resources/abstracted_pad/abstract_battery_handler.h` and `.cpp`

### Intentional differences

- Rust shares the holder, properties handler, and applet resource with `Arc<Mutex<_>>`; battery
  selection, low-battery flag clearing, change detection, and shared-memory publication otherwise
  preserve Eden's ordering and assignment-style rules.

## 2026-08-22 — `src/input_common/src/drivers/mod.rs` vs `src/input_common/CMakeLists.txt`

### Intentional differences

- Rust applies `cfg(target_os = "android")` to the Android driver module, which is the direct
  counterpart of Eden adding `drivers/android.{h,cpp}` only inside `if (ANDROID)`.

### Missing items

- The Android driver still lacks Eden's JNI device registry, vibration worker, and JNI-backed
  device mapping. Keeping the owner-local constants behind the same platform boundary avoids
  treating that unported Android behavior as dead Linux code.

## 2026-08-22 — `src/input_common/src/drivers/mod.rs` and `Cargo.toml` vs `src/input_common/CMakeLists.txt` (`ENABLE_LIBUSB`)

### Intentional differences

- Ruzu names the opt-in Cargo feature `libusb`; Eden uses the CMake option `ENABLE_LIBUSB`. Both
  keep the GameCube adapter source outside builds that do not provide its USB transport.

### Missing items

- The feature is not enabled by any frontend because the Rust port still lacks Eden's libusb
  context/device owners, scan/input threads, and `InputSubsystem::Impl` registration. That
  prerequisite is recorded in `PORTING_STATE.md`; the unfinished adapter is no longer compiled as
  if it were a functional default driver.

## 2026-08-22 — `src/input_common/src/drivers/joycon.rs` and `helpers/joycon_*` vs Eden's Joy-Con driver

### Intentional differences

- The always-registered Rust `joycon` engine currently retains parameter parsing, automapping,
  and UI naming so existing controller configurations remain readable. The unconsumed hardware
  stack is compiled only with the `joycon-hid` feature until its SDL3 HID owner is ported.

### Missing items

- Eden's controller arrays, HID enumeration, device registration, report input thread, protocol
  callbacks, and hardware output methods remain absent. Private no-op `Setup`, `ScanThread`, name,
  and result-translation bodies were removed because no state or caller could reach them; the
  exact prerequisite and resume condition are recorded in `PORTING_STATE.md`.

## 2026-08-22 — `src/input_common/src/drivers/mouse.rs` vs `src/input_common/drivers/mouse.h` and `.cpp`

### Intentional differences

- Rust collects pending input callbacks while holding the shared engine lock and dispatches them
  after releasing it. Stick decay, motion clamping, sample publication, and mutation ordering
  otherwise follow Eden's `NotifyChanged` path.
- Eden's `last_mouse_position` is initialized and assigned but never read. Ruzu omits this dead
  host-only field while retaining the live origin, stick, motion, wheel, and button state.

## 2026-08-22 — `src/input_common/src/drivers/sdl_driver.rs` duplicate mapping helpers

### Intentional differences

- The pure dual-controller side-selection predicate is a module-local Rust function in the same
  owner file, while Eden spells it as a private `SDLDriver` method. Ruzu's unused second copy was
  removed along with three stale parameter-package builders; the active owner-local builders are
  the ones called by `build_param_for_binding` and preserve Eden's `invert` key and value rules.

## 2026-08-22 — `src/input_common/src/drivers/udp_client.rs` vs `src/input_common/drivers/udp_client.h` and `.cpp`

### Intentional differences

- Eden retains `PadData::pad_index` and a `DeviceStatus` containing a mutex plus optional touch
  calibration, but no UDP implementation reads them. Ruzu omits that dead per-pad state; active
  touch calibration continues to come from `Settings::touch_device` in `on_pad_data`, as it does
  in Eden.

## 2026-08-22 — `src/input_common/src/helpers/stick_from_buttons.rs` updater lifetime

### Intentional differences

- Rust names the stored updater device `_updater` to make its ownership-only role explicit. It
  must remain alive because `InputFromButton::drop` unregisters its engine callback; Ruzu keeps the
  same lifetime as Eden's `unique_ptr` while leaving `force_update` limited to the five user
  inputs, exactly as upstream does.

## 2026-08-22 — `src/core` mechanical warning cleanup

### Intentional differences

- Rust no longer imports types that the corresponding ported implementation does not reference,
  no longer marks immutable bindings `mut`, and spells the inferred `BucketTree::Visitor` lifetime
  explicitly. These are compile-time ownership and namespace details; variables whose unused state
  may indicate missing Eden behavior were deliberately left unchanged for separate parity review.

## 2026-08-22 — `src/core/src/file_sys/fsmitm_romfsbuild.rs` vs `core/file_sys/fsmitm_romfsbuild.h` and `.cpp`

### Intentional differences

- Eden sorts vectors of `shared_ptr` nodes, whose pointees and parent links remain stable. Ruzu's
  nodes are index-backed, so it sorts separate index vectors and leaves node storage stable. The
  emitted file/directory ordering, reverse sibling ownership, entry offsets, hashes and metadata
  layout follow Eden's order.

### Fixed parity debt

- `add_directory` and `add_file` now assign parent ownership themselves, and `add_file` returns
  Eden's success value. The former path-based repair contained an empty parent loop and a
  placeholder sibling expression; both are removed in favor of the upstream ownership sequence.

## 2026-08-22 — `src/core/src/file_sys/fs_path_utility.rs` vs `core/file_sys/fs_path_utility.h`

### Intentional differences

- Eden advances raw `src` pointers and retains the pre-parse pointer to calculate consumed bytes.
  Ruzu's parsers return that consumed offset directly, so the duplicate `src_offset` and
  pre-Windows-parse binding were dead Rust locals and have been removed. Normalization order,
  length checks, backslash replacement and the preserved Nintendo `cur_pos + 1` bug are unchanged.

## 2026-08-22 — `src/core/src/file_sys/fssystem/compressed_storage.rs` vs `core/file_sys/fssystem/fssystem_compressed_storage.h`

### Intentional differences

- Rust read callbacks receive checked mutable slices instead of `(void*, size_t)`, and `Option<Entry>`
  represents Eden's `prev_entry.virt_offset == -1` sentinel without changing the batching order.
- Eden's `AccessRange::physical_size` is assigned but never read. Ruzu omits that dead cache-local
  field while retaining the virtual range and block-alignment state used by the algorithm.
- `get_entry_list` is retained as the nested core's parity API with `#[allow(dead_code)]`; Eden's
  outer `CompressedStorage` also does not forward this public method of its private nested class.

### Fixed parity debt

- `CacheManager::read` no longer reads the physical file at the virtual offset. It now finds the
  head and tail BKTR entries, expands alignment-required boundary blocks, and copies only the
  requested virtual range as Eden does.
- `CompressedStorageCore` now validates and traverses BKTR entries, batches at most 0x80 physical
  accesses, preserves aligned gaps, emits zero regions, resolves the configured decompressor, and
  enforces Eden's compressed-block size and offset checks.
- `Drop` now mirrors both upstream destructor/finalize layers, and the `VfsFile` adapter reports the
  virtual byte count actually available instead of the original request after an end-of-file clamp.

## 2026-08-22 — `src/core/src/hle/kernel/k_resource_limit.rs` vs `core/hle/kernel/k_resource_limit.h` and `.cpp`

### Intentional differences

- Rust stores the four mutable resource arrays and waiter count in `UnsafeCell` fields serialized
  by the owner-local `KLightLock`. This preserves Eden's mutation-through-shared-object model while
  allowing process and kernel owners to retain `Arc<KResourceLimit>` instead of an outer host mutex.
- Eden passes `KernelCore&` to reserve/release and constructs the object in its auto-object slab.
  Ruzu resolves the active hardware timer through the existing kernel owner and uses `Arc` lifetime
  management; isolated tests without an active kernel use a zero/current-expired tick fallback.
- `set_limit_value` returns `Result<(), ()>` rather than Eden's kernel `Result`; callers map the
  failure to `ResultInvalidState` at the SVC boundary.

### Fixed parity debt

- `reserve` now retains Eden's ten-second default timeout, wrapping-overflow rejection, hint-based
  wait decision, waiter-count ordering and retry loop. `release` broadcasts only when a waiter is
  present, after subtracting current and hint values.
- The prior `Arc<Mutex<KResourceLimit>>` ownership has been removed from kernel, process,
  page-table and service callers. The outer mutex would have remained locked while Eden's inner
  light lock was released for a wait, preventing another thread from releasing the resource.

## 2026-08-22 — `src/core/src/hle/kernel/k_scoped_resource_reservation.rs` vs `core/hle/kernel/k_scoped_resource_reservation.h`

### Intentional differences

- Rust retains an optional `Arc<KResourceLimit>` instead of Eden's raw pointer. `commit` consumes
  that optional owner, which has the same effect as assigning `nullptr`; the active kernel used by
  release remains owned by `KResourceLimit`'s runtime integration rather than a guard field.

### Fixed parity debt

- The explicit-timeout constructor now calls the timeout overload instead of silently using the
  default reservation path. Drop still releases only non-zero successful reservations, and commit
  still leaves the resource charged.

## 2026-08-22 — `src/core/src/hle/kernel/k_memory_manager.rs` vs `core/hle/kernel/k_memory_manager.h` and `.cpp`

### Intentional differences

- Rust represents Eden's `Impl*` pool chains with stable indices into the fixed manager array. The
  query methods traverse those same head/next links, and `get_pool_for_address` is named explicitly
  because Rust cannot overload it with the option-decoding `get_pool(u32)` function.

### Fixed parity debt

- Pool and total size queries now sum the owning `Impl` heaps instead of consulting Ruzu's duplicate
  `m_pool_sizes` cache. The cache, its setter, and the unused mutable-manager search were removed.
- Both free-size overloads now use Eden's lock scopes and manager traversal, and the address query
  returns the pool owned by the matching manager. This restores the memory data required by
  `GetSystemInfo` and removes the two dead private `Impl` query warnings.

## 2026-08-22 — `src/core/src/hle/kernel/svc/svc_info.rs` vs `core/hle/kernel/svc/svc_info.cpp`

### Fixed parity debt

- `get_system_info` now validates the zero handle and subtype ranges, reports total and used bytes
  for all four physical-memory pools, and returns Eden's privileged process-ID bounds 1 and 8.
  The former body returned `ResultNotImplemented` for every valid request.

## 2026-08-22 — `src/core/src/hle/kernel/svc/svc_types.rs` vs `core/hle/kernel/svc_types.h`

### Fixed parity debt

- Raw `SystemInfoType` conversion now rejects values outside Eden's three-value enum instead of
  silently turning an invalid value into `TotalPhysicalMemorySize`.

## 2026-08-22 — `src/core/src/hle/kernel/svc_dispatch.rs` vs `core/hle/kernel/svc.cpp`

### Fixed parity debt

- Both AArch32 and AArch64 dispatch paths now use Eden's generated-wrapper register layout for
  `GetSystemInfo`, return `ResultInvalidEnumValue` for an invalid raw type, and forward the system
  owner to the implementation. The AArch64 route was previously absent entirely.

## 2026-08-22 — `src/core/src/hle/kernel/k_memory_block_manager.rs` vs `core/hle/kernel/k_memory_block_manager.h` and `.cpp`

### Fixed parity debt

- Ruzu's unused allocator-free duplicate of `CoalesceForUpdate` was removed. All four update paths
  continue to call the single allocator-aware owner, which returns erased block nodes to the update
  allocator at the same point where Eden calls `allocator->Free(block)`.

## 2026-08-22 — `src/core/src/hle/kernel/k_page_table_base.rs` vs `core/hle/kernel/k_page_table_base.h` and `.cpp`

### Fixed parity debt

- The unused helper that cleared a guest virtual address through `zero_block` was removed. Eden's
  `ClearBackingRegion` accepts physical addresses, and every live Ruzu allocation path continues to
  call the physical-backing helper before mapping the new pages.

## 2026-08-22 — `src/core/src/hle/kernel/k_dynamic_resource_manager.rs` vs `core/hle/kernel/k_dynamic_resource_manager.h`

### Intentional differences

- Rust managers retain `Arc` owners for the page allocator and typed slab instead of Eden's raw
  non-owning pointers. The owner-provided constructor represents Eden's separate default
  construction and `Initialize` calls without changing which resource owns allocations.

### Fixed parity debt

- Managers can now attach to the dynamic page allocator and explicit slab heap owned by a
  `KSystemResource`. The memory-block and block-info slab aliases also live beside their manager
  aliases as they do upstream.

## 2026-08-22 — `src/core/src/hle/kernel/k_dynamic_slab_heap.rs` vs `core/hle/kernel/k_dynamic_slab_heap.h`

### Intentional differences

- Typed entries are host-owned `Box<T>` values guarded by mutexes rather than placement-constructed
  inside emulated kernel pages. Page consumption, capacity, used and peak accounting still follow
  the owner-provided `KDynamicPageManager`.

### Fixed parity debt

- Initialization now exposes the complete shared page-manager address range even when zero objects
  are pre-seeded, and lazy growth increases object count without changing that range, matching
  Eden's `Initialize` and `Allocate` state transitions.

## 2026-08-22 — `src/core/src/hle/kernel/k_page_group.rs` vs `core/hle/kernel/k_page_group.h` and `.cpp`

### Intentional differences

- Rust stores slab-owned `Box<KBlockInfo>` nodes in a vector instead of Eden's intrusive singly
  linked list. The unused link is retained as a zeroed pointer-sized field so `KBlockInfo` remains
  exactly 0x10 bytes and slab capacity matches Eden; iteration order and concatenation behavior are
  unchanged.

### Fixed parity debt

- New block nodes are now allocated from the selected `KBlockInfoManager`, allocation exhaustion is
  propagated, and finalize returns every node to that same manager. `close_and_reset` preserves
  Eden's per-node close-then-free ordering.

## 2026-08-22 — `src/core/src/hle/kernel/k_page_table_slab_heap.rs` vs `core/hle/kernel/k_page_table_slab_heap.h`

### Intentional differences

- Page-table contents and reference counts use indexed host allocations over the dynamic manager's
  full address range instead of pointers into kernel virtual memory. Unassigned shared-pool pages
  are represented by empty slots so interleaved slab allocations retain correct address indices.

### Fixed parity debt

- A zero-preseed heap now grows lazily from its owner page manager, tracks used entries, and sizes
  its reference-count table from the complete shared allocator range like Eden.

## 2026-08-22 — `src/core/src/hle/kernel/k_page_table_manager.rs` vs `core/hle/kernel/k_page_table_manager.h`

### Intentional differences

- Constructor injection of the shared slab replaces Eden's default construction followed by
  `Initialize`; all allocation, reference-count and range operations still belong to this manager.

### Fixed parity debt

- `get_used` now exposes the inherited dynamic-resource accounting required by
  `KSecureSystemResource::Finalize`.

## 2026-08-22 — `src/core/src/hle/kernel/k_system_resource.rs` vs `core/hle/kernel/k_system_resource.h` and `.cpp`

### Intentional differences

- `Arc` ownership replaces the base class's raw pointers to derived manager members, while the
  dynamic page allocator is mutex-protected so all three slabs can share it safely. Ruzu's secure
  address is the physical allocation directly because it has no separate kernel heap-VA mapping.
- Holding an `Arc<KResourceLimit>` supplies Eden's explicit `Open`/`Close` reference lifetime.

### Fixed parity debt

- Secure-resource initialization now creates the page-table, memory-block and block-info heaps over
  its dynamic page pool, initializes their managers, and publishes those exact managers through
  `KSystemResource::SetManagers`. Finalization verifies all three managers have no live objects.

## 2026-08-22 — `src/core/src/hle/kernel/board/k_system_control.rs` vs `core/hle/kernel/board/nintendo/nx/k_system_control.cpp`

### Fixed parity debt

- Secure-memory sizing and free validation now compare against the actual `Pool::Applet` value 1,
  and free uses page alignment only for `Pool::System` value 2. The former hard-coded values
  treated the System pool as Applet and selected the wrong alignment for Application memory.

## 2026-08-22 — `src/core/src/hle/kernel/k_page_table_base.rs` secure-manager ownership vs `core/hle/kernel/k_page_table_base.h` and `.cpp`

### Intentional differences

- Isolated Rust callers may omit a system resource and fall back to kernel-owned managers; runtime
  process initialization supplies the selected resource explicitly, as Eden does.

### Fixed parity debt

- Process page tables now retain the selected system resource's memory-block and block-info
  managers. Sentinel blocks, update allocators and every runtime `KPageGroup` therefore consume the
  process-owned secure pool when one exists instead of silently using global managers.
- `FinalizeUpdate` now only drains deferred page addresses. Its prior free calls executed code that
  is commented out in current Eden while guest-memory page-table allocation remains disabled.

## 2026-08-22 — `src/core/src/hle/kernel/k_process_page_table.rs` vs `core/hle/kernel/k_process_page_table.h`

### Fixed parity debt

- The thin wrapper now forwards the `KSystemResource` argument owned by Eden's
  `InitializeForProcess` interface instead of dropping it.

## 2026-08-22 — `src/core/src/hle/kernel/k_process.rs` secure-resource setup vs `core/hle/kernel/k_process.cpp`

### Fixed parity debt

- After creating a non-default secure system resource, process initialization now keeps its lock
  guard alive while forwarding the resource's base manager set into page-table initialization.

### Missing items

- Processes without a private secure resource still fall back to Ruzu's separate kernel manager
  fields rather than retaining Eden's application/system `KSystemResource` objects. The global
  resource-manager split remains a separate structural slice.

## 2026-08-22 — `src/core/src/hle/kernel/kernel.rs` vs `core/hle/kernel/kernel.cpp`

### Fixed parity debt

- `KernelCore` now owns and exposes the global `KBlockInfoManager`, using Eden's 4000-entry capacity,
  so default page groups no longer bypass block-info slab accounting.

### Missing items

- Ruzu still uses one global memory-block manager and independently backed manager heaps. Eden owns
  application/system manager pairs over one shared dynamic page pool, pre-reserves the page-table
  heap, and leaves exactly 64 dynamic pages; that broader resource-manager initialization remains
  to be ported.

## 2026-08-22 — `src/core/src/core.rs` kernel resource-manager boot wiring vs `core/hle/kernel/kernel.cpp`

### Fixed parity debt

- Kernel boot now initializes the global block-info manager beside the memory-block and page-table
  managers before any process page table is created.

## 2026-08-22 — `src/core/src/hle/kernel/k_dynamic_resource_manager.rs` allocator ownership vs `core/hle/kernel/k_dynamic_resource_manager.h`

### Intentional differences

- Rust managers and slabs share `Arc` owners instead of Eden's non-owning pointers. The nullable
  allocator is nevertheless retained on each manager, independently from the shared slab owner.

### Fixed parity debt

- `allocate` now passes the manager-selected nullable dynamic allocator into its slab. Application
  and system managers can therefore share pre-seeded entries while only the system manager grows
  the heap, matching Eden's resource-manager initialization.
- The unused synthetic `initialized` state was removed, and generic managers no longer select the
  page-table-only `ClearNode=true` policy.

## 2026-08-22 — `src/core/src/hle/kernel/k_dynamic_slab_heap.rs` allocator selection vs `core/hle/kernel/k_dynamic_slab_heap.h`

### Intentional differences

- Rust has no intrusive free-list pointer inside `Box<T>`, so Eden's `ClearNode` link clearing is
  unnecessary. Reused values are reset with `T::default()` when allocated to model Eden's
  destructor/`construct_at` object lifetime.

### Fixed parity debt

- Lazy growth now uses the nullable allocator supplied by the calling resource manager instead of
  an allocator implicitly selected by the shared heap.

## 2026-08-22 — `src/core/src/hle/kernel/k_memory_block.rs` slab geometry vs `core/hle/kernel/k_memory_block.h`

### Intentional differences

- Rust's ordered block container does not use Eden's intrusive red-black-tree links. Their packed
  0x1c-byte base storage is retained as zeroed reserved bytes solely to preserve slab geometry.

### Fixed parity debt

- `KMemoryBlock` now has Eden's 0x40-byte size and alignment, so the 20,000- and 10,000-object slab
  initializations consume the same number of dynamic pages as upstream.

## 2026-08-22 — `src/core/src/hle/kernel/k_page_table_slab_heap.rs` allocator selection vs `core/hle/kernel/k_page_table_slab_heap.h`

### Fixed parity debt

- Allocation accepts the nullable dynamic allocator selected by `KPageTableManager`; managers may
  still consume the shared pre-seeded page heap, but only the system manager can grow it.

## 2026-08-22 — `src/core/src/hle/kernel/k_page_table_manager.rs` vs `core/hle/kernel/k_page_table_manager.h`

### Intentional differences

- Rust constructor injection replaces Eden's default construction followed by `Initialize`, while
  retaining the same nullable allocator and shared page-table heap owners.

### Fixed parity debt

- Application and system page-table managers can now share one heap with Eden's distinct fixed and
  dynamic growth policies.

## 2026-08-22 — `src/core/src/hle/kernel/kernel.rs` resource-manager initialization vs `core/hle/kernel/kernel.cpp`

### Intentional differences

- Rust `Arc` ownership lets each `KSystemResource` retain its managers and their shared heaps, so
  `KernelCore` need not duplicate Eden's raw-pointer owner fields. The stateless host-emulated page
  buffer heap is initialized in Eden's order but need not remain stored.
- The manager region uses a non-guest synthetic kernel address because Ruzu does not map Eden's
  kernel virtual page-table region into host memory.
- Direct memory-block and block-info accessors remain as application-resource aliases for isolated
  legacy/test page-table construction; runtime processes use the two system-resource accessors.

### Fixed parity debt

- `initialize_resource_managers` now subtracts the reference-count area, initializes the shared
  dynamic page pool, pre-seeds the application/system memory-block and shared block-info heaps,
  assigns all remaining pages except 64 to the page-table heap, and asserts that exact reserve.
- Separate application and system manager sets now reproduce Eden's null versus shared dynamic
  allocator policy and are published through distinct `KSystemResource` owners.
- The obsolete combined memory-block capacity and standalone kernel page-table-manager path were
  removed; neither exists in Eden's ownership graph.

## 2026-08-22 — `src/core/src/core.rs` resource-manager boot wiring vs `core/hle/kernel/kernel.cpp`

### Intentional differences

- Ruzu supplies a stable synthetic kernel address with Eden's `KernelPageTableHeapSize`; the host
  model has no derived kernel virtual page-table mapping to query.

### Fixed parity debt

- Boot now invokes the single upstream-owned resource-manager initialization sequence instead of
  constructing three independent simplified managers.

## 2026-08-22 — `src/core/src/hle/kernel/k_process.rs` default system-resource ownership vs `core/hle/kernel/k_process.cpp` and `.h`

### Intentional differences

- Rust retains secure and default global resources in separate typed owners instead of one raw
  `KSystemResource*`; both branches still keep the selected resource alive for the process.

### Fixed parity debt

- A process without private secure memory now selects and retains Eden's application or system
  `KSystemResource` from `KernelCore`, then passes that exact manager set to page-table
  initialization. It no longer falls back to unrelated global manager fields.

## 2026-08-22 — `src/core/src/hle/kernel/k_process_page_table.rs` legacy resource wiring vs `core/hle/kernel/k_process_page_table.h`

### Intentional differences

- The compatibility `configure_address_space` path has no upstream counterpart and remains only
  for pre-`InitializeForProcess` callers.

### Fixed parity debt

- That compatibility path now retains the block-info manager beside the memory-block manager, so
  any page groups it creates return nodes to the same application resource owner.

## 2026-08-22 — `src/core/src/hle/kernel/k_system_resource.rs` manager attachment vs `core/hle/kernel/k_system_resource.cpp`

### Intentional differences

- Heap initialization remains explicit in the owning resource before Rust manager construction;
  Eden performs the equivalent heap and manager `Initialize` calls as two ordered phases.

### Fixed parity debt

- Secure-resource managers now attach to already initialized heaps and retain their own nullable
  allocator, matching the ownership boundary required by global application/system resources.

## 2026-08-22 — `src/core/src/hle/kernel/physical_core.rs` vs `core/hle/kernel/physical_core.{h,cpp}`

### Intentional differences

- The cooperative bootstrap loop reports Eden's `RunThread` return as
  `PhysicalCoreExecutionControl::Yield`; its caller then advances `CoreTiming` and preempts the
  emulated core. Eden obtains the same boundary by returning `void` to `CpuManager`.
- State published to `Interrupt` and the immutable single-core flag share one Rust mutex-backed
  state object. Eden stores the flag as a direct class member while guarding only accesses that
  race with the running ARM interface.

### Fixed parity debt

- After halt handling, single-core execution now returns after every JIT stop. Multi-core execution
  continues for an empty/unhandled halt and returns only for a non-step-completion `BreakLoop`,
  matching Eden's final `interrupt || m_is_single_core` condition.
- The unreachable fallback after the non-fallthrough execution loop and unused test imports were
  removed.

### Unintentional differences (to fix)

- The production fiber path still owns most `PhysicalCore::RunThread` halt analysis in
  `CpuManager::run_guest_thread_once`. Moving that behavior back into `physical_core.rs` requires a
  larger ownership refactor than this warning slice; the bootstrap path now has the corrected
  return predicate, but the duplicated production path remains structurally non-parity.

## 2026-08-22 — `src/core/src/arm/mod.rs` NCE build guard vs `src/core/CMakeLists.txt` and top-level `CMakeLists.txt`

### Intentional differences

- Rust expresses Eden's `HAS_NCE` source-list selection with a module-level `cfg` instead of a
  CMake conditional.

### Fixed parity debt

- The NCE backend is now compiled only for AArch64 Linux and Android, matching Eden's exact
  `HAS_NCE` predicate. Other architectures no longer compile an unusable native-AArch64 backend or
  report its relocation records as unread.

### Missing items

- The guarded AArch64 NCE implementation remains structurally incomplete: its patch code generation,
  assembly context switch, and signal-context handling still do not provide Eden behavior. This
  build-parity correction does not claim that backend as complete.

## 2026-08-22 — scheduler context lifecycle in `k_scheduler.rs`, `physical_core.rs`, and `core.rs` vs `k_scheduler.{h,cpp}`, `physical_core.{h,cpp}`, and `cpu_manager.cpp`

### Intentional differences

- The Rust-only synchronous `System::run_main_loop` bootstrap bypasses Eden's host-fiber scheduler,
  so it acquires and releases the running thread's context guard explicitly at the
  `PhysicalCore` boundary. The normal scheduler path continues to acquire in
  `ScheduleImplFiber` and release in `Unload`, as Eden does.

### Fixed parity debt

- Removed the unused cooperative `ScheduleImpl` duplicate. Eden has one `ScheduleImpl` path and
  always transfers a real switch to `ScheduleImplFiber`; the Rust scheduler now keeps only that
  fiber-owned implementation.
- The synchronous bootstrap now pairs its context-guard acquisition with an explicit release and
  returns the temporarily extracted ARM interface to its owning `KProcess` when execution ends.

## 2026-08-22 — synchronization-wait cancellation ownership in `k_thread.rs` and `k_synchronization_object.rs` vs `k_thread.{h,cpp}` and `k_synchronization_object.{h,cpp}`

### Fixed parity debt

- Removed the unused synchronization-node cleanup method from `KThread`. Eden owns that cleanup
  exclusively in `ThreadQueueImplForKSynchronizationObjectWait::CancelWait`; Ruzu's live queue
  callback remains in the matching synchronization-object module and is covered by a regression
  test that verifies every node is unlinked and cancellability is cleared.

## 2026-08-22 — `k_condition_variable.rs` timeout handling vs `k_condition_variable.{h,cpp}` and `k_scoped_scheduler_lock_and_sleep.h`

### Fixed parity debt

- Removed the unused host-`Instant` deadline conversion. Eden forwards positive timeout ticks
  unchanged to `KHardwareTimer::RegisterAbsoluteTask`, which Ruzu's active
  `KScopedSchedulerLockAndSleep` path already does.
- The timeout regression test now supplies a future absolute hardware tick instead of treating
  `1` as a relative duration, and verifies the guest thread's timer-completed wait state rather
  than the Rust bootstrap helper's pre-fiber return value.

## 2026-08-22 — `k_hardware_timer.rs` task delivery vs `k_hardware_timer.{h,cpp}`

### Intentional differences

- Rust resolves Eden's directly stored `KTimerTask*` through either the global scheduler's owned
  `Arc<KThreadLock>` or the raw pointer recorded for bootstrap waiters. This resolution remains
  inside `do_task` while the timer state and scheduler lock are held.

### Fixed parity debt

- Removed an unused target-resolution method that attempted to reacquire the timer-state mutex.
  Eden has no such helper, and using it from the live `DoTask` critical section would deadlock.
  The live GSC-backed task-delivery regression test remains the behavioral coverage.

## 2026-08-22 — `ipc_helpers.rs` `ResponseBuilder` state vs `ipc_helpers.h`

### Intentional differences

- Rust field-level `dead_code` annotations document the four constructor-state members that Eden
  stores but does not read after construction. Their names, values, placement, and ownership stay
  aligned with `IPC::ResponseBuilder`; only the Rust diagnostic is suppressed.

## 2026-08-22 — `server_manager.rs` host-thread naming vs `server_manager.{h,cpp}`

### Fixed parity debt

- Removed an unused generic thread-name formatter with no Eden counterpart. Additional service
  thread names remain constructed by `start_additional_host_threads`, matching Eden's method
  ownership and `name:index` format.

## 2026-08-22 — `applet_software_keyboard.rs` base-system ownership vs `applet_software_keyboard.{h,cpp}` and `applets.h`

### Intentional differences

- Rust flattens `FrontendApplet::system` into each applet implementation instead of using C++
  inheritance. `SoftwareKeyboard` retains that owner even though, like Eden's derived class, it
  never accesses the system directly; a field-level diagnostic annotation documents the retained
  base state.

## 2026-08-22 — `applet_profile_select.rs`, frontend `profile_select.rs`, and `applets.rs` vs `applet_profile_select.{h,cpp}`, frontend `profile_select.h`, and `applets.cpp`

### Intentional differences

- The Rust applet owns callback-visible completion, status, and final-data state through `Arc`
  containers because a frontend callback may outlive the mutable trait-object borrow. A separate
  executing flag defers locking the owning `Applet` when the default frontend invokes its callback
  synchronously; the enclosing accessor then observes `is_complete()` and signals it. Asynchronous
  callbacks still signal directly. Eden captures `this` and calls `Exit()` directly.
- Rust represents C++'s open-underlying-value `UiMode` and `UserSelectionPurpose` enums as
  transparent `u32` newtypes. This preserves all guest bit patterns without constructing an
  invalid Rust enum discriminant.
- `SelectionComplete` records an explicit Rust completion flag and pushes its output immediately;
  Eden also pushes and exits in the callback but leaves its otherwise-present `complete` field
  false. Rust needs the flag for the flattened `FrontendApplet::is_complete` interface and avoids
  pushing the already-moved output a second time if `Execute` is called again.

### Fixed parity debt

- Replaced the inert one-field stub with the Version1/2/3 initialization, exact 0x98/0xA0 input
  layouts, parameter conversion, profile-selection callback, cancellation status, exact 0x18
  output, request-exit handling, and applet completion signaling.
- Unknown library versions now follow Eden's non-fatal path: the applet logs the unsupported
  version, skips configuration decoding, and invokes the frontend with zeroed parameters.
- Restored `UiMode`, `UserSelectionPurpose`, `NintendoAccountStartupDialogType`,
  `UserSelectionSettingsForSystemService`, and `UiSettingsDisplayOptions` to their upstream-owned
  HLE module. The frontend now consumes those types instead of owning duplicates.
- `FrontendAppletHolder` now installs a default profile selector, preserves injected overrides,
  and constructs `ProfileSelect` for `AppletId::ProfileSelect`.
- The default frontend now always invokes the callback with a UUID value, using an invalid UUID
  when the configured profile is absent, matching Eden's `value_or(Common::UUID{})` call.

## 2026-08-22 — `applet_error.rs`, frontend `error.rs`, and `applets.rs` vs `applet_error.{h,cpp}`, frontend `error.{h,cpp}`, and `applets.cpp`

### Intentional differences

- C++ enum objects accept unknown underlying values; Rust represents `ErrorAppletMode` as a
  transparent `u8` newtype so the same input remains valid without creating an invalid enum.
- Guest `bool` fields are stored as `u8` in the raw argument layouts, avoiding invalid Rust bool
  representations while preserving their exact offsets and nonzero truth semantics.
- Rust callback-visible completion is stored in an `Arc` and defers locking the owning applet
  during a synchronous frontend call. The enclosing accessor observes `is_complete()` after that
  call and signals the applet; an asynchronous callback signals it directly. Eden captures `this`
  and calls `Exit()` directly.
- Frontend text is a Rust `String`, so fixed buffers are decoded lossily when they contain invalid
  UTF-8. Valid UTF-8 and zero termination match `StringFromFixedZeroTerminatedBuffer`.
- The unused upstream-local `ErrorCode::FromResult` helper is omitted; Eden has no call site for it.
- Rust's logging facade has no critical severity, so the default frontend uses `error` for Eden's
  `LOG_CRITICAL` messages.

### Fixed parity debt

- Replaced the inert `Error` stub with Eden's argument layouts and mode dispatch, including
  32/64-bit result decoding, system/application custom text, timestamped reports, reporter calls,
  request-exit handling, and completion output.
- Removed the frontend-only duplicate result type; `ErrorApplet` now receives the HLE-owned
  `ResultCode`, matching upstream ownership.
- `FrontendAppletHolder` now installs, preserves, and constructs the error applet backend for
  `AppletId::Error`.

## 2026-08-22 — `applet_general.rs`, frontend `general.rs`, and `applets.rs` vs `applet_general.{h,cpp}`, frontend `general.{h,cpp}`, and `applets.cpp`

### Intentional differences

- Open C++ mode enums are transparent integer newtypes so unknown guest values remain valid Rust
  values and reach Eden's unimplemented branches.
- Parental-control frontend methods take `&self` instead of C++'s mutable object reference so the
  frontend can be shared through `Arc`; implementations that need mutation use interior state.
- Callback state is shared through `Arc` atomics. Synchronous callbacks leave completion signaling
  to `ILibraryAppletAccessor`, while asynchronous callbacks signal the owner directly, avoiding a
  recursive owner-mutex lock.
- Rust sets explicit completion flags after Auth and PhotoViewer callbacks. Eden exits from those
  callbacks but never updates its otherwise-present `complete` fields; Rust needs the flags for
  the flattened `FrontendApplet::is_complete` contract.
- StubApplet also records completion for the flattened interface. Its interactive path still runs
  the following normal `Execute`, matching Eden's accessor ordering and duplicate fallback output.
- `StubApplet` retains Eden's unused applet ID and flattened base owner fields with field-level
  diagnostic annotations. They remain in their upstream owner rather than being deleted.
- Eden's fallback log reads the applet program ID while it owns a separate shared pointer. Rust's
  holder is called while the owner mutex is already locked, so it logs the applet ID only rather
  than recursively locking merely to format the program ID.

### Fixed parity debt

- Replaced the three inert structs with Auth PIN verification/registration/change dispatch,
  exact 0xC argument and 0x4 result payloads, PhotoViewer mode dispatch and empty output storage,
  and StubApplet channel draining plus normal/interactive 0x1000 fallback outputs.
- `FrontendAppletHolder` now installs and preserves parental-control/photo-viewer frontends,
  constructs Auth and PhotoViewer, and returns StubApplet for unsupported IDs instead of no
  frontend implementation.

## 2026-08-22 — `applet_web_browser_types.rs` and frontend `web_browser.rs` vs `applet_web_browser_types.h` and frontend `web_browser.{h,cpp}`

### Intentional differences

- C++'s little-endian and open enum wrappers are represented as transparent integer newtypes.
  This preserves exact wire values, supports unknown inputs safely, and is equivalent on every
  currently supported little-endian Ruzu target.
- Eden's dense hash map is a Rust `HashMap`; key equality and replacement semantics used by the
  TLV parser remain the same.

### Fixed parity debt

- Replaced the partial, mismatched input-TLV list with every Eden web version, shim, exit reason,
  input/output TLV type, document/display mode, and the exact 0x8/0x1010 wire structures.
- Removed the duplicate frontend `WebExitReason`. The frontend now consumes the HLE-owned type,
  including Eden's `WindowClosed = 4` value instead of the previous incorrect value 8, and uses
  reusable callback semantics matching `std::function`.
## 2026-08-22 — `src/common/src/settings.rs` vs Eden `src/common/settings.h` (`disable_web_applet`)

### Intentional differences

- Rust registers the setting through `for_each_setting_in_category_mut` instead of Eden's
  `SettingsRegistry` linkage; both expose the same label, Debugging category and persisted value.

### Unintentional differences (to fix)

- None in this setting slice.

### Missing items

- None in this setting slice.
## 2026-08-22 — Web frontend applet ownership vs Eden
`src/core/hle/service/am/frontend/applet_web_browser.{h,cpp}`,
`src/core/hle/service/am/frontend/applets.{h,cpp}` and
`src/core/frontend/applets/web_browser.{h,cpp}`

### Intentional differences

- Ruzu serializes the legacy and TLV return buffers field-by-field into zeroed byte vectors;
  Eden zero-initializes the corresponding C++ structs and copies them with `memcpy`. This keeps
  the same 0x1010/0x2000 wire bytes without reading Rust padding.
- Frontend callbacks retain the broker, weak applet owner and completion flags in `Arc`s so an
  asynchronous Rust frontend cannot borrow `WebBrowser`; a separate `frontend_executing` flag
  defers owner locking when Eden's default frontend invokes the callback synchronously.
- Cache paths use Ruzu's cache root name while preserving Eden's `fonts` and
  `offline_web_applet_<resource>/<title-id>` layout.
- Malformed shared-font files and unavailable optional Rust subsystem owners are logged and
  skipped instead of permitting C++ size underflow or dereferencing a null optional owner.
- `GetInputTLVData` mechanically consults `InputTLVExistsInMap` before indexing the map. Eden's two
  helpers are behaviorally equivalent but independent; the Rust call preserves both upstream
  helper owners without leaving one dead.

## 2026-08-22 — `nfp_types.rs` vs `nfp_types.h`

### Intentional differences

- Eden's `u16_be`/`u32_be`/`u64_be` wrappers are raw fixed-width integers in the packed Rust tag
  layouts. The date helpers perform the same endian conversion at their access boundary; the
  remaining packed consumers use unaligned reads and preserve the on-disk bit patterns.
- Eden's `Settings` union exposes named bit fields. Rust keeps the same one-byte owner and exposes
  explicit getters/setters for its three fields, avoiding references into a packed bit field.

### Fixed parity debt

- Restored Cabinet mode, write-date conversion, NFP size constants, exact public/private register,
  common, model, admin and aggregate payloads in their upstream owner.
- `TagInfo` is again an alias of the NFC wire type, and `AmiiboModelInfo` now stores its upstream
  enum/packed-tag types instead of untyped bytes.
- Zeroed defaults cover all reserved bytes; focused tests verify every frontend payload size and
  the upstream date/settings bit encoding.

## 2026-08-22 — Cabinet frontend ownership vs Eden
`core/hle/service/am/frontend/applet_cabinet.{h,cpp}`,
`core/frontend/applets/cabinet.{h,cpp}` and `core/hle/service/am/frontend/applets.cpp`

### Intentional differences

- The packed input stores mode/flag bytes as raw `u8` values and decodes the mode before use.
  Eden copies directly into C++ enums; the raw representation preserves the 0x1A8 layout without
  allowing malformed guest bytes to create an invalid Rust enum.
- Callback state, the broker and the NFC device are shared owners captured by the Rust callback.
  A synchronous callback defers locking the applet owner until the accessor observes
  `is_complete`; asynchronous callbacks signal it directly.
- Return data is assembled into a zero-filled 0x188-byte buffer at the exact upstream offsets,
  rather than copying a packed Rust object and risking padding or invalid-enum bytes.
- The Rust frontend callback is one-shot because every Eden Cabinet path invokes it once; this
  makes ownership explicit while preserving the observed call contract.

### Fixed parity debt

- Replaced the inert `is_complete`-only Cabinet with Eden's initialization, execution, four-mode
  dispatch, cancellation, result flags, request-exit and completion lifecycle.
- Removed the frontend's duplicate placeholder mode/tag/register types; it now consumes the
  NFP/NFC-owned types, and `FrontendAppletHolder` installs and routes the Cabinet backend.
- Focused tests verify the 0x1A8/0x188 binary contracts and the synchronous default-frontend
  cancellation path.

## 2026-08-22 — `nfc/common/device.rs` and `device_manager.rs` vs `nfc/common/device.{h,cpp}` and `device_manager.{h,cpp}`

### Intentional differences

- `NfcDevice` uses a stable `Arc<parking_lot::Mutex<_>>` inner allocation so the HID callback can
  hold a `Weak` owner safely. Eden's `shared_ptr<NfcDevice>` and raw `this` callback provide the
  same stable lifetime through C++ ownership.
- Kernel events are retained as shared Rust event owners rather than raw `KEvent*`; callback
  signaling and readable-event identity are preserved.
- Packed amiibo fields are accessed with unaligned reads/writes, and CRC payloads are assembled in
  explicit zero-free byte arrays instead of taking references to packed fields.

### Unintentional differences (to fix)

- `GetAmiiboDate` currently converts the host POSIX clock as UTC. Eden converts through the
  emulated `time:u` timezone rule, so a configured non-UTC guest timezone can differ around a
  calendar boundary.
- The legacy `DeviceManager::new()` path used by the standalone NFC interface has no `SystemRef`
  and therefore no HID controller callbacks. NFP and Cabinet use `new_with_system` or a direct
  controller owner and do have live NFC integration.

### Missing items

- The broader pre-existing partial NFC port still lacks Mifare/pass-through behavior,
  `GetRegisterInfoPrivate`, admin/all/debug/NTF operations, and full create/read/write application
  area behavior. Cabinet's required register mutation, delete, restore and format paths are ported.
- The no-key fallback reconstructs the encoded tag layout but does not yet populate Eden's
  generated fallback name, Mii and dates.

## 2026-08-22 — Upstream-unused constants in `amiibo_crypto.rs` and `caps_manager.rs`

### Intentional differences

- Rust marks `HMAC_DATA_START` and `NAND_ALBUM_FILE_LIMIT` with a targeted `dead_code` allowance.
  Eden declares the matching `HMAC_DATA_START` and `NandAlbumFileLimit` constants in their owning
  headers but does not reference either one in the corresponding implementations. Ruzu retains
  both constants in the matching Rust owners for structural and constant-placement parity.

## 2026-08-22 — `settings_server.rs` vs `settings_server.{h,cpp}` key-code maps

### Intentional differences

- Ruzu represents Eden's nullable `OutLargeData<KeyCodeMap>` as
  `Option<&mut [u8; 0x1000]>`; the IPC bridges derive that option from the HIPC output-buffer
  descriptor and only copy the map back after a successful result.
- Eden's `switch` has a defensive `default` label for invalid `KeyboardLayout` values. Rust's
  typed enum cannot contain an invalid discriminant, so the final arm names
  `EnglishUsInternational` explicitly.

### Fixed parity debt

- `GetKeyCodeMap`, `GetKeyCodeMap2`, and the newly ported `GetKeyCodeMapByPort` now perform Eden's
  `ResultNullPointer` validation before reading the current language.
- Command 12 is registered with its upstream name and consumes the same `u32 port`; as in Eden,
  the port is logged but does not affect layout selection.
- `GetKeyCodeMapImpl` again owns the output mutation and result return. Focused tests cover the
  three null-output paths, command registration, and the 0x1000-byte by-port result.

## 2026-08-22 — setting-format defaults vs Eden `setting_formats/*.cpp`

Compared `system_settings.rs`, `private_settings.rs`, `device_settings.rs`, and
`appln_settings.rs` with their matching Eden headers and implementations.

### Intentional differences

- Eden stores C++ enums and booleans directly in the binary payload. Ruzu stores their exact
  underlying `u64`/`u32`/`u8` representations in the format structs and converts at API
  boundaries. This keeps every on-disk bit pattern representable (including an all-zero or
  externally corrupted payload) without constructing invalid Rust enum or `bool` values; all
  offsets and sizes remain identical to Eden.

### Fixed parity debt

- Ported the four upstream-owned default constructors. System defaults now include Eden's exact
  version, flags, UUID, notification/TV/sleep/initial-launch settings, UTC timezone, feature flags,
  configured language, and derived keyboard layout; application defaults set only the default Mii
  author UUID, while private and device payloads remain fully zeroed.
- Focused tests verify the four payload sizes, deterministic zero regions, and every non-zero
  system default assigned by Eden.
- Replaced enum and `bool` fields in the persisted system/application payloads with raw wire
  integers, making future byte-for-byte load/store safe while preserving every compile-time
  offset assertion.
- `AccountNotificationSettings` now stores both `FriendPresenceOverlayPermission` members as raw
  `u8` values, closing the final invalid-enum hole nested inside `SystemSettings`. The implicit
  C++ alignment gap after `quest_flag` is represented by an explicit three-byte Rust field so the
  full payload has no unread implicit padding at that boundary.

## 2026-08-22 — `system_settings_server.rs` state ownership vs Eden
`system_settings_server.{h,cpp}`

### Intentional differences

- Rust keeps enum-valued persisted fields and their IPC boundary values as raw integers. Eden's
  C++ enums preserve out-of-range underlying values; raw Rust integers provide the same behavior
  without constructing invalid enum discriminants.
- The fixed-size EULA and account-notification arrays clamp a corrupted stored count before
  creating a Rust slice. Valid counts preserve Eden's fixed-array/count behavior, while malformed
  counts cannot create an out-of-bounds Rust slice.

### Fixed parity debt

- `ISystemSettingsServer` now owns `SystemSettings`, `PrivateSettings`, `DeviceSettings`, and
  `ApplnSettings` directly instead of duplicating selected values as unrelated loose fields.
- Every implemented getter/setter now reads or mutates its Eden-owned payload. This includes the
  private external-clock values, system clock contexts, all five audio-output modes, fixed-count
  EULA/account-notification arrays, and the system-owned packed initial-launch settings.
- Constructor initialization now follows Eden's order: default payloads first, configured region
  override second, then the temporary EULA entry derived from the user clock context.
- Removed unsafe enum `transmute` calls from the affected IPC handlers and added regressions for
  unknown raw enum values and four-payload ownership.

### Unintentional differences (to fix)

- Battery-lot and console-serial responses remain zeroed. Eden derives them from the common
  `serial_battery`, `serial_unit`, and region settings, which Ruzu does not yet own.

### Missing items

- The pre-existing partial service still omits Eden's implemented console-information-upload,
  automatic-application-download, USB 3.0, HTTP-auth-config, and account-user-settings commands.

## 2026-08-22 — `system_settings_server.rs` persistence vs Eden
`system_settings_server.{h,cpp}`

### Intentional differences

- The four verbatim payload types implement a private `SettingsPayload` marker. This is a Rust
  validity boundary for Eden's templated raw-memory I/O: only the four audited all-bit-pattern
  settings structs can be loaded into initialized Rust values.
- Rust propagates file create/read/flush failures as `false` instead of relying on iostream state,
  and an isolated test constructor skips host NAND I/O. Production construction, mutation and
  destruction retain Eden's persistence behavior.
- The service's existing outer `Mutex<ISystemSettingsServer>` serializes mutation and storage;
  Eden uses a second member mutex inside `SetSaveNeeded`. Both protect the same per-instance store
  operation, while Rust avoids locking the already exclusively borrowed object twice.

### Fixed parity debt

- Ported `LoadSettingsFile`, including directory creation, exact file-size validation, native-wire
  header parsing, `version >= 4`, default regeneration, second header validation and full payload
  loading.
- Ported `StoreSettingsFile` with Eden's `settings.tmp` then `settings.dat` rename ordering, and
  ported all four NAND save paths in `SetupSettings`/`StoreSettings`.
- Construction now loads all four files before applying the configured region and temporary EULA
  override. `SetSaveNeeded` stores immediately, and `Drop` mirrors the destructor's final store.
- `SETTINGS_MAGIC` and `SETTINGS_VERSION` are now consumed; the two corresponding `core` warnings
  are gone. Focused tests cover default creation, corrupt-header reset, forward-version acceptance,
  replacement ordering and payload round-trips.

## 2026-08-22 — upstream-dead title-version format in `patch_manager.rs`

### Intentional differences

- Eden declares `TitleVersionFormat::FourElements` and a corresponding `vX.Y.Z.W` branch, but all
  seven production calls use the default `ThreeElements` format. Ruzu removes the unused enum and
  parameter and retains the exact production `vX.Y.Z` calculation; the test-only four-element call
  that kept dead code alive was removed.

## 2026-08-22 — VI free-slot ownership in `display_list.rs` and `layer_list.rs`
vs Eden `display_list.h` and `layer_list.h`

### Intentional differences

- The private Rust helpers receive only their fixed array instead of borrowing the complete list.
  This permits `CreateDisplay`/`CreateLayer` to hold the returned mutable slot while independently
  advancing `next_id`; Eden obtains the equivalent disjoint members through a raw object pointer.

### Fixed parity debt

- `create_display` and `create_layer` now call their upstream-owned `get_free_*` helpers rather
  than duplicating the array search inline. Full-pool failure still leaves the ID unchanged;
  successful reuse selects the first free slot and advances the monotonic ID exactly once.
- Focused eight-slot exhaustion/reuse tests cover Eden's display IDs starting at zero and layer
  IDs starting at one. Both previously unused-helper warnings are gone.

## 2026-08-22 — `sfdnsres.rs` NetDB mapping and blocked hosts vs Eden
`sfdnsres.{h,cpp}`

### Intentional differences

- Eden retains `NetDbError::{Internal,NoRecovery,NoData}`, but its console-verified mapping and
  every response path can only construct `Success`, `HostNotFound`, or `TryAgain`. Ruzu omits the
  three unreachable discriminants while preserving their numeric values for all emitted results.

### Fixed parity debt

- Restored Eden's complete blocked-domain table and shared substring helper in both hostname and
  addrinfo resolution paths. Ruzu previously checked only `srv.nintendo.net` inline.
- Focused tests cover all distinct upstream NetDB mapping outcomes and the exact substring policy;
  the unused-variant warning is gone.

## 2026-08-22 — RO random mapping state and capacities in `ro.rs` vs Eden `ro.cpp`

### Intentional differences

- Rust implements the standard-library `std::mt19937_64` engine locally because Rust's standard
  library has no equivalent engine. Its default seed, state transition, tempering and output
  sequence match the C++ standard engine used by Eden.
- `process_contexts`, NRO records and NRR records remain heap-backed Rust vectors rather than C++
  inline `std::array` members. Their fixed logical capacities and first-free traversal are the
  same, while avoiding a very large stack move when constructing the shared context.

### Fixed parity debt

- Replaced the duplicated xorshift closure and unused method with one persistent MT19937-64 engine
  owned by `RoContext` and passed to `map_nro`, matching Eden's ownership and consumption order.
  Ruzu no longer consumes an extra random value after each map attempt.
- Raised `MAX_NRO_INFOS` and `MAX_NRR_INFOS` from 64 to Eden's 256-entry limits. Focused tests lock
  both capacities and the standard engine's default output sequence; the unused-method warning is
  gone.

## 2026-08-22 — upstream-absent Mii source helper in `mii.rs`

### Intentional differences

- Removed the unused `IDatabaseService::has_database_source` helper. Eden has no corresponding
  method; source-flag decisions remain owned by `mii_manager.rs`, matching `MiiManager` upstream.
  No service behavior or command payload changed.

## 2026-08-22 — upstream-absent empty filesystem helper in `fsp_srv.rs`

### Intentional differences

- Removed the unused `FspSrv::make_empty_filesystem` helper and its private VFS imports. Eden's
  `FSP_SRV` never substitutes an empty filesystem for an open failure, and no Ruzu command called
  this helper. Existing error responses and real filesystem construction paths are unchanged.

## 2026-08-22 — `network.rs` global room ownership vs Eden `network.{h,cpp}`

### Intentional differences

- The existing `RoomNetwork` compatibility owner remains for Rust frontends that already retain
  an explicit network lifetime. It installs its exact `Arc<Room>` and `Arc<RoomMember>` into the
  process-global registry, so the compatibility API and Eden's global accessors share objects
  rather than creating parallel transports.
- The registry uses a `LazyLock<Mutex<_>>` in place of C++ namespace-static `shared_ptr` objects.
  Teardown removes both strong global references before invoking potentially blocking network
  cleanup, so callbacks cannot deadlock the registry mutex.

### Fixed parity debt

- Ported `Network::Init`, `GetRoom`, `GetRoomMember`, and `Shutdown` in their matching network
  owner. The GTK frontend now obtains its `RoomMember` from that registry, making the same packet
  transport visible to core LDN services.
- A focused ownership test verifies that `RoomNetwork` and the global accessors return pointer-
  identical objects and that `Shutdown` clears both global handles.

## 2026-08-22 — `lan_discovery.rs` vs Eden `lan_discovery.{h,cpp}`

### Intentional differences

- `LanStation` stores its node ID and status while `LANDiscovery` indexes the corresponding
  `NetworkInfo::nodes` entry. Eden stores raw back-pointers to both the discovery owner and node;
  indexed ownership preserves the same 1–7 station mapping without self-referential Rust objects.
- The packet mutex is held through an `Arc<Mutex<()>>`, allowing Rust to retain the guard while
  mutating disjoint discovery state. `destroy_network_impl` and `disconnect_impl` mechanically
  preserve Eden's calls from already-locked close/finalize paths without recursively locking.
- Received `NetworkInfo` bytes are checked for valid Rust enum discriminants before the same
  native-layout copy Eden performs. Short or malformed room packets are ignored rather than
  creating an invalid Rust value; valid packets retain Eden's exact raw layout.
- Upstream's uncalled data-bearing `SendBroadcast` template and `GetStationCount` helper are not
  reproduced. Eden has no call to either; retaining them would introduce new dead-code warnings.

### Fixed parity debt

- Ported network-info initialization, both `GetNetworkInfo` forms, scan filtering, advertise data,
  AP/station state validation, create/destroy/connect/disconnect, initialization/finalization,
  node updates, packet send/receive, host disconnect handling, node-change accumulation, fake MAC
  construction and node-info construction in their matching owner.
- Restored the process-global `RoomMember` packet transport and callback event points. Session IDs
  use Eden's default `independent_bits_engine<mt19937,64>` sequence, and station IDs now span 1–7
  instead of Ruzu's previous 0–6.
- Focused tests cover station numbering, accumulated connect/disconnect changes and malformed enum
  payload rejection. The unread LAN state fields and unused `init_node_state_change` warning are
  gone.

### Binary layout verification

- PASS: `NodeInfo` and `NetworkInfo` are still sent as their existing 0x40-byte and 0x480-byte
  native payloads. The receive path validates every enum-bearing field before constructing the
  raw-copied `NetworkInfo`; no field order or padding changed.

## 2026-08-22 — upstream-dead monitor state in `ldn/monitor_service.rs`

### Intentional differences

- Eden declares `IMonitorService::state`, but `GetStateForMonitor` unconditionally returns
  `State::None` and neither monitor lifecycle method reads or changes the member. Ruzu removes the
  unread field while retaining that exact stub response, now covered by a focused regression.

## 2026-08-22 — upstream-dead parental-control title ID

### Intentional differences

- Removed `States::current_tid` from `parental_control_service.rs`. Eden declares the zero-valued
  member but never reads or writes it; active application identity remains owned by
  `states.application_info.application_id` on both sides. No command payload or state transition
  changed.

## 2026-08-22 — PSM session event ownership in `ptm/psm.rs` vs Eden `ptm/psm.{h,cpp}`

### Intentional differences

- `PSM` does not retain its constructor `SystemRef`: Eden's base `ServiceFramework` owns the
  system reference used to construct `IPsmSession`, whereas Rust's `ServiceContext` resolves the
  active kernel process itself. Retaining a second unread reference on `PSM` did not participate
  in either session construction or event signaling.
- Session enable flags use atomics because Rust service handlers receive shared references. Their
  initial values, command mutations and signal predicates match Eden's four booleans.

### Fixed parity debt

- `BindStateChangeEvent` now returns the readable end of the persistent event created by the
  session. It previously allocated and returned an unrelated event, so all three `Signal*`
  methods signaled an object the guest could never observe.
- A focused regression covers Eden's bind, enable, signal, unbind and suppressed-signal ordering;
  the unread `PSM::system` warning is gone.

### Missing items

- `GetBatteryVoltageState`, `GetBatteryAgePercentage`, and `GetBatteryChargeInfoFields` remain
  unimplemented. Porting the latter also requires Eden's `Common::GetPowerStatus` counterpart and
  its exact 0x54-byte `BatteryChargeInfoFields` payload.
- Battery percentage and charger type still use fixed stored values rather than Eden's host power
  status query.

## 2026-08-22 — application foreground request in `application_accessor.rs` vs Eden
`application_accessor.{h,cpp}`

### Intentional differences

- Rust retains a `Weak<Mutex<WindowSystem>>` in place of Eden's non-owning C++ reference. Active
  accessors require the owner to remain alive, while the weak reference prevents an ownership
  cycle with the AM service graph.
- Access to the shared applet and window system is serialized through their Rust mutexes. The
  command still delegates the transition to `WindowSystem`, matching Eden's method ownership.

### Fixed parity debt

- Ported command 101 `RequestForApplicationToGetForeground` and connected the previously unread
  `window_system` member to `WindowSystem::request_application_to_get_foreground`.
- A focused regression moves a tracked application out of the foreground, invokes the accessor,
  runs the window-system update and verifies that application interaction is restored. The unread
  field warning is gone.

### Missing items

- Eden's implemented commands `GetAppletStateChangedEvent`, `GetResult`,
  `GetCurrentLibraryApplet`, `PushLaunchParameter`, `GetApplicationControlProperty`, and
  `SetUsers` remain absent from this partial service port.

## 2026-08-22 — `am/process_creation.rs` vs Eden `am/process_creation.{h,cpp}`

### Intentional differences

- Rust passes loader, result and control outputs through mutable references and returns
  `Option<Process>` in place of C++ `unique_ptr` nullability. The loader remains assigned when
  process initialization fails, matching Eden's output ordering.
- `LoaderSystem` carries the content-provider and filesystem references required by Rust loader
  traits. `Process::initialize` transfers loader-produced build-ID and cheat registrations back
  to the owning `System`; this is the Rust transport for state Eden writes directly through its
  monolithic `Core::System`.
- The existing `build_application_launch_property` helper is shared with the transitional
  top-level load path. It remains in the matching `process_creation.rs` owner and performs the
  same `PatchManager` and content-slot queries as Eden.

### Fixed parity debt

- Ported the anonymous `CreateProcessImpl` and rewired `CreateProcess` through it, preserving
  loader construction, initialization and failure order.
- Replaced the `CreateApplicationProcess` stub with process initialization, NACP extraction or
  exact zeroed fallback, launch-property construction and ARP registration.
- Ported `ReinitializeProcess`, including program-NCA and loader failure handling. Focused tests
  cover loader retention on initialization failure, absent-content failure and every frontend
  storage-slot mapping.

### Missing items

- The top-level `System::load` path still performs its initial application loading directly and
  calls only `build_application_launch_property`; migrating that lifecycle into this helper is a
  separate ownership change because it currently retains the frontend's loader and process.

### Binary layout verification

- PASS: failed control reads produce exactly `size_of::<RawNACP>() == 0x4000` zero bytes, and the
  registered `ApplicationLaunchProperty` retains its existing verified 0x10-byte C layout.

## 2026-08-22 — `launch_timestamp_cache.rs` vs Eden `launch_timestamp_cache.{h,cpp}`

### Intentional differences

- Rust uses a `LazyLock<Mutex<CacheState>>` for Eden's namespace-static mutex, maps and loaded flag.
  Ordered `BTreeMap` storage makes serialized key order deterministic without changing lookup,
  update or persistence semantics.
- Filesystem and JSON failures are represented by `Result`/`Option` instead of streams and C++
  exceptions. Production behavior remains warning-and-return, including keeping the one-shot
  `loaded` flag set after a failed read or parse.
- JSON pretty-print whitespace is produced by `serde_json`; the persisted object shape, uppercase
  16-digit keys and values are compatible with Eden's parser.

### Fixed parity debt

- Added the missing core-owned launch timestamp cache with the exact `launched.json` cache path,
  lazy load, legacy raw-timestamp support, current `{timestamp, launch_count}` format, synchronous
  save, count increment and 2026-01-01 default timestamp.
- Hex key parsing preserves the prefix behavior of Eden's `std::stoull`, including whitespace,
  signs, `0x` and trailing non-hex text. Focused tests cover both JSON formats, malformed keys,
  serialization and the fixed fallback value.

## 2026-08-22 — `am/service/application_creator.rs` vs Eden
`am/service/application_creator.{h,cpp}`

### Intentional differences

- Rust stores the `SystemRef` explicitly because its `ServiceFramework` trait does not own the
  system as Eden's C++ base class does. `WindowSystem` is retained through a weak mutex-protected
  reference to preserve the existing Rust service graph without introducing an ownership cycle.
- Fallible process, content and window-system lookups use `Option`; the IPC adapters translate
  every failed lookup to Eden's `ResultUnknown` and successful calls return the same moved
  `IApplicationAccessor` interface.

### Fixed parity debt

- Ported the anonymous `CreateGuestApplication`, command 0 `CreateApplication`, and command 10
  `CreateSystemApplication` in their matching owner. Both paths validate the program NCA, create
  and configure the same applet type, track it as foreground, and return an accessor.
- Launch timestamps retain Eden's asymmetric ordering: normal applications save before process
  creation, while system applications save only after successful creation and tracking. A focused
  test verifies that both implemented upstream commands are wired to IPC handlers; the unread
  `window_system` warning is gone.

## 2026-08-22 — general-channel ownership in `core.rs` vs Eden `core.{h,cpp}`

### Intentional differences

- Rust keeps the channel data and optional `Arc<Event>` in one `parking_lot::Mutex` state object,
  while Eden keeps separate members behind one `std::mutex`. `GetGeneralChannel` returns a mapped
  guard instead of an unrestricted reference, preserving the same mutable stack access without
  allowing unsynchronized Rust aliases.
- Eden's lazy `ServiceContext` constructs the kernel event immediately. Rust's shared `Event`
  constructs its kernel bridge when IPC first requests a handle; host-visible signal state is
  retained before that point and copied into the bridge, so the guest observes the same state.

### Fixed parity debt

- Ported `GetGeneralChannel`, `PushGeneralChannelData`, `TryPopGeneralChannel`, and
  `GetGeneralChannelEvent` in their `System` owner. The channel remains LIFO, the event is created
  lazily, only the first push signals it, and removing the final item clears it.
- A focused regression verifies LIFO ordering, persistent event identity and both event-state
  transitions required by Eden's AM producer and consumer services.

## 2026-08-22 — `am/service/home_menu_functions.rs` vs Eden
`am/service/home_menu_functions.{h,cpp}`

### Intentional differences

- Rust retains the applet through `Arc<Mutex<Applet>>` rather than Eden's `shared_ptr`. Although
  no command dereferences it, both implementations deliberately keep the owning applet alive for
  the interface lifetime; the field is annotated accordingly instead of being removed as dead.
- Eden's unused `ServiceContext m_context` remains constructed even though the general-channel
  event moved to `System`. Rust removes the per-interface context and the previously invented
  private event; all event ownership now comes from the matching `System` owner.

### Fixed parity debt

- Ported command 20 `PopFromGeneralChannel` and command 40 `IsSleepEnabled`, and corrected command
  21 to return `System::GetGeneralChannelEvent` instead of an unrelated per-service event.
- Both applet-proxy owners now pass the same `SystemRef` into this interface. A focused regression
  verifies retained applet ownership, command wiring, empty-channel error and successful pop; the
  misleading unread-field warning is gone and `core` decreases from 69 to 68 warnings.

## 2026-08-22 — general-channel producer in `am/service/common_state_getter.rs` vs Eden
`am/service/common_state_getter.{h,cpp}`

### Intentional differences

- Eden's CMIF serializer presents `SharedPointer<IStorage>` directly to the method. Ruzu's handler
  resolves the domain object ID in the request manager, downcasts the same `IStorage` interface,
  then passes its copied byte vector to the behavior method. This follows the existing Rust CMIF
  ownership boundary while preserving Eden's storage-copy semantics.

### Fixed parity debt

- Ported and registered command 20 `PushToGeneralChannel`. It now copies `IStorage::GetData()` into
  the `System`-owned channel used by `IHomeMenuFunctions`, rather than leaving the only producer
  unimplemented.
- A focused regression verifies command wiring and the complete producer-to-system pop path.

## 2026-08-22 — event ownership in `am/service/lock_accessor.rs` vs Eden
`am/service/lock_accessor.{h,cpp}`

### Intentional differences

- Eden owns one `Event` through a `ServiceContext`. Ruzu's kernel bridge registers the matching
  `KEvent` and `KReadableEvent` in the owner process, retains only the signaling owner and readable
  object ID on the interface, and lets the process registry own the readable endpoint returned to
  guest handles.
- Unit tests without an installed `KernelCore` retain the isolated high-range object-ID fallback;
  production now allocates both IDs through `KernelCore::create_new_object_id`, matching Eden's
  central kernel allocation rather than using the fallback counter.

### Fixed parity debt

- Removed the stored event-owner ID and duplicate `Arc<KReadableEvent>` that were never consulted
  after registration. `TryLock`, `Unlock`, `GetEvent`, initial signaling and persistent readable
  endpoint identity are unchanged.
- The focused event regression now resolves the readable endpoint through its actual process
  owner. Both unread-field warnings are gone and `core` decreases from 68 to 67 warnings.

## 2026-08-22 — process ownership in `am/service/self_controller.rs` vs Eden
`am/service/self_controller.{h,cpp}`

### Intentional differences

- Eden retains `m_process` on both `ISelfController` and its `DisplayLayerManager`, even though the
  controller reads it only while initializing the manager. Rust transfers the `Arc<ProcessLock>`
  into `DisplayLayerManager`, the sole post-construction user, instead of holding a second strong
  reference for the interface lifetime.

### Fixed parity debt

- Removed the duplicate controller-owned process reference while preserving constructor
  initialization and destructor-time `DisplayLayerManager::Finalize` ordering.
- Ported the two small implemented commands found during the line-by-line audit: command 67
  `IsIlluminanceAvailable` returns false, and command 230 `Unknown230` consumes its `u32` input and
  returns a zeroed `u16` output, exactly like Eden.
- A focused regression verifies single process ownership after construction, release on controller
  destruction and both command registrations. The unread field warning is gone and `core`
  decreases from 67 to 66 warnings.

## 2026-08-22 — `am/service/storage.rs` and `storage_accessor.rs` vs Eden
`am/service/storage.{h,cpp}` and `storage_accessor.{h,cpp}`

### Intentional differences

- Eden's `ServiceFramework` base stores the `System` reference on every storage accessor. Rust's
  trait has no corresponding base state, so accessor constructors accept the forwarded
  `SystemRef` to preserve construction ownership but do not retain another unused copy.
- The systemless `IStorage::new` convenience delegates to a null `SystemRef` for isolated callers;
  every AM-owned construction path uses `new_with_system` or `new_with_backing` and forwards the
  active system.

### Fixed parity debt

- Split `Open` and `OpenTransferStorage` behavior from their IPC adapters and restored Eden's
  `IStorage` → accessor system flow. Regular storage still rejects transfer access and handled
  storage still rejects normal access with `ResultInvalidStorageType`.
- A focused regression covers the buffer-storage branch and its exact invalid-type result. The
  unread `IStorage::system` warning is gone and `core` decreases from 66 to 65 warnings.

## 2026-08-22 — `aoc/purchase_event_manager.rs` vs Eden
`aoc/purchase_event_manager.{h,cpp}`

### Intentional differences

- Rust stores the persistent event's `ServiceContext` handle and resolves its `Arc<Event>` when
  needed, instead of retaining Eden's raw `KEvent*`. `ServiceContext::Drop` closes its remaining
  events, providing the same interface-destruction ownership as Eden's explicit destructor.

### Fixed parity debt

- `GetPurchasedEvent` now returns the readable end of the constructor-owned persistent event. It
  previously created and returned an unrelated event on every call, leaving both actual owner
  fields unread and making future signaling invisible to the guest.
- Restored commands 0 `SetDefaultDeliveryTarget` and 1 `SetDeliveryTarget`, including client PID,
  `u64` and mapped input-buffer decoding; like Eden, both remain logged successful stubs.
- The event name now exactly matches Eden, and a focused regression verifies persistent identity,
  all five implemented command registrations and the exact no-product result. `core` decreases
  from 65 to 64 warnings.

## 2026-08-22 — `apm/apm.rs` and `apm_interface.rs` vs Eden APM
`apm/apm.{h,cpp}` and `apm/apm_interface.{h,cpp}`

### Intentional differences

- Rust shares the controller through `Arc<Mutex<Controller>>` rather than Eden's long-lived
  `Controller&`; the APM module uses `Arc<Module>` in place of `shared_ptr<Module>`. Both preserve
  the same service-wide controller and module lifetimes.
- Ruzu registers factories which create an interface per incoming session, while Eden registers
  shared interface instances in `ServerManager`. This follows Ruzu's existing server-manager
  connection boundary; the registered names, handlers and shared APM state now match Eden.

### Fixed parity debt

- Restored the compatibility-only `apm:p` registration and removed the extraneous
  `ServiceManager` parameter from `APM::LoopProcess`; the owning function and its launch site now
  have Eden's system-only flow.
- Removed the stale Rust-only `Mutex` import left behind by that signature correction. Eden's
  `apm.cpp` owns no lock in `LoopProcess`, and the import had no runtime purpose.
- Kept the otherwise unread `Module` owner on every APM interface instead of deleting it as dead
  code, with a focused lifetime regression proving that it is released with the interface.
- `GetPerformanceMode` now preserves Eden's unusual resultless two-word response rather than
  adding `ResultSuccess` and a third word. A focused regression verifies the raw IPC response
  size and payload placement.

### Unintentional differences (to fix)

- None in the reviewed APM registration and ownership slice.

### Missing items

- None for the reviewed APM registration and ownership slice.

### Binary layout verification

- PASS: this cleanup changes only an unused Rust import and no IPC payload or serialized type.

## 2026-08-22 — audio service event ownership vs Eden audio interfaces
`src/core/src/hle/service/audio/{audio_in,audio_out,audio_renderer}.rs` vs
`src/core/hle/service/audio/{audio_in,audio_out,audio_renderer}.{h,cpp}` and their matching
`src/audio_core/{in/audio_in,out/audio_out,renderer/audio_renderer}.{h,cpp}` owners

### Intentional differences

- The crate boundary prevents `core` from naming concrete `audio_core` session types. Ruzu keeps
  the existing owner-preserving callback wrappers in `core.rs`; the newly exposed `free` and
  `finalize` callbacks forward directly to the same concrete methods owned by Eden's
  `AudioCore::AudioIn::In`, `AudioCore::AudioOut::Out`, and `AudioCore::Renderer::Renderer`.
- Eden's `ServiceContext` owns a `KEvent` whose readable endpoint is returned by reference. Ruzu
  registers both endpoint objects in the requesting process, keeps the writable `Arc<KEvent>` on
  the service, and lets the concrete audio system retain the readable `Arc<KReadableEvent>` it
  signals. IPC returns the process-registered readable object ID, preserving stable endpoint
  identity without an extra service-owned readable reference.
- Eden balances `KProcess::Open/Close`; Ruzu's corresponding strong `Arc<ProcessLock>` is installed
  in the concrete audio system. Calling `free`/`finalize`, unregistering the event pair, and then
  dropping the concrete session releases that process owner in the same lifecycle order.

### Unintentional differences (to fix)

- None in the reviewed event ownership and service-destruction slice.

### Missing items

- None for `IAudioIn`, `IAudioOut`, and `IAudioRenderer` event cleanup ordering.

### Binary layout verification

- PASS: no raw IPC payload was changed. Existing focused tests continue to verify the 0x28-byte
  `AudioInBuffer` and `AudioOutBuffer` wire layouts.

### Fixed parity debt

- `IAudioIn` and `IAudioOut` now call their concrete `Free` exactly once before closing the event;
  previously their session IDs were never explicitly returned to the manager.
- `IAudioRenderer` now calls `Finalize` before closing the event instead of relying on the later
  concrete `Renderer::Drop`. This restores Eden's finalize-before-event-before-process ordering.
- Removed three duplicate readable-event fields and the artificial `is_initialized` mutex reads
  that existed only to silence dead-code warnings. Focused destructor regressions verify that
  cleanup runs while the readable endpoint is still registered and that both endpoints are
  released afterward; `core` decreases from 63 to 60 warnings.

## 2026-08-22 — `btm/btm_user_core.rs` vs Eden `btm_user_core.{h,cpp}`

### Intentional differences

- Eden's `ServiceContext::CloseEvent` accepts a raw `KEvent*`; Ruzu's existing context API closes
  its owned event by numeric context handle. `IBtmUserCore` therefore retains four private handles
  alongside the four service event owners so its `Drop` can preserve the same explicit close
  sequence.
- Ruzu's `Arc<Event>` fields own the event wrappers until Rust field destruction immediately after
  `Drop`; Eden retains non-owning `KEvent*` pointers whose storage is released by `CloseEvent`.
  Removing the context owner first and the service field owner second preserves the same externally
  observable endpoint lifetime.

### Unintentional differences (to fix)

- None in the reviewed command, event-identity, and destruction slice.

### Missing items

- None from Eden's `IBtmUserCore`; the remaining null command-table entries are also null upstream.

### Binary layout verification

- PASS: the implementation changes only host-side event ownership and does not alter an IPC
  payload. Commands 0, 17, 26, and 33 retain their success result, valid flag, and copy handle.

### Fixed parity debt

- Added explicit `scan`, `connection`, `service_discovery`, and `config` event closure in Eden's
  destructor order instead of relying on the later generic `ServiceContext::Drop` sweep. A focused
  regression verifies that both context and service owners release every event when the interface
  is destroyed.

## 2026-08-22 — `set/settings.rs` vs Eden `set/settings.{h,cpp}`

### Intentional differences

- Ruzu's `ServerManager` registration API accepts a session factory, whereas Eden accepts the
  shared service object directly. Each Rust closure now captures one preconstructed `Arc` and
  clones that owner per session; `make_system_settings_factory` isolates this mechanical adapter
  for the typed `set:sys` dependency used by other services.

### Unintentional differences (to fix)

- None in the reviewed service-registration ownership slice.

### Missing items

- None from Eden's `Set::LoopProcess` registration list.

### Binary layout verification

- PASS: no IPC payload or persisted settings structure changed.

### Fixed parity debt

- `set`, `set:cal`, `set:fd`, and `set:sys` now each retain one shared service allocation for the
  server lifetime, matching Eden's four `std::make_shared` registrations. Previously every client
  connection received fresh independent service state.
- A focused regression calls the production `set:sys` factory twice and verifies pointer identity
  and the concrete typed owner required by service-to-service access.

## 2026-08-22 — `btm/btm_system_core.rs` vs Eden `btm_system_core.{h,cpp}`

### Intentional differences

- Eden obtains a typed `shared_ptr<ISystemSettingsServer>` directly from `ServiceManager`. Ruzu's
  type-erased factory API requires a checked `Arc<dyn SessionRequestHandler>` downcast; the
  recovered `Arc<SystemSettingsService>` is the same singleton allocation registered by
  `set/settings.rs`.
- Ruzu's `ServiceContext::CloseEvent` accepts its numeric context handle, so the service retains
  two private handles alongside the two `Arc<Event>` owners. `Drop` closes them in Eden's radio,
  then audio-device order; Rust field destruction releases the remaining wrapper owners
  immediately afterward.
- `new_with_set_sys_provider` is a constructor-test adapter that injects the typed owner only after
  handler registration and event creation, preserving Eden's construction order. The test-only
  `SystemSettingsService::new_for_test` selects the already-existing non-persistent settings
  constructor; production `SystemSettingsService::new` is unchanged.
- Eden's three audio-device stubs receive scratch output buffers that remain unspecified when the
  returned count is zero. Ruzu leaves those guest buffers untouched; the authoritative zero count
  and zero total still tell the caller that no element is valid.

### Unintentional differences (to fix)

- None in the reviewed command table, settings dependency, IPC outputs, or event lifecycle.

### Missing items

- None from Eden's `IBtmSystemCore`; all methods with non-null upstream handlers are ported and all
  upstream null entries remain unimplemented.

### Binary layout verification

- PASS: `bool` outputs occupy one CMIF word, device counts/totals remain signed 32-bit values, and
  `ClientAppletResourceUserId` is sourced from the request PID as in Eden's serialization layer.
  The `std::array<u8, 0xFF>` output elements are not materialized because every matching stub
  reports zero valid elements.

### Fixed parity debt

- Ported commands 0, 1, 4–7, 13, 14, 17, 20, 22, and 23 with their exact success, boolean, count,
  handle, and PID behavior. Radio enable/disable/query now use the shared `set:sys` settings owner.
- Restored stable radio and audio-device readable-event handout and explicit destructor cleanup.
  Focused coverage verifies the full implemented/null command partition, shared settings mutation,
  typed service identity, zero-count stubs, stable event identity, and final owner release. The
  unread BTM system fields are gone and `core` decreases from 58 to 57 warnings.

## 2026-08-22 — `src/core/src/hle/service/caps/caps_c.rs` vs `src/core/hle/service/caps/caps_c.h` and `.cpp`

### Intentional differences

- Rust annotates the private `manager` field with `#[allow(dead_code)]`. Eden likewise retains the
  constructor-provided shared `AlbumManager` without dereferencing it; the field remains in its
  upstream owner to preserve lifetime and structure instead of being removed or renamed.

### Unintentional differences (to fix)

- None in the warning-producing ownership slice.

### Missing items

- None from `IAlbumControlService`; the command table and sole implemented command match Eden.

### Binary layout verification

- N/A: `IAlbumControlService` is not serialized or copied as a wire payload.

## 2026-08-22 — `src/core/src/hle/service/friend/{friend.rs,friend_interface.rs}` vs `src/core/hle/service/friend/{friend.cpp,friend_interface.cpp}`

### Intentional differences

- Rust flattens Eden's `Module::Interface` base into the concrete `Friend` allocation. Its two
  `Create*` methods are nevertheless implemented in `friend.rs`, while the concrete constructor
  and command table remain in `friend_interface.rs` with their upstream owners.
- The flattened fields are `pub(super)` solely so the sibling `friend.rs` implementation can
  access the same allocation; they remain private outside the Friend service module.
- Eden's service framework and `ServiceContext` hold `Core::System&` through their base classes.
  Ruzu retains the corresponding flattened `SystemRef` in each concrete service; its shared
  `ServiceContext` adapter obtains kernel objects through the existing Rust kernel registry.
- Ruzu retains each event as an `Arc<Event>` plus the numeric `ServiceContext` handle required by
  `close_event`. `Drop` explicitly closes the handle at the same lifecycle point as Eden, after
  which ordinary Rust field destruction releases the wrapper owner.
- Eden's readable-event `Signal` returns a result that is pushed directly into the response.
  Ruzu's infallible `Event::signal` returns `()`, so the handler signals first and pushes
  `RESULT_SUCCESS` before handing out the same readable endpoint.
- Field-local `#[allow(dead_code)]` attributes cover only owners that Eden deliberately retains
  without later dereferencing: the flattened module owner, service `SystemRef`s, and notification
  UUID. Removing them would change upstream lifetime ownership.

### Unintentional differences (to fix)

- None in the reviewed command tables, implemented handlers, payloads, system/module forwarding,
  or event lifecycle.

### Missing items

- None from Eden's `IFriendService`, `INotificationService`, `Module::Interface`, concrete `Friend`,
  or neighbor-detection service tables. All non-null upstream handlers are ported and every null
  entry remains unimplemented.

### Binary layout verification

- PASS: compile-time assertions preserve `SizedFriendFilter` and `SizedNotificationInfo` at 0x10
  bytes and `FriendsUserSetting` at 0x800 bytes. A byte-level regression verifies UUID placement,
  permissions, reception flag, NUL-terminated default friend code, next-issuable time, and fully
  zeroed reserved bytes.

### Fixed parity debt

- Restored the complete 112-command `IFriendService` table and exact 22-handler partition,
  including firmware-versioned V1/V2 entries that were absent from Ruzu.
- Ported the missing active Eden stubs: cancellation, friend-list synchronization and viewer
  count, received-request outputs, zeroed presence view, user settings, and summary overlay
  notification.
- Restored constructor `SystemRef` forwarding, stable readable-event identity, completion-event
  signaling, and explicit event closure for both Friend interfaces. The warning-producing fields
  are now either consumed by behavior or narrowly documented as upstream lifetime owners.

## 2026-08-22 — `src/core/src/hle/service/psc/time/service_manager.rs` vs `src/core/hle/service/psc/time/service_manager.{h,cpp}`

### Intentional differences

- Ruzu's service registry exposes `Arc<dyn SessionRequestHandler>` rather than Eden's typed
  `shared_ptr<ServiceManager>`. Direct consumers retain that singleton allocation and perform a
  checked downcast before calling the public service methods.
- Eden returns `KReadableEvent*` through an out-copy-handle wrapper. Ruzu's public method places
  the corresponding shared service `Event` in an `Option`; the IPC handler alone materializes and
  caches its kernel-readable handle.
- The flattened `system` field is retained with a field-local dead-code allowance. Eden's base
  `ServiceFramework` stores the same reference; Ruzu's active time paths access it through the
  captured tick callback and kernel-aware `Event` wrappers.

### Unintentional differences (to fix)

- Eden calls `CheckAndSetupServicesSAndP` after each setup command and dynamically registers
  `time:s` and `time:p` once all clock cores are initialized. Ruzu does not yet port that
  registration state or either helper, and `psc.rs` still treats related services separately.

### Missing items

- `CheckAndSetupServicesSAndP`, `SetupSAndP`, and the associated server-manager owner remain
  missing. The reviewed event methods for commands 50–60 and alarm methods for commands 200–202
  are present.

### Binary layout verification

- PASS: `AlarmInfo` continues to use its existing raw IPC layout. Invalid queries set only the
  validity flag and preserve the caller-initialized info/time outputs, matching Eden.

### Fixed parity debt

- Restored the three public service methods as the owners of commands 200–202; IPC handlers now
  delegate instead of owning their behavior.
- Restored `GetStandardLocalClockOperationEvent`,
  `GetStandardNetworkClockOperationEventForServiceManager`,
  `GetEphemeralNetworkClockOperationEventForServiceManager`, and
  `GetStandardUserSystemClockAutomaticCorrectionUpdatedEvent` as the owners of commands 50–60;
  their IPC handlers now only adapt results and handles.
- Replaced eager `then_some(&*pointer)` evaluation with explicit null branches before creating
  optional shared-memory references. This removes a real null dereference while preserving the
  constructor's `None` semantics.
- Corrected the local-clock regression expectation: when steady-clock source IDs differ, Eden
  derives a context from the current steady clock rather than copying the supplied context.
- Restored Eden's non-fatal error log when the boot-time timezone rule cannot be parsed; setup
  continues and initializes the remaining timezone state in the same order.

## 2026-08-22 — `src/core/src/hle/service/glue/time/alarm_worker.rs` vs `src/core/hle/service/glue/time/alarm_worker.{h,cpp}`

### Intentional differences

- Eden reaches CoreTiming and kernel event operations through `Core::System&`. Ruzu retains the
  shared `Arc<CoreTiming>` directly, while its service `Event` encapsulates the kernel bridge used
  by signal and clear operations.
- The Rust service registry erases concrete handler types. `AlarmWorker` retains the exact
  singleton `Arc<dyn SessionRequestHandler>` and performs a checked downcast to
  `TimeServiceManager` before each direct service call; ownership remains equivalent to Eden's
  typed `shared_ptr`.
- Eden's nullable event pointers are represented as `Option<Arc<Event>>` before initialization.
  Ruzu additionally retains the numeric `ServiceContext` handle needed by `close_event`.
- Eden carries an unused reference to `StandardSteadyClockResource`. Rust does not create a
  self-reference between sibling fields of `TimeWorker`; the resource has no behavior in
  `AlarmWorker`, and CoreTiming is supplied directly instead.

### Missing items

- None inside `AlarmWorker`; constructor state, `Initialize`, both getters,
  `OnPowerStateChanged`, `GetClosestAlarmInfo`, `AttachToClosestAlarmEvent`, and destructor ordering
  are present.

### Binary layout verification

- N/A: this slice creates and schedules event objects but serializes no raw payload.

### Fixed parity debt

- Replaced the unrelated synthetic closest-alarm event with the actual event owned by PSC
  `time:m` and restored stable endpoint identity.
- Timer creation now occurs during `Initialize` through `ServiceContext`; destruction unschedules
  the CoreTiming callback before closing that timer event, matching Eden's order.
- Removed the invented `with_refs` and setter construction paths. Required references now arrive
  through the production `TimeManager -> TimeWorker -> AlarmWorker` construction chain.

## 2026-08-22 — `src/core/src/hle/service/glue/time/{manager.rs,worker.rs}` vs `src/core/hle/service/glue/time/{manager,worker}.{h,cpp}`

### Intentional differences

- Ruzu passes the already-resolved singleton `time:m` handler and shared CoreTiming allocation into
  `TimeWorker`; this is the Rust ownership form of Eden resolving `time:m` during
  `TimeWorker::Initialize` and reaching CoreTiming through `Core::System&`.
- Unit tests may construct `Glue::Time::TimeManager` with a null `SystemRef`; that test-only path
  supplies an isolated CoreTiming allocation. Production always uses the System-owned allocation.

### Unintentional differences (to fix)

- `PmStateChangeHandler` still omits its reference to `AlarmWorker`; Ruzu currently constructs it
  independently. Its current behavior remains equivalent because Eden's constructor only stores
  that reference and leaves PM-module registration as a TODO, so both priorities stay zero.

### Missing items

- The PM-module registration described by Eden's own TODO remains absent in
  `pm_state_change_handler.rs`; this does not remove any implementation present in `worker.cpp`.

### Binary layout verification

- PASS: `SystemClockContext` and `SteadyClockTimePoint` remain respectively `0x20` and `0x18`
  bytes. Full zero-initialized buffers are written to `set:sys`, preserving the raw C layouts.

## 2026-08-22 — `src/core/src/hle/service/glue/time/file_timestamp_worker.rs` vs `src/core/hle/service/glue/time/file_timestamp_worker.{h,cpp}`

### Intentional differences

- Eden default-initializes nullable `shared_ptr` fields; Ruzu represents the same pre-initialize
  state as `Option<Arc<SystemClock>>` and `Option<Arc<TimeZoneService>>`.
- Failed or missing prerequisites return early through Rust `Option`/`Result` matching instead of
  C++'s short-circuit boolean expression. The call order remains initialized flag, clock read,
  timezone conversion.

### Missing items

- `IFileSystemProxy::SetCurrentPosixTime` remains absent exactly where Eden also leaves it as a
  TODO.

### Binary layout verification

- N/A: calendar values remain local typed objects and are not serialized by this worker.

### Fixed parity debt

- Restored both upstream service owners and the complete implemented portion of
  `SetFilesystemPosixTime`. Previously Ruzu returned after the initialized check without querying
  either service.
- A lifetime regression verifies that the exact clock and timezone allocations remain owned until
  the worker is destroyed.
## 2026-08-22 — `src/core/src/hle/service/glue/time/manager.rs` vs `src/core/hle/service/glue/time/manager.{h,cpp}`

### Intentional differences
- Rust stores the three manager-owned resources in `Arc<Mutex<_>>`: this preserves Eden's single
  allocation and reference-sharing contract without self-referential Rust structs.

### Unintentional differences (to fix)
- `m_set_sys` and `m_time_m` are acquired as temporary manager references rather than retained as
  manager fields. The exact singleton owners are now forwarded to and retained by `TimeWorker`, so
  runtime lifetime and behavior match while the manager's field layout remains structurally
  different.

### Missing items
- The final constructor-side filesystem timestamp update still stops at Eden's own filesystem TODO.

### Binary layout verification
- PASS: this ownership-only change does not serialize or raw-copy any payload.

## 2026-08-22 — `src/core/src/hle/service/glue/time/static.rs` vs `src/core/hle/service/glue/time/static.{h,cpp}`

### Intentional differences
- Eden's borrowed manager-resource references are represented by clones of the same
  `Arc<Mutex<_>>` allocations so returned IPC services can safely outlive a temporary manager lock.

### Unintentional differences (to fix)
- The Rust service does not yet retain Eden's `m_set_sys`, `m_time_m`, `m_time_sm`, and
  `m_time_zone` service owners as corresponding fields.

### Missing items
- Several public clock methods still return simplified success values instead of forwarding to
  the wrapped PSC service; these pre-existing differences remain outside this ownership slice.

### Binary layout verification
- PASS: resource identity and lifetime changed, but no IPC payload layout changed.

## 2026-08-22 — `src/core/src/hle/service/glue/time/time_zone.rs` vs `src/core/hle/service/glue/time/time_zone.{h,cpp}`

### Intentional differences
- C++ borrowed references to `FileTimestampWorker` and `TimeZoneBinary` use shared
  `Arc<Mutex<_>>` owners in Rust, preserving identity and synchronized mutation.

### Unintentional differences (to fix)
- `SetDeviceLocationName` now performs Eden's shared file-timestamp update, but it still does not
  persist the resulting name and update time through `set:sys` because that service owner is not
  retained yet.
- Rust owns only one optional operation event, while current Eden maintains a list of operation
  events and signals every registered reader after a location update.

### Missing items
- The `m_set_sys` owner and its two setters are not yet ported into this glue service.

### Binary layout verification
- PASS: no raw payload type or IPC response layout changed in this ownership slice.

## 2026-08-22 — `src/core/src/hle/service/glue/time/worker.rs` vs `src/core/hle/service/glue/time/worker.{h,cpp}`

### Intentional differences
- Eden's two borrowed resource references are represented by clones of the manager's exact
  `Arc<Mutex<_>>` allocations, avoiding an unsafe self-reference while preserving ownership.
- Rust's `JoinHandle` has no `std::jthread` stop token, so an `Arc<AtomicBool>` carries the same
  stop request and the exit event wakes the wait before join.
- The host worker rebuilds stable boxed `MultiWaitHolder` values for each wait iteration. This is
  the Rust counterpart of Eden's variadic `WaitAny` call and preserves the same event order and
  priority-dependent two-versus-nine event selection.

### Unintentional differences (to fix)
- `PmStateChangeHandler` does not retain the otherwise unused `AlarmWorker` reference. Eden's only
  current constructor behavior is storing that reference beside a TODO for PM-module setup, so the
  active priority and dispatch behavior remain equal at zero.

### Missing items
- PM-module registration remains absent from `pm_state_change_handler.rs`; Eden also marks that
  initialization as TODO.

### Binary layout verification
- PASS: `SystemClockContext` (`0x20`) and `SteadyClockTimePoint` (`0x18`) are copied into fully
  zero-initialized fixed-size settings buffers. Focused dispatch coverage verifies the complete
  local-clock payload written by the background worker.

## 2026-08-22 — `src/ruzu/src/boot.rs` vs Eden `src/yuzu/main_window.cpp`

### Intentional differences
- Before constructing video subsystems, the GTK frontend rejects an OpenGL renderer selected on
  macOS AArch64 with a dedicated multiline diagnostic explaining that Apple Silicon supports only
  the Vulkan renderer in ruzu. Eden uses one generic `ErrorVideoCore` dialog because its supported
  renderer set is not restricted by this Rust frontend's Apple platform port.
- The check consumes the selected global/per-game renderer value after boot configuration is
  applied. Vulkan, Null, macOS x86_64, and non-macOS hosts retain the existing load path and generic
  video-core error handling.

### Unintentional differences (to fix)
- None in this diagnostic slice.

### Missing items
- None.

### Binary layout verification
- N/A: this change only selects frontend error text before renderer construction.

## 2026-08-22 — `src/video_core/src/engines/fermi_2d.rs` vs `src/video_core/engines/fermi_2d.{h,cpp}`

### Intentional differences
- Eden indexes the `Regs::reg_array` union member directly. Rust derives the same word pointer
  from the enclosing `#[repr(C)] RegsStorageRaw` address, avoiding an unsafe union-field
  projection required by Rust 1.89 while preserving the exact offset and contiguous storage.

### Unintentional differences (to fix)
- None in this compiler-compatibility slice.

### Missing items
- None in this compiler-compatibility slice.

### Binary layout verification
- PASS: existing tests verify that `RegsStorageRaw::regs` is at offset zero, the runtime tail
  begins after `NUM_REGS_WORDS`, the total size is `ENGINE_REG_COUNT * 4`, and the word view spans
  both regions contiguously.

## 2026-08-22 — `src/video_core/build.rs` vs Eden root `CMakeLists.txt`

### Intentional differences
- Eden selects C++20 globally. The Rust build script selects C++17 only for the BCN shim and its
  bundled decoder sources, which require a modern C++ mode but not Eden's complete C++20 build
  environment; this also prevents Apple Clang from falling back to C++98.

### Unintentional differences (to fix)
- None in this build-compatibility slice.

### Missing items
- None.

### Binary layout verification
- N/A: this changes only the language mode used to compile the existing C++ shim sources.

## 2026-08-22 — `src/core/src/hle/service/psc/time/tzif.rs` vs Eden `externals/tz/tz/tz.{h,cpp}`

### Intentional differences
- Rust checks the input length and `TZif` magic before decoding the header. Eden's current
  `tzloadbody` copies the header without those guards; rejecting malformed input avoids an
  out-of-bounds read without changing valid Switch archive behavior.
- C++ `bool` storage in `ttinfo` and `Rule` is represented by raw `u8` fields. This preserves the
  same offsets while making every guest-provided bit pattern valid to decode in Rust.

### Unintentional differences (to fix)
- `parse_posix_tz` implements only the POSIX footer forms exercised by the embedded Switch
  archive. Eden retains the complete `tzparse` implementation, including its broader validation
  and transition-generation behavior.

### Missing items
- The remaining `tzparse` branches and edge cases not represented by the current embedded archive
  still need a literal port before the external TZ library can be called complete.

### Binary layout verification
- PASS: `TtInfo` is 0x10 bytes and `TzRule` is 0x4000 bytes with Eden's field offsets, fixed array
  capacities and explicit zeroed padding. Unaligned IPC decoding and full deterministic output are
  covered by focused tests.
- The parser now matches Eden's Switch-specific single 8-byte data block, counter ordering and
  256-byte footer bound; a regression parses the archive's `Etc/GMT` rule.
- `mktime_tzname` now preserves Eden's distinct success, overflow and not-found statuses and writes
  the normalized `CalendarTimeInternal` back only after a successful conversion.

## 2026-08-22 — `src/core/src/hle/service/psc/time/time_zone.rs` vs Eden `src/core/hle/service/psc/time/time_zone.{h,cpp}`

### Intentional differences
- Eden's recursive member mutex is paired with borrowed references. Rust combines the existing
  member mutex with the enclosing `TimeManager` mutex used by shared service owners.
- A zero-capacity Rust output slice returns zero results before writing. Eden writes the first
  element before checking `out_times_max_count`; valid CMIF requests provide output storage, while
  the Rust guard prevents malformed IPC from causing an out-of-bounds access.

### Binary layout verification
- PASS: parsing now updates `m_my_rule` only after success, failed output parsing preserves the
  caller's rule, getters enforce Eden's initialization boundary, and `ValidateRule` checks the
  fixed 0x4000-byte rule before conversion.
- `GetTimeZoneTime`, `ToCalendarTimeImpl` and `ToPosixTimeImpl` now retain Eden's ownership and
  locking boundaries. Reverse conversion preserves overflow/not-found mapping, normalized-calendar
  validation, two-result ambiguity detection and ascending result order; focused regressions cover
  the zero-result and two-result paths.

## 2026-08-22 — `src/core/src/hle/service/psc/time/time_zone_service.rs` and `static.rs` vs Eden PSC time services

### Intentional differences
- `Arc<Mutex<TimeManager>>` retains the single owner behind Eden's
  `StandardSteadyClockCore&` and `TimeZone&` references. Isolated constructors create a private
  manager for unit-level service use; production `StaticService` forwards its shared manager.
- Eden asserts that an `InLargeData` descriptor exists. Ruzu treats a missing descriptor as an
  empty buffer, retaining the same value-initialized rule without aborting the service process.

### Binary layout verification
- PASS: commands 8, 100 and 201 now exchange the fixed 0x4000-byte rule rather than serializing
  Rust `Vec` metadata. Command 7 mutates the manager-owned timezone and captures the shared
  standard steady-clock time point in Eden's order.
- Commands 100 and 201 now reproduce CMIF `InLargeData` decoding by zero-initializing the rule and
  copying the available prefix. Commands 201 and 202 allocate exactly the guest-advertised output
  capacity rather than inventing two output elements.

## 2026-08-22 — `src/core/src/hle/service/glue/time/time_zone.rs` vs Eden `src/core/hle/service/glue/time/time_zone.{h,cpp}`

### Intentional differences
- Eden's borrowed worker/binary references and shared service pointers use the corresponding
  `Arc<Mutex<_>>` or `Arc<_>` owners. Its one intrusive-list member operation event is represented
  by one stable optional `OperationEvent`; repeated handle requests reuse that event.
- Event materialization is deferred until an IPC context can create the kernel bridge. Eden owns
  its kernel event at service construction and recreates it on the first handle request.
- Eden asserts that an `InLargeData` descriptor exists. Ruzu treats a missing descriptor as an
  empty buffer, retaining the same value-initialized rule without aborting the service process.

### Binary layout verification
- PASS: location changes now follow Eden's validation, rule update, filesystem timestamp, wrapped
  readback, settings-name persistence, settings-time persistence and event-signal order. Rule IPC
  uses the complete deterministic 0x4000-byte payload, and output capacities come from the actual
  IPC buffers.
- Commands 100 and 201 preserve Eden's zero-initialize-then-copy-prefix `InLargeData` semantics;
  focused coverage verifies that bytes beyond an undersized input remain zero.

## 2026-08-22 — `src/core/src/hle/service/set/system_settings_server.rs` timezone forwarding vs Eden settings server

### Intentional differences
- Direct Rust forwarding methods return typed values or unit because the corresponding inner
  settings methods cannot fail; Eden expresses the same always-successful operations as `Result`.

### Unintentional differences (to fix)
- The broader pre-existing partial service differences remain recorded in the earlier
  `system_settings_server.rs` audit entry.

### Missing items
- No additional settings prerequisite is missing for timezone persistence.

### Binary layout verification
- PASS: `LocationName` remains 0x24 bytes and `SteadyClockTimePoint` remains 0x18 bytes. Focused
  round-trip coverage includes a negative signed time point and a nonzero homebrew test UUID.

## 2026-08-22 — `src/core/src/hle/service/hid/hid_debug_server.rs` vs Eden `src/core/hle/service/hid/hid_debug_server.{h,cpp}`

### Intentional differences
- Eden stores `shared_ptr` children inside `ResourceManager`. Ruzu retains the existing
  `Arc<Mutex<_>>` resource split and passes the shared `TouchResource`/`TouchScreenDriver` to the
  matching child operation; method ownership and operation order remain in `hid_debug_server.rs`.
- Eden's CMIF templates unwrap arguments and output parameters. Ruzu uses local typed CMIF
  handlers, including the map-alias `TouchState` input buffer and the aligned `(u32, u64)` request.
- `TouchScreen::IsActive` and `Gesture::IsActive` return their infallible boolean directly in the
  current Rust ownership adaptation. Eden returns `ResultSuccess` plus an output boolean; the
  service still evaluates both calls in Eden's order before combining their values.

### Unintentional differences (to fix)
- None remain in this file after the post-implementation comparison.

### Missing items
- None: all nine methods owned by Eden's `IHidDebugServer` are implemented, and all 158 command
  IDs/names have matching implemented-versus-null registration state.

### Binary layout verification
- PASS: `TouchState` is 0x28 bytes, `AutoPilotState` is 0x288 bytes, and
  `TouchScreenConfigurationForNx` is 0x10 bytes. Focused tests also cover the exact command IDs,
  active-handler set, and touch/gesture restart-then-stop lifecycle.

## 2026-08-22 — `src/core/src/hle/service/psc/time/power_state_service.rs` vs Eden `src/core/hle/service/psc/time/power_state_service.{h,cpp}`

### Intentional differences
- Eden's `Core::System&` belongs to the `ServiceFramework` base, not to
  `IPowerStateRequestHandler` itself. Ruzu's framework obtains the process/kernel owner from the
  IPC context when the shared service `Event` lazily materializes its readable copy handle, so the
  duplicate concrete-service `SystemRef` field and constructor parameter were removed.
- `Arc<PowerStateRequestManager>` preserves Eden's borrowed manager lifetime. The manager-owned
  `Event` caches its own kernel bridge, replacing the extra service-local readable-event cache.
- Eden leaves `out_priority` at its value-initialized zero when no request was cleared. The manual
  Rust IPC adapter writes that zero explicitly.

### Unintentional differences (to fix)
- Dynamic `time:p` registration remains part of the already-recorded missing `SetupSAndP` work in
  `service_manager.rs`; no additional behavior difference remains inside this service file.

### Missing items
- None in `IPowerStateRequestHandler`: both commands and their manager delegation are present.

### Binary layout verification
- N/A: the service exchanges scalar CMIF outputs and a copy handle, with no raw aggregate payload.
  Focused tests cover the exact handler table and pending/available/clear state transition through
  the shared manager.

## 2026-08-22 — `src/core/src/hle/service/olsc/remote_storage_controller.rs` and `olsc_service_for_system_service.rs` vs Eden OLSC counterparts

### Intentional differences
- Eden passes `Core::System&` to every child because it belongs to the C++ `ServiceFramework`
  base. Ruzu's framework obtains system state from each IPC context, so the remote controller has
  no duplicate concrete-service `SystemRef`; its parent constructs the otherwise stateless child
  without downcasting merely to recover that unused value.
- The typed CMIF template outputs of `GetSecondarySave` are represented by a private `repr(C)`
  adapter containing the boolean, explicit zero padding, and three `u64` values. This preserves
  the template's alignment while keeping the upstream method itself as the behavior owner.

### Unintentional differences (to fix)
- The broader pre-existing `IOlscServiceForSystemService` table and method parity were not part of
  this warning slice and still require a complete dedicated audit.

### Missing items
- None in `IRemoteStorageController`: all 30 registered IDs/names and all three upstream methods
  are present; commands 18 and 27 deliberately share `GetDataInfo` as in Eden.

### Binary layout verification
- PASS: `GetSecondarySave` writes a deterministic 0x20-byte output with the `[u64; 3]` at offset
  8; `GetDataInfo` writes exactly 0x38 zero bytes. Focused tests cover the layouts, exact handler
  table, implemented/null states, and all upstream stub outputs.

## 2026-08-22 — `src/core/src/hle/service/ns/ecommerce_interface.rs` and `service_getter_interface.rs` vs Eden NS counterparts

### Intentional differences
- Eden supplies `Core::System&` solely to the e-commerce interface's C++ `ServiceFramework` base.
  The Rust dispatcher obtains system state from the IPC context, so the concrete child stores no
  duplicate `SystemRef` and its getter constructs the stateless interface without one.
- Eden's typed `Out<SharedPointer<IECommerceInterface>>` wrapper is a direct Rust return plus a
  thin IPC handler that installs the child as a moved interface object.

### Unintentional differences (to fix)
- `IServiceGetterInterface` still has null handlers for the other getters except commands 7992
  and 7998. Several corresponding Rust modules are only command/data sketches rather than usable
  `ServiceFramework` owners, so completing them requires a separate structural NS slice.

### Missing items
- None in `IECommerceInterface`: all seven upstream commands remain registered as null handlers.
  Command 7992 now constructs and returns that exact child interface as Eden does.

### Binary layout verification
- N/A: this slice exchanges only a moved service interface and defines no raw payload.
  Focused tests cover the seven-entry child table and command 7992 registration.

## 2026-08-22 — `src/core/src/hle/service/nvdrv/core/container.rs` vs Eden `src/core/hle/service/nvdrv/core/container.{h,cpp}`

### Intentional differences
- `Container` is a cloneable Rust handle whose `Arc`-owned NvMap, syncpoint manager, device-file
  data, and session store remain shared. This models the borrowed `NvCore::Container&` retained by
  Eden's NVDEC/VIC base without a raw pointer or a second copy of the state.
- Eden exposes mutable `Host1xDeviceFile()` access. Ruzu keeps that state private and exposes the
  operation-shaped `take_accumulated_syncpoint`/`recycle_syncpoint` pair, preserving
  the constructor/destructor FIFO ordering while preventing unrelated mutation.
- Session process pointers are `Weak<ProcessLock>` and session state is mutex-protected. Missing or
  inactive sessions return `None` instead of relying on Eden's unchecked deque indexing.
- Ruzu releases the session mutex around `NvMap::unmap_all_handles` because that path re-enters the
  shared Rust session store; the observable teardown order remains unmap handles, release the
  preallocated area, deactivate the session, unregister the ASID, then recycle the ID.

### Unintentional differences (to fix)
- No remaining difference was found in the shared-handle and accumulated-syncpoint behavior
  changed by this slice.

### Missing items
- None in the `Container` interface used by NVDEC/VIC: session ownership, NvMap/syncpoint access,
  and accumulated-syncpoint take/return behavior are available through Rust ownership adapters.

### Binary layout verification
- N/A: `Container` and `Session` are internal ownership objects and are never serialized as raw
  guest payloads. A focused test verifies that a cloned handle resolves the exact same process
  session.

## 2026-08-22 — `src/core/src/hle/service/nvdrv/devices/nvhost_nvdec_common.rs`, `nvhost_nvdec.rs`, and `nvhost_vic.rs` vs Eden counterparts

### Intentional differences
- Rust composition replaces C++ inheritance: both concrete devices forward `query_event` to the
  common owner, while each concrete file retains its own ioctl table exactly where Eden defines it.
- The upstream-only `submit_timeout`, `nvmap_fd`, and `device_syncpoints` state and the unused
  `IoctlSubmitCommandBuffer`/`IocGetIdParams` declarations are omitted. Eden never reads any of
  them, and `SetNVMAPfd`/`SetSubmitTimeout` still log and return success exactly as observable by
  the guest.
- Malformed submissions are rejected safely: an absent session/process memory/Host1x returns
  `InvalidState`, non-positive command-buffer word counts are skipped, and a shorter fence array
  does not cause unchecked indexing. Eden assumes all of those invariants.
- Ruzu emits optional host1x trace records and converts each command list to native `u32` values
  because its Host1x interface accepts `Vec<u32>` rather than Eden's moved `CpuGuestMemory` view.

### Unintentional differences (to fix)
- The eager command-list snapshot noted above cannot preserve Eden's lazy guest-memory view if the
  guest mutates that memory before Host1x consumes it. Correcting that requires a dedicated Host1x
  queue/interface ownership change and is not safe to fold into this warning-removal slice.

### Missing items
- None in the ioctl/query-event surface: NVDEC exposes commands 0x01, 0x02, 0x03, 0x07, 0x09,
  0x0A, 0x23 and H/0x01; VIC exposes 0x01, 0x02, 0x03, 0x09, 0x0A and H/0x01. `GetClkRate`
  writes 614400000/0, and unknown query events return no event.

### Binary layout verification
- PASS: every live fixed/variable ioctl payload has `repr(C)` and an exact size assertion:
  `IoctlSetNvmapFD` 0x4, `IoctlSubmit` 0x10, `CommandBuffer` 0xC, `Reloc` 0x10,
  `SyncptIncr` 0x14, syncpoint/waitbase/clock-rate and map entries 0x8, and map parameters 0xC.
  Focused tests cover clock-rate serialization, concrete-device routing, `num_entries` bounds, and
  syncpoint recycling.

## 2026-08-22 — `src/core/src/hle/service/nvdrv/devices/nvhost_as_gpu.rs` vs Eden `src/core/hle/service/nvdrv/devices/nvhost_as_gpu.{h,cpp}`

### Intentional differences
- Eden retains a `Container&` after deriving `nvmap` from it but never reads that reference again.
  Ruzu removes the equivalent raw pointer and keeps only the live NvMap dependency.
- Eden's `std::map`/`unordered_dense::set` owners map to `BTreeMap`/`HashSet`; `Arc<Mapping>` lets
  allocation records and the mapping map refer to one Rust mapping owner instead of duplicating an
  offset and performing a second lookup.
- Eden owns a concrete `Tegra::MemoryManager`; Ruzu owns an `Arc<dyn GpuMemoryManagerHandle>` so the
  renderer backend can provide the platform implementation while preserving map/unmap ordering.
- Invalid device descriptors, absent memory managers, failed pins, missing allocators, and stale
  tracked mappings return an NV error or safe success instead of relying on Eden's assertions or
  unchecked dereferences. Optional trace calls do not alter the guest outputs.
- Rust uses one outer operation mutex plus mutexes required by the shared trait handles. `Remap` is
  also serialized even though Eden omits its otherwise customary `scoped_lock`.

### Unintentional differences (to fix)
- No remaining difference was found in the dead-container removal, `map_buffer_offsets` lifecycle,
  or `GetVARegionsImpl`/`GetVARegions1`/`GetVARegions3` ownership corrected by this slice.

### Missing items
- None: the ioctl1 commands A/1, A/2, A/3, A/5, A/6, A/8, A/9 and A/0x14, ioctl3 A/8,
  empty open/close hooks, and unknown-event behavior all have counterparts.

### Binary layout verification
- PASS: all ioctl payloads have `repr(C)` and exact upstream sizes, including the newly asserted
  0x4-byte `IoctlBindChannel`; `VaRegion` is 0x18 and `IoctlGetVaRegions` is 0x40. Focused tests
  cover tracked/untracked unmap behavior, the inline-region copy bound, and existing allocation,
  mapping, sparse, remap, free, and channel-binding behavior.

## 2026-08-22 — `src/core/src/hle/service/nvdrv/devices/nvhost_ctrl.rs`, `nvdevice.rs`, `nvdrv.rs`, and `nvdrv_interface.rs` vs Eden counterparts

### Intentional differences
- Rust exposes the persistent readable event as `Arc<Mutex<KReadableEvent>>` instead of returning
  Eden's owning `KEvent*`; copying that shared handle into the IPC object table preserves the same
  event identity and waiter state.
- The host-action closure captures the slot's atomic status and readable-event handle directly.
  Eden captures `this` and the slot, but Rust cannot lock the event-table mutex from this callback
  because `RegisterHostAction` may invoke it synchronously while `IocCtrlEventWait` holds that mutex.
- Optional event trace records add diagnostics without changing event status, handle, or IPC output.

### Unintentional differences (to fix)
- The warning-triggering process/scheduler owner adapter was dead: neither weak field was read and
  the live host-action callback already signals `KReadableEvent` directly. It has been removed from
  all four layers, restoring Eden's direct `QueryEvent` path.

### Missing items
- None for queried-event ownership or signalling. The callback retains and signals the same
  persistent readable event returned by `QueryEvent`.

### Binary layout verification
- PASS: `SyncpointEventValue` remains a 4-byte raw value and this cleanup does not alter any ioctl
  payload. A focused test verifies allocated-event decoding and matching-syncpoint lookup.

## 2026-08-22 — `src/core/src/hle/service/nvdrv/nvdrv_interface.rs` vs Eden `src/core/hle/service/nvdrv/nvdrv_interface.{h,cpp}`

### Intentional differences
- Eden's `Common::ScratchBuffer<u8>` owners map to reusable `Vec<u8>` fields. Ruzu clears the
  requested range before every dispatch instead of leaving reused bytes unspecified, preserving
  deterministic reserved/output bytes as required by the Rust raw-payload contract.
- The static `ServiceFramework` adapters and mutex-protected `NvdrvInterface` state split one C++
  `NVDRV` object into two Rust layers; the buffers remain owned by that per-service state.
- Optional ioctl tracing/history observes the service-owned buffers after dispatch without changing
  the guest-visible write condition or response.

### Unintentional differences (to fix)
- None in the output-buffer ownership corrected by this slice.

### Missing items
- None for ioctl scratch storage: ioctl1/ioctl2 reuse `output_buffer`, while ioctl3 reuses both
  `output_buffer` and `inline_output_buffer` before writing descriptors 0 and 1 respectively.

### Binary layout verification
- N/A: the reusable vectors are host-only storage. Their requested lengths still come directly from
  the IPC write descriptors, and a focused test verifies resize and deterministic clearing.

## 2026-08-22 — `src/core/src/cpu_manager.rs` and `src/core/src/hle/kernel/k_scheduler.rs` vs Eden `cpu_manager.{h,cpp}` and `k_scheduler.{h,cpp}`

### Intentional differences
- Ruzu's CPU loop passes a cached per-core JIT table into its `run_guest_thread_once` adaptation;
  the retained `_process_owner` `Arc` keeps those raw pointers valid without locking the process on
  every inner-loop iteration. Eden follows raw owner pointers from `KThread` in `PhysicalCore`.
- `wait_for_next_runnable_thread` is a Rust polling fallback around the scheduler priority queue;
  Eden's fiber switch loop waits through its scheduler/idle-thread lifecycle instead.

### Unintentional differences (to fix)
- The process owner was redundantly passed into `run_guest_thread_once` after the cached-JIT change,
  although the callee never read it. Ownership now remains explicitly in each caller only.
- The Rust-only runnable-thread polling helper accepted a current-thread ID that never influenced
  selection. The dead parameter and its forwarding were removed.

### Missing items
- This slice does not change the already documented Rust scheduler/fiber adaptations; it only
  removes parameters with no control-flow, lifetime, or selection role.

### Binary layout verification
- N/A: these are host execution-loop signatures and do not define serialized guest data.

## 2026-08-22 — service bootstrap in `services.rs`, `am/am.rs`, `aoc/addon_content_manager.rs`, and `filesystem/filesystem.rs` vs Eden counterparts

### Intentional differences
- Ruzu passes `SystemRef` and, for FileSystem, the shared Rust `FileSystemController` handle into
  each service process. Eden reaches the controller through `Core::System&`.
- The Rust launcher still uses explicit closures/macros for host and guest service processes instead
  of Eden's tables of `void (*)(Core::System&)` function pointers.

### Unintentional differences (to fix)
- `GenericStubService` has no Eden counterpart and guesses that command 0 returns a sub-interface.
  Removing its unused domain-header local and duplicate command read does not resolve that broader
  service-parity debt; concrete services must replace each remaining use.

### Missing items
- AM, AOC, and FileSystem no longer accept an unused global `ServiceManager`: like Eden, their
  `loop_process` entry points create and own a local `ServerManager` from the system context.
- The unused `ServiceManager` clone before PCTL launch was removed because PCTL already had the
  upstream-shaped system-only entry point.

### Binary layout verification
- N/A: this slice changes host service bootstrap signatures and local dispatch variables only.

## 2026-08-22 — `nvnflinger/window.rs` and `sockets/sockets.rs` vs Eden `window.h` and `sockets.h`

### Intentional differences
- Eden uses `enum class` plus `DECLARE_ENUM_FLAG_OPERATORS`; Ruzu expresses the same flag types
  with `bitflags!`, so their rustdoc belongs inside the macro invocation.

### Unintentional differences (to fix)
- The two type comments were attached to macro invocations rather than generated types and produced
  no Rust documentation. They now document `NativeWindowTransform` and `PollEvents` directly.

### Missing items
- None in this documentation-placement slice.

### Binary layout verification
- PASS: flag bases and values remain unchanged (`u32` for window transform, `u16` for poll events).

## 2026-08-22 — exhaustive value handling in `k_page_table_base.rs`, VI scaling, and `internal_network/sockets.rs` vs Eden counterparts

### Intentional differences
- Eden's `OperationType` switch ends in `UNREACHABLE`; Rust's closed enum makes the equivalent
  single-address `operate` match exhaustive at compile time, so an unreachable wildcard is omitted.
- POSIX platforms may define `EWOULDBLOCK` and `EAGAIN` to the same number. Ruzu uses one guarded
  match arm so both names map to `Errno::Again` without an unreachable-pattern diagnostic.

### Unintentional differences (to fix)
- VI previously transmuted an arbitrary IPC `u32` into `NintendoScaleMode`, which is undefined
  behavior for values outside 0..=4 and made its fallback arm illusory. `from_raw` now rejects such
  values before enum construction and returns `ResultOperationFailed`, matching Eden's `default`.

### Missing items
- None for these value-domain checks.

### Binary layout verification
- PASS: `NintendoScaleMode` remains `repr(u32)` with values 0 through 4; the new conversion does not
  alter its representation. `OperationType` discriminants and native errno values are unchanged.

## 2026-08-22 — `src/core/src/hle/kernel/svc/svc_ipc.rs` vs Eden `src/core/hle/kernel/svc/svc_ipc.cpp`, `k_client_session.cpp`, and `k_server_session.{h,cpp}`

### Intentional differences
- Ruzu retains an inline HLE dispatch fallback for ownerless test fixtures. Eden always queues the
  request through `KClientSession` and waits for the owning server thread.

### Unintentional differences (to fix)
- The inline fallback converted enqueue and receive failures to `ResultInvalidHandle`. Both phases
  now preserve the original Kernel result, matching Eden's `R_RETURN` chain from
  `KServerSession::OnRequest` through `KClientSession::SendSyncRequest` and `SendSyncRequestImpl`.

### Missing items
- None for result propagation on the inline request path. A focused regression test verifies that
  a closed session returns Kernel `ResultSessionClosed` rather than `ResultInvalidHandle`.

### Binary layout verification
- N/A: this change only preserves a 32-bit result code already produced by the session layer; no
  IPC payload or raw-memory structure changes.

## 2026-08-22 — `src/core/src/hle/kernel/k_server_session.rs` vs Eden `src/core/hle/kernel/k_server_session.{h,cpp}`

### Intentional differences
- Eden's pointer-descriptor constructors read through a const `MessageBuffer` view. Ruzu's
  `PointerDescriptor::from_raw` reads the same two words directly from the immutable source slice.

### Unintentional differences (to fix)
- The receive and send pointer helpers cloned the complete source message into mutable vectors and
  constructed unused `MessageBuffer` views. Those dead allocations are removed; descriptor offsets,
  memory-copy direction, validation, and destination writes remain unchanged.

### Missing items
- None in the pointer-descriptor source parsing covered by this cleanup.

### Binary layout verification
- PASS: each pointer descriptor is still decoded from the same two `u32` words and encoded into the
  same destination offsets. Existing focused tests cover send copying, receive linear-to-user
  copying, receive heap-to-heap copying, and end-to-end request pointer payload transfer.

## 2026-08-22 — `src/core/src/hle/kernel/k_condition_variable.rs` vs Eden `src/core/hle/kernel/k_condition_variable.{h,cpp}`

### Intentional differences
- `signal_to_address` returns the next-owner handle together with the result when releasing Ruzu's
  process mutex; the scheduler guard remains live through `end_wait`, preserving Eden's ordering
  without nesting the process mutex around the Rust thread lock.
- The active condition-variable wait queue is constructed in `wait_locked_after_sleep_guard`, where
  Ruzu's split implementation calls `BeginWait`. Eden constructs its stack queue at `Wait` entry.

### Unintentional differences (to fix)
- `signal_to_address` initialized `next_owner_thread` to `None` before unconditionally replacing it.
  The redundant initialization is removed by returning `(result, next_owner_thread)` from the
  process-locked section.
- `wait_locked` constructed a second queue that was never configured or passed to `BeginWait`; the
  unused duplicate is removed.

### Missing items
- None for the owner-transfer or wait-queue lifetimes changed by this cleanup.

### Binary layout verification
- N/A: condition-variable ownership and queues are host kernel state; no raw payload layout changes.

## 2026-08-22 — `src/core/src/hle/kernel/k_interrupt_manager.rs` vs Eden `src/core/hle/kernel/k_interrupt_manager.{h,cpp}`

### Intentional differences
- Ruzu snapshots the current thread fields before acquiring scheduler and process locks to preserve
  its host-mutex order. Eden accesses the embedded kernel objects directly under its scheduler lock.

### Unintentional differences (to fix)
- The snapshot included `KThread::current_core`, although both Eden and Ruzu use the interrupt's
  `core_id` for the pinned-thread lookup and pin operation. The unused field read is removed.

### Missing items
- None for interrupt-core selection: clear, pinned-thread lookup, pinning, and schedule request all
  continue to use the `core_id` argument supplied by the physical core.

### Binary layout verification
- N/A: the removed tuple element was temporary host state and was never serialized.

## 2026-08-22 — `src/core/src/hle/kernel/message_buffer.rs` vs Eden `src/core/hle/kernel/message_buffer.h`

### Intentional differences
- Rust names the header argument `_hdr` in `get_special_data_index` because, exactly as in Eden's
  formula, the special-data start depends only on the fixed message-header size and special-header
  size. Keeping the argument preserves the upstream helper signature without an unused warning.

### Unintentional differences (to fix)
- Ruzu had removed the header parameter from `get_special_data_index` while retaining it in the
  downstream index helpers. The parameter and forwarding chain now match Eden again.

### Missing items
- None for the special-data, pointer, map-alias, raw-data, and receive-list index dependency chain.

### Binary layout verification
- PASS: the index formula remains `MessageHeader::DATA_SIZE / 4 + spc.header_size / 4`; only the
  upstream-compatible header forwarding is restored. Existing message-buffer index and IPC copy
  tests exercise the downstream offsets.

## 2026-08-22 — `src/core/src/file_sys/patch_manager.rs` vs Eden `src/core/file_sys/patch_manager.{h,cpp}`

### Intentional differences
- Rust locks the shared filesystem controller and content-provider union while both temporary
  `PatchManager` values borrow them. Eden receives stable references directly from `Core::System`.
- When no content provider is installed, Ruzu returns empty metadata; Eden's accessor contract
  assumes the provider has already been initialized.

### Unintentional differences (to fix)
- Ruzu was missing `GetMetadataFromBaseOrUpdate`. The associated method now checks the application
  title first and, only when its NACP is absent, checks `GetUpdateTitleID(application_id)`.

### Missing items
- None for this base/update metadata lookup helper.

### Binary layout verification
- N/A: the method forwards existing `NACP` and virtual-file owners without changing their layout.
  A focused provider test verifies the exact base-then-update request order.

## 2026-08-22 — `src/core/src/hle/service/am/service/application_functions.rs` vs Eden `src/core/hle/service/am/service/application_functions.{h,cpp}`

### Intentional differences
- The Rust method returns a value-initialized `[u8; 16]` which the handler writes to CMIF as two
  `u64` values. Eden receives a value-initialized `Out<DisplayVersion>` from CMIF serialization.
- The bounded copy and fallback are extracted into a file-local helper so their byte-level behavior
  can be tested without constructing the full emulator system; ownership remains in the matching
  application-functions file.

### Unintentional differences (to fix)
- `GetDisplayVersion` previously ignored its service owner and always returned `"1.0.0"`. It now
  reads the applet program ID and uses `PatchManager::GetMetadataFromBaseOrUpdate`, matching Eden.

### Missing items
- None for display-version lookup, fallback, bounded copy, final NUL byte, or CMIF output size.

### Binary layout verification
- PASS: the response remains a deterministic 16-byte `DisplayVersion`; bytes beyond the copied
  string are zero and byte 15 is always NUL. Focused tests verify the fallback and 16-byte truncation.

## 2026-08-22 — `src/core/src/hle/service/hid/hid_server.rs` vs Eden `src/core/hle/service/hid/hid_server.{h,cpp}`

### Intentional differences
- Rust decodes `ClientAppletResourceUserId` directly as its single `u64` `pid` value and returns the
  IPC interface through `ResponseBuilder`; Eden expresses both through CMIF wrapper types.

### Unintentional differences (to fix)
- `CreateAppletResource` previously discarded the resource manager result without reproducing
  Eden's diagnostic. It now logs the ARUID and raw result before constructing the interface.

### Missing items
- None for the `CreateAppletResource` call, diagnostic, interface construction, or unconditional
  success behavior.

### Binary layout verification
- N/A: this correction only consumes the existing result for diagnostics and does not alter IPC
  payload or HID shared-memory layout. A focused test verifies that a manager failure is logged-only
  behavior and the handler still returns success plus an interface, as Eden does.

## 2026-08-22 — `src/core/src/hle/kernel/kernel.rs`, `src/core/src/core.rs` vs Eden `src/core/hle/kernel/kernel.{h,cpp}`

### Intentional differences
- Ruzu initializes the persistent font and IRS objects from `System::initialize_kernel` after its
  physical memory manager has been initialized. Eden performs the same ordered allocation inside
  `KernelCore::Impl::InitializeHackSharedMemory`; this split follows Ruzu's existing staged kernel
  initialization without changing object ownership or allocation order.
- Rust retains the registered kernel object as `(object_id, Arc<KSharedMemory>)`; Eden retains its
  intrusive `KSharedMemory*`. Both expose one stable kernel-owned object for its full boot lifetime.

### Unintentional differences (to fix)
- Ruzu previously lacked Eden's kernel-owned `font_shared_mem`. It now allocates it before IRS with
  owner permission `None`, user permission `Read`, size `0x1100000`, and clears it before IRS during
  shutdown.

### Missing items
- None for the font shared-memory field, initialization order, permissions, accessor, persistence,
  or shutdown order required by the platform font services.

### Binary layout verification
- N/A: the object owns raw shared pages rather than a serialized structure. A focused test verifies
  the exact allocation size and stable object identity across repeated initialization.

## 2026-08-22 — `src/core/src/hle/service/ns/platform_service_manager.rs` vs Eden `src/core/hle/service/ns/platform_service_manager.{h,cpp}`

### Intentional differences
- Rust registers the kernel object's stable ID and `Arc<KSharedMemory>` with the caller process so
  its IPC layer can translate the deferred copy object into a process handle. Eden's intrusive
  kernel object and CMIF `OutCopyHandle` perform the equivalent registration during serialization.
- The direct full-buffer copy is extracted into a file-local helper so it can be tested against a
  real `KSharedMemory` without constructing a complete emulator system.

### Unintentional differences (to fix)
- `GetSharedMemoryNativeHandle` previously allocated and cached a separate shared memory in each
  `pl:*` service, used incorrect owner permission `Read`, and redundantly resolved the caller twice.
  It now copies the complete font blob into `KernelCore::GetFontSharedMem()` on every request and
  returns that single kernel-owned object, matching Eden.

### Missing items
- None for font-buffer copying, shared-object ownership, caller registration, or returned handle
  identity in `GetSharedMemoryNativeHandle`.

### Binary layout verification
- PASS: the copied region remains exactly `0x1100000` bytes. A focused test pre-fills the complete
  kernel buffer and verifies that the service copy overwrites it byte-for-byte with the font blob.

## 2026-08-22 — `src/core/src/hle/service/nvnflinger/buffer_queue_consumer.rs` vs Eden `src/core/hle/service/nvnflinger/buffer_queue_consumer.{h,cpp}`

### Intentional differences
- The unused release-fence parameter is named `_release_fence` in Rust to make Eden's deliberate
  temporary non-use explicit while preserving the public method signature.

### Unintentional differences (to fix)
- Ruzu previously retained a sampled `[BQC_RELEASE]` diagnostic counter absent from Eden. It has
  been removed, and Eden's explanatory TODO beside the intentionally disabled fence assignment is
  now preserved at the matching point.

### Missing items
- Proper waiting on release fences remains an upstream TODO; Ruzu deliberately keeps the previous
  acquire fence exactly as Eden does rather than inventing behavior ahead of upstream.

### Binary layout verification
- N/A: no serialized or raw-memory payload changes. A focused state test verifies that release
  frees the acquired slot without replacing its existing fence.

## 2026-08-22 — `src/core/src/hle/service/nvnflinger/buffer_queue_producer.rs` vs Eden `src/core/hle/service/nvnflinger/buffer_queue_producer.cpp`

### Intentional differences
- Rust turns Eden's fatal `UNIMPLEMENTED_IF_MSG` and `ASSERT_MSG` paths into explicit panics because
  it does not use Eden's assertion macros.

### Unintentional differences (to fix)
- The Connect-listener panic helper accepted but ignored the transaction code even though Eden's
  listener diagnostic does not include it; the dead parameter is removed.
- The generic unsupported-transaction panic previously supplied `name` and `code` in reverse order.
  It now reports the numeric transaction first and its symbolic name second.

### Missing items
- Producer-listener parcel decoding remains intentionally unimplemented exactly where Eden raises
  `UNIMPLEMENTED_IF_MSG`.

### Binary layout verification
- N/A: only fatal diagnostics changed. Focused tests verify the exact listener and unsupported
  transaction messages.

# Current upstream parity debt

This file contains only active differences confirmed in the current source tree against
`~/Dev/emulators/zuyu`. Implementation history, diagnostics, commands, runtime logs, and audit
procedures are intentionally omitted.

## 2026-08-22 — `src/core/src/debugger/debugger_interface.rs` vs Eden `src/core/debugger/debugger_interface.h`

### Intentional differences
- Rust represents upstream `Kernel::KThread*` backend/frontend arguments as stable numeric thread
  identifiers. Kernel thread ownership remains in the process registries, avoiding non-owning raw
  pointers across the debugger connection thread.
- Rust traits replace the C++ virtual base classes. The eventual frontend/backend wiring passes the
  backend explicitly rather than constructing a self-referential Rust object.

### Unintentional differences (to fix)
- None.

### Missing items
- None in the action enum or declared backend/frontend operations.

### Binary layout verification
- N/A: these interfaces are not serialized. A focused test verifies the complete action set and its
  upstream declaration order.

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
- Fixed: `ITBlockCheck` was absent. `it_block_check` now rejects exactly an active, nonfinal Thumb
  IT position, using the same `IsInITBlock() && !IsLastInITBlock()` predicate as Eden.

### Missing items

- None for the three common helpers reviewed so far. Other
  pre-existing helpers in `common.h` were not re-audited or claimed by this prerequisite.

### Binary layout verification

- N/A: these helpers construct internal SSA operations or inspect translation state and serialize
  no guest-visible payload. Focused tests cover inactive, final, and nonfinal IT positions.

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

## 2026-08-24 — `src/rdynarmic/src/bin/{a32_diff.rs,compile_bench.rs}` vs Eden `src/dynarmic/src/dynarmic/interface/A32/config.h` and `tests/A32/testenv.h`

### Intentional differences

- Both executables are Ruzu developer tools without direct Eden executable counterparts. Their
  sparse differential address space and deterministic compilation workloads remain tool-local;
  callback ownership and configuration follow Eden's A32 interface.
- Rust owns callbacks in `A32UserConfig` instead of storing Eden's non-owning `UserCallbacks*`.

### Unintentional differences (to fix)

- Fixed: both tools implemented the architecture-merged legacy callbacks, including A64-only
  128-bit accesses and `u64` guest addresses. They now implement the A32 callback interface
  directly with `u32` addresses, typed A32 exceptions, and A32-exclusive callback names.
- Fixed: both tools built the merged legacy configuration and supplied irrelevant A64 register and
  address-space options. They now construct `A32UserConfig` and override only cycle counting, code
  cache size, and the optimization mask used by each workload.

### Missing items

- None in the callback/configuration ownership of these two developer tools.

### Binary layout verification

- N/A: the tools exchange no raw callback or configuration payload. Focused compile-time tests
  require both environments to implement the architecture-owned A32 callback trait.

## 2026-08-24 — `src/rdynarmic/src/{tests_a32.rs,tests_a32_fuzz.rs}` callback/configuration ownership vs Eden `src/dynarmic/src/dynarmic/interface/A32/config.h` and `tests/A32/testenv.h`

### Intentional differences

- Rust keeps deterministic and differential cases in crate-local test modules rather than Eden's
  Catch2 translation units. `Box<dyn A32UserCallbacks>` owns each environment for the Rust JIT,
  replacing Eden's non-owning callback pointer.
- Sparse test memory uses Rust maps and mutexes while preserving Eden's A32 `u32` guest-address
  domain and little-endian byte assembly.

### Unintentional differences (to fix)

- Fixed: the A32 test environments implemented the merged legacy interface, carried A64-only
  128-bit/cache callbacks, and widened guest addresses to `u64`. They now implement the exact A32
  callback inventory with typed exceptions and wrapping `u32` address arithmetic.
- Fixed: test JIT builders now construct `A32UserConfig` directly and mutate only the code-cache,
  optimization, cycle-counting, and coprocessor fields exercised by the corresponding test.

### Missing items

- None in the callback/configuration ownership covered by this slice. Differential tests still
  require their separately built Eden oracle executable at runtime.

### Binary layout verification

- N/A: callback/configuration objects are host-side state. Existing instruction tests cover the
  memory and coprocessor paths; a focused local fuzz-environment regression constructs and runs an
  A32 JIT without relying on the external oracle.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/{a32_core.rs,a64_core.rs}` vs Eden `src/dynarmic/src/dynarmic/backend/arm64/{a32_core.h,a64_core.h}` and architecture config headers

### Intentional differences

- Rust returns address-space emission errors from `run` and `step`; Eden's `GetOrEmit` path relies
  on assertions/allocation invariants and returns its entry point directly.
- The architecture-specific test callbacks are boxed by their Rust `UserConfig`; Eden stores a
  non-owning callback pointer.

### Unintentional differences (to fix)

- Fixed: both core test modules built the merged legacy configuration. Their callback inventories,
  address widths, vector values, and exception types now come directly from their matching A32 or
  A64 interface, and their builders return the architecture-owned configuration without adapters.
- Fixed: the A64 page-table test now assigns the page table and its address-space width through the
  A64-owned fields instead of mutating a shared memory carrier and converting it afterward.

### Missing items

- None in the `A32Core`/`A64Core` run and step surface or this test-configuration slice.

### Binary layout verification

- N/A: these core wrappers and test configurations serialize no guest payload. The complete
  `rdynarmic` test target compiles for `aarch64-unknown-linux-gnu` with both callback traits checked.

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
  global-monitor pointer and processor ID remain owned by the architecture-specific JIT
  configuration.
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

## 2026-08-22 — `src/core/src/file_sys/fssystem/hierarchical_integrity_verification_storage.rs` vs Eden `src/core/file_sys/fssystem/fssystem_hierarchical_integrity_verification_storage.{h,cpp}`

### Intentional differences
- Rust uses `Arc::get_mut` while a verification level is exclusively owned, and a scoped closure
  plus explicit error cleanup in place of Eden's `ON_RESULT_FAILURE` guards.
- Rust slices make Eden's non-null read-buffer assertion implicit.

### Unintentional differences (to fix)
- An allocated `top_verify` object was never used; the existing owned level zero was initialized
  instead. The dead allocation is removed.
- Initialization previously recreated every verification owner and retained partial level state on
  error. It now preserves constructor ownership and finalizes initialized levels on failure.
- `Read` previously returned zero on an uninitialized object instead of enforcing Eden's
  initialization precondition. It now asserts the same state contract.
- `GetSize` previously replaced Eden's direct signed-to-unsigned `-1` bit pattern with zero while
  uninitialized. It now returns the direct `usize` cast, yielding `usize::MAX` as upstream does.
- Ruzu previously relied only on member destruction; `Drop` now invokes `finalize` explicitly,
  matching Eden's destructor lifecycle.

### Missing items
- None for construction ownership, level initialization order, failure cleanup, finalization order,
  read routing, size reporting, or the accessors defined in the matching upstream files.

### Binary layout verification
- PASS: the level-information structure is asserted to have Eden's size `0x18` and alignment `0x4`.
  Focused tests verify failed-initialization cleanup/retry and the uninitialized `GetSize` bit pattern.

## 2026-08-22 — `src/core/src/hle/kernel/k_page_table_base.rs` and `k_process_page_table.rs` vs Eden `src/core/hle/kernel/k_page_table_base.{h,cpp}` and `k_process_page_table.h`

### Intentional differences
- Rust forwards through the composed `KProcessPageTable::base` member; Eden inherits
  `KProcessPageTable` from `KPageTableBase`.

### Unintentional differences (to fix)
- `LockForCodeMemory` previously returned a physical address, accepted a caller-selected
  permission, and tested `FlagCanCodeAlias`. It now fills and opens the caller's page group,
  tests `FlagCanCodeMemory`, and installs `KernelReadWrite | NotMapped` exactly like Eden.
- `UnlockForCodeMemory` previously omitted the page-group identity check and used
  `FlagCanCodeAlias`. It now forwards the exact page group into `UnlockMemory` and restores the
  source under `FlagCanCodeMemory`.
- The process-page-table wrapper previously omitted both code-memory forwarding methods and the
  block-info-manager accessor needed to construct an owner-matched page group.

### Missing items
- None for the `LockForCodeMemory` and `UnlockForCodeMemory` dependency slice.

### Binary layout verification
- N/A: these methods operate on existing page groups and memory-block state; no serialized payload
  changed.

## 2026-08-22 — `src/core/src/hle/kernel/k_code_memory.rs` vs Eden `src/core/hle/kernel/k_code_memory.{h,cpp}`

### Intentional differences
- `Arc<ProcessLock>` represents Eden's explicitly opened owner-process reference, and the outer
  `Mutex<KCodeMemory>` used by the typed object registry represents `m_lock`.
- Methods receive an already locked mutable current/owner process where needed so Rust never
  recursively locks the non-reentrant process mutex.
- `Drop` invokes `Finalize` only for initialized objects; Eden's auto-object lifecycle invokes
  `Finalize` before destruction under the same precondition.

### Unintentional differences (to fix)
- The former implementation fabricated physical pages from the guest virtual address and never
  retained its owner. Initialization now obtains the owner page table's block-info manager, locks
  the real source mapping, clears every physical byte to `0xFF`, retains the owner, and records the
  source address and size in Eden's order.
- `Map`, `Unmap`, `MapToOwner`, and `UnmapFromOwner` previously only toggled booleans. They now map
  the retained page group with `CodeOut/UserReadWrite` or `GeneratedCode/UserRead{Execute}` and
  preserve Eden's size and duplicate-map validation.
- Finalization now conditionally unlocks the original source, closes and finalizes its page group,
  and releases its owner reference in upstream order.

### Missing items
- None for `KCodeMemory` initialization, mapping, accessors, and finalization behavior.

### Binary layout verification
- N/A: `KCodeMemory` is an internal ownership object rather than a raw guest payload. Focused tests
  verify the page count, physical `0xFF` fill, memory states, permissions, and lock restoration.

## 2026-08-22 — `src/core/src/hle/kernel/k_process.rs` vs Eden `src/core/hle/kernel/k_process.{h,cpp}` code-memory ownership

### Intentional differences
- Ruzu's generic handle table stores opaque object IDs, so `KProcess` retains a typed
  `Arc<Mutex<KCodeMemory>>` registry beside its existing typed kernel-object registries. Eden's
  handle table and global auto-object container retain typed intrusive pointers directly.
- Removing the final handle finalizes immediately only when no external Rust owner exists; an
  external `Arc` retains the object and its strong owner-process reference like Eden's `Open`.

### Unintentional differences (to fix)
- Code-memory handles previously had no corresponding typed object and process teardown could not
  finalize their source mappings. Registration, lookup, last-handle removal, and process-finalize
  cleanup now preserve that lifecycle.

### Missing items
- None for process-owned lookup and release of code-memory handle objects.

### Binary layout verification
- N/A: the registry is host-only ownership state and does not alter `KProcess` guest ABI data.

## 2026-08-22 — `src/core/src/hle/kernel/svc/svc_code_memory.rs` and `svc_dispatch.rs` vs Eden `src/core/hle/kernel/svc/svc_code_memory.cpp` and generated `svc.cpp`

### Intentional differences
- Rust heap allocation and `Arc` ownership replace Eden's slab `Create/Register/Open/Close`
  mechanics; object IDs still come from `KernelCore` and handle-table insertion remains the
  publication point.
- Raw operation values are converted with fallible `TryFrom` because transmuting an invalid guest
  integer into a Rust enum would be undefined behavior. Conversion occurs after basic validation
  and handle lookup, preserving Eden's error precedence.

### Unintentional differences (to fix)
- `CreateCodeMemory` previously inserted only a synthetic opaque ID and never initialized an
  object. It now creates, initializes, registers, and publishes the typed `KCodeMemory`, with
  failure cleanup before returning.
- `ControlCodeMemory` previously edited current-process permissions directly and never used the
  retained physical pages or owner. Every operation now validates the matching address space and
  delegates to the corresponding `KCodeMemory` method.
- Both AArch32 SVCs previously returned unconditional stub success, while AArch64 fell through the
  generic stub arm. Both dispatch tables now use Eden's generated register layouts, including the
  split 64-bit address and size in `ControlCodeMemory64From32`.

### Missing items
- None for code-memory SVC validation, object lookup, operation dispatch, or 32/64-bit argument
  marshalling.

### Binary layout verification
- PASS: focused dispatch coverage verifies the generated AArch32 and AArch64 input/output register
  positions; operation discriminants remain `0..=3` with unknown values rejected explicitly.

## 2026-08-22 — `src/core/src/hle/service/jit/jit_code_memory.rs` vs Eden `src/core/hle/service/jit/jit_code_memory.{h,cpp}`

### Intentional differences
- `Arc<Mutex<KCodeMemory>>` represents Eden's raw pointer plus explicit `Open`/`Close` reference;
  the mapping helper accepts the Rust process owner and random generator as references rather than
  the C++ kernel argument.
- Rust obtains the retained `KCodeMemory` owner before calling its mapping methods because the
  process-wide mutex is the Rust counterpart of the page-table and object locking used upstream.

### Unintentional differences (to fix)
- The former file exposed only zero-valued size/address fields and documented the real behavior as
  blocked. `Initialize` now samples page-aligned addresses across the process alias-code region,
  retries indefinitely only for `ResultInvalidMemoryRegion`, maps with the requested permission,
  and publishes all members only after success.
- `Finalize` now asserts the matching owner unmap, releases the retained code-memory reference, and
  clears its object member in Eden's order.

### Missing items
- None for `CodeMemory::Initialize`, `Finalize`, `GetSize`, or `GetAddress`.

### Binary layout verification
- N/A: this is a host-side ownership helper, not a raw guest payload. Focused coverage verifies the
  sampled address, generated-code state, execute permission, getters, and final unmap.

## 2026-08-22 — `src/core/src/hle/service/jit/jit_context.rs` vs Eden `src/core/hle/service/jit/jit_context.{h,cpp}`

### Intentional differences
- Rust shares the local-memory/range/helper state through `Arc<Mutex<_>>` because `rdynarmic`
  owns its boxed callback object; Eden stores callbacks and the JIT beside their parent and uses
  direct references.
- `JitContext::new` returns the local `rdynarmic` construction error instead of relying on a C++
  constructor that always succeeds. Checked arithmetic prevents a malformed address from wrapping
  during host slice bounds checks.
- Mapped ranges are retained as interval pairs rather than Boost ICL nodes. Membership has the same
  half-open-range result used by every memory access, and no operation depends on interval count.
- Rust clears the persistent `USER_DEFINED1` halt bit before each invocation; Eden's Dynarmic
  `HaltExecution` lifecycle performs the equivalent reset internally.
- `JitContext` has a narrow `Send` implementation because the Rust IPC interface requires
  `Send + Sync`. Its backend pointers target stable owned allocations, and `IJitEnvironment`
  serializes every JIT call through one mutex.

### Unintentional differences (to fix)
- Fixed: the Rust service constructed the architecture-merged `JitConfig` and reached A64 through
  `LegacyA64Callbacks`. It now implements the A64-owned callback trait with typed vectors and
  exceptions and constructs `A64UserConfig` directly.
- Fixed: the legacy literal disabled cycle counting and supplied its shared memory carrier's
  64-bit address-space defaults. Eden only assigns `callbacks` on a default A64 configuration,
  leaving cycle counting enabled and both unused address-space widths at 36; Rust now uses those
  exact A64 defaults. The callback's no-op `AddTicks` and maximum tick budget preserve Eden's
  effective unlimited plugin execution.

### Missing items
- None for the public `JITContext` interface or the private behavior in `JITContextImpl` and
  `DynarmicCallbacks64`.

### Binary layout verification
- PASS: ELF dynamic/RELA/RELR entries use the shared `repr(C)` definitions; helper bytes are the
  exact `svc #0; ret` sequence, stack/heap alignment is 16 bytes, and focused execution coverage
  verifies the ninth integer argument at `[SP]`.

## 2026-08-22 — `src/rdynarmic/src/jit_config.rs` and A32/A64 backend callback wiring vs Eden `src/core/hle/service/jit/jit_context.cpp::DynarmicCallbacks64`

### Intentional differences
- The Rust backend exposes `instruction_synchronization_barrier_raised` as a default no-op trait
  method and wires it for every JIT configuration. This avoids a JIT-service-only backend type while
  leaving existing callback implementations behaviorally unchanged.
- The flag corresponding to Dynarmic's top-level `UserConfig::hook_isb` lives in the matching
  architecture-owned Rust `A64UserConfig`; its default remains false.

### Unintentional differences (to fix)
- None.

### Missing items
- None for the instruction-synchronization callback required by `JITContext`.

### Binary layout verification
- N/A: this adds a host callback slot only. A focused A64 execution test verifies one ISB produces
  exactly one callback before the terminating SVC when `hook_isb` is enabled on the active host
  backend; the default-disabled behavior matches Eden's `UserConfig`.

## 2026-08-22 — `src/core/src/hle/service/jit/jit.rs` vs Eden `src/core/hle/service/jit/jit.{h,cpp}`

### Intentional differences
- Rust places the mutable environment members behind one `Mutex` because service callbacks receive
  `&self`; this preserves their upstream ownership and serializes the same per-object operations.
- Typed copy handles are resolved through the caller process's Rust object registries, and
  `KScopedAutoObject<KProcess>` is represented by a retained `Arc<ProcessLock>`.
- CMIF arguments are parsed explicitly. Output buffers are copied from guest memory before plugin
  execution and written back afterward because Rust's memory bridge is mutex-owned rather than a
  directly borrowed span.
- Rust implements the `std::mt19937_64` member locally with its exact default seed and output
  sequence. The standard library does not provide this engine.
- `JitContext` construction can report backend allocation/emitter errors. Those Rust-only failure
  paths finalize both already-mapped code ranges before returning `ResultUnknown`; Eden's Dynarmic
  constructor is effectively infallible here.
- Eden resolves `_fini` and `nnjitpluginKeeper` but never invokes them. Rust retains both fields and
  uses narrow `allow(dead_code)` annotations rather than deleting ABI-visible symbol ownership or
  inventing calls absent upstream.

### Unintentional differences (to fix)
- None.

### Missing items
- None for `JITU`, `IJitEnvironment`, their command tables, callbacks, configuration, or lifecycle.

### Binary layout verification
- PASS: focused tests verify `CodeRange` is 16 bytes/aligned to 8, `Struct32` is 32 bytes, and
  `JITConfiguration` is 80 bytes. Callback execution tests cover `GenerateCode`'s 13-argument ABI,
  `Control`, cleared output-range sizes, and preserved output-buffer contents.

## 2026-08-22 — `src/video_core/src/shader_environment.rs` vs Eden `src/video_core/shader_environment.{h,cpp}`

### Intentional differences
- Rust validates serialized environment counts and their complete byte size against the remaining
  cache file before allocating. It also uses fallible reservations, rejects empty pipeline entries,
  and requires exactly one environment for compute pipelines. Eden relies on trusted cache contents
  and throwing stream reads; the additional validation prevents a malformed or same-version legacy
  cache from aborting the process in Rust's infallible allocation path.
- Pipeline-loader callbacks return `std::io::Result<()>` so key-read failures reach the same outer
  invalid-cache cleanup that Eden obtains from `ifstream` exceptions.

### Unintentional differences (to fix)
- None in the reviewed serialization and disk-cache loading slice.

### Missing items
- None in the reviewed serialization and disk-cache loading slice.

### Binary layout verification
- PASS: the magic, cache version, field order, field widths, stage-specific payloads, and pipeline
  key placement remain unchanged. Round-trip and malformed-cache tests cover valid compute entries,
  truncated data, invalid discriminants, empty entries, oversized environment counts, and oversized
  shader payloads.

## 2026-08-22 — `src/video_core/src/renderer_vulkan/pipeline_cache.rs` vs Eden `src/video_core/renderer_vulkan/vk_pipeline_cache.{h,cpp}`

### Intentional differences
- Rust key readers return `std::io::Result` instead of relying on `ifstream::failbit` exceptions.
  Dynamic-feature incompatibility remains a skipped valid entry and does not invalidate the cache.

### Unintentional differences (to fix)
- Cached compute and graphics key read failures were previously logged and swallowed, allowing a
  desynchronized reader to continue. They now propagate through `load_pipelines`, matching Eden's
  whole-file deletion on a failed key read.

### Missing items
- None in the reviewed disk-resource key-loading slice.

### Binary layout verification
- PASS: `ComputePipelineCacheKey` and `GraphicsPipelineKey` serialization is unchanged; only failure
  propagation after reading those existing layouts changed.

## 2026-08-22 — `src/video_core/src/renderer_opengl/gl_shader_cache.rs` vs Eden `src/video_core/renderer_opengl/gl_shader_cache.{h,cpp}`

### Intentional differences
- Rust key readers report `std::io::Result` explicitly instead of using throwing `ifstream` reads.

### Unintentional differences (to fix)
- Cached compute and graphics key read failures were previously logged and swallowed. They now
  reach `load_pipelines` and delete the invalid cache as Eden's stream exception path does.

### Missing items
- None in the reviewed disk-resource key-loading slice.

### Binary layout verification
- PASS: OpenGL pipeline key bytes and their placement after serialized environments are unchanged.

## 2026-08-22 — `src/core/src/debugger/debugger_interface.rs` vs Eden `src/core/debugger/debugger_interface.h`

### Intentional differences
- Rust passes retained `Arc<KThreadLock>` values instead of Eden's non-owning `KThread*`; this keeps
  the thread alive across the channel from a CPU thread to the debugger connection thread.
- The backend reference is an explicit argument to frontend callbacks because Rust traits cannot
  retain the same self-referential backend reference as Eden's `DebuggerFrontend` constructor.

### Unintentional differences (to fix)
- The former interface represented threads as opaque integers and watchpoints as split primitive
  fields. It now carries the matching kernel thread and `DebugWatchpoint` owners required by Eden's
  frontend contract.

### Missing items
- None in the debugger frontend/backend action and callback interface.

### Binary layout verification
- N/A: these are host-only Rust traits and retained kernel-object references, not guest payloads.

## 2026-08-22 — `src/core/src/debugger/debugger.rs` vs Eden `src/core/debugger/debugger.{h,cpp}`

### Intentional differences
- Rust's standard `TcpListener`, `TcpStream`, channel and owned thread replace Boost.Asio's acceptor,
  socket and asynchronous signal pipe. They retain the same single server thread, 4096-byte reads,
  replacement of an existing connection, and synchronous frontend callbacks.
- `Arc<ProcessLock>`/`Arc<KThreadLock>` replace Eden's scoped intrusive kernel-object references.
  Process locking supplies the matching thread-list lifetime while individual scheduler-aware Rust
  thread methods perform suspend/resume transitions.
- A shutdown request is handed to Ruzu's boot controller through an atomic flag instead of spawning
  a detached call to `System::Exit`; the Rust `System` is owned by that controller thread and cannot
  safely be mutably exited by the debugger thread.
- An empty process thread list leaves the active thread unset instead of dereferencing Eden's
  `threads.front()` precondition. A connected debugger still pauses every thread that exists.

### Unintentional differences (to fix)
- The previous file contained no server, connection state, process/thread ownership, signal path,
  pause/resume behavior, or debugger thread lifecycle. Those responsibilities now live in their
  matching module and execute in Eden's connection/action order.

### Missing items
- CPU-side step completion, breakpoint notification and watchpoint generation are prerequisites in
  their own upstream-owned modules; they are recorded in `PORTING_STATE.md` before the GDB command
  dispatcher is resumed.

### Binary layout verification
- N/A: the connection and synchronization state is host-only. Socket regression tests verify bind
  failure, deterministic thread shutdown and real packet routing.

## 2026-08-22 — `src/core/src/core.rs` debugger ownership vs Eden `src/core/core.{h,cpp}`

### Intentional differences
- Ruzu exposes notification forwarding methods on `System` because its CPU owners cannot borrow the
  debugger field directly while retaining a kernel thread `Arc`; Eden exposes `GetDebugger()`.
- The debugger-triggered exit flag is reset explicitly at initialization and after debugger
  destruction because it replaces Eden's detached `System::Exit()` call.

### Unintentional differences (to fix)
- `System` previously had no debugger owner. It now initializes the configured server, forwards
  thread notifications, sends shutdown before teardown, and destroys the debugger immediately after
  CPU-manager shutdown and before kernel shutdown, matching Eden's lifecycle ordering.

### Missing items
- None in `System`'s debugger ownership and initialization/detachment lifecycle.

### Binary layout verification
- N/A: the new members are host runtime state and do not alter any raw guest structure.

## 2026-08-22 — `src/ruzu/src/boot.rs` debugger lifecycle vs Eden `src/qt_common/render/emu_thread.cpp`

### Intentional differences
- Ruzu's non-Qt boot controller polls the atomic debugger shutdown request in its existing command
  loop; Eden's GDB backend invokes `System::Exit()` from a detached thread.

### Unintentional differences (to fix)
- The frontend previously ignored `use_gdbstub`. It now initializes the debugger after GPU/CPU
  readiness, observes debugger-requested exit, and detaches it before pausing and shutting down the
  application process in the same lifecycle positions as Eden.

### Missing items
- None in the frontend-owned debugger initialization and detachment slice.

### Binary layout verification
- N/A: this is frontend control flow only.

## 2026-08-22 — `src/core/src/debugger/gdbstub.rs` connection callback slice vs Eden `src/core/debugger/gdbstub.{h,cpp}`

### Intentional differences
- Rust returns explicit packet-completeness booleans from `process_data` so split TCP frames remain
  buffered without the recursive asynchronous-read structure used by Boost.Asio.

### Unintentional differences (to fix)
- Stop and watchpoint callbacks previously fabricated a default register context and only logged
  the reply. They now read the retained active thread and send the matching remote status packet.
- Packet acknowledgement, checksum rejection, escaping, replies and the initial supported-feature
  negotiation now use the live backend instead of remaining inert helpers.

### Missing items
- The complete register, memory, thread, query, breakpoint/watchpoint and `vCont` dispatcher remains
  interrupted behind the CPU stop/step and Dynarmic watchpoint prerequisites recorded in
  `PORTING_STATE.md`.

### Binary layout verification
- N/A for this framing slice; register byte order and architecture-specific XML remain owned by
  `gdbstub_arch.rs` and will be verified with the resumed command dispatcher.

## 2026-08-22 — `src/core/src/hle/kernel/physical_core.rs` vs Eden `src/core/hle/kernel/physical_core.{h,cpp}` debugger halt slice

### Intentional differences
- Ruzu's host-fiber dispatcher retains the `Arc<KThreadLock>` and JIT pointer in `cpu_manager.rs`,
  so that coordinator calls narrow `PhysicalCore` methods for the upstream-owned execution and halt
  decisions rather than holding the Rust thread mutex across guest execution.
- `exit_running` receives the retained thread owner only when debugging is enabled; it captures the
  JIT context before `UnlockThread`, matching Eden's `ExitContext` order without dereferencing the
  raw inner `KThread` outside its mutex.
- A data abort without a retained watchpoint is logged instead of dereferencing a null pointer. Eden
  relies on the invariant that only a matched debugger watchpoint emits `DataAbort`.

### Unintentional differences (to fix)
- Runtime execution previously ignored `StepPending` and always called `RunThread`; a successful
  step could also be misclassified as an SVC. It now calls `StepThread`, records `StepPerformed`,
  gives the step priority over simultaneous halt bits, and reports/suspends it when rescheduled.
- Breakpoint and prefetch-abort paths previously suspended without notifying the live debugger and
  did not refresh the saved thread context after rewinding. The matching owner now performs Eden's
  rewind, context save, notification and suspension ordering.

### Missing items
- A32/A64 Dynarmic callbacks still need to produce and retain matched watchpoints; that prerequisite
  is recorded in `PORTING_STATE.md` and is required before the data-abort path is complete.

### Binary layout verification
- N/A: this slice changes host execution ordering and retained thread state only.

## 2026-08-22 — `src/core/src/cpu_manager.rs` delegation vs Eden `src/core/hle/kernel/physical_core.cpp`

### Intentional differences
- Ruzu's CPU manager owns the fiber boundary and cached per-core JIT pointers. It therefore invokes
  the corresponding `PhysicalCore` decisions before/after the JIT call; the behavioral logic and
  ordering remain in `physical_core.rs`, matching upstream ownership as closely as the fiber model
  permits.

### Unintentional differences (to fix)
- The coordinator formerly classified breakpoint/data/prefetch halts itself and omitted the step
  state and debugger notifications. It now delegates those decisions and reschedules only after
  `PhysicalCore` has requested the upstream debug suspension.

### Missing items
- None in the CPU-manager delegation for the reviewed debugger halt slice.

### Binary layout verification
- N/A: no serialized or guest-visible layout changes.

## 2026-08-22 — `src/core/src/memory/memory.rs` vs Eden `src/core/memory.{h,cpp}` debugger-page marking

### Intentional differences
- Rust receives the process address as its underlying `u64`; the page-table bridge already uses raw
  virtual addresses throughout, while preserving Eden's address-space validation and page walk.

### Unintentional differences (to fix)
- `Memory::MarkRegionDebug` was absent. Watchpoint pages now lose fastmem access, transition from
  `Memory` to `DebugMemory`, and recover their biased host pointer when the last debug reference is
  removed, in Eden's protection-before-page-transition order.

### Missing items
- None for `MarkRegionDebug`.

### Binary layout verification
- N/A: this changes page-table entry state but introduces no serialized structure.

## 2026-08-22 — `src/core/src/hle/kernel/k_process.rs` vs Eden `src/core/hle/kernel/k_process.{h,cpp}` watchpoint ownership

### Intentional differences
- Rust's optional `Arc<Mutex<Memory>>` replaces Eden's directly owned `Memory`; initialized runtime
  processes always have it, while isolated `KProcess::new()` tests can still exercise table and
  reference-count behavior without a system memory owner.

### Unintentional differences (to fix)
- `DebugWatchpoint` stored its type as an untyped byte and insert/remove changed only the table.
  The field now uses the owning bitflag type, and both operations apply Eden's per-page reference
  counting and `MarkRegionDebug` calls for overlapping watchpoints.

### Missing items
- None for the reviewed watchpoint table and page-reference slice.

### Binary layout verification
- PASS: replacing the raw `u8` with a `u8`-backed bitflag preserves the field's size and alignment;
  focused assertions verify the 24-byte size and 8-byte alignment of the host structure.

## 2026-08-22 — `src/core/src/arm/arm_interface.rs` vs Eden `src/core/arm/arm_interface.{h,cpp}` watchpoints

### Intentional differences
- Rust JIT callbacks are moved owners rather than C++ objects retaining a parent reference. The
  process-array pointer therefore lives in a shared atomic slot, and a match is copied out instead
  of returning a reference whose lifetime cannot cross the callback mutex boundary.

### Unintentional differences (to fix)
- The interface previously duplicated the kernel watchpoint type with primitive addresses and did
  not expose `SetWatchpointArray` through every backend. It now consumes the `k_process.rs` owner and
  applies Eden's half-open range and access-bit matching literally.

### Missing items
- None for the reviewed watchpoint-array and matching slice.

### Binary layout verification
- N/A: the shared atomic pointer is host callback state; the process-owned watchpoint layout is
  verified in `k_process.rs`.

## 2026-08-22 — `src/core/src/arm/dynarmic/arm_dynarmic_32.rs` vs Eden `src/core/arm/dynarmic/arm_dynarmic_32.{h,cpp}` watchpoint callbacks

### Intentional differences
- The callback shares halted-watchpoint state with its Rust JIT owner through `Arc<Mutex<_>>` and
  invokes the existing Rust JIT halt bridge. Rust-only exclusive-read/128-bit callback extensions
  perform the same access check before their underlying memory operation.

### Unintentional differences (to fix)
- `CheckMemoryAccess` previously returned unconditionally and the halt translation discarded
  Dynarmic's memory-abort bit. Address validation, read/write matching, retained watchpoint state,
  prefetch/data-abort halts and exclusive-access ordering now match Eden.

### Missing items
- None for the reviewed A32 memory-access/watchpoint slice.

### Binary layout verification
- N/A: callback and halt state are host-only.

## 2026-08-22 — `src/core/src/arm/dynarmic/arm_dynarmic_64.rs` vs Eden `src/core/arm/dynarmic/arm_dynarmic_64.{h,cpp}` watchpoint callbacks

### Intentional differences
- The moved Rust callback uses a shared watchpoint-array pointer and mutex-protected copied match in
  place of Eden's parent reference and raw matched pointer; ownership and halt timing are preserved.

### Unintentional differences (to fix)
- `CheckMemoryAccess` previously returned unconditionally. It now derives Eden's exact enable state,
  validates addresses, distinguishes read/write watchpoints, retains the match and prevents writes
  after requesting the corresponding prefetch/data-abort halt.

### Missing items
- None for the reviewed A64 memory-access/watchpoint slice.

### Binary layout verification
- N/A: callback and halt state are host-only.

## 2026-08-22 — `src/core/src/hle/kernel/physical_core.rs` vs Eden `src/core/hle/kernel/physical_core.cpp` watchpoint loading

### Intentional differences
- Rust passes the address of the stable process-owned array while holding the process lock; Eden
  obtains the same array through `GetWatchpoints()`.

### Unintentional differences (to fix)
- `LoadContext` previously omitted `SetWatchpointArray`, and the data-abort path converted from a
  duplicate ARM watchpoint representation. It now wires the process owner after context/TLS setup
  and forwards that exact typed watchpoint to the debugger.

### Missing items
- None for the reviewed load-context/data-abort watchpoint slice.

### Binary layout verification
- N/A: this is host lifecycle wiring.

## 2026-08-22 — `src/core/src/debugger/gdbstub.rs` vs Eden `src/core/debugger/gdbstub.cpp` typed watchpoint reply

### Intentional differences
- None in the reviewed watchpoint classification.

### Unintentional differences (to fix)
- The reply path previously reconstructed a watchpoint type from an untyped byte. It now matches the
  owning kernel bitflag directly when selecting `rwatch`, `watch`, or `awatch`.

### Missing items
- The command dispatcher remains the next warning-driven slice recorded in `PORTING_STATE.md`.

### Binary layout verification
- N/A: this formats a remote-protocol reply and introduces no payload structure.

## 2026-08-22 — `src/core/src/arm/debug.rs` vs Eden `src/core/arm/debug.{h,cpp}` module discovery

### Intentional differences
- Rust page-table queries return `Option` and are asserted with `expect` where Eden uses `R_ASSERT`.
  Isolated tests can read the process-memory fallback when the runtime `Memory` bridge is absent.
- Module path bytes are converted lossily to Rust UTF-8 strings; valid UTF-8 and ASCII paths retain
  Eden's exact basename and declared-length behavior.

### Unintentional differences (to fix)
- `FindModules`, `GetModuleEnd` and the no-module entrypoint fallback previously returned empty or
  placeholder values. They now reproduce Eden's complete region walk, state/permission checks,
  module-path record parsing, three-segment end calculation and code-region fallback.
- The file previously invented opaque process/thread types and duplicated an empty module walker for
  symbolication. It now uses the owning kernel types and the single upstream-equivalent function.

### Missing items
- Existing backtrace symbol names are still not resolved/demangled because Ruzu has no counterpart
  for Eden `common/demangle.{h,cpp}`; this is independent of the GDB module-enumeration prerequisite.

### Binary layout verification
- PASS: the module path record is decoded as upstream's `u32`, `s32`, and 0x200-byte path in its
  exact 0x208-byte little-endian layout; focused coverage reads a real record from process memory.

## 2026-08-22 — `src/core/src/debugger/gdbstub.rs` vs Eden `src/core/debugger/gdbstub.{h,cpp}` command dispatcher

### Intentional differences
- Eden retains its backend, `System` and process as raw references. Rust receives the backend per
  callback, retains the process as `Arc<ProcessLock>`, and uses `Arc<KThreadLock>` for selected and
  resumed threads; pointer identity preserves Eden's `vCont` matching semantics.
- Eden's synchronous `ProcessData` reads from the socket until a packet is complete. Ruzu's
  asynchronous connection owner delivers fragments through `ClientData`, so an incomplete packet
  remains buffered until the next callback instead of blocking the debugger thread.
- Runtime Rust processes expose `Memory` through `Option<Arc<Mutex<_>>>`; missing memory and missing
  active threads return `E01` instead of dereferencing an invalid owner. Valid runtime paths retain
  Eden's command behavior and ordering.
- Rust rejects a malformed `M` packet whose decoded byte vector is shorter than its declared size,
  avoiding the out-of-bounds source read possible in the C++ expression while preserving every
  valid packet.

### Unintentional differences (to fix)
- The stub previously implemented only stop status, `qSupported`, kill, continue and step. It now
  ports Eden's complete register/memory dispatch, instruction restoration, software breakpoint and
  watchpoint lifecycle, query transfers, `vCont`, monitor output, pagination and escaping.
- Breakpoint removal now writes the saved instruction, invalidates the instruction cache, and only
  then erases the saved entry, matching Eden's lifecycle order.

### Missing items
- None in the reviewed GDB stub command and query surface.

### Binary layout verification
- N/A: the GDB remote protocol serializes explicit text and hexadecimal byte streams rather than
  raw host structures.

## 2026-08-23 — `src/shader_recompiler/src/frontend/mod.rs` tests vs Eden `src/shader_recompiler/frontend/maxwell/{decode.cpp,maxwell.inc}`

### Intentional differences
- Ruzu keeps native Rust decoder smoke tests in the module root; Eden's C++ test tree is excluded
  from the port, while the tested instruction words come directly from Eden's Maxwell table.

### Unintentional differences (to fix)
- The NOP and register-IADD instruction builders were unused, and the NOP test only checked that
  decoding did not panic. Both encodings now assert Eden's exact decoded opcode.

### Missing items
- None for the two reviewed decoder encodings.

### Binary layout verification
- N/A: the tests pass explicit 64-bit Maxwell instruction words to the decoder.

## 2026-08-23 — `src/rdynarmic/src/ir/opcode.rs` vs Eden `src/dynarmic/src/dynarmic/ir/{opcodes.inc,opcodes.h}`

### Intentional differences
- Rust retains 26 internal or decomposed opcodes that are not present in Eden's opcode enum. Their
  ownership and necessity remain active audit items; they are not treated as upstream parity.

### Unintentional differences (to fix)
- Seventy-three existing opcodes used semantic Rust renames such as `RotateRight32`,
  `VectorMaxSigned8`, and `PackedAbsDiffSumS8`. Their enum variants, metadata, emit dispatch,
  frontend calls, optimization matches, and tests now use Eden's exact opcode names.

### Missing items
- Fifteen Eden opcodes remain absent: seven `VectorBroadcastElement*`, four `VectorReduceAdd*`, and
  four `Vector{Signed,Unsigned}Multiply*` forms. Their existing composite or differently-owned Rust
  behavior must be replaced in prerequisite-backed slices.

### Binary layout verification
- PASS for this naming-only slice: enum cardinality and discriminants are unchanged; no serialized
  or ABI-visible structure changed.

## 2026-08-23 — rdynarmic vector broadcast-element IR/backends vs Eden Dynarmic

Rust files: `src/rdynarmic/src/ir/{opcode,emitter}.rs`,
`src/rdynarmic/src/backend/x64/{emit,emit_x64_vector}.rs`, and
`src/rdynarmic/src/backend/arm64/{emit_arm64,emit_arm64_vector}.rs`.

Eden files: `src/dynarmic/src/dynarmic/ir/{opcodes.inc,ir_emitter.h}`,
`src/dynarmic/src/dynarmic/backend/x64/emit_x64_vector.cpp`, and
`src/dynarmic/src/dynarmic/backend/arm64/emit_arm64_vector.cpp`.

### Intentional differences
- Rust dispatches enum variants through explicit `match` arms and passes `InstRef` into its register
  allocators; Eden uses generated x64 member dispatch and arm64 `EmitIR` template specializations.
  The opcode ownership, argument order, validation, emitted host instructions and value-definition
  order are preserved.
- The arm64 Rust helper takes `size` and `q` as runtime values; it is the direct counterpart of
  Eden's templated `EmitBroadcastElement<size>` helper and emits the same `DUP` encoding.

### Unintentional differences (to fix)
- The newly ported x64 methods now have the correct upstream owner, but older methods from Eden's
  `emit_x64_vector.cpp` are still distributed across several legacy `emit_vector_*.rs` files. That
  broader ownership migration remains part of the directory audit.

### Missing items
- Eight Eden IR opcodes remain absent after this slice: `VectorReduceAdd8/16/32/64` and
  `VectorSignedMultiply16/32` / `VectorUnsignedMultiply16/32`.

### Binary layout verification
- PASS: all seven opcodes use Eden's exact `U128(U128, U8)` metadata. No raw-memory payload or ABI
  structure is introduced; x64 instruction bytes and arm64 `DUP` encodings have focused coverage.

## 2026-08-23 — `src/rdynarmic/src/backend/arm64/inst.rs` vs Oaknut instructions used by Eden vector reductions

### Intentional differences
- Ruzu encodes AArch64 instructions directly as `u32` words rather than invoking Oaknut's
  overloaded `ADDV` and `ADDP` methods.

### Unintentional differences (to fix)
- The direct encoder previously lacked the scalar across-lane `ADDV` and pairwise `ADDP Dd,
  Vn.2D` forms required by Eden's `VectorReduceAdd*` backend. Both encodings are now present in the
  arm64 instruction owner.

### Missing items
- The four `VectorReduceAdd*` IR and backend paths remain paused until this prerequisite is
  independently committed.

### Binary layout verification
- PASS: GNU AArch64 binutils independently assembled the covered instructions as `0x4e31b820`,
  `0x4e71b862`, `0x4eb1b8a4`, and `0x5ef1b8e6`; focused Rust assertions require the same words.

## 2026-08-23 — rdynarmic vector reduce-add IR/frontend/backends vs Eden Dynarmic

Rust files: `src/rdynarmic/src/ir/{opcode,emitter}.rs`,
`src/rdynarmic/src/frontend/a64/translate/simd_across_lanes.rs`,
`src/rdynarmic/src/backend/x64/{emit,emit_x64_vector}.rs`, and
`src/rdynarmic/src/backend/arm64/{emit_arm64,emit_arm64_vector}.rs`.

Eden files: `src/dynarmic/src/dynarmic/ir/{opcodes.inc,ir_emitter.h}`,
`src/dynarmic/src/dynarmic/frontend/A64/translate/impl/simd_across_lanes.cpp`,
`src/dynarmic/src/dynarmic/backend/x64/emit_x64_vector.cpp`, and
`src/dynarmic/src/dynarmic/backend/arm64/emit_arm64_vector.cpp`.

### Intentional differences
- Rust uses explicit opcode matches and direct host-instruction APIs. Eden uses generated x64
  member dispatch and arm64 template specializations; operand realization and value-definition
  ordering remain the same.
- The arm64 Rust `emit_reduce` helper selects its element size at runtime; it directly corresponds
  to Eden's `EmitReduce<size>` template and selects the same scalar instruction for every size.

### Unintentional differences (to fix)
- ADDV previously expanded every lane into scalar IR additions and truncations. It now emits Eden's
  single dedicated reduction opcode after the same reserved-value check and operand read, removing
  both the behavioral and ownership divergence.

### Missing items
- Four Eden IR opcodes remain absent: `VectorSignedMultiply16/32` and
  `VectorUnsignedMultiply16/32`.

### Binary layout verification
- PASS: all four opcodes use Eden's exact `U128(U128)` metadata. The x64 instruction sequences are
  covered for SSSE3 and SSE2, and arm64 encodings were independently verified in the prerequisite
  slice; no raw guest-visible payload is introduced.

## 2026-08-23 — rdynarmic upper/lower multi-result pseudo-operations vs Eden Dynarmic

Rust files: `src/rdynarmic/src/ir/emitter.rs`,
`src/rdynarmic/src/backend/x64/{emit_x64,reg_alloc}.rs`, and
`src/rdynarmic/src/backend/arm64/emit_arm64.rs`.

Eden files: `src/dynarmic/src/dynarmic/ir/ir_emitter.h`,
`src/dynarmic/src/dynarmic/backend/x64/{emit_x64,reg_alloc}.{h,cpp}`, and
`src/dynarmic/src/dynarmic/backend/arm64/{emit_arm64,reg_alloc}.{h,cpp}`.

### Intentional differences
- Rust's arena-backed emitter exposes named `get_upper_from_op` and `get_lower_from_op` methods
  around the common pseudo-operation linker; Eden constructs the same two typed `Inst<U128>`
  values directly inside its multi-result emitter.
- Eden asserts on an ARM64 pseudo-result that its producer failed to define. The Rust backend
  returns its existing diagnostic `Err` for the same invariant violation, while preserving the
  argument accounting before the check.

### Unintentional differences (to fix)
- The x64 handlers previously extracted one 64-bit half from an ordinary `U128` and incorrectly
  defined the nominally 128-bit result in a GPR. They now only register the complete `U128` value
  already defined by the multi-result producer, exactly as Eden does.
- The handlers previously lived in `emit_a64.rs`; they now live in the matching
  `backend/x64/emit_x64.rs` owner corresponding to Eden's `emit_x64.cpp`.

### Missing items
- The interrupted `VectorSignedMultiply16/32` and `VectorUnsignedMultiply16/32` producer slice
  remains to be ported now that this prerequisite is available.
- Other generic x64 methods from Eden `emit_x64.cpp` are still distributed across older Ruzu
  modules and remain part of the structural ownership audit.

### Binary layout verification
- N/A: the change restores SSA pseudo-result ownership and register-allocation bookkeeping; it
  introduces no serialized payload or ABI-visible data structure.

## 2026-08-23 — rdynarmic vector multi-result multiply IR/backends vs Eden Dynarmic

Rust files: `src/rdynarmic/src/ir/{opcode,emitter}.rs`,
`src/rdynarmic/src/backend/x64/{emit,emit_x64_vector}.rs`, and
`src/rdynarmic/src/backend/arm64/{emit_arm64,emit_arm64_vector}.rs`.

Eden files: `src/dynarmic/src/dynarmic/ir/{opcodes.inc,ir_emitter.h}`,
`src/dynarmic/src/dynarmic/backend/x64/emit_x64_vector.cpp`, and
`src/dynarmic/src/dynarmic/backend/arm64/emit_arm64_vector.cpp`.

### Intentional differences
- Rust represents Eden's nullable associated pseudo-operation pointers as `Option<InstRef>` and
  dispatches the same per-opcode emitters through explicit `match` arms. Producer ownership,
  result-sensitive branches, host instruction ordering, and value-definition ordering are retained.
- Rust reports an invalid element size with `panic!`; this is the direct counterpart of Eden's
  `UNREACHABLE()` branch. Like Eden, the public emitter constructor exists only for signed
  multiplication even though both signed and unsigned producer backends are present.

### Unintentional differences (to fix)
- Four dead `Vector{Signed,Unsigned}MultiplyLong{16,32}` operations had no Eden counterpart and
  returned one widened vector instead of Eden's upper/lower multi-result contract. They and their
  fallback implementations are removed; the exact four Eden producer operations replace them.
- The pre-existing broad binary-vector metadata arm still assigns `U128(U128, U8)` to numerous
  operations that Eden declares as `U128(U128, U128)`. The four newly reviewed multiply producers
  are outside that arm and have their exact `Void(U128, U128)` metadata; the broader correction is
  a separate audit slice.

### Missing items
- None for the four reviewed multiply producers. The opcode inventory has no missing Eden names,
  while 22 Ruzu-only operations still require ownership and behavior review.

### Binary layout verification
- PASS: all four producer opcodes use Eden's exact `Void(U128, U128)` signature and both pseudo
  results remain full `U128` values. No raw-memory payload or guest ABI structure is introduced.

## 2026-08-23 — `src/rdynarmic/src/ir/opcode.rs` vs Eden `ir/opcodes.inc`

### Intentional differences
- Rust stores opcode metadata in an explicit `match`, whereas Eden generates it from
  `opcodes.inc`. The audit tool now expands every Rust grouped arm and compares all 725 shared
  return/argument signatures to retain line-item traceability.

### Unintentional differences (to fix)
- The audit found 126 shared signature mismatches. This slice fixes all 119 vector mismatches and
  all four CRC mismatches. `A32CoprocLoadWords`, `A32CoprocStoreWords`, and
  `A64DataCacheOperationRaised` remain intentionally stopped on their recorded behavioral
  prerequisites rather than receiving metadata-only changes.

### Missing items
- A32 coprocessor load/store construction and backend dispatch must be ported before removing their
  extra `U1` metadata argument.
- A64 data-cache callbacks and non-hooked lowering must be ported before adding the missing location
  descriptor argument.
- The 22 Ruzu-only opcode variants still require individual ownership and behavior review.

### Binary layout verification
- PASS for the reviewed metadata: vector operands retain full `U128` values and CRC8/16 retain the
  upstream `U32` bit pattern until the backend selects the instruction width. No serialized payload
  or ABI-visible structure changed.

## 2026-08-23 — rdynarmic `interface/a32/coprocessor*.rs` vs Eden `interface/A32/coprocessor*.h`

### Intentional differences
- Rust expresses Eden's abstract class as a `Send + Sync` trait behind `Arc`, and its
  `std::variant` actions as enums. `Option<Callback>` and explicit `CoprocessorException` variants
  preserve the same compile-time decisions.
- Callback functions use the platform C ABI and an optional raw `c_void` user pointer; this is the
  Rust FFI counterpart of Eden's native function pointer and `std::optional<void*>`.

### Unintentional differences (to fix)
- The existing x64 and arm64 backend emitters still implement a hard-coded CP15 subset. They must
  consume these interface actions through the configured 16-entry registry before the old paths
  are removed.

### Missing items
- `interface/A32/config.h::UserConfig::coprocessors` is not yet wired into `JitConfig` and both
  backend emit configurations.
- Eden's seven compile-time coprocessor action dispatchers are the next prerequisite slice.

### Binary layout verification
- PASS: `CoprocReg` is `repr(u8)`, contiguous from C0 through C15, and focused tests verify its
  size, alignment, discriminants, and conversion. Callback/action enums are host-only interfaces
  and are never raw-copied into guest-visible storage.

## 2026-08-23 — rdynarmic A32 coprocessor registry vs Eden `interface/A32/config.h`

### Intentional differences
- Ruzu's pre-existing combined `JitConfig` serves both A32 and A64, so the A32 registry is stored
  there and ignored by A64. Its type and owner are defined in the matching
  `interface/a32/config.rs` module.
- Rust initializes `[Option<Arc<dyn Coprocessor>>; 16]` through a named constructor; this is the
  direct ownership counterpart of Eden's zero-initialized `std::array<std::shared_ptr<...>, 16>`.

### Unintentional differences (to fix)
- The registry is now present at the public configuration boundary, but has not yet been forwarded
  to x64/arm64 emit configuration or consulted by the seven coprocessor emitters.

### Missing items
- Backend action dispatch and the core CP15 implementation must populate and consume slot 15 before
  replacing the current hard-coded CP15 subset.

### Binary layout verification
- N/A: the registry contains host-owned `Arc` trait objects and is never serialized or exposed to
  guest memory. Focused tests verify exactly 16 empty default slots.

## 2026-08-23 — rdynarmic A32 coprocessor frontend/IR vs Eden Dynarmic

Rust files: `src/rdynarmic/src/frontend/a32/{decoder,decoder_thumb32}.rs`,
`src/rdynarmic/src/frontend/a32/translate/{coprocessor,thumb32_coprocessor,vfp,mod,thumb32}.rs`,
and `src/rdynarmic/src/ir/{a32_emitter,opcode}.rs`.

Eden files: `src/dynarmic/src/dynarmic/frontend/A32/decoder/{arm,thumb32,vfp}.inc`,
`src/dynarmic/src/dynarmic/frontend/A32/translate/impl/{coprocessor,thumb32_coprocessor,vfp}.cpp`,
`src/dynarmic/src/dynarmic/frontend/A32/translate/translate_thumb.cpp`, and
`src/dynarmic/src/dynarmic/ir/{a32_ir_emitter.h,a32_ir_emitter.cpp,opcodes.inc}`.

### Intentional differences
- Rust represents Eden's generated decoder tables with explicit masked matches. The same VFP,
  ASIMD, unconditional ARM, and generic coprocessor priority is preserved; focused overlap tests
  cover the VFP-before-generic and VFP-before-Thumb32 boundaries.
- Rust constructs each fixed-size coprocessor metadata record with `u64::from_le_bytes` instead of
  Eden's `std::array<u8, 8>` plus `memcpy`. Field order, zeroed reserved bytes, and the resulting
  `U64` bit pattern are identical.
- Eden's `UndefinedInstruction` and `UnpredictableInstruction` visitor helpers map to the existing
  Rust translation helpers, which emit the same exception kind through Rust's IR API.

### Unintentional differences (to fix)
- Coprocessor metadata was previously packed in the decoder, placed `opc2` in the wrong byte, and
  could not represent CDP's `CRd`. Construction now belongs to `A32IREmitter`, with the seven exact
  upstream argument lists and byte layouts.
- ARM LDC/STC and all seven unconditional/Thumb32 coprocessor forms were previously absent or
  decoded as `Unknown`. Their validation, address calculation, option/writeback handling, and IR
  emission now follow the corresponding Eden visitors.
- VMSR/VMRS and the four two-word VFP moves were previously inferred inside the generic
  coprocessor owner, including non-upstream FPEXC behavior. Their exact decoder patterns and
  implementations now live in `vfp.rs`; generic CP10/CP11 forms take Eden's undefined path.
- `A32CoprocLoadWords` and `A32CoprocStoreWords` previously carried a separate `U1` argument.
  Their complete transfer metadata now lives in the packed `U64`, leaving Eden's exact
  `Void(U64, U32)` signature.

### Missing items
- None in the reviewed ARM/Thumb coprocessor frontend and A32 IR-emitter slice.

### Binary layout verification
- PASS: focused tests assert every byte of Eden's seven eight-byte coprocessor metadata layouts,
  including CDP `CRd`, one-word `opc2`, load/store option fields, and zeroed reserved bytes.

## 2026-08-23 — rdynarmic A32 coprocessor backends vs Eden Dynarmic

Rust files: `src/rdynarmic/src/backend/x64/{a32_emit_a32,emit_context,jit_state}.rs`,
`src/rdynarmic/src/backend/arm64/{emit_arm64,emit_arm64_a32_coprocessor,a32_address_space,a32_interface}.rs`,
and `src/rdynarmic/src/jit.rs`.

Eden files: `src/dynarmic/src/dynarmic/backend/x64/a32_emit_x64.cpp`,
`src/dynarmic/src/dynarmic/backend/x64/emit_x64.h`,
`src/dynarmic/src/dynarmic/backend/arm64/emit_arm64_a32_coprocessor.cpp`,
`src/dynarmic/src/dynarmic/backend/arm64/emit_arm64.h`, and
`src/dynarmic/src/dynarmic/interface/A32/a32.{h,cpp}`.

### Intentional differences
- Rust stores Eden's shared coprocessor objects as `Arc<dyn Coprocessor>` and forwards a cloned
  16-entry array into backend configuration. This preserves shared lifetime and the exact slot
  lookup while replacing C++ `shared_ptr` ownership.
- Rust's x64 and arm64 callback helpers use the existing backend ABI/register-allocation APIs
  rather than Eden's templated `ABI_CallFunction` helpers. They preserve the same optional user
  argument, result destination, input ordering, and register-allocation accounting.
- Missing coprocessors and compile-time exception actions use `unreachable!` at emission time,
  corresponding to Eden's currently unreachable `EmitCoprocessorException` implementation.

### Unintentional differences (to fix)
- Both backends previously hard-coded a small CP15 subset and silently ignored several generic
  actions. They now query the configured coprocessor and implement all seven upstream action
  families: callback, direct one-word access, direct two-word access, and exception paths.
- The registry previously stopped at `JitConfig`. It is now forwarded through x64 and arm64 emit
  configuration, with empty registries used only for A64 emitters where Eden has no A32 coprocessor
  configuration.
- CP15 UPRW/URO storage previously lived in x64/arm64 JIT state and was exposed through bespoke
  `A32Jit` accessors. Those non-upstream fields and accessors are removed; storage now belongs to
  the configured core CP15 object, as it does in Eden.

### Missing items
- None in the reviewed x64/arm64 A32 coprocessor emission slice.

### Binary layout verification
- PASS for the reviewed state change: the two non-upstream CP15 words are removed from backend JIT
  state, and every generated state access continues to use `offset_of!` rather than a persisted
  numeric offset. Coprocessor pointers and callbacks are host-only and are not raw-copied into a
  guest-visible structure.

## 2026-08-23 — `core/arm/dynarmic/dynarmic_cp15.rs` vs Eden `dynarmic_cp15.{h,cpp}`

Related Rust owner: `src/core/src/arm/dynarmic/arm_dynarmic_32.rs`; related Eden owner:
`src/core/arm/dynarmic/arm_dynarmic_32.{h,cpp}`.

### Intentional differences
- Rust uses `UnsafeCell<u32>` for UPRW/URO and the ignored-write target so a coprocessor shared by
  `Arc` can expose stable direct-access pointers through `&self`. Eden exposes pointers to mutable
  members through a `shared_ptr`; both rely on the same single guest-execution-thread lifetime.
- Eden's ignored-write target is process-global. Rust keeps one stable target in each CP15 object;
  its address and stored value are unobservable, while avoiding mutable global state.
- The CNTPCT callback reaches `ArmDynarmic32` through the existing post-placement atomic parent
  pointer. This is the Rust counterpart of Eden's constructor-time parent reference and is needed
  because the Rust CPU object moves into its final `Box` after JIT construction.
- Rust uses `log::error!` where Eden uses `LOG_CRITICAL`, and portable sequentially-consistent
  atomic fences on non-MSVC-x64 hosts where Eden selects compiler-specific barrier intrinsics.
  The MSVC x64 DSB/DMB instruction distinction is preserved explicitly.

### Unintentional differences (to fix)
- CP15 previously returned local result enums that the JIT interpreted through hard-coded paths.
  It now implements the upstream `Coprocessor` interface directly, including exact accepted
  encodings, direct UPRW/URO accesses, barrier callbacks, CNTPCT callback, and rejection behavior.
- `ArmDynarmic32` previously owned only a separate URO word and synchronized UPRW through bespoke
  JIT state. It now owns one shared CP15 object, installs it in registry slot 15 before JIT
  creation, and reads/writes thread context through that object in Eden's lifecycle order.

### Missing items
- None in the reviewed CP15 and `ArmDynarmic32` integration slice.

### Binary layout verification
- N/A: CP15 is a host-side polymorphic service object and is never serialized or raw-copied to
  guest memory. Focused tests verify that the two thread-register actions expose distinct stable
  words and that every accepted/rejected compile action matches Eden.

## 2026-08-23 — `src/rdynarmic/src/ir/acc_type.rs` vs Eden `ir/acc_type.h`

Related Rust users: `src/rdynarmic/src/backend/{x64/emit_x64_memory,arm64/emit_arm64_memory}.rs`.

### Intentional differences
- Rust spells Eden's uppercase enumerators with Rust `UpperCamelCase` (`ORDEREDRW` becomes
  `OrderedRw`, `DCZVA` becomes `Dczva`) and uses `repr(u8)`. The value is a typed IR immediate and
  is never passed through the host ABI or raw-copied; the explicit representation makes the exact
  contiguous discriminants reviewable.

### Unintentional differences (to fix)
- The former Rust enum had 15 values from a different access-type vocabulary and lacked Eden's
  `PTW`, `DC`, `IC`, `DCZVA`, `AT`, and `SWAP` entries. It now has Eden's exact 16-value inventory
  and declaration order.
- The active `OrderedAtomic` and `IfetchOrdered` aliases are renamed to the corresponding upstream
  `OrderedRw` and `Ifetch` values in both backend ordering checks and focused tests.
- The unused fallback conversion silently mapped every invalid byte to `Normal`, behavior with no
  upstream counterpart. It is removed; IR construction uses typed `AccType` values throughout.

### Missing items
- None for the `AccType` inventory or the reviewed backend ordering predicate.

### Binary layout verification
- PASS for the Rust IR representation: focused tests require size/alignment one byte and exact
  discriminants 0 through 15 in Eden declaration order. No guest or persisted binary structure
  contains this enum.

## 2026-08-23 — rdynarmic A64 cache-maintenance frontend vs Eden Dynarmic

Rust files: `src/rdynarmic/src/interface/a64/config.rs`,
`src/rdynarmic/src/frontend/a64/translate/{sys_dc,sys_ic}.rs`, and
`src/rdynarmic/src/ir/{a64_emitter,opcode}.rs`.

Eden files: `src/dynarmic/src/dynarmic/interface/A64/config.h`,
`src/dynarmic/src/dynarmic/frontend/A64/translate/impl/{sys_dc,sys_ic}.cpp`,
`src/dynarmic/src/dynarmic/frontend/A64/a64_ir_emitter.h`, and
`src/dynarmic/src/dynarmic/ir/opcodes.inc`.

### Intentional differences
- Rust gives both cache-operation enums an explicit `repr(u8)` and Rust-style `Va` spelling. They
  remain typed until `A64IREmitter` converts them to Eden's contiguous `U64` IR immediate.
- Eden generates instruction dispatch from decoder tables; Rust's generated decoder feeds methods
  in matching `sys_dc.rs` and `sys_ic.rs` owners. The decoded operands and visitor order are the
  same.

### Unintentional differences (to fix)
- All non-ZVA cache operations were previously NOPs in the unrelated `simd.rs` owner, while ZVA
  emitted eight hard-coded 64-bit normal stores. The nine DC visitors now emit Eden's exact typed
  cache operation and register value; the callback-config pass owns any later ZVA lowering.
- All three instruction-cache operations were previously NOPs. They now emit the exact operation
  and value, write the next PC, and terminate with `CheckHalt(ReturnToDispatch)` in Eden's order.
- `A64DataCacheOperationRaised` previously omitted the current location descriptor. Its emitter
  and opcode metadata now have Eden's exact `Void(U64, U64, U64)` contract.

### Missing items
- None in the reviewed cache-operation enums, frontend visitors, or A64 IR-emitter contract. The
  callback-config optimization and host backends are the recorded next prerequisite.

### Binary layout verification
- PASS: focused tests require the exact operation discriminants and all three data-cache IR
  arguments, including the location descriptor. These enums and IR immediates are host-internal;
  no guest-visible raw payload changes.

## 2026-08-23 — rdynarmic A64 cache callback/config/backends vs Eden Dynarmic

Rust files: `src/rdynarmic/src/ir/opt/a64_callback_config.rs`,
`src/rdynarmic/src/backend/x64/{a64_emit_x64,emit_a64,emit_context}.rs`,
`src/rdynarmic/src/backend/arm64/{a64_address_space,emit_arm64,emit_arm64_a64}.rs`,
`src/rdynarmic/src/{jit,jit_config}.rs`, and
`src/core/src/arm/dynarmic/arm_dynarmic_64.rs`.

Eden files: `src/dynarmic/src/dynarmic/ir/opt_passes.cpp`,
`src/dynarmic/src/dynarmic/backend/x64/a64_emit_x64.cpp`,
`src/dynarmic/src/dynarmic/backend/arm64/{a64_address_space,emit_arm64_a64}.{cpp,h}`,
`src/dynarmic/src/dynarmic/interface/A64/config.h`, and
`src/core/arm/dynarmic/arm_dynarmic_64.cpp`.

### Intentional differences
- Rust's index-backed IR arena renumbers later `InstRef` values after each insertion and recomputes
  use counts after the pass. Eden's list-backed iterator keeps instruction addresses stable; the
  emitted instruction order and operands are otherwise identical.
- Host callbacks use the existing Rust trait-object trampolines and register-allocation APIs. x64
  still reserves its callback-context ABI argument, while arm64 reserves `X0`; the guest operation
  and value therefore occupy the same two effective callback parameters as Eden.

### Unintentional differences (to fix)
- Ruzu still exposes one combined `JitConfig` for A32 and A64 instead of the two upstream
  `interface/{A32,A64}/config.h::UserConfig` owners. The newly reviewed `ctr_el0`, `dczid_el0`, and
  `hook_data_cache_operations` state is at least kept at that public configuration level rather
  than in backend memory options, but the broader configuration split remains structural debt.
- The pre-existing `hook_isb` field lived in `MemoryEmitConfig`. It now sits beside the other
  callback-policy state in the public combined `JitConfig` and is forwarded explicitly to both
  backends; splitting that combined owner remains part of the broader structural debt above.

### Missing items
- None in the reviewed A64 cache callback, unhooked lowering, x64 emission, arm64 emission, or
  configurable CTR/DCZID behavior.

### Binary layout verification
- PASS: the new configuration fields are host-only Rust values and are not serialized or copied to
  guest memory. Cache-operation IR keeps Eden's exact `Void(U64, U64, U64)` / `Void(U64, U64)`
  signatures, and focused native plus AArch64-QEMU tests verify callback argument bit patterns.

## 2026-08-23 — rdynarmic dead IR opcodes vs Eden Dynarmic IR/backend owners

Rust files: `src/rdynarmic/src/ir/opcode.rs` and
`src/rdynarmic/src/backend/x64/{emit,emit_vector_arrangement,emit_vector_helpers}.rs`.

Eden files: `src/dynarmic/src/dynarmic/ir/{opcodes.inc,ir_emitter.h}` and
`src/dynarmic/src/dynarmic/backend/x64/emit_x64_vector.cpp`.

### Intentional differences
- None in this slice. Rust retains its index-based insertion-point state, but that state is not an
  IR opcode, matching Eden's separation between `IREmitter` state and the opcode inventory.

### Unintentional differences (to fix)
- The Rust opcode enum formerly exposed `SetInsertionPoint` and `GetInsertionPoint` as void IR
  instructions. Neither had a producer or backend consumer; Eden exposes insertion-point changes
  solely as `IREmitter` methods. The two dead opcodes and their metadata are removed.
- Rust formerly exposed three immediate shuffle opcodes and x64 emitters with no frontend producer.
  Eden has no such IR opcodes and uses host shuffles locally inside the emitters that require them.
  The dead opcodes, dispatch arms, emitter functions, helper, and signature-only test are removed.

### Missing items
- None for the reviewed insertion-point state or the three dead shuffle operations.

### Binary layout verification
- PASS: the removed values were host-internal IR enum variants with no producer, persisted format,
  raw-memory payload, or guest-visible representation. The exact opcode audit now reports 725 Eden
  opcodes, 742 Rust opcodes, zero missing/shared-signature mismatches, and 17 remaining Rust extras.

## 2026-08-23 — rdynarmic signed vector comparison IR vs Eden `ir_emitter.h`

Rust files: `src/rdynarmic/src/ir/{emitter,opcode}.rs`,
`src/rdynarmic/src/backend/x64/{emit,emit_vector_compare}.rs`,
`src/rdynarmic/src/backend/arm64/{emit_arm64,emit_arm64_vector}.rs`, and
`src/rdynarmic/src/frontend/{a32/translate/asimd_three_regs,a64/translate/simd_scalar_three_same,a64/translate/simd_three_same,a64/translate/simd_two_register_misc}.rs`.

Eden files: `src/dynarmic/src/dynarmic/ir/{ir_emitter.h,opcodes.inc}` and
`src/dynarmic/src/dynarmic/backend/{x64/emit_x64_vector.cpp,arm64/emit_arm64_vector.cpp}`.

### Intentional differences
- Rust uses locals for intermediate `Value`s because nested mutable method calls cannot borrow the
  emitter repeatedly in one expression. The generated instruction order and dependencies are the
  same as Eden's nested expressions.

### Unintentional differences (to fix)
- `vector_greater_equal_signed` formerly emitted one of four Rust-only opcodes. It now emits
  `VectorGreaterS*`, then `VectorEqual*`, then `VectorOr`, matching Eden exactly.
- `vector_less_equal_signed` formerly emitted one of four Rust-only opcodes. It now emits
  `VectorGreaterS*` followed by `VectorNot`, matching Eden exactly.
- `vector_less_signed` formerly emitted one of four Rust-only opcodes. It now emits
  `VectorGreaterS*`, `VectorEqual*`, `VectorOr`, and `VectorNot` in Eden's order.
- Four dedicated unsigned-greater-or-equal opcodes had no producer because Rust already used Eden's
  max-plus-equal composition. All sixteen non-upstream comparison opcodes, metadata entries, x64
  emitters/fallback, arm64 emitters, dispatch arms, and signature-only tests are removed.
- `VectorGreaterUnsigned`, `VectorLessEqualUnsigned`, and `VectorLessUnsigned` were missing from
  Rust's `IREmitter`; three frontends instead expanded equivalent but differently ordered IR.
  The helpers now live with Eden's other comparison helpers and the A32/A64 comparison visitors call
  their exact upstream owners. A64 scalar signed `LE`/`LT` now likewise call the matching signed
  helpers instead of swapping operands into `GE`/`GT`.

### Missing items
- None for the seven reviewed signed/unsigned comparison helper compositions or their primitive
  backend operations.

### Binary layout verification
- PASS: focused emitter and A32/A64 frontend tests verify all four element sizes, instruction order,
  IR dependencies, and owner selection. The removed enum variants were host-internal and never
  serialized or raw-copied. The exact audit now reports 725 Eden opcodes, 726 Rust opcodes, no
  missing/shared-signature mismatches, and one remaining Rust-only diagnostic opcode.

## 2026-08-23 — rdynarmic A32 translation/IR diagnostic hook vs Eden Dynarmic

Rust files: `src/rdynarmic/src/frontend/a32/translate/mod.rs`,
`src/rdynarmic/src/ir/{a32_emitter,opcode}.rs`,
`src/rdynarmic/src/backend/x64/{emit,a32_emit_a32}.rs`,
`src/rdynarmic/src/backend/arm64/emit_arm64.rs`, and `src/rdynarmic/src/jit.rs`.

Eden files: `src/dynarmic/src/dynarmic/frontend/A32/translate/{translate_arm,translate_thumb}.cpp`,
`src/dynarmic/src/dynarmic/frontend/A32/a32_ir_emitter.{h,cpp}`,
`src/dynarmic/src/dynarmic/ir/opcodes.inc`, and the A32 x64/arm64 emitter owners.

### Intentional differences
- Ruzu's separate environment-gated block-entry `RUZU_A32_PC_TRACE` diagnostic remains outside the
  IR surface. Removing the per-instruction opcode restores normal translation parity without
  changing that disabled-by-default diagnostic facility.

### Unintentional differences (to fix)
- `RUZU_A32_PC_EXEC` formerly parsed a list of ARM guest PCs and appended a Rust-only host-call IR
  instruction after matching ARM instructions. Eden has no such opcode or translation step, and
  the Rust path did not cover Thumb instructions. Its environment parser, translator injection,
  A32 emitter method, opcode/metadata, side-effect classification, x64/arm64 dispatch and host-call
  emitters, and JIT guard exception are removed.

### Missing items
- Resolved by the later A32 translation callbacks/options slice: the loop now owns
  `PreCodeReadHook`, `PreCodeTranslationHook`, and per-instruction `GetTicksForCode` in matching
  callback and ARM/Thumb modules.

### Binary layout verification
- PASS: the removed opcode and its five operands were host-internal IR only and were not serialized,
  raw-copied, or guest-visible. The exact audit now reports 725 opcodes on both sides, zero missing
  or extra operations, zero shared-signature mismatches, and complete one-to-one metadata coverage.

## 2026-08-23 — `src/rdynarmic/src/interface/a32/arch_version.rs`, `frontend/a32/translate/a32_translate.rs`, and `ir/a32_emitter.rs` vs Eden A32 architecture/translation options

Eden files: `src/dynarmic/src/dynarmic/interface/A32/{arch_version.h,config.h}`,
`frontend/A32/translate/a32_translate.{h,cpp}`, and
`frontend/A32/a32_ir_emitter.{h,cpp}`.

### Intentional differences
- Rust's `ArchVersion` has `repr(u8)` and a `Default` of `V8`; the representation mirrors the C++
  underlying type, while the default mirrors `A32::UserConfig`. `TranslationOptions::default()`
  explicitly selects `V3`, preserving C++ value-initialization of `TranslationOptions{}`.
- `translate` creates and returns its `Block`, rather than receiving an output reference. This is
  an ownership-only adaptation; descriptor selection and ARM/Thumb dispatch order are unchanged.
- `A32IREmitter::with_location` remains a V8 convenience for existing Rust unit callers.
  Production A32 translation uses `with_location_and_arch` with the configured version.

### Unintentional differences (to fix)
- Ruzu still combines A32 and A64 public state in `JitConfig`, whereas Eden owns separate
  `interface/A32/config.h::UserConfig` and `interface/A64/config.h::UserConfig` types. The new A32
  fields are behaviorally forwarded, but this pre-existing structural split remains to be ported.

### Missing items
- The reviewed `ArchVersion`, `TranslationOptions`, `ALUWritePC`, and `LoadWritePC` contracts are
  present. The remaining item is the broader A32/A64 public configuration ownership split above.

### Binary layout verification
- PASS: `ArchVersion` is an eight-value `repr(u8)` enum in Eden declaration order. Translation and
  JIT options are host-only and are not raw-copied or serialized into guest-visible memory.

## 2026-08-23 — A32 translation callbacks and loops vs Eden `translate_callbacks.h`, `translate_arm.cpp`, and `translate_thumb.cpp`

Rust files: `src/rdynarmic/src/frontend/a32/translate/{translate_callbacks,translate_arm,translate_thumb}.rs`,
`src/rdynarmic/src/jit_config.rs`, `src/rdynarmic/src/backend/x64/a32_emit_x64.rs`,
`src/rdynarmic/src/backend/arm64/a32_address_space.rs`, and `src/rdynarmic/src/jit.rs`.

### Intentional differences
- `UserCallbacksAdapter` models C++ `UserCallbacks : TranslateCallbacks` by delegation because Rust
  traits do not inherit stored trait objects. The frontend depends only on the translation-time
  contract, while both host backends adapt the public callbacks at their compile boundary.
- Rust briefly reconstructs `A32IREmitter` around each callback to satisfy exclusive borrowing.
  It preserves the same block, architecture version, current location, callback order, and emitted
  instruction order as Eden's single long-lived `TranslatorVisitor`.
- ARM VFP/ASIMD/ARM and Thumb VFP/ASIMD/Thumb32 selection use Ruzu's unified decoder plus explicit
  classification. The precedence matches Eden, but the decoder implementation is not generated as
  three distinct matcher invocations.

### Unintentional differences (to fix)
- The per-instruction visitors are still coordinated by the broad Rust `translate/mod.rs`
  dispatcher, while Eden retains one method per matching `translate/impl/*.cpp` owner. Splitting
  those pre-existing ownership aggregates remains part of the structural audit.

### Missing items
- No callback or loop-order item remains in this slice: pre-read early termination, aligned code
  reads, pre-translation hooks, custom ticks, NoExecuteFault advancement, conditional-state exit,
  single-step terminals, and end-location updates are all wired and covered by focused tests.

### Binary layout verification
- N/A for guest layout: callbacks, options, and translation loop state are host-only. The adapter
  passes A32 PCs/instructions as exact `u32` values and tick counts as `u64`, matching Eden widths.

## 2026-08-23 — A32 hint/preload decoding and translation vs Eden decoder tables and hint owners

Rust files: `src/rdynarmic/src/frontend/a32/{decoder,decoder_thumb16,decoder_thumb32}.rs` and
`src/rdynarmic/src/frontend/a32/translate/{hint,thumb16,thumb32,thumb32_control}.rs`.

Eden files: `frontend/A32/decoder/{arm,thumb16,thumb32}.inc` and
`frontend/A32/translate/impl/{hint,thumb16,thumb32_control,thumb32_load_byte}.cpp`.

### Intentional differences
- Rust expresses generated decoder-table rows as explicit mask/value checks. The masks and values
  are derived directly from Eden's bitstrings and are placed before the same broader decode groups.

### Unintentional differences (to fix)
- Thumb32 preload behavior currently shares the existing broad `thumb32.rs` and
  `thumb32_control.rs` owners. Eden owns these methods in `thumb32_load_byte.cpp`; moving the whole
  related Thumb32 load-byte family, rather than only the new methods, remains a structural slice.

### Missing items
- The reviewed hint family now includes ARM/Thumb16/Thumb32 `SEVL`, all four Thumb32 `PLD` forms,
  all four Thumb32 `PLI` forms, the W-bit PLD/PLDW distinction, hook-disabled NOP behavior, and the
  register-PC UnpredictableInstruction checks.

### Binary layout verification
- N/A: decoder identifiers and exception IR are host-internal. Focused tests verify the exact
  16/32-bit encodings and exception discriminants rather than a raw guest payload.

## 2026-08-23 — `src/rdynarmic/src/backend/x64/emit_data_processing.rs` vs Eden `backend/x64/emit_x64_data_processing.cpp` (`ExtractRegister`)

### Intentional differences
- Rust's `change_bit` and assembler methods return `Result`; the emitter unwraps them at the same
  points where Eden relies on Xbyak assertions/errors. Register allocation and emission ownership
  remain in the matching x64 data-processing module.

### Unintentional differences (to fix)
- Fixed: Ruzu previously branched on whether `lsb` was immediate and advertised dynamic
  `ExtractRegister32`/`ExtractRegister64` paths that only panicked. Eden has one shared helper,
  obtains `lsb` through `GetImmediateU8`, and unconditionally emits `SHRD`; Ruzu now does the same,
  including Eden's scratch/source allocation and immediate-extraction order.

### Missing items
- None in the reviewed x64 `ExtractRegister32`/`ExtractRegister64` emitter slice. Dynamic `lsb` is
  not an upstream feature: both opcode signatures accept `U8`, while both host backends require an
  immediate at emission time.

### Binary layout verification
- N/A: this slice emits host x86-64 instructions and changes no raw-copied or serialized guest
  structure. The focused regression covers both 32-bit and 64-bit widths with an immediate `lsb`.

## 2026-08-23 — `src/rdynarmic/src/backend/arm64/inst.rs` prerequisites for Eden `backend/arm64/emit_arm64_packed.cpp`

### Intentional differences
- Eden delegates AArch64 encoding to Oaknut. Ruzu owns its equivalent bit encoders in `inst.rs`;
  the new helpers keep that existing platform-adaptation boundary and expose the exact instruction
  forms used by the upstream packed emitter.

### Unintentional differences (to fix)
- Fixed prerequisite: Ruzu lacked the 64-bit-vector forms of `MOVI`, `AND`, `EOR`, and `BSL`, the
  compare-against-zero forms of `CMGE`/`CMEQ`, and encoders for `UADDLV` and `SHRN`. The former
  `movi_v16b_imm` also accepted only two hard-coded immediates; its shared encoder now accepts the
  full `imm8` field while preserving its existing encodings.

### Missing items
- The encoders required by the complete `emit_arm64_packed.cpp` port are present. The packed
  emitter itself remains the next slice and is not claimed complete by this prerequisite commit.

### Binary layout verification
- PASS: focused encoder tests compare every new form against exact 32-bit words independently
  assembled with GNU AArch64 binutils, including both MOVI masks, all element widths used by
  `SHRN`/`UADDLV`, and the 64-bit vector logical/compare forms.

## 2026-08-23 — `src/rdynarmic/src/backend/arm64/a32_address_space.rs` vs Eden `backend/arm64/a32_address_space.cpp` (`GenerateIR` constant reads)

### Intentional differences
- Eden's central `Optimization::Optimize` obtains `MemoryReadCode` and `IsReadOnlyMemory` through
  `A32::UserCallbacks`. Ruzu invokes the already-separated Rust passes explicitly and supplies two
  closures over the same callback owner.

### Unintentional differences (to fix)
- Fixed: the ARM64 address space called `a32_constant_memory_reads` with an undefined `read_code`
  identifier. Native x86-64 builds did not compile this target-specific owner, but AArch64 builds
  failed. The closure now delegates `u32` A32 addresses to `UserCallbacks::memory_read_code` with
  the same widening used by the translation callback adapter.

### Missing items
- No constant-memory callback is missing in the reviewed `GenerateIR` path. Broader optimization
  pass order and ownership remain tracked separately from this compile-blocking correction.

### Binary layout verification
- N/A: this change only restores a host callback passed to an IR optimization; it changes no
  raw-copied or serialized guest structure and preserves the guest address as an unsigned 32-bit
  value before widening it to the public callback's `u64` parameter.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder.rs` vs Eden `frontend/A32/decoder/arm.inc` (literal loads)

### Intentional differences
- Eden generates its ARM decoder from per-instruction bit-pattern declarations in `arm.inc`.
  Ruzu's existing decoder is a handwritten decision tree, so the six literal patterns are routed
  explicitly inside the matching load/store decode families.

### Unintentional differences (to fix)
- Fixed: the extra-load/store decoder never produced `LDRD_lit`, `LDRH_lit`, `LDRSB_lit`, or
  `LDRSH_lit`, even though their identifiers existed. It now matches Eden's Rn=PC pattern priority
  and preserves Eden's fixed P=1/W=0 constraints for the doubleword and signed literal forms.

### Missing items
- None among Eden's six reviewed ARM literal-load patterns: `LDR`, `LDRB`, `LDRD`, `LDRH`,
  `LDRSB`, and `LDRSH` all have reachable Rust decoder identifiers.

### Binary layout verification
- N/A: the decoder classifies fixed 32-bit instruction words and defines no raw-copied payload.
  Focused tests cover all six Eden patterns plus non-literal PC encodings that must remain routed
  to immediate visitors and raise UnpredictableInstruction there.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/{load_store.rs,mod.rs}` vs Eden `frontend/A32/translate/impl/{load_store.cpp,a32_translate_impl.h}` (load visitors)

### Intentional differences
- Rust extracts typed fields from `DecodedArm` inside each matching snake-case visitor; Eden's
  generated decoder passes them as typed parameters. ARM condition-state bookkeeping remains in
  Ruzu's block translator rather than being repeated in every visitor.
- Rust uses `wrapping_add`/`wrapping_sub` to state C++ unsigned-`u32` address wraparound explicitly,
  and `Reg::from_u32` replaces Eden's register `operator+` after the same validity checks.

### Unintentional differences (to fix)
- Fixed: all six literal-load identifiers shared immediate visitors. Dedicated Rust visitors now
  own Eden's exact immediate PC-relative address calculation, access width/type, extension,
  destination handling, terminal choice, and LDRD endian-sensitive split order.
- Fixed: immediate and register-offset load visitors omitted Eden's register/writeback validation;
  LDR register-to-PC also omitted Eden's PopRSBHint branch. The reviewed `LDR`, `LDRB`, `LDRD`,
  `LDRH`, `LDRSB`, and `LDRSH` load methods now preserve those checks before reading operands.
- Frontend-wide pre-existing difference: Ruzu performs condition-state setup before dispatch,
  whereas Eden performs each visitor's encoding validation before `ArmConditionPassed`. Correcting
  that ordering requires restoring visitor-owned condition state across the A32 frontend, not a
  load/store-local helper, and remains a separate structural slice.

### Missing items
- None among the reviewed literal, immediate, and register variants of `LDR`, `LDRB`, `LDRD`,
  `LDRH`, `LDRSB`, and `LDRSH`. Store visitors and the unprivileged `*T` methods were not claimed
  by this prerequisite slice.

### Binary layout verification
- N/A: these visitors construct internal SSA and serialize no guest payload. Focused tests verify
  immediate address operands, absence of synthetic Add32/Sub32 operations, exception terminals,
  LDR-to-PC dispatch, LDRD access atomicity, and endian-dependent opcode ordering.

## 2026-08-23 — `src/rdynarmic/src/backend/arm64/emit_arm64_packed.rs` vs Eden `backend/arm64/emit_arm64_packed.cpp`

### Intentional differences
- Eden emits through Oaknut register wrappers. Ruzu propagates encoder/allocation failures with
  `Result` and passes realized vector-register indexes to its existing `inst.rs` encoder boundary;
  the upstream helper ownership and instruction ordering remain local to the matching packed file.
- Eden declares generic `EmitIR` specializations through `emit_arm64.h`. Ruzu's central dispatcher
  routes the same opcode set to `emit_packed_instruction`, while each implementation remains owned
  by the new file corresponding to `emit_arm64_packed.cpp`.

### Unintentional differences (to fix)
- Fixed: the ARM64 dispatcher rejected all 34 packed opcodes. The matching Rust owner now emits
  Eden's eight add/sub operations and optional GE results, eight mixed add/sub operations, twelve
  halving operations, eight saturating operations, absolute-difference sum, and packed selection.
- Fixed: the mixed add/sub family now preserves Eden's V0/V1/V2 scratch sequence, lane rotation,
  signed/unsigned widening and halving, GE mask construction, and final narrowing order.
- Fixed: saturating operations spill the deferred FPSR state before modifying host QC, matching
  Eden's lifecycle order rather than allowing a later FPSR restore to overwrite the result.

### Missing items
- None among the 34 explicit `EmitIR` specializations in the reviewed Eden file.

### Binary layout verification
- N/A: this file defines no raw-copied payload. AArch64 tests run under QEMU route all 34 opcodes
  and compare parity-sensitive GE, scratch-register, saturation, absolute-difference, and select
  sequences against exact 32-bit instruction words from the independently verified encoders.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (unprivileged loads)

### Intentional differences
- Eden builds an ordered matcher from `thumb32.inc`; Ruzu retains its existing handwritten
  decision tree. The Rust branch now makes the same `1110` low-control-nibble priority explicit
  before the broader `1PUW` immediate forms.

### Unintentional differences (to fix)
- Fixed: `LDRT`, `LDRBT`, `LDRHT`, `LDRSBT`, and `LDRSHT` decoded as their generic imm8 families.
  They now have distinct identifiers and win over `LDR/B/H/SB/SH_imm8` for the exact five Eden
  patterns.
- Fixed: negative-offset word/halfword/signed-byte literals with `Rn=PC` lost to the broader
  register/imm8 groups, and Eden's four reserved signed-halfword `Rt=PC` patterns did not decode as
  `NOP`. The handwritten decision tree now preserves those earlier table entries.
- Fixed: translation preserves the existing effective-address behavior for the newly distinct
  identifiers and applies Eden's PC-destination rejection before performing the load. Their final
  method ownership now lives in the matching load-byte, load-halfword, and load-word files.

### Missing items
- None among the five reviewed unprivileged word/byte/halfword load patterns. Store families were
  not part of this decoder prerequisite.

### Binary layout verification
- N/A: the decoder classifies 32-bit instruction words and defines no raw-copied payload. Focused
  tests cover all five exact `...1110...` encodings, adjacent `...1100...` imm8 encodings, the four
  negative literal forms, and the four reserved signed-halfword `NOP` patterns.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_load_byte.rs` vs Eden `frontend/A32/translate/impl/{thumb32_load_byte.cpp,a32_translate_impl.h}`

### Intentional differences
- Eden's generated matcher passes decoded fields as typed visitor parameters. Ruzu's matching
  snake-case methods read those fields from `DecodedThumb32`; each method and helper remains in the
  corresponding byte-load owner, while `thumb32.rs` only dispatches.
- Rust uses a higher-ranked function pointer over `A32IREmitter` for Eden's
  `ExtensionFunctionU8` member pointer and explicit `wrapping_add`/`wrapping_sub` for the same
  unsigned `u32` literal-address arithmetic.

### Unintentional differences (to fix)
- Fixed: byte-load and preload behavior was split between broad `thumb32.rs` and unrelated
  `thumb32_control.rs` owners. All 18 visitors plus `PLDHandler`, `PLIHandler`, `LoadByteLiteral`,
  `LoadByteRegister`, and `LoadByteImmediate` now live in `thumb32_load_byte.rs`.
- Fixed: generic byte loads omitted Eden's imm8 validation (`Rt=PC && W`, writeback aliasing, and
  `!P && !W`) and the register form's `Rm=PC` rejection. Validation now precedes all register reads,
  memory operations, and writeback.
- Fixed: the former shared address helper performed writeback before the memory read and before
  writing `Rt`. The immediate helper now preserves Eden's `read -> extend -> SetRegister(t) ->
  optional SetRegister(n)` order.
- Fixed: register byte loads skipped `LogicalShiftLeft` when `imm2` was zero. The helper now emits
  Eden's operation unconditionally, preserving IR shape as well as the result.
- Fixed: the distinct `LDRBT`/`LDRSBT` paths now apply their PC validation and then reuse the normal
  positive, pre-indexed, non-writeback imm8 visitor exactly like Eden.

### Missing items
- None among the 18 declarations and implementations in the reviewed byte-load/memory-hint owner.

### Binary layout verification
- N/A: this frontend constructs SSA and defines no raw-copied payload. Focused tests verify literal
  U-bit addressing, signed/unsigned extension selection, validation-before-side-effects, exact
  destination/writeback order, register/preload PC checks, unprivileged access type, and absence of
  unprivileged writeback.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_load_halfword.rs` vs Eden `frontend/A32/translate/impl/{thumb32_load_halfword.cpp,a32_translate_impl.h}`

### Intentional differences
- Eden's generated matcher passes decoded fields to visitor methods. Ruzu's matching methods read
  the fields from `DecodedThumb32`, use a higher-ranked Rust function pointer for
  `ExtensionFunctionU16`, and state unsigned literal-address wraparound explicitly.

### Unintentional differences (to fix)
- Fixed: all halfword visitors and helpers lived in the broad `thumb32.rs`; the ten reviewed
  methods plus `LoadHalfLiteral`, `LoadHalfRegister`, and `LoadHalfImmediate` now live in the
  matching `thumb32_load_halfword.rs` owner.
- Fixed: the generic implementation omitted Eden's imm8 validation and register `Rm=PC` rejection.
  The exact `!P && !W` -> `Rt=PC && W` -> writeback-alias validation order now runs before any IR
  side effect.
- Fixed: register loads read `Rn` before `Rm` and elided a zero-bit shift. They now preserve Eden's
  `GetRegister(m) -> GetRegister(n) -> LogicalShiftLeft` IR ordering.
- Fixed: the shared address helper wrote the destination before the base for writeback forms. The
  halfword helper now preserves Eden's distinct `read -> extend -> optional SetRegister(n) ->
  SetRegister(t)` order.
- Fixed: `LDRHT` and `LDRSHT` now apply their PC validation and reuse the normal positive,
  pre-indexed, non-writeback imm8 visitors.

### Missing items
- None among the ten declarations and implementations in the reviewed halfword-load owner.

### Binary layout verification
- N/A: this frontend constructs SSA and defines no raw-copied payload. Focused tests verify
  positive/negative literal addressing, signed/unsigned extension, register-read and zero-shift IR
  order, validation-before-side-effects, halfword-specific writeback order, and unprivileged
  no-writeback behavior.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_load_word.rs` vs Eden `frontend/A32/translate/impl/{thumb32_load_word.cpp,a32_translate_impl.h}`

### Intentional differences
- Eden's generated matcher passes typed decoded fields to its visitor. The five snake-case Rust
  visitors read the same fields from `DecodedThumb32`; the dispatcher remains routing-only.
- Rust spells unsigned literal-address wraparound explicitly and represents Eden's terminal
  variants with the existing `Terminal` enum.

### Unintentional differences (to fix)
- Fixed: the five word-load visitors lived in broad `thumb32.rs`; they now live in the matching
  `thumb32_load_word.rs` owner, and the shared address helper no longer owns their behavior.
- Fixed: word loads omitted Eden's undefined/writeback-alias, `Rm=PC`, and nonfinal-IT validation.
  The exact validation order now runs before any register read or memory operation.
- Fixed: register loads read `Rn` before `Rm` and elided `LogicalShiftLeft` for a zero shift. They
  now preserve Eden's `GetRegister(m) -> GetRegister(n) -> LogicalShiftLeft` IR order.
- Fixed: indexed loads previously performed writeback inside the shared address helper before the
  memory read. The word visitor now preserves Eden's read-before-writeback order, followed by the
  PC update and `PopRSBHint` only for post-indexed `SP` loads.
- Fixed: the distinct `LDRT` visitor now rejects `Rt=PC` and reuses the positive, pre-indexed,
  non-writeback imm8 path exactly like Eden.

### Missing items
- None among the five declarations and implementations in the reviewed word-load owner.

### Binary layout verification
- N/A: this frontend constructs SSA and defines no raw-copied payload. Focused tests verify
  literal U-bit addressing, PC and IT behavior, validation-before-side-effects, register and
  zero-shift order, writeback-before-PC order, pop terminal selection, and unprivileged behavior.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (store single data item)

### Intentional differences
- Eden builds a priority-ordered matcher from `thumb32.inc`; Ruzu retains its handwritten
  decision tree while spelling the same control-nibble and register-form masks explicitly.

### Unintentional differences (to fix)
- Fixed: the fifteen Eden store-single entries were collapsed into nine ARM-manual-style IDs.
  Ruzu now exposes the exact `_imm_1`, `_imm_2`, `_imm_3`, `*T`, and register identities used by
  the upstream visitor boundary for word, byte, and halfword stores.
- Fixed: every store with bit 11 clear was accepted as a register form, and every store with bit
  11 set was accepted as an indexed immediate. The decoder now requires the exact register mask,
  gives `1100` and `1110` their `_imm_2`/`*T` priority, accepts only `1PU1` as `_imm_1`, and rejects
  reserved controls as `Unknown`.

### Missing items
- None among the fifteen store-single decoder entries reviewed from `thumb32.inc`.

### Binary layout verification
- N/A: the decoder classifies 32-bit instruction words and defines no raw-copied payload. Focused
  tests cover all fifteen identities plus reserved register/control patterns.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_store_single_data_item.rs` vs Eden `frontend/A32/translate/impl/{thumb32_store_single_data_item.cpp,a32_translate_impl.h}`

### Intentional differences
- Eden passes typed matcher fields to the fifteen visitors; the snake-case Rust visitors read the
  same fields from `DecodedThumb32`, while the dispatcher only routes exact decoded identities.
- Rust function pointers stand in for Eden's immediate-store callbacks and register-store lambdas.
  Separate byte, halfword, and word callbacks retain the same truncation and memory-operation
  ownership inside the matching file.

### Unintentional differences (to fix)
- Fixed: all store-single behavior lived in broad `thumb32.rs` behind six combined methods and a
  shared address helper. The fifteen visitors, `StoreRegister`, `StoreImmediate`, and width-specific
  callbacks now live in the matching store-single owner.
- Fixed: register stores omitted Eden's `Rn=PC` undefined path and `Rt/Rm=PC` unpredictable path,
  read operands as `Rn -> Rm -> Rt`, and skipped a zero-bit shift. They now validate first and emit
  `GetRegister(m) -> GetRegister(n) -> GetRegister(t) -> LogicalShiftLeft` exactly.
- Fixed: immediate stores omitted per-encoding PC/alias validation and performed indexed writeback
  while calculating the address, before reading `Rt` and before the store. They now preserve Eden's
  `GetRegister(n) -> GetRegister(t) -> address -> store -> optional SetRegister(n)` order.
- Fixed: `_imm_2`, `_imm_3`, and `*T` visitors now enforce their fixed subtract/add and no-writeback
  modes instead of deriving all behavior from one broad decoded form.

### Missing items
- None among the fifteen declarations and implementations in the reviewed store-single owner.

### Binary layout verification
- N/A: this frontend constructs SSA and defines no raw-copied payload. Focused tests verify exact
  register/shift ordering, byte/halfword truncation, validation-before-side-effects, store-before-
  writeback ordering, fixed immediate modes, and unprivileged no-writeback behavior.

## 2026-08-23 — `src/rdynarmic/src/ir/a32_emitter.rs` vs Eden `frontend/A32/{a32_ir_emitter.h,a32_ir_emitter.cpp}` (memory access boundary)

### Intentional differences
- Rust's shared `Value` wrapper requires explicit byte/halfword coercion where Eden's C++ method
  signatures carry `U8`/`U16` statically. The emitted operand types and operation order match.
- `ExclusiveReadMemory64` returns a Rust tuple instead of `std::pair`; both expose separate low and
  high words, and `ExclusiveWriteMemory64` accepts those words separately in the same order.

### Unintentional differences (to fix)
- Fixed: normal and exclusive 16/32-bit reads, normal 64-bit reads, and the corresponding writes
  omitted Eden's `EFlag` byte reversal at the A32 emitter boundary.
- Fixed: the 64-bit exclusive API returned/accepted one packed value. It now extracts low then high,
  reverses each word without swapping in big-endian mode, and reverses then packs both write words
  exactly like Eden. Existing ARM synchronization callers were updated to preserve that boundary.

### Missing items
- None among the reviewed normal/exclusive 8/16/32/64-bit memory methods.

### Binary layout verification
- N/A: these methods emit typed SSA operations rather than raw-copied structures. Focused tests
  verify the exact reversal counts and the reverse-low -> reverse-high -> pack -> exclusive-write
  sequence for the parity-sensitive 64-bit path.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (dual/exclusive/table branch)

### Intentional differences
- Eden generates its first-match decoder from pattern strings; Ruzu retains the handwritten
  decoder and represents this family as an ordered mask table derived from the same strings.

### Unintentional differences (to fix)
- Fixed: six LDRD/STRD encodings were collapsed into three broad identities, which erased Eden's
  fixed `P`/`W` visitor boundary and admitted reserved forms.
- Fixed: `LDA`, `STL`, `TBB`, and `TBH` were absent. The decoder now exposes all eighteen exact
  identities in upstream priority order, including the previously stubbed exclusive variants.

### Missing items
- None among the eighteen reviewed decoder entries from the dual/exclusive/table-branch group.

### Binary layout verification
- N/A: the decoder classifies 32-bit instruction words. An independent pattern-string parser
  verifies all eighteen Rust mask/value pairs against `thumb32.inc`, and focused tests exercise
  every identity.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_load_store_dual.rs` vs Eden `frontend/A32/translate/impl/{thumb32_load_store_dual.cpp,a32_translate_impl.h}`

### Intentional differences
- Eden's generated matcher passes typed fields into visitor methods. The eighteen snake-case Rust
  visitors read the same fields from `DecodedThumb32`; dispatch remains in `thumb32.rs` and all
  behavior and helpers live in the matching owner.
- Rust represents Eden's `U32`/`U64` SSA wrappers with `Value` and its terminal variants with the
  existing `Terminal` enum.

### Unintentional differences (to fix)
- Fixed: dual-load/store and word-exclusive behavior lived in broad `thumb32.rs`, byte/half/dual
  exclusives were stubs, and ordered access was incorrectly used for `LDREX`/`STREX`. The complete
  family now lives in the upstream-owned file and uses exact atomic/ordered/normal access types.
- Fixed: dual operations omitted Eden's validation and side-effect ordering. The helpers now
  preserve validation-before-operands, endian word selection, atomic 64-bit access, and writeback.
- Fixed: table branches and load-acquire/store-release were missing. Their IT checks, address and
  branch IR order, location update, fast-dispatch terminal, and ordered accesses now match Eden.

### Missing items
- None among the four helpers and eighteen visitor declarations/implementations in the reviewed
  owner.

### Binary layout verification
- N/A: this frontend constructs SSA and defines no raw-copied payload. Focused tests verify all
  decoder identities, validation-before-side-effects, endian extraction and writeback order,
  access types, exclusive widths, and table-branch read width/terminal behavior.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (load/store multiple)

### Intentional differences
- Eden generates its ordered decoder from pattern strings; Ruzu retains a handwritten decoder and
  uses an ordered mask table derived from those same six strings.

### Unintentional differences (to fix)
- Fixed: `STMIA` and `LDMIA` were exposed as generic `STM` and `LDM` identities, obscuring the
  exact upstream visitor boundary.
- Fixed: the former decision tree did not enforce bit 15 as zero for `STMIA`, `STMDB`, and `PUSH`,
  so reserved store-multiple encodings could reach translation. Exact upstream masks and priority
  now reject those words and preserve the specialized `POP`/`PUSH` entries.

### Missing items
- None among the six reviewed load/store-multiple decoder entries.

### Binary layout verification
- N/A: the decoder classifies 32-bit instruction words. An independent pattern-string comparison
  verifies all six mask/value pairs, and focused tests cover every identity and reserved bit-15
  store forms.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_load_store_multiple.rs` vs Eden `frontend/A32/translate/impl/{thumb32_load_store_multiple.cpp,a32_translate_impl.h}`

### Intentional differences
- Eden's matcher supplies typed `Imm<15>`/`Imm<16>` fields. Rust reads the same instruction fields
  from `DecodedThumb32` and explicitly masks the store lists to retain the `Imm<15>` boundary.
- Rust represents Eden's `IR::U32` wrappers with `Value` and uses `count_ones` for `std::popcount`.

### Unintentional differences (to fix)
- Fixed: the six visitors and both helpers lived in broad `thumb32.rs`; they now live in the
  matching owner and the dispatcher only routes exact identities.
- Fixed: loads omitted all Eden validation, while invalid stores returned `false` silently instead
  of raising an unpredictable-instruction exception. PC/base/list/IT validation now runs in the
  exact upstream order before any operand or memory access.
- Fixed: generic decrement/increment implementations reconstructed addresses and writeback instead
  of preserving Eden's shared start/writeback values. The helpers now retain exact atomic access,
  register iteration, writeback, upper-location update, PC-load, and terminal ordering.

### Missing items
- None among `LDMHelper`, `STMHelper`, and the six reviewed visitor implementations.

### Binary layout verification
- N/A: this frontend constructs SSA and defines no raw-copied payload. Focused tests verify
  validation-before-side-effects, atomic access metadata, register/writeback order, shared
  decrement-before start/writeback state, and POP's update-before-PC-read terminal path.

## 2026-08-23 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `ir/ir_emitter.h` (`PackedAbsDiffSumU8`)

### Intentional differences
- Rust uses snake case and the shared `Value` wrapper in place of Eden's typed `U32` wrapper.

### Unintentional differences (to fix)
- Fixed: the `PackedAbsDiffSumU8` opcode and both host backends existed, but the owning base-IR
  emitter method was absent, preventing the Thumb32 `USAD8`/`USADA8` visitors from matching Eden's
  call boundary.

### Missing items
- None for the reviewed `PackedAbsDiffSumU8` wrapper.

### Binary layout verification
- N/A: this is a typed SSA operation. A focused test verifies the exact opcode and `a, b` operand
  order.

## 2026-08-23 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `ir/ir_emitter.h` (`MostSignificantWord`)

### Intentional differences
- Rust uses a concrete `ResultAndCarry` structure containing `Value` fields where Eden uses the
  templated `ResultAndCarry<U32>` type.

### Unintentional differences (to fix)
- Fixed: `most_significant_word` returned only the primary value and callers created a carry
  pseudo-operation only on rounding paths. It now eagerly creates and links `GetCarryFromOp` and
  returns both values exactly at the base-IR ownership boundary; all A32 and A64 callers consume
  `.result`, and rounding multiply callers consume the returned `.carry`.

### Missing items
- None for the reviewed `MostSignificantWord` result/carry contract and its existing Rust callers.

### Binary layout verification
- N/A: this is SSA metadata rather than a raw-copied structure. A focused test verifies producer,
  pseudo-opcode, and associated-pseudo-operation linkage.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (multiply)

### Intentional differences
- Eden generates one global first-match table from pattern strings; Ruzu preserves its handwritten
  outer decoder and uses an ordered mask table derived from the sixteen multiply strings.

### Unintentional differences (to fix)
- Fixed: ten multiply identities were absent and the existing manual decoder recognized only six
  coarse forms. All sixteen exact identities now retain accumulator/non-accumulator and selector
  field boundaries in upstream priority order.
- Fixed: the `FB0…FB7` multiply range fell through to load-byte/halfword decoding, while unrelated
  later prefixes were routed to the multiply helpers. Explicit `FB0…FB7` and `FB8…FBF` family
  boundaries now route multiply and long-multiply words before the broad handwritten groups.

### Missing items
- None among the sixteen reviewed Thumb32 multiply decoder entries. The long-multiply visitor owner
  remains a separate audit slice.

### Binary layout verification
- N/A: the decoder classifies 32-bit instruction words. Independent comparison against
  `thumb32.inc` verifies all sixteen mask/value pairs; focused tests cover every identity and the
  multiply/long-multiply/coprocessor prefix boundaries.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_multiply.rs` vs Eden `frontend/A32/translate/impl/{thumb32_multiply.cpp,a32_translate_impl.h}`

### Intentional differences
- Eden's generated matcher passes typed registers and selector bits. The snake-case Rust visitors
  read those exact fields from `DecodedThumb32`, and Rust variable swaps replace `std::swap`.
- Rust uses `Value` for Eden's typed SSA wrappers and explicit `ImmU1(false/true)` carry inputs.

### Unintentional differences (to fix)
- Fixed: only six of sixteen visitors existed, aggregated in `thumb32.rs`; the complete owner now
  implements MLA/MLS/MUL, all signed halfword/word multiply families, and USAD8/USADA8.
- Fixed: the six former visitors omitted some or all PC validation and emitted register reads in a
  different order. Every visitor now validates before side effects and preserves Eden's operand,
  extension, product, accumulation, destination, and Q-flag order.
- Fixed: packed absolute difference, halfword exchange/selection, overflow accumulation, and the
  exact eager most-significant-word carry boundary were absent. They now use the matching IR
  operations and upstream-owned base-emitter prerequisites.

### Missing items
- None among the sixteen declarations and implementations in the reviewed multiply owner.

### Binary layout verification
- N/A: this frontend constructs SSA and defines no raw-copied payload. Focused tests exercise all
  sixteen decoded visitors, validation-before-register-read, accumulator operand order, both Q
  updates around SMLAD accumulation, rounding carry use, and dedicated packed absolute difference.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (long multiply)

### Intentional differences
- Eden generates one global first-match table from pattern strings; Ruzu retains its handwritten
  outer decoder and uses an ordered mask table derived from the same ten long-multiply strings.

### Unintentional differences (to fix)
- Fixed: four identities (`SMLALD`, `SMLALXY`, `SMLSLD`, and `UMAAL`) were absent, and the former
  six-way decision accepted reserved selector bits for several existing families. The decoder now
  uses all ten exact upstream masks in upstream priority order.

### Missing items
- None among the ten reviewed long-multiply, long-multiply-accumulate, and divide decoder entries.

### Binary layout verification
- N/A: the decoder classifies 32-bit instruction words. An independent pattern-string parser
  verifies all ten mask/value pairs against `thumb32.inc`, and focused tests exercise every
  identity.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_long_multiply.rs` vs Eden `frontend/A32/translate/impl/{thumb32_long_multiply.cpp,a32_translate_impl.h}`

### Intentional differences
- Eden's generated matcher passes typed registers and selector bits. The snake-case Rust visitors
  read the same fields from `DecodedThumb32`, and Rust's `Value` represents Eden's typed SSA
  wrappers.
- Rust free-function pointers cannot name `IREmitter` methods with Eden's C++ member-function
  pointer type, so two mechanical signed/unsigned wrappers feed the matching `DivideOperation`
  helper boundary.

### Unintentional differences (to fix)
- Fixed: six partial implementations lived in broad `thumb32.rs`, omitted all PC/equal-destination
  validation, and read accumulator registers in a different order. All ten visitors now live in
  the matching owner and preserve validation-before-side-effects and exact operand order.
- Fixed: `SMLALD`, `SMLALXY`, `SMLSLD`, and `UMAAL` were missing. Their halfword selection/swap,
  signed extension, add/subtract nesting, accumulation, and low/high destination emission now
  follow Eden literally, including the family-specific high-word extraction order.

### Missing items
- None among `DivideOperation` and the ten reviewed declarations/implementations.

### Binary layout verification
- N/A: this frontend constructs SSA and defines no raw-copied payload. Focused tests verify all ten
  visitors, validation before register reads, `SMLAL`/`UMAAL` operand order, and the direct
  low-write-before-high-extraction ordering used by the dual-halfword accumulate families.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (branch)

### Intentional differences
- Eden generates one global first-match table from pattern strings; Ruzu retains its handwritten
  outer family routing and uses the same ordered mask/value entries within the branch family.

### Unintentional differences (to fix)
- Fixed: generic `B_t3`, `B_t4`, and `BL` identities obscured the exact `B_cond`, `B`, and `BL_imm`
  visitor boundaries. The four branch identities and masks now match `thumb32.inc` exactly.
- Fixed: the former decision tree treated the reserved `F7E…` conditional-branch space as a
  fabricated Thumb32 `SVC`; Eden has no such visitor and decodes it as `UDF`. All three upstream
  Thumb32 `UDF` patterns now retain their priority around the branch entries.

### Missing items
- None among the four reviewed branch decoder entries.

### Binary layout verification
- N/A: the decoder classifies 32-bit instruction words. An independent pattern-string parser
  verifies the four branch masks, and focused tests cover all identities plus `UDF` priority.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_branch.rs` vs Eden `frontend/A32/translate/impl/{thumb32_branch.cpp,a32_translate_impl.h}`

### Intentional differences
- Eden's generated matcher passes typed immediate fields. The snake-case Rust visitors consume the
  same fields through `DecodedThumb32` offset helpers, and Rust terminal variants represent Eden's
  `IR::Term` values.

### Unintentional differences (to fix)
- Fixed: all four implementations lived in broad `thumb32.rs` and omitted Eden's IT-block
  validation. They now live in the matching owner and reject non-final IT positions, while
  conditional branches reject every IT position before branch side effects.
- Fixed: `BLX_imm` accepted an odd low immediate bit. It now validates `lo[0]` before pushing the
  RSB or writing LR, then preserves Eden's aligned-PC, ARM-state, and IT-advance ordering.

### Missing items
- None among the four reviewed branch declarations/implementations.

### Binary layout verification
- N/A: this frontend constructs SSA and terminals rather than raw-copied payloads. Focused tests
  verify all four visitors, IT and low-bit validation before link side effects, RSB/LR order,
  aligned BLX targeting, and conditional then/else locations.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/mod.rs` vs Eden `frontend/A32/translate/impl/a32_translate_impl.h` (`ThumbExpandImm_C`)

### Intentional differences
- Eden receives separate typed `i`, `imm3`, and `imm8` fields; Rust receives their already
  concatenated twelve-bit value from `DecodedThumb32`.
- Rust represents Eden's `IR::U1` carry with the shared `Value` SSA wrapper.

### Unintentional differences (to fix)
- Fixed: Thumb modified-immediate expansion lived in the decoder and accepted a host `bool` carry,
  so non-rotated forms could not preserve Eden's runtime `GetCFlag` SSA value. The helper now lives
  at the translator-implementation boundary, returns `ImmAndCarry`, and forwards dynamic carry
  unchanged for all replication forms.
- Fixed: the caller hard-coded carry-in to false. It now reads C before expansion; rotated forms
  replace it with the immediate result's bit 31 exactly as Eden does.

### Missing items
- None for the reviewed `ThumbExpandImm_C` and `ThumbExpandImm` helper pair. The neighboring ARM
  expansion helpers were not part of this prerequisite slice.

### Binary layout verification
- N/A: these helpers build immediate SSA values. Exhaustive tests verify all 4096 immediate values,
  dynamic-carry identity for the 1024 replication forms, and immediate bit-31 carry for every
  rotated form.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (modified immediate)

### Intentional differences
- Eden generates one global first-match decoder from pattern strings; Ruzu retains its handwritten
  family routing and an ordered mask table derived from the same sixteen strings.

### Unintentional differences (to fix)
- Fixed: `ADD_imm` and `SUB_imm` hid the upstream `_1` visitor identities, and the former field
  decision tree did not make every fixed/variable bit directly auditable. All sixteen identities,
  masks, and priority positions now match `thumb32.inc` exactly.

### Missing items
- None among the sixteen reviewed modified-immediate decoder entries.

### Binary layout verification
- N/A: the decoder classifies 32-bit instruction words. An independent pattern parser verifies all
  sixteen mask/value pairs, and focused decoder tests exercise every identity.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_data_processing_modified_immediate.rs` vs Eden `frontend/A32/translate/impl/{thumb32_data_processing_modified_immediate.cpp,a32_translate_impl.h}`

### Intentional differences
- Eden's generated matcher passes typed immediate fields and registers; the snake-case Rust
  visitors read those same fields from `DecodedThumb32` and use `Value` for typed SSA values.
- Eden's soft `ASSERT` records an impossible decoder-contract violation and may continue; Rust
  `assert!` stops on the same impossible direct-dispatch state. Valid decoded instructions cannot
  reach those assertions.

### Unintentional differences (to fix)
- Fixed: all sixteen visitors were collapsed into a generic dispatcher in broad `thumb32.rs`,
  omitted the per-visitor PC/decode validation, and read registers that MOV/MVN do not own. Each
  visitor now lives in the matching file and validates before emitting carry or operand reads.
- Fixed: the generic path emitted flag extraction/writes before the destination register. Logical
  and arithmetic instructions now preserve Eden's result → destination → flag-extraction → flag
  write order, while TST/TEQ/CMN/CMP never write a destination.
- Fixed: BIC used `Not32` plus `And32`, and MVN/ORN emitted runtime `Not32`; the port now uses
  `AndNot32` and compile-time complemented immediates exactly like Eden. Runtime C carry is also
  retained through the verified translator-owned expansion helper.

### Missing items
- None among the sixteen reviewed declarations/implementations.

### Binary layout verification
- N/A: this frontend constructs SSA and defines no raw-copied payload. Focused tests cover all
  sixteen visitors, validation-before-inputs, dynamic carry/register order, exact logical opcodes,
  destination-before-flags ordering, and the no-destination test forms.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (plain binary immediate)

### Intentional differences
- Eden generates one global first-match decoder from pattern strings; Ruzu retains its handwritten
  family routing and an ordered mask table derived from the same fifteen entries.

### Unintentional differences (to fix)
- Fixed: the former field decision tree omitted both `SSAT16` and `USAT16`, hid five upstream
  visitor identities behind `*_wide`/`ADR_add`/`ADR_sub` names, and made the reserved UDF priority
  difficult to audit. The exact fifteen identities, masks, and source order now match
  `thumb32.inc`.

### Missing items
- None among the fifteen reviewed decoder entries, including the reserved UDF encoding.

### Binary layout verification
- N/A: the decoder classifies 32-bit instruction words. An independent pattern parser verifies all
  fifteen mask/value pairs, and focused tests decode every identity and exercise every non-UDF
  visitor.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_data_processing_plain_binary_immediate.rs` vs Eden `frontend/A32/translate/impl/{thumb32_data_processing_plain_binary_immediate.cpp,a32_translate_impl.h}`

### Intentional differences
- Eden passes typed matcher fields to each visitor; the snake-case Rust visitors extract the same
  fields from `DecodedThumb32`. A small Rust enum represents Eden's member-function pointer used by
  the two saturation helpers.
- Eden's two-argument shift IR methods do not expose a carry input; the shared Rust IR methods take
  one, so shifts whose carry result is unused receive an immediate false value.

### Unintentional differences (to fix)
- Fixed: nine partial visitors lived in broad `thumb32.rs`, omitted PC and bit-range validation,
  optimized away zero shifts, and used a different BFI mask/operation sequence. They now live in
  the matching owner and preserve validation, register-read, shift, mask, and destination-write
  ordering.
- Fixed: `SSAT`, `SSAT16`, `USAT`, and `USAT16` were successful no-op stubs. Both upstream helper
  boundaries and all four visitors are now ported, including the single source-register read for
  halfword saturation and destination-before-Q-flag ordering.

### Missing items
- None among the fourteen reviewed declarations/implementations or their two private saturation
  helpers.

### Binary layout verification
- N/A: this frontend constructs SSA and defines no raw-copied payload. Focused tests cover all
  decoder entries and visitors, validation before register reads, exact BFI read/shift behavior,
  both halfword saturation results and Q writes, bitfield shifts, and aligned architectural PC.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (shifted register)

### Intentional differences
- Eden generates one global first-match decoder from pattern strings; Ruzu retains its handwritten
  family routing and an ordered mask table derived from the same seventeen entries.

### Unintentional differences (to fix)
- Fixed: a field-based decision tree obscured the exact fixed bits and specialized-entry priority.
  The seventeen identities, masks, and source-order positions now match `thumb32.inc` directly.

### Missing items
- None among the seventeen reviewed shifted-register decoder entries.

### Binary layout verification
- N/A: the decoder classifies 32-bit instruction words. An independent pattern parser verifies all
  seventeen mask/value pairs, and focused tests decode and translate every identity.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_data_processing_shifted_register.rs` vs Eden `frontend/A32/translate/impl/{thumb32_data_processing_shifted_register.cpp,a32_translate_impl.h}`

### Intentional differences
- Eden's generated matcher passes typed fields; the snake-case Rust visitors extract the same
  fields from `DecodedThumb32`. The private `shifted_register` helper is a mechanical expression of
  Eden's repeated `EmitImmShift(GetRegister(m), ..., GetCFlag())` call.

### Unintentional differences (to fix)
- Fixed: all seventeen visitors were collapsed into a generic dispatcher in broad `thumb32.rs`.
  That path skipped per-visitor decode/PC validation, read Rn for MOV/MVN, and wrote flags before
  destination registers. The split visitors now preserve Eden's ownership, validation, operand,
  destination, and flags ordering.
- Fixed: BIC expanded NOT+AND rather than using `AndNot`, while PKH always selected the same half
  ownership and register-read order. Both now emit Eden's exact operations for each `tb` form;
  ADC/SBC also perform the second runtime carry read used by the arithmetic operation.

### Missing items
- None among the seventeen reviewed declarations/implementations.

### Binary layout verification
- N/A: this frontend constructs SSA and defines no raw-copied payload. Focused tests cover all
  visitors, validation before shift inputs, MOV input ownership, destination-before-flags order,
  BIC opcode choice, ADC/SBC carry reads, and both PKH source-order branches.

## 2026-08-23 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `ir/ir_emitter.h` (`PackedAddU16`)

### Intentional differences
- Rust represents Eden's templated `ResultAndGE<U32>` with a concrete `ResultAndGE` containing two
  shared SSA `Value` handles.

### Unintentional differences (to fix)
- Fixed: the `PackedAddU16` opcode and both backend emitters existed, but the Rust IR builder had no
  corresponding producer method or `ResultAndGE` type. The method now emits the packed operation
  followed by its associated `GetGEFromOp` pseudo-result exactly like Eden.

### Missing items
- None for the reviewed `PackedAddU16` builder prerequisite. The neighboring packed-operation
  builders remain outside this prerequisite slice and will be audited with their owners.

### Binary layout verification
- N/A: this is an SSA builder API. A focused test verifies operand order, result opcode, GE opcode,
  and the pseudo-operation link to its producer.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/helpers.rs` vs Eden `frontend/A32/translate/impl/common.h` (`Rotate`)

### Intentional differences
- Rust receives the two-bit `SignExtendRotation` field as its numeric decoded value and represents
  Eden's typed IR values with the shared `Value` wrapper.

### Unintentional differences (to fix)
- Fixed: `Rotate` had no counterpart in the Rust `common.h` owner. Its former inline substitute in
  broad `thumb32.rs` skipped the rotate instruction when the encoded rotation was zero; the owned
  helper now always emits Eden's register read followed by ROR using `rotate * 8` and false carry.

### Missing items
- None for the reviewed `Rotate` prerequisite.

### Binary layout verification
- N/A: this helper constructs SSA. A focused test verifies the source-register read and exact
  ROR-by-zero arguments that distinguish it from the previous substitute.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (register)

### Intentional differences
- Eden generates one global first-match decoder from pattern strings; Ruzu routes the `0xFA`
  family to an ordered mask table derived from the same sixteen register entries.

### Unintentional differences (to fix)
- Fixed: none of the four register-shift entries or eight `*16`/accumulate variants had a decoded
  Rust identity, and the other eight extension identities were unreachable through the Thumb32
  family router. All sixteen identities, masks, and specialized-before-accumulate priorities now
  match `thumb32.inc`.

### Missing items
- None among the sixteen reviewed data-processing-register decoder entries.

### Binary layout verification
- N/A: the decoder classifies 32-bit instruction words. An independent pattern parser verifies all
  sixteen mask/value pairs, and focused tests decode and translate every identity.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_data_processing_register.rs` vs Eden `frontend/A32/translate/impl/{thumb32_data_processing_register.cpp,a32_translate_impl.h}`

### Intentional differences
- Eden represents the four shift member-function pointers with a C++ function-pointer type; Rust
  passes the matching `ShiftType` to one `shift_instruction` helper. Typed matcher fields are read
  from `DecodedThumb32`.

### Unintentional differences (to fix)
- Fixed: eight extension visitors lived in broad `thumb32.rs`, omitted validation, optimized away
  rotate-by-zero, and used masks in place of Eden's byte/halfword extension operations. They now
  live in the matching owner and preserve exact validation, rotate, extraction, extension,
  accumulation, and destination ordering.
- Fixed: the four register shifts and four byte-pair extensions/accumulates were absent. The shift
  helper preserves Eden's `s` read → low-byte → C read → `m` read → shift → optional flags →
  destination order; `SXTAB16` and `UXTAB16` use the verified packed-add/GE prerequisite with the
  correct asymmetric operand order.

### Missing items
- None among the sixteen reviewed declarations/implementations or the private `ShiftInstruction`
  helper.

### Binary layout verification
- N/A: this frontend constructs SSA and defines no raw-copied payload. Focused tests cover all
  visitors, validation before input reads, shift/flags/destination ordering, rotate-by-zero, and
  both packed-accumulate operand orders plus GE pseudo-results.

## 2026-08-23 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `ir/ir_emitter.h` (`PackedSelect`)

### Intentional differences
- Rust represents Eden's typed `U32` operands and result through the shared SSA `Value` wrapper.

### Unintentional differences (to fix)
- Fixed: the `PackedSelect` opcode and backend emitters existed but the Rust IR builder exposed no
  producer method. It now forwards GE, first data operand, and second data operand in Eden's order.

### Missing items
- None for the reviewed `PackedSelect` builder prerequisite.

### Binary layout verification
- N/A: this is an SSA builder API. A focused test verifies the exact opcode and three-operand
  ordering.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (miscellaneous)

### Intentional differences
- Eden generates one global matcher; Ruzu checks the register table first within the `0xFA` family
  and then an ordered table derived from the same ten miscellaneous patterns.

### Unintentional differences (to fix)
- Fixed: the ten miscellaneous identities had no Thumb32 decode path, and five were absent from the
  Rust identity enum entirely. Every identity and exact mask now matches `thumb32.inc`.

### Missing items
- None among the ten reviewed miscellaneous decoder entries.

### Binary layout verification
- N/A: the decoder classifies 32-bit instruction words. An independent pattern parser verifies all
  ten mask/value pairs, and focused tests decode and translate every identity.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_misc.rs` vs Eden `frontend/A32/translate/impl/{thumb32_misc.cpp,a32_translate_impl.h}`

### Intentional differences
- Eden's generated matcher passes typed registers; the snake-case Rust visitors extract the same
  registers from `DecodedThumb32` and use the shared SSA `Value` representation.

### Unintentional differences (to fix)
- Fixed: five partial visitors lived in broad `thumb32.rs` without validation; RBIT only reversed
  bytes, and REV16/REVSH used different expanded sequences. All ten visitors now live in the
  matching owner and preserve exact validation and IR operation ordering.
- Fixed: the four saturating scalar operations and SEL were absent. Their runtime register order,
  intermediate/final Q writes, destination ordering, GE read, and verified `PackedSelect` call now
  match Eden.

### Missing items
- None among the ten reviewed declarations/implementations.

### Binary layout verification
- N/A: this frontend constructs SSA and defines no raw-copied payload. Focused tests cover all ten
  visitors, duplicated-register validation before reads, QDADD lifecycle order, full RBIT shape,
  and SEL register/GE/select ordering.

## 2026-08-23 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `ir/ir_emitter.h` (packed parallel builders)

### Intentional differences
- Rust uses concrete `Value` and `ResultAndGE` types where Eden's declarations use typed IR
  templates. Each method remains explicit to retain one-to-one auditability.

### Unintentional differences (to fix)
- Fixed: 31 of the 32 packed builders required by `thumb32_parallel.cpp` were absent even though
  their opcodes and backend emitters existed. The full add/sub/add-sub/sub-add, signed/unsigned,
  8/16-bit, saturating, and halving surface is now exposed with Eden's exact opcode and operand
  order; all twelve GE-producing operations emit and link `GetGEFromOp`.

### Missing items
- None among the 32 packed builders used by the reviewed Thumb32 parallel owner.

### Binary layout verification
- N/A: these methods construct SSA. A comprehensive focused test invokes every builder, checks all
  opcodes and operand order, and verifies every GE pseudo-result link.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (parallel)

### Intentional differences
- Eden generates one global matcher; Ruzu routes the `0xFA` family through register, parallel, then
  miscellaneous ordered mask tables matching those source sections.

### Unintentional differences (to fix)
- Fixed: all 36 parallel identities and decode paths were absent. The exact pattern-derived masks,
  identities, and source ordering are now present between the register and miscellaneous families.

### Missing items
- None among the 36 reviewed parallel decoder entries.

### Binary layout verification
- N/A: the decoder classifies 32-bit instruction words. An independent pattern parser verifies all
  36 mask/value pairs, and focused tests decode and translate every identity.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_parallel.rs` vs Eden `frontend/A32/translate/impl/{thumb32_parallel.cpp,a32_translate_impl.h}`

### Intentional differences
- Eden passes typed matcher registers; every explicit snake-case Rust visitor extracts the same
  fields from `DecodedThumb32` and uses the shared SSA `Value` representation.

### Unintentional differences (to fix)
- Fixed: all 36 visitors were absent. Each now preserves Eden's validation, `m`-then-`n` reads,
  packed opcode/operand order, destination write, and GE write where applicable.
- Fixed: QASX/QSAX/UQASX/UQSAX now preserve the explicit half extraction and signed/unsigned
  extension order, add/sub orientation, saturation pseudo-results, low/high packing, and final
  destination write without hiding the four visitors behind a generic dispatcher.

### Missing items
- None among the 36 reviewed declarations/implementations.

### Binary layout verification
- N/A: this frontend constructs SSA and defines no raw-copied payload. Focused tests cover all 36
  visitors, validation before reads, GE lifecycle ordering, and crossed-half saturation expansion.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb16.rs` vs Eden `frontend/A32/translate/impl/thumb16.cpp` (`thumb16_BX`)

### Intentional differences
- Rust exposes the snake-case visitor within the translation module so `thumb32_BXJ` can preserve
  Eden's direct delegation without duplicating the branch lifecycle.

### Unintentional differences (to fix)
- Fixed: the Rust visitor previously extracted its register from a complete Thumb16 decode and
  omitted Eden's non-final-IT-block rejection. It now owns the decoded `Reg` operand, validates IT
  state before reading that register, then preserves descriptor update, `BXWritePC`, and terminal
  selection order.

### Missing items
- None for the reviewed `thumb16_BX` prerequisite.

### Binary layout verification
- N/A: this visitor constructs SSA. A focused test verifies rejection before the source-register
  read in a non-final IT instruction.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (control)

### Intentional differences
- Eden generates one global first-match decoder from pattern strings; Rust keeps the same ordered
  control entries as explicit mask/value tuples in its branch-and-control decoder.

### Unintentional differences (to fix)
- Fixed: `BXJ` was absent, `CLREX` was unreachable, and a broad classifier accepted reserved
  encodings as hints, barriers, `MSR`, or `MRS`. All thirteen control identities now use Eden's
  exact ordered masks, and the `MRS_reg` identity retains its upstream name.

### Missing items
- None among the thirteen reviewed miscellaneous-control decoder entries; UDF and branch entries
  remain in their following upstream order.

### Binary layout verification
- N/A: the decoder classifies 32-bit instruction words. Focused tests cover every exact pattern,
  variable field, and reserved near-match rejection.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_control.rs` vs Eden `frontend/A32/translate/impl/{thumb32_control.cpp,a32_translate_impl.h}`

### Intentional differences
- Typed matcher fields are extracted from `DecodedThumb32`; Rust represents terminal and SSA
  values with its existing enums while preserving Eden's operation sequence.

### Unintentional differences (to fix)
- Fixed: `thumb32_BXJ` was missing. It now rejects `PC` before any register read and delegates to
  the verified `thumb16_BX` owner exactly as Eden does.

### Missing items
- None among the fourteen reviewed control declarations and implementations.

### Binary layout verification
- N/A: this frontend constructs SSA and defines no raw-copied payload. Focused tests verify both
  the `PC` rejection and the delegated R14 `PopRSBHint` lifecycle.

## 2026-08-23 — `src/rdynarmic/src/interface/optimization_flags.rs` vs Eden `interface/optimization_flags.h`

### Intentional differences
- Rust uses a transparent `u32` newtype with standard bitwise traits instead of a scoped C++ enum
  plus free operator overloads. `contains` and `bits` expose the checks needed by existing Rust
  consumers. `jit_config` temporarily re-exports the type while its shared configuration is split.

### Unintentional differences (to fix)
- Fixed: `OptimizationFlag` lived in the unrelated shared `jit_config.rs` owner and omitted Eden's
  `CodeSpeed` and `DisableVerification` values. The complete flag inventory and constants now live
  in the matching interface module with exact `u32` values.

### Missing items
- None for the reviewed flag values, constants, and bitwise operations.

### Binary layout verification
- PASS: `#[repr(transparent)]` preserves Eden's `std::uint32_t` underlying representation; focused
  tests verify every value plus four-byte size and alignment.

## 2026-08-23 — `src/rdynarmic/src/interface/a32/config.rs` vs Eden `interface/A32/config.h` (`Exception`)

### Intentional differences
- Rust exposes `as_u32` for the existing SSA immediate boundary. `frontend/a32/types.rs` retains a
  compatibility re-export so translation owners can migrate independently of this ownership move.

### Unintentional differences (to fix)
- Fixed: `A32::Exception` lived in `frontend/a32/types.rs` despite Eden owning it in the public
  configuration interface. The complete thirteen-value enum now lives beside the A32 configuration.

### Missing items
- None for the reviewed A32 exception inventory. `UserCallbacks` was restored in the later
  2026-08-24 configuration-owner slice; `UserConfig` remains outstanding.

### Binary layout verification
- PASS: `repr(i32)` matches the default C++ scoped-enum underlying representation used by Eden;
  focused tests verify every discriminant plus four-byte size and alignment.

## 2026-08-23 — `src/rdynarmic/src/interface/a64/config.rs` vs Eden `interface/A64/config.h` (public enums)

### Intentional differences
- Rust applies normal PascalCase spelling to Eden's `VA` acronym in variant names.
  `frontend/a64/types.rs` temporarily re-exports `Exception` for existing translation consumers.

### Unintentional differences (to fix)
- Fixed: `A64::Exception` lived in `frontend/a64/types.rs` and used an eight-byte representation.
  It now lives in the public configuration owner with Eden's ten values and default four-byte
  scoped-enum representation.
- Fixed: the already-owned data- and instruction-cache enums used `repr(u8)` even though Eden uses
  the default C++ scoped-enum representation. Both now use `repr(i32)`.

### Missing items
- None for the three reviewed A64 public enums. `UserCallbacks` was restored in the later
  2026-08-24 configuration-owner slice; `UserConfig` remains outstanding.

### Binary layout verification
- PASS: focused tests verify all discriminants and four-byte size/alignment for `Exception`,
  `DataCacheOperation`, and `InstructionCacheOperation`.

## 2026-08-23 — `src/rdynarmic/src/jit_config.rs` vs Eden `interface/{A32,A64}/config.h` (`UserCallbacks` exclusive surface)

### Intentional differences
- Rust still exposes one temporary shared callback trait while the interrupted configuration split
  is completed; the exact prerequisite and resume point are recorded in the project-local state
  file, which is excluded from commits.
- `set_halt_reason_ptr`, `set_pc_ptr`, and `set_upper_location_descriptor_ptr` are Rust ownership
  adapters used after the boxed callback and JIT state acquire stable addresses. They do not add
  guest-visible callback events.

### Unintentional differences (to fix)
- Fixed: the shared trait invented `exclusive_read_8/16/32/64/128` and `exclusive_clear` host
  callbacks. Neither Eden callback interface declares them: exclusive loads use `MemoryRead*`, and
  `Jit::ClearExclusiveState` only resets the backend-owned reservation state.
- The remaining shared A32/A64 callback trait still exposes each architecture's members to the
  other architecture. The active prerequisite slice will replace it with the two upstream-owned
  traits before the `UserConfig` split resumes.

### Missing items
- The architecture-owned traits were added in the later 2026-08-24 configuration-owner slice.
  Runtime consumers still use this shared trait through explicit compatibility implementations.

### Binary layout verification
- N/A: Rust trait objects are not copied into guest or JIT binary payloads; this review verifies
  method inventory and call ownership.

## 2026-08-23 — `src/rdynarmic/src/{jit.rs,backend/common/a32_callbacks.rs}` vs Eden `backend/x64/{a32_emit_x64_memory.cpp,a64_emit_x64_memory.cpp,a32_interface.cpp,a64_interface.cpp}`

### Intentional differences
- Rust trampolines make the callback target explicit and share mechanical reservation bookkeeping
  with the in-progress arm64 backend. The generated-code callback table retains internal fields
  named `exclusive_read_*`; those fields target the normal `memory_read_*` host methods just as
  Eden's emitters instantiate exclusive-read helpers with `UserCallbacks::MemoryRead*`.

### Unintentional differences (to fix)
- Fixed: x64 exclusive-read trampolines previously dispatched through non-upstream exclusive-read
  host methods, and clear dispatched an extra host event. They now read through `memory_read_*` and
  clear only `jit_state.exclusive_state`, preserving Eden's exact callback and lifecycle behavior.

### Missing items
- None for the reviewed x64 exclusive-read and clear callback behavior.

### Binary layout verification
- PASS: no layout changed. Focused tests verify that reads record the expected value and that clear
  resets only the reservation-state flag without modifying its stored values.

## 2026-08-23 — `src/rdynarmic/src/backend/arm64/{a32_address_space.rs,a64_address_space.rs}` vs Eden `backend/arm64/{a32_address_space.cpp,a64_address_space.cpp}`

### Intentional differences
- Rust uses explicit callback-context trampolines instead of Eden's generated devirtualized call
  trampolines; the target callback and monitor ordering remain the same.

### Unintentional differences (to fix)
- Fixed: arm64 exclusive-read trampolines called invented `exclusive_read_*` host methods. Both
  architectures now route the monitor closure and local fallback through the ordinary
  `memory_read_*` callbacks selected by Eden's `EmitExclusiveReadCallTrampoline` instantiations.

### Missing items
- None for the reviewed arm64 exclusive-read callback selection.

### Binary layout verification
- PASS: callback-context and JIT-state layouts are unchanged; this slice changes only the selected
  trait method.

## 2026-08-23 — `src/rdynarmic/src/{ir/a32_emitter.rs,frontend/a32/translate/{thumb16.rs,multiply.rs,thumb32_data_processing_modified_immediate.rs,thumb32_data_processing_shifted_register.rs,thumb32_data_processing_register.rs}}` vs Eden `frontend/A32/{a32_ir_emitter.{h,cpp},translate/impl/{thumb16.cpp,multiply.cpp,thumb32_data_processing_modified_immediate.cpp,thumb32_data_processing_shifted_register.cpp,thumb32_data_processing_register.cpp,a32_translate_impl.h}}`

### Intentional differences
- Rust exposes `nzcv_from` beside the A32 `nz_from` adapter because it cannot inherit Eden's
  generic `IR::IREmitter::NZCVFrom`; both methods emit the exact upstream opcode and keep the
  inherited C++ call surface visible to translation owners.

### Unintentional differences (to fix)
- Fixed: `A32IREmitter::nz_from` emitted `GetNZCVFromOp` instead of Eden's `GetNZFromOp`. Logical
  Thumb32 instructions could therefore consume stale host flags because their result operations do
  not own a `GetNZCVFromOp` pseudo-operation. Logical and shift visitors now use `GetNZFromOp`,
  while all arithmetic `SetCpsrNZCV` sites explicitly use `GetNZCVFromOp`.
- Fixed: the same pre-existing extractor mismatch affected Thumb16 logical/shift visitors and the
  six flag-setting ARM multiply visitors. Their `SetCpsrNZ`/`SetCpsrNZC` paths now match Eden's
  `NZFrom` calls.

### Missing items
- None for the reviewed A32 N/Z versus N/Z/C/V extraction surface and its affected visitors.

### Binary layout verification
- N/A: this slice changes SSA opcode selection only. Focused tests verify both helper opcodes,
  logical-versus-arithmetic Thumb16 selection, flag-setting multiply selection, and the Thumb32
  logical/arithmetic instruction streams. An x64 JIT regression executes Thumb32 `TST.W` followed
  by `BEQ` and verifies that the branch observes N/Z from the logical result.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/vfp.rs` vs Eden `frontend/A32/translate/impl/{vfp.cpp,a32_translate_impl.h}` (VFP memory transfers)

### Intentional differences
- Eden's decoder exposes separate A1/A2 VSTM and VLDM visitors. Rust currently decodes each family
  to one identity and selects the single- or double-register path from `sz` in the same `vfp.rs`
  owner; the source comment records this structural adaptation, while validation, ordering, and IR
  operations follow the corresponding Eden path.
- ARM condition handling remains in Rust's block-level conditional translator instead of being
  repeated inside every VFP visitor; the memory-transfer bodies execute only after that guard.

### Unintentional differences (to fix)
- Fixed: VPUSH, VPOP, VLDR, VSTR, VSTM, and VLDM used `AccType::Normal`; every 32-bit transfer now
  uses Eden's `AccType::ATOMIC`.
- Fixed: double-register transfers omitted Eden's E-flag word swap. BE-8 loads now exchange the two
  individually byte-reversed words before packing, and stores exchange low/high words before the
  emitter performs each word's endian conversion.
- Fixed: VPOP updated SP after its loads, and VSTM/VLDM updated Rn after their transfers. All three
  now perform writeback before memory accesses in Eden's exact order.
- Fixed: empty/out-of-range register lists returned success, VSTM/VLDM omitted addressing and PC
  validation, and VSTR did not use the aligned architectural PC base. These checks and the aligned
  base now match the corresponding Eden visitors.

### Missing items
- None for the reviewed VPUSH, VPOP, VLDR, VSTR, VSTM A1/A2, and VLDM A1/A2 behavior.

### Binary layout verification
- N/A: these visitors construct SSA and define no raw-copied payload. Focused tests verify atomic
  access tags, BE-8 double-word dependency order, pre-access writeback, and unpredictable empty
  lists across all six memory-transfer families.

## 2026-08-23 — `src/rdynarmic/src/frontend/a64/translate/simd_crypto_four_register.rs` vs Eden `frontend/A64/translate/impl/{simd_crypto_four_register.cpp,impl.h}`

### Intentional differences
- Rust extracts `Vm`, `Va`, `Vn`, and `Vd` from `DecodedInst` inside each visitor because its
  decoder dispatch passes a decoded instruction rather than Eden's typed visitor arguments. The
  register read order, IR operation nesting, and destination write remain identical.
- Rust's `add_32` and `rotate_right_32` builders take an explicit false carry input; this is the IR
  builder representation of Eden's non-flag-setting `Add` and `RotateRight` calls.

### Unintentional differences (to fix)
- Fixed: EOR3, BCAX, and SM3SS1 decoded successfully but fell through to the non-upstream
  `interpret_this_instruction` terminal. Their implementations and dispatch now live in the
  corresponding four-register crypto owner and emit Eden's exact IR sequence.

### Missing items
- None for the three visitors owned by Eden's `simd_crypto_four_register.cpp`.

### Binary layout verification
- N/A: these visitors construct SSA and define no raw-copied payload. Focused tests verify decoder
  identity, distinct source-register ownership, exact EOR3/BCAX opcode order, and SM3SS1 lane,
  rotation, addition, and destination-write shape.

## 2026-08-23 — `src/rdynarmic/src/frontend/a64/translate/simd_crypto_three_register.rs` vs Eden `frontend/A64/translate/impl/{simd_crypto_three_register.cpp,impl.h}`

### Intentional differences
- Rust's file-local `Sm3TtVariant`, `sm3tt1`, and `sm3tt2` mirror Eden's anonymous-namespace enum
  and helpers. Each public visitor extracts the typed register and two-bit immediate operands from
  `DecodedInst` before forwarding them to the matching helper.
- Rust's arithmetic and rotation builders take an explicit false carry input; operation order and
  data dependencies match Eden's non-flag-setting nested IR expressions.

### Unintentional differences (to fix)
- Fixed: SM3TT1A, SM3TT1B, SM3TT2A, and SM3TT2B decoded successfully but fell through to the
  non-upstream `interpret_this_instruction` terminal. Their helpers, visitor ownership, and
  dispatch now match Eden.

### Missing items
- None for the four visitors owned by Eden's `simd_crypto_three_register.cpp`.

### Binary layout verification
- N/A: these visitors construct SSA and define no raw-copied payload. Focused tests verify all
  four decoder identities, D/M/N register order, non-zero two-bit lane extraction, four result-lane
  writes, and the final destination write.

## 2026-08-23 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `ir/ir_emitter.h` (generic extension and signed-to-unsigned saturated-shift builders)

### Intentional differences
- Rust resolves an instruction-backed `Value` through `Block::inst_real_return_type` because its
  SSA references do not carry Eden's `UAny::GetType()` information inline. Immediate and
  instruction inputs nevertheless select the same extension opcode.
- Unsupported input types panic instead of reaching Eden's `UNREACHABLE`; both represent an
  internal IR construction error rather than guest-visible validation.

### Unintentional differences (to fix)
- Fixed: `zero_extend_to_quad` directly emitted `ZeroExtendLongToQuad` for every input, omitting
  Eden's preceding byte, half, or word extension. The generic sign/zero extension-to-word/long
  helpers and indeterminate aliases now mirror the complete reviewed upstream helper family.
- Fixed: `vector_signed_saturated_shift_left_unsigned` converted its U8 shift amount to a broadcast
  U128 operand. It now passes the exact U8 immediate required by Eden's builder and opcode
  signature.

### Missing items
- None for the reviewed generic extension helpers and
  `VectorSignedSaturatedShiftLeftUnsigned` builder.

### Binary layout verification
- N/A: this slice changes SSA builder selection and operand typing only. Focused tests verify all
  narrow-to-long opcode choices, identity behavior for already-wide values, the two-stage
  narrow-to-quad chain, and the saturated-shift U8 operand without an extra broadcast.

## 2026-08-23 — `src/rdynarmic/src/frontend/a64/translate/visitor.rs` vs Eden `frontend/A64/translate/impl/impl.cpp` (`V_scalar` write adapter)

### Intentional differences
- Rust retains a runtime U128 assertion for the 128-bit path because its `Value` type does not
  encode Eden's compile-time `UAnyU128` constraint. Valid translated instructions observe the same
  direct `SetQ` behavior.

### Unintentional differences (to fix)
- Fixed: scalar writes manually selected byte/half/word extensions from the requested data size
  before calling the formerly U64-only quad helper. They now call the corrected generic
  `zero_extend_to_quad(value)` exactly once, matching Eden's `V_scalar` implementation and the
  value's actual IR type.

### Missing items
- None for the reviewed `V_scalar(bitsize, vec, value)` write behavior.

### Binary layout verification
- N/A: this adapter constructs SSA and defines no raw-copied payload. The generic extension tests
  cover the resulting U8/U16/U32/U64-to-U128 chains.

## 2026-08-23 — `src/rdynarmic/src/frontend/a64/translate/simd_scalar_shift_by_immediate.rs` vs Eden `frontend/A64/translate/impl/{simd_scalar_shift_by_immediate.cpp,impl.h}`

### Intentional differences
- Rust visitors extract `immh`, `immb`, `Vn`, and `Vd` from `DecodedInst` before forwarding them
  to file-local helpers; Eden's decoder passes those typed operands directly.
- Rust uses `Fpcr::rmode()` and passes the resulting enum discriminant to its IR builders, which
  store rounding mode as U8 metadata. This preserves Eden's FPCR-controlled conversion mode.
- The unused `Narrowing::Truncation` variant is retained with a local dead-code allowance because
  it belongs to Eden's helper enum even though this scalar instruction family currently invokes
  only its two saturating modes.
- Rust arithmetic builders take an explicit false carry input for Eden's non-flag-setting `Add`
  operations.

### Unintentional differences (to fix)
- Fixed: SSRA, SRSHR, SRSRA, SQSHL-immediate, SQSHRN, USRA, URSHR, URSRA, SRI, SLI, SQSHLU,
  UQSHL-immediate, SQSHRUN, and UQSHRN decoded but fell through to the non-upstream interpreter
  terminal. All 14 now dispatch to their matching owner.
- Fixed: the existing SSHR, USHR, SHL, FCVTZS, FCVTZU, SCVTF, and UCVTF visitors bypassed Eden's
  file-local helper boundaries. The integer and FP paths now share the exact upstream helpers,
  validation, operation ordering, and accumulation behavior.
- Fixed: those existing integer and FP visitors applied `VectorGetElement` a second time to the
  scalar returned by `v_scalar_read`. They now consume `V_scalar` directly as Eden does.

### Missing items
- None for the 21 visitors and six file-local helpers implemented by the reviewed Eden source.

### Binary layout verification
- N/A: these visitors construct SSA and define no raw-copied payload. Focused tests cover all 21
  decoder identities, the six saturation opcodes, rounding/accumulation operation counts, single
  scalar source extraction, and reserved-value handling without interpreter fallback.

## 2026-08-23 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `ir/ir_emitter.h` (scalar saturated-arithmetic builders)

### Intentional differences
- Rust resolves instruction-backed operand types through the block arena because `Value::Inst`
  does not carry Eden's `UAny::GetType()` inline. It asserts equal operand types before selecting
  the same width-specific opcode.
- Unsupported widths panic instead of returning Eden's empty `UAny` or reaching `UNREACHABLE`;
  both are internal builder misuse and cannot be produced by a valid frontend visitor.

### Unintentional differences (to fix)
- Fixed: scalar signed/unsigned saturated add and subtract opcodes, plus signed saturating doubling
  multiply-high, existed in the IR and backends without their upstream builder methods. All five
  type-dispatching builders now live beside Eden's other saturated-arithmetic helpers.

### Missing items
- None for the reviewed scalar saturated-arithmetic builder family.

### Binary layout verification
- N/A: these builders construct SSA and define no raw-copied payload. A focused test verifies all
  18 valid width/operation opcode selections in upstream order.

## 2026-08-23 — `src/rdynarmic/src/frontend/a64/translate/simd_scalar_three_same.rs` vs Eden `frontend/A64/translate/impl/{simd_scalar_three_same.cpp,impl.h}`

### Intentional differences
- Rust visitors extract typed operands from `DecodedInst`; the three file-local helper boundaries,
  enums, validations, and visitor responsibilities otherwise mirror Eden.
- Rust passes `fpcr_controlled=true` explicitly to FP vector comparison builders and false/true
  carry inputs to non-flag-setting add/sub builders. These are required by Rust's lower-level IR
  API and represent Eden's implicit builder behavior.
- `SQRSHL_1` and `UQRSHL_1` remain header-only declarations in the reviewed Eden snapshot: they
  have neither decoder identities nor C++ definitions, so Rust does not invent unreachable visitor
  methods for them.

### Unintentional differences (to fix)
- Fixed: SQADD, SQSUB, SQDMULH, SQRDMULH, UQADD, UQSUB, SQSHL-register, SRSHL,
  UQSHL-register, URSHL, FMULX, FACGE, and FACGT decoded but fell through to the non-upstream
  interpreter terminal. All 13 now dispatch to their matching owner.
- Fixed: ADD and SUB applied `VectorGetElement` twice to values already returned by `V_scalar`.
  They now perform one scalar extraction per source as Eden does.
- Fixed: scalar integer comparisons, CMTST, SSHL, USHL, and scalar FP comparisons used
  `V_scalar` inputs where Eden uses vector `V(32/64)`. Their GetS/GetD/GetQ selection, vector
  operation shape, scalar result extraction, and SetD/SetQ ordering now match upstream.
- Fixed: the file used invented argument-decoding helpers and stored a register inside the
  comparison-variant enum. The enums and three file-local helpers now have Eden's ownership and
  responsibilities.

### Missing items
- None for the 37 visitors and three file-local helpers defined by the reviewed C++ source.

### Binary layout verification
- N/A: these visitors construct SSA and define no raw-copied payload. Focused tests cover all 13
  restored identities, scalar saturated opcode selection, corrected scalar/vector operand shapes,
  and reserved rounding-shift sizes without interpreter fallback.

## 2026-08-23 — `src/rdynarmic/src/frontend/a64/translate/simd_scalar_two_register_misc.rs` vs Eden `frontend/A64/translate/impl/{simd_scalar_two_register_misc.cpp,impl.h}`

### Intentional differences
- Rust visitors extract typed operands from `DecodedInst`; the two helper enums and three
  file-local helper responsibilities otherwise mirror Eden.
- Rust passes `fpcr_controlled=true` explicitly to FP vector comparisons and an explicit true
  carry input to non-flag-setting subtraction. These values are implicit defaults in Eden's IR
  builder API.
- Rust's `SaturatedNarrow` equivalent accepts the upstream narrowing operation as a lifetime-bound
  generic callable because a C++ member-function pointer cannot be represented directly across
  Rust emitter lifetimes. The selected IR methods and call ordering are unchanged.

### Unintentional differences (to fix)
- Fixed: FCMLE, FCMLT, FCVTXN, SQABS, SQNEG, SUQADD, and USQADD decoded but fell through to the
  temporary interpreter terminal. All seven now dispatch to their matching file owner.
- Fixed: scalar FP zero comparisons read `V_scalar` rather than Eden's `V(datasize)`, and the LE/LT
  comparison variants were absent. Reads, operand inversion, comparison opcodes, and scalar result
  extraction now match Eden.
- Fixed: ABS and NEG applied `VectorGetElement64` twice to values already returned by `V_scalar`.
  Each now performs exactly one scalar source extraction.
- Fixed: the conversion family encoded rounding modes as local integer constants and used invented
  helper ownership. It now uses `FP::RoundingMode`'s Rust counterpart, `FPCR::RMode`, and Eden's
  `ScalarFPConvertWithRound` boundary.
- Fixed: the saturated narrowing family used an invented enum dispatcher and a 128-bit scalar read.
  It now passes the matching IR operation to `SaturatedNarrow` and reads `V_scalar(2 * esize)`.

### Missing items
- None for the 34 visitors, two file-local enums, and three file-local helpers defined by the
  reviewed C++ source.

### Binary layout verification
- N/A: these visitors construct SSA and define no raw-copied payload. Focused tests cover all seven
  restored decoder identities, comparison operand ordering, ToOdd conversion metadata, reserved
  FCVTXN handling, scalar extraction counts, saturating accumulator reads, and narrowing opcodes.

## 2026-08-23 — `src/rdynarmic/src/frontend/a64/translate/simd_scalar_x_indexed_element.rs` vs Eden `frontend/A64/translate/impl/{simd_scalar_x_indexed_element.cpp,impl.h}`

### Intentional differences
- Rust visitors extract Eden's typed immediate and vector-register operands from `DecodedInst`.
  `CombineScalar`, `ExtraBehavior`, `MultiplyByElement`, and `MultiplyByElementHalfPrecision`
  otherwise remain file-local with the same responsibilities and branch ordering.
- Eden uses `ASSERT` for the unreachable half-precision plain/extended multiply modes; Rust uses
  `assert!` because the two decoded half-precision visitors select only accumulate or subtract.
- Six declarations in Eden's `impl.h` (`SQDMLAL_elt_1`, `SQDMLSL_elt_1`, `FMUL_elt_1`,
  `SQRDMLAH_elt_1`, `SQRDMLSH_elt_1`, and `FMULX_elt_1`) have no decoder identities or C++
  definitions in the reviewed snapshot. Rust does not invent unreachable implementations.

### Unintentional differences (to fix)
- Fixed: SQDMULL, SQDMULH, and SQRDMULH scalar-by-element identities decoded but fell through to
  the temporary interpreter terminal. All three now preserve Eden's size validation, combined
  index/register selection, scalar/vector operand shapes, saturation opcodes, and destination form.
- Fixed: the existing floating-point helpers used `V_scalar(idxdsize, Vm)` for the indexed source,
  then passed that scalar through `VectorGetElement`. They now read `V(idxdsize, Vm)` as Eden does.
- Fixed: the three upstream helpers lived as methods on `TranslatorVisitor`, obscuring their
  anonymous-namespace ownership, and `CombineScalar` was open-coded only in the floating-point
  path. All three helpers now have their matching file-local ownership.

### Missing items
- None for the nine visitors and three file-local helpers defined by the reviewed C++ source.

### Binary layout verification
- N/A: these visitors construct SSA and define no raw-copied payload. Focused tests cover both
  `CombineScalar` layouts, all three restored identities at 16/32 bits, reserved 8/64-bit sizes,
  indexed vector-read shape, and the existing floating-point family.

## 2026-08-23 — `src/rdynarmic/{build.rs,src/frontend/a64/decoder.rs}` vs Eden `frontend/A64/decoder/{a64.h,a64.inc}` (trailing instruction comments)

### Intentional differences
- Eden consumes `a64.inc` through C++ macros, while Rust's build script parses the same pattern
  table and generates its enum and two-tier lookup table. The parser therefore locates the closing
  `INST(...)` parenthesis explicitly before processing its three fields.

### Unintentional differences (to fix)
- Fixed: Rust required an active `INST(...)` line to end exactly at `)`. The trailing ARM-version
  comments on CFINV, RMIF, XAFlag, and AXFlag caused all four patterns to be silently omitted from
  the generated decoder, unlike Eden's preprocessor inclusion.

### Missing items
- None for active A64 instruction patterns with trailing comments in the reviewed table.

### Binary layout verification
- N/A: decoder patterns are generated metadata, not raw-copied payloads. A focused test verifies
  the four affected encodings decode to their exact upstream instruction identities.

## 2026-08-23 — `src/rdynarmic/src/frontend/a64/translate/system_flag_{manipulation,format}.rs` vs Eden `frontend/A64/translate/impl/system_flag_{manipulation,format}.cpp` and `impl.h`

### Intentional differences
- Rust visitors accept an unused `DecodedInst` for Eden's operand-free CFINV, XAFlag, and AXFlag
  because all dispatch methods share the generated decoder interface.
- Rust passes an explicit false carry input to 32-bit logical shifts. Eden's generic IR builder
  supplies the same non-flag-setting default implicitly.
- SETF8 and SETF16 are declarations only in Eden's `impl.h`; their decoder table entries are
  commented out and the reviewed snapshot provides no C++ definitions, so Rust does not invent
  unreachable implementations.

### Unintentional differences (to fix)
- Fixed: CFINV, RMIF, XAFlag, and AXFlag had active decoder entries but no matching Rust owner files
  or visitor methods. All four now dispatch and preserve Eden's exact raw-NZCV masks, rotations,
  conditional fast paths, boolean compositions, and final write ordering.

### Missing items
- None for the four visitors defined by the two reviewed C++ sources.

### Binary layout verification
- N/A: these visitors construct SSA and define no raw-copied payload. Focused tests verify CFINV's
  carry mask, RMIF's zero/full/partial mask paths, both flag-format operation shapes, raw-NZCV
  writes, and absence of interpreter fallback.

## 2026-08-23 — `src/rdynarmic/src/frontend/a64/translate/simd_sha512.rs` vs Eden `frontend/A64/translate/impl/{simd_sha512.cpp,impl.h}`

### Intentional differences
- Rust visitors extract typed decoder operands from `DecodedInst`; all ten methods remain in the
  matching file owner.
- Rust's 32-bit rotate and 64-bit add builders require explicit carry inputs, so two mechanical
  file-local adapters supply Eden's implicit false carry without changing operation ordering.
- Eden's two lambdas inside `SHA512Hash` are represented by file-local Rust functions because
  simultaneous closures borrowing the mutable emitter cannot coexist. They retain the same
  captured hash-part and upper/lower-Y inputs and are called at the same points.

### Unintentional differences (to fix)
- Fixed: SHA512SU0, SHA512SU1, SHA512H, SHA512H2, RAX1, XAR, SM3PARTW1, SM3PARTW2, SM4E, and
  SM4EKEY decoded but fell through to the temporary interpreter terminal. All ten now preserve
  Eden's exact register-read order, rotations, boolean functions, nested additions, lane updates,
  four-round SM4 loop, substitution-box calls, and destination writes.

### Missing items
- None for the ten visitors, two helper enums, and five principal file-local helpers defined by the
  reviewed C++ source.

### Binary layout verification
- N/A: these visitors construct SSA and define no raw-copied payload. Focused tests cover all ten
  decoder identities, SHA-512 choice-versus-majority IR shapes, and the four SM4 rounds with four
  S-box substitutions per round.

## 2026-08-23 — `src/rdynarmic/src/frontend/a64/translate/simd_shift_by_immediate.rs` vs Eden `frontend/A64/translate/impl/{simd_shift_by_immediate.cpp,impl.h}`

### Intentional differences
- Rust visitors extract Eden's typed immediate and vector-register operands from `DecodedInst`;
  the six anonymous-namespace helpers remain file-local with the same responsibilities.
- Rust passes `fpcr_controlled=true` explicitly to the four fixed-point vector conversion IR
  builders. Eden's builder API supplies the same value as its default argument.
- Rust computes Eden's `mcl::bit::ones<u64>(esize)` masks with an equivalent bounded `u64` shift.

### Unintentional differences (to fix)
- Fixed: SQSHL, SQSHLU, and UQSHL immediate, SRI, SLI, SCVTF, UCVTF, FCVTZS, and FCVTZU decoded but
  fell through to the temporary interpreter terminal. All nine now preserve Eden's validation,
  element-size and immediate calculations, source/destination reads, IR ordering, and writes.
- Fixed: the four existing shift helpers were methods on `TranslatorVisitor`, obscuring their
  anonymous-namespace ownership. They now have file-local ownership alongside the two restored
  saturating-shift and floating-conversion helpers.

### Missing items
- None for the 28 visitors and six file-local helpers defined by the reviewed C++ source.

### Binary layout verification
- N/A: these visitors construct SSA and define no raw-copied payload. Focused tests cover all nine
  restored identities, all three saturation modes, both shift-insert directions, fixed-point
  signedness/direction/fraction bits/rounding metadata, and absence of interpreter fallback.

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/simd_three_same.rs` vs Eden `frontend/A64/translate/impl/{simd_three_same.cpp,impl.h}`

### Intentional differences
- Rust visitors extract Eden's typed immediate and vector-register operands from `DecodedInst`.
  Three tuple helpers mechanically share identical operand extraction among visitors; they do not
  own IR behavior or merge differing validation rules.
- Rust passes `fpcr_controlled=true` explicitly to FP vector builders. Eden's builder API supplies
  the same value as its default argument.
- `FPPairedMinMax` selects its four Rust emitter methods through a file-local enum because C++
  member-function pointers do not map directly across Rust emitter lifetimes. Lane iteration,
  operation selection, and writes remain in the matching helper and preserve Eden's order.
- `FMLAL_vec_{1,2}`, `FMLSL_vec_{1,2}`, `SQRSHL_2`, and `UQRSHL_2` are declarations only in Eden's
  `impl.h`; their decoder entries are commented out and the reviewed source has no definitions, so
  Rust does not invent unreachable visitors.

### Unintentional differences (to fix)
- Fixed: FP16 FMLA/FMLS, PMUL, SQDMULH/SQRDMULH, SQSHL/SRSHL, and UQSHL/URSHL decoded but fell
  through to the temporary interpreter terminal. All nine now preserve Eden's validation,
  vector reads, IR operation selection, FP negation/multiply-add order, and destination writes.
- Fixed: the 12 anonymous-namespace helpers were implemented as visitor methods or replaced with
  broader invented dispatchers. Their ownership and responsibilities now mirror Eden; unsigned
  UABA/UABD behavior is once again owned directly by those visitors.
- Fixed: SMAX, SMIN, UMAX, and UMIN accepted `size=0b11` when Q was set because they shared the
  looser validation used by ADD and comparisons. Eden reserves size 3 for all four min/max visitors.
- Fixed: CMEQ, CMGE, CMHS, BIC, and ORN omitted the explicit `VectorZeroUpper` emitted by their
  visitor before Eden's common 64-bit destination write performs its own upper-zero operation.

### Missing items
- None for the 84 visitors and 12 file-local helpers defined by the reviewed C++ source.

### Binary layout verification
- N/A: these visitors construct SSA and define no raw-copied payload. Focused tests cover all nine
  restored identities, reserved size combinations, all restored helper operation families, the
  corrected min/max validation, and explicit lower-vector zeroing order.

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/simd_three_different.rs` vs Eden `frontend/A64/translate/impl/{simd_three_different.cpp,impl.h}`

### Intentional differences
- Rust visitors extract Eden's typed immediate and vector-register operands from `DecodedInst`;
  all four anonymous-namespace helpers remain file-local with matching responsibilities.
- Eden's `LongOperation` lambda for signed/unsigned extension is written as two explicit Rust
  matches because two closures borrowing the mutable emitter cannot coexist. Operand read,
  extension, arithmetic, and destination-write ordering are unchanged.
- `SQDMLAL_vec_2` and `SQDMLSL_vec_2` are declarations only in Eden's `impl.h`; their decoder
  entries are commented out and the reviewed source has no definitions, so Rust does not invent
  unreachable visitors.

### Unintentional differences (to fix)
- Fixed: PMULL and SQDMULL decoded but fell through to the temporary interpreter terminal. Both now
  preserve Eden's exact reserved size sets, Q-selected source halves, polynomial/saturating IR
  operations, and 128-bit destination writes.
- Fixed: the four anonymous-namespace helpers lived as visitor methods. Their ownership now matches
  Eden without changing the existing long, wide, multiply-long, or absolute-difference behavior.

### Missing items
- None for the 20 visitors and four file-local helpers defined by the reviewed C++ source.

### Binary layout verification
- N/A: these visitors construct SSA and define no raw-copied payload. Focused tests cover both
  restored identities at every encoded size, reserved combinations, selected lower/upper halves,
  and absence of interpreter fallback.

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/simd_three_same_extra.rs` vs Eden `frontend/A64/translate/impl/{simd_three_same_extra.cpp,impl.h}`

### Intentional differences
- Rust visitors extract Eden's typed immediate and vector-register operands from `DecodedInst`;
  the `DotProduct` helper and its extension-function parameter remain file-local.
- Rust gives the extension-function pointer an explicit emitter lifetime because a C++ member
  pointer has no direct Rust representation. It still selects the matching generic IR method at
  the same two visitor call sites.
- FCMLA and FCADD use initialized tuple values for each rotation because Rust forbids Eden's
  declaration-then-assignment form. Each branch emits the same element reads and negations before
  the same multiply-add or add operations.

### Unintentional differences (to fix)
- Fixed: SDOT, UDOT, FCMLA, and FCADD decoded but fell through to the temporary interpreter
  terminal. All four now preserve Eden's size validation, vector widths, lane iteration,
  signed/unsigned extension, accumulation, complex rotations, FP operation ordering, and writes.

### Missing items
- None for the four visitors and the file-local dot-product helper defined by the reviewed C++
  source and header.

### Binary layout verification
- N/A: these visitors construct SSA and define no raw-copied payload. Focused tests cover both dot
  product signedness modes, rejected sizes, all FCMLA/FCADD rotations, 32/64-bit FP operation
  selection, and absence of interpreter fallback.

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/a64_translate.rs` vs Eden `frontend/A64/translate/a64_translate.{h,cpp}` and backend translation call sites

### Intentional differences
- Rust's block-level `translate` allocates and returns its `Block`; Eden receives a reset block by
  mutable reference from each backend. Instruction loop ordering, terminal assertion, cycle count,
  single-step link, and end-location update remain the same.
- Rust represents `MemoryReadCodeFuncType` as a borrowed `dyn Fn` and uses `Option::map` for the
  single-instruction decoder result. Both preserve Eden's optional-code and decoder semantics.
- The implementation lives in its own `a64_translate.rs`; `translate/mod.rs` only declares and
  re-exports the owner, matching Rust module mechanics without retaining behavior in the dispatcher.

### Unintentional differences (to fix)
- Fixed: `TranslationOptions` lived in `visitor.rs`, omitted `define_unpredictable_behaviour`, and
  derived a false `hook_hint_instructions` default instead of Eden's true default.
- Fixed: both runtime backends discarded the configured define-unpredictable value; x64 also
  discarded `wall_clock_cntpct`. Both now construct the same option values as Eden.
- Fixed: block translation routed decoder misses to the extra interpreter terminal instead of
  raising `UnallocatedEncoding`, and Rust lacked Eden's `TranslateSingleInstruction` counterpart.
- Fixed: dispatch used a catch-all interpreter arm even though every active generated decoder
  identity now has an explicit visitor arm. The match is exhaustive.

### Missing items
- None for the options, memory-code callback type, block translation, and single-instruction
  translation declared and defined by the reviewed upstream pair.

### Binary layout verification
- N/A: translation options are passed as Rust values and no raw-memory ABI is exposed. Focused
  tests verify all defaults plus decoded and undecodable single-instruction bookkeeping.

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/system.rs` vs Eden `frontend/A64/translate/impl/{system.cpp,impl.h}` (interpreter-producer audit)

### Intentional differences
- Rust packs the decoded system-register fields into a `repr(u16)` enum while Eden uses its generic
  immediate concatenation into a `u32` enum. Every constant bit pattern is identical and the enum
  is not raw-copied or exposed through an ABI.
- Rust visitor methods receive `DecodedInst`; active methods extract the same operands before
  executing Eden's switch cases.
- `MSR_imm` and `SYS` remain declarations only in Eden's header: their decoder entries are
  commented out and this snapshot has no C++ definitions. Rust no longer invents dead fallback
  implementations for them.

### Unintentional differences (to fix)
- Fixed: unsupported decoded MRS/MSR register values produced the extra interpreter terminal;
  Eden reaches `UNREACHABLE()` after its switch. Rust now does the same and has focused panic tests.
- Fixed: CNTPCT block splitting used saturating subtraction for the cycle count. The nonempty-block
  guard makes Eden's literal decrement valid, so Rust now preserves it exactly.

### Missing items
- None among the active decoder identities defined by `system.cpp`.

### Binary layout verification
- N/A: system-register encodings are compile-time discriminants, not serialized payloads. Their
  values are preserved bit-for-bit.

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/load_store_exclusive.rs` vs Eden `frontend/A64/translate/impl/{load_store_exclusive.cpp,impl.h}` (interpreter-producer audit)

### Intentional differences
- Rust visitors extract operands from `DecodedInst`; pair visitors reconstruct Eden's
  `concatenate(Imm<1>{1}, sz)` from the encoded size bits before invoking the same shared helper.
- Rust represents Eden's optional registers with `Option<Reg>` and names the overloaded
  `ExclusiveMem` helpers `exclusive_mem_read` and `exclusive_mem_write`.

### Unintentional differences (to fix)
- Fixed: separate single/pair load/store implementations duplicated and reordered Eden's shared
  decode. The owner now has direct counterparts for `ExclusiveSharedDecodeAndOperation` and
  `OrderedSharedDecodeAndOperation`, including the exact alias-validation `else if` order.
- Fixed: STXR/STLXR omitted the `Rs == Rt` and `Rs == Rn` constrained-unpredictable checks, while
  STXP/STLXP unconditionally rejected `Rs == Rt` or `Rs == Rt2`. Both single and pair stores now
  honor `define_unpredictable_behaviour` and execute Eden's `Constraint_NONE` case when enabled.
- Fixed: the four impossible direct-width fallbacks produced an extra interpreter terminal. All
  width selection now goes through the shared `ExclusiveMem` helpers with unreachable defaults.

### Missing items
- None among the two file-local helpers and twelve visitor definitions in the reviewed upstream
  source and header.

### Binary layout verification
- N/A: these methods construct SSA and define no raw-copied payload. Focused tests cover all twelve
  decoded visitors, exclusive and ordered access tags, pair widths, alias failures, option-controlled
  `Constraint_NONE`, and the validation-order interaction when `Rs`, `Rn`, and `Rt` alias.

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/simd_vector_x_indexed_element.rs` vs Eden `frontend/A64/translate/impl/{simd_vector_x_indexed_element.cpp,impl.h}` (FCMLA fallback audit)

### Intentional differences
- Rust extracts operands from `DecodedInst` and initializes FCMLA's rotation-selected elements as
  tuples because Rust forbids Eden's declaration-then-assignment form.

### Unintentional differences (to fix)
- Fixed: FCMLA by element used the extra interpreter terminal for the unsupported half-precision
  form. Eden asserts that `esize != 16`; Rust now asserts at the identical point and has a focused
  regression test.
- The six anonymous-namespace helpers are currently visitor methods or duplicated field-extraction
  logic, including an extra `fp_multiply_by_element_fields` dispatcher. Their ownership must be
  restored in a later full owner slice; all 21 upstream visitor definitions are present.

### Missing items
- No visitor definition is missing; the remaining gap is the ownership/boundary mismatch for the
  six file-local helpers.

### Binary layout verification
- N/A: these visitors construct SSA and define no raw-copied payload.

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/visitor.rs` vs Eden `frontend/A64/translate/impl/{impl.cpp,impl.h}` (`ExclusiveMem`, `SignExtend`, and `ZeroExtend` helpers)

### Intentional differences
- Rust names Eden's overloaded `ExclusiveMem` methods `exclusive_mem_read` and
  `exclusive_mem_write` because Rust has no function overloading. The write helper keeps Eden's
  address, byte-size, access-type, value order.
- Invalid sizes use Rust `unreachable!` diagnostics where Eden uses `UNREACHABLE()` after its
  switches.

### Unintentional differences (to fix)
- Fixed: the generic exclusive-memory overloads were absent, forcing instruction owners to repeat
  width dispatch and making exact helper-boundary parity impossible.
- Fixed: the visitor-level destination-directed `SignExtend` and `ZeroExtend` helpers were absent;
  the existing broader signedness helper did not preserve Eden's method ownership.

### Missing items
- None for the four reviewed visitor-helper methods.

### Binary layout verification
- N/A: these helpers construct SSA and define no raw-copied payload. Focused tests verify all ten
  exclusive read/write width selections and all four byte-to-word/long extension selections.

## 2026-08-24 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `ir/ir_emitter.h` (`MemOp` ownership)

### Intentional differences
- Rust applies PascalCase variant spelling and derives comparison/debug traits; the enum remains a
  control-flow type and is not encoded into SSA or exposed through the JIT ABI.

### Unintentional differences (to fix)
- Fixed: `MemOp` was absent from the generic IR emitter owner, forcing A64 translation files to
  duplicate partial local enums. The shared owner now exposes Eden's Load, Store, and Prefetch
  inventory in the same conceptual location.

### Missing items
- None for the reviewed `MemOp` declaration.

### Binary layout verification
- N/A: neither implementation serializes or raw-copies this control-flow enum. A focused inventory
  test constructs all three upstream variants.

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/load_store_multiple_structures.rs` vs Eden `frontend/A64/translate/impl/load_store_multiple_structures.cpp` (`MemOp` ownership)

### Intentional differences
- Rust uses an explicit unreachable match arm for Prefetch; this upstream helper is called only by
  load and store decoder identities.

### Unintentional differences (to fix)
- Fixed: the file owned a duplicate two-variant `MemOp`. Its shared decode helper now consumes the
  generic IR-owner enum used by Eden.

### Missing items
- None for this ownership correction.

### Binary layout verification
- N/A: the enum only selects translation control flow. Existing focused multiple-structure tests
  pass unchanged after the owner migration.

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/load_store_single_structure.rs` vs Eden `frontend/A64/translate/impl/load_store_single_structure.cpp` (`MemOp` ownership)

### Intentional differences
- Rust uses an explicit unreachable match arm for Prefetch; no single-structure decoder identity
  supplies that operation.

### Unintentional differences (to fix)
- Fixed: the file owned a duplicate two-variant `MemOp`. Its shared decode helper now consumes the
  generic IR-owner enum used by Eden.

### Missing items
- None for this ownership correction.

### Binary layout verification
- N/A: the enum only selects translation control flow. Existing focused single-structure tests
  pass unchanged after the owner migration.

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/load_store_register_immediate.rs` vs Eden `frontend/A64/translate/impl/load_store_register_immediate.cpp` (`LoadStoreSIMD` `MemOp` ownership)

### Intentional differences
- Rust uses an explicit unreachable Prefetch arm in `load_store_simd`, matching Eden's default
  unreachable path because only SIMD load and store visitors call that helper.

### Unintentional differences (to fix)
- Fixed: `LoadStoreSIMD` accepted a file-local `SimdMemOp` instead of Eden's generic `IR::MemOp`.
  It now consumes the enum from the IR emitter owner.

### Missing items
- None for the reviewed SIMD-helper ownership correction.

### Binary layout verification
- N/A: the enum only selects translation control flow; no SSA or guest payload layout changes.

## 2026-08-24 — `src/rdynarmic/src/{ir/terminal.rs,ir/opt,frontend/a64/translate/visitor.rs,backend/{x64,arm64}}` vs Eden `ir/terminal.h`, `frontend/A64/translate/impl/impl.cpp`, and host terminal emitters (terminal inventory)

### Intentional differences
- Rust represents the C++ variant surface as an enum and uses `Box` for recursive storage. Backend
  emitters remain split according to Rust's existing host modules rather than C++ class overloads.
- Translation tests retain their positive opcode/exception/terminal assertions; 118 negative
  `Terminal::Interpret` assertions were removed because absence of the variant makes them
  tautological rather than behavioral checks.

### Unintentional differences (to fix)
- Fixed: Rust invented `Terminal::Interpret`, an A64 `interpret_this_instruction` producer, x64 and
  arm64 emitter cases, and a no-op `a64_merge_interpret_blocks` optimization. Eden has none of
  these; the variant, producer, optimizer, calls, and emitter paths are removed.
- Rust still permits conditional terminals to contain another recursive `Terminal`, whereas Eden
  restricts `If`, `CheckBit`, and `CheckHalt` children to `LeafTerminal`. Aligning this type-level
  invariant is a broader terminal-ownership refactor and remains outstanding.

### Missing items
- A distinct Rust `LeafTerminal` owner enforcing Eden's non-recursive conditional children.

### Binary layout verification
- N/A: terminals are compiler-owned control-flow values and are not raw-copied across an ABI. The
  bounded crate suite passes with 1075 tests and four ignored after removing the two extra
  Interpret-specific tests.

## 2026-08-24 — `src/rdynarmic/src/{jit_config.rs,jit.rs,backend/{common/a32_callbacks.rs,x64,arm64}}` vs Eden `interface/{A32,A64}/config.h` and host callback plumbing (interpreter-callback inventory)

### Intentional differences
- Rust still exposes a temporary shared `UserCallbacks` trait and constructs boxed x64 callback
  adapters, while Eden owns separate A32/A64 interfaces and devirtualizes their methods directly.
  That broader configuration-owner split remains the next parity slice.
- The arm64 Rust backend stores callback addresses in an explicit callback table before emitting
  trampolines; Eden's templated C++ emitters derive them directly from the architecture callback
  type. The surviving callback inventory and relocation targets now match.

### Unintentional differences (to fix)
- Fixed: Rust retained a non-upstream `interpreter_fallback` trait method after removing the only
  terminal that could invoke it. Its A32 forwarding helper, A32/A64 JIT trampolines, x64 callback
  slots, arm64 callback address/trampoline, relocation target, and prelude field are removed.
- Fixed: AES and SHA regression-test names still described the removed fallback architecture; they
  now state the positive upstream-IR contract that the tests actually verify.

### Missing items
- Separate A32 and A64 callback traits in their matching configuration owners remain missing; the
  shared callback interface is retained only until that structural prerequisite is implemented.

### Binary layout verification
- N/A: callback adapters, function pointers, and relocation discriminants are internal JIT
  plumbing and are not raw-copied guest payloads. Backend construction tests compile against the
  reduced callback inventory, and the bounded crate suite validates both host-independent paths.

## 2026-08-24 — `src/rdynarmic/src/interface/{a32,a64}/config.rs` vs Eden `interface/{A32,A64}/config.h` (`UserCallbacks` owners)

### Intentional differences
- Rust uses one architecture-owned trait per C++ callback struct. A32's four translation methods
  are repeated on that trait and forwarded by `UserCallbacksAdapter`, because Rust cannot override
  inherited trait defaults in the C++ manner while retaining the standalone `TranslateCallbacks`
  interface.
- Read callbacks take `&self` and write/event callbacks take `&mut self`; this expresses the
  mutation boundary that C++ leaves implicit without changing callback order or values.
- `jit_config.rs` temporarily implements both architecture traits for its legacy shared trait
  object. This mechanical bridge keeps existing callers buildable while backend ownership is
  migrated in the immediately following slices.

### Unintentional differences (to fix)
- Fixed: the matching A32 and A64 configuration owners lacked their `UserCallbacks` interfaces.
  They now expose Eden's exact architecture-specific method inventories, `u32` versus `u64`
  addresses, A64 vector shape, typed exceptions/cache operations, timing methods, and defaults.
- Fixed: the A32 frontend adapter depended directly on the unrelated shared callback trait. It is
  now generic over the A32-owned callback interface and forwards all four translation methods.
- The legacy shared trait and its raw integer event surface remain in use by runtime/backend
  consumers. They must be removed after those consumers migrate to the new typed owners.

### Missing items
- `UserConfig` was restored in the following 2026-08-24 configuration-owner slice.
- Direct A32 and A64 runtime/backend consumption of the new traits remains the next prerequisite
  before the legacy shared callback trait can be deleted.

### Binary layout verification
- PASS: A32/A64 exception and cache-event enums retain their verified four-byte layouts; A64
  `Vector = [u64; 2]` is verified as 16 bytes with eight-byte alignment. Trait objects themselves
  are host-side interfaces and are not raw-copied guest payloads.

## 2026-08-24 — `src/rdynarmic/src/interface/{a32,a64}/config.rs` vs Eden `interface/{A32,A64}/config.h` (`UserConfig` owners)

### Intentional differences
- Rust owns callbacks with `Box<dyn UserCallbacks>` and represents nullable pointers with `Option`;
  Eden accepts non-owning raw callback pointers and uses null/default optionals. The JIT lifetime
  remains responsible for keeping every callback and pointee alive.
- A32's page table is typed as a pointer to the exact fixed-size pointer array; A64's `void**` is
  represented as `*mut *mut c_void`. Fastmem bases use byte pointers rather than integer addresses.
- Each Rust configuration is constructed with `UserConfig::new(callbacks)` because a useful safe
  default cannot manufacture Eden's required callback pointer.

### Unintentional differences (to fix)
- Fixed: both architecture configuration owners lacked their complete `UserConfig` structures.
  All fields now live beside their upstream counterparts with exact architecture-specific integer
  widths, pointer shapes, constants, and initial values.
- Fixed: the only optimization predicate lived on the merged legacy `JitConfig`. Both owned
  configurations now apply Eden's unsafe-flag mask before testing the requested flag.
- Runtime JITs and host backends still consume the merged legacy `JitConfig`; migration to these
  new owners remains required before the old structure can be removed.

### Missing items
- Direct A32/A64 JIT and backend construction from their respective `UserConfig` types.
- Removal of the legacy shared `jit_config::JitConfig` after all callers have migrated.

### Binary layout verification
- N/A: these host configuration structures are not raw-copied across the guest ABI. Focused tests
  verify both upstream constants and every nonzero/true default, including A32 page-table geometry,
  A64 timer/cache registers, mirror/recompile switches, cycle counting, and optimization masking.

## 2026-08-24 — `src/rdynarmic/src/{jit.rs,jit_config.rs,backend/{common/a32_callbacks.rs,arm64/a32_{address_space,core,interface}.rs,arm64/emit_arm64.rs}}` vs Eden `interface/A32/config.h` and `backend/{x64,arm64}/a32_{interface,address_space}.*`

### Intentional differences
- Rust owns the A32 callback object in `A32::UserConfig` and uses lifecycle pointer setters after
  the boxed JIT state reaches a stable address. Eden's emulator callback object instead already
  owns a pointer to the public JIT wrapper; callback values and invocation ordering are unchanged.
- `jit_config.rs` retains a temporary consuming adapter from the legacy public merged
  configuration. It exists only at the compatibility boundary; the A32 JIT and both A32 host
  backends now store and consume the architecture-owned configuration directly.
- The Rust arm64 backend uses a stable `A32CallbackContext` and explicit callback-address tables
  where Eden devirtualizes C++ member-function pointers while emitting its prelude.

### Unintentional differences (to fix)
- Fixed: A32 runtime/backend consumers used the merged 64-bit-address callback surface. They now
  use `A32::UserCallbacks`, preserve 32-bit guest addresses, and forward typed A32 exceptions.
- Fixed: A32 callback tables exposed A64-only 128-bit reads/writes, cache-operation callbacks, and
  `GetCNTPCT`. Actual A32 paths are removed, and the arm64 A32 prelude leaves its shared
  `get_cntpct` relocation slot empty like Eden.
- Fixed: the arm64 A32 emitter hard-coded little-endian behavior. It now forwards
  `UserConfig::always_little_endian`, preserving Eden's CPSR.E policy.
- The x64 `EmitCallbacks` and `RawExclusiveWriteCallbacks` structures are still shared between
  A32 and A64, so A32 construction must populate unreachable placeholders for their A64-only
  128-bit/cache/counter slots. Splitting these backend callback owners remains required.

### Missing items
- Direct A64 runtime/backend migration to `interface/a64/config.rs::UserConfig` and removal of the
  legacy shared configuration/callback compatibility layer.
- Architecture-specific x64 callback-table types matching Eden's separate A32/A64 emitters.

### Binary layout verification
- N/A: the changed configuration objects and callback tables are host-side Rust structures and
  are not raw-copied guest payloads. A32 exception values retain their verified four-byte layout;
  focused callback/configuration tests and all four cross-target test builds pass.

## 2026-08-24 — `src/rdynarmic/src/backend/{common/emit_context.rs,x64/emit_x64_memory.rs,arm64/{emit_arm64.rs,emit_arm64_memory.rs}}` vs Eden `backend/{x64/emit_x64_memory.h,arm64/{emit_arm64.h,emit_arm64_memory.cpp}}` (`page_table_log2_stride`)

### Intentional differences
- Rust's existing `MemoryEmitConfig` is shared by the two host emitters, whereas Eden stores the
  field in each architecture `UserConfig` and copies it into the arm64 `EmitConfig`. Both A32 and
  A64 construction paths now forward the architecture-owned value into that mechanical backend
  container.
- The temporary public `JitConfig` compatibility bridge exposes the stride through its nested
  memory configuration until remaining callers migrate to architecture-owned configurations.

### Unintentional differences (to fix)
- Fixed: both x64 page-table lookups multiplied every index by eight. They now shift the index by
  the configured log2 stride before the unscaled pointer load, exactly like Eden, so both supported
  eight- and sixteen-byte entries address the correct pointer field.
- Fixed: arm64 used the scaled `LDR` form and therefore also hard-coded eight-byte entries. It now
  emits Eden's explicit `LSL` followed by an unscaled indexed `LDR`.
- Fixed: the A32 and A64 configuration adapters discarded the supplied stride and always selected
  three; they now preserve the value end-to-end.
- Fixed: A32 emitter construction conditioned `fastmem_exclusive_access` on two unrelated pointer
  presences. Eden forwards the configuration flag literally; both host construction paths now do
  the same.

### Missing items
- Removing the temporary merged `JitConfig` remains part of the architecture-configuration
  migration; no page-table stride behavior remains missing in the reviewed x64 or arm64 lookup.

### Binary layout verification
- N/A: the configuration is host-side. A native x64 execution test uses sixteen-byte entries with
  a poisoned second word and verifies that the JIT loads the first-word pointer; arm64 emission
  tests verify the configured `LSL #4` and unscaled indexed `LDR` sequence.

## 2026-08-24 — `src/core/src/arm/dynarmic/arm_dynarmic_{32,64}.rs` vs Eden `core/arm/dynarmic/arm_dynarmic_{32,64}.{h,cpp}` (`page_table_log2_stride`)

### Intentional differences
- Eden indexes its interleaved 32-byte `Common::PageTable::PageEntryData` records and therefore
  uses a log2 stride of five. Ruzu exposes its separate contiguous `PageInfo` pointer buffer to
  rdynarmic, so both JIT owners derive the stride from `size_of::<PageInfo>()` (eight bytes and a
  log2 stride of three on the supported 64-bit hosts). A compile-time assertion preserves the
  required power-of-two layout contract.

### Unintentional differences (to fix)
- Fixed: after rdynarmic gained the upstream `page_table_log2_stride` option, the two explicit
  core `MemoryEmitConfig` initializers did not forward their concrete page-table entry stride and
  no longer compiled.
- Fixed: `DynarmicCallbacks64` still implemented the removed, non-upstream
  `interpreter_fallback` compatibility callback after the A64 interface migration. The unreachable
  callback and its private trace helper are removed; unsupported instructions continue through
  the translator/JIT exception path owned by rdynarmic, matching Eden.

### Missing items
- None for this configuration field.

### Binary layout verification
- PASS: each core owner derives the emitted index stride from the exact `PageInfo` element type
  backing the pointer passed to rdynarmic and statically rejects a non-power-of-two entry size.

## 2026-08-24 — `src/rdynarmic/src/{interface/a64/config.rs,jit.rs,jit_config.rs,backend/arm64/a64_{address_space,core,interface}.rs,backend/arm64/emit_arm64.rs}` vs Eden `interface/A64/config.h` and `backend/{x64,arm64}/a64_{interface,address_space}.*`

### Intentional differences
- Rust owns the callback object with `Box<dyn UserCallbacks>` and installs stable JIT-state
  pointers through two lifecycle hooks. Eden receives a non-owning callback pointer whose owner
  already knows the public JIT object; callback arguments and installation order are preserved.
- `jit_config.rs` retains a consuming adapter for existing ruzu callers of the former merged
  public configuration. Production A64 JIT/backend state immediately converts at this boundary
  and stores only the A64-owned configuration and typed callback interface.
- The Rust arm64 backend stores an explicit callback context and function-address table where Eden
  devirtualizes C++ member functions into generated trampolines.

### Unintentional differences (to fix)
- Fixed: A64 runtime and arm64 backend owners consumed the merged configuration and raw-integer
  callback surface. They now consume `interface/a64/config.rs::{UserConfig,UserCallbacks}` and
  preserve A64 vector values, typed exceptions, cache operations, address widths, system-register
  values, memory policy, processor ID, and optimization masking.
- Fixed: the A64 x64 and arm64 callback trampolines exposed legacy method names and tuple-shaped
  128-bit values. Their calls now match the A64-owned interface, including exclusive writes and
  SVC/cache-event ownership.
- Fixed: both A64 host constructors treated an explicit zero code-cache size as a request for the
  default. Eden forwards the configured value literally; Rust now preserves it as well.

### Missing items
- The legacy shared `jit_config::{JitConfig,UserCallbacks}` remains as a caller compatibility
  boundary and still narrows its old read-only-memory query to 32 bits. It must be removed after
  all external construction sites use the separate A32/A64 owners.
- The x64 `EmitCallbacks` and `RawExclusiveWriteCallbacks` containers are still shared between
  A32 and A64; splitting those backend tables remains a separate ownership slice.

### Binary layout verification
- PASS: A64 exception and cache-operation enums remain four-byte values with upstream ordinal
  order, and `Vector = [u64; 2]` remains 16 bytes. Round-trip enum tests and typed callback tests
  cover the values crossing generated-code trampolines.

## 2026-08-24 — `src/core/src/arm/dynarmic/arm_dynarmic_{32,64}.rs` vs Eden `core/arm/dynarmic/arm_dynarmic_{32,64}.{h,cpp}` (architecture configuration ownership)

### Intentional differences
- Ruzu owns the callback object in `A64UserConfig` and installs stable halt/PC pointers after JIT
  allocation; Eden passes a non-owning `DynarmicCallbacks64*` whose parent already owns the JIT.
- A32 installs its parent pointer only after the Rust owner reaches a stable address; this is the
  lifecycle-safe equivalent of Eden's callback reference to `ArmDynarmic32`.
- Ruzu's split page table passes the contiguous `PageInfo` buffer and its derived stride, as
  recorded in the preceding page-table layout audit, instead of Eden's interleaved
  `PageEntryData` buffer.
- Ruzu reads instruction words directly and therefore does not need Eden's cached-code-page reset
  in `InstructionSynchronizationBarrierRaised`.

### Unintentional differences (to fix)
- Fixed: the production A64 core owner constructed the legacy architecture-merged `JitConfig`,
  which converted its callbacks and configuration through `LegacyA64Callbacks`. It now implements
  `interface/a64/config.rs::UserCallbacks` and constructs that owner's `UserConfig` directly,
  preserving typed vectors, exceptions, cache operations, widths, processor ID, system registers,
  memory policy, optimization mask, and timing fields.
- Fixed: `DynarmicCallbacks64` retained an unread copy of the exclusive-monitor pointer even though
  Eden's callback does not own that state. The pointer now exists only in the A64 JIT configuration,
  matching its actual consumer and removing the dead field.
- Fixed: the production A32 core owner also constructed the merged `JitConfig`. It now implements
  the A32-owned callback surface with 32-bit guest addresses, removes the unreachable A64-only
  128-bit/cache-counter callbacks, and constructs `A32UserConfig` directly with its coprocessor,
  page-table, optimization, endianness, processor, timing, and memory-policy fields.
- Fixed: A32 enabled fastmem exclusives only when both fastmem and the global-monitor pointer were
  present. Eden derives this option solely from `fastmem_pointer`; Ruzu now does the same while the
  monitor remains an independently optional configuration field.
- Fixed: A32 overrode Eden's conservative default `IsReadOnlyMemory` callback with a page-permission
  query. Removing the override restores the upstream optimization contract instead of folding
  reads under a Ruzu-specific policy.
- Fixed: `ArmDynarmic32` retained an unread exclusive-monitor field after construction. The pointer
  now lives only in the A32 configuration that consumes it.

### Missing items
- `InstructionCacheOperationRaised` still logs operations instead of invoking the owning JIT's
  range/all-cache invalidation methods and requesting `CacheInvalidation` like Eden. Restoring this
  requires a shared invalidation request owned across the Rust callback/JIT lifetime boundary.
- Other remaining production and test callers still use the temporary shared `JitConfig`; this
  slice removes the compatibility boundary from both production CPU owners.

### Binary layout verification
- N/A: the changed configuration and callback ownership are host-side. The architecture-owned
  exception, vector, and cache-operation layouts are verified in the two interface configuration
  modules; core compile-time regression tests require each callback owner to implement its exact
  architecture trait.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/{a32,a64}_interface.rs` vs Eden `backend/arm64/{a32,a64}_interface.cpp` and `interface/{A32,A64}/config.h` (test configuration ownership)

### Intentional differences
- Rust test callbacks retain optional shared pointer-observation state so lifecycle tests can
  assert that generated-code callback pointers target the final boxed interface state. Eden's
  production interfaces do not contain these Rust regression fixtures.

### Unintentional differences (to fix)
- Fixed: both ARM64 interface test modules built the obsolete architecture-merged `JitConfig` and
  reached the backend through its conversion adapter. They now construct the interface-owned A32
  and A64 configurations directly and override only the three test-specific settings.
- Fixed: the A32 fixture exposed 64-bit addresses and A64-only 128-bit callbacks. It now implements
  the A32 callback contract with 32-bit addresses, typed exceptions, and the upstream default
  exclusive-write behavior.
- Fixed: the A64 fixture used tuple-shaped 128-bit values, raw exception integers, and the legacy
  supervisor-call name. It now uses the upstream-shaped vector, typed exception, `call_svc`, and
  required physical-counter callback.

### Missing items
- The remaining legacy test-configuration users in other ARM64 backend files are outside this
  interface-owned slice and still need conversion before the merged compatibility layer can be
  removed.

### Binary layout verification
- PASS: compile-time trait checking on AArch64 now enforces A32 `u32` guest addresses and A64
  `[u64; 2]` vectors at these test/backend boundaries; this slice serializes no guest payload.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/{emit_context,emit_arm64_a64}.rs` vs Eden `backend/arm64/{emit_context.h,emit_arm64_a64.cpp}` and `interface/A64/config.h` (A64 emitter test ownership)

### Intentional differences
- Rust emission helpers return `Result` because instruction-buffer writes are fallible; the test
  fixtures exercise the same emitted instruction and relocation sequences as Eden through this
  error-aware API.

### Unintentional differences (to fix)
- Fixed: A64 emission-context and terminal-emitter tests constructed the obsolete merged
  configuration and converted it before building `EmitConfig`. Both now consume A64 `UserConfig`
  directly, including its direct `check_halt_on_memory_access` owner.
- Fixed: their callback fixtures exposed the legacy tuple/raw-integer API. They now implement the
  A64 callback contract with typed vectors, exceptions, SVC naming, physical counter, and upstream
  default exclusive-write behavior.
- Fixed: `emit_arm64_a64.rs` imported the optimization flags through the compatibility module even
  in production code. It now imports their upstream-equivalent interface owner directly.

### Missing items
- Other ARM64 A64 address-space and shared-memory emitter test fixtures still depend on the merged
  compatibility layer and remain separate ownership slices.

### Binary layout verification
- PASS: the AArch64 test build checks the `[u64; 2]` callback vector boundary; this test-only
  configuration migration does not alter emitted instruction encodings or guest data layouts.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/a64_address_space.rs` vs Eden `backend/arm64/a64_address_space.{h,cpp}` and `interface/A64/config.h` (test configuration ownership)

### Intentional differences
- Rust callback-thunk tests retain observable fields for memory and system events; Eden implements
  the corresponding production trampolines through devirtualized C++ member-function pointers.

### Unintentional differences (to fix)
- Fixed: the address-space fixture implemented both the merged callback API and A64 callback API,
  forwarding every method through an adapter. It now implements only A64 `UserCallbacks`, with the
  upstream vector, typed system-event, exclusive-write, SVC, and counter signatures.
- Fixed: its configuration was constructed as merged `JitConfig` and converted afterward. It now
  starts from A64 `UserConfig` and changes only cache size, cycle counting, and optimizations.
- Fixed: the thunk regression invoked exception and cache-operation callbacks with out-of-range raw
  integers that the typed A64 boundary correctly rejects. It now uses valid upstream enum ordinals.

### Missing items
- Shared ARM64 memory-emitter tests still construct the merged configuration and are handled in a
  later file-owned slice.

### Binary layout verification
- PASS: the AArch64 test build enforces the 16-byte `[u64; 2]` vector callback boundary; existing
  `Pair128` thunk assertions continue to verify low/high word ordering.

## 2026-08-24 — `src/core/src/cpu_manager.rs` vs Eden `core/cpu_manager.{h,cpp}` and `core/hle/kernel/physical_core.cpp` (ordinary JIT halt path)

### Intentional differences
- Explicit `RUZU_SPIN_TRACE` requests may still capture a halt context, and the Rust-only null-PC
  `BreakLoop` workaround captures one context to classify that known bridge failure. Neither path
  runs for an ordinary zero-reason cycle-budget expiration.

### Unintentional differences (to fix)
- Fixed: every Rust `Halted` event, including `Halted(0)` at each budget expiration, called
  `get_context()` and published PC/LR/SP plus 29 registers through release atomics. Eden simply
  continues after an ordinary halt and performs no equivalent diagnostic work.
- Fixed: thread 17 performed another unconditional context copy for its first 500 halts even when
  trace logging was disabled. The obsolete investigation probe and counter were removed.

### Missing items
- The larger Rust cooperative run-loop structure still differs from Eden's direct
  `PhysicalCore::RunThread`; this change is limited to removing non-upstream work from its hot
  ordinary-halt path.

### Binary layout verification
- N/A: this change affects host-side scheduling and diagnostic snapshots only.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/emit_arm64_memory.rs` vs Eden `backend/arm64/emit_arm64_memory.cpp` and `interface/A64/config.h` (memory-emitter test ownership)

### Intentional differences
- Rust memory-emission tests construct an `EmitConfig` explicitly around the fallible instruction
  writer; their expected ARM64 words and relocation records remain the behavioral oracle for the
  same helpers owned by Eden's file.

### Unintentional differences (to fix)
- Fixed: the shared memory-emitter fixture built the architecture-merged `JitConfig`, mutated its
  nested memory policy, then converted it to A64. It now constructs A64 `UserConfig` directly and
  sets `check_halt_on_memory_access` on its upstream-equivalent owner.
- Fixed: its callbacks used legacy raw exception values and tuple-shaped 128-bit memory values.
  They now implement the typed A64 callback boundary, including `get_cntpct` and upstream default
  exclusive-write behavior.

### Missing items
- The A32-specific ARM64 memory and coprocessor emitter fixtures remain on the compatibility layer
  and require their own A32-owned conversion.

### Binary layout verification
- PASS: the AArch64 test build enforces the A64 `[u64; 2]` callback vector layout; the existing
  memory-emission tests continue to assert instruction words for 8/16/32/64/128-bit paths.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/emit_arm64_a32_{memory,coprocessor}.rs` vs Eden `backend/arm64/emit_arm64_a32_{memory,coprocessor}.cpp` and `interface/A32/config.h` (A32 emitter test ownership)

### Intentional differences
- Rust keeps native unit-test harnesses next to these file-owned emitters and constructs the
  fallible instruction writer explicitly; the production emission sequences remain unchanged.

### Unintentional differences (to fix)
- Fixed: both A32 emitter fixtures built the architecture-merged `JitConfig` and callback adapter.
  They now construct A32 `UserConfig` directly and implement only A32 `UserCallbacks`.
- Fixed: the fixtures exposed A64-only 128-bit callbacks, `u64` guest addresses, and raw exception
  values. Their boundary now uses A32 `u32` addresses, typed exceptions and SVC naming, while
  retaining upstream's default exclusive-write behavior.
- Fixed: the memory fixture now sets `check_halt_on_memory_access` directly on its A32 owner, and
  the coprocessor fixture installs CP15 directly in the A32 coprocessor table.

### Missing items
- The shared ARM64 dispatcher and A32 dispatcher test fixtures in `emit_arm64.rs` and
  `emit_arm64_a32.rs` still use the merged compatibility layer and require separate owner-aligned
  conversion.

### Binary layout verification
- PASS: the AArch64 test build enforces the A32 `u32` callback-address boundary; the existing
  emitter tests continue to verify generated instruction words and this slice serializes no guest
  payload.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/{emit_arm64,emit_arm64_a32}.rs` vs Eden `backend/arm64/emit_arm64.{h,cpp}`, `backend/arm64/emit_arm64_a32.cpp`, and `interface/{A32,A64}/config.h` (dispatcher test ownership)

### Intentional differences
- Rust retains native instruction-word and relocation tests beside the corresponding shared and
  A32 dispatcher implementations; their fallible code-buffer API does not change the upstream
  emission ordering under test.

### Unintentional differences (to fix)
- Fixed: the shared dispatcher tests used one merged callback/configuration type for both guest
  architectures. They now use separate A32 and A64 fixtures with their respective address widths,
  vector representation, exception type, system callbacks, and configuration defaults.
- Fixed: the A32 dispatcher tests converted a merged configuration at every context boundary.
  Their helpers now own A32 `UserConfig` directly, including optimization, cycle-counting, and
  memory-abort fields.
- Fixed: `emit_arm64_a32.rs` imported `OptimizationFlag` through the compatibility module, and the
  shared dispatcher retained an unused `XFASTMEM` import. The flag now comes from its interface
  owner and the stale import is removed.

### Missing items
- The mixed-architecture test environment in `jit.rs` remains the final legacy merged-configuration
  consumer before the compatibility layer can be removed.

### Binary layout verification
- PASS: the AArch64 test build enforces A32 `u32` addresses and A64 `[u64; 2]` vectors at the
  dispatcher fixtures; existing tests continue to assert generated instruction words, JIT-state
  offsets, relocation ordering, and A32's fixed 32-bit memory spaces.

## 2026-08-24 — `src/rdynarmic/src/jit.rs` vs Eden `interface/A32/{a32,config}.h` and `backend/{x64,arm64}/a32_interface.cpp` (A32 test configuration ownership)

### Intentional differences
- Rust keeps its cross-backend native regression tests in the public JIT wrapper while Eden's
  backend interfaces are separate translation units. The fixtures now expose the A32-owned
  configuration and callback boundary directly despite that existing harness placement.
- The Rust JIT constructors remain fallible because executable-memory allocation and code
  generation report errors instead of relying on C++ assertions.

### Unintentional differences (to fix)
- Fixed: 42 A32 JIT fixtures constructed the obsolete architecture-merged `JitConfig`, including
  A64-only timer, cache-register, cache-hook, TLS, and 128-bit callback fields. They now construct
  A32 `UserConfig` directly with `u32` guest addresses and the A32-owned memory-policy fields.
- Fixed: the page-table fixtures reached A32 through a typeless compatibility pointer. They now
  expose the upstream 1,048,576-entry A32 page-table pointer type and preserve the configured
  entry stride, pointer mask, misalignment, absolute-offset, fastmem fallback, and halt policies.

### Missing items
- The A64 fixtures in the same Rust-native test module still construct the merged compatibility
  configuration. The common mock behavior still delegates through its legacy callback
  implementation until those A64 fixtures are migrated and both architecture traits can call
  architecture-neutral test-memory helpers directly.

### Binary layout verification
- PASS: native and AArch64 test builds enforce the A32 `u32` callback boundary and upstream-sized
  page-table pointer type. Existing focused fastmem, sixteen-byte page-table stride, and Thumb
  logical-flags regressions pass; this test-only migration serializes no guest payload.

## 2026-08-24 — `src/rdynarmic/src/{jit.rs,lib.rs}` and removed `jit_config.rs` vs Eden `interface/{A32,A64}/{a32,a64,config}.h` and `backend/{x64,arm64}/{a32,a64}_interface.cpp`

### Intentional differences
- Rust exposes fallible JIT constructors and boxes callback traits to represent C++ virtual
  callback ownership. The constructors now otherwise take the matching architecture `UserConfig`
  by value, as Eden does.
- Rust-native A32 and A64 integration regressions share memory-storage helper methods inside the
  test module; the two public callback implementations retain their distinct upstream signatures.

### Unintentional differences (to fix)
- Fixed: all 58 remaining A64 fixtures constructed an architecture-merged configuration containing
  A32 coprocessor/version fields. They now construct A64 `UserConfig` directly and use Eden's
  36-bit address-space defaults unless a test explicitly selects another width.
- Fixed: the shared test callback implementation and conversion adapters erased A32/A64 address,
  vector, exception, cache-operation, and default-exclusive-write differences. The fixtures now
  implement the two architecture callback traits directly over test-only storage helpers.
- Fixed: the non-upstream public `jit_config.rs` compatibility owner and its generic constructor
  conversions remained after every consumer had migrated. The module, re-export, adapters, and
  stale backend imports have been removed; optimization flags are imported from their interface
  owner.

### Missing items
- `jit.rs` remains a combined Rust wrapper for both guest architectures rather than mirroring
  Eden's backend-specific A32/A64 interface translation units. Splitting that established wrapper
  is a separate structural ownership slice because it also owns host callback trampolines and
  cache lifecycle.

### Binary layout verification
- PASS: native and AArch64 test builds enforce A64 `[u64; 2]` vectors, typed four-byte exceptions
  and cache operations, direct TLS/page-table pointer types, and A32 `u32` callback addresses.
  Focused A64 fastmem-fault, physical-counter, and exclusive-fallback execution tests pass; no
  serialized guest payload changed.

## 2026-08-24 — `src/rdynarmic/src/common/spin_lock.rs` vs Eden `common/spin_lock.h` and `common/spin_lock_{x64,arm64}.cpp`

### Intentional differences
- Rust uses a four-byte `AtomicU32` rather than lazily generating host routines for ordinary
  `SpinLock::lock` and `unlock`; acquire/release behavior and the x64 `xchg`/`mfence` strength are
  retained without allocating an executable helper page.

### Unintentional differences (to fix)
- Fixed: `SpinLock` lived inside the root exclusive-monitor module instead of its upstream
  `common/spin_lock` owner.
- Fixed: the x64 ordinary unlock used only a release store; it now performs the upstream-equivalent
  sequentially consistent exchange and fence.

### Missing items
- The AArch64 JIT-emitted `EmitSpinLockLock` and `EmitSpinLockUnlock` helpers remain part of the
  broader arm64 exclusive-fastmem backend parity work.

### Binary layout verification
- PASS: a focused test verifies that `SpinLock` has the four-byte size and alignment required by
  both upstream host emitters and Ruzu's generated x64 accesses.

## 2026-08-24 — `src/rdynarmic/src/common/spin_lock_x64.rs` vs Eden `common/spin_lock_x64.{h,cpp}`

### Intentional differences
- Rust emits through `rxbyak::CodeAssembler` and uses its native `umonitor` encoder instead of
  Eden's hand-written workaround for the historical Xbyak encoding bug.

### Unintentional differences (to fix)
- Fixed: the file lived under `backend/x64` even though upstream owns both declarations and
  implementation under `common`.
- Fixed: the acquire helper ignored WAITPKG and added a redundant explicit `lock` prefix to the
  implicitly locked memory `xchg`; both paths now follow Eden's emitted sequence.

### Missing items
- None for the two reviewed x64 emission helpers.

### Binary layout verification
- N/A: this owner emits host instructions rather than a raw-copied payload. Focused byte-level
  tests verify the PAUSE path, implicit-lock encoding, and UMONITOR/UMWAIT path.

## 2026-08-24 — `src/rdynarmic/src/interface/code_page.rs` vs Eden `interface/code_page.h`

### Intentional differences
- Rust expresses the public instruction array length with a constant expression over its native
  `u32` size.

### Unintentional differences (to fix)
- Fixed: Ruzu had no counterpart for Eden's public `CodePage` declaration and constant.

### Missing items
- None for this declaration.

### Binary layout verification
- PASS: `CodePage` is `repr(C)` and tests verify a 4096-byte size with `u32` alignment.

## 2026-08-24 — `src/rdynarmic/src/interface/halt_reason.rs` vs Eden `interface/halt_reason.h`

### Intentional differences
- Rust uses `bitflags` for Eden's operators and retains named aliases mapping Ruzu core events onto
  the corresponding upstream `UserDefined` bits.

### Unintentional differences (to fix)
- Fixed: the declaration lived at the crate root instead of its upstream `interface` owner; all
  internal and external consumers now use the owned type or its top-level public re-export.

### Missing items
- None for the upstream flag inventory and bitwise operations.

### Binary layout verification
- PASS: focused tests verify the four-byte representation and upstream bit values.

## 2026-08-24 — `src/rdynarmic/src/interface/exclusive_monitor.rs` vs Eden `interface/exclusive_monitor.h` and host `exclusive_monitor.cpp` implementations

### Intentional differences
- Rust stores the fixed-at-construction address/value sequences in non-resizing `Vec`s rather than
  Boost `static_vector`; the four-entry capacity is enforced and the host pointers remain stable.
- `Copy` is Rust's bound for the trivially-copyable template payload, and `MaybeUninit` represents
  Eden's uninitialized local before the exact-size `memcpy`.

### Unintentional differences (to fix)
- Fixed: the monitor lived at the crate root and also owned the unrelated `SpinLock` implementation.
- Fixed: construction accepted more than Eden's four-core static capacity, and `read_and_mark`
  cleared all sixteen reserved-value bytes before copying a smaller payload. It now enforces the
  upstream capacity and copies only `size_of::<T>()` bytes.

### Missing items
- None for the reviewed public methods, constants, state, and host-independent lifecycle.

### Binary layout verification
- N/A: Eden's monitor is a host-only C++ class containing Boost storage and is not raw-copied.
  Focused tests cover all supported widths, invalidation, clearing, and the capacity invariant.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/exclusive_monitor_friend.rs` vs Eden `backend/x64/exclusive_monitor_friend.h`

### Intentional differences
- Rust exposes the four friend operations as `unsafe` crate-local functions because raw-pointer
  validity, index bounds, and stable monitor ownership are caller contracts.

### Unintentional differences (to fix)
- Fixed: the four friend operations were extra public methods on `ExclusiveMonitor`, obscuring the
  upstream x64 owner. The emitter now calls the matching file-owned functions.

### Missing items
- None for the four friend accessors.

### Binary layout verification
- PASS: focused tests verify that the accessors address the monitor's four-byte lock storage,
  processor count, reservation-address slots, and 128-bit value slots.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/{emit_context.rs,emit_vector_helpers.rs,emit_vector_saturated.rs}` vs Eden `backend/x64/{emit_x64_vector.cpp,jitstate_info.h}` (SQSHLU/VQSHLU immediate fallback)

### Intentional differences
- Rust passes `ArchConfig` through `EmitContext` to select the A32/A64 `fpsr_qc` offset; Eden
  obtains the same architecture-specific offset from `BlockOfCode::GetJitStateInfo()`.
- Rust implements the element loop with fixed-size arrays instead of Eden's `VectorArray<T>`
  template. The signed input, unsigned result, saturation result and sticky QC behavior match.

### Unintentional differences (to fix)
- Fixed: the x64 fallback previously materialized Eden's scalar `Imm8` shift as an XMM value and
  read a different byte for every lane. Its ABI now takes one scalar `u8`, matching
  `EmitTwoArgumentFallbackWithSaturationAndImmediate`, and applies it to every lane.
- Fixed: the shared saturated fallbacks previously hard-coded `A64JitState::fpsr_qc`; A32 SIMD
  saturation could therefore write QC at the wrong state offset. All three shared helpers now use
  the active architecture's offset and Eden's byte-sized sticky OR.

### Missing items
- Eden's AVX2-specialized 32-bit SQSHLU emitter is not ported; Rust uses the behaviorally
  equivalent corrected scalar fallback for 8-, 16-, 32- and 64-bit lanes.

### Binary layout verification
- PASS: the fallback ABI is `(result pointer, input pointer, u8 immediate) -> u32`, with two
  16-byte stack slots plus the platform shadow space, matching Eden on System V and Windows x64.
  A focused full-JIT four-lane test verifies positive results, unsigned saturation,
  negative-to-zero and QC.

## 2026-08-24 — `src/rdynarmic/{build.rs,a64_decoder_parser.rs}` vs Eden `frontend/A64/decoder/{a64.h,a64.inc}` (closing `INST` delimiter)

### Intentional differences
- Eden expands `a64.inc` with the C++ preprocessor. Rust's build script must parse the same three
  macro fields to generate its decoder, so the parser is isolated in a build-support module that
  can also be compiled by the regression test.

### Unintentional differences (to fix)
- Fixed: `rfind(')')` could select a parenthesis from a trailing comment and silently omit an active
  decoder entry. The parser now selects the first closing parenthesis outside quoted fields, so
  parentheses in display names remain valid and trailing comments cannot pollute the bit string.

### Missing items
- None in the reviewed active-entry parsing path.

### Binary layout verification
- N/A: this code generates decoder metadata and does not serialize or raw-copy a payload. The
  regression fixture covers parentheses in both the quoted display name and trailing comment.

## 2026-08-24 — `src/rdynarmic/src/backend/block_range_information.rs` vs Eden `backend/block_range_information.{h,cpp}`

### Intentional differences
- Rust stores one closed range and descriptor per registration in a `Vec`, while Eden's Boost
  interval map splits/coalesces overlapping intervals and stores descriptor sets. Iterating every
  registered interval produces the same union of descriptors for invalidation without adding a
  nonstandard interval-map dependency.
- Rust accepts a slice of closed ranges in place of Boost's `interval_set`; callers construct the
  same closed invalidation intervals at their architecture boundary.

### Unintentional differences (to fix)
- Fixed: the shared owner was absent and its range lookup was duplicated partially in the ARM64
  address spaces or replaced by entry-PC filtering in the x64 emitters.
- Fixed: the ARM64 duplicates erased matched registrations, unlike Eden's current implementation,
  which deliberately retains them and carries an efficiency TODO.

### Missing items
- None for `AddRange`, `ClearCache`, `InvalidateRanges`, and the `u32`/`u64` instantiations.

### Binary layout verification
- N/A: this is host-only cache metadata and is never serialized or copied as a binary payload.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/a32_address_space.rs` vs Eden `backend/arm64/a32_address_space.{h,cpp}` (block ranges)

### Intentional differences
- Rust forwards a `HashSet<LocationDescriptor>` to its address-space invalidator rather than
  Eden's `ankerl::unordered_dense::set`; both represent the same unique descriptor set.

### Unintentional differences (to fix)
- Fixed: A32 owned a local `BlockRange32` vector and overlap loop instead of consuming the shared
  backend owner. Registration and invalidation now preserve Eden's exact start-PC through
  `EndLocation().PC() - 1` closed interval and descriptor lookup ordering.

### Missing items
- None in the reviewed `RegisterNewBasicBlock` and `InvalidateCacheRanges` paths.

### Binary layout verification
- N/A: block-range information is host-only cache metadata.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/a64_address_space.rs` vs Eden `backend/arm64/a64_address_space.{h,cpp}` (block ranges)

### Intentional differences
- Rust forwards a `HashSet<LocationDescriptor>` to its address-space invalidator rather than
  Eden's `ankerl::unordered_dense::set`; both represent the same unique descriptor set.

### Unintentional differences (to fix)
- Fixed: A64 owned a local `BlockRange64` vector and overlap loop instead of consuming the shared
  backend owner. Registration and invalidation now preserve Eden's exact start-PC through
  `EndLocation().PC() - 1` closed interval and descriptor lookup ordering.

### Missing items
- None in the reviewed `RegisterNewBasicBlock` and `InvalidateCacheRanges` paths.

### Binary layout verification
- N/A: block-range information is host-only cache metadata.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/a32_emit_x64.rs` vs Eden `backend/x64/a32_emit_x64.{h,cpp}` (block ranges)

### Intentional differences
- The Rust public wrapper still supplies one start/length pair, which this owner converts to the
  same closed `u32` interval that Eden's interface queues in its Boost interval set.

### Unintentional differences (to fix)
- Fixed: range invalidation filtered cached descriptors only by their entry PC. A write touching a
  later instruction in a compiled block could therefore leave stale host code active. Every
  emitted block now registers its complete guest-PC interval before cache insertion and
  invalidation removes all overlapping descriptors through `BlockRangeInformation`.
- Fixed: the x64-only `BlockCache::invalidate_range` entry-PC filter and its associated test were
  removed; exact-descriptor removal remains owned by the cache.
- Fixed: clearing the emitter cache did not clear its range metadata. It now follows Eden's
  `EmitX64::ClearCache` then `block_ranges.ClearCache` lifecycle.

### Missing items
- None in the reviewed range registration, clear, and invalidation paths.

### Binary layout verification
- N/A: this change affects host code-cache metadata only. A full-JIT regression test mutates the
  middle instruction of an A32 block and proves that invalidating four bytes recompiles it.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/a64_emit_x64.rs` vs Eden `backend/x64/a64_emit_x64.{h,cpp}` (block ranges)

### Intentional differences
- The Rust public wrapper still supplies one start/length pair, which this owner converts to the
  same closed `u64` interval that Eden's interface queues in its Boost interval set.

### Unintentional differences (to fix)
- Fixed: range invalidation filtered cached descriptors only by their entry PC. Every emitted A64
  block now registers the complete closed guest-PC interval before cache insertion and removes all
  descriptors overlapping the requested range.
- Fixed: clearing the emitter cache did not clear its range metadata, and invalidating the last
  cached block reset the whole code buffer even though Eden only unpatches and erases selected
  descriptors. Both lifecycle paths now match Eden.

### Missing items
- None in the reviewed range registration, clear, and invalidation paths.

### Binary layout verification
- N/A: this change affects host code-cache metadata only. A full-JIT regression test mutates the
  middle instruction of an A64 block and proves that invalidating four bytes recompiles it.

## 2026-08-24 — `src/rdynarmic/src/common/mod.rs` vs Eden `common/spin_lock_x64.{h,cpp}`

### Intentional differences
- Eden only builds its x64 backend on x64 hosts. Rust currently compiles the x64 code-generator
  modules on ARM64 as well, so the architecture-independent `rxbyak` emission helper must remain
  visible there even though the generated instructions are x64 instructions.

### Unintentional differences (to fix)
- Fixed: `spin_lock_x64` was hidden behind a host-architecture `cfg`, while the unconditionally
  compiled x64 exclusive-memory emitter imported it. Clean Apple Silicon builds consequently
  failed with an unresolved import.

### Missing items
- None in the module-ownership fix.

### Binary layout verification
- N/A: this only changes Rust module visibility for a host-code emission helper.
## 2026-08-24 — `src/rdynarmic/src/common/llvm_disassemble.rs` vs Eden `common/llvm_disassemble.{h,cpp}`

### Intentional differences
- Eden conditionally uses LLVM when `DYNARMIC_USE_LLVM` is enabled. That option defaults to OFF,
  and rdynarmic currently has no LLVM integration, so Rust ports the exact non-LLVM branch for all
  three helpers rather than adding a differently formatted disassembler dependency.
- Rust accepts typed instruction pointers instead of Eden's `void*`; the fallback only formats
  their numeric addresses and never dereferences them.

### Unintentional differences (to fix)
- Fixed: the entire common owner was absent, preventing the x64 public interfaces from exposing
  Eden's `Disassemble()` result without an incorrect empty-string stub.

### Missing items
- LLVM-enabled x64, AArch32, and AArch64 instruction decoding is not available until rdynarmic
  gains an explicit equivalent of Eden's optional `DYNARMIC_USE_LLVM` build mode.

### Binary layout verification
- N/A: the fallback formats pointers and fixed diagnostic strings; it does not serialize or copy
  an architectural payload.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/a32_interface.rs` vs Eden `backend/arm64/a32_interface.cpp` and `interface/A32/a32.h`

### Intentional differences
- Rust uses a boxed inner value for stable callback/state pointers and `Result` for fallible code
  allocation/emission. If such an operation fails, Rust restores `is_executing` before returning
  the error; Eden's corresponding operations do not expose a recoverable error path.
- `AtomicU32` with `SeqCst` ordering provides the atomic operation plus barrier required by Eden's
  A32 `HaltExecution`/`ClearHalt` sequence.
- Rust stores queued closed ranges in a `Vec` instead of Boost's coalescing interval set. Passing
  every range to the shared invalidator preserves the same union of affected guest blocks.

### Unintentional differences (to fix)
- Fixed: Run/Step reset `is_executing` before deferred invalidation; successful execution now keeps
  it set until invalidation completes, matching Eden's lifecycle order.
- Fixed: deferred invalidation released its mutex before clearing or invalidating the address
  space. The mutex is now held through the full operation and the halt bit is cleared under it.
- Fixed: range construction used `saturating_sub(1)`, so a zero length produced `start..=start`
  instead of Eden's unsigned `start + length - 1` result.
- Fixed: the backend returned no `Disassemble` surface and the FPSCR setter contained non-upstream
  environment-driven logging. ARM64 disassembly now returns Eden's empty string and the setter
  only updates architectural state.
- The diagnostic block-map/state-pointer and compile-only extensions still live in this upstream
  owner. They must move behind an explicit Ruzu extension boundary in a dedicated follow-up.
- Fixed: A32 `is_executing` lived in the backend inner value instead of the public interface owner.
  Both host backends now update the `interface/a32/a32.rs::Jit` field through an explicit mutable
  reference while retaining Eden's invalidation/execution ordering.

### Missing items
- None in the reviewed public A32 method inventory or deferred-invalidation behavior.

### Binary layout verification
- PASS: this slice does not alter `A32JitState`; register and extension-register access continues
  through the existing layout-verified state arrays.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/a64_interface.rs` vs Eden `backend/arm64/a64_interface.cpp` and `interface/A64/a64.h`

### Intentional differences
- Rust uses a boxed inner value for stable callback/state pointers and returns `Result` from
  fallible ARM64 code allocation/emission. Error paths restore `is_executing`; Eden has no
  equivalent recoverable error path.
- `AtomicU32` with `SeqCst` ordering is stronger than the atomic operations used by Eden and
  preserves their cross-thread halt visibility.
- Rust iterates the 32 two-lane vectors rather than copying them with `memcpy`; `Vector` is exactly
  `[u64; 2]`, so the value and lane ordering are identical without raw memory access.
- Queued closed ranges use a `Vec` instead of Boost's coalescing interval set; invalidating every
  queued range preserves the same union of affected guest blocks.

### Unintentional differences (to fix)
- Fixed: Run/Step reset `is_executing` before deferred invalidation, and deferred invalidation
  released its mutex before operating on the address space. Both lifecycle orders now match Eden.
- Fixed: zero-length range arithmetic used saturation instead of Eden's unsigned wrapping
  `start + length - 1` expression.
- Fixed: `GetRegisters`, `SetRegisters`, `GetVector`, `SetVector`, `GetVectors`, `SetVectors`, and
  the empty ARM64 `Disassemble` result were absent from this owner.
- Fixed in the x64 JIT-state ownership slice: TPIDR accessors that only mirrored Core-owned
  backing storage were removed from this upstream owner.

### Missing items
- None in the reviewed public A64 method inventory or deferred-invalidation behavior.

### Binary layout verification
- PASS: `Vector` is two contiguous `u64` lanes (16 bytes), and the focused aggregate-accessor test
  verifies all 32 vectors preserve low/high lane order. `A64JitState` itself is unchanged.

## 2026-08-24 — `src/rdynarmic/src/interface/a32/a32.rs` vs Eden `interface/A32/a32.h`

### Intentional differences
- Rust selects the x64 or ARM64 implementation with target `cfg` blocks instead of a link-selected
  C++ `Impl`, but the public `Jit` remains the owner of the backend object and `is_executing`.
- `read_halt_reason`, raw state-pointer access, individual register helpers,
  compile-only, and block-map dumping are Ruzu diagnostic/tool extensions beyond Eden's public
  interface. They delegate to host backends and do not replace an upstream method.

### Unintentional differences (to fix)
- Fixed: the public owner previously delegated `is_executing` to duplicated backend fields. It now
  owns one boolean on both hosts, and backend Run/Step receive that exact state so callbacks observe
  Eden's `false -> true -> false` lifecycle.

### Missing items
- The diagnostic/tool methods still need a separate explicit extension trait or module before the
  upstream public owner is structurally exact.

### Binary layout verification
- N/A: `Jit` is a host-only opaque owner in both implementations; no field is copied or serialized
  as an architectural payload.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/a32_interface.rs` vs Eden `backend/x64/a32_interface.cpp`

### Intentional differences
- Rust's fixed-size executable allocation is committed when `BlockOfCode` is created, so there is
  no separate `EnsureMemoryCommitted` operation after the one-megabyte capacity check.
- The emitter retains the same one-megabyte check as a defensive guard for direct emitter tests
  and tools. Production Run, Step, dispatcher lookup, and compile-only paths reach the interface
  check first.
- W^X transitions surround slow-path compilation explicitly; Eden performs them through its code
  emission machinery. Callback trampolines and diagnostic hooks are Rust ABI/adaptation code.

### Unintentional differences (to fix)
- Fixed: low code space was handled inside `A32EmitX64`, clearing emitter metadata without resetting
  the interface RSB. The cache-miss owner now applies Eden's strict `< 1 MiB` condition, requests a
  whole-cache invalidation, clears its halt request under the mutex, resets the RSB, and recompiles.
- Fixed: Run and Step stored execution state in the x64 backend. They now update the A32 public
  interface owner's field at Eden's exact lifecycle points.

### Missing items
- Diagnostic state-pointer, compile-only, trace, and block-map facilities remain mixed into
  this upstream-owned backend file pending an explicit Ruzu extension boundary.

### Binary layout verification
- PASS: no JIT-state field or callback ABI layout changed. Focused full-JIT tests prove that exactly
  one mebibyte remaining preserves existing blocks, while less than one mebibyte clears blocks and
  stale RSB descriptor/code-pointer entries.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/a64_interface.rs` vs Eden `backend/x64/a64_interface.cpp`

### Intentional differences
- Rust uses a fully committed fixed-size code allocation, so Eden's `EnsureMemoryCommitted` call has
  no separate counterpart; its preceding one-megabyte capacity policy is preserved literally.
- Rust executes the emitter-provided entrypoint directly. Eden rounds `GetCurrentBlock()` upward to
  sixteen bytes, but rdynarmic's entrypoint already identifies the first executable instruction;
  applying Eden's pointer rounding skips emitted code, as covered by the x64 execution regression.
- The emitter keeps a defensive capacity guard for direct callers, while all production interface
  cache misses now take the upstream-owned check. W^X transitions, callback trampolines, and Ruzu
  diagnostic hooks are host-language/runtime adaptations.

### Unintentional differences (to fix)
- Fixed: the emitter alone evacuated a nearly full code cache, leaving interface RSB entries that
  could point into reused code. Every A64 interface cache miss now applies Eden's strict `< 1 MiB`
  check through whole-cache invalidation before translation/emission.
- Fixed: range invalidation and whole-cache invalidation now share the same locked lifecycle, reset
  the RSB first, and process every queued closed range before Run/Step report non-execution.

### Missing items
- Fixed in the x64 JIT-state ownership slice: TPIDR values are no longer mirrored through backend
  accessors or raw-state fields. Diagnostic raw-pointer/trace facilities remain mixed into this
  upstream-owned backend file pending an explicit Ruzu extension boundary.

### Binary layout verification
- PASS: no A64 JIT-state or callback payload layout changed. The focused capacity test covers the
  exact threshold and verifies stale RSB descriptor/code-pointer removal below it.

## 2026-08-24 — `src/rdynarmic/src/interface/a64/a64.rs` vs Eden `interface/A64/a64.h`

### Intentional differences
- Target-specific Rust `Jit` definitions replace the C++ pImpl selected by the build, while retaining
  the same public method owner and complete register/vector surface.
- Raw state/halt pointers, halt inspection, and tuple vector compatibility are Ruzu
  integration extensions delegated to the selected backend.

### Unintentional differences (to fix)
- Fixed in the associated host owners: both x64 and ARM64 implementations now provide the complete
  aggregate register/vector accessors, disassembly surface, invalidation ordering, and execution
  state queried by this public interface.

### Missing items
- The Ruzu-only integration methods need an explicit extension boundary before this owner can be
  structurally identical to the upstream header.

### Binary layout verification
- PASS: public `Vector` remains `[u64; 2]`; aggregate access tests verify 31 GPRs and 32 vectors,
  including low/high lane order.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/a32_jitstate.rs` vs Eden `backend/x64/a32_jitstate.{h,cpp}`

### Intentional differences
- Rust spells the fields and methods with Rust naming conventions and uses explicit zeroed padding
  before `ext_reg`; the padding reproduces Eden's implicit `alignas(16)` gap and makes reserved
  bytes deterministic.
- Compile-time `offset_of!` helpers expose the same offsets to the Rust x64 emitter that C++ obtains
  with `offsetof`.

### Unintentional differences (to fix)
- Fixed: A32 state was combined with A64 in `jit_state.rs`; it now has the matching upstream owner.
- Fixed: the raw state had appended `exclusive_value` and `cntpct` fields absent from Eden. The safe
  no-global-monitor fallback value now belongs to `A32JitInner`; the unused A32 CNTPCT extension was
  removed.
- Fixed: `TransferJitState` was missing. The port copies Eden's exact architectural/MXCSR/FPSR
  fields, preserves `halt_reason`, clears `exclusive_state`, and conditionally copies or resets RSB.

### Missing items
- None in the state fields, constants, or method inventory of the reviewed header/source pair.

### Binary layout verification
- PASS: a program compiled against Eden's header and the Rust layout test both report alignment 16,
  size 528, and identical offsets for every field from `reg` at 0 through `fpsr_nzcv` at 512.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/a64_jitstate.rs` vs Eden `backend/x64/a64_jitstate.{h,cpp}`

### Intentional differences
- Rust uses explicit zeroed padding after `exclusive_state` to reproduce the C++ compiler's padding
  before `rsb_ptr`, and compile-time offset helpers stand in for C++ `offsetof` calls.

### Unintentional differences (to fix)
- Fixed: A64 state was combined with A32 in `jit_state.rs`; it now has the matching upstream owner.
- Fixed: `exclusive_value`, `tpidr_el0`, and `tpidrro_el0` incorrectly extended the generated-code
  state. The fallback exclusive value now belongs to `JitInner`, while TPIDR uses the configured
  stable pointers exactly like Eden.

### Missing items
- None in the state fields, constants, or method inventory of the reviewed header/source pair.

### Binary layout verification
- PASS: a program compiled against Eden's header and the Rust layout test both report alignment 16,
  size 960, and identical offsets for every field from `reg` at 0 through `fpcr` at 944.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/emit_a64.rs` vs Eden `backend/x64/a64_emit_x64.cpp` (TPIDR)

### Intentional differences
- Rust represents nullable TPIDR pointers as `Option<*mut u64>`/`Option<*const u64>` and materializes
  their addresses through rxbyak's Rust API; the emitted load/store sequence is otherwise the same.

### Unintentional differences (to fix)
- Fixed: TPIDR instructions read and wrote private fields appended to `A64JitState`. `MRS/MSR
  TPIDR_EL0` and `MRS TPIDRRO_EL0` now load/store through the pointers embedded from `UserConfig`;
  null reads return zero and null writes do nothing.

### Missing items
- None in the three reviewed TPIDR emitter methods.

### Binary layout verification
- N/A: TPIDR backing storage is host-owned and outside raw JIT state. An executing five-instruction
  regression verifies both configured reads and the configured write.

## 2026-08-24 — `src/rdynarmic/src/interface/{a32/a32,a64/a64}.rs` and host interfaces vs Eden `interface/{A32/a32,A64/a64}.h` and `backend/{x64,arm64}/*_interface.cpp`

### Intentional differences
- When no global monitor is configured, Ruzu's safe callback fallback retains expected exclusive
  values in the host interface owner. These values are not visible to generated code and are reset
  with the architectural state.

### Unintentional differences (to fix)
- Fixed: public A32 CNTPCT accessors stored an inert value that no A32 instruction consumed.
- Fixed: public A64 TPIDR accessors duplicated Core's configured backing storage; the read-only
  setter also cast Eden's `const` TPIDRRO pointer to mutable. Core now reads/writes its stable
  backing allocations directly, as Eden's `ArmDynarmic64` does.

### Missing items
- Ruzu diagnostic state pointers, halt inspection, compile-only, trace, and block-map helpers still
  require a separate extension boundary to make the public/upstream interface owners exact.

### Binary layout verification
- PASS: removing the extra values from raw state is covered by the exact A32/A64 layout assertions;
  the host interface fallback storage has no generated-code or serialized ABI.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/jitstate_info.rs` vs Eden `backend/x64/jitstate_info.h`

### Intentional differences
- Rust uses explicit `from_a32` and `from_a64` constant constructors because it cannot express
  Eden's templated constructor over arbitrary standard-layout JIT-state types.
- `EmitContext` carries a value copy supplied by `BlockOfCode` because Rust emission temporarily
  borrows the assembler separately from its owner. The copied ten-field inventory is immutable for
  the block and is the counterpart of calling Eden's `BlockOfCode::GetJitStateInfo()`.

### Unintentional differences (to fix)
- Fixed: the upstream file owner was missing and `block_of_code.rs` held only three offsets, while
  RSB, CPSR, and FPSR consumers independently selected A32 or A64 layouts.
- Fixed: the shared saturation emitter hard-coded `A64JitState::fpsr_qc`; A32 saturation now writes
  its own QC field through the `JitStateInfo` supplied by its `BlockOfCode`.

### Missing items
- None in the reviewed ten-field `JitStateInfo` inventory.

### Binary layout verification
- N/A: `JitStateInfo` describes host-side byte offsets and is neither copied to guest memory nor
  serialized. Focused tests verify all ten values against both exact JIT-state layouts.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/emit_saturation.rs` vs Eden `backend/x64/emit_x64_saturation.cpp`

### Intentional differences
- Runtime `SaturationOp` and bit-width matches replace Eden's template instantiations while keeping
  the same shared signed/unsigned helper boundaries and emitted operation ordering.
- The mechanical `emit_or_qc` helper centralizes Eden's repeated byte-sized QC update without
  moving its ownership outside this file.
- For the signed-saturation `N == 32` pseudo-result, Rust emits a zero value because its emission
  context holds an immutable IR block; Eden replaces uses with an immediate false during emission.
  Both paths expose the same value to generated code.

### Unintentional differences (to fix)
- Fixed: every QC update used the A64 offset, including A32 instructions.
- Fixed: unsigned saturated add/sub used branchful saturation and QC paths; they now preserve
  Eden's scratch-operand ownership, boundary move, `cmovae`, `setb`, and byte-sized sticky-QC OR.
- Fixed: signed doubling multiply used a compare/branch special case; both widths now reproduce
  Eden's doubled-product, sign test, conditional move, and sticky-QC sequence.

### Missing items
- None in the reviewed saturation opcode/helper inventory.

### Binary layout verification
- PASS: the focused A32 regression inspects emitted addressing and verifies that the QC write uses
  offset 508 rather than A64 offset 940; both offsets are covered by the exact JIT-state layouts.
  The executing scalar-saturation regression also verifies results and the architectural Q flag.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/a32_emit_a32.rs` vs Eden `backend/x64/a32_emit_x64.cpp` (`EmitA32OrQFlag`)

### Intentional differences
- Rust uses `ArgumentInfo` and rxbyak's Rust register conversions, preserving Eden's immediate and
  register branches without changing method ownership.

### Unintentional differences (to fix)
- Fixed: the register path ORed a full dirty 32-bit temporary into the one-bit `cpsr_q` field.
  It now ORs only `value.cvt8()`, while immediate one stores a dword one and immediate zero emits
  nothing, exactly as Eden does.

### Missing items
- None in the reviewed `EmitA32OrQFlag` method.

### Binary layout verification
- PASS: no state layout changed; the executing A32 scalar-saturation regression confirms `cpsr_q`
  remains exactly zero or one after SSAT/USAT/QADD-family flag updates.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/a32_emit_x64_memory.rs` vs Eden `backend/x64/{a32_emit_x64_memory.cpp,emit_x64_memory.cpp.inc}` and `a32_emit_x64.h`

### Intentional differences
- Rust expresses Eden's member methods as architecture-owned free functions. The A32 fallback-table
  loop, scalar stub primitives, and three fallback maps all live in this file; only the mechanical
  intra-buffer relative-call encoder is shared with A64.
- Opt-in `RUZU_*` diagnostic traps and tracing remain in the A32 memory owner. They emit no code
  unless explicitly selected through the environment and preserve the normal upstream path.
- Faulting direct-fastmem instructions resume at an inline memory-abort check when that check is
  enabled; normal fastmem execution jumps over it. This is the Rust exception-handler counterpart
  of Eden's deferred fallback handler calling `EmitCheckMemoryAbort` before joining `end`.

### Unintentional differences (to fix)
- Fixed: all A32 memory emitters and the per-instruction fallback stub were owned by
  `a32_emit_a32.rs`/`a32_emit_x64.rs`; they now live in the counterpart memory module.
- Fixed: `EmitCheckMemoryAbort` was absent. Callback, page-table fallback, direct-fastmem fallback,
  and exclusive paths now test `MemoryAbort`, restore the exact A32 PC/upper descriptor, and force
  a dispatcher return in Eden's ordering.
- Fixed: exclusive inline selection ignored `fastmem_exclusive_access`; disabled configurations
  now use the callback path as upstream requires.
- Fixed: inline exclusive reads used an unordered MOV and unordered fallback. They now use Eden's
  always-ordered `lock xadd` sequence and ordered callback stub for every scalar width.
- Fixed: a `do_not_fastmem` marker selected the entire non-inline exclusive path. The inline owner
  now retains Eden's monitor lock/address/value lifecycle and calls the pre-generated fallback
  under that lock when only the individual fastmem instruction has been disabled.
- Fixed: A32 reused the A64 fallback generator and emitted unused 128-bit tables. Its owner now
  generates exactly the upstream 8/16/32/64 inventory and registers Eden's exact per-width perf
  symbol names.
- Fixed: exclusive fastmem faults generated a second per-instruction callback stub, and exclusive
  writes resumed directly after `cmpxchg`, where callback-modified host flags could produce a
  wrong status. All A32 accesses now use the architecture-owned pre-generated tables; exclusive
  write faults resume in Eden's explicit `AL`-to-status continuation before unlocking.
- Fixed: generated accesses to `exclusive_state` used dword operations. Clear/set/test operations
  now use the byte-sized field semantics emitted by Eden.

### Missing items

### Binary layout verification
- PASS: no `A32JitState` field or offset changed. Memory-abort tests verify the embedded A32 resume
  PC, upper descriptor, halt-reason offset, and disabled no-op path; the fallback inventory test
  verifies exactly `2 * 14 * 14 * 4` entries per scalar table and rejects 128-bit A32 entries.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/{a32_emit_x64_memory,a32_interface}.rs` vs Eden `backend/x64/{a32_emit_x64_memory.cpp,emit_x64_memory.cpp.inc}`

### Intentional differences
- Rust host trampolines recover `A32JitInner` in `a32_interface.rs`; the exclusive-monitor
  operations corresponding to Eden's generated lambdas live beside the generated-code owner in
  `a32_emit_x64_memory.rs`.
- The shared callback container retains an unused clear-exclusive slot for the existing Rust FFI
  boundary, while A32 generated code clears `A32JitState::exclusive_state` directly like Eden.

### Unintentional differences (to fix)
- Fixed: non-inline exclusive callbacks previously owned the generated-code reservation lifecycle
  and maintained a private `exclusive_value`. The emitter now sets, tests, atomically consumes, and
  clears `exclusive_state` in Eden's exact order, while the global monitor alone owns the expected
  value.
- Fixed: non-inline exclusive operations could run without a global monitor. Their emitters now
  enforce Eden's `ASSERT(conf.global_monitor != nullptr)` precondition.
- Fixed: scalar exclusive reads did not explicitly zero-extend the callback result after the host
  call. They now apply Eden's per-width zero extension before the memory-abort check.
- Fixed: the A32 differential-test harness could generate LDREX/STREX without configuring Eden's
  required global monitor. Its one-CPU test configuration now owns a monitor for the complete JIT
  lifetime.

### Missing items
- No missing item remains in the reviewed A32 scalar non-inline exclusive path.

### Binary layout verification
- PASS: the host-only private `exclusive_value` was removed from `A32JitInner`; the generated-code
  `A32JitState` layout is unchanged. An executing LDREX/STREX/STREX regression verifies the global
  monitor value, first-store success, reservation consumption, and second-store failure.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/constants.rs` vs Eden `backend/x64/constants.h`

### Intentional differences
- Eden's `Cmp`, `CmpInt`, `Tern`, and `FpClass` namespaces are Rust modules, with constant and enum
  spellings changed only to Rust naming conventions.
- Rust has no default function arguments, so `fixup_lut` requires all eight `FpFixup` operands;
  callers that omit trailing operands upstream pass `FpFixup::Dest` explicitly.
- `convert_rounding_mode_to_x64_immediate` retains Eden's `Option<i32>` result. Consumers cast the
  proven two-bit value to `u8` only at rxbyak's more strongly typed instruction boundary.

### Unintentional differences (to fix)
- Fixed: the complete constants owner was absent and consumers embedded raw compare predicates and
  duplicated rounding-mode maps. The full predicate, ternary, floating-point class, fixup, range,
  and rounding inventories now live in the matching module.

### Missing items
- No item is missing from the reviewed constants owner; AVX-512 consumers that use some currently
  unreferenced constants remain part of their respective emitter-file audits.

### Binary layout verification
- PASS: `FpFixup`, `FpRangeSelect`, and `FpRangeSign` use `repr(u8)` and exhaustive tests verify all
  discriminants, bitmasks, LUT bit placement, aliases, and rounding immediates.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/oparg.rs` vs Eden `backend/x64/oparg.h`

### Intentional differences
- Rust stores the register-or-address alternatives in `RegMem` rather than a manually tagged C++
  union. The default/uninitialized C++ `Operand` state is represented by `None` and rejected when
  consumed; every upstream `UseOpArg` consumer initializes the wrapper before use.
- Rust passes the copyable wrapper through rxbyak's `Into<RegMem>` boundary rather than exposing
  C++ dereference operators. `set_bit` retains the same register conversion and address-size
  behavior.
- `EmitMul32` materializes an immediate second operand before the two-operand `imul`, because the
  Rust rxbyak surface does not expose Xbyak's three-operand immediate overload. The resulting
  lower 32-bit value is identical; only the emitted instruction sequence differs.

### Unintentional differences (to fix)
- Fixed: the matching owner and `RegAlloc::UseOpArg` boundary were absent. Add/sub, multiply,
  multiply-high, AND/OR/EOR, and AND-NOT consumers now use the wrapper at all 15 upstream sites
  instead of duplicating register-width conversion locally.
- Fixed: both AND-NOT emitters materialized immediate operands through the generic register path.
  They now preserve Eden's immediate `MOV`/`AND` branches and their exact signed-32-bit selection
  rule for the 64-bit operation.

### Missing items
- None in the reviewed `OpArg` owner or its current `emit_x64_data_processing.cpp` consumers.

### Binary layout verification
- N/A: `OpArg` is an emission-time tagged operand and is never serialized or copied into guest/JIT
  state. Focused tests cover the default state, all four upstream GPR widths, address-size changes
  without altering the address expression, and executing immediate AND-NOT paths.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/{block_of_code,a32_emit_x64,a64_emit_x64}.rs` vs Eden `backend/x64/{block_of_code,a32_emit_x64,a64_emit_x64}.{h,cpp}` (prelude lifecycle)

### Intentional differences
- Rust's `gen_run_code` returns byte-offset dispatcher labels to the owning emitter, whereas Eden's
  `BlockOfCode` stores native function pointers. Both now leave the prelude open until the
  architecture emitter explicitly completes it.
- `rxbyak` reserves the complete executable buffer up front, so there is no Linux operation
  corresponding to Eden's Windows-only incremental `EnsureMemoryCommitted` implementation.

### Unintentional differences (to fix)
- Fixed: `gen_run_code` completed the prelude before architecture fallback tables and terminal
  handlers were emitted. Cache clearing therefore rewound into permanent code and could overwrite
  it. A32 now emits fallbacks, terminal handlers, then completes the prelude exactly in Eden's
  constructor order; A64 does the same for its currently ported permanent stubs.
- Fixed: A64 generated terminal handlers before fallback tables and regenerated both after every
  cache clear. Permanent stubs now remain below `code_begin` and `clear_cache` only rewinds dynamic
  blocks, as Eden's `EmitX64::ClearCache` does.
- Fixed: both architecture emitters performed their own low-space cache clear in addition to the
  interface-owned capacity check. Capacity invalidation remains solely in the matching A32/A64
  interface owner.
- Fixed: x64 execution tests configured 4 MiB caches despite Eden documenting an approximately
  8 MiB minimum. The complete A32 prelude left less than the mandatory 1 MiB compilation reserve,
  causing perpetual clear/recompile dispatch. Test configurations now use 16 MiB.

### Missing items
- Resolved by the 2026-08-24 A64 memory-prelude follow-up below.

### Binary layout verification
- N/A: this changes generated-code lifecycle and test cache capacity, not either architecture's
  JIT-state layout. The executing x64 back-edge regression verifies bounded linked and unlinked
  dispatch after the corrected prelude boundary.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/{a64_emit_x64_memory,a64_emit_x64,a64_interface,emit_context,emit_x64_memory}.rs` vs Eden `backend/x64/{a64_emit_x64_memory.cpp,a64_emit_x64.{h,cpp},emit_x64_memory.{h,cpp.inc},callback.cpp}`

### Intentional differences
- Rust stores generated-code byte offsets in `Memory128Accessors` and resolves them relative to the
  owning code buffer; Eden stores native function pointers. The accessors remain below
  `code_begin` and survive cache clears under the same lifetime.
- Rust `ArgCallback` objects call explicit `extern "C"` trampolines instead of devirtualizing C++
  member functions. System V passes 128-bit values as scalar lanes and Windows uses 16-byte stack
  payloads after shadow space, preserving the platform ABI selected by Eden's `_WIN32` branches.
- Existing opt-in `RUZU_*` diagnostic traps remain in the A64 memory owner. They emit no guest path
  changes unless explicitly enabled through the environment.

### Unintentional differences (to fix)
- Fixed: A64 omitted `GenMemory128Accessors`, generated no ordinary 128-bit write fallbacks, and
  routed ordinary `ReadMemory128`/`WriteMemory128` through a separate callback-only owner. The
  generated accessors, all 6,048 fallback entries, dispatcher ownership, and permanent-prelude
  ordering now match Eden.
- Fixed: ordered 128-bit packing and extraction unconditionally emitted SSE4.1 instructions. The
  SSE2 `movq`/`punpck*qdq` alternatives now follow `emit_x64_memory.h`.
- Fixed: direct callback and deferred fastmem/page-table fallbacks skipped Eden's post-access
  `EmitCheckMemoryAbort`; all scalar and 128-bit paths now restore the exact A64 resume PC and force
  a dispatcher return when `MemoryAbort` is set.
- Fixed: the raw 128-bit exclusive-write trampoline used pointer payloads on System V. The generated
  accessor now fills `ABI_PARAM3` through `ABI_PARAM6` and calls a scalar-lane trampoline there,
  while Windows retains Eden's two pointer payloads and compiler-specific hidden-return ordering.

### Missing items

### Binary layout verification
- PASS: Windows generated accessors use exact 16-byte value/expected payloads after the 32-byte
  shadow space; System V transfers two 64-bit lanes per value. Linux execution covers direct and
  faulting fastmem `LDR/STR Q`, while Windows and AArch64 cross-target test builds pass.

## 2026-08-24 — `src/rdynarmic/src/tests_a32_fuzz.rs` and `tools/a32_oracle.cpp` vs Eden A32 JIT differential-test behavior

### Intentional differences
- Test-only runners retain one Rust JIT and one Eden oracle process per test thread. Between cases
  they reset CPU state, clear the complete code cache, replace code memory, clear data memory,
  restore the 200-tick budget, and use the same optimization configuration as the former
  one-process/one-JIT-per-case path; the first case starts from the equivalent fresh state.
- The local oracle adds a `BATCH` protocol around Eden's public A32 JIT interface. This is tooling
  only; the one-shot and existing `INIT` protocols remain compatible.

### Unintentional differences (to fix)
- Fixed: the differential harness rebuilt both JITs and spawned an Eden process for every case,
  making the full-coverage fuzz tests appear blocked for minutes. Persistent runners now preserve
  case isolation while removing initialization overhead.
- Fixed during verification: calling `ClearCache` before `Reset` lost Eden's pending
  `CacheInvalidation` halt bit because `Reset` zeroes the JIT state. Both runners now perform
  `Reset` before `ClearCache`, so the following `Run` consumes the invalidation exactly as Eden's
  A32 interface requires.
- Fixed: the Eden oracle left `UserConfig::global_monitor` null even though the generated Thumb32
  corpus includes exclusive loads and stores. Those cases terminated the oracle with SIGSEGV and
  were silently skipped; all oracle modes now own a one-core `ExclusiveMonitor`, matching the Rust
  runner and restoring the missing comparisons.

### Missing items
- None for the reviewed differential-test lifecycle. An older externally configured oracle falls
  back to the original one-shot protocol, and a failed batch session also falls back safely.

### Binary layout verification
- PASS: the text protocol still transfers exactly 15 input GPRs, an instruction count, the code
  words, 16 output GPRs, and CPSR. No production or guest-visible binary structure changed.

## 2026-08-24 — `src/core/src/hle/service/bcat/{bcat,service_creator}.rs` vs Eden `src/core/hle/service/bcat/{bcat,service_creator}.{h,cpp}`

### Intentional differences
- Rust uses explicit `ServiceFramework` callbacks and `CmifResponse` in place of Eden's compile-time
  `D<&IServiceCreator::...>` CMIF wrappers. Commands 0 and 1 read the same out-of-band client process
  ID, while command 2 reads the same raw application ID; all return one IPC interface.
- Rust shares the null BCAT backend through `Arc<Mutex<dyn BcatBackend + Send>>`; Eden owns the
  backend with `unique_ptr` and lends a reference to each `IBcatService`.
- Rust retains `SystemRef` and an `Arc<Mutex<FileSystemController>>` instead of C++ references. The
  controller and runtime program ID remain owned by `System`, and their lookup ordering matches
  Eden.

### Unintentional differences (to fix)
- None in commands 0, 1, or 2.

### Missing items
- None in the three commands implemented by Eden. Commands 3 and 4 remain null entries on both
  sides.

### Binary layout verification
- N/A: `ClientProcessId` is supplied by the HIPC handle descriptor rather than serialized in the
  raw CMIF payload. Command 2 carries one aligned `u64`; every response contains only a result code
  plus one IPC interface.

## 2026-08-24 — `src/core/src/hle/service/bcat/delivery_cache_storage_service.rs` vs Eden `src/core/hle/service/bcat/delivery_cache_storage_service.{h,cpp}` (`EnumerateDeliveryCacheDirectory`)

### Intentional differences
- Rust protects `entries` and `next_read_index` with mutexes because service callbacks receive a
  shared reference. Both values remain owned together by `IDeliveryCacheStorageService`, and the
  lock scope preserves Eden's count, copy, then index-advance ordering.

### Unintentional differences (to fix)
- None in the three storage-service commands implemented by Eden.

### Missing items
- None. Commands 0 and 1 return their corresponding child interfaces; command 10 uses Eden's HIPC
  map-alias output buffer and signed 32-bit count.

### Binary layout verification
- PASS: `DirectoryName` retains its fixed upstream payload layout; the handler copies complete
  entries and serializes the count as `s32`.

## 2026-08-24 — `src/video_core/src/texture_cache/image_view_info.rs` vs Eden `src/video_core/texture_cache/image_view_info.{h,cpp}`

### Intentional differences
- Rust expresses Eden's mutable local `TextureType` switch as an enum match. The promotion rules and
  subsequent view-type switch remain in the same file and order.

### Unintentional differences (to fix)
- Fixed: Ruzu previously skipped Eden's promotion of 1D, 2D, and cube TIC types to their array forms
  when `Depth() > 1` or `base_layer != 0`. A layered 2D TIC consequently reached the scalar 2D
  assertion and aborted the GPU thread.

### Missing items
- None in the TIC view-type selection path.

### Binary layout verification
- PASS: `ImageViewInfo` fields and `repr(C)` layout are unchanged; this correction only restores the
  constructor's type-selection control flow. Focused tests cover depth-driven and base-layer-driven
  2D-array promotion.

## 2026-08-24 — `src/core/src/hle/service/ns/{service_getter_interface,application_manager_interface}.rs` vs Eden `src/core/hle/service/ns/{service_getter_interface,application_manager_interface}.{h,cpp}`

### Intentional differences
- Rust returns the child service with an explicit `ResponseBuilder` IPC interface instead of Eden's
  `Out<SharedPointer<IApplicationManagerInterface>>` serialization wrapper. `SystemRef` preserves
  the same non-owning system lifetime.

### Unintentional differences (to fix)
- The application-manager child now exists and owns its command table, but most Eden-implemented
  application-manager callbacks and its service-context events are not yet ported.
- Several other `IServiceGetterInterface` child getters remain disconnected despite being wired in
  Eden; the application-manager getter required by the observed launch path is now connected.

### Missing items
- Full `IApplicationManagerInterface` method/event parity and the remaining service-getter child
  callbacks.

### Binary layout verification
- N/A: this slice creates and transfers IPC service objects; it does not introduce a raw-memory
  payload. The focused test verifies that command 7996 returns the application-manager owner with
  its current command table.

## 2026-08-25 — `src/shader_recompiler/src/frontend/{location,control_flow,maxwell_opcodes}.rs` and `pipeline_cache.rs` vs Eden Maxwell frontend

### Intentional differences
- Rust materializes Eden's `maxwell.inc` macro table once through `OnceLock`. The 280 names,
  encodings, source order, and first-match decode rule are identical to upstream.
- Rust converts absolute Maxwell `Location` ranges to indices into a cached instruction slice.
  The slice's absolute base offset is therefore carried into materialization so scheduling words
  are skipped on Eden's absolute 32-byte grid.

### Unintentional differences (to fix)
- The code-slice-only OpenGL helpers still build their CFG with the older relative-word builder;
  the environment-owned Vulkan path now uses the upstream absolute `Location` path.

### Missing items
- Direct ownership parity for upstream `Translate(Environment&, IR::Block*, location_begin,
  location_end)` remains part of the wider shader translation refactor.

### Binary layout verification
- N/A: these files decode and translate instructions without defining a serialized payload.
  Regression tests cover non-zero scheduling-grid phases and the upstream opcode collisions.

## 2026-08-25 — `src/video_core/src/{shader_environment,shader_cache}.rs` vs Eden `shader_environment.{h,cpp}` and `shader_cache.{h,cpp}`

### Intentional differences
- `GenericEnvironmentOwner` represents C++ base-subobject access without erasing the concrete
  graphics or compute environment required by virtual resource callbacks.

### Unintentional differences (to fix)
- None in the reviewed shader-size and slow cache-analysis paths.

### Missing items
- None in `TryFindSize` termination or `ShaderCache::MakeShaderInfo` CFG ownership.

### Binary layout verification
- PASS: the existing serialized environment layout is unchanged. Tests verify the non-proprietary
  EXIT terminator and the proprietary-driver self-branch behavior separately.

## 2026-08-25 — Vulkan shader/rasterizer invalidation and compute-cache parity

### Intentional differences
- Ruzu mirrors Maxwell dirty flags for a draw while Eden's state tracker points directly at live
  flags. When pipeline configuration rotates the command buffer, Ruzu reapplies Eden's
  invalidation mask to that draw-scoped mirror.
- `Option<Box<ComputePipeline>>` represents Eden's stable node-owned pointer and its null negative
  cache entry without moving successful pipelines when the Rust hash map grows.

### Unintentional differences (to fix)
- Graphics-pipeline translation and runtime-info construction remain owned by
  `graphics_pipeline.rs` instead of the upstream `vk_pipeline_cache.cpp` counterpart.
- Runtime graphics pipeline construction remains conditional on the asynchronous-shader setting,
  while Eden always submits compilation to its worker pool and controls only whether the caller
  waits.

### Missing items
- The graphics-pipeline ownership and runtime-info parity slice, including MoltenVK-only fragment
  color types, geometry passthrough/layer emulation, geometry point size, and the device XFB guard.

### Binary layout verification
- PASS: pipeline key serialization is unchanged. Focused tests cover negative compute-cache state,
  draw-scoped invalidation, image capability collection, and zero-register vector accesses.

## 2026-08-25 — `src/video_core/src/renderer_vulkan/{pipeline_cache,graphics_pipeline}.rs` vs Eden `vk_pipeline_cache.{h,cpp}` and `vk_graphics_pipeline.{h,cpp}`

### Intentional differences
- `GraphicsPipelineBuilder` is a Rust lifetime adapter for the state captured by Eden's pipeline
  worker jobs. Shader translation, `MakeRuntimeInfo`, SPIR-V emission, module creation, and layer
  emulation remain owned by `pipeline_cache.rs`; Vulkan graphics-pipeline construction remains
  owned by `graphics_pipeline.rs`.
- Rust stores the six translated programs as `Option<Program>` because `Program` has no inert
  default value. Eden uses a default-constructed `std::array<Program, 6>`; the same populated slots
  participate in runtime-info construction and emission.
- Disk environments are collected before worker submission to satisfy the Rust parser callback's
  borrow boundaries. Existing cache entries are skipped before submission rather than by
  `try_emplace` inside the worker; the resulting positive and negative runtime cache states are
  unchanged.
- GPU pipeline/shader logging remains unavailable because the project-wide Eden GPU logging
  subsystem is not ported. Standard shader errors and pipeline hashes remain logged.
- Android's configurable pipeline-worker count is not applicable to the currently supported
  desktop targets; non-Android worker-count selection matches Eden.

### Unintentional differences (to fix)
- None in the reviewed runtime-info, graphics/compute translation, pipeline build scheduling, or
  negative-cache paths.

### Missing items
- None in the ownership and behavior slice covered by the two parity reports. Project-wide GPU
  logging remains a separate missing subsystem.

### Binary layout verification
- PASS: compute and graphics pipeline cache keys and their serialized byte encodings are unchanged.
  The ownership move only relocates implementation logic. Focused tests cover MoltenVK-only color
  types, geometry point size, transform-feedback capability guards, geometry passthrough state,
  negative compute entries, and fixed-state serialization.

## 2026-08-25 — `src/video_core/src/query_cache/query_stream.rs` vs Eden `src/video_core/query_cache/query_stream.h`

### Intentional differences
- Rust represents Eden's virtual base class as `StreamerInterfaceBase` plus the
  `StreamerInterface` trait; both dependency masks remain owned by that base state.

### Unintentional differences (to fix)
- None. Fixed the inherited yuzu behavior where `get_dependent_mask` returned
  `dependence_mask`; it now returns Eden's distinct `dependent_mask`.

### Missing items
- None in the reviewed streamer base and simple-streamer owner.

### Binary layout verification
- N/A: the streamer state is internal and is not serialized or copied as a raw payload.
  The focused regression test verifies both directions of the dependency relationship.

## 2026-08-25 — `src/video_core/src/engines/puller.rs` vs Eden `src/video_core/engines/puller.{h,cpp}`

### Intentional differences
- Rust passes `true` to `RasterizerInterface::release_fences` because the Rust interface exposes
  the force argument explicitly; Eden's puller calls its argument-less wrapper.
- Raw engine identifiers are represented by the transparent `EngineID` newtype so unsupported
  values retain Eden's `static_cast<EngineID>` bit pattern.

### Unintentional differences (to fix)
- None in the corrected semaphore-trigger path. An unsatisfied `AcquireEqual`, `AcquireGequal`, or
  `AcquireMask` now releases fences once and returns, matching Eden's single-pass
  `do { ... } while (false)` control flow instead of busy-waiting in the puller thread.

### Missing items
- None in the reviewed bind, dispatch, fence, and semaphore paths. `NV01_TIMER` now binds and
  dispatches through its matching engine counterpart.

### Binary layout verification
- PASS: `PullerRegs` remains a 0x800-word register array and all typed accessors retain Eden's
  asserted word offsets. The focused regression test verifies the one-pass acquire state changes.

## 2026-08-25 — `src/video_core/src/engines/engine_interface.rs` vs Eden `src/video_core/engines/engine_interface.h`

### Intentional differences
- Rust extracts the inherited fields into `EngineInterfaceState` and exposes
  `has_pending_methods` to preserve Eden's guarded `ConsumeSink` behavior across trait objects.
- `EngineHandle` retains Eden's non-owning engine-pointer semantics for Rust fat pointers.

### Unintentional differences (to fix)
- None. Restored `Nv01Timer = 0` and the exact discriminants of all following `EngineTypes`.
- None. `consume_sink` now calls `consume_sink_impl` only when the method sink is non-empty, as
  Eden does.

### Missing items
- None in the reviewed interface and shared state.

### Binary layout verification
- N/A: this interface state is not serialized or copied as a raw payload. A focused test verifies
  every `EngineTypes` discriminant.

## 2026-08-25 — `src/video_core/src/engines/nv01_timer.rs` vs Eden `src/video_core/engines/nv01_timer.h`

### Intentional differences
- The ignored `MemoryManager&` constructor argument is accepted as an `Arc<Mutex<MemoryManager>>`
  to match the existing Rust engine construction boundary; neither implementation stores it.
- Inherited `EngineInterface` fields live in `EngineInterfaceState` because Rust has no field
  inheritance.

### Unintentional differences (to fix)
- None. Single and multi-method calls only log their arguments, and `consume_sink_impl` remains an
  intentional no-op exactly like Eden.

### Missing items
- None.

### Binary layout verification
- PASS: `Regs` is exactly 0x48 bytes and is deterministically zero-initialized. A focused layout
  test verifies its size.

## 2026-08-25 — `src/video_core/src/control/channel_state.rs` vs Eden `src/video_core/control/channel_state.{h,cpp}`

### Intentional differences
- Eden's optional `Payload` is represented by individually boxed optional engines and a boxed DMA
  pusher so their addresses remain stable for non-owning engine handles.
- Maxwell3D guest-memory and tick callbacks are Rust adapters required by the flattened owner.

### Unintentional differences (to fix)
- None. `init` now creates the NV01 timer and calls the file-owned NVK default-subchannel helper,
  binding 3D/compute/2D/copy to subchannels 0/1/3/4 in Eden's order.

### Missing items
- None in the reviewed payload construction, default binding, and rasterizer-binding lifecycle.

### Binary layout verification
- N/A: `ChannelState` is an internal owner and is not serialized or copied as an upstream C++
  payload. A focused test verifies every NVK default binding and the deliberately empty M2MF slot.

## 2026-08-25 — `src/video_core/src/renderer_vulkan/state_tracker.rs` vs Eden `src/video_core/renderer_vulkan/vk_state_tracker.{h,cpp}`

### Intentional differences
- Rust stores the bound channel's live dirty-flag array through `NonNull` and keeps an owned
  fallback array, preserving Eden's non-owning `Flags*` lifecycle.
- `apply_command_buffer_invalidation` applies Eden's mask to the draw-scoped Rust flag mirror when
  pipeline configuration rotates the command buffer.

### Unintentional differences (to fix)
- Fixed `SetupDirtyViewports`: both `surface_clip` words in table 1 now map to `Viewports`.
- Fixed `MakeInvalidationFlags`: command-buffer invalidation now contains exactly Eden's 37 named
  flags plus all 32 vertex-buffer, vertex-attribute, and vertex-binding flags; render targets,
  rescale flags, global depth bias, and viewport swizzles remain untouched.
- Fixed constructor state: the fallback flags now start clear like Eden's `default_flags{}` instead
  of starting with every known flag dirty.
- Restored the header-owned `invalidate_state_enable_flag` operation for scheduler pipeline-state
  transitions.

### Missing items
- None in the reviewed table setup, invalidation mask, channel binding, and touch/check methods.

### Binary layout verification
- N/A: dirty flags are internal boolean lookup arrays rather than serialized payloads. Focused
  tests verify the 133-bit invalidation mask and both surface-clip table entries.

## 2026-08-25 — `src/common/src/thread.rs` vs Eden `src/common/thread.{h,cpp}` (`SetCurrentThreadPriority` prerequisite)

### Intentional differences

- Rust reports a failed Linux/Android `setpriority` call through `std::io::Error`; Eden formats the
  same operating-system error through `GetLastErrorMsg`.
- Unsupported non-Unix/non-Windows targets retain a no-op fallback. Eden has a dedicated Haiku
  priority mapping which is not a supported Ruzu build target.

### Unintentional differences (to fix)

- None in the Linux, Windows, or generic POSIX priority mapping covered by this prerequisite.

### Missing items

- Android's topology-policy registration (`RememberCurrentThreadNice`) and the related
  performance/efficiency-core policy subsystem are not ported. They are outside the Linux worker
  priority prerequisite and depend on Eden's Android-only topology/ADPF infrastructure.
- Pre-existing thread-name, Event, Barrier, and topology-policy differences are outside this
  focused prerequisite review.

### Binary layout verification

- N/A: thread-priority selection has no serialized or guest-visible structure.

## 2026-08-25 — `src/video_core/src/renderer_vulkan/scheduler.rs` vs Eden `src/video_core/renderer_vulkan/vk_scheduler.{h,cpp}`

### Intentional differences

- Rust implements Eden's polymorphic in-place `TypedCommand` list as a typed header plus `FnOnce`
  payload in the same 0x8000-byte arena. Payload alignment, FIFO execution, destruction, arena
  reuse, overflow dispatch, and submission marking retain the upstream contracts.
- The worker queue uses `Arc`, mutexes and condition variables with an explicit in-flight count
  instead of C++ `jthread` stop tokens and an execution mutex. `wait_worker` still waits for both an
  empty queue and completion of the executing chunk; `Drop` drains work before requesting stop.
- Query-cache interactions use independently locked shared states rather than Eden's non-owning
  `QueryCache*`, preserving `CounterReset`, streaming-counter close, sample pause and conditional
  rendering order without creating aliased mutable Rust references.
- `StateTracker` is installed after fallible renderer construction and is therefore held as an
  optional non-owning pointer. Runtime construction installs it before scheduler recording begins.
- `request_renderpass_raw` is a Rust-only adapter for helper-owned Vulkan framebuffers which do not
  have an upstream texture-cache `Framebuffer` object. The ordinary `request_renderpass` path owns
  Eden's deferred-clear behavior.
- Rust exposes separate convenience methods for C++ default arguments (`flush`,
  `flush_with_signal`, `flush_with_semaphores`, `finish`, `finish_with_semaphores`). All forward to
  the same signal/wait semaphore ordering as Eden.
- Command buffers are explicitly reset before `vkBeginCommandBuffer`; Eden relies on Vulkan's
  implicit reset when beginning an executable command buffer from a resettable pool.
- `Scheduler::new` propagates `MasterSemaphore` construction failures as `vk::Result`; Eden
  propagates the equivalent Vulkan wrapper exception from its constructor.

### Unintentional differences (to fix)

- None in scheduler state ownership, render-pass state lifetime, deferred clears, pipeline-state
  transitions, worker submission, signal/wait semaphore forwarding, flush/finish ordering, or
  frame pacing after this correction.

### Missing items

- Eden's optional GPU-call logger hooks for render-pass begin/end and successful queue submission
  are absent because Ruzu does not yet port the `video_core/gpu_logging` subsystem.
- Android performance-core placement remains part of the unported topology/ADPF prerequisite
  recorded in the `common/thread.rs` entry above. It is a no-op in Eden on Linux, Windows and macOS.

### Binary layout verification

- N/A: scheduler state and command chunks are host-only and are never serialized or exposed to the
  guest. Focused tests verify the 0x8000-byte arena limit, command alignment/order/destruction,
  semaphore payloads, exact `Scheduler::State` defaults, and extended-dynamic-state transitions.

## 2026-08-25 — `src/video_core/src/renderer_vulkan/graphics_pipeline.rs` vs Eden `src/video_core/renderer_vulkan/vk_graphics_pipeline.{h,cpp}` (`UsesExtendedDynamicState` prerequisite)

### Intentional differences

- The recorded bind closure loads the eventual Vulkan pipeline handle from Rust's shared async
  build cell after its build wait. Eden captures `this` and reads its `vk::Pipeline` member at the
  same execution point.

### Unintentional differences (to fix)

- None in `UsesExtendedDynamicState` or the `ConfigureDraw` scheduler identity update: Rust now
  passes the stable `GraphicsPipeline` object even while its Vulkan handle is still being built.

### Missing items

- Broader `graphics_pipeline.rs` parity findings are handled by its dedicated
  `bugs/eden-parity/graphics_pipeline.md` review rather than this scheduler prerequisite.

### Binary layout verification

- N/A: no serialized pipeline-key or guest-visible structure changed in this prerequisite.

## 2026-08-25 — `src/video_core/src/renderer_vulkan/master_semaphore.rs` vs Eden `src/video_core/renderer_vulkan/vk_master_semaphore.{h,cpp}`

### Intentional differences

- Rust stores the fence-thread state in `Arc` and replaces `atomic::wait/notify_one` with a
  condition variable paired with the same free-fence mutex. The GPU tick is stored while holding
  that mutex, so the predicate check and notification cannot lose progress.
- Core Vulkan 1.3 and `VK_KHR_synchronization2` have distinct ash dispatch paths; Eden's device
  wrapper selects the corresponding core or extension function behind `Submit2`.
- Construction returns `Result<Self, vk::Result>` instead of throwing. Partial timeline semaphore
  and fence allocations are explicitly destroyed before returning an error, matching C++ RAII.
- Rust gives its two helper threads diagnostic names; Eden leaves these particular thread names to
  the operating system.

### Unintentional differences (to fix)

- None after restoring fixed-size synchronization2 submit arrays, fatal handling for non-timeout
  debug/fence wait failures, and destruction of a checked-out fence when queue submission fails.

### Missing items

- The focused unit test verifies file-owned constants. Timeline and fence submissions require a
  real Vulkan device and remain covered only by renderer integration/runtime validation.

### Binary layout verification

- N/A: `MasterSemaphore` owns host Vulkan synchronization objects and exposes no serialized or
  guest-visible raw-memory payload.

## 2026-08-25 — `src/video_core/src/renderer_vulkan/resource_pool.rs` vs Eden `src/video_core/renderer_vulkan/vk_resource_pool.{h,cpp}`

### Intentional differences

- `MasterSemaphore&` is retained as `Arc<MasterSemaphore>` so cloned Rust descriptor allocators can
  safely share the scheduler-owned timeline without a self-referential lifetime.
- `try_commit_resource` is the `Result` counterpart of Eden's exception-propagating
  `CommitResource`; its fallible grow path preserves Eden's resize-before-`Allocate` ordering.

### Unintentional differences (to fix)

- None after removing the external-tick variants and restoring Eden's failed-search sequence:
  query `KnownGpuTick`, call `Refresh`, query `KnownGpuTick` again, and only then grow the pool.
- None in search order, committed-tick assignment, overflow growth, or hint advancement.

### Missing items

- None.

### Binary layout verification

- N/A: the pool stores host-only resource indices and timeline ticks. A focused test verifies that
  the fallible Rust grow path publishes the resized tick range before invoking allocation, as
  Eden's `Grow` does.

## 2026-08-25 — `src/video_core/src/renderer_vulkan/descriptor_pool.rs` and descriptor-commit call sites vs Eden `src/video_core/renderer_vulkan/vk_descriptor_pool.{h,cpp}` (`ResourcePool` prerequisite)

### Intentional differences

- `DescriptorPool` retains an `Arc` to the scheduler's `MasterSemaphore` and passes it to each
  allocator. Eden passes the same stable semaphore reference through every `Allocator` overload.
- Vulkan allocation failures use `Result<_, vk::Result>` instead of C++ exceptions.

### Unintentional differences (to fix)

- None in descriptor resource retirement after this prerequisite: `DescriptorAllocator::commit`
  now delegates tick acquisition and refresh to `ResourcePool`, matching Eden instead of using a
  stale pair of ticks captured by individual draw-recording call sites.

### Missing items

- Broader descriptor-pool findings remain owned by its dedicated `bugs/eden-parity` report.

### Binary layout verification

- N/A: this prerequisite changes host ownership and resource-retirement timing only.

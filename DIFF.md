# Upstream parity notes

Keep actionable parity debt, intentional adaptations, significant corrections, and concrete
binary-layout contracts, with their upstream file references. Historical entries are scoped to
their audit date; they are not a certification of the current tree. Eden is the current reference;
older entries may refer to zuyu or dynarmic.

Omit empty audit categories, "nothing to fix" statements, generic unchanged-layout claims,
successful build/test totals, launch commands, binary paths, and temporary logs. Keep unresolved
test failures and validation limits. Do not append an entry for documentation-only cleanup.

## 2026-08-22 — `src/core/src/debugger/debugger_interface.rs` vs Eden `src/core/debugger/debugger_interface.h`

### Intentional differences

- Rust represents upstream `Kernel::KThread*` backend/frontend arguments as stable numeric thread
  identifiers. Kernel thread ownership remains in the process registries, avoiding non-owning raw
  pointers across the debugger connection thread.
- Rust traits replace the C++ virtual base classes. The eventual frontend/backend wiring passes the
  backend explicitly rather than constructing a self-referential Rust object.

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

## 2026-08-09 — `src/ruzu/src/game_list.rs` vs `src/yuzu/game_list.cpp` (`GameList::PopupContextMenu` and `AddGamePopup`)

### Intentional differences

- Upstream fully configures each `QAction`, including the checkable Favorite state, before
  `QMenu::exec` materializes and displays the menu. GTK resolves stateful `GMenu` rows through an
  action group, so ruzu installs that group and parents/styles the empty `GtkPopoverMenu` before
  assigning its menu model. This preserves upstream's single layout pass and avoids initially
  rendering Favorite as a stateless row before rebuilding it as a checkbox.

## 2026-08-09 — `src/ruzu/src/main_window.rs` vs `src/yuzu/main.{h,cpp}` (`GMainWindow::OnRestartGame`)

### Intentional differences

- Upstream calls `ShutdownGame()` and immediately continues to `BootGame()` after its Qt shutdown
  synchronization. The GTK frontend requests the same confirmed shutdown non-blockingly, retains a
  copy of `current_game_path`, and calls `boot_game` only after `LoadingEvent::StopComplete` has
  joined the emulation thread and released the native render target.
- A pending restart is discarded when teardown reports a failure or the application window is
  closing, preventing a shutdown callback from launching a new session behind an error or close.

## 2026-08-09 — `src/ruzu/src/configuration/qt_config.rs`, `configure_dialog.rs`, and `main.rs` vs `src/frontend_common/config.cpp` and `src/yuzu/configuration/qt_config.cpp`

### Intentional differences

- Rust keeps generic settings, Qt-compatible controls, and GTK UI values in separate writers over
  the same INI file. They execute in upstream order: generic `ReadValues`/`SaveValues` first, then
  frontend-owned controls and UI values.

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

## 2026-08-09 — `src/video_core/src/renderer_vulkan/turbo_mode.rs`, `renderer_vulkan/texture_cache.rs`, and `host_shaders/vulkan_turbo_mode.comp` vs `src/video_core/renderer_vulkan/vk_turbo_mode.{h,cpp}`, `vk_texture_cache.cpp`, and `host_shaders/vulkan_turbo_mode.comp`

### Intentional differences

- `TurboMode` keeps its dedicated device and allocator in a separately owned `TurboResources`
  bundle and exposes an `Arc` callback to `Scheduler`; upstream stores those two owners directly
  in `TurboMode` and captures the containing object from a `std::jthread`. The remaining workload
  resources are initialized by the worker, matching upstream; see the 2026-08-26 follow-up below.
- `TextureCacheRuntime` receives `cant_blit_msaa` during construction instead of retaining the full
  Vulkan `Device` wrapper. It uses the same predicate as upstream `Image::NeedsScaleHelper` and the
  same color or combined depth/stencil helper blits.

## 2026-08-09 — `src/video_core/src/host1x/codecs/vp8.rs`, `vp9.rs`, and `vp9_types.rs` vs `src/video_core/host1x/codecs/vp8.{h,cpp}`, `vp9.{h,cpp}`, and `src/video_core/host1x/codec_types.h`

### Intentional differences

- Decoder methods receive the current `NvdecRegisters` explicitly through the existing Rust
  `DecoderImpl` trait; upstream retains the register owner in the decoder base class.
- Rust `Vec<u8>` values replace upstream `ScratchBuffer` and `Stream` owners without changing the
  emitted VP8/VP9 byte order or frame buffering lifecycle.

## 2026-08-09 — `src/common/src/thread_worker.rs`, `src/video_core/src/rasterizer_interface.rs`, and renderer disk-cache loaders vs `src/common/thread_worker.h`, `src/video_core/rasterizer_interface.h`, and renderer shader caches

### Intentional differences

- Rust passes an `Arc<AtomicBool>` through `RasterizerInterface::load_disk_resources` instead of a
  copied `std::stop_token`. `StatefulThreadWorker::wait_for_requests_or_stop` polls that state while
  blocked because `std::sync::Condvar` has no stop-callback integration; observing cancellation
  permanently stops every worker and abandons queued work, matching upstream `request_stop()`
  semantics.
- The command-line frontend supplies a never-signaled cancellation owner because it has no loading
  dialog. The GTK frontend forwards the same stop state that owns its launch lifecycle.

## 2026-08-09 — `src/video_core/src/renderer_opengl/gl_state_tracker.rs` and `gl_rasterizer.rs` vs `src/video_core/renderer_opengl/gl_state_tracker.{h,cpp}` and `gl_rasterizer.cpp`

### Intentional differences

- `StateTracker` stores the active channel dirty flags as `NonNull<[bool; 256]>` and clears that
  borrowed pointer in `release_channel`; upstream stores a raw C++ pointer whose lifetime follows
  the channel owner implicitly.
- The scoped lock over the buffer and texture caches uses the existing retrying dual-lock helper
  because `parking_lot::ReentrantMutex` has no direct `std::scoped_lock` equivalent.

## 2026-08-09 — `src/video_core/src/texture_cache/texture_cache_base.rs` vs `src/video_core/texture_cache/texture_cache_base.h` and `control/channel_state_cache.inc`

### Intentional differences

- `channel_gpu_memory` is a Rust shared-owner mirror of upstream's live
  `channel_state->gpu_memory` reference. It is resynchronized after channel erasure so releasing an
  inactive channel preserves the active memory owner and releasing the active channel clears it.

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

## 2026-08-09 — `src/ruzu/src/{boot,main_window}.rs` vs Eden `src/yuzu/main_window.{h,cpp}`

### Intentional differences

- Eden starts play-time accounting directly in `OnStartGame`. Ruzu's boot thread emits a lossless
  `Started { program_id }` event so GTK performs the equivalent transition. Pause, resume, stop,
  restart, and guest-driven exit retain Eden's ordering.

## 2026-08-09 — `src/ruzu/src/configuration/configure_per_game_addons.rs` vs Eden `src/yuzu/configuration/configure_per_game_addons.{h,cpp,ui}`

### Intentional differences

- Eden reuses its persistent frontend `Core::System`. Ruzu rebuilds NAND, SDMC, and configured game
  directory providers while Configure Game is open, then queries the same `PatchManager` data.
- GTK uses a `gio::ListStore` rather than `QStandardItemModel`; patch name, version, enabled state,
  sorting, and disabled-addon persistence retain their upstream roles.

## 2026-08-09 — `src/common/src/settings.rs` vs Eden `src/common/settings.h`

### Intentional differences

- `ext_content_from_game_dirs` participates in ruzu's generic category visitor instead of Eden's
  C++ settings linkage, preserving the same default and persisted value.
- `gpu_fence_behavior` uses ruzu's generic switchable-setting visitor and GTK combo-row frontend
  instead of Eden's C++ linkage and Qt widget. The five enum values, persisted key, default, range,
  per-game switchability, and helper predicates match Eden.

## 2026-08-09 — `src/core/src/file_sys/registered_cache.rs` vs Eden `src/core/file_sys/registered_cache.{h,cpp}`

### Intentional differences

- `ExternalUpdateEntry::files` uses seven `Option<VirtualFile>` elements in place of nullable C++
  handles. The raw `ContentRecordType` index and seven-entry contract are unchanged.
- `open_container_as_nsp` probes NSP and then XCI directly, preserving Eden's final parser fallback
  without introducing a reverse dependency from `file_sys` to the loader dispatcher.

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

## 2026-08-09 — `src/video_core/src/renderer_vulkan/compute_pass.rs`, `descriptor_pool.rs`, and `update_descriptor.rs` vs Eden `src/video_core/renderer_vulkan/vk_compute_pass.{h,cpp}`, `vk_descriptor_pool.{h,cpp}`, and `vk_update_descriptor.{h,cpp}`

### Intentional differences

- Deferred `Send + 'static` scheduler closures retain a non-owning
  `DescriptorAllocatorReference`, the Rust counterpart of Eden's captured `this`. The allocator
  itself remains uniquely owned and move-only; its mutable resource-pool state is mutex-protected.
- Raw descriptor payload pointers are wrapped in a `Send` newtype. The queue owns one fixed
  allocation for the renderer lifetime, and its frame ring waits for the worker before recycling a
  slice, matching Eden's recorded `const DescriptorUpdateEntry*` lifetime.

## 2026-08-09 — `src/core/src/core.rs` and `src/core/src/hle/kernel/kernel.rs` vs Eden `src/core/hle/kernel/kernel.cpp`

### Intentional differences

- Ruzu still owns one shared `KMemoryBlockSlabManager` instead of Eden's separate application and
  system managers. Its runtime capacity is now the exact sum of Eden's 20000-entry application and
  10000-entry system heaps, so the adaptation no longer lowers the available resource limit.

### Missing items

- Separate application and system `KSystemResource` ownership remains to be ported before the two
  memory-block slab managers can be represented independently.

## 2026-08-09 — `src/core/src/hle/kernel/k_shared_memory.rs` vs Eden `src/core/hle/kernel/k_shared_memory.{h,cpp}`

### Unintentional differences (fixed)

- Allocation failure now returns `Kernel::ResultOutOfMemory` (`0xD001`) as Eden does; the previous
  raw `0xCE01` encoded `Kernel::ResultOutOfResource`.

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

### Missing items

- Cross-target dependency resolution was verified for Windows MSVC, macOS
  aarch64, and FreeBSD. Native linking and runtime execution still require CI
  or hardware for each target.

## 2026-08-18 — `src/ruzu/Info.plist` and `scripts/build-macos-app.sh` vs Eden `src/yuzu/Info.plist` and `src/yuzu/CMakeLists.txt`

### Intentional differences

- Eden uses CMake's `MACOSX_BUNDLE` target property; ruzu's Cargo workspace uses a dedicated
  packaging script after `cargo build --release --bin ruzu`. Both produce the same macOS bundle
  ownership and directory layout.
- Eden copies prebuilt `eden.icns` and `Assets.car` resources. Ruzu generates `ruzu.icns` from the
  frontend-owned rusty-lemon PNG because it does not have an Apple asset catalog.
- The local developer bundle receives an ad-hoc signature after MoltenVK is copied. Distribution
  identity signing and notarization remain release-pipeline responsibilities.

### Missing items

- Ruzu has no liquid-glass `Assets.car` equivalent to Eden's asset catalog.

## 2026-08-18 — `src/video_core/src/vulkan_common/vulkan_library.rs` vs Eden `src/video_core/vulkan_common/vulkan_library.cpp`

### Intentional differences

- Both implementations retain `LIBVULKAN_PATH` as the first explicit lookup and prefer the
  application bundle next. For an unbundled development `ruzu-cmd`, Rust additionally searches the
  sibling Eden build so performance and rendering comparisons use Eden's exact bundled MoltenVK.
- `scripts/build-macos-app.sh` likewise copies Eden's bundled MoltenVK when available, after an
  explicit `MOLTENVK_LIBRARY` override and before the Homebrew fallback.

### Missing items

- Distribution builds still need a release-owned MoltenVK artifact rather than relying on a sibling
  development checkout.

## 2026-08-18 — `src/ruzu_cmd/src/emu_window/emu_window_sdl3_vk.rs` vs Eden `src/yuzu_cmd/emu_window/emu_window_sdl3_vk.cpp`

### Intentional differences

- Ruzu stores the `CAMetalLayer` returned by `SDL_Metal_GetLayer` as the render surface and retains
  the `SDL_MetalView` separately for its lifetime. Eden stores the opaque Metal view directly while
  its Vulkan surface path consumes it as a `CAMetalLayer`; the Rust split keeps the consumed native
  object explicit without changing the Cocoa ownership boundary.

## 2026-08-18 — `src/video_core/src/vulkan_common/vulkan_device.rs` vs Eden `src/video_core/vulkan_common/vulkan_device.cpp`

### Unintentional differences (to fix)

- Eden explicitly disables `robustBufferAccess2` and `robustImageAccess2` while retaining
  `nullDescriptor`. Ruzu now applies the same feature mutation before passing the queried feature
  chain to `vkCreateDevice`; previously all robustness2 features advertised by MoltenVK remained
  enabled.

## 2026-08-18 — `src/video_core/src/renderer_vulkan/query_cache.rs` vs Eden `src/video_core/renderer_vulkan/vk_query_cache.cpp`

### Intentional differences

- Rust query reports share their measured slots and synchronized result through `Arc` rather than
  Eden's query IDs and `HostQueryBase::IsFinalValueSynced` flag. The report remains unavailable to
  the guest writeback callback until the matching async-flush set has been popped.

### Unintentional differences (to fix)

- `pending_flush_sets` is protected across the GPU and GPU-fencing threads, matching Eden's
  `flush_guard`. The initial Rust adaptation omitted this synchronization.

### Missing items

- The existing Rust lease-based bank owner
  remains an intentional structural adaptation documented in the 2026-08-09 query-cache entry.

## 2026-08-18 — `src/core/src/gpu_core.rs` and `src/video_core/src/gpu.rs` vs Eden `src/video_core/gpu.{h,cpp}`

### Intentional differences

- The cross-crate `GpuCoreInterface` exposes Eden's concrete `GPU` methods to `core`; its test
  doubles in `memory.rs`, `nvhost_as_gpu.rs`, and `nvhost_gpu.rs` implement `wait_for_composite`
  as a no-op because they have no GPU thread or renderer.
- Rust stores the pending composite fence in `AtomicU64` because the split interface is callable
  through shared references. Eden stores the same single pending fence as a plain `u64` under its
  HWC/GPU-thread lifecycle.

### Unintentional differences (to fix)

- `RequestComposite` now records the pending sync-operation fence and returns after
  `TickGPU`; it no longer waits synchronously. `WaitForComposite` consumes and waits that fence at
  the next HWC tick, including Eden's zero-fence and shutdown exits.

## 2026-08-18 — `src/core/src/hle/service/nvdrv/devices/nvdisp_disp0.rs` vs Eden `src/core/hle/service/nvdrv/devices/nvdisp_disp0.{h,cpp}`

### Intentional differences

- The Rust owner forwards through `GpuCoreInterface` because `core` cannot own the concrete
  `video_core::Gpu`; the call position and behavior match Eden's direct `system.GPU()` call.

## 2026-08-18 — `src/core/src/hle/service/nvnflinger/display.rs` and `hardware_composer.rs` vs Eden `src/core/hle/service/nvnflinger/display.h` and `hardware_composer.{h,cpp}`

### Intentional differences

- Rust uses `BTreeMap` and `Arc<Mutex<Layer>>` in place of Eden's `flat_map` and shared pointers;
  keys, layer ownership and mutation boundaries are unchanged.

### Unintentional differences (to fix)

- `ComposeLocked` now waits for the previous composite, releases eligible buffers before
  acquisition, interval-gates non-overlay acquisition, excludes overlays from game cadence,
  stable-sorts real z indices, composites only after a new acquisition, advances exactly one HWC
  frame, and returns one.
- Framebuffer release numbers are absolute (`frame_number + interval`), `last_acquire_frame` is
  tracked, and overlays release independently, matching Eden's lifecycle and ordering.

## 2026-08-18 — `src/core/src/hle/service/nvnflinger/surface_flinger.rs` vs Eden `src/core/hle/service/nvnflinger/surface_flinger.{h,cpp}`

### Intentional differences

- Rust returns `Option<Arc<Mutex<Layer>>>` from `find_layer` instead of a nullable shared pointer.

### Unintentional differences (to fix)

- `find_layer` is again a public SurfaceFlinger-owned operation, and the overlay setter
  updates the matching layer where Eden owns that mutation. Z-index writes remain owned by
  `Container`, which uses this lookup exactly as Eden does.

## 2026-08-18 — `src/core/src/hle/service/vi/container.rs`, `manager_display_service.rs`, and `system_display_service.rs` vs Eden `src/core/hle/service/vi/container.{h,cpp}`, `manager_display_service.{h,cpp}`, and `system_display_service.{h,cpp}`

### Intentional differences

- Rust returns `Result<T, ResultCode>` rather than writing C++ `Out<T>` parameters. The CMIF
  handlers retain Eden's wire ordering and signed-to-unsigned bit casts.

### Unintentional differences (to fix)

- SystemDisplayService now wires `GetLayerZ`, parses `SetLayerZ` as `layer_id: u64` followed by
  `z_value: u64`, preserves the low signed 32-bit z pattern, and forwards visibility instead of
  returning success without changing the layer.

## 2026-08-20 — `src/video_core/src/query_cache/bank_base.rs` vs Eden `src/video_core/query_cache/bank_base.h`

### Intentional differences

- `BankPool::ReserveBank` returns `Result` so a fallible Rust resource constructor can replace a
  C++ builder that propagates allocation failures through exceptions.
- The file was normalized from CRLF to LF while formatting the new implementation and tests.

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

### Missing items

- Existing parity debt outside this correction remains in the full Eden samples accumulation
  state machine (`amend_value`, `accumulation_value`, checkpoints and the complete
  `PresyncWrites`/`SyncWrites` lifecycle).
- A real Vulkan occlusion-query title run is still required; unit tests do not execute a device
  query pool.

## 2026-08-20 — `src/video_core/src/renderer_vulkan/scheduler.rs` vs Eden `src/video_core/renderer_vulkan/vk_scheduler.{h,cpp}`

### Intentional differences

- Rust stores shared handles to `SamplesQueryState`, `TfbCounterState` and `QueryRuntimeState`
  instead of Eden's non-owning `QueryCache*`. This avoids aliased `&mut` references while keeping
  `EndPendingOperations` and `EndRenderPass` call ordering identical.
- `clear_query_cache_state` releases those handles before the rasterizer's Vulkan resources are
  destroyed; Eden relies on C++ member lifetime and its raw pointer is not dereferenced afterward.

## 2026-08-20 — `src/video_core/src/renderer_vulkan/vk_rasterizer.rs` vs Eden `src/video_core/renderer_vulkan/vk_rasterizer.{h,cpp}`

### Intentional differences

- The Rust constructor installs safe query-state handles only after every fallible resource
  creation succeeds, rather than storing Eden's direct `QueryCache*`. This prevents failed
  construction from leaving a dangling scheduler registration.
- The destructor explicitly clears those handles after `finish` and before destroying the query
  cache's Vulkan resource owners.

## 2026-08-20 — `src/core/src/hle/service/am/service/library_applet_creator.rs` vs Eden `src/core/hle/service/am/service/library_applet_creator.{h,cpp}`

### Intentional differences

- Rust manually parses CMIF arguments and resolves the transfer-memory handle through the current
  process, replacing Eden's typed `InCopyHandle<KTransferMemory>` deserializer.
- Rust returns service objects through `ResponseBuilder` rather than C++ `Out<SharedPointer<T>>`.

## 2026-08-20 — `src/ruzu/src/applets/software_keyboard.rs` vs Eden `src/yuzu/applets/qt_software_keyboard.{h,cpp}`

### Intentional differences

- GTK widgets, CSS and a main-loop channel replace Qt Designer widgets, Qt queued signals and the
  dedicated `InputInterpreter` thread; the frontend remains owned by the GUI module.
- Inline hide destroys the GTK dialog and recreates it on the next show while retaining guest text
  state; Eden hides and reuses its Qt dialog. This avoids retaining a hidden modal GTK window.
- The GTK frontend uses a single-line `Entry` for every draw type and does not reproduce Eden's
  framebuffer-relative geometry, controller artwork or DPI-specific Qt layout.

### Unintentional differences (to fix)

- Normal submissions now retain the active dialog through
  `Failure`/`Confirm` text checks, and only `ExitKeyboard` tears it down.
- Controller callbacks no longer re-enter `active: RefCell` while it is borrowed, and the input
  edge which opened the keyboard is discarded instead of immediately activating X/Cancel.
- Inline appear parameters, guest text/cursor updates, `ChangedString`, `MovedCursor`, key-disable
  flags, optional number-pad symbols, Shift/Caps Lock transitions and wrapped grid navigation now
  follow Eden's corresponding paths.
### Missing items

- Eden's held-button autorepeat and rich multi-line `SwkbdTextDrawType::Box` presentation remain UI
  features of the excluded Qt frontend; they are not part of this GTK crash/lifecycle correction.

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

## 2026-08-20 — `src/core/src/hle/kernel/k_process.rs` vs Eden `src/core/hle/kernel/k_process.{h,cpp}` termination caller selection

### Intentional differences

- Rust represents Eden's `KThread* thread_to_not_terminate` as an `Option<u64>` thread id while
  preserving the same identity comparison in `terminate_children`.
- `exit_with_current_thread` performs Eden's final `GetCurrentThread(kernel).Exit(kernel)` after
  releasing the process guard because Rust cannot re-enter the thread lifecycle while borrowing
  `KProcess` through its owning cell.

## 2026-08-20 — `src/ruzu/src/overlay_dialog.rs` and `src/ruzu/src/main_window.rs` vs Eden `src/yuzu/util/overlay_dialog.{h,cpp,ui}` and `src/yuzu/main_window.{h,cpp}`

### Intentional differences

- GTK uses undecorated transient windows because they must remain above ruzu's native render child.
  The shutdown configuration keeps the visible 780-by-300 regular-text panel proportions; the
  interactive error configuration uses Eden's parent-sized dark backdrop and centered panel.
- Eden's `InputInterpreter` reads the aggregated NPad service state. Its Rust counterpart is not
  wired to that resource yet, so the interactive GTK overlay polls the same player/handheld
  emulated controllers every 50 ms and preserves the same rising-edge A/B and horizontal-input
  behavior.

### Missing items

- The two-action and rich-text overlay configurations remain outside this slice.

## 2026-08-20 — `src/ruzu/src/game_list.rs` and `src/ruzu/src/main_window.rs` vs Eden `src/yuzu/game/game_list.{h,cpp}` and `src/yuzu/main_window.{h,cpp}` shortcut dispatch

### Intentional differences

- A Rust callback replaces Eden's Qt `GameList::CreateShortcut` signal while retaining the same
  `(program_id, game_path, target)` payload and `GMainWindow` ownership of argument construction.
- GTK `gio::SimpleAction` objects replace the two `QAction` objects. Both remain hidden on macOS,
  matching Eden's compile-time guard.

## 2026-08-20 — `src/ruzu/src/util/game.rs` vs Eden `src/qt_common/util/game.{h,cpp}` shortcut creation

### Intentional differences

- GTK message dialogs replace `QtCommon::Frontend` dialogs, and GLib's XDG directory resolvers
  replace `QStandardPaths` on Linux.
- Linux icons and comments use the ruzu name (`ruzu-*.png`, `Ruzu Emulator`) instead of Eden's
  branding while preserving Eden's icon directory and title-id naming scheme.
- Windows creates the equivalent `.lnk` through the installed PowerShell `WScript.Shell` COM
  bridge and standard user-profile paths rather than directly owning `IShellLinkW`; this avoids a
  second Windows COM binding while preserving target, arguments, description and icon fields.

### Missing items

- `CreateHomeMenuShortcut` and the unrelated content-removal helpers from `qt_common/util/game.cpp`
  are outside this per-game shortcut slice.
- Eden's multi-resolution Windows ICO encoder is not yet ported; Windows currently stores the
  decoded icon as PNG before assigning it to the `.lnk`.

## 2026-08-20 — `src/ruzu/src/game_list.rs` vs Eden `src/yuzu/game/game_list.cpp` context-menu submenu presentation

### Intentional differences

- GTK `PopoverMenuFlags::NESTED` supplies the traditional child-popover behavior provided by
  Eden's `QMenu`; the toolkit-specific construction differs while retaining hover, click and
  keyboard access to each submenu.

## 2026-08-20 — `src/ruzu/src/overlay_dialog.rs` vs Eden `src/yuzu/util/overlay_dialog.cpp` and `src/yuzu/main_window.cpp` shutdown-dialog destruction

### Intentional differences

- GTK exposes window-manager closure and programmatic `Window::close` through the same
  `close-request` signal. Ruzu retains the signal id so it can remove the user-close guard before
  performing Eden's `OnEmulationStopped`-owned destruction.

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

### Missing items

- Eden's independent filesystem watchers for `Settings::values.external_content_dirs` are not
  present in Ruzu; configured game directories are refreshed explicitly by this button.
- `SetFirmwareVersion()` has no Ruzu status-label counterpart to update after refresh.

## 2026-08-20 — `src/ruzu/src/util/game.rs` and `src/ruzu/src/uisettings.rs` vs Eden `src/qt_common/util/game.{h,cpp}` and `src/yuzu/uisettings.h` metadata reset

### Intentional differences

- Rust reports recursive-removal errors through `std::io::Error` and GTK message dialogs; Eden
  uses `Common::FS::RemoveDirRecursively` and `QtCommon::Frontend` dialogs.
- The reload-pending flag is a module-level `AtomicBool` next to the frontend settings because
  Ruzu's cloneable `UISettings::Values` cannot directly contain an atomic member.

### Unintentional differences (to fix)

- `ResetMetadata` now removes the complete Ruzu `cache/game_list` directory, including the
  stale `<title-id>.pv.txt` Add-ons cache, and marks the game-list reload pending after success.

## 2026-08-20 — `src/ruzu/src/configuration/configure_filesystem.rs` vs Eden `src/yuzu/configuration/configure_filesystem.{h,cpp}` metadata-reset action

### Intentional differences

- The GTK button resolves its transient parent from the live widget root before calling the shared
  utility; Eden passes its `ConfigureFilesystem` widget through the global frontend dialog owner.

### Unintentional differences (to fix)

- The button now calls the shared metadata reset instead of logging an unavailable-action
  placeholder, and the main-window apply callback consumes the resulting reload-pending flag.

## 2026-08-20 — `src/hid_core/src/resources/ring_lifo.rs` vs Eden `src/hid_core/resources/ring_lifo.h`

### Intentional differences

- Rust uses the `LifoState` trait to express the C++ template requirement that every state expose
  `sampling_number`; this avoids an untyped raw-layout cast and does not change LIFO ownership.
- Rust bounds a corrupt `buffer_tail` to the backing array instead of reproducing C++ undefined
  behavior; the existing diagnostic remains available through `RUZU_TRACE_LIFO_CORRUPTION`.

## 2026-08-20 — `src/hid_core/src/resources/shared_memory_format.rs` vs Eden `src/hid_core/resources/shared_memory_format.h`

### Intentional differences

- The concrete shared-memory state types implement Rust's `LifoState` trait at their LIFO
  instantiation owner; Eden's C++ template accesses the same `sampling_number` members directly.

## 2026-08-20 — `src/hid_core/src/resources/six_axis/seven_six_axis.rs` vs Eden `src/hid_core/resources/six_axis/seven_six_axis.{h,cpp}`

### Intentional differences

- `SevenSixAxisState` converts its unsigned sampling number to `i64` for the common Rust
  `LifoState` interface; `as` preserves the underlying two's-complement bit pattern.

### Missing items

- The pre-existing incomplete `SevenSixAxis::on_update` integration remains outside this fix.

## 2026-08-20 — `src/hid_core/src/resources/npad/npad.rs` vs Eden `src/hid_core/resources/npad/npad.{h,cpp}` prefill regression

### Intentional differences

- Rust regression tests observe the shared-memory result directly after activation; Eden has no
  matching C++ unit test in the ported source tree.

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

### Missing items

- `PRET` flow analysis itself remains unimplemented, matching Eden. The pipeline cache now rejects
  that shader without terminating the GPU thread.

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

### Missing items

- Stable/nightly release tag formatting and auto-update endpoint constants are not used by Ruzu.

## 2026-08-20 — `src/ruzu/src/boot.rs` vs Eden `src/yuzu/main_window.cpp` `MainWindow::BootGame`

### Intentional differences

- The boot thread sends a typed `TitleChanged` event to GTK's main thread because GTK widgets may
  only be changed by their owning thread; Eden computes the same values on its Qt GUI thread.

## 2026-08-20 — `src/ruzu/src/main_window.rs` vs Eden `src/yuzu/main_window.{h,cpp}` `UpdateWindowTitle`

### Intentional differences

- Ruzu formats the default title directly instead of supporting Eden's optional
  `TITLE_BAR_FORMAT_IDLE` override, which has no Ruzu configuration owner.
- The same handler exists in each platform-specific GTK launch loop because those loops own their
  native render surfaces; all three consume the identical `TitleChanged` event.

### Missing items

- User-defined idle title-bar format overrides are not ported.

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

### Missing items

- Eden has no architecture-column behavior to port. Files whose executable metadata cannot be
  recovered display `Unknown`.

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

## 2026-08-21 — `src/shader_recompiler/src/runtime_info.rs` vs Eden `src/shader_recompiler/runtime_info.h`

### Intentional differences

- At the time of this review, Rust stored active transform-feedback entries in a `Vec`; this was
  restored to Eden's fixed 256-entry array by the 2026-08-26 transform-feedback parity pass below.

## 2026-08-21 — `src/video_core/src/transform_feedback.rs` vs Eden `src/video_core/transform_feedback.{h,cpp}`

### Intentional differences

- Invalid attribute indices are ignored safely in Rust; Eden indexes its fixed array directly.

## 2026-08-21 — `src/shader_recompiler/src/backend/spirv/spirv_emit_context.rs` vs Eden `src/shader_recompiler/backend/spirv/spirv_emit_context.{h,cpp}`

### Intentional differences

- SPIR-V construction uses `rspirv::dr::Builder` instead of Sirit.

## 2026-08-21 — renderer runtime-info propagation

Compared `src/video_core/src/renderer_vulkan/graphics_pipeline.rs` with Eden
`src/video_core/renderer_vulkan/vk_pipeline_cache.cpp`, and
`src/video_core/src/renderer_opengl/gl_shader_cache.rs` with Eden
`src/video_core/renderer_opengl/gl_shader_cache.cpp`.

### Intentional differences

- Rust maps the fixed pipeline key into owned `RuntimeInfo` values; Eden copies into fixed arrays.

## 2026-08-21 — `src/shader_recompiler/src/pipeline_cache.rs` runtime identity vs Eden runtime shader state

### Intentional differences

- Ruzu hashes runtime compiler inputs for its Rust pipeline cache; Eden keys the equivalent state
  through its fixed pipeline cache key.

## 2026-08-21 — `src/shader_recompiler/src/frontend/translate/load_store_attribute.rs` vs Eden `src/shader_recompiler/frontend/maxwell/translate/impl/load_store_attribute.cpp`

### Intentional differences

- Rust decodes instruction bit fields into integers and represents Eden's translation exceptions
  as panics.
- The Rust visitor stores the program header in an `Option`; generic `IPA` now requires it to be
  present, matching Eden's unconditional `env.SPH()` access.

## 2026-08-21 — `src/shader_recompiler/src/ir/value.rs` vs Eden `src/shader_recompiler/frontend/ir/attribute.h`

### Intentional differences

- The active Rust IR represents an attribute as a checked numeric newtype instead of a C++ enum;
  the numeric values and range predicates remain upstream-owned contracts.

### Unintentional differences (to fix)

- The crate still contains a second, enum-based `Attribute` in `ir/attribute.rs`. Consolidating
  those pre-existing parallel IR representations is a structural refactor outside this runtime
  correction; `IsLegacyAttribute` was added to the active translation type so current users share
  one predicate.

## 2026-08-21 — `src/shader_recompiler/src/frontend/translate_program.rs` vs Eden `src/shader_recompiler/frontend/maxwell/translate_program.cpp`

### Intentional differences

- Rust invokes the active attribute newtype's `is_legacy` method; Eden imports
  `IR::IsLegacyAttribute` from `attribute.h`.

## 2026-08-20 — `src/core/src/hle/service/filesystem/filesystem.rs` vs Eden `src/core/hle/service/filesystem/filesystem.{h,cpp}`

### Intentional differences

- Ruzu adds an optional frontend-owned `sdmc_open_override`. `OpenSDMC` returns it when installed,
  while every content-cache, modification-root, size, and normal launch path remains owned by the
  upstream-equivalent `SDMCFactory`.
- `set_sdmc_open_override` is a narrow Ruzu extension used only for standalone NRO launches. An
  overwriting `create_factories` call clears it together with the upstream factories so a view
  cannot leak into a later launch.

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

## 2026-08-20 — `src/ruzu/src/boot.rs` and `src/ruzu/src/main.rs` vs Eden `src/yuzu/main_window.cpp` `MainWindow::BootGame` and `src/yuzu/main.cpp`

### Intentional differences

- After the upstream-equivalent filesystem factories are created and before `System::Load`, Ruzu
  detects a standalone NRO and installs its per-launch homebrew SDMC view. Eden has no equivalent
  boot hook and relies on files already being present in its configured SDMC.
- The GTK entry point declares `homebrew_vfs` as a private frontend module; Eden's excluded Qt
  frontend has no corresponding source file.

## 2026-08-21 — workspace source layout vs Eden repository source layout

### Intentional differences

- Rust keeps each crate's conventional inner `src/` directory, so Eden's
  `src/video_core/foo.cpp` maps to Ruzu's `src/video_core/src/foo.rs`.
- Cargo manifests remain inside their crates, while the root `Cargo.toml` coordinates the
  workspace.
- The GTK frontend test for the quick-start action reaches the repository-level documentation
  through `../../../docs/quickstart.md`; Eden's excluded Qt frontend has different test ownership.

## 2026-08-21 — `src/ruzu/src/homebrew_vfs.rs` vs Eden `src/core/file_sys/vfs/vfs_layered.{h,cpp}` and `src/core/hle/service/filesystem/filesystem.cpp`

### Intentional differences

- Ruzu's frontend-owned writable SDMC view now treats an NRO directly inside a directory named
  `switch` as a conventional SD-card archive: the directory above `switch` becomes the writable
  upper layer. This exposes asset directories shipped beside `switch` without host links or a
  manual copy into the configured SDMC. Eden has no automatic host-package mount and continues to
  open only its configured `SDMCFactory` root.
- NROs in flat or per-application layouts retain the previous containing-directory root, and the
  configured SDMC remains the fallback layer in both cases.

## 2026-08-21 — `src/video_core/src/gpu.rs` and `src/video_core/src/gpu_thread.rs` vs Eden `src/video_core/gpu.{h,cpp}` and `src/video_core/gpu_thread.{h,cpp}`

### Intentional differences

- Ruzu exposes an idempotent `ThreadManager::shutdown` helper because Rust field destruction runs
  in declaration order. `Gpu::drop` invokes it explicitly to reproduce the relevant C++ reverse
  member destruction contract: `GPU::Impl::gpu_thread` is stopped and joined while `renderer` is
  still alive. Ruzu also stops the thread before freeing its boxed scheduler; Eden's scheduler is
  stored in-place and has a trivial destructor, so its storage remains within `GPU::Impl` while
  `gpu_thread` is destroyed.

## 2026-08-21 — `src/core/src/core.rs` vs Eden `src/core/core.{h,cpp}` (`System::Impl::ShutdownMainProcess`)

### Intentional differences

- Eden destroys `audio_core` before `gpu_core` and `CpuManager::Shutdown`. Ruzu retains
  `audio_core` until after `finalize_terminated_processes_after_cpu_shutdown`, because Rust kernel
  sessions can keep `IAudioRenderer` alive in the terminated-process table. Its finalizer waits
  for a signal from `AudioRenderSystemManager`; destroying `audio_core` at Eden's earlier point
  stops that worker first and deadlocks shutdown.

## 2026-08-21 — `src/common/src/settings.rs` vs Eden `src/common/settings.h` (`dd12266c`)

### Intentional differences

- Rust uses `cfg!(target_os = "windows")` for the setting's persistence flag instead of Eden's
  `_WIN32` preprocessor branch. The resulting platform behavior is identical.
- `enable_raw_input` was added to Ruzu's category visitor alongside the new setting. Its existing
  Rust declaration had incorrectly disabled persistence on every platform, while Eden persists it
  on Windows through the same settings linkage used by `disable_wgi_xinput`.

## 2026-08-21 — `src/input_common/src/drivers/sdl_driver.rs` vs Eden `src/input_common/drivers/sdl_driver.cpp` (`dd12266c`)

### Intentional differences

- Rust constructs temporary `CString` values before calling the SDL3 C API; Eden passes the SDL
  hint macros directly. Both set `SDL_JOYSTICK_RAWINPUT_CORRELATE_XINPUT` and `SDL_JOYSTICK_WGI`
  to `0` with `SDL_HINT_OVERRIDE`, only on Windows and only when the setting is enabled.

## 2026-08-21 — `src/ruzu/src/configuration/configure_input_advanced.rs` vs Eden `src/yuzu/configuration/configure_input_advanced.{cpp,ui}` (`dd12266c`)

### Intentional differences

- The excluded Qt frontend's `QCheckBox` is represented by Ruzu's GTK `CheckButton`; the label,
  tooltip, initial setting value, apply behavior, and Windows-only visibility match Eden.

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

### Missing items

- Pre-existing unimplemented ACC commands remain registered as
  stubs exactly where the Rust service framework represents Eden's null handlers.

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

### Missing items

- Commands represented by null handlers upstream remain named Rust
  stubs and deliberately return the service framework's unimplemented result.

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

## 2026-08-21 — `src/hid_core/src/resources/npad/{npad,npad_resource}.rs` vs Eden `src/hid_core/resources/npad/{npad,npad_resource}.cpp`

### Intentional differences

- `NPadResource::get_index_from_aruid` returns `Option<usize>` instead of Eden's sentinel
  `AruidIndexMax`. Invalid unregister requests now return before clearing state, preserving Eden's
  new guard exactly.
- `NPad::activate` returns success after logging an invalid ARUID because the following upstream
  null-data check also returns before the fallback index is consumed. `NPad::unregister` uses
  index zero only for the temporary controller cleanup, then calls the guarded resource owner,
  matching Eden's fallback and lifecycle ordering.

### Missing items

- Eden also adds a null `shared_memory_format` guard to
  `abstracted_pad/abstract_battery_handler.cpp`. Ruzu's pre-existing abstract battery handler does
  not yet own or dereference an applet resource at all, so that crash path is already absent and
  this commit requires no executable Rust change there. Full abstract-battery integration remains
  pre-existing parity work, not a shortcut added by this port.

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

- This entry supersedes the broader “none” claim in the earlier `a41a98028a` service entry:
  subsequent line-by-line review found and fixed missing NCM prerequisites, NAX parsing,
  metadata filtering, registered installation and game-card setup.

### Missing items

- `FileSystemController::GetExternalContentProvider`, BIS partition access, the standalone
  save-data controller, image-directory access and placeholder wrappers are now present in their
  upstream-owned controller file.

## 2026-08-21 — explicit service declarations vs Eden `a41a98028a` service owners

### Intentional differences

- Rust service-framework trait boilerplate remains implemented with the existing mechanical
  `impl_service_framework!` helper. It does not declare commands, own behavior or combine upstream
  files.

### Missing items

- Null Eden handlers remain explicit Rust
  unimplemented handlers with the same command IDs and labels.

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

### Missing items

- `Request` remains deliberately stubbed to a two-byte JSON object, exactly as in Eden commit
  `5c54abf353`; no network download is performed.

## 2026-08-21 — `src/core/src/hle/service/acc/{profile_manager.rs,acc.rs}` vs Eden `src/core/hle/service/acc/{profile_manager.cpp,acc.cpp}`

### Intentional differences

- Eden creates the automatic first user and the `BeginUserRegistration` user with the branded
  name `Eden`; Ruzu uses the direct product-name adaptation `ruzu`. Existing saved profiles are
  parsed without renaming, so a user-selected or migrated name is never overwritten.

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

### Missing items

- The installer has not yet been executed on a native Windows host; MSVC resource compilation,
  vcpkg runtime staging, NSIS generation, install, launch and uninstall still require that test.

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

## 2026-08-21 — `src/ruzu/src/user_data_migration.rs` vs Eden `src/yuzu/user_data_migration.{h,cpp}`

### Intentional differences

- Ruzu's migration policy remains the previously documented non-destructive, selective GTK flow.
  The first page now exposes `No migration` as a method instead of a separate `Start Fresh`
  response, clears and disables Firmware/Keys for that method, and has a single `Next` action.
- Completing `No migration` records the explicit one-time prompt marker and resumes the normal
  startup prerequisite chain, which presents Eden's missing-key question when appropriate.

### Missing items

- Per-game migration remains hidden as documented by the existing implementation.

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

### Missing items

- Eden installs a `QFileSystemWatcher` on external-content roots so later host filesystem changes
  trigger a metadata reset automatically. Ruzu currently detects those changes on the toolbar
  refresh or the next game-list rebuild; it has no directory watcher yet.

## 2026-08-21 — `src/ruzu/src/util/content.rs` and firmware menu vs Eden `src/qt_common/util/content.{h,cpp}` / `src/yuzu/main_window.cpp`

### Intentional differences

- GTK file selection is asynchronous. Once a source is returned, both paths converge on the same
  synchronous copy and firmware-only integrity verification routine, preserving Eden's ordering.
- Ruzu uses the Rust `zip` crate instead of `JlCompress`; `ZipFile::enclosed_name` additionally
  rejects entries that escape the fixed `ruzu/firmware` temporary root.
- The success message reports the number of verified NCA files. Eden reports the installed
  firmware display version, whose frontend lookup still depends on Ruzu's not-yet-faithful
  installed SystemVersion reader.

### Missing items

- Displaying the installed firmware version requires replacing Ruzu's hardcoded
  `get_firmware_version_impl` with Eden's SystemVersion archive lookup; that prerequisite is
  outside this frontend menu slice.

## 2026-08-21 — `UISettings::enable_gamemode` ownership vs Eden `src/qt_common/config/uisettings.h`

### Missing items

- Ruzu does not yet have Eden's `qt_common/gamemode.cpp` DBus activation owner; this pre-existing
  runtime integration gap is separate from the corrected setting ownership and UI placement.

## 2026-08-21 — `src/core/src/file_sys/fssystem/compression_configuration.rs` vs Eden `src/core/file_sys/fssystem/fssystem_compression_configuration.{h,cpp}`

### Intentional differences

- Ruzu calls the safe Rust `lz4_flex::decompress_into` API in place of Eden's
  `Common::Compression::DecompressDataLZ4`; both require the decompressed byte count to equal the
  requested destination size.

## 2026-08-21 — `src/core/src/hle/service/ns/language.rs` and `src/core/src/hle/service/set/settings_types.rs` vs Eden `src/core/hle/service/ns/language.{h,cpp}` and `src/core/hle/service/set/settings_types.h`

### Intentional differences

- Eden's partially initialized fixed-size Thai and Polish priority arrays zero-initialize their
  remaining enum slots. Rust arrays require every element explicitly, so Ruzu spells those zero
  values as trailing `ApplicationLanguage::AmericanEnglish` entries.

## 2026-08-21 — `src/common/src/logging/backend.rs` vs Eden `src/common/logging.{h,cpp}`

### Intentional differences

- Ruzu sends entries through a background Rust channel, while current Eden writes synchronously to
  each backend. This existing threading difference is outside this dead-code cleanup slice.
- Ruzu shares the active color-console flag with the logging thread through `Arc<AtomicBool>`;
  Eden stores the equivalent atomic flag directly in `ColorConsoleBackend`.

### Unintentional differences (to fix)

- The abandoned Rust `LoggerImpl`, duplicate
  `ColorConsoleBackend`, unused stacktrace hook, and redundant `LoggerState::color_console_enabled`
  were removed. The live file backend and `LoggerState` remain the only active owners.

### Missing items

- Eden's platform-specific Windows debugger and Android logcat backends remain platform-deferred.
- Eden's `log_flush_line`, `extended_logging`, and username-censoring behavior is not part of this
  cleanup and still requires a dedicated parity pass.

## 2026-08-21 — removal of `src/web_service/src/telemetry_json.rs` vs current Eden `src/web_service/`

### Unintentional differences (to fix)

- Ruzu's `telemetry_json.rs` was an incomplete port from an older source tree: current Eden
  has no `telemetry_json.{h,cpp}`, Ruzu had no production caller, and both HTTP submission methods
  were explicit stubs. The module and its public export were removed.

## 2026-08-21 — dead-code cleanup in `src/common/src/heap_tracker.rs` vs Eden `src/common/heap_tracker.{h,cpp}`

### Intentional differences

- The active Ruzu implementation currently uses two safe `BTreeMap` indexes where Eden owns two
  intrusive red-black trees over each `SeparateHeapMap`. This is an existing structural and
  performance divergence and remains explicit parity debt.

### Missing items

- A future parity slice must replace the active `BTreeMap` representation with the same dual-tree
  ownership model as Eden; retaining an unused partial tree beside it did not provide that parity.

## 2026-08-21 — `src/dedicated_room/src/main.rs` announcement credentials vs Eden `src/dedicated_room/yuzu_room.cpp`

### Intentional differences

- Ruzu retains the historical setting field names `yuzu_username` and `yuzu_token`; they are the
  existing Rust equivalents consumed by `AnnounceMultiplayerSession`.

## 2026-08-21 — current program ID in `src/common/src/settings.rs` / `src/core/src/core.rs` vs Eden `src/common/settings.{h,cpp}` / `src/core/core.cpp`

### Intentional differences

- Ruzu stores the process-global ID in `AtomicU64` because Rust global mutable state must be
  synchronized. Eden uses a plain file-local `u64`; relaxed atomic operations preserve the same
  value semantics without adding ordering to unrelated emulator state.

## 2026-08-21 — macro dumping in `src/video_core/src/macro.rs` vs Eden `src/video_core/macro.{h,cpp}`

### Intentional differences

- `dump_to_directory` isolates the mechanical file write so the filename and payload can be tested
  without mutating Ruzu's process-global dump path. It remains private in the upstream-owned macro
  module and does not change method ownership.
- Rust uses `bytemuck::cast_slice` for the same native `u32` byte representation that Eden writes
  through `reinterpret_cast<const char*>`.

## 2026-08-21 — `src/shader_recompiler/src/pipeline_cache.rs` vs Eden Maxwell decode/translate ownership

### Unintentional differences (to fix)

- The unused Ruzu-only `maxwell_opcode_is_unknown` wrapper was removed. Opcode decoding
  remains owned by the control-flow and translation modules that consume it, matching Eden's
  direct `Decode` use rather than making the unrelated pipeline cache an extra owner.

### Missing items

- Broader structured-control-flow parity remains a separate implementation slice.

## 2026-08-21 — `src/input_common/src/main_common.rs` vs Eden `src/input_common/main.{h,cpp}` mapping callback ownership

### Intentional differences

- Rust's callback captures the shared `Arc<Mutex<MappingFactory>>` rather than a raw `this`
  pointer. Consequently, the private `InputSubsystemImpl` methods receive that shared factory
  explicitly; their ownership and call chain still mirror Eden's `Impl` methods.

### Missing items

- `GCAdapter` and Android registration remain the already documented platform-specific gaps in
  this subsystem; they are not introduced by this callback correction.

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

- Callback keys now increment before insertion and therefore start at 1, matching Eden.
- Deleting an unknown callback now asserts instead of only logging, matching Eden's
  `ASSERT_MSG` contract.

### Missing items

- The downstream `ConsoleSixAxis` and `SevenSixAxis` resources do not yet consume this newly live
  console state in their update paths. That wiring belongs to those corresponding files and is a
  separate prerequisite-sensitive slice.

## 2026-08-21 — console six-axis ownership in `src/hid_core/src/resources/six_axis/console_six_axis.rs` / `resource_manager.rs` vs Eden counterparts

### Intentional differences

- Ruzu's `ControllerActivation` stores shared `Arc<Mutex<...>>` references in place of Eden's
  `ControllerBase` raw/reference members. `ConsoleSixAxis::new` receives the HID core and the
  resource manager supplies the applet resource during sampler initialization.
- The private `update_shared_memory` helper is a mechanical extraction of Eden's four assignments
  so their projection can be regression-tested without fabricating kernel-backed applet memory.

### Unintentional differences (to fix)

- The obsolete Ruzu-only `ConsoleMotionStatus` duplicate and the default status constructed by the
  resource manager were removed.
- Sampler initialization no longer assigns an applet resource to `SevenSixAxis`, matching Eden,
  which only assigns one to `ConsoleSixAxis`.

### Missing items

- `SevenSixAxis::on_update` still needs the `Core::System` timing/application-memory dependency
  owned by its Eden constructor. It remains a separate structural prerequisite and was not
  approximated in this slice.

## 2026-08-21 — `src/core/src/file_sys/fs_path_utility.rs` vs Eden `src/core/file_sys/fs_path_utility.h` bounded backslash replacement

### Intentional differences

- Rust uses a zero-initialized `Vec<u8>` plus a bounded slice copy for Eden's temporary allocation
  and `Strlcpy`; both reserve the caller-provided remaining buffer length and terminate the copied
  source within that bound.

### Unintentional differences (to fix)

- The Rust-only outer `relative_len` temporary was removed; `rlen` still advances `cur_pos` at the
  exact point where Eden consumes `relative_len`.

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

- Button toggle/lock transitions, configuration-mode suppression, modifier bit mapping, mouse
  projection, raw-value getters, notifications, and callback-key lifecycle now match upstream.

## 2026-08-21 — `src/common/src/random.rs` vs Eden `src/common/random.{h,cpp}`

### Intentional differences

- Rust represents `std::mt19937` with the local `Mt19937` type implementing the standard engine's
  state transition and tempering exactly.
- `fastrand` supplies the process-global host entropy in place of C++ `std::random_device`; both
  are cross-platform, nondeterministic host random sources and the upstream seed parameters remain
  intentionally ignored.

## 2026-08-21 — `src/core/src/hle/kernel/k_process.rs` vs Eden `src/core/hle/kernel/k_process.{h,cpp}` ASLR load offset

### Intentional differences

- Ruzu retains its `is_hbl` argument and assignment because this frontend state is currently owned
  by `KProcess`; it follows the upstream parameters without changing their order.
- The Rust `match` returns the selected address directly instead of declaring a zero-valued local
  and assigning it in every switch arm. Flag mutation and address selection remain in the upstream
  order.

### Missing items

- `load_from_metadata` still uses pool-size constants because its Rust signature does not yet carry
  Eden's `KernelCore` reference.
- Eden calls `InitializeInterfaces` before returning; Ruzu still creates the ARM interfaces later
  from `System::load`.

## 2026-08-21 — `src/core/src/loader/deconstructed_rom_directory.rs` vs Eden `src/core/loader/deconstructed_rom_directory.{h,cpp}` ASLR load offset

### Intentional differences

- The additional Ruzu `is_hbl` state is forwarded after Eden's five load parameters; it does not
  alter the upstream ASLR calculation.

### Missing items

- Eden's NCE patch collection, patch-section size, and direct-mapped fast-memory base are not yet
  integrated, so the corresponding argument remains zero on Ruzu's current backends.

## 2026-08-21 — `src/core/src/loader/kip.rs` vs Eden `src/core/loader/kip.{h,cpp}` ASLR load offset

### Intentional differences

- Rust keeps the loader's virtual file because the `AppLoader` trait has no C++-style base-class
  file member; loader ownership is otherwise unchanged.
- Ruzu's internal `is_hbl = false` argument follows Eden's load parameters.

## 2026-08-21 — `src/core/src/loader/nro.rs` vs Eden `src/core/loader/nro.{h,cpp}` ASLR load offset

### Intentional differences

- Ruzu's internal `is_hbl = false` argument follows Eden's load parameters.

### Missing items

- Eden's NCE patching, patch relocation, and direct-mapped fast-memory base remain unintegrated; the
  fast-memory argument therefore remains zero.

## 2026-08-21 — `src/common/src/intrusive_red_black_tree.rs` vs Eden `src/common/intrusive_red_black_tree.h` bidirectional iteration

### Intentional differences

- Pointer-based C++ iterator positions are represented by arena indices. Rust's immutable and
  mutable double-ended iterators therefore retain explicit front and back indices so mixed forward
  and reverse traversal cannot yield a node twice.
- `IntrusiveRedBlackTreeBaseNode` locates `self` in the arena before following its embedded node
  links; this linear lookup replaces the parent-pointer cast that Rust's arena representation
  cannot express safely.

## 2026-08-21 — `src/audio_core/src/sink/cubeb_sink.rs` vs Eden `src/audio_core/sink/cubeb_sink.{h,cpp}` stream metadata ownership

### Intentional differences

- Rust keeps the Cubeb backend object beside a shared `SinkStreamHandle`; this replaces Eden's
  `unique_ptr<CubebSinkStream>` ownership while keeping the stream metadata on `SinkStream`.

### Unintentional differences (to fix)

- The duplicate `name` and `stream_type` fields were removed
  from the Rust-only Cubeb wrapper; their canonical values remain on `SinkStream`, matching Eden's
  `CubebSinkStream` inheritance from `SinkStream`.

## 2026-08-21 — `src/core/src/hle/service/filesystem/filesystem.rs` vs Eden `src/core/hle/service/filesystem/filesystem.{h,cpp}` provider ownership

### Intentional differences

- Ruzu registers providers through its shared `ContentProviderUnion` rather than Eden's
  `Core::System::RegisterContentProvider`; both unions retain non-owning provider pointers.
- Rust `Box<T>` replaces each upstream `std::unique_ptr<T>` and provides the same stable heap
  address while `FileSystemController` itself is moved.

## 2026-08-21 — `src/ruzu/src/{main_window,gtk_compat}.rs` vs Eden `src/yuzu/main_window.{h,cpp}` stop confirmation lifecycle

### Intentional differences

- Eden's `ConfirmShutdownGame` uses a blocking `QMessageBox`, while GTK4 confirmation is
  asynchronous. Ruzu therefore retains a one-shot callback and explicit pending state until the
  user responds or the dialog is dismissed.
- Ruzu rejects overlapping Stop/Restart and window-close confirmations. This reproduces the
  exclusivity that Eden receives automatically from its blocking modal dialog.

### Unintentional differences (to fix)

- Dismissing or destroying a GTK question now completes
  it as a rejection, so `stop_confirmation_pending` and `close_confirmation_pending` cannot remain
  latched after the dialog disappears.

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

## 2026-08-21 — `src/audio_core/src/adsp/apps/opus/opus_decoder.rs` vs Eden `src/audio_core/adsp/apps/opus/opus_decoder.{h,cpp}`

### Intentional differences

- Focused Rust tests exercise the mailbox protocol and decoder lifecycle directly. Their success
  assertions now use the upstream Opus-domain constant `OPUS_OK`, rather than the numerically equal
  but unrelated HLE-service `ResultCode::SUCCESS`.

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

## 2026-08-21 — `src/common/src/time_zone.rs` vs Eden `src/common/time_zone.{h,cpp}`

### Intentional differences

- Rust uses a `LazyLock<HashMap<...>>` for Eden's immutable `std::map`; the key/value contents and
  lookup behavior are identical.
- Windows uses the thread-safe CRT functions `localtime_s` and `gmtime_s` to obtain owned `tm`
  values. Eden immediately copies the results of `std::localtime` and `std::gmtime`, so subsequent
  calculations see the same state without retaining their static buffers.
- Targets that are neither Unix nor Windows retain a GMT fallback because Eden does not define a
  separate platform implementation for them.

## 2026-08-21 — `src/common/src/tree.rs` vs Eden `src/common/tree.h`

### Intentional differences

- Rust stores links as indices into a caller-owned slice and uses `usize::MAX` as the null
  sentinel, instead of retaining raw `T*` links. Every upstream rotation, color repair, lookup,
  insertion, removal, and traversal helper remains owned by this file with the same ordering.
- `HasRBEntry` replaces Eden's `CheckRBEntry`, `IsRBEntry`, and `HasRBEntry` C++ concepts.
- Rust naming follows snake_case, and a returned index replaces each returned pointer.

## 2026-08-21 — removed `src/common/src/x64/cpu_wait.rs` vs Eden `src/common/thread.{h,cpp}`

### Unintentional differences (to fix)

- Ruzu's separate helper monitored the address of a temporary aligned zero rather than the
  `Event::is_set` state used by Eden. Consequently it could only expire by timer and could not be
  awakened by `Event::set`; retaining or moving it would not provide upstream behavior.

### Missing items

- Ruzu's `common/thread.rs` still uses the condition-variable `Event::wait_for` path on Windows and
  does not yet port Eden's x86-64 Windows `MONITORX`/`WAITPKG` branches. This is a separate,
  platform-specific implementation slice rather than a prerequisite for removing the unreachable
  helper.

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

## 2026-08-21 — `src/rdynarmic/src/backend/x64/a64_emit_x64.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/{emit_x64,a64_emit_x64}.{h,cpp}`

### Intentional differences

- Rust has no shared C++ `EmitX64` base object, so the A64 emitter directly owns its
  `ExceptionHandler`. It is declared before the owned code buffer and callback table so Rust's
  field drop order removes the registration first.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/a32_emit_x64.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/{emit_x64,a32_emit_x64}.{h,cpp}`

### Intentional differences

- Rust has no shared C++ `EmitX64` base object, so the A32 emitter directly owns its
  `ExceptionHandler`. It is declared before the owned code buffer and callback table so cleanup
  follows Eden's emitter-before-code lifetime.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/block_of_code.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/block_of_code.{h,cpp}`

### Intentional differences

- On Windows, Ruzu still places and registers SEH unwind metadata during `prelude_complete`; its
  Windows-only `Drop` remains a fallback for standalone code-buffer tests. Production cleanup is
  now first performed by the emitter-owned `ExceptionHandler`.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/block_of_code.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/block_of_code.{h,cpp}`

### Intentional differences

- Ruzu emits x64 through `rxbyak::CodeAssembler` and stores byte offsets into its owned code buffer;
  Eden derives `BlockOfCode` from C++ Xbyak and stores native code pointers.
- Rust uses `cfg(target_os = "windows")` for Eden's `_WIN32` callee-saved XMM6–XMM15 path.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_memory.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/{a64_emit_x64_memory.cpp,emit_x64_memory.cpp.inc}`

### Intentional differences

- Rust keeps the scalar callback emitters in this shared x64 module and represents the 128-bit
  callback return with an explicit stack buffer on Windows.
- Rust also passes 128-bit callback writes through an explicit pointer on Windows; Eden's C++ ABI
  passes its `Vector` aggregate indirectly there. System V continues to use two integer lanes.
- `rxbyak` memory-operand constructors replace C++ Xbyak's `ptr`/`xword` address frames.

### Missing items

- The fastmem/page-table 128-bit paths are owned by
  `backend/x64/a64_emit_x64_memory.rs`; this file intentionally remains the callback-only owner
  selected by the current dispatcher for `A64ReadMemory128`/`A64WriteMemory128`.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/a64_emit_x64_memory.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/a64_emit_x64_memory.cpp`

### Intentional differences

- Ruzu stores fallback entry offsets in Rust hash maps and calls explicit Rust trampolines; Eden
  stores native function pointers and devirtualizes C++ `UserCallbacks`.
- The Rust Windows read trampoline accepts an explicit output pointer after the fixed context and
  address arguments. This preserves the same stack-buffer transfer without relying on C++'s
  compiler-specific hidden-return ordering.

### Unintentional differences (to fix)

- Removed one unused register binding from Ruzu-only fastmem diagnostic emission; emitted code is
  unchanged.

### Missing items

- Ruzu's current dispatcher routes ordinary 128-bit accesses through callback-only
  `emit_memory.rs`; it does not yet select Eden's fastmem/page-table 128-bit read/write fallback
  path. This is pre-existing behavioral debt outside the ABI prerequisite fixed here.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_exclusive_memory.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/emit_x64_memory.cpp.inc`

### Intentional differences

- Ruzu owns architecture-specific exclusive emission in this Rust file, while Eden instantiates
  the shared C++ template include from its A64 emitter.
- Rust's Windows trampolines take explicit pointer payloads for 128-bit values instead of exposing
  the host compiler's aggregate ABI directly to generated code.

### Missing items

- No new missing item found in the callback-only exclusive slice; inline fastmem ownership was not
  re-audited as part of this prerequisite.

## 2026-08-21 — `src/rdynarmic/src/jit.rs` vs Eden `src/dynarmic/src/dynarmic/interface/A64/config.h` and x64 memory callback call sites

### Intentional differences

- Rust uses free `extern "C"` trampolines to recover `JitInner`; Eden invokes virtual
  `UserCallbacks` through `ArgCallback`/`Devirtualize`.
- On Windows, Rust gives the read and write trampolines explicit `Pair128` pointers. Eden obtains
  the equivalent indirect aggregate transfer from its C++ ABI and generated accessor stubs.

## 2026-08-21 — `src/rdynarmic/src/ir/opcode.rs` vs Eden `src/dynarmic/src/dynarmic/ir/opcodes.inc`

### Intentional differences

- Rust represents Eden's generated opcode table as an enum plus an explicit `OpcodeInfo` match.

## 2026-08-21 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `src/dynarmic/src/dynarmic/ir/ir_emitter.h`

### Intentional differences

- Rust's `ResultAndOverflow` stores the untyped `Value` enum instead of Eden's templated result
  type; opcode metadata enforces that every helper in this slice returns U32 plus U1.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_saturation.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/emit_x64_saturation.cpp`

### Intentional differences

- Rust passes the presence of Eden's `has_overflow_inst` template parameter explicitly and uses
  `Option<InstRef>` for the associated pseudo-operation.
- `rxbyak` register-width conversions replace C++ Xbyak's `changeBit` views.

## 2026-08-21 — `src/rdynarmic/src/backend/arm64/emit_arm64_saturation.rs` vs Eden `src/dynarmic/src/dynarmic/backend/arm64/emit_arm64_saturation.cpp`

### Intentional differences

- Ruzu's local ARM64 encoder has no EOR-immediate helper, so Eden's single
  `EOR Wscratch0, Wscratch0, 0x80000000` is emitted as a MOVZ/MOVK into `Wscratch1` followed by
  register EOR. The result and flags are identical.
- Eden's explicit `UNREACHABLE` specializations for generic scalar/vector saturation opcodes fall
  through Ruzu's common unsupported-opcode error if they survive required IR lowering; the four
  reachable scalar result-and-overflow operations remain owned by this matching file.

## 2026-08-21 — `src/rdynarmic/src/backend/arm64/inst.rs` vs Oaknut instructions used by Eden `emit_arm64_saturation.cpp`

### Intentional differences

- Ruzu encodes AArch64 instructions directly as `u32` words rather than calling Oaknut.

## 2026-08-21 — `src/rdynarmic/src/backend/{x64/emit.rs,arm64/emit_arm64.rs,arm64/mod.rs}` vs Eden backend saturation emitter registration

### Intentional differences

- Rust dispatches opcodes through explicit `match` arms and declares the ARM64 source module in
  `mod.rs`; Eden registers template specializations through its C++ emitter headers and build.

## 2026-08-21 — `src/rdynarmic/src/frontend/a32/translate/helpers.rs` vs Eden `src/dynarmic/src/dynarmic/frontend/A32/translate/impl/common.h`

### Intentional differences

- Rust returns the untyped internal `Value` enum where Eden's helper signatures distinguish U16
  and U32 at compile time; the emitted opcode metadata retains those types.

### Missing items

- Other
  pre-existing helpers in `common.h` were not re-audited or claimed by this prerequisite.

## 2026-08-21 — `src/rdynarmic/src/frontend/a32/translate/saturated.rs` vs Eden `src/dynarmic/src/dynarmic/frontend/A32/translate/impl/{saturated.cpp,a32_translate_impl.h}`

### Intentional differences

- Ruzu decodes fields from `DecodedArm::raw` inside each Rust method, while Eden's generated
  decoder passes typed immediates, booleans, and registers as method arguments.
- ARM condition state is emitted once at the Rust block-translation boundary; the method bodies
  therefore begin with Eden's pre-condition register validation and then emit the instruction
  body. Invalid PC operands still raise Unpredictable before any register read.

### Missing items

- Eden's `arm_QASX`, `arm_QSAX`, `arm_UQASX`, and `arm_UQSAX` remain absent because Ruzu's ARM
  decoder does not yet expose those instruction IDs. They are pre-existing parallel-instruction
  debt outside this scalar warning slice.

## 2026-08-21 — `src/rdynarmic/src/frontend/a32/translate/mod.rs` vs Eden ARM decoder/visitor dispatch for scalar saturation

### Intentional differences

- Rust uses an explicit `ArmInstId` match after block-level condition setup; Eden invokes visitor
  methods through generated decoder callbacks.

### Missing items

- The four parallel saturation IDs named in the `saturated.rs` audit remain absent from the Rust
  decoder and consequently from this dispatcher.

## 2026-08-21 — `src/rdynarmic/src/jit.rs` scalar saturation regression vs Eden `frontend/A32/translate/impl/saturated.cpp`

### Intentional differences

- The Rust-native regression executes a compact ARM instruction stream through each available
  host backend; Eden's C++ source defines the expected semantics but does not own this Rust test.

### Missing items

- This focused regression does not claim exhaustive immediate widths or every QDADD/QDSUB input;
  their IR ordering is covered by module tests.

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

## 2026-08-21 — `src/rdynarmic/src/backend/x64/{a32_emit_a32.rs,emit_a64.rs,emit_vector_multiply.rs}` warning-only cleanup vs Eden x64 emitter owners

### Intentional differences

- The Rust A32 emitter keeps the uniform `EmitContext` argument required by opcode dispatch but
  names it `_ctx`; Eden's `EmitA32ClearExclusive` likewise retains and leaves its
  `A32EmitContext&` parameter unnamed.
- Rust-native emitter regressions have no direct Eden test-file counterpart. Removing one unused
  synthetic `Inst` and three unnecessary `unsafe` call sites changes neither the emitted code nor
  the paired-min/max fallback calculations rechecked against Eden's `emit_x64_vector.cpp`.

## 2026-08-21 — `src/rdynarmic/src/frontend/a32/translate/thumb16.rs` PUSH/POP vs Eden `src/dynarmic/src/dynarmic/frontend/A32/translate/impl/{thumb16.cpp,a32_translate_impl.h}`

### Intentional differences

- Ruzu extracts `M`/`P` and the low register list from `DecodedThumb16`; Eden's generated decoder
  passes those fields as typed visitor arguments. Both construct the same 16-bit register mask.
- Rust uses `Reg::R13` for Eden's `Reg::SP` spelling and `Value::ImmU1` carry operands for the
  equivalent `ir.Add`/`ir.Sub` operations.

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

- The removed boolean assignment was
  overwritten by the complete `FlagInfo::set_not_required()` state before any read.

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

## 2026-08-21 — `src/core/src/file_sys/fssystem/hierarchical_sha3_storage.rs` vs Eden `src/core/file_sys/fssystem/{fssystem_hierarchical_sha3_storage.h,fssystem_hierarchical_sha3_storage.cpp}`

### Intentional differences

- Rust owns a copy of the caller-provided hash work buffer in a `Vec<u8>`; Eden retains and fills a
  caller-owned raw pointer. The buffer is not consulted after initialization in either
  implementation, while owned storage avoids an unsafe lifetime spanning the object.
- Rust represents the not-yet-initialized base storage with `Option` and returns zero from safe
  getters; Eden requires `Initialize` before `GetSize` or non-empty `Read`.
- The unused mutex was removed. Eden declares and constructs `m_mutex` but never locks it in either
  `Initialize` or `Read`, so the Rust field provided no synchronization or lifecycle behavior.

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

## 2026-08-21 — `src/common/src/fs/path_util.rs` `sanitize_path` vs Eden `src/common/fs/{path_util.h,path_util.cpp}` `SanitizePath`

### Intentional differences

- Rust builds the normalized result from borrowed UTF-8 components instead of Eden's mutable byte
  string and `string_view` vector. Separator selection, Windows network-prefix preservation,
  absolute-path handling, and component ordering remain the same for valid platform paths.

### Unintentional differences (to fix)

- Android content URIs are not bypassed before normalization. Android filesystem glue is an
  explicit project exception; this remains relevant only if that excluded frontend is introduced.

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

## 2026-08-21 — `src/core/src/loader/kip.rs` loader file ownership vs Eden `src/core/loader/{loader.h,kip.h,kip.cpp}`

### Intentional differences

- Eden inherits the protected `file` and `is_loaded` members from `AppLoader`; Rust's `AppLoader`
  is a trait, so `AppLoaderKip` owns both fields directly. The retained file is named `_file` to
  express base-class ownership while avoiding a false dead-field warning.

## 2026-08-21 — `src/core/src/loader/nax.rs` loader file ownership vs Eden `src/core/loader/{loader.h,nax.h,nax.cpp}`

### Intentional differences

- As for KIP, Rust's trait cannot own Eden `AppLoader::file`; `AppLoaderNax::_file` retains that
  base-class ownership directly while `Nax` separately retains its own backing-file reference.

## 2026-08-21 — `tools/capture_harness/{src/main.rs,example.toml}` vs no Eden source counterpart

### Intentional differences

- `capture_harness` is a Ruzu-specific developer tool and has no file under Eden's `src/` tree.
  Its embedded parser regression now uses the checked-in generic homebrew example timeline instead
  of referring to a missing, title-specific local fixture.

## 2026-08-21 — `externals/rxbyak/src/{encode.rs,platform/unix.rs}` vs Eden x64 Xbyak consumers under `src/common/x64` and `src/dynarmic`

### Intentional differences

- `rxbyak` is Ruzu's Rust replacement for Eden's external Xbyak dependency, so it has no direct
  file counterpart in Eden's `src/` tree. Platform allocation retains Eden/Dynarmic's writable-then-
  executable lifecycle and adds `MAP_JIT` only on macOS.
- The orphaned `emit_evex_leg` APX encoder was removed. No rxbyak instruction called it, no APX
  instruction was generated, and Eden's current Dynarmic consumers do not emit APX; retaining the
  unreachable partial capability only produced dead code.

### Missing items

- Full Intel APX instruction generation remains unsupported rather than being represented by one
  unreachable prefix helper.

## 2026-08-21 — `externals/rxbyak/tests/common/mod.rs` vs Eden test infrastructure

### Intentional differences

- Eden has no Rust integration-test crate counterpart. Each rxbyak integration-test binary imports
  the same shared operand tables and NASM helpers but deliberately uses only the subset relevant to
  its instruction family, so `dead_code` is allowed only inside that shared test module.

## 2026-08-21 — `core/src/hle/kernel/k_session.rs` vs Eden `core/hle/kernel/k_session.{h,cpp}`

### Intentional differences

- Rust stores the embedded client and server endpoints behind separate `Arc<Mutex<_>>` owners. `KSession::on_server_closed` does not lock the client endpoint solely to invoke `KClientSession::on_server_closed`, because that upstream method has an empty body; doing so introduced a Rust-only endpoint/session ABBA deadlock during concurrent close.

### Missing items

- The wider `KAutoObject` reference-count lifecycle remains represented by Rust registries and endpoint-close flags rather than Eden's intrusive kernel-object ownership.

## 2026-08-21 — `rdynarmic/backend/arm64/emit_arm64_cryptography.rs` vs Eden `dynarmic/backend/arm64/emit_arm64_cryptography.cpp`

### Intentional differences

- Rust writes verified AArch64 instruction words through `BlockOfCode`; Eden expresses the same `SHA256H`, `SHA256H2`, `SHA256SU0`, and `SHA256SU1` instructions through Oaknut.

### Missing items

- Eden's `SM4AccessSubstitutionBox` unreachable specialization remains outside this implementation slice.

## 2026-08-21 — `rdynarmic/backend/arm64/emit_arm64_cryptography.rs` vs Eden `dynarmic/backend/arm64/emit_arm64_cryptography.cpp` (CRC32 completion)

### Intentional differences

- Rust selects the 32-bit or 64-bit data register with a boolean passed to the owner-local `emit_crc` helper; Eden expresses the same distinction through the `EmitCRC<bitsize>` template parameter.
- Rust writes verified AArch64 instruction words through `BlockOfCode`; Eden invokes the corresponding Oaknut CRC32 instruction methods.

### Missing items

- Eden's `SM4AccessSubstitutionBox` unreachable specialization remains the only cryptography opcode specialization not represented by this Rust owner.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/jit_state.rs` vs `src/dynarmic/src/dynarmic/backend/x64/a32_jitstate.{h,cpp}`

### Intentional differences

- Rust uses `debug_assert_eq!` for Eden's `DEBUG_ASSERT` on the stored FPSCR NZCV mask.

## 2026-08-21 — `src/rdynarmic/src/frontend/a64/translate/{simd_scalar_three_same.rs,simd_scalar_two_register_misc.rs,visitor.rs}` vs `src/dynarmic/src/dynarmic/frontend/A64/translate/impl/{simd_scalar_three_same.cpp,simd_scalar_two_register_misc.cpp,impl.h}`

### Intentional differences

- Rust's decoded instruction object supplies Eden's `sz`, `Vm`, `Vn`, and `Vd` parameters to the
  matching snake-case methods; the comparison helper boundaries remain file-local like upstream.

### Missing items

- This audit covers only the two scalar FCMEQ methods discovered through warning analysis; the
  remaining A64 translator surface is not claimed complete here.

## 2026-08-21 — `src/rdynarmic/src/{bin/a32_diff.rs,ir/opt/a64_get_set_elimination.rs,jit.rs}` warning audit vs Eden developer/test infrastructure

### Intentional differences

- `a32_diff` is a Ruzu-specific differential tool with no Eden source counterpart; removing its
  write-only CPSR divergence flag preserves its diagnostics and resynchronization behavior.
- AArch64-only mock callback builders are compiled only on AArch64, matching the architecture guard
  already applied to their sole tests.

## 2026-08-24 — `src/rdynarmic/src/bin/{a32_diff.rs,compile_bench.rs}` vs Eden `src/dynarmic/src/dynarmic/interface/A32/config.h` and `tests/A32/testenv.h`

### Intentional differences

- Both executables are Ruzu developer tools without direct Eden executable counterparts. Their
  sparse differential address space and deterministic compilation workloads remain tool-local;
  callback ownership and configuration follow Eden's A32 interface.
- Rust owns callbacks in `A32UserConfig` instead of storing Eden's non-owning `UserCallbacks*`.

## 2026-08-24 — `src/rdynarmic/src/{tests_a32.rs,tests_a32_fuzz.rs}` callback/configuration ownership vs Eden `src/dynarmic/src/dynarmic/interface/A32/config.h` and `tests/A32/testenv.h`

### Intentional differences

- Rust keeps deterministic and differential cases in crate-local test modules rather than Eden's
  Catch2 translation units. `Box<dyn A32UserCallbacks>` owns each environment for the Rust JIT,
  replacing Eden's non-owning callback pointer.
- Sparse test memory uses Rust maps and mutexes while preserving Eden's A32 `u32` guest-address
  domain and little-endian byte assembly.

### Missing items

- Differential tests still
  require their separately built Eden oracle executable at runtime.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/{a32_core.rs,a64_core.rs}` vs Eden `src/dynarmic/src/dynarmic/backend/arm64/{a32_core.h,a64_core.h}` and architecture config headers

### Intentional differences

- Rust returns address-space emission errors from `run` and `step`; Eden's `GetOrEmit` path relies
  on assertions/allocation invariants and returns its entry point directly.
- The architecture-specific test callbacks are boxed by their Rust `UserConfig`; Eden stores a
  non-owning callback pointer.

## 2026-08-21 — `src/rdynarmic/src/frontend/a32/{decoder.rs,decoder_thumb32.rs,translate/thumb32.rs}` vs Eden `src/dynarmic/src/dynarmic/frontend/A32/{decoder,translate/impl}`

### Intentional differences

- Ruzu's handwritten decoder helpers replace Eden's generated instruction-pattern tables. Their
  internal signatures now carry only the bitfields they actually inspect; decoded instruction
  names and translation ownership remain unchanged.
- Regression names describe the observed instruction sequence generically instead of referring to
  a commercial title; opcodes, fixtures, and assertions are unchanged.

### Missing items

- The broader handwritten-decoder parity surface is outside this focused no-behavior-change audit.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_vector_arrangement.rs` vs `src/dynarmic/src/dynarmic/backend/x64/emit_x64_vector.cpp` (narrow/sign-extend/zero-extend slice)

### Intentional differences

- Rust releases scratch-register locks explicitly where Eden's register-allocation wrappers release
  them by scope; emitted instruction ordering is otherwise preserved.

### Missing items

- Other vector-arrangement emitters remain under the separate warning/parity audit.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_vector_arrangement.rs` vs `src/dynarmic/src/dynarmic/backend/x64/emit_x64.h` and `emit_x64_vector.cpp` (broadcast/deinterleave slice)

### Intentional differences

- Rust explicitly releases temporary register-allocation locks; Eden releases its scoped register
  wrappers on scope exit. The emitted instruction order is unchanged.
- The focused regression tests provide no-op callback objects because Rust's `EmitContext` owns a
  complete callback configuration even though these vector emitters only query host features.

### Unintentional differences (to fix)

- Removed the `RUZU_BCAST64_*` diagnostic machine-code injection. It had no Eden equivalent and
  could reserve or overwrite architectural host XMM registers outside the register allocator.

### Missing items

- Other vector-arrangement emitters remain under separate parity slices; none of the broadcast or
  deinterleave methods audited here are missing.

## 2026-08-21 — `externals/rxbyak/src/assembler.rs` AVX packed immediate shifts

### Intentional differences

- The Rust API suffixes immediate packed-shift overloads with `_imm`, consistent with the existing
  legacy SSE methods, because Rust does not support C++-style method overloading.

### Missing items

- Other AVX packed immediate-shift element widths are not required by the interrupted Eden emitter
  slice and were not part of this focused prerequisite.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_vector_basic.rs` vs `src/dynarmic/src/dynarmic/backend/x64/emit_x64.h` and `emit_x64_vector.cpp` (CLZ/popcount/reverse-bits slice)

### Intentional differences

- Rust explicitly releases temporary register-allocation locks; Eden releases its scoped register
  wrappers on scope exit. The emitted instruction ordering is otherwise preserved.
- Eden's single `emit_x64_vector.cpp` translation unit is split into responsibility-based Rust
  emitter modules; these methods remain together in `emit_vector_basic.rs` and retain their
  one-to-one upstream names and dispatch ownership.

### Unintentional differences (to fix)

- Removed the unused 32-bit CLZ and reverse-bits host fallbacks: Eden has no corresponding
  fallbacks because every supported x86-64 host executes their baseline SSE implementations.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_vector_misc.rs` vs `src/dynarmic/src/dynarmic/backend/x64/emit_x64_vector.cpp` (rounding-halving-add slice)

### Intentional differences

- Rust explicitly releases temporary register-allocation locks; Eden releases its scoped register
  wrappers on scope exit. The emitted instruction ordering is otherwise preserved.
- The public Rust dispatch methods retain their explicit signed/unsigned and element-width names;
  both families delegate to private element-size helpers matching Eden's two static helpers.

### Unintentional differences (to fix)

- Removed all six scalar RHADD fallbacks. Eden emits native SSE2 for every supported width, so the
  unused unsigned 8/16-bit fallbacks were dead code and the remaining fallbacks represented parity
  debt.

### Missing items

- Other
  `emit_vector_misc.rs` families remain outside this focused warning-driven audit.

## 2026-08-21 — `src/rdynarmic/src/ir/opt/polyfill.rs` and `backend/x64/emit_vector_multiply.rs` vs `src/dynarmic/src/dynarmic/ir/opt_passes.{h,cpp}` and `backend/x64/emit_x64_vector.cpp` (widening-multiply slice)

### Intentional differences

- Rust rebuilds its arena-backed instruction list while preserving the original SSA mapping;
  Eden inserts replacement instructions before the current linked-list node and redirects its
  uses. Both produce two sign/zero extensions followed by a multiply at twice the element width.
- Rust's `unreachable!()` is the direct assertion equivalent of Eden's `UNREACHABLE()` macro.

### Unintentional differences (to fix)

- Removed the six x64 callback/SSE implementations that had no Eden counterpart. The matching x64
  emitters now assert unreachable after legalization exactly like Eden.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_vector_multiply.rs` vs `src/dynarmic/src/dynarmic/backend/x64/emit_x64_vector.cpp` (paired-add slice)

### Intentional differences

- Rust explicitly releases temporary register-allocation locks; Eden releases its scoped register
  wrappers on scope exit. The emitted instruction ordering is otherwise preserved.
- Eden declares emitter ownership through its opcode-driven emitter declaration machinery; Rust's
  matching functions are dispatched explicitly from `backend/x64/emit.rs`.

### Unintentional differences (to fix)

- Removed the scalar callback implementations and the Ruzu-only
  `RUZU_FORCE_PAIRED_ADD8_FALLBACK` diagnostic branch. Eden emits these operations natively on
  every supported x86-64 host.

## 2026-08-21 — `externals/rxbyak/src/assembler.rs` vs Xbyak 7.35.2 `xbyak_mnemonic.h` (packed immediate qword shifts)

### Intentional differences

- Rust names immediate overloads `vpsllq_imm` and `vpsraq_imm` because Rust does not support the
  C++ API's overloads distinguished only by the final operand type.
- The existing Rust `vex_packed_shift_imm` helper corresponds to Xbyak's shared
  `opAVX_X_X_XM` encoding path and receives the instruction flags explicitly.

### Unintentional differences (to fix)

- The pre-existing word and dword immediate forms now also retain Xbyak's EVEX flags, and the
  common validator accepts equal-width ZMM operands instead of rejecting a supported form.

## 2026-08-21 — `src/rdynarmic/src/backend/x64/emit_vector_multiply.rs` vs `src/dynarmic/src/dynarmic/backend/x64/emit_x64.h` and `emit_x64_vector.cpp` (widening paired-add slice)

### Intentional differences

- Rust explicitly releases temporary register-allocation locks; Eden releases its scoped register
  wrappers on scope exit. The emitted instruction and value-definition ordering is preserved.
- Rust materializes Eden's `code.Const(xword, ...)` through the emitter-owned constant pool and
  passes the resulting XMM memory operand to `movdqa`.

### Unintentional differences (to fix)

- Removed all six scalar callbacks plus the two alternative `pmaddwd`/`pmaddubsw` implementations.
  They had no Eden counterpart and bypassed its emitter structure.

## 2026-08-21 — `src/frontend_common/src/config.rs` vs `src/frontend_common/config.h` and `config.cpp` (config-array ownership audit)

### Intentional differences

- Rust names the live array-stack element `ConfigArrayEntry` and exposes it because `BaseConfig`
  is shared across frontend crates; its three fields and stack ownership match Eden's private
  `Config::ConfigArray`.

## 2026-08-21 — `src/ruzu_cmd/src/sdl_config.rs` vs `src/yuzu_cmd/sdl_config.h` and `sdl_config.cpp` (configuration-path ownership audit)

### Intentional differences

- Rust composes `BaseConfig` instead of inheriting C++ `Config`; the base object remains the owner
  of the resolved configuration location and INI state.

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

## 2026-08-21 — `src/network/src/room_member.rs` vs `src/network/room_member.h` (default join-argument import audit)

### Intentional differences

- Rust has no default function arguments, so callers pass `NO_PREFERRED_IP` explicitly to `join`
  and `send_join_request`; the constant remains owned by `room.rs`, matching Eden's `room.h`.

## 2026-08-21 — `src/rdynarmic/src/backend/arm64/emit_arm64_cryptography.rs` vs Eden `src/dynarmic/src/dynarmic/backend/arm64/emit_arm64_cryptography.cpp` (AES operations)

### Intentional differences

- Rust emits the four AArch64 instruction words through the local `inst.rs` encoder rather than
  Oaknut. Register allocation, realization, and instruction ordering remain identical.
- The two single-round operations share a mechanical Rust helper, as do the two mix-column
  operations; each helper preserves the corresponding upstream method body and state ownership.

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

- Resolved by the 2026-08-26 allocator passes recorded below: all image and buffer factories now
  return VMA-backed owners, and GPU allocation tracking is wired to the ported logger.

- Eden reports `MemoryCommit`, image, and buffer allocations/deallocations through `GPULogger` when
  GPU memory tracking is active. Ruzu does not yet have the corresponding GPU logging subsystem.
- Ruzu's raw-handle `create_image`, `create_buffer`, and `create_mapped_buffer` compatibility paths
  still use dedicated Vulkan allocations. Eden returns owning VMA-backed `vk::Image`/`vk::Buffer`
  wrappers; `create_owned_buffer` already uses VMA but the remaining callers have not all migrated.

## 2026-08-21 — `rdynarmic/backend/arm64/emit_arm64_data_processing.rs` vs Eden `dynarmic/backend/arm64/emit_arm64_data_processing.cpp` (masked shifts)

### Intentional differences

- Rust casts the masked shift count to `u8` only after applying Eden's 32-bit or 64-bit mask, because the local instruction encoders accept the already-valid immediate as `u8`.

## 2026-08-21 — `src/rdynarmic/src/backend/arm64/emit_arm64_data_processing.rs` vs Eden `src/dynarmic/src/dynarmic/backend/arm64/emit_arm64_data_processing.cpp` (scalar integer min/max)

### Intentional differences

- Rust writes the `CMP` and `CSEL` instruction words through the local `inst.rs` encoder rather
  than Oaknut. Argument acquisition, W/X register allocation, realization, flag spilling, and
  instruction ordering are identical.

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
- Rust's `Packet<'a>` borrows the compressed byte span directly while the C shim creates and frees
  the temporary `AVPacket` around `avcodec_send_packet`. This preserves Eden's non-owning packet
  data lifetime without exposing FFmpeg structure layouts through Rust FFI.
- `AVFrame`, `AVCodecContext`, `AVBufferRef`, and FFmpeg enum constants remain owned by the native
  shim. Rust accessors copy pointer arrays/strides rather than returning C array pointers, while the
  backing `AVFrame` and all ownership/destruction remain identical.
- `build.rs` probes libva on Linux and FreeBSD to define Eden's optional `LIBVA_FOUND` path; other
  targets compile the same non-libva branch selected by Eden's build configuration.

## 2026-08-21 — `src/video_core/src/renderer_metal/{metal_device,metal_buffer,metal_format,metal_image,metal_image_view,metal_layer,metal_scheduler,metal_presenter,metal_shader,metal_pipeline_cache,metal_staging_buffer_pool,metal_sampler}.rs` vs Eden renderer ownership

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
- The raw `ash::Device` is retained by `Smaa` to destroy its raw Vulkan handles in `Drop`. Runtime
  draw/update/upload operations receive Eden's high-level `Device&` through `AntiAliasPass::draw`.

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

- Rust omits Eden's `last_index_count`, `current_buffer`, and `immediate_buffer_capacity` members.
  None is read or updated upstream: indirect draw state is held by the channel bindings, async
  staging ownership uses the queued optionals directly, and `ScratchBuffer::resize_destructive`
  manages the immediate allocation capacity internally.

## 2026-08-21 — `src/video_core/src/host1x/gpu_device_memory_manager.rs` vs `src/core/device_memory_manager.h` and `.inc` (`UpdatePagesCachedBatch`)

### Intentional differences

- Ruzu exposes the batch operation through its `DeviceTracker` trait so the generic Rust word
  manager can call the concrete `MaxwellDeviceMemoryManager`; Eden's C++ template parameter calls
  that concrete method directly.

### Unintentional differences (to fix)

- Resolved on 2026-08-26: the public single-range path no longer returns early when `size == 0`;
  it now constructs Eden's zero-length range guard and executes the acquire/read setup without a
  page-counter update or caching callback.

## 2026-08-21 — `src/video_core/src/buffer_cache/word_manager.rs` vs `src/video_core/buffer_cache/word_manager.h`

### Intentional differences

- Ruzu omits Eden's now-unused `NotifyRasterizer` helper after all three mutation paths moved to
  `CollectChangedRanges` and `ApplyCollectedRanges`; keeping the superseded single-range helper
  would only retain dead code.
- The Rust callback adapter uses `Option<bool>` to represent Eden's compile-time distinction
  between callbacks returning `bool` and callbacks returning `void`.
- Eden has no native Metal renderer. These files are a macOS platform extension under the same
  conceptual ownership as Eden's `RendererBase`, renderer-specific scheduler and presentation
  services; they do not replace or modify the read-only Eden tree.
- `MetalScheduler` records directly into serial `MTLCommandBuffer` objects. It preserves Eden's
  guest operation order but does not reproduce Vulkan command pools, image layouts, render-pass
  objects or descriptor sets because those concepts are not native Metal synchronization or
  binding primitives.
- The frontend already owns a `CAMetalLayer` for MoltenVK. `MetalLayer` retains that exact object
  and confines renderer access to the GPU thread instead of creating a second view hierarchy.
- Shader-recompiler SPIR-V is retained as backend-neutral compiler output and translated to MSL
  with explicit independent Metal buffer/texture/sampler indices. No Vulkan runtime object is
  involved in this translation.
- Apple Silicon buffer allocations use shared unified memory with Metal-managed hazard tracking;
  images use private textures. Guest formats are mapped directly to `MTLPixelFormat`, and formats
  without a byte-compatible representation carry an explicit conversion requirement rather than
  falling back silently.
- `MetalStagingBufferPool` preserves Eden's 128 MiB upload-stream ownership, 256-byte alignment,
  16 timeline-stamped regions, reusable dedicated upload/download allocations, three size-class
  caches, round-robin `TickFrame` reclamation and explicit deferred-release lifecycle. The
  scheduler is passed per operation instead of stored by reference because a Rust renderer can
  move after construction. `MetalScheduler` batches operations into one active native command
  buffer and assigns monotonic completion ticks in submission order; synchronous submissions
  first commit the active batch.
- `MetalImageView` preserves the per-image-view variants created by Eden's Vulkan backend (1D and
  array, 2D/rect and array, 3D, cube and cube array), subresource ranges and component swizzles.
  The implementation uses native Metal texture views and declares `PixelFormatView` on the owner
  image instead of translating Vulkan image-view structures.
- `MetalSampler` preserves Eden's normal, default-anisotropy, forced-nearest and non-comparison
  sampler ownership and creation conditions. Metal's three native border colors preserve Eden's
  fallback selection; the exact Maxwell color and mirror-once-border requirement remain attached
  to the sampler for mandatory shader emulation because Metal has no custom-border-color API.
- Scheduler polling treats both `Completed` and `Error` as terminal. This preserves Eden's device
  error propagation while preventing a failed Metal command buffer from permanently blocking the
  ordered completion queue.
- `MetalDeviceProfile` queries the actual `MTLDevice` family, argument-buffer/read-write tiers,
  compressed-format support, sample counts, memory/threadgroup limits and shader/pipeline
  capabilities. Backend policy never infers features from Apple marketing generation names.
- Direct resource-binding limits which Metal does not expose through individual selectors are
  derived from Apple's published GPU-family tables: Apple7 (M1) through Apple10 (M5) expose 31
  buffers, 128 textures, 16 samplers, 31 vertex attributes and 8 color render targets per stage.
  Tier-2 argument-buffer sampler capacity remains family-specific (996 on Apple7/8 and 500,000 on
  Apple9/10). A focused family-policy test verifies the M1/M5 distinction without matching device
  names; sample counts remain queried individually with `supportsTextureSampleCount` rather than
  inferred from those tables.
- Capability policy is evaluated from the captured profile: argument binding selects direct,
  Tier 1 or Tier 2 resources; MSAA selects only a reported sample count; storage images require a
  reported read/write tier; cache budgeting derives from `recommendedMaxWorkingSetSize`. Calls
  introduced after macOS 11 are guarded by runtime OS availability, so a newer SDK does not send
  an unavailable selector to an older Apple Silicon system.
- `MetalScheduler` is the sole owner of active blit, compute and render encoders. It preserves
  Eden's `RequestOutsideRenderPassOperationContext`/`EndRenderPass` ordering while expressing the
  native Metal invariant that only one encoder may be active on a command buffer. Consecutive
  transfers share one blit encoder; changing operation class or flushing ends it explicitly.
- `MetalImage::upload_memory` and `download_memory` consume the common `BufferImageCopy` model,
  preserving Eden's subresource/layer iteration and operation ordering. Native Metal row/image
  pitches are calculated in format blocks, ranges are validated before encoding, and transfers
  are recorded through the scheduler-owned blit encoder. A native GPU round-trip test verifies
  shared staging to private texture and back.
- `MetalTextureCacheRuntime::copy_image` owns native image-to-image copies, matching Eden method
  ownership. It validates formats, samples, mip bounds, 3D depth and array layers before encoding;
  a GPU test verifies upload, image copy and download ordering in one scheduler batch.
- `MetalFramebuffer` retains the exact `MetalImageView` objects selected by `RenderTargets` and
  materializes a `MTLRenderPassDescriptor` with persistent LOAD/STORE attachments. Sparse MRT
  slots remain sparse in Metal while `num_color_buffers` counts actual attachments as Eden does.
- Metal multisample textures require one mip and some Apple GPUs expose only 1x/2x/4x. The owner
  records the guest sample count but allocates the highest supported count not exceeding it; this
  is the native fallback replacing Vulkan's guaranteed sample-count conversion.
- A combined `Depth32Float_Stencil8` texture is itself the Metal depth sampling view; Apple Metal
  rejects a cast to `Depth32Float`. Stencil sampling uses the legal `X32_Stencil8` view. This is the
  native equivalent of Eden's separate aspect views rather than a literal format cast.
- `MetalPipelineCache` owns `Shader::Profile`, `HostTranslateInfo`, render-pipeline keys and
  render/compute pipeline maps, matching Eden `PipelineCache` ownership. `metal_shader.rs` owns the
  backend module build corresponding to `vk_shader_util.cpp`: shader-recompiler SPIR-V is translated
  by SPIRV-Cross, then compiled to retained native `MTLLibrary`/`MTLFunction` objects.
- The initial compiler ABI deliberately targets MSL 2.3, direct resource bindings and a fixed
  32-lane subgroup. Device features which require a newer MSL language or an argument-buffer runtime
  remain disabled even when newer silicon reports them; profile flags describe the complete usable
  compiler/runtime path, not silicon in isolation.
- `MetalRenderPipelineKey` retains all eight color formats and blend states, depth/stencil formats,
  sample count, topology, alpha-to-coverage/one and rasterization state. Metal compiles these into
  `MTLRenderPipelineState`; compute shaders compile into `MTLComputePipelineState`. Native tests
  verify both states are created from shader-recompiler programs and reused by their cache keys.
- `MetalShaderBindingLayout` reflects Eden's SPIR-V descriptor order but compacts it into Metal's
  independent buffer, texture and sampler namespaces. A grouped CBUF/SSBO descriptor may advance
  Eden's Vulkan binding by `count` while declaring one scalar SPIR-V resource; Metal therefore
  allocates one argument for that reflected resource. True SPIR-V descriptor arrays consume their
  literal element count. Push constants reserve `buffer(0)` before ordinary buffers, and the
  retained module exposes the exact layout that the draw/dispatch encoder must bind.
- Runtime-sized and specialization-constant descriptor arrays, aliased resource classes and MSL
  auxiliary buffers fail compilation explicitly. Direct bindings are validated against the
  selected Apple-family buffer/texture/sampler limits instead of allowing SPIRV-Cross to emit an
  out-of-range Metal argument.

### Resolved differences

- Eden's `size_bytes` is a template parameter and its five tracking channels occupy one fixed
  `std::array`. The 2026-08-26 parity pass replaced Ruzu's runtime-sized stack-or-heap storage with
  the compile-time-sized inline storage recorded in the later entry below.
- The 2026-08-26 parity pass also stopped silently discarding notifications from a default manager
  with a null tracker; an invalid notifying use now fails explicitly.

## 2026-08-22 — `src/video_core/src/vulkan_common/vulkan_debug_callback.rs` vs `src/video_core/vulkan_common/vulkan_debug_callback.h` and `.cpp`

### Intentional differences

- Rust's `DebugUtilsMessenger` owns both the Vulkan handle and the `ash` extension loader needed
  to destroy it. This is the RAII counterpart of Eden's `vk::DebugUtilsMessenger`, whose instance
  dispatch table is retained by the wrapper internally.

### Missing items

- Resolved by the 2026-08-26 GPU-logging parity pass recorded below: validation messages are now
  forwarded when Vulkan-call logging is active.

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

## 2026-08-22 — `src/ruzu/src/boot.rs` vs Eden `src/yuzu/main_window.cpp`

### Intentional differences

- Before constructing video subsystems, the GTK frontend rejects an OpenGL renderer selected on
  macOS AArch64 with a dedicated multiline diagnostic explaining that Apple Silicon supports only
  the Vulkan renderer in ruzu. Eden uses one generic `ErrorVideoCore` dialog because its supported
  renderer set is not restricted by this Rust frontend's Apple platform port.
- The check consumes the selected global/per-game renderer value after boot configuration is
  applied. Vulkan, Null, macOS x86_64, and non-macOS hosts retain the existing load path and generic
  video-core error handling.

## 2026-08-22 — `src/video_core/src/engines/fermi_2d.rs` vs `src/video_core/engines/fermi_2d.{h,cpp}`

### Intentional differences

- Eden indexes the `Regs::reg_array` union member directly. Rust derives the same word pointer
  from the `#[repr(C)] RegsUnionRaw` address, avoiding an unsafe union-field projection required
  by Rust 1.89 while preserving the exact offset, size, and contiguous storage.

## 2026-08-26 — Fermi2D raw-register and blit parity

Rust files:
- `src/video_core/src/engines/fermi_2d.rs`
- `src/video_core/src/engines/sw_blitter/blitter.rs`
- `src/video_core/src/texture_cache/image_info.rs`
- `src/video_core/src/renderer_vulkan/blit_image.rs`
- `src/video_core/src/renderer_vulkan/texture_cache.rs`

Eden files:
- `src/video_core/engines/fermi_2d.{h,cpp}`
- `src/video_core/engines/sw_blitter/blitter.cpp`
- `src/video_core/texture_cache/image_info.cpp`
- `src/video_core/renderer_vulkan/blit_image.{h,cpp}`
- `src/video_core/renderer_vulkan/vk_texture_cache.cpp`

### Intentional differences

- `Surface::format` and `Surface::linear` use raw `u32` storage, and `Operation` is a transparent
  `u32` newtype. C++ enums can retain arbitrary register bit patterns; constructing an invalid Rust
  enum discriminant would be undefined behavior. The raw Rust representations preserve every bit,
  keep the same four-byte ABI, and expose the upstream named values as constants.
- Rust checks the length of the slice passed to `call_multi_method`; Eden receives an unchecked
  pointer whose caller contract guarantees `amount` readable words.

## 2026-08-22 — `src/video_core/build.rs` vs Eden root `CMakeLists.txt`

### Intentional differences

- Eden selects C++20 globally. The Rust build script selects C++17 only for the BCN shim and its
  bundled decoder sources, which require a modern C++ mode but not Eden's complete C++20 build
  environment; this also prevents Apple Clang from falling back to C++98.

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

## 2026-08-22 — `src/core/src/hle/service/psc/time/time_zone.rs` vs Eden `src/core/hle/service/psc/time/time_zone.{h,cpp}`

### Intentional differences

- Eden's recursive member mutex is paired with borrowed references. Rust combines the existing
  member mutex with the enclosing `TimeManager` mutex used by shared service owners.
- A zero-capacity Rust output slice returns zero results before writing. Eden writes the first
  element before checking `out_times_max_count`; valid CMIF requests provide output storage, while
  the Rust guard prevents malformed IPC from causing an out-of-bounds access.

## 2026-08-22 — `src/core/src/hle/service/psc/time/time_zone_service.rs` and `static.rs` vs Eden PSC time services

### Intentional differences

- `Arc<Mutex<TimeManager>>` retains the single owner behind Eden's
  `StandardSteadyClockCore&` and `TimeZone&` references. Isolated constructors create a private
  manager for unit-level service use; production `StaticService` forwards its shared manager.
- Eden asserts that an `InLargeData` descriptor exists. Ruzu treats a missing descriptor as an
  empty buffer, retaining the same value-initialized rule without aborting the service process.

## 2026-08-22 — `src/core/src/hle/service/glue/time/time_zone.rs` vs Eden `src/core/hle/service/glue/time/time_zone.{h,cpp}`

### Intentional differences

- Eden's borrowed worker/binary references and shared service pointers use the corresponding
  `Arc<Mutex<_>>` or `Arc<_>` owners. Its one intrusive-list member operation event is represented
  by one stable optional `OperationEvent`; repeated handle requests reuse that event.
- Event materialization is deferred until an IPC context can create the kernel bridge. Eden owns
  its kernel event at service construction and recreates it on the first handle request.
- Eden asserts that an `InLargeData` descriptor exists. Ruzu treats a missing descriptor as an
  empty buffer, retaining the same value-initialized rule without aborting the service process.

## 2026-08-22 — `src/core/src/hle/service/set/system_settings_server.rs` timezone forwarding vs Eden settings server

### Intentional differences

- Direct Rust forwarding methods return typed values or unit because the corresponding inner
  settings methods cannot fail; Eden expresses the same always-successful operations as `Result`.

### Unintentional differences (to fix)

- The broader pre-existing partial service differences remain recorded in the earlier
  `system_settings_server.rs` audit entry.

### Missing items

- No additional settings prerequisite is missing for timezone persistence.

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

- Command 7992 now constructs and returns that exact child interface as Eden does.

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

## 2026-08-22 — `src/core/src/hle/service/nvdrv/nvdrv_interface.rs` vs Eden `src/core/hle/service/nvdrv/nvdrv_interface.{h,cpp}`

### Intentional differences

- Eden's `Common::ScratchBuffer<u8>` owners map to reusable `Vec<u8>` fields. Ruzu clears the
  requested range before every dispatch instead of leaving reused bytes unspecified, preserving
  deterministic reserved/output bytes as required by the Rust raw-payload contract.
- The static `ServiceFramework` adapters and mutex-protected `NvdrvInterface` state split one C++
  `NVDRV` object into two Rust layers; the buffers remain owned by that per-service state.
- Optional ioctl tracing/history observes the service-owned buffers after dispatch without changing
  the guest-visible write condition or response.

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

## 2026-08-22 — `nvnflinger/window.rs` and `sockets/sockets.rs` vs Eden `window.h` and `sockets.h`

### Intentional differences

- Eden uses `enum class` plus `DECLARE_ENUM_FLAG_OPERATORS`; Ruzu expresses the same flag types
  with `bitflags!`, so their rustdoc belongs inside the macro invocation.

### Unintentional differences (to fix)

- The two type comments were attached to macro invocations rather than generated types and produced
  no Rust documentation. They now document `NativeWindowTransform` and `PollEvents` directly.

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

## 2026-08-22 — `src/core/src/hle/kernel/svc/svc_ipc.rs` vs Eden `src/core/hle/kernel/svc/svc_ipc.cpp`, `k_client_session.cpp`, and `k_server_session.{h,cpp}`

### Intentional differences

- Ruzu retains an inline HLE dispatch fallback for ownerless test fixtures. Eden always queues the
  request through `KClientSession` and waits for the owning server thread.

### Unintentional differences (to fix)

- The inline fallback converted enqueue and receive failures to `ResultInvalidHandle`. Both phases
  now preserve the original Kernel result, matching Eden's `R_RETURN` chain from
  `KServerSession::OnRequest` through `KClientSession::SendSyncRequest` and `SendSyncRequestImpl`.

## 2026-08-22 — `src/core/src/hle/kernel/k_server_session.rs` vs Eden `src/core/hle/kernel/k_server_session.{h,cpp}`

### Intentional differences

- Eden's pointer-descriptor constructors read through a const `MessageBuffer` view. Ruzu's
  `PointerDescriptor::from_raw` reads the same two words directly from the immutable source slice.

### Unintentional differences (to fix)

- The receive and send pointer helpers cloned the complete source message into mutable vectors and
  constructed unused `MessageBuffer` views. Those dead allocations are removed; descriptor offsets,
  memory-copy direction, validation, and destination writes remain unchanged.

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

## 2026-08-22 — `src/core/src/hle/kernel/k_interrupt_manager.rs` vs Eden `src/core/hle/kernel/k_interrupt_manager.{h,cpp}`

### Intentional differences

- Ruzu snapshots the current thread fields before acquiring scheduler and process locks to preserve
  its host-mutex order. Eden accesses the embedded kernel objects directly under its scheduler lock.

### Unintentional differences (to fix)

- The snapshot included `KThread::current_core`, although both Eden and Ruzu use the interrupt's
  `core_id` for the pinned-thread lookup and pin operation. The unused field read is removed.

## 2026-08-22 — `src/core/src/hle/kernel/message_buffer.rs` vs Eden `src/core/hle/kernel/message_buffer.h`

### Intentional differences

- Rust names the header argument `_hdr` in `get_special_data_index` because, exactly as in Eden's
  formula, the special-data start depends only on the fixed message-header size and special-header
  size. Keeping the argument preserves the upstream helper signature without an unused warning.

### Unintentional differences (to fix)

- Ruzu had removed the header parameter from `get_special_data_index` while retaining it in the
  downstream index helpers. The parameter and forwarding chain now match Eden again.

## 2026-08-22 — `src/core/src/file_sys/patch_manager.rs` vs Eden `src/core/file_sys/patch_manager.{h,cpp}`

### Intentional differences

- Rust locks the shared filesystem controller and content-provider union while both temporary
  `PatchManager` values borrow them. Eden receives stable references directly from `Core::System`.
- When no content provider is installed, Ruzu returns empty metadata; Eden's accessor contract
  assumes the provider has already been initialized.

### Unintentional differences (to fix)

- Ruzu was missing `GetMetadataFromBaseOrUpdate`. The associated method now checks the application
  title first and, only when its NACP is absent, checks `GetUpdateTitleID(application_id)`.

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

## 2026-08-22 — `src/core/src/hle/service/hid/hid_server.rs` vs Eden `src/core/hle/service/hid/hid_server.{h,cpp}`

### Intentional differences

- Rust decodes `ClientAppletResourceUserId` directly as its single `u64` `pid` value and returns the
  IPC interface through `ResponseBuilder`; Eden expresses both through CMIF wrapper types.

### Unintentional differences (to fix)

- `CreateAppletResource` previously discarded the resource manager result without reproducing
  Eden's diagnostic. It now logs the ARUID and raw result before constructing the interface.

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

## 2026-08-22 — `src/rdynarmic/src/jit_config.rs` and A32/A64 backend callback wiring vs Eden `src/core/hle/service/jit/jit_context.cpp::DynarmicCallbacks64`

### Intentional differences

- The Rust backend exposes `instruction_synchronization_barrier_raised` as a default no-op trait
  method and wires it for every JIT configuration. This avoids a JIT-service-only backend type while
  leaving existing callback implementations behaviorally unchanged.
- The flag corresponding to Dynarmic's top-level `UserConfig::hook_isb` lives in the matching
  architecture-owned Rust `A64UserConfig`; its default remains false.

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

## 2026-08-22 — `src/video_core/src/shader_environment.rs` vs Eden `src/video_core/shader_environment.{h,cpp}`

### Intentional differences

- Rust validates serialized environment counts and their complete byte size against the remaining
  cache file before allocating. It also uses fallible reservations, rejects empty pipeline entries,
  and requires exactly one environment for compute pipelines. Eden relies on trusted cache contents
  and throwing stream reads; the additional validation prevents a malformed or same-version legacy
  cache from aborting the process in Rust's infallible allocation path.
- Pipeline-loader callbacks return `std::io::Result<()>` so key-read failures reach the same outer
  invalid-cache cleanup that Eden obtains from `ifstream` exceptions.

## 2026-08-22 — `src/video_core/src/renderer_vulkan/pipeline_cache.rs` vs Eden `src/video_core/renderer_vulkan/vk_pipeline_cache.{h,cpp}`

### Intentional differences

- Rust key readers return `std::io::Result` instead of relying on `ifstream::failbit` exceptions.
  Dynamic-feature incompatibility remains a skipped valid entry and does not invalidate the cache.

### Unintentional differences (to fix)

- Cached compute and graphics key read failures were previously logged and swallowed, allowing a
  desynchronized reader to continue. They now propagate through `load_pipelines`, matching Eden's
  whole-file deletion on a failed key read.

## 2026-08-22 — `src/video_core/src/renderer_opengl/gl_shader_cache.rs` vs Eden `src/video_core/renderer_opengl/gl_shader_cache.{h,cpp}`

### Intentional differences

- Rust key readers report `std::io::Result` explicitly instead of using throwing `ifstream` reads.

### Unintentional differences (to fix)

- Cached compute and graphics key read failures were previously logged and swallowed. They now
  reach `load_pipelines` and delete the invalid cache as Eden's stream exception path does.

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

## 2026-08-22 — `src/ruzu/src/boot.rs` debugger lifecycle vs Eden `src/qt_common/render/emu_thread.cpp`

### Intentional differences

- Ruzu's non-Qt boot controller polls the atomic debugger shutdown request in its existing command
  loop; Eden's GDB backend invokes `System::Exit()` from a detached thread.

### Unintentional differences (to fix)

- The frontend previously ignored `use_gdbstub`. It now initializes the debugger after GPU/CPU
  readiness, observes debugger-requested exit, and detaches it before pausing and shutting down the
  application process in the same lifecycle positions as Eden.

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

## 2026-08-22 — `src/core/src/memory/memory.rs` vs Eden `src/core/memory.{h,cpp}` debugger-page marking

### Intentional differences

- Rust receives the process address as its underlying `u64`; the page-table bridge already uses raw
  virtual addresses throughout, while preserving Eden's address-space validation and page walk.

### Unintentional differences (to fix)

- `Memory::MarkRegionDebug` was absent. Watchpoint pages now lose fastmem access, transition from
  `Memory` to `DebugMemory`, and recover their biased host pointer when the last debug reference is
  removed, in Eden's protection-before-page-transition order.

## 2026-08-22 — `src/core/src/hle/kernel/k_process.rs` vs Eden `src/core/hle/kernel/k_process.{h,cpp}` watchpoint ownership

### Intentional differences

- Rust's optional `Arc<Mutex<Memory>>` replaces Eden's directly owned `Memory`; initialized runtime
  processes always have it, while isolated `KProcess::new()` tests can still exercise table and
  reference-count behavior without a system memory owner.

### Unintentional differences (to fix)

- `DebugWatchpoint` stored its type as an untyped byte and insert/remove changed only the table.
  The field now uses the owning bitflag type, and both operations apply Eden's per-page reference
  counting and `MarkRegionDebug` calls for overlapping watchpoints.

## 2026-08-22 — `src/core/src/arm/arm_interface.rs` vs Eden `src/core/arm/arm_interface.{h,cpp}` watchpoints

### Intentional differences

- Rust JIT callbacks are moved owners rather than C++ objects retaining a parent reference. The
  process-array pointer therefore lives in a shared atomic slot, and a match is copied out instead
  of returning a reference whose lifetime cannot cross the callback mutex boundary.

### Unintentional differences (to fix)

- The interface previously duplicated the kernel watchpoint type with primitive addresses and did
  not expose `SetWatchpointArray` through every backend. It now consumes the `k_process.rs` owner and
  applies Eden's half-open range and access-bit matching literally.

## 2026-08-22 — `src/core/src/arm/dynarmic/arm_dynarmic_32.rs` vs Eden `src/core/arm/dynarmic/arm_dynarmic_32.{h,cpp}` watchpoint callbacks

### Intentional differences

- The callback shares halted-watchpoint state with its Rust JIT owner through `Arc<Mutex<_>>` and
  invokes the existing Rust JIT halt bridge. Rust-only exclusive-read/128-bit callback extensions
  perform the same access check before their underlying memory operation.

### Unintentional differences (to fix)

- `CheckMemoryAccess` previously returned unconditionally and the halt translation discarded
  Dynarmic's memory-abort bit. Address validation, read/write matching, retained watchpoint state,
  prefetch/data-abort halts and exclusive-access ordering now match Eden.

## 2026-08-22 — `src/core/src/arm/dynarmic/arm_dynarmic_64.rs` vs Eden `src/core/arm/dynarmic/arm_dynarmic_64.{h,cpp}` watchpoint callbacks

### Intentional differences

- The moved Rust callback uses a shared watchpoint-array pointer and mutex-protected copied match in
  place of Eden's parent reference and raw matched pointer; ownership and halt timing are preserved.

### Unintentional differences (to fix)

- `CheckMemoryAccess` previously returned unconditionally. It now derives Eden's exact enable state,
  validates addresses, distinguishes read/write watchpoints, retains the match and prevents writes
  after requesting the corresponding prefetch/data-abort halt.

## 2026-08-22 — `src/core/src/hle/kernel/physical_core.rs` vs Eden `src/core/hle/kernel/physical_core.cpp` watchpoint loading

### Intentional differences

- Rust passes the address of the stable process-owned array while holding the process lock; Eden
  obtains the same array through `GetWatchpoints()`.

### Unintentional differences (to fix)

- `LoadContext` previously omitted `SetWatchpointArray`, and the data-abort path converted from a
  duplicate ARM watchpoint representation. It now wires the process owner after context/TLS setup
  and forwards that exact typed watchpoint to the debugger.

## 2026-08-22 — `src/core/src/debugger/gdbstub.rs` vs Eden `src/core/debugger/gdbstub.cpp` typed watchpoint reply

### Unintentional differences (to fix)

- The reply path previously reconstructed a watchpoint type from an untyped byte. It now matches the
  owning kernel bitflag directly when selecting `rwatch`, `watch`, or `awatch`.

### Missing items

- The command dispatcher remains the next warning-driven slice recorded in `PORTING_STATE.md`.

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

## 2026-08-23 — `src/shader_recompiler/src/frontend/mod.rs` tests vs Eden `src/shader_recompiler/frontend/maxwell/{decode.cpp,maxwell.inc}`

### Intentional differences

- Ruzu keeps native Rust decoder smoke tests in the module root; Eden's C++ test tree is excluded
  from the port, while the tested instruction words come directly from Eden's Maxwell table.

### Unintentional differences (to fix)

- The NOP and register-IADD instruction builders were unused, and the NOP test only checked that
  decoding did not panic. Both encodings now assert Eden's exact decoded opcode.

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

- The opcode inventory has no missing Eden names,
  while 22 Ruzu-only operations still require ownership and behavior review.

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

## 2026-08-23 — rdynarmic dead IR opcodes vs Eden Dynarmic IR/backend owners

Rust files: `src/rdynarmic/src/ir/opcode.rs` and
`src/rdynarmic/src/backend/x64/{emit,emit_vector_arrangement,emit_vector_helpers}.rs`.

Eden files: `src/dynarmic/src/dynarmic/ir/{opcodes.inc,ir_emitter.h}` and
`src/dynarmic/src/dynarmic/backend/x64/emit_x64_vector.cpp`.

### Unintentional differences (to fix)

- The Rust opcode enum formerly exposed `SetInsertionPoint` and `GetInsertionPoint` as void IR
  instructions. Neither had a producer or backend consumer; Eden exposes insertion-point changes
  solely as `IREmitter` methods. The two dead opcodes and their metadata are removed.
- Rust formerly exposed three immediate shuffle opcodes and x64 emitters with no frontend producer.
  Eden has no such IR opcodes and uses host shuffles locally inside the emitters that require them.
  The dead opcodes, dispatch arms, emitter functions, helper, and signature-only test are removed.

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

## 2026-08-23 — `src/rdynarmic/src/backend/x64/emit_data_processing.rs` vs Eden `backend/x64/emit_x64_data_processing.cpp` (`ExtractRegister`)

### Intentional differences

- Rust's `change_bit` and assembler methods return `Result`; the emitter unwraps them at the same
  points where Eden relies on Xbyak assertions/errors. Register allocation and emission ownership
  remain in the matching x64 data-processing module.

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

## 2026-08-23 — `src/rdynarmic/src/backend/arm64/a32_address_space.rs` vs Eden `backend/arm64/a32_address_space.cpp` (`GenerateIR` constant reads)

### Intentional differences

- Eden's central `Optimization::Optimize` obtains `MemoryReadCode` and `IsReadOnlyMemory` through
  `A32::UserCallbacks`. Ruzu invokes the already-separated Rust passes explicitly and supplies two
  closures over the same callback owner.

### Missing items

- No constant-memory callback is missing in the reviewed `GenerateIR` path. Broader optimization
  pass order and ownership remain tracked separately from this compile-blocking correction.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder.rs` vs Eden `frontend/A32/decoder/arm.inc` (literal loads)

### Intentional differences

- Eden generates its ARM decoder from per-instruction bit-pattern declarations in `arm.inc`.
  Ruzu's existing decoder is a handwritten decision tree, so the six literal patterns are routed
  explicitly inside the matching load/store decode families.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/{load_store.rs,mod.rs}` vs Eden `frontend/A32/translate/impl/{load_store.cpp,a32_translate_impl.h}` (load visitors)

### Intentional differences

- Rust extracts typed fields from `DecodedArm` inside each matching snake-case visitor; Eden's
  generated decoder passes them as typed parameters. ARM condition-state bookkeeping remains in
  Ruzu's block translator rather than being repeated in every visitor.
- Rust uses `wrapping_add`/`wrapping_sub` to state C++ unsigned-`u32` address wraparound explicitly,
  and `Reg::from_u32` replaces Eden's register `operator+` after the same validity checks.

### Unintentional differences (to fix)

- Frontend-wide pre-existing difference: Ruzu performs condition-state setup before dispatch,
  whereas Eden performs each visitor's encoding validation before `ArmConditionPassed`. Correcting
  that ordering requires restoring visitor-owned condition state across the A32 frontend, not a
  load/store-local helper, and remains a separate structural slice.

## 2026-08-23 — `src/rdynarmic/src/backend/arm64/emit_arm64_packed.rs` vs Eden `backend/arm64/emit_arm64_packed.cpp`

### Intentional differences

- Eden emits through Oaknut register wrappers. Ruzu propagates encoder/allocation failures with
  `Result` and passes realized vector-register indexes to its existing `inst.rs` encoder boundary;
  the upstream helper ownership and instruction ordering remain local to the matching packed file.
- Eden declares generic `EmitIR` specializations through `emit_arm64.h`. Ruzu's central dispatcher
  routes the same opcode set to `emit_packed_instruction`, while each implementation remains owned
  by the new file corresponding to `emit_arm64_packed.cpp`.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (unprivileged loads)

### Intentional differences

- Eden builds an ordered matcher from `thumb32.inc`; Ruzu retains its existing handwritten
  decision tree. The Rust branch now makes the same `1110` low-control-nibble priority explicit
  before the broader `1PUW` immediate forms.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_load_byte.rs` vs Eden `frontend/A32/translate/impl/{thumb32_load_byte.cpp,a32_translate_impl.h}`

### Intentional differences

- Eden's generated matcher passes decoded fields as typed visitor parameters. Ruzu's matching
  snake-case methods read those fields from `DecodedThumb32`; each method and helper remains in the
  corresponding byte-load owner, while `thumb32.rs` only dispatches.
- Rust uses a higher-ranked function pointer over `A32IREmitter` for Eden's
  `ExtensionFunctionU8` member pointer and explicit `wrapping_add`/`wrapping_sub` for the same
  unsigned `u32` literal-address arithmetic.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_load_halfword.rs` vs Eden `frontend/A32/translate/impl/{thumb32_load_halfword.cpp,a32_translate_impl.h}`

### Intentional differences

- Eden's generated matcher passes decoded fields to visitor methods. Ruzu's matching methods read
  the fields from `DecodedThumb32`, use a higher-ranked Rust function pointer for
  `ExtensionFunctionU16`, and state unsigned literal-address wraparound explicitly.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_load_word.rs` vs Eden `frontend/A32/translate/impl/{thumb32_load_word.cpp,a32_translate_impl.h}`

### Intentional differences

- Eden's generated matcher passes typed decoded fields to its visitor. The five snake-case Rust
  visitors read the same fields from `DecodedThumb32`; the dispatcher remains routing-only.
- Rust spells unsigned literal-address wraparound explicitly and represents Eden's terminal
  variants with the existing `Terminal` enum.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (store single data item)

### Intentional differences

- Eden builds a priority-ordered matcher from `thumb32.inc`; Ruzu retains its handwritten
  decision tree while spelling the same control-nibble and register-form masks explicitly.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_store_single_data_item.rs` vs Eden `frontend/A32/translate/impl/{thumb32_store_single_data_item.cpp,a32_translate_impl.h}`

### Intentional differences

- Eden passes typed matcher fields to the fifteen visitors; the snake-case Rust visitors read the
  same fields from `DecodedThumb32`, while the dispatcher only routes exact decoded identities.
- Rust function pointers stand in for Eden's immediate-store callbacks and register-store lambdas.
  Separate byte, halfword, and word callbacks retain the same truncation and memory-operation
  ownership inside the matching file.

## 2026-08-23 — `src/rdynarmic/src/ir/a32_emitter.rs` vs Eden `frontend/A32/{a32_ir_emitter.h,a32_ir_emitter.cpp}` (memory access boundary)

### Intentional differences

- Rust's shared `Value` wrapper requires explicit byte/halfword coercion where Eden's C++ method
  signatures carry `U8`/`U16` statically. The emitted operand types and operation order match.
- `ExclusiveReadMemory64` returns a Rust tuple instead of `std::pair`; both expose separate low and
  high words, and `ExclusiveWriteMemory64` accepts those words separately in the same order.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (dual/exclusive/table branch)

### Intentional differences

- Eden generates its first-match decoder from pattern strings; Ruzu retains the handwritten
  decoder and represents this family as an ordered mask table derived from the same strings.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_load_store_dual.rs` vs Eden `frontend/A32/translate/impl/{thumb32_load_store_dual.cpp,a32_translate_impl.h}`

### Intentional differences

- Eden's generated matcher passes typed fields into visitor methods. The eighteen snake-case Rust
  visitors read the same fields from `DecodedThumb32`; dispatch remains in `thumb32.rs` and all
  behavior and helpers live in the matching owner.
- Rust represents Eden's `U32`/`U64` SSA wrappers with `Value` and its terminal variants with the
  existing `Terminal` enum.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (load/store multiple)

### Intentional differences

- Eden generates its ordered decoder from pattern strings; Ruzu retains a handwritten decoder and
  uses an ordered mask table derived from those same six strings.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_load_store_multiple.rs` vs Eden `frontend/A32/translate/impl/{thumb32_load_store_multiple.cpp,a32_translate_impl.h}`

### Intentional differences

- Eden's matcher supplies typed `Imm<15>`/`Imm<16>` fields. Rust reads the same instruction fields
  from `DecodedThumb32` and explicitly masks the store lists to retain the `Imm<15>` boundary.
- Rust represents Eden's `IR::U32` wrappers with `Value` and uses `count_ones` for `std::popcount`.

## 2026-08-23 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `ir/ir_emitter.h` (`PackedAbsDiffSumU8`)

### Intentional differences

- Rust uses snake case and the shared `Value` wrapper in place of Eden's typed `U32` wrapper.

## 2026-08-23 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `ir/ir_emitter.h` (`MostSignificantWord`)

### Intentional differences

- Rust uses a concrete `ResultAndCarry` structure containing `Value` fields where Eden uses the
  templated `ResultAndCarry<U32>` type.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (multiply)

### Intentional differences

- Eden generates one global first-match table from pattern strings; Ruzu preserves its handwritten
  outer decoder and uses an ordered mask table derived from the sixteen multiply strings.

### Missing items

- The long-multiply visitor owner
  remains a separate audit slice.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_multiply.rs` vs Eden `frontend/A32/translate/impl/{thumb32_multiply.cpp,a32_translate_impl.h}`

### Intentional differences

- Eden's generated matcher passes typed registers and selector bits. The snake-case Rust visitors
  read those exact fields from `DecodedThumb32`, and Rust variable swaps replace `std::swap`.
- Rust uses `Value` for Eden's typed SSA wrappers and explicit `ImmU1(false/true)` carry inputs.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (long multiply)

### Intentional differences

- Eden generates one global first-match table from pattern strings; Ruzu retains its handwritten
  outer decoder and uses an ordered mask table derived from the same ten long-multiply strings.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_long_multiply.rs` vs Eden `frontend/A32/translate/impl/{thumb32_long_multiply.cpp,a32_translate_impl.h}`

### Intentional differences

- Eden's generated matcher passes typed registers and selector bits. The snake-case Rust visitors
  read the same fields from `DecodedThumb32`, and Rust's `Value` represents Eden's typed SSA
  wrappers.
- Rust free-function pointers cannot name `IREmitter` methods with Eden's C++ member-function
  pointer type, so two mechanical signed/unsigned wrappers feed the matching `DivideOperation`
  helper boundary.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (branch)

### Intentional differences

- Eden generates one global first-match table from pattern strings; Ruzu retains its handwritten
  outer family routing and uses the same ordered mask/value entries within the branch family.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_branch.rs` vs Eden `frontend/A32/translate/impl/{thumb32_branch.cpp,a32_translate_impl.h}`

### Intentional differences

- Eden's generated matcher passes typed immediate fields. The snake-case Rust visitors consume the
  same fields through `DecodedThumb32` offset helpers, and Rust terminal variants represent Eden's
  `IR::Term` values.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/mod.rs` vs Eden `frontend/A32/translate/impl/a32_translate_impl.h` (`ThumbExpandImm_C`)

### Intentional differences

- Eden receives separate typed `i`, `imm3`, and `imm8` fields; Rust receives their already
  concatenated twelve-bit value from `DecodedThumb32`.
- Rust represents Eden's `IR::U1` carry with the shared `Value` SSA wrapper.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (modified immediate)

### Intentional differences

- Eden generates one global first-match decoder from pattern strings; Ruzu retains its handwritten
  family routing and an ordered mask table derived from the same sixteen strings.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_data_processing_modified_immediate.rs` vs Eden `frontend/A32/translate/impl/{thumb32_data_processing_modified_immediate.cpp,a32_translate_impl.h}`

### Intentional differences

- Eden's generated matcher passes typed immediate fields and registers; the snake-case Rust
  visitors read those same fields from `DecodedThumb32` and use `Value` for typed SSA values.
- Eden's soft `ASSERT` records an impossible decoder-contract violation and may continue; Rust
  `assert!` stops on the same impossible direct-dispatch state. Valid decoded instructions cannot
  reach those assertions.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (plain binary immediate)

### Intentional differences

- Eden generates one global first-match decoder from pattern strings; Ruzu retains its handwritten
  family routing and an ordered mask table derived from the same fifteen entries.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_data_processing_plain_binary_immediate.rs` vs Eden `frontend/A32/translate/impl/{thumb32_data_processing_plain_binary_immediate.cpp,a32_translate_impl.h}`

### Intentional differences

- Eden passes typed matcher fields to each visitor; the snake-case Rust visitors extract the same
  fields from `DecodedThumb32`. A small Rust enum represents Eden's member-function pointer used by
  the two saturation helpers.
- Eden's two-argument shift IR methods do not expose a carry input; the shared Rust IR methods take
  one, so shifts whose carry result is unused receive an immediate false value.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (shifted register)

### Intentional differences

- Eden generates one global first-match decoder from pattern strings; Ruzu retains its handwritten
  family routing and an ordered mask table derived from the same seventeen entries.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_data_processing_shifted_register.rs` vs Eden `frontend/A32/translate/impl/{thumb32_data_processing_shifted_register.cpp,a32_translate_impl.h}`

### Intentional differences

- Eden's generated matcher passes typed fields; the snake-case Rust visitors extract the same
  fields from `DecodedThumb32`. The private `shifted_register` helper is a mechanical expression of
  Eden's repeated `EmitImmShift(GetRegister(m), ..., GetCFlag())` call.

## 2026-08-23 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `ir/ir_emitter.h` (`PackedAddU16`)

### Intentional differences

- Rust represents Eden's templated `ResultAndGE<U32>` with a concrete `ResultAndGE` containing two
  shared SSA `Value` handles.

### Missing items

- The neighboring packed-operation
  builders remain outside this prerequisite slice and will be audited with their owners.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/helpers.rs` vs Eden `frontend/A32/translate/impl/common.h` (`Rotate`)

### Intentional differences

- Rust receives the two-bit `SignExtendRotation` field as its numeric decoded value and represents
  Eden's typed IR values with the shared `Value` wrapper.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (register)

### Intentional differences

- Eden generates one global first-match decoder from pattern strings; Ruzu routes the `0xFA`
  family to an ordered mask table derived from the same sixteen register entries.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_data_processing_register.rs` vs Eden `frontend/A32/translate/impl/{thumb32_data_processing_register.cpp,a32_translate_impl.h}`

### Intentional differences

- Eden represents the four shift member-function pointers with a C++ function-pointer type; Rust
  passes the matching `ShiftType` to one `shift_instruction` helper. Typed matcher fields are read
  from `DecodedThumb32`.

## 2026-08-23 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `ir/ir_emitter.h` (`PackedSelect`)

### Intentional differences

- Rust represents Eden's typed `U32` operands and result through the shared SSA `Value` wrapper.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (miscellaneous)

### Intentional differences

- Eden generates one global matcher; Ruzu checks the register table first within the `0xFA` family
  and then an ordered table derived from the same ten miscellaneous patterns.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_misc.rs` vs Eden `frontend/A32/translate/impl/{thumb32_misc.cpp,a32_translate_impl.h}`

### Intentional differences

- Eden's generated matcher passes typed registers; the snake-case Rust visitors extract the same
  registers from `DecodedThumb32` and use the shared SSA `Value` representation.

## 2026-08-23 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `ir/ir_emitter.h` (packed parallel builders)

### Intentional differences

- Rust uses concrete `Value` and `ResultAndGE` types where Eden's declarations use typed IR
  templates. Each method remains explicit to retain one-to-one auditability.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (parallel)

### Intentional differences

- Eden generates one global matcher; Ruzu routes the `0xFA` family through register, parallel, then
  miscellaneous ordered mask tables matching those source sections.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_parallel.rs` vs Eden `frontend/A32/translate/impl/{thumb32_parallel.cpp,a32_translate_impl.h}`

### Intentional differences

- Eden passes typed matcher registers; every explicit snake-case Rust visitor extracts the same
  fields from `DecodedThumb32` and uses the shared SSA `Value` representation.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb16.rs` vs Eden `frontend/A32/translate/impl/thumb16.cpp` (`thumb16_BX`)

### Intentional differences

- Rust exposes the snake-case visitor within the translation module so `thumb32_BXJ` can preserve
  Eden's direct delegation without duplicating the branch lifecycle.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/decoder_thumb32.rs` vs Eden `frontend/A32/decoder/{thumb32.h,thumb32.inc}` (control)

### Intentional differences

- Eden generates one global first-match decoder from pattern strings; Rust keeps the same ordered
  control entries as explicit mask/value tuples in its branch-and-control decoder.

### Missing items

- UDF and branch entries
  remain in their following upstream order.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/thumb32_control.rs` vs Eden `frontend/A32/translate/impl/{thumb32_control.cpp,a32_translate_impl.h}`

### Intentional differences

- Typed matcher fields are extracted from `DecodedThumb32`; Rust represents terminal and SSA
  values with its existing enums while preserving Eden's operation sequence.

## 2026-08-23 — `src/rdynarmic/src/interface/optimization_flags.rs` vs Eden `interface/optimization_flags.h`

### Intentional differences

- Rust uses a transparent `u32` newtype with standard bitwise traits instead of a scoped C++ enum
  plus free operator overloads. `contains` and `bits` expose the checks needed by existing Rust
  consumers. `jit_config` temporarily re-exports the type while its shared configuration is split.

## 2026-08-23 — `src/rdynarmic/src/interface/a32/config.rs` vs Eden `interface/A32/config.h` (`Exception`)

### Intentional differences

- Rust exposes `as_u32` for the existing SSA immediate boundary. `frontend/a32/types.rs` retains a
  compatibility re-export so translation owners can migrate independently of this ownership move.

### Missing items

- `UserCallbacks` was restored in the later
  2026-08-24 configuration-owner slice; `UserConfig` remains outstanding.

## 2026-08-23 — `src/rdynarmic/src/interface/a64/config.rs` vs Eden `interface/A64/config.h` (public enums)

### Intentional differences

- Rust applies normal PascalCase spelling to Eden's `VA` acronym in variant names.
  `frontend/a64/types.rs` temporarily re-exports `Exception` for existing translation consumers.

### Missing items

- `UserCallbacks` was restored in the later
  2026-08-24 configuration-owner slice; `UserConfig` remains outstanding.

## 2026-08-23 — `src/rdynarmic/src/jit_config.rs` vs Eden `interface/{A32,A64}/config.h` (`UserCallbacks` exclusive surface)

### Intentional differences

- Rust still exposes one temporary shared callback trait while the interrupted configuration split
  is completed; the exact prerequisite and resume point are recorded in the project-local state
  file, which is excluded from commits.
- `set_halt_reason_ptr`, `set_pc_ptr`, and `set_upper_location_descriptor_ptr` are Rust ownership
  adapters used after the boxed callback and JIT state acquire stable addresses. They do not add
  guest-visible callback events.

### Unintentional differences (to fix)

- The remaining shared A32/A64 callback trait still exposes each architecture's members to the
  other architecture. The active prerequisite slice will replace it with the two upstream-owned
  traits before the `UserConfig` split resumes.

### Missing items

- The architecture-owned traits were added in the later 2026-08-24 configuration-owner slice.
  Runtime consumers still use this shared trait through explicit compatibility implementations.

## 2026-08-23 — `src/rdynarmic/src/{jit.rs,backend/common/a32_callbacks.rs}` vs Eden `backend/x64/{a32_emit_x64_memory.cpp,a64_emit_x64_memory.cpp,a32_interface.cpp,a64_interface.cpp}`

### Intentional differences

- Rust trampolines make the callback target explicit and share mechanical reservation bookkeeping
  with the in-progress arm64 backend. The generated-code callback table retains internal fields
  named `exclusive_read_*`; those fields target the normal `memory_read_*` host methods just as
  Eden's emitters instantiate exclusive-read helpers with `UserCallbacks::MemoryRead*`.

## 2026-08-23 — `src/rdynarmic/src/backend/arm64/{a32_address_space.rs,a64_address_space.rs}` vs Eden `backend/arm64/{a32_address_space.cpp,a64_address_space.cpp}`

### Intentional differences

- Rust uses explicit callback-context trampolines instead of Eden's generated devirtualized call
  trampolines; the target callback and monitor ordering remain the same.

## 2026-08-23 — `src/rdynarmic/src/{ir/a32_emitter.rs,frontend/a32/translate/{thumb16.rs,multiply.rs,thumb32_data_processing_modified_immediate.rs,thumb32_data_processing_shifted_register.rs,thumb32_data_processing_register.rs}}` vs Eden `frontend/A32/{a32_ir_emitter.{h,cpp},translate/impl/{thumb16.cpp,multiply.cpp,thumb32_data_processing_modified_immediate.cpp,thumb32_data_processing_shifted_register.cpp,thumb32_data_processing_register.cpp,a32_translate_impl.h}}`

### Intentional differences

- Rust exposes `nzcv_from` beside the A32 `nz_from` adapter because it cannot inherit Eden's
  generic `IR::IREmitter::NZCVFrom`; both methods emit the exact upstream opcode and keep the
  inherited C++ call surface visible to translation owners.

## 2026-08-23 — `src/rdynarmic/src/frontend/a32/translate/vfp.rs` vs Eden `frontend/A32/translate/impl/{vfp.cpp,a32_translate_impl.h}` (VFP memory transfers)

### Intentional differences

- Eden's decoder exposes separate A1/A2 VSTM and VLDM visitors. Rust currently decodes each family
  to one identity and selects the single- or double-register path from `sz` in the same `vfp.rs`
  owner; the source comment records this structural adaptation, while validation, ordering, and IR
  operations follow the corresponding Eden path.
- ARM condition handling remains in Rust's block-level conditional translator instead of being
  repeated inside every VFP visitor; the memory-transfer bodies execute only after that guard.

## 2026-08-23 — `src/rdynarmic/src/frontend/a64/translate/simd_crypto_four_register.rs` vs Eden `frontend/A64/translate/impl/{simd_crypto_four_register.cpp,impl.h}`

### Intentional differences

- Rust extracts `Vm`, `Va`, `Vn`, and `Vd` from `DecodedInst` inside each visitor because its
  decoder dispatch passes a decoded instruction rather than Eden's typed visitor arguments. The
  register read order, IR operation nesting, and destination write remain identical.
- Rust's `add_32` and `rotate_right_32` builders take an explicit false carry input; this is the IR
  builder representation of Eden's non-flag-setting `Add` and `RotateRight` calls.

## 2026-08-23 — `src/rdynarmic/src/frontend/a64/translate/simd_crypto_three_register.rs` vs Eden `frontend/A64/translate/impl/{simd_crypto_three_register.cpp,impl.h}`

### Intentional differences

- Rust's file-local `Sm3TtVariant`, `sm3tt1`, and `sm3tt2` mirror Eden's anonymous-namespace enum
  and helpers. Each public visitor extracts the typed register and two-bit immediate operands from
  `DecodedInst` before forwarding them to the matching helper.
- Rust's arithmetic and rotation builders take an explicit false carry input; operation order and
  data dependencies match Eden's non-flag-setting nested IR expressions.

## 2026-08-23 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `ir/ir_emitter.h` (generic extension and signed-to-unsigned saturated-shift builders)

### Intentional differences

- Rust resolves an instruction-backed `Value` through `Block::inst_real_return_type` because its
  SSA references do not carry Eden's `UAny::GetType()` information inline. Immediate and
  instruction inputs nevertheless select the same extension opcode.
- Unsupported input types panic instead of reaching Eden's `UNREACHABLE`; both represent an
  internal IR construction error rather than guest-visible validation.

## 2026-08-23 — `src/rdynarmic/src/frontend/a64/translate/visitor.rs` vs Eden `frontend/A64/translate/impl/impl.cpp` (`V_scalar` write adapter)

### Intentional differences

- Rust retains a runtime U128 assertion for the 128-bit path because its `Value` type does not
  encode Eden's compile-time `UAnyU128` constraint. Valid translated instructions observe the same
  direct `SetQ` behavior.

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

## 2026-08-23 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `ir/ir_emitter.h` (scalar saturated-arithmetic builders)

### Intentional differences

- Rust resolves instruction-backed operand types through the block arena because `Value::Inst`
  does not carry Eden's `UAny::GetType()` inline. It asserts equal operand types before selecting
  the same width-specific opcode.
- Unsupported widths panic instead of returning Eden's empty `UAny` or reaching `UNREACHABLE`;
  both are internal builder misuse and cannot be produced by a valid frontend visitor.

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

## 2026-08-23 — `src/rdynarmic/{build.rs,src/frontend/a64/decoder.rs}` vs Eden `frontend/A64/decoder/{a64.h,a64.inc}` (trailing instruction comments)

### Intentional differences

- Eden consumes `a64.inc` through C++ macros, while Rust's build script parses the same pattern
  table and generates its enum and two-tier lookup table. The parser therefore locates the closing
  `INST(...)` parenthesis explicitly before processing its three fields.

## 2026-08-23 — `src/rdynarmic/src/frontend/a64/translate/system_flag_{manipulation,format}.rs` vs Eden `frontend/A64/translate/impl/system_flag_{manipulation,format}.cpp` and `impl.h`

### Intentional differences

- Rust visitors accept an unused `DecodedInst` for Eden's operand-free CFINV, XAFlag, and AXFlag
  because all dispatch methods share the generated decoder interface.
- Rust passes an explicit false carry input to 32-bit logical shifts. Eden's generic IR builder
  supplies the same non-flag-setting default implicitly.
- SETF8 and SETF16 are declarations only in Eden's `impl.h`; their decoder table entries are
  commented out and the reviewed snapshot provides no C++ definitions, so Rust does not invent
  unreachable implementations.

## 2026-08-23 — `src/rdynarmic/src/frontend/a64/translate/simd_sha512.rs` vs Eden `frontend/A64/translate/impl/{simd_sha512.cpp,impl.h}`

### Intentional differences

- Rust visitors extract typed decoder operands from `DecodedInst`; all ten methods remain in the
  matching file owner.
- Rust's 32-bit rotate and 64-bit add builders require explicit carry inputs, so two mechanical
  file-local adapters supply Eden's implicit false carry without changing operation ordering.
- Eden's two lambdas inside `SHA512Hash` are represented by file-local Rust functions because
  simultaneous closures borrowing the mutable emitter cannot coexist. They retain the same
  captured hash-part and upper/lower-Y inputs and are called at the same points.

## 2026-08-23 — `src/rdynarmic/src/frontend/a64/translate/simd_shift_by_immediate.rs` vs Eden `frontend/A64/translate/impl/{simd_shift_by_immediate.cpp,impl.h}`

### Intentional differences

- Rust visitors extract Eden's typed immediate and vector-register operands from `DecodedInst`;
  the six anonymous-namespace helpers remain file-local with the same responsibilities.
- Rust passes `fpcr_controlled=true` explicitly to the four fixed-point vector conversion IR
  builders. Eden's builder API supplies the same value as its default argument.
- Rust computes Eden's `mcl::bit::ones<u64>(esize)` masks with an equivalent bounded `u64` shift.

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

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/a64_translate.rs` vs Eden `frontend/A64/translate/a64_translate.{h,cpp}` and backend translation call sites

### Intentional differences

- Rust's block-level `translate` allocates and returns its `Block`; Eden receives a reset block by
  mutable reference from each backend. Instruction loop ordering, terminal assertion, cycle count,
  single-step link, and end-location update remain the same.
- Rust represents `MemoryReadCodeFuncType` as a borrowed `dyn Fn` and uses `Option::map` for the
  single-instruction decoder result. Both preserve Eden's optional-code and decoder semantics.
- The implementation lives in its own `a64_translate.rs`; `translate/mod.rs` only declares and
  re-exports the owner, matching Rust module mechanics without retaining behavior in the dispatcher.

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

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/load_store_exclusive.rs` vs Eden `frontend/A64/translate/impl/{load_store_exclusive.cpp,impl.h}` (interpreter-producer audit)

### Intentional differences

- Rust visitors extract operands from `DecodedInst`; pair visitors reconstruct Eden's
  `concatenate(Imm<1>{1}, sz)` from the encoded size bits before invoking the same shared helper.
- Rust represents Eden's optional registers with `Option<Reg>` and names the overloaded
  `ExclusiveMem` helpers `exclusive_mem_read` and `exclusive_mem_write`.

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/simd_vector_x_indexed_element.rs` vs Eden `frontend/A64/translate/impl/{simd_vector_x_indexed_element.cpp,impl.h}` (FCMLA fallback audit)

### Intentional differences

- Rust extracts operands from `DecodedInst` and initializes FCMLA's rotation-selected elements as
  tuples because Rust forbids Eden's declaration-then-assignment form.

### Unintentional differences (to fix)

- The six anonymous-namespace helpers are currently visitor methods or duplicated field-extraction
  logic, including an extra `fp_multiply_by_element_fields` dispatcher. Their ownership must be
  restored in a later full owner slice; all 21 upstream visitor definitions are present.

### Missing items

- No visitor definition is missing; the remaining gap is the ownership/boundary mismatch for the
  six file-local helpers.

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/visitor.rs` vs Eden `frontend/A64/translate/impl/{impl.cpp,impl.h}` (`ExclusiveMem`, `SignExtend`, and `ZeroExtend` helpers)

### Intentional differences

- Rust names Eden's overloaded `ExclusiveMem` methods `exclusive_mem_read` and
  `exclusive_mem_write` because Rust has no function overloading. The write helper keeps Eden's
  address, byte-size, access-type, value order.
- Invalid sizes use Rust `unreachable!` diagnostics where Eden uses `UNREACHABLE()` after its
  switches.

## 2026-08-24 — `src/rdynarmic/src/ir/emitter.rs` vs Eden `ir/ir_emitter.h` (`MemOp` ownership)

### Intentional differences

- Rust applies PascalCase variant spelling and derives comparison/debug traits; the enum remains a
  control-flow type and is not encoded into SSA or exposed through the JIT ABI.

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/load_store_multiple_structures.rs` vs Eden `frontend/A64/translate/impl/load_store_multiple_structures.cpp` (`MemOp` ownership)

### Intentional differences

- Rust uses an explicit unreachable match arm for Prefetch; this upstream helper is called only by
  load and store decoder identities.

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/load_store_single_structure.rs` vs Eden `frontend/A64/translate/impl/load_store_single_structure.cpp` (`MemOp` ownership)

### Intentional differences

- Rust uses an explicit unreachable match arm for Prefetch; no single-structure decoder identity
  supplies that operation.

## 2026-08-24 — `src/rdynarmic/src/frontend/a64/translate/load_store_register_immediate.rs` vs Eden `frontend/A64/translate/impl/load_store_register_immediate.cpp` (`LoadStoreSIMD` `MemOp` ownership)

### Intentional differences

- Rust uses an explicit unreachable Prefetch arm in `load_store_simd`, matching Eden's default
  unreachable path because only SIMD load and store visitors call that helper.

## 2026-08-24 — `src/rdynarmic/src/{ir/terminal.rs,ir/opt,frontend/a64/translate/visitor.rs,backend/{x64,arm64}}` vs Eden `ir/terminal.h`, `frontend/A64/translate/impl/impl.cpp`, and host terminal emitters (terminal inventory)

### Intentional differences

- Rust represents the C++ variant surface as an enum and uses `Box` for recursive storage. Backend
  emitters remain split according to Rust's existing host modules rather than C++ class overloads.
- Translation tests retain their positive opcode/exception/terminal assertions; 118 negative
  `Terminal::Interpret` assertions were removed because absence of the variant makes them
  tautological rather than behavioral checks.

### Unintentional differences (to fix)

- Rust still permits conditional terminals to contain another recursive `Terminal`, whereas Eden
  restricts `If`, `CheckBit`, and `CheckHalt` children to `LeafTerminal`. Aligning this type-level
  invariant is a broader terminal-ownership refactor and remains outstanding.

### Missing items

- A distinct Rust `LeafTerminal` owner enforcing Eden's non-recursive conditional children.

## 2026-08-24 — `src/rdynarmic/src/{jit_config.rs,jit.rs,backend/{common/a32_callbacks.rs,x64,arm64}}` vs Eden `interface/{A32,A64}/config.h` and host callback plumbing (interpreter-callback inventory)

### Intentional differences

- Rust still exposes a temporary shared `UserCallbacks` trait and constructs boxed x64 callback
  adapters, while Eden owns separate A32/A64 interfaces and devirtualizes their methods directly.
  That broader configuration-owner split remains the next parity slice.
- The arm64 Rust backend stores callback addresses in an explicit callback table before emitting
  trampolines; Eden's templated C++ emitters derive them directly from the architecture callback
  type. The surviving callback inventory and relocation targets now match.

### Missing items

- Separate A32 and A64 callback traits in their matching configuration owners remain missing; the
  shared callback interface is retained only until that structural prerequisite is implemented.

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

- The legacy shared trait and its raw integer event surface remain in use by runtime/backend
  consumers. They must be removed after those consumers migrate to the new typed owners.

### Missing items

- `UserConfig` was restored in the following 2026-08-24 configuration-owner slice.
- Direct A32 and A64 runtime/backend consumption of the new traits remains the next prerequisite
  before the legacy shared callback trait can be deleted.

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

- Runtime JITs and host backends still consume the merged legacy `JitConfig`; migration to these
  new owners remains required before the old structure can be removed.

### Missing items

- Direct A32/A64 JIT and backend construction from their respective `UserConfig` types.
- Removal of the legacy shared `jit_config::JitConfig` after all callers have migrated.

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

- The x64 `EmitCallbacks` and `RawExclusiveWriteCallbacks` structures are still shared between
  A32 and A64, so A32 construction must populate unreachable placeholders for their A64-only
  128-bit/cache/counter slots. Splitting these backend callback owners remains required.

### Missing items

- Direct A64 runtime/backend migration to `interface/a64/config.rs::UserConfig` and removal of the
  legacy shared configuration/callback compatibility layer.
- Architecture-specific x64 callback-table types matching Eden's separate A32/A64 emitters.

## 2026-08-24 — `src/rdynarmic/src/backend/{common/emit_context.rs,x64/emit_x64_memory.rs,arm64/{emit_arm64.rs,emit_arm64_memory.rs}}` vs Eden `backend/{x64/emit_x64_memory.h,arm64/{emit_arm64.h,emit_arm64_memory.cpp}}` (`page_table_log2_stride`)

### Intentional differences

- Rust's existing `MemoryEmitConfig` is shared by the two host emitters, whereas Eden stores the
  field in each architecture `UserConfig` and copies it into the arm64 `EmitConfig`. Both A32 and
  A64 construction paths now forward the architecture-owned value into that mechanical backend
  container.
- The temporary public `JitConfig` compatibility bridge exposes the stride through its nested
  memory configuration until remaining callers migrate to architecture-owned configurations.

### Missing items

- Removing the temporary merged `JitConfig` remains part of the architecture-configuration
  migration; no page-table stride behavior remains missing in the reviewed x64 or arm64 lookup.

## 2026-08-24 — `src/core/src/arm/dynarmic/arm_dynarmic_{32,64}.rs` vs Eden `core/arm/dynarmic/arm_dynarmic_{32,64}.{h,cpp}` (`page_table_log2_stride`)

### Intentional differences

- Eden indexes its interleaved 32-byte `Common::PageTable::PageEntryData` records and therefore
  uses a log2 stride of five. Ruzu exposes its separate contiguous `PageInfo` pointer buffer to
  rdynarmic, so both JIT owners derive the stride from `size_of::<PageInfo>()` (eight bytes and a
  log2 stride of three on the supported 64-bit hosts). A compile-time assertion preserves the
  required power-of-two layout contract.

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

### Missing items

- The legacy shared `jit_config::{JitConfig,UserCallbacks}` remains as a caller compatibility
  boundary and still narrows its old read-only-memory query to 32 bits. It must be removed after
  all external construction sites use the separate A32/A64 owners.
- The x64 `EmitCallbacks` and `RawExclusiveWriteCallbacks` containers are still shared between
  A32 and A64; splitting those backend tables remains a separate ownership slice.

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

### Missing items

- `InstructionCacheOperationRaised` still logs operations instead of invoking the owning JIT's
  range/all-cache invalidation methods and requesting `CacheInvalidation` like Eden. Restoring this
  requires a shared invalidation request owned across the Rust callback/JIT lifetime boundary.
- Other remaining production and test callers still use the temporary shared `JitConfig`; this
  slice removes the compatibility boundary from both production CPU owners.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/{a32,a64}_interface.rs` vs Eden `backend/arm64/{a32,a64}_interface.cpp` and `interface/{A32,A64}/config.h` (test configuration ownership)

### Intentional differences

- Rust test callbacks retain optional shared pointer-observation state so lifecycle tests can
  assert that generated-code callback pointers target the final boxed interface state. Eden's
  production interfaces do not contain these Rust regression fixtures.

### Missing items

- The remaining legacy test-configuration users in other ARM64 backend files are outside this
  interface-owned slice and still need conversion before the merged compatibility layer can be
  removed.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/{emit_context,emit_arm64_a64}.rs` vs Eden `backend/arm64/{emit_context.h,emit_arm64_a64.cpp}` and `interface/A64/config.h` (A64 emitter test ownership)

### Intentional differences

- Rust emission helpers return `Result` because instruction-buffer writes are fallible; the test
  fixtures exercise the same emitted instruction and relocation sequences as Eden through this
  error-aware API.

### Missing items

- Other ARM64 A64 address-space and shared-memory emitter test fixtures still depend on the merged
  compatibility layer and remain separate ownership slices.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/a64_address_space.rs` vs Eden `backend/arm64/a64_address_space.{h,cpp}` and `interface/A64/config.h` (test configuration ownership)

### Intentional differences

- Rust callback-thunk tests retain observable fields for memory and system events; Eden implements
  the corresponding production trampolines through devirtualized C++ member-function pointers.

### Missing items

- Shared ARM64 memory-emitter tests still construct the merged configuration and are handled in a
  later file-owned slice.

## 2026-08-24 — `src/core/src/cpu_manager.rs` vs Eden `core/cpu_manager.{h,cpp}` and `core/hle/kernel/physical_core.cpp` (ordinary JIT halt path)

### Intentional differences

- Explicit `RUZU_SPIN_TRACE` requests may still capture a halt context, and the Rust-only null-PC
  `BreakLoop` workaround captures one context to classify that known bridge failure. Neither path
  runs for an ordinary zero-reason cycle-budget expiration.

### Missing items

- The larger Rust cooperative run-loop structure still differs from Eden's direct
  `PhysicalCore::RunThread`; this change is limited to removing non-upstream work from its hot
  ordinary-halt path.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/emit_arm64_memory.rs` vs Eden `backend/arm64/emit_arm64_memory.cpp` and `interface/A64/config.h` (memory-emitter test ownership)

### Intentional differences

- Rust memory-emission tests construct an `EmitConfig` explicitly around the fallible instruction
  writer; their expected ARM64 words and relocation records remain the behavioral oracle for the
  same helpers owned by Eden's file.

### Missing items

- The A32-specific ARM64 memory and coprocessor emitter fixtures remain on the compatibility layer
  and require their own A32-owned conversion.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/emit_arm64_a32_{memory,coprocessor}.rs` vs Eden `backend/arm64/emit_arm64_a32_{memory,coprocessor}.cpp` and `interface/A32/config.h` (A32 emitter test ownership)

### Intentional differences

- Rust keeps native unit-test harnesses next to these file-owned emitters and constructs the
  fallible instruction writer explicitly; the production emission sequences remain unchanged.

### Missing items

- The shared ARM64 dispatcher and A32 dispatcher test fixtures in `emit_arm64.rs` and
  `emit_arm64_a32.rs` still use the merged compatibility layer and require separate owner-aligned
  conversion.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/{emit_arm64,emit_arm64_a32}.rs` vs Eden `backend/arm64/emit_arm64.{h,cpp}`, `backend/arm64/emit_arm64_a32.cpp`, and `interface/{A32,A64}/config.h` (dispatcher test ownership)

### Intentional differences

- Rust retains native instruction-word and relocation tests beside the corresponding shared and
  A32 dispatcher implementations; their fallible code-buffer API does not change the upstream
  emission ordering under test.

### Missing items

- The mixed-architecture test environment in `jit.rs` remains the final legacy merged-configuration
  consumer before the compatibility layer can be removed.

## 2026-08-24 — `src/rdynarmic/src/jit.rs` vs Eden `interface/A32/{a32,config}.h` and `backend/{x64,arm64}/a32_interface.cpp` (A32 test configuration ownership)

### Intentional differences

- Rust keeps its cross-backend native regression tests in the public JIT wrapper while Eden's
  backend interfaces are separate translation units. The fixtures now expose the A32-owned
  configuration and callback boundary directly despite that existing harness placement.
- The Rust JIT constructors remain fallible because executable-memory allocation and code
  generation report errors instead of relying on C++ assertions.

### Missing items

- The A64 fixtures in the same Rust-native test module still construct the merged compatibility
  configuration. The common mock behavior still delegates through its legacy callback
  implementation until those A64 fixtures are migrated and both architecture traits can call
  architecture-neutral test-memory helpers directly.

## 2026-08-24 — `src/rdynarmic/src/{jit.rs,lib.rs}` and removed `jit_config.rs` vs Eden `interface/{A32,A64}/{a32,a64,config}.h` and `backend/{x64,arm64}/{a32,a64}_interface.cpp`

### Intentional differences

- Rust exposes fallible JIT constructors and boxes callback traits to represent C++ virtual
  callback ownership. The constructors now otherwise take the matching architecture `UserConfig`
  by value, as Eden does.
- Rust-native A32 and A64 integration regressions share memory-storage helper methods inside the
  test module; the two public callback implementations retain their distinct upstream signatures.

### Missing items

- `jit.rs` remains a combined Rust wrapper for both guest architectures rather than mirroring
  Eden's backend-specific A32/A64 interface translation units. Splitting that established wrapper
  is a separate structural ownership slice because it also owns host callback trampolines and
  cache lifecycle.

## 2026-08-24 — `src/rdynarmic/src/common/spin_lock.rs` vs Eden `common/spin_lock.h` and `common/spin_lock_{x64,arm64}.cpp`

### Intentional differences

- Rust uses a four-byte `AtomicU32` rather than lazily generating host routines for ordinary
  `SpinLock::lock` and `unlock`; acquire/release behavior and the x64 `xchg`/`mfence` strength are
  retained without allocating an executable helper page.

### Missing items

- The AArch64 JIT-emitted `EmitSpinLockLock` and `EmitSpinLockUnlock` helpers remain part of the
  broader arm64 exclusive-fastmem backend parity work.

## 2026-08-24 — `src/rdynarmic/src/common/spin_lock_x64.rs` vs Eden `common/spin_lock_x64.{h,cpp}`

### Intentional differences

- Rust emits through `rxbyak::CodeAssembler` and uses its native `umonitor` encoder instead of
  Eden's hand-written workaround for the historical Xbyak encoding bug.

## 2026-08-24 — `src/rdynarmic/src/interface/code_page.rs` vs Eden `interface/code_page.h`

### Intentional differences

- Rust expresses the public instruction array length with a constant expression over its native
  `u32` size.

## 2026-08-24 — `src/rdynarmic/src/interface/halt_reason.rs` vs Eden `interface/halt_reason.h`

### Intentional differences

- Rust uses `bitflags` for Eden's operators and retains named aliases mapping Ruzu core events onto
  the corresponding upstream `UserDefined` bits.

## 2026-08-24 — `src/rdynarmic/src/interface/exclusive_monitor.rs` vs Eden `interface/exclusive_monitor.h` and host `exclusive_monitor.cpp` implementations

### Intentional differences

- Rust stores the fixed-at-construction address/value sequences in non-resizing `Vec`s rather than
  Boost `static_vector`; the four-entry capacity is enforced and the host pointers remain stable.
- `Copy` is Rust's bound for the trivially-copyable template payload, and `MaybeUninit` represents
  Eden's uninitialized local before the exact-size `memcpy`.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/exclusive_monitor_friend.rs` vs Eden `backend/x64/exclusive_monitor_friend.h`

### Intentional differences

- Rust exposes the four friend operations as `unsafe` crate-local functions because raw-pointer
  validity, index bounds, and stable monitor ownership are caller contracts.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/{emit_context.rs,emit_vector_helpers.rs,emit_vector_saturated.rs}` vs Eden `backend/x64/{emit_x64_vector.cpp,jitstate_info.h}` (SQSHLU/VQSHLU immediate fallback)

### Intentional differences

- Rust passes `ArchConfig` through `EmitContext` to select the A32/A64 `fpsr_qc` offset; Eden
  obtains the same architecture-specific offset from `BlockOfCode::GetJitStateInfo()`.
- Rust implements the element loop with fixed-size arrays instead of Eden's `VectorArray<T>`
  template. The signed input, unsigned result, saturation result and sticky QC behavior match.

### Missing items

- Eden's AVX2-specialized 32-bit SQSHLU emitter is not ported; Rust uses the behaviorally
  equivalent corrected scalar fallback for 8-, 16-, 32- and 64-bit lanes.

## 2026-08-24 — `src/rdynarmic/{build.rs,a64_decoder_parser.rs}` vs Eden `frontend/A64/decoder/{a64.h,a64.inc}` (closing `INST` delimiter)

### Intentional differences

- Eden expands `a64.inc` with the C++ preprocessor. Rust's build script must parse the same three
  macro fields to generate its decoder, so the parser is isolated in a build-support module that
  can also be compiled by the regression test.

## 2026-08-24 — `src/rdynarmic/src/backend/block_range_information.rs` vs Eden `backend/block_range_information.{h,cpp}`

### Intentional differences

- Rust stores one closed range and descriptor per registration in a `Vec`, while Eden's Boost
  interval map splits/coalesces overlapping intervals and stores descriptor sets. Iterating every
  registered interval produces the same union of descriptors for invalidation without adding a
  nonstandard interval-map dependency.
- Rust accepts a slice of closed ranges in place of Boost's `interval_set`; callers construct the
  same closed invalidation intervals at their architecture boundary.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/a32_address_space.rs` vs Eden `backend/arm64/a32_address_space.{h,cpp}` (block ranges)

### Intentional differences

- Rust forwards a `HashSet<LocationDescriptor>` to its address-space invalidator rather than
  Eden's `ankerl::unordered_dense::set`; both represent the same unique descriptor set.

## 2026-08-24 — `src/rdynarmic/src/backend/arm64/a64_address_space.rs` vs Eden `backend/arm64/a64_address_space.{h,cpp}` (block ranges)

### Intentional differences

- Rust forwards a `HashSet<LocationDescriptor>` to its address-space invalidator rather than
  Eden's `ankerl::unordered_dense::set`; both represent the same unique descriptor set.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/a32_emit_x64.rs` vs Eden `backend/x64/a32_emit_x64.{h,cpp}` (block ranges)

### Intentional differences

- The Rust public wrapper still supplies one start/length pair, which this owner converts to the
  same closed `u32` interval that Eden's interface queues in its Boost interval set.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/a64_emit_x64.rs` vs Eden `backend/x64/a64_emit_x64.{h,cpp}` (block ranges)

### Intentional differences

- The Rust public wrapper still supplies one start/length pair, which this owner converts to the
  same closed `u64` interval that Eden's interface queues in its Boost interval set.

## 2026-08-24 — `src/rdynarmic/src/common/mod.rs` vs Eden `common/spin_lock_x64.{h,cpp}`

### Intentional differences

- Eden only builds its x64 backend on x64 hosts. Rust currently compiles the x64 code-generator
  modules on ARM64 as well, so the architecture-independent `rxbyak` emission helper must remain
  visible there even though the generated instructions are x64 instructions.

## 2026-08-24 — `src/rdynarmic/src/common/llvm_disassemble.rs` vs Eden `common/llvm_disassemble.{h,cpp}`

### Intentional differences

- Eden conditionally uses LLVM when `DYNARMIC_USE_LLVM` is enabled. That option defaults to OFF,
  and rdynarmic currently has no LLVM integration, so Rust ports the exact non-LLVM branch for all
  three helpers rather than adding a differently formatted disassembler dependency.
- Rust accepts typed instruction pointers instead of Eden's `void*`; the fallback only formats
  their numeric addresses and never dereferences them.

### Missing items

- LLVM-enabled x64, AArch32, and AArch64 instruction decoding is not available until rdynarmic
  gains an explicit equivalent of Eden's optional `DYNARMIC_USE_LLVM` build mode.

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

- The diagnostic block-map/state-pointer and compile-only extensions still live in this upstream
  owner. They must move behind an explicit Ruzu extension boundary in a dedicated follow-up.
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

- Fixed in the x64 JIT-state ownership slice: TPIDR accessors that only mirrored Core-owned
  backing storage were removed from this upstream owner.

## 2026-08-24 — `src/rdynarmic/src/interface/a32/a32.rs` vs Eden `interface/A32/a32.h`

### Intentional differences

- Rust selects the x64 or ARM64 implementation with target `cfg` blocks instead of a link-selected
  C++ `Impl`, but the public `Jit` remains the owner of the backend object and `is_executing`.
- `read_halt_reason`, raw state-pointer access, individual register helpers,
  compile-only, and block-map dumping are Ruzu diagnostic/tool extensions beyond Eden's public
  interface. They delegate to host backends and do not replace an upstream method.

### Missing items

- The diagnostic/tool methods still need a separate explicit extension trait or module before the
  upstream public owner is structurally exact.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/a32_interface.rs` vs Eden `backend/x64/a32_interface.cpp`

### Intentional differences

- Rust's fixed-size executable allocation is committed when `BlockOfCode` is created, so there is
  no separate `EnsureMemoryCommitted` operation after the one-megabyte capacity check.
- The emitter retains the same one-megabyte check as a defensive guard for direct emitter tests
  and tools. Production Run, Step, dispatcher lookup, and compile-only paths reach the interface
  check first.
- W^X transitions surround slow-path compilation explicitly; Eden performs them through its code
  emission machinery. Callback trampolines and diagnostic hooks are Rust ABI/adaptation code.

### Missing items

- Diagnostic state-pointer, compile-only, trace, and block-map facilities remain mixed into
  this upstream-owned backend file pending an explicit Ruzu extension boundary.

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

### Missing items

- Fixed in the x64 JIT-state ownership slice: TPIDR values are no longer mirrored through backend
  accessors or raw-state fields. Diagnostic raw-pointer/trace facilities remain mixed into this
  upstream-owned backend file pending an explicit Ruzu extension boundary.

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

## 2026-08-24 — `src/rdynarmic/src/backend/x64/a32_jitstate.rs` vs Eden `backend/x64/a32_jitstate.{h,cpp}`

### Intentional differences

- Rust spells the fields and methods with Rust naming conventions and uses explicit zeroed padding
  before `ext_reg`; the padding reproduces Eden's implicit `alignas(16)` gap and makes reserved
  bytes deterministic.
- Compile-time `offset_of!` helpers expose the same offsets to the Rust x64 emitter that C++ obtains
  with `offsetof`.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/a64_jitstate.rs` vs Eden `backend/x64/a64_jitstate.{h,cpp}`

### Intentional differences

- Rust uses explicit zeroed padding after `exclusive_state` to reproduce the C++ compiler's padding
  before `rsb_ptr`, and compile-time offset helpers stand in for C++ `offsetof` calls.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/emit_a64.rs` vs Eden `backend/x64/a64_emit_x64.cpp` (TPIDR)

### Intentional differences

- Rust represents nullable TPIDR pointers as `Option<*mut u64>`/`Option<*const u64>` and materializes
  their addresses through rxbyak's Rust API; the emitted load/store sequence is otherwise the same.

## 2026-08-24 — `src/rdynarmic/src/interface/{a32/a32,a64/a64}.rs` and host interfaces vs Eden `interface/{A32/a32,A64/a64}.h` and `backend/{x64,arm64}/*_interface.cpp`

### Intentional differences

- When no global monitor is configured, Ruzu's safe callback fallback retains expected exclusive
  values in the host interface owner. These values are not visible to generated code and are reset
  with the architectural state.

### Missing items

- Ruzu diagnostic state pointers, halt inspection, compile-only, trace, and block-map helpers still
  require a separate extension boundary to make the public/upstream interface owners exact.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/jitstate_info.rs` vs Eden `backend/x64/jitstate_info.h`

### Intentional differences

- Rust uses explicit `from_a32` and `from_a64` constant constructors because it cannot express
  Eden's templated constructor over arbitrary standard-layout JIT-state types.
- `EmitContext` carries a value copy supplied by `BlockOfCode` because Rust emission temporarily
  borrows the assembler separately from its owner. The copied ten-field inventory is immutable for
  the block and is the counterpart of calling Eden's `BlockOfCode::GetJitStateInfo()`.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/emit_saturation.rs` vs Eden `backend/x64/emit_x64_saturation.cpp`

### Intentional differences

- Runtime `SaturationOp` and bit-width matches replace Eden's template instantiations while keeping
  the same shared signed/unsigned helper boundaries and emitted operation ordering.
- The mechanical `emit_or_qc` helper centralizes Eden's repeated byte-sized QC update without
  moving its ownership outside this file.
- For the signed-saturation `N == 32` pseudo-result, Rust emits a zero value because its emission
  context holds an immutable IR block; Eden replaces uses with an immediate false during emission.
  Both paths expose the same value to generated code.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/a32_emit_a32.rs` vs Eden `backend/x64/a32_emit_x64.cpp` (`EmitA32OrQFlag`)

### Intentional differences

- Rust uses `ArgumentInfo` and rxbyak's Rust register conversions, preserving Eden's immediate and
  register branches without changing method ownership.

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

## 2026-08-24 — `src/rdynarmic/src/backend/x64/{a32_emit_x64_memory,a32_interface}.rs` vs Eden `backend/x64/{a32_emit_x64_memory.cpp,emit_x64_memory.cpp.inc}`

### Intentional differences

- Rust host trampolines recover `A32JitInner` in `a32_interface.rs`; the exclusive-monitor
  operations corresponding to Eden's generated lambdas live beside the generated-code owner in
  `a32_emit_x64_memory.rs`.
- The shared callback container retains an unused clear-exclusive slot for the existing Rust FFI
  boundary, while A32 generated code clears `A32JitState::exclusive_state` directly like Eden.

### Missing items

- No missing item remains in the reviewed A32 scalar non-inline exclusive path.

## 2026-08-24 — `src/rdynarmic/src/backend/x64/constants.rs` vs Eden `backend/x64/constants.h`

### Intentional differences

- Eden's `Cmp`, `CmpInt`, `Tern`, and `FpClass` namespaces are Rust modules, with constant and enum
  spellings changed only to Rust naming conventions.
- Rust has no default function arguments, so `fixup_lut` requires all eight `FpFixup` operands;
  callers that omit trailing operands upstream pass `FpFixup::Dest` explicitly.
- `convert_rounding_mode_to_x64_immediate` retains Eden's `Option<i32>` result. Consumers cast the
  proven two-bit value to `u8` only at rxbyak's more strongly typed instruction boundary.

### Missing items

- No item is missing from the reviewed constants owner; AVX-512 consumers that use some currently
  unreferenced constants remain part of their respective emitter-file audits.

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

## 2026-08-24 — `src/rdynarmic/src/backend/x64/{block_of_code,a32_emit_x64,a64_emit_x64}.rs` vs Eden `backend/x64/{block_of_code,a32_emit_x64,a64_emit_x64}.{h,cpp}` (prelude lifecycle)

### Intentional differences

- Rust's `gen_run_code` returns byte-offset dispatcher labels to the owning emitter, whereas Eden's
  `BlockOfCode` stores native function pointers. Both now leave the prelude open until the
  architecture emitter explicitly completes it.
- `rxbyak` reserves the complete executable buffer up front, so there is no Linux operation
  corresponding to Eden's Windows-only incremental `EnsureMemoryCommitted` implementation.

### Missing items

- Resolved by the 2026-08-24 A64 memory-prelude follow-up below.

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

## 2026-08-24 — `src/rdynarmic/src/tests_a32_fuzz.rs` and `tools/a32_oracle.cpp` vs Eden A32 JIT differential-test behavior

### Intentional differences

- Test-only runners retain one Rust JIT and one Eden oracle process per test thread. Between cases
  they reset CPU state, clear the complete code cache, replace code memory, clear data memory,
  restore the 200-tick budget, and use the same optimization configuration as the former
  one-process/one-JIT-per-case path; the first case starts from the equivalent fresh state.
- The local oracle adds a `BATCH` protocol around Eden's public A32 JIT interface. This is tooling
  only; the one-shot and existing `INIT` protocols remain compatible.

### Unintentional differences (to fix)

- Fixed during verification: calling `ClearCache` before `Reset` lost Eden's pending
  `CacheInvalidation` halt bit because `Reset` zeroes the JIT state. Both runners now perform
  `Reset` before `ClearCache`, so the following `Run` consumes the invalidation exactly as Eden's
  A32 interface requires.
### Missing items

- An older externally configured oracle falls
  back to the original one-shot protocol, and a failed batch session also falls back safely.

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

### Missing items

- Commands 3 and 4 remain null entries on both
  sides.

## 2026-08-24 — `src/core/src/hle/service/bcat/delivery_cache_storage_service.rs` vs Eden `src/core/hle/service/bcat/delivery_cache_storage_service.{h,cpp}` (`EnumerateDeliveryCacheDirectory`)

### Intentional differences

- Rust protects `entries` and `next_read_index` with mutexes because service callbacks receive a
  shared reference. Both values remain owned together by `IDeliveryCacheStorageService`, and the
  lock scope preserves Eden's count, copy, then index-advance ordering.

## 2026-08-24 — `src/video_core/src/texture_cache/image_view_info.rs` vs Eden `src/video_core/texture_cache/image_view_info.{h,cpp}`

### Intentional differences

- Rust expresses Eden's mutable local `TextureType` switch as an enum match. The promotion rules and
  subsequent view-type switch remain in the same file and order.

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

## 2026-08-24 — `src/shader_recompiler/src/frontend/translate/load_store_local_shared.rs` vs Eden `src/shader_recompiler/frontend/maxwell/translate/impl/load_store_local_shared.cpp`

### Intentional differences

- Rust uses the existing `IR::Reg` counterpart and converts its index only at the legacy
  `TranslatorVisitor::x`/`set_x` boundary. Register ownership, alignment checks, and arithmetic stay
  in the translating module as in Eden.

## 2026-08-24 — `src/shader_recompiler/src/frontend/{decode.rs,translate/mod.rs}` vs Eden `src/shader_recompiler/frontend/maxwell/{decode.cpp,translate/translate.cpp}`

### Intentional differences

- Rust keeps the generated mask-table lookup returning `Option`, while the upstream-owned
  `decode.rs::decode` wrapper converts an unmatched word to `NOP`. This preserves Eden's public
  `Decode` contract without rebuilding the generated table API.
- Eden's failed soft assertion is represented by an error log because Ruzu has no process-wide
  `AssertFailSoftImpl` setting in this crate.

### Missing items

- Other CFG/branch-tracking callers still consume the optional low-level decoder directly. Their
  current unmatched-word behavior is equivalent to NOP for control-flow analysis, but they do not
  emit Eden's soft diagnostic.

## 2026-08-24 — `src/shader_recompiler/src/frontend/{location.rs,control_flow.rs}` vs Eden `src/shader_recompiler/frontend/maxwell/{location.h,control_flow.cpp}`

### Intentional differences

- Rust spells Eden's implicit `u32`-to-`Location` construction as explicit `Location::new` calls
  for absolute, relative, and indirect branch targets.

## 2026-08-24 — `src/shader_recompiler/src/pipeline_cache.rs` translation driver vs Eden `src/shader_recompiler/frontend/maxwell/{translate/translate.cpp,translate_program.cpp}`

### Intentional differences

- Rust materializes structured actions from a cached instruction slice and flat word indices.
  Eden iterates absolute `Location` values and reads each instruction through `Environment`.
  The Rust slice driver therefore converts each word index back to its absolute byte offset before
  applying Eden's scheduling-word rule.

### Missing items

- The compatibility driver still represents instruction ranges as slice-local word indices rather
  than retaining Eden's absolute `Location` values through structured translation.

## 2026-08-24 — `src/video_core/src/buffer_cache/buffer_cache.rs` vs Eden `src/video_core/buffer_cache/buffer_cache.h`

### Intentional differences

- Rust collects tracker callbacks before mutating `gpu_modified_ranges` to satisfy exclusive
  borrowing, then performs Eden's range construction and clearing in the same order.
- Device memory is optional during Ruzu's cache setup lifecycle, so the final writes are guarded
  until the memory manager has been attached.

## 2026-08-24 — `src/video_core/src/engines/maxwell_3d.rs::hle_bind_shader` vs Eden `src/video_core/macro.cpp::HLE_BindShader::Execute`

### Intentional differences

- Rust addresses the flattened Maxwell register array through the corresponding register constants;
  Eden accesses the typed `Regs` members directly.

## 2026-08-24 — `src/shader_recompiler/src/frontend/translate/{mod.rs,load_store_local_shared.rs}` and `src/shader_recompiler/src/pipeline_cache.rs` vs Eden `src/shader_recompiler/frontend/maxwell/{translate/impl/impl.h,translate/impl/load_store_local_shared.cpp,translate/translate.cpp,translate_program.cpp}`

### Intentional differences

- Runtime translation now retains a shared Rust reference to `Environment`, corresponding to
  Eden's `TranslatorVisitor::env`. Reduced instruction-level fixtures may still construct a
  visitor without an environment and use their explicit program-header/program metadata.
- Rust retains its cached instruction slice while materializing structured actions; Eden reads
  each instruction through `Environment`. The active environment is nevertheless passed to every
  runtime `TranslatorVisitor`, preserving the state ownership required by instruction handlers.

## 2026-08-25 — `src/video_core/src/renderer_vulkan/present_manager.rs` vs Eden `src/video_core/renderer_vulkan/vk_present_manager.{h,cpp}`

### Intentional differences

- Rust exposes Eden's function-local two-element wait-stage constant through a private helper so
  the production synchronization contract can be covered by a focused unit test.
- Rust owns Vulkan images and memory explicitly rather than through Eden's memory-allocator
  wrappers; this does not change the verified barrier or semaphore ordering.

### Missing items

- Eden's optional storage-image presentation path and frame-generation integration are not ported;
  they are independent of the verified ordinary composite-to-swapchain synchronization path.

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

## 2026-08-25 — `src/video_core/src/{shader_environment,shader_cache}.rs` vs Eden `shader_environment.{h,cpp}` and `shader_cache.{h,cpp}`

### Intentional differences

- `GenericEnvironmentOwner` represents C++ base-subobject access without erasing the concrete
  graphics or compute environment required by virtual resource callbacks.

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
- Android's configurable pipeline-worker count is not applicable to the currently supported
  desktop targets; non-Android worker-count selection matches Eden.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/pipeline_cache.rs` vs Eden `src/video_core/renderer_vulkan/vk_pipeline_cache.{h,cpp}`

### Intentional differences

- Rust centralizes Eden's duplicated graphics/compute shader-name and shader-info formatting in
  two file-local helpers so the exact payload contract is directly testable. Both hooks remain in
  the matching pipeline-cache owner.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/graphics_pipeline.rs` vs Eden `src/video_core/renderer_vulkan/vk_graphics_pipeline.{h,cpp}`

### Intentional differences

- Rust splits construction from synchronous or worker-backed Vulkan pipeline completion so the
  worker owns an immutable snapshot instead of capturing a movable `this`. The shader build
  notification still begins at graphics-pipeline construction and completes from the selected
  build path.
- Scheduler pipeline identity uses the stable Rust `GraphicsPipeline` address through a raw
  identity handle; this is the direct counterpart of Eden tracking `GraphicsPipeline*`.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/graphics_pipeline.rs` vs Eden `src/video_core/renderer_vulkan/vk_graphics_pipeline.{h,cpp}`

### Intentional differences

- Rust moves Eden's two inline `fmt::format` expressions into file-local formatting helpers so
  their exact payloads can be covered without constructing Vulkan objects. The helpers remain in
  the matching pipeline owner file; logger access uses the process-wide singleton functions.

## 2026-08-26 — `src/video_core/src/host1x/{host1x,nvdec}.rs` vs Eden `src/video_core/host1x/host1x.{h,cpp}` and `nvdec.cpp`

### Intentional differences

- Rust stores active Host1x devices in a `HashMap` of `CDmaPusher` owners instead of Eden's fixed
  array of `Nvdec`/`Vic` variants. Each pusher owns the same concrete processor and dropping an
  entry performs the corresponding device destructor lifecycle.
- `Arc<Frame>` and a mutex-protected Rust `HashMap`/`VecDeque` replace `shared_ptr<Frame>` and the
  mutex-protected C++ containers while preserving the per-FD `FrameDevice` boundary.

## 2026-08-26 — `src/video_core/src/host1x/codecs/h264.rs` vs Eden `src/video_core/host1x/codecs/h264.{h,cpp}` and `codec_types.h`

### Intentional differences

- The Rust decoder trait returns an owned `Vec<u8>` from `compose_frame`; Eden returns a span into
  `frame_scratch`. This avoids retaining a borrow into the decoder while the shared decode path
  queries offsets and mutably drives `DecodeApi`; the generated byte sequence is identical.
- Guest-memory reads use the Rust `MemoryManager` slice API. Its boolean result is currently
  unconditionally true, while Eden's corresponding operation returns `void` and zero-fills
  unmapped pages.
- Eden's unused `scan_scratch` member is omitted; both implementations use the same immutable
  4x4 and 8x8 zig-zag orders directly.

## 2026-08-25 — `src/video_core/src/query_cache/query_stream.rs` vs Eden `src/video_core/query_cache/query_stream.h`

### Intentional differences

- Rust represents Eden's virtual base class as `StreamerInterfaceBase` plus the
  `StreamerInterface` trait; both dependency masks remain owned by that base state.

## 2026-08-25 — `src/video_core/src/engines/puller.rs` vs Eden `src/video_core/engines/puller.{h,cpp}`

### Intentional differences

- Rust passes `true` to `RasterizerInterface::release_fences` because the Rust interface exposes
  the force argument explicitly; Eden's puller calls its argument-less wrapper.
- Raw engine identifiers are represented by the transparent `EngineID` newtype so unsupported
  values retain Eden's `static_cast<EngineID>` bit pattern.

## 2026-08-25 — `src/video_core/src/engines/engine_interface.rs` vs Eden `src/video_core/engines/engine_interface.h`

### Intentional differences

- Rust extracts the inherited fields into `EngineInterfaceState` and exposes
  `has_pending_methods` to preserve Eden's guarded `ConsumeSink` behavior across trait objects.
- `EngineHandle` retains Eden's non-owning engine-pointer semantics for Rust fat pointers.

## 2026-08-25 — `src/video_core/src/engines/nv01_timer.rs` vs Eden `src/video_core/engines/nv01_timer.h`

### Intentional differences

- The ignored `MemoryManager&` constructor argument is accepted as an `Arc<Mutex<MemoryManager>>`
  to match the existing Rust engine construction boundary; neither implementation stores it.
- Inherited `EngineInterface` fields live in `EngineInterfaceState` because Rust has no field
  inheritance.

### Unintentional differences (to fix)

- Single and multi-method calls only log their arguments, and `consume_sink_impl` remains an
  intentional no-op exactly like Eden.

## 2026-08-25 — `src/video_core/src/control/channel_state.rs` vs Eden `src/video_core/control/channel_state.{h,cpp}`

### Intentional differences

- Eden's optional `Payload` is represented by individually boxed optional engines and a boxed DMA
  pusher so their addresses remain stable for non-owning engine handles.
- Maxwell3D guest-memory and tick callbacks are Rust adapters required by the flattened owner.

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

## 2026-08-25 — `src/common/src/thread.rs` vs Eden `src/common/thread.{h,cpp}` (`SetCurrentThreadPriority` prerequisite)

### Intentional differences

- Rust reports a failed Linux/Android `setpriority` call through `std::io::Error`; Eden formats the
  same operating-system error through `GetLastErrorMsg`.
- Unsupported non-Unix/non-Windows targets retain a no-op fallback. Eden has a dedicated Haiku
  priority mapping which is not a supported Ruzu build target.

### Missing items

- Android's topology-policy registration (`RememberCurrentThreadNice`) and the related
  performance/efficiency-core policy subsystem are not ported. They are outside the Linux worker
  priority prerequisite and depend on Eden's Android-only topology/ADPF infrastructure.
- Pre-existing thread-name, Event, Barrier, and topology-policy differences are outside this
  focused prerequisite review.

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
- `Scheduler::new` propagates `MasterSemaphore` and initial command-pool construction failures as
  `vk::Result`; Eden propagates the equivalent Vulkan wrapper exceptions from its constructor.

### Missing items

- Resolved by the 2026-08-26 GPU-logging scheduler entry below: render-pass begin/end and successful
  queue submissions now reach the ported logger.
- Android performance-core placement remains part of the unported topology/ADPF prerequisite
  recorded in the `common/thread.rs` entry above. It is a no-op in Eden on Linux, Windows and macOS.

## 2026-08-25 — `src/video_core/src/renderer_vulkan/graphics_pipeline.rs` vs Eden `src/video_core/renderer_vulkan/vk_graphics_pipeline.{h,cpp}` (`UsesExtendedDynamicState` prerequisite)

### Intentional differences

- The recorded bind closure loads the eventual Vulkan pipeline handle from Rust's shared async
  build cell after its build wait. Eden captures `this` and reads its `vk::Pipeline` member at the
  same execution point.

### Missing items

- Broader `graphics_pipeline.rs` parity findings are handled by its dedicated
  `bugs/eden-parity/graphics_pipeline.md` review rather than this scheduler prerequisite.

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

### Missing items

- The focused unit test verifies file-owned constants. Timeline and fence submissions require a
  real Vulkan device and remain covered only by renderer integration/runtime validation.

## 2026-08-25 — `src/video_core/src/renderer_vulkan/resource_pool.rs` vs Eden `src/video_core/renderer_vulkan/vk_resource_pool.{h,cpp}`

### Intentional differences

- `MasterSemaphore&` is retained as `Arc<MasterSemaphore>` so cloned Rust descriptor allocators can
  safely share the scheduler-owned timeline without a self-referential lifetime.
- `try_commit_resource` is the `Result` counterpart of Eden's exception-propagating
  `CommitResource`; its fallible grow path preserves Eden's resize-before-`Allocate` ordering.

## 2026-08-25 — `src/video_core/src/renderer_vulkan/descriptor_pool.rs` and descriptor-commit call sites vs Eden `src/video_core/renderer_vulkan/vk_descriptor_pool.{h,cpp}` (`ResourcePool` prerequisite)

### Intentional differences

- `ResourcePool` retains an `Arc` to the scheduler's `MasterSemaphore` instead of Eden's raw
  pointer. Each `DescriptorPool::allocator*` overload receives the scheduler explicitly and clones
  that same semaphore into the newly returned allocator.
- Vulkan allocation failures use `Result<_, vk::Result>` instead of C++ exceptions.

### Missing items

- Broader descriptor-pool findings remain owned by its dedicated `bugs/eden-parity` report.

## 2026-08-25 — `src/video_core/src/renderer_vulkan/command_pool.rs` vs Eden `src/video_core/renderer_vulkan/vk_command_pool.{h,cpp}`

### Intentional differences

- Eden's `vk::CommandPool` and `vk::CommandBuffers` wrappers are represented by an ash handle and
  `Vec<vk::CommandBuffer>`; `Drop` explicitly destroys every successfully created pool.
- `CommandPool::commit` returns `Result` so Vulkan wrapper exceptions propagate through
  `Scheduler::new`; failures during worker rotation remain fatal at the scheduler worker boundary.

### Unintentional differences (to fix)

- Fixed partial-allocation lifetime: an empty pool entry is now published before Vulkan creation,
  matching Eden's `pools.emplace_back()`, so a pool handle is retained and destroyed if command
  buffer allocation fails.
- Fixed error propagation: pool creation and command-buffer allocation no longer panic through
  `expect`; they return the original `vk::Result` like Eden's wrapper exception.

## 2026-08-25 — `src/video_core/src/renderer_vulkan/descriptor_pool.rs` vs Eden `src/video_core/renderer_vulkan/vk_descriptor_pool.{h,cpp}`

### Intentional differences

- Descriptor banks use `Box` for Eden's `unique_ptr` address stability and allocators retain a
  non-owning `NonNull` pointer. Rust adds mutexes around the bank and allocator state so deferred
  `Send + 'static` scheduler commands can perform Eden's captured-`this` operations safely; the
  read-search/write-insert critical sections and lack of a second search after lock promotion
  remain identical.
- Vulkan wrappers are raw ash handles owned and destroyed by `DescriptorPool::drop`; allocation
  failures propagate as `vk::Result` rather than exceptions.

### Unintentional differences (to fix)

- Restored the file-owned `accumulate` and `make_bank_info` helpers and the single-`ShaderInfo`
  allocator overload instead of flattening their logic into `allocator_for_infos`.
- Restored `DescriptorAllocator::allocate` as the owner of set-vector growth.
- Fixed bank publication order: `bank_infos` and `banks` are both extended before `allocate_pool`,
  matching Eden and preserving their index invariant even when Vulkan creation fails.
- Descriptor count accumulation and pool-size multiplication now retain Eden's unsigned wrapping
  bit patterns in checked Rust builds.
- Removed the Rust-only successful-bank debug log; Eden's helper returns without emitting a log.
- Restored Eden's explicit `Device` and `Scheduler` parameters on all three allocator overloads;
  `DescriptorPool` no longer owns the master semaphore.
- Restored move-only allocator ownership and `Box`-stable banks instead of sharing both through
  `Arc` clones.

## 2026-08-25 — `src/video_core/src/delayed_destruction_ring.rs` vs Eden `src/video_core/delayed_destruction_ring.h`

### Intentional differences

- `new`/`Default` explicitly construct the array through `std::array::from_fn`; Eden obtains the
  same zero-indexed empty vectors from C++ default member construction.
- Rust consumes `T` in `push`, matching Eden's rvalue-reference plus `std::move` contract.

### Unintentional differences (to fix)

- Replaced the heap-allocated outer `Vec<Vec<T>>` with `[Vec<T>; TICKS_TO_DESTROY]`, directly
  matching Eden's `std::array<std::vector<T>, TICKS_TO_DESTROY>` storage and allocation behavior.
- Restored conditional copy support with `Clone`, corresponding to the implicitly copyable C++
  template whenever `T` itself is copyable.

## 2026-08-25 — `src/video_core/src/fence_manager.rs` vs Eden `src/video_core/fence_manager.h`

### Intentional differences

- Rust represents Eden's derived fence-manager virtual methods with call-site closures. The async
  release thread therefore stores `PopAsyncFlushes` as a pre-operation and obtains host waiting
  through `FenceBase::wait_for_fence`; both execute at the same points as Eden's virtual calls.
- The fence queue, pending operations, and uncommitted operations share an `Arc<Mutex<_>>` so the
  worker can own them safely. The async signal path keeps this mutex locked for Eden's complete
  `guard.lock()` interval, from moving uncommitted operations through queuing, publication, and
  the optional command flush.

### Unintentional differences (to fix)

- Fixed the async guard interval: no worker can observe or consume a fence between extraction of
  its operations and publication of the matching queue entry.
- Fixed synchronous release ordering: pending operations are moved and run while the fence stays
  at the queue front; the fence is removed only after the operations complete.
- Removed the Rust-only boolean return values from `signal_reference`, `signal_fence`, and
  `signal_sync_point`; Eden's methods return `void` and no caller consumed those values.

### Missing items

- Eden calls `SetCurrentThreadToPerformanceCores` in the async worker. It is a no-op on the current
  Linux target, while its Android behavior depends on the still-unported topology/ADPF subsystem
  already recorded in the `common/thread.rs` audit entry.

## 2026-08-26 — `src/video_core/src/surface.rs` vs Eden `src/video_core/surface.{h,cpp}`

### Intentional differences

- Rust expresses Eden's macro-generated `DefaultBlockWidth`, `DefaultBlockHeight`, and
  `BitsPerBlock` switches as three format-indexed arrays in the same file. An exhaustive audit
  verifies all 112 names, positions, widths, heights, and bit sizes against
  `PIXEL_FORMAT_LIST`.
- The three guest-format conversion functions accept raw `u32` values. This preserves Eden's
  ability to receive an out-of-range enum bit pattern without constructing an invalid Rust enum.
- Eden's fail-soft assertion optionally executes a debugger trap; Rust logs the same failure and
  panics when `use_debug_asserts` is enabled because a portable Rust debugger trap is unavailable.
- Eden's duplicate `PixelFormat` sentinel aliases are module constants in Rust because Rust enums
  reject duplicate discriminants. They remain owned by `surface.rs` and have the same values.
- `IsViewCompatible` and `IsCopyCompatible` remain in `compatible_formats.rs`, the counterpart of
  Eden's owning `compatible_formats.cpp`; call sites address that owner directly because Rust
  modules do not separately reproduce Eden's enclosing `VideoCore::Surface` namespace.

## 2026-08-26 — `src/video_core/src/host_shaders/blit_{color,depth,depth_stencil}_msaa.frag` vs Eden `src/video_core/host_shaders/blit_{color,depth,depth_stencil}_msaa.frag`

### Intentional differences

- Rust's `build.rs` generates the SPIR-V word slices consumed by `ash`; Eden's CMake rules generate
  C++ headers. Both invoke `glslangValidator` for the same GLSL source files and SPIR-V 1.3 target.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/blit_image.rs` vs Eden `src/video_core/renderer_vulkan/blit_image.{h,cpp}`

### Intentional differences

- `BlitImageView` and `BlitFramebufferInfo` are copyable snapshots of the exact `ImageView` and
  `Framebuffer` fields captured by Eden's deferred scheduler lambdas. This keeps the Rust closures
  owned and `'static` without moving image or framebuffer owners.
- Vulkan wrapper exceptions are represented by `Result`/boolean failure propagation. Raw ash
  handles are destroyed explicitly in `Drop`, in the reverse dependency order supplied by Eden's
  RAII members.
- Eden's fail-soft assertions optionally trap a debugger. Rust logs the same condition and panics
  when `use_debug_asserts` is enabled.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/texture_cache.rs` blit integration vs Eden `src/video_core/renderer_vulkan/vk_texture_cache.{h,cpp}`

### Intentional differences

- Rust's existing scale-helper implementation separates color and depth/stencil bodies while both
  remain owned by `texture_cache.rs`; their selection, regions, sample scaling, framebuffer state,
  and blit calls now follow Eden's single `Image::BlitScaleHelper` method.
- Scheduler-facing calls pass `BlitImageView`/`BlitFramebufferInfo` snapshots rather than C++
  references, preserving the same handles, formats, ranges, extents, sample count, and stencil
  capability across deferred recording.

## 2026-08-26 — `src/video_core/src/texture_cache/accelerated_swizzle.rs` vs Eden `src/video_core/texture_cache/accelerated_swizzle.{h,cpp}`

### Intentional differences

- The implicit padding introduced by C++ `alignas(16)` members is represented by explicit,
  zero-initialized Rust fields so the full compute-shader payload is deterministic.
- `Common::AlignUpLog2` accepts the tile width through its Rust `u64` API and converts the result
  back to the upstream `u32` payload type.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/present/anti_alias_pass.rs` vs Eden `src/video_core/renderer_vulkan/present/anti_alias_pass.h`

### Intentional differences

- Rust uses a trait for Eden's abstract base class; the single virtual `Draw` contract and mutable
  image/view outputs remain identical.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/present/{fxaa,smaa}.rs` Draw integration vs Eden `src/video_core/renderer_vulkan/present/{fxaa,smaa}.{h,cpp}`

### Intentional differences

- The passes retain a cloned raw `ash::Device` for explicit destruction of Vulkan handles; Eden's
  wrapper members carry that destruction context through RAII.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/present/layer.rs` and `renderer_vulkan/blit_screen.rs` anti-alias wiring vs Eden `src/video_core/renderer_vulkan/present/layer.{h,cpp}` and `renderer_vulkan/vk_blit_screen.cpp`

### Intentional differences

- Vulkan wrapper handles are still represented by raw ash handles with explicit Rust destruction.

## 2026-08-26 — `src/video_core/src/textures/astc.rs` vs Eden `src/video_core/textures/astc.{h,cpp}`

### Intentional differences

- `IntegerEncodedValue` stores the mutually exclusive trit/quint payload in one Rust field rather
  than a C++ union. `IntegerEncodedVector` uses 256 inline `SmallVec` entries; valid blocks remain
  inline and are limited to at most 64 weight values and 32 color values before decoding.
- Borrow-checker adaptations return endpoint pairs and transferred signed values rather than
  mutating two aliased output references. The formulas and update order remain unchanged.
- `OutputBitStream` advances `bits_written`, making Eden's declared bit-capacity guard effective.
  Eden currently never increments that member; none of its callers reads it, and all valid writes
  fit in the same 128-bit endpoint buffer.
- Worker closures carry checked `Send` pointer wrappers because the shared Rust worker queue owns
  `'static` jobs. `decompress` waits after every depth slice, so the input/output borrows remain
  alive exactly as long as Eden's captured spans and no input copy is made.
- Rust skips undersized compressed input blocks and out-of-range output rows rather than allowing
  `span::subspan`/`memcpy` to access invalid memory. Valid texture buffers take the identical path.

## 2026-08-26 — `src/video_core/src/query_cache/bank_base.rs` vs Eden `src/video_core/query_cache/bank_base.h`

### Intentional differences

- C++ template constraints are represented by the local `BankLike` trait. A fallible builder
  returns `Result`, the Rust equivalent of an exception escaping Eden's `ReserveBank`.
- C++ default arguments for reference counts remain explicit arguments at Rust call sites.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/query_cache.rs` bank reservation integration vs Eden `src/video_core/renderer_vulkan/vk_query_cache.cpp`

### Intentional differences

- Vulkan construction failures propagate as `vk::Result`; Eden propagates wrapper exceptions.
  Samples banks remain `Arc`-owned as documented by the broader query-cache audit.

### Missing items

- Broader samples-streamer debt
  remains tracked in its existing query-cache audit entry.

## 2026-08-26 — `src/video_core/src/textures/bcn.rs` vs Eden `src/video_core/textures/bcn.{h,cpp}`

### Intentional differences

- Worker closures carry checked `Send` pointer wrappers because Rust's shared worker queue owns
  `'static` jobs. `compress_bcn` waits after every depth slice, so the captured input/output spans
  remain alive for exactly Eden's worker lifetime and each job writes a distinct compressed row.
- Rust validates the input and output span lengths before exposing their pointers to worker jobs;
  Eden relies on its callers to satisfy the same buffer-size precondition and would otherwise
  access outside the spans.
- The C `stb_dxt` shim fixes the mode to `STB_DXT_NORMAL`, preserving the two direct Eden calls
  without exposing the C++ implementation through Rust FFI.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/present_manager.rs` vs Eden `src/video_core/renderer_vulkan/vk_present_manager.{h,cpp}`

### Intentional differences

- Eden passes stable `Frame*` values through its presentation queue. Rust keeps each owning
  `Frame` in its unavailable pool slot and queues a copy of only the handles consumed by the
  present thread; the same index returns to the free queue after presentation.
- The present-thread state is held in an `Arc` context rather than borrowing the enclosing
  renderer. Swapchain image count and view format are cached atomically because the upstream
  renderer reads them concurrently without taking `PresentManager::swapchain_mutex`, whereas the
  Rust swapchain itself is protected by a mutex.
- Frame images use `AllocatedImage` ownership and a non-owning stable allocator pointer. Resource
  replacement explicitly destroys framebuffer and views before releasing the old image, avoiding
  a Vulkan-invalid dependency order that raw C++ wrapper assignment can transiently create.
- Android surface recreation and performance-core policy retain their platform branches, but the
  excluded Android JNI/ADPF implementation is not built. The current non-LSFG build takes Eden's
  `CanStoreToFrame == false` branch.

### Unintentional differences (to fix)

- Fixed both submit wait stages to `TRANSFER`, matching the copy/blit command buffer.
- Fixed frame creation flags and usage to include `MUTABLE_FORMAT | EXTENDED_USAGE` and
  `TRANSFER_SRC | TRANSFER_DST | COLOR_ATTACHMENT | SAMPLED`, with the storage-view path retained.
- Fixed the pre-copy source stages to Eden's graphics/compute/transfer set.
- Restored `Frame::index`, `Frame::storage_view`, `MaxExtraFrames`, present-thread naming,
  high-priority selection, performance-core call, and the `MAX_FRAMES_IN_FLIGHT` owner/name.
- Fixed frame-image allocation ownership, current swapchain-format selection during recreation,
  device-loss reporting, non-surface error propagation, and surface-loss retries across both
  swapchain recreation and the copy path.
- Fixed destruction order so the present thread stops before frames are released and every frame
  releases fence, semaphore, framebuffer, views, then image before the command pool.

### Missing items

- Optional `HAS_LSFG` frame-generation scheduling is not enabled in Ruzu; all non-LSFG
  `PresentManager` behavior is present.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/blit_screen.rs` vs Eden `src/video_core/renderer_vulkan/vk_blit_screen.{h,cpp}`

### Intentional differences

- C++ reference members are explicit call dependencies in Rust to avoid a self-referential
  `RendererVulkan`. The present-manager-owned path uses a frame index and a same-file mechanical
  `draw_layers` tail so no mutable frame borrow aliases the manager during recreation.
- `WindowAdaptPass` uses `Option` for Eden's nullable `unique_ptr`; Vulkan construction failures
  panic at the same points where Eden's wrapper throws.

### Unintentional differences (to fix)

- Removed the invented `BlitFrame` snapshot and restored mutable `Frame` recreation on every
  `presentation_recreate_required` path, including capture and applet frames.
- Restored `PrepareFrame`, `std::list<Layer>` ownership through `LinkedList`, Eden's concrete
  nearest-neighbor initial filter, and `image_index = 0` after every resource or framebuffer
  rebuild.
- Restored the full-layout `CreateFramebuffer` interface and use of the caller's high-level
  `Device` in `WaitIdle` and framebuffer creation.
- Moved pipeline selection, push-constant/descriptor allocation, layer configuration, and draw
  recording back to `present/window_adapt_pass.rs`, their upstream owner.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/present/window_adapt_pass.rs` and `present/filters.rs` vs Eden `src/video_core/renderer_vulkan/present/window_adapt_pass.{h,cpp}`

### Intentional differences

- Construction helpers return their newly created handle because Rust cannot call mutating methods
  on a partially initialized value. The helper names, ownership, call order, and inputs match Eden.
- Raw ash handles retain a cloned logical-device dispatch table and are destroyed explicitly in
  reverse C++ member order.

### Unintentional differences (to fix)

- Restored `Draw` ownership of blend-pipeline selection, `ConfigureDraw`, push constants,
  descriptor sets, and command recording; `BlitScreen` no longer preconfigures this state.
- Restored the high-level `Device&` constructor interface and the five upstream-owned creation
  helper boundaries instead of flattening them into `new`.
- Removed the invented public sampler accessor; Eden exposes only descriptor-set layout and render
  pass accessors.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/present/util.rs` and `renderer_vulkan.rs` frame ownership integration vs Eden `src/video_core/renderer_vulkan/present/util.{h,cpp}` and `renderer_vulkan.cpp`

### Intentional differences

- `create_wrapped_image_allocation` is the owning Rust return form of Eden's `vk::Image`; it shares
  the exact `CreateWrappedImage` create-info owner with the legacy raw-handle form.
- Rust explicitly destroys and nulls the local/app-capture framebuffer and view before the owning
  image drops. Eden obtains the same reverse resource order from wrapper destructors.

### Unintentional differences (to fix)

- Restored zero-initialized `Frame` dimensions in `RenderToBuffer` and
  `RenderAppletCaptureLayer`, so the first `DrawToFrame` performs Eden's presentation-frame
  recreation rather than bypassing it.
- Replaced allocator-retained raw images in those frames with per-frame owning allocations, so
  recreation and local-frame destruction release the replaced images instead of leaking them.
- Updated all `BlitScreen` construction, framebuffer, draw, and present-manager construction calls
  to the restored ownership and lifecycle interfaces.

## 2026-08-26 — `src/common/src/thread.rs` vs Eden `src/common/thread.{h,cpp}` (`SetCurrentThreadToPerformanceCores` integration)

### Intentional differences

- The function is a no-op on the current Linux, Windows, and macOS targets exactly like Eden's
  non-Android branch. Android ADPF/topology policy remains excluded by the project's documented
  platform exceptions.

### Unintentional differences (to fix)

- Restored the named function so presentation and other worker owners can keep Eden's explicit
  thread-policy call sites instead of omitting them.

### Missing items

- Android's ADPF session and core-group implementation is not built.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/buffer_cache.rs` vs Eden `src/video_core/renderer_vulkan/vk_buffer_cache.{h,cpp}`

### Intentional differences

- Rust's common-cache trait names the three concrete handle combinations for `CopyBuffer` and
  supplies the mapped-uniform-buffer span through a callback. They preserve Eden's destination,
  source, staging ownership, descriptor insertion, and command ordering without returning a borrow
  tied simultaneously to the runtime and staging pool.
- `DeviceReference` and stable `NonNull` service pointers represent Eden's reference members, and
  the two anonymous quad-index subclasses are represented by topology-keyed state plus same-file
  generation helpers. Method and constant ownership remains in `buffer_cache.rs`.
- The unused, default-constructed `MemoryCommit` member left in Eden's anonymous
  `QuadIndexBuffer` is omitted; allocation ownership already resides in Eden's `vk::Buffer` and
  Ruzu's corresponding `AllocatedBuffer`, and neither implementation reads that extra member.
- Ash exposes raw Vulkan handles rather than Eden's owning `vk::Buffer`; the allocation itself is
  retained by `AllocatedBuffer`, and its views are explicitly destroyed before that allocation.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_memory_allocator.rs` vs Eden `src/video_core/vulkan_common/vulkan_wrapper.{h,cpp}` (`AllocatedBuffer` integration)

### Intentional differences

- Eden's `vk::Buffer` combines the Vulkan handle and VMA allocation in its wrapper layer. Ash has
  no owning buffer wrapper, so the existing `AllocatedBuffer` in the allocator module owns the same
  VMA allocation and provides the mapped-memory operations required by `vk_buffer_cache.cpp`.
- VMA calls are serialized through the allocator mutex because Ruzu creates VMA for external
  synchronization; Eden stores and calls its opaque allocator handle directly.

### Missing items

- Other wrapper/allocator coverage remains
  tracked by the dedicated Vulkan wrapper and memory-allocator audits.

## 2026-08-26 — `src/video_core/src/buffer_cache/buffer_base.rs` vs Eden `src/video_core/buffer_cache/buffer_base.h`

### Intentional differences

- Eden's `VAddr` and `DAddr` are both aliases of `u64`. Ruzu retains its existing `VAddr` alias for
  the private CPU address and spells the public cached device-address field as `u64`, preserving
  the same type identity, value, visibility, and bit pattern without introducing another local
  address alias.
- Eden's class-static page constants are module constants in the corresponding Rust file. The
  owner, values, and visibility remain local to `buffer_base.rs`.

## 2026-08-26 — buffer-cache base contract and backend adapters vs Eden buffer-cache headers/runtimes

### Intentional differences

- Eden relies on C++ template duck typing for the backend runtime, buffer, async allocation, and
  memory tracker. Rust makes those contracts explicit with traits; the concrete device tracker is
  the second `BufferCache` type parameter because `MemoryTrackerBase` itself is a Rust generic.
- `HostBindings` retains slot identifiers instead of C++ `Buffer*` values, then resolves the
  backend buffers from the same slot vector at the runtime boundary. This avoids retaining Rust
  references across cache mutations while preserving binding order and values.
- `TextureBufferBinding::default` initializes `format` to `PixelFormat::Invalid`; Eden's implicit
  default construction leaves that member indeterminate until a valid binding is assigned. Rust
  cannot safely represent an uninitialized enum, and no binding path consumes the sentinel format.
- Vulkan's common trait receives the concrete cached `Buffer` and forwards its native handle to
  the existing `VkBuffer`-shaped runtime method. This is the Rust equivalent of Eden's
  `if constexpr` branch calling `buffer.Handle()`; OpenGL forwards the buffer object directly.
- The private `BufferCache` storage and its private constants remain beside the method bodies in
  `buffer_cache.rs`. A sibling-module struct would require widening every private field merely so
  Rust could implement the upstream methods; shared types and runtime interfaces remain owned by
  `buffer_cache_base.rs`.

## 2026-08-26 — `src/video_core/src/buffer_cache/buffer_cache.rs` vs Eden `src/video_core/buffer_cache/buffer_cache_base.h` and `buffer_cache.h`

### Intentional differences

- The page table is a boxed fixed-size array instead of an inline `std::array`. Its length and
  indexing contract match Eden, while boxing avoids constructing and moving a multi-megabyte Rust
  cache object on the host stack.
- GPU/device-memory owners are optional boxed Rust interfaces rather than C++ references and raw
  pointers. Missing-owner guards are restricted to states Eden's production lifecycle does not
  enter.
- Resolver/reader variants, explicit temporary collections, and temporary field extraction split
  mutable borrows that C++ can hold simultaneously. Range order, copy order, and mutations match
  the owning Eden methods.
- Garbage collection first records the same eligible LRU identifiers and then deletes them in the
  same order. Eden deletes inside the LRU callback; Rust cannot mutate the slot vector while that
  callback holds the LRU borrow, and buffer deletion does not affect selection of later LRU items.
- Eden's overlapping `std::copy_n` calls are undefined by the C++ standard. The current GCC 13.3
  libstdc++ specialization lowers this trivially-copyable array operation to `memmove`; Rust's
  reverse shift reproduces its observed `[0, old0, old1, ...]` result deterministically.
- `ImmediateBufferWithData` falls back to a guest-memory read if the direct base pointer is absent.
  Eden would form a non-empty span from a null pointer for that invalid lifecycle state; Rust does
  not construct an invalid slice.
- The non-Android Rust frontend always uses Eden's optimized vertex-buffer batching path. Eden's
  alternate unoptimized path is selected only by an Android-specific setting.

### Missing items

- Eden's three unread members
  (`last_index_count`, `current_buffer`, and `immediate_buffer_capacity`) remain intentionally
  omitted as recorded above.

## 2026-08-26 — `src/video_core/src/buffer_cache/memory_tracker_base.rs` vs Eden `src/video_core/buffer_cache/memory_tracker_base.h`

### Intentional differences

- Rust stores stable `(pool_index, slot_index)` locations instead of raw `Manager*` values. Each
  32-manager batch is a boxed fixed array, so its elements stay at stable addresses while the
  outer collection grows, and every lookup resolves the same manager selected by Eden.
- The fixed top-tier array is boxed to avoid placing it on the Rust object stack; its compile-time
  length and indexing contract match Eden's `std::array`.
- `cached_cpu_write` records page identifiers during manager iteration and inserts them afterward
  to split simultaneous mutable borrows of the manager pool and cached-page set. The same page IDs
  are inserted into an unordered set.
- Both page iterators detect an index beyond the 34-bit tracked address space, emit a rate-limited
  diagnostic, and stop. Eden indexes its fixed array out of bounds in that invalid state, which is
  undefined behavior; valid device-address ranges follow the same iteration path.

## 2026-08-26 — `src/video_core/src/buffer_cache/usage_tracker.rs` vs Eden `src/video_core/buffer_cache/usage_tracker.h`

### Intentional differences

- Eden's `~u64{0} >> (64 - num_bits)` has undefined C++ behavior when `num_bits == 0`.
  GCC 13.3 on the supported x86-64 host lowers the variable shift to the hardware modulo-64
  operation, yielding an all-ones mask. Rust spells out that conservative over-marking explicitly
  so sub-64-byte and zero-length ranges do not panic or silently become no-ops.

## 2026-08-26 — `src/video_core/src/buffer_cache/word_manager.rs` vs Eden `src/video_core/buffer_cache/word_manager.h`

### Intentional differences

- Stable Rust cannot use `Type::Max * DivCeil(SIZE_BYTES, BYTES_PER_WORD)` directly as a generic
  array length. Ruzu stores the same fixed inline words as `[[u64; STACK_WORDS]; Type::Max]` and
  validates that `STACK_WORDS` equals Eden's compile-time `num_words`; arrays remain contiguous in
  the same type-major, word-minor order and no heap allocation occurs.
- Mutable operations obtain raw pointers to separate tracking channels because Rust cannot hold
  the overlapping mutable slice borrows that Eden expresses as independent `std::span` values.
- The callback adapter uses `Option<bool>` for Eden's compile-time distinction between callbacks
  returning `bool` and callbacks returning `void`.
- Eden's unused `NotifyRasterizer` legacy helper remains omitted. Every active mutation path in
  both trees uses `CollectChangedRanges` followed by the batched `ApplyCollectedRanges` operation.
- Applying ranges through a default manager without a tracker fails explicitly. Eden would
  dereference its null default tracker in this invalid lifecycle state; valid managers behave
  identically.

## 2026-08-26 — `src/video_core/src/capture.rs` vs Eden `src/video_core/capture.h`

### Intentional differences

- Ruzu's shared `align_up_log2` accepts and returns `u64`, so the `u32` framebuffer height is
  widened for the constant evaluation and narrowed after alignment. The result and unsigned bit
  pattern match Eden's templated `Common::AlignUpLog2` call.

## 2026-08-26 — `src/video_core/src/cdma_pusher.rs` vs Eden `src/video_core/cdma_pusher.h` and `.cpp`

### Intentional differences

- Eden uses virtual inheritance for each device's `ProcessMethod`; Ruzu stores the corresponding
  `ProcessMethodHook` trait object. NvDec and Vic install their concrete processors, and the call
  remains owned by the CDMA pusher's `SetMethod1` path.
- Ruzu keeps the worker-owned parser/register state behind mutexes because the pusher is shared
  through `Arc`; its condition variable plus explicit stop flag and joined thread reproduce the
  `std::jthread` wait, stop, and destruction ordering.
- The core/video bridge materializes a safe-read command list into an owned vector before enqueue,
  whereas Eden's `CpuGuestMemory` may retain a direct guest span when contiguous. This avoids a
  borrowed, self-referential guest-memory view crossing the Rust worker-thread boundary; normal
  submitted command buffers are immutable after submission, so command order and contents match.
- Ruzu diagnoses and skips a THI register index beyond `NUM_REGS`. Eden indexes the fixed register
  array out of bounds for such an invalid command, which is undefined behavior; valid methods use
  the identical register write and dispatch sequence.

## 2026-08-26 — `src/video_core/src/host1x/control.rs` vs Eden `src/video_core/host1x/control.h` and `.cpp`

### Intentional differences

- `Control` owns the shared syncpoint manager rather than receiving the parent `Host1x&` on each
  method call. This avoids a parent/device reference cycle while preserving the same manager and
  wait operation.
- `Method` is a transparent `u32` newtype rather than a Rust enum so Eden's default switch arm can
  safely receive arbitrary values produced by `Control::Method(raw)`.

## 2026-08-26 — `src/video_core/src/control/channel_state_cache.rs` vs Eden `src/video_core/control/channel_state_cache.h`, `.cpp`, and `.inc`

### Intentional differences

- Rust stores each payload in a `Box` inside the deque so the active payload address remains stable
  when `VecDeque` reallocates; Eden obtains the same stability directly from `std::deque<P>`.
- Rust retains GPU memory through `Arc<Mutex<MemoryManager>>` where Eden stores non-owning
  references and pointers. Engine references remain stable raw addresses because their owning
  `ChannelState` boxes outlive registered cache entries.
- Rust's `&mut self` mutation methods replace Eden's internal `config_mutex`; cache owners provide
  cross-thread synchronization around the complete cache object.
- The derived `OnGPUASRegister(map_id)` virtual call is a one-argument closure so a texture cache
  can mutably borrow its page-table storage beside `channel_caches`. Its argument and call point
  match Eden.
- Reusing a free payload slot drops the previous Rust value before replacement. Eden uses placement
  construction in the occupied deque slot; Rust cannot safely begin a second object lifetime
  without ending the first one.

## 2026-08-26 — removed `src/video_core/src/command_processor.rs` and `gpu_context.rs` vs Eden `src/video_core/dma_pusher.{h,cpp}` and `gpu_thread.{h,cpp}`

### Missing items

- `dma_pusher.rs`, `gpu_thread.rs`, and `control/scheduler.rs`
  remain the active counterparts of Eden's submission path.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/compute_pass.rs` vs Eden `src/video_core/renderer_vulkan/vk_compute_pass.h` and `.cpp`

### Intentional differences

- Rust retains Eden's `const Device&` through `DeviceReference`, whose pointee is owned in stable
  boxed renderer storage. Recorded commands clone only the logical ash dispatch table; capability
  decisions remain owned by `Device`.
- `ComputePass::new` passes Eden's explicit device and scheduler arguments through to
  `DescriptorPool::allocator`. A shared scheduler borrow is sufficient because allocator creation
  only obtains the scheduler's master semaphore.
- ASTC and 3D-unswizzle entry points receive decomposed image handles/state because mutably
  borrowing the texture-cache runtime and one of its slot-map images simultaneously is not safe in
  Rust. Image initialization exchange, compute-unswizzle-buffer allocation, and storage-view lookup
  remain performed by the texture-cache owner immediately before these calls.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/compute_pipeline.rs` vs Eden `src/video_core/renderer_vulkan/vk_compute_pipeline.h` and `.cpp`

### Intentional differences

- `ComputePipelineRuntime` groups the scheduler and three stable renderer-owned descriptor
  services that Eden receives as constructor references. Rust stores them as `NonNull` because the
  pipeline is built asynchronously and cached in stable boxed storage.
- The asynchronously published Vulkan pipeline is an `Arc<Mutex<VkPipeline>>`. This lets recorded
  scheduler closures perform Eden's late `IsBound()` check without capturing a borrow of the cache
  entry; `is_bound` remains on `ComputePipeline` as the direct upstream counterpart.
- `configure` receives a compute-register snapshot, a guest-memory reader, and the already-loaded
  push-descriptor dispatch table instead of borrowing the engine, memory manager, and wrapper
  device simultaneously. Descriptor collection and command ordering remain the same.

### Missing items

- Compute dispatch logging remains owned by the Vulkan rasterizer, as in
  Eden, and is covered by its separate parity entry.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/compute_pipeline.rs` vs Eden `src/video_core/renderer_vulkan/vk_compute_pipeline.h` and `.cpp`

### Intentional differences

- Rust calls the same process-wide GPU logger through free functions returning the singleton;
  Eden spells the access as `GPU::Logging::GPULogger::GetInstance()`.

## 2026-08-26 — `src/video_core/src/host_shaders/compute_shaders.rs` and compute `.comp` files vs Eden `src/video_core/host_shaders/*.comp`

### Intentional differences

- Eden's CMake build generates one C++ source-string header for each non-Vulkan-only shader and a
  SPIR-V header for each non-OpenGL-only shader. Rust exposes runtime GLSL with `include_str!` and
  compiles Vulkan SPIR-V in `build.rs`; both paths now read the same upstream-owned `.comp` files.
- `block_linear_unswizzle_3d_bcn.comp` has a final newline in the Rust tree while Eden's file ends
  immediately after the closing brace. Its GLSL token stream and behavior are identical.

## 2026-08-26 — `src/video_core/src/engines/const_buffer_info.rs` vs Eden `src/video_core/engines/const_buffer_info.h`

### Intentional differences

- Rust applies `repr(C)` explicitly and derives value traits; Eden receives the equivalent natural
  C++ aggregate layout and value-initializes the containing Maxwell state.
- `Maxwell3D::process_cb_bind` rejects a shader-slot value outside the 18-entry array. Eden uses
  unchecked `std::array::operator[]`; preserving that undefined behavior would be unsound in Rust.

## 2026-08-26 — `src/video_core/src/control/scheduler.rs` vs Eden `src/video_core/control/scheduler.h` and `.cpp`

### Intentional differences

- Rust stores the channel map inside the mutex that represents Eden's `scheduling_guard`, rather
  than keeping the map and mutex as separate fields.
- A channel is retained as `Arc<Mutex<ChannelState>>` instead of `shared_ptr<ChannelState>`. The
  per-channel lock provides Rust's required synchronization while the map lock is still released
  before DMA push and dispatch, preserving Eden's global-lock scope.
- Missing channels and uninitialized DMA pushers panic through `expect`; these correspond to Eden's
  assertion and required initialized `payload` invariant.

## 2026-08-26 — `src/video_core/src/texture_cache/decode_bc.rs` vs Eden `src/video_core/texture_cache/decode_bc.h` and `.cpp`

### Intentional differences

- Rust reaches Eden's vendored C++ `bc_decoder` through thin `extern "C"` functions in
  `src/video_core/src/textures/bcn_shim.cpp`; every wrapper delegates directly to the matching
  `bcn::DecodeBc1` through `bcn::DecodeBc7` function without owning decode behavior.
- The shared Rust traversal uses a closure in place of Eden's function-valued template parameter,
  and splits signed and unsigned decoder signatures at the FFI boundary. The format dispatch,
  constants, loop nesting, offsets, and decoder arguments are unchanged.
- Rust returns for zero extents or undersized input/output spans instead of performing pointer
  arithmetic outside a span or dividing by a zero block width. Valid `BufferImageCopy` inputs take
  the same path as Eden.

## 2026-08-26 — `src/common/src/alignment.rs` and `div_ceil.rs` vs Eden `src/common/alignment.h` and `div_ceil.h`

### Intentional differences

- Rust exposes fixed `u64`, `u32`, and `usize` functions instead of C++ constrained function
  templates. The selected integer width is explicit at each call site and preserves the same
  arithmetic width.
- Signed alignment has a separate Rust entry point; it performs Eden's conversion through the
  corresponding unsigned width before converting the result back.
- Eden's unused `AlignmentAllocator` C++ container adapter has no standalone Rust type. Rust
  allocation sites that need stronger alignment request it through `std::alloc::Layout`; Eden has
  no source-tree consumer of this adapter to map here.

## 2026-08-26 — `src/video_core/src/textures/decoders.rs` vs Eden `src/video_core/textures/decoders.h` and `.cpp`

### Intentional differences

- Rust validates the linear span and the last byte actually visited in the tiled span once before
  the full-image loop. Eden relies on its internal callers and indexes those spans unchecked; some
  compressed-image callers intentionally omit untouched tail padding from the containing GOB.
  After that one safe-API guard, Rust uses the same fixed-size non-overlapping copy in the per-pixel
  loop; subrectangle copies retain their existing range guards because their linear-span layout is
  not a full image.
- Eden's `ASSERT_MSG` for an unsupported bytes-per-pixel value is represented by `panic!`.
- The eight C++ `BPP_CASE` macro expansions are written as explicit Rust match arms.

### Unintentional differences (to fix)

- Corrected: the full-image loop previously performed `checked_add`, two bounds comparisons, slice
  construction, and conditional skipping for every pixel. Eden performs a direct fixed-size
  `memcpy`; Ruzu now validates the spans once and gives the hot loop the same shape.
- Corrected during full-suite validation: the first global guard required the complete theoretical
  GOB allocation (`slice_size * depth_blocks`) even when Eden's caller supplied only the bytes
  touched by a compressed image. The guard now derives the monotonic final visited tiled offset;
  the BC tile-count regression again passes without restoring per-pixel checks.

## 2026-08-26 — `src/audio_core/src/renderer/memory/pool_mapper.rs` vs Eden `src/audio_core/renderer/memory/pool_mapper.cpp`

### Intentional differences

- Ruzu obtains the 4 KiB guest-page value from `common::PAGE_SIZE_U64`; Eden names the same value
  `Core::Memory::YUZU_PAGESIZE`. This avoids introducing a dependency on a C++ memory-header
  boundary while retaining the shared constant rather than a literal.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/descriptor_buffer.rs` vs Eden `src/video_core/renderer_vulkan/vk_descriptor_buffer.h` and `.cpp`

### Intentional differences

- Rust stores the same non-owning device relationship as a `DeviceReference`, because a C++
  reference cannot be stored directly without making every renderer type lifetime-parameterized.
  The renderer parent retains ownership and destroys the ring first.
- Vulkan allocation failures use `Result<VulkanError>` instead of constructor exceptions.
- The raw mapped pointers make the ring non-`Send` by inference. The explicit `Send` implementation
  records Eden's GPU-thread ownership contract; mutation still requires exclusive access.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/descriptor_pool.rs` and descriptor-allocation call sites vs Eden `src/video_core/renderer_vulkan/vk_descriptor_pool.h` and `.cpp`

### Intentional differences

- Eden's `vk::DescriptorPool` wrapper destroys each handle through RAII. Ruzu currently stores raw
  ash handles, so `DescriptorPool` retains a non-owning `DeviceReference` and destroys them in
  `Drop`.
- Rust uses `Result<_, vk::Result>` for Vulkan failures and mutexes for state reached by deferred
  `Send + 'static` scheduler commands. `DescriptorAllocatorReference` is the non-owning counterpart
  of Eden's scheduler lambdas capturing `this`.
- Allocator methods accept a shared scheduler borrow because they only call the logically const
  `get_master_semaphore`; Eden spells the parameter as a non-const reference.

## 2026-08-26 — `src/video_core/src/texture_cache/descriptor_table.rs` and Maxwell3D TIC/TSC accessors vs Eden `src/video_core/texture_cache/descriptor_table.h` and `src/video_core/engines/maxwell_3d.{h,cpp}`

### Intentional differences

- Rust accepts a `GpuMemoryReader` trait object where Eden's `read` accepts a concrete
  `Tegra::MemoryManager` reference. Channel-specific texture-cache state can therefore pass either
  its direct memory manager or the backend-independent SMMU reader without changing descriptor
  table behavior.
- `read_with` is the Rust overload used by call sites that own a locked channel memory manager;
  both read paths execute the same owner-local descriptor-cache logic.
- Rust initializes descriptor storage through `T::default()` before exposing its bytes to the
  memory reader. Eden default-constructs the `std::pair<T, bool>` storage before
  `ReadBlockUnsafe`; the supported POD descriptor types accept every overwritten bit pattern.

## 2026-08-26 — `src/video_core/src/dirty_flags.rs` vs Eden `src/video_core/dirty_flags.h` and `.cpp`

### Intentional differences

- Rust spells Eden's unnamed `u8` enum as module constants and the two `std::pair<u8, u8>` results
  as `(u8, u8)` tuples.
- Rust names the register-structure word counts explicitly because it cannot apply C++ `sizeof`
  to the separately modeled Maxwell register fields. Each count was checked against Eden's field
  type and static size assertions.

## 2026-08-26 — `src/video_core/src/dma_pusher.rs` vs Eden `src/video_core/dma_pusher.h` and `.cpp`

### Intentional differences

- Rust stores `System`, `MemoryManager`, and `ChannelState` through the existing non-owning or
  synchronized handle types rather than C++ references. The embedded `Puller` retains a duplicate
  non-owning `ChannelState` pointer because borrowing the embedded object and its complete parent
  mutably at the same time is not representable as safe Rust; `install_self_reference` verifies
  that both pointers identify the same upstream-owned channel state.
- The synchronization predicate and condition variable live in an `Arc<DmaSyncState>` so Eden's
  asynchronous fence callback can outlive the stack frame without capturing a raw `this` pointer.

## 2026-08-26 — Draw-manager and topology consumers vs Eden `src/video_core/engines/draw_manager.cpp`, `maxwell_3d.h/.cpp`, and renderer counterparts

### Intentional differences

- Rust passes Maxwell3D access through the `Maxwell3DAccess` trait where each Eden method receives
  `Maxwell3D&`. This preserves the per-call relationship without creating overlapping mutable
  borrows between the engine and its embedded draw manager.
- The bulk inline-index overload accepts a `&[u32]`; the slice length is Eden's `amount`, and
  `bytemuck::cast_slice` preserves the same native in-memory word representation used by Eden's
  `reinterpret_cast` and `memcpy` paths.
- Test builds retain compatibility draw snapshots before invoking the rasterizer. This state is
  absent from production builds and exists only for renderer-independent regression assertions.
- C++ permits the explicit override-enum to primitive-enum cast to retain `Legacy*` discriminants
  that `PrimitiveTopology` does not declare. Rust cannot hold an undeclared enum discriminant
  safely, so its `PrimitiveTopology` declares representation-only `Legacy*` variants with the same
  `u32` values. OpenGL/Vulkan conversion, pipeline runtime info, query counting, and depth-bias
  consumers retain Eden's respective invalid/default handling for those values.

## 2026-08-26 — `src/video_core/src/engines/engine_upload.rs` vs Eden `src/video_core/engines/engine_upload.{h,cpp}`

### Intentional differences

- Eden stores a `Registers&` inside `Upload::State`. Rust passes the owning engine's register view
  to each entry point to avoid a movable self-referential engine; every owner constructs that view
  immediately before the same Eden call boundary.
- `Common::ScratchBuffer<u8>` is represented by reusable `Vec<u8>` storage. The block-linear
  `GpuGuestMemoryScoped<SafeReadCachedWrite>` lifecycle is expressed as an ordered
  `read_block`/swizzle/`write_block_cached` sequence while holding the Rust memory-manager owner.
- Runtime `MemoryManager&` and `RasterizerInterface*` ownership is represented by
  `Arc<Mutex<MemoryManager>>` and `RasterizerHandle`; the optional memory-manager state exists only
  for reduced `cfg(test)` Maxwell fixtures.
- Invalid short upload input panics at Rust slice construction rather than invoking Eden's C++
  out-of-bounds behavior. Valid command streams use the same line slices.

### Unintentional differences (to fix)

- Resolved: `ProcessExec`, word accumulation, and linear destination calculations now use wrapping
  unsigned arithmetic matching Eden's `u32`/`GPUVAddr` operations in debug and release builds.
- Resolved: single-word accumulation now uses native byte order, matching Eden's `memcpy` from the
  host `u32`, instead of forcing little-endian bytes.
- Resolved: the linear path no longer depends on the unrelated memory-manager owner, silently skips
  short lines, or falls back to a direct memory write when the required rasterizer is absent.

## 2026-08-26 — `src/video_core/src/engines/mod.rs` vs Eden `src/video_core/engines/`

### Intentional differences

- Rust requires `mod.rs` to declare the source modules; Eden expresses the same source membership
  through its C++ build files and includes.
- The legacy standalone `inline_to_memory` fixture remains visible only under `cfg(test)` until its
  dedicated parity report is reviewed; production uses Eden's `KeplerMemory` plus `Upload::State`.

### Unintentional differences (to fix)

- Resolved: removed the duplicate `ClassId` enum. The canonical class identifiers remain in
  `puller.rs`, matching Eden's `EngineID` ownership in `puller.h`.
- Resolved: removed the invented `SubChannel` enum. In particular, Rust no longer labels subchannel
  2 as inline-to-memory when Eden explicitly reserves it for the unexposed M2MF engine; the NVK
  default bindings remain owned by `control/channel_state.rs` at 0, 1, 3, and 4.
- Resolved: removed the production `Engine` dispatcher trait, which had no runtime consumer and no
  Eden counterpart. Register-write and deferred-write helpers needed by native Rust tests now live
  in their concrete engine owners under `cfg(test)`.
- Resolved: `engines/mod.rs` no longer owns the shared `ENGINE_REG_COUNT` or `PendingWrite`
  compatibility payload. Maxwell3D and MaxwellDMA now own their distinct upstream register counts
  and their local deferred-write integration payloads.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/present/filters.rs` and `present/util.rs` vs Eden `src/video_core/renderer_vulkan/present/filters.{h,cpp}` and `present/util.{h,cpp}`

### Intentional differences

- ash 0.37 predates `VK_QCOM_filter_cubic_weights`; the Rust counterpart declares the extension's
  four-value enum and `VkSamplerCubicWeightsCreateInfoQCOM` payload locally with the exact Vulkan
  ABI instead of obtaining generated declarations from ash.
- `WindowAdaptPass` is returned directly rather than through `std::unique_ptr`; ownership and
  construction timing are unchanged.

### Unintentional differences (to fix)

- Restored Eden's QCOM-weighted hardware cubic path: any cubic weight now uses `VK_FILTER_CUBIC_EXT`
  when both cubic extensions are supported, and non-Catmull-Rom modes chain the selected weight into
  sampler creation. The shader fallback remains selected under Eden's exact condition.
- Restored `CreateCubicSampler`'s high-level `Device` input and its linear-filter fallback when
  `VK_EXT_filter_cubic` is unavailable.

## 2026-08-26 — `src/video_core/src/texture_cache/format_lookup_table.rs` and `texture_cache/util.rs` vs Eden `src/video_core/texture_cache/format_lookup_table.{h,cpp}` and `texture_cache/util.cpp`

### Intentional differences

- TIC accessors currently expose their packed fields as `u32`, so the raw lookup entry point keeps
  those bit patterns until the canonical enum-based public function is reached. Both paths feed the
  same upstream hash and fallback table.
- Unknown tuples use an error log followed by `A8B8G8R8_UNORM`, matching Eden's
  `UNIMPLEMENTED_MSG` plus explicit fallback without relying on C++ logging macros.

### Unintentional differences (to fix)

- Removed the duplicate `TextureFormat` and incomplete `ComponentType` declarations from the lookup
  table. The function now consumes the canonical types owned by `textures/texture.rs`, matching
  Eden's ownership in `textures/texture.h`.
- Restored `SNORM_FORCE_FP16` and `UNORM_FORCE_FP16` flow through `PixelFormatFromTIC`: unsupported
  tuples now reach Eden's logged `A8B8G8R8_UNORM` fallback instead of returning `Invalid` early.

## 2026-08-26 — `src/video_core/src/texture_cache/formatter.rs` vs Eden `src/video_core/texture_cache/formatter.{h,cpp}`

### Intentional differences

- Rust implements `Display` instead of `fmt::formatter` specializations and gives the three C++
  `Name` overloads distinct snake-case names. Their ownership and formatted output are unchanged.

### Unintentional differences (to fix)

- Resolved: every one of Eden's 112 `PIXEL_FORMAT_LIST` entries now returns its exact enumerator
  spelling instead of valid ASTC, BC sRGB, ETC2, EAC, and packed formats falling through to
  `Invalid`.
- Resolved: image-view addresses now use lower-case hexadecimal digits, matching Eden's `{:#x}`
  formatting used by the already-correct image formatter.

## 2026-08-26 — `src/video_core/src/host_shaders/fragment_shaders.rs` and `host_shaders/*.frag` vs Eden `src/video_core/host_shaders/CMakeLists.txt` and `host_shaders/*.frag`

### Intentional differences

- Eden generates C++ string headers from each GLSL file with CMake. Rust uses `include_str!` on the
  same per-shader files, while `build.rs` separately compiles the Vulkan-compatible sources to
  SPIR-V. This preserves one authoritative source per shader without generated Rust source headers.
- Rust exposes source constants for Vulkan-only shaders as well; Eden skips only their generated
  string headers because its Vulkan backend consumes the generated SPIR-V headers directly.

### Unintentional differences (to fix)

- Resolved: replaced 37 manually copied string literals with direct file embedding. The stale
  bicubic, Gaussian-comment, and depth/stencil-blit copies can no longer disagree with the shader
  files compiled by the build.
- Resolved: synchronized `present_bicubic.frag` with Eden's Catmull-Rom implementation and restored
  the unsigned stencil sampler plus explicit integer conversion in
  `vulkan_blit_depth_stencil.frag`.
- Resolved: restored the four OpenGL shader files that existed only as Rust string literals and the
  seven fragment-source constants omitted from the old module. All 44 Eden fragment shader files
  now have one matching file and one embedded Rust constant.

## 2026-08-26 — `src/video_core/src/framebuffer_config.rs` and framebuffer bridge consumers vs Eden `src/video_core/framebuffer_config.{h,cpp}`

### Intentional differences

- The `core`/`video_core` crate dependency direction requires a small `gpu_core::FramebufferConfig`
  bridge. It now forwards Eden's canonical `PixelFormat`, `BufferTransformFlags`, and
  `Rectangle<i32>` unchanged; only `BlendMode` remains mirrored across the crate boundary.
- Rust logs unsupported residual transform bits with `warn!`, corresponding to Eden's
  `UNIMPLEMENTED_MSG`, then continues with the same normalized coordinates.

### Unintentional differences (to fix)

- Resolved: removed the local pixel-format, transform-flag, and rectangle replacements. The
  framebuffer descriptor and all OpenGL, Vulkan, null-renderer, surface, and texture-cache
  consumers now use their canonical upstream-owned types.
- Resolved: a crop rectangle whose width or height is zero now falls back to the framebuffer
  dimensions even when it is not located at the origin, matching `Common::Rectangle::IsEmpty`.
- Resolved: `PixelFormatFromGPUPixelFormat` now accepts the canonical Android `PixelFormat` instead
  of an untyped `u32`; downstream conversions no longer unwrap and reconstruct its raw value.

## 2026-08-26 — `src/video_core/src/fsr.rs` vs Eden `src/video_core/fsr.{h,cpp}`

### Intentional differences

- Rust fixed-size array references replace C array parameters, while preserving their four-word
  shape and mutation order.
- `f32::to_bits` is the direct Rust equivalent of Eden's `std::bit_cast<u32>`.

### Unintentional differences (to fix)

- Resolved: RCAS sharpening now uses `(-sharpness).exp2()`, the direct equivalent of Eden's
  `std::exp2f(-sharpness)`, instead of routing the same mathematical expression through `powf`.

## 2026-08-26 — Vulkan `present/{fxaa,fsr,smaa}.rs` vs Eden `renderer_vulkan/present/{fxaa,fsr,smaa}.{h,cpp}`

### Intentional differences

- Rust passes the render-pass initial layout explicitly because Rust has no default function
  arguments; the selected value now equals Eden's `CreateWrappedRenderPass` default.
- Raw Vulkan handles are destroyed explicitly in `Drop`, corresponding to Eden's RAII wrappers.

### Unintentional differences (to fix)

- Resolved: FXAA, FSR, and all three SMAA render passes now start in `GENERAL` with attachment
  `LOAD`, matching Eden. They no longer opt into `UNDEFINED`/`DONT_CARE`, which Eden reserves here
  for the explicitly different window-adaptation pass.

## 2026-08-26 — `src/video_core/src/renderer_opengl/gl_blit_screen.rs` vs Eden `src/video_core/renderer_opengl/gl_blit_screen.{h,cpp}`

### Intentional differences

- Non-owning C++ references are represented by renderer-owned raw pointers/handles because the
  referenced Rust objects must remain heap-stable while `RendererOpenGL` owns `BlitScreen`.
- `current_window_adapt` is optional until the first pass is created; Eden's default enum value is
  observationally irrelevant while its `window_adapt` pointer is null.
- `GL_ALPHA_TEST` is declared locally because the generated core-profile Rust GL bindings omit this
  compatibility enumerator that Eden still disables.

### Unintentional differences (to fix)

- Resolved: when an existing window-adaptation pass no longer matches the configured scaling
  filter, Rust now performs Eden's second callback read before selecting the replacement pass. It
  no longer reuses the value read for the early-return comparison.

## 2026-08-26 — `src/video_core/src/renderer_opengl/gl_buffer_cache.rs` vs Eden `src/video_core/renderer_opengl/gl_buffer_cache.{h,cpp}`

### Intentional differences

- The common C++ buffer-cache template is expressed through Rust traits, while all OpenGL runtime
  methods and constants remain owned by this matching backend file.
- The staging pool is a shared synchronized owner and the device is a non-owning stable pointer,
  adapting Eden's references to the renderer's Rust ownership graph. A context-free constructor is
  compiled only for unit tests.
- Optional NV extension entry points are loaded explicitly because the generated GL bindings do not
  expose them; their signatures and call sites match Eden.
- Explicit `Drop` ordering mirrors reverse C++ member destruction for GL resource wrappers.

### Unintentional differences (to fix)

- Resolved: `Buffer::view` now creates its texture before translating the surface format, matching
  Eden's allocation and failure ordering.
- Resolved: GPU-address, binding-index, program-parameter-index, and initial memory-budget additions
  now preserve C++ unsigned wrapping instead of panicking in debug Rust on overflow.
- Resolved: unified index-buffer size alignment is truncated back to Eden's `u32` result before it
  is converted to `GLsizeiptr`.

## 2026-08-26 — `src/video_core/src/renderer_opengl/gl_compute_pipeline.rs` vs Eden `src/video_core/renderer_opengl/gl_compute_pipeline.{h,cpp}`

### Intentional differences

- Stable non-owning pointers and synchronized shared owners adapt Eden's references and raw
  `MemoryManager*` to the renderer's Rust ownership graph; `SetEngine` still replaces both live
  channel objects before `Configure` uses them.
- `Configure` is split into file-local helpers so Rust can release the GPU-memory guard before
  `FillImageViews` borrows the texture cache. The helpers retain Eden's operation order and remain
  owned by the matching compute-pipeline file.
- `SmallVec` supplies the inline storage of Eden's `static_vector`; checked insertion prevents its
  heap-spill capability from changing the upstream fixed-capacity invariant.

### Unintentional differences (to fix)

- Resolved: `Shader::NumDescriptors` and the constructor's combined texture/image counts now use
  wrapping `u32` addition, preserving C++ unsigned arithmetic instead of panicking in debug Rust.
- Resolved: sampler, texture, and image binding counters now use signed 32-bit `GLsizei` semantics
  through indexing, scaling-mask shifts, and the final OpenGL calls.
- Resolved: compute descriptor views and samplers can no longer grow beyond Eden's 80- and
  64-element `static_vector` capacities.

## 2026-08-26 — `src/video_core/src/renderer_opengl/gl_device.rs` vs Eden `src/video_core/renderer_opengl/gl_device.{h,cpp}`

### Intentional differences

- Construction returns `Result` instead of throwing and receives the frontend's already-computed
  strict-context flag because the Qt `EmuWindow` owner is outside the `video_core` crate.
- The Rust GL bindings do not expose GLAD's extension booleans, so the matching flags are derived
  from Eden's copied extension-name list. The same extension names and conjunctions are used.
- A null `glGetString` result becomes an empty owned string rather than constructing a C++ string
  from a null pointer; valid OpenGL contexts follow the identical path.

### Unintentional differences (to fix)

- Resolved: NVIDIA's GLSL-workaround version parser now preserves `std::atoi` prefix semantics,
  including leading whitespace/signs and suffixes after the numeric major version. It no longer
  silently returns zero merely because the major-version substring has a non-numeric suffix.

## 2026-08-26 — `src/video_core/src/renderer_opengl/gl_graphics_pipeline.rs` vs Eden `src/video_core/renderer_opengl/gl_graphics_pipeline.{h,cpp}`

### Intentional differences

- Eden's constructor-selected `ConfigureImpl<Spec>` function pointer is represented by a private
  `ConfigureSpec` enum. Selection order, enabled stages, descriptor-family gates, and the complete
  configure operation order remain identical.
- Stable non-owning pointers and a synchronized completed-build slot adapt Eden's references and
  worker lambda without letting a Rust worker mutate a partially constructed object. Program and
  fence publication still precede `MarkShaderComplete`, and synchronous/parallel fence creation
  follows the same conditions.
- Maxwell register data is borrowed through the renderer's live draw view and the GPU-memory guard
  is released before `FillImageViews`; this preserves Eden's descriptor snapshot and cache-lock
  ordering within the Rust ownership graph.
- Absent stages use `Option<Shader::Info>` instead of Eden's default-constructed `Shader::Info`;
  the selected configure specialization excludes those stages, so their descriptor state is never
  consumed.
- Rust zero-initializes fixed OpenGL-handle staging arrays because safe Rust cannot expose
  partially initialized arrays. Like Eden, every OpenGL call consumes only the populated prefix.

### Unintentional differences (to fix)

- Resolved: GLSL strings and SPIR-V vectors are now moved into the one-shot program-build task and
  released after compilation, rather than cloned and retained for every cached pipeline's entire
  lifetime.
- Resolved: cumulative descriptor totals, base uniform/storage bindings, and transform-feedback
  stride arithmetic now preserve Eden's unsigned `u32` wrapping semantics in debug builds.
- Resolved: global sampler, texture, and image binding counters now retain OpenGL's signed 32-bit
  `GLsizei` representation through pointer selection, array indexing, and final bind calls.
- Resolved: per-stage view traversal now advances by the wrapped `Shader::NumDescriptors` result,
  rather than independently summing descriptor counts in host `usize` arithmetic.

## 2026-08-26 — `src/video_core/src/renderer_opengl/gl_rasterizer.rs` vs Eden `src/video_core/renderer_opengl/gl_rasterizer.{h,cpp}`

### Intentional differences

- Heap-stable cache owners and non-owning pointers replace Eden's reference members; declaration
  order makes the DMA borrower drop before both cache owners.
- GPU ticks, GPU-cache invalidation, and guest-memory access cross the renderer ownership boundary
  through installed callbacks. Calls remain at Eden's corresponding rasterizer lifecycle points.
- Draw, clear, texture-draw, and indirect-draw register access uses scoped engine snapshots so Rust
  can release engine locks before cache operations; each snapshot contains the same state Eden reads.
- `BeginTransformFeedback` and `EndTransformFeedback` are associated methods without a `self`
  receiver because their snapshot arguments avoid borrowing the whole rasterizer while the shader
  cache lends a pipeline; they remain owned by `RasterizerOpenGL` as in Eden.
- Query-cache operations receive the current `AnyCommandQueued` value explicitly instead of storing
  a self-reference from `QueryCache` back into `RasterizerOpenGL`.
- `StateTracker::release_channel` clears Rust's borrowed dirty-flag pointer before its channel owner
  can be destroyed. Eden retains a raw pointer and relies on a subsequent bind before reuse.

### Unintentional differences (to fix)

- Resolved: `OnCacheInvalidation` now calls `ShaderCache::invalidate_region`, including
  `RemovePendingShaders`, instead of the deferred `on_cache_invalidation` path. This matches Eden
  and prevents stale shader lookup entries after a cache invalidation notification.
- Resolved: the private, currently uncalled `SyncClipEnabled` method and its
  `last_clip_distance_mask` state are restored, including the shader-dirty gate, guest enable mask,
  change suppression, and eight OpenGL clip-distance enables. Eden's unimplemented
  `SyncClipCoef` placeholder also retains a same-owner diagnostic counterpart.

## 2026-08-26 — `src/video_core/src/engines/{draw_manager,maxwell_3d}.rs` support for Eden `src/video_core/renderer_opengl/gl_rasterizer.cpp`

### Intentional differences

- Ruzu's draw-time register view uses a `Maxwell3DAccess` method and a snapshot field to expose
  Eden's direct `maxwell3d->regs.user_clip_enable.raw` read to the renderer.

### Unintentional differences (to fix)

- Resolved: draw views now carry the raw user-clip enable mask required by the restored
  `RasterizerOpenGL::SyncClipEnabled` owner.

## 2026-08-26 — `src/video_core/src/renderer_opengl/gl_resource_manager.rs` vs Eden `src/video_core/renderer_opengl/gl_resource_manager.{h,cpp}`

### Intentional differences

- Rust `Drop` replaces every C++ destructor and naturally releases the destination's previous
  resource on assignment. Eden's `OGLPipeline` move-assignment uniquely omits `Release`; reproducing
  that leak would violate Rust assignment semantics rather than observable OpenGL ownership intent.
- A mechanical macro emits wrappers whose create/delete signatures are identical. Wrappers with
  distinct behavior (`OGLTexture`, shader/program objects, syncs, framebuffers, and queries) remain
  explicit in this upstream-owned file.
- The `gl` bindings do not expose `glDeleteProgramsARB`; the optional entry point is loaded beside
  the other GLASM functions in `gl_shader_util.rs`, while `OGLAssemblyProgram::release` retains the
  resource lifecycle here.

### Unintentional differences (to fix)

- Resolved: `OGLSync::is_signaled` now reproduces Eden's always-on fail-soft assertion for
  `GL_WAIT_FAILED`, including fatal behavior when `use_debug_asserts` is enabled, before applying
  the same `status != GL_TIMEOUT_EXPIRED` completion test.

## 2026-08-26 — `src/video_core/src/renderer_opengl/gl_shader_cache.rs` vs Eden `src/video_core/renderer_opengl/gl_shader_cache.{h,cpp}`

### Intentional differences

- Cache owners, the program manager, state tracker, context factory, and shader notification use
  stable Rust handles/pointers in place of Eden reference members. Disk workers compile through a
  temporary cache facade and return completed pipelines to the renderer thread instead of mutating
  the live cache through a captured `this` pointer.
- Disk entries are collected before scheduling so `load_pipelines` can keep safe borrowed callbacks;
  the same compute-then-graphics work is queued, progress begins at `(0, total)`, and completed
  pipelines are inserted after the cancellation-aware wait.
- The Rust recompiler owns indexed IR blocks and instructions in each `Program`; consequently the
  OpenGL `ShaderPools` objects preserve worker-context lifecycle but are not allocation owners as
  Eden's pointer-based IR pools are.
- The renderer path may disable asynchronous compilation when the frontend supplies no shareable GL
  context factory. Eden always receives an `EmuWindow` capable of constructing its worker contexts.

### Unintentional differences (to fix)

- Resolved: `CurrentGraphicsPipeline` now uses `current_pipeline` as Eden's actual fast path instead
  of performing a hash-map lookup on every unchanged draw.
- Resolved: `CurrentGraphicsPipelineSlowPath` updates `current_pipeline` only after obtaining a
  non-null pipeline. A cached or newly compiled failure no longer replaces the prior current
  pipeline key.
- Resolved: `ShaderCache` destruction now relies on declaration-order worker destruction, which
  requests stop and joins before cached pipelines are dropped. The removed custom `Drop` first
  drained every queued compile, unlike Eden's default destructor.
- Verified already correct: both graphics and compute maps retain `None` entries after compilation
  failure, matching Eden's `try_emplace` negative-cache behavior.
- Verified already correct: runtime graphics compilation supplies workers only when
  `use_asynchronous_shaders` is true. Unlike Vulkan's cache, Eden's OpenGL cache does not always
  build runtime pipelines on the pool.

## 2026-08-26 — `src/video_core/src/renderer_opengl/gl_shader_context.rs` vs Eden `src/video_core/renderer_opengl/gl_shader_context.h`

### Intentional differences

- A frontend-supplied `SharedContextFactory` replaces Eden's direct `EmuWindow::CreateSharedContext`
  call, preserving one independently owned shared GL context per shader worker.
- `Context::Drop` explicitly controls the self-referential lifetime that Eden expresses with
  `GraphicsContext::Scoped`: pools are destroyed while the context is current, `DoneCurrent` runs,
  then the boxed context is destroyed.

### Unintentional differences (to fix)

- Resolved: `ShaderPools::Drop` now releases live flow blocks, IR blocks, and instructions in Eden's
  reverse-member destruction order. Automatic Rust field destruction previously visited the three
  pools in declaration order.

## 2026-08-26 — `src/video_core/src/gpu.rs` vs Eden `src/video_core/gpu.{h,cpp}`

### Intentional differences

- `Mutex<Option<Box<dyn GraphicsContext + Send>>>` replaces Eden's thread-confined
  `unique_ptr<GraphicsContext>` while preserving lazy construction and GPU ownership.
- The renderer exposes shared-context construction through the split-crate `RendererBase` trait;
  Eden reaches the same frontend owner through `RendererBase::GetRenderWindow()`.

### Unintentional differences (to fix)

- Resolved: `Gpu::obtain_context` and `Gpu::release_context` now reproduce Eden's lazy shared-context
  creation, `MakeCurrent`, and `DoneCurrent` lifecycle. The CPU context was previously absent.
- Resolved: `RequestComposite` now counts and registers the exact fence vector supplied by its
  caller. The removed negative-ID filter changed Eden's invalid-input behavior and counter size.

### Missing items

- `ReleaseChannel` remains deliberately unimplemented exactly as in Eden.

## 2026-08-26 — `src/core/src/{cpu_manager.rs,gpu_core.rs}` vs Eden `src/core/cpu_manager.cpp` and `src/video_core/gpu.h`

### Intentional differences

- `GpuCoreInterface` bridges the crate boundary that Eden does not have. Its context methods forward
  directly to the concrete GPU owner.
- Context acquisition skips only null-system unit-test kernels; an initialized system still
  requires a GPU exactly where Eden dereferences `system.GPU()`.

### Unintentional differences (to fix)

- Resolved: the synchronous single-core CPU thread now calls `obtain_context` after the GPU barrier,
  at the same point and under the same `!is_async_gpu && !is_multicore` condition as Eden.

## 2026-08-26 — `src/video_core/src/{renderer_base.rs,renderer_opengl/renderer_opengl.rs}` vs Eden `src/video_core/renderer_base.h` and `src/video_core/renderer_opengl/renderer_opengl.{h,cpp}`

### Intentional differences

- The frontend-provided `SharedContextFactory` is retained by `RendererOpenGL` in place of Eden's
  retained `EmuWindow&`; both create a fresh shared context per request.
- Non-OpenGL renderers inherit a no-op graphics context, matching their frontend dummy-context
  behavior without adding backend-specific state.

### Unintentional differences (to fix)

- Resolved: OpenGL shared-context creation is now non-optional at renderer construction and remains
  available to `GPU::ObtainContext`, rather than being consumed solely by shader workers.

## 2026-08-26 — `src/common/src/address_space.rs` vs Eden `src/common/address_space.{h,inc}` (`FlatAllocator` prerequisite)

### Intentional differences

- Rust specializes the generic address-space template as `FlatAllocatorBool<u32/u64>` for the two
  allocator instantiations used by Ruzu. Mutex ownership and bool-backed block storage remain local
  to this specialization.

### Unintentional differences (to fix)

- Resolved: the linear fixed-block search now uses Eden's literal
  `gap < size || predecessor.Mapped()` selection condition. The former Rust predicate selected a
  conventional free gap instead, changing the address returned when a request straddled a fixed
  mapping.
- Resolved: all guest-VA additions and subtractions in the bool-backed map/allocator now use
  explicit wrapping operations, preserving the unsigned C++ bit patterns in debug builds.

## 2026-08-26 — `src/video_core/src/host1x/gpu_device_memory_manager.rs` vs Eden `src/core/device_memory_manager.{h,inc}` and `src/video_core/host1x/gpu_device_memory_manager.{h,cpp}`

### Intentional differences

- Rust owns the active Maxwell specialization in `video_core/host1x` because moving the generic
  implementation into the `core` crate would introduce the existing `core`/`video_core` dependency
  cycle. Dense Rust tables replace Eden's reserved `VirtualBuffer` arrays, and atomics protect the
  shared translation cache used through `Arc`.
- Host pointers are range-checked against the captured device-memory allocation before indexing the
  dense physical table. Eden relies on the invariant that every pointer belongs to that allocation;
  the Rust check prevents an invalid pointer from becoming an out-of-bounds table access.
- Test-only host-pointer mapping and callbacks support reduced fixtures. Runtime ASID mappings use
  registered process memory and the same physical-base-relative encoding as Eden.

### Unintentional differences (to fix)

- Resolved: `Allocate` and `Free` now forward the exact byte size to `FlatAllocator` instead of
  silently rounding to 4 KiB, and `Free` no longer ignores address zero.
- Resolved: the missing `AllocateFixed`, `ApplyOpOnPAddr`,
  `GetPhysicalRawAddressFromDAddr`, `HAS_FLUSH_INVALIDATION`, and `AS_BITS` API pieces are present;
  buffer-cache code consumes `AS_BITS` from its upstream owner instead of duplicating `34`.
- Resolved: `UpdatePagesCachedCountNoLock` executes Eden's acquire fence before reading backing
  metadata, including for a zero-size request. Range coalescing and span bounds now preserve
  unsigned wrapping arithmetic.
- Resolved: the module no longer describes the dense physical/device table implementation as an
  unfinished SMMU subset.

## 2026-08-26 — `src/video_core/src/gpu_thread.rs` and `src/video_core/src/gpu.rs` vs Eden `src/video_core/gpu_thread.{h,cpp}` and `src/video_core/gpu.cpp`

### Intentional differences

- `Arc<AtomicBool>` plus explicit queue/condition-variable wakeups implement the stop-token portion
  of Eden's `std::jthread`; `ThreadManager::shutdown` joins before renderer and scheduler teardown.
- Rust retains stable rasterizer, GPU, graphics-context, and scheduler handles across the spawned
  closure because those owners cross Rust trait/crate boundaries. Eden captures references to the
  same owners directly.
- `last_fence` is atomic for shared Rust access, although every mutation remains serialized by
  `write_lock` exactly as in Eden. The worker body is a same-file helper rather than an inline
  closure so it can borrow the shared synchronization state safely.

### Unintentional differences (to fix)

- Resolved: `SynchState` now owns the upstream `BoundedSPSCQueue`; the removed MPSC wrapper had an
  extra producer mutex despite `write_lock` already serializing producers.
- Resolved: `is_async` remains owned only by `Gpu` and is passed to every thread-manager operation,
  matching Eden's method signatures and avoiding duplicated mode state.
- Resolved: stop now wakes a caller blocked on a fence, the wait predicate observes the stop flag,
  and the worker checks stop immediately after `PopWait` before dispatching the returned command.
- Resolved: the worker calls `SetCurrentThreadToPerformanceCores`, requires the renderer context and
  rasterizer installed by `StartThread`, and treats both `monostate` and the non-queued combined
  flush/invalidate command as assertion failures.
- Resolved: fence increment uses unsigned wrapping semantics and the non-upstream GPU-thread profile,
  submit timing, trace emissions, CLI environment switch, and dump hooks were removed from the hot
  path.

## 2026-08-26 — `src/video_core/src/host1x/codecs/vp9.rs` vs Eden `src/video_core/host1x/codecs/vp9.{h,cpp}`

### Intentional differences

- Rust stores the range encoder bytes directly in a `Vec<u8>` instead of wrapping Eden's
  `Common::Stream`; indexed carry propagation preserves the same seek/peek/write order.
- `DecoderImpl::compose_frame` returns an owned `Vec<u8>` across the Rust trait boundary instead of
  Eden's span into `frame_scratch`; header and payload concatenation order is unchanged.

### Unintentional differences (to fix)

- Resolved: probability remapping and range normalization now use Eden's literal arithmetic and
  `countl_zero` formulas. The removed `MAP_LUT` and `NORM_LUT` constants had no upstream owner and
  obscured the bitstream comparison.
- Resolved: unsigned range arithmetic and bit extraction preserve Eden's `u32` wrapping and shift
  semantics explicitly, including in debug builds.

## 2026-08-26 — `src/video_core/src/host1x/sync_manager.rs` vs Eden `src/video_core/host1x/sync_manager.{h,cpp}`

### Intentional differences

- Rust expresses Eden's default `SyncptIncr(..., done = false)` constructor argument explicitly at
  its two call sites and uses `Vec::drain` for the same completed-prefix erase operation.
- The upstream `increment_lock` member is retained but intentionally remains unacquired, matching
  the current Eden implementation rather than inventing synchronization behavior.

### Unintentional differences (to fix)

- Resolved: the previously missing `SyncptIncr` and `SyncptIncrManager` owners now live in the
  corresponding `host1x/sync_manager.rs` module. Handle allocation, ordered completion, guest/host
  increment order, and prefix erasure follow Eden literally.

## 2026-08-26 — `src/video_core/src/host_shaders/mod.rs` and source exports vs Eden `src/video_core/host_shaders/`

### Intentional differences

- Rust groups source strings by shader stage and generates Vulkan SPIR-V from `build.rs`; Eden's
  CMake helpers generate C++ headers. The source files passed to the compilers are the same.
- Three copied shader files retain a final newline absent from Eden. GLSL parsing and generated
  instructions are unaffected.

### Unintentional differences (to fix)

- Resolved: vertex shaders and `opengl_smaa.glsl` are no longer duplicated as large Rust raw
  strings. Their exported constants now use `include_str!`, like the compute and fragment exports,
  so each upstream shader has a single auditable source owner.
- Resolved: `opengl_present.vert` now exists beside the other shader sources instead of living only
  inside `vertex_shaders.rs`.

## 2026-08-26 — `src/video_core/src/texture_cache/image_base.rs` vs Eden `src/video_core/texture_cache/image_base.{h,cpp}`

### Intentional differences

- Rust uses `Vec` for Eden's inline-capacity `small_vector` slice metadata. Element ordering and
  lookup behavior are unchanged; only small-allocation strategy differs.

### Unintentional differences (to fix)

- Resolved: `ImageBase::null` now retains the in-class `CPU_MODIFIED` default exactly like Eden's
  empty `NullImageParams` constructor.
- Resolved: `layer_mip_offset` now follows C++ usual arithmetic conversions for its mixed
  `s32`/`u32` division and remainder. Offsets with bit 31 set no longer take the signed Rust path.
- Resolved: the missing `has_scaled` accessor is present, address/range calculations preserve
  unsigned wrapping, and alias block rounding uses the upstream-owned common `div_ceil` helper.

## 2026-08-26 — `src/video_core/src/texture_cache/image_info.rs` vs Eden `src/video_core/texture_cache/image_info.{h,cpp}`

### Intentional differences

- Rust represents Eden's anonymous `block`/`pitch` union as `TilingMode`. Accessors expose zeros for
  the inactive variant, matching the zero-initialized bytes used by every upstream constructor.
- The file-local `fail_soft` helper implements Eden's `ASSERT`/`UNIMPLEMENTED` policy using the same
  `use_debug_asserts` setting because Rust has no C++ assertion macro expansion.

### Unintentional differences (to fix)

- Resolved: invalid MSAA values now report the assertion and fall back to 1x, and unknown DMA byte
  sizes return `PixelFormat::Invalid`, matching Eden instead of panicking unconditionally.
- Resolved: TIC type/tiling checks and render-target/zeta dimension-control checks are fail-soft by
  default. Invalid inputs continue through the same constructor branches as Eden.
- Resolved: multisample width and height expansion now preserves unsigned C++ wrapping, and the
  obsolete placeholder description for the already-ported `PixelFormat` owner was removed.

## 2026-08-26 — `src/video_core/src/texture_cache/image_view_base.rs` vs Eden `src/video_core/texture_cache/image_view_base.{h,cpp}`

### Intentional differences

- Rust's split base/backend slot retains the constructor's `ImageViewInfo` beside, rather than
  inside, `ImageViewBase` so an OpenGL or Vulkan backend view can be rematerialized after its
  derived payload is released. Eden constructs the derived object directly from the same info and
  therefore does not need this Rust-only lifetime adapter.
- The file-local `fail_soft` helper implements Eden's `ASSERT_MSG` policy through the same
  `use_debug_asserts` setting because Rust has no C++ assertion macro expansion.

### Unintentional differences (to fix)

- Resolved: `ImageViewBase` no longer owns non-upstream swizzle bytes or an `is_render_target`
  helper. OpenGL and Vulkan constructors now consume `ImageViewInfo` directly, preserving Eden's
  method and state ownership.
- Resolved: compatibility and buffer-type assertions are fail-soft by default and run after base
  initialization, in the same lifecycle position as Eden.
- Resolved: Vulkan framebuffer subresource ranges use the base view format's full aspect mask;
  descriptor swizzle affects only initial Vulkan image-view creation, as in Eden.
- Resolved: depth/stencil component swizzles now replace unsupported integer/float `ONE` sources
  with `ZERO`, guarded by the same maintenance5 property as Eden.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_device.rs` maintenance5 prerequisite vs Eden `src/video_core/vulkan_common/vulkan_device.{h,cpp}`

### Intentional differences

- The workspace's ash 0.37 bindings predate `VK_KHR_maintenance5`, so its feature and property
  payloads are declared locally with their Vulkan ABI structure-type values. They remain in the
  corresponding device owner and participate in the same feature/property `pNext` chains.
- Rust retains the four maintenance5 property answers as booleans after physical-device discovery
  instead of retaining a self-referential raw property-chain node inside `Device`.

### Unintentional differences (to fix)

- Resolved: maintenance5 is queried, suitability-filtered, enabled on the logical device, and
  exposed through the upstream `IsKhrMaintenance5Supported`, `SupportsPolygonModePointSize`,
  `SupportsDepthStencilSwizzleOne`, and `SupportsEarlyFragmentTests` counterparts.

## 2026-08-26 — `src/video_core/src/texture_cache/image_view_info.rs` vs Eden `src/video_core/texture_cache/image_view_info.{h,cpp}`

### Intentional differences

- Rust decodes the stored swizzle byte through the canonical enum instead of C++ `static_cast`.
  The unnamed three-bit TIC value is represented explicitly as `SwizzleSource::Invalid`, while
  bytes outside the TIC range report Eden's fail-soft assertion and use that same invalid path.
- The file-local `fail_soft` helper implements Eden's `ASSERT` policy through the shared
  `use_debug_asserts` setting because Rust has no C++ assertion macro expansion.

### Unintentional differences (to fix)

- Resolved: removed the duplicate file-local `SwizzleSource`; `ImageViewInfo` now re-exports and
  returns the canonical type owned by `textures/texture.rs`, matching Eden's ownership.
- Resolved: the missing `Texture1DArray` height assertion is present, all constructor assertions
  are fail-soft by default, and invalid texture types retain the already-initialized default view
  type instead of panicking unconditionally.
- Resolved: mip-count subtraction and cube-array layer multiplication preserve C++ unsigned
  wrapping instead of overflowing under Rust debug arithmetic.

## 2026-08-26 — `src/video_core/src/textures/texture.rs` swizzle prerequisite vs Eden `src/video_core/textures/texture.h`

### Intentional differences

- Rust names raw TIC value 1 `Invalid` so the safe enum can represent every value of Eden's
  three-bit `SwizzleSource` bitfield. Eden leaves that value unnamed but C++ enum casts still carry
  it to backend validation.

### Unintentional differences (to fix)

- Resolved: `SwizzleSource::from_raw` no longer rejects the representable raw value 1 before the
  texture-cache and backend validation paths can reproduce Eden's behavior.

## 2026-08-26 — backend invalid-swizzle handling vs Eden `renderer_{opengl,vulkan}/{gl_texture_cache,maxwell_to_vk}.cpp`

### Intentional differences

- Rust spells Eden's fall-through assertion branches as explicit `SwizzleSource::Invalid` match
  arms because exhaustive matching is required for the safe enum.

### Unintentional differences (to fix)

- Resolved: invalid OpenGL swizzles now report and return `GL_NONE`; invalid Vulkan swizzles report
  and return the zero-initialized `VkComponentSwizzle`, matching Eden's fallback results.

## 2026-08-26 — removed `src/video_core/src/engines/inline_to_memory.rs` vs Eden engine ownership

### Unintentional differences (to fix)

- Resolved: removed the test-only `InlineToMemory` engine and its module declaration. It duplicated
  A140/P2MF state behind a Rust-only register engine, and its block-linear mode incorrectly fell
  back to a pitched linear write.
- Verified: runtime A140 single- and multi-method dispatch already targets `KeplerMemory`, whose
  matching `engine_upload::State` owner performs Eden's linear rasterizer upload or block-linear
  `swizzle_subrect` path.

## 2026-08-26 — `src/video_core/src/invalidation_accumulator.rs` vs Eden `src/video_core/invalidation_accumulator.h`

### Intentional differences

- `MemoryManager::flush_caching` temporarily moves the accumulator out of `self` so its callback
  can inspect the remaining memory-manager state without overlapping Rust borrows. Callback order,
  accumulator reset, and subsequent rasterizer invalidation remain identical to Eden.

### Unintentional differences (to fix)

- Resolved: removed the Rust-only `has_collected` and `last_collection` state. Address zero is once
  again Eden's empty sentinel, including its loss of an invalidation range aligned to zero.
- Resolved: restored the single `invalidate_all` operation that invokes buffered ranges, invokes
  the current range, clears all state, and returns the upstream boolean in that exact order.
- Resolved: range-end, alignment, and accumulated-size arithmetic now wraps as unsigned C++
  arithmetic rather than panicking on Rust debug overflow.
- Resolved: `MemoryManager::flush_caching` consumes the unified API instead of relying on the
  non-upstream `any_accumulated`/`callback`/`clear` protocol.

## 2026-08-26 — `src/video_core/src/engines/kepler_compute.rs` vs Eden `src/video_core/engines/kepler_compute.{h,cpp}`

### Intentional differences

- Rust reads the raw 0x100-byte QMD into `LaunchParamsLayout`, then exposes decoded bitfields
  through `LaunchParams`; Eden overlays bitfields directly on its raw struct. The raw read size,
  offsets, and every exposed field are unchanged.
- The rasterizer receives a synchronous `DispatchCall` snapshot of the current engine state.
  Re-reading the same engine through the channel's raw pointer while `call_method` holds `&mut
  KeplerCompute` would create forbidden Rust aliasing; all snapshot fields come from Eden's
  engine-owned registers immediately before the call.
- The upload state receives an owner-local register snapshot instead of retaining a
  self-referential pointer into `regs`. Method bounds are checked before Rust array access rather
  than relying on C++'s asserted indexing contract.

### Unintentional differences (to fix)

- Resolved: `get_tic_entry` and `get_tsc_entry` are compiled as runtime-private methods instead of
  existing only in test builds. Their pool offset arithmetic now preserves unsigned wrapping.

## 2026-08-26 — `src/video_core/src/engines/kepler_memory.rs` vs Eden `src/video_core/engines/kepler_memory.{h,cpp}`

### Intentional differences

- Rust keeps the 0x7F-word register storage as a `repr(C)` array and mechanically materializes an
  `engine_upload::Registers` snapshot. This avoids retaining Eden's self-referential
  `Upload::State` reference into the owning register union while preserving every field offset.
- Rust checks the method index before array access; Eden asserts the contract and then indexes the
  C++ array. Valid command-stream behavior is identical.

### Unintentional differences (to fix)

- Resolved: `NUM_REGS` is owned by `Regs`, matching Eden, and the upload/interface implementation
  state is private rather than exposed as public engine state.
- Resolved: `bind_rasterizer` directly resets and sets the two constant execution-mask positions,
  and sink consumption no longer silently drops out-of-range methods.
- Resolved: the default multi-method path uses wrapping `u32` subtraction for
  `methods_pending - i`, matching C++ instead of saturating at zero.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/present/layer.rs` vs Eden `src/video_core/renderer_vulkan/present/layer.{h,cpp}`

### Intentional differences

- Rust retains Eden's allocator and scheduler references as `NonNull` and the shared device-memory
  owner as `Arc`; the enclosing renderer owns all three longer than every `Layer`.
- Scheduler closures copy the command's Vulkan handles instead of capturing `this`, because Rust
  requires queued closures to be `'static`. `resource_ticks` still waits for every such command
  before any corresponding allocation is released.
- Raw Ash image views and the descriptor pool are destroyed explicitly. `AllocatedImage` and
  `AllocatedBuffer` provide the RAII ownership that Eden obtains from its Vulkan wrappers.

### Unintentional differences (to fix)

- Resolved: the two Rust draw entry points were merged back into the single upstream-owned
  `configure_draw`, and every helper again receives the high-level `Device` at the same boundary as
  Eden.
- Resolved: framebuffer helpers now receive `FramebufferConfig`, use unsigned wrapping arithmetic,
  preserve Eden's fail-soft unknown-format fallback, and the canonical settings `AntiAliasing`
  enum owns the cached setting.
- Resolved: `create_raw_images` is a separate Layer method again. Raw images and the staging buffer
  now own VMA allocations and are released after Eden's per-image tick waits instead of remaining
  retained by the global allocator.
- Resolved: refresh no longer clears `anti_alias_setting`; it resets only the anti-alias variant,
  and raw image views retain Eden's replacement/destructor lifetime rather than being cleared by
  `release_raw_images`.
- Resolved: a scope guard updates the resource tick on every exit from `configure_draw`, including
  panic unwinding, matching Eden's `SCOPE_EXIT` lifecycle.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/present/fsr.rs` vs Eden `src/video_core/renderer_vulkan/present/fsr.{h,cpp}`

### Intentional differences

- The retained allocator reference is represented by `NonNull<MemoryAllocator>`, and a raw Ash
  device is retained only to destroy raw non-image Vulkan handles in `Drop`.
- Queued commands copy image handles and the logical device rather than borrowing the FSR object;
  `UploadImages` still finishes before returning and Layer's resource tick protects draw commands.

### Unintentional differences (to fix)

- Resolved: construction, all creation helpers, `upload_images`, `update_descriptor_sets`, and
  `draw` receive Eden's high-level `Device`; shader capability selection is owned by
  `create_shaders` again.
- Resolved: EASU and RCAS images are owning VMA allocations, so their lifetime follows the FSR
  object instead of the global allocator.
- Resolved: the stage enum, count, and per-image resources are private like Eden's nested members.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/present/sgsr.rs` vs Eden `src/video_core/renderer_vulkan/present/sgsr.{h,cpp}`

### Intentional differences

- Rust retains the allocator reference as `NonNull` and the logical Ash device solely for explicit
  raw-handle destruction. Command closures copy handles to satisfy the scheduler's `'static`
  contract.

### Unintentional differences (to fix)

- Resolved: `draw`, `upload_images`, and `update_descriptor_sets` receive the high-level `Device`
  and use its logical handle at runtime.
- Resolved: SGSR images are owning VMA allocations and both upstream-owned `memory_allocator` and
  `edge_dir` state are retained instead of being discarded after construction.
- Resolved: `Drop` now releases per-image framebuffers, views, allocations, and descriptor-handle
  storage before sampler, render pass, pipeline, shaders, layouts, and descriptor pool, matching
  the effective reverse declaration order of Eden's RAII members.

### Missing items

- Eden declares but does not define or call `Initialize`; there is no executable method to port.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/present/fxaa.rs` vs Eden `src/video_core/renderer_vulkan/present/fxaa.{h,cpp}`

### Intentional differences

- Rust explicitly destroys raw framebuffers, views, pipelines, layouts, shaders, sampler, and
  render pass; owning images then release through VMA. The retained Ash device exists only for
  that `Drop` implementation.
- Queued commands copy raw handles rather than borrowing the pass across the scheduler boundary.

### Unintentional differences (to fix)

- Resolved: the constructor and all creation helpers receive Eden's high-level `Device` rather
  than a stored raw device.
- Resolved: every per-frame FXAA image owns its VMA allocation and is released when the pass is
  replaced by Layer.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/present/smaa.rs` vs Eden `src/video_core/renderer_vulkan/present/smaa.{h,cpp}`

### Intentional differences

- Eden's allocator reference is retained as `NonNull`; the raw Ash device is retained only for
  explicit destruction of non-image Vulkan handles. Scheduler closures copy handles instead of
  borrowing `self`.

### Unintentional differences (to fix)

- Resolved: the constructor and all creation helpers receive the high-level `Device`, matching
  Eden's ownership boundary.
- Resolved: both static lookup images and all three dynamic images per frame own VMA allocations;
  replacing the SMAA pass now releases them with the pass.
- Resolved: SMAA's nested enums, counts, and `Images` structure are private like upstream.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/present/util.rs` and `src/video_core/src/vulkan_common/vulkan_memory_allocator.rs` vs Eden presentation utilities and `vulkan_memory_allocator.{h,cpp}`

### Intentional differences

- Rust represents Eden's move-only `vk::Image` and `vk::Buffer` wrappers as `AllocatedImage` and
  `AllocatedBuffer`, retaining the externally synchronized VMA allocator in `Arc<Mutex<_>>`.
- Allocation failures return `VulkanError`; presentation helpers convert them to the same fatal
  construction failure that Eden obtains from `vk::Check` exceptions.

### Unintentional differences (to fix)

- Resolved: `create_owned_image` now uses VMA with `WITHIN_BUDGET`,
  `AUTO_PREFER_DEVICE`, and preferred `DEVICE_LOCAL`, exactly matching Eden's `CreateImage`
  allocation policy.
- Resolved: the allocator-retained raw-image compatibility path was removed. The single
  `create_wrapped_image` helper now returns the owning image wrapper used by presentation frames
  and passes.
- Resolved: Layer staging and `upload_image` staging use the VMA-backed owning buffer path instead
  of dedicated Vulkan allocations.
- Resolved: `transition_image_layout` uses Eden's graphics-and-compute stage mask on both sides of
  the barrier instead of the broader `ALL_COMMANDS` mask.

### Missing items

- GPU allocation/deallocation logging remains unavailable because Ruzu has not ported Eden's GPU
  logging subsystem; this does not alter allocation policy or resource lifetime.

## 2026-08-26 — `src/video_core/src/macro.rs` vs Eden `src/video_core/macro.{h,cpp}`

### Intentional differences

- Rust uses inline `macro_hle`, `macro_interpreter`, and x86-64-only `macro_jit_x64` scopes inside
  the single physical `macro.rs` counterpart. They provide conditional compilation and name
  scoping without moving any upstream-owned implementation into another source file.
- `AnyCachedMacro::execute` receives the active `Maxwell3D` as a non-owning raw pointer,
  corresponding to Eden's non-null reference. The pointer is never retained by an HLE,
  interpreter, or JIT cache entry; this permits the enclosing Maxwell owner to pass itself through
  Rust's enum dispatch.
- The JIT state's second pointer addresses Maxwell's boxed register array instead of storing Eden's
  `Core::System*`. Rust cannot emit a stable member offset into a non-`repr(C)` `Maxwell3D`; method
  sends use the engine's owner-local system bridge, while register reads remain direct native
  indexed loads.
- Invalid macro-code and parameter indexing terminates through Rust bounds checks after reporting
  the corresponding assertion. Eden's fail-soft assertion would otherwise continue into invalid
  C++ span access, which has no defined behavior to preserve.

### Unintentional differences (to fix)

- Resolved: the former `macro_engine/` split and duplicate root `macro_interpreter.rs` were
  consolidated into the single counterpart of Eden's `macro.h`/`macro.cpp`.
- Resolved: `AnyCachedMacro` is one Rust enum mirroring Eden's `std::variant`; `CacheInfo` no longer
  stores two boxed programs plus a discriminator, and `get_hle_program` is again a free function
  rather than state owned by `MacroEngine`.
- Resolved: cached HLE, interpreter, and JIT programs receive the current Maxwell owner on every
  execution instead of retaining callbacks or the first engine pointer seen at compilation.
- Resolved: HLE clear depth, transform-feedback byte-count draws, refreshed-topology fallbacks,
  wrapping indirect sizes, replacement attributes, and cleanup ordering now follow the matching
  `HLE_*::Execute`/`Fallback` implementations.
- Resolved: interpreter assertions use Eden's fail-soft policy for validly recoverable cases, and
  the x86-64 emitter follows Eden's optimizer, delay-slot, method-send, and parameter-fetch paths.

## 2026-08-26 — `src/video_core/src/engines/maxwell_dma.rs` vs Eden `src/video_core/engines/maxwell_dma.{h,cpp}`

### Intentional differences

- Rust represents the register union as a zero-initialized 0x800-word array plus typed accessors.
  DMA fallback writes are collected as `PendingWrite` values because the Rust engine boundary does
  not expose Eden's scoped guest-memory guards; destination data is read first where the upstream
  cached-write guard preserves bytes outside the copied subrectangle.

### Unintentional differences (to fix)

- Resolved: `call_multi_method` now consumes exactly `amount` words and derives `is_last_call` from
  the wrapping unsigned `methods_pending - i <= 1` expression used by Eden.
- Resolved: launching DMA now rejects non-`NONE` interrupt types before selecting or executing a
  copy path, matching the assertion at the head of Eden's `Launch`.

## 2026-08-26 — `src/video_core/src/renderer_opengl/maxwell_to_gl.rs` vs Eden `src/video_core/renderer_opengl/maxwell_to_gl.h`

### Intentional differences

- Rust conversion functions accept the raw register `u32` encodings where the C++ signatures use
  scoped enum types; every accepted discriminant and fallback result remains the same.
- The mirror-clamp extension path queries the current OpenGL extension list once through the loaded
  bindings instead of reading glad's generated `GL_EXT_texture_mirror_clamp` global.

### Unintentional differences (to fix)

- Resolved: `front_face` now accepts Eden's `0x900`/`0x901` Maxwell encodings instead of unrelated
  compact values.
- Resolved: `cull_face` now accepts Eden's `0x404`/`0x405`/`0x408` Maxwell encodings.
- Resolved: floating-point `Size_R16_G16_B16` vertex attributes (`0x05`) now map to
  `GL_HALF_FLOAT` together with the other three 16-bit floating formats.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/maxwell_to_vk.rs` vs Eden `src/video_core/renderer_vulkan/maxwell_to_vk.{h,cpp}`

### Intentional differences

- Rust uses normalized typed Maxwell enums after raw register decoding; Eden's conversion
  functions accept the corresponding C++ scoped enums directly.
- `primitive_topology` omits Eden's unused `Device` parameter. The sampler and vertex-format paths
  retain the device reference because it is part of their upstream ownership and behavior.

### Unintentional differences (to fix)

- Resolved: `Sampler::wrap_mode` no longer sends the invalid `0xcafe` address mode on Nvidia.
  `Clamp` selects edge for nearest filtering and border for linear filtering on every driver.
- Resolved: `MirrorOnceClampOGL` returns mirror-clamp-to-edge without an extra warning.
- Resolved: `vertex_format` again owns the `Device` lookup and always calls
  `get_supported_format` with `VERTEX_BUFFER` and `FormatType::Buffer`; static and dynamic vertex
  input creation now both pass their live device owner.
- Resolved: the obsolete Nvidia and scaled-format snapshots were removed from the texture-cache
  and rasterizer constructors once those decisions returned to the upstream-owned `Device` paths.

## 2026-08-26 — `src/video_core/src/memory_manager.rs` vs Eden `src/video_core/memory_manager.{h,cpp}`

### Intentional differences

- The Rust owner graph keeps the device-memory manager in an `Arc` and places the page-table
  implementation behind the public `MemoryManager` adapter used by channel mutexes. Eden stores
  direct references and a raw rasterizer pointer in one C++ class.
- Rasterizer notifications may be deferred by the nvdrv adapter and replayed after releasing the
  memory-manager mutex. This preserves Eden's effective lock ordering while avoiding the Rust
  CPU/GPU-thread ABBA cycle.
- `for_each_mapped_device_segment` is a mechanical borrow-checker adapter around Eden's nested
  `MemoryOperation` calls. It invokes the rasterizer immediately, in the same page order and with
  the same chunk sizes; it does not allocate or merge ranges.

### Unintentional differences (to fix)

- Resolved: `FlushRegion`, `InvalidateRegion`, and `IsMemoryDirty` no longer coalesce physically
  adjacent small pages through `GetSubmappedRangeImpl`; each mapped page produces the same
  rasterizer call as Eden, and dirty checking stops on the first positive result.
- Resolved: the continuous-big-page branch of `IsGranularRange` uses Eden's exact
  `(page_index & big_page_mask) + size` calculation.
- Resolved: `GetID` and `ModifyGPUMemory` now read the same identifier stored by the actual page
  table owner instead of maintaining separate outer and inner identifiers.
- Resolved: `HAS_FLUSH_INVALIDATION` is owned by `MemoryManager`, and the guest-memory adapter
  references that constant instead of duplicating the literal.

### Missing items

- No upstream public operation is missing after auditing map/sparse-map/unmap, address translation,
  scalar and block access, range queries, cache invalidation, copy, page-kind/layout queries, and
  span access.

## 2026-08-26 — `src/video_core/src/vulkan_common/nsight_aftermath_tracker.rs` vs Eden `src/video_core/vulkan_common/nsight_aftermath_tracker.{h,cpp}`

### Intentional differences

- Ruzu has no `HAS_NSIGHT_AFTERMATH` build configuration or proprietary NVIDIA SDK bindings, so it
  implements Eden's header-only unsupported-build path. The SDK-enabled DLL loading, callbacks,
  shader dumps, crash dumps, and JSON decoding remain platform tooling outside this build.

### Unintentional differences (to fix)

- Resolved: the unsupported-build tracker is now stateless like Eden's `#ifndef
  HAS_NSIGHT_AFTERMATH` class instead of retaining an unused mutex and initialization flag.
- Resolved: the non-upstream `is_initialized` method was removed, and the module documentation now
  names the Eden source tree.

### Missing items

- The proprietary `HAS_NSIGHT_AFTERMATH` implementation is unavailable; the no-SDK constructor,
  destructor, and `SaveShader` behavior are present.

## 2026-08-26 — removed `src/video_core/src/renderer_null/null_backend.rs`; factory comparison with Eden `src/video_core/video_core.{h,cpp}`

### Intentional differences

- Eden's anonymous `CreateRenderer` owns backend selection inside `video_core.cpp`. Ruzu's frontend
  constructs the platform graphics context and concrete renderer before binding it through
  `video_core::video_core::create_gpu`, because the `video_core` crate does not depend on the SDL/GTK
  window implementation.

### Unintentional differences (to fix)

- Resolved: the unused `NullBackend`, `BackendType`, and generic `GpuBackend` abstraction was
  removed. It had no Eden file or call site and duplicated the real `RendererBase` factory path.
- Resolved: `renderer_null/mod.rs` now only dispatches the two upstream-owned null renderer modules
  and names the Eden source tree.

### Missing items

- The null selection arm itself is present in the frontend renderer factory and constructs
  `renderer_null::RendererNull`; no separate null-backend object exists upstream.

## 2026-08-26 — `src/video_core/src/renderer_null/null_rasterizer.rs` vs Eden `src/video_core/renderer_null/null_rasterizer.{h,cpp}`

### Intentional differences

- Ruzu stores the syncpoint manager and a GPU-tick callback instead of Eden's `Tegra::GPU&`,
  following the existing Rust renderer ownership boundary while preserving the two uses of that
  reference: `GetTicks()` and Host1x syncpoint increments.
- Test-only inline-upload and surface-copy controls exercise callers without changing production
  behavior; non-test builds retain Eden's no-op upload and unconditional successful surface copy.

### Unintentional differences (to fix)

- Resolved: `Query` now writes through the currently bound channel's `MemoryManager`, in Eden's
  ticks-then-payload order, instead of routing GPU addresses through a global raw-pointer
  translation callback and the CPU-address guest-memory writer.
- Resolved: the null draw, texture draw, clear, and compute-dispatch paths are true no-ops without
  extra trace logging, and `LoadDiskResources` is explicitly implemented by its upstream owner.
- Resolved: flush-area alignment uses wrapping unsigned arithmetic like C++, stale source-tree
  references were corrected, and duplicate parameterless DMA image helpers were removed.

## 2026-08-26 — `src/video_core/src/host1x/nvdec_common.rs` vs Eden `src/video_core/host1x/nvdec_common.h`

### Intentional differences

- Rust exposes the anonymous C++ register union as one `repr(C)` `u64` array plus named constants
  and accessors. This avoids unsafe union reads while retaining every named field's exact slot and
  the complete raw register view used by method dispatch.
- `VideoCodec` is a transparent `u64` newtype with upstream-named associated constants rather than
  a Rust enum, because the guest register may contain an unnamed value that C++ preserves through
  `static_cast`.

### Unintentional differences (to fix)

- Resolved: unknown codec values are no longer collapsed to `None`; their complete 64-bit pattern
  reaches NVDEC and FFmpeg callers unchanged.
- Resolved: `ControlParams` owns all five upstream bitfields, and constants/accessors now cover
  every named NVDEC register, including the H.264, VP8, HVEC, and VP9 scratch-buffer fields that
  were previously absent from the Rust surface.

## 2026-08-26 — `src/video_core/src/host1x/nvdec.rs` vs Eden `src/video_core/host1x/nvdec.{h,cpp}`

### Intentional differences

- The Rust Host1x owner supplies `Arc` handles for the frame queue and memory manager instead of a
  C++ `Host1x&`; `ProcessMethodHook` replaces `CDmaPusher` inheritance. Construction and `Drop`
  still open and close the same frame-queue identifier.
- Eden's inherited `Decoder::Decode()` is represented by the decoder-owned Rust free function
  invoked with each concrete codec variant; the decode implementation remains owned by
  `host1x/codecs/decoder.rs`.

### Unintentional differences (to fix)

- Resolved: the decoder is again a concrete H264/VP8/VP9/None sum type matching Eden's
  `std::variant`, and `Execute` dispatches each alternative explicitly and reports the monostate.
- Resolved: the unused `wait_needed` state and non-upstream 32 ms execution delay were removed;
  only Eden's 8 ms delay for disabled NVDEC emulation remains.
- Resolved: per-frame trace instrumentation absent from Eden was removed, and raw unknown codec
  values are preserved by the corrected `nvdec_common` representation.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/pipeline_helper.rs` vs Eden `src/video_core/renderer_vulkan/pipeline_helper.h`

### Intentional differences

- `DeviceReference` provides the stable non-owning device relationship represented by Eden's
  `const Device*`, and Rust `Vec` replaces the inline-capacity `small_vector` owners.
- A missing sampler or image-view slot falls back to the renderer's null resources because Rust's
  typed slot API exposes lookup failure explicitly; valid descriptor paths follow Eden's handle
  selection and cursor order.

### Unintentional differences (to fix)

- Resolved: rescaling/render-area constants and layouts are no longer duplicated in
  `pipeline_helper.rs`. The helper and the graphics/compute consumers now use the definitions owned
  by `shader_recompiler/backend/spirv/emit_spirv.rs`, matching Eden's `using` declarations.
- Verified: descriptor type order, descriptor-buffer writes, layout/template flags, push-constant
  range sizing, image/sampler fallback selection, modification tracking, and rescaling bit packing
  retain the upstream order and conditions.

## 2026-08-26 — `src/video_core/src/pte_kind.rs` vs Eden `src/video_core/pte_kind.h`

### Intentional differences

- Rust uses a transparent `u8` newtype rather than an enum so raw PTE values remain representable;
  every named constant and `is_pitch_kind` still maps directly to Eden.

### Unintentional differences (to fix)

- Resolved: removed thirteen named kinds absent from Eden and restored Eden's exact names for
  `C32_MS2_2CRA`, `C64_MS2_2CRA`, and `SMASKED_MESSAGE`.

## 2026-08-26 — `src/video_core/src/query_cache/query_cache_base.rs` vs Eden `src/video_core/query_cache/query_cache_base.h` and `query_cache.h`

### Intentional differences

- C++ template dependencies are represented by bound Rust trait-object owners; counter, cache,
  conditional-rendering, and async-flush ordering remains in `QueryCacheBase`.

### Unintentional differences (to fix)

- Resolved: address calculations and query accumulation now retain Eden's unsigned wrapping
  behavior in debug builds instead of relying on Rust's overflow-checked `+`.
- Resolved: query-cache module headers now identify the actual Eden source counterparts rather
  than the stale pre-fork project name.

### Missing items

- The generic lifecycle hooks are present. The Vulkan samples streamer still needs to be wired
  through the complete `PresyncWrites`/`SyncWrites` lifecycle in its owning backend file.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/query_cache.rs`, `vk_rasterizer.rs`, and `query_cache/query_cache_base.rs` vs Eden `src/video_core/renderer_vulkan/vk_query_cache.{h,cpp}` and `query_cache/query_cache.h`

### Intentional differences

- Vulkan query banks and reports use `Arc` ownership so fence-thread callbacks cannot outlive a
  bank. Samples reports materialize Eden's bank chain as ordered spans; transform-feedback reports
  retain a persistently mapped per-bank readback mirror instead of a movable staging-pool slice.
- The shared query-cache owner exposes the three mechanical WFI phases separately. This lets the
  Vulkan-owned streamers join the single Eden barrier pair without storing a pointer to the movable
  Rust rasterizer; `notify_wfi` itself still performs the original phase order unchanged.

### Unintentional differences (to fix)

- Resolved: samples queries now implement Eden's complete pending-sync lifecycle, accumulation
  checkpoints, reset operations, current-query replication, amendment carry, ordered bank resolve,
  mobile-driver guard, and post-presync history abandonment.
- Resolved: the accumulation buffer has Eden's transfer-source usage and is cleared during streamer
  construction; repeated WFI operations no longer recopy an unbounded history.
- Resolved: transform-feedback counters now participate in WFI guest-buffer synchronization and
  async host readback, while primitive queries reuse Eden's last byte-count query and stride when
  one exists.
- Resolved: `QueryCacheRuntime` again owns all Vulkan streamers, the conditional-rendering resolve
  buffer is created on unsupported hosts with only the supported usage flags, and guest-generated
  and host-buffer sync values share the same page grouping logic.

### Missing items

- A live Vulkan query-pool validation remains necessary because unit tests cannot execute recorded
  device commands.

## 2026-08-26 — `src/video_core/src/rasterizer_interface.rs` vs Eden `src/video_core/rasterizer_interface.h` and `rasterizer_download_area.h`

### Intentional differences

- Rust passes draw, clear, indirect-draw, and compute snapshots through the trait because the
  current backend ownership graph does not retain Eden's mutable engine pointers.
- Query-type arguments remain raw `u32` values so every five-bit hardware report value, including
  values without a named Rust enum variant, preserves its upstream bit pattern.

### Unintentional differences (to fix)

- Resolved: the rasterizer interface no longer owns a duplicate download-area structure with the
  corrected spelling `preemptive`; it re-exports the single type owned by
  `rasterizer_download_area.rs`, including Eden's `preemtive` field spelling.
- Resolved: `Maxwell3D::process_counter_reset` now maps all four clear-report values to Eden's exact
  query types instead of sending unrelated numeric counter identifiers.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/render_pass_cache.rs` vs Eden `src/video_core/renderer_vulkan/vk_render_pass_cache.{h,cpp}`

### Intentional differences

- Vulkan creation failures use `Result` instead of C++ exceptions, and raw `VkRenderPass` handles
  are destroyed explicitly by `Drop` rather than by Eden's move-only wrapper.
- Rust uses `HashMap` and growable attachment vectors in place of
  `ankerl::unordered_dense::map` and `boost::container::static_vector`; key equality and attachment
  ordering are unchanged and these containers are not externally observable.

### Unintentional differences (to fix)

- Resolved: a render-pass key is now inserted before Vulkan creation. A failed creation leaves a
  cached null handle, so later lookups return null without retrying, matching Eden's
  `try_emplace`-before-`CreateRenderPass` lifecycle.

## 2026-08-26 — `src/video_core/src/renderer_null/renderer_null.rs` vs Eden `src/video_core/renderer_null/renderer_null.{h,cpp}` and `renderer_base.{h,cpp}`

### Intentional differences

- The Rust renderer receives `Arc` callbacks for `GPU::RendererFrameEndNotify` and
  `EmuWindow::OnFrameDisplayed`, plus the frontend's shared framebuffer layout, instead of retaining
  mutable references across the renderer/GPU/window ownership cycle.

### Unintentional differences (to fix)

- Resolved: non-empty Null composites now notify frame end and then frame displayed in Eden's exact
  order; they no longer increment `RendererBase::m_current_frame` or emit a backend-only trace.
- Resolved: Null construction and `refresh_base_settings` recalculate the live framebuffer layout,
  and screenshot requests use the inherited base lifecycle instead of immediately reporting
  failure.

## 2026-08-26 — `src/video_core/src/renderer_opengl/renderer_opengl.rs` vs Eden `src/video_core/renderer_opengl/renderer_opengl.{h,cpp}` and `renderer_base.cpp`

### Intentional differences

- Rust owns the device, tracker, rasterizer, presentation passes, and context through heap-stable
  owners and non-owning pointers instead of C++ references; declaration order preserves Eden's
  effective reverse destruction order.
- The frontend layout and frame notifications use shared state and callbacks to avoid retaining
  mutable GPU/window references across the Rust ownership cycle. The callback order remains
  `RendererFrameEndNotify`, rasterizer tick, swap, then `OnFrameDisplayed`.
- Construction releases the current context after GL resources are initialized so it can be moved
  to the renderer thread; Eden's frontend transfers its context through the C++ window owner.

### Unintentional differences (to fix)

- Resolved: OpenGL construction now performs the inherited `RendererBase` framebuffer-layout
  refresh before entering the backend constructor body.
- Resolved: an empty composite now returns before reading frontend layout state or making the GL
  context current, matching Eden's first operation in `Composite`.

## 2026-08-26 — `src/video_core/src/renderer_opengl/present/layer.rs` vs Eden `src/video_core/renderer_opengl/present/layer.{h,cpp}`

### Intentional differences

- Rust retains the heap-stable rasterizer through a non-owning pointer and reads device memory
  through the renderer-owned callback, replacing Eden's two C++ references without moving layer
  behavior out of its upstream owner.
- `AntiAlias` represents Eden's `variant<monostate, FXAA, SMAA>`, while `Option<FSR>` represents
  its optional FSR owner; Rust field order preserves the effective C++ destruction order.

### Unintentional differences (to fix)

- Resolved: the fallback display path now computes `framebuffer.address + framebuffer.offset` with
  unsigned wraparound. It no longer panics in debug builds where Eden's `DAddr` arithmetic wraps.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/renderer_vulkan.rs` vs Eden `src/video_core/renderer_vulkan/renderer_vulkan.{h,cpp}`

### Intentional differences

- Rust uses explicit heap-stable Vulkan owners, shared surface/swapchain synchronization, frontend
  callbacks, and `Drop` in place of Eden's reference members and `vk::` RAII wrappers. Field and
  explicit cleanup order preserve Eden's dependent-resource teardown.
- `current_framebuffer_layout_for_present` reconciles the frontend layout with the cached WSI
  extent without blocking on the present thread; this is the existing Rust/MoltenVK ownership
  adaptation and does not change the surrounding `Composite` lifecycle order.

### Unintentional differences (to fix)

- Resolved: construction now performs Eden's inherited `RendererBase` layout refresh first and
  initializes the swapchain from that live layout rather than a separate drawable-size argument.
- Resolved: `Composite` now renders screenshots and obtains its render frame before requesting the
  outside-render-pass context and reading presentation layout/swapchain state, matching Eden's
  ordering.
- Resolved: applet-layer rendering now requests an outside-render-pass operation context after
  lazy frame creation and before `DrawToFrame`.
- Resolved: screenshots retain the complete requested framebuffer layout instead of replacing its
  screen rectangle, and their byte-size multiplication preserves Eden's unsigned `u32` wraparound.

### Missing items

- Eden's optional `HAS_LSFG` frame-generation path is not built or exposed by Ruzu.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/fence_manager.rs`, `scheduler.rs`, and `vk_rasterizer.rs` vs Eden `src/video_core/renderer_vulkan/vk_fence_manager.{h,cpp}`

### Intentional differences

- Each Rust fence retains a clone of the scheduler-owned semaphore handle for thread-safe
  `IsFree` and `Wait`, while Eden retains a `Scheduler&`. `Queue` receives the live mutable
  scheduler because the complete Rust scheduler is owned by and moves with the rasterizer.

### Unintentional differences (to fix)

- Resolved: `InnerFence::queue` now captures `Scheduler::current_tick` and flushes in that order;
  this behavior no longer belongs to `RasterizerVulkan`.
- Resolved: `InnerFence::is_signaled` and `InnerFence::wait` now own the scheduler synchronization
  queries, and the Vulkan fence-manager overrides delegate directly to the inner fence.
- Resolved: the Rust-only public `wait_tick` and `is_stubbed` accessors were removed after their
  rasterizer-side callers were eliminated.

## 2026-08-26 — `src/video_core/src/texture_cache/samples_helper.rs` and `image_info.rs` vs Eden `src/video_core/texture_cache/samples_helper.h` and `image_info.cpp`

### Intentional differences

- Invalid raw MSAA values follow Ruzu's existing fail-soft assertion policy before selecting
  `Msaa1x1`; valid modes are passed to the helpers without conversion.

### Unintentional differences (to fix)

- Resolved: `samples_helper.rs` no longer owns a duplicate `MsaaMode` enum left from the early
  texture-port scaffold. It consumes `textures::texture::MsaaMode`, matching Eden's include and
  type ownership, and `image_info.rs` decodes directly to that canonical type.

## 2026-08-26 — `src/video_core/src/lib.rs` module tree vs Eden `src/video_core`

### Intentional differences

- Rust uses `lib.rs` module declarations in place of C++ build-system source lists.

### Unintentional differences (to fix)

- Resolved: removed the unused root `swapchain.rs` phase-one stub and its module declaration. Eden
  has no root `video_core/swapchain.*`; the implemented swapchain remains owned by
  `renderer_vulkan/swapchain.rs`.
- Resolved: removed the isolated root `shader/{mod,decoder,interpreter}.rs` software-interpreter
  prototype and its only consumer, the unused root `rasterizer.rs` CPU-renderer prototype. Neither
  had runtime callers or an Eden counterpart; configured rendering remains owned by the mirrored
  OpenGL, Vulkan, and Null backends, and Maxwell shader translation by `shader_recompiler`.
- Resolved: removed the unused root `swizzle.rs` CPU detiling prototype. Eden has no matching root
  module, and Ruzu's live GOB paths remain owned by the mirrored `textures/decoders.rs` and
  `texture_cache/accelerated_swizzle.rs` modules.
- Resolved: removed the unused root `syncpoint.rs` prototype. Eden owns this functionality only in
  `host1x/syncpoint_manager.{h,cpp}`, and every live Ruzu caller already uses the corresponding
  `host1x/syncpoint_manager.rs` implementation.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_memory_allocator.rs` vs Eden `src/video_core/vulkan_common/vulkan_memory_allocator.{h,cpp}` (remaining buffer paths)

### Intentional differences

- Rust exposes `AllocatedBuffer` instead of Eden's move-only `vk::Buffer` wrapper; it owns the VMA
  allocation and destroys buffer plus allocation together.
- Rust owners call `AllocatedBuffer::handle()` when ash requires a raw `VkBuffer`; the owning
  wrapper remains in the same query, texture, turbo, staging, cache, or presentation object that
  owns Eden's move-only `vk::Buffer`.
- Rust retains Eden's `[[maybe_unused]] MemoryUsagePropertyFlags` helper and allocator-owned device
  state with local dead-code annotations because ash/VMA wrappers carry the handles used at runtime.

### Unintentional differences (to fix)

- Resolved: raw-handle buffers now use VMA `create_buffer` with `WITHIN_BUDGET`, the same
  usage/mapping flags, memory-type mask, preferred flags, and ANV stream workaround as Eden.
- Resolved: persistently mapped upload/download buffers now reuse the VMA-owning buffer path;
  flush and invalidate operate on the VMA allocation instead of dedicated `VkDeviceMemory`.
- Resolved: raw-handle destruction and allocator teardown call VMA `destroy_buffer` rather than
  pairing `vkDestroyBuffer` with a dedicated `vkFreeMemory` allocation.
- Resolved: removed the allocator-global retained-buffer registry. `MemoryAllocator::create_buffer`
  now has one canonical owning return type, and every VMA allocation is destroyed by the field that
  owns the corresponding buffer lifecycle.
- Resolved: renamed the VMA image factory from the Rust-only `create_owned_image` spelling to the
  direct `create_image` counterpart of Eden's `CreateImage`.
- Resolved: removed the Rust-only `create_mapped_buffer` factory and its extra host-visible failure
  branch. Upload and download callers now use Eden's single `CreateBuffer` allocation path; callers
  that require mapping consume the mapped span carried by that same owning wrapper.
- Resolved: removed the `MappedBuffer` type alias. Device-local, upload, download, and stream
  allocations now all visibly use the same `AllocatedBuffer` type, mirroring Eden's single
  `vk::Buffer` owner.
- Resolved: removed the unused `MemoryPropertyFlags` and `FindType` methods; current Eden has no
  such allocator methods and its VMA paths do not call them.

### Missing items

- Resolved by the later 2026-08-26 GPU-memory logging entry.

## 2026-08-26 — Vulkan buffer owners vs Eden renderer Vulkan buffer ownership

### Intentional differences

- Ash command and descriptor APIs consume copied raw handles, so Ruzu extracts those handles from
  `AllocatedBuffer` before recording closures. The owning wrapper remains in the enclosing object
  for the same lifetime as Eden's `vk::Buffer` member.
- `TurboResources` uses `Option<AllocatedBuffer>` during fallible staged initialization; it drops
  the buffer explicitly after `device_wait_idle` and before the dedicated Vulkan device owner.

### Unintentional differences (to fix)

- Resolved: query scan, accumulation, transform-feedback, and conditional-resolve buffers are now
  owned by `query_cache.rs` instead of an allocator-global registry.
- Resolved: temporary texture buffers and per-image compute-unswizzle buffers are now owned by
  `texture_cache.rs`; image views are destroyed before the unswizzle field is released, matching
  Eden's `Image` destruction order.
- Resolved: turbo, staging, buffer-cache, layer, and presentation utility allocations all use the
  canonical owning `MemoryAllocator::create_buffer` path.
- Resolved: presentation, texture-cache, and present-manager image allocations use the canonical
  owning `MemoryAllocator::create_image` path; no second Rust-only image factory remains.

### Missing items

- GPU allocation/deallocation logging remains unavailable with the unported GPU logger.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/vk_rasterizer.rs` vs Eden `src/video_core/renderer_vulkan/vk_rasterizer.{h,cpp}` (channel cache locking)

### Intentional differences

- Ruzu's texture and buffer caches use separate `parking_lot::ReentrantMutex` values. The local
  two-lock retry helper supplies Eden's deadlock-safe `std::scoped_lock` behavior without changing
  cache ownership or the texture-before-buffer operation order.
- The separate Rust `StateTracker` releases its per-channel table before the channel caches are
  erased; this owner has no direct counterpart call in Eden's rasterizer method.

### Unintentional differences (to fix)

- Resolved: channel creation, binding, and release now hold the buffer-cache and texture-cache
  mutexes together around both cache operations, matching Eden's lifecycle synchronization.
- Resolved: fallback flush areas now use the shared 0x1000 device-page constant and common
  alignment helpers instead of a private literal. The end-address addition preserves unsigned
  wraparound before alignment.

### Missing items

- Resolved by the later 2026-08-26 rasterizer GPU-logging entry.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_library.rs` vs Eden `src/video_core/vulkan_common/vulkan_library.{h,cpp}`

### Intentional differences

- Ash owns dynamic-loader resolution and returns an `Entry`; Eden retains a shared
  `Common::DynamicLibrary` and fills its dispatch table separately.
- The Android frontend-provided driver-library path is not available in Ruzu's current frontend
  interface.

### Unintentional differences (to fix)

- Resolved: removed the development-only hard-coded lookup into a separate Eden build tree on
  macOS. Explicit `LIBVULKAN_PATH`, the active application bundle, and the system loader remain the
  only library sources.

### Missing items

- Android frontend-owned driver-library injection remains unported.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_debug_callback.rs` vs Eden `src/video_core/vulkan_common/vulkan_debug_callback.{h,cpp}`

### Intentional differences

- Validation messages are routed through Rust's logging facade before the GPU logger; this is the
  Rust counterpart of Eden's standard logging macros.

### Unintentional differences (to fix)

- Resolved: the Android false-positive ID for `vkCmdSetLogicOpEXT` is `0x1257b492`, exactly as in
  Eden. The previous Rust constant had dropped the final hexadecimal digit and matched a different
  message ID.

### Missing items

- Resolved by the later 2026-08-26 callback entry after the GPU logger was ported.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_instance.rs` vs Eden `src/video_core/vulkan_common/vulkan_instance.cpp`

### Intentional differences

- Rust creates the instance synchronously through ash instead of wrapping creation in
  `std::async(...).get()`; ash owns the loader dispatch and does not expose Eden's dynamic-library
  locking boundary.
- Application and engine names identify Ruzu while preserving Eden's version and API metadata
  fields.

### Unintentional differences (to fix)

- Resolved: optional-extension probing and final required-extension validation now consume one
  shared enumeration snapshot, preserving Eden's protection against inconsistent consecutive
  extension queries on affected drivers.
- Resolved: `VkApplicationInfo::apiVersion` now receives the Vulkan version reported by the loader
  instead of an unconditional Vulkan 1.3 value.
- Resolved: extension discovery and validation precede the available-version check in the same
  lifecycle order as Eden.

## 2026-08-26 — `src/video_core/src/textures/workers.rs` vs Eden `src/video_core/textures/workers.{h,cpp}` and `src/common/thread_worker.h`

### Intentional differences

- Rust uses `Mutex`/`Condvar`, boxed `FnOnce` jobs, and joined `std::thread` handles in place of
  Eden's `UniqueFunction`, `condition_variable_any`, and `std::jthread` stop tokens.

### Unintentional differences (to fix)

- Resolved: queued texture jobs are consumed FIFO through `VecDeque::pop_front`, matching Eden's
  `std::queue::front/pop`, instead of the previous LIFO `Vec::pop` order.
- Resolved: completion waits compare monotonically increasing scheduled and completed counts, as
  Eden does. This closes the interval in which the queue was empty but a removed request had not
  yet incremented Ruzu's former active counter.

## 2026-08-26 — `src/video_core/src/host1x/syncpoint_manager.rs` vs Eden `src/video_core/host1x/syncpoint_manager.{h,cpp}`

### Intentional differences

- Rust represents Eden's stable `std::list` iterators with monotonic action identifiers and returns
  `Option<ActionHandle>` for Eden's nullable/default iterator.
- Rust stores the action lists under the same mutex as the condition-variable guard because
  `std::sync::Condvar` waits on a mutex guard; guest and host values remain separate atomics.

### Unintentional differences (to fix)

- Resolved: removed environment-controlled syncpoint logging, stderr output, and trace events from
  registration, increment, action dispatch, and waits. Eden performs only the synchronization and
  callback operations in this owner.

## 2026-08-26 — `src/video_core/src/shader_cache.rs` and `shader_environment.rs` vs Eden `src/video_core/shader_cache.{h,cpp}` and `shader_environment.{h,cpp}`

### Intentional differences

- Rust returns `Option` where Eden returns nullable pointers and uses owned `Box` values plus raw
  stable pointers to reproduce `unique_ptr` storage ownership.
- Rust validates missing channel owners and GPU-memory readers because these are optional during
  isolated tests; the live renderer installs the same owners Eden keeps as references.

### Unintentional differences (to fix)

- Resolved: disabled and non-rasterized shader stages now clear only their unique hash and preserve
  the cached shader-info slot, matching Eden's `RefreshStages` lifecycle.
- Resolved: pending-removal and invalidation-page membership now enforce Eden's assertions instead
  of silently accepting an internally inconsistent shader cache.
- Resolved: removed the Rust-only shader-stage stall counters and shader-word/analyzer environment
  tracing from cache refresh, registration, CFG sizing, sentinel lookup, and constant-buffer reads.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/swapchain.rs` vs Eden `src/video_core/renderer_vulkan/vk_swapchain.{h,cpp}`

### Intentional differences

- Eden's frame-generation override is absent because Ruzu does not port that subsystem.

### Unintentional differences (to fix)

- Resolved: Turbo speed mode now unlocks FIFO/FIFO-relaxed presentation and selects Mailbox or
  Immediate when available, matching `ChooseSwapPresentMode`.
- Resolved: unavailable Immediate mode now falls back through Mailbox before FIFO, preserving
  Eden's ordered fallback.
- Resolved: mutable swapchain view formats now place the selected base surface format first and
  include the additional RGBA formats on Android.

### Missing items

- Frame-generation-specific presentation policy remains absent with its unported subsystem.

## 2026-08-26 — `src/video_core/src/engines/sw_blitter/converter.rs` vs Eden `src/video_core/engines/sw_blitter/converter.{h,cpp}`

### Intentional differences

- Ruzu retains the correct FP16 mantissa mask `0x03ff` when unpacking 16-bit floats. Eden uses its
  sign mask `0x8000` a second time, which discards all ten mantissa bits; reproducing that apparent
  copy/paste error would corrupt ordinary non-integral FP16 values.

## 2026-08-26 — Vulkan scheduler tick consumers vs Eden `vk_scheduler.h`, `vk_texture_cache.cpp`, and `vk_query_cache.cpp`

### Unintentional differences (to fix)

- Resolved: removed the Rust-only `Scheduler::pending_tick` synonym. Texture lifetime tracking and
  transform-feedback query-bank reservation now call `Scheduler::current_tick` directly, matching
  Eden's `Scheduler::CurrentTick()` call sites and ownership.

## 2026-08-26 — `src/video_core/src/engines/{mod,maxwell_3d,maxwell_dma}.rs`, `dirty_flags.rs`, and `macro.rs` vs Eden engine register ownership

### Intentional differences

- Maxwell3D and MaxwellDMA retain file-local `PendingWrite` payloads for Ruzu's deferred
  guest-memory integration. Eden writes through engine-owned guest-memory guards instead; keeping
  the adaptation in each concrete engine preserves the upstream ownership boundary.

### Unintentional differences (to fix)

- Resolved: the catch-all `engines/mod.rs` no longer owns one shared 0xE00 register count.
  Maxwell3D owns `NUM_REGS = 0xE00`, while MaxwellDMA owns `NUM_REGS = 0x800`, exactly where and
  with the values declared by Eden.
- Resolved: dirty-state tables and macro register reads now refer explicitly to Maxwell3D's
  register count instead of an engine-global constant.

## 2026-08-26 — `src/video_core/src/gpu_logging/*.rs` and GPU-log settings vs Eden `src/video_core/gpu_logging/*.{h,cpp}` and `common/settings{,_enums}.h`

### Intentional differences

- Rust uses one `OnceLock<GpuLogger>` plus internal mutexes and atomics instead of Eden's leaked
  raw singleton pointer and independently locked mutable members. Ring-buffer, memory, extension,
  file, and captured-state ownership remain separate, matching Eden's synchronization domains.
- Log filenames and headers use the Ruzu product name. File and directory ownership uses
  `std::fs` and `RuzuPath` rather than Eden's `IOFile` and `EdenPath` wrappers.
- Rust hashes its thread identifier with `DefaultHasher`; Eden uses the implementation-defined
  `std::hash<std::thread::id>`. The value is diagnostic only and remains a `u32` in each log entry.
- Rust locks both statistics domains while formatting `get_statistics`. Eden locks only its memory
  mutex while also reading the Vulkan-call counter written under a different mutex; reproducing
  that data race would be incorrect.
- Eden declares a private `RotateLogFile` method but provides no definition or caller. Rust omits
  that non-executable declaration and ports the actual inline rotation performed by `Initialize`.
- Android environment setup uses Rust's process-environment API; Qualcomm remains the same
  explicit future-integration stub as Eden.

### Missing items

- Runtime Vulkan call sites are wired in their corresponding file-level audit slices; this entry
  covers the logger subsystem and its settings prerequisite only.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_device.rs` GPU logging lifecycle vs Eden `src/video_core/vulkan_common/vulkan_device.{h,cpp}`

### Intentional differences

- Ruzu's loaded-extension owner is a `BTreeSet`, so the diagnostic extension list is sorted rather
  than retaining Eden's vector insertion order. Classification into Qualcomm and standard groups,
  all extension names, and the total count are unchanged.
- Vulkan fixed-size character arrays are decoded through `CStr::to_string_lossy`; valid driver
  strings are byte-for-byte unchanged while invalid UTF-8 is made printable for the diagnostic log.
- Rust `Drop` calls `shutdown_gpu_logging` before automatic field destruction. This preserves
  Eden's destructor order: logger shutdown precedes VMA allocator and logical-device destruction.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_debug_callback.rs` GPU logger routing vs Eden `src/video_core/vulkan_common/vulkan_debug_callback.{h,cpp}`

### Intentional differences

- Vulkan strings must enter Ruzu's UTF-8 `str` logging interfaces. Invalid message text retains
  the existing printable placeholder, while an invalid message-ID name falls back to Eden's
  `VulkanDebug` generic name; valid Vulkan strings are forwarded unchanged.
- Rust's ash callback flag types use `contains` in place of Eden's bitwise flag tests, preserving
  Eden's Validation-before-Performance message-type priority and severity priority.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_memory_allocator.rs` GPU memory tracking vs Eden `src/video_core/vulkan_common/vulkan_memory_allocator.{h,cpp}`

### Intentional differences

- Eden converts opaque `VkDeviceMemory` handles with `reinterpret_cast<uintptr_t>`; ash exposes the
  same opaque bits through `Handle::as_raw`, then Rust casts them to `usize` for the logger key.
- Ruzu releases its externally synchronized VMA mutex immediately after retrieving allocation
  metadata and before entering the independent GPU logger. Eden's VMA calls do not require this
  Rust mutex, while the allocation/logging order is unchanged.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/scheduler.rs` GPU logging hooks vs Eden `src/video_core/renderer_vulkan/vk_scheduler.{h,cpp}`

### Intentional differences

- The Rust scheduler formats Eden's render-pass diagnostic through a file-local mechanical helper
  so its exact `renderArea=<w>x<h>, numImages=<n>` payload can be regression-tested.
- Queue submission runs on Ruzu's Rust worker context rather than Eden's captured scheduler
  closure; the successful-submit logger call remains under the same submission mutex and follows
  the same master-semaphore call.

### Missing items

- The separate Android worker-topology dependency
  remains recorded in the earlier full scheduler audit.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/vk_rasterizer.rs` GPU logging hooks vs Eden `src/video_core/renderer_vulkan/vk_rasterizer.{h,cpp}`

### Intentional differences

- Ruzu's direct and indirect draws share `prepare_draw`, so the two Eden inline formatting blocks
  are represented by file-local, call-specific helpers. They preserve method ownership and make
  the exact call names and parameter strings directly testable.
- Vulkan success is forwarded through ash's `vk::Result::SUCCESS.as_raw()` rather than Eden's
  `VK_SUCCESS`; both pass the same signed integer zero to the logger.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_device.rs` border-color-swizzle prerequisite vs Eden `src/video_core/vulkan_common/vulkan_device.{h,cpp}`

### Intentional differences

- Ruzu retains the validated extension bit and `borderColorSwizzleFromImage` bit as Rust booleans
  after device creation rather than retaining Eden's feature-chain aggregate. The queried ash
  payload remains in the logical-device `pNext` chain until `vkCreateDevice` returns.
- The suitability predicate is mechanically extracted into a file-local function so all four
  upstream requirements can be covered by a focused regression test without a Vulkan device.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/texture_cache.rs` sampler extension logging vs Eden `src/video_core/renderer_vulkan/vk_texture_cache.{h,cpp}`

### Intentional differences

- The two inline Eden predicates are represented by a file-local fixed-size array helper so their
  exact enablement and custom-border-before-swizzle order can be tested without constructing a
  Vulkan sampler. Each enabled branch still queries the logger state separately, as Eden does.
- Ruzu's validated `custom_border_color_supported` boolean combines Eden's extension,
  `customBorderColors`, and `customBorderColorWithoutFormat` checks during device discovery.

### Missing items

- The report's broader image,
  transfer, blit, and layout-transition review remains part of the continuing texture-cache audit.

## 2026-08-26 — `src/video_core/src/texture_cache/texture_cache.rs` blit ownership vs Eden `src/video_core/texture_cache/texture_cache.{h,cpp}`

### Intentional differences

- Rust expresses Eden's compile-time `TextureCache<P>` runtime call through a
  `TextureCacheParams::blit_image` policy method. The common cache still owns every decision and
  constructed object; the policy implementations only unwrap the concrete OpenGL/Vulkan objects
  and invoke the corresponding backend runtime operation.
- Vulkan passes copyable framebuffer state to its blit helper because Rust cannot retain Eden's
  mutable framebuffer/view references while also borrowing the runtime. OpenGL forwards the
  framebuffer handles and buffer masks from the common cache's typed framebuffer slots.
- `get_framebuffer_id` is fallible for Vulkan, so common `blit_image` returns `false` if framebuffer
  construction fails. Eden propagates the equivalent Vulkan construction failure by exception.

### Missing items

- The common texture-cache audit beyond `GetBlitImages`, `BlitImage`,
  `RenderTargetFromImage`, `FindImage`, and `InsertImage` is not complete in this entry.

## 2026-08-26 — `src/common/src/thread.rs` and `thread_worker.rs` vs Eden `src/common/thread.{h,cpp}` and `thread_worker.h`

### Intentional differences

- Rust's `std::thread::Builder` owns thread naming instead of calling
  `Common::SetCurrentThreadName` from inside the worker. Placement is still applied before the
  per-thread state maker runs.
- The Android ADPF/core-affinity bodies remain no-ops because the Android JNI integration is an
  explicit port exception. Desktop hosts follow Eden's no-op affinity branches, while every
  non-default placement still lowers the worker priority first.

## 2026-08-26 — `src/video_core/src/texture_cache/texture_cache_base.rs`, `texture_cache.rs`, and `util.rs` storage ownership vs Eden `src/video_core/texture_cache/texture_cache_base.h`, `texture_cache.{h,cpp}`, and `util.{h,cpp}`

### Intentional differences

- Rust keeps per-address-space GPU page tables in a `Vec` and stores stable indices in channel
  state rather than retaining pointers into Eden's `std::deque`. The indices preserve the same
  shared address-space ownership without self-referential Rust pointers.
- `AsyncDecodeContext` is held by `Arc` while its queued closure runs, and its decoded bytes and
  copy list share one mutex-protected output object. Eden retains a `unique_ptr`, passes a raw
  pointer to the worker, and uses the mutex as the publication boundary; completion ordering and
  object lifetime remain equivalent.
- `sparse_views` names its inline values `ImageMapId`, because both ports insert IDs from
  `slot_map_views`. Eden's header spells the alias `ImageViewId`; both aliases are the same
  `SlotId`, and Eden's implementation indexes `slot_map_views` with the stored values.
- Ruzu builds the supported desktop/non-`YUZU_LEGACY` profile, so the 4-GiB threshold and eight-
  tick destruction rings match that Eden configuration. Eden's current CMake labels
  `YUZU_LEGACY` Android-only, and the Android frontend is an explicit port exception.

## 2026-08-26 — `src/video_core/src/texture_cache/types.rs` vs Eden `src/video_core/texture_cache/types.h`

### Intentional differences

- Rust represents Eden's flag enum and generated bitwise operators with a `u32`-backed
  `bitflags` type. Its four bit positions and combined mask remain identical.
- C++ comparison operators are represented by the corresponding Rust equality, ordering, and
  hashing derives where the structures expose those operations.

## 2026-08-26 — `src/video_core/src/texture_cache/util.rs`, `image_base.rs`, and `texture_cache_base.rs` vs Eden `src/video_core/texture_cache/util.{h,cpp}`, `image_base.{h,cpp}`, and `texture_cache.h`

### Intentional differences

- `UnswizzleImage` consumes bytes already read by the common cache, and `SwizzleImage` receives
  read/write callbacks. Eden passes `Tegra::MemoryManager` directly; the Rust split avoids holding
  a memory-manager lock across mutable texture-cache borrows while preserving the same unsafe
  read, read-modify-write, guest-offset, and layer ordering.
- `is_valid_entry_with_range_valid` is a mechanical callback form used while the common cache
  already owns the channel-memory lock; it performs Eden's address-only translation followed by
  the exact sized-range translation in the same order.
- Rust rejects undersized spans and mip counts that would make slice indexing out of bounds after
  Eden's fail-soft assertion. Valid inputs retain Eden's behavior without reproducing C++ undefined
  behavior. It also returns early for an invalid zero-byte pixel format before invoking decoders;
  Eden relies on callers never supplying that invalid format.
- `CommonTextureCacheParams`, the backend-neutral Rust test policy, now allocates the requested
  mapped staging span. Eden always obtains such a span from a concrete renderer runtime; this
  target-only policy keeps common-cache tests subject to that same contract.
- Eden's `FixSmallVectorADL` works around a Boost/GCC 12 ADL defect. Rust has no Boost range
  niebloids, so no equivalent compatibility copy is required.

## 2026-08-26 — `src/video_core/src/transform_feedback.rs`, `engines/maxwell_3d.rs`, and `src/shader_recompiler/src/runtime_info.rs` vs Eden `src/video_core/transform_feedback.{h,cpp}`, `engines/maxwell_3d.h`, and `src/shader_recompiler/runtime_info.h`

### Intentional differences

- Rust represents Eden's nested `TransformFeedbackState::Layout` as the top-level
  `TransformFeedbackLayout` in the matching transform-feedback module because Rust has no nested
  structure declarations.
- Rust ignores an invalid transform-feedback attribute index instead of reproducing Eden's direct
  out-of-bounds array access. Valid indices preserve Eden's assignment and maximum-count ordering.
- Eden's `UNIMPLEMENTED_IF` diagnostics are represented by the project's fail-soft Rust helper:
  violations are logged and become fatal only when debug assertions are enabled.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/turbo_mode.rs` and `renderer_vulkan.rs` vs Eden `src/video_core/renderer_vulkan/vk_turbo_mode.{h,cpp}` and `renderer_vulkan.cpp`

### Intentional differences

- Rust represents `std::jthread` stop-token ownership with an `AtomicBool`, condition-variable
  notification, and an explicit join in `Drop`. The scheduler callback retains only the shared
  notification state instead of capturing `this`, avoiding a dangling Rust reference while
  preserving Eden's submission timestamp update.
- Fallible Vulkan setup or dispatch in the worker is logged and ends that worker. Eden's Vulkan
  wrappers throw from `Run`; an uncaught C++ worker exception would terminate the process. This is
  the Rust `Result` error-propagation adaptation rather than a workload-order change.
- Android calls the same external `adrenotools_set_turbo` C ABI through a conditional Rust link
  declaration instead of including the C header.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/update_descriptor.rs` and `graphics_pipeline.rs` vs Eden `src/video_core/renderer_vulkan/vk_update_descriptor.{h,cpp}` and `vk_graphics_pipeline.cpp`

### Intentional differences

- Rust stores payload positions as indices into the queue-owned fixed allocation instead of raw
  pointer fields. `upload_start: Option<usize>` preserves Eden's initial null pointer and produces
  the same payload address after `acquire` without retaining self-referential Rust pointers.
- The retained `const Device&` is represented by the existing non-owning `DeviceReference`.
- `DescriptorUpdateEntry::default` zeroes the complete union storage. Eden activates a
  one-byte `std::monostate`; deterministic Rust padding avoids exposing uninitialized bytes if a
  payload is inspected before its selected union member is overwritten.
- The private `acquire_with_wait` callback is a mechanical test seam for Eden's `WaitWorker` call;
  the public `acquire` method still sets descriptor-buffer mode first and supplies the scheduler's
  exact wait operation.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/present/util.rs` and presentation callers vs Eden `src/video_core/renderer_vulkan/present/util.{h,cpp}`

### Intentional differences

- Command-recording helpers receive raw Ash device and command-buffer handles because Rust's
  scheduler closures own copied dispatch tables and handles instead of borrowing Eden's
  `vk::CommandBuffer` wrapper. Barrier contents and command ordering are unchanged.
- Vulkan objects are raw handles with explicit destruction in their presentation owners rather
  than Eden's move-only `vk::*` wrappers. `CreateWrappedDescriptorSets` consequently also needs
  the logical device that Eden obtains from its descriptor-pool wrapper.
- C++ default arguments and the descriptor-layout initializer-list overload are explicit Rust
  arguments. Callers pass Eden's default combined-image-sampler type and vertex/fragment stages;
  the common slice implementation also accepts compute stages for the matching upstream path.
- Small create-info builders are same-file mechanical test seams for Eden's local vectors and
  input-assembly structure; they do not move behavior out of the `present/util.rs` owner.

## 2026-08-26 — `src/video_core/src/renderer_opengl/util_shaders.rs` vs Eden `src/video_core/renderer_opengl/util_shaders.{h,cpp}`

### Intentional differences

- Rust retains the existing shared, locked `ProgramManagerHandle` instead of Eden's non-owning
  `ProgramManager&`; the lock covers each complete utility operation and preserves the program
  bind, dispatch, and guest-state restoration ordering.
- Program fields are declared in Eden's reverse member order because Rust drops structure fields
  in declaration order, while C++ destroys members in reverse declaration order. The program
  manager handle is dropped last so every OpenGL program is released first.
- `ImageInfo` is cloned once per operation to satisfy Rust borrowing while preserving Eden's
  immutable snapshot. Its tagged `TilingMode` explicitly reads `block.width` for the otherwise
  invalid block-linear pitch path, matching the first `u32` shared by Eden's anonymous union.
- Eden's fail-soft assertion handler is represented by an error log and a panic only when
  `use_debug_asserts` is enabled; normal execution continues after the same failed invariants.

## 2026-08-26 — `src/video_core/src/host_shaders/vertex_shaders.rs` vs Eden `src/video_core/host_shaders/CMakeLists.txt` and vertex shader sources

### Intentional differences

- Eden generates one C++ header per GLSL source, while Rust exposes the same source text through
  `include_str!` constants and separately compiles the Vulkan subset to SPIR-V in `build.rs`.

## 2026-08-26 — `src/video_core/src/host1x/vic.rs` vs Eden `src/video_core/host1x/vic.{h,cpp}`

### Intentional differences

- Rust retains the frame queue and GMMU manager through `Arc` handles instead of inheriting
  `CDmaPusher` and retaining `Host1x&`; the same queue lookup, memory operations, close-on-drop,
  and method-index semantics remain in the VIC owner.
- Rust executes Eden's scalar conversion paths on every host rather than duplicating its optional
  SSE4.1 branches. VIC's valid intermediate components are ten-bit values, for which the scalar
  and SIMD paths produce identical pixels; this avoids architecture-specific unsafe intrinsics.
- FFmpeg plane pointers are exposed as checked Rust slices, invalid register indices are ignored,
  and malformed rectangle ranges are skipped instead of reproducing C++ out-of-bounds access.
  Valid frame and method inputs retain Eden's arithmetic and ordering.
- The two simultaneous YUV `GpuGuestMemoryScoped<SafeWrite>` destinations use independently owned
  Rust fallback storage because one mutable scratch buffer cannot safely back both live objects.
  Direct-span access, reverse destruction/writeback order, and safe GPU invalidation match Eden;
  the single-destination ABGR paths use Eden's corresponding swizzle or luma scratch backup.
- The unused C++ surface-offset spans are omitted from the private read-method signatures; Eden
  does not read them in any progressive or interlaced path.

## 2026-08-26 — `src/video_core/src/video_core.rs` vs Eden `src/video_core/video_core.{h,cpp}`

### Intentional differences

- Rust passes a renderer factory into `create_gpu` because SDL/GTK window handles and concrete
  graphics-context construction remain frontend-owned. The factory receives the backend selected
  from the common settings, while `video_core.rs` owns the shared lifecycle and ordering.
- Renderer construction failures use `Result` and propagate to the frontend loading error path;
  dropping the still-unbound `Box<Gpu>` is the Rust equivalent of Eden's logged `gpu.reset()`.
- `RUZU_DISABLE_ASYNC_GPU` remains a diagnostic override layered onto Eden's asynchronous-GPU
  setting. Both frontends now receive that decision from the single `create_gpu` owner.

## 2026-08-26 — `src/video_core/src/vulkan_common/vk_enum_string_helper.rs` vs Eden `src/video_core/vulkan_common/vk_enum_string_helper.h` and Vulkan Utility Libraries `vk_enum_string_helper.h`

### Intentional differences

- Eden includes the complete generated Vulkan Utility Libraries header. Rust exposes the ten
  wrappers already owned by this module and derives registry-known names from Ash; raw-value
  overrides remain here for Vulkan 1.4 values and canonical aliases absent from Ash 1.3.251.
- The Rust wrappers return owned `String` values instead of generated `const char*` values. This
  preserves the exact observable text while allowing the registry-known portion to be assembled
  from Ash's `Debug` names.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/vk_rasterizer.rs` vs Eden `src/video_core/renderer_vulkan/vk_rasterizer.{h,cpp}` (clear command ordering)

### Intentional differences

- Optional Ash extension dispatch tables and channel GPU-memory ownership remain checked before
  use. Eden reaches those paths through assumed-valid pointers; valid draw and render-target paths
  issue the same commands, while invalid Rust state fails softly instead of calling a missing
  function pointer.
- Command-buffer invalidation and per-channel release are forwarded explicitly to Ruzu's separate
  `StateTracker`; Eden's tracker observes the same lifecycle through scheduler and live Maxwell
  state ownership.

## 2026-08-26 — `src/video_core/src/vulkan_common/vma.rs` vs Eden `src/video_core/vulkan_common/vma.h`

### Intentional differences

- Eden compiles VMA with static Vulkan symbols disabled and dynamic resolution enabled, passing
  `vkGetInstanceProcAddr` and `vkGetDeviceProcAddr`. The `vk-mem` crate compiles the same VMA
  implementation with both built-in loaders disabled and supplies the complete Ash function table
  explicitly. Both avoid a static Vulkan-library dependency; the Rust binding requires the latter
  integration and preserves the same allocator operations.
- Eden stores an opaque `VmaAllocator` handle and promises external synchronization. Rust owns the
  allocator through `Arc<Mutex<vk_mem::Allocator>>`; `vulkan_device.rs` sets the matching
  `EXTERNALLY_SYNCHRONIZED` allocator flag and the mutex supplies its required serialization.
- Eden selects the one translation unit defining `VMA_IMPLEMENTATION` in each frontend. The
  `vk-mem` build script compiles its own `wrapper.cpp` translation unit with that definition.

### Missing items

- Allocator creation flags and block-size policy remain
  in `vulkan_device.rs`, matching Eden's `vulkan_device.cpp`; allocation policy remains in
  `vulkan_memory_allocator.rs`.

## 2026-08-26 — `src/video_core/src/host1x/codecs/vp9_types.rs` vs Eden `src/video_core/host1x/codec_types.h` (VP9 section) and `src/video_core/host1x/codecs/vp9.cpp`

### Intentional differences

- Eden groups the raw H.264, VP9, and VP8 codec structures in one header. Rust keeps the VP9
  portion in a codec-specific module so the decoder can import it without exposing unrelated raw
  formats; the module documentation now names the exact upstream owner and every VP9 type remains
  in the same order as that contiguous header section.
- Rust uses `bitflags` for `FrameFlags`, native enums for the C++ scoped enums, and explicit byte
  arrays for upstream padding macros. Their sizes, discriminants, offsets, and raw bytes match the
  C++ representations.

## 2026-08-26 — `src/video_core/src/vulkan_common/{mod.rs,vulkan.rs}` vs Eden `src/video_core/vulkan_common/vulkan.h`

### Intentional differences

- Ash owns Vulkan types, opaque handles, and dynamic function dispatch, so Eden's
  `VK_NO_PROTOTYPES`, target-specific `VK_USE_PLATFORM_*` preprocessor selection, Windows macro
  sanitation, and `VkSurfaceKHR_T` forward declaration have no source-level Rust counterparts.
- Rust's `mod.rs` declares the same subsystem files instead of serving as a C++ include
  aggregator. The actual constants owned by Eden's header live in the matching `vulkan.rs` module.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_debug_callback.rs` vs Eden `src/video_core/vulkan_common/vulkan_debug_callback.{h,cpp}` (report re-audit)

### Intentional differences

- Rust converts Vulkan C strings to UTF-8 before passing them to the `log` and GPU-logging APIs;
  null or invalid strings receive printable fallbacks. Vulkan guarantees valid callback strings,
  so valid calls retain Eden's exact message, ID-name, type-prefix, and priority behavior.
- Rust's `error!` level is the closest available counterpart to Eden's `LOG_CRITICAL`; warning,
  info, and verbose/debug levels map directly.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_device.rs` vs Eden `src/video_core/vulkan_common/vulkan_device.{h,cpp}`

### Intentional differences

- `ash` 0.37 predates `VK_KHR_maintenance5` and `VK_KHR_maintenance6`; Ruzu therefore owns
  ABI-compatible feature/property payloads locally and retains their queried answers as booleans
  after logical-device creation. Maintenance 7/8 names remain owned by `vulkan.rs`, matching the
  fallback definitions in Eden's `vulkan.h`.
- The extension-selection and suitability policies are mechanically extracted into file-local
  functions so promotion rules, mandatory capabilities, and MoltenVK fallbacks can be tested
  without constructing a physical device.
- Ruzu additionally enables `VK_KHR_portability_subset` when advertised because MoltenVK requires
  applications to enable it; this platform requirement is represented by `ash` rather than Eden's
  current device-extension macros.

### Unintentional differences (to fix)

- Resolved: the feature/extension inventory now includes Vulkan memory model, image robustness,
  maintenance 1–4 and 6–8, and ASTC decode mode, with Eden's API-version promotion rules,
  suitability filtering, accessors, and logical-device feature-chain lifetime.
- Resolved: suitability now checks Eden's exact mandatory feature set, leaves 8/16-bit storage in
  the recommended/optional sets, applies only Eden's four MoltenVK fallbacks, and reports an
  unsuitable device while continuing device creation as upstream does.
- Resolved: optimal ASTC support now requires every upstream sampled-image, linear-filter, and
  transfer feature bit for all 28 LDR formats; the accessor returns that computed result.
- Resolved: `VK_KHR_robustness2` name preference and Radeon GPU Profiler detection now follow
  Eden's extension/tooling paths.

### Missing items

- Android/AArch64 still lacks Eden's AdrenoTools BCn driver patch and `OverrideBcnFormats` path.
  This requires the Android-only debug setting, API-level query, and AdrenoTools BCn ABI rather
  than a safe local edit to the host-independent device path.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/texture_cache.rs` vs Eden `src/video_core/renderer_vulkan/vk_texture_cache.{h,cpp}`

### Intentional differences

- The file-local `is_ldr_astc_format` predicate operates on Vulkan's contiguous LDR ASTC enum
  range; this is the Rust counterpart of Eden's `IsLdrAstcFormat` helper used by the image-view
  constructor.

### Unintentional differences (to fix)

- Resolved: LDR ASTC image views now prepend `VkImageViewASTCDecodeModeEXT` with
  `VK_FORMAT_R8G8B8A8_UNORM` when the extension is enabled, while preserving Eden's following
  `VkImageViewUsageCreateInfo` node and conditional ordering.

### Missing items

- The audited ASTC decode-mode image-view path is complete; broader texture-cache parity remains
  tracked by its dedicated reports.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_instance.rs` Haiku prerequisite vs Eden `src/video_core/vulkan_common/vulkan_instance.{h,cpp}`

### Intentional differences

- Platform variants and their code are selected with Rust `cfg(target_os)` attributes instead of
  Eden's preprocessor branches; the resulting Haiku-only branch owns the same XCB extension name.

### Unintentional differences (to fix)

- Resolved: `WindowSystemType::Xcb` and `VK_KHR_xcb_surface` are now present on Haiku, completing
  the platform dispatch consumed by `vulkan_surface.rs`.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_surface.rs` vs Eden `src/video_core/vulkan_common/vulkan_surface.{h,cpp}`

### Intentional differences

- Ash's XCB extension loader replaces Eden's function-pointer lookup through
  `vkGetInstanceProcAddr`; both submit the same `VkXcbSurfaceCreateInfoKHR` fields and return the
  same raw surface handle ownership to the caller.
- The repeated upstream initialization error is a mechanical file-local helper so its exact
  `VkResult` can be tested without constructing a native window or Vulkan instance.

### Unintentional differences (to fix)

- Resolved: the previously omitted Haiku/XCB path now forwards `display_connection` as
  `xcb_connection_t*` and converts `render_surface` through `uintptr_t` to `xcb_window_t`.
- Resolved: every platform creation failure now logs Eden's platform-specific message and becomes
  `VK_ERROR_INITIALIZATION_FAILED`; successful calls returning a null surface are rejected by the
  same final guard as upstream.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_wrapper.rs` vs Eden `src/video_core/vulkan_common/vulkan_wrapper.{h,cpp}`

### Intentional differences

- Ash owns the Vulkan dispatch tables and most raw-handle method wrappers; Ruzu keeps the
  file-owned helpers and the two top-level RAII owners that are not supplied by Ash.
- The application and engine names use `ruzu Emulator` instead of Eden's inherited
  `yuzu Emulator` branding. Their version fields and requested Vulkan 1.3 API now match exactly.
- Ash 0.37 predates the HoneyKrisp and KosmicKrisp `VkDriverId` names, so the switch retains their
  registered values 26 and 28 as named local compatibility constants.
- Rust rejects an object name containing an interior NUL before constructing the Vulkan C string;
  all valid names follow Eden's optional-dispatch and result-checking path.

### Unintentional differences (to fix)

- Resolved: object naming now loads `vkSetDebugUtilsObjectNameEXT` as a genuinely optional
  function and returns success when absent, instead of invoking Ash's generated panic fallback.
- Resolved: physical-device tooling properties now preserve Eden's exact two unchecked calls,
  initial allocation length, and absence behavior rather than adding result filtering and an
  `VK_INCOMPLETE` retry loop.
- Resolved: `Instance::create` again owns the application-info construction and Apple portability
  flag, pins application, engine, and API versions to 1.3.0, and rejects a created instance whose
  `vkDestroyInstance` entry point cannot be loaded, as upstream does.
- Resolved: `get_driver_name` is again owned by the wrapper, includes HoneyKrisp and KosmicKrisp,
  returns Eden's exact `Nvidia` and `llvmpipe` spellings, and falls back to `driver_name`.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_instance.rs` ownership call site vs Eden `src/video_core/vulkan_common/vulkan_instance.cpp`

### Intentional differences

- Extension and layer names remain owned as `CString` values until the wrapper's Vulkan call.

### Unintentional differences (to fix)

- Resolved: the frontend instance factory no longer owns `VkApplicationInfo` or the Apple create
  flag; it passes Eden's `available` version argument to the matching wrapper method.

## 2026-08-26 — `src/video_core/src/vulkan_common/vulkan_device.rs` driver-name delegation vs Eden `src/video_core/vulkan_common/vulkan_device.cpp`

### Intentional differences

- The Rust device retains Ash's logical-device owner and delegates file-owned wrapper behavior
  through module functions rather than C++ member wrappers.
- Existing infallible buffer/shader naming methods use `expect` to unwind on the same Vulkan
  failure for which Eden's wrapper throws; the framebuffer path can return `VulkanError` directly.

### Unintentional differences (to fix)

- Resolved: `Device::get_driver_name` now delegates to the wrapper-owned driver-property mapping,
  matching Eden's `Device::GetDriverName` ownership boundary.
- Resolved: shader-module and buffer naming reuse the wrapper-owned optional dispatch helper, so a
  missing debug-utils symbol no longer reaches Ash's panic fallback.

## 2026-08-26 — `src/video_core/src/renderer_vulkan/present/window_adapt_pass.rs` vs Eden `src/video_core/renderer_vulkan/present/window_adapt_pass.{h,cpp}`

### Intentional differences

- Ash raw handles plus an explicit `Drop` implementation replace Eden's move-only Vulkan wrapper
  members. The Rust owner is established before resource creation so panic unwinding releases the
  same successfully created resources as C++ constructor exception unwinding.
- Rust's `LinkedList<Layer>` is the direct standard-library counterpart of Eden's
  `std::list<Layer>` at this interface boundary.

### Unintentional differences (to fix)

- Resolved: construction no longer creates raw handles as unowned locals before assembling the
  pass; every successfully created handle, including the moved sampler and fragment shader, is
  owned and cleaned up if a later creation fails.
- Resolved: the three presentation pipelines are assigned sequentially to the owner, so failure
  while creating the second or third pipeline cannot leak an earlier one.
- The report's claimed caller-side layer preconfiguration is obsolete: `draw` already owns
  `configure_draw`, pipeline selection, command recording, and draw ordering exactly as Eden does.

## 2026-08-26 — `src/video_core/src/textures/workers.rs` vs Eden `src/video_core/textures/workers.{h,cpp}`

### Intentional differences

- `OnceLock` supplies Rust's function-local-static equivalent. Rust process statics are not
  destructed during normal runtime shutdown, whereas Eden's local static destroys its `jthread`
  pool; explicit `ThreadWorker` owners still stop and join identically, and process termination
  reclaims the singleton threads.
- `available_parallelism()` replaces `std::thread::hardware_concurrency()`; both clamp the reported
  value to at least two before halving it, and the Rust error fallback therefore produces the same
  one-worker minimum.
- ASTC and BCN call the common port's `queue_stateless_work` adapter because Rust cannot overload
  the stateful `queue_work` closure signature for the `StatefulThreadWorker<()>` alias.

### Unintentional differences (to fix)

- Resolved: `textures/workers.rs` no longer reimplements `Common::ThreadWorker`. It now owns only
  the `ImageTranscode` singleton construction and returns the implementation owned by
  `common/thread_worker.rs`, matching Eden's file and method boundaries.
- The report's LIFO statement is obsolete: the common owner consumes `VecDeque::pop_front`, and a
  single-worker regression test verifies Eden's FIFO ordering.
- The report's BCN usage warning is obsolete: both ASTC decompression and BCN compression queue
  one job per row/stride on this shared pool and wait after each depth plane, matching Eden.

## 2026-08-26 — `src/video_core/src/query_cache_top.rs` and `src/video_core/src/renderer_opengl/gl_query_cache.rs` vs Eden `src/video_core/query_cache.h` and `src/video_core/renderer_opengl/gl_query_cache.{h,cpp}`

### Intentional differences

- Rust expresses the CRTP relationship among the legacy cache, cached query, counter stream, and
  host counter with `LegacyCachedQuery` and `CounterHandle` traits. The shared lifecycle and
  renderer-specific method ownership remain split at the same boundary as Eden.
- `Arc<Mutex<_>>` replaces `shared_ptr` ownership for host counters and the OpenGL query pool, and
  `ReentrantMutex` replaces Eden's `recursive_mutex`.
- Rasterizer services and `AnyCommandQueued()` are passed explicitly through the Rust call sites
  rather than retained as self-referential backend references. The values are sampled at the same
  operations where Eden calls the rasterizer.
- `PopAsyncFlushes` clones the front batch's scalar slot identifiers so Rust can mutate the cache
  while preserving Eden's requirement that the original front batch remains queued throughout
  processing.

### Unintentional differences (to fix)

- Resolved: `FlushAndRemoveRegion` now retains a cached-page map entry after its last query is
  erased, matching `std::erase_if(contents, ...)` on Eden's existing map value.
- Resolved: `PopAsyncFlushes` no longer removes a non-null batch before collecting its queries; it
  keeps that batch at the front and pops it only after processing, matching Eden's lifecycle order.

## 2026-08-26 — `src/video_core/src/renderer_opengl/gl_staging_buffer_pool.rs` vs Eden `src/video_core/renderer_opengl/gl_staging_buffer_pool.{h,cpp}`

### Intentional differences

- Mapped spans retain their pointer and length as separate fields because Rust cannot store a
  mutable slice without imposing a lifetime on the movable RAII result.
- Rust declares GL-owning fields in C++ reverse-destruction order: allocation buffers precede
  their sync objects, stream fences precede the stream buffer, and download buffers precede upload
  buffers. Rust's declaration-order drop therefore matches Eden's reverse member destruction.
- The renderer shares its single staging-pool owner through `Arc<Mutex<_>>`; allocation selection,
  fence creation, deferred release, and stream-buffer requests remain serialized at the same owner.

### Unintentional differences (to fix)

- Resolved: `STREAM_BUFFER_SIZE`, `NUM_SYNCS`, `REGION_SIZE`, and `MAX_ALIGNMENT` are private
  `StreamBuffer` associated constants again, rather than module-level constants, matching their
  upstream class ownership.

## 2026-08-26 — OpenGL texture cache and common `CopyImage` ownership vs Eden `gl_texture_cache.{h,cpp}`, `texture_cache.{h,cpp}`, and `vk_texture_cache.{h,cpp}`

### Intentional differences

- Rust expresses the C++ `TextureCache<P>` calls through `TextureCacheParams`; the common cache
  owns copy selection, scaling, view construction, and framebuffer construction, while each
  backend adapter only obtains its concrete objects and calls the matching runtime method.
- OpenGL reads the mutex-protected live `resolution_info` value when scaling instead of retaining
  Eden's reference into the global settings object. This preserves live settings semantics without
  a self-referential unsynchronised pointer.
- Vulkan framebuffer construction is fallible in Rust, so the conversion path returns if creating
  the destination render target fails; Eden propagates the equivalent Vulkan failure by exception.

### Unintentional differences (to fix)

- Resolved: `TextureCache<P>::CopyImage` again belongs to the common cache. Rescaled-coordinate
  handling, emulated-copy selection, reinterpretation, conversion-view construction, and extent
  validation are no longer duplicated in the OpenGL and Vulkan backends.
- Resolved: `JoinImages` preserves Eden's two distinct call paths: alias copies use common
  `CopyImage`, while non-alias shrink copies call the runtime directly because
  `MakeShrinkImageCopies` already applied its scaling factors.
- Resolved: Vulkan `TextureCacheRuntime::ShouldReinterpret` checks both destination and source
  depth/stencil formats when stencil export is unavailable, and the method is owned by the Vulkan
  runtime rather than the generic adapter.
- Resolved: OpenGL now uses the canonical `NUM_RT` and `Shader::NUM_TEXTURE_TYPES` owners, retains
  Eden's file-local accelerated-format table, materializes `StorageViews` before probing it, and
  uses unsigned wrapping arithmetic for the device-memory budget.

## 2026-08-26 — `src/video_core/src/shader_notify.rs` vs Eden `src/video_core/shader_notify.{h,cpp}`

### Intentional differences

- Rust protects Eden's four non-atomic reporting fields with a mutex because callers may query
  progress concurrently. The two counters remain independent atomics and retain Eden's ordering.
- `Option<Instant>` represents Eden's default-constructed `steady_clock::time_point`; the value is
  only read after the same `completed` transition has stored a completion time.

## 2026-08-26 — `src/video_core/src/renderer_opengl/gl_fence_manager.rs` vs Eden `src/video_core/renderer_opengl/gl_fence_manager.{h,cpp}`

### Intentional differences

- Rust composition delegates Eden's templated `GenericFenceManager` base to the common
  `FenceManager<Fence>` owner. `Arc<Mutex<GLInnerFence>>` replaces `shared_ptr<GLInnerFence>` so
  callback-owned fence handles can be shared safely.
- The test-only forced-stub switch allows the common fence lifecycle to be exercised without an
  OpenGL context and is absent from production builds.

## 2026-08-26 — `src/video_core/src/host1x/codec_types.rs` vs Eden `src/video_core/host1x/codec_types.h`

### Intentional differences

- C++ `BitField` unions are represented by integer backing fields plus typed accessors. In
  particular, the 64-bit H264 parameter union uses two `u32` words so the containing guest payload
  retains Eden's four-byte alignment.
- Rust `Vec<u8>` and deterministic zeroed `Default` implementations replace `std::vector<u8>` and
  C++ aggregate initialization without changing guest-memory serialization.

### Unintentional differences (to fix)

- Resolved: the H264, VP9, and VP8 codec payload declarations, their inline conversions, and their
  layout assertions now share the `host1x/codec_types.rs` owner matching Eden. They are no longer
  split across `codecs/h264.rs`, `codecs/vp8.rs`, and the removed `codecs/vp9_types.rs` module.

## 2026-08-27 — `src/video_core/src/renderer_vulkan/query_cache.rs` vs Eden `src/video_core/renderer_vulkan/vk_query_cache.{h,cpp}`

### Intentional differences

- Rust splits the mutable `SamplesStreamer`/`TFBCounterStreamer` and `QueryRuntimeBackend` borrows
  before calling `QueryCacheRuntime::sync_host_values`. Eden reaches the same runtime-owned method
  through the streamer's stable reference to `QueryCacheRuntime`.

### Unintentional differences (to fix)

- Resolved: `sync_samples_writes` and `sync_tfb_writes` no longer use `Option::take` to move their
  mutex-owning streamers out of and back into `QueryCacheRuntime`. Eden's streamers remain at stable
  addresses inside the heap-owned `QueryCacheRuntimeImpl`; retaining the Rust streamers in place
  prevents the fence-release thread from waiting on the abandoned address of a moved mutex.

### Missing items

- Live query-pool validation remains necessary for recorded Vulkan commands; the startup
  regression was additionally exercised through five consecutive release launches.

## 2026-08-27 — `src/shader_recompiler/src/frontend/translate/{mod.rs,load_store_local_shared.rs}` and `src/shader_recompiler/src/pipeline_cache.rs` vs Eden `src/shader_recompiler/frontend/maxwell/translate/{impl/impl.h,impl/load_store_local_shared.cpp,translate.cpp}`

### Intentional differences

- Reduced Rust instruction tests and compatibility callers may construct a `TranslatorVisitor`
  without an `Environment`; those paths retain the explicit SPH/program fallback. Every runtime
  environment translation supplies the upstream-owned environment reference.

### Unintentional differences (to fix)

- Resolved: the runtime visitor had lost Eden's `Environment& env` member, so local-memory bounds
  used a cloned graphics program header. Compute allocations live in
  `Environment::local_memory_size`; a zero-valued header therefore discarded valid immediate
  `STL` writes and left their consumer buffers uninitialized.

## 2026-08-27 — `src/shader_recompiler/src/backend/spirv/spirv_emit_context.rs` vs Eden `src/shader_recompiler/backend/spirv/spirv_emit_context.cpp`

### Intentional differences

- Rust transports Eden's shader exceptions through typed `panic_any` payloads so the Vulkan
  pipeline cache can reproduce `catch (const Shader::Exception&)` without treating unrelated Rust
  panics as shader compilation failures.

### Unintentional differences (to fix)

- Resolved: fragment-stage stores of `PointSize`, `ClipDistance`, `Layer`, or `ViewportIndex`
  previously used ordinary Rust assertions and could terminate the GPU thread. They now raise the
  same typed `NotImplementedException` as Eden, allowing pipeline creation to log, cache the
  failure, and continue.

## 2026-08-29 — `src/video_core/src/texture_cache/texture_cache.rs` vs Eden `src/video_core/texture_cache/texture_cache.h`

### Intentional differences

- A non-owning `MemoryManagerHandle` represents Eden's directly stored `Tegra::MemoryManager*`
  while the owning `Arc<Mutex<MemoryManager>>` keeps the channel object alive. Descriptor reads and
  memory-read callbacks use that stable handle under the texture-cache/channel serialization that
  protects Eden's pointer; reduced tests without a bound channel retain the owned fallback path.

### Unintentional differences (to fix)

- Resolved: texture downloads previously wrote through `image.cpu_addr` as one linear device-memory
  range whenever the channel mutex was held. Eden always calls `SwizzleImage(*gpu_memory,
  image.gpu_addr, ...)`, preserving every GPU page-table segment. Ruzu now does the same in both
  unlocked and re-entrant callback paths.
- Resolved: `VisitImageView` and ordinary `GetSamplerId` previously locked the Rust channel
  `MemoryManager` once for every TIC/TSC descriptor read. Eden reads both tables through its stored
  non-owning pointer while the texture cache is already serialized. Ruzu now uses the matching
  stable handle, removing two atomic mutex operations per descriptor from every draw.

## 2026-08-29 — `src/core/src/hle/service/am/library_applet_storage.rs` vs Eden `src/core/hle/service/am/library_applet_storage.{h,cpp}`

### Intentional differences

- Rust keeps the transfer-memory object ID in the composed base storage so
  `HandleLibraryAppletStorage` can return it without duplicating the backing storage state.

### Unintentional differences (to fix)

- Resolved: ordinary `TransferMemoryLibraryAppletStorage` exposed its transfer-memory handle,
  causing `IStorage::Open` to reject it. It now matches Eden's null `GetHandle()` result; only
  `HandleLibraryAppletStorage` exposes the handle.

## 2026-08-29 — `src/core/src/hle/kernel/k_scheduler.rs` vs Eden `src/core/hle/kernel/k_scheduler.{h,cpp}`

### Intentional differences

- Before bootstrap has installed a scheduler-owned current thread, Rust may fall back to its
  host-thread TLS pointer. Once installed, `self.current_thread` is the counterpart of Eden's
  per-core `GetCurrentThreadPointer(kernel)` and remains authoritative across fiber yields.

### Unintentional differences (to fix)

- Resolved: `ScheduleImpl` preferred the fiber-shared TLS pointer even when the scheduler already
  owned a current thread. During cross-core migration this could unload a stale thread and spin
  forever trying to reacquire the actual current thread's context guard. Current-thread selection
  now follows Eden's per-core scheduler pointer before the bootstrap fallback.

## 2026-08-29 — `src/ruzu/src/configuration/configure_dialog.rs` vs Eden `src/yuzu/configuration/configure_dialog.{h,cpp}` and `configure_input_player.{h,cpp}`

### Intentional differences

- GTK closes the modeless configuration window asynchronously, so the dialog owner explicitly
  stops input mapping and disables the eight player controllers plus the handheld controller
  before page destruction. Eden obtains the same final state synchronously from the
  `ConfigureInputPlayer` destructors when its stack-owned modal dialog returns.

## 2026-08-29 — `src/ruzu/src/{main_window.rs,main.rs,i18n.rs}` vs Eden `src/yuzu/{bootmanager.{h,cpp},main.cpp,main_window.cpp}`

### Intentional differences

- GTK native render children require explicit tracking of their logical origin, size, and surface
  scale. Eden's Qt layout owns the child render widget and invokes `OnFramebufferSizeChanged` from
  `GRenderWindow::resizeEvent` and screen changes.
- Eden selects Qt's `windowsvista` style before constructing `QApplication`. Ruzu supplies the
  GTK-specific `GTK_CSD=0` default before GTK initialization so Win32 owns every decorated
  toplevel frame; an explicit caller-provided value is preserved.
- Qt resolves the system locale internally. Ruzu queries the Windows user locale when POSIX locale
  variables are absent, then resolves it through the available frontend catalogs.

## 2026-08-29 — `src/ruzu/src/{user_data_migration.rs,migration_worker.rs,gtk_compat.rs}` vs no Eden frontend counterpart

### Intentional differences

- User-data migration is a Ruzu GTK onboarding feature and has no Eden Qt counterpart. It keeps
  legacy sources read-only, validates Windows junctions without treating ordinary directories as
  reparse points, repairs imported absolute storage paths, dismisses failed flows exactly once,
  and formats estimates as localized MB or GB at the requested threshold.

## 2026-08-29 — `build.bat`, `scripts/build.ps1`, and `README.md` vs no Eden source-file counterpart

### Intentional differences

- Ruzu's Windows bootstrap/build/package workflow is repository tooling rather than a ported C++
  module. It discovers an existing standalone vcpkg through explicit, environment, PATH, ancestor,
  and conventional locations; otherwise it installs under `%LOCALAPPDATA%\Ruzu\vcpkg`. Release is
  the default, Debug is explicit, and the executable plus runtime DLLs are staged under
  `build\x86_64-pc-windows-msvc\<profile>`.
- `build.bat package -ForcePackage` is an explicit test-release escape hatch that bypasses only
  the Git `main`-branch checks. The default package path remains strict, and dependency, staging,
  runtime-file, and NSIS validation are never bypassed.

## 2026-08-29 — `src/common/{build.rs,src/scm_rev.rs}` vs Eden `CMakeModules/GenerateSCMRev.cmake` and `src/common/scm_rev.{h,cpp.in}`

### Intentional differences

- Eden obtains `CMAKE_CXX_COMPILER_ID` and `CMAKE_CXX_COMPILER_VERSION` from CMake. Ruzu's Cargo
  build script queries the selected native compiler directly and exports the equivalent
  `COMPILER_ID` compile-time value.

## 2026-08-30 — `src/core/src/file_sys/vfs/vfs_real.rs` vs Eden `src/core/file_sys/vfs/vfs_real.{h,cpp}`

### Intentional differences

- Rust passes an explicit `DirectorySeparator::PlatformDefault` to `sanitize_path`; Eden's
  `FS::SanitizePath` uses the platform default implicitly. Both now normalize the directory root
  and its relative child with the same separator before applying `IsWithinRoot`.

### Unintentional differences (to fix)

- The prior
  Windows-only mismatch compared a backslash-normalized root with a slash-normalized child, so
  every valid relative lookup failed and `RegisteredCache` indexed zero firmware entries.

## 2026-08-30 — `src/rdynarmic/src/backend/x64/emit_a64.rs` and `src/rdynarmic/src/jit.rs` vs Eden `src/dynarmic/src/dynarmic/backend/x64/emit_x64.{h,cpp}`

### Intentional differences

- Eden obtains an IR argument's resolved type directly from `IR::Value::GetType()`. Rust
  `Value::Inst` stores only an arena reference, so the fallback resolves the same type through the
  owning block's `inst_real_return_type` before selecting the 8/16/32/64-bit x64 register view.

## 2026-08-30 — `src/rdynarmic/src/backend/x64/{a64_interface.rs,a64_emit_x64_memory.rs,callback.rs}` vs Eden `src/dynarmic/src/dynarmic/backend/x64/a64_emit_x64_memory.cpp` and `a64_emit_x64.h`

### Intentional differences

- Eden devirtualizes the C++ member callback and receives MSVC's hidden aggregate-return pointer
  after the callback object. Rust uses an explicit `Pair128*` trampoline argument, while the
  generated accessor preserves the same register contract: context in `RCX`, return pointer in
  `RDX`, and guest address moved to `R8`.

## 2026-08-30 — `externals/rxbyak/src/{code_array.rs,platform/{mod.rs,windows.rs,unix.rs}}` vs Eden `src/dynarmic/src/dynarmic/backend/x64/block_of_code.{h,cpp}`

### Intentional differences

- Eden implements its custom virtual-memory allocator and `EnsureMemoryCommitted` directly in
  `BlockOfCode`. Ruzu's assembler owns the backing allocation, so the same lifecycle is implemented
  in the vendored Rxbyak `CodeBuffer`; `BlockOfCode` still receives one fixed, non-moving address
  range and all generated-code ownership remains unchanged.
- Unix keeps one lazy `mmap` for the complete range and treats explicit commitment as a no-op,
  matching the effective upstream behavior outside Windows.

## 2026-08-30 — `src/rdynarmic/src/backend/x64/{hostloc.rs,reg_alloc.rs,a64_emit_x64.rs,emit_data_processing.rs}` vs Eden `src/dynarmic/src/dynarmic/backend/x64/{hostloc.h,reg_alloc.h,reg_alloc.cpp,a64_emit_x64.cpp}`

### Intentional differences

- Eden represents candidate registers with `std::bitset<32>`; Rust uses `&[HostLoc]`/`Vec<HostLoc>`
  and explicitly visits the corresponding numeric host-location indices. Candidate membership,
  conditional A64 removal of `R13`/`R14`, and selection priority remain identical.
- Eden stores `lru_counter` in a two-bit C++ bit-field. Rust stores it as `u8` and masks each
  increment to two bits, preserving the effective wrap while avoiding implementation-defined Rust
  layout.

## 2026-08-30 — `src/core/src/file_sys/patch_manager.rs` vs Eden `src/core/file_sys/patch_manager.{h,cpp}`

### Intentional differences

- Rust exposes content-provider origin tracking through the `ContentProvider` trait instead of
  Eden's concrete `ContentProviderUnion` casts. The versioned and per-origin probes therefore use
  trait methods, while the final unversioned fallback retains Eden's union-wide `GetEntryRaw`
  behavior.

## 2026-08-30 — `src/ruzu/src/boot.rs` vs Eden `src/yuzu/main_window.cpp` (`BootGame` title metadata)

### Intentional differences

- Eden normally obtains the visible version from the Control NACP returned by
  `PatchManager::GetControlMetadata`. Ruzu keeps that primary path, then uses the first enabled
  update patch's non-empty NACP display string when an external/manual Program update is usable but
  its Control RomFS was not patchable. This GTK-frontend fallback consumes the same live
  `PatchManager` state as the game list and still honors disabled updates.

### Unintentional differences (to fix)

- LM3 now reports `1.4.0` instead of the base NACP's
  `1.0.0`, matching Eden, while the technical CNMT revision remains `v0.5.0` in patch logs.

## 2026-08-30 — `src/audio_core/src/sink/sink_stream.rs` vs Eden `src/audio_core/sink/sink_stream.{h,cpp}`

### Intentional differences

- Rust stores the sample and buffer FIFOs in `VecDeque` and shares the release state through
  atomics plus a condition variable; these replace Eden's `RingBuffer`, `SPSCQueue`, mutex, and
  condition variable without moving the owned stream behavior out of `sink_stream.rs`.
- Rust accepts immutable guest sample slices and enqueues converted samples separately, whereas
  Eden performs the downmix in its mutable scratch span before pushing the shortened span.
- `discard_buffers`, stop-aware waits, and the backend start/stop closure are Rust integration
  state used by the existing ADSP and Cubeb ownership model; they do not change the reviewed
  AudioOut sample-count calculation.
- Rust guards zero-frame and out-of-range channel copies that Eden assumes are prevented by its
  callers.

## 2026-08-30 — `src/video_core/src/vulkan_common/vulkan_device.rs` vs Eden `src/video_core/vulkan_common/vulkan_device.{h,cpp}`

### Intentional differences

- Eden passes the physical device's `ApiVersion()` directly to VMA 3.3.0, which accepts Vulkan
  1.4. Ruzu currently uses `ash` 0.37 with `vk-mem` 0.3, whose bundled VMA contract asserts that
  allocator creation receives at most Vulkan 1.3. Ruzu therefore clamps only the VMA allocator's
  advertised API version to 1.3; the physical-device version retained and reported by `Device`
  remains unchanged. This also matches Ruzu's Vulkan 1.3 instance creation.

## 2026-08-30 — `src/shader_recompiler/src/frontend/translate/internal_stage_buffer_entry_read.rs` vs Eden `src/shader_recompiler/frontend/maxwell/translate/impl/internal_stage_buffer_entry_read.cpp`

### Intentional differences

- Rust decodes the instruction fields into local enums instead of using Eden's overlapping C++
  `BitField` union. Field positions, enum values, and the fact that `imm` includes bit 31 are
  preserved.
- Eden's Patch and Prim branches reinterpret the typed `IR::U32` index as `Patch`/`Attribute` and
  rely on those modes carrying immediate indices. Rust checks that invariant before constructing
  the corresponding strongly typed IR value; the valid instruction behavior is unchanged.

## 2026-08-30 — `src/shader_recompiler/src/ir/emitter.rs` (`uconvert_u64_from_u32`) vs Eden `src/shader_recompiler/frontend/ir/ir_emitter.{h,cpp}` (`UConvert`)

### Intentional differences

- Eden exposes one runtime-polymorphic `UConvert(result_bitsize, value)` method. Rust exposes the
  exact U32-to-U64 specialization required by ISBERD, following the existing typed emitter API;
  both emit `Opcode::ConvertU64U32`.

### Missing items

- The other Eden `UConvert` width combinations remain outside this focused prerequisite slice.

## 2026-08-30 — `src/shader_recompiler/src/runtime_info.rs` vs Eden `src/shader_recompiler/runtime_info.h` (`InputTopologyVertices`)

### Intentional differences

- Rust owns `vertices` as a `const` method on `InputTopology` instead of wrapping the enum in
  Eden's stateless `InputTopologyVertices` helper struct. The helper remains in the corresponding
  `runtime_info.rs` owner and returns the same value for every topology.

## 2026-08-30 — `src/shader_recompiler/src/backend/spirv/emit_spirv_context_get_set.rs` vs Eden `src/shader_recompiler/backend/spirv/emit_spirv_context_get_set.cpp` (`EmitInvocationInfo`)

### Intentional differences

- Rspirv builder calls replace Eden's typed `EmitContext::OpShiftLeftLogical`/`Const` wrappers;
  both construct the same unsigned 32-bit left shift by 16.

## 2026-08-30 — `src/video_core/src/renderer_vulkan/texture_cache.rs` image-allocation diagnostics vs Eden `src/video_core/renderer_vulkan/vk_texture_cache.cpp` (`MakeImage`/`Image::Image`)

### Intentional differences

- Ruzu logs the complete `ImageInfo`, Vulkan create-info, and current memory-budget usage when a
  VMA image allocation fails. With `RUZU_TRACE_VULKAN_IMAGE_ALLOC` set, it also logs allocations
  made after heap usage reaches 4 GiB. These diagnostics do not alter image creation or ownership.

### Missing items

- The underlying LM3 memory-pressure cause remains under investigation; this entry covers only
  the non-invasive allocation diagnostics.

## 2026-08-30 — `src/audio_core/src/renderer/command/data_source/decode.rs` vs Eden `src/audio_core/renderer/command/data_source/decode.{h,cpp}` (`DecodeAdpcm`)

### Intentional differences

- Rust retains bounds-checked coefficient lookup and opt-in diagnostics around Eden's direct
  array indexing. With the upstream three-bit predictor mask, every possible header maps to one
  of the eight valid coefficient pairs, so valid decoding behavior is identical.

## 2026-08-30 — `src/audio_core/src/adsp/apps/opus/opus_multistream_decode_object.rs` vs `src/audio_core/adsp/apps/opus/opus_multistream_decode_object.{h,cpp}`

### Intentional differences

- Rust owns the opaque libopus decoder storage in an aligned `Vec<usize>` instead of placing the
  C `OpusMSDecoder` immediately after the C++ wrapper in a caller-owned work buffer. The object
  remains owned by the matching ADSP Opus module and uses the same libopus entry points.
- Rust represents Eden's `self` pointer comparison with the emulated work-buffer identifier and
  returns `OPUS_INVALID_STATE` before calling libopus with absent decoder storage.

## 2026-08-30 — `src/core/src/file_sys/control_metadata.rs` vs Eden `src/core/file_sys/control_metadata.{h,cpp}` (`Language` / `LANGUAGE_NAMES`)

### Intentional differences

- Rust declares the array length as `Language::Count as usize` instead of C++
  `size_t(Language::Count)`; both bind the table length to the owning NACP language enum.

### Missing items

- The existing Rust file still lacks Eden's compressed 18+-entry `LanguageEntryData` handling and
  several named late `RawNACP` fields. Those pre-existing, larger structural differences are
  outside this focused table correction; uncompressed raw NACP bytes remain preserved by the
  current fixed-size representation.

## 2026-08-30 — `src/ruzu/src/migration_worker.rs` vs Eden `src/yuzu/migration_worker.{h,cpp}` (copy activation of firmware)

### Intentional differences

- Eden removes its entire destination user directory before copying. Ruzu retains its existing
  selective, transactional migration model so configuration, keys, SDMC, NAND user data, saves,
  and mods can be merged without deleting unrelated Ruzu-owned data.
- Within that selective model, `nand/system/Contents` is now prepared as a separate exact-copy
  tree. This preserves Eden's whole-installation invariant for firmware without broadening the
  deletion boundary to the rest of Ruzu's NAND.

### Missing items

- Ruzu intentionally does not expose Eden's destructive whole-tree `Move` strategy. Its `Copy`
  and `Link` strategies remain the supported non-destructive subset documented in this file.

## 2026-08-30 — `src/core/src/hle/service/ns/application_manager_interface.rs` vs Eden `src/core/hle/service/ns/application_manager_interface.{h,cpp}` (`IsQualificationTransitionSupportedByProcessId`)

### Intentional differences

- Rust uses an explicit IPC handler with `RequestParser`/`ResponseBuilder` instead of Eden's CMIF
  `D<>` serializer. The command remains owned by the matching application-manager module and has
  the same input, result, and output ordering.

### Missing items

- The remainder of this pre-existing partial interface still has implemented Eden commands that
  are only represented in Ruzu's command inventory. They require separate ownership/parity passes.

## 2026-08-30 — `src/core/src/hle/service/acc/acc.rs` vs Eden `src/core/hle/service/acc/acc.{h,cpp}` (`IManagerForSystemService` / `GetBaasAccountManagerForSystemService`)

### Intentional differences

- Rust stores the account UUID as `u128` and constructs its service object through an `Arc<dyn
  SessionRequestHandler>` instead of Eden's `Common::UUID` and `PushIpcInterface` template. The
  object remains owned by the matching ACC source module.
- Eden passes `Core::System&` into `IManagerForSystemService`; the Rust object omits that field
  because none of Eden's currently implemented methods use it.

### Unintentional differences (to fix)

- `LoadSaveDataThumbnail` remains unimplemented intentionally because Eden also registers command
  112 with a null handler; it was not the fatal player-select divergence.

### Missing items

- Other null ACC commands outside `IManagerForSystemService` remain separate pre-existing parity
  work.

## 2026-08-30 — `src/core/src/hle/service/am/service/application_functions.rs` vs Eden `src/core/hle/service/am/service/application_functions.{h,cpp}` (`EnsureSaveData`)

### Intentional differences

- Eden returns `ResultTargetNotFound` from `SaveDataController::CreateSaveData`; Ruzu's existing
  controller represents creation as `Option<VirtualDir>`, so the matching service method converts
  `None` to the same FS result code.
- Rust returns the `Out<u64>` value from the owned method to the CMIF handler instead of receiving
  an `Out<u64>` wrapper. The successful value and response ordering remain identical.

### Unintentional differences (to fix)

- Command 20 no longer reports unconditional stub success: it consumes
  the 16-byte UUID, constructs a zero-initialized account `SaveDataAttribute` using the applet's
  program ID, creates it in `SaveDataSpaceId::User`, propagates creation failure, then returns size
  zero on success, matching Eden's ordering.

### Missing items

- Other pre-existing missing handlers in this partial interface remain
  separate parity slices.

## 2026-08-30 — `src/audio_core/src/device/audio_buffers.rs` vs Eden `src/audio_core/device/audio_buffers.h`

### Intentional differences

- Rust uses a `parking_lot::Mutex` and a lock-held `release_buffer_locked` helper instead of
  Eden's recursive mutex and nested call to `ReleaseBuffer`. The ring ownership and mutation
  ordering remain unchanged.

## 2026-08-30 — `src/core/src/hle/kernel/k_event.rs` vs Eden `src/core/hle/kernel/k_event.{h,cpp}` (`Signal` / `Clear`)

### Intentional differences

- Eden embeds `KReadableEvent` directly in `KEvent`; Ruzu resolves it from the process object map
  as an `Arc<Mutex<KReadableEvent>>`. Ruzu must therefore acquire that Rust-owned wrapper mutex
  before the scheduler lock and release it explicitly before scheduler-unlock may reschedule the
  current fiber. The scheduler guard still covers the readable-event state transition and the
  nested `KReadableEvent::Signal` / `Clear` call, preserving Eden's protected operation and
  recursive-lock behavior without introducing an AB-BA lock cycle.
- `signal_arc` and `clear_arc` remain Rust ownership adapters for callers that hold shared event
  and process objects; Eden accesses the embedded readable end directly.

### Missing items

- `KEvent` still represents upstream object ownership and post-destroy resource accounting through
  process object IDs rather than the complete `KAutoObject` lifecycle; that pre-existing structural
  gap is outside this deadlock correction.

## 2026-08-30 — `src/core/src/hle/kernel/k_server_session.rs` vs Eden `src/core/hle/kernel/k_server_session.{h,cpp}` (`OnRequest` / `NotifyAvailable`)

### Intentional differences

- Ruzu's shared-owner `ServerManager` has a host `Condvar` fallback in addition to Eden's kernel
  `MultiWait`. `KServerSession::notify_available` therefore also wakes that host-only condition;
  it does not alter or re-signal kernel event state.

### Missing items

- The pre-existing `Arc<Mutex<KSessionRequest>>` and shared-owner server adapters remain structural
  differences from Eden's intrusive request objects; they require a separate ownership pass.

## 2026-08-30 — `src/video_core/src/renderer_vulkan/graphics_pipeline.rs` vs Eden `src/video_core/renderer_vulkan/vk_graphics_pipeline.{h,cpp}` (`ConfigureDraw`)

### Intentional differences

- Eden suppresses `BindDescriptorBuffersEXT` when its scheduler-side chunk cache is unchanged.
  Ruzu records work for a worker-owned command buffer, and Vulkan validation proved that this
  recording-side cache could survive the command buffer whose binding it described. Ruzu still
  updates the scheduler cache, but records the binding immediately before every corresponding
  `SetDescriptorBufferOffsetsEXT`, making each recorded command stream self-contained.

### Missing items

- Other pre-existing graphics
  pipeline parity work remains separate.

## 2026-08-30 — `src/video_core/src/renderer_vulkan/compute_pipeline.rs` vs Eden `src/video_core/renderer_vulkan/vk_compute_pipeline.{h,cpp}` (`Configure`)

### Intentional differences

- As in the graphics path, Ruzu records `BindDescriptorBuffersEXT` immediately before the offset
  command instead of allowing the recording-side scheduler cache to omit it across a worker-owned
  command-buffer lifetime. The scheduler chunk cache is still updated for state bookkeeping.

### Missing items

- Other pre-existing compute pipeline
  parity work remains separate.

## 2026-08-30 — `src/video_core/src/engines/draw_manager.rs`, `renderer_vulkan/state_tracker.rs`, `renderer_vulkan/vk_rasterizer.rs`, and `renderer_vulkan/texture_cache.rs` vs Eden `src/video_core/engines/maxwell_3d.{h,cpp}`, `renderer_vulkan/vk_state_tracker.{h,cpp}`, `renderer_vulkan/vk_rasterizer.{h,cpp}`, and `texture_cache/texture_cache.h` (live dirty flags / `PrepareDraw`)

### Intentional differences

- Eden passes its persistent `Maxwell3D*` through the renderer. Ruzu's production draw, indirect,
  draw-texture, and clear views expose a short-lived `NonNull<[bool; 256]>` to the stable
  channel-owned dirty array; this is the narrow unsafe ownership adapter needed for the same
  single live flag owner. Snapshot-only unit-test views retain an owned fallback array.
- Eden binds host geometry inside `GraphicsPipeline::Configure`, before descriptor-buffer
  allocation. If that allocation forces `Scheduler::Finish`, Ruzu explicitly repeats
  `BindHostGeometryBuffers` on the fresh command buffer. The live StateTracker already invalidates
  the same Maxwell flags consumed by `UpdateDynamicStates`; only snapshot fixtures explicitly copy
  the invalidation mask. This preserves upstream state ownership while adapting the worker-owned
  command-buffer lifecycle.

### Unintentional differences (to fix)

- Corrected: Ruzu previously copied all 256 Maxwell dirty flags at every draw boundary and scanned
  them again to propagate consumed flags. It also combined that mirror with a second live flag
  owner in the Vulkan texture-cache adapter. All production paths now consume and mutate the one
  live channel array, matching Eden and removing the per-draw copy/scan.
- Corrected earlier in this slice: index, vertex, transform-feedback, and indirect buffer commands
  could remain in the submitted command buffer while the draw was recorded in its successor.
  Vulkan validation reported missing vertex and index bindings immediately before the deterministic
  device loss.

### Missing items

- Other pre-existing rasterizer parity work
  remains separate.

## 2026-08-30 — `src/video_core/src/renderer_vulkan/texture_cache.rs` vs Eden `src/video_core/texture_cache/texture_cache.h` and `renderer_vulkan/vk_texture_cache.{h,cpp}` (`DownloadMemory`)

### Intentional differences

- Rust reconstructs a temporary slice from the persistently mapped staging pointer after
  `Scheduler::Finish`; Eden carries the equivalent `StagingBufferRef::mapped_span` directly.
  Ownership remains with the staging pool in both implementations.

### Unintentional differences (to fix)

- Resolved: Ruzu previously allocated a new `Vec<u8>` and copied the complete mapped download
  buffer before calling `SwizzleImage`. Eden passes the mapped staging span directly after
  `runtime.Finish()`. Ruzu now preserves that order and avoids the extra allocation and full-image
  host copy.

## 2026-08-30 — `src/video_core/src/texture_cache/texture_cache.rs` vs Eden `src/video_core/texture_cache/texture_cache.h` (`RunGarbageCollector`)

### Intentional differences

- Rust collects at most the current 10/20/40-element iteration quota into a temporary vector before
  mutating the slot storage; Eden's LRU callback performs the same ordered mutations in place. The
  bounded candidate order, iteration quota, thresholds, and deletion decisions remain identical.

### Unintentional differences (to fix)

- Ruzu previously stopped the complete LRU scan
  when one aggressive deletion brought memory below `critical_memory`. Eden only quarters the
  remaining iteration quota, disables aggressive mode, and continues scanning. Ruzu now preserves
  that ordering and continuation behavior.
- Ruzu previously copied every eligible LRU identifier into a temporary vector before applying
  Eden's 10/20/40-element quota. The collection is now bounded by the current quota, matching
  Eden's early callback termination and avoiding work proportional to the complete cache each frame.
- Ruzu previously allowed both the aggressive quartering and high-priority halving transitions to
  run after one deletion. Eden uses `if`/`else if`; Ruzu now preserves that mutually exclusive
  ordering.

### Missing items

- Other pre-existing texture-cache
  parity work remains separate.

## 2026-08-30 — video-core runtime hot paths vs the corresponding Eden paths (diagnostic cleanup)

### Unintentional differences (to fix)

- Corrected: investigation-only memory, descriptor, allocation, GMMU, render-target, and
  garbage-collector probes remained in production hot paths. Even while disabled, their live
  environment lookups accounted for about 9.6% of sampled GPU-thread CPU cycles. The probes and
  behavior-changing diagnostic bypasses are now removed rather than merely cached.

## 2026-08-30 — `src/video_core/src/renderer_vulkan/graphics_pipeline.rs` and `vk_rasterizer.rs` vs Eden `src/video_core/renderer_vulkan/vk_graphics_pipeline.{h,cpp}` and `vk_rasterizer.{h,cpp}` (channel memory ownership / `PrepareDraw`)

### Intentional differences

- Ruzu retains an owning `Arc<Mutex<MemoryManager>>` in the rasterizer because channel ownership is
  shared in Rust. A copyable `MemoryManagerHandle` is the non-owning counterpart of Eden's stored
  `Tegra::MemoryManager*`; it is cleared or rebound with the current channel before its owner can be
  released.
- `GpuTickGuard` stores a non-owning callback pointer for the draw scope while the rasterizer keeps
  the callback `Arc` alive. This reproduces Eden's `SCOPE_EXIT { gpu.TickWork(); }` without an
  atomic `Arc` clone on every draw.

### Unintentional differences (to fix)

- Corrected: `GraphicsPipeline::Configure`, render-target readers, compute configuration, and the
  Vulkan buffer-cache adapter previously reacquired `Arc<Mutex<MemoryManager>>` for individual
  four/eight-byte address and descriptor reads. Eden calls its already-bound pointer directly.
  These GPU-thread paths now share the stable channel handle and retain one lock only for mutable
  `FlushCaching`.
- Corrected: releasing a non-current Vulkan channel unconditionally cleared the rasterizer's active
  owning memory reference while the buffer cache retained a separate stale owner. The current
  channel memory pointer and buffer-cache adapter are now rebound after `EraseChannel`, matching
  `ChannelSetupCaches::EraseChannel` ownership.

## 2026-08-31 — `src/ruzu/src/gtk_compat.rs` vs Eden `src/yuzu/main_window.cpp` (`MainWindow::question`)

### Intentional differences

- Eden assigns the question title to a Qt window title. Ruzu omits the equivalent GTK window-title
  property on Linux because GTK's client-side decoration renders it immediately above the identical
  primary message label; Windows and macOS retain the distinct native window title.
- GTK standard compatibility dialogs set their response as both the default and focused widget,
  repeating focus after native surface mapping because `present()` is asynchronous.

### Unintentional differences (to fix)

- Corrected: Linux question dialogs displayed the emulator name twice in adjacent headings.

## 2026-08-31 — `src/video_core/src/renderer_{vulkan,opengl}/*_rasterizer.rs` vs Eden `src/video_core/renderer_{vulkan,opengl}/*_rasterizer.cpp` (`OnCPUWrite`)

### Intentional differences

- Before marking an overlapping texture CPU-modified, Ruzu synchronously downloads any safe
  GPU-modified image covering the write. Eden calls `WriteMemory` directly. Because texture dirty
  state is image-wide rather than range-based, a small CPU write otherwise causes the next refresh
  to upload stale guest bytes over the GPU-newer remainder of the image. The download still uses
  the existing backend-owned `DownloadMemory` implementation, preserves modification-tick order,
  and runs under the same texture-cache mutex.

### Unintentional differences (to fix)

- Resolved: partial CPU writes could replace recent GPU render-target contents with stale guest
  backing when the image was refreshed.

## 2026-08-31 — `src/audio_core/src/sink/{sink_stream,sink,null_sink,cubeb_sink,sdl3_sink}.rs` vs Eden `src/audio_core/sink/{sink_stream.h,sink_stream.cpp,sink.h,null_sink.h,cubeb_sink.cpp,sdl3_sink.cpp}`

### Intentional differences

- Eden's virtual `SinkStream` base is represented by an object-safe Rust `SinkStream` trait. Its
  common callback-visible data lives in `SinkStreamBase`, shared through
  `Arc<parking_lot::Mutex<_>>`; concrete backend streams and native handles are not inside that
  mutex.
- Eden gives service users a non-owning pointer into the sink's `unique_ptr` vector. Rust uses
  `Arc<dyn SinkStream>` handles. `CloseStream` therefore calls `Finalize` before removing the
  sink-owned handle, making native resource destruction occur at the same lifecycle edge even if a
  service temporarily retains another `Arc`.
- Cubeb and SDL callbacks retain only the shared common base rather than a pointer to the complete
  concrete stream. This makes callback lifetime explicit and prevents native `Stop` from holding a
  lock needed by the callback it waits to finish.
- The backend lifecycle `Mutex` serializes explicit
  `Stopped -> Starting -> Running -> Stopping -> Finalized` transitions and native start, stop, and
  finalize calls. Eden relies on its caller lifecycle for this serialization; the state machine is
  a Rust shared-ownership adaptation and is never acquired by an audio callback.
- The existing test-only recording Null sink uses `BaseSinkStream` so command-processing tests can
  inspect samples. Production `NullSinkStreamImpl` retains Eden's no-op append/release behavior.

### Unintentional differences (to fix)

- Corrected: lifecycle ownership previously lived in generic `backend_ctl` closures attached to a
  globally locked common stream. `CubebSinkStream` and `SDLSinkStream` now own their native handles
  and implement `Finalize`, `Start`, and `Stop` in their upstream-owned modules.
- Corrected: calling native Cubeb/SDL stop while holding the common stream mutex could deadlock when
  the native API waited for a callback that needed the same mutex.
- Corrected: production `NullSink` created one stream per name/type instead of returning its single
  `NullSinkStreamImpl` for every acquisition.
- Corrected: the Cubeb input callback derived its frame count from the empty output span instead of
  the input span.
- Corrected: Cubeb's state callback marked the stream paused on `Drained`; Eden's callback is
  behaviorally empty.
- Corrected: SDL callback userdata remained retained after `SDL_DestroyAudioStream` when another
  `Arc<dyn SinkStream>` outlived `CloseStream`. Finalization now releases callback state only after
  native stream destruction, preserving Eden's teardown order.

### Missing items

- The common Rust base still uses mutex-protected `VecDeque` storage where Eden uses `RingBuffer`
  and `SPSCQueue`. This is pre-existing common-stream implementation debt, not backend lifecycle
  ownership, but remains a performance/structure difference.
- The Android Oboe backend remains a base-only stub as documented in `oboe_sink.rs`.

## 2026-08-31 — `src/core/src/arm/dynarmic/arm_dynarmic_64.rs`, `src/core/src/{cpu_manager.rs,hle/kernel/kernel.rs}`, and `src/rdynarmic/src/backend/arm64/jit_state.rs` vs host-backend ownership

### Intentional differences

- Per-thread alternate signal-stack registration is compiled only for the x64 backend that owns
  `exception_handler`; the ARM64 backend has no corresponding x64 signal handler.
- ARM64 diagnostic state reads use the ARM64 `A64JitState` and expose direct accessors for its native
  NZCV, FPCR, FPSR, vector array, and PC offset.

### Unintentional differences (to fix)

- Corrected: unconditional imports and calls into `rdynarmic::backend::x64` made `origin/main`
  fail to compile on Apple Silicon before `audio_core` could be validated.

## 2026-08-27 — `src/core/src/arm/dynarmic/arm_dynarmic_64.rs` vs Eden/Dynarmic A64 host backends

### Intentional differences

- Rust uses a private `A64JitStateHostAccess` trait to expose the state fields needed by the
  architecture-neutral core diagnostics. The implementation selects Dynarmic's native Arm64 or
  x64 state type at compile time; it does not merge or reinterpret their distinct layouts.
- The x64 alternate signal-stack registration remains confined to x64 hosts. Dynarmic's Arm64
  backend does not own the x64 exception-handler module.

## 2026-08-27 — `src/video_core/src/renderer_metal` vs Eden video-core cache contracts

### Intentional differences

- Eden has no native Metal renderer. The Metal implementation remains a ruzu-owned backend while
  implementing the same common buffer-cache, texture-cache, and rasterizer contracts as Eden's
  OpenGL and Vulkan backends.
- Metal stores a pending vertex binding for later encoder application rather than recording a
  Vulkan command or issuing an immediate OpenGL bind.

### Missing items

- The broader direct-MSL backend parity work remains tracked by its owning implementation slices;
  this entry only covers adaptation to the current Eden-derived video-core interfaces.

## 2026-09-01 — `src/core/src/hle/service/am/{lifecycle_manager.rs,window_system.rs}` vs Eden `src/core/hle/service/am/{lifecycle_manager,window_system}.{h,cpp}`

### Intentional differences

- Eden protects lifecycle exit state with `Applet::lock`. Ruzu executes HLE handlers on guest fibers, and releasing the scheduler lock may suspend such a fiber while it still owns the Rust `Mutex<Applet>`. `LifecycleManager` therefore owns an `Arc<LifecycleExitRequest>` containing the requested/acknowledged/event-cache state, and `WindowSystem` keeps a matching handle for each tracked applet. `OnExitRequested` retains Eden's window-system lock and iteration order but delivers the same `Exit` transition without acquiring the rest of the applet state.
- `LifecycleExitRequest` disables guest dispatch while its small state lock spans `Event::signal` or `Event::clear`. The event keeps its existing Rust lock order before acquiring the recursive scheduler lock; the lifecycle state is released before dispatch is enabled and a fiber switch becomes possible.

## 2026-09-01 — `src/core/src/hle/service/server_manager.rs`, `src/core/src/hle/kernel/{svc/svc_ipc.rs,k_server_session.rs}` vs Eden `src/core/hle/{service/server_manager.cpp,kernel/svc/svc_ipc.cpp,kernel/k_server_session.cpp}`

### Intentional differences

- Eden's stable `Session*` and independent deferred-list mutex are represented by stable session
  identifiers inside `Arc<Mutex<ServerManager>>`. Ruzu captures the selected session under that
  mutex, releases it for `CompleteSyncRequest`, then reacquires it only to store a deferral,
  remove a closed session, or relink the holder.
- Relinking still occurs at Eden's transaction boundary, but the bridged wakeup event is signaled
  after releasing `Mutex<ServerManager>` because the Rust event bridge can enter the scheduler and
  switch a host fiber.
- Unit-test systems synchronously drain the real shared ServerManager event loop because they do
  not start host fibers. The adapter only selects/transports events and invokes the same
  `complete_sync_request_shared` transaction as runtime.
- The Rust SVC retains an `Arc` to the calling thread while it waits. Eden obtains the same result
  through the blocking `KClientSession::SendSyncRequest`; retaining the owner explicitly avoids
  consulting a thread-local pointer after the scheduler handoff.

### Unintentional differences (to fix)

- Corrected: initial requests and deferred retries used separate transaction implementations, and
  the deferred path could execute a service callback while holding the global ServerManager mutex.
- Corrected: ownerless sessions could make `svc_ipc.rs` execute the HLE callback, response write,
  and reply inline instead of returning `ResultInvalidHandle` as an invalid routing state.
- Corrected: the test `sm:` setup published the ServiceManager after managed-port registration, so
  sessions created through that port lacked their ServerManager queue, wakeup, and owner links.

## 2026-09-01 — `src/core/src/hle/kernel/k_scheduler.rs`, `src/core/src/hle/service/server_manager.rs` vs Eden `src/core/hle/kernel/k_scheduler.{h,cpp}`, `src/core/hle/service/server_manager.{h,cpp}`

### Intentional differences

- Eden's native service callback stack is not suspended when a scheduler-lock release changes the
  selected guest thread. Ruzu's cooperative fibers share the callback's Rust stack, so an
  `HleIpcHostFiberContext` records only the current host-fiber handoff while the unified IPC
  transaction is active. Guest thread state and priority queues are still updated under the
  scheduler lock, and `RescheduleOtherCores` still interrupts other cores immediately.
- The pending state is thread-local and nesting-aware because nested HLE IPC callbacks execute on
  the same host thread. Only the outer transaction consumes the coalesced handoff request.
- Only preemption of a still-runnable guest-core service fiber is deferred. A service thread that
  has entered `Waiting` (for example in `KLightLock::LockSlowPath`) must switch immediately so the
  operation that owns the wait cannot continue before acquiring its lock. Native HLE dummy-thread
  waits also remain immediate; they block their OS thread rather than moving the Rust callback
  stack to another cooperative fiber.
- `complete_sync_request_shared` has a private transaction-body helper so callback, response,
  deferred/closed handling, relinking, wakeup, and all temporary Rust guards return before the
  outer function may enter `reschedule_current_core_raw`. Eden does not need this extra lexical
  boundary because its callback stack is not moved between cooperative fibers.

### Unintentional differences (to fix)

- Corrected: `RescheduleCurrentCore` and `RescheduleCurrentHLEThread` could suspend an HLE service
  fiber from a scheduler-lock release before the service callback returned. A callback such as a
  host syncpoint action could therefore leave its Rust mutex locked while another service fiber on
  the same host thread re-entered that mutex.
- Corrected during validation: the initial deferral also intercepted mandatory waits. In MK8D a
  contended `KLightLock` marked `HLE:nvservices` as waiting, but the IPC context let execution
  continue without lock ownership; waiter metadata was then corrupted and `RemoveWaiterImpl`
  panicked. Required wait handoffs and native dummy-thread waits now retain Eden's immediate order.

### Missing items

- Explicit waits that invoke `reschedule_current_core_raw`, and implicit waits identified by a
  non-runnable current thread at `RescheduleCurrentCore`, remain immediate handoff points; they are
  not converted into IPC preemption deferrals by this slice.

## 2026-09-01 — `src/core/src/hle/service/sm/sm.rs`, `src/core/src/hle/service/glue/time/manager.rs` vs Eden `src/core/hle/service/sm/sm.{h,cpp}`, `src/core/hle/service/glue/time/manager.{h,cpp}`

### Intentional differences

- Eden's `SM` stores a non-owning `ServiceManager&`. Ruzu represents that reference as
  `Weak<Mutex<ServiceManager>>`, upgrading it only for the duration of an IPC operation.
- Ruzu's existing split construction passes a borrowed service-manager handle to
  `TimeManager::initialize`; it is not retained in `TimeManager`, matching Eden's member layout.

### Unintentional differences (to fix)

- Corrected: `SM` and `Glue::TimeManager` retained strong `Arc` ownership of the same
  `ServiceManager` whose registered factories owned those services. The resulting cycles prevented
  service destruction between emulation sessions, leaving `TimeWorker` threads and NFC callbacks
  alive; a later controller reload could then deadlock in the stale NFC callback.

## 2026-09-01 — `src/hid_core/src/{hid_core.rs,frontend/emulated_controller.rs}` vs Eden `src/hid_core/{hid_core.cpp,frontend/emulated_controller.cpp}`

### Intentional differences

- Eden stores each `EmulatedController` behind a pointer and its NFC callback can call back into
  that controller during `ReloadFromSettings`. Ruzu's controller owner is an outer
  `Arc<Mutex<EmulatedController>>`; `HIDCore::reload_input_devices` therefore retains the same
  disconnected/connected callbacks, releases that outer mutex, dispatches them in their original
  order, then reacquires it for Eden's following `ReloadInput` step.

### Unintentional differences (to fix)

- Corrected: Ruzu dispatched the NFC disconnected callback while
  `HIDCore::reload_input_devices` still owned the controller mutex. `NfcDevice::Finalize` then
  re-entered the same controller and permanently blocked the boot thread.

## 2026-09-01 — NIFM applet reply and POSIX socket error parity

### Intentional differences

- `src/core/src/hle/service/sockets/sfdnsres.rs` rejects unresolved NSD service identifiers with
  `EAI_AGAIN` and blocks the parent `nintendo.net` domain. Eden leaves NSD expansion as a TODO and
  otherwise delegates to host DNS; Ruzu deliberately keeps official service names away from the
  host resolver and reports resolution as temporarily unavailable.
- Upstream defines native `Socket` bodies in the monolithic `core/internal_network/network.cpp`.
  Ruzu keeps the same `Socket` ownership together with its `SocketBase` trait in
  `src/core/src/internal_network/sockets.rs`, while process-wide initialization remains in
  `network.rs`.

### Unintentional differences (to fix)

- Corrected `IRequest::GetAppletInfo` from a five-word to Eden's six-word IPC response.
- Corrected `BSD::PollWork::Response` parity by skipping `WriteBuffer` when the guest supplied no
  output buffer; the former unconditional call produced an unbounded warning loop for zero-buffer
  polls.
- Corrected the POSIX socket backend to query `SO_ERROR`, forward Eden's supported `SOL_SOCKET`
  options, translate `EISCONN`, suppress `SIGPIPE` on send, preserve poll's zero-result validation,
  and return the native error when socket creation or nonblocking setup fails.
- Corrected BSD connect handling so `ISCONN` becomes success, and accepted every guest sockaddr
  length as Eden does instead of asserting on lengths other than 0, 6, or 16.

### Missing items

- Full NSD service-identifier and environment-placeholder expansion is not ported; NSD-marked
  requests use the safety failure above.
- The native Windows socket backend remains outside this POSIX correction slice.

## 2026-09-01 — `src/core/src/hle/service/ns/application_manager_interface.rs` vs Eden `src/core/hle/service/ns/application_manager_interface.{h,cpp}`

### Intentional differences

- Rust decodes the guest `u8` into the existing `StorageId` enum before calling the filesystem
  controller; valid values retain Eden's raw enum representation and behavior.

### Unintentional differences (to fix)

- Corrected command 71, `GetStorageSize`, which was advertised as implemented but had no handler.
  It now returns total size followed by free size from `FileSystemController`, matching Eden.

### Missing items

- Other command-table entries marked implemented but still lacking handlers remain outside this
  runtime-failure slice.

## 2026-09-01 — `src/core/src/hle/service/am/frontend/applet_error.rs` vs Eden `src/core/hle/service/am/frontend/applet_error.{h,cpp}`

### Intentional differences

- Ruzu defers `Exit` while a synchronous frontend callback is executing so the callback cannot
  re-enter the Rust-owned applet mutex. It performs the deferred `Exit` immediately after the
  frontend call returns.

### Unintentional differences (to fix)

- Corrected synchronous error frontends marking completion without ever executing `Exit`; the
  owner applet is now completed and its state-change event is signalled as in Eden.

## 2026-09-01 — `src/core/src/hle/service/am/service/application_functions.rs` vs Eden `src/core/hle/service/am/service/application_functions.{h,cpp}`

### Unintentional differences (to fix)

- Corrected `GetDisplayVersion` logging from info to debug. Eden uses `LOG_DEBUG`; the former info
  level flooded ordinary diagnostic runs when software repeatedly queried the version.

## 2026-09-01 — `src/core/src/hle/service/am/{applet.rs,display_layer_manager.rs,window_system.rs}` vs Eden `src/core/hle/service/am/{applet,display_layer_manager,window_system}.{h,cpp}`

### Intentional differences

- Ruzu identifies applets by ARUID in `WindowSystemInner` and upgrades them from the owned map,
  instead of retaining Eden's non-owning `Applet*` root pointers. The ordering and applet state
  transitions remain owned by `window_system.rs`.
- Fallible VI calls whose Eden return value is explicitly discarded are likewise discarded with
  `let _ =` in Rust.

### Unintentional differences (to fix)

- Corrected managed and shared library-applet layer creation to apply Eden's blending, initial
  z-index, and overlay-layer classification.
- Corrected `WindowSystem::Update` to update the overlay root, preserve its visibility, and prevent
  a foreground overlay from sharing guest input with the application.
- Corrected every applet-state update to apply Eden's foreground/obscured z-index to both managed
  and system-shared layers. This removes nondeterministic composition ordering between an
  application and a foreground library applet.

### Missing items

- Eden's reserved-applet winding state is not yet represented in Ruzu's `Applet`, so
  `UpdateAppletStateLocked` cannot yet treat an `is_winding` child as obscuring its parent.
- Eden's current `OnSystemButtonPress` routing and long-home overlay foreground toggle are not yet
  ported; `overlay_in_foreground` therefore retains its default false state unless another owner
  changes it.

## 2026-09-01 — `src/core/src/hle/service/hle_ipc.rs` vs Eden `src/core/hle/service/hle_ipc.{h,cpp}`

### Intentional differences

- Eden's `SessionRequestHandler` is not protected by a Rust mutex. Ruzu therefore logs domain slot
  indices and command IDs while holding `Mutex<SessionRequestManager>`, and obtains the service
  name only after releasing that mutex in `complete_sync_request`.

### Unintentional differences (to fix)

- Corrected domain dispatch selection and domain-handler insertion so they no longer call
  `service_name()` while holding the session-manager mutex. With a shared mutex-backed service,
  two concurrent domain requests could otherwise invert the manager/service lock order and stop
  the service permanently before either IPC response was written.

## 2026-09-01 — `src/ruzu/src/applets/error.rs` and frontend wiring vs Eden `src/yuzu/applets/qt_error.{h,cpp}`

### Intentional differences

- GTK requests cross an `mpsc` channel polled by the main loop instead of queued Qt signals. The
  ownership and ordering remain the same: the emulation thread stores the completion callback,
  the UI thread owns the dialog, and dismissing the dialog invokes the callback exactly once.
- The error request is displayed through the GTK `ErrorOverlayDialog` counterpart of Eden's
  `OverlayDialog`, rather than a native message box. Its full-width action row owns the visible
  focus border and accepts mouse, keyboard, and controller A/B input while emulation is running.
- The timestamp fallback displays Unix seconds because the GTK frontend does not currently own an
  upstream-equivalent locale-aware date formatter.

### Unintentional differences (to fix)

- Corrected the missing GUI error frontend. Ruzu previously installed no `ErrorApplet` frontend,
  so selecting HLE fell back to `DefaultErrorApplet`, which logged the error but never invoked its
  completion callback.

### Missing items

- Locale-aware date and time formatting equivalent to `QDateTime::fromSecsSinceEpoch` remains to
  be added to the GTK-only timestamp presentation path.

## 2026-09-01 — `src/core/src/frontend/applets/error.rs` vs Eden `src/core/frontend/applets/error.{h,cpp}`

### Intentional differences

- After logging an error, Ruzu's non-graphical fallback invokes the supplied completion callback.
  Eden's default frontend ignores it, which leaves an HLE Error applet permanently incomplete.
  This keeps the HLE default usable by frontends such as `ruzu-cmd` that do not install a graphical
  error display.

## 2026-09-01 — `src/common/src/settings.rs` vs Eden `src/common/settings.h`

### Intentional differences

- Ruzu now defaults `error_applet_mode` to HLE. Eden defaults to the real LLE applet, but both Eden
  and the former Ruzu path leave the tested system Error applet alive after its Close action. The
  HLE default uses the frontend callback contract and returns control to the caller; users can
  still select the real applet explicitly for LLE parity testing.

### Missing items

- The underlying LLE Error applet exit incompatibility remains; changing the explicit per-title or
  global preference to `Real applet` can still reproduce it.

## 2026-09-01 — `src/core/src/hle/service/am/service/library_applet_accessor.rs` vs Eden `src/core/hle/service/am/service/library_applet_accessor.{h,cpp}`

### Unintentional differences (to fix)

- Corrected `Start` and `Terminate` so they no longer assign `Applet::is_process_running`
  directly. Eden delegates process-state observation to `EventObserver`; in particular, a frontend
  applet has no guest process and must never be marked as running. The former assignment could
  leave its caller obscured and non-interactive after the frontend dialog completed.

## 2026-09-01 — `src/video_core/src/texture_cache/texture_cache.rs` vs Eden `src/video_core/texture_cache/texture_cache.h`

### Intentional differences

- Ruzu retains an `Arc<Mutex<MemoryManager>>` for channel ownership and a lifetime-coupled,
  non-owning `MemoryManagerHandle` for upstream-equivalent cache callbacks. Eden stores the
  non-owning `Tegra::MemoryManager*` directly.

### Unintentional differences (to fix)

- Corrected DMA download writeback in `write_downloaded_buffer` to use the existing non-owning
  channel memory handle, matching Eden's direct `gpu_memory->WriteBlockUnsafe` call. Locking the
  owning `Arc<Mutex<MemoryManager>>` while the fencing thread already owned the rasterizer inverted
  the safe-read `MemoryManager -> Rasterizer` order and could deadlock both GPU threads.

## 2026-09-02 — `src/video_core/src/renderer_vulkan/texture_cache.rs` vs Eden `src/video_core/renderer_vulkan/vk_texture_cache.{h,cpp}`

### Intentional differences

- For the raw 64-bit depth/stencil-to-color reinterpretation pair, Ruzu routes through
  `BlitImageHelper` instead of Eden's temporary-buffer round trip. Vulkan requires a
  buffer-image copy region to select one depth or stencil aspect, so Eden's combined-aspect region
  is invalid and can leave the destination depth attachment unchanged on conforming drivers.

### Unintentional differences (to fix)

- The former Rust port reproduced Eden's invalid combined depth/stencil transfer literally. The
  affected pair now uses explicit depth and stencil views while all other reinterpretation pairs
  retain the upstream ordering and buffer path.

### Missing items

- Devices without `VK_EXT_shader_stencil_export` retain the pre-existing limitation for restoring
  per-fragment stencil values through a shader; the conversion reports failure instead of issuing
  an invalid Vulkan copy.

## 2026-09-02 — `src/video_core/src/renderer_vulkan/blit_image.rs` vs Eden `src/video_core/renderer_vulkan/blit_image.{h,cpp}`

### Intentional differences

- `reinterpret_d32s8_rg32` and its two conversion pipelines are a Vulkan-correct extension to
  Eden's helper. They create per-mip, per-layer views and retain transient framebuffers until the
  scheduler tick completes, following the existing `CopyMSAA` resource lifetime model.
- The color attachment is viewed as `R32G32_UINT`: shaders use `floatBitsToUint` and
  `uintBitsToFloat`, avoiding floating-point conversion or denormal flushing while preserving the
  raw two-word texel representation.

### Unintentional differences (to fix)

- The pre-existing conversion helper had no path for this depth/stencil alias pair. Focused tests
  now cover direction selection, compatible integer attachment views, raw bit preservation, and
  mip extents.

### Missing items

- A non-extension fallback capable of scattering the second word's low byte into the stencil plane
  would be required for devices without fragment-shader stencil export.

## 2026-09-03 — `src/ruzu/src/configuration/configure_hotkeys.rs` vs Eden `src/yuzu/configuration/configure_hotkeys.{h,cpp}`

### Intentional differences

- GTK `ColumnView` cells use gesture controllers and an asynchronous sequence-dialog callback in
  place of Qt's `QTreeView::doubleClicked` signal and blocking `QDialog::exec`; the action and
  keyboard-binding columns retain Eden's ownership and both select the keyboard binding.
- GTK list cells are recycled, so each binding tracks weak label references while visible. This
  keeps Clear All and Restore Defaults model-driven without retaining GTK widgets past unbind.

### Missing items

- Controller-hotkey polling and the per-row Restore/Clear context menu remain unported.

## 2026-09-03 — `src/ruzu/src/util/sequence_dialog.rs` vs Eden `src/yuzu/util/sequence_dialog/sequence_dialog.{h,cpp}`

### Intentional differences

- The GTK dialog returns its single captured chord through a completion callback rather than a
  nested blocking event loop. Modifier-only presses and focus traversal are consumed so Tab and
  other keys can be assigned, matching Eden's `focusNextPrevChild(false)` behavior.

## 2026-09-03 — `src/ruzu/src/uisettings.rs`, `src/ruzu/src/configuration/qt_config.rs` vs Eden `src/qt_common/config/uisettings.{h,cpp}`, `src/qt_common/config/qt_config.cpp`

### Intentional differences

- Rust flattens `ContextualShortcut` into `Shortcut` while retaining the same name, group,
  keyboard sequence, controller sequence, context, and repeat fields. The static defaults remain
  owned by `uisettings` and preserve Eden's positional order.
- The frontend writes the QSettings-compatible `Shortcuts` INI section through its existing Rust
  INI adapter rather than Qt's `BeginGroup`/`Write*Setting` API.
- The product-specific default action is named `Exit ruzu`; all other action names, values,
  contexts, repeat flags, and ordering match Eden.

## 2026-09-03 — `src/ruzu/src/hotkeys.rs` vs Eden `src/yuzu/hotkeys.{h,cpp}`

### Intentional differences

- GTK application accelerators replace Qt `QShortcut` instances. Window-owned emulation input
  uses the same registry values through an explicit match because the native render surface is not
  a GTK widget and its capture-phase key handler intentionally stops event propagation.
- GTK native keypad labels such as `KP\u{2009}-` are normalized to stable GDK key names such as
  `KP_Subtract` only at the runtime registry boundary. The user-facing and persisted native label
  remains unchanged, matching Eden's display-oriented shortcut storage.
- Ruzu currently registers accelerators only for frontend actions that have functional GTK action
  counterparts; reapplying a changed binding replaces or clears the former accelerator.

### Missing items

- Controller-shortcut parsing, HID callbacks, repeat dispatch, and GTK action counterparts for the
  remaining default actions are not yet ported.

## 2026-09-03 — `src/frontend_common/src/content_manager.rs` vs Eden `src/frontend_common/content_manager.h`

### Intentional differences

- `install_nsp` receives the launcher's `FileSystemController` directly instead of a complete
  `Core::System`; the idle GTK frontend does not retain a booted system, while the controller owns
  the same User NAND `RegisteredCache` used by upstream.
- The duplicated upstream raw-copy lambdas are represented by one mechanical private helper in the
  same module. It retains the 1 MiB buffer, callback order, partial-output truncation on cancel, and
  ignored VFS write result exactly.
- Installation callbacks require `Send + Sync` because the GTK frontend performs disk work on its
  install worker rather than capturing GUI objects in the copy callback.

### Missing items

- The pre-existing `remove_dlc`, `remove_all_dlc`, `remove_update`, `remove_base_content`,
  `remove_mod`, and `verify_game_contents` stubs remain outside this installation slice.

## 2026-09-03 — `src/ruzu/src/install_dialog.rs` vs Eden `src/yuzu/install_dialog.{h,cpp}`

### Intentional differences

- GTK returns the checked paths through a response callback instead of Qt's blocking `exec()` and
  `GetFiles()` call. Every selected file is still checked initially and only checked paths are
  returned.
- A scrolling `GtkListBox` with a fixed initial dialog size replaces Qt's list-column size hint and
  `GetMinimumWidth`; this is toolkit layout mechanics only.

## 2026-09-03 — `src/ruzu/src/main_window.rs` vs Eden `src/yuzu/main_window.{h,cpp}` (`OnMenuInstallToNAND`, `InstallNCA`)

### Intentional differences

- GTK's asynchronous chooser and dialogs collect NCA title-type choices before showing progress;
  Eden obtains each choice inside its blocking install loop. File order, default `Game` selection,
  title-type mapping, NAND selection, and per-file result classification remain the same.
- Ruzu runs the complete install batch on a worker and polls progress on the GTK main loop. Eden
  uses `QtConcurrent` only for NSP files and installs NCA files synchronously; keeping VFS work off
  GTK avoids retaining GUI borrows across the copy callback without changing install ordering.
- Progress uses exact per-file byte fractions instead of Eden's integer count of 1 MiB callback
  steps. Cancellation is still observed at the same copy-block boundaries.

## 2026-09-03 — `src/ruzu/src/{uisettings.rs,configuration/qt_config.rs}` vs Eden `src/qt_common/config/{uisettings.h,qt_config.cpp}` (`roms_path`)

### Intentional differences

- The plain `roms_path` value is loaded and saved by focused Rust functions rather than as part of
  Qt's monolithic `ReadPathValues` / `SavePathValues` pass. Its `Paths\\romsPath` key, default
  marker, and update-after-confirmation ordering match upstream.

## 2026-09-03 — `src/ruzu/src/gtk_compat.rs` vs Eden `src/yuzu/main_window.cpp` (`QFileDialog::getOpenFileNames`)

### Intentional differences

- `GtkFileChooserNative` with `select_multiple=true` replaces Qt's static multi-file chooser and
  reports its `GFile` list asynchronously. Filters, initial folder, modality, acceptance, and
  cancellation semantics are preserved.

## 2026-09-04 — `src/common/src/host_memory.rs` vs Eden `src/common/host_memory.{h,cpp}`

### Intentional differences

- The Windows Rust constructor combines Eden's `Impl` constructor and `Init`; consequently the
  constructor argument performs the allocations directly while the parity-owned `backing_size`
  field is retained but not read later. A local `dead_code` allowance documents that ownership
  difference without removing upstream state.

## 2026-09-04 — `src/common/{build.rs,src/scm_rev.rs}` vs Eden `CMakeModules/GenerateSCMRev.cmake` and `src/common/scm_rev.cpp.in`

### Intentional differences

- Eden receives `CMAKE_CXX_COMPILER_ID` and `CMAKE_CXX_COMPILER_VERSION` directly from CMake. Ruzu
  queries the explicitly configured `CXX` first and, for an MSVC target without a developer-shell
  `PATH`, locates the latest installed Visual C++ toolset through Microsoft's `vswhere` before
  parsing `cl /Bv`. This reports the compiler that Cargo's native dependencies select instead of a
  hard-coded Visual Studio release.

## 2026-09-04 — `src/core/src/arm/dynarmic/dynarmic_cp15.rs` vs Eden `src/core/arm/dynarmic/dynarmic_cp15.{h,cpp}`

### Intentional differences

- The unused Rust `fence` import is now selected only for the platforms that use it. The MSVC x64
  path continues to use its dedicated barrier, matching Eden's MSVC-intrinsic versus
  `__sync_synchronize` platform split.

## 2026-09-04 — `src/core/src/file_sys/vfs/vfs_real.rs` vs Eden `src/core/file_sys/vfs/vfs_real.{h,cpp}` (`RealVfsDirectory::GetFileTimeStamp`)

### Intentional differences

- Rust obtains the same Windows creation, access, and write timestamps through
  `std::os::windows::fs::MetadataExt` instead of Eden's `_wstat64`. `FILETIME` values are converted
  from 100 ns ticks since 1601 to signed Unix seconds before preserving Eden's signed-to-unsigned
  bit pattern.

## 2026-09-04 — `src/core/src/internal_network/sockets.rs` vs Eden `src/core/internal_network/{sockets.h,network.cpp}`

### Intentional differences

- Unix-only imports and unused stub parameters are now conditionally consumed. The
  `is_non_blocking` field remains owned by `Socket` on every platform, as in Eden, with a localized
  allowance on platforms where Ruzu's backend cannot read it yet.

### Unintentional differences (to fix)

- Ruzu's native Windows socket operations remain stubs, whereas Eden implements them. This is a
  pre-existing subsystem-sized parity gap and was not replaced with warning suppression at module
  scope.

### Missing items

- Native Windows initialization, non-blocking mode, polling, and the other WinSock-backed socket
  operations remain to be ported.

## 2026-09-04 — `src/core/src/hle/service/sockets/bsd.rs` vs Eden `src/core/hle/service/sockets/bsd.cpp` (`DuplicateSocketImpl`)

### Intentional differences

- The source OS descriptor local is now compiled only on Unix, where it is consumed. Runtime
  behavior is unchanged.

### Unintentional differences (to fix)

- Eden duplicates the service's shared `Socket` ownership; Ruzu currently duplicates the Unix OS
  descriptor and returns failure on Windows. This pre-existing ownership/platform divergence needs
  a dedicated socket-lifecycle parity pass.

### Missing items

- Windows duplication and exact shared-socket ownership parity.

## 2026-09-04 — `src/rdynarmic/src/backend/x64/{a32_emit_a32.rs,a32_emit_x64_memory.rs,a32_interface.rs,a64_emit_x64.rs,a64_emit_x64_memory.rs,a64_interface.rs,abi.rs,block_of_code.rs,emit.rs,emit_aes.rs,emit_crc32.rs,emit_floating_point.rs,emit_fp_vector.rs,emit_fp_vector_convert.rs,emit_packed.rs,emit_vector_basic.rs,emit_vector_misc.rs,emit_vector_multiply.rs,emit_vector_saturated.rs,emit_vector_shift.rs,exception_handler.rs}`

### Intentional differences

- Rust function items passed to generated x64 code are now converted explicitly through
  `*const ()` before their integer address representation. This is the compiler-recommended,
  type-explicit spelling of the existing address conversion and preserves every emitted target
  address.
- Windows `CONTEXT` offsets used solely by the SEH unwind regression test are test-gated. The live
  RSP/RIP offsets remain available to the exception handler.
- The unused explanatory `UWRC_RSP` constant and unreachable global `unregister_all` helper were
  removed. Per-code-buffer registration is still paired with `unregister_code_block` from
  `BlockOfCode::drop`, so registration lifecycle and ordering are unchanged.

### Missing items

- No upstream-owned method or constant was removed; the Rust Windows SEH integration has no Eden
  source-file counterpart to compare line-for-line.

## 2026-09-04 — `src/video_core/src/macro.rs` vs Eden `src/video_core/macro.{h,cpp}` (`MacroJIT_SendThunk`, `MacroJIT_ErrorThunk`)

### Intentional differences

- The two thunk function items now pass explicitly through `*const ()` before becoming the host
  integer address consumed by the far-call emitter. The address and call sequence are unchanged.

## 2026-09-04 — `src/ruzu/src/boot.rs` vs platform renderer context ownership

### Intentional differences

- `opengl_context_source` is now allowed to be unused on every non-Linux target, matching the
  existing Linux-only GLX consumption. This GTK frontend is outside the excluded Qt `yuzu`
  source-tree parity contract.

## 2026-09-05 — `src/core/src/internal_network/network_interface.rs` vs Eden `src/core/internal_network/network_interface.{h,cpp}` (`GetAvailableNetworkInterfaces`, Windows)

### Intentional differences

- Eden stores the `GetAdaptersAddresses` result in a zeroed `std::vector<u8>`. Rust uses a zeroed
  `Vec<usize>` with the same requested byte capacity so the FFI buffer is explicitly aligned for
  `IP_ADAPTER_ADDRESSES` before it is cast.
- Eden converts `FriendlyName` with `Common::UTF16ToUTF8`; Rust uses the Windows `OsStringExt`
  conversion and a lossy UTF-8 representation for malformed host UTF-16. Valid Windows adapter
  names are identical.
- The required IP Helper declarations are enabled as target-specific `winapi` features in
  `src/core/Cargo.toml`; this replaces Eden's Windows SDK includes without changing ownership.

## 2026-09-05 — `src/core/src/internal_network/network_interface.rs` vs Eden `src/core/internal_network/network_interface.cpp` (`GetSelectedNetworkInterface`, `SelectFirstNetworkInterface`)

### Intentional differences

- At the user's request, an empty setting no longer selects the first enumerated adapter blindly.
  Ruzu first selects an IPv4 interface that is usable, appears physical, and has a gateway; it then
  falls back to a usable physical interface without a gateway.
- Loopback/link-local/multicast/unspecified addresses and names identifying common virtual-machine,
  container, tunnel, or VPN adapters are not automatic candidates. They remain in the enumerated
  list and an exact stored interface name still selects them, preserving explicit user choice.
- If only excluded adapters exist, Ruzu leaves the automatic selection empty instead of falling
  back to Eden's first entry.

## 2026-09-05 — `src/ruzu/src/configuration/configure_network.rs` vs Eden `src/yuzu/configuration/configure_network.{h,cpp}` (`SetConfiguration`)

### Intentional differences

- When no interface is configured, the GTK dropdown displays the core-selected probable physical
  interface instead of GTK's implicit first row. If core intentionally finds no automatic
  candidate, the dropdown is explicitly left unselected; selecting any listed virtual/VPN adapter
  manually is still supported and saved normally.

## 2026-09-05 — `src/ruzu/src/util/game.rs` vs Eden `src/qt_common/util/game.{h,cpp}` (`OpenRootDataFolder`)

### Intentional differences

- Eden delegates a local-file URL to `QDesktopServices`. Ruzu keeps GIO's default URI launcher on
  non-Windows hosts, but invokes `explorer.exe` directly on Windows because the bundled GTK/GIO
  runtime does not reliably provide a default `file://` URI handler there.
- The Windows command is built through a small mechanical helper so the exact executable and the
  single native path argument can be regression-tested without opening an Explorer window.

### Missing items

- The other Eden standard-folder helpers remain outside this focused root-data-folder slice.

## 2026-09-05 — `src/ruzu/src/file_menu.rs` vs Eden `src/yuzu/main_window.{h,cpp}` (`OnOpenRootDataFolder`)

### Intentional differences

- GTK registers an application-scoped `gio::SimpleAction`; its activation now delegates to the
  matching `util/game.rs` owner just as Eden's main-window slot delegates to `QtCommon::Game`.

## 2026-09-05 — `src/ruzu/src/about_dialog.rs` vs Eden `src/yuzu/about_dialog.{h,cpp}` and `src/yuzu/aboutdialog.ui`

### Intentional differences

- The Qt Designer form is represented directly by GTK widgets in the matching `about_dialog.rs`
  owner. It preserves Eden's dedicated 700x385 dialog, 200px logo, two-column content, build
  identity, wrapped description, external-link row, trademark notice, and OK response.
- Ruzu-specific copy identifies the project as a yuzu-to-Rust port produced through AI agents.
- At the user's request, Website, Source Code, Contributors, and License point to Ruzu's GitHub
  repository; Eden's Discord, Stoat, and Twitter links are deliberately omitted.
- The packaged Ruzu logo is always used instead of first looking up Eden's Qt theme icon.

### Missing items

- Eden appends its UTC build date and supports a custom idle-title format. Ruzu's generated
  `common::scm_rev` metadata does not currently expose either value, so this dialog shows the same
  build name, version, and compiler triplet used in Ruzu's main-window title.

## 2026-09-05 — `src/ruzu/src/{main_window.rs,gtk_compat.rs,i18n.rs}` vs Eden `src/yuzu/main_window.{h,cpp}` (`OpenURL`, `OnOpenQuickstartGuide`)

### Intentional differences

- Eden delegates every external URL to `QDesktopServices`. Ruzu preserves GIO on non-Windows
  systems but uses native `ShellExecuteW` on Windows because the bundled GTK/GIO runtime does not
  reliably provide an `https` URI launcher there.
- The quickstart destination exactly matches Eden's yuzu mirror guide. About-dialog hyperlinks use
  the same platform launcher.
- Ruzu's context-free translation layer still changes visible yuzu branding to ruzu, but now skips
  URI spans so the upstream `yuzu-mirror.github.io` host remains byte-for-byte intact.

## 2026-09-05 — `src/ruzu/src/{game_list.rs,util/game.rs}` vs Eden `src/yuzu/game/game_list.{h,cpp}`, `src/yuzu/main_window.{h,cpp}` (`AddPermDirPopup`, `OnGameListOpenDirectory`)

### Intentional differences

- Eden routes the game-list signal through `QDesktopServices::openUrl`. Ruzu keeps the action in `game_list.rs` and reuses the platform folder adapter in `util/game.rs`: `explorer.exe` receives one native path argument on Windows, while GIO remains the launcher on non-Windows hosts. This is required because the packaged Windows GIO runtime does not reliably register a `file://` URI handler.
- Ruzu uses a fixed translated error-dialog title; Eden includes the rejected path in its title. The path is still written to the log together with the platform error.

## 2026-09-05 — `src/common/{build.rs,src/scm_rev.rs}` vs Eden `CMakeModules/GenerateSCMRev.cmake` and `src/common/scm_rev.cpp.in`

### Intentional differences

- Eden selects release formatting when its packaging pipeline provides a `GIT-RELEASE` file and
  then exposes `GIT_TAG` as `BUILD_VERSION`. Ruzu has no CMake release-file generation step, so
  Cargo recognizes an exact `v<workspace-version>` tag at `HEAD`; source-package builders can
  provide the same value through `GIT_TAG`. Both expose the tag alone for release builds and
  retain `<ten-character-commit>-<branch>` for development builds.
- The workspace version is `0.0.1`; Windows staging and NSIS already consume that single Cargo
  version source, producing `Ruzu-Windows-0.0.1-x64-msvc` and the matching installer name.

### Missing items

- Eden's stable/nightly update-feed metadata remains outside Ruzu's current frontend scope.

## 2026-09-05 - src/rdynarmic/src/backend/arm64/a64_address_space.rs vs dynarmic/backend/arm64/a64_address_space.{h,cpp}

### Intentional differences

- Eden maintains producer/pseudo-operation links incrementally through
  `IR::Inst::Use/UndoUse` in `ir/microinstruction.{h,cpp}`. Rust's indexed arena
  rebuilds them after IR transformations, as its A32 path already does. A64 now
  restores the same invariant after callback expansion and optimization, before
  verification and emission.

## 2026-09-05 - src/rdynarmic/src/backend/arm64/emit_arm64_data_processing.rs vs dynarmic/backend/arm64/emit_arm64_data_processing.cpp

### Intentional differences

- Rust encodes the register-materialization fallback explicitly rather than
  passing an immediate/register variant through upstream's `MaybeAddSubImm`
  lambda. The instruction selection and operand bits follow `EmitAddSub`.

### Validation

- Gameplay beyond the menu is not validated.
- The full rdynarmic suite did not pass:
  `normal_callback_trampolines_populate_prelude_and_extend_cache_base` failed
  with 768 versus 800, followed by SIGABRT in the A32
  `run_existing_block_calls_arm64_prelude` test. These failures are outside the
  repaired A64 callback-expansion path; the full crate is not certified green.

## 2026-09-05 - src/ruzu_cmd/src/main.rs vs eden/src/yuzu_cmd/yuzu.cpp

### Intentional differences

- Rust exposes a general renderer override; Eden's force-null override is the
  lifecycle reference. Both update the shared setting before System/window
  creation. Rust keeps the configured OpenGL shader variant when not overridden.

## 2026-09-05 - src/video_core/src/renderer_metal/metal_blit_helper.rs vs eden/src/video_core/renderer_vulkan/blit_image.h/.cpp

### Intentional differences

- Native Metal uses depth/stencil aspect blits plus integer compute packing,
  not Vulkan fragment passes, for D32S8/RG32 reinterpretation. Helper owns the
  native pipelines; the runtime creates this helper lazily. No SPIR-V involved.
- Metal command-buffer retention and ordered, tracked encoder transitions
  replace Vulkan resource barriers and scheduler-recorded API commands.

### Missing items

- General Fermi2D runtime blits were connected by the later BlitImage slice below.
  Remaining conversion coverage is documented in that slice.

## 2026-09-05 - src/video_core/src/renderer_metal/metal_image.rs vs eden/src/video_core/renderer_vulkan/vk_texture_cache.h/.cpp (Image)

### Intentional differences

- Image owns region/subresource translation for packed D32S8 upload/download;
  the runtime supplies its conversion helper. Generic Metal texture/buffer
  transfers reject combined D32S8 instead of issuing an invalid aspect copy.
- Existing native/compatibility-storage authority is synchronized before a
  download and updated after upload; no extra CPU wait is added.

### Missing items

- MSAA D32S8 transfers are explicitly rejected, not silently approximated.
  Full Image rescaling/download-policy parity is not claimed by this change.

## 2026-09-05 - src/video_core/src/renderer_metal/metal_texture_cache.rs vs eden/src/video_core/renderer_vulkan/vk_texture_cache.h/.cpp (TextureCacheRuntime)

### Intentional differences

- The D32S8/RG32 pair uses GPU-only packing through a private intermediate
  buffer. All source regions are captured before writing destination regions,
  preserving ReinterpretImage ordering. No finish/readback between stages.
- Copy layouts retain Metal's row-alignment requirements, not Vulkan's layout.
- The packed D32S8 upload hook selects the dedicated Image transfer method.

### Missing items

- General async image download/rescaling remain incomplete. Fermi2D runtime
  blits were connected by the later BlitImage slice below.
- Gameplay performance/parity remains unverified by the packed-conversion tests.

## 2026-09-05 - src/video_core/src/engines/sw_blitter/converter.rs vs eden/src/video_core/engines/sw_blitter/converter.h/.cpp

### Intentional differences

- Existing runtime format dispatch remains. Upstream's per-format stack word
  array becomes a four-word Rust stack array, sized for the largest supported
  128-bit format. A construction-time invariant checks the maximum. No
  allocation is performed inside either pixel loop.

### Unintentional differences (to fix)

- Fixed 226 erroneous RGB_TO_SRGB_LUT literals using Eden's exact table;
  independently rechecked all 256 literals. SRGB_TO_RGB_LUT already matched.
- Fixed per-pixel Vec allocation in ConvertTo/ConvertFrom. Factory-owned
  format metadata is unchanged. This audit does not certify every old format.

## 2026-09-05 - src/video_core/src/renderer_metal/metal_blit_helper.rs vs eden/src/video_core/renderer_vulkan/blit_image.h/.cpp and host_shaders/blit_*.frag

### Intentional differences

- Native MSL render pipelines implement region scaling/filtering, depth/stencil
  export and MSAA copies: Metal's blit encoder cannot express all these Vulkan
  blit operations. Helper ownership and the BlitColor, BlitColorMSAA,
  BlitDepthStencil and ResolveDepthStencil boundaries follow Eden.
- Pixel-space rectangles are normalized against the selected source view for
  Metal sampling; nearest/linear samplers retain white border color, mip zero
  and disabled anisotropy. The existing float fragment now uses explicit LOD 0,
  as does Eden's blit_color_float.frag.
- Pipeline identity uses native attachment formats/sample count, source MSAA,
  numeric category, aspect and operation instead of a Vulkan render-pass handle.
  Operation remains in the key; color blending is disabled as in Eden's helper.
- Integer color copies use typed texture reads instead of float conversion.
  MSAA color resolves average float samples (integer resolves select sample 0);
  depth/stencil resolve selects sample 0 as in blit_depth_stencil_msaa.frag.
- Native stencil-only resolve and same-count MSAA depth/stencil copies are
  implemented instead of inheriting Vulkan driver-dependent rejection.
- New encoders end the previous producer encoder and record on the same native
  command buffer, using tracked-resource ordering. No per-copy CPU finish or
  readback is introduced. Guest draws rebind state after the helper encoder ends.
- The shared recording helper is a mechanical factorization of native encoder
  setup within the same owning module, not a new cross-module dispatcher.

### Missing items

- This slice does not implement every ConvertImage helper in Eden's class.
  Dark Souls remains unverified; matched gameplay performance remains pending.

## 2026-09-05 - src/video_core/src/renderer_metal/metal_image_view.rs vs eden/src/video_core/renderer_vulkan/vk_texture_cache.h/.cpp (ImageView)

### Intentional differences

- DepthView/StencilView equivalents for blits are lazy OnceCell-owned native
  2D/2DMS views. Existing array attachment views are retained for layered draws.
  Levels/slices are relative to the already restricted source view; a selected
  nonzero guest mip/layer must not accidentally become base image mip/layer 0.

### Unintentional differences (to fix)

- Fixed the Metal validation error caused by binding a 2DArray attachment view
  to a depth2d/texture2d shader. Native partial mip/layer tests now pass.

## 2026-09-05 - src/video_core/src/renderer_metal/metal_texture_cache.rs vs eden/src/video_core/renderer_vulkan/vk_texture_cache.h/.cpp (BlitImage)

### Intentional differences

- Runtime receives the independently boxed rasterizer-owned blit helper and
  scheduler. Params::blit_image consumes common image-view/framebuffer slots,
  synchronizes native/compatibility storage and marks the written storage.
  No second CPU-address image store or copied GetBlitImages implementation.
- FRAMEBUFFER_BLITS is enabled only with its concrete backend hook. Metal
  shader operations replace Vulkan transfer commands where Metal has no
  equivalent filtering/scaling encoder operation.
- Invalid aspect/numeric/sample combinations are reported explicitly;
  non-color/MSAA copies require matching formats and SrcCopy. Integer and
  depth/stencil blits require point filtering. The existing common void-hook
  interface logs runtime failures, like the Vulkan backend.

### Missing items

- General image rescaling and async download policy are not completed by this
  slice. Implement their prerequisites if a gameplay trace requires them.

## 2026-09-05 - src/video_core/src/renderer_metal/metal_rasterizer.rs vs eden/src/video_core/renderer_vulkan/vk_rasterizer.h/.cpp (AccelerateSurfaceCopy)

### Intentional differences

- Blit helper is boxed to keep the runtime's owner pointer stable across moves.
  AccelerateSurfaceCopy holds the cache mutex and forwards dst/src/config to
  the common cache, matching Eden's lock and argument ordering.

### Missing items

- Matched release gameplay performance/rendering measurements still required.

## 2026-09-05 - src/shader_recompiler/src/backend/msl/msl_emit_context.rs and emit_msl.rs vs eden/src/shader_recompiler/backend/glsl/emit_glsl.cpp (DefineVariables/EmitCode)

### Intentional differences

- Eden has no direct MSL emitter. The relevant source-language contract is
  GLSL DefineVariables: declare temporaries in function scope, separately from
  assignments emitted in the structured control-flow walk. MSL retains its
  existing per-InstRef names, not GLSL's reusable register allocator.
- Structured MSL programs now accumulate typed, zero-initialized declarations
  in the context and insert them at the start of the function body. Expression
  evaluation and stores remain at their original positions. Straight-line
  programs without a syntax list keep inline declarations; no nested syntax
  scopes exist in that path. Phi declarations retain their existing behavior.

### Unintentional differences (to fix)

- Fixed loop-local SSA definitions becoming out-of-scope C++ identifiers in
  loop exits. IR dominance does not imply visibility across source-language
  braces. The native compiler regression failed with an undeclared identifier
  before this change. No failed pipeline caching or draw suppression is used.

### Missing items

- Matched gameplay timing remains pending. The synthetic compiler regression
  and the exercised game shaders do not establish complete shader coverage.

## 2026-09-05 - src/video_core/src/renderer_metal/metal_shader.rs (native compiler regression)

### Intentional differences

- Native Metal-only test constructs an IR loop whose computed value is used
  by the exit block, emits direct MSL, and compiles through MTLDevice. Eden has
  no Metal counterpart; GLSL's DefineVariables establishes the scope contract.

### Unintentional differences (to fix)

- No production behavior changed in this file. The regression fails before
  the context fix with an undeclared loop value, as observed in Harbinger.

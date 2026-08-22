// SPDX-License-Identifier: GPL-3.0-or-later
//
// In-process game boot — the launcher counterpart of Eden's
// `MainWindow::BootGame` (`main_window.cpp`) + the emulation-thread setup. It drives
// the same `ruzu_core::core::System` lifecycle that `ruzu_cmd` drives, but with
// a GTK-owned native child surface instead of an SDL window, and without
// SDL's event loop (GTK owns the main loop).
//
// Boot runs on a dedicated background thread so `System::load` (heavy: ROM
// parse + shader/pipeline cache build) never blocks the GTK main thread — the
// same split Eden uses (its emulation/GPU thread does the heavy work and posts
// progress to the GUI thread via `Qt::QueuedConnection`). Presentation runs on
// the GPU thread inside `video_core`, reading `shown_state` / `framebuffer_layout`
// directly, so frames land in the native child surface with no per-frame work
// here. Renderer selection follows upstream's OpenGL/Vulkan/Null switch.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, OnceLock, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use ruzu_core::frontend::emu_window::WindowSystemInfo;
use ruzu_core::frontend::framebuffer_layout::FramebufferLayout;
use ruzu_core::perf_stats::PerfStatsResults;

use crate::loading_screen::LoadStage;

/// Frontend-owned OpenGL context source. Upstream keeps this state in
/// `GRenderWindow::main_context` and creates shared contexts from it.
#[derive(Clone)]
pub struct OpenGLContextSource {
    #[cfg(target_os = "linux")]
    glx: crate::render_window_x11::GlxContextSource,
}

impl OpenGLContextSource {
    #[cfg(target_os = "linux")]
    pub fn from_glx(glx: crate::render_window_x11::GlxContextSource) -> Self {
        Self { glx }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BootParameters {
    pub applet: ruzu_core::hle::service::am::applet_manager::FrontendAppletParameters,
    pub cabinet_mode: Option<ruzu_core::hle::service::am::frontend::applet_cabinet::CabinetMode>,
    /// Upstream `StartGameType::Global`: bypass the title's custom config.
    pub use_global_configuration: bool,
}

impl Default for BootParameters {
    fn default() -> Self {
        use ruzu_core::hle::service::am::am_types::{AppletId, AppletType};
        use ruzu_core::hle::service::am::applet_manager::{FrontendAppletParameters, LaunchType};

        Self {
            applet: FrontendAppletParameters {
                applet_id: AppletId::Application,
                applet_type: AppletType::Application,
                launch_type: LaunchType::FrontendInitiated,
                program_index: 0,
                previous_program_index: -1,
                ..FrontendAppletParameters::default()
            },
            cabinet_mode: None,
            use_global_configuration: false,
        }
    }
}

enum EmulationCommand {
    Stop,
    ForceStop,
    Pause(SyncSender<()>),
    Resume(SyncSender<()>),
    /// Graphics-relevant half of upstream `Core::System::ApplySettings()`.
    ///
    /// `System` is owned by the boot thread in Reden, so the GTK thread must
    /// marshal `Renderer().RefreshBaseSettings()` to that owner instead of
    /// touching the renderer directly.
    ApplyRendererSettings(SyncSender<()>),
    CaptureScreenshot {
        path: std::path::PathBuf,
        layout: FramebufferLayout,
    },
}

/// Assets read from the current application loader for the loading screen.
#[derive(Debug, Default)]
pub struct LoadingScreenAssets {
    pub logo: Option<Vec<u8>>,
    pub banner: Option<Vec<u8>>,
}

/// Values passed to Eden's `MainWindow::UpdateWindowTitle` after a title has
/// loaded and before disk shaders are prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningTitle {
    pub title_name: String,
    pub title_version: String,
    pub gpu_vendor: String,
}

/// Cross-thread events consumed by the GTK loading screen.
#[derive(Debug)]
pub enum LoadingEvent {
    /// Per-title/global settings have been selected and are ready for the GUI
    /// to display, matching Eden's pre-launch `UpdateStatusButtons()` point.
    ConfigurationApplied,
    TitleChanged(RunningTitle),
    Assets(LoadingScreenAssets),
    Progress {
        stage: LoadStage,
        value: usize,
        total: usize,
    },
    Started {
        program_id: u64,
    },
    FirstFrame,
    Failed {
        message: String,
        detail: String,
    },
    Stopped {
        before_first_frame: bool,
    },
    StopComplete,
}

fn running_title(system: &ruzu_core::core::System, filepath: &str) -> RunningTitle {
    use ruzu_core::file_sys::patch_manager::PatchManager;
    use ruzu_core::loader::loader::ResultStatus;

    let loader = system.get_app_loader();
    let mut title_name = String::new();
    let title_result = loader.read_title(&mut title_name);
    let mut title_version = String::new();

    if let Some(content_provider) = system.get_content_provider().cloned() {
        let filesystem_controller = system.get_filesystem_controller();
        let filesystem_controller = filesystem_controller
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let content_provider = content_provider
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let patch_manager = PatchManager::new(
            system.runtime_program_id(),
            &filesystem_controller,
            &*content_provider,
        );
        if let Some(metadata) = patch_manager.get_control_metadata().0 {
            title_version = metadata.get_version_string();
            title_name = metadata.get_application_name();
        }
    }

    if title_result != ResultStatus::Success || title_name.is_empty() {
        title_name = std::path::Path::new(filepath).file_name().map_or_else(
            || filepath.to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
    }
    let instruction_set_suffix = if system.runtime_is_64bit() {
        crate::i18n::tr("(64-bit)")
    } else {
        crate::i18n::tr("(32-bit)")
    };
    title_name = format!("{title_name} {instruction_set_suffix}");
    let gpu_vendor = system
        .gpu_core()
        .map_or_else(String::new, |gpu| gpu.get_device_vendor());
    log::info!(
        "Booting game: {:016X} | {} | {}",
        system.runtime_program_id(),
        title_name,
        title_version
    );

    RunningTitle {
        title_name,
        title_version,
        gpu_vendor,
    }
}

/// Loading event callback marshaled to the GTK main thread.
pub type LoadingEventFn = Arc<dyn Fn(LoadingEvent) + Send + Sync + 'static>;

/// A running emulation session. Dropping (or calling [`Self::stop`]) shuts the
/// system down: `System::pause` + `System::shutdown_main_process`, mirroring the
/// tail of `ruzu_cmd`'s `main`.
pub struct EmulationSession {
    command_tx: Option<Sender<EmulationCommand>>,
    join: Option<JoinHandle<()>>,
    perf_results: Arc<RwLock<PerfStatsResults>>,
    shaders_building: Arc<AtomicI32>,
    running: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    program_id: Arc<AtomicU64>,
    exit_locked: Arc<AtomicBool>,
    frontend_stop_requested: Arc<AtomicBool>,
}

impl EmulationSession {
    /// Return the most recent 500 ms performance sample while the guest runs.
    ///
    /// Upstream's GUI calls `System::GetAndResetPerfStats()` from its status
    /// timer. The Rust `System` remains owned by the boot thread, so that thread
    /// performs the same reset and publishes this copy for GTK.
    pub fn perf_stats(&self) -> Option<PerfStatsResults> {
        if !self.running.load(Ordering::Acquire) {
            return None;
        }
        Some(
            *self
                .perf_results
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// Return the latest `GPU::ShaderNotify().ShadersBuilding()` sample.
    pub fn shaders_building(&self) -> Option<i32> {
        self.running
            .load(Ordering::Acquire)
            .then(|| self.shaders_building.load(Ordering::Acquire))
    }

    pub fn program_id(&self) -> Option<u64> {
        self.running
            .load(Ordering::Acquire)
            .then(|| self.program_id.load(Ordering::Acquire))
    }

    /// Return the application id as soon as `System::Load` publishes it.
    pub fn loaded_program_id(&self) -> Option<u64> {
        let program_id = self.program_id.load(Ordering::Acquire);
        (program_id != 0).then_some(program_id)
    }

    /// Whether the running application requested that frontend exits be
    /// confirmed. Mirrors `Core::System::GetExitLocked()`.
    pub fn exit_locked(&self) -> bool {
        self.exit_locked.load(Ordering::Acquire)
    }

    /// Begin the upstream graceful shutdown path without blocking the GTK main
    /// loop. Completion is reported through [`LoadingEvent::StopComplete`].
    pub fn request_stop(&mut self) -> bool {
        if let Some(tx) = self.command_tx.take() {
            self.frontend_stop_requested.store(true, Ordering::Release);
            let _ = tx.send(EmulationCommand::Stop);
            true
        } else {
            false
        }
    }

    /// Begin the immediate shutdown path used when the frontend itself closes.
    /// Upstream `GMainWindow::ShutdownGame` calls `OnEmulationStopTimeExpired`
    /// immediately instead of waiting for the normal graceful-exit timer.
    pub fn request_force_stop(&mut self) -> bool {
        if let Some(tx) = self.command_tx.take() {
            self.frontend_stop_requested.store(true, Ordering::Release);
            let _ = tx.send(EmulationCommand::ForceStop);
            true
        } else {
            false
        }
    }

    pub fn capture_screenshot(&self, path: std::path::PathBuf, layout: FramebufferLayout) -> bool {
        self.command_tx.as_ref().is_some_and(|tx| {
            tx.send(EmulationCommand::CaptureScreenshot { path, layout })
                .is_ok()
        })
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    pub fn pause(&self) -> bool {
        if self.is_paused() {
            return true;
        }
        let Some(tx) = self.command_tx.as_ref() else {
            return false;
        };
        let (completed_tx, completed_rx) = std::sync::mpsc::sync_channel(0);
        let completed =
            tx.send(EmulationCommand::Pause(completed_tx)).is_ok() && completed_rx.recv().is_ok();
        if completed {
            self.paused.store(true, Ordering::Release);
        }
        completed
    }

    pub fn resume(&self) -> bool {
        if !self.is_paused() {
            return true;
        }
        let Some(tx) = self.command_tx.as_ref() else {
            return false;
        };
        let (completed_tx, completed_rx) = std::sync::mpsc::sync_channel(0);
        let completed =
            tx.send(EmulationCommand::Resume(completed_tx)).is_ok() && completed_rx.recv().is_ok();
        if completed {
            self.paused.store(false, Ordering::Release);
        }
        completed
    }

    /// Apply live graphics settings to the active renderer.
    ///
    /// Upstream `ConfigureDialog::ApplyConfiguration()` calls
    /// `Core::System::ApplySettings()`, whose renderer-side operation is
    /// `Renderer().RefreshBaseSettings()`. The renderer lives on Reden's boot
    /// thread, so this synchronous command preserves the same ordering before
    /// the configuration dialog reports that applying has completed.
    pub fn apply_renderer_settings(&self) -> bool {
        let Some(tx) = self.command_tx.as_ref() else {
            return false;
        };
        let (completed_tx, completed_rx) = std::sync::mpsc::sync_channel(0);
        tx.send(EmulationCommand::ApplyRendererSettings(completed_tx))
            .is_ok()
            && completed_rx.recv().is_ok()
    }

    /// Signal the boot thread to shut the system down and join it.
    pub fn stop(&mut self) {
        let _ = self.request_stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for EmulationSession {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Boot `filepath` into a new emulation session using the given render surface.
///
/// `window_info` carries the native presentation surface. On Linux,
/// `opengl_context_source` owns the root GLX share group used by the renderer
/// and asynchronous shader workers. `shown_state` / `framebuffer_layout` are
/// updated by the window on visibility and resize.
pub fn boot_game(
    window_info: WindowSystemInfo,
    drawable_size: (u32, u32),
    shown_state: Arc<AtomicBool>,
    framebuffer_layout: Arc<RwLock<FramebufferLayout>>,
    opengl_context_source: Option<OpenGLContextSource>,
    hid_core: Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
    controller_applet: Option<Arc<dyn ruzu_core::frontend::applets::controller::ControllerApplet>>,
    software_keyboard: Option<
        Arc<dyn ruzu_core::frontend::applets::software_keyboard::SoftwareKeyboardApplet>,
    >,
    tas: Option<Arc<parking_lot::Mutex<input_common::drivers::tas_input::Tas>>>,
    filepath: String,
    parameters: BootParameters,
    loading_event: LoadingEventFn,
) -> EmulationSession {
    let (command_tx, command_rx) = std::sync::mpsc::channel::<EmulationCommand>();
    let perf_results = Arc::new(RwLock::new(PerfStatsResults::default()));
    let shaders_building = Arc::new(AtomicI32::new(0));
    let running = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    let program_id = Arc::new(AtomicU64::new(0));
    let frontend_stop_requested = Arc::new(AtomicBool::new(false));
    let (exit_locked_tx, exit_locked_rx) = std::sync::mpsc::sync_channel(1);
    let boot_perf_results = Arc::clone(&perf_results);
    let boot_shaders_building = Arc::clone(&shaders_building);
    let boot_running = Arc::clone(&running);
    let boot_program_id = Arc::clone(&program_id);
    let boot_frontend_stop_requested = Arc::clone(&frontend_stop_requested);

    let join = std::thread::Builder::new()
        .name("ruzu-boot".into())
        .spawn(move || {
            run_boot(
                window_info,
                drawable_size,
                shown_state,
                framebuffer_layout,
                opengl_context_source,
                hid_core,
                controller_applet,
                software_keyboard,
                tas,
                filepath,
                parameters,
                loading_event,
                command_rx,
                boot_perf_results,
                boot_shaders_building,
                boot_running,
                boot_program_id,
                exit_locked_tx,
                boot_frontend_stop_requested,
            );
        })
        .expect("spawn boot thread");

    let exit_locked = exit_locked_rx
        .recv()
        .expect("boot thread publishes System exit-lock state");

    EmulationSession {
        command_tx: Some(command_tx),
        join: Some(join),
        perf_results,
        shaders_building,
        running,
        paused,
        program_id,
        exit_locked,
        frontend_stop_requested,
    }
}

/// The boot body, run on the boot thread. Faithful to upstream's selected
/// renderer factory and to the corresponding `ruzu_cmd` backend paths.
fn run_boot(
    window_info: WindowSystemInfo,
    drawable_size: (u32, u32),
    shown_state: Arc<AtomicBool>,
    framebuffer_layout: Arc<RwLock<FramebufferLayout>>,
    opengl_context_source: Option<OpenGLContextSource>,
    hid_core: Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
    controller_applet: Option<Arc<dyn ruzu_core::frontend::applets::controller::ControllerApplet>>,
    software_keyboard: Option<
        Arc<dyn ruzu_core::frontend::applets::software_keyboard::SoftwareKeyboardApplet>,
    >,
    tas: Option<Arc<parking_lot::Mutex<input_common::drivers::tas_input::Tas>>>,
    filepath: String,
    parameters: BootParameters,
    loading_event: LoadingEventFn,
    command_rx: Receiver<EmulationCommand>,
    perf_results: Arc<RwLock<PerfStatsResults>>,
    shaders_building: Arc<AtomicI32>,
    running: Arc<AtomicBool>,
    program_id: Arc<AtomicU64>,
    exit_locked_tx: SyncSender<Arc<AtomicBool>>,
    frontend_stop_requested: Arc<AtomicBool>,
) {
    use ruzu_core::core::{System, SystemRef, SystemResultStatus};

    let first_frame_displayed = Arc::new(AtomicBool::new(false));
    let guest_exit_requested = Arc::new(AtomicBool::new(false));

    loading_event(LoadingEvent::Progress {
        stage: LoadStage::Prepare,
        value: 0,
        total: 0,
    });

    // Upstream `GMainWindow::BootGame` selects the title's `QtConfig` before
    // constructing/loading the emulation system. The explicit Global menu
    // action restores every switchable setting instead.
    if apply_boot_configuration(&filepath, parameters.use_global_configuration) {
        hid_core.lock().reload_input_devices();
    }
    loading_event(LoadingEvent::ConfigurationApplied);

    // Log configuration (upstream logs settings during EmuWindow construction).
    common::settings::log_settings(&common::settings::values());

    // System init — upstream `Core::System system{}; system.Initialize();`.
    let tas_hid_core = Arc::clone(&hid_core);
    let mut system = System::new_with_hid_core(hid_core);
    let _ = exit_locked_tx.send(system.exit_locked_state());
    system.initialize();
    if controller_applet.is_some() || software_keyboard.is_some() {
        log::info!(
            "Installing GUI frontend applets (controller={} software_keyboard={})",
            controller_applet.is_some(),
            software_keyboard.is_some()
        );
        system.set_frontend_applet_set(
            ruzu_core::hle::service::am::frontend::applets::FrontendAppletSet {
                cabinet: None,
                controller: controller_applet,
                error: None,
                parental_controls: None,
                photo_viewer: None,
                profile_select: None,
                software_keyboard,
                web_browser: None,
            },
        );
    }
    system
        .frontend_applet_holder_mut()
        .set_current_applet_id(parameters.applet.applet_id);
    if let Some(mode) = parameters.cabinet_mode {
        system.frontend_applet_holder_mut().set_cabinet_mode(mode);
    }

    // Content provider / filesystem / factories (upstream core.cpp:367-370).
    {
        // The game-list worker owns and refreshes the process-wide manual
        // provider. Boot reuses the same union, matching QtCommon::provider.
        let content_provider = crate::game_list::frontend_content_provider_union();
        system.set_content_provider(content_provider);
        if system.get_filesystem().is_none() {
            system.set_filesystem(crate::game_list::frontend_vfs());
        }
        let vfs = system.get_filesystem().unwrap().clone();
        let filesystem_controller = system.get_filesystem_controller();
        let mut filesystem_controller = filesystem_controller.lock().unwrap();
        filesystem_controller.create_factories(Arc::clone(&vfs), false);
        if let Ok(sdmc_root) = filesystem_controller.open_sdmc() {
            if let Some(homebrew_sdmc) = crate::homebrew_vfs::make_homebrew_sdmc_view(
                vfs,
                std::path::Path::new(&filepath),
                sdmc_root,
            ) {
                log::info!(
                    "Using the standalone NRO package root as the writable SDMC layer for {}",
                    filepath
                );
                filesystem_controller.set_sdmc_open_override(Some(homebrew_sdmc));
            }
        }
        drop(filesystem_controller);
        system.clear_user_channel();
    }

    // Subsystem factory (upstream SetupForApplicationProcess): Host1x + GPU +
    // selected renderer + AudioCore. Called during `system.load()`.
    let renderer_backend = *common::settings::values().renderer_backend.get_value();
    if let Some(detail) = renderer_backend_unavailable_detail(renderer_backend) {
        log::error!("Renderer backend {renderer_backend:?} is unavailable on this host");
        loading_event(LoadingEvent::Failed {
            message: "Unable to start the game".to_owned(),
            detail: detail.to_owned(),
        });
        return;
    }
    let frame_loading_event = Arc::clone(&loading_event);
    let frame_displayed = Arc::clone(&first_frame_displayed);
    system.set_subsystem_factory(Box::new(move |system| {
        use std::sync::Arc;

        // Host1x (upstream core.cpp:277).
        let host1x =
            video_core::host1x::host1x::Host1x::new_with_system(SystemRef::from_ref(system));
        let syncpoints = host1x.syncpoint_manager().clone();
        let device_memory = host1x.memory_manager().clone();
        system.set_host1x_core(Box::new(host1x));

        // GPU (upstream core.cpp:278). The frontend-owned subsystem factory
        // replaces `VideoCore::CreateGPU` in this Rust dependency graph, so it
        // must retain that function's leading `Settings::UpdateRescalingInfo`
        // call before any renderer observes `resolution_info`.
        {
            let mut values = common::settings::values_mut();
            common::settings::update_rescaling_info(&mut values);
        }
        let system_ref = SystemRef::from_ref(&system);
        let use_async_gpu = *common::settings::values()
            .use_asynchronous_gpu_emulation
            .get_value()
            && std::env::var_os("RUZU_DISABLE_ASYNC_GPU").is_none();
        let use_nvdec = *common::settings::values().nvdec_emulation.get_value()
            != common::settings_enums::NvdecEmulation::Off;
        let gpu = Box::new(video_core::gpu::Gpu::new(use_async_gpu, use_nvdec));
        gpu.set_system_ref(system_ref);
        let gpu_ptr = gpu.as_ref() as *const video_core::gpu::Gpu as usize;

        // Mutex-free raw pointer to the process `Memory`, shared by the GPU-side
        // memory callbacks. They run on the GPU thread while holding rasterizer
        // cache locks; taking the `Memory` mutex there deadlocks against guest
        // writes that re-enter the rasterizer. Every `Memory` method used below
        // takes `&self`; the pointee lives in the `Arc<Mutex<..>>` the system
        // keeps alive for the whole session. (Faithful to `ruzu_cmd`.)
        let memory_raw: Arc<OnceLock<usize>> = Arc::new(OnceLock::new());
        fn memory_raw_of(
            cell: &OnceLock<usize>,
            memory: &Arc<std::sync::Mutex<ruzu_core::memory::memory::Memory>>,
        ) -> *const ruzu_core::memory::memory::Memory {
            *cell.get_or_init(|| {
                let guard = memory.lock().unwrap();
                &*guard as *const ruzu_core::memory::memory::Memory as usize
            }) as *const ruzu_core::memory::memory::Memory
        }

        let frame_displayed_notify: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(tas) = tas.as_ref() {
                use hid_core::hid_types::NpadIdType;
                use input_common::drivers::tas_input::TasAnalog;

                let controller = tas_hid_core
                    .lock()
                    .get_emulated_controller(NpadIdType::Player1);
                let controller = controller.lock();
                let buttons = controller
                    .get_buttons_values()
                    .iter()
                    .enumerate()
                    .fold(0u64, |buttons, (index, value)| {
                        buttons | (u64::from(value.value) << index)
                    });
                let sticks = controller.get_sticks();
                drop(controller);
                let mut tas = tas.lock();
                tas.record_input(
                    buttons,
                    TasAnalog {
                        x: sticks.left.x as f32 / 32767.0,
                        y: sticks.left.y as f32 / 32767.0,
                    },
                    TasAnalog {
                        x: sticks.right.x as f32 / 32767.0,
                        y: sticks.right.y as f32 / 32767.0,
                    },
                );
                tas.update_thread();
            }
            if !frame_displayed.swap(true, Ordering::AcqRel) {
                frame_loading_event(LoadingEvent::FirstFrame);
            }
        });
        let frame_end_notify: Arc<dyn Fn() + Send + Sync> = Arc::new(move || unsafe {
            let gpu_ref = &*(gpu_ptr as *const video_core::gpu::Gpu);
            gpu_ref.renderer_frame_end_notify();
        });
        let renderer: Box<dyn video_core::renderer_base::RendererBase> = match renderer_backend {
            common::settings_enums::RendererBackend::OpenGlGlsl
            | common::settings_enums::RendererBackend::OpenGlGlasm
            | common::settings_enums::RendererBackend::OpenGlSpirV => {
                #[cfg(target_os = "linux")]
                {
                    let source = opengl_context_source.as_ref().ok_or_else(|| {
                        "OpenGL renderer selected without a GLX context source".to_owned()
                    })?;
                    let context = Box::new(source.glx.create_context().map_err(|error| {
                        format!("Failed to create OpenGL renderer context: {error}")
                    })?);
                    let worker_source = source.glx.clone();
                    let shared_context_factory: video_core::renderer_opengl::gl_shader_context::SharedContextFactory =
                        Arc::new(move || {
                            Box::new(worker_source.create_offscreen_context().unwrap_or_else(|error| {
                                panic!("failed to create shared OpenGL shader context: {error}")
                            }))
                        });
                    let mut renderer = video_core::renderer_opengl::RendererOpenGL::new(
                        crate::render_window_x11::GlxContextSource::get_proc_address,
                        syncpoints.clone(),
                        Arc::clone(&device_memory),
                        // SAFETY: this renderer is immediately bound to `gpu` below;
                        // `Gpu` drops the renderer and shader workers first.
                        unsafe { gpu.shader_notify_handle() },
                        false,
                        context,
                        Some(shared_context_factory),
                        Arc::clone(&framebuffer_layout),
                        Arc::clone(&frame_end_notify),
                        Arc::clone(&frame_displayed_notify),
                    )
                    .map_err(|error| format!("Failed to create OpenGL renderer: {error}"))?;
                    renderer.rasterizer_mut().set_invalidate_gpu_cache_callback(
                        Arc::new(move || unsafe {
                            (&*(gpu_ptr as *const video_core::gpu::Gpu)).invalidate_gpu_cache();
                        }),
                    );

                    // The shader cache consumes GPU virtual addresses. Route
                    // them through the bound channel's GMMU, using the same
                    // mutex-free CPU/device reader as the general GPU path.
                    let system_ref_gpu = SystemRef::from_ref(&system);
                    let memory_raw_shader = Arc::clone(&memory_raw);
                    renderer.rasterizer_mut().set_gpu_memory_reader(Arc::new(
                        move |gpu_va, destination: &mut [u8]| {
                            let cpu_reader = |address: u64, output: &mut [u8]| {
                                let system = system_ref_gpu.get();
                                if let Some(memory) = system.memory_shared() {
                                    let memory = unsafe {
                                        &*memory_raw_of(&memory_raw_shader, &memory)
                                    };
                                    if memory.read_block(address, output) {
                                        return;
                                    }
                                }
                                let device_memory = system.device_memory();
                                let base = ruzu_core::device_memory::dram_memory_map::BASE;
                                if address >= base {
                                    let offset = (address - base) as usize;
                                    let backing = device_memory.buffer.backing_base_pointer();
                                    unsafe {
                                        std::ptr::copy_nonoverlapping(
                                            backing.add(offset),
                                            output.as_mut_ptr(),
                                            output.len(),
                                        );
                                    }
                                }
                            };
                            unsafe {
                                (&*(gpu_ptr as *const video_core::gpu::Gpu)).read_gpu_memory(
                                    gpu_va,
                                    destination,
                                    &cpu_reader,
                                );
                            }
                        },
                    ));
                    Box::new(renderer)
                }
                #[cfg(not(target_os = "linux"))]
                {
                    return Err(
                        "The GTK OpenGL context bridge is not available on this platform"
                            .to_owned(),
                    );
                }
            }
            common::settings_enums::RendererBackend::Vulkan => {
                #[cfg(target_os = "macos")]
                {
                    let _ = drawable_size;
                    Box::new(
                        video_core::renderer_metal::renderer_metal::RendererMetal::new(
                            &window_info,
                            Arc::clone(&shown_state),
                            Arc::clone(&framebuffer_layout),
                            frame_displayed_notify,
                            frame_end_notify,
                            syncpoints.clone(),
                            Arc::clone(&device_memory),
                        )
                        .map_err(|error| format!("Failed to create Metal renderer: {error}"))?,
                    )
                }
                #[cfg(not(target_os = "macos"))]
                {
                    Box::new(
                        video_core::renderer_vulkan::renderer_vulkan::RendererVulkan::new(
                            // SAFETY: this renderer is immediately bound to `gpu` below;
                            // `Gpu` drops the renderer before its shader notifier.
                            unsafe { gpu.shader_notify_handle() },
                            &window_info,
                            drawable_size,
                            Arc::clone(&shown_state),
                            Arc::clone(&framebuffer_layout),
                            frame_displayed_notify,
                            frame_end_notify,
                            syncpoints.clone(),
                            Arc::clone(&device_memory),
                        )
                        .map_err(|error| format!("Failed to create Vulkan renderer: {error}"))?,
                    )
                }
            }
            common::settings_enums::RendererBackend::Null => Box::new(
                video_core::renderer_null::renderer_null::RendererNull::new(syncpoints.clone()),
            ),
        };
        gpu.bind_renderer(renderer);

        // GPU-side guest memory reader (SMMU → page table → DRAM-direct).
        let system_ref = SystemRef::from_ref(&system);
        let memory_raw_reader = memory_raw.clone();
        gpu.set_guest_memory_reader(Arc::new(move |addr, output: &mut [u8]| {
            let sys = system_ref.get();
            if let Some(host1x) = sys.host1x_core() {
                let host_ptr = host1x.smmu_lookup(addr);
                if host_ptr != 0 {
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            host_ptr as *const u8,
                            output.as_mut_ptr(),
                            output.len(),
                        );
                    }
                    return true;
                }
            }
            if let Some(memory) = sys.memory_shared() {
                let m = unsafe { &*memory_raw_of(&memory_raw_reader, &memory) };
                if m.read_block(addr, output) {
                    return true;
                }
                let dm = sys.device_memory();
                let base = ruzu_core::device_memory::dram_memory_map::BASE;
                if addr >= base {
                    let offset = (addr - base) as usize;
                    let backing = dm.buffer.backing_base_pointer();
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            backing.add(offset),
                            output.as_mut_ptr(),
                            output.len(),
                        );
                    }
                    return true;
                }
            }
            false
        }));

        // GPU-side guest memory writer (same resolution order).
        let system_ref = SystemRef::from_ref(&system);
        let memory_raw_writer = memory_raw.clone();
        gpu.set_guest_memory_writer(Arc::new(move |addr, data: &[u8]| {
            let sys = system_ref.get();
            if let Some(host1x) = sys.host1x_core() {
                let host_ptr = host1x.smmu_lookup(addr);
                if host_ptr != 0 {
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            data.as_ptr(),
                            host_ptr as *mut u8,
                            data.len(),
                        );
                    }
                    return;
                }
            }
            if let Some(memory) = sys.memory_shared() {
                let m = unsafe { &*memory_raw_of(&memory_raw_writer, &memory) };
                if m.write_block(addr, data) {
                    return;
                }
            }
            let dm = sys.device_memory();
            let base = ruzu_core::device_memory::dram_memory_map::BASE;
            if addr >= base {
                let offset = (addr - base) as usize;
                let backing = dm.buffer.backing_base_pointer();
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        data.as_ptr(),
                        backing.add(offset) as *mut u8,
                        data.len(),
                    );
                }
            }
        }));

        // GPU VA → CPU VA translator for rasterizer-side query writes.
        let gpu_ptr_for_translator = gpu.as_ref() as *const video_core::gpu::Gpu;
        unsafe { gpu.install_gpu_to_cpu_translator(gpu_ptr_for_translator) };

        system.set_gpu_core(gpu);

        // AudioCore (upstream core.cpp:283).
        let ac = audio_core::AudioCore::new(SystemRef::from_ref(system));
        system.set_audio_core(Box::new(ac));
        Ok(())
    }));

    // Load the ROM (upstream `system.Load(...)`). Triggers the factory above.
    let load_result = system.load_with_parameters(&filepath, parameters.applet);
    if load_result != SystemResultStatus::Success {
        log::error!("Failed to load ROM '{filepath}': {load_result:?}");
        loading_event(LoadingEvent::Failed {
            message: "Unable to start the game".to_owned(),
            detail: load_error_detail(load_result).to_owned(),
        });
        return;
    }
    program_id.store(system.runtime_program_id(), Ordering::Release);
    loading_event(LoadingEvent::TitleChanged(running_title(
        &system, &filepath,
    )));

    let loader = system.get_app_loader();
    let mut assets = LoadingScreenAssets::default();
    let mut buffer = Vec::new();
    if loader.read_banner(&mut buffer) == ruzu_core::loader::loader::ResultStatus::Success {
        assets.banner = Some(std::mem::take(&mut buffer));
    }
    if loader.read_logo(&mut buffer) == ruzu_core::loader::loader::ResultStatus::Success {
        assets.logo = Some(buffer);
    }
    loading_event(LoadingEvent::Assets(assets));

    // Build the disk pipeline cache before starting execution (upstream order).
    if *common::settings::values().use_disk_shader_cache.get_value() {
        if let Some(gpu_any) = system.gpu_core() {
            if let Some(gpu) = gpu_any.as_any().downcast_ref::<video_core::gpu::Gpu>() {
                let mut renderer_guard = gpu.renderer();
                if let Some(renderer) = renderer_guard.as_mut() {
                    let rasterizer = renderer.read_rasterizer();
                    unsafe {
                        if let Some(rasterizer) = rasterizer.as_mut() {
                            let loading_event = Arc::clone(&loading_event);
                            let callback: video_core::rasterizer_interface::DiskResourceLoadCallback =
                                Arc::new(move |stage, value, total| {
                                    let stage = match stage {
                                        video_core::rasterizer_interface::LoadCallbackStage::Prepare => {
                                            LoadStage::Prepare
                                        }
                                        video_core::rasterizer_interface::LoadCallbackStage::Build => {
                                            LoadStage::Build
                                        }
                                        video_core::rasterizer_interface::LoadCallbackStage::Complete => {
                                            LoadStage::Complete
                                        }
                                    };
                                    loading_event(LoadingEvent::Progress {
                                        stage,
                                        value,
                                        total,
                                    });
                                });
                            rasterizer.load_disk_resources(
                                system.runtime_program_id(),
                                Arc::clone(&frontend_stop_requested),
                                callback,
                            );
                        }
                    }
                }
            }
        }
    }

    // Upstream emits Complete immediately after disk resources are loaded and
    // before releasing the graphics context / starting the GPU.
    loading_event(LoadingEvent::Progress {
        stage: LoadStage::Complete,
        value: 0,
        total: 0,
    });

    // GPU start (upstream `system.GPU().Start()`).
    if let Some(gpu_any) = system.gpu_core() {
        if let Some(gpu) = gpu_any.as_any().downcast_ref::<video_core::gpu::Gpu>() {
            gpu.start();
        }
    }

    system.get_cpu_manager().on_gpu_ready();
    let exit_requested = Arc::clone(&guest_exit_requested);
    system.register_exit_callback(Box::new(move || {
        exit_requested.store(true, Ordering::Release);
    }));

    // Run the guest (upstream `system.Run()`): starts CPU threads in background.
    system.run();
    loading_event(LoadingEvent::Started {
        program_id: system.runtime_program_id(),
    });

    // GTK owns the main event loop. The boot thread retains ownership of
    // `System`, samples the same counters as upstream's 500 ms GUI timer, and
    // waits for a stop request between samples.
    let (stopped_by_frontend, force_stop) = loop {
        if guest_exit_requested.load(Ordering::Acquire) {
            break (false, false);
        }
        match command_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(EmulationCommand::Stop) | Err(RecvTimeoutError::Disconnected) => {
                frontend_stop_requested.store(true, Ordering::Release);
                break (true, false);
            }
            Ok(EmulationCommand::ForceStop) => {
                frontend_stop_requested.store(true, Ordering::Release);
                break (true, true);
            }
            Ok(EmulationCommand::CaptureScreenshot { path, layout }) => {
                request_screenshot(&system, path, layout);
            }
            Ok(EmulationCommand::Pause(completed)) => {
                system.pause();
                let _ = completed.send(());
            }
            Ok(EmulationCommand::Resume(completed)) => {
                system.run();
                let _ = completed.send(());
            }
            Ok(EmulationCommand::ApplyRendererSettings(completed)) => {
                if let Some(gpu) = system
                    .gpu_core()
                    .and_then(|gpu| gpu.as_any().downcast_ref::<video_core::gpu::Gpu>())
                {
                    gpu.refresh_renderer_settings();
                }
                let _ = completed.send(());
            }
            Err(RecvTimeoutError::Timeout) => {
                let sample = system.get_and_reset_perf_stats();
                *perf_results
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = sample;
                let building = system
                    .gpu_core()
                    .and_then(|gpu| gpu.as_any().downcast_ref::<video_core::gpu::Gpu>())
                    .map(|gpu| gpu.shader_notify().shaders_building())
                    .unwrap_or(0);
                shaders_building.store(building, Ordering::Release);
                running.store(true, Ordering::Release);
            }
        }
    };
    running.store(false, Ordering::Release);
    shaders_building.store(0, Ordering::Release);

    if stopped_by_frontend {
        // Upstream `RequestGameExit`: let the application process its normal
        // exit request before `OnEmulationStopTimeExpired` forces shutdown.
        system.set_shutting_down(true);
        system.set_exit_requested(true);
        system.get_applet_manager().request_exit();
        if !force_stop {
            let timeout = if system.debugger_enabled() {
                Duration::ZERO
            } else if system.get_exit_locked() {
                Duration::from_secs(5)
            } else {
                Duration::from_secs(1)
            };
            let deadline = std::time::Instant::now() + timeout;
            while !guest_exit_requested.load(Ordering::Acquire)
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    log::info!("Emulation stopping: pause + shutdown");
    system.pause();
    system.shutdown_main_process();
    loading_event(terminal_event_after_shutdown(
        frontend_stop_requested.load(Ordering::Acquire),
        first_frame_displayed.load(Ordering::Acquire),
    ));
}

/// Select global or per-title settings in upstream `GMainWindow::BootGame`
/// order, before `System::Initialize` and `System::Load` consume them.
fn apply_boot_configuration(filepath: &str, use_global_configuration: bool) -> bool {
    {
        let mut values = common::settings::values_mut();
        common::settings::restore_global_state(&mut values, false);
        values.players.set_global(true);
    }
    common::settings::set_configuring_global(true);

    if use_global_configuration {
        return false;
    }

    let Some(program_id) = read_program_id(filepath) else {
        return false;
    };
    let config_path = per_game_config_path(program_id, filepath);
    common::settings::set_configuring_global(false);
    let mut config = frontend_common::config::BaseConfig::new(
        frontend_common::config::ConfigType::PerGameConfig,
    );
    config.initialize(&config_path);
    crate::configuration::qt_config::load_per_game_control_values(&config_path);
    common::settings::set_configuring_global(true);
    true
}

/// Resolve the title id through the same loader path upstream creates before
/// constructing its per-game `QtConfig`.
fn read_program_id(filepath: &str) -> Option<u64> {
    use ruzu_core::file_sys::fs_filesystem::OpenMode;
    use ruzu_core::file_sys::registered_cache::ContentProviderUnion;
    use ruzu_core::file_sys::vfs::vfs_real::RealVfsFilesystem;
    use ruzu_core::hle::service::filesystem::filesystem::FileSystemController;
    use ruzu_core::loader::loader::{get_loader, ResultStatus, System as LoaderSystem};

    let vfs = RealVfsFilesystem::new();
    let content_provider = Arc::new(std::sync::Mutex::new(ContentProviderUnion::new()));
    let mut controller = FileSystemController::new();
    controller.set_content_provider(Arc::clone(&content_provider));
    controller.create_factories(vfs.clone(), false);
    let controller = Arc::new(std::sync::Mutex::new(controller));
    let mut loader_system = LoaderSystem::new(Some(content_provider), Some(controller));
    let file = vfs.arc_open_file(filepath, OpenMode::READ)?;
    let loader = get_loader(&mut loader_system, file, 0, 0)?;
    let mut program_id = 0;
    (loader.read_program_id(&mut program_id) == ResultStatus::Success).then_some(program_id)
}

fn per_game_config_path(program_id: u64, filepath: &str) -> std::path::PathBuf {
    let filename = if program_id == 0 {
        std::path::Path::new(filepath)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("game")
            .to_string()
    } else {
        format!("{program_id:016X}")
    };
    common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::ConfigDir)
        .join("custom")
        .join(format!("{filename}.ini"))
}

fn request_screenshot(
    system: &ruzu_core::core::System,
    path: std::path::PathBuf,
    layout: FramebufferLayout,
) {
    let Some(gpu) = system
        .gpu_core()
        .and_then(|gpu| gpu.as_any().downcast_ref::<video_core::gpu::Gpu>())
    else {
        log::error!("Cannot capture screenshot: GPU renderer is unavailable");
        return;
    };
    let mut renderer = gpu.renderer();
    let Some(renderer) = renderer.as_mut() else {
        log::error!("Cannot capture screenshot: renderer is unavailable");
        return;
    };
    if renderer.is_screenshot_pending() {
        log::warn!("A screenshot is already requested or in progress");
        return;
    }

    let width = layout.width;
    let height = layout.height;
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    let pixels_ptr = pixels.as_mut_ptr().cast();
    renderer.request_screenshot(
        pixels_ptr,
        Box::new(move |invert_y| {
            if invert_y {
                let stride = width as usize * 4;
                for y in 0..height as usize / 2 {
                    let opposite = height as usize - y - 1;
                    let (top, bottom) = pixels.split_at_mut(opposite * stride);
                    top[y * stride..(y + 1) * stride].swap_with_slice(&mut bottom[..stride]);
                }
            }
            let result = (|| -> Result<(), Box<dyn std::error::Error>> {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let stride = cairo::Format::Rgb24.stride_for_width(width)?;
                let surface = cairo::ImageSurface::create_for_data(
                    pixels,
                    cairo::Format::Rgb24,
                    width as i32,
                    height as i32,
                    stride,
                )?;
                let mut file = std::fs::File::create(&path)?;
                surface.write_to_png(&mut file)?;
                Ok(())
            })();
            match result {
                Ok(()) => log::info!("Screenshot saved to {:?}", path),
                Err(error) => log::error!("Failed to save screenshot to {:?}: {error}", path),
            }
        }),
        layout,
    );
}

/// Select the terminal frontend event after the emulation thread has completed
/// `ShutdownMainProcess`. Upstream emits `QThread::finished` only after that
/// teardown; publishing earlier makes GTK join the still-running boot thread.
fn terminal_event_after_shutdown(
    frontend_stop_requested: bool,
    first_frame_displayed: bool,
) -> LoadingEvent {
    if frontend_stop_requested {
        LoadingEvent::StopComplete
    } else {
        LoadingEvent::Stopped {
            before_first_frame: !first_frame_displayed,
        }
    }
}

fn load_error_detail(status: ruzu_core::core::SystemResultStatus) -> &'static str {
    use ruzu_core::core::SystemResultStatus;

    match status {
        SystemResultStatus::ErrorGetLoader => "The ROM format is not supported.",
        SystemResultStatus::ErrorVideoCore => {
            "The video renderer could not be initialized. Check the log for details."
        }
        SystemResultStatus::ErrorLoader => {
            "The game data could not be loaded. Check the log for details."
        }
        _ => "An unknown error occurred while loading the game. Check the log for details.",
    }
}

fn renderer_backend_unavailable_detail(
    backend: common::settings_enums::RendererBackend,
) -> Option<&'static str> {
    use common::settings_enums::RendererBackend;

    if cfg!(all(target_os = "macos", target_arch = "aarch64"))
        && matches!(
            backend,
            RendererBackend::OpenGlGlsl
                | RendererBackend::OpenGlGlasm
                | RendererBackend::OpenGlSpirV
        )
    {
        Some(
            "The video renderer could not be initialized.\n\
             On Apple Silicon, only the Vulkan renderer is supported. \
             Select Vulkan in Configure > Graphics.",
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruzu_core::core::SystemResultStatus;

    #[test]
    fn default_boot_parameters_launch_an_application() {
        use ruzu_core::hle::service::am::am_types::{AppletId, AppletType};
        use ruzu_core::hle::service::am::applet_manager::LaunchType;

        let parameters = BootParameters::default();
        assert_eq!(parameters.applet.applet_id, AppletId::Application);
        assert_eq!(parameters.applet.applet_type, AppletType::Application);
        assert_eq!(parameters.applet.launch_type, LaunchType::FrontendInitiated);
        assert_eq!(parameters.applet.previous_program_index, -1);
        assert!(parameters.cabinet_mode.is_none());
        assert!(!parameters.use_global_configuration);
    }

    #[test]
    fn per_game_config_uses_title_id_or_filename_like_upstream() {
        let title = per_game_config_path(0x0100_1234_5678_9000, "/games/ignored.nsp");
        assert!(title.ends_with("custom/0100123456789000.ini"));

        let homebrew = per_game_config_path(0, "/games/sample.nro");
        assert!(homebrew.ends_with("custom/sample.nro.ini"));
    }

    #[test]
    fn requesting_stop_is_idempotent_and_marks_frontend_shutdown() {
        let (command_tx, command_rx) = std::sync::mpsc::channel();
        let frontend_stop_requested = Arc::new(AtomicBool::new(false));
        let mut session = EmulationSession {
            command_tx: Some(command_tx),
            join: None,
            perf_results: Arc::new(RwLock::new(PerfStatsResults::default())),
            shaders_building: Arc::new(AtomicI32::new(0)),
            running: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            program_id: Arc::new(AtomicU64::new(0)),
            exit_locked: Arc::new(AtomicBool::new(false)),
            frontend_stop_requested: Arc::clone(&frontend_stop_requested),
        };

        assert!(session.request_stop());
        assert!(frontend_stop_requested.load(Ordering::Acquire));
        assert!(matches!(command_rx.recv(), Ok(EmulationCommand::Stop)));
        assert!(!session.request_stop());
    }

    #[test]
    fn requesting_force_stop_bypasses_the_graceful_stop_command() {
        let (command_tx, command_rx) = std::sync::mpsc::channel();
        let frontend_stop_requested = Arc::new(AtomicBool::new(false));
        let mut session = EmulationSession {
            command_tx: Some(command_tx),
            join: None,
            perf_results: Arc::new(RwLock::new(PerfStatsResults::default())),
            shaders_building: Arc::new(AtomicI32::new(0)),
            running: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            program_id: Arc::new(AtomicU64::new(0)),
            exit_locked: Arc::new(AtomicBool::new(false)),
            frontend_stop_requested: Arc::clone(&frontend_stop_requested),
        };

        assert!(session.request_force_stop());
        assert!(frontend_stop_requested.load(Ordering::Acquire));
        assert!(matches!(command_rx.recv(), Ok(EmulationCommand::ForceStop)));
        assert!(!session.request_force_stop());
    }

    #[test]
    fn pause_and_resume_round_trip_updates_session_state() {
        let (command_tx, command_rx) = std::sync::mpsc::channel();
        let mut session = EmulationSession {
            command_tx: Some(command_tx),
            join: None,
            perf_results: Arc::new(RwLock::new(PerfStatsResults::default())),
            shaders_building: Arc::new(AtomicI32::new(0)),
            running: Arc::new(AtomicBool::new(true)),
            paused: Arc::new(AtomicBool::new(false)),
            program_id: Arc::new(AtomicU64::new(0)),
            exit_locked: Arc::new(AtomicBool::new(false)),
            frontend_stop_requested: Arc::new(AtomicBool::new(false)),
        };
        let worker = std::thread::spawn(move || {
            let Ok(EmulationCommand::Pause(completed)) = command_rx.recv() else {
                panic!("expected pause command");
            };
            completed.send(()).unwrap();
            let Ok(EmulationCommand::Resume(completed)) = command_rx.recv() else {
                panic!("expected resume command");
            };
            completed.send(()).unwrap();
            assert!(matches!(command_rx.recv(), Ok(EmulationCommand::Stop)));
        });

        assert!(!session.is_paused());
        assert!(session.pause());
        assert!(session.is_paused());
        assert!(session.resume());
        assert!(!session.is_paused());
        session.stop();
        worker.join().unwrap();
    }

    #[test]
    fn terminal_event_is_selected_only_after_shutdown_completes() {
        assert!(matches!(
            terminal_event_after_shutdown(false, true),
            LoadingEvent::Stopped {
                before_first_frame: false
            }
        ));
        assert!(matches!(
            terminal_event_after_shutdown(false, false),
            LoadingEvent::Stopped {
                before_first_frame: true
            }
        ));
        assert!(matches!(
            terminal_event_after_shutdown(true, true),
            LoadingEvent::StopComplete
        ));
    }

    #[test]
    fn load_errors_have_frontend_facing_details() {
        assert_eq!(
            load_error_detail(SystemResultStatus::ErrorGetLoader),
            "The ROM format is not supported."
        );
        assert!(load_error_detail(SystemResultStatus::ErrorVideoCore).contains("renderer"));
        assert!(load_error_detail(SystemResultStatus::ErrorUnknown).contains("unknown"));
    }

    #[test]
    fn renderer_error_is_specific_to_opengl_on_apple_silicon() {
        use common::settings_enums::RendererBackend;

        let apple_silicon = cfg!(all(target_os = "macos", target_arch = "aarch64"));
        for backend in [
            RendererBackend::OpenGlGlsl,
            RendererBackend::OpenGlGlasm,
            RendererBackend::OpenGlSpirV,
        ] {
            let detail = renderer_backend_unavailable_detail(backend);
            assert_eq!(detail.is_some(), apple_silicon);
            if let Some(detail) = detail {
                assert!(detail.contains("Apple Silicon"));
                assert!(detail.contains("only the Vulkan renderer is supported"));
                assert!(detail.contains('\n'));
            }
        }
        assert_eq!(
            renderer_backend_unavailable_detail(RendererBackend::Vulkan),
            None
        );
        assert_eq!(
            renderer_backend_unavailable_detail(RendererBackend::Null),
            None
        );
    }
}

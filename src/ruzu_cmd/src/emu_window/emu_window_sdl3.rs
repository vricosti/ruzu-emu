// SPDX-FileCopyrightText: 2016 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Base SDL3 emulator window.
//!
//! Port of `yuzu_cmd/emu_window/emu_window_sdl3.h` and
//! `yuzu_cmd/emu_window/emu_window_sdl3.cpp`.
//!
//! `EmuWindowSdl3` is the base SDL3-backed window type. It handles SDL event
//! processing, keyboard/mouse/touch input forwarding to `InputSubsystem`, and
//! window lifecycle management. Derived types provide the graphics-context
//! specific initialization (OpenGL, Vulkan, Null).

use sdl3::sys::everything as sdl;
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use common::settings_enums::FullscreenMode;
use input_common::drivers::mouse::MouseButton;
use input_common::InputSubsystem;
use ruzu_core::core::SystemRef;
use ruzu_core::frontend::framebuffer_layout::{
    default_frame_layout, FramebufferLayout, ScreenUndocked,
};
use ruzu_core::perf_stats::PerfStatsResults;

// SDL_TOUCH_MOUSEID is defined in SDL_touch.h as ((Uint32)-1).
// It is not exported by sdl3-sys as a Rust constant, so we define it here.
const SDL_TOUCH_MOUSEID: sdl::SDL_MouseID = sdl::SDL_TOUCH_MOUSEID;

/// Whether the environment-gated benchmark sampler owns the destructive
/// `PerfStats` read. The title bar reuses its last sample while this is set.
static PERF_LOG_ACTIVE: AtomicBool = AtomicBool::new(false);
static PERF_LOG_LAST_FPS_MILLI: AtomicU64 = AtomicU64::new(0);
static PERF_LOG_LAST_SPEED_MILLI: AtomicU64 = AtomicU64::new(0);

fn perf_log_last_results() -> PerfStatsResults {
    PerfStatsResults {
        average_game_fps: PERF_LOG_LAST_FPS_MILLI.load(Ordering::Relaxed) as f64 / 1_000.0,
        emulation_speed: PERF_LOG_LAST_SPEED_MILLI.load(Ordering::Relaxed) as f64 / 1_000.0,
        ..Default::default()
    }
}

/// Starts an optional fixed-interval performance sampler.
///
/// `update_title_bar` only runs when SDL delivers an event. The sampler makes
/// benchmark output independent of event frequency and does nothing unless
/// `RUZU_PERF_LOG` is configured.
pub fn schedule_perf_log_if_requested(system: SystemRef) {
    let Some(path) = std::env::var_os("RUZU_PERF_LOG") else {
        return;
    };
    let interval_ms = std::env::var("RUZU_PERF_LOG_INTERVAL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1_000)
        .max(100);

    PERF_LOG_ACTIVE.store(true, Ordering::Relaxed);
    let spawn_result = std::thread::Builder::new()
        .name("PerfLog".to_string())
        .spawn(move || {
            use std::io::Write;

            let mut file = match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                Ok(file) => file,
                Err(error) => {
                    PERF_LOG_ACTIVE.store(false, Ordering::Relaxed);
                    log::error!("[PERF_LOG] cannot open {:?}: {error}", path);
                    return;
                }
            };
            let start = std::time::Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(interval_ms));
                if system.is_null() {
                    continue;
                }
                let results = system.get().get_and_reset_perf_stats();
                PERF_LOG_LAST_FPS_MILLI.store(
                    (results.average_game_fps * 1_000.0).max(0.0) as u64,
                    Ordering::Relaxed,
                );
                PERF_LOG_LAST_SPEED_MILLI.store(
                    (results.emulation_speed * 1_000.0).max(0.0) as u64,
                    Ordering::Relaxed,
                );
                let _ = writeln!(
                    file,
                    "{:.3} fps={:.2} system_fps={:.2} speed={:.2} frametime_ms={:.3}",
                    start.elapsed().as_secs_f64(),
                    results.average_game_fps,
                    results.system_fps,
                    results.emulation_speed * 100.0,
                    results.frametime * 1_000.0
                );
            }
        });
    if let Err(error) = spawn_result {
        PERF_LOG_ACTIVE.store(false, Ordering::Relaxed);
        log::error!("[PERF_LOG] cannot create sampler thread: {error}");
    }
}

/// A no-op graphics context used as a placeholder.
/// Maps to C++ `DummyContext` in `emu_window_sdl3.h`.
pub struct DummyContext;

/// SDL3-based emulator window base.
///
/// Maps to C++ class `EmuWindow_SDL3` in
/// `yuzu_cmd/emu_window/emu_window_sdl3.h`.
pub struct EmuWindowSdl3 {
    /// Host input drivers and their registered factories.
    /// Maps to C++ `input_subsystem`.
    pub input_subsystem: InputSubsystem,

    /// Whether the window is still open (close not yet requested).
    /// Maps to C++ `is_open`.
    pub is_open: bool,

    /// Whether the window is shown (not minimized).
    /// Maps to C++ `is_shown`.
    pub is_shown: bool,

    /// Shared visibility flag used by render backends running on the GPU thread.
    pub shown_state: Arc<AtomicBool>,

    /// Shared framebuffer layout used by render backends running on the GPU thread.
    pub framebuffer_layout: Arc<RwLock<FramebufferLayout>>,

    /// Tracks when the title bar was last updated (SDL ticks).
    /// Maps to C++ `last_time`.
    pub last_time: u64,

    /// Core instance used by the upstream title-bar performance update.
    pub system: SystemRef,

    /// Raw SDL3 window pointer.
    /// Maps to C++ `render_window`.
    pub render_window: *mut sdl::SDL_Window,
}

impl EmuWindowSdl3 {
    /// Creates a new SDL3 window, initializing SDL3 subsystems and the input
    /// subsystem.
    ///
    /// Maps to C++ `EmuWindow_SDL3::EmuWindow_SDL3`.
    ///
    /// # Safety
    /// Calls into SDL3 C API. The caller must ensure SDL3 is not already
    /// initialized in an incompatible way. Exits the process on failure,
    /// matching upstream behavior.
    pub fn new(system: SystemRef) -> Self {
        // Rust binaries do not use SDL's SDL_main wrapper. This must precede
        // `InputSubsystem::initialize`: SDLDriver may itself initialize the
        // joystick/gamepad subsystems before the frontend initializes video.
        unsafe { sdl::SDL_SetMainReady() };
        let mut input_subsystem = InputSubsystem::new();
        input_subsystem.initialize();

        // Maps to: SDL_Init(SDL_INIT_VIDEO | SDL_INIT_JOYSTICK | SDL_INIT_GAMEPAD)
        let ret = unsafe {
            sdl::SDL_Init(sdl::SDL_INIT_VIDEO | sdl::SDL_INIT_JOYSTICK | sdl::SDL_INIT_GAMEPAD)
        };
        if !ret {
            let err = unsafe { CStr::from_ptr(sdl::SDL_GetError()) }.to_string_lossy();
            log::error!("Failed to initialize SDL3: {}, Exiting...", err);
            std::process::exit(1);
        }
        EmuWindowSdl3 {
            input_subsystem,
            is_open: true,
            is_shown: true,
            shown_state: Arc::new(AtomicBool::new(true)),
            framebuffer_layout: Arc::new(RwLock::new(default_frame_layout(
                ScreenUndocked::WIDTH,
                ScreenUndocked::HEIGHT,
            ))),
            last_time: 0,
            system,
            render_window: std::ptr::null_mut(),
        }
    }

    /// Returns whether the window is still open (no close request yet).
    ///
    /// Maps to C++ `EmuWindow_SDL3::IsOpen`.
    pub fn is_open(&self) -> bool {
        self.is_open
    }

    /// Returns whether the window is shown (not minimized).
    ///
    /// Maps to C++ `EmuWindow_SDL3::IsShown`.
    pub fn is_shown(&self) -> bool {
        self.is_shown
    }

    pub fn shown_state(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shown_state)
    }

    pub fn framebuffer_layout(&self) -> Arc<RwLock<FramebufferLayout>> {
        Arc::clone(&self.framebuffer_layout)
    }

    /// Updates the current framebuffer layout.
    ///
    /// Maps to upstream `Core::Frontend::EmuWindow::UpdateCurrentFramebufferLayout`.
    pub(crate) fn update_current_framebuffer_layout(&mut self, width: u32, height: u32) {
        *self.framebuffer_layout.write().unwrap() =
            default_frame_layout(width.max(1), height.max(1));
    }

    /// Waits for and dispatches the next SDL event.
    /// Called on the main thread.
    ///
    /// Maps to C++ `EmuWindow_SDL3::WaitEvent`.
    pub fn wait_event(&mut self) {
        // Maps to: SDL_WaitEvent dispatch loop
        let mut event: sdl::SDL_Event = unsafe { std::mem::zeroed() };
        let ret = unsafe { sdl::SDL_WaitEvent(&mut event) };
        if !ret {
            let err_ptr = sdl::SDL_GetError();
            let err = unsafe { CStr::from_ptr(err_ptr) }.to_string_lossy();
            if err.is_empty() {
                // SDL spurious wakeup — see upstream comment about SDL issue #5780.
                return;
            }
            log::error!("SDL_WaitEvent failed: {}", err);
            std::process::exit(1);
        }

        self.dispatch_event(&event);
        if self.is_open {
            self.update_title_bar();
        }
    }

    /// Polls and dispatches all pending SDL events without blocking.
    /// Returns true if at least one event was processed.
    ///
    /// Used by the GL render loop which needs to run continuously.
    pub fn poll_events(&mut self) -> bool {
        let mut had_events = false;
        let mut event: sdl::SDL_Event = unsafe { std::mem::zeroed() };
        while unsafe { sdl::SDL_PollEvent(&mut event) } {
            self.dispatch_event(&event);
            had_events = true;
        }
        if self.is_open {
            self.update_title_bar();
        }
        had_events
    }

    /// Get the window drawable size in pixels.
    pub fn get_drawable_size(&self) -> (i32, i32) {
        let mut w: i32 = 0;
        let mut h: i32 = 0;
        unsafe { sdl::SDL_GetWindowSizeInPixels(self.render_window, &mut w, &mut h) };
        (w, h)
    }

    fn update_title_bar(&mut self) {
        // Update window title every ~2 seconds.
        let current_time = unsafe { sdl::SDL_GetTicks() };
        if current_time > self.last_time + 2000 {
            // Maps to upstream `system.GetAndResetPerfStats()`. The optional
            // sampler owns this destructive read while a benchmark is active.
            let results = if self.system.is_null() {
                PerfStatsResults::default()
            } else if PERF_LOG_ACTIVE.load(Ordering::Relaxed) {
                perf_log_last_results()
            } else {
                self.system.get().get_and_reset_perf_stats()
            };
            let title = format!(
                "ruzu | FPS: {:.0} ({:.0}%)\0",
                results.average_game_fps,
                results.emulation_speed * 100.0
            );
            unsafe {
                sdl::SDL_SetWindowTitle(self.render_window, title.as_ptr() as *const _);
            }
            self.last_time = current_time;
        }
    }

    fn dispatch_event(&mut self, event: &sdl::SDL_Event) {
        match event.event_type() {
            sdl::SDL_EVENT_WINDOW_RESIZED
            | sdl::SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED
            | sdl::SDL_EVENT_WINDOW_MAXIMIZED
            | sdl::SDL_EVENT_WINDOW_RESTORED => self.on_resize(),
            sdl::SDL_EVENT_WINDOW_MINIMIZED => {
                self.is_shown = false;
                self.shown_state.store(false, Ordering::Relaxed);
                self.on_resize();
            }
            sdl::SDL_EVENT_WINDOW_EXPOSED => {
                self.is_shown = true;
                self.shown_state.store(true, Ordering::Relaxed);
                self.on_resize();
            }
            sdl::SDL_EVENT_WINDOW_CLOSE_REQUESTED => {
                log::info!("SDL window close event received");
                self.is_open = false;
            }
            sdl::SDL_EVENT_KEY_DOWN | sdl::SDL_EVENT_KEY_UP => {
                let scancode = unsafe { event.key.scancode.value() };
                let state = if unsafe { event.key.down } { 1 } else { 0 };
                self.on_key_event(scancode, state);
            }
            sdl::SDL_EVENT_MOUSE_MOTION => {
                let which = unsafe { event.motion.which };
                if which != SDL_TOUCH_MOUSEID {
                    let x = unsafe { event.motion.x } as i32;
                    let y = unsafe { event.motion.y } as i32;
                    self.on_mouse_motion(x, y);
                }
            }
            sdl::SDL_EVENT_MOUSE_BUTTON_DOWN | sdl::SDL_EVENT_MOUSE_BUTTON_UP => {
                let which = unsafe { event.button.which };
                if which != SDL_TOUCH_MOUSEID {
                    let button = unsafe { event.button.button } as u32;
                    let state = if unsafe { event.button.down } { 1 } else { 0 };
                    let x = unsafe { event.button.x } as i32;
                    let y = unsafe { event.button.y } as i32;
                    self.on_mouse_button(button, state, x, y);
                }
            }
            sdl::SDL_EVENT_FINGER_DOWN => {
                let x = unsafe { event.tfinger.x };
                let y = unsafe { event.tfinger.y };
                let id = unsafe { event.tfinger.touchID.value() } as usize;
                self.on_finger_down(x, y, id);
            }
            sdl::SDL_EVENT_FINGER_MOTION => {
                let x = unsafe { event.tfinger.x };
                let y = unsafe { event.tfinger.y };
                let id = unsafe { event.tfinger.touchID.value() } as usize;
                self.on_finger_motion(x, y, id);
            }
            sdl::SDL_EVENT_FINGER_UP => {
                self.on_finger_up();
            }
            sdl::SDL_EVENT_QUIT => {
                log::info!("SDL quit event received");
                self.is_open = false;
            }
            _ => {}
        }
    }

    /// Loads and sets the window icon from the embedded yuzu.bmp data.
    ///
    /// Maps to C++ `EmuWindow_SDL3::SetWindowIcon`.
    /// Note: The embedded icon data (yuzu_icon / yuzu_icon_size from yuzu_icon.h)
    /// is not ported. This logs a warning and returns early, matching the upstream
    /// graceful-failure path.
    pub fn set_window_icon(&self) {
        // Upstream: SDL_RWFromConstMem((void*)yuzu_icon, yuzu_icon_size)
        // then SDL_LoadBMP_RW / SDL_SetWindowIcon / SDL_FreeSurface.
        // The embedded BMP data from yuzu_icon.h is not ported.
        log::warn!("set_window_icon: embedded icon data not ported, skipping.");
    }

    // -----------------------------------------------------------------------
    // Protected helpers — called from wait_event
    // -----------------------------------------------------------------------

    /// Called when a key is pressed or released.
    ///
    /// Maps to C++ `EmuWindow_SDL3::OnKeyEvent`.
    pub(crate) fn on_key_event(&mut self, key: i32, state: u8) {
        if let Some(keyboard) = self.input_subsystem.get_keyboard_mut() {
            if state != 0 {
                keyboard.press_key(key);
            } else {
                keyboard.release_key(key);
            }
        }
    }

    /// Converts an SDL mouse button constant to the `MouseButton` enum used by
    /// `InputCommon`.
    ///
    /// Maps to C++ `EmuWindow_SDL3::SDLButtonToMouseButton`.
    pub(crate) fn sdl_button_to_mouse_button(&self, button: u32) -> MouseButton {
        // SDL_BUTTON_LEFT=1, SDL_BUTTON_MIDDLE=2, SDL_BUTTON_RIGHT=3,
        // SDL_BUTTON_X1=4, SDL_BUTTON_X2=5
        match button {
            1 => MouseButton::Left,     // SDL_BUTTON_LEFT
            3 => MouseButton::Right,    // SDL_BUTTON_RIGHT
            2 => MouseButton::Wheel,    // SDL_BUTTON_MIDDLE
            4 => MouseButton::Backward, // SDL_BUTTON_X1
            5 => MouseButton::Forward,  // SDL_BUTTON_X2
            _ => MouseButton::Undefined,
        }
    }

    /// Translates a pixel-space position to a normalized touch position.
    ///
    /// Maps to C++ `EmuWindow_SDL3::MouseToTouchPos`.
    pub(crate) fn mouse_to_touch_pos(&self, touch_x: i32, touch_y: i32) -> (f32, f32) {
        // Maps to: int w, h; SDL_GetWindowSize(render_window, &w, &h);
        let mut w: i32 = 1;
        let mut h: i32 = 1;
        if !self.render_window.is_null() {
            unsafe { sdl::SDL_GetWindowSize(self.render_window, &mut w, &mut h) };
        }
        let w = w.max(1);
        let h = h.max(1);
        let fx = (touch_x as f32) / (w as f32);
        let fy = (touch_y as f32) / (h as f32);
        (fx.clamp(0.0, 1.0), fy.clamp(0.0, 1.0))
    }

    /// Called when a mouse button is pressed or released.
    ///
    /// Maps to C++ `EmuWindow_SDL3::OnMouseButton`.
    pub(crate) fn on_mouse_button(&mut self, button: u32, state: u8, x: i32, y: i32) {
        let mouse_button = self.sdl_button_to_mouse_button(button);
        let touch = self.mouse_to_touch_pos(x, y);
        if let Some(mouse) = self.input_subsystem.get_mouse_mut() {
            if state != 0 {
                mouse.press_button(x, y, mouse_button);
                mouse.press_mouse_button(mouse_button);
                mouse.press_touch_button(touch.0, touch.1, mouse_button);
            } else {
                mouse.release_button(mouse_button);
            }
            mouse.notify_changed();
        }
    }

    /// Called when the mouse cursor moves.
    ///
    /// Maps to C++ `EmuWindow_SDL3::OnMouseMotion`.
    pub(crate) fn on_mouse_motion(&mut self, x: i32, y: i32) {
        let touch = self.mouse_to_touch_pos(x, y);
        if let Some(mouse) = self.input_subsystem.get_mouse_mut() {
            mouse.move_cursor(x, y, 0, 0);
            mouse.mouse_move(touch.0, touch.1);
            mouse.touch_move(touch.0, touch.1);
            mouse.notify_changed();
        }
    }

    /// Called when a finger starts touching the touchscreen.
    ///
    /// Maps to C++ `EmuWindow_SDL3::OnFingerDown`.
    pub(crate) fn on_finger_down(&mut self, x: f32, y: f32, id: usize) {
        if let Some(touch_screen) = self.input_subsystem.get_touch_screen_mut() {
            touch_screen.touch_pressed(x, y, id);
        }
    }

    /// Called when a finger moves on the touchscreen.
    ///
    /// Maps to C++ `EmuWindow_SDL3::OnFingerMotion`.
    pub(crate) fn on_finger_motion(&mut self, x: f32, y: f32, id: usize) {
        if let Some(touch_screen) = self.input_subsystem.get_touch_screen_mut() {
            touch_screen.touch_moved(x, y, id);
        }
    }

    /// Called when a finger lifts from the touchscreen.
    ///
    /// Maps to C++ `EmuWindow_SDL3::OnFingerUp`.
    pub(crate) fn on_finger_up(&mut self) {
        if let Some(touch_screen) = self.input_subsystem.get_touch_screen_mut() {
            touch_screen.release_all_touch();
        }
    }

    /// Called when the window is resized or restored.
    ///
    /// Maps to C++ `EmuWindow_SDL3::OnResize`.
    pub(crate) fn on_resize(&mut self) {
        // Maps to: int width, height; SDL_GL_GetDrawableSize(render_window, &width, &height);
        // then UpdateCurrentFramebufferLayout(width, height).
        if self.render_window.is_null() {
            return;
        }
        let mut width: i32 = 0;
        let mut height: i32 = 0;
        unsafe { sdl::SDL_GetWindowSizeInPixels(self.render_window, &mut width, &mut height) };
        let width = width.max(1) as u32;
        let height = height.max(1) as u32;
        self.update_current_framebuffer_layout(width, height);
        log::trace!("on_resize: {}x{}", width, height);
    }

    /// Shows or hides the mouse cursor.
    ///
    /// Maps to C++ `EmuWindow_SDL3::ShowCursor`.
    pub(crate) fn show_cursor(&self, show: bool) {
        // Maps to: SDL_ShowCursor(show_cursor ? SDL_ENABLE : SDL_DISABLE)
        unsafe {
            if show {
                let _ = sdl::SDL_ShowCursor();
            } else {
                let _ = sdl::SDL_HideCursor();
            }
        }
    }

    /// Applies the current fullscreen mode setting.
    ///
    /// Maps to C++ `EmuWindow_SDL3::Fullscreen`.
    pub(crate) fn fullscreen(&self) {
        if self.render_window.is_null() {
            return;
        }
        let fullscreen_mode = *common::settings::values().fullscreen_mode.get_value();
        if fullscreen_mode == FullscreenMode::Exclusive {
            unsafe {
                let display = sdl::SDL_GetDisplayForWindow(self.render_window);
                let display_mode = sdl::SDL_GetDesktopDisplayMode(display);
                if !display_mode.is_null() {
                    sdl::SDL_SetWindowSize(
                        self.render_window,
                        (*display_mode).w,
                        (*display_mode).h,
                    );
                    sdl::SDL_SetWindowFullscreenMode(self.render_window, display_mode);
                } else {
                    let err = CStr::from_ptr(sdl::SDL_GetError()).to_string_lossy();
                    log::error!("SDL_GetDesktopDisplayMode failed: {}", err);
                }
                if sdl::SDL_SetWindowFullscreen(self.render_window, true) {
                    return;
                }
                let err = CStr::from_ptr(sdl::SDL_GetError()).to_string_lossy();
                log::error!("Fullscreening failed: {}", err);
                log::info!("Attempting to use borderless fullscreen...");
            }
        }

        unsafe {
            sdl::SDL_SetWindowFullscreenMode(self.render_window, std::ptr::null());
            if sdl::SDL_SetWindowFullscreen(self.render_window, true) {
                return;
            }
            let err = CStr::from_ptr(sdl::SDL_GetError()).to_string_lossy();
            log::error!("Borderless fullscreening failed: {}", err);
            log::info!("Falling back on a maximised window...");
            sdl::SDL_MaximizeWindow(self.render_window);
        }
    }

    /// Called when the minimum client area size changes.
    ///
    /// Maps to C++ `EmuWindow_SDL3::OnMinimalClientAreaChangeRequest`.
    pub(crate) fn on_minimal_client_area_change_request(&self, min_width: u32, min_height: u32) {
        // Maps to: SDL_SetWindowMinimumSize(render_window, minimal_size.first, minimal_size.second)
        if !self.render_window.is_null() {
            unsafe {
                sdl::SDL_SetWindowMinimumSize(
                    self.render_window,
                    min_width as i32,
                    min_height as i32,
                )
            };
        }
    }
}

impl Drop for EmuWindowSdl3 {
    /// Shuts down the input subsystem and SDL3.
    ///
    /// Maps to C++ `EmuWindow_SDL3::~EmuWindow_SDL3`.
    fn drop(&mut self) {
        if !self.system.is_null() {
            self.system.get().hid_core().lock().unload_input_devices();
        }
        self.input_subsystem.shutdown();
        unsafe { sdl::SDL_Quit() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hid_core::frontend::emulated_controller::{
        get_simple_npad_button_state, set_simple_npad_button,
    };
    use hid_core::hid_types::NpadButton;

    #[test]
    fn key_events_do_not_update_the_diagnostic_npad_bridge() {
        set_simple_npad_button(NpadButton::ALL, false);

        let mut window = std::mem::ManuallyDrop::new(EmuWindowSdl3 {
            input_subsystem: InputSubsystem::new(),
            is_open: true,
            is_shown: true,
            shown_state: Arc::new(AtomicBool::new(true)),
            framebuffer_layout: Arc::new(RwLock::new(default_frame_layout(
                ScreenUndocked::WIDTH,
                ScreenUndocked::HEIGHT,
            ))),
            last_time: 0,
            system: SystemRef::null(),
            render_window: std::ptr::null_mut(),
        });

        window.on_key_event(4, 1);
        assert_eq!(
            get_simple_npad_button_state().raw,
            NpadButton::NONE,
            "SDL keyboard events must only use the configured keyboard engine"
        );
    }
}

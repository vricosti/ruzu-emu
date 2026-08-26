// SPDX-FileCopyrightText: 2022 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Null-renderer SDL3 emulator window.
//!
//! Port of `yuzu_cmd/emu_window/emu_window_sdl3_null.h` and
//! `yuzu_cmd/emu_window/emu_window_sdl3_null.cpp`.
//!
//! `EmuWindowSdl3Null` creates a plain SDL3 window without any graphics API
//! context. It is used with the `RendererNull` backend, which renders nothing
//! and is useful for benchmarking CPU-only performance without GPU overhead.
//!
//! `CreateSharedContext` returns a `DummyContext`, mirroring the Vulkan
//! variant, since there is no real graphics context to share.

use sdl3::sys::everything as sdl;
use std::ffi::CStr;
use std::sync::{Arc, RwLock};

use super::emu_window_sdl3::{DummyContext, EmuWindowSdl3};
use ruzu_core::core::SystemRef;
use ruzu_core::frontend::framebuffer_layout::FramebufferLayout;

// Screen layout constants.
// Maps to C++ `Layout::ScreenUndocked::Width` / `Layout::ScreenUndocked::Height`.
const SCREEN_UNDOCKED_WIDTH: i32 = 1280;
const SCREEN_UNDOCKED_HEIGHT: i32 = 720;

/// Null-renderer SDL3 emulator window.
///
/// Maps to C++ class `EmuWindow_SDL3_Null` in
/// `yuzu_cmd/emu_window/emu_window_sdl3_null.h`.
pub struct EmuWindowSdl3Null {
    /// Shared base window state.
    base: EmuWindowSdl3,
}

impl EmuWindowSdl3Null {
    /// Creates the SDL3 window for use with the null renderer.
    ///
    /// Maps to C++ `EmuWindow_SDL3_Null::EmuWindow_SDL3_Null`.
    pub fn new(system: SystemRef, fullscreen: bool) -> Self {
        let mut base = EmuWindowSdl3::new(system);

        let window_title = b"ruzu-cmd (Null)\0";
        // No OpenGL/Vulkan flags — plain resizable window.
        let window_flags = sdl::SDL_WINDOW_RESIZABLE | sdl::SDL_WINDOW_HIGH_PIXEL_DENSITY;

        // Maps to: render_window = SDL_CreateWindow(...)
        let render_window = unsafe {
            sdl::SDL_CreateWindow(
                window_title.as_ptr() as *const _,
                SCREEN_UNDOCKED_WIDTH,
                SCREEN_UNDOCKED_HEIGHT,
                window_flags,
            )
        };

        if render_window.is_null() {
            let err = unsafe { CStr::from_ptr(sdl::SDL_GetError()) }.to_string_lossy();
            log::error!("Failed to create SDL3 window! {}", err);
            std::process::exit(1);
        }

        base.render_window = render_window;

        // Maps to: SetWindowIcon()
        base.set_window_icon();

        // Maps to: if (fullscreen) { Fullscreen(); ShowCursor(false); }
        if fullscreen {
            base.fullscreen();
            base.show_cursor(false);
        }

        // Maps to: OnResize(); OnMinimalClientAreaChangeRequest(...); SDL_PumpEvents()
        base.on_resize();
        base.on_minimal_client_area_change_request(256, 256);
        unsafe { sdl::SDL_PumpEvents() };

        log::info!("ruzu-cmd | Null window initialized");

        EmuWindowSdl3Null { base }
    }

    /// Returns a `DummyContext` — no real graphics context is needed.
    ///
    /// Maps to C++ `EmuWindow_SDL3_Null::CreateSharedContext`.
    pub fn create_shared_context(&self) -> DummyContext {
        DummyContext
    }

    /// Returns whether the window is still open.
    pub fn is_open(&self) -> bool {
        self.base.is_open()
    }

    /// Waits for and dispatches the next SDL event.
    pub fn wait_event(&mut self) {
        self.base.wait_event();
    }

    /// Returns the raw SDL window pointer.
    pub fn raw_window(&self) -> *mut sdl::SDL_Window {
        self.base.render_window
    }

    /// Returns the live framebuffer layout owned by the base emulation window.
    pub fn framebuffer_layout(&self) -> Arc<RwLock<FramebufferLayout>> {
        self.base.framebuffer_layout()
    }
}

impl Drop for EmuWindowSdl3Null {
    /// Default destructor — base `EmuWindowSdl3` handles SDL cleanup.
    ///
    /// Maps to C++ `EmuWindow_SDL3_Null::~EmuWindow_SDL3_Null` (`= default`).
    fn drop(&mut self) {}
}

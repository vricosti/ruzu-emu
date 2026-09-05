// SPDX-FileCopyrightText: 2014 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/frontend/emu_window.h and emu_window.cpp
//! Emulator window abstract interface.

use crate::frontend::framebuffer_layout::{self, minimum_size, FramebufferLayout};
use crate::frontend::graphics_context::GraphicsContext;

/// Information for the Graphics Backends signifying what type of screen pointer is in
/// WindowInformation.
///
/// Corresponds to upstream `Core::Frontend::WindowSystemType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowSystemType {
    Headless,
    Windows,
    X11,
    Wayland,
    Cocoa,
    Android,
}

impl Default for WindowSystemType {
    fn default() -> Self {
        Self::Headless
    }
}

/// Data structure to store emuwindow configuration.
///
/// Corresponds to upstream `EmuWindow::WindowConfig`.
#[derive(Debug, Clone)]
pub struct WindowConfig {
    pub fullscreen: bool,
    pub res_width: i32,
    pub res_height: i32,
    pub min_client_area_size: (u32, u32),
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            fullscreen: false,
            res_width: 0,
            res_height: 0,
            min_client_area_size: (minimum_size::WIDTH, minimum_size::HEIGHT),
        }
    }
}

/// Data describing host window system information.
///
/// Corresponds to upstream `EmuWindow::WindowSystemInfo`.
#[derive(Debug, Clone)]
pub struct WindowSystemInfo {
    /// Window system type. Determines which GL context or Vulkan WSI is used.
    pub type_: WindowSystemType,
    /// Connection to a display server (X11/Wayland). Opaque pointer.
    pub display_connection: usize,
    /// Render surface. Opaque pointer to native window handle.
    pub render_surface: usize,
    /// Scale of the render surface. For hidpi systems, >1.
    pub render_surface_scale: f32,
}

impl Default for WindowSystemInfo {
    fn default() -> Self {
        Self {
            type_: WindowSystemType::Headless,
            display_connection: 0,
            render_surface: 0,
            render_surface_scale: 1.0,
        }
    }
}

/// Abstraction class used to provide an interface between emulation code and the frontend.
///
/// Corresponds to upstream `Core::Frontend::EmuWindow`.
pub trait EmuWindow {
    /// Called from GPU thread when a frame is displayed.
    fn on_frame_displayed(&mut self) {}

    /// Returns a GraphicsContext that the frontend provides for rendering.
    fn create_shared_context(&self) -> Box<dyn GraphicsContext>;

    /// Returns if window is shown (not minimized).
    fn is_shown(&self) -> bool;
}

/// Base state for EmuWindow implementations.
///
/// Holds the common fields that upstream `EmuWindow` has as protected/private members.
pub struct EmuWindowBase {
    pub window_info: WindowSystemInfo,
    pub strict_context_required: bool,
    pub framebuffer_layout: FramebufferLayout,
    pub client_area_width: u32,
    pub client_area_height: u32,
    pub config: WindowConfig,
    pub active_config: WindowConfig,
}

impl EmuWindowBase {
    /// Create a new EmuWindowBase.
    ///
    /// Corresponds to upstream `EmuWindow::EmuWindow()`.
    pub fn new() -> Self {
        let config = WindowConfig::default();
        let active_config = config.clone();
        Self {
            window_info: WindowSystemInfo::default(),
            strict_context_required: false,
            framebuffer_layout: FramebufferLayout::default(),
            client_area_width: 0,
            client_area_height: 0,
            config,
            active_config,
        }
    }

    /// Returns currently active configuration.
    pub fn get_active_config(&self) -> &WindowConfig {
        &self.active_config
    }

    pub fn strict_context_required(&self) -> bool {
        self.strict_context_required
    }

    /// Requests the internal configuration to be replaced.
    pub fn set_config(&mut self, val: WindowConfig) {
        self.config = val;
    }

    /// Returns system information about the drawing area.
    pub fn get_window_info(&self) -> &WindowSystemInfo {
        &self.window_info
    }

    /// Gets the framebuffer layout (width, height, and screen regions).
    pub fn get_framebuffer_layout(&self) -> &FramebufferLayout {
        &self.framebuffer_layout
    }

    /// Convenience method to update the current frame layout.
    ///
    /// Corresponds to upstream `EmuWindow::UpdateCurrentFramebufferLayout`.
    pub fn update_current_framebuffer_layout(&mut self, width: u32, height: u32) {
        self.notify_framebuffer_layout_changed(framebuffer_layout::default_frame_layout(
            width, height,
        ));
    }

    /// Processes any pending configuration changes.
    ///
    /// Corresponds to upstream `EmuWindow::ProcessConfigurationChanges`.
    pub fn process_configuration_changes(&mut self) {
        if self.config.min_client_area_size != self.active_config.min_client_area_size {
            self.on_minimal_client_area_change_request(self.config.min_client_area_size);
            self.config.min_client_area_size = self.active_config.min_client_area_size;
        }
    }

    /// Update framebuffer layout with the given parameter.
    pub fn notify_framebuffer_layout_changed(&mut self, layout: FramebufferLayout) {
        self.framebuffer_layout = layout;
    }

    /// Update internal client area size.
    pub fn notify_client_area_size_changed(&mut self, size: (u32, u32)) {
        self.client_area_width = size.0;
        self.client_area_height = size.1;
    }

    /// Converts a screen position into the equivalent touchscreen position.
    ///
    /// Corresponds to upstream `EmuWindow::MapToTouchScreen`.
    pub fn map_to_touch_screen(&self, framebuffer_x: u32, framebuffer_y: u32) -> (f32, f32) {
        let (fb_x, fb_y) = self.clip_to_touch_screen(framebuffer_x, framebuffer_y);
        let screen = &self.framebuffer_layout.screen;
        let x = (fb_x - screen.left) as f32 / (screen.right - screen.left) as f32;
        let y = (fb_y - screen.top) as f32 / (screen.bottom - screen.top) as f32;
        (x, y)
    }

    /// Clip the provided coordinates to be inside the touchscreen area.
    ///
    /// Corresponds to upstream `EmuWindow::ClipToTouchScreen`.
    pub fn clip_to_touch_screen(&self, new_x: u32, new_y: u32) -> (u32, u32) {
        let screen = &self.framebuffer_layout.screen;
        let x = new_x.max(screen.left).min(screen.right.saturating_sub(1));
        let y = new_y.max(screen.top).min(screen.bottom.saturating_sub(1));
        (x, y)
    }

    /// Handler called when the minimal client area was requested to be changed.
    /// Default implementation ignores the request.
    fn on_minimal_client_area_change_request(&mut self, _size: (u32, u32)) {
        // By default, ignore this request and do nothing.
    }
}

impl Default for EmuWindowBase {
    fn default() -> Self {
        Self::new()
    }
}

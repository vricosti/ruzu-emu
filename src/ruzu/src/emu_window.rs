// SPDX-License-Identifier: GPL-3.0-or-later
//
// GTK-backed `EmuWindow` — the launcher counterpart of `ruzu_cmd`'s
// `EmuWindowSdl3Vk`, and conceptually of upstream yuzu's `GRenderWindow`
// (`bootmanager.cpp`). It exposes exactly the native window-system data the
// Vulkan (MoltenVK) renderer needs, sourced from the `CAMetalLayer` embedded in
// the GTK window by `render_window::attach_metal_layer`:
//
//   * `window_info()`        → `WindowSystemInfo { Cocoa, render_surface = CAMetalLayer }`
//   * `shown_state()`        → `Arc<AtomicBool>` (window visible)
//   * `framebuffer_layout()` → `Arc<RwLock<FramebufferLayout>>` (updated on resize)
//
// The Vulkan renderer runs presentation on the GPU thread and reads these
// shared handles directly, so once the emulation system is booted with this
// window's `window_info`, frames are presented straight into the GTK window's
// Metal layer — no per-frame work on the GTK main thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use ruzu_core::frontend::emu_window::WindowSystemInfo;
use ruzu_core::frontend::framebuffer_layout::{default_frame_layout, FramebufferLayout};

#[cfg(target_os = "macos")]
use crate::render_window::EmbeddedMetalLayer;
#[cfg(target_os = "macos")]
use ruzu_core::frontend::emu_window::WindowSystemType;

/// Native render-window state shared with the Vulkan renderer.
///
/// The `render_surface` pointer inside `window_info` is a `CAMetalLayer` owned
/// by the GTK window's content view; this struct must not outlive the window.
pub struct GtkEmuWindow {
    window_info: WindowSystemInfo,
    shown_state: Arc<AtomicBool>,
    framebuffer_layout: Arc<RwLock<FramebufferLayout>>,
}

impl GtkEmuWindow {
    /// Build from an embedded `CAMetalLayer` (macOS). Mirrors the tail of
    /// `EmuWindowSdl3Vk::new`, which fills `window_info` for `Cocoa` and seeds
    /// the framebuffer layout from the drawable size.
    #[cfg(target_os = "macos")]
    pub fn from_metal_layer(layer: EmbeddedMetalLayer) -> Self {
        let window_info = WindowSystemInfo {
            type_: WindowSystemType::Cocoa,
            display_connection: 0,
            render_surface: layer.metal_layer as usize,
            render_surface_scale: layer.scale,
        };
        Self::from_window_info(window_info, layer.drawable_size)
    }

    /// Build from a fully-resolved `WindowSystemInfo` and drawable size.
    pub fn from_window_info(window_info: WindowSystemInfo, drawable_size: (u32, u32)) -> Self {
        let layout = default_frame_layout(drawable_size.0, drawable_size.1);
        Self {
            window_info,
            shown_state: Arc::new(AtomicBool::new(true)),
            framebuffer_layout: Arc::new(RwLock::new(layout)),
        }
    }

    /// Native window-system info for Vulkan surface creation
    /// (`vulkan_surface::create_surface`).
    pub fn window_info(&self) -> &WindowSystemInfo {
        &self.window_info
    }

    /// Shared visibility flag the renderer polls to skip presentation while the
    /// window is hidden/minimized.
    pub fn shown_state(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shown_state)
    }

    /// Shared framebuffer layout the renderer reads each present.
    pub fn framebuffer_layout(&self) -> Arc<RwLock<FramebufferLayout>> {
        Arc::clone(&self.framebuffer_layout)
    }

    /// Update the visibility flag (GTK `map`/`unmap`).
    pub fn set_shown(&self, shown: bool) {
        self.shown_state.store(shown, Ordering::SeqCst);
    }

    /// Recompute the framebuffer layout after a resize. Mirrors
    /// `EmuWindowBase::update_current_framebuffer_layout`.
    pub fn update_framebuffer_layout(&mut self, width: u32, height: u32) {
        *self.framebuffer_layout.write().unwrap() = default_frame_layout(width, height);
    }
}

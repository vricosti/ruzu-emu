// SPDX-License-Identifier: GPL-3.0-or-later
//
// Windows render surface — the GTK counterpart of upstream's `RenderWidget`
// (`zuyu/src/yuzu/bootmanager.cpp`).
//
// Upstream gives the Vulkan widget `Qt::WA_NativeWindow`, then obtains its
// `HWND` through `QWindow::winId()` in `qt_common.cpp`. GTK4 widgets share the
// toplevel's native surface, so this frontend creates the equivalent Win32
// child window directly and keeps it above GTK's render page.

#![cfg(target_os = "windows")]

use std::ffi::c_void;

use gtk::prelude::*;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, SetWindowPos, ShowWindow, SWP_NOACTIVATE, SWP_NOZORDER,
    SW_HIDE, SW_SHOWNA, WS_CHILD, WS_CLIPSIBLINGS, WS_DISABLED, WS_EX_NOACTIVATE,
};

#[link(name = "gtk-4")]
extern "C" {
    /// GDK Win32 backend: return the `HWND` backing a `GdkWin32Surface`.
    fn gdk_win32_surface_get_handle(surface: *mut c_void) -> HWND;
}

const STATIC_WINDOW_CLASS: [u16; 7] = [
    b'S' as u16,
    b'T' as u16,
    b'A' as u16,
    b'T' as u16,
    b'I' as u16,
    b'C' as u16,
    0,
];

/// QCursor::setPos for this borderless child HWND. Its client and window
/// origins coincide; x/y are physical render pixels, matching GetWindowRect.
pub unsafe fn warp_render_pointer(window: HWND, x: i32, y: i32) -> bool {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowRect, SetCursorPos};
    if window.is_null() { return false; }
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    if unsafe { GetWindowRect(window, &mut rect) } == 0 { return false; }
    unsafe { SetCursorPos(rect.left + x, rect.top + y) != 0 }
}

pub unsafe fn render_pointer_position(window: HWND) -> Option<(f64, f64)> {
    use windows_sys::Win32::Foundation::{POINT, RECT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowRect, GetCursorPos};
    if window.is_null() { return None; }
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    let mut point: POINT = unsafe { std::mem::zeroed() };
    if unsafe { GetWindowRect(window, &mut rect) } == 0 || unsafe { GetCursorPos(&mut point) } == 0 { return None; }
    Some(((point.x - rect.left) as f64, (point.y - rect.top) as f64))
}

/// A native child `HWND` covering the render area.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddedWin32Window {
    /// Child `HWND`, passed as `WindowSystemInfo::render_surface`.
    pub window: HWND,
    /// Size in physical pixels.
    pub drawable_size: (u32, u32),
    /// GDK surface scale, passed as `render_surface_scale`.
    pub scale: f32,
}

fn scale_render_rect(gtk_render_rect: (f64, f64, f64, f64), scale: f64) -> (i32, i32, i32, i32) {
    let (x, y, width, height) = gtk_render_rect;
    (
        (x * scale).round() as i32,
        (y * scale).round() as i32,
        (width.max(1.0) * scale).round().max(1.0) as i32,
        (height.max(1.0) * scale).round().max(1.0) as i32,
    )
}

/// Create an initially hidden native child window inside `window`.
///
/// `WS_DISABLED` preserves the frontend's existing input ownership: GTK's
/// toplevel receives pointer and keyboard events and forwards them through the
/// same input-subsystem path as upstream `GRenderWindow`.
pub fn attach_render_window(
    window: &gtk::Window,
    gtk_render_rect: Option<(f64, f64, f64, f64)>,
) -> Option<EmbeddedWin32Window> {
    let surface = window.surface()?;
    let parent = unsafe { gdk_win32_surface_get_handle(surface.as_ptr() as *mut c_void) };
    if parent.is_null() {
        return None;
    }

    let scale = surface.scale_factor().max(1) as f64;
    let rect = gtk_render_rect.unwrap_or((
        0.0,
        0.0,
        window.width().max(1) as f64,
        window.height().max(1) as f64,
    ));
    let (x, y, width, height) = scale_render_rect(rect, scale);
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };

    let child = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE,
            STATIC_WINDOW_CLASS.as_ptr(),
            std::ptr::null(),
            WS_CHILD | WS_DISABLED | WS_CLIPSIBLINGS,
            x,
            y,
            width,
            height,
            parent,
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if child.is_null() {
        log::error!("CreateWindowExW failed for the embedded Win32 render surface");
        return None;
    }

    log::info!(
        "Embedded Win32 render window {child:p} in parent {parent:p} \
         ({width}x{height} @ {scale}x)"
    );

    Some(EmbeddedWin32Window {
        window: child,
        drawable_size: (width as u32, height as u32),
        scale: scale as f32,
    })
}

/// Show or hide the child render window.
pub fn set_render_window_hidden(window: HWND, hidden: bool) {
    if window.is_null() {
        return;
    }
    unsafe {
        ShowWindow(window, if hidden { SW_HIDE } else { SW_SHOWNA });
    }
}

/// Move and resize the child window, returning its new physical drawable size.
pub fn resize_render_window(
    window: &gtk::Window,
    child: HWND,
    gtk_render_rect: (f64, f64, f64, f64),
) -> Option<(u32, u32)> {
    if child.is_null() {
        return None;
    }
    let scale = window
        .surface()
        .map(|surface| surface.scale_factor())
        .unwrap_or(1)
        .max(1) as f64;
    let (x, y, width, height) = scale_render_rect(gtk_render_rect, scale);
    let resized = unsafe {
        SetWindowPos(
            child,
            std::ptr::null_mut(),
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
    };
    if resized == 0 {
        log::error!("SetWindowPos failed for the embedded Win32 render surface");
        return None;
    }
    Some((width as u32, height as u32))
}

/// Destroy the child render target after the emulation thread and Vulkan
/// surface have stopped using it.
pub fn destroy_render_window(window: HWND) {
    if window.is_null() {
        return;
    }
    unsafe {
        DestroyWindow(window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_rect_is_scaled_to_physical_pixels() {
        assert_eq!(
            scale_render_rect((12.0, 34.0, 640.0, 360.0), 1.5),
            (18, 51, 960, 540)
        );
        assert_eq!(scale_render_rect((0.0, 0.0, 0.0, 0.0), 2.0), (0, 0, 2, 2));
    }
}

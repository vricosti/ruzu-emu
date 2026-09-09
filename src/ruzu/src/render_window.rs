// SPDX-License-Identifier: GPL-3.0-or-later
//
// macOS render-surface bridge — conceptual counterpart of upstream
// `GRenderWindow` (`/Users/vricosti/Dev/emulators/zuyu/src/yuzu/bootmanager.cpp`).
//
// GTK4's `GtkMacosContentView` composites its own content (the GTK scene) in a
// way that draws over any plain `NSView` subview we add — so a Metal subview
// embedded in the content view is NOT visible above the GTK UI. The robust
// approach that sidesteps GTK's compositing entirely is a **borderless child
// `NSWindow`** whose content view hosts the `CAMetalLayer`: the WindowServer
// composites a child window above its parent, guaranteed. We attach it with
// `addChildWindow:ordered:NSWindowAbove` (so it tracks the parent's position)
// and show/hide it via `alphaValue` (loading vs. rendering).
//
// The `CAMetalLayer` is what the Vulkan/MoltenVK renderer creates its
// `VkSurfaceKHR` from (via `VK_EXT_metal_surface`), exactly as `ruzu_cmd`'s
// `emu_window_sdl3_vk.rs` does.

#![cfg(target_os = "macos")]

use gtk::prelude::*;
use std::ffi::c_void;

use cocoa::foundation::{NSPoint, NSRect, NSSize};
use core_foundation::base::TCFType;
use core_graphics::color::CGColor;
use objc::runtime::{Object, NO, YES};
use objc::{class, msg_send, sel, sel_impl};

// NSWindow style / backing constants (AppKit).
const NS_WINDOW_STYLE_MASK_BORDERLESS: u64 = 0;
const NS_BACKING_STORE_BUFFERED: u64 = 2;

/// QCursor::setPos for the child NSWindow. Input is logical render coordinates
/// measured from its top-left. Cocoa's global bottom-left origin is converted
/// to Quartz's primary-display top-left origin before moving the pointer.
pub unsafe fn warp_render_pointer(window: *mut c_void, x: f64, y: f64) -> bool {
    use core_graphics::{display::CGDisplay, geometry::CGPoint};
    if window.is_null() { return false; }
    let frame: NSRect = unsafe { msg_send![window as *mut Object, frame] };
    let primary_height = CGDisplay::main().bounds().size.height;
    CGDisplay::warp_mouse_cursor_position(CGPoint::new(
        frame.origin.x + x, primary_height - frame.origin.y - frame.size.height + y,
    )).is_ok()
}

pub unsafe fn render_pointer_position(window: *mut c_void) -> Option<(f64, f64)> {
    use core_graphics::{display::CGDisplay, event::CGEvent, event_source::{CGEventSource, CGEventSourceStateID}};
    if window.is_null() { return None; }
    let frame: NSRect = unsafe { msg_send![window as *mut Object, frame] };
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()?;
    let point = CGEvent::new(source).ok()?.location();
    Some((point.x - frame.origin.x,
        point.y - (CGDisplay::main().bounds().size.height - frame.origin.y - frame.size.height)))
}

extern "C" {
    /// GDK macOS backend: returns the `NSWindow*` backing a `GdkMacosSurface`.
    /// Available since GTK 4.8; the symbol lives in the linked `libgtk-4`.
    fn gdk_macos_surface_get_native_window(surface: *mut c_void) -> *mut c_void;
}

/// A `CAMetalLayer` embedded via a borderless child `NSWindow`, usable as a
/// Vulkan render surface. The pointers are owned by the child window, which is
/// retained by the parent's child-window list; this handle must not outlive the
/// parent window.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddedMetalLayer {
    /// `CAMetalLayer*` — pass as `WindowSystemInfo::render_surface`.
    pub metal_layer: *mut c_void,
    /// The borderless child `NSWindow*` hosting the layer. Shown/hidden via
    /// `set_render_view_hidden`.
    pub child_window: *mut c_void,
    /// Drawable size in physical pixels (points × backing scale).
    pub drawable_size: (u32, u32),
    /// Backing scale factor (Retina = 2.0), for `render_surface_scale`.
    pub scale: f32,
}

/// Resize/reposition the render child window and its layer to a new render area
/// (GTK window coordinates, points). Returns the new drawable size in pixels.
/// Called on the GTK main thread when the window is resized.
///
/// Updates the layer's `drawableSize` so MoltenVK reports the new
/// `currentExtent`; the renderer's present thread then recreates the swapchain
/// at the new native size (see the `device_wait_idle` in
/// `video_core::renderer_vulkan::swapchain::Swapchain::create`, which drains
/// in-flight presents before the old swapchain is destroyed — without it this
/// is a use-after-free crash on MoltenVK). The caller must also update the
/// shared framebuffer layout to the returned size so the frame is rendered at
/// the new resolution (upstream `OnFramebufferSizeChanged`).
pub fn resize_child_window(
    window: &gtk::Window,
    child_window: *mut c_void,
    metal_layer: *mut c_void,
    gtk_render_rect: (f64, f64, f64, f64),
) -> Option<(u32, u32)> {
    let surface = window.surface()?;
    let parent = unsafe { gdk_macos_surface_get_native_window(surface.as_ptr() as *mut c_void) }
        as *mut Object;
    if parent.is_null() || child_window.is_null() || metal_layer.is_null() {
        return None;
    }
    unsafe {
        let content_view: *mut Object = msg_send![parent, contentView];
        if content_view.is_null() {
            return None;
        }
        let scale: f64 = msg_send![parent, backingScaleFactor];
        let scale = if scale > 0.0 { scale } else { 1.0 };

        let (gx, gy, gw, gh) = gtk_render_rect;
        let render_bounds = NSRect {
            origin: NSPoint { x: gx, y: gy },
            size: NSSize {
                width: gw,
                height: gh,
            },
        };
        let local_bounds = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: render_bounds.size,
        };
        let rect_in_window: NSRect = msg_send![content_view, convertRect: render_bounds toView: std::ptr::null_mut::<Object>()];
        let screen_rect: NSRect = msg_send![parent, convertRectToScreen: rect_in_window];

        let cw = child_window as *mut Object;
        let _: () = msg_send![cw, setFrame: screen_rect display: YES];
        let ml = metal_layer as *mut Object;
        let _: () = msg_send![ml, setFrame: local_bounds];
        // Drive MoltenVK's surface extent to the new native size so the renderer
        // recreates the swapchain (rather than scaling a fixed image).
        let drawable = NSSize {
            width: gw * scale,
            height: gh * scale,
        };
        let _: () = msg_send![ml, setDrawableSize: drawable];

        Some((
            (gw * scale).round().max(1.0) as u32,
            (gh * scale).round().max(1.0) as u32,
        ))
    }
}

/// Show or hide the render child window. Called on the GTK main thread.
/// `alphaValue` keeps the child-window link (and parent tracking) intact,
/// unlike `orderOut`.
pub fn set_render_view_hidden(child_window: *mut c_void, hidden: bool) {
    if child_window.is_null() {
        return;
    }
    let window = child_window as *mut Object;
    let alpha: f64 = if hidden { 0.0 } else { 1.0 };
    unsafe {
        let _: () = msg_send![window, setAlphaValue: alpha];
    }
}

/// Create a Metal-backed borderless child window over `window`'s render area
/// and return the `CAMetalLayer`. Must be called after the window is realized
/// (its `GdkSurface` exists); returns `None` if the native handles are
/// unavailable. The child window starts hidden (`alphaValue = 0`).
///
/// `gtk_render_rect` is the render area in GTK window coordinates (top-left
/// origin, points): `(x, y, width, height)` — used to size the child window so
/// it covers only that area (e.g. the central stack, leaving the bottom status
/// bar visible). `None` covers the full content view.
pub fn attach_metal_layer(
    window: &gtk::Window,
    gtk_render_rect: Option<(f64, f64, f64, f64)>,
) -> Option<EmbeddedMetalLayer> {
    let surface = window.surface()?;
    let surface_ptr = surface.as_ptr() as *mut c_void;

    let parent = unsafe { gdk_macos_surface_get_native_window(surface_ptr) } as *mut Object;
    if parent.is_null() {
        return None;
    }

    unsafe {
        let content_view: *mut Object = msg_send![parent, contentView];
        if content_view.is_null() {
            return None;
        }
        let full_bounds: NSRect = msg_send![content_view, bounds];
        let scale: f64 = msg_send![parent, backingScaleFactor];
        let scale = if scale > 0.0 { scale } else { 1.0 };

        // Render area in content-view coordinates. `GtkMacosContentView` is a
        // *flipped* NSView (top-left origin, Y down — same as GTK), so GTK's
        // top-left rect maps straight through; `convertRect:toView:` below
        // handles the flip to window base coordinates. (Flipping Y manually here
        // double-flips and pushes the render down by the status-bar height.)
        let render_bounds = match gtk_render_rect {
            Some((gx, gy, gw, gh)) => NSRect {
                origin: NSPoint { x: gx, y: gy },
                size: NSSize {
                    width: gw,
                    height: gh,
                },
            },
            None => full_bounds,
        };
        let bounds = render_bounds;
        // The child window's content view uses window-local coordinates.
        let local_bounds = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: render_bounds.size,
        };

        // Render-area rect in screen coordinates (child-window frame).
        let rect_in_window: NSRect = msg_send![content_view, convertRect: render_bounds toView: std::ptr::null_mut::<Object>()];
        let screen_rect: NSRect = msg_send![parent, convertRectToScreen: rect_in_window];

        // CAMetalLayer.
        let metal_layer: *mut Object = msg_send![class!(CAMetalLayer), layer];
        if metal_layer.is_null() {
            return None;
        }
        let _: () = msg_send![metal_layer, setContentsScale: scale];
        let _: () = msg_send![metal_layer, setOpaque: YES];
        let _: () = msg_send![metal_layer, setFrame: local_bounds];
        // Black background shown before the first presented frame and behind any
        // letterboxing; the Vulkan swapchain overwrites the game area.
        let background = CGColor::rgb(0.0, 0.0, 0.0, 1.0);
        let bg_ref = background.as_concrete_TypeRef();
        let _: () = msg_send![metal_layer, setBackgroundColor: bg_ref];

        // Metal-backed content view for the child window.
        let render_view: *mut Object = msg_send![class!(NSView), alloc];
        let render_view: *mut Object = msg_send![render_view, initWithFrame: local_bounds];
        let _: () = msg_send![render_view, setLayer: metal_layer];
        let _: () = msg_send![render_view, setWantsLayer: YES];

        // Borderless child window hosting the render view.
        let child_window: *mut Object = msg_send![class!(NSWindow), alloc];
        let child_window: *mut Object = msg_send![
            child_window,
            initWithContentRect: screen_rect
            styleMask: NS_WINDOW_STYLE_MASK_BORDERLESS
            backing: NS_BACKING_STORE_BUFFERED
            defer: NO
        ];
        let _: () = msg_send![child_window, setContentView: render_view];
        let _: () = msg_send![child_window, setOpaque: YES];
        let _: () = msg_send![child_window, setHasShadow: NO];
        // Unlike upstream's in-widget render surface, this native child window
        // sits above the GTK event target. Let pointer events pass through to
        // the parent so `GMainWindow` can forward them through the same
        // Mouse::PressButton/PressTouchButton path as GRenderWindow.
        let _: () = msg_send![child_window, setIgnoresMouseEvents: YES];
        // Start hidden; revealed once presentation begins.
        let _: () = msg_send![child_window, setAlphaValue: 0.0f64];
        // Attach above the parent so it tracks the parent's movement and the
        // WindowServer composites it over the GTK content.
        let _: () = msg_send![parent, addChildWindow: child_window ordered: 1i64];

        let drawable_size = (
            (bounds.size.width * scale).round().max(1.0) as u32,
            (bounds.size.height * scale).round().max(1.0) as u32,
        );

        log::info!(
            "Embedded render surface: {}x{} points, scale {scale}, drawable {}x{}",
            bounds.size.width,
            bounds.size.height,
            drawable_size.0,
            drawable_size.1,
        );

        Some(EmbeddedMetalLayer {
            metal_layer: metal_layer as *mut c_void,
            child_window: child_window as *mut c_void,
            drawable_size,
            scale: scale as f32,
        })
    }
}

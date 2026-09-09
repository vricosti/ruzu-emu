// SPDX-License-Identifier: GPL-3.0-or-later
//
// Linux/X11 render surface — the counterpart of upstream's `RenderWidget`
// (`/home/vricosti/Dev/emulators/zuyu/src/yuzu/bootmanager.cpp`) and of the
// `render_window.rs` CAMetalLayer bridge used on macOS.
//
// Upstream embeds a *native child window* inside `GRenderWindow`'s layout:
//
// ```cpp
// class RenderWidget : public QWidget {
//     explicit RenderWidget(GRenderWindow* parent) : QWidget(parent) {
//         setAttribute(Qt::WA_NativeWindow);
//         setAttribute(Qt::WA_PaintOnScreen);
//         ...
// ```
//
// and then hands that child's native handle to the renderer — per platform, in
// `qt_common.cpp`'s `GetWindowSystemInfo`:
//
// ```cpp
// wsi.display_connection = pni->nativeResourceForWindow("display", window);
// wsi.render_surface     = reinterpret_cast<void*>(window->winId());   // X11
// ```
//
// Qt creates that native child for you. GTK4 has no equivalent — its widgets
// are all drawn into the toplevel's single surface — so the child `Window` is
// created directly with Xlib here, parented to the GTK toplevel's XID and moved
// to cover the render area. That is the same shape as the macOS path, which
// likewise creates its own child window/layer rather than reusing the
// toplevel's.
//
// Only X11 is handled. Under Wayland the equivalent needs a `wl_subsurface`,
// which GTK4 does not expose; running with `GDK_BACKEND=x11` (XWayland) works
// on a Wayland session in the meantime.

use std::ffi::{c_void, CString};
use std::os::raw::c_int;
use std::sync::Arc;

use gtk::prelude::*;

use x11::{glx, xlib};

/// QCursor::setPos(mapToGlobal(...)) for the native render child. Coordinates
/// are physical child pixels, not GTK logical pixels. Caller owns both handles.
pub unsafe fn warp_render_pointer(display: *mut xlib::Display, window: xlib::Window, x: i32, y: i32) -> bool {
    if display.is_null() || window == 0 { return false; }
    unsafe {
        xlib::XWarpPointer(display, 0, window, 0, 0, 0, 0, x, y);
        xlib::XFlush(display);
    }
    true
}

pub unsafe fn render_pointer_position(display: *mut xlib::Display, window: xlib::Window) -> Option<(f64, f64)> {
    if display.is_null() || window == 0 { return None; }
    let (mut root, mut child, mut root_x, mut root_y, mut x, mut y, mut mask) = (0, 0, 0, 0, 0, 0, 0);
    let valid = unsafe { xlib::XQueryPointer(display, window, &mut root, &mut child, &mut root_x, &mut root_y, &mut x, &mut y, &mut mask) };
    (valid != 0).then_some((x as f64, y as f64))
}

use ruzu_core::frontend::graphics_context::GraphicsContext;

type GlxCreateContextAttribsArb = unsafe extern "C" fn(
    *mut xlib::Display,
    glx::GLXFBConfig,
    glx::GLXContext,
    xlib::Bool,
    *const c_int,
) -> glx::GLXContext;

type GlxSwapIntervalExt = unsafe extern "C" fn(*mut xlib::Display, glx::GLXDrawable, c_int);

/// Initialize Xlib's cross-thread locking before GTK/GDK opens the display.
/// OpenGL shader workers make GLX contexts current from background threads,
/// matching upstream's `QOpenGLContext::supportsThreadedOpenGL()` contract.
pub fn initialize_xlib_threads() -> bool {
    unsafe { xlib::XInitThreads() != 0 }
}

/// Root GLX context retained for the lifetime of every shared renderer context.
/// This mirrors upstream `GRenderWindow::main_context`.
struct GlxShareGroup {
    display: usize,
    context: usize,
}

unsafe impl Send for GlxShareGroup {}
unsafe impl Sync for GlxShareGroup {}

impl Drop for GlxShareGroup {
    fn drop(&mut self) {
        if self.display == 0 || self.context == 0 {
            return;
        }
        unsafe {
            glx::glXDestroyContext(
                self.display as *mut xlib::Display,
                self.context as glx::GLXContext,
            );
        }
    }
}

/// Copyable frontend source for renderer and shader-worker GLX contexts.
/// The root share group is reference counted. Renderer contexts target the
/// X11 child window, while shader workers receive private offscreen pbuffers,
/// matching upstream `OpenGLSharedContext`'s `main_surface` distinction.
#[derive(Clone)]
pub struct GlxContextSource {
    display: usize,
    window: glx::GLXDrawable,
    fb_config: usize,
    share_group: Arc<GlxShareGroup>,
}

unsafe impl Send for GlxContextSource {}
unsafe impl Sync for GlxContextSource {}

impl GlxContextSource {
    fn new(
        display: *mut xlib::Display,
        window: glx::GLXDrawable,
        fb_config: glx::GLXFBConfig,
    ) -> Result<Self, String> {
        let context = create_glx_context(display, fb_config, std::ptr::null_mut())?;
        Ok(Self {
            display: display as usize,
            window,
            fb_config: fb_config as usize,
            share_group: Arc::new(GlxShareGroup {
                display: display as usize,
                context: context as usize,
            }),
        })
    }

    /// Create one renderer/worker context sharing objects with the root.
    pub fn create_context(&self) -> Result<GlxContext, String> {
        let context = create_glx_context(
            self.display as *mut xlib::Display,
            self.fb_config as glx::GLXFBConfig,
            self.share_group.context as glx::GLXContext,
        )?;
        Ok(GlxContext {
            source: self.clone(),
            context: context as usize,
            drawable: self.window,
            owns_pbuffer: false,
            swap_interval_initialized: false,
        })
    }

    /// Create an offscreen shared context for one shader worker.
    ///
    /// Upstream `OpenGLSharedContext(share_context)` constructs a private
    /// `QOffscreenSurface` when no presentation surface is supplied. Sharing
    /// the presentation drawable between GLX contexts can serialize unrelated
    /// worker and render operations in Mesa, including zero-timeout sync
    /// probes. A one-pixel pbuffer is the direct GLX counterpart.
    pub fn create_offscreen_context(&self) -> Result<GlxContext, String> {
        let display = self.display as *mut xlib::Display;
        let attributes = [glx::GLX_PBUFFER_WIDTH, 1, glx::GLX_PBUFFER_HEIGHT, 1, 0];
        let pbuffer = unsafe {
            glx::glXCreatePbuffer(
                display,
                self.fb_config as glx::GLXFBConfig,
                attributes.as_ptr(),
            )
        };
        if pbuffer == 0 {
            return Err("unable to create an offscreen GLX pbuffer".to_owned());
        }
        let context = match create_glx_context(
            display,
            self.fb_config as glx::GLXFBConfig,
            self.share_group.context as glx::GLXContext,
        ) {
            Ok(context) => context,
            Err(error) => {
                unsafe { glx::glXDestroyPbuffer(display, pbuffer) };
                return Err(error);
            }
        };
        Ok(GlxContext {
            source: self.clone(),
            context: context as usize,
            drawable: pbuffer,
            owns_pbuffer: true,
            // Offscreen contexts never swap, so no swap interval is installed.
            swap_interval_initialized: true,
        })
    }

    /// Resolve an OpenGL entry point through GLX.
    pub fn get_proc_address(name: &'static str) -> *const c_void {
        let Ok(name) = CString::new(name) else {
            return std::ptr::null();
        };
        unsafe {
            glx::glXGetProcAddressARB(name.as_ptr().cast()).map_or(std::ptr::null(), |function| {
                function as *const () as *const c_void
            })
        }
    }
}

/// GLX implementation of upstream `OpenGLSharedContext`.
pub struct GlxContext {
    source: GlxContextSource,
    context: usize,
    drawable: glx::GLXDrawable,
    owns_pbuffer: bool,
    swap_interval_initialized: bool,
}

unsafe impl Send for GlxContext {}

impl GraphicsContext for GlxContext {
    fn swap_buffers(&mut self) {
        if self.owns_pbuffer {
            return;
        }
        unsafe {
            glx::glXSwapBuffers(self.source.display as *mut xlib::Display, self.drawable);
        }
    }

    fn make_current(&mut self) {
        let context = self.context as glx::GLXContext;
        unsafe {
            if glx::glXGetCurrentContext() != context
                && glx::glXMakeContextCurrent(
                    self.source.display as *mut xlib::Display,
                    self.drawable,
                    self.drawable,
                    context,
                ) == 0
            {
                log::error!("glXMakeCurrent failed for the embedded render window");
                return;
            }
        }
        if !self.swap_interval_initialized {
            set_swap_interval(self.source.display as *mut xlib::Display, self.drawable);
            self.swap_interval_initialized = true;
        }
    }

    fn done_current(&mut self) {
        unsafe {
            if glx::glXGetCurrentContext() == self.context as glx::GLXContext {
                glx::glXMakeContextCurrent(
                    self.source.display as *mut xlib::Display,
                    0,
                    0,
                    std::ptr::null_mut(),
                );
            }
        }
    }
}

impl Drop for GlxContext {
    fn drop(&mut self) {
        self.done_current();
        if self.context != 0 {
            unsafe {
                glx::glXDestroyContext(
                    self.source.display as *mut xlib::Display,
                    self.context as glx::GLXContext,
                );
                if self.owns_pbuffer && self.drawable != 0 {
                    glx::glXDestroyPbuffer(
                        self.source.display as *mut xlib::Display,
                        self.drawable,
                    );
                }
            }
        }
    }
}

fn create_glx_context(
    display: *mut xlib::Display,
    fb_config: glx::GLXFBConfig,
    share_context: glx::GLXContext,
) -> Result<glx::GLXContext, String> {
    let create = unsafe {
        glx::glXGetProcAddressARB(c"glXCreateContextAttribsARB".as_ptr().cast()).map(|function| {
            std::mem::transmute::<unsafe extern "C" fn(), GlxCreateContextAttribsArb>(function)
        })
    }
    .ok_or_else(|| "GLX_ARB_create_context is unavailable".to_owned())?;
    let flags = if *common::settings::values().renderer_debug.get_value() {
        glx::arb::GLX_CONTEXT_DEBUG_BIT_ARB
    } else {
        0
    };
    let attributes = [
        glx::arb::GLX_CONTEXT_MAJOR_VERSION_ARB,
        4,
        glx::arb::GLX_CONTEXT_MINOR_VERSION_ARB,
        6,
        glx::arb::GLX_CONTEXT_PROFILE_MASK_ARB,
        glx::arb::GLX_CONTEXT_COMPATIBILITY_PROFILE_BIT_ARB,
        glx::arb::GLX_CONTEXT_FLAGS_ARB,
        flags,
        0,
    ];
    let context = unsafe {
        create(
            display,
            fb_config,
            share_context,
            xlib::True,
            attributes.as_ptr(),
        )
    };
    if context.is_null() {
        Err("unable to create an OpenGL 4.6 compatibility context".to_owned())
    } else {
        Ok(context)
    }
}

fn set_swap_interval(display: *mut xlib::Display, drawable: glx::GLXDrawable) {
    let Some(function) =
        (unsafe { glx::glXGetProcAddressARB(c"glXSwapIntervalEXT".as_ptr().cast()) })
    else {
        return;
    };
    let function =
        unsafe { std::mem::transmute::<unsafe extern "C" fn(), GlxSwapIntervalExt>(function) };
    let interval = if *common::settings::values().vsync_mode.get_value()
        == common::settings_enums::VSyncMode::Immediate
    {
        0
    } else {
        1
    };
    unsafe { function(display, drawable, interval) };
}

/// A native X11 child window covering the render area.
///
/// Mirrors `EmbeddedMetalLayer` on macOS so `main_window` can treat both the
/// same way.
pub struct EmbeddedX11Window {
    /// `Display*` — pass as `WindowSystemInfo::display_connection`.
    pub display: *mut c_void,
    /// The child `Window` XID — pass as `WindowSystemInfo::render_surface`.
    pub window: u64,
    /// Size in physical pixels.
    pub drawable_size: (u32, u32),
    /// Scale factor, for `render_surface_scale`.
    pub scale: f32,
    /// GLX context source for OpenGL renderer and shader workers.
    pub glx_context_source: Option<GlxContextSource>,
    /// Colormap paired with the GLX visual. Freed after the child window.
    pub colormap: usize,
}

/// Create the child render window inside `window`, covering `gtk_render_rect`
/// (GTK coordinates within the toplevel; `None` covers the whole window).
///
/// Returns `None` when the GTK window is not realized or is not an X11 surface.
pub fn attach_render_window(
    window: &gtk::Window,
    gtk_render_rect: Option<(f64, f64, f64, f64)>,
) -> Option<EmbeddedX11Window> {
    let surface = window.surface()?;

    // GTK must be on the X11 backend; under a native Wayland surface there is
    // no XID to parent to.
    let x11_surface = surface.downcast_ref::<gdk4_x11::X11Surface>()?;
    let parent_xid = x11_surface.xid();

    let display = surface.display();
    let x11_display = display.downcast_ref::<gdk4_x11::X11Display>()?;
    // SAFETY: the display is owned by GDK and outlives the child window we
    // create under it; we only use the pointer while `window` is alive.
    let xdisplay = unsafe { x11_display.xdisplay() };
    if xdisplay.is_null() || parent_xid == 0 {
        return None;
    }

    let scale = surface.scale_factor().max(1) as f32;
    let (x, y, width, height) =
        gtk_render_rect.unwrap_or((0.0, 0.0, window.width() as f64, window.height() as f64));
    // Zero-sized windows are invalid in X and would fail the swapchain later.
    let width = (width.max(1.0) * scale as f64) as u32;
    let height = (height.max(1.0) * scale as f64) as u32;

    let screen = unsafe { xlib::XDefaultScreen(xdisplay) };
    let attributes = [
        glx::GLX_X_RENDERABLE,
        xlib::True,
        glx::GLX_DRAWABLE_TYPE,
        glx::GLX_WINDOW_BIT | glx::GLX_PBUFFER_BIT,
        glx::GLX_RENDER_TYPE,
        glx::GLX_RGBA_BIT,
        glx::GLX_X_VISUAL_TYPE,
        glx::GLX_TRUE_COLOR,
        glx::GLX_RED_SIZE,
        8,
        glx::GLX_GREEN_SIZE,
        8,
        glx::GLX_BLUE_SIZE,
        8,
        glx::GLX_ALPHA_SIZE,
        8,
        glx::GLX_DOUBLEBUFFER,
        xlib::True,
        0,
    ];
    let mut config_count = 0;
    let configs =
        unsafe { glx::glXChooseFBConfig(xdisplay, screen, attributes.as_ptr(), &mut config_count) };
    if configs.is_null() || config_count == 0 {
        log::error!("Cannot create embedded OpenGL surface: no suitable GLX FBConfig");
        return None;
    }
    let fb_config = unsafe { *configs };
    let visual_info = unsafe { glx::glXGetVisualFromFBConfig(xdisplay, fb_config) };
    unsafe { xlib::XFree(configs.cast()) };
    if visual_info.is_null() {
        log::error!("Cannot create embedded OpenGL surface: GLX FBConfig has no X11 visual");
        return None;
    }

    let (child, colormap) = unsafe {
        let root = xlib::XRootWindow(xdisplay, screen);
        let colormap =
            xlib::XCreateColormap(xdisplay, root, (*visual_info).visual, xlib::AllocNone);
        let mut window_attributes: xlib::XSetWindowAttributes = std::mem::zeroed();
        window_attributes.background_pixel = xlib::XBlackPixel(xdisplay, screen);
        window_attributes.border_pixel = 0;
        window_attributes.colormap = colormap;
        let child = xlib::XCreateWindow(
            xdisplay,
            parent_xid,
            (x * scale as f64) as i32,
            (y * scale as f64) as i32,
            width,
            height,
            0,
            (*visual_info).depth,
            xlib::InputOutput as u32,
            (*visual_info).visual,
            xlib::CWBackPixel | xlib::CWBorderPixel | xlib::CWColormap,
            &mut window_attributes,
        );
        xlib::XFree(visual_info.cast());
        if child == 0 {
            xlib::XFreeColormap(xdisplay, colormap);
            return None;
        }
        // The renderer paints this window; X must not send it Expose events we
        // would have to service, and the child starts hidden so the loading
        // screen shows through until `set_render_window_hidden(.., false)`.
        xlib::XSelectInput(xdisplay, child, 0);
        xlib::XFlush(xdisplay);
        (child, colormap)
    };

    let glx_context_source = match GlxContextSource::new(xdisplay, child, fb_config) {
        Ok(source) => Some(source),
        Err(error) => {
            log::error!("Cannot initialize embedded OpenGL contexts: {error}");
            None
        }
    };

    log::info!(
        "Embedded X11 render window 0x{child:x} in parent 0x{parent_xid:x} \
         ({width}x{height} @ {scale}x)"
    );

    Some(EmbeddedX11Window {
        display: xdisplay as *mut c_void,
        window: child,
        drawable_size: (width, height),
        scale,
        glx_context_source,
        colormap: colormap as usize,
    })
}

/// Show or hide the child render window.
///
/// The macOS path fades its child `NSWindow` via `setAlphaValue`; X11 has no
/// per-window alpha here, so the window is mapped/unmapped instead — the same
/// observable effect.
pub fn set_render_window_hidden(display: *mut c_void, window: u64, hidden: bool) {
    if display.is_null() || window == 0 {
        return;
    }
    let xdisplay = display as *mut xlib::Display;
    unsafe {
        if hidden {
            xlib::XUnmapWindow(xdisplay, window);
        } else {
            xlib::XMapWindow(xdisplay, window);
        }
        xlib::XFlush(xdisplay);
    }
}

/// Move and resize the child window to cover `gtk_render_rect`, returning the
/// new drawable size in physical pixels so the caller can rebuild the frame
/// layout (upstream `OnFramebufferSizeChanged`).
pub fn resize_render_window(
    window: &gtk::Window,
    display: *mut c_void,
    child: u64,
    gtk_render_rect: (f64, f64, f64, f64),
) -> Option<(u32, u32)> {
    if display.is_null() || child == 0 {
        return None;
    }
    let scale = window
        .surface()
        .map(|s| s.scale_factor())
        .unwrap_or(1)
        .max(1) as f64;
    let (x, y, width, height) = gtk_render_rect;
    let width = (width.max(1.0) * scale) as u32;
    let height = (height.max(1.0) * scale) as u32;

    let xdisplay = display as *mut xlib::Display;
    unsafe {
        xlib::XMoveResizeWindow(
            xdisplay,
            child,
            (x * scale) as i32,
            (y * scale) as i32,
            width,
            height,
        );
        xlib::XFlush(xdisplay);
    }
    Some((width, height))
}

/// Destroy the child window. Called when emulation stops, so a subsequent boot
/// starts from a fresh surface rather than reusing one whose swapchain is gone.
pub fn destroy_render_window(display: *mut c_void, window: u64, colormap: usize) {
    if display.is_null() || window == 0 {
        return;
    }
    let xdisplay = display as *mut xlib::Display;
    unsafe {
        xlib::XDestroyWindow(xdisplay, window);
        if colormap != 0 {
            xlib::XFreeColormap(xdisplay, colormap as xlib::Colormap);
        }
        xlib::XFlush(xdisplay);
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// GTK counterpart of Eden's RenderWidget/GetWindowSystemInfo (bootmanager.cpp,
// qt_common.cpp). GTK widgets have no independent native surface. Own a Wayland
// subsurface for the renderer instead of letting Vulkan/EGL overwrite GTK's
// toplevel surface. The session must join its GPU/present workers before Drop.

use gdk4_wayland::prelude::*;
use gtk::prelude::*;
use khronos_egl as egl;
use ruzu_core::frontend::{
    emu_window::{WindowSystemInfo, WindowSystemType},
    graphics_context::GraphicsContext,
};
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    sync::{Arc, Mutex},
};
use wayland_client::{
    delegate_noop,
    protocol::{wl_region, wl_registry, wl_subcompositor, wl_subsurface, wl_surface},
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};

#[derive(Default)]
struct ProtocolState {
    subcompositor: Option<wl_subcompositor::WlSubcompositor>,
    registry: Option<wl_registry::WlRegistry>,
}
impl Drop for ProtocolState {
    fn drop(&mut self) {
        if let Some(subcompositor) = &self.subcompositor {
            subcompositor.destroy();
        }
        if let Some(registry) = &self.registry {
            // wl_registry has no protocol destructor, only a local proxy.
            if let Some(backend) = registry.backend().upgrade() {
                let _ = backend.destroy_object(&registry.id());
            }
        }
    }
}
impl Dispatch<wl_registry::WlRegistry, ()> for ProtocolState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            if interface == "wl_subcompositor" && state.subcompositor.is_none() {
                state.subcompositor = Some(registry.bind(name, 1, qh, ()));
            }
        }
    }
}
delegate_noop!(ProtocolState: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(ProtocolState: ignore wl_subsurface::WlSubsurface);
delegate_noop!(ProtocolState: ignore wl_surface::WlSurface);
delegate_noop!(ProtocolState: ignore wl_region::WlRegion);

/// Logical subsurface coordinates and physical render-buffer extent. Never send
/// zero/negative buffer scales or extents to the compositor/graphics driver.
fn geometry(rect: (f64, f64, f64, f64), scale: i32) -> Option<(i32, i32, u32, u32)> {
    let (x, y, width, height) = rect;
    if ![x, y, width, height].iter().all(|v| v.is_finite()) || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let scale = scale.max(1) as f64;
    Some((
        x.round() as i32,
        y.round() as i32,
        (width * scale).round().clamp(1.0, i32::MAX as f64) as u32,
        (height * scale).round().clamp(1.0, i32::MAX as f64) as u32,
    ))
}

fn native_geometry(host: &gtk::Window, rect: (f64, f64, f64, f64), scale: i32) -> Option<(i32, i32, u32, u32)> {
    // GtkNative's transform maps surface -> widget coordinates. The renderer
    // receives widget coordinates, so invert it (CSD/shadow margins included).
    let (tx, ty) = host.surface_transform();
    geometry((rect.0 - tx, rect.1 - ty, rect.2, rect.3), scale)
}

pub struct WaylandRenderWindow {
    parent: RefCell<wl_surface::WlSurface>,
    connection: Connection,
    queue: RefCell<EventQueue<ProtocolState>>,
    state: RefCell<ProtocolState>,
    surface: wl_surface::WlSurface,
    subsurface: RefCell<wl_subsurface::WlSubsurface>,
    hidden: Cell<bool>,
    pub drawable_size: (u32, u32),
    pub window_info: WindowSystemInfo,
    egl: Option<EglContextSource>,
    // Last field: keep the foreign display alive until protocol objects and
    // event queues are destroyed, including failed construction paths.
    host: RefCell<gtk::Window>,
}

impl WaylandRenderWindow {
    pub fn new(
        host: &gtk::Window,
        rect: (f64, f64, f64, f64),
        opengl: bool,
    ) -> Result<Self, String> {
        let native = host
            .surface()
            .and_then(|s| s.downcast::<gdk4_wayland::WaylandSurface>().ok())
            .ok_or("No native Wayland surface")?;
        let display = native
            .display()
            .downcast::<gdk4_wayland::WaylandDisplay>()
            .map_err(|_| "No native Wayland display")?;
        let parent = native.wl_surface().ok_or("Missing wl_surface")?;
        let connection = Connection::from_backend(
            parent
                .backend()
                .upgrade()
                .ok_or("Closed Wayland connection")?,
        );
        let mut queue = connection.new_event_queue();
        let qh = queue.handle();
        let registry = connection.display().get_registry(&qh, ());
        let mut state = ProtocolState {
            registry: Some(registry),
            subcompositor: None,
        };
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        let subcompositor = state
            .subcompositor
            .as_ref()
            .ok_or("Compositor does not support subsurfaces")?;
        let compositor = display.wl_compositor().ok_or("Missing wl_compositor")?;
        let scale = native.scale_factor().max(1);
        let (x, y, width, height) = native_geometry(host, rect, scale).ok_or("Invalid render extent")?;
        let surface = compositor.create_surface(&qh, ());
        let subsurface = subcompositor.get_subsurface(&surface, &parent, &qh, ());
        // Keyboard/pointer/touch continue to reach GTK's existing controllers.
        let region = compositor.create_region(&qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();
        surface.set_buffer_scale(scale);
        subsurface.set_position(x, y);
        subsurface.set_desync();
        subsurface.place_below(&parent);
        surface.commit();
        host.queue_draw();
        let window_info = WindowSystemInfo {
            type_: WindowSystemType::Wayland,
            display_connection: connection.backend().display_ptr() as usize,
            render_surface: surface.id().as_ptr() as usize,
            render_surface_scale: scale as f32,
        };
        let mut result = Self {
            host: RefCell::new(host.clone()),
            parent: RefCell::new(parent),
            connection,
            queue: RefCell::new(queue),
            state: RefCell::new(state),
            surface,
            subsurface: RefCell::new(subsurface),
            hidden: Cell::new(true),
            drawable_size: (width, height),
            window_info,
            egl: None,
        };
        if opengl {
            // Borrow GDK's initialized EGLDisplay; never eglTerminate a shared display.
            match display
                .egl_display()
                .ok_or_else(|| "GDK did not initialize EGL".to_owned())
                .and_then(|egl_display| {
                    EglContextSource::new(egl_display, &result.surface, width, height)
                }) {
                Ok(source) => result.egl = Some(source),
                Err(error) => log::warn!(
                    "Wayland OpenGL contexts unavailable (Vulkan remains available): {error}"
                ),
            }
        }
        result.connection.flush().map_err(|e| e.to_string())?;
        log::info!("Embedded native Wayland render surface ({width}x{height} @ {scale}x)");
        Ok(result)
    }

    pub fn opengl_source(&self) -> Option<EglContextSource> {
        self.egl.clone()
    }

    pub fn dispatch_pending(&self) {
        if let Err(error) = self
            .queue
            .borrow_mut()
            .dispatch_pending(&mut self.state.borrow_mut())
        {
            log::error!("Wayland render surface events: {error}");
        }
    }

    pub fn set_hidden(&self, hidden: bool) {
        if self.hidden.replace(hidden) == hidden {
            return;
        }
        // Do not attach NULL while the renderer is concurrently presenting.
        // Stacking below GTK hides the game without touching its buffer state.
        let sub = self.subsurface.borrow();
        if hidden {
            sub.place_below(&self.parent.borrow());
        } else {
            sub.place_above(&self.parent.borrow());
        }
        self.host.borrow().queue_draw();
        let _ = self.connection.flush();
    }

    pub fn resize(&self, rect: (f64, f64, f64, f64)) -> Option<(u32, u32)> {
        let host = self.host.borrow();
        let scale = host.scale_factor().max(1);
        let (x, y, width, height) = native_geometry(&host, rect, scale)?;
        self.subsurface.borrow().set_position(x, y);
        self.surface.set_buffer_scale(scale);
        // EGL resize and buffer swap access the same wl_egl_window; serialize.
        if let Some(egl) = &self.egl {
            egl.0
                .native
                .lock()
                .unwrap()
                .resize(width as i32, height as i32, 0, 0);
        }
        host.queue_draw();
        let _ = self.connection.flush();
        Some((width, height))
    }

    pub fn reparent(&self, host: &gtk::Window) -> bool {
        let Some(native) = host
            .surface()
            .and_then(|s| s.downcast::<gdk4_wayland::WaylandSurface>().ok())
        else {
            return false;
        };
        let Some(parent) = native.wl_surface() else {
            return false;
        };
        if native.display() != gtk::prelude::WidgetExt::display(&*self.host.borrow()) {
            return false;
        }
        // Qt recreates the subsurface role to reparent its native child. Doing
        // that to a presented surface reproducibly corrupts Mutter 46's actor
        // ownership and can terminate the desktop session, even with protocol
        // roundtrips between destruction and recreation. Keep the native parent
        // fixed until Stop; supporting live moves requires renderer-coordinated
        // surface replacement, not reusing this wl_surface with another parent.
        *self.parent.borrow() == parent
    }
}

impl Drop for WaylandRenderWindow {
    fn drop(&mut self) {
        // Called only after all renderer contexts and present workers have exited.
        self.egl.take();
        self.subsurface.get_mut().destroy();
        self.surface.destroy();
        self.host.get_mut().queue_draw();
        let _ = self.connection.flush();
    }
}

// Counterpart of qt_common/render/context.h OpenGLSharedContext: root context,
// shared presentation context and independent offscreen shader-worker contexts.
type EglApi = egl::DynamicInstance<egl::EGL1_5>;
struct EglShareGroup {
    api: EglApi,
    display: egl::Display,
    config: egl::Config,
    root: egl::Context,
    native: Mutex<wayland_egl::WlEglSurface>,
}
// EGL contexts may migrate threads when not current. Each child context has
// exactly one owner; the root is never made current. Native window is locked.
unsafe impl Send for EglShareGroup {}
unsafe impl Sync for EglShareGroup {}
impl Drop for EglShareGroup {
    fn drop(&mut self) {
        let _ = self.api.destroy_context(self.display, self.root);
    }
}
#[derive(Clone)]
pub struct EglContextSource(Arc<EglShareGroup>);
impl EglContextSource {
    fn new(
        display: egl::Display,
        surface: &wl_surface::WlSurface,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let api = unsafe { EglApi::load_required() }.map_err(|e| e.to_string())?;
        api.bind_api(egl::OPENGL_API).map_err(|e| e.to_string())?;
        let config = api
            .choose_first_config(
                display,
                &[
                    egl::SURFACE_TYPE,
                    egl::WINDOW_BIT | egl::PBUFFER_BIT,
                    egl::RENDERABLE_TYPE,
                    egl::OPENGL_BIT,
                    egl::RED_SIZE,
                    8,
                    egl::GREEN_SIZE,
                    8,
                    egl::BLUE_SIZE,
                    8,
                    egl::ALPHA_SIZE,
                    8,
                    egl::NONE,
                ],
            )
            .map_err(|e| e.to_string())?
            .ok_or("No EGL OpenGL config")?;
        let native = wayland_egl::WlEglSurface::new(surface.id(), width as i32, height as i32)
            .map_err(|e| e.to_string())?;
        let root = api
            .create_context(display, config, None, &context_attributes())
            .map_err(|e| e.to_string())?;
        Ok(Self(Arc::new(EglShareGroup {
            api,
            display,
            config,
            root,
            native: Mutex::new(native),
        })))
    }
    pub fn get_proc_address(&self, name: &'static str) -> *const c_void {
        self.0
            .api
            .get_proc_address(name)
            .map_or(std::ptr::null(), |p| p as *const c_void)
    }
    pub fn create_context(&self, offscreen: bool) -> Result<EglContext, String> {
        let group = &self.0;
        group
            .api
            .bind_api(egl::OPENGL_API)
            .map_err(|e| e.to_string())?;
        let context = group
            .api
            .create_context(
                group.display,
                group.config,
                Some(group.root),
                &context_attributes(),
            )
            .map_err(|e| e.to_string())?;
        let surface = if offscreen {
            group.api.create_pbuffer_surface(
                group.display,
                group.config,
                &[egl::WIDTH, 1, egl::HEIGHT, 1, egl::NONE],
            )
        } else {
            unsafe {
                group.api.create_window_surface(
                    group.display,
                    group.config,
                    group.native.lock().unwrap().ptr() as *mut _,
                    None,
                )
            }
        };
        let surface = match surface {
            Ok(surface) => surface,
            Err(error) => {
                let _ = group.api.destroy_context(group.display, context);
                return Err(error.to_string());
            }
        };
        Ok(EglContext {
            source: self.clone(),
            context,
            surface,
            offscreen,
            interval_set: false,
        })
    }
}
fn context_attributes() -> Vec<i32> {
    let mut attributes = vec![
        egl::CONTEXT_MAJOR_VERSION,
        4,
        egl::CONTEXT_MINOR_VERSION,
        6,
        egl::CONTEXT_OPENGL_PROFILE_MASK,
        egl::CONTEXT_OPENGL_COMPATIBILITY_PROFILE_BIT,
    ];
    if *common::settings::values().renderer_debug.get_value() {
        attributes.extend([egl::CONTEXT_OPENGL_DEBUG, egl::TRUE as i32]);
    }
    attributes.push(egl::NONE);
    attributes
}
pub struct EglContext {
    source: EglContextSource,
    context: egl::Context,
    surface: egl::Surface,
    offscreen: bool,
    interval_set: bool,
}
unsafe impl Send for EglContext {}
impl GraphicsContext for EglContext {
    fn make_current(&mut self) {
        let group = &self.source.0;
        if let Err(error) = group.api.make_current(
            group.display,
            Some(self.surface),
            Some(self.surface),
            Some(self.context),
        ) {
            log::error!("eglMakeCurrent failed: {error}");
            return;
        }
        if !self.interval_set {
            let interval = if self.offscreen
                || *common::settings::values().vsync_mode.get_value()
                    == common::settings_enums::VSyncMode::Immediate
            {
                0
            } else {
                1
            };
            self.interval_set = group.api.swap_interval(group.display, interval).is_ok();
        }
    }
    fn done_current(&mut self) {
        let group = &self.source.0;
        if group.api.get_current_context() == Some(self.context) {
            let _ = group.api.make_current(group.display, None, None, None);
        }
    }
    fn swap_buffers(&mut self) {
        if self.offscreen {
            return;
        }
        let group = &self.source.0;
        let _native = group.native.lock().unwrap();
        if let Err(error) = group.api.swap_buffers(group.display, self.surface) {
            log::error!("eglSwapBuffers failed: {error}");
        }
    }
}
impl Drop for EglContext {
    fn drop(&mut self) {
        self.done_current();
        let group = &self.source.0;
        let _ = group.api.destroy_surface(group.display, self.surface);
        let _ = group.api.destroy_context(group.display, self.context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subsurface_position_is_logical_and_buffer_extent_is_physical() {
        assert_eq!(
            geometry((12.0, 34.0, 640.0, 360.0), 2),
            Some((12, 34, 1280, 720))
        );
        assert_eq!(geometry((-2.0, 3.0, 1.0, 1.0), 0), Some((-2, 3, 1, 1)));
        assert!(geometry((0.0, 0.0, 0.0, 40.0), 1).is_none());
        assert!(geometry((0.0, f64::NAN, 40.0, 40.0), 1).is_none());
    }

    #[test]
    #[ignore = "requires native Wayland, Vulkan and OpenGL 4.6; run alone with GDK_BACKEND=wayland"]
    fn native_wayland_surface_lifecycle_and_graphics_contexts() {
        assert!(std::env::var_os("RUZU_WAYLAND_TEST_ISOLATED").is_some(),
            "Run only on an isolated nested compositor or VM; the host-switch sequence triggered a Mutter crash");
        gtk::init().unwrap();
        let context = gtk::glib::MainContext::default();
        let first = gtk::Window::builder()
            .title("Ruzu Wayland surface test")
            .default_width(640)
            .default_height(400)
            .build();
        let second = gtk::Window::builder()
            .title("Ruzu Wayland detached test")
            .default_width(640)
            .default_height(400)
            .build();
        first.present();
        second.present();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !first.is_mapped() || !second.is_mapped() || first.width() < 1 || second.width() < 1 {
            assert!(std::time::Instant::now() < deadline);
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        for _ in 0..3 {
            let render =
                WaylandRenderWindow::new(&first, (10.0, 20.0, 320.0, 240.0), true).unwrap();
            assert_eq!(render.window_info.type_, WindowSystemType::Wayland);
            assert_ne!(render.window_info.render_surface, 0);
            let entry = unsafe { ash::Entry::load() }.unwrap();
            let extensions = [
                ash::khr::surface::NAME.as_ptr(),
                ash::khr::wayland_surface::NAME.as_ptr(),
            ];
            unsafe {
                let instance = entry
                    .create_instance(
                        &ash::vk::InstanceCreateInfo::default()
                            .enabled_extension_names(&extensions),
                        None,
                    )
                    .unwrap();
                let info = video_core::vulkan_common::vulkan_surface::WindowSystemInfo {
                    window_type:
                        video_core::vulkan_common::vulkan_instance::WindowSystemType::Wayland,
                    display_connection: render.window_info.display_connection as _,
                    render_surface: render.window_info.render_surface as _,
                };
                let surface = video_core::vulkan_common::vulkan_surface::create_surface(
                    &entry, &instance, &info,
                )
                .unwrap();
                let surface_api = ash::khr::surface::Instance::new(&entry, &instance);
                assert!(instance
                    .enumerate_physical_devices()
                    .unwrap()
                    .iter()
                    .any(|device| !surface_api
                        .get_physical_device_surface_formats(*device, surface)
                        .unwrap()
                        .is_empty()));
                surface_api.destroy_surface(surface, None);
                instance.destroy_instance(None);
            }
            let source = render.opengl_source().expect("EGL source");
            {
                let mut gl = source.create_context(false).unwrap();
                gl.make_current();
                type ClearColor = unsafe extern "system" fn(f32, f32, f32, f32);
                type Clear = unsafe extern "system" fn(u32);
                unsafe {
                    let clear_color: ClearColor =
                        std::mem::transmute(source.get_proc_address("glClearColor"));
                    let clear: Clear = std::mem::transmute(source.get_proc_address("glClear"));
                    clear_color(0.1, 0.3, 0.7, 1.0);
                    clear(0x4000);
                }
                gl.swap_buffers();
                gl.done_current();
                let worker = source.clone();
                std::thread::spawn(move || {
                    let mut gl = worker.create_context(true).unwrap();
                    gl.make_current();
                    gl.done_current();
                })
                .join()
                .unwrap();
                render.set_hidden(false);
                render.resize((20.0, 30.0, 400.0, 300.0)).unwrap();
                assert!(!render.reparent(&second));
                assert!(render.reparent(&first));
                render.set_hidden(true);
                render
                    .queue
                    .borrow_mut()
                    .roundtrip(&mut render.state.borrow_mut())
                    .unwrap();
            }
            drop(source);
            drop(render);
            while context.pending() {
                context.iteration(false);
            }
        }
        first.destroy();
        second.destroy();
    }
}

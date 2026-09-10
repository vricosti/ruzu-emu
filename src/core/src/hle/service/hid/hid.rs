//! Port of zuyu/src/core/hle/service/hid/hid.h and hid.cpp
//!
//! Entry point for the HID service module.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use hid_core::resource_manager::{
    ResourceManager, DEFAULT_UPDATE_NS, MOTION_UPDATE_NS, MOUSE_KEYBOARD_UPDATE_NS, NPAD_UPDATE_NS,
};
use hid_core::resources::hid_firmware_settings::HidFirmwareSettings;
use hid_core::resources::shared_memory_holder::KSharedMemoryBacking;

use crate::core_timing;
use crate::hle::kernel::k_shared_memory::{KSharedMemory, MemoryPermission};
use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{
    HLERequestContext, SessionRequestHandler, SessionRequestHandlerFactory,
    SessionRequestHandlerPtr,
};
use crate::hle::service::os::event::Event;
use crate::hle::service::server_manager::ServerManager;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// `hid:tmp`, matching upstream `IHidTemporaryServer` in `hid.cpp`.
struct IHidTemporaryServer {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IHidTemporaryServer {
    fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(0, None, "GetConsoleSixAxisSensorCalibrationValues")]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IHidTemporaryServer {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        "hid:tmp"
    }
}

impl ServiceFramework for IHidTemporaryServer {
    fn get_service_name(&self) -> &str {
        "hid:tmp"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// `ahid:cd`, matching upstream `AHID_CD` in `hid.cpp`.
struct AhidCd {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl AhidCd {
    fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "AcquireDevice"),
                (1, None, "ReleaseDevice"),
                (2, None, "GetCtrlSession"),
                (3, None, "GetReadSession"),
                (4, None, "GetWriteSession"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for AhidCd {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        "ahid:cd"
    }
}

impl ServiceFramework for AhidCd {
    fn get_service_name(&self) -> &str {
        "ahid:cd"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// `ahid:hdr`, matching upstream `AHID_HDR` in `hid.cpp`.
struct AhidHdr {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl AhidHdr {
    fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "GetDeviceEntries"),
                (1, None, "GetDeviceList"),
                (2, None, "GetDeviceParameters"),
                (3, None, "AttachDevice"),
                (4, None, "DetachDevice"),
                (5, None, "SetDeviceFilter"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for AhidHdr {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        "ahid:hdr"
    }
}

impl ServiceFramework for AhidHdr {
    fn get_service_name(&self) -> &str {
        "ahid:hdr"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Implementation of `hid_core::KSharedMemoryBacking` that allocates real
/// `KSharedMemory` objects from the kernel.
///
/// Mirrors upstream `SharedMemoryHolder::Initialize`:
/// `KSharedMemory::Create + Initialize + Register`. Passed once into
/// `ResourceManager::set_shared_memory_backing` at HID service startup so
/// every `AppletResource::create_applet_resource` allocates a kernel-backed
/// page that the daemon writes to and the guest maps via
/// `IAppletResource::GetSharedMemoryHandle`.
struct HidKSharedMemoryBacking {
    system: crate::core::SystemRef,
}

impl HidKSharedMemoryBacking {
    fn new(system: crate::core::SystemRef) -> Self {
        Self { system }
    }
}

impl KSharedMemoryBacking for HidKSharedMemoryBacking {
    fn create(&self, size: usize) -> Option<(*mut u8, Arc<dyn Any + Send + Sync>)> {
        let system_ptr =
            self.system.get() as *const crate::core::System as *mut crate::core::System;
        // SAFETY: SystemRef holds a stable pointer for the program lifetime;
        // the &mut access below is serialized by virtue of being called from
        // hid::loop_process initialization (single-threaded path).
        let device_memory_ptr = unsafe { (*system_ptr).device_memory() as *const _ };
        let kernel = unsafe { (*system_ptr).kernel_mut()? };

        let mut shared_memory = KSharedMemory::new();
        if shared_memory
            .initialize(
                unsafe { &*device_memory_ptr },
                kernel.memory_manager_mut(),
                MemoryPermission::None,
                MemoryPermission::Read,
                size,
            )
            .is_error()
        {
            return None;
        }

        let host_ptr = shared_memory.get_pointer_mut(0);
        if host_ptr.is_null() {
            return None;
        }

        // Box the KSharedMemory inside an Arc<dyn Any> for hid_core's opaque
        // keepalive slot. The `core` side recovers it via downcast in
        // IAppletResource::GetSharedMemoryHandle.
        let keepalive: Arc<dyn Any + Send + Sync> = Arc::new(shared_memory);
        Some((host_ptr, keepalive))
    }
}

/// Named services registered by the HID module:
/// - "hid"      -> IHidServer
/// - "hid:dbg"  -> IHidDebugServer
/// - "hid:sys"  -> IHidSystemServer
/// - "hidbus"   -> Hidbus
/// - "irs"      -> IRS
/// - "irs:sys"  -> IRS_SYS
/// - "xcd:sys"  -> XCD_SYS
pub fn loop_process(system: crate::core::SystemRef) {
    let firmware_settings = Arc::new(HidFirmwareSettings::new());
    let hid_core = system.get().hid_core();
    hid_core.lock().reload_input_devices();

    let resource_manager = Arc::new(parking_lot::Mutex::new(ResourceManager::new(
        firmware_settings.clone(),
        hid_core,
    )));

    // Wire up the kernel-backed shared-memory factory before any IPC handler
    // can run. Mirrors upstream's behavior where `SharedMemoryHolder::Initialize`
    // takes a `Core::System&` directly; ruzu cannot do that across the
    // hid_core/core crate boundary so we inject the backing as a trait object.
    resource_manager
        .lock()
        .set_shared_memory_backing(Arc::new(HidKSharedMemoryBacking::new(system.clone())));

    resource_manager.lock().initialize();

    let core_timing = system.get().core_timing();
    let npad_update_event = core_timing::create_event(
        "HID::UpdatePadCallback".to_string(),
        Box::new({
            let resource_manager = resource_manager.clone();
            move |_time, ns_late| {
                resource_manager.lock().update_npad(ns_late);
                None
            }
        }),
    );
    let default_update_event = core_timing::create_event(
        "HID::UpdateDefaultCallback".to_string(),
        Box::new({
            let resource_manager = resource_manager.clone();
            move |_time, ns_late| {
                resource_manager.lock().update_controllers(ns_late);
                None
            }
        }),
    );
    let mouse_keyboard_update_event = core_timing::create_event(
        "HID::UpdateMouseKeyboardCallback".to_string(),
        Box::new({
            let resource_manager = resource_manager.clone();
            move |_time, ns_late| {
                resource_manager.lock().update_mouse_keyboard(ns_late);
                None
            }
        }),
    );
    let motion_update_event = core_timing::create_event(
        "HID::UpdateMotionCallback".to_string(),
        Box::new({
            let resource_manager = resource_manager.clone();
            move |_time, ns_late| {
                resource_manager.lock().update_motion(ns_late);
                None
            }
        }),
    );
    let touch_update_event = core_timing::create_event(
        "HID::TouchUpdateCallback".to_string(),
        Box::new({
            let resource_manager = resource_manager.clone();
            move |time, _ns_late| {
                resource_manager.lock().update_touch_screen(time);
                None
            }
        }),
    );

    {
        core_timing.schedule_looping_event(
            NPAD_UPDATE_NS,
            NPAD_UPDATE_NS,
            &npad_update_event,
            false,
        );
        core_timing.schedule_looping_event(
            DEFAULT_UPDATE_NS,
            DEFAULT_UPDATE_NS,
            &default_update_event,
            false,
        );
        core_timing.schedule_looping_event(
            MOUSE_KEYBOARD_UPDATE_NS,
            MOUSE_KEYBOARD_UPDATE_NS,
            &mouse_keyboard_update_event,
            false,
        );
        core_timing.schedule_looping_event(
            MOTION_UPDATE_NS,
            MOTION_UPDATE_NS,
            &motion_update_event,
            false,
        );
        core_timing.schedule_looping_event(
            Duration::from_nanos(
                hid_core::resources::touch_screen::touch_screen_resource::GESTURE_UPDATE_PERIOD_NS,
            ),
            Duration::from_nanos(
                hid_core::resources::touch_screen::touch_screen_resource::GESTURE_UPDATE_PERIOD_NS,
            ),
            &touch_update_event,
            false,
        );
    }

    if std::env::var_os("RUZU_HID_HOST_POLL_NPAD").is_some() {
        let resource_manager = resource_manager.clone();
        std::thread::Builder::new()
            .name("ruzu-hid-npad-poll".to_string())
            .spawn(move || loop {
                std::thread::sleep(NPAD_UPDATE_NS);
                resource_manager.lock().update_npad(Duration::ZERO);
            })
            .expect("spawn HID NPad polling thread");
    }

    let _hid_update_events = [
        npad_update_event,
        default_update_event,
        mouse_keyboard_update_event,
        motion_update_event,
        touch_update_event,
    ];

    let server_manager = ServerManager::new_shared(system);
    let npad_style_set_events = Arc::new(parking_lot::Mutex::new(
        BTreeMap::<(u64, u32), Arc<Event>>::new(),
    ));

    {
        let mut server_manager = server_manager.lock().unwrap();

        // "hid" -> IHidServer
        {
            let rm = resource_manager.clone();
            let fw = firmware_settings.clone();
            let style_set_events = Arc::clone(&npad_style_set_events);
            let system_ref = system;
            let factory: SessionRequestHandlerFactory =
                Box::new(move || -> SessionRequestHandlerPtr {
                    Arc::new(super::hid_server::IHidServer::new(
                        system_ref,
                        rm.clone(),
                        fw.clone(),
                        Arc::clone(&style_set_events),
                    ))
                });
            server_manager.register_named_service("hid", factory, 64);
        }

        // "hid:dbg" -> IHidDebugServer
        {
            let rm = resource_manager.clone();
            let fw = firmware_settings.clone();
            let factory: SessionRequestHandlerFactory =
                Box::new(move || -> SessionRequestHandlerPtr {
                    Arc::new(super::hid_debug_server::IHidDebugServer::new(
                        rm.clone(),
                        fw.clone(),
                    ))
                });
            server_manager.register_named_service("hid:dbg", factory, 64);
        }

        // "hid:sys" -> IHidSystemServer
        {
            let rm = resource_manager.clone();
            let fw = firmware_settings.clone();
            let factory: SessionRequestHandlerFactory =
                Box::new(move || -> SessionRequestHandlerPtr {
                    Arc::new(super::hid_system_server::IHidSystemServer::new(
                        rm.clone(),
                        fw.clone(),
                    ))
                });
            server_manager.register_named_service("hid:sys", factory, 64);
        }

        server_manager.register_named_service(
            "hid:tmp",
            Box::new(|| -> SessionRequestHandlerPtr { Arc::new(IHidTemporaryServer::new()) }),
            64,
        );

        // "hidbus" -> Hidbus
        {
            let service = Arc::new(super::hidbus::Hidbus::new(system.clone()));
            let factory: SessionRequestHandlerFactory =
                Box::new(move || -> SessionRequestHandlerPtr {
                    service.clone()
                });
            server_manager.register_named_service("hidbus", factory, 64);
        }

        // "irs" -> IRS
        {
            let system = system.clone();
            let factory: SessionRequestHandlerFactory =
                Box::new(move || -> SessionRequestHandlerPtr {
                    Arc::new(super::irs::Irs::new(system.clone()))
                });
            server_manager.register_named_service("irs", factory, 64);
        }

        // "irs:sys" -> IRS_SYS
        {
            let factory: SessionRequestHandlerFactory =
                Box::new(move || -> SessionRequestHandlerPtr {
                    Arc::new(super::irs::IrsSys::new())
                });
            server_manager.register_named_service("irs:sys", factory, 64);
        }

        server_manager.register_named_service(
            "ahid:cd",
            Box::new(|| -> SessionRequestHandlerPtr { Arc::new(AhidCd::new()) }),
            64,
        );
        server_manager.register_named_service(
            "ahid:hdr",
            Box::new(|| -> SessionRequestHandlerPtr { Arc::new(AhidHdr::new()) }),
            64,
        );

        // "xcd:sys" -> XCD_SYS
        {
            let factory: SessionRequestHandlerFactory =
                Box::new(move || -> SessionRequestHandlerPtr {
                    Arc::new(super::xcd::XcdSys::new())
                });
            server_manager.register_named_service("xcd:sys", factory, 64);
        }
    }

    ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a41_service_tables_match_upstream() {
        assert_eq!(IHidTemporaryServer::new().handlers().len(), 1);
        assert_eq!(AhidCd::new().handlers().len(), 5);
        assert_eq!(AhidHdr::new().handlers().len(), 6);
    }
}

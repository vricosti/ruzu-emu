// SPDX-FileCopyrightText: 2021 yuzu Emulator Project
// SPDX-FileCopyrightText: 2021 Skyline Team and Contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/nvdrv/nvdrv.h
//! Port of zuyu/src/core/hle/service/nvdrv/nvdrv.cpp

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use super::core::container::{Container, SessionId};
use super::devices::nvdevice::NvDevice;
use super::devices::nvdisp_disp0::NvDispDisp0;
use super::devices::nvhost_as_gpu::NvHostAsGpu;
use super::devices::nvhost_ctrl::NvHostCtrl;
use super::devices::nvhost_ctrl_gpu::NvHostCtrlGpu;
use super::devices::nvhost_gpu::NvHostGpu;
use super::devices::nvhost_nvdec::NvHostNvDec;
use super::devices::nvhost_nvjpg::NvHostNvJpg;
use super::devices::nvhost_vic::NvHostVic;
use super::devices::nvmap::NvMapDevice;
use super::nvdata::*;
use crate::core::SystemRef;
use crate::hle::kernel::k_readable_event::KReadableEvent;
use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::kernel_helpers::ServiceContext;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

fn should_trace_ioctl_summary() -> bool {
    std::env::var_os("RUZU_NVDRV_TRACE").is_some()
        || std::env::var_os("RUZU_TRACE_NVDRV_IOCTL").is_some()
}

/// EventInterface manages kernel event creation and destruction for nvdrv.
///
/// Port of EventInterface from nvdrv.h/nvdrv.cpp.
/// In the C++ code, this manages KEvent creation/destruction via ServiceContext.
/// The shared context is owned by Module. The handle map bridges the existing
/// Rust NvDevice readable-end API to ServiceContext's owning event handles.
pub struct EventInterface {
    system: SystemRef,
    service_context: Arc<Mutex<ServiceContext>>,
    event_handles: Mutex<HashMap<u64, u32>>,
}

impl EventInterface {
    pub fn new(system: SystemRef) -> Self {
        Self::with_context(system, Arc::new(Mutex::new(ServiceContext::new_with_system(
            system, "nvdrv".to_owned(),
        ))))
    }

    fn with_context(system: SystemRef, service_context: Arc<Mutex<ServiceContext>>) -> Self {
        Self { system, service_context, event_handles: Mutex::new(HashMap::new()) }
    }

    pub fn system(&self) -> SystemRef {
        self.system
    }

    /// Creates a persistent readable event owned by nvdrv.
    pub fn create_event(&self, name: &str) -> Option<Arc<Mutex<KReadableEvent>>> {
        let mut context = self.service_context.lock().unwrap();
        let handle = context.create_event(name.to_owned());
        let readable_event = context.get_event(handle)?
            .readable_event().expect("NVDRV requires a kernel-backed event");
        let object_id = readable_event.lock().unwrap().object_id;
        self.event_handles.lock().unwrap().insert(object_id, handle);
        Some(readable_event)
    }

    /// Frees a previously created event.
    /// In the C++ code: module.service_context.CloseEvent(event)
    pub fn free_event(&self, event: Arc<Mutex<KReadableEvent>>) {
        let object_id = event.lock().unwrap().object_id;
        let handle = self.event_handles.lock().unwrap().remove(&object_id);
        if let Some(handle) = handle {
            self.service_context.lock().unwrap().close_event(handle);
        }
    }
}

/// The main nvdrv module, managing device file descriptors and dispatching ioctls.
pub struct Module {
    system: crate::core::SystemRef,
    container: Container,
    next_fd: Mutex<DeviceFD>,
    open_files: Mutex<HashMap<DeviceFD, Arc<dyn NvDevice + Send + Sync>>>,
    gpu_files: Mutex<HashMap<DeviceFD, Arc<NvHostGpu>>>,
    disp_files: Mutex<HashMap<DeviceFD, Arc<NvDispDisp0>>>,
    nvmap_files: Mutex<HashMap<DeviceFD, Arc<NvMapDevice>>>,
    service_context: Arc<Mutex<ServiceContext>>,
    events_interface: Arc<EventInterface>,
}

impl Module {
    pub fn new(system: crate::core::SystemRef) -> Arc<Self> {
        let service_context = Arc::new(Mutex::new(ServiceContext::new_with_system(
            system, "nvdrv".to_owned(),
        )));
        Arc::new(Self {
            system,
            container: Container::new_with_system(system),
            next_fd: Mutex::new(1),
            open_files: Mutex::new(HashMap::new()),
            gpu_files: Mutex::new(HashMap::new()),
            disp_files: Mutex::new(HashMap::new()),
            nvmap_files: Mutex::new(HashMap::new()),
            events_interface: Arc::new(EventInterface::with_context(system, Arc::clone(&service_context))),
            service_context,
        })
    }

    pub fn verify_fd(&self, fd: DeviceFD) -> NvResult {
        if fd < 0 {
            log::error!("Invalid DeviceFD={}!", fd);
            return NvResult::InvalidState;
        }
        let files = self.open_files.lock().unwrap();
        if !files.contains_key(&fd) {
            log::error!("Could not find DeviceFD={}!", fd);
            return NvResult::NotImplemented;
        }
        NvResult::Success
    }

    pub fn open(&self, device_name: &str, session_id: SessionId) -> DeviceFD {
        // Upstream rejects unsupported names before consuming a descriptor;
        // unrecognized nvhost nodes use its experimental NX2 device implementation.
        if !device_name.starts_with("/dev/nvhost-")
            && device_name != "/dev/nvmap"
            && device_name != "/dev/nvdisp_disp0"
        {
            log::error!("Trying to open unknown device {}", device_name);
            return INVALID_NVDRV_FD;
        }
        let mut next_fd = self.next_fd.lock().unwrap();
        let fd = *next_fd;
        *next_fd += 1;
        drop(next_fd);

        let device: Arc<dyn NvDevice + Send + Sync> = match device_name {
            "/dev/nvhost-as-gpu" => Arc::new(NvHostAsGpu::new(self.system, self, &self.container)),
            "/dev/nvhost-gpu" => {
                let gpu = Arc::new(NvHostGpu::new(
                    self.system,
                    Arc::clone(&self.events_interface),
                    &self.container,
                ));
                self.gpu_files.lock().unwrap().insert(fd, Arc::clone(&gpu));
                gpu
            }
            "/dev/nvhost-ctrl-gpu" => Arc::new(NvHostCtrlGpu::new(
                self.system,
                Arc::clone(&self.events_interface),
            )),
            "/dev/nvmap" => {
                let nvmap = Arc::new(NvMapDevice::new(
                    self.container.get_nv_map_file(),
                    &self.container,
                ));
                self.nvmap_files
                    .lock()
                    .unwrap()
                    .insert(fd, Arc::clone(&nvmap));
                nvmap
            }
            "/dev/nvdisp_disp0" => {
                let disp = Arc::new(NvDispDisp0::new(
                    self.system,
                    self.container.get_nv_map_file(),
                ));
                self.disp_files
                    .lock()
                    .unwrap()
                    .insert(fd, Arc::clone(&disp));
                disp
            }
            "/dev/nvhost-ctrl" => Arc::new(NvHostCtrl::new(
                Arc::clone(&self.events_interface),
                self.container.get_syncpoint_manager(),
            )),
            "/dev/nvhost-nvdec" => Arc::new(NvHostNvDec::new(self.system, &self.container)),
            "/dev/nvhost-nvjpg" => Arc::new(NvHostNvJpg::new()),
            "/dev/nvhost-vic" => Arc::new(NvHostVic::new(self.system, &self.container)),
            name if name.starts_with("/dev/nvhost-") => {
                log::warn!("Opening unknown device {} as a Switch 2 stub", name);
                Arc::new(super::devices::nx2_stub_device::Nx2StubDevice::new(name.to_owned()))
            }
            _ => {
                log::error!("Trying to open unknown device {}", device_name);
                return INVALID_NVDRV_FD;
            }
        };

        device.on_open(session_id, fd);

        let mut files = self.open_files.lock().unwrap();
        files.insert(fd, device);

        log::info!(
            "[NVDRV_OPEN] fd={} device={} session_id={}",
            fd,
            device_name,
            session_id.id
        );

        fd
    }

    pub fn ioctl1(
        &self,
        fd: DeviceFD,
        command: Ioctl,
        input: &[u8],
        output: &mut [u8],
    ) -> NvResult {
        if fd < 0 {
            let tid = crate::hle::kernel::kernel::get_current_thread_id_fast().unwrap_or(0);
            log::error!(
                "Invalid DeviceFD={}! ioctl1=0x{:08X} tid={}",
                fd,
                command.raw,
                tid
            );
            return NvResult::InvalidState;
        }
        if std::env::var_os("RUZU_TRACE_IOCTL_ENTER").is_some() {
            let n = input.len().min(64);
            let mut hex = String::new();
            for (i, b) in input.iter().enumerate().take(n) {
                if i > 0 && (i & 3) == 0 {
                    hex.push(' ');
                }
                use std::fmt::Write;
                let _ = write!(hex, "{:02x}", b);
            }
            eprintln!(
                "[IOCTL_ENTER] fd={} ioctl=0x{:08X} in_sz=0x{:x} out_sz=0x{:x} head={}",
                fd,
                command.raw,
                input.len(),
                output.len(),
                hex
            );
        }
        if should_trace_ioctl_summary() {
            use std::collections::HashMap;
            use std::sync::{Mutex, OnceLock};
            // Per (fd, cmd, group) counter. First occurrence always logs;
            // subsequent ones log at power-of-two increments per combo.
            static COMBO_COUNTS: OnceLock<Mutex<HashMap<(i32, u32, u8), u64>>> = OnceLock::new();
            let counts = COMBO_COUNTS.get_or_init(|| Mutex::new(HashMap::new()));
            let key = (fd, command.cmd(), command.group());
            let mut map = counts.lock().unwrap();
            let n = map.entry(key).or_insert(0);
            let current = *n;
            *n += 1;
            drop(map);
            if current == 0 || current.is_power_of_two() {
                log::info!(
                    "[NVDRV_IOCTL1] fd={} cmd=0x{:X} group=0x{:X} n={}",
                    fd,
                    command.cmd(),
                    command.group(),
                    current
                );
            }
        }
        let files = self.open_files.lock().unwrap();
        let result = if let Some(device) = files.get(&fd) {
            let device = Arc::clone(device);
            drop(files);
            device.ioctl1(fd, command, input, output)
        } else {
            log::error!("Could not find DeviceFD={}!", fd);
            NvResult::NotImplemented
        };
        if std::env::var_os("RUZU_TRACE_IOCTL_FP").is_some() {
            // Show up to 64 bytes so we see full MapBufferEx (40B) / SubmitGPFIFO fence tails.
            let n = output.len().min(64);
            let mut hex = String::new();
            for (i, b) in output.iter().enumerate().take(n) {
                if i > 0 && (i & 3) == 0 {
                    hex.push(' ');
                }
                use std::fmt::Write;
                let _ = write!(hex, "{:02x}", b);
            }
            eprintln!(
                "[IOCTL_FP] fd={} ioctl=0x{:08X} out_sz=0x{:x} nv_result=0x{:x} head={}",
                fd,
                command.raw,
                output.len(),
                result as u32,
                hex
            );
        }
        result
    }

    pub fn ioctl2(
        &self,
        fd: DeviceFD,
        command: Ioctl,
        input: &[u8],
        inline_input: &[u8],
        output: &mut [u8],
    ) -> NvResult {
        if fd < 0 {
            let tid = crate::hle::kernel::kernel::get_current_thread_id_fast().unwrap_or(0);
            log::error!(
                "Invalid DeviceFD={}! ioctl2=0x{:08X} tid={}",
                fd,
                command.raw,
                tid
            );
            return NvResult::InvalidState;
        }
        let files = self.open_files.lock().unwrap();
        if let Some(device) = files.get(&fd) {
            let device = Arc::clone(device);
            drop(files);
            device.ioctl2(fd, command, input, inline_input, output)
        } else {
            log::error!("Could not find DeviceFD={}!", fd);
            NvResult::NotImplemented
        }
    }

    pub fn ioctl3(
        &self,
        fd: DeviceFD,
        command: Ioctl,
        input: &[u8],
        output: &mut [u8],
        inline_output: &mut [u8],
    ) -> NvResult {
        if fd < 0 {
            let tid = crate::hle::kernel::kernel::get_current_thread_id_fast().unwrap_or(0);
            log::error!(
                "Invalid DeviceFD={}! ioctl3=0x{:08X} tid={}",
                fd,
                command.raw,
                tid
            );
            return NvResult::InvalidState;
        }
        let files = self.open_files.lock().unwrap();
        if let Some(device) = files.get(&fd) {
            let device = Arc::clone(device);
            drop(files);
            device.ioctl3(fd, command, input, output, inline_output)
        } else {
            log::error!("Could not find DeviceFD={}!", fd);
            NvResult::NotImplemented
        }
    }

    pub fn close(&self, fd: DeviceFD) -> NvResult {
        if fd < 0 {
            log::error!("Invalid DeviceFD={}!", fd);
            return NvResult::InvalidState;
        }
        let mut files = self.open_files.lock().unwrap();
        if let Some(device) = files.remove(&fd) {
            self.gpu_files.lock().unwrap().remove(&fd);
            self.disp_files.lock().unwrap().remove(&fd);
            self.nvmap_files.lock().unwrap().remove(&fd);
            device.on_close(fd);
            NvResult::Success
        } else {
            log::error!("Could not find DeviceFD={}!", fd);
            NvResult::NotImplemented
        }
    }

    /// Queries an event for a given device fd and event_id.
    ///
    /// Port of Module::QueryEvent from nvdrv.cpp.
    pub fn query_event(
        &self,
        fd: DeviceFD,
        event_id: u32,
    ) -> (NvResult, Option<Arc<Mutex<KReadableEvent>>>) {
        if fd < 0 {
            log::error!("Invalid DeviceFD={}!", fd);
            return (NvResult::InvalidState, None);
        }

        let files = self.open_files.lock().unwrap();
        let device = match files.get(&fd) {
            Some(d) => Arc::clone(d),
            None => {
                log::error!("Could not find DeviceFD={}!", fd);
                return (NvResult::NotImplemented, None);
            }
        };
        drop(files);

        let result = device.query_event(event_id);
        let object_id = result.as_ref().map(|e| e.lock().unwrap().object_id);
        log::info!(
            "[NVDRV_QUERY_EVENT] fd={} event_id={} object_id={:?}",
            fd,
            event_id,
            object_id
        );
        match result {
            Some(event) => (NvResult::Success, Some(event)),
            None => (NvResult::BadParameter, None),
        }
    }

    pub fn get_container(&self) -> &Container {
        &self.container
    }

    pub fn get_events_interface(&self) -> &Arc<EventInterface> {
        &self.events_interface
    }

    pub fn get_gpu_device(&self, fd: DeviceFD) -> Option<Arc<NvHostGpu>> {
        self.gpu_files.lock().unwrap().get(&fd).cloned()
    }

    pub fn get_disp_device(&self, fd: DeviceFD) -> Option<Arc<NvDispDisp0>> {
        self.disp_files.lock().unwrap().get(&fd).cloned()
    }

    pub fn get_nvmap_device(&self, fd: DeviceFD) -> Option<Arc<NvMapDevice>> {
        self.nvmap_files.lock().unwrap().get(&fd).cloned()
    }
}

pub struct NvgemC {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl NvgemC {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "Initialize"),
                (1, None, "GetEventHandle"),
                (2, None, "ControlNotification"),
                (3, None, "SetNotificationPerm"),
                (4, None, "SetCoreDumpPerm"),
                (5, None, "GetAruid"),
                (6, None, "Reset"),
                (7, None, "GetAruid2"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for NvgemC {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "nvgem:c"
    }
}

impl ServiceFramework for NvgemC {
    fn get_service_name(&self) -> &str {
        "nvgem:c"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct NvgemCd {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl NvgemCd {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "Initialize"),
                (1, None, "GetAruid"),
                (2, None, "ReadNextBlock"),
                (3, None, "GetNextBlockSize"),
                (4, None, "ReadNextBlock2"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for NvgemCd {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "nvgem:cd"
    }
}

impl ServiceFramework for NvgemCd {
    fn get_service_name(&self) -> &str {
        "nvgem:cd"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct NvdbgD {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl NvdbgD {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "Open"),
                (1, None, "Ioctl"),
                (2, None, "Close"),
                (4, None, "QueryEvent"),
                (9, None, "DumpStatus"),
                (10, None, "InitializeDevtools"),
                (11, None, "Ioctl2"),
                (12, None, "Ioctl3"),
                (13, None, "SetConfiguration"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for NvdbgD {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "nvdbg:d"
    }
}

impl ServiceFramework for NvdbgD {
    fn get_service_name(&self) -> &str {
        "nvdbg:d"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Launches Nvidia services. Ownership matches Eden `nvdrv.cpp::LoopProcess`.
pub fn loop_process(system: crate::core::SystemRef) {
    use super::nvdrv_interface::NvdrvService;
    use super::nvmemp::Nvmemp;

    let server_manager = crate::hle::service::server_manager::ServerManager::new_shared(system);
    let module = Module::new(system);
    {
        let mut server_manager = server_manager.lock().unwrap();
        for name in ["nvdrv", "nvdrv:a", "nvdrv:s", "nvdrv:t"] {
            let service_name = name.to_owned();
            let module = Arc::clone(&module);
            server_manager.register_named_service(
                name,
                Box::new(move || Arc::new(NvdrvService::new(Arc::clone(&module), &service_name))),
                64,
            );
        }
        server_manager.register_named_service("nvgem:c", Box::new(|| Arc::new(NvgemC::new())), 64);
        server_manager.register_named_service(
            "nvgem:cd",
            Box::new(|| Arc::new(NvgemCd::new())),
            64,
        );
        let firmware_major =
            crate::hle::service::set::system_settings_server::get_firmware_version_impl(
                crate::hle::service::set::settings_types::GetFirmwareVersionType::Version1,
            )
            .major;
        if firmware_major >= 10 {
            server_manager.register_named_service(
                "nvdbg:d",
                Box::new(|| Arc::new(NvdbgD::new())),
                64,
            );
        }
        server_manager.register_named_service("nvmemp", Box::new(|| Arc::new(Nvmemp::new())), 64);
    }
    // Eden's LoopProcess keeps this shared_ptr on a native stack until the
    // server exits. A stopped cooperative guest fiber can abandon that stack
    // without running Rust destructors. Transfer (do not clone) the local
    // owner to the existing post-CPU service cleanup, so Module's devices and
    // their NX2 memory listeners are destroyed before System goes away.
    system
        .get()
        .kernel()
        .expect("NVDRV service requires a kernel")
        .retain_service_lifetime_owner(module);
    crate::hle::service::server_manager::ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod nx2_open_tests {
    use super::*;

    #[test]
    fn shutdown_releases_nvdrv_module_and_open_address_space() {
        use crate::hle::service::server_manager::ServerManager;

        let _without_global = crate::hle::kernel::kernel::ScopedKernelForTest::without_kernel();
        let mut system = crate::core::System::new_boxed_for_test();
        let kernel = system.kernel().unwrap();
        let module = Module::new(SystemRef::from_ref(&system));
        let weak_module = Arc::downgrade(&module);
        let fd = module.open("/dev/nvhost-as-gpu", SessionId::default());
        let weak_device = Arc::downgrade(module.open_files.lock().unwrap().get(&fd).unwrap());
        let manager = ServerManager::new_shared(SystemRef::from_ref(&system));
        let service_module = Arc::clone(&module);
        manager.lock().unwrap().register_named_service(
            "nvdrv:s",
            Box::new(move || Arc::new(super::super::nvdrv_interface::NvdrvService::new(
                Arc::clone(&service_module), "nvdrv:s",
            ))),
            64,
        );
        // The display services also retain Module. Reproduce that real graph,
        // not only the NVDRV factory: their abandoned locals used to keep the
        // old address-space listeners alive after the NVDRV local was fixed.
        let flinger = Arc::new(crate::hle::service::nvnflinger::nvnflinger::Nvnflinger::new(
            SystemRef::from_ref(&system),
        ));
        let weak_flinger = Arc::downgrade(&flinger);
        let binder = Arc::clone(flinger.get_binder_driver());
        manager.lock().unwrap().register_named_service("dispdrv",
            Box::new(move || binder.clone()), 64);
        let vi = crate::hle::service::vi::container::Container::new(SystemRef::from_ref(&system));
        let weak_vi = Arc::downgrade(&vi);
        let vi_for_stop = Arc::downgrade(&vi);
        let stop = system.register_stop_callback(move || {
            if let Some(vi) = vi_for_stop.upgrade() { vi.on_terminate(); }
        });
        kernel.track_server_manager_for_test(Arc::clone(&manager));
        kernel.retain_service_lifetime_owner(module);
        kernel.retain_service_lifetime_owner(flinger);
        kernel.retain_service_lifetime_owner((vi, stop));
        assert!(weak_module.upgrade().is_some());
        assert!(weak_device.upgrade().is_some());

        // Keep the manager Arc alive, as on an abandoned fiber stack. Its
        // factories and the transferred LoopProcess owner must both release.
        system.shutdown_main_process();
        assert!(weak_vi.upgrade().is_none());
        assert!(weak_flinger.upgrade().is_none());
        assert!(weak_module.upgrade().is_none());
        assert!(weak_device.upgrade().is_none());
        drop(manager);
    }

    #[test]
    fn event_interface_uses_module_context_and_releases_its_owner() {
        let _without_global = crate::hle::kernel::kernel::ScopedKernelForTest::without_kernel();
        let system = crate::core::System::new_boxed_for_test();
        let module = Module::new(SystemRef::from_ref(&system));
        let interface = module.get_events_interface();
        assert!(Arc::ptr_eq(&module.service_context, &interface.service_context));
        let readable = interface.create_event("NvdrvLifetime").unwrap();
        let (id, parent) = {
            let event = readable.lock().unwrap();
            (event.object_id, event.parent_id)
        };
        assert!(parent.is_some_and(|id| id != 0));
        let handle = interface.event_handles.lock().unwrap()[&id];
        let owner = module.service_context.lock().unwrap().get_event(handle).unwrap();
        assert!(Arc::ptr_eq(&readable, &owner.readable_event().unwrap()));
        interface.free_event(Arc::clone(&readable));
        assert!(module.service_context.lock().unwrap().get_event(handle).is_none());
        assert!(!interface.event_handles.lock().unwrap().contains_key(&id));
        assert_eq!(readable.lock().unwrap().object_id, id);
    }

    #[test]
    fn unknown_nvhost_uses_upstream_stub_and_other_unknown_names_do_not_consume_fd() {
        let system = crate::core::System::new_boxed_for_test();
        let module = Module::new(SystemRef::from_ref(&system));
        assert_eq!(module.open("/dev/not-nvhost", SessionId::default()), INVALID_NVDRV_FD);
        let fd = module.open("/dev/nvhost-tsg-gpu", SessionId::default());
        assert_eq!(fd, 1);
        let files = module.open_files.lock().unwrap();
        let device = files.get(&fd).unwrap();
        assert!(device.as_any().is::<super::super::devices::nx2_stub_device::Nx2StubDevice>());
        let mut output = [0xFF; 8];
        let mut inline = [0xFF; 4];
        assert_eq!(device.ioctl3(fd, Ioctl { raw: 0 }, &[1, 2, 3], &mut output, &mut inline), NvResult::Success);
        assert_eq!(output, [1, 2, 3, 0, 0, 0, 0, 0]);
        assert_eq!(inline, [0; 4]);
        drop(files);
        assert_eq!(module.close(fd), NvResult::Success);
    }
}

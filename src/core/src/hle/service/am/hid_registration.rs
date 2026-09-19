// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/hid_registration.h
//! Port of zuyu/src/core/hle/service/am/hid_registration.cpp

use std::sync::Arc;

use hid_core::resource_manager::ResourceManager;

use crate::core::SystemRef;
use crate::hle::service::os::process::Process;

/// Port of HidRegistration
///
/// Manages HID resource registration for an applet process.
/// On construction, registers the process's applet resource user ID
/// with the HID resource manager and enables vibration. On destruction,
/// unregisters vibration and the applet resource user ID.
pub struct HidRegistration {
    /// Cached state instead of upstream's reference to a non-moving Process.
    initialized: bool,
    /// Upstream: obtained via `system.ServiceManager().GetService<HID::IHidServer>("hid")`
    /// then `m_hid_server->GetResourceManager()`.
    resource_manager: Option<Arc<parking_lot::Mutex<ResourceManager>>>,
    /// Cached PID for Drop.
    pid: u64,
}

unsafe impl Send for HidRegistration {}
unsafe impl Sync for HidRegistration {}

impl HidRegistration {
    /// Creates a new HidRegistration.
    ///
    /// Upstream constructor calls:
    /// - RegisterAppletResourceUserId(pid, true)
    /// - SetAruidValidForVibration(pid, true)
    pub fn new(system: SystemRef, process: &Process) -> Self {
        let resource_manager = if !system.is_null() {
            system
                .get()
                .service_manager()
                .map(|service_manager| {
                    let handler = crate::hle::service::sm::sm::ServiceManager::get_service_blocking(
                        &service_manager,
                        system,
                        "hid",
                    );
                    handler
                        .as_any()
                        .downcast_ref::<crate::hle::service::hid::hid_server::IHidServer>()
                        .map(|hid_server| hid_server.get_resource_manager())
                })
                .flatten()
        } else {
            None
        };

        let mut registration = Self {
            initialized: false,
            resource_manager,
            pid: 0,
        };
        registration.register_current_process(process);
        registration
    }

    /// Upstream RegisterCurrentProcess. Pass the current owner explicitly:
    /// Applet is movable, so retaining a pointer to its Process is not safe.
    pub fn register_current_process(&mut self, process: &Process) {
        self.pid = process.get_process_id();
        self.initialized = process.is_initialized();
        if self.initialized {
            if let Some(ref rm) = self.resource_manager {
                let rm = rm.lock();
                rm.register_applet_resource_user_id(self.pid, true);
                rm.set_aruid_valid_for_vibration(self.pid, true);
            }
        }
    }

    /// Forward input enable/disable to HID resource manager.
    ///
    /// Upstream calls:
    /// - SetAruidValidForVibration(pid, enable)
    /// - EnableInput(pid, enable)
    pub fn enable_applet_to_get_input(&self, enable: bool) {
        if let Some(ref rm) = self.resource_manager {
            let rm = rm.lock();
            rm.set_aruid_valid_for_vibration(self.pid, enable);
            rm.enable_input(self.pid, enable);
        }
    }
}

impl Drop for HidRegistration {
    /// Upstream destructor calls:
    /// - SetAruidValidForVibration(pid, false)
    /// - UnregisterAppletResourceUserId(pid)
    fn drop(&mut self) {
        if self.initialized {
            if let Some(ref rm) = self.resource_manager {
                let rm = rm.lock();
                rm.set_aruid_valid_for_vibration(self.pid, false);
                rm.unregister_applet_resource_user_id(self.pid);
            }
        }
    }
}

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
    /// - EnablePadInput(pid, enable_pad)
    /// - EnableTouchScreen(pid, enable_touch)
    /// - SetAruidValidForVibration(pid, enable_pad)
    pub fn enable_applet_to_get_input(&self, enable_pad: bool, enable_touch: bool) {
        if !self.initialized {
            return;
        }
        if let Some(ref rm) = self.resource_manager {
            let rm = rm.lock();
            rm.enable_pad_input(self.pid, enable_pad);
            rm.enable_touch_screen(self.pid, enable_touch);
            rm.set_aruid_valid_for_vibration(self.pid, enable_pad);
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

#[cfg(test)]
mod tests {
    use super::*;
    use hid_core::hid_core::HIDCore;
    use hid_core::resources::hid_firmware_settings::HidFirmwareSettings;

    #[test]
    fn pad_touch_and_vibration_are_independent_of_other_input_flags() {
        let manager = Arc::new(parking_lot::Mutex::new(ResourceManager::new(
            Arc::new(HidFirmwareSettings::new()),
            Arc::new(parking_lot::Mutex::new(HIDCore::new())),
        )));
        let pid = 0x51;
        manager.lock().register_applet_resource_user_id(pid, true);
        let resource = manager.lock().get_applet_resource().unwrap();
        let mut registration = HidRegistration {
            initialized: true,
            resource_manager: Some(manager),
            pid,
        };
        for (pad, touch) in [(false, true), (true, false), (false, false), (true, true)] {
            registration.enable_applet_to_get_input(pad, touch);
            let resource = resource.lock();
            let flags = resource.get_aruid_data(pid).unwrap().flag;
            assert_eq!(flags.enable_pad_input(), pad);
            assert_eq!(flags.enable_touchscreen(), touch);
            assert_eq!(resource.is_vibration_aruid_active(pid), pad);
            assert!(flags.enable_six_axis_sensor());
            assert!(flags.bit_18());
        }
        registration.initialized = false;
        registration.enable_applet_to_get_input(false, false);
        let flags = resource.lock().get_aruid_data(pid).unwrap().flag;
        assert!(flags.enable_pad_input());
        assert!(flags.enable_touchscreen());
        assert!(resource.lock().is_vibration_aruid_active(pid));
        registration.initialized = true;
    }
}

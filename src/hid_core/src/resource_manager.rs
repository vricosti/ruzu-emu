// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of hid_core/resource_manager.h and hid_core/resource_manager.cpp

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use common::ResultCode;

use crate::hid_core::HIDCore;
use crate::hid_result;
use crate::hid_types::*;
use crate::hid_util;
use crate::resources::applet_resource::{
    AppletResource, AppletResourceHolder, HandheldConfig, SYSTEM_ARUID,
};
use crate::resources::debug_pad::debug_pad::DebugPad;
use crate::resources::digitizer::digitizer::Digitizer;
use crate::resources::hid_firmware_settings::HidFirmwareSettings;
use crate::resources::keyboard::keyboard::Keyboard;
use crate::resources::mouse::debug_mouse::DebugMouse;
use crate::resources::mouse::mouse::Mouse;
use crate::resources::npad::npad::NPad;
use crate::resources::palma::palma::Palma;
use crate::resources::six_axis::console_six_axis::ConsoleSixAxis;
use crate::resources::six_axis::seven_six_axis::SevenSixAxis;
use crate::resources::six_axis::six_axis::SixAxis;
use crate::resources::system_buttons::capture_button::CaptureButton;
use crate::resources::system_buttons::home_button::HomeButton;
use crate::resources::system_buttons::sleep_button::SleepButton;
use crate::resources::touch_screen::gesture::Gesture;
use crate::resources::touch_screen::touch_screen::TouchScreen;
use crate::resources::touch_screen::touch_screen_driver::TouchScreenDriver;
use crate::resources::touch_screen::touch_screen_resource::TouchResource;
use crate::resources::unique_pad::unique_pad::UniquePad;

/// Updating period for each HID device.
/// Period time is obtained by measuring the number of samples in a second on HW using a homebrew
/// Correct npad_update_ns is 4ms this is overclocked to lower input lag
pub const NPAD_UPDATE_NS: Duration = Duration::from_nanos(1_000_000); // 1ms, 1000Hz
pub const DEFAULT_UPDATE_NS: Duration = Duration::from_nanos(4_000_000); // 4ms, 250Hz
pub const MOUSE_KEYBOARD_UPDATE_NS: Duration = Duration::from_nanos(8_000_000); // 8ms, 125Hz
pub const MOTION_UPDATE_NS: Duration = Duration::from_nanos(5_000_000); // 5ms, 200Hz

pub struct ResourceManager {
    is_initialized: bool,
    shared_mutex: parking_lot::RwLock<()>,
    applet_resource: Option<Arc<Mutex<AppletResource>>>,
    handheld_config: Option<Arc<Mutex<HandheldConfig>>>,
    firmware_settings: Option<Arc<HidFirmwareSettings>>,
    hid_core: Option<Arc<Mutex<HIDCore>>>,

    // HID resource objects matching upstream fields
    debug_pad: Option<Arc<Mutex<DebugPad>>>,
    mouse: Option<Arc<Mutex<Mouse>>>,
    debug_mouse: Option<Arc<Mutex<DebugMouse>>>,
    keyboard: Option<Arc<Mutex<Keyboard>>>,
    unique_pad: Option<Arc<Mutex<UniquePad>>>,
    npad: Option<Arc<Mutex<NPad>>>,
    home_button: Option<Arc<Mutex<HomeButton>>>,
    sleep_button: Option<Arc<Mutex<SleepButton>>>,
    capture_button: Option<Arc<Mutex<CaptureButton>>>,
    digitizer: Option<Arc<Mutex<Digitizer>>>,
    palma: Option<Arc<Mutex<Palma>>>,
    six_axis: Option<Arc<Mutex<SixAxis>>>,
    seven_six_axis: Option<Arc<Mutex<SevenSixAxis>>>,
    console_six_axis: Option<Arc<Mutex<ConsoleSixAxis>>>,

    // Touch resources
    gesture: Option<Arc<Mutex<Gesture>>>,
    touch_screen: Option<Arc<Mutex<TouchScreen>>>,
    touch_resource: Option<Arc<Mutex<TouchResource>>>,
    touch_driver: Option<Arc<Mutex<TouchScreenDriver>>>,
}

impl ResourceManager {
    pub fn new(firmware_settings: Arc<HidFirmwareSettings>, hid_core: Arc<Mutex<HIDCore>>) -> Self {
        Self {
            is_initialized: false,
            shared_mutex: parking_lot::RwLock::new(()),
            applet_resource: Some(Arc::new(Mutex::new(AppletResource::new()))),
            handheld_config: None,
            firmware_settings: Some(firmware_settings),
            hid_core: Some(hid_core),
            debug_pad: None,
            mouse: None,
            debug_mouse: None,
            keyboard: None,
            unique_pad: None,
            npad: None,
            home_button: None,
            sleep_button: None,
            capture_button: None,
            digitizer: None,
            palma: None,
            six_axis: None,
            seven_six_axis: None,
            console_six_axis: None,
            gesture: None,
            touch_screen: None,
            touch_resource: None,
            touch_driver: None,
        }
    }

    /// Wires the kernel-backed shared-memory factory used by AppletResource.
    /// Must be called by the `core` crate during HID startup, before any IPC
    /// path triggers `create_applet_resource` / `register_core_applet_resource`.
    pub fn set_shared_memory_backing(
        &self,
        backing: Arc<dyn crate::resources::shared_memory_holder::KSharedMemoryBacking>,
    ) {
        if let Some(ref applet_resource) = self.applet_resource {
            applet_resource.lock().set_shared_memory_backing(backing);
        }
    }

    pub fn initialize(&mut self) {
        if self.is_initialized {
            return;
        }

        // Upstream: system.HIDCore().ReloadInputDevices()
        if let Some(ref hid_core) = self.hid_core {
            hid_core.lock().reload_input_devices();
        }

        self.initialize_handheld_config();
        self.initialize_hid_common_sampler();
        self.initialize_touch_screen_sampler();
        self.initialize_console_six_axis_sampler();
        self.initialize_ahid_sampler();

        self.is_initialized = true;
    }

    pub fn get_applet_resource(&self) -> Option<Arc<Mutex<AppletResource>>> {
        self.applet_resource.clone()
    }

    pub fn get_capture_button(&self) -> Option<Arc<Mutex<CaptureButton>>> {
        self.capture_button.clone()
    }

    pub fn get_console_six_axis(&self) -> Option<Arc<Mutex<ConsoleSixAxis>>> {
        self.console_six_axis.clone()
    }

    pub fn get_debug_mouse(&self) -> Option<Arc<Mutex<DebugMouse>>> {
        self.debug_mouse.clone()
    }

    pub fn get_debug_pad(&self) -> Option<Arc<Mutex<DebugPad>>> {
        self.debug_pad.clone()
    }

    pub fn get_digitizer(&self) -> Option<Arc<Mutex<Digitizer>>> {
        self.digitizer.clone()
    }

    pub fn get_gesture(&self) -> Option<Arc<Mutex<Gesture>>> {
        self.gesture.clone()
    }

    pub fn get_home_button(&self) -> Option<Arc<Mutex<HomeButton>>> {
        self.home_button.clone()
    }

    pub fn get_keyboard(&self) -> Option<Arc<Mutex<Keyboard>>> {
        self.keyboard.clone()
    }

    pub fn get_mouse(&self) -> Option<Arc<Mutex<Mouse>>> {
        self.mouse.clone()
    }

    pub fn get_npad(&self) -> Option<Arc<Mutex<NPad>>> {
        self.npad.clone()
    }

    pub fn get_palma(&self) -> Option<Arc<Mutex<Palma>>> {
        self.palma.clone()
    }

    pub fn get_seven_six_axis(&self) -> Option<Arc<Mutex<SevenSixAxis>>> {
        self.seven_six_axis.clone()
    }

    pub fn get_six_axis(&self) -> Option<Arc<Mutex<SixAxis>>> {
        self.six_axis.clone()
    }

    pub fn get_sleep_button(&self) -> Option<Arc<Mutex<SleepButton>>> {
        self.sleep_button.clone()
    }

    pub fn get_touch_screen(&self) -> Option<Arc<Mutex<TouchScreen>>> {
        self.touch_screen.clone()
    }

    pub fn get_touch_resource(&self) -> Option<Arc<Mutex<TouchResource>>> {
        self.touch_resource.clone()
    }

    pub fn get_touch_driver(&self) -> Option<Arc<Mutex<TouchScreenDriver>>> {
        self.touch_driver.clone()
    }

    pub fn get_unique_pad(&self) -> Option<Arc<Mutex<UniquePad>>> {
        self.unique_pad.clone()
    }

    pub fn create_applet_resource(&self, aruid: u64) -> ResultCode {
        if aruid == SYSTEM_ARUID {
            let result = self.register_core_applet_resource();
            if result.is_error() {
                return result;
            }
            // Upstream: GetNpad()->ActivateNpadResource()
            if let Some(ref npad) = self.npad {
                npad.lock().activate_npad_resource();
            }
            return ResultCode::SUCCESS;
        }

        let mut result = self.create_applet_resource_impl(aruid);
        if result == hid_result::RESULT_ARUID_NOT_REGISTERED {
            // ruzu can construct AM applets before the hid service is reachable,
            // so HidRegistration may miss its constructor-time registration.
            // Preserve the upstream resource-manager ownership by repairing the
            // missing ARUID registration here, before retrying CreateAppletResource.
            let register_result = self.register_applet_resource_user_id(aruid, true);
            if register_result.is_success()
                || register_result == hid_result::RESULT_ARUID_ALREADY_REGISTERED
            {
                result = self.create_applet_resource_impl(aruid);
            } else {
                result = register_result;
            }
        }
        if result.is_error() {
            return result;
        }

        // Homebrew doesn't try to activate some controllers, so we activate them by default
        if let Some(ref npad) = self.npad {
            npad.lock().activate();
        }
        if let Some(ref six_axis) = self.six_axis {
            six_axis.lock().activation.activate();
        }
        if let (Some(touch_screen), Some(touch_resource), Some(touch_driver)) = (
            self.touch_screen.as_ref(),
            self.touch_resource.as_ref(),
            self.touch_driver.as_ref(),
        ) {
            let mut resource = touch_resource.lock();
            let mut driver = touch_driver.lock();
            let _ = touch_screen.lock().activate(&mut resource, &mut driver);
        }
        if let (Some(gesture), Some(touch_resource), Some(touch_driver)) = (
            self.gesture.as_ref(),
            self.touch_resource.as_ref(),
            self.touch_driver.as_ref(),
        ) {
            let mut resource = touch_resource.lock();
            let mut driver = touch_driver.lock();
            let _ = gesture.lock().activate(&mut resource, &mut driver);
        }

        // Upstream: GetNpad()->ActivateNpadResource(aruid)
        if let Some(ref npad) = self.npad {
            npad.lock().activate_npad_resource_with_aruid(aruid);
        }
        ResultCode::SUCCESS
    }

    fn create_applet_resource_impl(&self, aruid: u64) -> ResultCode {
        let _lock = self.shared_mutex.write();
        if let Some(ref resource) = self.applet_resource {
            resource.lock().create_applet_resource(aruid)
        } else {
            ResultCode::SUCCESS
        }
    }

    pub fn register_core_applet_resource(&self) -> ResultCode {
        let _lock = self.shared_mutex.write();
        if let Some(ref resource) = self.applet_resource {
            resource.lock().register_core_applet_resource()
        } else {
            ResultCode::SUCCESS
        }
    }

    pub fn unregister_core_applet_resource(&self) -> ResultCode {
        let _lock = self.shared_mutex.write();
        if let Some(ref resource) = self.applet_resource {
            resource.lock().unregister_core_applet_resource()
        } else {
            ResultCode::SUCCESS
        }
    }

    pub fn register_applet_resource_user_id(&self, aruid: u64, enable_input: bool) -> ResultCode {
        let _lock = self.shared_mutex.write();
        let mut result = ResultCode::SUCCESS;
        if let Some(ref resource) = self.applet_resource {
            result = resource
                .lock()
                .register_applet_resource_user_id(aruid, enable_input);
            if result.is_error() {
                return result;
            }
        }
        if let Some(ref npad) = self.npad {
            result = npad.lock().register_applet_resource_user_id(aruid);
        }
        result
    }

    pub fn unregister_applet_resource_user_id(&self, aruid: u64) {
        let _lock = self.shared_mutex.write();
        if let Some(ref resource) = self.applet_resource {
            resource.lock().unregister_applet_resource_user_id(aruid);
        }
        if let Some(ref npad) = self.npad {
            npad.lock().unregister_applet_resource_user_id(aruid);
        }
    }

    /// Port of upstream `ResourceManager::GetSharedMemoryHandle`.
    ///
    /// The upstream owner returns a `Kernel::KSharedMemory*`. The Rust port
    /// preserves method ownership and validation here. `hid_core` returns the
    /// holder's keepalive as `Arc<dyn Any + Send + Sync>` (since it cannot
    /// name `KSharedMemory` directly), and the `core` IPC layer downcasts it
    /// to `Arc<KSharedMemory>` to register on the process handle table.
    pub fn get_shared_memory_handle(
        &self,
        aruid: u64,
    ) -> Result<std::sync::Arc<dyn std::any::Any + Send + Sync>, ResultCode> {
        let _lock = self.shared_mutex.read();
        let Some(ref resource) = self.applet_resource else {
            return Err(hid_result::RESULT_SHARED_MEMORY_NOT_INITIALIZED);
        };
        resource.lock().get_shared_memory_handle(aruid)
    }

    pub fn free_applet_resource_id(&self, aruid: u64) {
        let _lock = self.shared_mutex.write();
        if let Some(ref resource) = self.applet_resource {
            resource.lock().free_applet_resource_id(aruid);
        }
        if let Some(ref npad) = self.npad {
            npad.lock().free_applet_resource_id(aruid);
        }
    }

    pub fn enable_input(&self, aruid: u64, is_enabled: bool) {
        let _lock = self.shared_mutex.read();
        if let Some(ref resource) = self.applet_resource {
            resource.lock().enable_input(aruid, is_enabled);
        }
    }

    pub fn enable_six_axis_sensor(&self, aruid: u64, is_enabled: bool) {
        let _lock = self.shared_mutex.read();
        if let Some(ref resource) = self.applet_resource {
            resource.lock().enable_six_axis_sensor(aruid, is_enabled);
        }
    }

    pub fn enable_pad_input(&self, aruid: u64, is_enabled: bool) {
        let _lock = self.shared_mutex.read();
        if let Some(ref resource) = self.applet_resource {
            resource.lock().enable_pad_input(aruid, is_enabled);
        }
    }

    pub fn enable_touch_screen(&self, aruid: u64, is_enabled: bool) {
        let _lock = self.shared_mutex.read();
        if let Some(ref resource) = self.applet_resource {
            resource.lock().enable_touch_screen(aruid, is_enabled);
        }
    }

    pub fn set_aruid_valid_for_vibration(&self, aruid: u64, is_enabled: bool) -> ResultCode {
        let _lock = self.shared_mutex.write();
        if let Some(ref resource) = self.applet_resource {
            let _has_changed = resource
                .lock()
                .set_aruid_valid_for_vibration(aruid, is_enabled);
            // Upstream: if has_changed, iterates npad->GetAllVibrationDevices() (but the
            // loop body is an upstream TODO). Also checks vibration_handler session aruid.
        }

        if let Some(ref npad) = self.npad {
            let npad_guard = npad.lock();
            let session_aruid = npad_guard.get_vibration_handler_session_aruid();
            if aruid != session_aruid {
                npad_guard.end_permit_vibration_session();
            }
        }

        ResultCode::SUCCESS
    }

    pub fn set_force_handheld_style_vibration(&self, is_forced: bool) {
        if let Some(ref config) = self.handheld_config {
            config.lock().is_force_handheld_style_vibration = is_forced;
        }
    }

    pub fn is_vibration_aruid_active(&self, aruid: u64) -> Result<bool, ResultCode> {
        let _lock = self.shared_mutex.read();
        if let Some(ref resource) = self.applet_resource {
            Ok(resource.lock().is_vibration_aruid_active(aruid))
        } else {
            Ok(false)
        }
    }

    /// Port of ResourceManager::GetVibrationDeviceInfo.
    pub fn get_vibration_device_info(
        &self,
        handle: &VibrationDeviceHandle,
    ) -> Result<VibrationDeviceInfo, ResultCode> {
        let is_valid = hid_util::is_vibration_handle_valid(handle);
        if is_valid.is_error() {
            return Err(is_valid);
        }

        let mut check_device_index = false;

        let device_type = match handle.npad_type {
            NpadStyleIndex::Fullkey
            | NpadStyleIndex::Handheld
            | NpadStyleIndex::JoyconDual
            | NpadStyleIndex::JoyconLeft
            | NpadStyleIndex::JoyconRight => {
                check_device_index = true;
                VibrationDeviceType::LinearResonantActuator
            }
            NpadStyleIndex::GameCube => VibrationDeviceType::GcErm,
            NpadStyleIndex::N64 => VibrationDeviceType::N64,
            _ => VibrationDeviceType::Unknown,
        };

        let position = if check_device_index {
            match handle.device_index {
                DeviceIndex::Left => VibrationDevicePosition::Left,
                DeviceIndex::Right => VibrationDevicePosition::Right,
                _ => {
                    log::error!("DeviceIndex should never be None!");
                    VibrationDevicePosition::None
                }
            }
        } else {
            VibrationDevicePosition::None
        };

        Ok(VibrationDeviceInfo {
            device_type,
            position,
        })
    }

    /// Port of `ResourceManager::GetVibrationDevice(...)->Activate()`.
    pub fn activate_vibration_device(&self, handle: &VibrationDeviceHandle) -> ResultCode {
        let is_valid = hid_util::is_vibration_handle_valid(handle);
        if is_valid.is_error() {
            return is_valid;
        }
        let Some(ref npad) = self.npad else {
            return ResultCode::SUCCESS;
        };
        let mut npad = npad.lock();
        match npad.get_vibration_device_mut(handle) {
            Some(device) => device.activate(),
            None => ResultCode::SUCCESS,
        }
    }

    /// Port of `ResourceManager::SendVibrationValue`.
    pub fn send_vibration_value(
        &self,
        aruid: u64,
        handle: &VibrationDeviceHandle,
        value: &VibrationValue,
    ) -> ResultCode {
        let has_active_aruid = match self.is_vibration_aruid_active(aruid) {
            Ok(active) => active,
            Err(result) => return result,
        };
        if !has_active_aruid {
            return ResultCode::SUCCESS;
        }
        let is_valid = hid_util::is_vibration_handle_valid(handle);
        if is_valid.is_error() {
            return is_valid;
        }
        let Some(ref npad) = self.npad else {
            return ResultCode::SUCCESS;
        };
        let mut npad = npad.lock();
        let Some(device) = npad.get_vibration_device_mut(handle) else {
            return ResultCode::SUCCESS;
        };
        if !device.is_active() {
            return ResultCode::SUCCESS;
        }
        device.send_vibration_value(value)
    }

    /// Port of `NpadVibrationDevice::GetActualVibrationValue` through
    /// `ResourceManager::GetNSVibrationDevice`.
    pub fn get_actual_vibration_value(
        &self,
        aruid: u64,
        handle: &VibrationDeviceHandle,
    ) -> Result<VibrationValue, ResultCode> {
        let has_active_aruid = self.is_vibration_aruid_active(aruid)?;
        if !has_active_aruid {
            return Ok(DEFAULT_VIBRATION_VALUE);
        }
        let is_valid = hid_util::is_vibration_handle_valid(handle);
        if is_valid.is_error() {
            return Err(is_valid);
        }
        let Some(ref npad) = self.npad else {
            return Ok(DEFAULT_VIBRATION_VALUE);
        };
        let mut npad = npad.lock();
        let Some(device) = npad.get_vibration_device_mut(handle) else {
            return Ok(DEFAULT_VIBRATION_VALUE);
        };
        device.get_actual_vibration_value()
    }

    /// Port of `IHidServer::IsVibrationDeviceMounted`.
    pub fn is_vibration_device_mounted(
        &self,
        handle: &VibrationDeviceHandle,
    ) -> Result<bool, ResultCode> {
        let is_valid = hid_util::is_vibration_handle_valid(handle);
        if is_valid.is_error() {
            return Err(is_valid);
        }
        let Some(ref npad) = self.npad else {
            return Ok(false);
        };
        let mut npad = npad.lock();
        Ok(npad
            .get_vibration_device_mut(handle)
            .is_some_and(|device| device.is_vibration_mounted()))
    }

    /// Port of ResourceManager::GetTouchScreenFirmwareVersion.
    pub fn get_touch_screen_firmware_version(&self) -> Result<FirmwareVersion, ResultCode> {
        Ok(FirmwareVersion::default())
    }

    /// Port of ResourceManager::UpdateControllers.
    /// Upstream calls: debug_pad->OnUpdate, digitizer->OnUpdate, unique_pad->OnUpdate,
    /// palma->OnUpdate, home_button->OnUpdate, sleep_button->OnUpdate,
    /// capture_button->OnUpdate.
    pub fn update_controllers(&self, _ns_late: Duration) {
        let _lock = self.shared_mutex.read();

        // Get the active aruid and check if we have valid shared memory
        let applet_resource = match self.applet_resource {
            Some(ref ar) => ar.clone(),
            None => return,
        };

        let hid_core = match self.hid_core {
            Some(ref hc) => hc.clone(),
            None => return,
        };

        let mut ar_guard = applet_resource.lock();
        let active_aruid = ar_guard.get_active_aruid();
        let data = match ar_guard.get_aruid_data(active_aruid) {
            Some(d) => d.clone(),
            None => return,
        };

        if !data.flag.is_assigned() {
            return;
        }

        let shared_memory: *mut crate::resources::shared_memory_format::SharedMemoryFormat =
            match ar_guard.get_shared_memory_format_mut(active_aruid) {
                Some(sm) => sm as *mut _,
                None => return,
            };
        // SAFETY: We hold the applet_resource lock and shared_mutex read lock,
        // so no other thread can modify the shared memory concurrently.
        let shared_memory = unsafe { &mut *shared_memory };

        let (other_controller, player_one_controller) = {
            let hid_core = hid_core.lock();
            (
                hid_core.get_emulated_controller(NpadIdType::Other),
                hid_core.get_emulated_controller(NpadIdType::Player1),
            )
        };
        let timestamp_ns = 0i64; // Upstream passes core_timing.GetGlobalTimeNs().count()

        // debug_pad->OnUpdate(core_timing)
        if let Some(ref debug_pad) = self.debug_pad {
            let mut dp = debug_pad.lock();
            let controller = other_controller.lock();
            let button_state = controller.get_debug_pad_buttons();
            let sticks = controller.get_sticks();
            let stick_state = crate::resources::debug_pad::debug_pad::StickState {
                left: sticks.left,
                right: sticks.right,
            };
            dp.on_update(
                &mut shared_memory.debug_pad,
                &button_state,
                &stick_state,
            );
        }

        // digitizer->OnUpdate(core_timing)
        if let Some(ref digitizer) = self.digitizer {
            let mut dig = digitizer.lock();
            dig.on_update(&mut shared_memory.digitizer, timestamp_ns);
        }

        // unique_pad->OnUpdate(core_timing)
        if let Some(ref unique_pad) = self.unique_pad {
            let mut up = unique_pad.lock();
            up.on_update(&mut shared_memory.unique_pad, timestamp_ns);
        }

        // palma->OnUpdate(core_timing)
        if let Some(ref palma) = self.palma {
            let mut p = palma.lock();
            p.on_update();
        }

        // home_button->OnUpdate(core_timing)
        if let Some(ref home_button) = self.home_button {
            let mut hb = home_button.lock();
            let controller = player_one_controller.lock();
            let home_buttons = controller.get_home_buttons();
            hb.on_update(&mut shared_memory.home_button, home_buttons);
        }

        // sleep_button->OnUpdate(core_timing)
        if let Some(ref sleep_button) = self.sleep_button {
            let mut sb = sleep_button.lock();
            sb.on_update(&mut shared_memory.sleep_button);
        }

        // capture_button->OnUpdate(core_timing)
        if let Some(ref capture_button) = self.capture_button {
            let mut cb = capture_button.lock();
            let controller = player_one_controller.lock();
            // Upstream quirk: uses GetHomeButtons() for capture button too
            let home_buttons = controller.get_home_buttons();
            cb.on_update(
                &mut shared_memory.capture_button,
                CaptureButtonState {
                    raw: home_buttons.raw,
                },
            );
        }
    }

    /// Port of ResourceManager::UpdateNpad.
    /// Upstream calls npad->OnUpdate(core_timing).
    pub fn update_npad(&self, _ns_late: Duration) {
        let _lock = self.shared_mutex.read();

        if let Some(ref npad) = self.npad {
            let mut npad_guard = npad.lock();
            // NPad::OnUpdate is complex — it iterates all aruids and controller data,
            // reads input from EmulatedController and writes to shared memory.
            // The npad holds its own applet_resource_holder for this purpose.
            npad_guard.on_update();
        }
    }

    /// Port of ResourceManager::UpdateMouseKeyboard.
    /// Upstream calls: mouse->OnUpdate, debug_mouse->OnUpdate, keyboard->OnUpdate.
    pub fn update_mouse_keyboard(&self, _ns_late: Duration) {
        let _lock = self.shared_mutex.read();

        let applet_resource = match self.applet_resource {
            Some(ref ar) => ar.clone(),
            None => return,
        };

        let hid_core = match self.hid_core {
            Some(ref hc) => hc.clone(),
            None => return,
        };

        let mut ar_guard = applet_resource.lock();
        let active_aruid = ar_guard.get_active_aruid();
        let data = match ar_guard.get_aruid_data(active_aruid) {
            Some(d) => d.clone(),
            None => return,
        };

        if !data.flag.is_assigned() {
            return;
        }

        let shared_memory: *mut crate::resources::shared_memory_format::SharedMemoryFormat =
            match ar_guard.get_shared_memory_format_mut(active_aruid) {
                Some(sm) => sm as *mut _,
                None => return,
            };
        let shared_memory = unsafe { &mut *shared_memory };

        let hc_guard = hid_core.lock();
        let devices = hc_guard.get_emulated_devices();

        // mouse->OnUpdate(core_timing)
        if let Some(ref mouse) = self.mouse {
            let mut m = mouse.lock();
            let mouse_button_state = devices.get_mouse_buttons();
            let mouse_position_state = devices.get_mouse_position();
            let mouse_wheel_state = devices.get_mouse_wheel();
            m.on_update(
                &mut shared_memory.mouse,
                &mouse_button_state,
                &crate::resources::mouse::mouse::MousePosition {
                    x: mouse_position_state.x,
                    y: mouse_position_state.y,
                },
                &mouse_wheel_state,
            );
        }

        // debug_mouse->OnUpdate(core_timing)
        if let Some(ref debug_mouse) = self.debug_mouse {
            let mut dm = debug_mouse.lock();
            let mouse_button_state = devices.get_mouse_buttons();
            let mouse_position_state = devices.get_mouse_position();
            let mouse_wheel_state = devices.get_mouse_wheel();
            dm.on_update(
                &mut shared_memory.debug_mouse,
                &mouse_button_state,
                &crate::resources::mouse::mouse::MousePosition {
                    x: mouse_position_state.x,
                    y: mouse_position_state.y,
                },
                &mouse_wheel_state,
            );
        }

        // keyboard->OnUpdate(core_timing)
        if let Some(ref keyboard) = self.keyboard {
            let mut kb = keyboard.lock();
            let keyboard_state = devices.get_keyboard();
            let keyboard_modifier = devices.get_keyboard_modifier();
            kb.on_update(
                &mut shared_memory.keyboard,
                &keyboard_state,
                &keyboard_modifier,
            );
        }
    }

    /// Port of ResourceManager::UpdateMotion.
    /// Upstream calls: six_axis->OnUpdate, seven_six_axis->OnUpdate,
    /// console_six_axis->OnUpdate.
    pub fn update_motion(&self, _ns_late: Duration) {
        let _lock = self.shared_mutex.read();

        // six_axis->OnUpdate(core_timing)
        if let Some(ref six_axis) = self.six_axis {
            let mut sa = six_axis.lock();
            sa.on_update();
        }

        // seven_six_axis->OnUpdate(core_timing)
        if let Some(ref seven_six_axis) = self.seven_six_axis {
            let mut ssa = seven_six_axis.lock();
            ssa.on_update();
        }

        // console_six_axis->OnUpdate(core_timing)
        if let Some(ref console_six_axis) = self.console_six_axis {
            console_six_axis.lock().on_update();
        }
    }

    /// Port of the touch-update callback wired from upstream
    /// `ResourceManager::InitializeTouchScreenSampler()`.
    ///
    /// In upstream this is invoked by the CoreTiming event `touch_update_event`
    /// and forwards to `TouchResource::OnTouchUpdate(time)`.
    pub fn update_touch_screen(&self, timestamp: i64) {
        let _lock = self.shared_mutex.read();

        let is_handheld_hid_enabled = self
            .handheld_config
            .as_ref()
            .map(|cfg| cfg.lock().is_handheld_hid_enabled)
            .unwrap_or(true);

        if let (Some(touch_screen), Some(touch_resource), Some(touch_driver)) = (
            self.touch_screen.as_ref(),
            self.touch_resource.as_ref(),
            self.touch_driver.as_ref(),
        ) {
            let touch_screen = touch_screen.lock();
            let mut touch_resource = touch_resource.lock();
            let mut touch_driver = touch_driver.lock();
            touch_screen.on_touch_update(
                &mut touch_resource,
                &mut touch_driver,
                is_handheld_hid_enabled,
                timestamp,
            );
        }
    }

    fn initialize_handheld_config(&mut self) {
        let mut config = HandheldConfig {
            is_handheld_hid_enabled: true,
            is_joycon_rail_enabled: true,
            is_force_handheld_style_vibration: false,
            is_force_handheld: false,
        };
        if let Some(ref fw) = self.firmware_settings {
            if fw.is_handheld_forced() {
                config.is_joycon_rail_enabled = false;
            }
        }
        self.handheld_config = Some(Arc::new(Mutex::new(config)));
    }

    fn initialize_hid_common_sampler(&mut self) {
        // Create all resource objects. Upstream passes HIDCore& to each constructor.
        // In Rust, we store Arc<Mutex<HIDCore>> references where needed.
        self.debug_pad = Some(Arc::new(Mutex::new(DebugPad::new())));
        self.mouse = Some(Arc::new(Mutex::new(Mouse::new())));
        self.debug_mouse = Some(Arc::new(Mutex::new(DebugMouse::new())));
        self.keyboard = Some(Arc::new(Mutex::new(Keyboard::new())));
        self.unique_pad = Some(Arc::new(Mutex::new(UniquePad::new())));
        self.npad = Some(Arc::new(Mutex::new(NPad::new_with_hid_core(
            self.hid_core
                .as_ref()
                .expect("ResourceManager requires HIDCore")
                .clone(),
        ))));
        self.home_button = Some(Arc::new(Mutex::new(HomeButton::new())));
        self.sleep_button = Some(Arc::new(Mutex::new(SleepButton::new())));
        self.capture_button = Some(Arc::new(Mutex::new(CaptureButton::new())));
        self.digitizer = Some(Arc::new(Mutex::new(Digitizer::new())));
        self.palma = Some(Arc::new(Mutex::new(Palma::new())));
        self.six_axis = Some(Arc::new(Mutex::new(SixAxis::new(
            &self.hid_core.as_ref().expect("ResourceManager requires HIDCore").lock(),
        ))));

        // Wire SetAppletResource for each controller that needs it.
        // Upstream: each_resource->SetAppletResource(applet_resource, &shared_mutex)
        // In the Rust port, we store the applet_resource Arc in each controller's activation.
        if let Some(ref applet_resource) = self.applet_resource {
            if let Some(ref hid_core) = self.hid_core {
                // Wire debug_pad
                if let Some(ref debug_pad) = self.debug_pad {
                    let mut dp = debug_pad.lock();
                    dp.activation.set_applet_resource(applet_resource.clone());
                    dp.activation.set_hid_core(hid_core.clone());
                }
                // Wire digitizer
                if let Some(ref digitizer) = self.digitizer {
                    let mut d = digitizer.lock();
                    d.activation.set_applet_resource(applet_resource.clone());
                    d.activation.set_hid_core(hid_core.clone());
                }
                // Wire unique_pad
                if let Some(ref unique_pad) = self.unique_pad {
                    let mut up = unique_pad.lock();
                    up.activation.set_applet_resource(applet_resource.clone());
                    up.activation.set_hid_core(hid_core.clone());
                }
                // Wire keyboard
                if let Some(ref keyboard) = self.keyboard {
                    let mut kb = keyboard.lock();
                    kb.activation.set_applet_resource(applet_resource.clone());
                    kb.activation.set_hid_core(hid_core.clone());
                }
                // Wire npad externals
                if let Some(ref npad) = self.npad {
                    let mut n = npad.lock();
                    n.set_npad_externals(AppletResourceHolder {
                        applet_resource: Some(applet_resource.clone()),
                        handheld_config: self.handheld_config.clone(),
                    });
                }
                // Wire six_axis
                if let Some(ref six_axis) = self.six_axis {
                    let mut sa = six_axis.lock();
                    sa.activation.set_applet_resource(applet_resource.clone());
                    sa.activation.set_hid_core(hid_core.clone());
                }
                // Wire mouse
                if let Some(ref mouse) = self.mouse {
                    let mut m = mouse.lock();
                    m.activation.set_applet_resource(applet_resource.clone());
                    m.activation.set_hid_core(hid_core.clone());
                }
                // Wire debug_mouse
                if let Some(ref debug_mouse) = self.debug_mouse {
                    let mut dm = debug_mouse.lock();
                    dm.activation.set_applet_resource(applet_resource.clone());
                    dm.activation.set_hid_core(hid_core.clone());
                }
                // Wire home_button
                if let Some(ref home_button) = self.home_button {
                    let mut hb = home_button.lock();
                    hb.activation.set_applet_resource(applet_resource.clone());
                    hb.activation.set_hid_core(hid_core.clone());
                }
                // Wire sleep_button
                if let Some(ref sleep_button) = self.sleep_button {
                    let mut sb = sleep_button.lock();
                    sb.activation.set_applet_resource(applet_resource.clone());
                    sb.activation.set_hid_core(hid_core.clone());
                }
                // Wire capture_button
                if let Some(ref capture_button) = self.capture_button {
                    let mut cb = capture_button.lock();
                    cb.activation.set_applet_resource(applet_resource.clone());
                    cb.activation.set_hid_core(hid_core.clone());
                }
                // Wire palma
                if let Some(ref palma) = self.palma {
                    let mut p = palma.lock();
                    p.activation.set_applet_resource(applet_resource.clone());
                    p.activation.set_hid_core(hid_core.clone());
                }
            }
        }

        // Upstream schedules looping timing events here:
        // system.CoreTiming().ScheduleLoopingEvent(npad_update_ns, npad_update_ns, npad_update_event);
        // system.CoreTiming().ScheduleLoopingEvent(default_update_ns, default_update_ns, default_update_event);
        // etc.
        // CoreTiming integration would schedule these callbacks to call
        // update_controllers/update_npad/update_mouse_keyboard/update_motion periodically.
    }

    fn initialize_touch_screen_sampler(&mut self) {
        // This is nn.hid.TouchScreenSampler
        self.touch_resource = Some(Arc::new(Mutex::new(TouchResource::new())));
        self.touch_driver = self
            .hid_core
            .as_ref()
            .map(|hid_core| Arc::new(Mutex::new(TouchScreenDriver::new(Arc::clone(hid_core)))));
        self.touch_screen = Some(Arc::new(Mutex::new(TouchScreen::new())));
        self.gesture = Some(Arc::new(Mutex::new(Gesture::new())));

        if let Some(ref touch_resource) = self.touch_resource {
            let mut resource = touch_resource.lock();
            if let Some(ref applet_resource) = self.applet_resource {
                resource.set_applet_resource(applet_resource.clone());
            }
            if let Some(ref handheld_config) = self.handheld_config {
                resource.set_handheld_config(handheld_config.clone());
            }
        }
    }

    fn initialize_console_six_axis_sampler(&mut self) {
        let Some(hid_core) = self.hid_core.as_ref().cloned() else {
            return;
        };
        self.console_six_axis = Some(Arc::new(Mutex::new(ConsoleSixAxis::new(hid_core))));
        self.seven_six_axis = Some(Arc::new(Mutex::new(SevenSixAxis::new())));

        // Wire console_six_axis to applet_resource
        if let Some(ref applet_resource) = self.applet_resource {
            if let Some(ref console_six_axis) = self.console_six_axis {
                console_six_axis
                    .lock()
                    .activation
                    .set_applet_resource(applet_resource.clone());
            }
            if let (Some(hid_core), Some(seven_six_axis)) =
                (self.hid_core.as_ref(), self.seven_six_axis.as_ref())
            {
                seven_six_axis
                    .lock()
                    .activation
                    .set_hid_core(hid_core.clone());
            }
        }
    }

    fn initialize_ahid_sampler(&mut self) {
        // Upstream TODO: not yet implemented in C++ upstream
    }
}

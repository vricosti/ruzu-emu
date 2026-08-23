//! Port of Eden src/core/hle/service/hid/hid_debug_server.h and hid_debug_server.cpp
//!
//! IHidDebugServer service ("hid:dbg").

use std::collections::BTreeMap;
use std::sync::Arc;

use hid_core::hid_types::{TouchScreenConfigurationForNx, TouchScreenModeForNx};
use hid_core::resource_manager::ResourceManager;
use hid_core::resources::hid_firmware_settings::HidFirmwareSettings;
use hid_core::resources::touch_screen::touch_types::{AutoPilotState, TouchState};

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::cmif_serialization::{CmifInArrayBuffer, CmifRequest, CmifResponse};
use crate::hle::service::cmif_types::buffer_attr;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

#[inline]
fn to_ipc_result(result: common::ResultCode) -> ResultCode {
    ResultCode::new(result.raw())
}

/// IHidDebugServer - debug interface for HID.
pub struct IHidDebugServer {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    resource_manager: Arc<parking_lot::Mutex<ResourceManager>>,
    firmware_settings: Arc<HidFirmwareSettings>,
}

impl IHidDebugServer {
    /// Recover the concrete CRTP-equivalent service from the dispatcher trait object.
    fn as_self(this: &dyn ServiceFramework) -> &Self {
        // Handler callbacks are registered only in this concrete service's map.
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    fn get_resource_manager(&self) -> parking_lot::MutexGuard<'_, ResourceManager> {
        let mut resource_manager = self.resource_manager.lock();
        resource_manager.initialize();
        resource_manager
    }

    fn deactivate_touch_screen(&self) -> ResultCode {
        log::info!("IHidDebugServer::DeactivateTouchScreen called");
        if self.firmware_settings.is_device_managed() {
            return RESULT_SUCCESS;
        }

        let resource_manager = self.get_resource_manager();
        let touch_screen = resource_manager
            .get_touch_screen()
            .expect("initialized HID resource manager must own TouchScreen");
        let touch_resource = resource_manager
            .get_touch_resource()
            .expect("initialized HID resource manager must own TouchResource");
        let touch_driver = resource_manager
            .get_touch_driver()
            .expect("initialized HID resource manager must own TouchScreenDriver");
        let result = touch_screen
            .lock()
            .deactivate(&mut touch_resource.lock(), &mut touch_driver.lock());
        to_ipc_result(result)
    }

    fn set_touch_screen_auto_pilot_state(&self, states: &[TouchState]) -> ResultCode {
        let mut auto_pilot = AutoPilotState::default();
        let count = states.len().min(auto_pilot.state.len());
        auto_pilot.count = count as u64;
        auto_pilot.state[..count].copy_from_slice(&states[..count]);
        log::info!(
            "IHidDebugServer::SetTouchScreenAutoPilotState called, auto_pilot_count={}",
            auto_pilot.count
        );

        let resource_manager = self.get_resource_manager();
        let touch_screen = resource_manager
            .get_touch_screen()
            .expect("initialized HID resource manager must own TouchScreen");
        let touch_resource = resource_manager
            .get_touch_resource()
            .expect("initialized HID resource manager must own TouchResource");
        let result = touch_screen
            .lock()
            .set_touch_screen_auto_pilot_state(&mut touch_resource.lock(), &auto_pilot);
        to_ipc_result(result)
    }

    fn unset_touch_screen_auto_pilot_state(&self) -> ResultCode {
        log::info!("IHidDebugServer::UnsetTouchScreenAutoPilotState called");
        let resource_manager = self.get_resource_manager();
        let touch_screen = resource_manager
            .get_touch_screen()
            .expect("initialized HID resource manager must own TouchScreen");
        let touch_resource = resource_manager
            .get_touch_resource()
            .expect("initialized HID resource manager must own TouchResource");
        let result = touch_screen
            .lock()
            .unset_touch_screen_auto_pilot_state(&mut touch_resource.lock());
        to_ipc_result(result)
    }

    fn get_touch_screen_configuration(
        &self,
        aruid: u64,
    ) -> Result<TouchScreenConfigurationForNx, ResultCode> {
        log::info!(
            "IHidDebugServer::GetTouchScreenConfiguration called, applet_resource_user_id={}",
            aruid
        );
        let resource_manager = self.get_resource_manager();
        let touch_screen = resource_manager
            .get_touch_screen()
            .expect("initialized HID resource manager must own TouchScreen");
        let touch_resource = resource_manager
            .get_touch_resource()
            .expect("initialized HID resource manager must own TouchResource");
        let (result, mut configuration) = touch_screen
            .lock()
            .get_touch_screen_configuration(&touch_resource.lock(), aruid);
        if result.is_error() {
            return Err(to_ipc_result(result));
        }
        if configuration.mode != TouchScreenModeForNx::Heat2
            && configuration.mode != TouchScreenModeForNx::Finger
        {
            configuration.mode = TouchScreenModeForNx::UseSystemSetting;
        }
        Ok(configuration)
    }

    fn process_touch_screen_auto_tune(&self) -> ResultCode {
        log::info!("IHidDebugServer::ProcessTouchScreenAutoTune called");
        let resource_manager = self.get_resource_manager();
        let touch_screen = resource_manager
            .get_touch_screen()
            .expect("initialized HID resource manager must own TouchScreen");
        let touch_resource = resource_manager
            .get_touch_resource()
            .expect("initialized HID resource manager must own TouchResource");
        let touch_driver = resource_manager
            .get_touch_driver()
            .expect("initialized HID resource manager must own TouchScreenDriver");
        let result = touch_screen
            .lock()
            .process_touch_screen_auto_tune(&touch_resource.lock(), &touch_driver.lock());
        to_ipc_result(result)
    }

    fn force_stop_touch_screen_management(&self) -> ResultCode {
        log::info!("IHidDebugServer::ForceStopTouchScreenManagement called");
        if !self.firmware_settings.is_device_managed() {
            return RESULT_SUCCESS;
        }

        let resource_manager = self.get_resource_manager();
        let touch_screen = resource_manager
            .get_touch_screen()
            .expect("initialized HID resource manager must own TouchScreen");
        let gesture = resource_manager
            .get_gesture()
            .expect("initialized HID resource manager must own Gesture");
        let touch_resource = resource_manager
            .get_touch_resource()
            .expect("initialized HID resource manager must own TouchResource");
        let touch_driver = resource_manager
            .get_touch_driver()
            .expect("initialized HID resource manager must own TouchScreenDriver");

        if self.firmware_settings.is_touch_i2c_managed() {
            let mut touch_resource = touch_resource.lock();
            let mut touch_driver = touch_driver.lock();
            let is_touch_active = touch_screen.lock().is_active(&touch_resource);
            let is_gesture_active = gesture.lock().is_active(&touch_resource);
            if is_touch_active {
                let result = touch_screen
                    .lock()
                    .deactivate(&mut touch_resource, &mut touch_driver);
                if result.is_error() {
                    return to_ipc_result(result);
                }
            }
            if is_gesture_active {
                let result = gesture
                    .lock()
                    .deactivate(&mut touch_resource, &mut touch_driver);
                if result.is_error() {
                    return to_ipc_result(result);
                }
            }
        }
        RESULT_SUCCESS
    }

    fn force_restart_touch_screen_management(
        &self,
        basic_gesture_id: u32,
        aruid: u64,
    ) -> ResultCode {
        log::info!(
            "IHidDebugServer::ForceRestartTouchScreenManagement called, basic_gesture_id={}, applet_resource_user_id={}",
            basic_gesture_id,
            aruid
        );
        let resource_manager = self.get_resource_manager();
        let touch_screen = resource_manager
            .get_touch_screen()
            .expect("initialized HID resource manager must own TouchScreen");
        let gesture = resource_manager
            .get_gesture()
            .expect("initialized HID resource manager must own Gesture");

        if !self.firmware_settings.is_device_managed()
            || !self.firmware_settings.is_touch_i2c_managed()
        {
            return RESULT_SUCCESS;
        }

        let touch_resource = resource_manager
            .get_touch_resource()
            .expect("initialized HID resource manager must own TouchResource");
        let touch_driver = resource_manager
            .get_touch_driver()
            .expect("initialized HID resource manager must own TouchScreenDriver");
        let mut touch_resource = touch_resource.lock();
        let mut touch_driver = touch_driver.lock();

        let result = gesture
            .lock()
            .activate(&mut touch_resource, &mut touch_driver);
        if result.is_error() {
            return to_ipc_result(result);
        }
        let result =
            gesture
                .lock()
                .activate_with_aruid(&mut touch_resource, aruid, basic_gesture_id);
        if result.is_error() {
            return to_ipc_result(result);
        }
        let result = touch_screen
            .lock()
            .activate(&mut touch_resource, &mut touch_driver);
        if result.is_error() {
            return to_ipc_result(result);
        }
        let result = touch_screen
            .lock()
            .activate_with_aruid(&mut touch_resource, aruid);
        to_ipc_result(result)
    }

    fn is_touch_screen_managed(&self) -> Result<bool, ResultCode> {
        log::info!("IHidDebugServer::IsTouchScreenManaged called");
        let resource_manager = self.get_resource_manager();
        let touch_screen = resource_manager
            .get_touch_screen()
            .expect("initialized HID resource manager must own TouchScreen");
        let gesture = resource_manager
            .get_gesture()
            .expect("initialized HID resource manager must own Gesture");
        let touch_resource = resource_manager
            .get_touch_resource()
            .expect("initialized HID resource manager must own TouchResource");
        let touch_resource = touch_resource.lock();
        let is_touch_active = touch_screen.lock().is_active(&touch_resource);
        let is_gesture_active = gesture.lock().is_active(&touch_resource);
        Ok(is_touch_active || is_gesture_active)
    }

    fn deactivate_gesture(&self) -> ResultCode {
        log::info!("IHidDebugServer::DeactivateGesture called");
        if self.firmware_settings.is_device_managed() {
            return RESULT_SUCCESS;
        }

        let resource_manager = self.get_resource_manager();
        let gesture = resource_manager
            .get_gesture()
            .expect("initialized HID resource manager must own Gesture");
        let touch_resource = resource_manager
            .get_touch_resource()
            .expect("initialized HID resource manager must own TouchResource");
        let touch_driver = resource_manager
            .get_touch_driver()
            .expect("initialized HID resource manager must own TouchScreenDriver");
        let result = gesture
            .lock()
            .deactivate(&mut touch_resource.lock(), &mut touch_driver.lock());
        to_ipc_result(result)
    }

    fn deactivate_touch_screen_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(service.deactivate_touch_screen());
    }

    fn set_touch_screen_auto_pilot_state_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut storage =
            CmifInArrayBuffer::<TouchState, { buffer_attr::BufferAttr_HipcMapAlias }>::from_ctx(
                ctx, 0,
            );
        let states = storage.as_in_array();
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(service.set_touch_screen_auto_pilot_state(states.as_slice()));
    }

    fn unset_touch_screen_auto_pilot_state_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(service.unset_touch_screen_auto_pilot_state());
    }

    fn get_touch_screen_configuration_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut request = CmifRequest::new(ctx);
        let aruid = request.u64();
        match service.get_touch_screen_configuration(aruid) {
            Ok(configuration) => {
                let mut response = CmifResponse::new(ctx, 6, 0, 0);
                response.push_result(RESULT_SUCCESS);
                response.push_raw(&configuration);
            }
            Err(result) => {
                let mut response = CmifResponse::new(ctx, 2, 0, 0);
                response.push_result(result);
            }
        }
    }

    fn process_touch_screen_auto_tune_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(service.process_touch_screen_auto_tune());
    }

    fn force_stop_touch_screen_management_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(service.force_stop_touch_screen_management());
    }

    fn force_restart_touch_screen_management_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut request = CmifRequest::new(ctx);
        let basic_gesture_id = request.u32();
        request.align_for::<u64>();
        let aruid = request.u64();
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response
            .push_result(service.force_restart_touch_screen_management(basic_gesture_id, aruid));
    }

    fn is_touch_screen_managed_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        match service.is_touch_screen_managed() {
            Ok(is_managed) => {
                let mut response = CmifResponse::new(ctx, 3, 0, 0);
                response.push_result(RESULT_SUCCESS);
                response.push_bool(is_managed);
            }
            Err(result) => {
                let mut response = CmifResponse::new(ctx, 2, 0, 0);
                response.push_result(result);
            }
        }
    }

    fn deactivate_gesture_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(service.deactivate_gesture());
    }

    pub fn new(
        resource_manager: Arc<parking_lot::Mutex<ResourceManager>>,
        firmware_settings: Arc<HidFirmwareSettings>,
    ) -> Self {
        // clang-format off
        let handlers = build_handler_map(&[
            (0, None, "DeactivateDebugPad"),
            (1, None, "SetDebugPadAutoPilotState"),
            (2, None, "UnsetDebugPadAutoPilotState"),
            (
                10,
                Some(Self::deactivate_touch_screen_handler),
                "DeactivateTouchScreen",
            ),
            (
                11,
                Some(Self::set_touch_screen_auto_pilot_state_handler),
                "SetTouchScreenAutoPilotState",
            ),
            (
                12,
                Some(Self::unset_touch_screen_auto_pilot_state_handler),
                "UnsetTouchScreenAutoPilotState",
            ),
            (
                13,
                Some(Self::get_touch_screen_configuration_handler),
                "GetTouchScreenConfiguration",
            ),
            (
                14,
                Some(Self::process_touch_screen_auto_tune_handler),
                "ProcessTouchScreenAutoTune",
            ),
            (
                15,
                Some(Self::force_stop_touch_screen_management_handler),
                "ForceStopTouchScreenManagement",
            ),
            (
                16,
                Some(Self::force_restart_touch_screen_management_handler),
                "ForceRestartTouchScreenManagement",
            ),
            (
                17,
                Some(Self::is_touch_screen_managed_handler),
                "IsTouchScreenManaged",
            ),
            (20, None, "DeactivateMouse"),
            (21, None, "SetMouseAutoPilotState"),
            (22, None, "UnsetMouseAutoPilotState"),
            (23, None, "AddMouseSideWheelDelta"),
            (25, None, "SetDebugMouseAutoPilotState"),
            (26, None, "UnsetDebugMouseAutoPilotState"),
            (30, None, "DeactivateKeyboard"),
            (31, None, "SetKeyboardAutoPilotState"),
            (32, None, "UnsetKeyboardAutoPilotState"),
            (50, None, "DeactivateXpad"),
            (51, None, "SetXpadAutoPilotState"),
            (52, None, "UnsetXpadAutoPilotState"),
            (53, None, "DeactivateJoyXpad"),
            (60, None, "ClearNpadSystemCommonPolicy"),
            (61, None, "DeactivateNpad"),
            (62, None, "ForceDisconnectNpad"),
            (
                91,
                Some(Self::deactivate_gesture_handler),
                "DeactivateGesture",
            ),
            (110, None, "DeactivateHomeButton"),
            (111, None, "SetHomeButtonAutoPilotState"),
            (112, None, "UnsetHomeButtonAutoPilotState"),
            (120, None, "DeactivateSleepButton"),
            (121, None, "SetSleepButtonAutoPilotState"),
            (122, None, "UnsetSleepButtonAutoPilotState"),
            (123, None, "DeactivateInputDetector"),
            (130, None, "DeactivateCaptureButton"),
            (131, None, "SetCaptureButtonAutoPilotState"),
            (132, None, "UnsetCaptureButtonAutoPilotState"),
            (133, None, "SetShiftAccelerometerCalibrationValue"),
            (134, None, "GetShiftAccelerometerCalibrationValue"),
            (135, None, "SetShiftGyroscopeCalibrationValue"),
            (136, None, "GetShiftGyroscopeCalibrationValue"),
            (140, None, "DeactivateConsoleSixAxisSensor"),
            (141, None, "GetConsoleSixAxisSensorSamplingFrequency"),
            (142, None, "DeactivateSevenSixAxisSensor"),
            (143, None, "GetConsoleSixAxisSensorCountStates"),
            (144, None, "GetAccelerometerFsr"),
            (145, None, "SetAccelerometerFsr"),
            (146, None, "GetAccelerometerOdr"),
            (147, None, "SetAccelerometerOdr"),
            (148, None, "GetGyroscopeFsr"),
            (149, None, "SetGyroscopeFsr"),
            (150, None, "GetGyroscopeOdr"),
            (151, None, "SetGyroscopeOdr"),
            (152, None, "GetWhoAmI"),
            (201, None, "ActivateFirmwareUpdate"),
            (202, None, "DeactivateFirmwareUpdate"),
            (203, None, "StartFirmwareUpdate"),
            (204, None, "GetFirmwareUpdateStage"),
            (205, None, "GetFirmwareVersion"),
            (206, None, "GetDestinationFirmwareVersion"),
            (207, None, "DiscardFirmwareInfoCacheForRevert"),
            (208, None, "StartFirmwareUpdateForRevert"),
            (209, None, "GetAvailableFirmwareVersionForRevert"),
            (210, None, "IsFirmwareUpdatingDevice"),
            (211, None, "StartFirmwareUpdateIndividual"),
            (212, None, "GetDetailFirmwareVersion"),
            (215, None, "SetUsbFirmwareForceUpdateEnabled"),
            (216, None, "SetAllKuinaDevicesToFirmwareUpdateMode"),
            (221, None, "UpdateControllerColor"),
            (222, None, "ConnectUsbPadsAsync"),
            (223, None, "DisconnectUsbPadsAsync"),
            (224, None, "UpdateDesignInfo"),
            (225, None, "GetUniquePadDriverState"),
            (226, None, "GetSixAxisSensorDriverStates"),
            (227, None, "GetRxPacketHistory"),
            (228, None, "AcquireOperationEventHandle"),
            (229, None, "ReadSerialFlash"),
            (230, None, "WriteSerialFlash"),
            (231, None, "GetOperationResult"),
            (232, None, "EnableShipmentMode"),
            (233, None, "ClearPairingInfo"),
            (234, None, "GetUniquePadDeviceTypeSetInternal"),
            (235, None, "EnableAnalogStickPower"),
            (236, None, "RequestKuinaUartClockCal"),
            (237, None, "GetKuinaUartClockCal"),
            (238, None, "SetKuinaUartClockTrim"),
            (239, None, "KuinaLoopbackTest"),
            (240, None, "RequestBatteryVoltage"),
            (241, None, "GetBatteryVoltage"),
            (242, None, "GetUniquePadPowerInfo"),
            (243, None, "RebootUniquePad"),
            (244, None, "RequestKuinaFirmwareVersion"),
            (245, None, "GetKuinaFirmwareVersion"),
            (246, None, "GetVidPid"),
            (247, None, "GetAnalogStickCalibrationValue"),
            (248, None, "GetUniquePadIdsFull"),
            (249, None, "ConnectUniquePad"),
            (250, None, "IsVirtual"),
            (251, None, "GetAnalogStickModuleParam"),
            (253, None, "ClearStorageForShipment"),
            (261, None, "UpdateDesignInfo12"),
            (262, None, "GetUniquePadButtonCount"),
            (267, None, "SetAnalogStickCalibration"),
            (268, None, "ResetAnalogStickCalibration"),
            (301, None, "GetAbstractedPadHandles"),
            (302, None, "GetAbstractedPadState"),
            (303, None, "GetAbstractedPadsState"),
            (321, None, "SetAutoPilotVirtualPadState"),
            (322, None, "UnsetAutoPilotVirtualPadState"),
            (323, None, "UnsetAllAutoPilotVirtualPadState"),
            (324, None, "AttachHdlsWorkBuffer"),
            (325, None, "ReleaseHdlsWorkBuffer"),
            (326, None, "DumpHdlsNpadAssignmentState"),
            (327, None, "DumpHdlsStates"),
            (328, None, "ApplyHdlsNpadAssignmentState"),
            (329, None, "ApplyHdlsStateList"),
            (330, None, "AttachHdlsVirtualDevice"),
            (331, None, "DetachHdlsVirtualDevice"),
            (332, None, "SetHdlsState"),
            (350, None, "AddRegisteredDevice"),
            (351, None, "GetRegisteredDevicesCountDebug"),
            (352, None, "DeleteRegisteredDevicesDebug"),
            (400, None, "DisableExternalMcuOnNxDevice"),
            (401, None, "DisableRailDeviceFiltering"),
            (402, None, "EnableWiredPairing"),
            (403, None, "EnableShipmentModeAutoClear"),
            (404, None, "SetRailEnabled"),
            (500, None, "SetFactoryInt"),
            (501, None, "IsFactoryBootEnabled"),
            (550, None, "SetAnalogStickModelDataTemporarily"),
            (551, None, "GetAnalogStickModelData"),
            (552, None, "ResetAnalogStickModelData"),
            (600, None, "ConvertPadState"),
            (601, None, "IsButtonConfigSupported"),
            (602, None, "IsButtonConfigEmbeddedSupported"),
            (603, None, "DeleteButtonConfig"),
            (604, None, "DeleteButtonConfigEmbedded"),
            (605, None, "SetButtonConfigEnabled"),
            (606, None, "SetButtonConfigEmbeddedEnabled"),
            (607, None, "IsButtonConfigEnabled"),
            (608, None, "IsButtonConfigEmbeddedEnabled"),
            (609, None, "SetButtonConfigEmbedded"),
            (610, None, "SetButtonConfigFull"),
            (611, None, "SetButtonConfigLeft"),
            (612, None, "SetButtonConfigRight"),
            (613, None, "GetButtonConfigEmbedded"),
            (614, None, "GetButtonConfigFull"),
            (615, None, "GetButtonConfigLeft"),
            (616, None, "GetButtonConfigRight"),
            (650, None, "AddButtonPlayData"),
            (651, None, "StartButtonPlayData"),
            (652, None, "StopButtonPlayData"),
            (700, None, "GetRailAttachEventCount"),
            (2000, None, "DeactivateDigitizer"),
            (2001, None, "SetDigitizerAutoPilotState"),
            (2002, None, "UnsetDigitizerAutoPilotState"),
            (3000, None, "ReloadFirmwareDebugSettings"),
        ]);
        // clang-format on

        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
            resource_manager,
            firmware_settings,
        }
    }
}

impl SessionRequestHandler for IHidDebugServer {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "hid:dbg"
    }
}

impl ServiceFramework for IHidDebugServer {
    fn get_service_name(&self) -> &str {
        "hid:dbg"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hid_core::hid_core::HIDCore;

    fn make_server() -> IHidDebugServer {
        let firmware_settings = Arc::new(HidFirmwareSettings::new());
        let hid_core = Arc::new(parking_lot::Mutex::new(HIDCore::new()));
        let resource_manager = Arc::new(parking_lot::Mutex::new(ResourceManager::new(
            Arc::clone(&firmware_settings),
            hid_core,
        )));
        IHidDebugServer::new(resource_manager, firmware_settings)
    }

    #[test]
    fn command_table_matches_upstream_ids_and_implemented_handlers() {
        let server = make_server();
        let expected_ids = [
            0, 1, 2, 10, 11, 12, 13, 14, 15, 16, 17, 20, 21, 22, 23, 25, 26, 30, 31, 32, 50, 51,
            52, 53, 60, 61, 62, 91, 110, 111, 112, 120, 121, 122, 123, 130, 131, 132, 133, 134,
            135, 136, 140, 141, 142, 143, 144, 145, 146, 147, 148, 149, 150, 151, 152, 201, 202,
            203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 215, 216, 221, 222, 223, 224, 225,
            226, 227, 228, 229, 230, 231, 232, 233, 234, 235, 236, 237, 238, 239, 240, 241, 242,
            243, 244, 245, 246, 247, 248, 249, 250, 251, 253, 261, 262, 267, 268, 301, 302, 303,
            321, 322, 323, 324, 325, 326, 327, 328, 329, 330, 331, 332, 350, 351, 352, 400, 401,
            402, 403, 404, 500, 501, 550, 551, 552, 600, 601, 602, 603, 604, 605, 606, 607, 608,
            609, 610, 611, 612, 613, 614, 615, 616, 650, 651, 652, 700, 2000, 2001, 2002, 3000,
        ];
        let actual_ids = server.handlers.keys().copied().collect::<Vec<_>>();
        assert_eq!(actual_ids, expected_ids);

        let implemented_ids = server
            .handlers
            .iter()
            .filter_map(|(id, info)| info.handler_callback.map(|_| *id))
            .collect::<Vec<_>>();
        assert_eq!(implemented_ids, [10, 11, 12, 13, 14, 15, 16, 17, 91]);
        assert_eq!(server.handlers[&2002].name, "UnsetDigitizerAutoPilotState");
        assert_eq!(server.handlers[&3000].name, "ReloadFirmwareDebugSettings");
    }

    #[test]
    fn force_restart_and_stop_preserve_upstream_activation_order_contract() {
        let server = make_server();
        assert_eq!(server.is_touch_screen_managed(), Ok(false));

        assert_eq!(
            server.force_restart_touch_screen_management(7, 0x1234),
            RESULT_SUCCESS
        );
        assert_eq!(server.is_touch_screen_managed(), Ok(true));

        assert_eq!(server.force_stop_touch_screen_management(), RESULT_SUCCESS);
        assert_eq!(server.is_touch_screen_managed(), Ok(false));
    }

    #[test]
    fn cmif_payload_layouts_match_upstream() {
        assert_eq!(std::mem::size_of::<TouchState>(), 0x28);
        assert_eq!(std::mem::size_of::<AutoPilotState>(), 0x288);
        assert_eq!(std::mem::size_of::<TouchScreenConfigurationForNx>(), 0x10);
    }
}

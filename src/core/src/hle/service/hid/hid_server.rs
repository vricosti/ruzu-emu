//! Port of zuyu/src/core/hle/service/hid/hid_server.h and hid_server.cpp
//!
//! IHidServer service ("hid").

use std::collections::BTreeMap;
use std::sync::Arc;

use hid_core::hid_types::*;
use hid_core::resource_manager::ResourceManager;
use hid_core::resources::hid_firmware_settings::HidFirmwareSettings;
use hid_core::resources::npad::npad_types::*;

use crate::core::SystemRef;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::os::event::Event;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// Convert `common::ResultCode` (used by hid_core) to `crate::hle::result::ResultCode`
/// (used by the service IPC layer). Both are u32 newtypes with identical layout.
#[inline]
fn to_ipc_result(r: common::ResultCode) -> ResultCode {
    ResultCode::new(r.raw())
}

pub struct IHidServer {
    system: SystemRef,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    resource_manager: Arc<parking_lot::Mutex<ResourceManager>>,
    firmware_settings: Arc<HidFirmwareSettings>,
    npad_style_set_events: NpadStyleSetEvents,
}

pub type NpadStyleSetEvents = Arc<parking_lot::Mutex<BTreeMap<(u64, u32), Arc<Event>>>>;

impl IHidServer {
    fn trace_npad(cmd: u32, result: ResultCode, aruid: u64, arg0: u64, arg1: u64, out0: u64) {
        if !common::trace::is_enabled(common::trace::cat::HID_NPAD) {
            return;
        }
        common::trace::emit_raw(
            common::trace::cat::HID_NPAD,
            &[
                cmd as u64,
                result.get_inner_value() as u64,
                aruid,
                arg0,
                arg1,
                out0,
                0,
                0,
            ],
        );
    }

    pub fn new(
        system: SystemRef,
        resource_manager: Arc<parking_lot::Mutex<ResourceManager>>,
        firmware_settings: Arc<HidFirmwareSettings>,
        npad_style_set_events: NpadStyleSetEvents,
    ) -> Self {
        // clang-format off
        let handlers = build_handler_map(&[
            (
                0,
                Some(Self::create_applet_resource),
                "CreateAppletResource",
            ),
            (1, Some(Self::activate_debug_pad), "ActivateDebugPad"),
            (11, Some(Self::activate_touch_screen), "ActivateTouchScreen"),
            (21, Some(Self::activate_mouse), "ActivateMouse"),
            (26, Some(Self::activate_debug_mouse), "ActivateDebugMouse"),
            (31, Some(Self::activate_keyboard), "ActivateKeyboard"),
            (
                32,
                Some(Self::send_keyboard_lock_key_event),
                "SendKeyboardLockKeyEvent",
            ),
            (
                40,
                Some(Self::acquire_xpad_id_event_handle),
                "AcquireXpadIdEventHandle",
            ),
            (
                41,
                Some(Self::release_xpad_id_event_handle),
                "ReleaseXpadIdEventHandle",
            ),
            (51, Some(Self::activate_xpad), "ActivateXpad"),
            (55, Some(Self::get_xpad_ids), "GetXpadIds"),
            (56, Some(Self::activate_joy_xpad), "ActivateJoyXpad"),
            (
                58,
                Some(Self::get_joy_xpad_lifo_handle),
                "GetJoyXpadLifoHandle",
            ),
            (59, Some(Self::get_joy_xpad_ids), "GetJoyXpadIds"),
            (
                60,
                Some(Self::activate_six_axis_sensor),
                "ActivateSixAxisSensor",
            ),
            (
                61,
                Some(Self::deactivate_six_axis_sensor),
                "DeactivateSixAxisSensor",
            ),
            (
                62,
                Some(Self::get_six_axis_sensor_lifo_handle),
                "GetSixAxisSensorLifoHandle",
            ),
            (
                63,
                Some(Self::activate_joy_six_axis_sensor),
                "ActivateJoySixAxisSensor",
            ),
            (
                64,
                Some(Self::deactivate_joy_six_axis_sensor),
                "DeactivateJoySixAxisSensor",
            ),
            (
                65,
                Some(Self::get_joy_six_axis_sensor_lifo_handle),
                "GetJoySixAxisSensorLifoHandle",
            ),
            (66, Some(Self::start_six_axis_sensor), "StartSixAxisSensor"),
            (67, Some(Self::stop_six_axis_sensor), "StopSixAxisSensor"),
            (
                68,
                Some(Self::is_six_axis_sensor_fusion_enabled),
                "IsSixAxisSensorFusionEnabled",
            ),
            (
                69,
                Some(Self::enable_six_axis_sensor_fusion),
                "EnableSixAxisSensorFusion",
            ),
            (
                70,
                Some(Self::set_six_axis_sensor_fusion_parameters),
                "SetSixAxisSensorFusionParameters",
            ),
            (
                71,
                Some(Self::get_six_axis_sensor_fusion_parameters),
                "GetSixAxisSensorFusionParameters",
            ),
            (
                72,
                Some(Self::reset_six_axis_sensor_fusion_parameters),
                "ResetSixAxisSensorFusionParameters",
            ),
            (
                73,
                Some(Self::stub_success_handler),
                "SetAccelerometerParameters",
            ),
            (
                74,
                Some(Self::stub_success_handler),
                "GetAccelerometerParameters",
            ),
            (
                75,
                Some(Self::stub_success_handler),
                "ResetAccelerometerParameters",
            ),
            (
                76,
                Some(Self::stub_success_handler),
                "SetAccelerometerPlayMode",
            ),
            (
                77,
                Some(Self::stub_success_handler),
                "GetAccelerometerPlayMode",
            ),
            (
                78,
                Some(Self::stub_success_handler),
                "ResetAccelerometerPlayMode",
            ),
            (
                79,
                Some(Self::set_gyroscope_zero_drift_mode),
                "SetGyroscopeZeroDriftMode",
            ),
            (
                80,
                Some(Self::get_gyroscope_zero_drift_mode),
                "GetGyroscopeZeroDriftMode",
            ),
            (
                81,
                Some(Self::reset_gyroscope_zero_drift_mode),
                "ResetGyroscopeZeroDriftMode",
            ),
            (
                82,
                Some(Self::is_six_axis_sensor_at_rest),
                "IsSixAxisSensorAtRest",
            ),
            (
                83,
                Some(Self::is_firmware_update_available_for_six_axis_sensor),
                "IsFirmwareUpdateAvailableForSixAxisSensor",
            ),
            (
                84,
                Some(Self::enable_six_axis_sensor_unaltered_passthrough),
                "EnableSixAxisSensorUnalteredPassthrough",
            ),
            (
                85,
                Some(Self::is_six_axis_sensor_unaltered_passthrough_enabled),
                "IsSixAxisSensorUnalteredPassthroughEnabled",
            ),
            (
                86,
                Some(Self::stub_success_handler),
                "StoreSixAxisSensorCalibrationParameter",
            ),
            (
                87,
                Some(Self::load_six_axis_sensor_calibration_parameter),
                "LoadSixAxisSensorCalibrationParameter",
            ),
            (
                88,
                Some(Self::get_six_axis_sensor_ic_information),
                "GetSixAxisSensorIcInformation",
            ),
            (
                89,
                Some(Self::reset_is_six_axis_sensor_device_newly_assigned),
                "ResetIsSixAxisSensorDeviceNewlyAssigned",
            ),
            (91, Some(Self::activate_gesture), "ActivateGesture"),
            (
                100,
                Some(Self::set_supported_npad_style_set),
                "SetSupportedNpadStyleSet",
            ),
            (
                101,
                Some(Self::get_supported_npad_style_set),
                "GetSupportedNpadStyleSet",
            ),
            (
                102,
                Some(Self::set_supported_npad_id_type),
                "SetSupportedNpadIdType",
            ),
            (103, Some(Self::activate_npad), "ActivateNpad"),
            (104, Some(Self::deactivate_npad), "DeactivateNpad"),
            (
                106,
                Some(Self::acquire_npad_style_set_update_event_handle),
                "AcquireNpadStyleSetUpdateEventHandle",
            ),
            (107, Some(Self::disconnect_npad), "DisconnectNpad"),
            (
                108,
                Some(Self::get_player_led_pattern),
                "GetPlayerLedPattern",
            ),
            (
                109,
                Some(Self::activate_npad_with_revision),
                "ActivateNpadWithRevision",
            ),
            (
                120,
                Some(Self::set_npad_joy_hold_type),
                "SetNpadJoyHoldType",
            ),
            (
                121,
                Some(Self::get_npad_joy_hold_type),
                "GetNpadJoyHoldType",
            ),
            (
                122,
                Some(Self::set_npad_joy_assignment_mode_single_by_default),
                "SetNpadJoyAssignmentModeSingleByDefault",
            ),
            (
                123,
                Some(Self::set_npad_joy_assignment_mode_single),
                "SetNpadJoyAssignmentModeSingle",
            ),
            (
                124,
                Some(Self::set_npad_joy_assignment_mode_dual),
                "SetNpadJoyAssignmentModeDual",
            ),
            (
                125,
                Some(Self::merge_single_joy_as_dual_joy),
                "MergeSingleJoyAsDualJoy",
            ),
            (
                126,
                Some(Self::start_lr_assignment_mode),
                "StartLrAssignmentMode",
            ),
            (
                127,
                Some(Self::stop_lr_assignment_mode),
                "StopLrAssignmentMode",
            ),
            (
                128,
                Some(Self::set_npad_handheld_activation_mode),
                "SetNpadHandheldActivationMode",
            ),
            (
                129,
                Some(Self::get_npad_handheld_activation_mode),
                "GetNpadHandheldActivationMode",
            ),
            (130, Some(Self::swap_npad_assignment), "SwapNpadAssignment"),
            (
                131,
                Some(Self::is_unintended_home_button_input_protection_enabled),
                "IsUnintendedHomeButtonInputProtectionEnabled",
            ),
            (
                132,
                Some(Self::enable_unintended_home_button_input_protection),
                "EnableUnintendedHomeButtonInputProtection",
            ),
            (
                133,
                Some(Self::set_npad_joy_assignment_mode_single_with_destination),
                "SetNpadJoyAssignmentModeSingleWithDestination",
            ),
            (
                134,
                Some(Self::set_npad_analog_stick_use_center_clamp),
                "SetNpadAnalogStickUseCenterClamp",
            ),
            (
                135,
                Some(Self::set_npad_capture_button_assignment),
                "SetNpadCaptureButtonAssignment",
            ),
            (
                136,
                Some(Self::clear_npad_capture_button_assignment),
                "ClearNpadCaptureButtonAssignment",
            ),
            (
                200,
                Some(Self::get_vibration_device_info),
                "GetVibrationDeviceInfo",
            ),
            (201, Some(Self::send_vibration_value), "SendVibrationValue"),
            (
                202,
                Some(Self::get_actual_vibration_value),
                "GetActualVibrationValue",
            ),
            (
                203,
                Some(Self::create_active_vibration_device_list),
                "CreateActiveVibrationDeviceList",
            ),
            (204, Some(Self::permit_vibration), "PermitVibration"),
            (
                205,
                Some(Self::is_vibration_permitted),
                "IsVibrationPermitted",
            ),
            (
                206,
                Some(Self::send_vibration_values),
                "SendVibrationValues",
            ),
            (
                207,
                Some(Self::send_vibration_gc_erm_command),
                "SendVibrationGcErmCommand",
            ),
            (
                208,
                Some(Self::get_actual_vibration_gc_erm_command),
                "GetActualVibrationGcErmCommand",
            ),
            (
                209,
                Some(Self::begin_permit_vibration_session),
                "BeginPermitVibrationSession",
            ),
            (
                210,
                Some(Self::end_permit_vibration_session),
                "EndPermitVibrationSession",
            ),
            (
                211,
                Some(Self::is_vibration_device_mounted),
                "IsVibrationDeviceMounted",
            ),
            (
                212,
                Some(Self::send_vibration_value_in_bool),
                "SendVibrationValueInBool",
            ),
            (
                300,
                Some(Self::activate_console_six_axis_sensor),
                "ActivateConsoleSixAxisSensor",
            ),
            (
                301,
                Some(Self::start_console_six_axis_sensor),
                "StartConsoleSixAxisSensor",
            ),
            (
                302,
                Some(Self::stop_console_six_axis_sensor),
                "StopConsoleSixAxisSensor",
            ),
            (
                303,
                Some(Self::activate_seven_six_axis_sensor),
                "ActivateSevenSixAxisSensor",
            ),
            (
                304,
                Some(Self::start_seven_six_axis_sensor),
                "StartSevenSixAxisSensor",
            ),
            (
                305,
                Some(Self::stop_seven_six_axis_sensor),
                "StopSevenSixAxisSensor",
            ),
            (
                306,
                Some(Self::initialize_seven_six_axis_sensor),
                "InitializeSevenSixAxisSensor",
            ),
            (
                307,
                Some(Self::finalize_seven_six_axis_sensor),
                "FinalizeSevenSixAxisSensor",
            ),
            (
                308,
                Some(Self::stub_success_handler),
                "SetSevenSixAxisSensorFusionStrength",
            ),
            (
                309,
                Some(Self::stub_success_handler),
                "GetSevenSixAxisSensorFusionStrength",
            ),
            (
                310,
                Some(Self::reset_seven_six_axis_sensor_timestamp),
                "ResetSevenSixAxisSensorTimestamp",
            ),
            (
                400,
                Some(Self::is_usb_full_key_controller_enabled),
                "IsUsbFullKeyControllerEnabled",
            ),
            (
                401,
                Some(Self::stub_success_handler),
                "EnableUsbFullKeyController",
            ),
            (
                402,
                Some(Self::stub_success_handler),
                "IsUsbFullKeyControllerConnected",
            ),
            (403, Some(Self::stub_success_handler), "HasBattery"),
            (404, Some(Self::stub_success_handler), "HasLeftRightBattery"),
            (
                405,
                Some(Self::stub_success_handler),
                "GetNpadInterfaceType",
            ),
            (
                406,
                Some(Self::stub_success_handler),
                "GetNpadLeftRightInterfaceType",
            ),
            (
                407,
                Some(Self::stub_success_handler),
                "GetNpadOfHighestBatteryLevel",
            ),
            (
                408,
                Some(Self::stub_success_handler),
                "GetNpadOfHighestBatteryLevelForJoyRight",
            ),
            (
                500,
                Some(Self::get_palma_connection_handle),
                "GetPalmaConnectionHandle",
            ),
            (501, Some(Self::initialize_palma), "InitializePalma"),
            (
                502,
                Some(Self::acquire_palma_operation_complete_event),
                "AcquirePalmaOperationCompleteEvent",
            ),
            (
                503,
                Some(Self::get_palma_operation_info),
                "GetPalmaOperationInfo",
            ),
            (504, Some(Self::play_palma_activity), "PlayPalmaActivity"),
            (
                505,
                Some(Self::set_palma_fr_mode_type),
                "SetPalmaFrModeType",
            ),
            (506, Some(Self::read_palma_step), "ReadPalmaStep"),
            (507, Some(Self::enable_palma_step), "EnablePalmaStep"),
            (508, Some(Self::reset_palma_step), "ResetPalmaStep"),
            (
                509,
                Some(Self::read_palma_application_section),
                "ReadPalmaApplicationSection",
            ),
            (
                510,
                Some(Self::write_palma_application_section),
                "WritePalmaApplicationSection",
            ),
            (
                511,
                Some(Self::read_palma_unique_code),
                "ReadPalmaUniqueCode",
            ),
            (
                512,
                Some(Self::set_palma_unique_code_invalid),
                "SetPalmaUniqueCodeInvalid",
            ),
            (
                513,
                Some(Self::write_palma_activity_entry),
                "WritePalmaActivityEntry",
            ),
            (
                514,
                Some(Self::write_palma_rgb_led_pattern_entry),
                "WritePalmaRgbLedPatternEntry",
            ),
            (
                515,
                Some(Self::write_palma_wave_entry),
                "WritePalmaWaveEntry",
            ),
            (
                516,
                Some(Self::set_palma_data_base_identification_version),
                "SetPalmaDataBaseIdentificationVersion",
            ),
            (
                517,
                Some(Self::get_palma_data_base_identification_version),
                "GetPalmaDataBaseIdentificationVersion",
            ),
            (
                518,
                Some(Self::suspend_palma_feature),
                "SuspendPalmaFeature",
            ),
            (
                519,
                Some(Self::get_palma_operation_result),
                "GetPalmaOperationResult",
            ),
            (520, Some(Self::read_palma_play_log), "ReadPalmaPlayLog"),
            (521, Some(Self::reset_palma_play_log), "ResetPalmaPlayLog"),
            (
                522,
                Some(Self::set_is_palma_all_connectable),
                "SetIsPalmaAllConnectable",
            ),
            (
                523,
                Some(Self::set_is_palma_paired_connectable),
                "SetIsPalmaPairedConnectable",
            ),
            (524, Some(Self::pair_palma), "PairPalma"),
            (525, Some(Self::set_palma_boost_mode), "SetPalmaBoostMode"),
            (
                526,
                Some(Self::cancel_write_palma_wave_entry),
                "CancelWritePalmaWaveEntry",
            ),
            (
                527,
                Some(Self::enable_palma_boost_mode),
                "EnablePalmaBoostMode",
            ),
            (
                528,
                Some(Self::get_palma_bluetooth_address),
                "GetPalmaBluetoothAddress",
            ),
            (
                529,
                Some(Self::set_disallowed_palma_connection),
                "SetDisallowedPalmaConnection",
            ),
            (
                1000,
                Some(Self::set_npad_communication_mode),
                "SetNpadCommunicationMode",
            ),
            (
                1001,
                Some(Self::get_npad_communication_mode),
                "GetNpadCommunicationMode",
            ),
            (
                1002,
                Some(Self::set_touch_screen_configuration),
                "SetTouchScreenConfiguration",
            ),
            (
                1003,
                Some(Self::is_firmware_update_needed_for_notification),
                "IsFirmwareUpdateNeededForNotification",
            ),
            (
                1004,
                Some(Self::set_touch_screen_resolution),
                "SetTouchScreenResolution",
            ),
            (2000, Some(Self::activate_digitizer), "ActivateDigitizer"),
        ]);
        // clang-format on

        Self {
            system,
            handlers,
            handlers_tipc: BTreeMap::new(),
            resource_manager,
            firmware_settings,
            npad_style_set_events,
        }
    }

    /// Downcast helper: recovers `&IHidServer` from `&dyn ServiceFramework`.
    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    /// Upstream: `IHidServer::GetResourceManager()`.
    pub fn get_resource_manager(&self) -> Arc<parking_lot::Mutex<ResourceManager>> {
        self.resource_manager.clone()
    }

    fn stub_success_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) HID command {}", ctx.get_command());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 0: CreateAppletResource
    fn create_applet_resource(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();

        let result = server.resource_manager.lock().create_applet_resource(aruid);
        log::debug!(
            "IHidServer::CreateAppletResource called, applet_resource_user_id={}, result={:#x}",
            aruid,
            result.raw()
        );

        let applet_resource: Arc<dyn SessionRequestHandler> =
            Arc::new(super::applet_resource::IAppletResource::new(
                server.system,
                server.resource_manager.clone(),
                aruid,
            ));

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(applet_resource);
    }

    // cmd 1: ActivateDebugPad
    fn activate_debug_pad(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::ActivateDebugPad called, aruid={}", aruid);

        let rm = server.resource_manager.lock();
        if !server.firmware_settings.is_device_managed() {
            if let Some(ref debug_pad) = rm.get_debug_pad() {
                let (result, _) = debug_pad.lock().activation.activate();
                if result.is_error() {
                    let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                    rb.push_result(to_ipc_result(result));
                    return;
                }
            }
        }
        let result = if let Some(ref debug_pad) = rm.get_debug_pad() {
            let (r, _) = debug_pad.lock().activation.activate_with_aruid(aruid);
            to_ipc_result(r)
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 11: ActivateTouchScreen
    fn activate_touch_screen(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(_this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::ActivateTouchScreen called, aruid={}", aruid);

        let rm = server.resource_manager.lock();
        let result = if let (Some(touch_screen), Some(touch_resource), Some(touch_driver)) = (
            rm.get_touch_screen(),
            rm.get_touch_resource(),
            rm.get_touch_driver(),
        ) {
            let screen = touch_screen.lock();
            let mut resource = touch_resource.lock();
            let mut driver = touch_driver.lock();

            if !server.firmware_settings.is_device_managed() {
                let first = screen.activate(&mut resource, &mut driver);
                if first.is_error() {
                    to_ipc_result(first)
                } else {
                    to_ipc_result(screen.activate_with_aruid(&mut resource, aruid))
                }
            } else {
                to_ipc_result(screen.activate_with_aruid(&mut resource, aruid))
            }
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 21: ActivateMouse
    fn activate_mouse(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::ActivateMouse called, aruid={}", aruid);

        let rm = server.resource_manager.lock();
        if !server.firmware_settings.is_device_managed() {
            if let Some(ref mouse) = rm.get_mouse() {
                let (result, _) = mouse.lock().activation.activate();
                if result.is_error() {
                    let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                    rb.push_result(to_ipc_result(result));
                    return;
                }
            }
        }
        let result = if let Some(ref mouse) = rm.get_mouse() {
            let (r, _) = mouse.lock().activation.activate_with_aruid(aruid);
            to_ipc_result(r)
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 26: ActivateDebugMouse — upstream has nullptr handler (unimplemented)
    fn activate_debug_mouse(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::ActivateDebugMouse called, aruid={}", aruid);

        let rm = server.resource_manager.lock();
        if !server.firmware_settings.is_device_managed() {
            if let Some(ref debug_mouse) = rm.get_debug_mouse() {
                let (result, _) = debug_mouse.lock().activation.activate();
                if result.is_error() {
                    let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                    rb.push_result(to_ipc_result(result));
                    return;
                }
            }
        }
        let result = if let Some(ref debug_mouse) = rm.get_debug_mouse() {
            let (r, _) = debug_mouse.lock().activation.activate_with_aruid(aruid);
            to_ipc_result(r)
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 31: ActivateKeyboard
    fn activate_keyboard(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::ActivateKeyboard called, aruid={}", aruid);

        let rm = server.resource_manager.lock();
        if !server.firmware_settings.is_device_managed() {
            if let Some(ref keyboard) = rm.get_keyboard() {
                let (result, _) = keyboard.lock().activation.activate();
                if result.is_error() {
                    let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                    rb.push_result(to_ipc_result(result));
                    return;
                }
            }
        }
        let result = if let Some(ref keyboard) = rm.get_keyboard() {
            let (r, _) = keyboard.lock().activation.activate_with_aruid(aruid);
            to_ipc_result(r)
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 32: SendKeyboardLockKeyEvent
    fn send_keyboard_lock_key_event(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let flags = rp.pop_u32();
        log::warn!(
            "(STUBBED) IHidServer::SendKeyboardLockKeyEvent called, flags={}",
            flags
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 40: AcquireXpadIdEventHandle — stubbed since 10.0.0+
    fn acquire_xpad_id_event_handle(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!(
            "IHidServer::AcquireXpadIdEventHandle called, aruid={}",
            aruid
        );
        // This function has been stubbed since 10.0.0+
        // Upstream: *out_event = nullptr; R_SUCCEED();
        // Push a null copy handle (0)
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_objects(0);
    }

    // cmd 41: ReleaseXpadIdEventHandle — stubbed since 10.0.0+
    fn release_xpad_id_event_handle(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!(
            "IHidServer::ReleaseXpadIdEventHandle called, aruid={}",
            aruid
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 51: ActivateXpad — stubbed since 10.0.0+
    fn activate_xpad(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let basic_xpad_id = rp.pop_u32();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::debug!(
            "IHidServer::ActivateXpad called, basic_xpad_id={}, aruid={}",
            basic_xpad_id,
            aruid
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 55: GetXpadIds — hardcoded since 10.0.0+
    fn get_xpad_ids(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("IHidServer::GetXpadIds called");
        // Upstream writes [0, 1, 2, 3] to the output buffer and returns count=4
        let ids: [u32; 4] = [0, 1, 2, 3];
        let mut buf = Vec::with_capacity(16);
        for id in &ids {
            buf.extend_from_slice(&id.to_le_bytes());
        }
        ctx.write_buffer(&buf, 0);
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(4); // count
    }

    // cmd 56: ActivateJoyXpad — stubbed since 10.0.0+
    fn activate_joy_xpad(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let joy_xpad_id = rp.pop_u32();
        log::debug!(
            "IHidServer::ActivateJoyXpad called, joy_xpad_id={}",
            joy_xpad_id
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 58: GetJoyXpadLifoHandle — stubbed since 10.0.0+
    fn get_joy_xpad_lifo_handle(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let joy_xpad_id = rp.pop_u32();
        log::debug!(
            "IHidServer::GetJoyXpadLifoHandle called, joy_xpad_id={}",
            joy_xpad_id
        );
        // Upstream: *out_shared_memory_handle = nullptr; R_SUCCEED();
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_objects(0);
    }

    // cmd 59: GetJoyXpadIds — hardcoded since 10.0.0+
    fn get_joy_xpad_ids(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("IHidServer::GetJoyXpadIds called");
        // Upstream: *out_basic_xpad_id_count = 0; R_SUCCEED();
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(0i64 as u64); // count = 0
    }

    // cmd 60: ActivateSixAxisSensor — stubbed since 10.0.0+
    fn activate_six_axis_sensor(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let joy_xpad_id = rp.pop_u32();
        log::debug!(
            "IHidServer::ActivateSixAxisSensor called, joy_xpad_id={}",
            joy_xpad_id
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 61: DeactivateSixAxisSensor — stubbed since 10.0.0+
    fn deactivate_six_axis_sensor(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let joy_xpad_id = rp.pop_u32();
        log::debug!(
            "IHidServer::DeactivateSixAxisSensor called, joy_xpad_id={}",
            joy_xpad_id
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 62: GetSixAxisSensorLifoHandle — stubbed since 10.0.0+
    fn get_six_axis_sensor_lifo_handle(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let joy_xpad_id = rp.pop_u32();
        log::debug!(
            "IHidServer::GetSixAxisSensorLifoHandle called, joy_xpad_id={}",
            joy_xpad_id
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_objects(0);
    }

    // cmd 63: ActivateJoySixAxisSensor — stubbed since 10.0.0+
    fn activate_joy_six_axis_sensor(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let joy_xpad_id = rp.pop_u32();
        log::debug!(
            "IHidServer::ActivateJoySixAxisSensor called, joy_xpad_id={}",
            joy_xpad_id
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 64: DeactivateJoySixAxisSensor — stubbed since 10.0.0+
    fn deactivate_joy_six_axis_sensor(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let joy_xpad_id = rp.pop_u32();
        log::debug!(
            "IHidServer::DeactivateJoySixAxisSensor called, joy_xpad_id={}",
            joy_xpad_id
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 65: GetJoySixAxisSensorLifoHandle — stubbed since 10.0.0+
    fn get_joy_six_axis_sensor_lifo_handle(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let joy_xpad_id = rp.pop_u32();
        log::debug!(
            "IHidServer::GetJoySixAxisSensorLifoHandle called, joy_xpad_id={}",
            joy_xpad_id
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_objects(0);
    }

    // cmd 66: StartSixAxisSensor
    fn start_six_axis_sensor(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::StartSixAxisSensor called, npad_type={:?}, npad_id={}, device_index={:?}, aruid={}",
            sixaxis_handle.npad_type, sixaxis_handle.npad_id, sixaxis_handle.device_index, aruid);

        let rm = server.resource_manager.lock();
        let result = if let Some(ref six_axis) = rm.get_six_axis() {
            to_ipc_result(six_axis.lock().set_six_axis_enabled(&sixaxis_handle, true))
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 67: StopSixAxisSensor
    fn stop_six_axis_sensor(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::StopSixAxisSensor called, npad_type={:?}, npad_id={}, device_index={:?}, aruid={}",
            sixaxis_handle.npad_type, sixaxis_handle.npad_id, sixaxis_handle.device_index, aruid);

        let rm = server.resource_manager.lock();
        let result = if let Some(ref six_axis) = rm.get_six_axis() {
            to_ipc_result(six_axis.lock().set_six_axis_enabled(&sixaxis_handle, false))
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 68: IsSixAxisSensorFusionEnabled
    fn is_six_axis_sensor_fusion_enabled(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::IsSixAxisSensorFusionEnabled called, npad_type={:?}, npad_id={}, device_index={:?}, aruid={}",
            sixaxis_handle.npad_type, sixaxis_handle.npad_id, sixaxis_handle.device_index, aruid);

        let rm = server.resource_manager.lock();
        let (result, is_enabled) = if let Some(ref six_axis) = rm.get_six_axis() {
            match six_axis
                .lock()
                .is_six_axis_sensor_fusion_enabled(&sixaxis_handle)
            {
                Ok(v) => (RESULT_SUCCESS, v),
                Err(e) => (to_ipc_result(e), false),
            }
        } else {
            (RESULT_SUCCESS, false)
        };

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_bool(is_enabled);
    }

    // cmd 69: EnableSixAxisSensorFusion
    fn enable_six_axis_sensor_fusion(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let is_enabled = rp.pop_bool();
        let _padding1 = rp.pop_u8();
        let _padding2 = rp.pop_u16();
        let sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::EnableSixAxisSensorFusion called, is_enabled={}, npad_type={:?}, npad_id={}, device_index={:?}, aruid={}",
            is_enabled, sixaxis_handle.npad_type, sixaxis_handle.npad_id, sixaxis_handle.device_index, aruid);

        let rm = server.resource_manager.lock();
        let result = if let Some(ref six_axis) = rm.get_six_axis() {
            to_ipc_result(
                six_axis
                    .lock()
                    .set_six_axis_fusion_enabled(&sixaxis_handle, is_enabled),
            )
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 70: SetSixAxisSensorFusionParameters
    fn set_six_axis_sensor_fusion_parameters(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let sixaxis_fusion: SixAxisSensorFusionParameters = rp.pop_raw();
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::SetSixAxisSensorFusionParameters called, npad_type={:?}, npad_id={}, device_index={:?}, p1={}, p2={}, aruid={}",
            sixaxis_handle.npad_type, sixaxis_handle.npad_id, sixaxis_handle.device_index,
            sixaxis_fusion.parameter1, sixaxis_fusion.parameter2, aruid);

        let rm = server.resource_manager.lock();
        let result = if let Some(ref six_axis) = rm.get_six_axis() {
            to_ipc_result(
                six_axis
                    .lock()
                    .set_six_axis_fusion_parameters(&sixaxis_handle, sixaxis_fusion),
            )
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 71: GetSixAxisSensorFusionParameters
    fn get_six_axis_sensor_fusion_parameters(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::GetSixAxisSensorFusionParameters called, npad_type={:?}, npad_id={}, device_index={:?}, aruid={}",
            sixaxis_handle.npad_type, sixaxis_handle.npad_id, sixaxis_handle.device_index, aruid);

        let rm = server.resource_manager.lock();
        let (result, fusion) = if let Some(ref six_axis) = rm.get_six_axis() {
            match six_axis
                .lock()
                .get_six_axis_fusion_parameters(&sixaxis_handle)
            {
                Ok(f) => (RESULT_SUCCESS, f),
                Err(e) => (to_ipc_result(e), SixAxisSensorFusionParameters::default()),
            }
        } else {
            (RESULT_SUCCESS, SixAxisSensorFusionParameters::default())
        };

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(result);
        rb.push_raw(&fusion);
    }

    // cmd 72: ResetSixAxisSensorFusionParameters
    fn reset_six_axis_sensor_fusion_parameters(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::ResetSixAxisSensorFusionParameters called, npad_type={:?}, npad_id={}, device_index={:?}, aruid={}",
            sixaxis_handle.npad_type, sixaxis_handle.npad_id, sixaxis_handle.device_index, aruid);

        // Since these parameters are unknown just use what HW outputs
        let fusion_parameters = SixAxisSensorFusionParameters {
            parameter1: 0.03,
            parameter2: 0.4,
        };

        let rm = server.resource_manager.lock();
        if let Some(ref six_axis) = rm.get_six_axis() {
            let mut sa = six_axis.lock();
            let r = sa.set_six_axis_fusion_parameters(&sixaxis_handle, fusion_parameters);
            if r.is_error() {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(to_ipc_result(r));
                return;
            }
            let r = sa.set_six_axis_fusion_enabled(&sixaxis_handle, true);
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(to_ipc_result(r));
        } else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_SUCCESS);
        }
    }

    // cmd 79: SetGyroscopeZeroDriftMode
    fn set_gyroscope_zero_drift_mode(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let drift_mode_raw = rp.pop_u32();
        let drift_mode: GyroscopeZeroDriftMode = unsafe { core::mem::transmute(drift_mode_raw) };
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::SetGyroscopeZeroDriftMode called, npad_type={:?}, npad_id={}, device_index={:?}, drift_mode={:?}, aruid={}",
            sixaxis_handle.npad_type, sixaxis_handle.npad_id, sixaxis_handle.device_index, drift_mode, aruid);

        let rm = server.resource_manager.lock();
        let result = if let Some(ref six_axis) = rm.get_six_axis() {
            to_ipc_result(
                six_axis
                    .lock()
                    .set_gyroscope_zero_drift_mode(&sixaxis_handle, drift_mode),
            )
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 80: GetGyroscopeZeroDriftMode
    fn get_gyroscope_zero_drift_mode(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::GetGyroscopeZeroDriftMode called, npad_type={:?}, npad_id={}, device_index={:?}, aruid={}",
            sixaxis_handle.npad_type, sixaxis_handle.npad_id, sixaxis_handle.device_index, aruid);

        let rm = server.resource_manager.lock();
        let (result, drift_mode) = if let Some(ref six_axis) = rm.get_six_axis() {
            match six_axis
                .lock()
                .get_gyroscope_zero_drift_mode(&sixaxis_handle)
            {
                Ok(m) => (RESULT_SUCCESS, m),
                Err(e) => (to_ipc_result(e), GyroscopeZeroDriftMode::Standard),
            }
        } else {
            (RESULT_SUCCESS, GyroscopeZeroDriftMode::Standard)
        };

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_u32(drift_mode as u32);
    }

    // cmd 81: ResetGyroscopeZeroDriftMode
    fn reset_gyroscope_zero_drift_mode(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::ResetGyroscopeZeroDriftMode called, npad_type={:?}, npad_id={}, device_index={:?}, aruid={}",
            sixaxis_handle.npad_type, sixaxis_handle.npad_id, sixaxis_handle.device_index, aruid);

        let rm = server.resource_manager.lock();
        let result =
            if let Some(ref six_axis) = rm.get_six_axis() {
                to_ipc_result(six_axis.lock().set_gyroscope_zero_drift_mode(
                    &sixaxis_handle,
                    GyroscopeZeroDriftMode::Standard,
                ))
            } else {
                RESULT_SUCCESS
            };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 82: IsSixAxisSensorAtRest
    fn is_six_axis_sensor_at_rest(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::IsSixAxisSensorAtRest called, npad_type={:?}, npad_id={}, device_index={:?}, aruid={}",
            sixaxis_handle.npad_type, sixaxis_handle.npad_id, sixaxis_handle.device_index, aruid);

        let rm = server.resource_manager.lock();
        let (result, is_at_rest) = if let Some(ref six_axis) = rm.get_six_axis() {
            match six_axis.lock().is_six_axis_sensor_at_rest(&sixaxis_handle) {
                Ok(v) => (RESULT_SUCCESS, v),
                Err(e) => (to_ipc_result(e), false),
            }
        } else {
            (RESULT_SUCCESS, false)
        };

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_bool(is_at_rest);
    }

    // cmd 83: IsFirmwareUpdateAvailableForSixAxisSensor
    fn is_firmware_update_available_for_six_axis_sensor(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let _sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::IsFirmwareUpdateAvailableForSixAxisSensor called, aruid={}",
            aruid
        );
        // Upstream delegates to npad->IsFirmwareUpdateAvailableForSixAxisSensor which is not
        // yet ported. Return false (no update available).
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(false);
    }

    // cmd 84: EnableSixAxisSensorUnalteredPassthrough
    fn enable_six_axis_sensor_unaltered_passthrough(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let is_enabled = rp.pop_bool();
        let _padding1 = rp.pop_u8();
        let _padding2 = rp.pop_u16();
        let sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let aruid = rp.pop_u64();
        log::debug!("(STUBBED) IHidServer::EnableSixAxisSensorUnalteredPassthrough called, is_enabled={}, aruid={}", is_enabled, aruid);

        let rm = server.resource_manager.lock();
        let result = if let Some(ref six_axis) = rm.get_six_axis() {
            to_ipc_result(
                six_axis
                    .lock()
                    .enable_six_axis_sensor_unaltered_passthrough(&sixaxis_handle, is_enabled),
            )
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 85: IsSixAxisSensorUnalteredPassthroughEnabled
    fn is_six_axis_sensor_unaltered_passthrough_enabled(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::debug!(
            "(STUBBED) IHidServer::IsSixAxisSensorUnalteredPassthroughEnabled called, aruid={}",
            aruid
        );

        let rm = server.resource_manager.lock();
        let (result, is_enabled) = if let Some(ref six_axis) = rm.get_six_axis() {
            match six_axis
                .lock()
                .is_six_axis_sensor_unaltered_passthrough_enabled(&sixaxis_handle)
            {
                Ok(v) => (RESULT_SUCCESS, v),
                Err(e) => (to_ipc_result(e), false),
            }
        } else {
            (RESULT_SUCCESS, false)
        };

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_bool(is_enabled);
    }

    // cmd 87: LoadSixAxisSensorCalibrationParameter
    fn load_six_axis_sensor_calibration_parameter(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::LoadSixAxisSensorCalibrationParameter called, aruid={}",
            aruid
        );

        let rm = server.resource_manager.lock();
        if let Some(ref six_axis) = rm.get_six_axis() {
            let sa = six_axis.lock();
            let params = sa.get_sixaxis_calibration(&sixaxis_handle);
            ctx.write_buffer(params, 0);
        } else {
            ctx.write_buffer(&[0u8; 0x744], 0);
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 88: GetSixAxisSensorIcInformation
    fn get_six_axis_sensor_ic_information(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::GetSixAxisSensorIcInformation called, aruid={}",
            aruid
        );

        let rm = server.resource_manager.lock();
        if let Some(ref six_axis) = rm.get_six_axis() {
            let sa = six_axis.lock();
            let ic_info = sa.get_sixaxis_ic_information(&sixaxis_handle);
            ctx.write_buffer(ic_info, 0);
        } else {
            ctx.write_buffer(&[0u8; 0xC8], 0);
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 89: ResetIsSixAxisSensorDeviceNewlyAssigned
    fn reset_is_six_axis_sensor_device_newly_assigned(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let sixaxis_handle: SixAxisSensorHandle = rp.pop_raw();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::warn!(
            "IHidServer::ResetIsSixAxisSensorDeviceNewlyAssigned called, npad_type={:?}, npad_id={}, device_index={:?}, aruid={}",
            sixaxis_handle.npad_type,
            sixaxis_handle.npad_id,
            sixaxis_handle.device_index,
            aruid
        );
        let rm = server.resource_manager.lock();
        let result = if let Some(ref npad) = rm.get_npad() {
            to_ipc_result(
                npad.lock()
                    .reset_is_six_axis_sensor_device_newly_assigned(aruid, &sixaxis_handle),
            )
        } else {
            RESULT_SUCCESS
        };
        Self::trace_npad(
            89,
            result,
            aruid,
            sixaxis_handle.npad_type as u64,
            sixaxis_handle.npad_id as u64,
            sixaxis_handle.device_index as u64,
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 91: ActivateGesture
    fn activate_gesture(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let basic_gesture_id = rp.pop_u32();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::info!(
            "IHidServer::ActivateGesture called, basic_gesture_id={}, aruid={}",
            basic_gesture_id,
            aruid
        );

        let rm = server.resource_manager.lock();
        let result = if let (Some(gesture), Some(touch_resource), Some(touch_driver)) = (
            rm.get_gesture(),
            rm.get_touch_resource(),
            rm.get_touch_driver(),
        ) {
            let gesture = gesture.lock();
            let mut resource = touch_resource.lock();
            let mut driver = touch_driver.lock();

            if !server.firmware_settings.is_device_managed() {
                let first = gesture.activate(&mut resource, &mut driver);
                if first.is_error() {
                    to_ipc_result(first)
                } else {
                    to_ipc_result(gesture.activate_with_aruid(
                        &mut resource,
                        aruid,
                        basic_gesture_id,
                    ))
                }
            } else {
                to_ipc_result(gesture.activate_with_aruid(&mut resource, aruid, basic_gesture_id))
            }
        } else {
            RESULT_SUCCESS
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 100: SetSupportedNpadStyleSet
    fn set_supported_npad_style_set(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let supported_style_set_raw = rp.pop_u32();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        let supported_style_set = NpadStyleSet::from_bits_truncate(supported_style_set_raw);
        log::debug!(
            "IHidServer::SetSupportedNpadStyleSet called, style_set={:?}, aruid={}",
            supported_style_set,
            aruid
        );

        let rm = server.resource_manager.lock();
        let result = if let Some(ref npad) = rm.get_npad() {
            let r = npad
                .lock()
                .set_supported_npad_style_set(aruid, supported_style_set);
            if r.is_error() {
                to_ipc_result(r)
            } else {
                RESULT_SUCCESS
            }
        } else {
            RESULT_SUCCESS
        };
        Self::trace_npad(100, result, aruid, supported_style_set_raw as u64, 0, 0);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 101: GetSupportedNpadStyleSet
    fn get_supported_npad_style_set(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!(
            "IHidServer::GetSupportedNpadStyleSet called, aruid={}",
            aruid
        );

        let rm = server.resource_manager.lock();
        let (result, style_set) = if let Some(ref npad) = rm.get_npad() {
            match npad.lock().get_supported_npad_style_set(aruid) {
                Ok(ss) => (RESULT_SUCCESS, ss),
                Err(e) => (to_ipc_result(e), NpadStyleSet::NONE),
            }
        } else {
            (RESULT_SUCCESS, NpadStyleSet::NONE)
        };
        Self::trace_npad(101, result, aruid, 0, 0, style_set.bits() as u64);

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(result);
        rb.push_u32(style_set.bits());
    }

    // cmd 102: SetSupportedNpadIdType
    fn set_supported_npad_id_type(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::SetSupportedNpadIdType called, aruid={}", aruid);

        let buffer = ctx.read_buffer(0);
        let npad_count = buffer.len() / core::mem::size_of::<u32>();
        let mut npad_ids = Vec::with_capacity(npad_count);
        for i in 0..npad_count {
            let offset = i * 4;
            if offset + 4 <= buffer.len() {
                let raw = u32::from_le_bytes([
                    buffer[offset],
                    buffer[offset + 1],
                    buffer[offset + 2],
                    buffer[offset + 3],
                ]);
                let npad_id: NpadIdType = unsafe { core::mem::transmute(raw) };
                npad_ids.push(npad_id);
            }
        }
        if std::env::var_os("RUZU_TRACE_HID_RESOURCE").is_some() {
            let raw_ids: Vec<u32> = npad_ids.iter().map(|id| *id as u32).collect();
            log::info!(
                "[HID_RESOURCE] SetSupportedNpadIdType aruid=0x{:X} count={} ids={:X?}",
                aruid,
                raw_ids.len(),
                raw_ids
            );
        }

        let rm = server.resource_manager.lock();
        let result = if let Some(ref npad) = rm.get_npad() {
            to_ipc_result(npad.lock().set_supported_npad_id_type(aruid, &npad_ids))
        } else {
            RESULT_SUCCESS
        };
        Self::trace_npad(102, result, aruid, npad_count as u64, 0, 0);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 103: ActivateNpad
    fn activate_npad(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::ActivateNpad called, aruid={}", aruid);

        let rm = server.resource_manager.lock();
        let register_result = rm.register_applet_resource_user_id(aruid, true);
        let create_result = rm.create_applet_resource(aruid);
        let result = if let Some(ref npad) = rm.get_npad() {
            let mut ng = npad.lock();
            let _ = ng.register_applet_resource_user_id(aruid);
            if !ng.is_active() {
                let _ = ng.activate();
            }
            ng.npad_resource_mut()
                .set_npad_revision(aruid, NpadRevision::Revision0);
            to_ipc_result(ng.activate_for_aruid(aruid))
        } else {
            RESULT_SUCCESS
        };
        if std::env::var_os("RUZU_TRACE_HID_RESOURCE").is_some() {
            log::info!(
                "[HID_RESOURCE] ActivateNpad aruid=0x{:X} register=0x{:08X} create=0x{:08X} activate=0x{:08X}",
                aruid,
                register_result.0,
                create_result.0,
                result.0
            );
        }
        Self::trace_npad(
            103,
            result,
            aruid,
            register_result.0 as u64,
            create_result.0 as u64,
            0,
        );

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 104: DeactivateNpad — does nothing since 10.0.0+
    fn deactivate_npad(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::DeactivateNpad called, aruid={}", aruid);
        // This function does nothing since 10.0.0+
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 106: AcquireNpadStyleSetUpdateEventHandle
    fn acquire_npad_style_set_update_event_handle(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let npad_id_raw = rp.pop_u32();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        let unknown = rp.pop_u64();
        let npad_id: NpadIdType = unsafe { core::mem::transmute(npad_id_raw) };
        log::debug!("IHidServer::AcquireNpadStyleSetUpdateEventHandle called, npad_id=0x{:X}, aruid={}, unknown={}",
            npad_id_raw, aruid, unknown);
        let event = {
            let mut events = server.npad_style_set_events.lock();
            Arc::clone(
                events
                    .entry((aruid, npad_id_raw))
                    .or_insert_with(|| Arc::new(Event::new())),
            )
        };
        let result = {
            let resource_manager = server.resource_manager.lock();
            if let Some(npad) = resource_manager.get_npad() {
                let event_to_signal = Arc::clone(&event);
                to_ipc_result(
                    npad.lock()
                        .npad_resource_mut()
                        .register_style_set_update_event(
                            aruid,
                            npad_id,
                            Arc::new(move || event_to_signal.signal()),
                        ),
                )
            } else {
                RESULT_SUCCESS
            }
        };
        let handle = if result.is_success() {
            event.copy_handle(ctx).unwrap_or(0)
        } else {
            0
        };
        Self::trace_npad(
            106,
            result,
            aruid,
            npad_id_raw as u64,
            unknown,
            handle as u64,
        );
        let mut rb = ResponseBuilder::new(ctx, 2, u32::from(result.is_success()), 0);
        rb.push_result(result);
        if result.is_success() {
            rb.push_copy_objects(handle);
        }
    }

    // cmd 107: DisconnectNpad
    fn disconnect_npad(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let npad_id_raw = rp.pop_u32();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        let _npad_id: NpadIdType = unsafe { core::mem::transmute(npad_id_raw) };
        log::debug!(
            "IHidServer::DisconnectNpad called, npad_id=0x{:X}, aruid={}",
            npad_id_raw,
            aruid
        );
        // Upstream delegates to npad->DisconnectNpad. The Rust NPad does not yet have
        // this method. Return success.
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 108: GetPlayerLedPattern
    fn get_player_led_pattern(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let npad_id_raw = rp.pop_u32();
        let npad_id: NpadIdType = unsafe { core::mem::transmute(npad_id_raw) };
        log::debug!(
            "IHidServer::GetPlayerLedPattern called, npad_id={:?}",
            npad_id
        );

        let pattern = match npad_id {
            NpadIdType::Player1 => LedPattern::new(1, 0, 0, 0),
            NpadIdType::Player2 => LedPattern::new(1, 1, 0, 0),
            NpadIdType::Player3 => LedPattern::new(1, 1, 1, 0),
            NpadIdType::Player4 => LedPattern::new(1, 1, 1, 1),
            NpadIdType::Player5 => LedPattern::new(1, 0, 0, 1),
            NpadIdType::Player6 => LedPattern::new(1, 0, 1, 0),
            NpadIdType::Player7 => LedPattern::new(1, 0, 1, 1),
            NpadIdType::Player8 => LedPattern::new(0, 1, 1, 0),
            _ => LedPattern::new(0, 0, 0, 0),
        };

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(pattern.raw);
    }

    // cmd 109: ActivateNpadWithRevision
    fn activate_npad_with_revision(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let revision_raw = rp.pop_u32();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        let revision = NpadRevision::from_raw(revision_raw);
        log::debug!(
            "IHidServer::ActivateNpadWithRevision called, revision={:?}, aruid={}",
            revision,
            aruid
        );

        let rm = server.resource_manager.lock();
        let register_result = rm.register_applet_resource_user_id(aruid, true);
        let create_result = rm.create_applet_resource(aruid);
        let result = if let Some(ref npad) = rm.get_npad() {
            let mut ng = npad.lock();
            let _ = ng.register_applet_resource_user_id(aruid);
            if !ng.is_active() {
                let _ = ng.activate();
            }
            ng.npad_resource_mut().set_npad_revision(aruid, revision);
            to_ipc_result(ng.activate_for_aruid(aruid))
        } else {
            RESULT_SUCCESS
        };
        if std::env::var_os("RUZU_TRACE_HID_RESOURCE").is_some() {
            log::info!(
                "[HID_RESOURCE] ActivateNpadWithRevision aruid=0x{:X} register=0x{:08X} create=0x{:08X} activate=0x{:08X}",
                aruid,
                register_result.0,
                create_result.0,
                result.0
            );
        }
        Self::trace_npad(
            109,
            result,
            aruid,
            revision_raw as u64,
            register_result.0 as u64,
            create_result.0 as u64,
        );

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 120: SetNpadJoyHoldType
    fn set_npad_joy_hold_type(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        let hold_type_raw = rp.pop_u64();
        let hold_type: NpadJoyHoldType = unsafe { core::mem::transmute(hold_type_raw) };
        log::debug!(
            "IHidServer::SetNpadJoyHoldType called, aruid={}, hold_type={:?}",
            aruid,
            hold_type
        );

        if hold_type != NpadJoyHoldType::Horizontal && hold_type != NpadJoyHoldType::Vertical {
            log::error!("Invalid npad joy hold type: {:?}", hold_type);
        }

        let rm = server.resource_manager.lock();
        let result = if let Some(ref npad) = rm.get_npad() {
            to_ipc_result(npad.lock().set_npad_joy_hold_type(aruid, hold_type))
        } else {
            RESULT_SUCCESS
        };
        Self::trace_npad(120, result, aruid, hold_type_raw, 0, 0);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 121: GetNpadJoyHoldType
    fn get_npad_joy_hold_type(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::GetNpadJoyHoldType called, aruid={}", aruid);

        let rm = server.resource_manager.lock();
        let (result, hold_type) = if let Some(ref npad) = rm.get_npad() {
            match npad.lock().get_npad_joy_hold_type(aruid) {
                Ok(ht) => (RESULT_SUCCESS, ht),
                Err(e) => (to_ipc_result(e), NpadJoyHoldType::Vertical),
            }
        } else {
            (RESULT_SUCCESS, NpadJoyHoldType::Vertical)
        };
        Self::trace_npad(121, result, aruid, 0, 0, hold_type as u64);

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(result);
        rb.push_u64(hold_type as u64);
    }

    // cmd 122: SetNpadJoyAssignmentModeSingleByDefault
    fn set_npad_joy_assignment_mode_single_by_default(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let npad_id_raw = rp.pop_u32();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        let npad_id: NpadIdType = unsafe { core::mem::transmute(npad_id_raw) };
        log::info!(
            "IHidServer::SetNpadJoyAssignmentModeSingleByDefault called, npad_id={:?}, aruid={}",
            npad_id,
            aruid
        );

        let rm = server.resource_manager.lock();
        if let Some(ref npad) = rm.get_npad() {
            npad.lock()
                .set_npad_joy_assignment_mode_single_by_default(aruid, npad_id);
        }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 123: SetNpadJoyAssignmentModeSingle
    fn set_npad_joy_assignment_mode_single(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let npad_id_raw = rp.pop_u32();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        let npad_joy_device_type_raw = rp.pop_u64();
        let npad_id: NpadIdType = unsafe { core::mem::transmute(npad_id_raw) };
        let npad_joy_device_type: NpadJoyDeviceType =
            unsafe { core::mem::transmute(npad_joy_device_type_raw as i64) };
        log::info!(
            "IHidServer::SetNpadJoyAssignmentModeSingle called, npad_id={:?}, aruid={}",
            npad_id,
            aruid
        );
        let rm = server.resource_manager.lock();
        if let Some(ref npad) = rm.get_npad() {
            npad.lock()
                .set_npad_joy_assignment_mode_single(aruid, npad_id, npad_joy_device_type);
        }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 124: SetNpadJoyAssignmentModeDual
    fn set_npad_joy_assignment_mode_dual(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let npad_id_raw = rp.pop_u32();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        let npad_id: NpadIdType = unsafe { core::mem::transmute(npad_id_raw) };
        log::debug!(
            "IHidServer::SetNpadJoyAssignmentModeDual called, npad_id={:?}, aruid={}",
            npad_id,
            aruid
        );

        let rm = server.resource_manager.lock();
        if let Some(ref npad) = rm.get_npad() {
            npad.lock()
                .set_npad_joy_assignment_mode_dual(aruid, npad_id);
        }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 125: MergeSingleJoyAsDualJoy
    fn merge_single_joy_as_dual_joy(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let npad_id_1_raw = rp.pop_u32();
        let npad_id_2_raw = rp.pop_u32();
        let aruid = rp.pop_u64();
        let _npad_id_1: NpadIdType = unsafe { core::mem::transmute(npad_id_1_raw) };
        let _npad_id_2: NpadIdType = unsafe { core::mem::transmute(npad_id_2_raw) };
        log::debug!("IHidServer::MergeSingleJoyAsDualJoy called, npad_id_1=0x{:X}, npad_id_2=0x{:X}, aruid={}",
            npad_id_1_raw, npad_id_2_raw, aruid);
        // Upstream: npad->MergeSingleJoyAsDualJoy. Not yet ported. Return success.
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 126: StartLrAssignmentMode
    fn start_lr_assignment_mode(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::StartLrAssignmentMode called, aruid={}", aruid);

        let rm = server.resource_manager.lock();
        let result = if let Some(ref npad) = rm.get_npad() {
            to_ipc_result(npad.lock().start_lr_assignment_mode(aruid))
        } else {
            RESULT_SUCCESS
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 127: StopLrAssignmentMode
    fn stop_lr_assignment_mode(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::StopLrAssignmentMode called, aruid={}", aruid);

        let rm = server.resource_manager.lock();
        let result = if let Some(ref npad) = rm.get_npad() {
            to_ipc_result(npad.lock().stop_lr_assignment_mode(aruid))
        } else {
            RESULT_SUCCESS
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 128: SetNpadHandheldActivationMode
    fn set_npad_handheld_activation_mode(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        let activation_mode_raw = rp.pop_u64();
        let activation_mode: NpadHandheldActivationMode =
            unsafe { core::mem::transmute(activation_mode_raw) };
        log::debug!(
            "IHidServer::SetNpadHandheldActivationMode called, aruid={}, activation_mode={:?}",
            aruid,
            activation_mode
        );

        if (activation_mode as u64) >= (NpadHandheldActivationMode::MaxActivationMode as u64) {
            log::error!("Activation mode should be always None, Single or Dual");
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_SUCCESS);
            return;
        }

        let rm = server.resource_manager.lock();
        let result = if let Some(ref npad) = rm.get_npad() {
            to_ipc_result(
                npad.lock()
                    .set_npad_handheld_activation_mode(aruid, activation_mode),
            )
        } else {
            RESULT_SUCCESS
        };
        Self::trace_npad(128, result, aruid, activation_mode_raw, 0, 0);

        log::info!(
            "IHidServer::SetNpadHandheldActivationMode aruid={} activation_mode={:?} result=0x{:X}",
            aruid,
            activation_mode,
            result.get_inner_value()
        );

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 129: GetNpadHandheldActivationMode
    fn get_npad_handheld_activation_mode(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!(
            "IHidServer::GetNpadHandheldActivationMode called, aruid={}",
            aruid
        );

        let rm = server.resource_manager.lock();
        let (result, mode) = if let Some(ref npad) = rm.get_npad() {
            match npad.lock().get_npad_handheld_activation_mode(aruid) {
                Ok(m) => (RESULT_SUCCESS, m),
                Err(e) => (to_ipc_result(e), NpadHandheldActivationMode::Dual),
            }
        } else {
            (RESULT_SUCCESS, NpadHandheldActivationMode::Dual)
        };
        Self::trace_npad(129, result, aruid, 0, 0, mode as u64);

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(result);
        rb.push_u64(mode as u64);
    }

    // cmd 130: SwapNpadAssignment
    fn swap_npad_assignment(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let npad_id_1_raw = rp.pop_u32();
        let npad_id_2_raw = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::debug!(
            "IHidServer::SwapNpadAssignment called, npad_id_1=0x{:X}, npad_id_2=0x{:X}, aruid={}",
            npad_id_1_raw,
            npad_id_2_raw,
            aruid
        );
        // Upstream: npad->SwapNpadAssignment. Not yet ported. Return success.
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 131: IsUnintendedHomeButtonInputProtectionEnabled
    fn is_unintended_home_button_input_protection_enabled(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let npad_id_raw = rp.pop_u32();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        let npad_id: NpadIdType = unsafe { core::mem::transmute(npad_id_raw) };
        log::info!("IHidServer::IsUnintendedHomeButtonInputProtectionEnabled called, npad_id={:?}, aruid={}", npad_id, aruid);

        if !hid_core::hid_util::is_npad_id_valid(npad_id) {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(to_ipc_result(hid_core::hid_result::RESULT_INVALID_NPAD_ID));
            return;
        }

        let rm = server.resource_manager.lock();
        let (result, is_enabled) = if let Some(ref npad) = rm.get_npad() {
            match npad
                .lock()
                .npad_resource()
                .get_home_protection_enabled(aruid, npad_id)
            {
                Ok(v) => (RESULT_SUCCESS, v),
                Err(e) => (to_ipc_result(e), false),
            }
        } else {
            (RESULT_SUCCESS, false)
        };

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_bool(is_enabled);
    }

    // cmd 132: EnableUnintendedHomeButtonInputProtection
    fn enable_unintended_home_button_input_protection(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let is_enabled = rp.pop_bool();
        let _padding1 = rp.pop_u8();
        let _padding2 = rp.pop_u16();
        let npad_id_raw = rp.pop_u32();
        let aruid = rp.pop_u64();
        let npad_id: NpadIdType = unsafe { core::mem::transmute(npad_id_raw) };
        log::debug!("IHidServer::EnableUnintendedHomeButtonInputProtection called, is_enabled={}, npad_id={:?}, aruid={}",
            is_enabled, npad_id, aruid);

        if !hid_core::hid_util::is_npad_id_valid(npad_id) {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(to_ipc_result(hid_core::hid_result::RESULT_INVALID_NPAD_ID));
            return;
        }

        let rm = server.resource_manager.lock();
        let result = if let Some(ref npad) = rm.get_npad() {
            to_ipc_result(
                npad.lock()
                    .npad_resource_mut()
                    .set_home_protection_enabled(aruid, npad_id, is_enabled),
            )
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 133: SetNpadJoyAssignmentModeSingleWithDestination
    fn set_npad_joy_assignment_mode_single_with_destination(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let npad_id_raw = rp.pop_u32();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        let npad_joy_device_type_raw = rp.pop_u64();
        let npad_id: NpadIdType = unsafe { core::mem::transmute(npad_id_raw) };
        let npad_joy_device_type: NpadJoyDeviceType =
            unsafe { core::mem::transmute(npad_joy_device_type_raw as i64) };
        log::info!("IHidServer::SetNpadJoyAssignmentModeSingleWithDestination called, npad_id={:?}, aruid={}", npad_id, aruid);
        let mut is_reassigned = false;
        let mut new_npad_id = NpadIdType::default();
        let rm = server.resource_manager.lock();
        if let Some(ref npad) = rm.get_npad() {
            (is_reassigned, new_npad_id) = npad.lock().set_npad_mode(
                aruid,
                npad_id,
                npad_joy_device_type,
                NpadJoyAssignmentMode::Single,
            );
        }
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(is_reassigned);
        rb.push_u32(new_npad_id as u32);
    }

    // cmd 134: SetNpadAnalogStickUseCenterClamp
    fn set_npad_analog_stick_use_center_clamp(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let use_center_clamp = rp.pop_bool();
        let _padding1 = rp.pop_u8();
        let _padding2 = rp.pop_u16();
        let _padding3 = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::info!(
            "IHidServer::SetNpadAnalogStickUseCenterClamp called, use_center_clamp={}, aruid={}",
            use_center_clamp,
            aruid
        );

        let rm = server.resource_manager.lock();
        if let Some(ref npad) = rm.get_npad() {
            npad.lock()
                .npad_resource_mut()
                .set_npad_analog_stick_use_center_clamp(aruid, use_center_clamp);
        }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 135: SetNpadCaptureButtonAssignment
    fn set_npad_capture_button_assignment(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let npad_styleset_raw = rp.pop_u32();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        let button = rp.pop_u64();
        log::info!("IHidServer::SetNpadCaptureButtonAssignment called, npad_styleset=0x{:X}, aruid={}, button=0x{:X}",
            npad_styleset_raw, aruid, button);
        // Upstream: npad->SetNpadCaptureButtonAssignment. Not yet ported. Return success.
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 136: ClearNpadCaptureButtonAssignment
    fn clear_npad_capture_button_assignment(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::info!(
            "IHidServer::ClearNpadCaptureButtonAssignment called, aruid={}",
            aruid
        );
        // Upstream: npad->ClearNpadCaptureButtonAssignment. Not yet ported. Return success.
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 200: GetVibrationDeviceInfo
    fn get_vibration_device_info(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let handle: VibrationDeviceHandle = rp.pop_raw();
        log::debug!("IHidServer::GetVibrationDeviceInfo called, npad_type={:?}, npad_id={}, device_index={:?}", handle.npad_type, handle.npad_id, handle.device_index);

        let rm = server.resource_manager.lock();
        match rm.get_vibration_device_info(&handle) {
            Ok(info) => {
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_raw(&info);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(to_ipc_result(result));
            }
        }
    }

    // cmd 201: SendVibrationValue
    fn send_vibration_value(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let handle: VibrationDeviceHandle = rp.pop_raw();
        let value: VibrationValue = rp.pop_raw();
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::SendVibrationValue called, aruid={}", aruid);
        let result = server
            .resource_manager
            .lock()
            .send_vibration_value(aruid, &handle, &value);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(to_ipc_result(result));
    }

    // cmd 202: GetActualVibrationValue
    fn get_actual_vibration_value(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let vibration_device_handle: VibrationDeviceHandle = rp.pop_raw();
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::GetActualVibrationValue called, npad_type={:?}, npad_id={}, device_index={:?}, aruid={}",
            vibration_device_handle.npad_type, vibration_device_handle.npad_id,
            vibration_device_handle.device_index, aruid);

        let rm = server.resource_manager.lock();
        let (result, vibration_value) =
            match rm.get_actual_vibration_value(aruid, &vibration_device_handle) {
                Ok(value) => (RESULT_SUCCESS, value),
                Err(result) => (to_ipc_result(result), DEFAULT_VIBRATION_VALUE),
            };

        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(result);
        rb.push_raw(&vibration_value);
    }

    // cmd 203: CreateActiveVibrationDeviceList
    fn create_active_vibration_device_list(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        log::debug!("IHidServer::CreateActiveVibrationDeviceList called");

        let device_list: Arc<dyn SessionRequestHandler> = Arc::new(
            super::active_vibration_device_list::IActiveVibrationDeviceList::new(
                server.resource_manager.clone(),
            ),
        );

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(device_list);
    }

    // cmd 204: PermitVibration
    fn permit_vibration(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let can_vibrate = rp.pop_bool();
        log::debug!(
            "IHidServer::PermitVibration called, can_vibrate={}",
            can_vibrate
        );

        let rm = server.resource_manager.lock();
        let result = if let Some(ref npad) = rm.get_npad() {
            let volume = if can_vibrate { 1.0f32 } else { 0.0f32 };
            to_ipc_result(npad.lock().set_vibration_master_volume(volume))
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 205: IsVibrationPermitted
    fn is_vibration_permitted(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        log::debug!("IHidServer::IsVibrationPermitted called");

        let rm = server.resource_manager.lock();
        let (result, is_permitted) = if let Some(ref npad) = rm.get_npad() {
            match npad.lock().get_vibration_master_volume() {
                Ok(volume) => (RESULT_SUCCESS, volume > 0.0f32),
                Err(e) => (to_ipc_result(e), false),
            }
        } else {
            (RESULT_SUCCESS, false)
        };

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_bool(is_permitted);
    }

    // cmd 206: SendVibrationValues
    fn send_vibration_values(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!("IHidServer::SendVibrationValues called, aruid={}", aruid);

        let handles_buf = ctx.read_buffer(0);
        let values_buf = ctx.read_buffer(1);
        let handle_size = core::mem::size_of::<VibrationDeviceHandle>();
        let value_size = core::mem::size_of::<VibrationValue>();
        let handle_count = if handle_size > 0 {
            handles_buf.len() / handle_size
        } else {
            0
        };
        let value_count = if value_size > 0 {
            values_buf.len() / value_size
        } else {
            0
        };

        if handle_count != value_count {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(to_ipc_result(
                hid_core::hid_result::RESULT_VIBRATION_ARRAY_SIZE_MISMATCH,
            ));
            return;
        }
        let rm = server.resource_manager.lock();
        let mut result = RESULT_SUCCESS;
        for index in 0..handle_count {
            let handle_offset = index * handle_size;
            let value_offset = index * value_size;
            let handle = unsafe {
                std::ptr::read_unaligned(
                    handles_buf[handle_offset..].as_ptr() as *const VibrationDeviceHandle
                )
            };
            let value = unsafe {
                std::ptr::read_unaligned(
                    values_buf[value_offset..].as_ptr() as *const VibrationValue
                )
            };
            let send_result = rm.send_vibration_value(aruid, &handle, &value);
            if send_result.is_error() {
                result = to_ipc_result(send_result);
                break;
            }
        }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 207: SendVibrationGcErmCommand
    fn send_vibration_gc_erm_command(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let vibration_device_handle: VibrationDeviceHandle = rp.pop_raw();
        let aruid = rp.pop_u64();
        let gc_erm_command = rp.pop_u64();
        log::debug!(
            "IHidServer::SendVibrationGcErmCommand called, aruid={}, gc_erm_command={}",
            aruid,
            gc_erm_command
        );

        // Upstream checks IsVibrationAruidActive, validates handle, gets GcVibrationDevice.
        // GcVibrationDevice not yet ported. Check active aruid and return success.
        let rm = server.resource_manager.lock();
        let has_active_aruid = match rm.is_vibration_aruid_active(aruid) {
            Ok(v) => v,
            Err(_) => false,
        };
        if !has_active_aruid {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_SUCCESS);
            return;
        }
        let _ = vibration_device_handle;
        // GcVibrationDevice::SendVibrationGcErmCommand not yet ported.
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 208: GetActualVibrationGcErmCommand
    fn get_actual_vibration_gc_erm_command(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let vibration_device_handle: VibrationDeviceHandle = rp.pop_raw();
        let aruid = rp.pop_u64();
        log::debug!(
            "IHidServer::GetActualVibrationGcErmCommand called, aruid={}",
            aruid
        );

        // Upstream: checks active aruid, gets GcVibrationDevice, calls GetActualVibrationGcErmCommand.
        // GcVibrationDevice not yet ported. Return Stop (0) matching upstream fallback.
        let rm = server.resource_manager.lock();
        let has_active_aruid = match rm.is_vibration_aruid_active(aruid) {
            Ok(v) => v,
            Err(_) => false,
        };
        let _ = vibration_device_handle;
        let gc_erm_command: u64 = if !has_active_aruid { 0 } else { 0 }; // Stop = 0

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(gc_erm_command);
    }

    // cmd 209: BeginPermitVibrationSession
    fn begin_permit_vibration_session(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!(
            "IHidServer::BeginPermitVibrationSession called, aruid={}",
            aruid
        );

        let rm = server.resource_manager.lock();
        let result = if let Some(ref npad) = rm.get_npad() {
            to_ipc_result(npad.lock().begin_permit_vibration_session(aruid))
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 210: EndPermitVibrationSession
    fn end_permit_vibration_session(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        log::debug!("IHidServer::EndPermitVibrationSession called");

        let rm = server.resource_manager.lock();
        let result = if let Some(ref npad) = rm.get_npad() {
            to_ipc_result(npad.lock().end_permit_vibration_session())
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 211: IsVibrationDeviceMounted
    fn is_vibration_device_mounted(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let vibration_device_handle: VibrationDeviceHandle = rp.pop_raw();
        let aruid = rp.pop_u64();
        log::debug!(
            "IHidServer::IsVibrationDeviceMounted called, aruid={}",
            aruid
        );
        let (result, is_mounted) = match server
            .resource_manager
            .lock()
            .is_vibration_device_mounted(&vibration_device_handle)
        {
            Ok(is_mounted) => (RESULT_SUCCESS, is_mounted),
            Err(result) => (to_ipc_result(result), false),
        };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_bool(is_mounted);
    }

    // cmd 212: SendVibrationValueInBool
    fn send_vibration_value_in_bool(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let is_vibrating = rp.pop_bool();
        let _padding1 = rp.pop_u8();
        let _padding2 = rp.pop_u16();
        let vibration_device_handle: VibrationDeviceHandle = rp.pop_raw();
        let aruid = rp.pop_u64();
        log::debug!(
            "IHidServer::SendVibrationValueInBool called, is_vibrating={}, aruid={}",
            is_vibrating,
            aruid
        );

        // Upstream checks IsVibrationAruidActive, validates handle, gets N64VibrationDevice.
        // N64VibrationDevice not yet ported. Return success.
        let rm = server.resource_manager.lock();
        let has_active_aruid = match rm.is_vibration_aruid_active(aruid) {
            Ok(v) => v,
            Err(_) => false,
        };
        let _ = vibration_device_handle;
        if !has_active_aruid {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_SUCCESS);
            return;
        }
        // N64VibrationDevice::SendValueInBool not yet ported.
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 300: ActivateConsoleSixAxisSensor
    fn activate_console_six_axis_sensor(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::info!(
            "IHidServer::ActivateConsoleSixAxisSensor called, aruid={}",
            aruid
        );

        let rm = server.resource_manager.lock();
        if !server.firmware_settings.is_device_managed() {
            if let Some(ref console_six_axis) = rm.get_console_six_axis() {
                let (result, _) = console_six_axis.lock().activation.activate();
                if result.is_error() {
                    let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                    rb.push_result(to_ipc_result(result));
                    return;
                }
            }
        }
        let result = if let Some(ref console_six_axis) = rm.get_console_six_axis() {
            let (r, _) = console_six_axis
                .lock()
                .activation
                .activate_with_aruid(aruid);
            to_ipc_result(r)
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 301: StartConsoleSixAxisSensor
    fn start_console_six_axis_sensor(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _console_sixaxis_handle: u32 = rp.pop_u32();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::StartConsoleSixAxisSensor called, aruid={}",
            aruid
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 302: StopConsoleSixAxisSensor
    fn stop_console_six_axis_sensor(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _console_sixaxis_handle: u32 = rp.pop_u32();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::StopConsoleSixAxisSensor called, aruid={}",
            aruid
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 303: ActivateSevenSixAxisSensor
    fn activate_seven_six_axis_sensor(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::info!(
            "IHidServer::ActivateSevenSixAxisSensor called, aruid={}",
            aruid
        );

        let rm = server.resource_manager.lock();
        if !server.firmware_settings.is_device_managed() {
            if let Some(ref seven_six_axis) = rm.get_seven_six_axis() {
                let (result, _) = seven_six_axis.lock().activation.activate();
                if result.is_error() {
                    let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                    rb.push_result(to_ipc_result(result));
                    return;
                }
            }
        }
        if let Some(ref seven_six_axis) = rm.get_seven_six_axis() {
            seven_six_axis.lock().activation.activate_with_aruid(aruid);
        }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 304: StartSevenSixAxisSensor
    fn start_seven_six_axis_sensor(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::StartSevenSixAxisSensor called, aruid={}",
            aruid
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 305: StopSevenSixAxisSensor
    fn stop_seven_six_axis_sensor(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::StopSevenSixAxisSensor called, aruid={}",
            aruid
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 306: InitializeSevenSixAxisSensor
    fn initialize_seven_six_axis_sensor(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        let t_mem_1_size = rp.pop_u64();
        let t_mem_2_size = rp.pop_u64();
        log::warn!("IHidServer::InitializeSevenSixAxisSensor called, t_mem_1_size=0x{:08X}, t_mem_2_size=0x{:08X}, aruid={}",
            t_mem_1_size, t_mem_2_size, aruid);

        // Upstream asserts sizes are 0x1000 and 0x7F000 respectively.
        // Activate console six axis and seven six axis controllers.
        let rm = server.resource_manager.lock();
        if let Some(ref console_six_axis) = rm.get_console_six_axis() {
            console_six_axis.lock().activation.activate();
        }
        if let Some(ref seven_six_axis) = rm.get_seven_six_axis() {
            seven_six_axis.lock().activation.activate();
            // Upstream: seven_six_axis->SetTransferMemoryAddress(t_mem_1->GetSourceAddress());
            // Transfer memory is not yet wired. Skip the address set.
        }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 307: FinalizeSevenSixAxisSensor
    fn finalize_seven_six_axis_sensor(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::FinalizeSevenSixAxisSensor called, aruid={}",
            aruid
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 310: ResetSevenSixAxisSensorTimestamp
    fn reset_seven_six_axis_sensor_timestamp(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::warn!(
            "IHidServer::ResetSevenSixAxisSensorTimestamp called, aruid={}",
            aruid
        );

        let rm = server.resource_manager.lock();
        if let Some(ref seven_six_axis) = rm.get_seven_six_axis() {
            seven_six_axis.lock().reset_timestamp();
        }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 400: IsUsbFullKeyControllerEnabled
    fn is_usb_full_key_controller_enabled(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) IHidServer::IsUsbFullKeyControllerEnabled called");
        Self::trace_npad(400, RESULT_SUCCESS, 0, 0, 0, 0);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(false);
    }

    // cmd 500: GetPalmaConnectionHandle
    fn get_palma_connection_handle(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let npad_id_raw = rp.pop_u32();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::GetPalmaConnectionHandle called, npad_id=0x{:X}, aruid={}",
            npad_id_raw,
            aruid
        );
        // Upstream delegates to palma->GetPalmaConnectionHandle. Return a zeroed handle.
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(0); // PalmaConnectionHandle
    }

    // cmd 501: InitializePalma
    fn initialize_palma(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::InitializePalma called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 502: AcquirePalmaOperationCompleteEvent
    fn acquire_palma_operation_complete_event(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::AcquirePalmaOperationCompleteEvent called");
        // Upstream returns a KReadableEvent. Return a null copy handle.
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_objects(0);
    }

    // cmd 503: GetPalmaOperationInfo
    fn get_palma_operation_info(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::GetPalmaOperationInfo called");
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(0); // operation_type
    }

    // cmd 504: PlayPalmaActivity
    fn play_palma_activity(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        let palma_activity = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::PlayPalmaActivity called, palma_activity={}",
            palma_activity
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 505: SetPalmaFrModeType
    fn set_palma_fr_mode_type(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        let fr_mode = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::SetPalmaFrModeType called, fr_mode={}",
            fr_mode
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 506: ReadPalmaStep
    fn read_palma_step(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::ReadPalmaStep called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 507: EnablePalmaStep
    fn enable_palma_step(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let is_enabled = rp.pop_bool();
        let _padding1 = rp.pop_u8();
        let _padding2 = rp.pop_u16();
        let _padding3 = rp.pop_u32();
        let _connection_handle = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::EnablePalmaStep called, is_enabled={}",
            is_enabled
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 508: ResetPalmaStep
    fn reset_palma_step(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::ResetPalmaStep called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 509: ReadPalmaApplicationSection
    fn read_palma_application_section(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        let offset = rp.pop_u64();
        let size = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::ReadPalmaApplicationSection called, offset={}, size={}",
            offset,
            size
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 510: WritePalmaApplicationSection
    fn write_palma_application_section(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        let offset = rp.pop_u64();
        let size = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::WritePalmaApplicationSection called, offset={}, size={}",
            offset,
            size
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 511: ReadPalmaUniqueCode
    fn read_palma_unique_code(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::ReadPalmaUniqueCode called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 512: SetPalmaUniqueCodeInvalid
    fn set_palma_unique_code_invalid(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::SetPalmaUniqueCodeInvalid called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 513: WritePalmaActivityEntry
    fn write_palma_activity_entry(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::WritePalmaActivityEntry called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 514: WritePalmaRgbLedPatternEntry
    fn write_palma_rgb_led_pattern_entry(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        let unknown = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::WritePalmaRgbLedPatternEntry called, unknown={}",
            unknown
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 515: WritePalmaWaveEntry
    fn write_palma_wave_entry(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        let _wave_set = rp.pop_u64();
        let _unknown = rp.pop_u64();
        let _t_mem_size = rp.pop_u64();
        let _size = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::WritePalmaWaveEntry called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 516: SetPalmaDataBaseIdentificationVersion
    fn set_palma_data_base_identification_version(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let database_id_version = rp.pop_u32();
        let _padding = rp.pop_u32();
        let _connection_handle = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::SetPalmaDataBaseIdentificationVersion called, database_id_version={}", database_id_version);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 517: GetPalmaDataBaseIdentificationVersion
    fn get_palma_data_base_identification_version(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::GetPalmaDataBaseIdentificationVersion called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 518: SuspendPalmaFeature
    fn suspend_palma_feature(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let feature = rp.pop_u64();
        let _connection_handle = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::SuspendPalmaFeature called, feature={}",
            feature
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 519: GetPalmaOperationResult
    fn get_palma_operation_result(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::GetPalmaOperationResult called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 520: ReadPalmaPlayLog
    fn read_palma_play_log(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let unknown = rp.pop_u16();
        let _padding = rp.pop_u16();
        let _padding2 = rp.pop_u32();
        let _connection_handle = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::ReadPalmaPlayLog called, unknown={}",
            unknown
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 521: ResetPalmaPlayLog
    fn reset_palma_play_log(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let unknown = rp.pop_u16();
        let _padding = rp.pop_u16();
        let _padding2 = rp.pop_u32();
        let _connection_handle = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::ResetPalmaPlayLog called, unknown={}",
            unknown
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 522: SetIsPalmaAllConnectable
    fn set_is_palma_all_connectable(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let is_palma_all_connectable = rp.pop_bool();
        let _padding1 = rp.pop_u8();
        let _padding2 = rp.pop_u16();
        let _padding3 = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::SetIsPalmaAllConnectable called, is_palma_all_connectable={}, aruid={}", is_palma_all_connectable, aruid);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 523: SetIsPalmaPairedConnectable
    fn set_is_palma_paired_connectable(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let is_palma_paired_connectable = rp.pop_bool();
        let _padding1 = rp.pop_u8();
        let _padding2 = rp.pop_u16();
        let _padding3 = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::SetIsPalmaPairedConnectable called, is_palma_paired_connectable={}, aruid={}", is_palma_paired_connectable, aruid);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 524: PairPalma
    fn pair_palma(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::PairPalma called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 525: SetPalmaBoostMode
    fn set_palma_boost_mode(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let is_enabled = rp.pop_bool();
        log::warn!(
            "(STUBBED) IHidServer::SetPalmaBoostMode called, is_enabled={}",
            is_enabled
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 526: CancelWritePalmaWaveEntry
    fn cancel_write_palma_wave_entry(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::CancelWritePalmaWaveEntry called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 527: EnablePalmaBoostMode
    fn enable_palma_boost_mode(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let is_enabled = rp.pop_bool();
        let _padding1 = rp.pop_u8();
        let _padding2 = rp.pop_u16();
        let _padding3 = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::warn!(
            "(STUBBED) IHidServer::EnablePalmaBoostMode called, is_enabled={}, aruid={}",
            is_enabled,
            aruid
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 528: GetPalmaBluetoothAddress
    fn get_palma_bluetooth_address(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _connection_handle = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::GetPalmaBluetoothAddress called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 529: SetDisallowedPalmaConnection
    fn set_disallowed_palma_connection(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!(
            "(STUBBED) IHidServer::SetDisallowedPalmaConnection called, aruid={}",
            aruid
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 1000: SetNpadCommunicationMode — stubbed since 2.0.0+
    fn set_npad_communication_mode(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        let communication_mode = rp.pop_u64();
        log::debug!(
            "IHidServer::SetNpadCommunicationMode called, aruid={}, communication_mode={}",
            aruid,
            communication_mode
        );
        // This function has been stubbed since 2.0.0+
        Self::trace_npad(1000, RESULT_SUCCESS, aruid, communication_mode, 0, 0);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    // cmd 1001: GetNpadCommunicationMode — stubbed since 2.0.0+
    fn get_npad_communication_mode(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();
        log::debug!(
            "IHidServer::GetNpadCommunicationMode called, aruid={}",
            aruid
        );
        // This function has been stubbed since 2.0.0+
        // Return NpadCommunicationMode::Default (0)
        Self::trace_npad(
            1001,
            RESULT_SUCCESS,
            aruid,
            0,
            0,
            NpadCommunicationMode::Default as u64,
        );
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(NpadCommunicationMode::Default as u64);
    }

    // cmd 1002: SetTouchScreenConfiguration
    fn set_touch_screen_configuration(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let touchscreen_config: TouchScreenConfigurationForNx = rp.pop_raw();
        let aruid = rp.pop_u64();
        log::info!(
            "IHidServer::SetTouchScreenConfiguration called, mode={:?}, aruid={}",
            touchscreen_config.mode,
            aruid
        );

        let rm = server.resource_manager.lock();
        let result = if let (Some(touch_screen), Some(touch_resource)) =
            (rm.get_touch_screen(), rm.get_touch_resource())
        {
            to_ipc_result(touch_screen.lock().set_touch_screen_configuration(
                &mut touch_resource.lock(),
                &touchscreen_config,
                aruid,
            ))
        } else {
            RESULT_SUCCESS
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 1003: IsFirmwareUpdateNeededForNotification
    fn is_firmware_update_needed_for_notification(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let unknown = rp.pop_u32();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::warn!("(STUBBED) IHidServer::IsFirmwareUpdateNeededForNotification called, unknown={}, aruid={}", unknown, aruid);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(false);
    }

    // cmd 1004: SetTouchScreenResolution
    fn set_touch_screen_resolution(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let width = rp.pop_u32();
        let height = rp.pop_u32();
        let aruid = rp.pop_u64();
        log::info!(
            "IHidServer::SetTouchScreenResolution called, width={}, height={}, aruid={}",
            width,
            height,
            aruid
        );

        let rm = server.resource_manager.lock();
        let result = if let (Some(touch_screen), Some(touch_resource)) =
            (rm.get_touch_screen(), rm.get_touch_resource())
        {
            to_ipc_result(touch_screen.lock().set_touch_screen_resolution(
                &mut touch_resource.lock(),
                width,
                height,
                aruid,
            ))
        } else {
            RESULT_SUCCESS
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    // cmd 2000: ActivateDigitizer — upstream has nullptr handler
    fn activate_digitizer(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) IHidServer::ActivateDigitizer called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }
}

impl SessionRequestHandler for IHidServer {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "hid"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl ServiceFramework for IHidServer {
    fn get_service_name(&self) -> &str {
        "hid"
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

    #[test]
    fn create_applet_resource_reports_success_and_returns_interface_after_manager_error() {
        let firmware_settings = Arc::new(HidFirmwareSettings::new());
        let resource_manager = Arc::new(parking_lot::Mutex::new(ResourceManager::new(
            Arc::clone(&firmware_settings),
            Arc::new(parking_lot::Mutex::new(HIDCore::new())),
        )));
        let aruid = 0x1122_3344_5566_7788;

        // No shared-memory backing is installed, so the manager call fails. Eden logs this
        // diagnostic result but still returns a successful IPC interface.
        assert!(resource_manager
            .lock()
            .create_applet_resource(aruid)
            .is_error());

        let server = IHidServer::new(
            SystemRef::null(),
            resource_manager,
            firmware_settings,
            Arc::new(parking_lot::Mutex::new(BTreeMap::new())),
        );
        let mut ctx = HLERequestContext::new();
        ctx.cmd_buf[2] = aruid as u32;
        ctx.cmd_buf[3] = (aruid >> 32) as u32;

        IHidServer::create_applet_resource(&server, &mut ctx);

        assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
        assert_eq!(ctx.outgoing_move_objects.len(), 1);
    }
}

//! Port of zuyu/src/core/hle/service/hid/hid_system_server.h and hid_system_server.cpp
//!
//! IHidSystemServer service ("hid:sys").

use std::collections::BTreeMap;
use std::sync::Arc;

use hid_core::hid_types::{NpadIdType, NpadStyleSet};
use hid_core::resource_manager::ResourceManager;
use hid_core::resources::hid_firmware_settings::HidFirmwareSettings;

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

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SetNpadSystemExtStateEnabledParameters {
    is_enabled: bool,
    _padding: [u8; 7],
    applet_resource_user_id: u64,
}

const _: () = assert!(std::mem::size_of::<SetNpadSystemExtStateEnabledParameters>() == 0x10);

/// IHidSystemServer - system-level HID interface.
pub struct IHidSystemServer {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    resource_manager: Arc<parking_lot::Mutex<ResourceManager>>,
    firmware_settings: Arc<HidFirmwareSettings>,
    acquire_device_registered_event: Arc<Event>,
    joy_detach_event: Event,
}

impl IHidSystemServer {
    // ===== Commands that are nullptr in upstream (no handler) but we need a named stub =====

    /// Upstream: nullptr (cmd 31)
    fn send_keyboard_lock_key_event_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SendKeyboardLockKeyEvent called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 101)
    fn acquire_home_button_event_handle_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) AcquireHomeButtonEventHandle called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 111)
    fn activate_home_button_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) ActivateHomeButton called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 121)
    fn acquire_sleep_button_event_handle_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) AcquireSleepButtonEventHandle called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 131)
    fn activate_sleep_button_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) ActivateSleepButton called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 141)
    fn acquire_capture_button_event_handle_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) AcquireCaptureButtonEventHandle called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 151)
    fn activate_capture_button_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) ActivateCaptureButton called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::GetPlatformConfig (cmd 161)
    fn get_platform_config_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let platform_config = server.firmware_settings.get_platform_config();

        log::info!(
            "GetPlatformConfig called, platform_config={}",
            platform_config.raw
        );

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(platform_config.raw);
    }

    /// Upstream: nullptr (cmd 210)
    fn acquire_nfc_device_update_event_handle_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) AcquireNfcDeviceUpdateEventHandle called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 211)
    fn get_npads_with_nfc_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) GetNpadsWithNfc called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 212)
    fn acquire_nfc_activate_event_handle_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) AcquireNfcActivateEventHandle called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 213)
    fn activate_nfc_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) ActivateNfc called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 214)
    fn get_xcd_handle_for_npad_with_nfc_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetXcdHandleForNpadWithNfc called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 215)
    fn is_nfc_activated_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) IsNfcActivated called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 230)
    fn acquire_ir_sensor_event_handle_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) AcquireIrSensorEventHandle called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 231)
    fn activate_ir_sensor_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) ActivateIrSensor called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 232)
    fn get_ir_sensor_state_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) GetIrSensorState called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 233)
    fn get_xcd_handle_for_npad_with_ir_sensor_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetXcdHandleForNpadWithIrSensor called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 301)
    fn activate_npad_system_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) ActivateNpadSystem called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::ApplyNpadSystemCommonPolicy (cmd 303)
    fn apply_npad_system_common_policy_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();

        log::info!("ApplyNpadSystemCommonPolicy called, aruid={}", aruid);

        let rm = server.resource_manager.lock();
        let result = if let Some(npad) = rm.get_npad() {
            to_ipc_result(npad.lock().apply_npad_system_common_policy(aruid))
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// Upstream: IHidSystemServer::EnableAssigningSingleOnSlSrPress (cmd 304)
    fn enable_assigning_single_on_sl_sr_press_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();

        log::info!("EnableAssigningSingleOnSlSrPress called, aruid={}", aruid);

        let rm = server.resource_manager.lock();
        if let Some(npad) = rm.get_npad() {
            npad.lock().assigning_single_on_sl_sr_press(aruid, true);
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::DisableAssigningSingleOnSlSrPress (cmd 305)
    fn disable_assigning_single_on_sl_sr_press_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();

        log::info!("DisableAssigningSingleOnSlSrPress called, aruid={}", aruid);

        let rm = server.resource_manager.lock();
        if let Some(npad) = rm.get_npad() {
            npad.lock().assigning_single_on_sl_sr_press(aruid, false);
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::GetLastActiveNpad (cmd 306)
    fn get_last_active_npad_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };

        let rm = server.resource_manager.lock();
        let (result, npad_id) = if let Some(npad) = rm.get_npad() {
            npad.lock().get_last_active_npad()
        } else {
            (
                common::ResultCode::SUCCESS,
                hid_core::hid_types::NpadIdType::Player1,
            )
        };

        log::debug!("GetLastActiveNpad called, npad_id={:?}", npad_id);

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(to_ipc_result(result));
        rb.push_u32(npad_id as u32);
    }

    /// Upstream: nullptr (cmd 307)
    fn get_npad_system_ext_style_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetNpadSystemExtStyle called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::ApplyNpadSystemCommonPolicyFull (cmd 308)
    fn apply_npad_system_common_policy_full_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();

        log::info!("ApplyNpadSystemCommonPolicyFull called, aruid={}", aruid);

        let rm = server.resource_manager.lock();
        let result = if let Some(npad) = rm.get_npad() {
            to_ipc_result(npad.lock().apply_npad_system_common_policy_full(aruid))
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// Upstream: IHidSystemServer::GetNpadFullKeyGripColor (cmd 309)
    fn get_npad_full_key_grip_color_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let npad_id = rp.pop_u32();

        log::debug!(
            "(STUBBED) GetNpadFullKeyGripColor called, npad_id={}",
            npad_id
        );

        // Upstream also has a TODO here: "Get colors from Npad"
        // Default-initialized NpadColor (all zeros) matches upstream stub behavior.
        let left_color: u32 = 0;
        let right_color: u32 = 0;

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(left_color);
        rb.push_u32(right_color);
    }

    /// Upstream: IHidSystemServer::GetMaskedSupportedNpadStyleSet (cmd 310)
    fn get_masked_supported_npad_style_set_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();

        log::info!("GetMaskedSupportedNpadStyleSet called, aruid={}", aruid);

        let rm = server.resource_manager.lock();
        let (result, style_set) = if let Some(npad) = rm.get_npad() {
            npad.lock().get_masked_supported_npad_style_set(aruid)
        } else {
            (common::ResultCode::SUCCESS, NpadStyleSet::NONE)
        };

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(to_ipc_result(result));
        rb.push_u32(style_set.bits());
    }

    /// Upstream: nullptr (cmd 311)
    fn set_npad_player_led_blinking_device_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetNpadPlayerLedBlinkingDevice called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::SetSupportedNpadStyleSetAll (cmd 312)
    fn set_supported_npad_style_set_all_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();

        log::info!("SetSupportedNpadStyleSetAll called, aruid={}", aruid);

        let rm = server.resource_manager.lock();
        if let Some(npad) = rm.get_npad() {
            npad.lock()
                .set_supported_npad_style_set(aruid, NpadStyleSet::ALL);
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::GetNpadCaptureButtonAssignment (cmd 313)
    fn get_npad_capture_button_assignment_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();

        log::debug!(
            "(STUBBED) GetNpadCaptureButtonAssignment called, aruid={}",
            aruid
        );

        // Stubbed: return empty list (count = 0)
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(0);
    }

    /// Upstream: IHidSystemServer::GetAppletDetailedUiType (cmd 315)
    fn get_applet_detailed_ui_type_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let npad_id = rp.pop_u32();
        let npad_id_type = match npad_id {
            0 => NpadIdType::Player1,
            1 => NpadIdType::Player2,
            2 => NpadIdType::Player3,
            3 => NpadIdType::Player4,
            4 => NpadIdType::Player5,
            5 => NpadIdType::Player6,
            6 => NpadIdType::Player7,
            7 => NpadIdType::Player8,
            0x10 => NpadIdType::Other,
            0x20 => NpadIdType::Handheld,
            _ => NpadIdType::Invalid,
        };

        let detailed_ui_type = server
            .resource_manager
            .lock()
            .get_npad()
            .map(|npad| npad.lock().get_applet_detailed_ui_type(npad_id_type))
            .unwrap_or_default();

        log::debug!(
            "GetAppletDetailedUiType called, npad_id={} footer={:?}",
            npad_id,
            detailed_ui_type.footer
        );

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&detailed_ui_type);
    }

    /// Upstream: IHidSystemServer::GetNpadInterfaceType (cmd 316)
    fn get_npad_interface_type_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let npad_id = rp.pop_u32();

        log::debug!("(STUBBED) GetNpadInterfaceType called, npad_id={}", npad_id);

        // Upstream returns NpadInterfaceType::Bluetooth (1)
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u8(1); // NpadInterfaceType::Bluetooth
    }

    /// Upstream: IHidSystemServer::GetNpadLeftRightInterfaceType (cmd 317)
    fn get_npad_left_right_interface_type_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let npad_id = rp.pop_u32();

        log::debug!(
            "(STUBBED) GetNpadLeftRightInterfaceType called, npad_id={}",
            npad_id
        );

        // Upstream returns two NpadInterfaceType::Bluetooth (each u8)
        // ResponseBuilder sizes: 2 base + 2 data words = 4 (two u8s fit in one u32 word,
        // but upstream uses rb{ctx, 4} = 2 extra data words for two PushEnum<u8>)
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u8(1); // left: NpadInterfaceType::Bluetooth
        rb.push_u8(1); // right: NpadInterfaceType::Bluetooth
    }

    /// Upstream: IHidSystemServer::HasBattery (cmd 318)
    fn has_battery_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let npad_id = rp.pop_u32();

        log::debug!("(STUBBED) HasBattery called, npad_id={}", npad_id);

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(false);
    }

    /// Upstream: IHidSystemServer::HasLeftRightBattery (cmd 319)
    fn has_left_right_battery_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let npad_id = rp.pop_u32();

        log::debug!("(STUBBED) HasLeftRightBattery called, npad_id={}", npad_id);

        // Upstream pushes struct { bool left; bool right; } as raw
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(false); // left
        rb.push_bool(false); // right
    }

    /// Upstream: IHidSystemServer::GetUniquePadsFromNpad (cmd 321)
    fn get_unique_pads_from_npad_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let npad_id = rp.pop_u32();

        log::debug!(
            "(STUBBED) GetUniquePadsFromNpad called, npad_id={}",
            npad_id
        );

        // Stubbed: return empty list (count = 0)
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(0);
    }

    /// Upstream: IHidSystemServer::SetNpadSystemExtStateEnabled (cmd 322)
    fn set_npad_system_ext_state_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let parameters = rp.pop_raw::<SetNpadSystemExtStateEnabledParameters>();

        log::info!(
            "SetNpadSystemExtStateEnabled called, is_enabled={}, aruid={}",
            parameters.is_enabled,
            parameters.applet_resource_user_id
        );

        let rm = server.resource_manager.lock();
        let result = if let Some(npad) = rm.get_npad() {
            to_ipc_result(npad.lock().set_npad_system_ext_state_enabled(
                parameters.applet_resource_user_id,
                parameters.is_enabled,
            ))
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// Upstream: nullptr (cmd 328)
    fn attach_abstracted_pad_to_npad_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) AttachAbstractedPadToNpad called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 329)
    fn detach_abstracted_pad_all_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) DetachAbstractedPadAll called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 330)
    fn check_abstracted_pad_connection_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) CheckAbstractedPadConnection called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 500)
    fn set_applet_resource_user_id_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetAppletResourceUserId called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::RegisterAppletResourceUserId (cmd 501)
    fn register_applet_resource_user_id_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let enable_input = rp.pop_bool();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();

        log::info!(
            "RegisterAppletResourceUserId called, enable_input={}, aruid={}",
            enable_input,
            aruid
        );

        let result = server
            .resource_manager
            .lock()
            .register_applet_resource_user_id(aruid, enable_input);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(to_ipc_result(result));
    }

    /// Upstream: IHidSystemServer::UnregisterAppletResourceUserId (cmd 502)
    fn unregister_applet_resource_user_id_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();

        log::info!("UnregisterAppletResourceUserId called, aruid={}", aruid);

        server
            .resource_manager
            .lock()
            .unregister_applet_resource_user_id(aruid);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::EnableAppletToGetInput (cmd 503)
    fn enable_applet_to_get_input_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let is_enabled = rp.pop_bool();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();

        log::info!(
            "EnableAppletToGetInput called, is_enabled={}, aruid={}",
            is_enabled,
            aruid
        );

        server
            .resource_manager
            .lock()
            .enable_input(aruid, is_enabled);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::SetAruidValidForVibration (cmd 504)
    fn set_aruid_valid_for_vibration_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let is_enabled = rp.pop_bool();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();

        log::info!(
            "SetAruidValidForVibration called, is_enabled={}, aruid={}",
            is_enabled,
            aruid
        );

        server
            .resource_manager
            .lock()
            .set_aruid_valid_for_vibration(aruid, is_enabled);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::EnableAppletToGetSixAxisSensor (cmd 505)
    /// Note: upstream calls EnableTouchScreen despite the name.
    fn enable_applet_to_get_six_axis_sensor_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let is_enabled = rp.pop_bool();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();

        log::info!(
            "EnableAppletToGetSixAxisSensor called, is_enabled={}, aruid={}",
            is_enabled,
            aruid
        );

        // Upstream calls EnableTouchScreen here (yes, this is intentional in upstream)
        server
            .resource_manager
            .lock()
            .enable_touch_screen(aruid, is_enabled);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::EnableAppletToGetPadInput (cmd 506)
    fn enable_applet_to_get_pad_input_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let is_enabled = rp.pop_bool();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();

        log::info!(
            "EnableAppletToGetPadInput called, is_enabled={}, aruid={}",
            is_enabled,
            aruid
        );

        let rm = server.resource_manager.lock();
        rm.enable_pad_input(aruid, is_enabled);
        if let Some(npad) = rm.get_npad() {
            npad.lock().enable_applet_to_get_input(aruid);
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::EnableAppletToGetTouchScreen (cmd 507)
    fn enable_applet_to_get_touch_screen_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let is_enabled = rp.pop_bool();
        let _padding = rp.pop_u32();
        let aruid = rp.pop_u64();

        log::info!(
            "EnableAppletToGetTouchScreen called, is_enabled={}, aruid={}",
            is_enabled,
            aruid
        );

        server
            .resource_manager
            .lock()
            .enable_touch_screen(aruid, is_enabled);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::SetVibrationMasterVolume (cmd 510)
    fn set_vibration_master_volume_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let master_volume = rp.pop_f32();

        log::info!("SetVibrationMasterVolume called, volume={}", master_volume);

        let result = if let Some(npad) = server.resource_manager.lock().get_npad() {
            to_ipc_result(npad.lock().set_vibration_master_volume(master_volume))
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// Upstream: IHidSystemServer::GetVibrationMasterVolume (cmd 511)
    fn get_vibration_master_volume_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };

        let (result, master_volume) = if let Some(npad) = server.resource_manager.lock().get_npad()
        {
            match npad.lock().get_vibration_master_volume() {
                Ok(vol) => (RESULT_SUCCESS, vol),
                Err(e) => (to_ipc_result(e), 0.0f32),
            }
        } else {
            (RESULT_SUCCESS, 1.0f32)
        };

        log::info!("GetVibrationMasterVolume called, volume={}", master_volume);

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_f32(master_volume);
    }

    /// Upstream: IHidSystemServer::BeginPermitVibrationSession (cmd 512)
    fn begin_permit_vibration_session_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        let mut rp = RequestParser::new(ctx);
        let aruid = rp.pop_u64();

        log::info!("BeginPermitVibrationSession called, aruid={}", aruid);

        let result = if let Some(npad) = server.resource_manager.lock().get_npad() {
            to_ipc_result(npad.lock().begin_permit_vibration_session(aruid))
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// Upstream: IHidSystemServer::EndPermitVibrationSession (cmd 513)
    fn end_permit_vibration_session_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        log::info!("EndPermitVibrationSession called");

        let result = if let Some(npad) = server.resource_manager.lock().get_npad() {
            to_ipc_result(npad.lock().end_permit_vibration_session())
        } else {
            RESULT_SUCCESS
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// Upstream: nullptr (cmd 514)
    fn unknown514_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) Unknown514 called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 520)
    fn enable_handheld_hids_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) EnableHandheldHids called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 521)
    fn disable_handheld_hids_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) DisableHandheldHids called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 522)
    fn set_joy_con_rail_enabled_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) SetJoyConRailEnabled called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::IsJoyConRailEnabled (cmd 523)
    fn is_joy_con_rail_enabled_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let is_attached = true;
        log::warn!(
            "(STUBBED) IsJoyConRailEnabled called, is_attached={}",
            is_attached
        );

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(is_attached);
    }

    /// Upstream: nullptr (cmd 524)
    fn is_handheld_hids_enabled_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) IsHandheldHidsEnabled called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::IsJoyConAttachedOnAllRail (cmd 525)
    fn is_joy_con_attached_on_all_rail_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let is_attached = true;
        log::debug!(
            "(STUBBED) IsJoyConAttachedOnAllRail called, is_attached={}",
            is_attached
        );

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(is_attached);
    }

    /// Upstream: IHidSystemServer::AcquireConnectionTriggerTimeoutEvent (cmd 544)
    fn acquire_connection_trigger_timeout_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        log::info!("AcquireConnectionTriggerTimeoutEvent called");

        let handle = server
            .acquire_device_registered_event
            .copy_handle(ctx)
            .unwrap_or(0);
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_objects(handle);
    }

    /// Upstream: IHidSystemServer::AcquireDeviceRegisteredEventForControllerSupport (cmd 546)
    fn acquire_device_registered_event_for_controller_support_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let server = unsafe { &*(this as *const dyn ServiceFramework as *const IHidSystemServer) };
        log::info!("AcquireDeviceRegisteredEventForControllerSupport called");

        let handle = server
            .acquire_device_registered_event
            .copy_handle(ctx)
            .unwrap_or(0);
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_objects(handle);
    }

    /// Upstream: IHidSystemServer::GetRegisteredDevices (cmd 548)
    fn get_registered_devices_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) GetRegisteredDevices called");

        // Upstream returns empty list with count = 0
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(0);
    }

    /// Upstream: nullptr (cmd 549)
    fn get_connectable_registered_devices_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetConnectableRegisteredDevices called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 700)
    fn activate_unique_pad_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) ActivateUniquePad called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::AcquireUniquePadConnectionEventHandle (cmd 702)
    fn acquire_unique_pad_connection_event_handle_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) AcquireUniquePadConnectionEventHandle called");

        // Upstream: rb{ctx, 2, 1} + PushCopyObjects(event). Stubbed with handle=0.
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_objects(0);
    }

    /// Upstream: IHidSystemServer::GetUniquePadIds (cmd 703)
    fn get_unique_pad_ids_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) GetUniquePadIds called");

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(0);
    }

    /// Upstream: IHidSystemServer::AcquireJoyDetachOnBluetoothOffEventHandle (cmd 751)
    fn acquire_joy_detach_on_bluetooth_off_event_handle_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("(STUBBED) AcquireJoyDetachOnBluetoothOffEventHandle called");

        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let Some(id) = service.joy_detach_event.copy_object_id(ctx) else {
            ResponseBuilder::new(ctx, 2, 0, 0).push_result(crate::hle::result::RESULT_UNKNOWN);
            return;
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(id);
    }

    /// Upstream: nullptr (cmd 800)
    fn list_six_axis_sensor_handles_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) ListSixAxisSensorHandles called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 801)
    fn is_six_axis_sensor_user_calibration_supported_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsSixAxisSensorUserCalibrationSupported called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 802)
    fn reset_six_axis_sensor_calibration_values_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) ResetSixAxisSensorCalibrationValues called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 803)
    fn start_six_axis_sensor_user_calibration_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) StartSixAxisSensorUserCalibration called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 804)
    fn cancel_six_axis_sensor_user_calibration_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) CancelSixAxisSensorUserCalibration called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 805)
    fn get_unique_pad_bluetooth_address_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetUniquePadBluetoothAddress called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 806)
    fn disconnect_unique_pad_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) DisconnectUniquePad called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 807)
    fn get_unique_pad_type_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) GetUniquePadType called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 808)
    fn get_unique_pad_interface_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) GetUniquePadInterface called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 809)
    fn get_unique_pad_serial_number_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetUniquePadSerialNumber called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 810)
    fn get_unique_pad_controller_number_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetUniquePadControllerNumber called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 811)
    fn get_six_axis_sensor_user_calibration_stage_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetSixAxisSensorUserCalibrationStage called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 812)
    fn get_console_unique_six_axis_sensor_handle_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetConsoleUniqueSixAxisSensorHandle called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 821)
    fn start_analog_stick_manual_calibration_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) StartAnalogStickManualCalibration called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 822)
    fn retry_current_analog_stick_manual_calibration_stage_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) RetryCurrentAnalogStickManualCalibrationStage called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 823)
    fn cancel_analog_stick_manual_calibration_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) CancelAnalogStickManualCalibration called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 824)
    fn reset_analog_stick_manual_calibration_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) ResetAnalogStickManualCalibration called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 825)
    fn get_analog_stick_state_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) GetAnalogStickState called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 826)
    fn get_analog_stick_manual_calibration_stage_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetAnalogStickManualCalibrationStage called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 827)
    fn is_analog_stick_button_pressed_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsAnalogStickButtonPressed called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 828)
    fn is_analog_stick_in_release_position_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsAnalogStickInReleasePosition called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 829)
    fn is_analog_stick_in_circumference_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsAnalogStickInCircumference called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 830)
    fn set_notification_led_pattern_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetNotificationLedPattern called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 831)
    fn set_notification_led_pattern_with_timeout_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetNotificationLedPatternWithTimeout called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 832)
    fn prepare_hids_for_notification_wake_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) PrepareHidsForNotificationWake called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::IsUsbFullKeyControllerEnabled (cmd 850)
    fn is_usb_full_key_controller_enabled_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let is_enabled = false;
        log::warn!(
            "(STUBBED) IsUsbFullKeyControllerEnabled called, is_enabled={}",
            is_enabled
        );

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(is_enabled);
    }

    /// Upstream: IHidSystemServer::EnableUsbFullKeyController (cmd 851)
    fn enable_usb_full_key_controller_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let is_enabled = rp.pop_bool();

        log::warn!(
            "(STUBBED) EnableUsbFullKeyController called, is_enabled={}",
            is_enabled
        );

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 852)
    fn is_usb_connected_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) IsUsbConnected called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::IsHandheldButtonPressedOnConsoleMode (cmd 870)
    fn is_handheld_button_pressed_on_console_mode_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let button_pressed = false;
        log::debug!(
            "(STUBBED) IsHandheldButtonPressedOnConsoleMode called, is_enabled={}",
            button_pressed
        );

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(button_pressed);
    }

    /// Upstream: nullptr (cmd 900)
    fn activate_input_detector_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) ActivateInputDetector called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 901)
    fn notify_input_detector_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) NotifyInputDetector called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::InitializeFirmwareUpdate (cmd 1000)
    fn initialize_firmware_update_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) InitializeFirmwareUpdate called");

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1001)
    fn get_firmware_version_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) GetFirmwareVersion called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1002)
    fn get_available_firmware_version_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetAvailableFirmwareVersion called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1003)
    fn is_firmware_update_available_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsFirmwareUpdateAvailable called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::CheckFirmwareUpdateRequired (cmd 1004)
    fn check_firmware_update_required_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) CheckFirmwareUpdateRequired called");

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1005)
    fn start_firmware_update_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) StartFirmwareUpdate called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1006)
    fn abort_firmware_update_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) AbortFirmwareUpdate called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1007)
    fn get_firmware_update_state_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetFirmwareUpdateState called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1008)
    fn activate_audio_control_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) ActivateAudioControl called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1009)
    fn acquire_audio_control_event_handle_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) AcquireAudioControlEventHandle called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1010)
    fn get_audio_control_states_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) GetAudioControlStates called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1011)
    fn deactivate_audio_control_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) DeactivateAudioControl called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1050)
    fn is_six_axis_sensor_accurate_user_calibration_supported_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsSixAxisSensorAccurateUserCalibrationSupported called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1051)
    fn start_six_axis_sensor_accurate_user_calibration_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) StartSixAxisSensorAccurateUserCalibration called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1052)
    fn cancel_six_axis_sensor_accurate_user_calibration_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) CancelSixAxisSensorAccurateUserCalibration called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1053)
    fn get_six_axis_sensor_accurate_user_calibration_state_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetSixAxisSensorAccurateUserCalibrationState called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1100)
    fn get_hidbus_system_service_object_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetHidbusSystemServiceObject called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::SetFirmwareHotfixUpdateSkipEnabled (cmd 1120)
    fn set_firmware_hotfix_update_skip_enabled_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) SetFirmwareHotfixUpdateSkipEnabled called");

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::InitializeUsbFirmwareUpdate (cmd 1130)
    fn initialize_usb_firmware_update_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) InitializeUsbFirmwareUpdate called");

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::FinalizeUsbFirmwareUpdate (cmd 1131)
    fn finalize_usb_firmware_update_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) FinalizeUsbFirmwareUpdate called");

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::CheckUsbFirmwareUpdateRequired (cmd 1132)
    fn check_usb_firmware_update_required_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) CheckUsbFirmwareUpdateRequired called");

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1133)
    fn start_usb_firmware_update_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) StartUsbFirmwareUpdate called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1134)
    fn get_usb_firmware_update_state_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetUsbFirmwareUpdateState called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::InitializeUsbFirmwareUpdateWithoutMemory (cmd 1135)
    fn initialize_usb_firmware_update_without_memory_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) InitializeUsbFirmwareUpdateWithoutMemory called");

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::SetTouchScreenMagnification (cmd 1150)
    fn set_touch_screen_magnification_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let point1x = rp.pop_f32();
        let point1y = rp.pop_f32();
        let point2x = rp.pop_f32();
        let point2y = rp.pop_f32();

        log::info!(
            "(STUBBED) SetTouchScreenMagnification called, point1=({},{}), point2=({},{})",
            point1x,
            point1y,
            point2x,
            point2y
        );

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::GetTouchScreenFirmwareVersion (cmd 1151)
    fn get_touch_screen_firmware_version_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("(STUBBED) GetTouchScreenFirmwareVersion called");

        // Upstream: rb{ctx, 6}, pushes FirmwareVersion (4 u32 words = 16 bytes)
        // Return default (zero) firmware version
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(0); // major
        rb.push_u32(0); // minor
        rb.push_u32(0); // micro
        rb.push_u32(0); // revision
    }

    /// Upstream: IHidSystemServer::SetTouchScreenDefaultConfiguration (cmd 1152)
    fn set_touch_screen_default_configuration_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("(STUBBED) SetTouchScreenDefaultConfiguration called");

        // Upstream parses TouchScreenConfigurationForNx, applies it. Stubbed here.
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::GetTouchScreenDefaultConfiguration (cmd 1153)
    fn get_touch_screen_default_configuration_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("(STUBBED) GetTouchScreenDefaultConfiguration called");

        // Upstream: rb{ctx, 6}, pushes TouchScreenConfigurationForNx (4 u32 words)
        // Return default (zero) configuration
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(0); // mode
        rb.push_u32(0); // reserved
        rb.push_u32(0); // reserved
        rb.push_u32(0); // reserved
    }

    /// Upstream: nullptr (cmd 1154)
    fn is_firmware_available_for_notification_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsFirmwareAvailableForNotification called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::SetForceHandheldStyleVibration (cmd 1155)
    fn set_force_handheld_style_vibration_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let is_forced = rp.pop_bool();

        log::info!(
            "(STUBBED) SetForceHandheldStyleVibration called, is_forced={}",
            is_forced
        );

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1156)
    fn send_connection_trigger_without_timeout_event_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SendConnectionTriggerWithoutTimeoutEvent called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1157)
    fn cancel_connection_trigger_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) CancelConnectionTrigger called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1200)
    fn is_button_config_supported_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsButtonConfigSupported called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1201)
    fn is_button_config_embedded_supported_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsButtonConfigEmbeddedSupported called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1202)
    fn delete_button_config_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) DeleteButtonConfig called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1203)
    fn delete_button_config_embedded_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) DeleteButtonConfigEmbedded called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1204)
    fn set_button_config_enabled_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetButtonConfigEnabled called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1205)
    fn set_button_config_embedded_enabled_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetButtonConfigEmbeddedEnabled called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1206)
    fn is_button_config_enabled_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) IsButtonConfigEnabled called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1207)
    fn is_button_config_embedded_enabled_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsButtonConfigEmbeddedEnabled called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1208)
    fn set_button_config_embedded_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetButtonConfigEmbedded called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1209)
    fn set_button_config_full_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) SetButtonConfigFull called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1210)
    fn set_button_config_left_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) SetButtonConfigLeft called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1211)
    fn set_button_config_right_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) SetButtonConfigRight called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1212)
    fn get_button_config_embedded_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetButtonConfigEmbedded called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1213)
    fn get_button_config_full_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) GetButtonConfigFull called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1214)
    fn get_button_config_left_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) GetButtonConfigLeft called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1215)
    fn get_button_config_right_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) GetButtonConfigRight called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1250)
    fn is_custom_button_config_supported_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsCustomButtonConfigSupported called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1251)
    fn is_default_button_config_embedded_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsDefaultButtonConfigEmbedded called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1252)
    fn is_default_button_config_full_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsDefaultButtonConfigFull called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1253)
    fn is_default_button_config_left_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsDefaultButtonConfigLeft called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1254)
    fn is_default_button_config_right_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsDefaultButtonConfigRight called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1255)
    fn is_button_config_storage_embedded_empty_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsButtonConfigStorageEmbeddedEmpty called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1256)
    fn is_button_config_storage_full_empty_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsButtonConfigStorageFullEmpty called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1257)
    fn is_button_config_storage_left_empty_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsButtonConfigStorageLeftEmpty called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1258)
    fn is_button_config_storage_right_empty_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) IsButtonConfigStorageRightEmpty called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1259)
    fn get_button_config_storage_embedded_deprecated_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetButtonConfigStorageEmbeddedDeprecated called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1260)
    fn get_button_config_storage_full_deprecated_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetButtonConfigStorageFullDeprecated called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1261)
    fn get_button_config_storage_left_deprecated_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetButtonConfigStorageLeftDeprecated called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1262)
    fn get_button_config_storage_right_deprecated_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetButtonConfigStorageRightDeprecated called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1263)
    fn set_button_config_storage_embedded_deprecated_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetButtonConfigStorageEmbeddedDeprecated called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1264)
    fn set_button_config_storage_full_deprecated_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetButtonConfigStorageFullDeprecated called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1265)
    fn set_button_config_storage_left_deprecated_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetButtonConfigStorageLeftDeprecated called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1266)
    fn set_button_config_storage_right_deprecated_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetButtonConfigStorageRightDeprecated called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1267)
    fn delete_button_config_storage_embedded_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) DeleteButtonConfigStorageEmbedded called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1268)
    fn delete_button_config_storage_full_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) DeleteButtonConfigStorageFull called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1269)
    fn delete_button_config_storage_left_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) DeleteButtonConfigStorageLeft called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1270)
    fn delete_button_config_storage_right_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) DeleteButtonConfigStorageRight called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: IHidSystemServer::IsUsingCustomButtonConfig (cmd 1271)
    fn is_using_custom_button_config_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let is_enabled = false;
        log::debug!(
            "(STUBBED) IsUsingCustomButtonConfig called, is_enabled={}",
            is_enabled
        );

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(is_enabled);
    }

    /// Upstream: IHidSystemServer::IsAnyCustomButtonConfigEnabled (cmd 1272)
    fn is_any_custom_button_config_enabled_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let is_enabled = false;
        log::debug!(
            "(STUBBED) IsAnyCustomButtonConfigEnabled called, is_enabled={}",
            is_enabled
        );

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(is_enabled);
    }

    /// Upstream: nullptr (cmd 1273)
    fn set_all_custom_button_config_enabled_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetAllCustomButtonConfigEnabled called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1274)
    fn set_default_button_config_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetDefaultButtonConfig called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1275)
    fn set_all_default_button_config_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetAllDefaultButtonConfig called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1276)
    fn set_hid_button_config_embedded_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetHidButtonConfigEmbedded called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1277)
    fn set_hid_button_config_full_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetHidButtonConfigFull called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1278)
    fn set_hid_button_config_left_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetHidButtonConfigLeft called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1279)
    fn set_hid_button_config_right_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetHidButtonConfigRight called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1280)
    fn get_hid_button_config_embedded_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetHidButtonConfigEmbedded called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1281)
    fn get_hid_button_config_full_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetHidButtonConfigFull called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1282)
    fn get_hid_button_config_left_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetHidButtonConfigLeft called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1283)
    fn get_hid_button_config_right_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetHidButtonConfigRight called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1284)
    fn get_button_config_storage_embedded_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetButtonConfigStorageEmbedded called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1285)
    fn get_button_config_storage_full_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetButtonConfigStorageFull called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1286)
    fn get_button_config_storage_left_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetButtonConfigStorageLeft called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1287)
    fn get_button_config_storage_right_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) GetButtonConfigStorageRight called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1288)
    fn set_button_config_storage_embedded_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetButtonConfigStorageEmbedded called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1289)
    fn set_button_config_storage_full_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) SetButtonConfigStorageFull called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1290)
    fn delete_button_config_storage_right_1290_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) DeleteButtonConfigStorageRight (1290) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Upstream: nullptr (cmd 1291)
    fn delete_button_config_storage_right_1291_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("(STUBBED) DeleteButtonConfigStorageRight (1291) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    pub fn new(
        resource_manager: Arc<parking_lot::Mutex<ResourceManager>>,
        firmware_settings: Arc<HidFirmwareSettings>,
    ) -> Self {
        // clang-format off
        let handlers = build_handler_map(&[
            (
                31,
                Some(Self::send_keyboard_lock_key_event_handler),
                "SendKeyboardLockKeyEvent",
            ),
            (
                101,
                Some(Self::acquire_home_button_event_handle_handler),
                "AcquireHomeButtonEventHandle",
            ),
            (
                111,
                Some(Self::activate_home_button_handler),
                "ActivateHomeButton",
            ),
            (
                121,
                Some(Self::acquire_sleep_button_event_handle_handler),
                "AcquireSleepButtonEventHandle",
            ),
            (
                131,
                Some(Self::activate_sleep_button_handler),
                "ActivateSleepButton",
            ),
            (
                141,
                Some(Self::acquire_capture_button_event_handle_handler),
                "AcquireCaptureButtonEventHandle",
            ),
            (
                151,
                Some(Self::activate_capture_button_handler),
                "ActivateCaptureButton",
            ),
            (
                161,
                Some(Self::get_platform_config_handler),
                "GetPlatformConfig",
            ),
            (
                210,
                Some(Self::acquire_nfc_device_update_event_handle_handler),
                "AcquireNfcDeviceUpdateEventHandle",
            ),
            (
                211,
                Some(Self::get_npads_with_nfc_handler),
                "GetNpadsWithNfc",
            ),
            (
                212,
                Some(Self::acquire_nfc_activate_event_handle_handler),
                "AcquireNfcActivateEventHandle",
            ),
            (213, Some(Self::activate_nfc_handler), "ActivateNfc"),
            (
                214,
                Some(Self::get_xcd_handle_for_npad_with_nfc_handler),
                "GetXcdHandleForNpadWithNfc",
            ),
            (215, Some(Self::is_nfc_activated_handler), "IsNfcActivated"),
            (216, None, "GetAbstractedPadIdForNpadWithNfc"),
            (217, None, "SetNfcEvent"),
            (218, None, "GetNfcInfo"),
            (219, None, "StartNfcDiscovery"),
            (220, None, "StopNfcDiscovery"),
            (221, None, "StartNtagRead"),
            (222, None, "StartNtagWrite"),
            (223, None, "SendNfcRawData"),
            (224, None, "RegisterMifareKey"),
            (225, None, "ClearMifareKey"),
            (226, None, "StartMifareRead"),
            (227, None, "StartMifareWrite"),
            (
                230,
                Some(Self::acquire_ir_sensor_event_handle_handler),
                "AcquireIrSensorEventHndle",
            ),
            (
                231,
                Some(Self::activate_ir_sensor_handler),
                "ActivateIrSensor",
            ),
            (
                232,
                Some(Self::get_ir_sensor_state_handler),
                "GetIrSensorState",
            ),
            (
                233,
                Some(Self::get_xcd_handle_for_npad_with_ir_sensor_handler),
                "GetXcdHandleForNpadWihIrSensor",
            ),
            (234, None, "GetNpadJoyHoldType"),
            (241, None, "GetDataFormat"),
            (242, None, "SetDataFormat"),
            (243, None, "GetMcuState"),
            (244, None, "SetMcuState"),
            (245, None, "GetMcuVersionForNfc"),
            (246, None, "CheckNfcDevicePower"),
            (247, None, "SetMcuStateImmediate"),
            (
                301,
                Some(Self::activate_npad_system_handler),
                "ActivateNpadSystem",
            ),
            (
                303,
                Some(Self::apply_npad_system_common_policy_handler),
                "ApplyNpadSystemCommonPolicy",
            ),
            (
                304,
                Some(Self::enable_assigning_single_on_sl_sr_press_handler),
                "EnableAssigningSingleOnSlSrPress",
            ),
            (
                305,
                Some(Self::disable_assigning_single_on_sl_sr_press_handler),
                "DisableAssigningSingleOnSlSrPress",
            ),
            (
                306,
                Some(Self::get_last_active_npad_handler),
                "GetLastActiveNpad",
            ),
            (
                307,
                Some(Self::get_npad_system_ext_style_handler),
                "GetNpadSystemExtStyle",
            ),
            (
                308,
                Some(Self::apply_npad_system_common_policy_full_handler),
                "ApplyNpadSystemCommonPolicyFull",
            ),
            (
                309,
                Some(Self::get_npad_full_key_grip_color_handler),
                "GetNpadFullKeyGripColor",
            ),
            (
                310,
                Some(Self::get_masked_supported_npad_style_set_handler),
                "GetMaskedSupportedNpadStyleSet",
            ),
            (
                311,
                Some(Self::set_npad_player_led_blinking_device_handler),
                "SetNpadPlayerLedBlinkingDevice",
            ),
            (
                312,
                Some(Self::set_supported_npad_style_set_all_handler),
                "SetSupportedNpadStyleSetAll",
            ),
            (
                313,
                Some(Self::get_npad_capture_button_assignment_handler),
                "GetNpadCaptureButtonAssignment",
            ),
            (314, None, "GetAppletFooterUiType"),
            (
                315,
                Some(Self::get_applet_detailed_ui_type_handler),
                "GetAppletDetailedUiType",
            ),
            (
                316,
                Some(Self::get_npad_interface_type_handler),
                "GetNpadInterfaceType",
            ),
            (
                317,
                Some(Self::get_npad_left_right_interface_type_handler),
                "GetNpadLeftRightInterfaceType",
            ),
            (318, Some(Self::has_battery_handler), "HasBattery"),
            (
                319,
                Some(Self::has_left_right_battery_handler),
                "HasLeftRightBattery",
            ),
            (
                321,
                Some(Self::get_unique_pads_from_npad_handler),
                "GetUniquePadsFromNpad",
            ),
            (
                322,
                Some(Self::set_npad_system_ext_state_enabled_handler),
                "SetNpadSystemExtStateEnabled",
            ),
            (323, None, "GetLastActiveUniquePad"),
            (324, None, "GetUniquePadButtonSet"),
            (325, None, "GetUniquePadColor"),
            (326, None, "GetUniquePadAppletDetailedUiType"),
            (327, None, "GetAbstractedPadIdDataFromNpad"),
            (
                328,
                Some(Self::attach_abstracted_pad_to_npad_handler),
                "AttachAbstractedPadToNpad",
            ),
            (
                329,
                Some(Self::detach_abstracted_pad_all_handler),
                "DetachAbstractedPadAll",
            ),
            (
                330,
                Some(Self::check_abstracted_pad_connection_handler),
                "CheckAbstractedPadConnection",
            ),
            (
                500,
                Some(Self::set_applet_resource_user_id_handler),
                "SetAppletResourceUserId",
            ),
            (
                501,
                Some(Self::register_applet_resource_user_id_handler),
                "RegisterAppletResourceUserId",
            ),
            (
                502,
                Some(Self::unregister_applet_resource_user_id_handler),
                "UnregisterAppletResourceUserId",
            ),
            (
                503,
                Some(Self::enable_applet_to_get_input_handler),
                "EnableAppletToGetInput",
            ),
            (
                504,
                Some(Self::set_aruid_valid_for_vibration_handler),
                "SetAruidValidForVibration",
            ),
            (
                505,
                Some(Self::enable_applet_to_get_six_axis_sensor_handler),
                "EnableAppletToGetSixAxisSensor",
            ),
            (
                506,
                Some(Self::enable_applet_to_get_pad_input_handler),
                "EnableAppletToGetPadInput",
            ),
            (
                507,
                Some(Self::enable_applet_to_get_touch_screen_handler),
                "EnableAppletToGetTouchScreen",
            ),
            (
                510,
                Some(Self::set_vibration_master_volume_handler),
                "SetVibrationMasterVolume",
            ),
            (
                511,
                Some(Self::get_vibration_master_volume_handler),
                "GetVibrationMasterVolume",
            ),
            (
                512,
                Some(Self::begin_permit_vibration_session_handler),
                "BeginPermitVibrationSession",
            ),
            (
                513,
                Some(Self::end_permit_vibration_session_handler),
                "EndPermitVibrationSession",
            ),
            (514, Some(Self::unknown514_handler), "Unknown514"),
            (
                520,
                Some(Self::enable_handheld_hids_handler),
                "EnableHandheldHids",
            ),
            (
                521,
                Some(Self::disable_handheld_hids_handler),
                "DisableHandheldHids",
            ),
            (
                522,
                Some(Self::set_joy_con_rail_enabled_handler),
                "SetJoyConRailEnabled",
            ),
            (
                523,
                Some(Self::is_joy_con_rail_enabled_handler),
                "IsJoyConRailEnabled",
            ),
            (
                524,
                Some(Self::is_handheld_hids_enabled_handler),
                "IsHandheldHidsEnabled",
            ),
            (
                525,
                Some(Self::is_joy_con_attached_on_all_rail_handler),
                "IsJoyConAttachedOnAllRail",
            ),
            (540, None, "AcquirePlayReportControllerUsageUpdateEvent"),
            (541, None, "GetPlayReportControllerUsages"),
            (542, None, "AcquirePlayReportRegisteredDeviceUpdateEvent"),
            (543, None, "GetRegisteredDevicesOld"),
            (
                544,
                Some(Self::acquire_connection_trigger_timeout_event_handler),
                "AcquireConnectionTriggerTimeoutEvent",
            ),
            (545, None, "SendConnectionTrigger"),
            (
                546,
                Some(Self::acquire_device_registered_event_for_controller_support_handler),
                "AcquireDeviceRegisteredEventForControllerSupport",
            ),
            (547, None, "GetAllowedBluetoothLinksCount"),
            (
                548,
                Some(Self::get_registered_devices_handler),
                "GetRegisteredDevices",
            ),
            (
                549,
                Some(Self::get_connectable_registered_devices_handler),
                "GetConnectableRegisteredDevices",
            ),
            (
                700,
                Some(Self::activate_unique_pad_handler),
                "ActivateUniquePad",
            ),
            (
                702,
                Some(Self::acquire_unique_pad_connection_event_handle_handler),
                "AcquireUniquePadConnectionEventHandle",
            ),
            (
                703,
                Some(Self::get_unique_pad_ids_handler),
                "GetUniquePadIds",
            ),
            (
                751,
                Some(Self::acquire_joy_detach_on_bluetooth_off_event_handle_handler),
                "AcquireJoyDetachOnBluetoothOffEventHandle",
            ),
            (
                800,
                Some(Self::list_six_axis_sensor_handles_handler),
                "ListSixAxisSensorHandles",
            ),
            (
                801,
                Some(Self::is_six_axis_sensor_user_calibration_supported_handler),
                "IsSixAxisSensorUserCalibrationSupported",
            ),
            (
                802,
                Some(Self::reset_six_axis_sensor_calibration_values_handler),
                "ResetSixAxisSensorCalibrationValues",
            ),
            (
                803,
                Some(Self::start_six_axis_sensor_user_calibration_handler),
                "StartSixAxisSensorUserCalibration",
            ),
            (
                804,
                Some(Self::cancel_six_axis_sensor_user_calibration_handler),
                "CancelSixAxisSensorUserCalibration",
            ),
            (
                805,
                Some(Self::get_unique_pad_bluetooth_address_handler),
                "GetUniquePadBluetoothAddress",
            ),
            (
                806,
                Some(Self::disconnect_unique_pad_handler),
                "DisconnectUniquePad",
            ),
            (
                807,
                Some(Self::get_unique_pad_type_handler),
                "GetUniquePadType",
            ),
            (
                808,
                Some(Self::get_unique_pad_interface_handler),
                "GetUniquePadInterface",
            ),
            (
                809,
                Some(Self::get_unique_pad_serial_number_handler),
                "GetUniquePadSerialNumber",
            ),
            (
                810,
                Some(Self::get_unique_pad_controller_number_handler),
                "GetUniquePadControllerNumber",
            ),
            (
                811,
                Some(Self::get_six_axis_sensor_user_calibration_stage_handler),
                "GetSixAxisSensorUserCalibrationStage",
            ),
            (
                812,
                Some(Self::get_console_unique_six_axis_sensor_handle_handler),
                "GetConsoleUniqueSixAxisSensorHandle",
            ),
            (
                821,
                Some(Self::start_analog_stick_manual_calibration_handler),
                "StartAnalogStickManualCalibration",
            ),
            (
                822,
                Some(Self::retry_current_analog_stick_manual_calibration_stage_handler),
                "RetryCurrentAnalogStickManualCalibrationStage",
            ),
            (
                823,
                Some(Self::cancel_analog_stick_manual_calibration_handler),
                "CancelAnalogStickManualCalibration",
            ),
            (
                824,
                Some(Self::reset_analog_stick_manual_calibration_handler),
                "ResetAnalogStickManualCalibration",
            ),
            (
                825,
                Some(Self::get_analog_stick_state_handler),
                "GetAnalogStickState",
            ),
            (
                826,
                Some(Self::get_analog_stick_manual_calibration_stage_handler),
                "GetAnalogStickManualCalibrationStage",
            ),
            (
                827,
                Some(Self::is_analog_stick_button_pressed_handler),
                "IsAnalogStickButtonPressed",
            ),
            (
                828,
                Some(Self::is_analog_stick_in_release_position_handler),
                "IsAnalogStickInReleasePosition",
            ),
            (
                829,
                Some(Self::is_analog_stick_in_circumference_handler),
                "IsAnalogStickInCircumference",
            ),
            (
                830,
                Some(Self::set_notification_led_pattern_handler),
                "SetNotificationLedPattern",
            ),
            (
                831,
                Some(Self::set_notification_led_pattern_with_timeout_handler),
                "SetNotificationLedPatternWithTimeout",
            ),
            (
                832,
                Some(Self::prepare_hids_for_notification_wake_handler),
                "PrepareHidsForNotificationWake",
            ),
            (
                850,
                Some(Self::is_usb_full_key_controller_enabled_handler),
                "IsUsbFullKeyControllerEnabled",
            ),
            (
                851,
                Some(Self::enable_usb_full_key_controller_handler),
                "EnableUsbFullKeyController",
            ),
            (852, Some(Self::is_usb_connected_handler), "IsUsbConnected"),
            (
                870,
                Some(Self::is_handheld_button_pressed_on_console_mode_handler),
                "IsHandheldButtonPressedOnConsoleMode",
            ),
            (
                900,
                Some(Self::activate_input_detector_handler),
                "ActivateInputDetector",
            ),
            (
                901,
                Some(Self::notify_input_detector_handler),
                "NotifyInputDetector",
            ),
            (
                1000,
                Some(Self::initialize_firmware_update_handler),
                "InitializeFirmwareUpdate",
            ),
            (
                1001,
                Some(Self::get_firmware_version_handler),
                "GetFirmwareVersion",
            ),
            (
                1002,
                Some(Self::get_available_firmware_version_handler),
                "GetAvailableFirmwareVersion",
            ),
            (
                1003,
                Some(Self::is_firmware_update_available_handler),
                "IsFirmwareUpdateAvailable",
            ),
            (
                1004,
                Some(Self::check_firmware_update_required_handler),
                "CheckFirmwareUpdateRequired",
            ),
            (
                1005,
                Some(Self::start_firmware_update_handler),
                "StartFirmwareUpdate",
            ),
            (
                1006,
                Some(Self::abort_firmware_update_handler),
                "AbortFirmwareUpdate",
            ),
            (
                1007,
                Some(Self::get_firmware_update_state_handler),
                "GetFirmwareUpdateState",
            ),
            (
                1008,
                Some(Self::activate_audio_control_handler),
                "ActivateAudioControl",
            ),
            (
                1009,
                Some(Self::acquire_audio_control_event_handle_handler),
                "AcquireAudioControlEventHandle",
            ),
            (
                1010,
                Some(Self::get_audio_control_states_handler),
                "GetAudioControlStates",
            ),
            (
                1011,
                Some(Self::deactivate_audio_control_handler),
                "DeactivateAudioControl",
            ),
            (
                1050,
                Some(Self::is_six_axis_sensor_accurate_user_calibration_supported_handler),
                "IsSixAxisSensorAccurateUserCalibrationSupported",
            ),
            (
                1051,
                Some(Self::start_six_axis_sensor_accurate_user_calibration_handler),
                "StartSixAxisSensorAccurateUserCalibration",
            ),
            (
                1052,
                Some(Self::cancel_six_axis_sensor_accurate_user_calibration_handler),
                "CancelSixAxisSensorAccurateUserCalibration",
            ),
            (
                1053,
                Some(Self::get_six_axis_sensor_accurate_user_calibration_state_handler),
                "GetSixAxisSensorAccurateUserCalibrationState",
            ),
            (
                1100,
                Some(Self::get_hidbus_system_service_object_handler),
                "GetHidbusSystemServiceObject",
            ),
            (
                1120,
                Some(Self::set_firmware_hotfix_update_skip_enabled_handler),
                "SetFirmwareHotfixUpdateSkipEnabled",
            ),
            (
                1130,
                Some(Self::initialize_usb_firmware_update_handler),
                "InitializeUsbFirmwareUpdate",
            ),
            (
                1131,
                Some(Self::finalize_usb_firmware_update_handler),
                "FinalizeUsbFirmwareUpdate",
            ),
            (
                1132,
                Some(Self::check_usb_firmware_update_required_handler),
                "CheckUsbFirmwareUpdateRequired",
            ),
            (
                1133,
                Some(Self::start_usb_firmware_update_handler),
                "StartUsbFirmwareUpdate",
            ),
            (
                1134,
                Some(Self::get_usb_firmware_update_state_handler),
                "GetUsbFirmwareUpdateState",
            ),
            (
                1135,
                Some(Self::initialize_usb_firmware_update_without_memory_handler),
                "InitializeUsbFirmwareUpdateWithoutMemory",
            ),
            (
                1150,
                Some(Self::set_touch_screen_magnification_handler),
                "SetTouchScreenMagnification",
            ),
            (
                1151,
                Some(Self::get_touch_screen_firmware_version_handler),
                "GetTouchScreenFirmwareVersion",
            ),
            (
                1152,
                Some(Self::set_touch_screen_default_configuration_handler),
                "SetTouchScreenDefaultConfiguration",
            ),
            (
                1153,
                Some(Self::get_touch_screen_default_configuration_handler),
                "GetTouchScreenDefaultConfiguration",
            ),
            (
                1154,
                Some(Self::is_firmware_available_for_notification_handler),
                "IsFirmwareAvailableForNotification",
            ),
            (
                1155,
                Some(Self::set_force_handheld_style_vibration_handler),
                "SetForceHandheldStyleVibration",
            ),
            (
                1156,
                Some(Self::send_connection_trigger_without_timeout_event_handler),
                "SendConnectionTriggerWithoutTimeoutEvent",
            ),
            (
                1157,
                Some(Self::cancel_connection_trigger_handler),
                "CancelConnectionTrigger",
            ),
            (
                1200,
                Some(Self::is_button_config_supported_handler),
                "IsButtonConfigSupported",
            ),
            (
                1201,
                Some(Self::is_button_config_embedded_supported_handler),
                "IsButtonConfigEmbeddedSupported",
            ),
            (
                1202,
                Some(Self::delete_button_config_handler),
                "DeleteButtonConfig",
            ),
            (
                1203,
                Some(Self::delete_button_config_embedded_handler),
                "DeleteButtonConfigEmbedded",
            ),
            (
                1204,
                Some(Self::set_button_config_enabled_handler),
                "SetButtonConfigEnabled",
            ),
            (
                1205,
                Some(Self::set_button_config_embedded_enabled_handler),
                "SetButtonConfigEmbeddedEnabled",
            ),
            (
                1206,
                Some(Self::is_button_config_enabled_handler),
                "IsButtonConfigEnabled",
            ),
            (
                1207,
                Some(Self::is_button_config_embedded_enabled_handler),
                "IsButtonConfigEmbeddedEnabled",
            ),
            (
                1208,
                Some(Self::set_button_config_embedded_handler),
                "SetButtonConfigEmbedded",
            ),
            (
                1209,
                Some(Self::set_button_config_full_handler),
                "SetButtonConfigFull",
            ),
            (
                1210,
                Some(Self::set_button_config_left_handler),
                "SetButtonConfigLeft",
            ),
            (
                1211,
                Some(Self::set_button_config_right_handler),
                "SetButtonConfigRight",
            ),
            (
                1212,
                Some(Self::get_button_config_embedded_handler),
                "GetButtonConfigEmbedded",
            ),
            (
                1213,
                Some(Self::get_button_config_full_handler),
                "GetButtonConfigFull",
            ),
            (
                1214,
                Some(Self::get_button_config_left_handler),
                "GetButtonConfigLeft",
            ),
            (
                1215,
                Some(Self::get_button_config_right_handler),
                "GetButtonConfigRight",
            ),
            (
                1250,
                Some(Self::is_custom_button_config_supported_handler),
                "IsCustomButtonConfigSupported",
            ),
            (
                1251,
                Some(Self::is_default_button_config_embedded_handler),
                "IsDefaultButtonConfigEmbedded",
            ),
            (
                1252,
                Some(Self::is_default_button_config_full_handler),
                "IsDefaultButtonConfigFull",
            ),
            (
                1253,
                Some(Self::is_default_button_config_left_handler),
                "IsDefaultButtonConfigLeft",
            ),
            (
                1254,
                Some(Self::is_default_button_config_right_handler),
                "IsDefaultButtonConfigRight",
            ),
            (
                1255,
                Some(Self::is_button_config_storage_embedded_empty_handler),
                "IsButtonConfigStorageEmbeddedEmpty",
            ),
            (
                1256,
                Some(Self::is_button_config_storage_full_empty_handler),
                "IsButtonConfigStorageFullEmpty",
            ),
            (
                1257,
                Some(Self::is_button_config_storage_left_empty_handler),
                "IsButtonConfigStorageLeftEmpty",
            ),
            (
                1258,
                Some(Self::is_button_config_storage_right_empty_handler),
                "IsButtonConfigStorageRightEmpty",
            ),
            (
                1259,
                Some(Self::get_button_config_storage_embedded_deprecated_handler),
                "GetButtonConfigStorageEmbeddedDeprecated",
            ),
            (
                1260,
                Some(Self::get_button_config_storage_full_deprecated_handler),
                "GetButtonConfigStorageFullDeprecated",
            ),
            (
                1261,
                Some(Self::get_button_config_storage_left_deprecated_handler),
                "GetButtonConfigStorageLeftDeprecated",
            ),
            (
                1262,
                Some(Self::get_button_config_storage_right_deprecated_handler),
                "GetButtonConfigStorageRightDeprecated",
            ),
            (
                1263,
                Some(Self::set_button_config_storage_embedded_deprecated_handler),
                "SetButtonConfigStorageEmbeddedDeprecated",
            ),
            (
                1264,
                Some(Self::set_button_config_storage_full_deprecated_handler),
                "SetButtonConfigStorageFullDeprecated",
            ),
            (
                1265,
                Some(Self::set_button_config_storage_left_deprecated_handler),
                "SetButtonConfigStorageLeftDeprecated",
            ),
            (
                1266,
                Some(Self::set_button_config_storage_right_deprecated_handler),
                "SetButtonConfigStorageRightDeprecated",
            ),
            (
                1267,
                Some(Self::delete_button_config_storage_embedded_handler),
                "DeleteButtonConfigStorageEmbedded",
            ),
            (
                1268,
                Some(Self::delete_button_config_storage_full_handler),
                "DeleteButtonConfigStorageFull",
            ),
            (
                1269,
                Some(Self::delete_button_config_storage_left_handler),
                "DeleteButtonConfigStorageLeft",
            ),
            (
                1270,
                Some(Self::delete_button_config_storage_right_handler),
                "DeleteButtonConfigStorageRight",
            ),
            (
                1271,
                Some(Self::is_using_custom_button_config_handler),
                "IsUsingCustomButtonConfig",
            ),
            (
                1272,
                Some(Self::is_any_custom_button_config_enabled_handler),
                "IsAnyCustomButtonConfigEnabled",
            ),
            (
                1273,
                Some(Self::set_all_custom_button_config_enabled_handler),
                "SetAllCustomButtonConfigEnabled",
            ),
            (
                1274,
                Some(Self::set_default_button_config_handler),
                "SetDefaultButtonConfig",
            ),
            (
                1275,
                Some(Self::set_all_default_button_config_handler),
                "SetAllDefaultButtonConfig",
            ),
            (
                1276,
                Some(Self::set_hid_button_config_embedded_handler),
                "SetHidButtonConfigEmbedded",
            ),
            (
                1277,
                Some(Self::set_hid_button_config_full_handler),
                "SetHidButtonConfigFull",
            ),
            (
                1278,
                Some(Self::set_hid_button_config_left_handler),
                "SetHidButtonConfigLeft",
            ),
            (
                1279,
                Some(Self::set_hid_button_config_right_handler),
                "SetHidButtonConfigRight",
            ),
            (
                1280,
                Some(Self::get_hid_button_config_embedded_handler),
                "GetHidButtonConfigEmbedded",
            ),
            (
                1281,
                Some(Self::get_hid_button_config_full_handler),
                "GetHidButtonConfigFull",
            ),
            (
                1282,
                Some(Self::get_hid_button_config_left_handler),
                "GetHidButtonConfigLeft",
            ),
            (
                1283,
                Some(Self::get_hid_button_config_right_handler),
                "GetHidButtonConfigRight",
            ),
            (
                1284,
                Some(Self::get_button_config_storage_embedded_handler),
                "GetButtonConfigStorageEmbedded",
            ),
            (
                1285,
                Some(Self::get_button_config_storage_full_handler),
                "GetButtonConfigStorageFull",
            ),
            (
                1286,
                Some(Self::get_button_config_storage_left_handler),
                "GetButtonConfigStorageLeft",
            ),
            (
                1287,
                Some(Self::get_button_config_storage_right_handler),
                "GetButtonConfigStorageRight",
            ),
            (
                1288,
                Some(Self::set_button_config_storage_embedded_handler),
                "SetButtonConfigStorageEmbedded",
            ),
            (
                1289,
                Some(Self::set_button_config_storage_full_handler),
                "SetButtonConfigStorageFull",
            ),
            (
                1290,
                Some(Self::delete_button_config_storage_right_1290_handler),
                "DeleteButtonConfigStorageRight",
            ),
            (
                1291,
                Some(Self::delete_button_config_storage_right_1291_handler),
                "DeleteButtonConfigStorageRight",
            ),
        ]);
        // clang-format on

        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
            resource_manager,
            firmware_settings,
            acquire_device_registered_event: Arc::new(Event::new()),
            joy_detach_event: Event::new(),
        }
    }
}

impl SessionRequestHandler for IHidSystemServer {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "hid:sys"
    }
}

impl ServiceFramework for IHidSystemServer {
    fn get_service_name(&self) -> &str {
        "hid:sys"
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

    #[test]
    fn joy_detach_event_returns_a_readable_object_instead_of_null() {
        use crate::hle::kernel::k_process::{KProcess, ProcessLock};
        use crate::hle::kernel::k_readable_event::KReadableEvent;
        use crate::hle::kernel::k_thread::{KThread, KThreadLock};
        use crate::hle::service::hle_ipc::KAutoObjectRef;
        let settings = Arc::new(HidFirmwareSettings::new());
        let hid = Arc::new(parking_lot::Mutex::new(hid_core::hid_core::HIDCore::new()));
        let manager = Arc::new(parking_lot::Mutex::new(ResourceManager::new(settings.clone(), hid)));
        let service = IHidSystemServer::new(manager, settings);
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let readable = Arc::new(std::sync::Mutex::new(KReadableEvent::new()));
        readable.lock().unwrap().initialize(1, 2);
        service.joy_detach_event.attach_kernel_event(readable.clone(), process.clone());
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        let mut ctx = HLERequestContext::new_with_thread(thread, 0);
        service.handlers[&751].handler_callback.unwrap()(&service, &mut ctx);
        assert!(matches!(ctx.outgoing_copy_objects.as_slice(), [KAutoObjectRef::ObjectId(2)]));
        assert!(!readable.lock().unwrap().is_signaled());
    }

    #[test]
    fn set_npad_system_ext_state_enabled_parameters_preserve_aruid_offset() {
        assert_eq!(
            std::mem::offset_of!(
                SetNpadSystemExtStateEnabledParameters,
                applet_resource_user_id
            ),
            8
        );

        let mut ctx = HLERequestContext::new();
        ctx.cmd_buf[0] = 1;
        ctx.cmd_buf[1] = 0;
        ctx.cmd_buf[2] = 0x5566_7788;
        ctx.cmd_buf[3] = 0x1122_3344;
        let mut parser = RequestParser::from_buffer(&ctx);
        let parameters = parser.pop_raw::<SetNpadSystemExtStateEnabledParameters>();

        assert!(parameters.is_enabled);
        assert_eq!(parameters.applet_resource_user_id, 0x1122_3344_5566_7788);
    }
}

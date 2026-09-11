// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of hid_core/resources/npad/npad.h and npad.cpp
//!
//! Main NPad controller resource managing all npad-related state including
//! style sets, vibration, six-axis sensors, and abstracted pad management.

use common::input::PollingMode;
use common::ResultCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::hid_core::with_controller;

use crate::frontend::emulated_controller::{
    apply_simple_npad_stick_buttons, get_simple_npad_button_state, AnalogSticks, BatteryLevelState,
    ControllerColors, ControllerTriggerType, ControllerUpdateCallback, EmulatedDeviceIndex,
};
use crate::hid_core::{EmulatedControllerHandle, HIDCore, AVAILABLE_CONTROLLERS};
use crate::hid_result;
use crate::hid_types::*;
use crate::hid_util;
use crate::resources::abstracted_pad::abstract_pad::{AbstractPad, FullAbstractPad};
use crate::resources::applet_resource::{AppletResourceHolder, ARUID_INDEX_MAX};
use crate::resources::npad::npad_resource::NPadResource;
use crate::resources::npad::npad_types::*;
use crate::resources::npad::npad_vibration::NpadVibration;
use crate::resources::shared_memory_format::NpadInternalState;
use crate::resources::vibration::vibration_device::NpadVibrationDevice;

static NPAD_UPDATE_TRACE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct NpadControllerData {
    device: Option<EmulatedControllerHandle>,
    shared_memory_assigned: bool,
    is_active: bool,
    is_connected: bool,
    is_dual_left_connected: bool,
    is_dual_right_connected: bool,
    npad_pad_state: NPadGenericState,
    npad_libnx_state: NPadGenericState,
    npad_trigger_state: NpadGcTriggerState,
    callback_key: Option<i32>,
}

impl Default for NpadControllerData {
    fn default() -> Self {
        Self {
            device: None,
            shared_memory_assigned: false,
            is_active: false,
            is_connected: false,
            is_dual_left_connected: true,
            is_dual_right_connected: true,
            npad_pad_state: NPadGenericState::default(),
            npad_libnx_state: NPadGenericState::default(),
            npad_trigger_state: NpadGcTriggerState::default(),
            callback_key: None,
        }
    }
}

type ControllerData = [[NpadControllerData; MAX_SUPPORTED_NPAD_ID_TYPES]; ARUID_INDEX_MAX];
type ControllerCallbackEvents = [[u32; MAX_SUPPORTED_NPAD_ID_TYPES]; ARUID_INDEX_MAX];

fn controller_trigger_bit(trigger: ControllerTriggerType) -> u32 {
    1 << (trigger as u32)
}

fn two_mut<T, const N: usize>(
    values: &mut [T; N],
    first: usize,
    second: usize,
) -> (&mut T, &mut T) {
    debug_assert_ne!(first, second);
    if first < second {
        let (left, right) = values.split_at_mut(second);
        (&mut left[first], &mut right[0])
    } else {
        let (left, right) = values.split_at_mut(first);
        (&mut right[0], &mut left[second])
    }
}

fn trace_npad_update_env_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_NPAD_UPDATE").is_some())
}

fn trace_npad_state_env_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_NPAD_STATE").is_some())
}

fn trace_npad_update(
    aruid: u64,
    entry_index: usize,
    buttons: u64,
    sampling_number: i64,
    style_bits: u32,
    sixaxis_properties: u32,
) {
    if !common::trace::is_enabled(common::trace::cat::HID_NPAD) {
        return;
    }
    common::trace::emit_raw(
        common::trace::cat::HID_NPAD,
        &[
            0xFFFF,
            0,
            aruid,
            entry_index as u64,
            buttons,
            sampling_number as u64,
            style_bits as u64,
            sixaxis_properties as u64,
        ],
    );
}

/// Main NPad controller resource
pub struct NPad {
    hid_core: Option<Arc<parking_lot::Mutex<HIDCore>>>,
    controller_data: Box<ControllerData>,
    callback_events: Arc<parking_lot::Mutex<ControllerCallbackEvents>>,
    press_state: AtomicU64,
    npad_resource: NPadResource,
    abstracted_pads: FullAbstractPad,
    vibration: NpadVibration,
    vibration_devices: [NpadVibrationDevice; 2],
    ref_counter: i32,
    applet_resource_holder: AppletResourceHolder,
}

impl Default for NPad {
    fn default() -> Self {
        let abstracted_pads = std::array::from_fn(|index| {
            let mut pad = AbstractPad::new();
            pad.set_npad_id(hid_util::index_to_npad_id_type(index));
            pad
        });
        Self {
            hid_core: None,
            controller_data: Box::new(std::array::from_fn(|_| {
                std::array::from_fn(|_| NpadControllerData::default())
            })),
            callback_events: Arc::new(parking_lot::Mutex::new(
                [[0; MAX_SUPPORTED_NPAD_ID_TYPES]; ARUID_INDEX_MAX],
            )),
            press_state: AtomicU64::new(0),
            npad_resource: NPadResource::new(),
            abstracted_pads,
            vibration: NpadVibration::new(),
            vibration_devices: [NpadVibrationDevice::new(), NpadVibrationDevice::new()],
            ref_counter: 0,
            applet_resource_holder: AppletResourceHolder::new(),
        }
    }
}

impl NPad {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn new_with_hid_core(hid_core: Arc<parking_lot::Mutex<HIDCore>>) -> Self {
        let mut npad = Self::default();
        npad.hid_core = Some(Arc::clone(&hid_core));
        let controllers: Vec<_> = {
            let hid_core = hid_core.lock();
            (0..AVAILABLE_CONTROLLERS)
                .map(|index| hid_core.get_emulated_controller_by_index(index))
                .collect()
        };
        let player_1 = Arc::clone(&controllers[0]);
        npad.vibration_devices[0].mount(
            Arc::clone(&player_1),
            DeviceIndex::Left,
            npad.vibration.clone(),
        );
        npad.vibration_devices[1].mount(player_1, DeviceIndex::Right, npad.vibration.clone());
        for aruid_index in 0..ARUID_INDEX_MAX {
            for (controller_index, device) in controllers.iter().enumerate() {
                let callback_events = Arc::clone(&npad.callback_events);
                let callback = ControllerUpdateCallback {
                    on_change: Arc::new(move |trigger| {
                        callback_events.lock()[aruid_index][controller_index] |=
                            controller_trigger_bit(trigger);
                    }),
                    is_npad_service: true,
                };
                let callback_key = device.lock().set_callback(callback);
                let controller = &mut npad.controller_data[aruid_index][controller_index];
                controller.device = Some(Arc::clone(device));
                controller.callback_key = Some(callback_key);
            }
        }
        npad
    }

    /// Port of NPad::Activate().
    pub fn activate(&mut self) -> ResultCode {
        if self.ref_counter == i32::MAX - 1 {
            return hid_result::RESULT_NPAD_RESOURCE_OVERFLOW;
        }

        if self.ref_counter == 0 {
            // Upstream TODO: Activate handlers and AbstractedPad
        }

        self.ref_counter += 1;
        ResultCode::SUCCESS
    }

    pub fn is_active(&self) -> bool {
        self.ref_counter != 0
    }

    /// Port of NPad::Activate(u64 aruid).
    pub fn activate_for_aruid(&mut self, aruid: u64) -> ResultCode {
        let Some(applet_resource) = self.applet_resource_holder.applet_resource.clone() else {
            return ResultCode::SUCCESS;
        };
        let mut applet_resource = applet_resource.lock();

        let aruid_index = applet_resource.get_index_from_aruid(aruid);
        if aruid_index >= ARUID_INDEX_MAX {
            log::error!("Invalid aruid:{aruid:016X}");
            return ResultCode::SUCCESS;
        }

        {
            let data = applet_resource.get_aruid_data_by_index(aruid_index);
            if !data.flag.is_assigned() {
                return ResultCode::SUCCESS;
            }
        }

        let Some(shared) = applet_resource.get_shared_memory_format_by_index_mut(aruid_index)
        else {
            return ResultCode::SUCCESS;
        };

        for (controller_index, entry) in shared.npad.npad_entry.iter_mut().enumerate() {
            let npad = &mut entry.internal_state;
            npad.fullkey_color.attribute = ColorAttribute::NoController;
            npad.joycon_color.attribute = ColorAttribute::NoController;

            // HW seems to initialize the first 19 entries.
            for _ in 0..19 {
                Self::write_empty_entry(npad);
            }

            let controller = &mut self.controller_data[aruid_index][controller_index];
            controller.shared_memory_assigned = true;
            controller.is_active = true;
        }

        ResultCode::SUCCESS
    }

    /// Port of NPad::ActivateNpadResource().
    pub fn activate_npad_resource(&mut self) -> ResultCode {
        self.npad_resource.activate()
    }

    /// Port of NPad::ActivateNpadResource(u64 aruid).
    pub fn activate_npad_resource_with_aruid(&mut self, aruid: u64) -> ResultCode {
        self.npad_resource.activate_with_aruid(aruid)
    }

    pub fn free_applet_resource_id(&mut self, _aruid: u64) {
        self.npad_resource.free_applet_resource_id(_aruid);
    }

    /// Port of NPad::RegisterAppletResourceUserId.
    pub fn register_applet_resource_user_id(&mut self, aruid: u64) -> ResultCode {
        self.npad_resource.register_applet_resource_user_id(aruid)
    }

    /// Port of NPad::UnregisterAppletResourceUserId.
    pub fn unregister_applet_resource_user_id(&mut self, aruid: u64) {
        let aruid_index = match self.npad_resource.get_index_from_aruid(aruid) {
            Some(index) => index,
            None => {
                log::error!("Invalid aruid:{aruid:016X}");
                0
            }
        };
        for controller in &mut self.controller_data[aruid_index] {
            controller.is_active = false;
            controller.is_connected = false;
            controller.shared_memory_assigned = false;
        }
        self.npad_resource.unregister_applet_resource_user_id(aruid);
    }

    /// Port of NPad::SetNpadExternals.
    pub fn set_npad_externals(&mut self, holder: AppletResourceHolder) {
        for abstracted_pad in &mut self.abstracted_pads {
            abstracted_pad.set_applet_resource(holder.applet_resource.clone());
        }
        self.applet_resource_holder = holder;
    }

    /// Port of NPad::OnUpdate.
    pub fn on_update(&mut self) {
        let trace_update = trace_npad_update_env_enabled();
        let trace_index = if trace_update {
            NPAD_UPDATE_TRACE_COUNTER.fetch_add(1, Ordering::Relaxed)
        } else {
            0
        };
        if self.ref_counter == 0 {
            if trace_update && trace_index % 1000 == 0 {
                log::info!("[NPAD_UPDATE] skip ref_counter=0");
            }
            return;
        }

        if let Some(controller) = self.controller_data[0][0].device.clone() {
            if controller.lock().is_connected(false) {
                for (index, device_index) in [DeviceIndex::Left, DeviceIndex::Right]
                    .into_iter()
                    .enumerate()
                {
                    if !self.vibration_devices[index].is_vibration_mounted() {
                        self.vibration_devices[index].mount(
                            Arc::clone(&controller),
                            device_index,
                            self.vibration.clone(),
                        );
                    }
                }
            } else {
                for device in &mut self.vibration_devices {
                    device.unmount();
                }
            }
        }

        let Some(applet_resource) = self.applet_resource_holder.applet_resource.clone() else {
            if trace_update && trace_index % 1000 == 0 {
                log::info!("[NPAD_UPDATE] skip no_applet_resource");
            }
            return;
        };
        let pending_events = {
            let mut events = self.callback_events.lock();
            std::mem::replace(
                &mut *events,
                [[0; MAX_SUPPORTED_NPAD_ID_TYPES]; ARUID_INDEX_MAX],
            )
        };
        let mut applet_resource = applet_resource.lock();
        let mut last_active_controller = None;
        let mut abstracted_pad_updates = [false; MAX_SUPPORTED_NPAD_ID_TYPES];

        for aruid_index in 0..ARUID_INDEX_MAX {
            let (assigned, aruid, enable_input) = {
                let data = applet_resource.get_aruid_data_by_index(aruid_index);
                (
                    data.flag.is_assigned(),
                    data.aruid,
                    data.flag.enable_pad_input(),
                )
            };
            if !assigned {
                if trace_update && trace_index % 1000 == 0 && aruid != 0 {
                    log::info!(
                        "[NPAD_UPDATE] skip aruid=0x{:X} assigned=false enable_input={}",
                        aruid,
                        enable_input
                    );
                }
                continue;
            }

            let is_set = self
                .npad_resource
                .is_supported_npad_style_set(aruid)
                .unwrap_or(false);
            if !is_set {
                if trace_update && trace_index % 1000 == 0 {
                    log::info!(
                        "[NPAD_UPDATE] skip aruid=0x{:X} is_supported_style_set=false enable_input={}",
                        aruid,
                        enable_input
                    );
                }
                continue;
            }

            if trace_update && trace_index % 1000 == 0 {
                log::info!(
                    "[NPAD_UPDATE] active aruid=0x{:X} enable_input={} buttons=0x{:X}",
                    aruid,
                    enable_input,
                    get_simple_npad_button_state().raw.bits()
                );
            }

            let Some(shared) = applet_resource.get_shared_memory_format_by_index_mut(aruid_index)
            else {
                continue;
            };

            for entry_index in 0..MAX_SUPPORTED_NPAD_ID_TYPES {
                let npad = &mut shared.npad.npad_entry[entry_index].internal_state;
                let controller = &mut self.controller_data[aruid_index][entry_index];
                controller.shared_memory_assigned = true;
                let Some(device) = controller.device.as_ref().cloned() else {
                    continue;
                };

                let mut device = device.lock();
                // Notifications a `connect` below raises are delivered only
                // after this guard is released; see `with_controller`.
                let mut connect_callbacks: Vec<
                    crate::frontend::emulated_controller::DeferredControllerCallback,
                > = Vec::new();
                let controller_type = device.get_npad_style_index(false);
                let npad_id = device.get_npad_id_type();
                let is_connected = device.is_connected(false);
                let events = pending_events[aruid_index][entry_index];

                if !is_connected {
                    if controller.is_connected {
                        Self::disconnect_controller(npad, controller);
                        self.npad_resource
                            .signal_style_set_update_event(aruid, npad_id);
                    }
                    continue;
                }
                if controller_type == NpadStyleIndex::None {
                    continue;
                }

                let colors = device.get_colors();
                let battery = device.get_battery();
                let reconnected =
                    events & controller_trigger_bit(ControllerTriggerType::Disconnected) != 0
                        && events & controller_trigger_bit(ControllerTriggerType::Connected) != 0;
                if reconnected && controller.is_connected {
                    // Upstream handles controller callbacks synchronously, so a
                    // Disconnect/SetType/Connect sequence updates shared memory
                    // in order. The Rust callback adapter coalesces callbacks
                    // until OnUpdate; replay the lost disconnect before using
                    // the final connected type.
                    Self::disconnect_controller(npad, controller);
                    self.npad_resource
                        .signal_style_set_update_event(aruid, npad_id);
                }
                if !controller.is_connected
                    && self
                        .npad_resource
                        .is_controller_supported(aruid, controller_type)
                {
                    Self::init_newly_added_controller(
                        npad,
                        controller,
                        controller_type,
                        colors,
                        battery,
                    );
                    connect_callbacks = device.run_deferred(|device| device.connect(false)).1;
                    device.set_led_pattern();
                    if controller_type == NpadStyleIndex::JoyconDual {
                        if controller.is_dual_left_connected {
                            device.set_polling_mode(
                                EmulatedDeviceIndex::LeftIndex,
                                PollingMode::Active,
                            );
                        }
                        if controller.is_dual_right_connected {
                            device.set_polling_mode(
                                EmulatedDeviceIndex::RightIndex,
                                PollingMode::Active,
                            );
                        }
                    } else {
                        device
                            .set_polling_mode(EmulatedDeviceIndex::AllDevices, PollingMode::Active);
                    }
                    self.npad_resource
                        .signal_style_set_update_event(aruid, npad_id);
                    Self::write_empty_entry(npad);
                    // Defer shared-owner updates until both the controller and applet-resource
                    // guards are released. Their handlers traverse AppletResource themselves.
                    last_active_controller = Some(npad_id);
                    abstracted_pad_updates[hid_util::npad_id_type_to_index(npad_id)] = true;
                } else {
                    if events
                        & (controller_trigger_bit(ControllerTriggerType::Battery)
                            | controller_trigger_bit(ControllerTriggerType::All))
                        != 0
                    {
                        npad.battery_level_dual = battery.dual.battery_level;
                        npad.battery_level_left = battery.left.battery_level;
                        npad.battery_level_right = battery.right.battery_level;
                    }
                }

                if !enable_input || !controller.shared_memory_assigned || !controller.is_active {
                    drop(device);
                    for callback in connect_callbacks {
                        callback.dispatch();
                    }
                    continue;
                }

                device.status_update();
                let simple_buttons = get_simple_npad_button_state().raw;
                let mut button_state = device.get_npad_buttons();
                button_state.raw |= simple_buttons;
                let mut stick_state = device.get_sticks();
                apply_simple_npad_stick_buttons(&mut stick_state, simple_buttons);
                let trigger_state = device.get_triggers();
                drop(device);
                for callback in connect_callbacks {
                    callback.dispatch();
                }

                Self::request_pad_state_update(
                    controller,
                    controller_type,
                    button_state,
                    stick_state,
                    trigger_state,
                );

                let pad_state = &mut controller.npad_pad_state;
                let libnx_state = &mut controller.npad_libnx_state;
                let trigger_state = &mut controller.npad_trigger_state;
                libnx_state.connection_status.raw = 1;

                match controller_type {
                    NpadStyleIndex::None => unreachable!(),
                    NpadStyleIndex::Fullkey
                    | NpadStyleIndex::NES
                    | NpadStyleIndex::SNES
                    | NpadStyleIndex::N64
                    | NpadStyleIndex::SegaGenesis => {
                        pad_state.connection_status.raw = 0x3;
                        libnx_state.connection_status.raw |= 1 << 1;
                        pad_state.sampling_number =
                            npad.fullkey_lifo.read_current_entry().state.sampling_number + 1;
                        npad.fullkey_lifo.write_next_entry(*pad_state);
                    }
                    NpadStyleIndex::Handheld => {
                        pad_state.connection_status.raw = 0x3f;
                        libnx_state.connection_status.raw |= 0x3e;
                        pad_state.sampling_number = npad
                            .handheld_lifo
                            .read_current_entry()
                            .state
                            .sampling_number
                            + 1;
                        npad.handheld_lifo.write_next_entry(*pad_state);
                    }
                    NpadStyleIndex::JoyconDual => {
                        pad_state.connection_status.raw = 1;
                        if controller.is_dual_left_connected {
                            pad_state.connection_status.raw |= 1 << 2;
                            libnx_state.connection_status.raw |= 1 << 2;
                        }
                        if controller.is_dual_right_connected {
                            pad_state.connection_status.raw |= 1 << 4;
                            libnx_state.connection_status.raw |= 1 << 4;
                        }
                        pad_state.sampling_number = npad
                            .joy_dual_lifo
                            .read_current_entry()
                            .state
                            .sampling_number
                            + 1;
                        npad.joy_dual_lifo.write_next_entry(*pad_state);
                    }
                    NpadStyleIndex::JoyconLeft => {
                        pad_state.connection_status.raw = 1 | (1 << 2);
                        libnx_state.connection_status.raw |= 1 << 2;
                        pad_state.sampling_number = npad
                            .joy_left_lifo
                            .read_current_entry()
                            .state
                            .sampling_number
                            + 1;
                        npad.joy_left_lifo.write_next_entry(*pad_state);
                    }
                    NpadStyleIndex::JoyconRight => {
                        pad_state.connection_status.raw = 1 | (1 << 4);
                        libnx_state.connection_status.raw |= 1 << 4;
                        pad_state.sampling_number = npad
                            .joy_right_lifo
                            .read_current_entry()
                            .state
                            .sampling_number
                            + 1;
                        npad.joy_right_lifo.write_next_entry(*pad_state);
                    }
                    NpadStyleIndex::GameCube => {
                        pad_state.connection_status.raw = 0x3;
                        libnx_state.connection_status.raw |= 1 << 1;
                        pad_state.sampling_number =
                            npad.fullkey_lifo.read_current_entry().state.sampling_number + 1;
                        trigger_state.sampling_number = npad
                            .gc_trigger_lifo
                            .read_current_entry()
                            .state
                            .sampling_number
                            + 1;
                        npad.fullkey_lifo.write_next_entry(*pad_state);
                        npad.gc_trigger_lifo.write_next_entry(*trigger_state);
                    }
                    NpadStyleIndex::Pokeball => {
                        pad_state.connection_status.raw = 1;
                        pad_state.sampling_number =
                            npad.palma_lifo.read_current_entry().state.sampling_number + 1;
                        npad.palma_lifo.write_next_entry(*pad_state);
                    }
                    _ => continue,
                }

                libnx_state.npad_buttons = pad_state.npad_buttons;
                libnx_state.l_stick = pad_state.l_stick;
                libnx_state.r_stick = pad_state.r_stick;
                libnx_state.sampling_number = npad
                    .system_ext_lifo
                    .read_current_entry()
                    .state
                    .sampling_number
                    + 1;
                npad.system_ext_lifo.write_next_entry(*libnx_state);

                self.press_state
                    .fetch_or(pad_state.npad_buttons.raw.bits(), Ordering::Relaxed);
                if !pad_state.npad_buttons.raw.is_empty() {
                    last_active_controller = Some(npad_id);
                }
                if trace_npad_state_env_enabled() && !pad_state.npad_buttons.raw.is_empty() {
                    log::info!(
                        "[NPAD_STATE] aruid=0x{:X} entry={} buttons=0x{:X} sampling={}",
                        aruid,
                        entry_index,
                        pad_state.npad_buttons.raw.bits(),
                        pad_state.sampling_number
                    );
                }
                if trace_update && trace_index % 600 == 0 {
                    trace_npad_update(
                        aruid,
                        entry_index,
                        pad_state.npad_buttons.raw.bits(),
                        pad_state.sampling_number,
                        npad.style_tag.raw.bits(),
                        npad.sixaxis_fullkey_properties.raw as u32,
                    );
                }
            }
        }

        drop(applet_resource);
        for (abstracted_pad, needs_update) in
            self.abstracted_pads.iter_mut().zip(abstracted_pad_updates)
        {
            if needs_update {
                abstracted_pad.update();
            }
        }
        if let (Some(hid_core), Some(npad_id)) = (self.hid_core.as_ref(), last_active_controller) {
            hid_core.lock().set_last_active_controller(npad_id);
        }
    }

    fn init_newly_added_controller(
        npad: &mut NpadInternalState,
        controller: &mut NpadControllerData,
        controller_type: NpadStyleIndex,
        body_colors: ControllerColors,
        battery_level: BatteryLevelState,
    ) {
        npad.style_tag.raw = NpadStyleSet::NONE;
        npad.device_type.raw = 0;
        npad.system_properties.raw = 0;
        npad.fullkey_color = NpadFullKeyColorState::default();
        npad.joycon_color = NpadJoyColorState::default();
        npad.battery_level_dual = NpadBatteryLevel::Empty;
        npad.battery_level_left = NpadBatteryLevel::Empty;
        npad.battery_level_right = NpadBatteryLevel::Empty;

        match controller_type {
            NpadStyleIndex::None => return,
            NpadStyleIndex::Fullkey => {
                npad.fullkey_color.attribute = ColorAttribute::Ok;
                npad.fullkey_color.fullkey = body_colors.fullkey;
                npad.battery_level_dual = battery_level.dual.battery_level;
                npad.style_tag.raw.insert(NpadStyleSet::FULLKEY);
                npad.device_type.raw |= 1 << 0;
                npad.system_properties.raw |= (1 << 11) | (1 << 13) | (1 << 14);
                npad.system_properties
                    .set_is_charging_joy_dual(battery_level.dual.is_charging);
                npad.applet_footer_type = AppletFooterUiType::SwitchProController;
                npad.sixaxis_fullkey_properties.set_is_newly_assigned(true);
            }
            NpadStyleIndex::Handheld => {
                npad.fullkey_color.attribute = ColorAttribute::Ok;
                npad.joycon_color.attribute = ColorAttribute::Ok;
                npad.fullkey_color.fullkey = body_colors.fullkey;
                npad.joycon_color.left = body_colors.left;
                npad.joycon_color.right = body_colors.right;
                npad.style_tag.raw.insert(NpadStyleSet::HANDHELD);
                npad.device_type.raw |= (1 << 2) | (1 << 3);
                npad.system_properties.raw |= (1 << 11) | (1 << 13) | (1 << 14) | (1 << 15);
                npad.system_properties
                    .set_is_charging_joy_dual(battery_level.left.is_charging);
                npad.system_properties
                    .set_is_charging_joy_left(battery_level.left.is_charging);
                npad.system_properties
                    .set_is_charging_joy_right(battery_level.right.is_charging);
                npad.assignment_mode = NpadJoyAssignmentMode::Dual;
                npad.applet_footer_type = AppletFooterUiType::HandheldJoyConLeftJoyConRight;
                npad.sixaxis_handheld_properties.set_is_newly_assigned(true);
            }
            NpadStyleIndex::JoyconDual => {
                npad.fullkey_color.attribute = ColorAttribute::Ok;
                npad.joycon_color.attribute = ColorAttribute::Ok;
                npad.style_tag.raw.insert(NpadStyleSet::JOY_DUAL);
                if controller.is_dual_left_connected {
                    npad.joycon_color.left = body_colors.left;
                    npad.battery_level_left = battery_level.left.battery_level;
                    npad.device_type.raw |= 1 << 4;
                    npad.system_properties.raw |= 1 << 14;
                    npad.system_properties
                        .set_is_charging_joy_left(battery_level.left.is_charging);
                    npad.sixaxis_dual_left_properties
                        .set_is_newly_assigned(true);
                }
                if controller.is_dual_right_connected {
                    npad.joycon_color.right = body_colors.right;
                    npad.battery_level_right = battery_level.right.battery_level;
                    npad.device_type.raw |= 1 << 5;
                    npad.system_properties.raw |= 1 << 13;
                    npad.system_properties
                        .set_is_charging_joy_right(battery_level.right.is_charging);
                    npad.sixaxis_dual_right_properties
                        .set_is_newly_assigned(true);
                }
                npad.system_properties.raw |= (1 << 11) | (1 << 15);
                npad.assignment_mode = NpadJoyAssignmentMode::Dual;
                if controller.is_dual_left_connected && controller.is_dual_right_connected {
                    npad.applet_footer_type = AppletFooterUiType::JoyDual;
                    npad.fullkey_color.fullkey = body_colors.left;
                    npad.battery_level_dual = battery_level.left.battery_level;
                    npad.system_properties
                        .set_is_charging_joy_dual(battery_level.left.is_charging);
                } else if controller.is_dual_left_connected {
                    npad.applet_footer_type = AppletFooterUiType::JoyDualLeftOnly;
                    npad.fullkey_color.fullkey = body_colors.left;
                    npad.battery_level_dual = battery_level.left.battery_level;
                    npad.system_properties
                        .set_is_charging_joy_dual(battery_level.left.is_charging);
                } else {
                    npad.applet_footer_type = AppletFooterUiType::JoyDualRightOnly;
                    npad.fullkey_color.fullkey = body_colors.right;
                    npad.battery_level_dual = battery_level.right.battery_level;
                    npad.system_properties
                        .set_is_charging_joy_dual(battery_level.right.is_charging);
                }
            }
            NpadStyleIndex::JoyconLeft => {
                npad.fullkey_color.attribute = ColorAttribute::Ok;
                npad.fullkey_color.fullkey = body_colors.left;
                npad.joycon_color.attribute = ColorAttribute::Ok;
                npad.joycon_color.left = body_colors.left;
                npad.battery_level_dual = battery_level.left.battery_level;
                npad.style_tag.raw.insert(NpadStyleSet::JOY_LEFT);
                npad.device_type.raw |= 1 << 4;
                npad.system_properties.raw |= (1 << 12) | (1 << 14);
                npad.system_properties
                    .set_is_charging_joy_left(battery_level.left.is_charging);
                npad.applet_footer_type = AppletFooterUiType::JoyLeftHorizontal;
                npad.sixaxis_left_properties.set_is_newly_assigned(true);
            }
            NpadStyleIndex::JoyconRight => {
                npad.fullkey_color.attribute = ColorAttribute::Ok;
                npad.fullkey_color.fullkey = body_colors.right;
                npad.joycon_color.attribute = ColorAttribute::Ok;
                npad.joycon_color.right = body_colors.right;
                npad.battery_level_right = battery_level.right.battery_level;
                npad.style_tag.raw.insert(NpadStyleSet::JOY_RIGHT);
                npad.device_type.raw |= 1 << 5;
                npad.system_properties.raw |= (1 << 12) | (1 << 13);
                npad.system_properties
                    .set_is_charging_joy_right(battery_level.right.is_charging);
                npad.applet_footer_type = AppletFooterUiType::JoyRightHorizontal;
                npad.sixaxis_right_properties.set_is_newly_assigned(true);
            }
            NpadStyleIndex::GameCube => {
                npad.style_tag.raw.insert(NpadStyleSet::GC);
                npad.device_type.raw |= 1 << 0;
                npad.system_properties.raw |= (1 << 11) | (1 << 13);
            }
            NpadStyleIndex::Pokeball => {
                npad.style_tag.raw.insert(NpadStyleSet::PALMA);
                npad.device_type.raw |= 1 << 6;
                npad.sixaxis_fullkey_properties.set_is_newly_assigned(true);
            }
            NpadStyleIndex::NES => {
                npad.style_tag.raw.insert(NpadStyleSet::LARK);
                npad.device_type.raw |= 1 << 0;
            }
            NpadStyleIndex::SNES => {
                npad.style_tag.raw.insert(NpadStyleSet::LUCIA);
                npad.device_type.raw |= 1 << 0;
                npad.applet_footer_type = AppletFooterUiType::Lucia;
            }
            NpadStyleIndex::N64 => {
                npad.style_tag.raw.insert(NpadStyleSet::LAGOON);
                npad.device_type.raw |= 1 << 0;
                npad.applet_footer_type = AppletFooterUiType::Lagon;
            }
            NpadStyleIndex::SegaGenesis => {
                npad.style_tag.raw.insert(NpadStyleSet::LAGER);
                npad.device_type.raw |= 1 << 0;
            }
            _ => {}
        }

        controller.is_connected = true;
    }

    fn disconnect_controller(npad: &mut NpadInternalState, controller: &mut NpadControllerData) {
        npad.style_tag.raw = NpadStyleSet::NONE;
        npad.device_type.raw = 0;
        npad.system_properties.raw = 0;
        npad.button_properties.raw = 0;
        npad.sixaxis_fullkey_properties.raw = 0;
        npad.sixaxis_handheld_properties.raw = 0;
        npad.sixaxis_dual_left_properties.raw = 0;
        npad.sixaxis_dual_right_properties.raw = 0;
        npad.sixaxis_left_properties.raw = 0;
        npad.sixaxis_right_properties.raw = 0;
        npad.battery_level_dual = NpadBatteryLevel::Empty;
        npad.battery_level_left = NpadBatteryLevel::Empty;
        npad.battery_level_right = NpadBatteryLevel::Empty;
        npad.fullkey_color = NpadFullKeyColorState::default();
        npad.joycon_color = NpadJoyColorState::default();
        npad.applet_footer_type = AppletFooterUiType::None;
        controller.is_dual_left_connected = true;
        controller.is_dual_right_connected = true;
        controller.is_connected = false;
        Self::write_empty_entry(npad);
    }

    fn disconnect_npad(npad: &mut NpadInternalState, controller: &mut NpadControllerData) {
        if let Some(device) = &controller.device {
            with_controller(device, |device| device.disconnect());
        }
        Self::disconnect_controller(npad, controller);
    }

    fn update_controller_at(
        npad: &mut NpadInternalState,
        controller: &mut NpadControllerData,
        controller_type: NpadStyleIndex,
    ) {
        let Some(device) = controller.device.as_ref().cloned() else {
            return;
        };
        with_controller(&device, |device| device.set_npad_style_index(controller_type));
        with_controller(&device, |device| device.connect(false));
        let (is_connected, body_colors, battery_level) = {
            let device = device.lock();
            (
                device.is_connected(false),
                device.get_colors(),
                device.get_battery(),
            )
        };
        if !is_connected {
            return;
        }
        Self::init_newly_added_controller(
            npad,
            controller,
            controller_type,
            body_colors,
            battery_level,
        );
        Self::write_empty_entry(npad);
    }

    fn request_pad_state_update(
        controller: &mut NpadControllerData,
        controller_type: NpadStyleIndex,
        button_state: NpadButtonState,
        stick_state: AnalogSticks,
        trigger_state: NpadGcTriggerState,
    ) {
        let pad_entry = &mut controller.npad_pad_state;
        let right_button_mask = NpadButton::A
            | NpadButton::B
            | NpadButton::X
            | NpadButton::Y
            | NpadButton::STICK_R
            | NpadButton::R
            | NpadButton::ZR
            | NpadButton::PLUS
            | NpadButton::STICK_R_LEFT
            | NpadButton::STICK_R_UP
            | NpadButton::STICK_R_RIGHT
            | NpadButton::STICK_R_DOWN;
        let left_button_mask = NpadButton::LEFT
            | NpadButton::UP
            | NpadButton::RIGHT
            | NpadButton::DOWN
            | NpadButton::STICK_L
            | NpadButton::L
            | NpadButton::ZL
            | NpadButton::MINUS
            | NpadButton::STICK_L_LEFT
            | NpadButton::STICK_L_UP
            | NpadButton::STICK_L_RIGHT
            | NpadButton::STICK_L_DOWN;

        pad_entry.npad_buttons.raw = NpadButton::NONE;
        if controller_type != NpadStyleIndex::JoyconLeft {
            pad_entry.npad_buttons.raw = button_state.raw & right_button_mask;
            pad_entry.r_stick = stick_state.right;
        }
        if controller_type != NpadStyleIndex::JoyconRight {
            pad_entry.npad_buttons.raw |= button_state.raw & left_button_mask;
            pad_entry.l_stick = stick_state.left;
        }
        if matches!(
            controller_type,
            NpadStyleIndex::JoyconLeft | NpadStyleIndex::JoyconDual
        ) {
            pad_entry.npad_buttons.raw |=
                button_state.raw & (NpadButton::LEFT_SL | NpadButton::LEFT_SR);
        }
        if matches!(
            controller_type,
            NpadStyleIndex::JoyconRight | NpadStyleIndex::JoyconDual
        ) {
            pad_entry.npad_buttons.raw |=
                button_state.raw & (NpadButton::RIGHT_SL | NpadButton::RIGHT_SR);
        }
        if controller_type == NpadStyleIndex::GameCube {
            controller.npad_trigger_state.left = trigger_state.left;
            controller.npad_trigger_state.right = trigger_state.right;
            pad_entry.npad_buttons.raw.remove(NpadButton::ZL);
            pad_entry
                .npad_buttons
                .raw
                .set(NpadButton::ZR, button_state.raw.contains(NpadButton::R));
            pad_entry
                .npad_buttons
                .raw
                .set(NpadButton::L, button_state.raw.contains(NpadButton::ZL));
            pad_entry
                .npad_buttons
                .raw
                .set(NpadButton::R, button_state.raw.contains(NpadButton::ZR));
        }
    }

    /// Get the vibration handler session aruid.
    pub fn get_vibration_handler_session_aruid(&self) -> u64 {
        self.vibration.get_session_aruid()
    }

    /// Port of NPad::GetAndResetPressState.
    pub fn get_and_reset_press_state(&self) -> NpadButton {
        NpadButton::from_bits_truncate(self.press_state.swap(0, Ordering::Relaxed))
    }

    pub fn set_supported_npad_style_set(
        &mut self,
        aruid: u64,
        supported_style_set: NpadStyleSet,
    ) -> ResultCode {
        if let Some(hid_core) = self.hid_core.as_ref() {
            hid_core.lock().set_supported_style_tag(NpadStyleTag {
                raw: supported_style_set,
            });
        }
        let result = self
            .npad_resource
            .set_supported_npad_style_set(aruid, supported_style_set);
        if result.is_success() {
            self.on_update();
        }
        result
    }

    pub fn get_supported_npad_style_set(&self, aruid: u64) -> Result<NpadStyleSet, ResultCode> {
        self.npad_resource.get_supported_npad_style_set(aruid)
    }

    pub fn set_supported_npad_id_type(
        &mut self,
        aruid: u64,
        supported_npad_list: &[NpadIdType],
    ) -> ResultCode {
        let result = self
            .npad_resource
            .set_supported_npad_id_type(aruid, supported_npad_list);
        if result.is_success() {
            self.on_update();
        }
        result
    }

    pub fn set_npad_joy_hold_type(&mut self, aruid: u64, hold_type: NpadJoyHoldType) -> ResultCode {
        self.npad_resource.set_npad_joy_hold_type(aruid, hold_type)
    }

    pub fn get_npad_joy_hold_type(&self, aruid: u64) -> Result<NpadJoyHoldType, ResultCode> {
        self.npad_resource.get_npad_joy_hold_type(aruid)
    }

    pub fn set_npad_handheld_activation_mode(
        &mut self,
        aruid: u64,
        mode: NpadHandheldActivationMode,
    ) -> ResultCode {
        let result = self
            .npad_resource
            .set_npad_handheld_activation_mode(aruid, mode);
        if result.is_success() {
            self.on_update();
        }
        result
    }

    pub fn get_npad_handheld_activation_mode(
        &self,
        aruid: u64,
    ) -> Result<NpadHandheldActivationMode, ResultCode> {
        self.npad_resource.get_npad_handheld_activation_mode(aruid)
    }

    pub fn set_npad_communication_mode(
        &mut self,
        _communication_mode: NpadCommunicationMode,
    ) -> ResultCode {
        ResultCode::SUCCESS
    }

    pub fn get_npad_communication_mode(&self) -> NpadCommunicationMode {
        NpadCommunicationMode::Default
    }

    pub fn set_npad_joy_assignment_mode_single_by_default(
        &mut self,
        aruid: u64,
        npad_id: NpadIdType,
    ) -> ResultCode {
        self.set_npad_mode(
            aruid,
            npad_id,
            NpadJoyDeviceType::Left,
            NpadJoyAssignmentMode::Single,
        );
        ResultCode::SUCCESS
    }

    pub fn set_npad_joy_assignment_mode_single(
        &mut self,
        aruid: u64,
        npad_id: NpadIdType,
        npad_device_type: NpadJoyDeviceType,
    ) -> ResultCode {
        self.set_npad_mode(
            aruid,
            npad_id,
            npad_device_type,
            NpadJoyAssignmentMode::Single,
        );
        ResultCode::SUCCESS
    }

    pub fn set_npad_joy_assignment_mode_dual(
        &mut self,
        aruid: u64,
        npad_id: NpadIdType,
    ) -> ResultCode {
        self.set_npad_mode(
            aruid,
            npad_id,
            NpadJoyDeviceType::Left,
            NpadJoyAssignmentMode::Dual,
        );
        ResultCode::SUCCESS
    }

    /// Port of upstream `NPad::StartLrAssignmentMode`.
    pub fn start_lr_assignment_mode(&mut self, aruid: u64) -> ResultCode {
        let is_enabled = match self.npad_resource.get_lr_assignment_mode(aruid) {
            Ok(value) => value,
            Err(e) => return e,
        };
        if !is_enabled {
            return self.npad_resource.set_lr_assignment_mode(aruid, true);
        }
        ResultCode::SUCCESS
    }

    /// Port of upstream `NPad::StopLrAssignmentMode`.
    pub fn stop_lr_assignment_mode(&mut self, aruid: u64) -> ResultCode {
        let is_enabled = match self.npad_resource.get_lr_assignment_mode(aruid) {
            Ok(value) => value,
            Err(e) => return e,
        };
        if is_enabled {
            return self.npad_resource.set_lr_assignment_mode(aruid, false);
        }
        ResultCode::SUCCESS
    }

    /// Port of upstream `NPad::SetNpadMode`.
    pub fn set_npad_mode(
        &mut self,
        aruid: u64,
        npad_id: NpadIdType,
        npad_device_type: NpadJoyDeviceType,
        assignment_mode: NpadJoyAssignmentMode,
    ) -> (bool, NpadIdType) {
        if !hid_util::is_npad_id_valid(npad_id) {
            log::error!("Invalid NpadIdType npad_id:{:?}", npad_id);
            return (false, NpadIdType::default());
        }

        let Some(applet_resource) = self.applet_resource_holder.applet_resource.clone() else {
            return (false, NpadIdType::default());
        };
        let mut applet_resource = applet_resource.lock();
        let aruid_index = applet_resource.get_index_from_aruid(aruid);
        if aruid_index >= ARUID_INDEX_MAX {
            return (false, NpadIdType::default());
        }

        let Some(shared) = applet_resource.get_shared_memory_format_by_index_mut(aruid_index)
        else {
            return (false, NpadIdType::default());
        };
        let npad_index = hid_util::npad_id_type_to_index(npad_id);
        let shared_memory = &mut shared.npad.npad_entry[npad_index].internal_state;
        if shared_memory.assignment_mode != assignment_mode {
            shared_memory.assignment_mode = assignment_mode;
        }

        let controller = &mut self.controller_data[aruid_index][npad_index];
        let Some(device) = controller.device.as_ref().cloned() else {
            return (false, NpadIdType::default());
        };
        let (is_connected, controller_type) = {
            let device = device.lock();
            (
                device.is_connected(false),
                device.get_npad_style_index(false),
            )
        };
        if !is_connected {
            return (false, NpadIdType::default());
        }

        if assignment_mode == NpadJoyAssignmentMode::Dual {
            match controller_type {
                NpadStyleIndex::JoyconLeft => {
                    Self::disconnect_npad(shared_memory, controller);
                    controller.is_dual_left_connected = true;
                    controller.is_dual_right_connected = false;
                    Self::update_controller_at(
                        shared_memory,
                        controller,
                        NpadStyleIndex::JoyconDual,
                    );
                }
                NpadStyleIndex::JoyconRight => {
                    Self::disconnect_npad(shared_memory, controller);
                    controller.is_dual_left_connected = false;
                    controller.is_dual_right_connected = true;
                    Self::update_controller_at(
                        shared_memory,
                        controller,
                        NpadStyleIndex::JoyconDual,
                    );
                }
                _ => {}
            }
            return (false, NpadIdType::default());
        }

        if controller_type != NpadStyleIndex::JoyconDual {
            return (false, NpadIdType::default());
        }

        if controller.is_dual_left_connected && !controller.is_dual_right_connected {
            Self::disconnect_npad(shared_memory, controller);
            Self::update_controller_at(shared_memory, controller, NpadStyleIndex::JoyconLeft);
            return (false, NpadIdType::default());
        }
        if !controller.is_dual_left_connected && controller.is_dual_right_connected {
            Self::disconnect_npad(shared_memory, controller);
            Self::update_controller_at(shared_memory, controller, NpadStyleIndex::JoyconRight);
            return (false, NpadIdType::default());
        }

        let new_npad_id = self
            .hid_core
            .as_ref()
            .map(|hid_core| hid_core.lock().get_first_disconnected_npad_id())
            .unwrap_or_default();
        let new_npad_index = hid_util::npad_id_type_to_index(new_npad_id);
        if new_npad_index == npad_index {
            return (false, NpadIdType::default());
        }

        let (shared_memory, shared_memory_2) =
            two_mut(&mut shared.npad.npad_entry, npad_index, new_npad_index);
        let (controller, controller_2) = two_mut(
            &mut self.controller_data[aruid_index],
            npad_index,
            new_npad_index,
        );
        let shared_memory = &mut shared_memory.internal_state;
        let shared_memory_2 = &mut shared_memory_2.internal_state;

        Self::disconnect_npad(shared_memory, controller);
        if npad_device_type == NpadJoyDeviceType::Left {
            Self::update_controller_at(shared_memory, controller, NpadStyleIndex::JoyconLeft);
            controller_2.is_dual_left_connected = false;
            controller_2.is_dual_right_connected = true;
        } else {
            Self::update_controller_at(shared_memory, controller, NpadStyleIndex::JoyconRight);
            controller_2.is_dual_left_connected = true;
            controller_2.is_dual_right_connected = false;
        }
        Self::update_controller_at(shared_memory_2, controller_2, NpadStyleIndex::JoyconDual);
        (true, new_npad_id)
    }

    pub fn apply_npad_system_common_policy(&mut self, aruid: u64) -> ResultCode {
        self.npad_resource
            .apply_npad_system_common_policy(aruid, false)
    }

    pub fn apply_npad_system_common_policy_full(&mut self, aruid: u64) -> ResultCode {
        self.npad_resource
            .apply_npad_system_common_policy(aruid, true)
    }

    pub fn clear_npad_system_common_policy(&mut self, aruid: u64) -> ResultCode {
        self.npad_resource.clear_npad_system_common_policy(aruid)
    }

    pub fn set_vibration_master_volume(&self, master_volume: f32) -> ResultCode {
        self.vibration.set_vibration_master_volume(master_volume)
    }

    /// Port of NPad::ResetIsSixAxisSensorDeviceNewlyAssigned.
    pub fn reset_is_six_axis_sensor_device_newly_assigned(
        &mut self,
        aruid: u64,
        sixaxis_handle: &SixAxisSensorHandle,
    ) -> ResultCode {
        let valid = hid_util::is_sixaxis_handle_valid(sixaxis_handle);
        if valid.is_error() {
            return valid;
        }

        let Some(applet_resource) = self.applet_resource_holder.applet_resource.clone() else {
            return ResultCode::SUCCESS;
        };
        let mut applet_resource = applet_resource.lock();
        let Some(shared) = applet_resource.get_shared_memory_format_mut(aruid) else {
            return ResultCode::SUCCESS;
        };
        let npad_id: NpadIdType =
            unsafe { std::mem::transmute::<u32, NpadIdType>(sixaxis_handle.npad_id as u32) };
        let index = hid_util::npad_id_type_to_index(npad_id);
        let npad = &mut shared.npad.npad_entry[index].internal_state;

        match sixaxis_handle.npad_type {
            NpadStyleIndex::Fullkey | NpadStyleIndex::Pokeball => {
                npad.sixaxis_fullkey_properties.set_is_newly_assigned(false);
            }
            NpadStyleIndex::Handheld => {
                npad.sixaxis_handheld_properties
                    .set_is_newly_assigned(false);
            }
            NpadStyleIndex::JoyconDual => match sixaxis_handle.device_index {
                DeviceIndex::Left => npad
                    .sixaxis_dual_left_properties
                    .set_is_newly_assigned(false),
                DeviceIndex::Right => npad
                    .sixaxis_dual_right_properties
                    .set_is_newly_assigned(false),
                _ => {}
            },
            NpadStyleIndex::JoyconLeft => {
                npad.sixaxis_left_properties.set_is_newly_assigned(false);
            }
            NpadStyleIndex::JoyconRight => {
                npad.sixaxis_right_properties.set_is_newly_assigned(false);
            }
            _ => {}
        }

        ResultCode::SUCCESS
    }

    pub fn get_vibration_master_volume(&self) -> Result<f32, ResultCode> {
        self.vibration.get_vibration_master_volume()
    }

    pub fn begin_permit_vibration_session(&self, aruid: u64) -> ResultCode {
        self.vibration.begin_permit_vibration_session(aruid)
    }

    pub fn end_permit_vibration_session(&self) -> ResultCode {
        self.vibration.end_permit_vibration_session()
    }

    /// Port of `NPad::GetVibrationDevice` for standard LRA devices.
    ///
    /// ruzu does not yet model `AbstractPad`, so the default Player1 fullkey
    /// controller owns two virtual mounted LRA devices. This mirrors the
    /// controller state faked in `on_update`.
    pub fn get_vibration_device_mut(
        &mut self,
        handle: &VibrationDeviceHandle,
    ) -> Option<&mut NpadVibrationDevice> {
        if crate::hid_util::is_vibration_handle_valid(handle).is_error() {
            return None;
        }
        match handle.npad_type {
            NpadStyleIndex::Fullkey
            | NpadStyleIndex::Handheld
            | NpadStyleIndex::JoyconDual
            | NpadStyleIndex::JoyconLeft
            | NpadStyleIndex::JoyconRight => match handle.device_index {
                DeviceIndex::Left => Some(&mut self.vibration_devices[0]),
                DeviceIndex::Right => Some(&mut self.vibration_devices[1]),
                _ => None,
            },
            _ => None,
        }
    }

    /// Port of NPad::AssigningSingleOnSlSrPress.
    pub fn assigning_single_on_sl_sr_press(&mut self, aruid: u64, is_enabled: bool) -> ResultCode {
        let is_currently_enabled = match self
            .npad_resource
            .is_assigning_single_on_sl_sr_press_enabled(aruid)
        {
            Ok(v) => v,
            Err(e) => return ResultCode(e.raw()),
        };
        if is_enabled != is_currently_enabled {
            let result = self
                .npad_resource
                .set_assigning_single_on_sl_sr_press(aruid, is_enabled);
            return result;
        }
        ResultCode::SUCCESS
    }

    /// Port of NPad::GetLastActiveNpad.
    /// Upstream delegates to hid_core.GetLastActiveController().
    /// NPad needs a reference to HidCore (which has get_last_active_controller())
    /// but that wiring is not yet in place. Returns Player1 as a safe default until
    /// NPad receives an HidCore reference matching upstream's constructor signature.
    pub fn get_last_active_npad(&self) -> (ResultCode, NpadIdType) {
        (ResultCode::SUCCESS, NpadIdType::Player1)
    }

    /// Port of NPad::GetMaskedSupportedNpadStyleSet.
    pub fn get_masked_supported_npad_style_set(&self, aruid: u64) -> (ResultCode, NpadStyleSet) {
        match self
            .npad_resource
            .get_masked_supported_npad_style_set(aruid)
        {
            Ok(style_set) => (ResultCode::SUCCESS, style_set),
            Err(e) => {
                if e == hid_result::RESULT_UNDEFINED_STYLESET {
                    (ResultCode::SUCCESS, NpadStyleSet::NONE)
                } else {
                    (ResultCode(e.raw()), NpadStyleSet::NONE)
                }
            }
        }
    }

    /// Port of NPad::SetNpadSystemExtStateEnabled.
    pub fn set_npad_system_ext_state_enabled(
        &mut self,
        aruid: u64,
        is_enabled: bool,
    ) -> ResultCode {
        let result = self
            .npad_resource
            .set_npad_system_ext_state_enabled(aruid, is_enabled);
        if result.is_success() {
            for abstracted_pad in &mut self.abstracted_pads {
                abstracted_pad.enable_applet_to_get_input(aruid);
            }
        }
        result
    }

    /// Port of NPad::EnableAppletToGetInput.
    pub fn enable_applet_to_get_input(&mut self, aruid: u64) {
        for abstracted_pad in &mut self.abstracted_pads {
            abstracted_pad.enable_applet_to_get_input(aruid);
        }
    }

    /// Port of NPad::GetAppletDetailedUiType.
    pub fn get_applet_detailed_ui_type(&self, npad_id: NpadIdType) -> AppletDetailedUiType {
        let Some(applet_resource) = self.applet_resource_holder.applet_resource.clone() else {
            return AppletDetailedUiType::default();
        };
        let applet_resource = applet_resource.lock();
        let aruid = applet_resource.get_active_aruid();
        let Some(shared) = applet_resource.get_shared_memory_format(aruid) else {
            return AppletDetailedUiType::default();
        };

        let npad_index = hid_util::npad_id_type_to_index(npad_id);
        let shared_memory = &shared.npad.npad_entry[npad_index].internal_state;
        AppletDetailedUiType {
            ui_variant: 0,
            _padding: [0; 2],
            footer: shared_memory.applet_footer_type,
        }
    }

    /// Port of NPad::WriteEmptyEntry.
    fn write_empty_entry(npad: &mut NpadInternalState) {
        let mut dummy_pad_state = NPadGenericState::default();
        let mut dummy_gc_state = NpadGcTriggerState::default();

        dummy_pad_state.sampling_number =
            npad.fullkey_lifo.read_current_entry().sampling_number + 1;
        npad.fullkey_lifo.write_next_entry(dummy_pad_state);

        dummy_pad_state.sampling_number =
            npad.handheld_lifo.read_current_entry().sampling_number + 1;
        npad.handheld_lifo.write_next_entry(dummy_pad_state);

        dummy_pad_state.sampling_number =
            npad.joy_dual_lifo.read_current_entry().sampling_number + 1;
        npad.joy_dual_lifo.write_next_entry(dummy_pad_state);

        dummy_pad_state.sampling_number =
            npad.joy_left_lifo.read_current_entry().sampling_number + 1;
        npad.joy_left_lifo.write_next_entry(dummy_pad_state);

        dummy_pad_state.sampling_number =
            npad.joy_right_lifo.read_current_entry().sampling_number + 1;
        npad.joy_right_lifo.write_next_entry(dummy_pad_state);

        dummy_pad_state.sampling_number = npad.palma_lifo.read_current_entry().sampling_number + 1;
        npad.palma_lifo.write_next_entry(dummy_pad_state);

        dummy_pad_state.sampling_number =
            npad.system_ext_lifo.read_current_entry().sampling_number + 1;
        npad.system_ext_lifo.write_next_entry(dummy_pad_state);

        dummy_gc_state.sampling_number =
            npad.gc_trigger_lifo.read_current_entry().sampling_number + 1;
        npad.gc_trigger_lifo.write_next_entry(dummy_gc_state);
    }

    pub fn npad_resource(&self) -> &NPadResource {
        &self.npad_resource
    }

    pub fn npad_resource_mut(&mut self) -> &mut NPadResource {
        &mut self.npad_resource
    }
}

impl Drop for NPad {
    fn drop(&mut self) {
        for controllers in self.controller_data.iter_mut() {
            for controller in controllers {
                let (Some(device), Some(callback_key)) =
                    (controller.device.as_ref(), controller.callback_key.take())
                else {
                    continue;
                };
                device.lock().delete_callback(callback_key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::sync::Arc;

    use common::input::{
        register_input_factory, unregister_input_factory, AnalogStatus, CallbackStatus,
        InputCallback, InputDevice, InputDeviceFactory, InputType, StickStatus,
    };
    use common::param_package::ParamPackage;
    use common::settings_input::native_analog;
    use parking_lot::Mutex;

    use super::NPad;
    use crate::frontend::emulated_controller::HID_JOYSTICK_MAX;
    use crate::hid_core::HIDCore;
    use crate::hid_types::{NpadIdType, NpadStyleIndex, NpadStyleSet};
    use crate::resources::applet_resource::{AppletResource, AppletResourceHolder};
    use crate::resources::npad::npad_types::{NpadJoyAssignmentMode, NpadJoyDeviceType};
    use crate::resources::shared_memory_holder::KSharedMemoryBacking;

    struct TestSharedMemoryBacking;

    impl KSharedMemoryBacking for TestSharedMemoryBacking {
        fn create(&self, size: usize) -> Option<(*mut u8, Arc<dyn Any + Send + Sync>)> {
            let mut bytes = vec![0u8; size].into_boxed_slice();
            let ptr = bytes.as_mut_ptr();
            let keepalive: Arc<dyn Any + Send + Sync> = Arc::new(bytes);
            Some((ptr, keepalive))
        }
    }

    struct TestStickDevice {
        callback: InputCallback,
        x: f32,
        y: f32,
    }

    impl InputDevice for TestStickDevice {
        fn force_update(&mut self) {
            self.trigger_on_change(&CallbackStatus {
                input_type: InputType::Stick,
                stick_status: StickStatus {
                    x: AnalogStatus {
                        raw_value: self.x,
                        ..Default::default()
                    },
                    y: AnalogStatus {
                        raw_value: self.y,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            });
        }

        fn set_callback(&mut self, callback: InputCallback) {
            self.callback = callback;
        }

        fn trigger_on_change(&self, status: &CallbackStatus) {
            if let Some(on_change) = &self.callback.on_change {
                on_change(status);
            }
        }
    }

    struct TestStickFactory;

    impl InputDeviceFactory for TestStickFactory {
        fn create(&self, params: &ParamPackage) -> Box<dyn InputDevice> {
            Box::new(TestStickDevice {
                callback: InputCallback { on_change: None },
                x: params.get_float("test_x", 0.0),
                y: params.get_float("test_y", 0.0),
            })
        }
    }

    #[test]
    fn set_npad_mode_updates_shared_assignment_mode() {
        const ARUID: u64 = 0x51;

        let mut applet_resource = AppletResource::new();
        applet_resource.set_shared_memory_backing(Arc::new(TestSharedMemoryBacking));
        assert!(applet_resource
            .register_applet_resource_user_id(ARUID, true)
            .is_success());
        assert!(applet_resource.create_applet_resource(ARUID).is_success());

        let applet_resource = Arc::new(Mutex::new(applet_resource));
        let mut npad = NPad::new();
        npad.set_npad_externals(AppletResourceHolder {
            applet_resource: Some(applet_resource.clone()),
            handheld_config: None,
        });

        let (is_reassigned, new_npad_id) = npad.set_npad_mode(
            ARUID,
            NpadIdType::Player1,
            NpadJoyDeviceType::Left,
            NpadJoyAssignmentMode::Single,
        );
        assert!(!is_reassigned);
        assert_eq!(new_npad_id, NpadIdType::default());

        let resource = applet_resource.lock();
        let shared = resource.get_shared_memory_format(ARUID).unwrap();
        assert_eq!(
            shared.npad.npad_entry[0].internal_state.assignment_mode,
            NpadJoyAssignmentMode::Single
        );
    }

    #[test]
    fn set_npad_mode_splits_dual_joycon_like_upstream() {
        const ARUID: u64 = 0x51;

        let mut applet_resource = AppletResource::new();
        applet_resource.set_shared_memory_backing(Arc::new(TestSharedMemoryBacking));
        assert!(applet_resource
            .register_applet_resource_user_id(ARUID, true)
            .is_success());
        assert!(applet_resource.create_applet_resource(ARUID).is_success());
        let applet_resource = Arc::new(Mutex::new(applet_resource));

        let hid_core = Arc::new(Mutex::new(HIDCore::new()));
        let mut npad = NPad::new_with_hid_core(Arc::clone(&hid_core));
        npad.set_npad_externals(AppletResourceHolder {
            applet_resource: Some(Arc::clone(&applet_resource)),
            handheld_config: None,
        });
        assert!(npad.register_applet_resource_user_id(ARUID).is_success());
        assert!(npad.activate_npad_resource_with_aruid(ARUID).is_success());
        assert!(npad
            .set_supported_npad_style_set(
                ARUID,
                NpadStyleSet::JOY_DUAL | NpadStyleSet::JOY_LEFT | NpadStyleSet::JOY_RIGHT,
            )
            .is_success());
        assert!(npad.activate().is_success());
        assert!(npad.activate_for_aruid(ARUID).is_success());

        let player_1 = hid_core.lock().get_emulated_controller(NpadIdType::Player1);
        {
            let mut player_1 = player_1.lock();
            player_1.set_npad_style_index(NpadStyleIndex::JoyconDual);
            player_1.connect(false);
        }
        npad.on_update();

        let (is_reassigned, new_npad_id) = npad.set_npad_mode(
            ARUID,
            NpadIdType::Player1,
            NpadJoyDeviceType::Left,
            NpadJoyAssignmentMode::Single,
        );

        assert!(is_reassigned);
        assert_eq!(new_npad_id, NpadIdType::Player2);
        let player_2 = hid_core.lock().get_emulated_controller(NpadIdType::Player2);
        assert_eq!(
            player_1.lock().get_npad_style_index(false),
            NpadStyleIndex::JoyconLeft
        );
        let player_2 = player_2.lock();
        assert_eq!(
            player_2.get_npad_style_index(false),
            NpadStyleIndex::JoyconDual
        );
        assert!(player_2.is_connected(false));

        let resource = applet_resource.lock();
        let shared = resource.get_shared_memory_format(ARUID).unwrap();
        assert!(shared.npad.npad_entry[0]
            .internal_state
            .style_tag
            .raw
            .contains(NpadStyleSet::JOY_LEFT));
        assert!(shared.npad.npad_entry[1]
            .internal_state
            .style_tag
            .raw
            .contains(NpadStyleSet::JOY_DUAL));
    }

    #[test]
    fn lr_assignment_mode_start_stop_matches_upstream() {
        const ARUID: u64 = 0x51;

        let mut npad = NPad::new();
        assert!(npad.register_applet_resource_user_id(ARUID).is_success());
        assert!(npad.activate_npad_resource_with_aruid(ARUID).is_success());

        assert_eq!(npad.npad_resource.get_lr_assignment_mode(ARUID), Ok(false));
        assert!(npad.start_lr_assignment_mode(ARUID).is_success());
        assert_eq!(npad.npad_resource.get_lr_assignment_mode(ARUID), Ok(true));
        assert!(npad.start_lr_assignment_mode(ARUID).is_success());
        assert_eq!(npad.npad_resource.get_lr_assignment_mode(ARUID), Ok(true));
        assert!(npad.stop_lr_assignment_mode(ARUID).is_success());
        assert_eq!(npad.npad_resource.get_lr_assignment_mode(ARUID), Ok(false));
        assert!(npad.stop_lr_assignment_mode(ARUID).is_success());
        assert_eq!(npad.npad_resource.get_lr_assignment_mode(ARUID), Ok(false));
    }

    #[test]
    fn on_update_uses_hid_core_connected_controllers() {
        const ARUID: u64 = 0x51;

        let mut applet_resource = AppletResource::new();
        applet_resource.set_shared_memory_backing(Arc::new(TestSharedMemoryBacking));
        assert!(applet_resource
            .register_applet_resource_user_id(ARUID, true)
            .is_success());
        assert!(applet_resource.create_applet_resource(ARUID).is_success());

        let hid_core = Arc::new(Mutex::new(HIDCore::new()));
        {
            let hid_core = hid_core.lock();
            let p1 = hid_core.get_emulated_controller(NpadIdType::Player1);
            let p2 = hid_core.get_emulated_controller(NpadIdType::Player2);
            drop(hid_core);

            let mut p1 = p1.lock();
            p1.set_npad_style_index(NpadStyleIndex::Fullkey);
            p1.connect(false);
            drop(p1);

            let mut p2 = p2.lock();
            p2.set_npad_style_index(NpadStyleIndex::Fullkey);
            p2.disconnect();
        }

        let applet_resource = Arc::new(Mutex::new(applet_resource));
        let mut npad = NPad::new_with_hid_core(hid_core);
        npad.set_npad_externals(AppletResourceHolder {
            applet_resource: Some(applet_resource.clone()),
            handheld_config: None,
        });
        assert!(npad.register_applet_resource_user_id(ARUID).is_success());
        assert!(npad.activate_npad_resource_with_aruid(ARUID).is_success());
        assert!(npad
            .set_supported_npad_style_set(ARUID, NpadStyleSet::FULLKEY)
            .is_success());
        assert!(npad.activate().is_success());
        assert!(npad.activate_for_aruid(ARUID).is_success());

        npad.on_update();

        let resource = applet_resource.lock();
        let shared = resource.get_shared_memory_format(ARUID).unwrap();
        assert!(shared.npad.npad_entry[0]
            .internal_state
            .style_tag
            .raw
            .contains(NpadStyleSet::FULLKEY));
        assert!(!shared.npad.npad_entry[1]
            .internal_state
            .style_tag
            .raw
            .contains(NpadStyleSet::FULLKEY));
    }

    #[test]
    fn on_update_writes_controller_sticks_to_fullkey_and_system_ext_lifos() {
        const ARUID: u64 = 0x51;
        const ENGINE: &str = "npad_test_stick";

        register_input_factory(ENGINE, Arc::new(TestStickFactory));

        let mut left_stick = ParamPackage::default();
        left_stick.set_str("engine", ENGINE.to_string());
        left_stick.set_float("test_x", 0.75);
        left_stick.set_float("test_y", -0.25);

        let mut right_stick = ParamPackage::default();
        right_stick.set_str("engine", ENGINE.to_string());
        right_stick.set_float("test_x", -0.5);
        right_stick.set_float("test_y", 0.5);

        let hid_core = Arc::new(Mutex::new(HIDCore::new()));
        {
            let hid_core = hid_core.lock();
            let controller = hid_core.get_emulated_controller(NpadIdType::Player1);
            drop(hid_core);
            let mut controller = controller.lock();
            controller.set_stick_param(native_analog::Values::LStick as usize, left_stick);
            controller.set_stick_param(native_analog::Values::RStick as usize, right_stick);
            controller.set_npad_style_index(NpadStyleIndex::Fullkey);
            controller.connect(false);
        }

        let mut applet_resource = AppletResource::new();
        applet_resource.set_shared_memory_backing(Arc::new(TestSharedMemoryBacking));
        assert!(applet_resource
            .register_applet_resource_user_id(ARUID, true)
            .is_success());
        assert!(applet_resource.create_applet_resource(ARUID).is_success());
        let applet_resource = Arc::new(Mutex::new(applet_resource));

        let mut npad = NPad::new_with_hid_core(hid_core);
        npad.set_npad_externals(AppletResourceHolder {
            applet_resource: Some(Arc::clone(&applet_resource)),
            handheld_config: None,
        });
        assert!(npad.register_applet_resource_user_id(ARUID).is_success());
        assert!(npad.activate_npad_resource_with_aruid(ARUID).is_success());
        assert!(npad
            .set_supported_npad_style_set(ARUID, NpadStyleSet::FULLKEY)
            .is_success());
        assert!(npad.activate().is_success());
        assert!(npad.activate_for_aruid(ARUID).is_success());

        npad.on_update();

        let resource = applet_resource.lock();
        let npad_state = &resource
            .get_shared_memory_format(ARUID)
            .unwrap()
            .npad
            .npad_entry[0]
            .internal_state;
        let fullkey = npad_state.fullkey_lifo.read_current_entry().state;
        let system_ext = npad_state.system_ext_lifo.read_current_entry().state;

        assert_eq!(fullkey.l_stick.x, (0.75 * HID_JOYSTICK_MAX as f32) as i32);
        assert_eq!(fullkey.l_stick.y, (-0.25 * HID_JOYSTICK_MAX as f32) as i32);
        assert_eq!(fullkey.r_stick.x, (-0.5 * HID_JOYSTICK_MAX as f32) as i32);
        assert_eq!(fullkey.r_stick.y, (0.5 * HID_JOYSTICK_MAX as f32) as i32);
        assert_eq!(system_ext.l_stick.x, fullkey.l_stick.x);
        assert_eq!(system_ext.l_stick.y, fullkey.l_stick.y);
        assert_eq!(system_ext.r_stick.x, fullkey.r_stick.x);
        assert_eq!(system_ext.r_stick.y, fullkey.r_stick.y);

        drop(resource);
        unregister_input_factory(ENGINE);
    }

    #[test]
    fn activate_for_aruid_prefills_npad_lifos_like_upstream() {
        const ARUID: u64 = 0x51;

        let mut applet_resource = AppletResource::new();
        applet_resource.set_shared_memory_backing(Arc::new(TestSharedMemoryBacking));
        assert!(applet_resource
            .register_applet_resource_user_id(ARUID, true)
            .is_success());
        assert!(applet_resource.create_applet_resource(ARUID).is_success());

        let applet_resource = Arc::new(Mutex::new(applet_resource));
        let mut npad = NPad::new();
        npad.set_npad_externals(AppletResourceHolder {
            applet_resource: Some(applet_resource.clone()),
            handheld_config: None,
        });

        assert!(npad.activate_for_aruid(ARUID).is_success());

        let resource = applet_resource.lock();
        let shared = resource.get_shared_memory_format(ARUID).unwrap();
        let state = &shared.npad.npad_entry[0].internal_state;
        // `WriteEmptyEntry` derives each state sample from the preceding
        // atomic marker, while `Lifo::WriteNextEntry` publishes twice that
        // state sample. Nineteen upstream prefill writes therefore produce
        // 2^19 - 1 rather than a linear sample count.
        const EXPECTED_PREFILL_SAMPLE: i64 = (1 << 19) - 1;
        assert_eq!(state.fullkey_lifo.buffer_count, 16);
        assert_eq!(state.fullkey_lifo.buffer_tail, 2);
        assert_eq!(
            state
                .fullkey_lifo
                .read_current_entry()
                .state
                .sampling_number,
            EXPECTED_PREFILL_SAMPLE
        );
        assert_eq!(
            state
                .system_ext_lifo
                .read_current_entry()
                .state
                .sampling_number,
            EXPECTED_PREFILL_SAMPLE
        );
        assert_eq!(
            state
                .gc_trigger_lifo
                .read_current_entry()
                .state
                .sampling_number,
            EXPECTED_PREFILL_SAMPLE
        );
    }

    #[test]
    fn on_update_routes_each_controller_style_to_its_upstream_lifo() {
        const ARUID: u64 = 0x52;
        let cases = [
            (NpadStyleIndex::Handheld, NpadStyleSet::HANDHELD, 0x3f),
            (NpadStyleIndex::JoyconDual, NpadStyleSet::JOY_DUAL, 0x15),
            (NpadStyleIndex::JoyconLeft, NpadStyleSet::JOY_LEFT, 0x05),
            (NpadStyleIndex::JoyconRight, NpadStyleSet::JOY_RIGHT, 0x11),
            (NpadStyleIndex::GameCube, NpadStyleSet::GC, 0x03),
            (NpadStyleIndex::Pokeball, NpadStyleSet::PALMA, 0x01),
        ];

        for (style, style_set, expected_connection) in cases {
            let hid_core = Arc::new(Mutex::new(HIDCore::new()));
            let controller = hid_core.lock().get_emulated_controller(NpadIdType::Player1);
            {
                let mut controller = controller.lock();
                controller.set_npad_style_index(style);
                controller.connect(false);
            }

            let mut applet_resource = AppletResource::new();
            applet_resource.set_shared_memory_backing(Arc::new(TestSharedMemoryBacking));
            assert!(applet_resource
                .register_applet_resource_user_id(ARUID, true)
                .is_success());
            assert!(applet_resource.create_applet_resource(ARUID).is_success());
            let applet_resource = Arc::new(Mutex::new(applet_resource));

            let mut npad = NPad::new_with_hid_core(hid_core);
            npad.set_npad_externals(AppletResourceHolder {
                applet_resource: Some(Arc::clone(&applet_resource)),
                handheld_config: None,
            });
            assert!(npad.register_applet_resource_user_id(ARUID).is_success());
            assert!(npad.activate_npad_resource_with_aruid(ARUID).is_success());
            assert!(npad
                .set_supported_npad_style_set(ARUID, NpadStyleSet::ALL)
                .is_success());
            assert!(npad.activate().is_success());
            assert!(npad.activate_for_aruid(ARUID).is_success());
            npad.on_update();

            let resource = applet_resource.lock();
            let state = &resource
                .get_shared_memory_format(ARUID)
                .unwrap()
                .npad
                .npad_entry[0]
                .internal_state;
            assert!(
                state.style_tag.raw.contains(style_set),
                "{style:?} did not expose {style_set:?}"
            );

            let connection_status = match style {
                NpadStyleIndex::Handheld => {
                    state
                        .handheld_lifo
                        .read_current_entry()
                        .state
                        .connection_status
                        .raw
                }
                NpadStyleIndex::JoyconDual => {
                    state
                        .joy_dual_lifo
                        .read_current_entry()
                        .state
                        .connection_status
                        .raw
                }
                NpadStyleIndex::JoyconLeft => {
                    state
                        .joy_left_lifo
                        .read_current_entry()
                        .state
                        .connection_status
                        .raw
                }
                NpadStyleIndex::JoyconRight => {
                    state
                        .joy_right_lifo
                        .read_current_entry()
                        .state
                        .connection_status
                        .raw
                }
                NpadStyleIndex::GameCube => {
                    assert!(
                        state
                            .gc_trigger_lifo
                            .read_current_entry()
                            .state
                            .sampling_number
                            > 19
                    );
                    state
                        .fullkey_lifo
                        .read_current_entry()
                        .state
                        .connection_status
                        .raw
                }
                NpadStyleIndex::Pokeball => {
                    state
                        .palma_lifo
                        .read_current_entry()
                        .state
                        .connection_status
                        .raw
                }
                _ => unreachable!(),
            };
            assert_eq!(
                connection_status, expected_connection,
                "{style:?} used the wrong connection bits"
            );
        }
    }

    #[test]
    fn coalesced_reconnect_reinitializes_shared_memory_for_final_style() {
        const ARUID: u64 = 0x53;

        let hid_core = Arc::new(Mutex::new(HIDCore::new()));
        let controller = hid_core.lock().get_emulated_controller(NpadIdType::Player1);
        {
            let mut controller = controller.lock();
            controller.set_npad_style_index(NpadStyleIndex::JoyconDual);
            controller.connect(false);
            controller.enable_configuration();
            controller.set_npad_style_index(NpadStyleIndex::Fullkey);
            controller.disable_configuration();
        }

        let mut applet_resource = AppletResource::new();
        applet_resource.set_shared_memory_backing(Arc::new(TestSharedMemoryBacking));
        assert!(applet_resource
            .register_applet_resource_user_id(ARUID, true)
            .is_success());
        assert!(applet_resource.create_applet_resource(ARUID).is_success());
        let applet_resource = Arc::new(Mutex::new(applet_resource));

        let mut npad = NPad::new_with_hid_core(Arc::clone(&hid_core));
        npad.set_npad_externals(AppletResourceHolder {
            applet_resource: Some(Arc::clone(&applet_resource)),
            handheld_config: None,
        });
        assert!(npad.register_applet_resource_user_id(ARUID).is_success());
        assert!(npad.activate_npad_resource_with_aruid(ARUID).is_success());
        assert!(npad.activate().is_success());
        assert!(npad.activate_for_aruid(ARUID).is_success());

        assert!(npad
            .set_supported_npad_style_set(ARUID, NpadStyleSet::JOY_DUAL)
            .is_success());
        {
            let resource = applet_resource.lock();
            let state = &resource
                .get_shared_memory_format(ARUID)
                .unwrap()
                .npad
                .npad_entry[0]
                .internal_state;
            assert_eq!(state.style_tag.raw, NpadStyleSet::JOY_DUAL);
        }

        // Supporting Fullkey again restores the configured controller type.
        // Disconnect, Type and Connect callbacks can all arrive before the
        // next update, but upstream applies them synchronously in this order.
        assert!(npad
            .set_supported_npad_style_set(ARUID, NpadStyleSet::ALL)
            .is_success());

        assert_eq!(
            controller.lock().get_npad_style_index(false),
            NpadStyleIndex::Fullkey
        );
        let resource = applet_resource.lock();
        let state = &resource
            .get_shared_memory_format(ARUID)
            .unwrap()
            .npad
            .npad_entry[0]
            .internal_state;
        assert_eq!(state.style_tag.raw, NpadStyleSet::FULLKEY);
        assert!(
            state
                .fullkey_lifo
                .read_current_entry()
                .state
                .connection_status
                .raw
                != 0
        );
    }

    #[test]
    fn abstracted_pads_are_owned_and_indexed_like_upstream() {
        let npad = NPad::new();
        for (index, pad) in npad.abstracted_pads.iter().enumerate() {
            assert_eq!(
                pad.get_last_active_npad(),
                crate::hid_util::index_to_npad_id_type(index)
            );
        }
    }
}

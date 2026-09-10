// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `input_common/input_poller.h` and `input_common/input_poller.cpp`.
//!
//! Provides InputFactory and OutputFactory that create input/output device
//! instances from ParamPackage parameters and an InputEngine.

use std::sync::Arc;

use parking_lot::Mutex;

use common::input::{
    AnalogProperties, AnalogStatus, BatteryLevel, BatteryStatus, BodyColorStatus, ButtonStatus,
    CallbackStatus, CameraFormat, CameraStatus, DriverResult, InputCallback, InputDevice,
    InputType, LedStatus, MifareRequest, MotionStatus, NfcState, NfcStatus, OutputDevice,
    PollingMode, StickStatus, TouchStatus, TriggerStatus, VibrationStatus,
};
use common::param_package::ParamPackage;
use common::uuid::UUID;

use crate::input_engine::{
    EngineInputType, InputEngine, InputIdentifier, PadIdentifier, UpdateCallback,
};

/// The half of an input device the engine's update callback has to reach.
///
/// Upstream registers `UpdateCallback engine_callback{[this]() { OnChange(); }}`
/// with `InputEngine::SetCallback`, so the engine calls straight back into the
/// device. A device here is handed out as a `Box<dyn InputDevice>` and cannot be
/// captured that way, so everything `OnChange` touches — the consumer's callback
/// and the value last reported to it — lives behind an `Arc` that the device and
/// the engine's closure both hold.
///
/// Registering with no callback at all, which this port did before, leaves the
/// engine with nothing to call: every binding then reports only what
/// `ForceUpdate` read at load time and never moves again.
struct DeviceState<T> {
    consumer: Mutex<InputCallback>,
    last_value: Mutex<T>,
}

impl<T> DeviceState<T> {
    fn new(last_value: T) -> Arc<Self> {
        Arc::new(Self {
            consumer: Mutex::new(InputCallback { on_change: None }),
            last_value: Mutex::new(last_value),
        })
    }

    /// Upstream `InputDevice::TriggerOnChange`.
    fn trigger(&self, status: &CallbackStatus) {
        // The consumer can reload devices and replace this callback.
        let callback = self.consumer.lock().on_change.clone();
        if let Some(on_change) = callback {
            on_change(status);
        }
    }
}

// ---- Helper: extract identifier from params ----

fn identifier_from_params(params: &ParamPackage) -> PadIdentifier {
    PadIdentifier {
        guid: UUID::from_string(&params.get_str("guid", "")),
        port: params.get_int("port", 0) as usize,
        pad: params.get_int("pad", 0) as usize,
    }
}

fn make_analog(raw_value: f32, properties: AnalogProperties) -> AnalogStatus {
    AnalogStatus {
        value: 0.0,
        raw_value,
        properties,
    }
}

// ---- DummyInput ----
// Port of DummyInput class from input_poller.cpp

struct DummyInput {
    callback: InputCallback,
}

impl DummyInput {
    fn new() -> Self {
        Self {
            callback: InputCallback { on_change: None },
        }
    }
}

impl InputDevice for DummyInput {
    fn set_callback(&mut self, callback: InputCallback) {
        self.callback = callback;
    }

    fn trigger_on_change(&self, status: &CallbackStatus) {
        if let Some(ref on_change) = self.callback.on_change {
            on_change(status);
        }
    }
}

// ---- InputFromButton ----
// Port of InputFromButton class from input_poller.cpp

/// Everything `InputFromButton::OnChange` reads, shared with the engine.
struct ButtonSource {
    identifier: PadIdentifier,
    button: i32,
    turbo: bool,
    toggle: bool,
    inverted: bool,
    input_engine: Arc<Mutex<InputEngine>>,
    state: Arc<DeviceState<bool>>,
}

impl ButtonSource {
    /// Upstream `InputFromButton::GetStatus`.
    fn get_status(&self) -> ButtonStatus {
        let engine = self.input_engine.lock();
        ButtonStatus {
            value: engine.get_button(&self.identifier, self.button),
            inverted: self.inverted,
            toggle: self.toggle,
            turbo: self.turbo,
            ..Default::default()
        }
    }

    /// Upstream `InputFromButton::OnChange`: report only real transitions.
    fn on_change(&self) {
        let status = CallbackStatus {
            input_type: InputType::Button,
            button_status: self.get_status(),
            ..Default::default()
        };
        {
            let mut last = self.state.last_value.lock();
            if status.button_status.value == *last {
                return;
            }
            *last = status.button_status.value;
        }
        self.state.trigger(&status);
    }
}

struct InputFromButton {
    source: Arc<ButtonSource>,
    callback_key: i32,
}

impl InputFromButton {
    fn new(
        identifier: PadIdentifier,
        button: i32,
        turbo: bool,
        toggle: bool,
        inverted: bool,
        input_engine: Arc<Mutex<InputEngine>>,
    ) -> Self {
        let source = Arc::new(ButtonSource {
            identifier: identifier.clone(),
            button,
            turbo,
            toggle,
            inverted,
            input_engine: Arc::clone(&input_engine),
            state: DeviceState::new(false),
        });

        let callback_source = Arc::clone(&source);
        let callback_key = {
            let mut engine = input_engine.lock();
            engine.set_callback(InputIdentifier {
                identifier,
                r#type: EngineInputType::Button,
                index: button,
                callback: UpdateCallback {
                    on_change: Some(Arc::new(move |_| callback_source.on_change())),
                },
            })
        };
        Self {
            source,
            callback_key,
        }
    }
}

impl InputDevice for InputFromButton {
    fn force_update(&mut self) {
        let status = CallbackStatus {
            input_type: InputType::Button,
            button_status: self.source.get_status(),
            ..Default::default()
        };
        *self.source.state.last_value.lock() = status.button_status.value;
        self.trigger_on_change(&status);
    }

    fn set_callback(&mut self, callback: InputCallback) {
        *self.source.state.consumer.lock() = callback;
    }

    fn trigger_on_change(&self, status: &CallbackStatus) {
        self.source.state.trigger(status);
    }
}

impl Drop for InputFromButton {
    fn drop(&mut self) {
        let mut engine = self.source.input_engine.lock();
        engine.delete_callback(self.callback_key);
    }
}

// ---- InputFromHatButton ----
// Port of InputFromHatButton class from input_poller.cpp

/// Everything `InputFromHatButton::OnChange` reads, shared with the engine.
struct HatButtonSource {
    identifier: PadIdentifier,
    button: i32,
    direction: u8,
    turbo: bool,
    toggle: bool,
    inverted: bool,
    input_engine: Arc<Mutex<InputEngine>>,
    state: Arc<DeviceState<bool>>,
}

impl HatButtonSource {
    /// Upstream `InputFromHatButton::GetStatus`.
    fn get_status(&self) -> ButtonStatus {
        let engine = self.input_engine.lock();
        ButtonStatus {
            value: engine.get_hat_button(&self.identifier, self.button, self.direction),
            inverted: self.inverted,
            toggle: self.toggle,
            turbo: self.turbo,
            ..Default::default()
        }
    }

    /// Upstream `InputFromHatButton::OnChange`.
    fn on_change(&self) {
        let status = CallbackStatus {
            input_type: InputType::Button,
            button_status: self.get_status(),
            ..Default::default()
        };
        {
            let mut last = self.state.last_value.lock();
            if status.button_status.value == *last {
                return;
            }
            *last = status.button_status.value;
        }
        self.state.trigger(&status);
    }
}

struct InputFromHatButton {
    source: Arc<HatButtonSource>,
    callback_key: i32,
}

impl InputFromHatButton {
    fn new(
        identifier: PadIdentifier,
        button: i32,
        direction: u8,
        turbo: bool,
        toggle: bool,
        inverted: bool,
        input_engine: Arc<Mutex<InputEngine>>,
    ) -> Self {
        let source = Arc::new(HatButtonSource {
            identifier: identifier.clone(),
            button,
            direction,
            turbo,
            toggle,
            inverted,
            input_engine: Arc::clone(&input_engine),
            state: DeviceState::new(false),
        });

        let callback_source = Arc::clone(&source);
        let callback_key = {
            let mut engine = input_engine.lock();
            engine.set_callback(InputIdentifier {
                identifier,
                r#type: EngineInputType::HatButton,
                index: button,
                callback: UpdateCallback {
                    on_change: Some(Arc::new(move |_| callback_source.on_change())),
                },
            })
        };
        Self {
            source,
            callback_key,
        }
    }
}

impl InputDevice for InputFromHatButton {
    fn force_update(&mut self) {
        let status = CallbackStatus {
            input_type: InputType::Button,
            button_status: self.source.get_status(),
            ..Default::default()
        };
        *self.source.state.last_value.lock() = status.button_status.value;
        self.trigger_on_change(&status);
    }

    fn set_callback(&mut self, callback: InputCallback) {
        *self.source.state.consumer.lock() = callback;
    }

    fn trigger_on_change(&self, status: &CallbackStatus) {
        self.source.state.trigger(status);
    }
}

impl Drop for InputFromHatButton {
    fn drop(&mut self) {
        let mut engine = self.source.input_engine.lock();
        engine.delete_callback(self.callback_key);
    }
}

// ---- InputFromStick ----
// Port of InputFromStick class from input_poller.cpp

/// Everything `InputFromStick::OnChange` reads, shared with the engine.
struct StickSource {
    identifier: PadIdentifier,
    axis_x: i32,
    axis_y: i32,
    properties_x: AnalogProperties,
    properties_y: AnalogProperties,
    input_engine: Arc<Mutex<InputEngine>>,
    invert_axis_y: bool,
    state: Arc<DeviceState<(f32, f32)>>,
}

impl StickSource {
    /// Upstream `InputFromStick::GetStatus`.
    fn get_status(&self) -> StickStatus {
        let engine = self.input_engine.lock();
        let mut status = StickStatus::default();
        status.x = make_analog(
            engine.get_axis(&self.identifier, self.axis_x),
            self.properties_x.clone(),
        );
        let mut raw_y = engine.get_axis(&self.identifier, self.axis_y);
        // Kept for compatibility with old yuzu versions: SDL's vertical axis
        // runs the other way round from Nintendo's.
        if self.invert_axis_y {
            raw_y = -raw_y;
        }
        status.y = make_analog(raw_y, self.properties_y.clone());
        status
    }

    /// Upstream `InputFromStick::OnChange`.
    fn on_change(&self) {
        let status = CallbackStatus {
            input_type: InputType::Stick,
            stick_status: self.get_status(),
            ..Default::default()
        };
        {
            let mut last = self.state.last_value.lock();
            let fresh = (
                status.stick_status.x.raw_value,
                status.stick_status.y.raw_value,
            );
            if fresh == *last {
                return;
            }
            *last = fresh;
        }
        self.state.trigger(&status);
    }
}

struct InputFromStick {
    source: Arc<StickSource>,
    callback_key_x: i32,
    callback_key_y: i32,
}

impl InputFromStick {
    fn new(
        identifier: PadIdentifier,
        axis_x: i32,
        axis_y: i32,
        properties_x: AnalogProperties,
        properties_y: AnalogProperties,
        input_engine: Arc<Mutex<InputEngine>>,
    ) -> Self {
        let invert_axis_y = {
            let engine = input_engine.lock();
            engine.get_engine_name() == "sdl"
        };
        let source = Arc::new(StickSource {
            identifier: identifier.clone(),
            axis_x,
            axis_y,
            properties_x,
            properties_y,
            input_engine: Arc::clone(&input_engine),
            invert_axis_y,
            state: DeviceState::new((0.0, 0.0)),
        });

        // Upstream hands the *same* `engine_callback` to both axes.
        let (callback_key_x, callback_key_y) = {
            let mut engine = input_engine.lock();
            let source_x = Arc::clone(&source);
            let kx = engine.set_callback(InputIdentifier {
                identifier: identifier.clone(),
                r#type: EngineInputType::Analog,
                index: axis_x,
                callback: UpdateCallback {
                    on_change: Some(Arc::new(move |_| source_x.on_change())),
                },
            });
            let source_y = Arc::clone(&source);
            let ky = engine.set_callback(InputIdentifier {
                identifier,
                r#type: EngineInputType::Analog,
                index: axis_y,
                callback: UpdateCallback {
                    on_change: Some(Arc::new(move |_| source_y.on_change())),
                },
            });
            (kx, ky)
        };
        Self {
            source,
            callback_key_x,
            callback_key_y,
        }
    }
}

impl InputDevice for InputFromStick {
    fn force_update(&mut self) {
        let status = CallbackStatus {
            input_type: InputType::Stick,
            stick_status: self.source.get_status(),
            ..Default::default()
        };
        *self.source.state.last_value.lock() = (
            status.stick_status.x.raw_value,
            status.stick_status.y.raw_value,
        );
        self.trigger_on_change(&status);
    }

    fn set_callback(&mut self, callback: InputCallback) {
        *self.source.state.consumer.lock() = callback;
    }

    fn trigger_on_change(&self, status: &CallbackStatus) {
        self.source.state.trigger(status);
    }
}

impl Drop for InputFromStick {
    fn drop(&mut self) {
        let mut engine = self.source.input_engine.lock();
        engine.delete_callback(self.callback_key_x);
        engine.delete_callback(self.callback_key_y);
    }
}

// ---- InputFromTouch ----
// Port of InputFromTouch class from input_poller.cpp

struct InputFromTouch {
    callback_key_button: i32,
    callback_key_x: i32,
    callback_key_y: i32,
    input_engine: Arc<Mutex<InputEngine>>,
    state: Arc<Mutex<TouchCallbackState>>,
}

struct TouchCallbackState {
    button: i32,
    axis_x: i32,
    axis_y: i32,
    touch_status: TouchStatus,
    callback: InputCallback,
}

impl TouchCallbackState {
    fn update(&mut self, event: &crate::input_engine::MappingData) {
        let changed = match event.r#type {
            EngineInputType::Button if event.index == self.button => {
                let changed = self.touch_status.pressed.value != event.button_value;
                self.touch_status.pressed.value = event.button_value;
                changed
            }
            EngineInputType::Analog if event.index == self.axis_x => {
                let changed = self.touch_status.x.raw_value != event.axis_value;
                self.touch_status.x.raw_value = event.axis_value;
                changed
            }
            EngineInputType::Analog if event.index == self.axis_y => {
                let changed = self.touch_status.y.raw_value != event.axis_value;
                self.touch_status.y.raw_value = event.axis_value;
                changed
            }
            _ => false,
        };
        if changed {
            self.notify();
        }
    }

    fn notify(&self) {
        if let Some(ref on_change) = self.callback.on_change {
            on_change(&CallbackStatus {
                input_type: InputType::Touch,
                touch_status: self.touch_status,
                ..Default::default()
            });
        }
    }
}

fn touch_update_callback(state: Arc<Mutex<TouchCallbackState>>) -> UpdateCallback {
    UpdateCallback {
        on_change: Some(Arc::new(move |event| state.lock().update(event))),
    }
}

impl InputFromTouch {
    fn new(
        identifier: PadIdentifier,
        button: i32,
        toggle: bool,
        inverted: bool,
        axis_x: i32,
        axis_y: i32,
        properties_x: AnalogProperties,
        properties_y: AnalogProperties,
        input_engine: Arc<Mutex<InputEngine>>,
    ) -> Self {
        let state = Arc::new(Mutex::new(TouchCallbackState {
            button,
            axis_x,
            axis_y,
            touch_status: TouchStatus {
                pressed: ButtonStatus {
                    inverted,
                    toggle,
                    ..Default::default()
                },
                x: make_analog(0.0, properties_x),
                y: make_analog(0.0, properties_y),
                ..Default::default()
            },
            callback: InputCallback { on_change: None },
        }));
        let (kb, kx, ky) = {
            let mut engine = input_engine.lock();
            let kb = engine.set_callback(InputIdentifier {
                identifier: identifier.clone(),
                r#type: EngineInputType::Button,
                index: button,
                callback: touch_update_callback(Arc::clone(&state)),
            });
            let kx = engine.set_callback(InputIdentifier {
                identifier: identifier.clone(),
                r#type: EngineInputType::Analog,
                index: axis_x,
                callback: touch_update_callback(Arc::clone(&state)),
            });
            let ky = engine.set_callback(InputIdentifier {
                identifier,
                r#type: EngineInputType::Analog,
                index: axis_y,
                callback: touch_update_callback(Arc::clone(&state)),
            });
            (kb, kx, ky)
        };
        Self {
            callback_key_button: kb,
            callback_key_x: kx,
            callback_key_y: ky,
            input_engine,
            state,
        }
    }
}

impl InputDevice for InputFromTouch {
    fn force_update(&mut self) {
        self.state.lock().notify();
    }

    fn set_callback(&mut self, callback: InputCallback) {
        self.state.lock().callback = callback;
    }

    fn trigger_on_change(&self, status: &CallbackStatus) {
        if let Some(ref on_change) = self.state.lock().callback.on_change {
            on_change(status);
        }
    }
}

impl Drop for InputFromTouch {
    fn drop(&mut self) {
        let mut engine = self.input_engine.lock();
        engine.delete_callback(self.callback_key_button);
        engine.delete_callback(self.callback_key_x);
        engine.delete_callback(self.callback_key_y);
    }
}

// ---- InputFromTrigger ----
// Port of InputFromTrigger class from input_poller.cpp

/// Everything `InputFromTrigger::OnChange` reads, shared with the engine.
struct TriggerSource {
    identifier: PadIdentifier,
    button: i32,
    toggle: bool,
    inverted: bool,
    axis: i32,
    properties: AnalogProperties,
    input_engine: Arc<Mutex<InputEngine>>,
    state: Arc<DeviceState<(bool, f32)>>,
}

impl TriggerSource {
    /// Upstream `InputFromTrigger::GetStatus`.
    fn get_status(&self) -> TriggerStatus {
        let engine = self.input_engine.lock();
        TriggerStatus {
            analog: make_analog(
                engine.get_axis(&self.identifier, self.axis),
                self.properties.clone(),
            ),
            pressed: ButtonStatus {
                value: engine.get_button(&self.identifier, self.button),
                inverted: self.inverted,
                toggle: self.toggle,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Upstream `InputFromTrigger::OnChange`.
    fn on_change(&self) {
        let status = CallbackStatus {
            input_type: InputType::Trigger,
            trigger_status: self.get_status(),
            ..Default::default()
        };
        {
            let mut last = self.state.last_value.lock();
            let fresh = (
                status.trigger_status.pressed.value,
                status.trigger_status.analog.raw_value,
            );
            if fresh == *last {
                return;
            }
            *last = fresh;
        }
        self.state.trigger(&status);
    }
}

struct InputFromTrigger {
    source: Arc<TriggerSource>,
    callback_key_button: i32,
    axis_callback_key: i32,
}

impl InputFromTrigger {
    fn new(
        identifier: PadIdentifier,
        button: i32,
        toggle: bool,
        inverted: bool,
        axis: i32,
        properties: AnalogProperties,
        input_engine: Arc<Mutex<InputEngine>>,
    ) -> Self {
        let source = Arc::new(TriggerSource {
            identifier: identifier.clone(),
            button,
            toggle,
            inverted,
            axis,
            properties,
            input_engine: Arc::clone(&input_engine),
            state: DeviceState::new((false, 0.0)),
        });

        let (callback_key_button, axis_callback_key) = {
            let mut engine = input_engine.lock();
            let button_source = Arc::clone(&source);
            let kb = engine.set_callback(InputIdentifier {
                identifier: identifier.clone(),
                r#type: EngineInputType::Button,
                index: button,
                callback: UpdateCallback {
                    on_change: Some(Arc::new(move |_| button_source.on_change())),
                },
            });
            let axis_source = Arc::clone(&source);
            let ka = engine.set_callback(InputIdentifier {
                identifier,
                r#type: EngineInputType::Analog,
                index: axis,
                callback: UpdateCallback {
                    on_change: Some(Arc::new(move |_| axis_source.on_change())),
                },
            });
            (kb, ka)
        };
        Self {
            source,
            callback_key_button,
            axis_callback_key,
        }
    }
}

impl InputDevice for InputFromTrigger {
    fn force_update(&mut self) {
        let status = CallbackStatus {
            input_type: InputType::Trigger,
            trigger_status: self.source.get_status(),
            ..Default::default()
        };
        *self.source.state.last_value.lock() = (
            status.trigger_status.pressed.value,
            status.trigger_status.analog.raw_value,
        );
        self.trigger_on_change(&status);
    }

    fn set_callback(&mut self, callback: InputCallback) {
        *self.source.state.consumer.lock() = callback;
    }

    fn trigger_on_change(&self, status: &CallbackStatus) {
        self.source.state.trigger(status);
    }
}

impl Drop for InputFromTrigger {
    fn drop(&mut self) {
        let mut engine = self.source.input_engine.lock();
        engine.delete_callback(self.callback_key_button);
        engine.delete_callback(self.axis_callback_key);
    }
}

// ---- InputFromAnalog ----
// Port of InputFromAnalog class from input_poller.cpp

/// Everything `InputFromAnalog::OnChange` reads, shared with the engine.
struct AnalogSource {
    identifier: PadIdentifier,
    axis: i32,
    properties: AnalogProperties,
    input_engine: Arc<Mutex<InputEngine>>,
    state: Arc<DeviceState<f32>>,
}

impl AnalogSource {
    /// Upstream `InputFromAnalog::GetStatus`.
    fn get_status(&self) -> AnalogStatus {
        let engine = self.input_engine.lock();
        make_analog(
            engine.get_axis(&self.identifier, self.axis),
            self.properties.clone(),
        )
    }

    /// Upstream `InputFromAnalog::OnChange`.
    fn on_change(&self) {
        let status = CallbackStatus {
            input_type: InputType::Analog,
            analog_status: self.get_status(),
            ..Default::default()
        };
        {
            let mut last = self.state.last_value.lock();
            if status.analog_status.raw_value == *last {
                return;
            }
            *last = status.analog_status.raw_value;
        }
        self.state.trigger(&status);
    }
}

struct InputFromAnalog {
    source: Arc<AnalogSource>,
    callback_key: i32,
}

impl InputFromAnalog {
    fn new(
        identifier: PadIdentifier,
        axis: i32,
        properties: AnalogProperties,
        input_engine: Arc<Mutex<InputEngine>>,
    ) -> Self {
        let source = Arc::new(AnalogSource {
            identifier: identifier.clone(),
            axis,
            properties,
            input_engine: Arc::clone(&input_engine),
            state: DeviceState::new(0.0),
        });

        let callback_source = Arc::clone(&source);
        let callback_key = {
            let mut engine = input_engine.lock();
            engine.set_callback(InputIdentifier {
                identifier,
                r#type: EngineInputType::Analog,
                index: axis,
                callback: UpdateCallback {
                    on_change: Some(Arc::new(move |_| callback_source.on_change())),
                },
            })
        };
        Self {
            source,
            callback_key,
        }
    }
}

impl InputDevice for InputFromAnalog {
    fn force_update(&mut self) {
        let status = CallbackStatus {
            input_type: InputType::Analog,
            analog_status: self.source.get_status(),
            ..Default::default()
        };
        *self.source.state.last_value.lock() = status.analog_status.raw_value;
        self.trigger_on_change(&status);
    }

    fn set_callback(&mut self, callback: InputCallback) {
        *self.source.state.consumer.lock() = callback;
    }

    fn trigger_on_change(&self, status: &CallbackStatus) {
        self.source.state.trigger(status);
    }
}

impl Drop for InputFromAnalog {
    fn drop(&mut self) {
        let mut engine = self.source.input_engine.lock();
        engine.delete_callback(self.callback_key);
    }
}

// ---- InputFromBattery ----
// Port of InputFromBattery class from input_poller.cpp

/// Everything `InputFromBattery::OnChange` reads, shared with the engine.
struct BatterySource {
    identifier: PadIdentifier,
    input_engine: Arc<Mutex<InputEngine>>,
    state: Arc<DeviceState<BatteryStatus>>,
}

impl BatterySource {
    /// Upstream `InputFromBattery::GetStatus`.
    fn get_status(&self) -> BatteryStatus {
        let engine = self.input_engine.lock();
        engine.get_battery(&self.identifier)
    }

    /// Upstream `InputFromBattery::OnChange`.
    fn on_change(&self) {
        let status = CallbackStatus {
            input_type: InputType::Battery,
            battery_status: self.get_status(),
            ..Default::default()
        };
        {
            let mut last = self.state.last_value.lock();
            if status.battery_status == *last {
                return;
            }
            *last = status.battery_status;
        }
        self.state.trigger(&status);
    }
}

struct InputFromBattery {
    source: Arc<BatterySource>,
    callback_key: i32,
}

impl InputFromBattery {
    fn new(identifier: PadIdentifier, input_engine: Arc<Mutex<InputEngine>>) -> Self {
        let source = Arc::new(BatterySource {
            identifier: identifier.clone(),
            input_engine: Arc::clone(&input_engine),
            state: DeviceState::new(BatteryLevel::Charging),
        });
        let callback_source = Arc::clone(&source);
        let callback_key = {
            let mut engine = input_engine.lock();
            engine.set_callback(InputIdentifier {
                identifier,
                r#type: EngineInputType::Battery,
                index: 0,
                callback: UpdateCallback {
                    on_change: Some(Arc::new(move |_| callback_source.on_change())),
                },
            })
        };
        Self {
            source,
            callback_key,
        }
    }
}

impl InputDevice for InputFromBattery {
    fn force_update(&mut self) {
        let status = CallbackStatus {
            input_type: InputType::Battery,
            battery_status: self.source.get_status(),
            ..Default::default()
        };
        *self.source.state.last_value.lock() = status.battery_status;
        self.trigger_on_change(&status);
    }

    fn set_callback(&mut self, callback: InputCallback) {
        *self.source.state.consumer.lock() = callback;
    }

    fn trigger_on_change(&self, status: &CallbackStatus) {
        self.source.state.trigger(status);
    }
}

impl Drop for InputFromBattery {
    fn drop(&mut self) {
        self.source
            .input_engine
            .lock()
            .delete_callback(self.callback_key);
    }
}

// ---- InputFromColor ----
// Port of InputFromColor class from input_poller.cpp

/// Everything `InputFromColor::OnChange` reads, shared with the engine.
struct ColorSource {
    identifier: PadIdentifier,
    input_engine: Arc<Mutex<InputEngine>>,
    state: Arc<DeviceState<BodyColorStatus>>,
}

impl ColorSource {
    /// Upstream `InputFromColor::GetStatus`.
    fn get_status(&self) -> BodyColorStatus {
        self.input_engine.lock().get_color(&self.identifier)
    }

    /// Upstream `InputFromColor::OnChange`.
    fn on_change(&self) {
        let status = CallbackStatus {
            input_type: InputType::Color,
            color_status: self.get_status(),
            ..Default::default()
        };
        {
            let mut last = self.state.last_value.lock();
            if status.color_status == *last {
                return;
            }
            *last = status.color_status;
        }
        self.state.trigger(&status);
    }
}

struct InputFromColor {
    source: Arc<ColorSource>,
    callback_key: i32,
}

impl InputFromColor {
    fn new(identifier: PadIdentifier, input_engine: Arc<Mutex<InputEngine>>) -> Self {
        let source = Arc::new(ColorSource {
            identifier: identifier.clone(),
            input_engine: Arc::clone(&input_engine),
            state: DeviceState::new(BodyColorStatus::default()),
        });
        let callback_source = Arc::clone(&source);
        let callback_key = {
            let mut engine = input_engine.lock();
            engine.set_callback(InputIdentifier {
                identifier,
                r#type: EngineInputType::Color,
                index: 0,
                callback: UpdateCallback {
                    on_change: Some(Arc::new(move |_| callback_source.on_change())),
                },
            })
        };
        Self {
            source,
            callback_key,
        }
    }
}

impl InputDevice for InputFromColor {
    fn force_update(&mut self) {
        let status = CallbackStatus {
            input_type: InputType::Color,
            color_status: self.source.get_status(),
            ..Default::default()
        };
        *self.source.state.last_value.lock() = status.color_status;
        self.trigger_on_change(&status);
    }

    fn set_callback(&mut self, callback: InputCallback) {
        *self.source.state.consumer.lock() = callback;
    }

    fn trigger_on_change(&self, status: &CallbackStatus) {
        self.source.state.trigger(status);
    }
}

impl Drop for InputFromColor {
    fn drop(&mut self) {
        self.source
            .input_engine
            .lock()
            .delete_callback(self.callback_key);
    }
}

// ---- InputFromMotion ----
// Port of InputFromMotion class from input_poller.cpp

/// Everything `InputFromMotion::OnChange` reads, shared with the engine.
struct MotionSource {
    identifier: PadIdentifier,
    motion_sensor: i32,
    gyro_threshold: f32,
    input_engine: Arc<Mutex<InputEngine>>,
    state: Arc<DeviceState<()>>,
}

impl MotionSource {
    /// Upstream `InputFromMotion::GetStatus`.
    fn get_status(&self) -> MotionStatus {
        let engine = self.input_engine.lock();
        let bm = engine.get_motion(&self.identifier, self.motion_sensor);
        let props = AnalogProperties {
            deadzone: 0.0,
            range: 1.0,
            threshold: self.gyro_threshold,
            offset: 0.0,
            ..Default::default()
        };
        let mut s = MotionStatus::default();
        s.accel.x = make_analog(bm.accel_x, props.clone());
        s.accel.y = make_analog(bm.accel_y, props.clone());
        s.accel.z = make_analog(bm.accel_z, props.clone());
        s.gyro.x = make_analog(bm.gyro_x, props.clone());
        s.gyro.y = make_analog(bm.gyro_y, props.clone());
        s.gyro.z = make_analog(bm.gyro_z, props);
        s.delta_timestamp = bm.delta_timestamp;
        s
    }

    /// Upstream `InputFromMotion::OnChange`, which reports every sample
    /// unconditionally — motion data is continuous.
    fn on_change(&self) {
        let status = CallbackStatus {
            input_type: InputType::Motion,
            motion_status: self.get_status(),
            ..Default::default()
        };
        self.state.trigger(&status);
    }
}

struct InputFromMotion {
    source: Arc<MotionSource>,
    callback_key: i32,
}

impl InputFromMotion {
    fn new(
        identifier: PadIdentifier,
        motion_sensor: i32,
        gyro_threshold: f32,
        input_engine: Arc<Mutex<InputEngine>>,
    ) -> Self {
        let source = Arc::new(MotionSource {
            identifier: identifier.clone(),
            motion_sensor,
            gyro_threshold,
            input_engine: Arc::clone(&input_engine),
            state: DeviceState::new(()),
        });
        let callback_source = Arc::clone(&source);
        let callback_key = {
            let mut engine = input_engine.lock();
            engine.set_callback(InputIdentifier {
                identifier,
                r#type: EngineInputType::Motion,
                index: motion_sensor,
                callback: UpdateCallback {
                    on_change: Some(Arc::new(move |_| callback_source.on_change())),
                },
            })
        };
        Self {
            source,
            callback_key,
        }
    }
}

impl InputDevice for InputFromMotion {
    fn force_update(&mut self) {
        let status = CallbackStatus {
            input_type: InputType::Motion,
            motion_status: self.source.get_status(),
            ..Default::default()
        };
        self.trigger_on_change(&status);
    }

    fn set_callback(&mut self, callback: InputCallback) {
        *self.source.state.consumer.lock() = callback;
    }

    fn trigger_on_change(&self, status: &CallbackStatus) {
        self.source.state.trigger(status);
    }
}

impl Drop for InputFromMotion {
    fn drop(&mut self) {
        self.source
            .input_engine
            .lock()
            .delete_callback(self.callback_key);
    }
}

// ---- InputFromAxisMotion ----
// Port of InputFromAxisMotion class from input_poller.cpp

/// Everything `InputFromAxisMotion::OnChange` reads, shared with the engine.
struct AxisMotionSource {
    identifier: PadIdentifier,
    axis_x: i32,
    axis_y: i32,
    axis_z: i32,
    properties_x: AnalogProperties,
    properties_y: AnalogProperties,
    properties_z: AnalogProperties,
    input_engine: Arc<Mutex<InputEngine>>,
    state: Arc<DeviceState<(f32, f32, f32)>>,
}

impl AxisMotionSource {
    /// Upstream `InputFromAxisMotion::GetStatus`.
    fn get_status(&self) -> MotionStatus {
        let engine = self.input_engine.lock();
        let mut s = MotionStatus::default();
        s.gyro.x = make_analog(
            engine.get_axis(&self.identifier, self.axis_x),
            self.properties_x.clone(),
        );
        s.gyro.y = make_analog(
            engine.get_axis(&self.identifier, self.axis_y),
            self.properties_y.clone(),
        );
        s.gyro.z = make_analog(
            engine.get_axis(&self.identifier, self.axis_z),
            self.properties_z.clone(),
        );
        s.delta_timestamp = 1000;
        s.force_update = true;
        s
    }

    /// Upstream `InputFromAxisMotion::OnChange`.
    fn on_change(&self) {
        let status = CallbackStatus {
            input_type: InputType::Motion,
            motion_status: self.get_status(),
            ..Default::default()
        };
        {
            let mut last = self.state.last_value.lock();
            let fresh = (
                status.motion_status.gyro.x.raw_value,
                status.motion_status.gyro.y.raw_value,
                status.motion_status.gyro.z.raw_value,
            );
            if fresh == *last {
                return;
            }
            *last = fresh;
        }
        self.state.trigger(&status);
    }
}

struct InputFromAxisMotion {
    source: Arc<AxisMotionSource>,
    callback_key_x: i32,
    callback_key_y: i32,
    callback_key_z: i32,
}

impl InputFromAxisMotion {
    fn new(
        identifier: PadIdentifier,
        axis_x: i32,
        axis_y: i32,
        axis_z: i32,
        properties_x: AnalogProperties,
        properties_y: AnalogProperties,
        properties_z: AnalogProperties,
        input_engine: Arc<Mutex<InputEngine>>,
    ) -> Self {
        let source = Arc::new(AxisMotionSource {
            identifier: identifier.clone(),
            axis_x,
            axis_y,
            axis_z,
            properties_x,
            properties_y,
            properties_z,
            input_engine: Arc::clone(&input_engine),
            state: DeviceState::new((0.0, 0.0, 0.0)),
        });

        let (callback_key_x, callback_key_y, callback_key_z) = {
            let mut engine = input_engine.lock();
            let mut register = |index: i32, identifier: PadIdentifier| {
                let source = Arc::clone(&source);
                engine.set_callback(InputIdentifier {
                    identifier,
                    r#type: EngineInputType::Analog,
                    index,
                    callback: UpdateCallback {
                        on_change: Some(Arc::new(move |_| source.on_change())),
                    },
                })
            };
            let kx = register(axis_x, identifier.clone());
            let ky = register(axis_y, identifier.clone());
            let kz = register(axis_z, identifier);
            (kx, ky, kz)
        };
        Self {
            source,
            callback_key_x,
            callback_key_y,
            callback_key_z,
        }
    }
}

impl InputDevice for InputFromAxisMotion {
    fn force_update(&mut self) {
        let status = CallbackStatus {
            input_type: InputType::Motion,
            motion_status: self.source.get_status(),
            ..Default::default()
        };
        *self.source.state.last_value.lock() = (
            status.motion_status.gyro.x.raw_value,
            status.motion_status.gyro.y.raw_value,
            status.motion_status.gyro.z.raw_value,
        );
        self.trigger_on_change(&status);
    }

    fn set_callback(&mut self, callback: InputCallback) {
        *self.source.state.consumer.lock() = callback;
    }

    fn trigger_on_change(&self, status: &CallbackStatus) {
        self.source.state.trigger(status);
    }
}

impl Drop for InputFromAxisMotion {
    fn drop(&mut self) {
        let mut engine = self.source.input_engine.lock();
        engine.delete_callback(self.callback_key_x);
        engine.delete_callback(self.callback_key_y);
        engine.delete_callback(self.callback_key_z);
    }
}

// ---- InputFromCamera ----
// Port of InputFromCamera class from input_poller.cpp

/// Everything `InputFromCamera::OnChange` reads, shared with the engine.
struct CameraSource {
    identifier: PadIdentifier,
    input_engine: Arc<Mutex<InputEngine>>,
    state: Arc<DeviceState<()>>,
}

impl CameraSource {
    /// Upstream `InputFromCamera::GetStatus`.
    fn get_status(&self) -> CameraStatus {
        self.input_engine.lock().get_camera(&self.identifier)
    }

    /// Upstream `InputFromCamera::OnChange`.
    fn on_change(&self) {
        let camera = self.get_status();
        let status = CallbackStatus {
            input_type: InputType::IrSensor,
            camera_status: camera.format,
            raw_data: camera.data,
            ..Default::default()
        };
        self.state.trigger(&status);
    }
}

struct InputFromCamera {
    source: Arc<CameraSource>,
    callback_key: i32,
}

impl InputFromCamera {
    fn new(identifier: PadIdentifier, input_engine: Arc<Mutex<InputEngine>>) -> Self {
        let source = Arc::new(CameraSource {
            identifier: identifier.clone(),
            input_engine: Arc::clone(&input_engine),
            state: DeviceState::new(()),
        });
        let callback_source = Arc::clone(&source);
        let callback_key = {
            let mut engine = input_engine.lock();
            engine.set_callback(InputIdentifier {
                identifier,
                r#type: EngineInputType::Camera,
                index: 0,
                callback: UpdateCallback {
                    on_change: Some(Arc::new(move |_| callback_source.on_change())),
                },
            })
        };
        Self {
            source,
            callback_key,
        }
    }
}

impl InputDevice for InputFromCamera {
    fn force_update(&mut self) {
        self.source.on_change();
    }

    fn set_callback(&mut self, callback: InputCallback) {
        *self.source.state.consumer.lock() = callback;
    }

    fn trigger_on_change(&self, status: &CallbackStatus) {
        self.source.state.trigger(status);
    }
}

impl Drop for InputFromCamera {
    fn drop(&mut self) {
        self.source
            .input_engine
            .lock()
            .delete_callback(self.callback_key);
    }
}

// ---- InputFromNfc ----
// Port of InputFromNfc class from input_poller.cpp

/// Everything `InputFromNfc::OnChange` reads, shared with the engine.
struct NfcSource {
    identifier: PadIdentifier,
    input_engine: Arc<Mutex<InputEngine>>,
    state: Arc<DeviceState<()>>,
}

impl NfcSource {
    /// Upstream `InputFromNfc::GetStatus`.
    fn get_status(&self) -> NfcStatus {
        self.input_engine.lock().get_nfc(&self.identifier)
    }

    /// Upstream `InputFromNfc::OnChange`.
    fn on_change(&self) {
        let status = CallbackStatus {
            input_type: InputType::Nfc,
            nfc_status: self.get_status(),
            ..Default::default()
        };
        self.state.trigger(&status);
    }
}

struct InputFromNfc {
    source: Arc<NfcSource>,
    callback_key: i32,
}

impl InputFromNfc {
    fn new(identifier: PadIdentifier, input_engine: Arc<Mutex<InputEngine>>) -> Self {
        let source = Arc::new(NfcSource {
            identifier: identifier.clone(),
            input_engine: Arc::clone(&input_engine),
            state: DeviceState::new(()),
        });
        let callback_source = Arc::clone(&source);
        let callback_key = {
            let mut engine = input_engine.lock();
            engine.set_callback(InputIdentifier {
                identifier,
                r#type: EngineInputType::Nfc,
                index: 0,
                callback: UpdateCallback {
                    on_change: Some(Arc::new(move |_| callback_source.on_change())),
                },
            })
        };
        Self {
            source,
            callback_key,
        }
    }
}

impl InputDevice for InputFromNfc {
    fn force_update(&mut self) {
        self.source.on_change();
    }

    fn set_callback(&mut self, callback: InputCallback) {
        *self.source.state.consumer.lock() = callback;
    }

    fn trigger_on_change(&self, status: &CallbackStatus) {
        self.source.state.trigger(status);
    }
}

impl Drop for InputFromNfc {
    fn drop(&mut self) {
        self.source
            .input_engine
            .lock()
            .delete_callback(self.callback_key);
    }
}

// ---- OutputFromIdentifier ----
// Port of OutputFromIdentifier class from input_poller.cpp

struct OutputFromIdentifier {
    identifier: PadIdentifier,
    input_engine: Arc<Mutex<InputEngine>>,
}

impl OutputFromIdentifier {
    /// Clone the concrete driver before invoking it. This mirrors C++ virtual
    /// dispatch without retaining the composed `InputEngine` mutex across a
    /// driver call that may publish fresh input through that same engine.
    fn output_handler(&self) -> Option<Arc<dyn crate::input_engine::InputEngineOutput>> {
        self.input_engine.lock().output_handler()
    }
}

impl OutputDevice for OutputFromIdentifier {
    fn set_led(&mut self, status: &LedStatus) -> DriverResult {
        self.output_handler()
            .map_or(DriverResult::NotSupported, |output| {
                output.set_leds(&self.identifier, status)
            })
    }

    fn set_vibration(&mut self, status: &VibrationStatus) -> DriverResult {
        self.output_handler()
            .map_or(DriverResult::NotSupported, |output| {
                output.set_vibration(&self.identifier, status)
            })
    }

    fn is_vibration_enabled(&self) -> bool {
        self.output_handler()
            .is_some_and(|output| output.is_vibration_enabled(&self.identifier))
    }

    fn set_polling_mode(&mut self, mode: PollingMode) -> DriverResult {
        self.output_handler()
            .map_or(DriverResult::NotSupported, |output| {
                output.set_polling_mode(&self.identifier, mode)
            })
    }

    fn set_camera_format(&mut self, format: CameraFormat) -> DriverResult {
        self.output_handler()
            .map_or(DriverResult::NotSupported, |output| {
                output.set_camera_format(&self.identifier, format)
            })
    }

    fn supports_nfc(&self) -> NfcState {
        self.output_handler()
            .map_or(NfcState::NotSupported, |output| {
                output.supports_nfc(&self.identifier)
            })
    }

    fn start_nfc_polling(&mut self) -> NfcState {
        self.output_handler()
            .map_or(NfcState::NotSupported, |output| {
                output.start_nfc_polling(&self.identifier)
            })
    }

    fn stop_nfc_polling(&mut self) -> NfcState {
        self.output_handler()
            .map_or(NfcState::NotSupported, |output| {
                output.stop_nfc_polling(&self.identifier)
            })
    }

    fn read_amiibo_data(&mut self, out_data: &mut Vec<u8>) -> NfcState {
        self.output_handler()
            .map_or(NfcState::NotSupported, |output| {
                output.read_amiibo_data(&self.identifier, out_data)
            })
    }

    fn write_nfc_data(&mut self, data: &[u8]) -> NfcState {
        self.output_handler()
            .map_or(NfcState::NotSupported, |output| {
                output.write_nfc_data(&self.identifier, data)
            })
    }

    fn read_mifare_data(
        &mut self,
        request: &MifareRequest,
        out_data: &mut MifareRequest,
    ) -> NfcState {
        self.output_handler()
            .map_or(NfcState::NotSupported, |output| {
                output.read_mifare_data(&self.identifier, request, out_data)
            })
    }

    fn write_mifare_data(&mut self, request: &MifareRequest) -> NfcState {
        self.output_handler()
            .map_or(NfcState::NotSupported, |output| {
                output.write_mifare_data(&self.identifier, request)
            })
    }
}

// ---- OutputFactory ----
// Port of `OutputFactory` class from input_poller.h

pub struct OutputFactory {
    input_engine: Arc<Mutex<InputEngine>>,
}

impl OutputFactory {
    pub fn new(input_engine: Arc<Mutex<InputEngine>>) -> Self {
        Self { input_engine }
    }

    /// Port of OutputFactory::Create
    pub fn create(&self, params: &ParamPackage) -> Box<dyn OutputDevice> {
        let identifier = identifier_from_params(params);
        self.input_engine.lock().pre_set_controller(&identifier);
        Box::new(OutputFromIdentifier {
            identifier,
            input_engine: Arc::clone(&self.input_engine),
        })
    }
}

impl common::input::OutputDeviceFactory for OutputFactory {
    fn create(&self, params: &ParamPackage) -> Box<dyn OutputDevice> {
        OutputFactory::create(self, params)
    }
}

// ---- InputFactory ----
// Port of `InputFactory` class from input_poller.h

pub struct InputFactory {
    input_engine: Arc<Mutex<InputEngine>>,
}

impl InputFactory {
    pub fn new(input_engine: Arc<Mutex<InputEngine>>) -> Self {
        Self { input_engine }
    }

    /// Port of InputFactory::Create
    pub fn create(&self, params: &ParamPackage) -> Box<dyn InputDevice> {
        if params.has("battery") {
            return self.create_battery_device(params);
        }
        if params.has("color") {
            return self.create_color_device(params);
        }
        if params.has("camera") {
            return self.create_camera_device(params);
        }
        if params.has("nfc") {
            return self.create_nfc_device(params);
        }
        if params.has("button") && params.has("axis") {
            return self.create_trigger_device(params);
        }
        if params.has("button") && params.has("axis_x") && params.has("axis_y") {
            return self.create_touch_device(params);
        }
        if params.has("button") || params.has("code") {
            return self.create_button_device(params);
        }
        if params.has("hat") {
            return self.create_hat_button_device(params);
        }
        if params.has("axis_x") && params.has("axis_y") && params.has("axis_z") {
            return self.create_motion_device(params.clone());
        }
        if params.has("motion") {
            return self.create_motion_device(params.clone());
        }
        if params.has("axis_x") && params.has("axis_y") {
            return self.create_stick_device(params);
        }
        if params.has("axis") {
            return self.create_analog_device(params);
        }
        log::error!("Invalid parameters given");
        Box::new(DummyInput::new())
    }

    fn create_button_device(&self, params: &ParamPackage) -> Box<dyn InputDevice> {
        let id = identifier_from_params(params);
        let button_id = params.get_int("button", 0);
        let keyboard_key = params.get_int("code", 0);
        let toggle = params.get_int("toggle", 0) != 0;
        let inverted = params.get_int("inverted", 0) != 0;
        let turbo = params.get_int("turbo", 0) != 0;
        {
            let mut e = self.input_engine.lock();
            e.pre_set_controller(&id);
            e.pre_set_button(&id, button_id);
            e.pre_set_button(&id, keyboard_key);
        }
        let key = if keyboard_key != 0 {
            keyboard_key
        } else {
            button_id
        };
        Box::new(InputFromButton::new(
            id,
            key,
            turbo,
            toggle,
            inverted,
            Arc::clone(&self.input_engine),
        ))
    }

    fn create_hat_button_device(&self, params: &ParamPackage) -> Box<dyn InputDevice> {
        let id = identifier_from_params(params);
        let button_id = params.get_int("hat", 0);
        let direction = self
            .input_engine
            .lock()
            .get_hat_button_id(&params.get_str("direction", ""));
        let toggle = params.get_int("toggle", 0) != 0;
        let inverted = params.get_int("inverted", 0) != 0;
        let turbo = params.get_int("turbo", 0) != 0;
        {
            let mut e = self.input_engine.lock();
            e.pre_set_controller(&id);
            e.pre_set_hat_button(&id, button_id);
        }
        Box::new(InputFromHatButton::new(
            id,
            button_id,
            direction,
            turbo,
            toggle,
            inverted,
            Arc::clone(&self.input_engine),
        ))
    }

    fn create_stick_device(&self, params: &ParamPackage) -> Box<dyn InputDevice> {
        let dz = params.get_float("deadzone", 0.15).clamp(0.0, 1.0);
        let rng = params.get_float("range", 0.95).clamp(0.25, 1.50);
        let thr = params.get_float("threshold", 0.5).clamp(0.0, 1.0);
        let id = identifier_from_params(params);
        let ax = params.get_int("axis_x", 0);
        let px = AnalogProperties {
            deadzone: dz,
            range: rng,
            threshold: thr,
            offset: params.get_float("offset_x", 0.0).clamp(-1.0, 1.0),
            inverted: params.get_str("invert_x", "+") == "-",
            ..Default::default()
        };
        let ay = params.get_int("axis_y", 1);
        let py = AnalogProperties {
            deadzone: dz,
            range: rng,
            threshold: thr,
            offset: params.get_float("offset_y", 0.0).clamp(-1.0, 1.0),
            inverted: params.get_str("invert_y", "+") != "+",
            ..Default::default()
        };
        {
            let mut e = self.input_engine.lock();
            e.pre_set_controller(&id);
            e.pre_set_axis(&id, ax);
            e.pre_set_axis(&id, ay);
        }
        Box::new(InputFromStick::new(
            id,
            ax,
            ay,
            px,
            py,
            Arc::clone(&self.input_engine),
        ))
    }

    fn create_analog_device(&self, params: &ParamPackage) -> Box<dyn InputDevice> {
        let id = identifier_from_params(params);
        let axis = params.get_int("axis", 0);
        let props = AnalogProperties {
            deadzone: params.get_float("deadzone", 0.0).clamp(0.0, 1.0),
            range: params.get_float("range", 1.0).clamp(0.25, 1.50),
            threshold: params.get_float("threshold", 0.5).clamp(0.0, 1.0),
            offset: params.get_float("offset", 0.0).clamp(-1.0, 1.0),
            inverted: params.get_str("invert", "+") == "-",
            inverted_button: params.get_int("inverted", 0) != 0,
            toggle: params.get_int("toggle", 0) != 0,
        };
        {
            let mut e = self.input_engine.lock();
            e.pre_set_controller(&id);
            e.pre_set_axis(&id, axis);
        }
        Box::new(InputFromAnalog::new(
            id,
            axis,
            props,
            Arc::clone(&self.input_engine),
        ))
    }

    fn create_trigger_device(&self, params: &ParamPackage) -> Box<dyn InputDevice> {
        let id = identifier_from_params(params);
        let button = params.get_int("button", 0);
        let toggle = params.get_int("toggle", 0) != 0;
        let inverted = params.get_int("inverted", 0) != 0;
        let axis = params.get_int("axis", 0);
        let props = AnalogProperties {
            deadzone: params.get_float("deadzone", 0.0).clamp(0.0, 1.0),
            range: params.get_float("range", 1.0).clamp(0.25, 2.50),
            threshold: params.get_float("threshold", 0.5).clamp(0.0, 1.0),
            offset: params.get_float("offset", 0.0).clamp(-1.0, 1.0),
            inverted: params.get_int("invert", 0) != 0,
            ..Default::default()
        };
        {
            let mut e = self.input_engine.lock();
            e.pre_set_controller(&id);
            e.pre_set_axis(&id, axis);
            e.pre_set_button(&id, button);
        }
        Box::new(InputFromTrigger::new(
            id,
            button,
            toggle,
            inverted,
            axis,
            props,
            Arc::clone(&self.input_engine),
        ))
    }

    fn create_touch_device(&self, params: &ParamPackage) -> Box<dyn InputDevice> {
        let dz = params.get_float("deadzone", 0.0).clamp(0.0, 1.0);
        let rng = params.get_float("range", 1.0).clamp(0.25, 1.50);
        let thr = params.get_float("threshold", 0.5).clamp(0.0, 1.0);
        let id = identifier_from_params(params);
        let button = params.get_int("button", 0);
        let toggle = params.get_int("toggle", 0) != 0;
        let inverted = params.get_int("inverted", 0) != 0;
        let ax = params.get_int("axis_x", 0);
        let px = AnalogProperties {
            deadzone: dz,
            range: rng,
            threshold: thr,
            offset: params.get_float("offset_x", 0.0).clamp(-1.0, 1.0),
            inverted: params.get_str("invert_x", "+") == "-",
            ..Default::default()
        };
        let ay = params.get_int("axis_y", 1);
        let py = AnalogProperties {
            deadzone: dz,
            range: rng,
            threshold: thr,
            offset: params.get_float("offset_y", 0.0).clamp(-1.0, 1.0),
            inverted: params.get_int("invert_y", 0) != 0,
            ..Default::default()
        };
        {
            let mut e = self.input_engine.lock();
            e.pre_set_controller(&id);
            e.pre_set_axis(&id, ax);
            e.pre_set_axis(&id, ay);
            e.pre_set_button(&id, button);
        }
        Box::new(InputFromTouch::new(
            id,
            button,
            toggle,
            inverted,
            ax,
            ay,
            px,
            py,
            Arc::clone(&self.input_engine),
        ))
    }

    fn create_battery_device(&self, params: &ParamPackage) -> Box<dyn InputDevice> {
        let id = identifier_from_params(params);
        self.input_engine.lock().pre_set_controller(&id);
        Box::new(InputFromBattery::new(id, Arc::clone(&self.input_engine)))
    }

    fn create_color_device(&self, params: &ParamPackage) -> Box<dyn InputDevice> {
        let id = identifier_from_params(params);
        self.input_engine.lock().pre_set_controller(&id);
        Box::new(InputFromColor::new(id, Arc::clone(&self.input_engine)))
    }

    fn create_motion_device(&self, params: ParamPackage) -> Box<dyn InputDevice> {
        let id = identifier_from_params(&params);
        if params.has("motion") {
            let ms = params.get_int("motion", 0);
            let gt = params.get_float("threshold", 0.007);
            {
                let mut e = self.input_engine.lock();
                e.pre_set_controller(&id);
                e.pre_set_motion(&id, ms);
            }
            return Box::new(InputFromMotion::new(
                id,
                ms,
                gt,
                Arc::clone(&self.input_engine),
            ));
        }
        let dz = params.get_float("deadzone", 0.15).clamp(0.0, 1.0);
        let rng = params.get_float("range", 1.0).clamp(0.25, 1.50);
        let thr = params.get_float("threshold", 0.5).clamp(0.0, 1.0);
        let ax = params.get_int("axis_x", 0);
        let px = AnalogProperties {
            deadzone: dz,
            range: rng,
            threshold: thr,
            offset: params.get_float("offset_x", 0.0).clamp(-1.0, 1.0),
            inverted: params.get_str("invert_x", "+") == "-",
            ..Default::default()
        };
        let ay = params.get_int("axis_y", 1);
        let py = AnalogProperties {
            deadzone: dz,
            range: rng,
            threshold: thr,
            offset: params.get_float("offset_y", 0.0).clamp(-1.0, 1.0),
            inverted: params.get_str("invert_y", "+") != "+",
            ..Default::default()
        };
        let az = params.get_int("axis_z", 1);
        let pz = AnalogProperties {
            deadzone: dz,
            range: rng,
            threshold: thr,
            offset: params.get_float("offset_z", 0.0).clamp(-1.0, 1.0),
            inverted: params.get_str("invert_z", "+") != "+",
            ..Default::default()
        };
        {
            let mut e = self.input_engine.lock();
            e.pre_set_controller(&id);
            e.pre_set_axis(&id, ax);
            e.pre_set_axis(&id, ay);
            e.pre_set_axis(&id, az);
        }
        Box::new(InputFromAxisMotion::new(
            id,
            ax,
            ay,
            az,
            px,
            py,
            pz,
            Arc::clone(&self.input_engine),
        ))
    }

    fn create_camera_device(&self, params: &ParamPackage) -> Box<dyn InputDevice> {
        let id = identifier_from_params(params);
        self.input_engine.lock().pre_set_controller(&id);
        Box::new(InputFromCamera::new(id, Arc::clone(&self.input_engine)))
    }

    fn create_nfc_device(&self, params: &ParamPackage) -> Box<dyn InputDevice> {
        let id = identifier_from_params(params);
        self.input_engine.lock().pre_set_controller(&id);
        Box::new(InputFromNfc::new(id, Arc::clone(&self.input_engine)))
    }
}

impl common::input::InputDeviceFactory for InputFactory {
    fn create(&self, params: &ParamPackage) -> Box<dyn InputDevice> {
        InputFactory::create(self, params)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::input_engine::InputEngineMetadata;

    struct TestHatMetadata;

    #[test]
    fn motion_binding_preserves_threshold_samples_and_callback_lifetime() {
        use crate::input_engine::BasicMotion;
        for threshold in [None, Some(0.025)] {
            let engine = Arc::new(Mutex::new(InputEngine::new("test".to_string())));
            let factory = InputFactory::new(Arc::clone(&engine));
            let mut params = ParamPackage::default();
            params.set_int("motion", 2);
            if let Some(threshold) = threshold {
                params.set_float("threshold", threshold);
            }
            let identifier = identifier_from_params(&params);
            let mut device = factory.create(&params);
            let received = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::clone(&received);
            device.set_callback(InputCallback {
                on_change: Some(Arc::new(move |status| {
                    sink.lock().push(status.motion_status.clone());
                })),
            });
            let sample = BasicMotion {
                accel_x: 0.25, accel_y: -0.5, accel_z: -1.0,
                gyro_x: 0.125, gyro_y: -0.25, gyro_z: 0.5,
                delta_timestamp: 5_000,
            };
            // Continuous motion must notify even when two samples are equal.
            for _ in 0..2 {
                let pending = engine.lock().set_motion(&identifier, 2, &sample);
                pending.dispatch();
            }
            let values = received.lock();
            assert_eq!(values.len(), 2);
            for value in values.iter() {
                assert_eq!(value.delta_timestamp, 5_000);
                for (axis, expected) in [(&value.accel.x, 0.25), (&value.accel.y, -0.5),
                    (&value.accel.z, -1.0), (&value.gyro.x, 0.125),
                    (&value.gyro.y, -0.25), (&value.gyro.z, 0.5)] {
                    assert_eq!(axis.raw_value, expected);
                    assert_eq!(axis.properties.threshold, threshold.unwrap_or(0.007));
                    assert_eq!(axis.properties.deadzone, 0.0);
                    assert_eq!(axis.properties.range, 1.0);
                }
            }
            drop(values);
            drop(device);
            let pending = engine.lock().set_motion(&identifier, 2, &sample);
            pending.dispatch();
            assert_eq!(received.lock().len(), 2);
        }
    }

    impl InputEngineMetadata for TestHatMetadata {
        fn get_hat_button_id(&self, direction_name: &str) -> u8 {
            match direction_name {
                "left" => 0x08,
                _ => 0,
            }
        }
    }

    #[test]
    fn hat_device_uses_the_concrete_engine_direction_id() {
        let engine = Arc::new(Mutex::new(InputEngine::new("test".to_string())));
        engine
            .lock()
            .set_metadata_handler(Arc::new(TestHatMetadata));
        let factory = InputFactory::new(Arc::clone(&engine));

        let mut params = ParamPackage::default();
        params.set_int("hat", 0);
        params.set_str("direction", "left".to_string());
        let identifier = identifier_from_params(&params);
        let mut device = factory.create(&params);

        let pressed = Arc::new(AtomicBool::new(false));
        let callback_pressed = Arc::clone(&pressed);
        device.set_callback(InputCallback {
            on_change: Some(Arc::new(move |status| {
                callback_pressed.store(status.button_status.value, Ordering::Relaxed);
            })),
        });

        let pending = engine.lock().set_hat_button(&identifier, 0, 0x08);
        pending.dispatch();
        assert!(pressed.load(Ordering::Relaxed));
    }
}

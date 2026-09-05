// SPDX-FileCopyrightText: Copyright 2017 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `input_common/main.h` and `input_common/main.cpp`.
//!
//! Provides the InputSubsystem that manages all input device factories and drivers.

use std::collections::HashMap;
use std::sync::Arc;

use common::input::{
    register_input_factory, register_output_factory, unregister_input_factory,
    unregister_output_factory, ButtonNames,
};
use common::param_package::ParamPackage;
use common::uuid::UUID;

use crate::drivers::camera::Camera;
use crate::drivers::joycon::Joycons;
use crate::drivers::keyboard::Keyboard;
use crate::drivers::mouse::Mouse;
use crate::drivers::sdl_driver::SDLDriver;
use crate::drivers::tas_input;
use crate::drivers::touch_screen::TouchScreen;
use crate::drivers::udp_client::UdpClient;
use crate::drivers::virtual_amiibo::VirtualAmiibo;
use crate::drivers::virtual_gamepad::VirtualGamepad;
use crate::helpers::stick_from_buttons::StickFromButton;
use crate::helpers::touch_from_buttons::TouchFromButton;
use crate::input_engine::{InputEngine, MappingData, PadIdentifier};
use crate::input_mapping::MappingFactory;
use crate::input_poller::{InputFactory, OutputFactory};
use parking_lot::Mutex;

/// Port of `Polling` namespace from main.h
pub mod polling {
    /// Type of input desired for mapping purposes.
    /// Port of Polling::InputType enum from main.h
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum InputType {
        None,
        Button,
        Stick,
        Motion,
        Touch,
    }

    impl Default for InputType {
        fn default() -> Self {
            InputType::None
        }
    }
}

// Port of type aliases from main.h
// Using i32 as placeholder for Settings::NativeAnalog::Values etc.
pub type AnalogMapping = HashMap<i32, ParamPackage>;
pub type ButtonMapping = HashMap<i32, ParamPackage>;
pub type MotionMapping = HashMap<i32, ParamPackage>;

/// Dummy engine to get periodic updates.
/// Port of UpdateEngine from main.cpp
struct UpdateEngine {
    engine: Arc<Mutex<InputEngine>>,
    last_state: bool,
}

impl UpdateEngine {
    const IDENTIFIER: PadIdentifier = PadIdentifier {
        guid: UUID::new(), // UUID{} in C++ is a zero UUID
        port: 0,
        pad: 0,
    };

    fn new(input_engine: String) -> Self {
        let mut engine = InputEngine::new(input_engine);
        engine.pre_set_controller(&Self::IDENTIFIER);
        Self {
            engine: Arc::new(Mutex::new(engine)),
            last_state: false,
        }
    }

    fn engine(&self) -> Arc<Mutex<InputEngine>> {
        Arc::clone(&self.engine)
    }

    fn pump_events(&mut self) {
        let callbacks = self
            .engine
            .lock()
            .set_button(&Self::IDENTIFIER, 0, self.last_state);
        callbacks.dispatch();
        self.last_state = !self.last_state;
    }
}

/// Port of InputSubsystem::Impl from main.cpp
struct InputSubsystemImpl {
    /// Shared, because every engine's mapping callback writes into it from the
    /// driver's thread. Upstream hands each engine a lambda capturing the
    /// `Impl`, which owns the factory outright.
    mapping_factory: Option<Arc<Mutex<MappingFactory>>>,

    update_engine: Option<UpdateEngine>,
    keyboard: Option<Keyboard>,
    mouse: Option<Mouse>,
    touch_screen: Option<TouchScreen>,
    udp_client: Option<UdpClient>,
    tas_input: Option<Arc<Mutex<tas_input::Tas>>>,
    camera: Option<Camera>,
    virtual_amiibo: Option<VirtualAmiibo>,
    virtual_gamepad: Option<VirtualGamepad>,
    /// Upstream registers this under `HAVE_SDL3`; it backs every `engine:sdl`
    /// binding, i.e. all physical gamepads.
    sdl: Option<SDLDriver>,
    /// Upstream registers the dedicated Nintendo-controller engine immediately
    /// after SDL when SDL support is available.
    joycon: Option<Joycons>,
    // GCAdapter and Android remain unported.
}

impl InputSubsystemImpl {
    fn new() -> Self {
        Self {
            mapping_factory: None,
            update_engine: None,
            keyboard: None,
            mouse: None,
            touch_screen: None,
            udp_client: None,
            tas_input: None,
            camera: None,
            virtual_amiibo: None,
            virtual_gamepad: None,
            sdl: None,
            joycon: None,
        }
    }

    /// The callback upstream builds in `RegisterEngine`:
    /// `MappingCallback{[this](const MappingData& data) { RegisterInput(data); }}`.
    fn mapping_callback(
        mapping_factory: &Arc<Mutex<MappingFactory>>,
    ) -> crate::input_engine::MappingCallback {
        let mapping_factory = Arc::clone(mapping_factory);
        crate::input_engine::MappingCallback {
            on_data: Some(Box::new(move |data| {
                Self::register_input(&mapping_factory, data)
            })),
        }
    }

    /// Port of `InputSubsystem::Impl::RegisterEngine`.
    fn register_engine(
        engine: Arc<Mutex<InputEngine>>,
        mapping_factory: &Arc<Mutex<MappingFactory>>,
    ) {
        let name = {
            let mut engine = engine.lock();
            engine.set_mapping_callback(Self::mapping_callback(mapping_factory));
            engine.get_engine_name().to_string()
        };
        register_input_factory(&name, Arc::new(InputFactory::new(Arc::clone(&engine))));
        register_output_factory(&name, Arc::new(OutputFactory::new(engine)));
    }

    /// Port of Impl::Initialize
    fn initialize(&mut self) {
        self.mapping_factory = Some(Arc::new(Mutex::new(MappingFactory::new())));

        self.update_engine = Some(UpdateEngine::new("updater".to_string()));
        self.keyboard = Some(Keyboard::new("keyboard".to_string()));
        self.mouse = Some(Mouse::new("mouse".to_string()));
        self.touch_screen = Some(TouchScreen::new("touch".to_string()));
        self.udp_client = Some(UdpClient::new("cemuhookudp".to_string()));
        // Upstream `Impl::Initialize` calls `RegisterEngine` for every engine,
        // which registers both an input and an output factory under the
        // engine's name. Anything left out here can never resolve a binding:
        // a `engine:keyboard` button would simply never be found.
        //
        // Upstream's `RegisterEngine` also hands each engine a `MappingCallback`
        // that funnels into `RegisterInput`. Without it the mapping factory
        // never sees an event, and the Controls page can never capture a
        // binding.
        let mapping_factory = Arc::clone(self.mapping_factory.as_ref().unwrap());
        for engine in [
            self.update_engine.as_ref().unwrap().engine(),
            self.keyboard.as_ref().unwrap().engine(),
            self.mouse.as_ref().unwrap().engine(),
            self.touch_screen.as_ref().unwrap().engine(),
            self.udp_client.as_ref().unwrap().engine(),
        ] {
            Self::register_engine(engine, &mapping_factory);
        }
        let tas = Arc::new(Mutex::new(tas_input::Tas::new("tas".to_string())));
        Self::register_engine(tas.lock().engine(), &mapping_factory);
        self.tas_input = Some(tas);
        let camera = Camera::new("camera".to_string());
        Self::register_engine(camera.engine(), &mapping_factory);
        self.camera = Some(camera);
        let virtual_amiibo = VirtualAmiibo::new("virtual_amiibo".to_string());
        Self::register_engine(virtual_amiibo.engine(), &mapping_factory);
        self.virtual_amiibo = Some(virtual_amiibo);
        let virtual_gamepad = VirtualGamepad::new("virtual_gamepad".to_string());
        Self::register_engine(virtual_gamepad.engine(), &mapping_factory);
        self.virtual_gamepad = Some(virtual_gamepad);

        // Upstream: `RegisterEngine("sdl", sdl);` under HAVE_SDL3.
        let sdl = SDLDriver::new("sdl".to_string());
        let sdl_engine = sdl.engine();
        Self::register_engine(sdl_engine, &mapping_factory);
        self.sdl = Some(sdl);
        let joycon = Joycons::new("joycon".to_string());
        Self::register_engine(joycon.engine(), &mapping_factory);
        self.joycon = Some(joycon);

        register_input_factory("touch_from_button", Arc::new(TouchFromButton::new()));
        register_input_factory("analog_from_button", Arc::new(StickFromButton::new()));
    }

    /// Port of Impl::Shutdown
    fn shutdown(&mut self) {
        for name in [
            "updater",
            "keyboard",
            "mouse",
            "touch",
            "cemuhookudp",
            "tas",
            "camera",
            "virtual_amiibo",
            "virtual_gamepad",
            "sdl",
            "joycon",
        ] {
            unregister_input_factory(name);
            unregister_output_factory(name);
        }
        unregister_input_factory("touch_from_button");
        unregister_input_factory("analog_from_button");
        self.update_engine = None;
        self.keyboard = None;
        self.mouse = None;
        self.touch_screen = None;
        self.udp_client = None;
        self.tas_input = None;
        self.camera = None;
        self.virtual_amiibo = None;
        self.virtual_gamepad = None;
        self.sdl = None;
        self.joycon = None;
        self.mapping_factory = None;
    }

    /// Port of Impl::GetInputDevices
    fn get_input_devices(&self) -> Vec<ParamPackage> {
        let mut devices = vec![{
            let mut p = ParamPackage::default();
            p.set_str("display", "Any".to_string());
            p.set_str("engine", "any".to_string());
            p
        }];

        if let Some(ref keyboard) = self.keyboard {
            devices.extend(keyboard.get_input_devices());
        }
        if let Some(ref mouse) = self.mouse {
            devices.extend(mouse.get_input_devices());
        }
        if let Some(ref udp_client) = self.udp_client {
            devices.extend(udp_client.get_input_devices());
        }
        if let Some(ref joycon) = self.joycon {
            devices.extend(joycon.get_input_devices());
        }
        if let Some(ref sdl) = self.sdl {
            devices.extend(sdl.get_input_devices());
        }

        devices
    }

    /// Port of Impl::BeginConfiguration.
    fn begin_configuration(&mut self) {
        if let Some(ref keyboard) = self.keyboard {
            keyboard.engine().lock().begin_configuration();
        }
        if let Some(ref mouse) = self.mouse {
            mouse.engine().lock().begin_configuration();
        }
        if let Some(ref udp_client) = self.udp_client {
            udp_client.engine().lock().begin_configuration();
        }
        if let Some(ref sdl) = self.sdl {
            sdl.engine().lock().begin_configuration();
        }
        if let Some(ref joycon) = self.joycon {
            joycon.engine().lock().begin_configuration();
        }
    }

    /// Port of Impl::EndConfiguration.
    fn end_configuration(&mut self) {
        if let Some(ref keyboard) = self.keyboard {
            keyboard.engine().lock().end_configuration();
        }
        if let Some(ref mouse) = self.mouse {
            mouse.engine().lock().end_configuration();
        }
        if let Some(ref udp_client) = self.udp_client {
            udp_client.engine().lock().end_configuration();
        }
        if let Some(ref sdl) = self.sdl {
            sdl.engine().lock().end_configuration();
        }
        if let Some(ref joycon) = self.joycon {
            joycon.engine().lock().end_configuration();
        }
    }

    /// Port of Impl::PumpEvents
    fn pump_events(&mut self) {
        if let Some(ref mut update_engine) = self.update_engine {
            update_engine.pump_events();
        }
        if let Some(ref sdl) = self.sdl {
            sdl.pump_events();
        }
    }

    /// Port of Impl::RegisterInput
    fn register_input(mapping_factory: &Arc<Mutex<MappingFactory>>, data: &MappingData) {
        mapping_factory.lock().register_input(data);
    }

    /// Get the analog mapping for a device by finding the matching engine.
    /// Upstream: `Impl::GetAnalogMappingForDevice` (main.cpp:200-209).
    fn get_analog_mapping_for_device(&self, params: &ParamPackage) -> AnalogMapping {
        let engine = params.get_str("engine", "");
        if engine.is_empty() || engine == "any" {
            return HashMap::new();
        }
        if self
            .mouse
            .as_ref()
            .map_or(false, |m| m.engine().lock().get_engine_name() == engine)
        {
            return self
                .mouse
                .as_ref()
                .unwrap()
                .get_analog_mapping_for_device(params);
        }
        if let Some(ref udp_client) = self.udp_client {
            if udp_client.engine().lock().get_engine_name() == engine {
                return udp_client.get_analog_mapping_for_device(params);
            }
        }
        if let Some(ref sdl) = self.sdl {
            if sdl.engine().lock().get_engine_name() == engine {
                return sdl.get_analog_mapping_for_device(params);
            }
        }
        if let Some(ref joycon) = self.joycon {
            if joycon.engine().lock().get_engine_name() == engine {
                return joycon.get_analog_mapping_for_device(params);
            }
        }
        // Keyboard, touch_screen, tas_input, camera, virtual_amiibo, virtual_gamepad
        // don't have analog mappings — they use the default (empty).
        HashMap::new()
    }

    /// Get the button mapping for a device by finding the matching engine.
    /// Upstream: `Impl::GetButtonMappingForDevice` (main.cpp:211-220).
    fn get_button_mapping_for_device(&self, params: &ParamPackage) -> ButtonMapping {
        let engine = params.get_str("engine", "");
        if engine.is_empty() || engine == "any" {
            return HashMap::new();
        }
        if let Some(ref udp_client) = self.udp_client {
            if udp_client.engine().lock().get_engine_name() == engine {
                return udp_client.get_button_mapping_for_device(params);
            }
        }
        if let Some(ref sdl) = self.sdl {
            if sdl.engine().lock().get_engine_name() == engine {
                return sdl.get_button_mapping_for_device(params);
            }
        }
        if let Some(ref joycon) = self.joycon {
            if joycon.engine().lock().get_engine_name() == engine {
                return joycon.get_button_mapping_for_device(params);
            }
        }
        // The remaining engines provide no custom button mappings.
        HashMap::new()
    }

    /// Get the motion mapping for a device by finding the matching engine.
    /// Upstream: `Impl::GetMotionMappingForDevice` (main.cpp:222-231).
    fn get_motion_mapping_for_device(&self, params: &ParamPackage) -> MotionMapping {
        let engine = params.get_str("engine", "");
        if engine.is_empty() || engine == "any" {
            return HashMap::new();
        }
        if let Some(ref udp_client) = self.udp_client {
            if udp_client.engine().lock().get_engine_name() == engine {
                return udp_client.get_motion_mapping_for_device(params);
            }
        }
        if let Some(ref sdl) = self.sdl {
            if sdl.engine().lock().get_engine_name() == engine {
                return sdl.get_motion_mapping_for_device(params);
            }
        }
        if let Some(ref joycon) = self.joycon {
            if joycon.engine().lock().get_engine_name() == engine {
                return joycon.get_motion_mapping_for_device(params);
            }
        }
        HashMap::new()
    }

    /// Get the UI button name for a device.
    /// Upstream: `Impl::GetButtonName` (main.cpp:233-244).
    fn get_button_name(&self, params: &ParamPackage) -> ButtonNames {
        let engine = params.get_str("engine", "");
        if engine.is_empty() || engine == "any" {
            return ButtonNames::Undefined;
        }
        if self
            .mouse
            .as_ref()
            .map_or(false, |m| m.engine().lock().get_engine_name() == engine)
        {
            return self.mouse.as_ref().unwrap().get_ui_name(params);
        }
        if let Some(ref udp_client) = self.udp_client {
            if udp_client.engine().lock().get_engine_name() == engine {
                return udp_client.get_ui_name(params);
            }
        }
        if let Some(ref sdl) = self.sdl {
            if sdl.engine().lock().get_engine_name() == engine {
                return sdl.get_ui_name(params);
            }
        }
        if let Some(ref joycon) = self.joycon {
            if joycon.engine().lock().get_engine_name() == engine {
                return joycon.get_ui_name(params);
            }
        }
        ButtonNames::Invalid
    }

    /// Check if stick axes are inverted.
    /// Upstream: `Impl::IsStickInverted` (main.cpp:246-254).
    fn is_stick_inverted(&self, params: &ParamPackage) -> bool {
        let engine = params.get_str("engine", "");
        if engine.is_empty() || engine == "any" {
            return false;
        }
        if let Some(ref udp_client) = self.udp_client {
            if udp_client.engine().lock().get_engine_name() == engine {
                return udp_client.is_stick_inverted(params);
            }
        }
        if let Some(ref sdl) = self.sdl {
            if sdl.engine().lock().get_engine_name() == engine {
                return sdl.is_stick_inverted(params);
            }
        }
        false
    }
}

/// Port of `InputSubsystem` class from main.h / main.cpp
pub struct InputSubsystem {
    imp: InputSubsystemImpl,
}

impl InputSubsystem {
    /// Port of InputSubsystem::InputSubsystem
    pub fn new() -> Self {
        Self {
            imp: InputSubsystemImpl::new(),
        }
    }

    /// Initializes and registers all built-in input device factories.
    /// Port of InputSubsystem::Initialize
    pub fn initialize(&mut self) {
        self.imp.initialize();
    }

    /// Unregisters all built-in input device factories and shuts them down.
    /// Port of InputSubsystem::Shutdown
    pub fn shutdown(&mut self) {
        self.imp.shutdown();
    }

    /// Retrieves the underlying keyboard device.
    /// Port of InputSubsystem::GetKeyboard
    pub fn get_keyboard(&self) -> Option<&Keyboard> {
        self.imp.keyboard.as_ref()
    }

    /// Retrieves the underlying keyboard device (mutable).
    pub fn get_keyboard_mut(&mut self) -> Option<&mut Keyboard> {
        self.imp.keyboard.as_mut()
    }

    /// Retrieves the underlying mouse device.
    /// Port of InputSubsystem::GetMouse
    pub fn get_mouse(&self) -> Option<&Mouse> {
        self.imp.mouse.as_ref()
    }

    /// Retrieves the underlying mouse device (mutable).
    pub fn get_mouse_mut(&mut self) -> Option<&mut Mouse> {
        self.imp.mouse.as_mut()
    }

    /// Retrieves the underlying touch screen device.
    /// Port of InputSubsystem::GetTouchScreen
    pub fn get_touch_screen(&self) -> Option<&TouchScreen> {
        self.imp.touch_screen.as_ref()
    }

    /// Retrieves the underlying touch screen device (mutable).
    pub fn get_touch_screen_mut(&mut self) -> Option<&mut TouchScreen> {
        self.imp.touch_screen.as_mut()
    }

    /// Retrieves the underlying TAS input device.
    /// Port of InputSubsystem::GetTas
    pub fn get_tas(&self) -> Option<Arc<Mutex<tas_input::Tas>>> {
        self.imp.tas_input.as_ref().map(Arc::clone)
    }

    /// Retrieves the underlying TAS input device (mutable).
    pub fn get_tas_mut(&mut self) -> Option<Arc<Mutex<tas_input::Tas>>> {
        self.imp.tas_input.as_ref().map(Arc::clone)
    }

    /// Retrieves the underlying camera input device.
    /// Port of InputSubsystem::GetCamera
    pub fn get_camera(&self) -> Option<&Camera> {
        self.imp.camera.as_ref()
    }

    /// Retrieves the underlying camera input device (mutable).
    pub fn get_camera_mut(&mut self) -> Option<&mut Camera> {
        self.imp.camera.as_mut()
    }

    /// Retrieves the underlying virtual amiibo input device.
    /// Port of InputSubsystem::GetVirtualAmiibo
    pub fn get_virtual_amiibo(&self) -> Option<&VirtualAmiibo> {
        self.imp.virtual_amiibo.as_ref()
    }

    /// Retrieves the underlying virtual amiibo input device (mutable).
    pub fn get_virtual_amiibo_mut(&mut self) -> Option<&mut VirtualAmiibo> {
        self.imp.virtual_amiibo.as_mut()
    }

    /// Retrieves the underlying virtual gamepad input device.
    /// Port of InputSubsystem::GetVirtualGamepad
    pub fn get_virtual_gamepad(&self) -> Option<&VirtualGamepad> {
        self.imp.virtual_gamepad.as_ref()
    }

    /// Retrieves the underlying virtual gamepad input device (mutable).
    pub fn get_virtual_gamepad_mut(&mut self) -> Option<&mut VirtualGamepad> {
        self.imp.virtual_gamepad.as_mut()
    }

    /// Returns all available input devices.
    /// Port of InputSubsystem::GetInputDevices
    pub fn get_input_devices(&self) -> Vec<ParamPackage> {
        self.imp.get_input_devices()
    }

    /// Retrieves the analog mappings for the given device.
    /// Port of InputSubsystem::GetAnalogMappingForDevice
    pub fn get_analog_mapping_for_device(&self, device: &ParamPackage) -> AnalogMapping {
        self.imp.get_analog_mapping_for_device(device)
    }

    /// Retrieves the button mappings for the given device.
    /// Port of InputSubsystem::GetButtonMappingForDevice
    pub fn get_button_mapping_for_device(&self, device: &ParamPackage) -> ButtonMapping {
        self.imp.get_button_mapping_for_device(device)
    }

    /// Retrieves the motion mappings for the given device.
    /// Port of InputSubsystem::GetMotionMappingForDevice
    pub fn get_motion_mapping_for_device(&self, device: &ParamPackage) -> MotionMapping {
        self.imp.get_motion_mapping_for_device(device)
    }

    /// Returns an enum containing the name to be displayed from the input engine.
    /// Port of InputSubsystem::GetButtonName
    pub fn get_button_name(&self, params: &ParamPackage) -> ButtonNames {
        self.imp.get_button_name(params)
    }

    /// Returns true if device is a controller.
    /// Port of InputSubsystem::IsController
    pub fn is_controller(&self, params: &ParamPackage) -> bool {
        let engine_name = params.get_str("engine", "");
        matches!(
            engine_name.as_str(),
            "mouse"
                | "gcpad"
                | "cemuhookudp"
                | "tas"
                | "virtual_gamepad"
                | "sdl"
                | "joycon"
                | "android"
        )
    }

    /// Returns true if axis of a stick aren't mapped in the correct direction.
    /// Port of InputSubsystem::IsStickInverted
    pub fn is_stick_inverted(&self, device: &ParamPackage) -> bool {
        if device.has("axis_x") && device.has("axis_y") {
            return self.imp.is_stick_inverted(device);
        }
        false
    }

    /// Reloads the input devices.
    /// Port of InputSubsystem::ReloadInputDevices
    /// Upstream: calls `udp_client->ReloadSockets()`.
    pub fn reload_input_devices(&mut self) {
        if let Some(ref mut udp_client) = self.imp.udp_client {
            udp_client.reload_sockets();
        }
    }

    /// Start polling from all backends for a desired input type.
    /// Port of InputSubsystem::BeginMapping
    pub fn begin_mapping(&mut self, input_type: polling::InputType) {
        self.imp.begin_configuration();
        if let Some(ref mapping_factory) = self.imp.mapping_factory {
            mapping_factory.lock().begin_mapping(input_type);
        }
    }

    /// Returns an input event with mapping information.
    /// Port of InputSubsystem::GetNextInput
    pub fn get_next_input(&mut self) -> ParamPackage {
        if let Some(ref mapping_factory) = self.imp.mapping_factory {
            mapping_factory.lock().get_next_input()
        } else {
            ParamPackage::default()
        }
    }

    /// Stop polling from all backends.
    /// Port of InputSubsystem::StopMapping
    pub fn stop_mapping(&mut self) {
        self.imp.end_configuration();
        if let Some(ref mapping_factory) = self.imp.mapping_factory {
            mapping_factory.lock().stop_mapping();
        }
    }

    /// Signals SDL driver for new input events.
    /// Port of InputSubsystem::PumpEvents
    pub fn pump_events(&mut self) {
        self.imp.pump_events();
    }
}

impl Default for InputSubsystem {
    fn default() -> Self {
        Self::new()
    }
}

/// Generates a serialized param package for creating a keyboard button device.
/// Port of GenerateKeyboardParam from main.cpp
pub fn generate_keyboard_param(key_code: i32) -> String {
    let mut param = ParamPackage::default();
    param.set_str("engine", "keyboard".to_string());
    param.set_int("code", key_code);
    param.set_int("toggle", 0);
    param.serialize()
}

/// Generates a serialized param package for creating an analog device taking input from keyboard.
/// Port of GenerateAnalogParamFromKeys from main.cpp
pub fn generate_analog_param_from_keys(
    key_up: i32,
    key_down: i32,
    key_left: i32,
    key_right: i32,
    key_modifier: i32,
    modifier_scale: f32,
) -> String {
    let mut circle_pad_param = ParamPackage::default();
    circle_pad_param.set_str("engine", "analog_from_button".to_string());
    circle_pad_param.set_str("up", generate_keyboard_param(key_up));
    circle_pad_param.set_str("down", generate_keyboard_param(key_down));
    circle_pad_param.set_str("left", generate_keyboard_param(key_left));
    circle_pad_param.set_str("right", generate_keyboard_param(key_right));
    circle_pad_param.set_str("modifier", generate_keyboard_param(key_modifier));
    circle_pad_param.set_str("modifier_scale", modifier_scale.to_string());
    circle_pad_param.serialize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc as StdArc, Mutex as StdMutex};

    use common::input::{CallbackStatus, InputCallback, InputType};

    #[test]
    fn keyboard_param_serializes_false_through_integer_overload() {
        let param = ParamPackage::from_serialized(&generate_keyboard_param(42));

        assert_eq!(param.get_str("engine", ""), "keyboard");
        assert_eq!(param.get_int("code", -1), 42);
        assert_eq!(param.get_str("toggle", ""), "0");
    }

    #[test]
    fn configuration_excludes_updater_but_includes_keyboard() {
        let mapping_factory = Arc::new(Mutex::new(MappingFactory::new()));
        let update_engine = UpdateEngine::new("updater".to_string());
        update_engine
            .engine()
            .lock()
            .set_mapping_callback(InputSubsystemImpl::mapping_callback(&mapping_factory));
        let keyboard = Keyboard::new("keyboard".to_string());
        keyboard
            .engine()
            .lock()
            .set_mapping_callback(InputSubsystemImpl::mapping_callback(&mapping_factory));

        let mut imp = InputSubsystemImpl::new();
        imp.mapping_factory = Some(Arc::clone(&mapping_factory));
        imp.update_engine = Some(update_engine);
        imp.keyboard = Some(keyboard);
        imp.begin_configuration();
        mapping_factory
            .lock()
            .begin_mapping(polling::InputType::Button);

        imp.pump_events();
        assert_eq!(
            mapping_factory
                .lock()
                .get_next_input()
                .get_str("engine", ""),
            ""
        );

        imp.keyboard.as_mut().unwrap().press_key(42);
        let input = mapping_factory.lock().get_next_input();
        assert_eq!(input.get_str("engine", ""), "keyboard");
        assert_eq!(input.get_int("code", -1), 42);

        imp.end_configuration();
        mapping_factory.lock().stop_mapping();
    }

    fn capture_status(
        device: &mut dyn common::input::InputDevice,
    ) -> StdArc<StdMutex<CallbackStatus>> {
        let status = StdArc::new(StdMutex::new(CallbackStatus::default()));
        let output = StdArc::clone(&status);
        device.set_callback(InputCallback {
            on_change: Some(StdArc::new(move |value| {
                *output.lock().unwrap() = value.clone();
            })),
        });
        status
    }

    #[test]
    fn initialize_registers_updater_composite_and_udp_factories() {
        let mut subsystem = InputSubsystem::new();
        subsystem.initialize();

        let analog = generate_analog_param_from_keys(11, 12, 13, 14, 15, 0.5);
        let mut analog = common::input::create_input_device_from_string(&analog);
        let analog_status = capture_status(analog.as_mut());
        subsystem.get_keyboard().unwrap().press_key(11);
        let analog_status = analog_status.lock().unwrap().clone();
        assert_eq!(analog_status.input_type, InputType::Stick);
        assert!(analog_status.stick_status.y.raw_value > 0.99);

        let mut touch_params = ParamPackage::default();
        touch_params.set_str("engine", "touch_from_button".to_string());
        touch_params.set_str("button", generate_keyboard_param(21));
        touch_params.set_float("x", 640.0);
        touch_params.set_float("y", 360.0);
        let mut touch = common::input::create_input_device(&touch_params);
        let touch_status = capture_status(touch.as_mut());
        subsystem.get_keyboard().unwrap().press_key(21);
        let touch_status = touch_status.lock().unwrap().clone();
        assert_eq!(touch_status.input_type, InputType::Touch);
        assert!(touch_status.touch_status.pressed.value);
        assert_eq!(touch_status.touch_status.x.raw_value, 0.5);
        assert_eq!(touch_status.touch_status.y.raw_value, 0.5);

        let mut udp_params = ParamPackage::default();
        udp_params.set_str("engine", "cemuhookudp".to_string());
        udp_params.set_str("guid", UUID::default().raw_string());
        udp_params.set_int("port", 26760);
        udp_params.set_int("pad", 0);
        udp_params.set_int("motion", 0);
        let mut udp = common::input::create_input_device(&udp_params);
        let udp_status = capture_status(udp.as_mut());
        udp.force_update();
        assert_eq!(udp_status.lock().unwrap().input_type, InputType::Motion);

        // `EmulatedController::SetDefaultOutputParams` always installs
        // `engine:joycon` bindings. Upstream registers that engine alongside
        // SDL, even when no physical Joy-Con is connected.
        let mut joycon_params = ParamPackage::default();
        joycon_params.set_str("engine", "joycon".to_string());
        joycon_params.set_str("guid", UUID::default().raw_string());
        joycon_params.set_int("port", 0);
        joycon_params.set_int("pad", 0);
        joycon_params.set_int("axis_x", 100);
        joycon_params.set_int("axis_y", 101);
        let mut joycon = common::input::create_input_device(&joycon_params);
        let joycon_status = capture_status(joycon.as_mut());
        joycon.force_update();
        assert_eq!(joycon_status.lock().unwrap().input_type, InputType::Stick);

        let mut amiibo_params = ParamPackage::default();
        amiibo_params.set_str("engine", "virtual_amiibo".to_string());
        amiibo_params.set_int("nfc", 0);
        let mut amiibo_input = common::input::create_input_device(&amiibo_params);
        let amiibo_status = capture_status(amiibo_input.as_mut());
        let mut amiibo = common::input::create_output_device(&amiibo_params);
        assert_eq!(amiibo.supports_nfc(), common::input::NfcState::Success);
        assert_eq!(
            amiibo.set_polling_mode(common::input::PollingMode::NFC),
            common::input::DriverResult::Success
        );
        assert_eq!(amiibo.start_nfc_polling(), common::input::NfcState::Success);
        assert_eq!(
            subsystem.get_virtual_amiibo().unwrap().get_current_state(),
            crate::drivers::virtual_amiibo::State::WaitingForAmiibo
        );

        let mut tag = vec![0u8; 0x21c];
        assert_eq!(
            subsystem
                .get_virtual_amiibo_mut()
                .unwrap()
                .load_amiibo_from_data(&mut tag),
            crate::drivers::virtual_amiibo::Info::Success
        );
        assert_eq!(
            amiibo_status.lock().unwrap().nfc_status.state,
            common::input::NfcState::NewAmiibo
        );
        assert_eq!(amiibo.stop_nfc_polling(), common::input::NfcState::Success);
        assert_eq!(
            amiibo_status.lock().unwrap().nfc_status.state,
            common::input::NfcState::AmiiboRemoved
        );

        let mut camera_params = ParamPackage::default();
        camera_params.set_str("engine", "camera".to_string());
        camera_params.set_str("guid", UUID::default().raw_string());
        camera_params.set_int("port", 0);
        camera_params.set_int("pad", 0);
        camera_params.set_int("camera", 0);
        let mut camera_input = common::input::create_input_device(&camera_params);
        let camera_status = capture_status(camera_input.as_mut());
        let mut camera_output = common::input::create_output_device(&camera_params);
        assert_eq!(
            camera_output.set_camera_format(common::input::CameraFormat::Size20x15),
            common::input::DriverResult::Success
        );
        assert_eq!(subsystem.get_camera().unwrap().get_image_width(), 20);
        subsystem
            .get_camera_mut()
            .unwrap()
            .set_camera_data(2, 2, &[0x11, 0x22, 0x33, 0x44]);
        let camera_status = camera_status.lock().unwrap().clone();
        assert_eq!(camera_status.input_type, InputType::IrSensor);
        assert_eq!(
            camera_status.camera_status,
            common::input::CameraFormat::Size20x15
        );
        assert_eq!(camera_status.raw_data.len(), 20 * 15);
        assert_eq!(camera_status.raw_data[0], 0x11);
        assert_eq!(camera_status.raw_data[19], 0x22);
        assert_eq!(camera_status.raw_data[14 * 20], 0x33);

        let mut gamepad_params = ParamPackage::default();
        gamepad_params.set_str("engine", "virtual_gamepad".to_string());
        gamepad_params.set_str("guid", UUID::default().raw_string());
        gamepad_params.set_int("port", 0);
        gamepad_params.set_int("pad", 0);
        gamepad_params.set_int("button", 0);
        let mut gamepad_input = common::input::create_input_device(&gamepad_params);
        let gamepad_status = capture_status(gamepad_input.as_mut());
        subsystem
            .get_virtual_gamepad_mut()
            .unwrap()
            .set_button_state(
                0,
                crate::drivers::virtual_gamepad::VirtualButton::ButtonA,
                true,
            );
        let gamepad_status = gamepad_status.lock().unwrap().clone();
        assert_eq!(gamepad_status.input_type, InputType::Button);
        assert!(gamepad_status.button_status.value);

        drop(amiibo_input);
        drop(amiibo);
        drop(camera_input);
        drop(camera_output);
        drop(gamepad_input);
        drop(joycon);
        drop(udp);
        drop(touch);
        drop(analog);
        subsystem.shutdown();
    }

    #[test]
    fn release_all_keys_clears_pressed_controller_bindings() {
        let mut keyboard = Keyboard::new("focus_test_keyboard".to_string());
        let engine = keyboard.engine();
        let identifier = PadIdentifier::default();
        engine.lock().pre_set_button(&identifier, 42);

        keyboard.press_key(42);
        assert!(engine.lock().get_button(&identifier, 42));

        keyboard.release_all_keys();
        assert!(!engine.lock().get_button(&identifier, 42));
    }
}

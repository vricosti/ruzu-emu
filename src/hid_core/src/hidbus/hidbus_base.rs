// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of hid_core/hidbus/hidbus_base.h and hidbus_base.cpp

use common::ResultCode;
use std::sync::Arc;

/// Boundary for HidbusBase's kernel event and System::ApplicationMemory.
/// The core crate supplies an owner retaining the event until the device is
/// dropped. hid_core cannot depend on core (core already depends on hid_core).
pub trait HidbusRuntime: Send + Sync {
    fn signal_send_command_async_event(&self);
    fn write_memory(&self, address: u64, data: &[u8]);
}

/// This is nn::hidbus::JoyPollingMode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u32)]
pub enum JoyPollingMode {
    #[default]
    SixAxisSensorDisable = 0,
    SixAxisSensorEnable = 1,
    ButtonOnly = 2,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct DataAccessorHeader {
    pub result: ResultCode,
    pub _padding: u32,
    pub unused: [u8; 0x18],
    pub latest_entry: u64,
    pub total_entries: u64,
}
const _: () = assert!(std::mem::size_of::<DataAccessorHeader>() == 0x30);

impl Default for DataAccessorHeader {
    fn default() -> Self {
        Self {
            result: ResultCode(u32::MAX),
            _padding: 0,
            unused: [0; 0x18],
            latest_entry: 0,
            total_entries: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct JoyDisableSixAxisPollingData {
    pub data: [u8; 0x26],
    pub out_size: u8,
    pub _padding: u8,
    pub sampling_number: u64,
}
const _: () = assert!(std::mem::size_of::<JoyDisableSixAxisPollingData>() == 0x30);

impl Default for JoyDisableSixAxisPollingData {
    fn default() -> Self {
        // SAFETY: All fields are plain data types, zero is valid.
        unsafe { std::mem::zeroed() }
    }
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct JoyEnableSixAxisPollingData {
    pub data: [u8; 0x8],
    pub out_size: u8,
    pub _padding: [u8; 0x7],
    pub sampling_number: u64,
}
const _: () = assert!(std::mem::size_of::<JoyEnableSixAxisPollingData>() == 0x18);

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct JoyButtonOnlyPollingData {
    pub data: [u8; 0x2c],
    pub out_size: u8,
    pub _padding: [u8; 0x3],
    pub sampling_number: u64,
}
const _: () = assert!(std::mem::size_of::<JoyButtonOnlyPollingData>() == 0x38);

impl Default for JoyButtonOnlyPollingData {
    fn default() -> Self {
        // SAFETY: All fields are plain data types, zero is valid.
        unsafe { std::mem::zeroed() }
    }
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct JoyDisableSixAxisPollingEntry {
    pub sampling_number: u64,
    pub polling_data: JoyDisableSixAxisPollingData,
}
const _: () = assert!(std::mem::size_of::<JoyDisableSixAxisPollingEntry>() == 0x38);

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct JoyEnableSixAxisPollingEntry {
    pub sampling_number: u64,
    pub polling_data: JoyEnableSixAxisPollingData,
}
const _: () = assert!(std::mem::size_of::<JoyEnableSixAxisPollingEntry>() == 0x20);

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct JoyButtonOnlyPollingEntry {
    pub sampling_number: u64,
    pub polling_data: JoyButtonOnlyPollingData,
}
const _: () = assert!(std::mem::size_of::<JoyButtonOnlyPollingEntry>() == 0x40);

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct JoyDisableSixAxisDataAccessor {
    pub header: DataAccessorHeader,
    pub entries: [JoyDisableSixAxisPollingEntry; 0xB],
}
const _: () = assert!(std::mem::size_of::<JoyDisableSixAxisDataAccessor>() == 0x298);

impl Default for JoyDisableSixAxisDataAccessor {
    fn default() -> Self {
        Self {
            header: DataAccessorHeader::default(),
            entries: [JoyDisableSixAxisPollingEntry::default(); 0xB],
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct JoyEnableSixAxisDataAccessor {
    pub header: DataAccessorHeader,
    pub entries: [JoyEnableSixAxisPollingEntry; 0xB],
}
const _: () = assert!(std::mem::size_of::<JoyEnableSixAxisDataAccessor>() == 0x190);

impl Default for JoyEnableSixAxisDataAccessor {
    fn default() -> Self {
        Self {
            header: DataAccessorHeader::default(),
            entries: [JoyEnableSixAxisPollingEntry::default(); 0xB],
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ButtonOnlyPollingDataAccessor {
    pub header: DataAccessorHeader,
    pub entries: [JoyButtonOnlyPollingEntry; 0xB],
}
const _: () = assert!(std::mem::size_of::<ButtonOnlyPollingDataAccessor>() == 0x2F0);

impl Default for ButtonOnlyPollingDataAccessor {
    fn default() -> Self {
        Self {
            header: DataAccessorHeader::default(),
            entries: [JoyButtonOnlyPollingEntry::default(); 0xB],
        }
    }
}

/// Base trait for hidbus devices
pub trait HidbusDevice {
    fn base(&self) -> &HidbusBase;
    fn base_mut(&mut self) -> &mut HidbusBase;

    /// HidbusBase::ActivateDevice, including virtual OnInit dispatch.
    fn activate_device(&mut self) {
        if self.base().is_activated { return; }
        self.base_mut().is_activated = true;
        self.on_init();
    }
    /// HidbusBase::DeactivateDevice calls OnRelease before clearing activation.
    fn deactivate_device(&mut self) {
        if self.base().is_activated { self.on_release(); }
        self.base_mut().is_activated = false;
    }
    fn is_device_activated(&self) -> bool { self.base().is_device_activated() }
    fn enable(&mut self, enable: bool) { self.base_mut().enable(enable); }
    fn is_enabled(&self) -> bool { self.base().is_enabled() }
    fn is_polling_mode(&self) -> bool { self.base().is_polling_mode() }
    fn get_polling_mode(&self) -> JoyPollingMode { self.base().get_polling_mode() }
    fn set_polling_mode(&mut self, mode: JoyPollingMode) { self.base_mut().set_polling_mode(mode); }
    fn disable_polling_mode(&mut self) { self.base_mut().disable_polling_mode(); }
    fn set_transfer_memory_address(&mut self, address: u64) { self.base_mut().set_transfer_memory_address(address); }

    fn on_init(&mut self) {}
    fn on_release(&mut self) {}
    fn on_update(&mut self) {}
    fn get_device_id(&self) -> u8 {
        0
    }
    fn set_command(&mut self, _data: &[u8]) -> bool {
        false
    }
    fn get_reply(&self, _out_data: &mut [u8]) -> u64 {
        0
    }
}

/// Base implementation for hidbus devices
pub struct HidbusBase {
    pub runtime: Arc<dyn HidbusRuntime>,
    pub is_activated: bool,
    pub device_enabled: bool,
    pub polling_mode_enabled: bool,
    pub polling_mode: JoyPollingMode,
    pub disable_sixaxis_data: JoyDisableSixAxisDataAccessor,
    pub enable_sixaxis_data: JoyEnableSixAxisDataAccessor,
    pub button_only_data: ButtonOnlyPollingDataAccessor,
    pub transfer_memory: u64,
}

impl HidbusBase {
    pub fn new(runtime: Arc<dyn HidbusRuntime>) -> Self {
        Self {
            runtime,
            is_activated: false,
            device_enabled: false,
            polling_mode_enabled: false,
            polling_mode: JoyPollingMode::default(),
            disable_sixaxis_data: JoyDisableSixAxisDataAccessor::default(),
            enable_sixaxis_data: JoyEnableSixAxisDataAccessor::default(),
            button_only_data: ButtonOnlyPollingDataAccessor::default(),
            transfer_memory: 0,
        }
    }

    pub fn is_device_activated(&self) -> bool {
        self.is_activated
    }

    pub fn enable(&mut self, enable: bool) {
        self.device_enabled = enable;
    }

    pub fn is_enabled(&self) -> bool {
        self.device_enabled
    }

    pub fn is_polling_mode(&self) -> bool {
        self.polling_mode_enabled
    }

    pub fn get_polling_mode(&self) -> JoyPollingMode {
        self.polling_mode
    }

    pub fn set_polling_mode(&mut self, mode: JoyPollingMode) {
        self.polling_mode = mode;
        self.polling_mode_enabled = true;
    }

    pub fn disable_polling_mode(&mut self) {
        self.polling_mode_enabled = false;
    }

    pub fn set_transfer_memory_address(&mut self, t_mem: u64) {
        self.transfer_memory = t_mem;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[derive(Default)]
    pub(crate) struct TestRuntime {
        pub signals: std::sync::atomic::AtomicUsize,
        pub writes: std::sync::Mutex<Vec<(u64, Vec<u8>)>>,
    }
    impl HidbusRuntime for TestRuntime {
        fn signal_send_command_async_event(&self) {
            self.signals.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        fn write_memory(&self, address: u64, data: &[u8]) {
            self.writes.lock().unwrap().push((address, data.to_vec()));
        }
    }

    #[test]
    fn activation_dispatches_hooks_once_with_upstream_state_order() {
        struct Device { base: HidbusBase, calls: Vec<&'static str> }
        impl HidbusDevice for Device {
            fn base(&self) -> &HidbusBase { &self.base }
            fn base_mut(&mut self) -> &mut HidbusBase { &mut self.base }
            fn on_init(&mut self) {
                assert!(self.base.is_activated);
                self.calls.push("init");
            }
            fn on_release(&mut self) {
                assert!(self.base.is_activated);
                self.calls.push("release");
            }
        }
        let mut device = Device { base: HidbusBase::new(Arc::new(TestRuntime::default())), calls: Vec::new() };
        let erased: &mut dyn HidbusDevice = &mut device;
        erased.deactivate_device();
        erased.activate_device();
        erased.activate_device();
        erased.deactivate_device();
        erased.deactivate_device();
        erased.activate_device();
        assert!(erased.is_device_activated());
        assert_eq!(device.calls, ["init", "release", "init"]);
    }

    #[test]
    fn concrete_backends_expose_the_hidbus_interface() {
        let devices: Vec<Box<dyn HidbusDevice>> = vec![
            Box::new(super::super::ringcon::RingController::new(
                Arc::new(parking_lot::Mutex::new(crate::frontend::emulated_controller::EmulatedController::new(crate::hid_types::NpadIdType::Player1))),
                Arc::new(TestRuntime::default()))),
            Box::new(super::super::stubbed::HidbusStubbed::new(Arc::new(TestRuntime::default()))),
            Box::new(super::super::starlink::Starlink::new(Arc::new(TestRuntime::default()))),
        ];
        for (mut device, expected_id) in devices.into_iter().zip([0x20, 0xff, 0x28]) {
            assert_eq!(device.get_device_id(), expected_id);
            device.activate_device();
            device.enable(true);
            device.set_polling_mode(JoyPollingMode::SixAxisSensorEnable);
            device.set_transfer_memory_address(0x1000);
            assert!(device.is_device_activated());
            assert!(device.is_enabled());
            assert!(device.is_polling_mode());
            assert_eq!(device.base().transfer_memory, 0x1000);
            device.disable_polling_mode();
            device.deactivate_device();
            assert!(!device.is_device_activated());
            assert!(!device.is_polling_mode());
        }
    }

    #[test]
    fn polling_accessor_defaults_match_upstream() {
        let base = HidbusBase::new(Arc::new(TestRuntime::default()));
        assert_eq!(base.disable_sixaxis_data.header.result.raw(), u32::MAX);
        assert_eq!(base.enable_sixaxis_data.header.result.raw(), u32::MAX);
        assert_eq!(base.button_only_data.header.result.raw(), u32::MAX);
        assert_eq!(base.disable_sixaxis_data.entries.len(), 0xB);
        assert_eq!(base.enable_sixaxis_data.entries.len(), 0xB);
        assert_eq!(base.button_only_data.entries.len(), 0xB);
    }

    #[test]
    fn device_owns_its_runtime_until_destruction() {
        let runtime = Arc::new(TestRuntime::default());
        let weak = Arc::downgrade(&runtime);
        let device = super::super::stubbed::HidbusStubbed::new(runtime);
        assert!(weak.upgrade().is_some());
        drop(device);
        assert!(weak.upgrade().is_none());
    }
}

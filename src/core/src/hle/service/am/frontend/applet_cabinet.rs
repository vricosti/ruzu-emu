// SPDX-FileCopyrightText: Copyright 2022 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `core/hle/service/am/frontend/applet_cabinet.{h,cpp}`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use parking_lot::Mutex as ParkingMutex;

use crate::core::SystemRef;
use crate::frontend::applets::cabinet::{CabinetApplet, CabinetParameters};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::am::am_types::{CommonArguments, LibraryAppletMode};
use crate::hle::service::am::applet::Applet;
use crate::hle::service::am::applet_data_broker::AppletDataBroker;
use crate::hle::service::kernel_helpers::ServiceContext;
use crate::hle::service::mii::mii_types::{Age, Gender, Nickname, Race};
use crate::hle::service::nfc::common::device::NfcDevice;
use crate::hle::service::nfc::nfc_types::{DeviceState, NfcProtocol, TagInfo};
pub use crate::hle::service::nfp::nfp_types::CabinetMode;
use crate::hle::service::nfp::nfp_types::{
    ModelType, MountTarget, RegisterInfo, RegisterInfoPrivate,
};
use crate::hle::service::os::event::Event;

use super::applets::FrontendApplet;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CabinetAppletVersion {
    Version1 = 0x1,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CabinetFlags {
    None = 0,
    DeviceHandle = 1,
    TagInfo = 2,
    RegisterInfo = 4,
    All = 7,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CabinetResult {
    Cancel = 0,
    TagInfo = 2,
    RegisterInfo = 4,
    All = 6,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AmiiboSettingsStartParam {
    pub device_handle: u64,
    pub param_1: [u8; 0x20],
    pub param_2: u8,
    pub _padding: [u8; 7],
}
const _: () = assert!(std::mem::size_of::<AmiiboSettingsStartParam>() == 0x30);

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct StartParamForAmiiboSettings {
    pub param_1: u8,
    /// Raw wire value, decoded before use so malformed guest data cannot form an invalid enum.
    pub applet_mode: u8,
    pub flags: u8,
    pub amiibo_settings_1: u8,
    pub device_handle: u64,
    pub tag_info: TagInfo,
    pub register_info: RegisterInfo,
    pub amiibo_settings_3: [u8; 0x20],
    pub _padding: [u8; 0x24],
}
const _: () = assert!(std::mem::size_of::<StartParamForAmiiboSettings>() == 0x1A8);

impl Default for StartParamForAmiiboSettings {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[derive(Clone, Copy, Default)]
struct CabinetInput {
    applet_mode: CabinetMode,
    device_handle: u64,
    tag_info: TagInfo,
    register_info: RegisterInfo,
}

fn parse_input(data: &[u8]) -> Option<CabinetInput> {
    if data.len() < std::mem::size_of::<StartParamForAmiiboSettings>() {
        return None;
    }
    let applet_mode = match data[1] {
        0 => CabinetMode::StartNicknameAndOwnerSettings,
        1 => CabinetMode::StartGameDataEraser,
        2 => CabinetMode::StartRestorer,
        3 => CabinetMode::StartFormatter,
        mode => {
            log::error!("Unknown CabinetMode={mode}");
            CabinetMode::StartNicknameAndOwnerSettings
        }
    };
    Some(CabinetInput {
        applet_mode,
        device_handle: u64::from_le_bytes(data[4..12].try_into().unwrap()),
        tag_info: unsafe { std::ptr::read_unaligned(data[12..].as_ptr().cast::<TagInfo>()) },
        register_info: unsafe {
            std::ptr::read_unaligned(data[0x64..].as_ptr().cast::<RegisterInfo>())
        },
    })
}

fn output_data(
    result: u8,
    device_handle: u64,
    tag_info: &TagInfo,
    register_info: &RegisterInfo,
) -> Vec<u8> {
    let mut data = vec![0; 0x188];
    data[0] = result;
    data[4..12].copy_from_slice(&device_handle.to_le_bytes());
    unsafe {
        std::ptr::copy_nonoverlapping(
            (tag_info as *const TagInfo).cast::<u8>(),
            data[12..].as_mut_ptr(),
            std::mem::size_of::<TagInfo>(),
        );
        std::ptr::copy_nonoverlapping(
            (register_info as *const RegisterInfo).cast::<u8>(),
            data[0x64..].as_mut_ptr(),
            std::mem::size_of::<RegisterInfo>(),
        );
    }
    data
}

pub struct Cabinet {
    system: SystemRef,
    applet: Weak<Mutex<Applet>>,
    broker: Arc<AppletDataBroker>,
    applet_mode: LibraryAppletMode,
    frontend: Arc<dyn CabinetApplet>,
    initialized: bool,
    is_complete: Arc<AtomicBool>,
    frontend_executing: Arc<AtomicBool>,
    nfp_device: Option<Arc<ParkingMutex<NfcDevice>>>,
    availability_change_event: Arc<Event>,
    service_context: ServiceContext,
    applet_input_common: CabinetInput,
}

impl Cabinet {
    pub fn new(
        system: SystemRef,
        applet: Weak<Mutex<Applet>>,
        broker: Arc<AppletDataBroker>,
        applet_mode: LibraryAppletMode,
        frontend: Arc<dyn CabinetApplet>,
    ) -> Self {
        let mut service_context = ServiceContext::new("CabinetApplet".to_string());
        let event_handle =
            service_context.create_event("CabinetApplet:AvailabilityChangeEvent".to_string());
        let availability_change_event = service_context
            .get_event(event_handle)
            .expect("just created Cabinet availability-change event");
        Self {
            system,
            applet,
            broker,
            applet_mode,
            frontend,
            initialized: false,
            is_complete: Arc::new(AtomicBool::new(false)),
            frontend_executing: Arc::new(AtomicBool::new(false)),
            nfp_device: None,
            availability_change_event,
            service_context,
            applet_input_common: CabinetInput::default(),
        }
    }

    fn exit(applet: &Weak<Mutex<Applet>>) {
        let Some(applet) = applet.upgrade() else {
            return;
        };
        let mut applet = applet.lock().unwrap();
        applet.is_completed = true;
        applet.signal_state_changed_event_without_process();
    }

    fn finish_cancel(
        input: CabinetInput,
        device: &Arc<ParkingMutex<NfcDevice>>,
        broker: &AppletDataBroker,
        complete: &AtomicBool,
        applet: &Weak<Mutex<Applet>>,
        frontend_executing: &AtomicBool,
    ) {
        device.lock().finalize();
        broker.get_out_data().push(output_data(
            CabinetResult::Cancel as u8,
            input.device_handle,
            &TagInfo::default(),
            &RegisterInfo::default(),
        ));
        complete.store(true, Ordering::Release);
        if !frontend_executing.load(Ordering::Acquire) {
            Self::exit(applet);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn display_completed(
        apply_changes: bool,
        amiibo_name: String,
        input: CabinetInput,
        device: Arc<ParkingMutex<NfcDevice>>,
        broker: Arc<AppletDataBroker>,
        complete: Arc<AtomicBool>,
        applet: Weak<Mutex<Applet>>,
        frontend_executing: Arc<AtomicBool>,
    ) {
        if !apply_changes {
            Self::finish_cancel(
                input,
                &device,
                &broker,
                &complete,
                &applet,
                &frontend_executing,
            );
        }

        let mut nfp_device = device.lock();
        if !matches!(
            nfp_device.get_current_state(),
            DeviceState::TagFound | DeviceState::TagMounted
        ) {
            drop(nfp_device);
            Self::finish_cancel(
                input,
                &device,
                &broker,
                &complete,
                &applet,
                &frontend_executing,
            );
            nfp_device = device.lock();
        }

        if nfp_device.get_current_state() == DeviceState::TagFound {
            nfp_device.mount(ModelType::Amiibo as u32, MountTarget::All as u32);
        }

        match input.applet_mode {
            CabinetMode::StartNicknameAndOwnerSettings => {
                let mut register_info = RegisterInfoPrivate::default();
                let length = amiibo_name.len().min(register_info.amiibo_name.len() - 1);
                register_info.amiibo_name[..length]
                    .copy_from_slice(&amiibo_name.as_bytes()[..length]);
                register_info
                    .mii_store_data
                    .build_random(Age::All, Gender::All, Race::All);
                let mut nickname = Nickname::default();
                nickname.data[..4].copy_from_slice(&[
                    b'r' as u16,
                    b'u' as u16,
                    b'z' as u16,
                    b'u' as u16,
                ]);
                register_info.mii_store_data.set_nickname(nickname);
                nfp_device.set_register_info_private(&register_info);
            }
            CabinetMode::StartGameDataEraser => {
                nfp_device.delete_application_area();
            }
            CabinetMode::StartRestorer => {
                nfp_device.restore();
            }
            CabinetMode::StartFormatter => {
                nfp_device.format();
            }
        }

        let register_info = nfp_device.get_register_info();
        let tag_info = nfp_device.get_tag_info();
        nfp_device.finalize();
        let mut result = CabinetResult::Cancel as u8;
        let register_info = register_info.map_or_else(
            |_| RegisterInfo::default(),
            |register_info| {
                result |= CabinetResult::RegisterInfo as u8;
                register_info
            },
        );
        let tag_info = tag_info.map_or_else(
            |_| TagInfo::default(),
            |tag_info| {
                result |= CabinetResult::TagInfo as u8;
                tag_info
            },
        );
        drop(nfp_device);

        broker.get_out_data().push(output_data(
            result,
            input.device_handle,
            &tag_info,
            &register_info,
        ));
        complete.store(true, Ordering::Release);
        if !frontend_executing.load(Ordering::Acquire) {
            Self::exit(&applet);
        }
    }
}

impl FrontendApplet for Cabinet {
    fn initialize(&mut self) {
        self.is_complete.store(false, Ordering::Release);
        let common_data = self
            .broker
            .get_in_data()
            .pop()
            .expect("Cabinet::Initialize missing common arguments");
        assert!(common_data.len() >= std::mem::size_of::<CommonArguments>());
        let input_data = self
            .broker
            .get_in_data()
            .pop()
            .expect("Cabinet::Initialize missing StartParamForAmiiboSettings");
        self.applet_input_common =
            parse_input(&input_data).expect("Cabinet input data must be at least 0x1A8 bytes");
        self.initialized = true;
    }

    fn get_status(&self) -> ResultCode {
        RESULT_SUCCESS
    }

    fn execute_interactive(&mut self) {
        panic!("Attempted to call interactive execution on non-interactive applet.");
    }

    fn execute(&mut self) {
        if self.is_complete.load(Ordering::Acquire) {
            return;
        }

        if self.nfp_device.is_none() {
            let hid_core = self.system.get().hid_core();
            let npad_id = hid_core.lock().get_first_npad_id();
            let controller = hid_core.lock().get_emulated_controller(npad_id);
            let mut device = NfcDevice::new_with_controller(
                npad_id as u64,
                Some(controller),
                Some(Arc::clone(&self.availability_change_event)),
                &mut self.service_context,
                self.system,
            );
            device.initialize();
            device.start_detection(NfcProtocol::ALL);
            self.nfp_device = Some(Arc::new(ParkingMutex::new(device)));
        }

        let parameters = CabinetParameters {
            tag_info: self.applet_input_common.tag_info,
            register_info: self.applet_input_common.register_info,
            mode: self.applet_input_common.applet_mode,
        };
        let device = Arc::clone(self.nfp_device.as_ref().unwrap());
        let input = self.applet_input_common;
        let broker = Arc::clone(&self.broker);
        let complete = Arc::clone(&self.is_complete);
        let applet = self.applet.clone();
        let frontend_executing = Arc::clone(&self.frontend_executing);

        self.frontend_executing.store(true, Ordering::Release);
        self.frontend.show_cabinet_applet(
            Box::new(move |apply_changes, amiibo_name| {
                Self::display_completed(
                    apply_changes,
                    amiibo_name,
                    input,
                    device,
                    broker,
                    complete,
                    applet,
                    frontend_executing,
                );
            }),
            &parameters,
            Arc::clone(self.nfp_device.as_ref().unwrap()),
        );
        self.frontend_executing.store(false, Ordering::Release);
    }

    fn request_exit(&mut self) {
        self.frontend.close();
    }

    fn get_library_applet_mode(&self) -> LibraryAppletMode {
        self.applet_mode
    }

    fn is_initialized(&self) -> bool {
        self.initialized
    }

    fn is_complete(&self) -> bool {
        self.is_complete.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::System;
    use crate::frontend::applets::cabinet::DefaultCabinetApplet;
    use crate::hle::service::os::process::Process;

    fn bytes_of<T>(value: &T) -> Vec<u8> {
        unsafe {
            std::slice::from_raw_parts((value as *const T).cast::<u8>(), std::mem::size_of::<T>())
                .to_vec()
        }
    }

    #[test]
    fn cabinet_wire_layouts_and_cancel_output_match_upstream() {
        assert_eq!(std::mem::size_of::<StartParamForAmiiboSettings>(), 0x1A8);
        let output = output_data(0, 0x1234, &TagInfo::default(), &RegisterInfo::default());
        assert_eq!(output.len(), 0x188);
        assert_eq!(
            u64::from_le_bytes(output[4..12].try_into().unwrap()),
            0x1234
        );
        assert!(output[12..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn default_frontend_cancels_and_consumes_completion_state() {
        let system = System::new();
        let system_ref = SystemRef::from_ref(&system);
        let owner = Arc::new(Mutex::new(Applet::new(system_ref, Process::new(), false)));
        let broker = Arc::new(AppletDataBroker::new());
        broker.get_in_data().push(bytes_of(&CommonArguments {
            library_version: CabinetAppletVersion::Version1 as u32,
            ..CommonArguments::default()
        }));
        broker
            .get_in_data()
            .push(bytes_of(&StartParamForAmiiboSettings::default()));

        let mut cabinet = Cabinet::new(
            system_ref,
            Arc::downgrade(&owner),
            Arc::clone(&broker),
            LibraryAppletMode::AllForeground,
            Arc::new(DefaultCabinetApplet),
        );
        cabinet.initialize();
        cabinet.execute();

        assert!(cabinet.is_initialized());
        assert!(cabinet.is_complete());
        let output = broker.get_out_data().pop().unwrap();
        assert_eq!(output.len(), 0x188);
        assert_eq!(output[0], CabinetResult::Cancel as u8);
    }
}

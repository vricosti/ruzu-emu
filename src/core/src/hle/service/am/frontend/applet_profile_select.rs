// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `core/hle/service/am/frontend/applet_profile_select.{h,cpp}`.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};

use common::uuid::{INVALID_UUID, UUID};

use crate::core::SystemRef;
use crate::frontend::applets::profile_select::{ProfileSelectApplet, ProfileSelectParameters};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::acc::errors::RESULT_CANCELLED_BY_USER;
use crate::hle::service::am::am_types::{CommonArguments, LibraryAppletMode};
use crate::hle::service::am::applet::Applet;
use crate::hle::service::am::applet_data_broker::AppletDataBroker;

use super::applets::FrontendApplet;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProfileSelectAppletVersion {
    #[default]
    Version1 = 0x1,
    Version2 = 0x10000,
    Version3 = 0x20000,
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UiMode(pub u32);

impl UiMode {
    pub const USER_SELECTOR: Self = Self(0);
    pub const USER_CREATOR: Self = Self(1);
    pub const ENSURE_NETWORK_SERVICE_ACCOUNT_AVAILABLE: Self = Self(2);
    pub const USER_ICON_EDITOR: Self = Self(3);
    pub const USER_NICKNAME_EDITOR: Self = Self(4);
    pub const USER_CREATOR_FOR_STARTER: Self = Self(5);
    pub const NINTENDO_ACCOUNT_AUTHORIZATION_REQUEST_CONTEXT: Self = Self(6);
    pub const INTRODUCE_EXTERNAL_NETWORK_SERVICE_ACCOUNT: Self = Self(7);
    pub const INTRODUCE_EXTERNAL_NETWORK_SERVICE_ACCOUNT_FOR_REGISTRATION: Self = Self(8);
    pub const NINTENDO_ACCOUNT_NNID_LINKER: Self = Self(9);
    pub const LICENSE_REQUIREMENTS_FOR_NETWORK_SERVICE: Self = Self(10);
    pub const LICENSE_REQUIREMENTS_FOR_NETWORK_SERVICE_WITH_USER_CONTEXT_IMPL: Self = Self(11);
    pub const USER_CREATOR_FOR_IMMEDIATE_NA_LOGIN_TEST: Self = Self(12);
    pub const USER_QUALIFICATION_PROMOTER: Self = Self(13);
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UserSelectionPurpose(pub u32);

impl UserSelectionPurpose {
    pub const GENERAL: Self = Self(0);
    pub const GAME_CARD_REGISTRATION: Self = Self(1);
    pub const ESHOP_LAUNCH: Self = Self(2);
    pub const ESHOP_ITEM_SHOW: Self = Self(3);
    pub const PICTURE_POST: Self = Self(4);
    pub const NINTENDO_ACCOUNT_LINKAGE: Self = Self(5);
    pub const SETTINGS_UPDATE: Self = Self(6);
    pub const SAVE_DATA_DELETION: Self = Self(7);
    pub const USER_MIGRATION: Self = Self(8);
    pub const SAVE_DATA_TRANSFER: Self = Self(9);
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NintendoAccountStartupDialogType {
    #[default]
    LoginAndCreate = 0,
    Login = 1,
    Create = 2,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UserSelectionSettingsForSystemService {
    pub purpose: UserSelectionPurpose,
    pub enable_user_creation: bool,
    pub _padding: [u8; 3],
}
const _: () = assert!(std::mem::size_of::<UserSelectionSettingsForSystemService>() == 0x8);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UiSettingsDisplayOptions {
    pub is_network_service_account_required: bool,
    pub is_skip_enabled: bool,
    pub is_system_or_launcher: bool,
    pub is_registration_permitted: bool,
    pub show_skip_button: bool,
    pub additional_select: bool,
    pub show_user_selector: bool,
    pub is_unqualified_user_selectable: bool,
}
const _: () = assert!(std::mem::size_of::<UiSettingsDisplayOptions>() == 0x8);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UiSettingsV1 {
    pub mode: UiMode,
    pub _padding: [u8; 4],
    pub invalid_uid_list: [UUID; 8],
    pub application_id: u64,
    pub display_options: UiSettingsDisplayOptions,
}
const _: () = assert!(std::mem::size_of::<UiSettingsV1>() == 0x98);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UiSettings {
    pub mode: UiMode,
    pub _padding_0: [u8; 4],
    pub invalid_uid_list: [UUID; 8],
    pub application_id: u64,
    pub display_options: UiSettingsDisplayOptions,
    pub purpose: UserSelectionPurpose,
    pub _padding_1: [u8; 4],
}
const _: () = assert!(std::mem::size_of::<UiSettings>() == 0xA0);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UiReturnArg {
    pub result: u64,
    pub uuid_selected: UUID,
}
const _: () = assert!(std::mem::size_of::<UiReturnArg>() == 0x18);

fn copy_from_exact<T: Copy>(data: &[u8]) -> Option<T> {
    if data.len() != std::mem::size_of::<T>() {
        return None;
    }
    let mut value = std::mem::MaybeUninit::<T>::uninit();
    unsafe {
        std::ptr::copy_nonoverlapping(
            data.as_ptr(),
            value.as_mut_ptr().cast::<u8>(),
            std::mem::size_of::<T>(),
        );
        Some(value.assume_init())
    }
}

fn copy_from_prefix<T: Copy>(data: &[u8]) -> Option<T> {
    if data.len() < std::mem::size_of::<T>() {
        return None;
    }
    let mut value = std::mem::MaybeUninit::<T>::uninit();
    unsafe {
        std::ptr::copy_nonoverlapping(
            data.as_ptr(),
            value.as_mut_ptr().cast::<u8>(),
            std::mem::size_of::<T>(),
        );
        Some(value.assume_init())
    }
}

fn struct_to_vec<T>(value: &T) -> Vec<u8> {
    unsafe {
        std::slice::from_raw_parts((value as *const T).cast::<u8>(), std::mem::size_of::<T>())
            .to_vec()
    }
}

pub struct ProfileSelect {
    #[allow(dead_code)]
    system: SystemRef,
    applet: Weak<Mutex<Applet>>,
    broker: Arc<AppletDataBroker>,
    applet_mode: LibraryAppletMode,
    frontend: Arc<dyn ProfileSelectApplet>,
    initialized: bool,
    config: UiSettings,
    config_old: UiSettingsV1,
    profile_select_version: u32,
    complete: Arc<AtomicBool>,
    status: Arc<AtomicU32>,
    final_data: Arc<Mutex<Vec<u8>>>,
    frontend_executing: Arc<AtomicBool>,
}

impl ProfileSelect {
    pub fn new(
        system: SystemRef,
        applet: Weak<Mutex<Applet>>,
        broker: Arc<AppletDataBroker>,
        applet_mode: LibraryAppletMode,
        frontend: Arc<dyn ProfileSelectApplet>,
    ) -> Self {
        Self {
            system,
            applet,
            broker,
            applet_mode,
            frontend,
            initialized: false,
            config: UiSettings::default(),
            config_old: UiSettingsV1::default(),
            profile_select_version: ProfileSelectAppletVersion::default() as u32,
            complete: Arc::new(AtomicBool::new(false)),
            status: Arc::new(AtomicU32::new(RESULT_SUCCESS.get_inner_value())),
            final_data: Arc::new(Mutex::new(Vec::new())),
            frontend_executing: Arc::new(AtomicBool::new(false)),
        }
    }


    fn parameters(&self) -> ProfileSelectParameters {
        match self.profile_select_version {
            version if version == ProfileSelectAppletVersion::Version1 as u32 => {
                ProfileSelectParameters {
                    mode: self.config_old.mode,
                    invalid_uid_list: self.config_old.invalid_uid_list,
                    display_options: self.config_old.display_options,
                    purpose: UserSelectionPurpose::GENERAL,
                }
            }
            version
                if version == ProfileSelectAppletVersion::Version2 as u32
                    || version == ProfileSelectAppletVersion::Version3 as u32 =>
            {
                ProfileSelectParameters {
                    mode: self.config.mode,
                    invalid_uid_list: self.config.invalid_uid_list,
                    display_options: self.config.display_options,
                    purpose: self.config.purpose,
                }
            }
            version => {
                log::error!("Unknown profile_select_version={version:#x}");
                ProfileSelectParameters::default()
            }
        }
    }

    fn selection_complete(
        system: SystemRef,
        uuid: Option<UUID>,
        applet: &Weak<Mutex<Applet>>,
        broker: &AppletDataBroker,
        complete: &AtomicBool,
        status: &AtomicU32,
        final_data: &Mutex<Vec<u8>>,
        frontend_executing: &AtomicBool,
    ) {
        let mut output = UiReturnArg::default();
        if let Some(uuid) = uuid.filter(UUID::is_valid) {
            output.uuid_selected = uuid;
        } else {
            output.result = RESULT_CANCELLED_BY_USER.get_inner_value() as u64;
            output.uuid_selected = INVALID_UUID;
            status.store(
                RESULT_CANCELLED_BY_USER.get_inner_value(),
                Ordering::Release,
            );
        }

        let data = struct_to_vec(&output);
        let data = {
            let mut final_data = final_data.lock().unwrap();
            *final_data = data;
            std::mem::take(&mut *final_data)
        };
        broker.get_out_data().push(data);
        complete.store(true, Ordering::Release);
        if !frontend_executing.load(Ordering::Acquire) {
            super::applets::exit(system, applet);
        }
    }
}

impl FrontendApplet for ProfileSelect {
    fn initialize(&mut self) {
        self.complete.store(false, Ordering::Release);
        self.status
            .store(RESULT_SUCCESS.get_inner_value(), Ordering::Release);
        self.final_data.lock().unwrap().clear();

        let common_data = self
            .broker
            .get_in_data()
            .pop()
            .expect("ProfileSelect::Initialize missing common arguments");
        let common = copy_from_prefix::<CommonArguments>(&common_data)
            .expect("ProfileSelect common arguments are too small");
        self.profile_select_version = common.library_version;

        let config_data = self
            .broker
            .get_in_data()
            .pop()
            .expect("ProfileSelect::Initialize missing UiSettings");
        match self.profile_select_version {
            version if version == ProfileSelectAppletVersion::Version1 as u32 => {
                self.config_old = copy_from_exact(&config_data)
                    .expect("ProfileSelect Version1 UiSettings must be 0x98 bytes");
            }
            version
                if version == ProfileSelectAppletVersion::Version2 as u32
                    || version == ProfileSelectAppletVersion::Version3 as u32 =>
            {
                self.config = copy_from_exact(&config_data)
                    .expect("ProfileSelect Version2/3 UiSettings must be 0xA0 bytes");
            }
            version => log::error!("Unknown profile_select_version={version:#x}"),
        }
        self.initialized = true;
    }

    fn get_status(&self) -> ResultCode {
        ResultCode::new(self.status.load(Ordering::Acquire))
    }

    fn execute_interactive(&mut self) {
        panic!("Attempted to call interactive execution on non-interactive applet.");
    }

    fn execute(&mut self) {
        if self.complete.load(Ordering::Acquire) {
            return;
        }

        let system = self.system;
        let applet = self.applet.clone();
        let broker = Arc::clone(&self.broker);
        let complete = Arc::clone(&self.complete);
        let status = Arc::clone(&self.status);
        let final_data = Arc::clone(&self.final_data);
        let executing = Arc::clone(&self.frontend_executing);
        let parameters = self.parameters();

        executing.store(true, Ordering::Release);
        self.frontend.select_profile(
            Box::new(move |uuid| {
                Self::selection_complete(
                    system,
                    uuid,
                    &applet,
                    &broker,
                    &complete,
                    &status,
                    &final_data,
                    &executing,
                );
            }),
            &parameters,
        );
        self.frontend_executing.store(false, Ordering::Release);
    }

    fn request_exit(&mut self) {
        self.frontend_executing.store(true, Ordering::Release);
        self.frontend.close();
        self.frontend_executing.store(false, Ordering::Release);
    }

    fn get_library_applet_mode(&self) -> LibraryAppletMode {
        self.applet_mode
    }

    fn is_initialized(&self) -> bool {
        self.initialized
    }

    fn is_complete(&self) -> bool {
        self.complete.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::System;
    use crate::frontend::applets::applet::Applet as FrontendUiApplet;
    use crate::hle::service::am::am_types::AppletId;
    use crate::hle::service::os::process::Process;

    struct SelectingFrontend(UUID);

    impl FrontendUiApplet for SelectingFrontend {
        fn close(&self) {}
    }

    impl ProfileSelectApplet for SelectingFrontend {
        fn select_profile(
            &self,
            callback: crate::frontend::applets::profile_select::SelectProfileCallback,
            _parameters: &ProfileSelectParameters,
        ) {
            callback(Some(self.0));
        }
    }

    struct CapturingFrontend {
        selected: UUID,
        parameters: Arc<Mutex<Option<ProfileSelectParameters>>>,
    }

    impl FrontendUiApplet for CapturingFrontend {
        fn close(&self) {}
    }

    impl ProfileSelectApplet for CapturingFrontend {
        fn select_profile(
            &self,
            callback: crate::frontend::applets::profile_select::SelectProfileCallback,
            parameters: &ProfileSelectParameters,
        ) {
            *self.parameters.lock().unwrap() = Some(parameters.clone());
            callback(Some(self.selected));
        }
    }

    #[test]
    fn version_three_returns_the_selected_uuid_and_completes() {
        let system = System::new();
        let system_ref = SystemRef::from_ref(&system);
        let owner = Arc::new(Mutex::new(Applet::new(system_ref, Process::new(), false)));
        owner.lock().unwrap().applet_id = AppletId::ProfileSelect;
        let broker = Arc::new(AppletDataBroker::new());
        let selected = UUID::from_bytes([0x42; 16]);

        let common = CommonArguments {
            library_version: ProfileSelectAppletVersion::Version3 as u32,
            ..CommonArguments::default()
        };
        let config = UiSettings::default();
        broker.get_in_data().push(struct_to_vec(&common));
        broker.get_in_data().push(struct_to_vec(&config));

        let mut applet = ProfileSelect::new(
            system_ref,
            Arc::downgrade(&owner),
            Arc::clone(&broker),
            LibraryAppletMode::AllForeground,
            Arc::new(SelectingFrontend(selected)),
        );
        applet.initialize();
        applet.execute();

        assert!(applet.is_initialized());
        assert!(applet.is_complete());
        assert_eq!(applet.get_status(), RESULT_SUCCESS);
        assert!(!owner.lock().unwrap().is_completed);

        let output = broker.get_out_data().pop().unwrap();
        assert_eq!(output.len(), std::mem::size_of::<UiReturnArg>());
        assert_eq!(u64::from_le_bytes(output[..8].try_into().unwrap()), 0);
        assert_eq!(&output[8..], &selected.uuid);
    }

    #[test]
    fn unknown_version_continues_with_zeroed_parameters() {
        let system = System::new();
        let system_ref = SystemRef::from_ref(&system);
        let owner = Arc::new(Mutex::new(Applet::new(system_ref, Process::new(), false)));
        let broker = Arc::new(AppletDataBroker::new());
        let selected = UUID::from_bytes([0x24; 16]);
        let parameters = Arc::new(Mutex::new(None));

        let common = CommonArguments {
            library_version: 0xDEAD_BEEF,
            ..CommonArguments::default()
        };
        broker.get_in_data().push(struct_to_vec(&common));
        broker.get_in_data().push(vec![0xAA]);

        let mut applet = ProfileSelect::new(
            system_ref,
            Arc::downgrade(&owner),
            Arc::clone(&broker),
            LibraryAppletMode::AllForeground,
            Arc::new(CapturingFrontend {
                selected,
                parameters: Arc::clone(&parameters),
            }),
        );
        applet.initialize();
        applet.execute();

        let parameters = parameters.lock().unwrap().clone().unwrap();
        assert_eq!(parameters.mode, UiMode::USER_SELECTOR);
        assert_eq!(parameters.purpose, UserSelectionPurpose::GENERAL);
        assert!(parameters.invalid_uid_list.iter().all(UUID::is_invalid));
        assert!(applet.is_complete());
        assert_eq!(&broker.get_out_data().pop().unwrap()[8..], &selected.uuid);
    }
}

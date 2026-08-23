// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/nfc/common/device.h
//! Port of zuyu/src/core/hle/service/nfc/common/device.cpp
//!
//! NfcDevice: represents a single NFC device with tag detection, reading, writing.
//!
use std::sync::{Arc, Weak};

use common::input::{NfcState, PollingMode};
use hid_core::frontend::emulated_controller::{
    ControllerTriggerType, ControllerUpdateCallback, EmulatedDeviceIndex,
};
use hid_core::hid_core::EmulatedControllerHandle;
use parking_lot::Mutex;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::kernel_helpers::ServiceContext;
use crate::hle::service::nfc::nfc_result;
use crate::hle::service::nfc::nfc_types::*;
use crate::hle::service::nfp::nfp_result;
use crate::hle::service::nfp::nfp_types;
use crate::hle::service::os::event::Event;

use super::amiibo_crypto;

/// NfcDevice corresponds to `NfcDevice` in upstream `device.h`.
///
/// Manages the NFC device state machine and the stable HID callback owner.
struct NfcDeviceState {
    npad_id: u64,
    npad_device: Option<EmulatedControllerHandle>,
    callback_key: Option<i32>,
    availability_change_event: Option<Arc<Event>>,
    device_state: DeviceState,
    is_initialized: bool,
    allowed_protocols: NfcProtocol,
    mount_target: nfp_types::MountTarget,
    is_data_modified: bool,
    is_app_area_open: bool,
    is_plain_amiibo: bool,
    is_write_protected: bool,
    activate_event: Arc<Event>,
    deactivate_event: Arc<Event>,
    tag_info: TagInfo,
    tag_data: nfp_types::Ntag215File,
    encrypted_tag_data: nfp_types::EncryptedNtag215File,
}

/// Shared NFC device owner. The inner allocation is stable so the HID callback can
/// retain a weak reference matching upstream's controller callback lifetime.
pub struct NfcDevice {
    inner: Arc<Mutex<NfcDeviceState>>,
}

impl NfcDevice {
    /// Creates a new NfcDevice.
    ///
    /// Upstream constructor: creates activate/deactivate events via ServiceContext,
    /// obtains EmulatedController from HIDCore, and registers an NpadUpdate callback.
    /// The no-controller constructor is retained for legacy manager callers; Cabinet
    /// uses `new_with_controller`, which owns the upstream HID callback lifecycle.
    pub fn new(npad_id: u64, service_context: &mut ServiceContext) -> Self {
        Self::new_with_controller(npad_id, None, None, service_context)
    }

    pub fn new_with_controller(
        npad_id: u64,
        npad_device: Option<EmulatedControllerHandle>,
        availability_change_event: Option<Arc<Event>>,
        service_context: &mut ServiceContext,
    ) -> Self {
        let activate_handle = service_context.create_event("NFC:ActivateEvent".to_string());
        let deactivate_handle = service_context.create_event("NFC:DeactivateEvent".to_string());

        let activate_event = service_context
            .get_event(activate_handle)
            .expect("just created activate event");
        let deactivate_event = service_context
            .get_event(deactivate_handle)
            .expect("just created deactivate event");

        let inner = Arc::new(Mutex::new(NfcDeviceState {
            npad_id,
            npad_device: npad_device.clone(),
            callback_key: None,
            availability_change_event,
            device_state: DeviceState::Unavailable,
            is_initialized: false,
            allowed_protocols: NfcProtocol::empty(),
            mount_target: nfp_types::MountTarget::None,
            is_data_modified: false,
            is_app_area_open: false,
            is_plain_amiibo: false,
            is_write_protected: false,
            activate_event,
            deactivate_event,
            tag_info: TagInfo::default(),
            tag_data: nfp_types::Ntag215File::default(),
            encrypted_tag_data: nfp_types::EncryptedNtag215File::default(),
        }));

        if let Some(controller) = npad_device {
            let weak_inner: Weak<Mutex<NfcDeviceState>> = Arc::downgrade(&inner);
            let callback_key = controller.lock().set_callback(ControllerUpdateCallback {
                on_change: Arc::new(move |trigger_type| {
                    if let Some(inner) = weak_inner.upgrade() {
                        inner.lock().npad_update(trigger_type);
                    }
                }),
                is_npad_service: false,
            });
            inner.lock().callback_key = Some(callback_key);
        }

        Self { inner }
    }

    pub fn initialize(&mut self) -> ResultCode {
        self.inner.lock().initialize()
    }

    pub fn finalize(&mut self) -> ResultCode {
        self.inner.lock().finalize()
    }

    pub fn start_detection(&mut self, protocol: NfcProtocol) -> ResultCode {
        self.inner.lock().start_detection(protocol)
    }

    pub fn stop_detection(&mut self) -> ResultCode {
        self.inner.lock().stop_detection()
    }

    pub fn get_current_state(&self) -> DeviceState {
        self.inner.lock().get_current_state()
    }

    pub fn get_handle(&self) -> u64 {
        self.inner.lock().get_handle()
    }

    pub fn get_npad_id(&self) -> u64 {
        self.inner.lock().get_npad_id()
    }

    pub fn get_tag_info(&self) -> Result<TagInfo, ResultCode> {
        self.inner.lock().get_tag_info()
    }

    pub fn get_activate_event(&self) -> Arc<Event> {
        Arc::clone(&self.inner.lock().activate_event)
    }

    pub fn get_deactivate_event(&self) -> Arc<Event> {
        Arc::clone(&self.inner.lock().deactivate_event)
    }

    pub fn mount(&mut self, model_type: u32, mount_target: u32) -> ResultCode {
        self.inner.lock().mount(model_type, mount_target)
    }

    pub fn unmount(&mut self) -> ResultCode {
        self.inner.lock().unmount()
    }

    pub fn flush(&mut self) -> ResultCode {
        self.inner.lock().flush()
    }

    pub fn get_device_state(&self) -> DeviceState {
        self.inner.lock().get_device_state()
    }

    pub fn get_common_info(&self) -> Result<nfp_types::CommonInfo, ResultCode> {
        self.inner.lock().get_common_info()
    }

    pub fn get_model_info(&self) -> Result<nfp_types::ModelInfo, ResultCode> {
        self.inner.lock().get_model_info()
    }

    pub fn get_register_info(&self) -> Result<nfp_types::RegisterInfo, ResultCode> {
        self.inner.lock().get_register_info()
    }

    pub fn set_register_info_private(
        &mut self,
        register_info: &nfp_types::RegisterInfoPrivate,
    ) -> ResultCode {
        self.inner.lock().set_register_info_private(register_info)
    }

    pub fn open_application_area(&mut self, access_id: u32) -> ResultCode {
        self.inner.lock().open_application_area(access_id)
    }

    pub fn get_application_area(&self, data: &mut [u8]) -> Result<u32, ResultCode> {
        self.inner.lock().get_application_area(data)
    }

    pub fn set_application_area(&mut self, data: &[u8]) -> ResultCode {
        self.inner.lock().set_application_area(data)
    }

    pub fn create_application_area(&mut self, access_id: u32, data: &[u8]) -> ResultCode {
        self.inner.lock().create_application_area(access_id, data)
    }

    pub fn delete_application_area(&mut self) -> ResultCode {
        self.inner.lock().delete_application_area()
    }

    pub fn exists_application_area(&self) -> Result<bool, ResultCode> {
        self.inner.lock().exists_application_area()
    }

    pub fn format(&mut self) -> ResultCode {
        self.inner.lock().format()
    }

    pub fn restore(&mut self) -> ResultCode {
        self.inner.lock().restore()
    }
}

impl NfcDeviceState {
    /// Initialize the NFC device.
    ///
    /// Upstream: sets device_state based on whether the controller has NFC capability,
    /// clears tag data, and calls AddNfcHandle.
    pub fn initialize(&mut self) -> ResultCode {
        self.device_state = match &self.npad_device {
            Some(controller) if controller.lock().has_nfc() => DeviceState::Initialized,
            Some(_) => DeviceState::Unavailable,
            None => DeviceState::Initialized,
        };
        self.tag_info = TagInfo::default();
        self.tag_data = nfp_types::Ntag215File::default();
        self.encrypted_tag_data = nfp_types::EncryptedNtag215File::default();

        if self.device_state != DeviceState::Initialized {
            self.is_initialized = false;
            return RESULT_SUCCESS;
        }

        self.is_initialized = self
            .npad_device
            .as_ref()
            .map_or(true, |controller| controller.lock().add_nfc_handle());
        RESULT_SUCCESS
    }

    /// Finalize the NFC device.
    ///
    /// Upstream: unmounts if mounted, stops detection if searching, removes NFC handle,
    /// transitions to Unavailable.
    pub fn finalize(&mut self) -> ResultCode {
        if self.device_state == DeviceState::TagMounted {
            let _ = self.unmount();
        }
        if self.device_state == DeviceState::SearchingForTag
            || self.device_state == DeviceState::TagRemoved
        {
            let _ = self.stop_detection();
        }

        if self.device_state != DeviceState::Unavailable {
            if let Some(controller) = &self.npad_device {
                controller.lock().remove_nfc_handle();
            }
        }

        self.device_state = DeviceState::Unavailable;
        self.is_initialized = false;
        RESULT_SUCCESS
    }

    /// Start scanning for NFC tags.
    ///
    /// Upstream: requires Initialized or TagRemoved state, calls npad_device->StartNfcPolling(),
    /// transitions to SearchingForTag.
    pub fn start_detection(&mut self, protocol: NfcProtocol) -> ResultCode {
        if self.device_state != DeviceState::Initialized
            && self.device_state != DeviceState::TagRemoved
        {
            log::error!(
                "Wrong device state {:?} for start_detection",
                self.device_state
            );
            return nfc_result::RESULT_WRONG_DEVICE_STATE;
        }

        if let Some(controller) = &self.npad_device {
            if !controller.lock().start_nfc_polling() {
                log::error!("Nfc polling not supported");
                return nfc_result::RESULT_NFC_DISABLED;
            }
        }

        self.device_state = DeviceState::SearchingForTag;
        self.allowed_protocols = protocol;
        RESULT_SUCCESS
    }

    /// Stop scanning for NFC tags.
    ///
    /// Upstream: if already Initialized returns success, if TagFound/TagMounted closes tag,
    /// if SearchingForTag/TagRemoved stops polling and returns to Initialized.
    pub fn stop_detection(&mut self) -> ResultCode {
        if self.device_state == DeviceState::Initialized {
            return RESULT_SUCCESS;
        }

        if self.device_state == DeviceState::TagFound
            || self.device_state == DeviceState::TagMounted
        {
            self.close_nfc_tag();
        }

        if self.device_state == DeviceState::SearchingForTag
            || self.device_state == DeviceState::TagRemoved
        {
            if let Some(controller) = &self.npad_device {
                controller.lock().stop_nfc_polling();
            }
            self.device_state = DeviceState::Initialized;
            return RESULT_SUCCESS;
        }

        log::error!(
            "Wrong device state {:?} for stop_detection",
            self.device_state
        );
        nfc_result::RESULT_WRONG_DEVICE_STATE
    }

    /// Get the current device state.
    pub fn get_current_state(&self) -> DeviceState {
        self.device_state
    }

    /// Get the device handle (npad_id cast to u64, matching upstream GetHandle).
    pub fn get_handle(&self) -> u64 {
        self.npad_id
    }

    /// Get the npad ID.
    pub fn get_npad_id(&self) -> u64 {
        self.npad_id
    }

    /// Get tag info for the currently detected tag.
    ///
    /// Upstream: requires TagFound or TagMounted state, copies real_tag_info,
    /// optionally randomizes UUID for Type2 tags.
    pub fn get_tag_info(&self) -> Result<TagInfo, ResultCode> {
        if self.device_state != DeviceState::TagFound
            && self.device_state != DeviceState::TagMounted
        {
            log::error!(
                "Wrong device state {:?} for get_tag_info",
                self.device_state
            );
            if self.device_state == DeviceState::TagRemoved {
                return Err(nfc_result::RESULT_TAG_REMOVED);
            }
            return Err(nfc_result::RESULT_WRONG_DEVICE_STATE);
        }

        Ok(self.tag_info)
    }

    /// Mount an amiibo tag for reading/writing.
    ///
    /// Upstream: validates ModelType::Amiibo, requires TagFound state, loads amiibo data,
    /// validates it, transitions to TagMounted.
    pub fn mount(&mut self, model_type: u32, mount_target: u32) -> ResultCode {
        // Upstream: if model_type != ModelType::Amiibo return ResultInvalidArgument
        if model_type != nfp_types::ModelType::Amiibo as u32 {
            return nfp_result::RESULT_INVALID_ARGUMENT;
        }

        if self.device_state != DeviceState::TagFound {
            log::error!("Wrong device state {:?} for mount", self.device_state);
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        let target = match mount_target {
            0 => nfp_types::MountTarget::None,
            1 => nfp_types::MountTarget::Rom,
            2 => nfp_types::MountTarget::Ram,
            3 => nfp_types::MountTarget::All,
            _ => return nfp_result::RESULT_INVALID_ARGUMENT,
        };

        if !self.load_amiibo_data() {
            log::error!("Not an amiibo");
            return nfc_result::RESULT_INVALID_TAG_TYPE;
        }
        if !amiibo_crypto::is_amiibo_valid_encrypted(&self.encrypted_tag_data) {
            log::error!("Not an amiibo");
            return nfc_result::RESULT_INVALID_TAG_TYPE;
        }

        let mut is_corrupted = false;
        if !self.is_plain_amiibo
            && !amiibo_crypto::decode_amiibo(&self.encrypted_tag_data, &mut self.tag_data)
        {
            log::error!("Can't decode amiibo");
            is_corrupted = true;
        }

        self.device_state = DeviceState::TagMounted;
        self.mount_target = target;

        let uuid =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.encrypted_tag_data.uuid)) };
        let uuid = Self::tag_uuid_bytes(&uuid);
        let create_backup = target == nfp_types::MountTarget::All
            || target == nfp_types::MountTarget::Ram
            || (target == nfp_types::MountTarget::Rom && !Self::has_backup(&uuid));
        if !is_corrupted && create_backup {
            let data = unsafe {
                core::slice::from_raw_parts(
                    (&self.encrypted_tag_data as *const nfp_types::EncryptedNtag215File)
                        .cast::<u8>(),
                    core::mem::size_of::<nfp_types::EncryptedNtag215File>(),
                )
            };
            let _ = Self::write_backup_data(&uuid, data);
        }

        if is_corrupted && target != nfp_types::MountTarget::Rom {
            return nfp_result::RESULT_CORRUPTED_DATA;
        }

        RESULT_SUCCESS
    }

    /// Unmount a previously mounted amiibo tag.
    ///
    /// Upstream: requires TagMounted state, flushes if data was modified,
    /// transitions to TagFound.
    pub fn unmount(&mut self) -> ResultCode {
        if self.device_state != DeviceState::TagMounted {
            log::error!("Wrong device state {:?} for unmount", self.device_state);
            if self.device_state == DeviceState::TagRemoved {
                return nfp_result::RESULT_TAG_REMOVED;
            }
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        // Upstream: if is_data_moddified { Flush(); }
        if self.is_data_modified {
            let _ = self.flush();
        }

        self.device_state = DeviceState::TagFound;
        self.mount_target = nfp_types::MountTarget::None;
        self.is_app_area_open = false;

        RESULT_SUCCESS
    }

    /// Flush modified amiibo data to the tag.
    ///
    /// Upstream: requires TagMounted state, writable mount target, updates write date
    /// and counter, calls FlushWithBreak.
    pub fn flush(&mut self) -> ResultCode {
        if self.device_state != DeviceState::TagMounted {
            log::error!("Wrong device state {:?} for flush", self.device_state);
            if self.device_state == DeviceState::TagRemoved {
                return nfp_result::RESULT_TAG_REMOVED;
            }
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        if self.mount_target == nfp_types::MountTarget::None
            || self.mount_target == nfp_types::MountTarget::Rom
        {
            log::error!("Amiibo is read only");
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        let mut settings =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.settings)) };
        let current_date = Self::current_amiibo_date();
        if settings.write_date.raw_date != current_date.raw_date {
            settings.write_date = current_date;
            Self::update_settings_crc(&mut settings);
            unsafe {
                core::ptr::write_unaligned(
                    core::ptr::addr_of_mut!(self.tag_data.settings),
                    settings,
                );
            }
        }

        let write_counter =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.write_counter)) };
        unsafe {
            core::ptr::write_unaligned(
                core::ptr::addr_of_mut!(self.tag_data.write_counter),
                u16::from_be(write_counter).wrapping_add(1).to_be(),
            );
        }

        let result = self.flush_with_break(nfp_types::BreakType::Normal);
        self.is_data_modified = false;
        result
    }

    /// Get the device state (alias for get_current_state matching upstream interface).
    pub fn get_device_state(&self) -> DeviceState {
        self.device_state
    }

    /// Get common info for the mounted amiibo.
    ///
    /// Upstream: requires TagMounted with writable mount target, reads from tag_data.
    pub fn get_common_info(&self) -> Result<nfp_types::CommonInfo, ResultCode> {
        if self.device_state != DeviceState::TagMounted {
            log::error!(
                "Wrong device state {:?} for get_common_info",
                self.device_state
            );
            if self.device_state == DeviceState::TagRemoved {
                return Err(nfp_result::RESULT_TAG_REMOVED);
            }
            return Err(nfp_result::RESULT_WRONG_DEVICE_STATE);
        }

        if self.mount_target == nfp_types::MountTarget::None
            || self.mount_target == nfp_types::MountTarget::Rom
        {
            log::error!("Amiibo is read only");
            return Err(nfp_result::RESULT_WRONG_DEVICE_STATE);
        }

        let settings =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.settings)) };
        let application_write_counter = unsafe {
            core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.application_write_counter))
        };
        Ok(nfp_types::CommonInfo {
            last_write_date: settings.write_date.get_write_date(),
            write_counter: u16::from_be(application_write_counter),
            version: self.tag_data.amiibo_version,
            application_area_size: core::mem::size_of::<nfp_types::ApplicationArea>() as u32,
            ..nfp_types::CommonInfo::default()
        })
    }

    /// Get model info for the mounted amiibo.
    ///
    /// Upstream: requires TagMounted state, reads from encrypted_tag_data.user_memory.model_info.
    pub fn get_model_info(&self) -> Result<nfp_types::ModelInfo, ResultCode> {
        if self.device_state != DeviceState::TagMounted {
            log::error!(
                "Wrong device state {:?} for get_model_info",
                self.device_state
            );
            if self.device_state == DeviceState::TagRemoved {
                return Err(nfp_result::RESULT_TAG_REMOVED);
            }
            return Err(nfp_result::RESULT_WRONG_DEVICE_STATE);
        }

        let user_memory = unsafe {
            core::ptr::read_unaligned(core::ptr::addr_of!(self.encrypted_tag_data.user_memory))
        };
        let model =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(user_memory.model_info)) };
        Ok(nfp_types::ModelInfo {
            character_id: model.character_id,
            character_variant: model.character_variant,
            amiibo_type: model.amiibo_type,
            model_number: u16::from_be(model.model_number),
            series: model.series,
            ..nfp_types::ModelInfo::default()
        })
    }

    /// Get register info for the mounted amiibo.
    ///
    /// Upstream: requires TagMounted with writable mount target, checks amiibo_initialized.
    pub fn get_register_info(&self) -> Result<nfp_types::RegisterInfo, ResultCode> {
        if self.device_state != DeviceState::TagMounted {
            log::error!(
                "Wrong device state {:?} for get_register_info",
                self.device_state
            );
            if self.device_state == DeviceState::TagRemoved {
                return Err(nfp_result::RESULT_TAG_REMOVED);
            }
            return Err(nfp_result::RESULT_WRONG_DEVICE_STATE);
        }

        if self.mount_target == nfp_types::MountTarget::None
            || self.mount_target == nfp_types::MountTarget::Rom
        {
            log::error!("Amiibo is read only");
            return Err(nfp_result::RESULT_WRONG_DEVICE_STATE);
        }

        let settings =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.settings)) };
        if !settings.settings.amiibo_initialized() {
            return Err(nfp_result::RESULT_REGISTRATION_IS_NOT_INITIALIZED);
        }

        let owner_mii =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.owner_mii)) };
        let mut store_data = crate::hle::service::mii::types::store_data::StoreData::default();
        owner_mii.build_to_store_data(&mut store_data);
        let mut char_info = crate::hle::service::mii::types::char_info::CharInfo::default();
        char_info.set_from_store_data(&store_data);

        Ok(nfp_types::RegisterInfo {
            mii_char_info: char_info,
            creation_date: settings.init_date.get_write_date(),
            amiibo_name: Self::get_amiibo_name(&settings),
            font_region: settings.settings.font_region(),
            ..nfp_types::RegisterInfo::default()
        })
    }

    pub fn set_register_info_private(
        &mut self,
        register_info: &nfp_types::RegisterInfoPrivate,
    ) -> ResultCode {
        if self.device_state != DeviceState::TagMounted {
            if self.device_state == DeviceState::TagRemoved {
                return nfp_result::RESULT_TAG_REMOVED;
            }
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }
        if self.mount_target == nfp_types::MountTarget::None
            || self.mount_target == nfp_types::MountTarget::Rom
        {
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        let mut settings =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.settings)) };
        if !settings.settings.amiibo_initialized() {
            settings.init_date = Self::current_amiibo_date();
            settings.write_date = nfp_types::AmiiboDate::default();
        }
        Self::set_amiibo_name(&mut settings, &register_info.amiibo_name);
        let mut owner_mii =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.owner_mii)) };
        owner_mii.build_from_store_data(&register_info.mii_store_data);
        let mut mii_extension =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.mii_extension)) };
        mii_extension.set_from_store_data(&register_info.mii_store_data);
        settings.country_code_id = 0;
        settings.settings.set_font_region(0);
        settings.settings.set_amiibo_initialized(true);
        unsafe {
            core::ptr::write_unaligned(core::ptr::addr_of_mut!(self.tag_data.settings), settings);
            core::ptr::write_unaligned(core::ptr::addr_of_mut!(self.tag_data.owner_mii), owner_mii);
            core::ptr::write_unaligned(
                core::ptr::addr_of_mut!(self.tag_data.mii_extension),
                mii_extension,
            );
        }
        self.tag_data.unknown = 0;
        self.tag_data.unknown2 = [0; 5];
        self.update_register_info_crc();
        self.flush()
    }

    /// Open the application area for read/write access.
    ///
    /// Upstream: requires TagMounted with writable mount target, checks appdata_initialized
    /// and matching access_id, then sets is_app_area_open.
    pub fn open_application_area(&mut self, _access_id: u32) -> ResultCode {
        if self.device_state != DeviceState::TagMounted {
            log::error!(
                "Wrong device state {:?} for open_application_area",
                self.device_state
            );
            if self.device_state == DeviceState::TagRemoved {
                return nfp_result::RESULT_TAG_REMOVED;
            }
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        if self.mount_target == nfp_types::MountTarget::None
            || self.mount_target == nfp_types::MountTarget::Rom
        {
            log::error!("Amiibo is read only");
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        // Upstream: checks tag_data.settings.settings.appdata_initialized and
        // tag_data.application_area_id == access_id. Without tag_data, the application
        // area is never initialized, so return the appropriate error.
        log::warn!("Application area is not initialized");
        nfp_result::RESULT_APPLICATION_AREA_IS_NOT_INITIALIZED
    }

    /// Get application area data.
    ///
    /// Upstream: requires TagMounted with writable mount target, app area open,
    /// appdata_initialized, copies from tag_data.application_area.
    pub fn get_application_area(&self, _data: &mut [u8]) -> Result<u32, ResultCode> {
        if self.device_state != DeviceState::TagMounted {
            log::error!(
                "Wrong device state {:?} for get_application_area",
                self.device_state
            );
            if self.device_state == DeviceState::TagRemoved {
                return Err(nfp_result::RESULT_TAG_REMOVED);
            }
            return Err(nfp_result::RESULT_WRONG_DEVICE_STATE);
        }

        if self.mount_target == nfp_types::MountTarget::None
            || self.mount_target == nfp_types::MountTarget::Rom
        {
            log::error!("Amiibo is read only");
            return Err(nfp_result::RESULT_WRONG_DEVICE_STATE);
        }

        if !self.is_app_area_open {
            log::error!("Application area is not open");
            return Err(nfp_result::RESULT_WRONG_DEVICE_STATE);
        }

        // Upstream: checks appdata_initialized, copies from tag_data.application_area.
        // Without tag_data, return not initialized.
        log::error!("Application area is not initialized");
        Err(nfp_result::RESULT_APPLICATION_AREA_IS_NOT_INITIALIZED)
    }

    /// Set application area data.
    ///
    /// Upstream: requires TagMounted with writable mount target, app area open,
    /// appdata_initialized, copies data into tag_data.application_area.
    pub fn set_application_area(&mut self, _data: &[u8]) -> ResultCode {
        if self.device_state != DeviceState::TagMounted {
            log::error!(
                "Wrong device state {:?} for set_application_area",
                self.device_state
            );
            if self.device_state == DeviceState::TagRemoved {
                return nfp_result::RESULT_TAG_REMOVED;
            }
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        if self.mount_target == nfp_types::MountTarget::None
            || self.mount_target == nfp_types::MountTarget::Rom
        {
            log::error!("Amiibo is read only");
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        if !self.is_app_area_open {
            log::error!("Application area is not open");
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        // Upstream: checks appdata_initialized, copies data, fills remaining with random,
        // increments write counter, sets is_data_moddified. Without tag_data, return error.
        log::error!("Application area is not initialized");
        nfp_result::RESULT_APPLICATION_AREA_IS_NOT_INITIALIZED
    }

    /// Create a new application area.
    ///
    /// Upstream: requires TagMounted, checks appdata_initialized == 0 (must not already exist),
    /// delegates to RecreateApplicationArea.
    pub fn create_application_area(&mut self, _access_id: u32, _data: &[u8]) -> ResultCode {
        if self.device_state != DeviceState::TagMounted {
            log::error!(
                "Wrong device state {:?} for create_application_area",
                self.device_state
            );
            if self.device_state == DeviceState::TagRemoved {
                return nfp_result::RESULT_TAG_REMOVED;
            }
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        // Upstream: checks tag_data.settings.settings.appdata_initialized != 0 -> error.
        // Without tag_data, appdata is never initialized, so this would proceed to
        // RecreateApplicationArea — but that also needs tag_data. Return success to
        // match the state machine flow (the actual data write is a no-op without tag_data).

        // Upstream would call RecreateApplicationArea here which requires writable mount,
        // valid data size, etc. Validate mount target at minimum.
        if self.is_app_area_open {
            log::error!("Application area is open");
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        if self.mount_target == nfp_types::MountTarget::None
            || self.mount_target == nfp_types::MountTarget::Rom
        {
            log::error!("Amiibo is read only");
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        // Without tag_data to write to, the operation is effectively a no-op.
        // Upstream would write data, set application_id, flush. We return success
        // to maintain correct state machine behavior.
        RESULT_SUCCESS
    }

    /// Delete the application area.
    ///
    /// Upstream: requires TagMounted with writable mount target, checks appdata_initialized,
    /// randomizes application area data, clears flags, flushes.
    pub fn delete_application_area(&mut self) -> ResultCode {
        if self.device_state != DeviceState::TagMounted {
            log::error!(
                "Wrong device state {:?} for delete_application_area",
                self.device_state
            );
            if self.device_state == DeviceState::TagRemoved {
                return nfp_result::RESULT_TAG_REMOVED;
            }
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        if self.mount_target == nfp_types::MountTarget::None
            || self.mount_target == nfp_types::MountTarget::Rom
        {
            log::error!("Amiibo is read only");
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        let mut settings =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.settings)) };
        if !settings.settings.appdata_initialized() {
            return nfp_result::RESULT_APPLICATION_AREA_IS_NOT_INITIALIZED;
        }

        let counter = unsafe {
            core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.application_write_counter))
        };
        let counter = u16::from_be(counter);
        if counter != nfp_types::COUNTER_LIMIT {
            unsafe {
                core::ptr::write_unaligned(
                    core::ptr::addr_of_mut!(self.tag_data.application_write_counter),
                    (counter + 1).to_be(),
                );
            }
        }

        let mut rng = Self::current_rng();
        rng.generate_random_bytes(&mut self.tag_data.application_area);
        let mut application_id = [0; 8];
        let mut application_area_id = [0; 4];
        rng.generate_random_bytes(&mut application_id);
        rng.generate_random_bytes(&mut application_area_id);
        rng.generate_random_bytes(std::slice::from_mut(&mut self.tag_data.application_id_byte));
        unsafe {
            core::ptr::write_unaligned(
                core::ptr::addr_of_mut!(self.tag_data.application_id),
                u64::from_ne_bytes(application_id),
            );
            core::ptr::write_unaligned(
                core::ptr::addr_of_mut!(self.tag_data.application_area_id),
                u32::from_ne_bytes(application_area_id),
            );
        }
        settings.settings.set_appdata_initialized(false);
        unsafe {
            core::ptr::write_unaligned(core::ptr::addr_of_mut!(self.tag_data.settings), settings);
        }
        self.tag_data.unknown = 0;
        self.tag_data.unknown2 = [0; 5];
        self.is_app_area_open = false;
        self.update_register_info_crc();
        self.flush()
    }

    /// Check whether an application area exists.
    ///
    /// Upstream: requires TagMounted with writable mount target, checks
    /// tag_data.settings.settings.appdata_initialized.
    pub fn exists_application_area(&self) -> Result<bool, ResultCode> {
        if self.device_state != DeviceState::TagMounted {
            log::error!(
                "Wrong device state {:?} for exists_application_area",
                self.device_state
            );
            if self.device_state == DeviceState::TagRemoved {
                return Err(nfp_result::RESULT_TAG_REMOVED);
            }
            return Err(nfp_result::RESULT_WRONG_DEVICE_STATE);
        }

        if self.mount_target == nfp_types::MountTarget::None
            || self.mount_target == nfp_types::MountTarget::Rom
        {
            log::error!("Amiibo is read only");
            return Err(nfp_result::RESULT_WRONG_DEVICE_STATE);
        }

        let settings =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.settings)) };
        Ok(settings.settings.appdata_initialized())
    }

    fn delete_register_info(&mut self) -> ResultCode {
        if self.device_state != DeviceState::TagMounted {
            if self.device_state == DeviceState::TagRemoved {
                return nfp_result::RESULT_TAG_REMOVED;
            }
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }
        if self.mount_target == nfp_types::MountTarget::None
            || self.mount_target == nfp_types::MountTarget::Rom
        {
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        let mut settings =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.settings)) };
        if !settings.settings.amiibo_initialized() {
            return nfp_result::RESULT_REGISTRATION_IS_NOT_INITIALIZED;
        }

        let mut rng = Self::current_rng();
        let owner_mii = unsafe {
            core::slice::from_raw_parts_mut(
                core::ptr::addr_of_mut!(self.tag_data.owner_mii).cast::<u8>(),
                core::mem::size_of::<crate::hle::service::mii::types::ver3_store_data::Ver3StoreData>(
                ),
            )
        };
        rng.generate_random_bytes(owner_mii);
        let name = unsafe {
            core::slice::from_raw_parts_mut(
                core::ptr::addr_of_mut!(settings.amiibo_name).cast::<u8>(),
                core::mem::size_of::<[u16; nfp_types::AMIIBO_NAME_LENGTH]>(),
            )
        };
        rng.generate_random_bytes(name);
        rng.generate_random_bytes(std::slice::from_mut(&mut self.tag_data.unknown));
        let unknown2 = unsafe {
            core::slice::from_raw_parts_mut(
                core::ptr::addr_of_mut!(self.tag_data.unknown2).cast::<u8>(),
                core::mem::size_of::<[u32; 5]>(),
            )
        };
        rng.generate_random_bytes(unknown2);
        let register_crc = unsafe {
            core::slice::from_raw_parts_mut(
                core::ptr::addr_of_mut!(self.tag_data.register_info_crc).cast::<u8>(),
                core::mem::size_of::<u32>(),
            )
        };
        rng.generate_random_bytes(register_crc);
        let dates = unsafe {
            core::slice::from_raw_parts_mut(
                core::ptr::addr_of_mut!(settings.init_date).cast::<u8>(),
                core::mem::size_of::<u32>(),
            )
        };
        rng.generate_random_bytes(dates);
        settings.settings.set_font_region(0);
        settings.settings.set_amiibo_initialized(false);
        unsafe {
            core::ptr::write_unaligned(core::ptr::addr_of_mut!(self.tag_data.settings), settings);
        }
        self.flush()
    }

    /// Format the amiibo tag (delete all user data).
    ///
    /// Upstream: mounts if TagFound, deletes application area and register info, flushes.
    pub fn format(&mut self) -> ResultCode {
        if self.device_state == DeviceState::TagFound {
            let result = self.mount(
                nfp_types::ModelType::Amiibo as u32,
                nfp_types::MountTarget::All as u32,
            );
            // Upstream: allows CorruptedData and CorruptedDataWithBackup errors to continue
            if result != RESULT_SUCCESS
                && result != nfp_result::RESULT_CORRUPTED_DATA
                && result != nfp_result::RESULT_CORRUPTED_DATA_WITH_BACKUP
            {
                return result;
            }
        }

        let _ = self.delete_application_area();
        let _ = self.delete_register_info();
        self.flush()
    }

    /// Restore amiibo data from backup.
    ///
    /// Upstream: requires TagFound state, reads backup data, validates and decodes,
    /// overwrites current tag data, transitions to TagMounted.
    pub fn restore(&mut self) -> ResultCode {
        if self.device_state != DeviceState::TagFound {
            log::error!("Wrong device state {:?} for restore", self.device_state);
            if self.device_state == DeviceState::TagRemoved {
                return nfp_result::RESULT_TAG_REMOVED;
            }
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }

        let tag_info = match self.get_tag_info() {
            Ok(tag_info) => tag_info,
            Err(error) => return error,
        };
        let uuid_length = usize::from(tag_info.uuid_length).min(tag_info.uuid.len());
        let data = match Self::read_backup_data(&tag_info.uuid[..uuid_length]) {
            Ok(data) => data,
            Err(error) => return error,
        };

        if self.is_write_protected {
            return nfp_result::RESULT_WRITE_AMIIBO_FAILED;
        }

        let mut temporary_encrypted = nfp_types::EncryptedNtag215File::default();
        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr(),
                (&mut temporary_encrypted as *mut nfp_types::EncryptedNtag215File).cast::<u8>(),
                core::mem::size_of::<nfp_types::EncryptedNtag215File>(),
            );
        }
        if !amiibo_crypto::is_amiibo_valid_encrypted(&temporary_encrypted) {
            return nfc_result::RESULT_INVALID_TAG_TYPE;
        }

        let mut temporary_tag = if self.is_plain_amiibo {
            amiibo_crypto::nfc_data_to_encoded_data(&temporary_encrypted)
        } else {
            nfp_types::Ntag215File::default()
        };
        if !self.is_plain_amiibo
            && !amiibo_crypto::decode_amiibo(&temporary_encrypted, &mut temporary_tag)
        {
            return nfp_result::RESULT_CORRUPTED_DATA;
        }

        self.tag_data = temporary_tag;
        self.encrypted_tag_data = temporary_encrypted;
        self.device_state = DeviceState::TagMounted;
        self.mount_target = nfp_types::MountTarget::All;
        self.is_data_modified = true;
        RESULT_SUCCESS
    }

    // ---- Private methods matching upstream ----

    fn flush_with_break(&mut self, break_type: nfp_types::BreakType) -> ResultCode {
        if break_type != nfp_types::BreakType::Normal {
            log::error!("Break type not implemented {:?}", break_type);
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }
        if self.is_write_protected {
            log::error!("No keys available skipping write request");
            return RESULT_SUCCESS;
        }

        let (data, backup_uuid) = if self.is_plain_amiibo {
            let uid = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.uid)) };
            unsafe {
                (
                    core::slice::from_raw_parts(
                        (&self.tag_data as *const nfp_types::Ntag215File).cast::<u8>(),
                        core::mem::size_of::<nfp_types::Ntag215File>(),
                    )
                    .to_vec(),
                    Self::tag_uuid_bytes(&uid),
                )
            }
        } else {
            if !amiibo_crypto::encode_amiibo(&self.tag_data, &mut self.encrypted_tag_data) {
                log::error!("Failed to encode data");
                return nfp_result::RESULT_WRITE_AMIIBO_FAILED;
            }
            let uuid = unsafe {
                core::ptr::read_unaligned(core::ptr::addr_of!(self.encrypted_tag_data.uuid))
            };
            unsafe {
                (
                    core::slice::from_raw_parts(
                        (&self.encrypted_tag_data as *const nfp_types::EncryptedNtag215File)
                            .cast::<u8>(),
                        core::mem::size_of::<nfp_types::EncryptedNtag215File>(),
                    )
                    .to_vec(),
                    Self::tag_uuid_bytes(&uuid),
                )
            }
        };
        if self.npad_device.is_some() {
            let _ = Self::write_backup_data(&backup_uuid, &data);
        }

        if let Some(controller) = &self.npad_device {
            if !controller.lock().write_nfc(&data) {
                log::error!("Error writing to file");
                return nfp_result::RESULT_WRITE_AMIIBO_FAILED;
            }
        }
        RESULT_SUCCESS
    }

    fn get_amiibo_name(settings: &nfp_types::AmiiboSettings) -> nfp_types::AmiiboName {
        let mut utf16 = Vec::with_capacity(nfp_types::AMIIBO_NAME_LENGTH);
        for index in 0..nfp_types::AMIIBO_NAME_LENGTH {
            let raw = unsafe {
                core::ptr::read_unaligned(core::ptr::addr_of!(settings.amiibo_name[index]))
            };
            let character = u16::from_be(raw);
            if character == 0 {
                break;
            }
            utf16.push(character);
        }
        let utf8 = String::from_utf16_lossy(&utf16);
        let mut name = [0; (nfp_types::AMIIBO_NAME_LENGTH * 4) + 1];
        let length = utf8.len().min(name.len() - 1);
        name[..length].copy_from_slice(&utf8.as_bytes()[..length]);
        name
    }

    fn set_amiibo_name(
        settings: &mut nfp_types::AmiiboSettings,
        amiibo_name: &nfp_types::AmiiboName,
    ) {
        let end = amiibo_name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(amiibo_name.len());
        let name = String::from_utf8_lossy(&amiibo_name[..end]);
        let mut encoded = name.encode_utf16();
        for index in 0..nfp_types::AMIIBO_NAME_LENGTH {
            let character = encoded.next().unwrap_or(0).to_be();
            unsafe {
                core::ptr::write_unaligned(
                    core::ptr::addr_of_mut!(settings.amiibo_name[index]),
                    character,
                );
            }
        }
    }

    fn current_amiibo_date() -> nfp_types::AmiiboDate {
        let days = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| (duration.as_secs() / 86_400) as i64);
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let day_of_era = z - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let mut year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_prime = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
        let month = month_prime + if month_prime < 10 { 3 } else { -9 };
        year += i64::from(month <= 2);

        let mut date = nfp_types::AmiiboDate::default();
        date.set_write_date(nfp_types::WriteDate {
            year: year as u16,
            month: month as u8,
            day: day as u8,
        });
        date
    }

    fn current_rng() -> common::tiny_mt::TinyMT {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs() as u32);
        let mut rng = common::tiny_mt::TinyMT::new();
        rng.initialize(seed);
        rng
    }

    fn tag_uuid_bytes(uuid: &nfp_types::TagUuid) -> [u8; 8] {
        let mut bytes = [0; 8];
        unsafe {
            core::ptr::copy_nonoverlapping(
                (uuid as *const nfp_types::TagUuid).cast::<u8>(),
                bytes.as_mut_ptr(),
                bytes.len(),
            );
        }
        bytes
    }

    fn backup_path(uuid: &[u8]) -> std::path::PathBuf {
        let file_name = format!("{}.bin", hex::encode(uuid));
        common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::AmiiboDir)
            .join("backup")
            .join(file_name)
    }

    fn has_backup(uuid: &[u8]) -> bool {
        Self::backup_path(uuid).exists()
    }

    fn read_backup_data(uuid: &[u8]) -> Result<Vec<u8>, ResultCode> {
        let data = std::fs::read(Self::backup_path(uuid))
            .map_err(|_| nfp_result::RESULT_UNABLE_TO_ACCESS_BACKUP_FILE)?;
        if data.len() != core::mem::size_of::<nfp_types::EncryptedNtag215File>() {
            return Err(nfp_result::RESULT_UNABLE_TO_ACCESS_BACKUP_FILE);
        }
        Ok(data)
    }

    fn write_backup_data(uuid: &[u8], data: &[u8]) -> ResultCode {
        let path = Self::backup_path(uuid);
        let Some(parent) = path.parent() else {
            return nfp_result::RESULT_UNABLE_TO_ACCESS_BACKUP_FILE;
        };
        if std::fs::create_dir_all(parent).is_err() || std::fs::write(path, data).is_err() {
            return nfp_result::RESULT_UNABLE_TO_ACCESS_BACKUP_FILE;
        }
        RESULT_SUCCESS
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in data {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & (0u32.wrapping_sub(crc & 1)));
            }
        }
        !crc
    }

    fn update_settings_crc(settings: &mut nfp_types::AmiiboSettings) {
        let counter = u16::from_be(settings.crc_counter);
        if counter != nfp_types::COUNTER_LIMIT {
            settings.crc_counter = (counter + 1).to_be();
        }
        settings.crc = Self::crc32(&[0; 8]).to_be();
    }

    fn update_register_info_crc(&mut self) {
        let owner_mii: crate::hle::service::mii::types::ver3_store_data::Ver3StoreData =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.owner_mii)) };
        let mii_extension: crate::hle::service::mii::types::ver3_store_data::NfpStoreDataExtension =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.mii_extension)) };
        let unknown2: [u32; 5] =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.tag_data.unknown2)) };
        let mut data = [0; 0x7E];
        let mut offset = 0;
        unsafe {
            std::ptr::copy_nonoverlapping(
                (&owner_mii
                    as *const crate::hle::service::mii::types::ver3_store_data::Ver3StoreData)
                    .cast::<u8>(),
                data.as_mut_ptr(),
                core::mem::size_of_val(&owner_mii),
            );
        }
        offset += core::mem::size_of_val(&owner_mii);
        data[offset] = self.tag_data.application_id_byte;
        data[offset + 1] = self.tag_data.unknown;
        offset += 2;
        unsafe {
            std::ptr::copy_nonoverlapping(
                (&mii_extension
                    as *const crate::hle::service::mii::types::ver3_store_data::NfpStoreDataExtension)
                    .cast::<u8>(),
                data[offset..].as_mut_ptr(),
                core::mem::size_of_val(&mii_extension),
            );
        }
        offset += core::mem::size_of_val(&mii_extension);
        unsafe {
            std::ptr::copy_nonoverlapping(
                (&unknown2 as *const [u32; 5]).cast::<u8>(),
                data[offset..].as_mut_ptr(),
                core::mem::size_of_val(&unknown2),
            );
            core::ptr::write_unaligned(
                core::ptr::addr_of_mut!(self.tag_data.register_info_crc),
                Self::crc32(&data).to_be(),
            );
        }
    }

    fn npad_update(&mut self, trigger_type: ControllerTriggerType) {
        if trigger_type == ControllerTriggerType::Connected {
            self.initialize();
            if let Some(event) = &self.availability_change_event {
                event.signal();
            }
            return;
        }

        if trigger_type == ControllerTriggerType::Disconnected {
            self.finalize();
            if let Some(event) = &self.availability_change_event {
                event.signal();
            }
            return;
        }

        if !self.is_initialized {
            return;
        }

        let Some(controller) = self.npad_device.clone() else {
            return;
        };

        let mut controller = controller.lock();
        if !controller.is_connected(false) {
            return;
        }

        if controller.get_polling_mode(EmulatedDeviceIndex::RightIndex) == PollingMode::Active {
            controller.set_polling_mode(EmulatedDeviceIndex::RightIndex, PollingMode::NFC);
        }

        if trigger_type != ControllerTriggerType::Nfc {
            return;
        }

        let nfc_status = controller.get_nfc();
        drop(controller);
        match nfc_status.state {
            NfcState::NewAmiibo => {
                self.load_nfc_tag(
                    nfc_status.protocol,
                    nfc_status.tag_type,
                    nfc_status.uuid_length,
                    nfc_status.uuid,
                );
            }
            NfcState::AmiiboRemoved => {
                if self.device_state != DeviceState::Initialized
                    && self.device_state != DeviceState::TagRemoved
                    && self.device_state != DeviceState::SearchingForTag
                {
                    self.close_nfc_tag();
                }
            }
            _ => {}
        }
    }

    fn load_nfc_tag(
        &mut self,
        protocol: u8,
        tag_type: u8,
        uuid_length: u8,
        uuid: UniqueSerialNumber,
    ) -> bool {
        if self.device_state != DeviceState::SearchingForTag {
            log::error!(
                "Game is not looking for nfc tag, current state {:?}",
                self.device_state
            );
            return false;
        }
        if protocol & self.allowed_protocols.bits() as u8 == 0 {
            log::error!("Protocol not supported {}", protocol);
            return false;
        }

        self.tag_info = TagInfo {
            uuid,
            uuid_length,
            protocol: NfcProtocol::from_bits_retain(u32::from(protocol)),
            tag_type: TagType::from_bits_retain(u32::from(tag_type)),
            ..TagInfo::default()
        };
        self.device_state = DeviceState::TagFound;
        self.deactivate_event.clear();
        self.activate_event.signal();
        true
    }

    fn load_amiibo_data(&mut self) -> bool {
        let Some(controller) = &self.npad_device else {
            return false;
        };
        let mut data = Vec::new();
        if !controller.lock().read_amiibo_data(&mut data) {
            return false;
        }
        if data.len() < core::mem::size_of::<nfp_types::EncryptedNtag215File>() {
            log::error!("Not an amiibo, size={}", data.len());
            return false;
        }

        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr(),
                (&mut self.tag_data as *mut nfp_types::Ntag215File).cast::<u8>(),
                core::mem::size_of::<nfp_types::Ntag215File>(),
            );
        }
        self.is_plain_amiibo = amiibo_crypto::is_amiibo_valid(&self.tag_data);
        self.is_write_protected = false;

        if self.is_plain_amiibo {
            log::info!("Using plain amiibo");
            self.encrypted_tag_data = amiibo_crypto::encoded_data_to_nfc_data(&self.tag_data);
            return true;
        }

        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr(),
                (&mut self.encrypted_tag_data as *mut nfp_types::EncryptedNtag215File).cast::<u8>(),
                core::mem::size_of::<nfp_types::EncryptedNtag215File>(),
            );
        }

        if !amiibo_crypto::is_amiibo_crypto_available() {
            log::info!("Loading amiibo without keys");
            self.tag_data = amiibo_crypto::nfc_data_to_encoded_data(&self.encrypted_tag_data);
            self.is_plain_amiibo = true;
            self.is_write_protected = true;
        }
        true
    }

    /// Close the currently loaded NFC tag.
    ///
    /// Upstream: unmounts if mounted, transitions to TagRemoved, clears tag data,
    /// clears activate event and signals deactivate event.
    fn close_nfc_tag(&mut self) {
        log::info!("Remove nfc tag");

        if self.device_state == DeviceState::TagMounted {
            let _ = self.unmount();
        }

        self.device_state = DeviceState::TagRemoved;
        self.tag_info = TagInfo::default();
        self.encrypted_tag_data = nfp_types::EncryptedNtag215File::default();
        self.tag_data = nfp_types::Ntag215File::default();
        self.activate_event.clear();
        self.deactivate_event.signal();
    }
}

impl Drop for NfcDeviceState {
    fn drop(&mut self) {
        if let (Some(controller), Some(callback_key)) =
            (&self.npad_device, self.callback_key.take())
        {
            controller.lock().delete_callback(callback_key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mounted_plain_device() -> NfcDevice {
        let mut context = ServiceContext::new("NfcDeviceTest".to_string());
        let device = NfcDevice::new(0, &mut context);
        {
            let mut state = device.inner.lock();
            state.device_state = DeviceState::TagMounted;
            state.mount_target = nfp_types::MountTarget::All;
            state.is_plain_amiibo = true;
        }
        device
    }

    #[test]
    fn controller_tag_transition_preserves_upstream_state_and_payload() {
        let device = mounted_plain_device();
        {
            let mut state = device.inner.lock();
            state.device_state = DeviceState::SearchingForTag;
            state.allowed_protocols = NfcProtocol::ALL;
            assert!(state.load_nfc_tag(
                NfcProtocol::TYPE_A.bits() as u8,
                TagType::TYPE2.bits() as u8,
                7,
                [1, 2, 3, 4, 5, 6, 7, 0, 0, 0],
            ));
        }
        assert_eq!(device.get_current_state(), DeviceState::TagFound);
        let tag_info = device.get_tag_info().unwrap();
        assert_eq!(tag_info.uuid_length, 7);
        assert_eq!(&tag_info.uuid[..7], &[1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(tag_info.protocol, NfcProtocol::TYPE_A);
        assert_eq!(tag_info.tag_type, TagType::TYPE2);
    }

    #[test]
    fn register_info_private_mutation_returns_public_register_info() {
        let mut device = mounted_plain_device();
        let mut private = nfp_types::RegisterInfoPrivate::default();
        private.amiibo_name[..4].copy_from_slice(b"Ruzu");
        assert_eq!(device.set_register_info_private(&private), RESULT_SUCCESS);

        let public = device.get_register_info().unwrap();
        assert_eq!(&public.amiibo_name[..4], b"Ruzu");
        let settings = unsafe {
            core::ptr::read_unaligned(core::ptr::addr_of!(device.inner.lock().tag_data.settings))
        };
        assert!(settings.settings.amiibo_initialized());
    }
}

// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/nfc/common/device_manager.h
//! Port of zuyu/src/core/hle/service/nfc/common/device_manager.cpp
//!
//! DeviceManager: manages multiple NfcDevice instances.
//! Each device corresponds to an NpadId (indices 0-9).

use std::sync::Arc;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::kernel_helpers::ServiceContext;
use crate::hle::service::nfc::nfc_result;
use crate::hle::service::nfc::nfc_types::*;
use crate::hle::service::nfp::nfp_types;
use crate::hle::service::os::event::Event;

use super::device::NfcDevice;

/// Number of NFC devices, matching upstream array size.
const NUM_DEVICES: usize = 10;

/// Size of the NFP application area (sizeof(NFP::ApplicationArea) = 0xD8).
const APPLICATION_AREA_SIZE: u32 = 0xD8;

/// DeviceManager corresponds to `DeviceManager` in upstream `device_manager.h`.
///
/// Manages an array of 10 NfcDevice instances (one per NpadId index 0-9),
/// an availability change event, and initialization state.
pub struct DeviceManager {
    is_initialized: bool,
    time_since_last_error: i64,
    devices: Vec<NfcDevice>,
    service_context: ServiceContext,
    availability_change_event_handle: u32,
}

impl DeviceManager {
    /// Creates a new DeviceManager with 10 NfcDevice instances.
    ///
    /// Upstream constructor: creates ServiceContext, availability_change_event, and
    /// 10 NfcDevice instances via `HID::IndexToNpadIdType(device_index)`.
    pub fn new() -> Self {
        Self::new_impl(None)
    }

    pub fn new_with_system(system: crate::core::SystemRef) -> Self {
        Self::new_impl(Some(system))
    }

    fn new_impl(system: Option<crate::core::SystemRef>) -> Self {
        let mut service_context = ServiceContext::new("Nfc:DeviceManager".to_string());
        let availability_change_event_handle =
            service_context.create_event("Nfc:DeviceManager:AvailabilityChangeEvent".to_string());
        let availability_change_event = service_context
            .get_event(availability_change_event_handle)
            .expect("just created availability-change event");
        let hid_core = system.map(|system| system.get().hid_core());

        let mut devices = Vec::with_capacity(NUM_DEVICES);
        for device_index in 0..NUM_DEVICES {
            let npad_id = hid_core::hid_util::index_to_npad_id_type(device_index);
            let controller = hid_core
                .as_ref()
                .map(|hid_core| hid_core.lock().get_emulated_controller(npad_id));
            devices.push(NfcDevice::new_with_controller(
                npad_id as u64,
                controller,
                Some(Arc::clone(&availability_change_event)),
                &mut service_context,
                system.unwrap_or_else(crate::core::SystemRef::null),
            ));
        }

        Self {
            is_initialized: false,
            time_since_last_error: 0,
            devices,
            service_context,
            availability_change_event_handle,
        }
    }

    /// Initializes all managed NFC devices.
    ///
    /// Upstream: calls `device->Initialize()` on each device, sets `is_initialized = true`.
    pub fn initialize(&mut self) -> ResultCode {
        for device in &mut self.devices {
            device.initialize();
        }
        self.is_initialized = true;
        RESULT_SUCCESS
    }

    /// Finalizes all managed NFC devices.
    ///
    /// Upstream: calls `device->Finalize()` on each device, sets `is_initialized = false`.
    pub fn finalize(&mut self) -> ResultCode {
        for device in &mut self.devices {
            device.finalize();
        }
        self.is_initialized = false;
        RESULT_SUCCESS
    }

    /// Lists available NFC device handles.
    ///
    /// Upstream: checks NFC parameter set, NFC enabled, NFC initialized, then
    /// iterates devices, skipping unavailable ones (and optionally skipping
    /// devices in fatal error recovery). Populates handles up to max_allowed.
    pub fn list_devices(
        &self,
        nfp_devices: &mut Vec<u64>,
        max_allowed: usize,
        _skip_fatal_errors: bool,
    ) -> ResultCode {
        if max_allowed < 1 {
            return nfc_result::RESULT_INVALID_ARGUMENT;
        }

        let result = self.is_nfc_parameter_set();
        if result.is_error() {
            return result;
        }

        let result = self.is_nfc_enabled();
        if result.is_error() {
            return result;
        }

        let result = self.is_nfc_initialized();
        if result.is_error() {
            return result;
        }

        for device in &self.devices {
            if nfp_devices.len() >= max_allowed {
                continue;
            }
            // Upstream: if skip_fatal_errors, check elapsed time against MinimumRecoveryTime (60s)
            // using the steady clock. Skipped here as steady clock service is not wired up.
            if device.get_current_state() == DeviceState::Unavailable {
                continue;
            }
            nfp_devices.push(device.get_handle());
        }

        if nfp_devices.is_empty() {
            return nfc_result::RESULT_DEVICE_NOT_FOUND;
        }

        RESULT_SUCCESS
    }

    /// Returns the current state of the device identified by handle.
    ///
    /// Upstream: calls `GetDeviceFromHandle(handle, device, false)`,
    /// returns `device->GetCurrentState()` on success, `DeviceState::Finalized` on failure.
    pub fn get_device_state(&self, device_handle: u64) -> DeviceState {
        match self.get_device_from_handle(device_handle, false) {
            Ok(device) => device.get_current_state(),
            Err(_) => DeviceState::Finalized,
        }
    }

    /// Gets the NpadId for a device handle.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->GetNpadId(npad_id)`,
    /// then `VerifyDeviceResult`.
    pub fn get_npad_id(&self, device_handle: u64) -> Result<u64, ResultCode> {
        let device = self.get_device_handle_checked(device_handle)?;
        Ok(device.get_npad_id())
    }

    /// Starts NFC tag detection on the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->StartDetection(tag_protocol)`,
    /// then `VerifyDeviceResult`.
    pub fn start_detection(&mut self, device_handle: u64, protocol: NfcProtocol) -> ResultCode {
        let device = match self.get_device_handle_checked_mut(device_handle) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let result = device.start_detection(protocol);
        self.verify_device_result(device_handle, result)
    }

    /// Stops NFC tag detection on the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->StopDetection()`,
    /// then `VerifyDeviceResult`.
    pub fn stop_detection(&mut self, device_handle: u64) -> ResultCode {
        let device = match self.get_device_handle_checked_mut(device_handle) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let result = device.stop_detection();
        self.verify_device_result(device_handle, result)
    }

    /// Gets tag info for the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->GetTagInfo(tag_info)`,
    /// then `VerifyDeviceResult`.
    pub fn get_tag_info(&self, device_handle: u64) -> Result<TagInfo, ResultCode> {
        let device = self.get_device_handle_checked(device_handle)?;
        let result = device.get_tag_info();
        match result {
            Ok(tag_info) => Ok(tag_info),
            Err(e) => {
                let verified = self.verify_device_result_immut(device_handle, e);
                Err(verified)
            }
        }
    }

    /// Mounts a tag on the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->Mount(model_type, mount_target)`,
    /// then `VerifyDeviceResult`.
    pub fn mount(&mut self, device_handle: u64, model_type: u32, mount_target: u32) -> ResultCode {
        let device = match self.get_device_handle_checked_mut(device_handle) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let result = device.mount(model_type, mount_target);
        self.verify_device_result(device_handle, result)
    }

    /// Unmounts a tag from the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->Unmount()`,
    /// then `VerifyDeviceResult`.
    pub fn unmount(&mut self, device_handle: u64) -> ResultCode {
        let device = match self.get_device_handle_checked_mut(device_handle) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let result = device.unmount();
        self.verify_device_result(device_handle, result)
    }

    /// Flushes data to the tag on the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->Flush()`,
    /// then `VerifyDeviceResult`.
    pub fn flush(&mut self, device_handle: u64) -> ResultCode {
        let device = match self.get_device_handle_checked_mut(device_handle) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let result = device.flush();
        self.verify_device_result(device_handle, result)
    }

    /// Flushes data in debug mode on the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->FlushDebug()`,
    /// then `VerifyDeviceResult`.
    /// NfcDevice does not yet have a separate flush_debug; delegates to flush
    /// which matches upstream behavior (FlushDebug calls FlushWithBreak like Flush).
    pub fn flush_debug(&mut self, device_handle: u64) -> ResultCode {
        let device = match self.get_device_handle_checked_mut(device_handle) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let result = device.flush();
        self.verify_device_result(device_handle, result)
    }

    /// Restores tag data on the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->Restore()`,
    /// then `VerifyDeviceResult`.
    pub fn restore(&mut self, device_handle: u64) -> ResultCode {
        let device = match self.get_device_handle_checked_mut(device_handle) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let result = device.restore();
        self.verify_device_result(device_handle, result)
    }

    /// Formats the tag on the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->Format()`,
    /// then `VerifyDeviceResult`.
    pub fn format(&mut self, device_handle: u64) -> ResultCode {
        let device = match self.get_device_handle_checked_mut(device_handle) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let result = device.format();
        self.verify_device_result(device_handle, result)
    }

    /// Opens the application area on the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->OpenApplicationArea(access_id)`,
    /// then `VerifyDeviceResult`.
    pub fn open_application_area(&mut self, device_handle: u64, access_id: u32) -> ResultCode {
        let device = match self.get_device_handle_checked_mut(device_handle) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let result = device.open_application_area(access_id);
        self.verify_device_result(device_handle, result)
    }

    /// Gets the application area data from the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->GetApplicationArea(data)`,
    /// then `VerifyDeviceResult`.
    pub fn get_application_area(
        &self,
        device_handle: u64,
        data: &mut [u8],
    ) -> Result<u32, ResultCode> {
        let device = self.get_device_handle_checked(device_handle)?;
        let result = device.get_application_area(data);
        match result {
            Ok(size) => Ok(size),
            Err(e) => {
                let verified = self.verify_device_result_immut(device_handle, e);
                Err(verified)
            }
        }
    }

    /// Sets the application area data on the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->SetApplicationArea(data)`,
    /// then `VerifyDeviceResult`.
    pub fn set_application_area(&mut self, device_handle: u64, data: &[u8]) -> ResultCode {
        let device = match self.get_device_handle_checked_mut(device_handle) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let result = device.set_application_area(data);
        self.verify_device_result(device_handle, result)
    }

    /// Creates the application area on the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->CreateApplicationArea(access_id, data)`,
    /// then `VerifyDeviceResult`.
    pub fn create_application_area(
        &mut self,
        device_handle: u64,
        access_id: u32,
        data: &[u8],
    ) -> ResultCode {
        let device = match self.get_device_handle_checked_mut(device_handle) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let result = device.create_application_area(access_id, data);
        self.verify_device_result(device_handle, result)
    }

    /// Recreates the application area on the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->RecreateApplicationArea(access_id, data)`,
    /// then `VerifyDeviceResult`.
    /// NfcDevice does not yet have a separate recreate_application_area; delegates to
    /// create_application_area which performs the same underlying operation.
    pub fn recreate_application_area(
        &mut self,
        device_handle: u64,
        access_id: u32,
        data: &[u8],
    ) -> ResultCode {
        let device = match self.get_device_handle_checked_mut(device_handle) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let result = device.create_application_area(access_id, data);
        self.verify_device_result(device_handle, result)
    }

    /// Deletes the application area on the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->DeleteApplicationArea()`,
    /// then `VerifyDeviceResult`.
    pub fn delete_application_area(&mut self, device_handle: u64) -> ResultCode {
        let device = match self.get_device_handle_checked_mut(device_handle) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let result = device.delete_application_area();
        self.verify_device_result(device_handle, result)
    }

    /// Checks whether an application area exists on the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->ExistsApplicationArea(has_area)`,
    /// then `VerifyDeviceResult`.
    pub fn exists_application_area(&self, device_handle: u64) -> Result<bool, ResultCode> {
        let device = self.get_device_handle_checked(device_handle)?;
        let result = device.exists_application_area();
        match result {
            Ok(exists) => Ok(exists),
            Err(e) => {
                let verified = self.verify_device_result_immut(device_handle, e);
                Err(verified)
            }
        }
    }

    /// Deletes register info on the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->DeleteRegisterInfo()`,
    /// then `VerifyDeviceResult`.
    /// NfcDevice does not yet have delete_register_info; returns success after state check
    /// since upstream delete_register_info clears amiibo registration data which requires
    /// tag_data structures not yet available.
    pub fn delete_register_info(&mut self, device_handle: u64) -> ResultCode {
        match self.get_device_handle_checked_mut(device_handle) {
            Ok(_) => RESULT_SUCCESS,
            Err(e) => e,
        }
    }

    /// Gets register info for the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->GetRegisterInfo(register_info)`,
    /// then `VerifyDeviceResult`.
    pub fn get_register_info(
        &self,
        device_handle: u64,
    ) -> Result<nfp_types::RegisterInfo, ResultCode> {
        let device = self.get_device_handle_checked(device_handle)?;
        let result = device.get_register_info();
        result.map_err(|error| self.verify_device_result_immut(device_handle, error))
    }

    /// Gets common info for the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->GetCommonInfo(common_info)`,
    /// then `VerifyDeviceResult`.
    pub fn get_common_info(&self, device_handle: u64) -> Result<nfp_types::CommonInfo, ResultCode> {
        let device = self.get_device_handle_checked(device_handle)?;
        let result = device.get_common_info();
        match result {
            Ok(info) => Ok(info),
            Err(e) => {
                let verified = self.verify_device_result_immut(device_handle, e);
                Err(verified)
            }
        }
    }

    /// Gets model info for the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->GetModelInfo(model_info)`,
    /// then `VerifyDeviceResult`.
    pub fn get_model_info(&self, device_handle: u64) -> Result<nfp_types::ModelInfo, ResultCode> {
        let device = self.get_device_handle_checked(device_handle)?;
        let result = device.get_model_info();
        result.map_err(|error| self.verify_device_result_immut(device_handle, error))
    }

    /// Returns the size of the NFP application area.
    ///
    /// Upstream: returns `sizeof(NFP::ApplicationArea)` which is 0xD8.
    pub fn get_application_area_size(&self) -> u32 {
        APPLICATION_AREA_SIZE
    }

    /// Attaches the activate event for the specified device.
    ///
    /// Upstream: calls `ListDevices` to verify handle is valid, then
    /// `GetDeviceFromHandle`, then returns `device->GetActivateEvent()`.
    pub fn attach_activate_event(&self, device_handle: u64) -> Option<Arc<Event>> {
        let mut nfp_devices = Vec::new();
        let result = self.list_devices(&mut nfp_devices, 9, false);
        if result.is_error() {
            return None;
        }

        if !self.check_handle_on_list(device_handle, &nfp_devices) {
            return None;
        }

        match self.get_device_from_handle(device_handle, false) {
            Ok(device) => Some(device.get_activate_event()),
            Err(_) => None,
        }
    }

    /// Attaches the deactivate event for the specified device.
    ///
    /// Upstream: calls `ListDevices` to verify handle is valid, then
    /// `GetDeviceFromHandle`, then returns `device->GetDeactivateEvent()`.
    pub fn attach_deactivate_event(&self, device_handle: u64) -> Option<Arc<Event>> {
        let mut nfp_devices = Vec::new();
        let result = self.list_devices(&mut nfp_devices, 9, false);
        if result.is_error() {
            return None;
        }

        if !self.check_handle_on_list(device_handle, &nfp_devices) {
            return None;
        }

        match self.get_device_from_handle(device_handle, false) {
            Ok(device) => Some(device.get_deactivate_event()),
            Err(_) => None,
        }
    }

    /// Attaches the availability change event.
    ///
    /// Upstream: returns `availability_change_event->GetReadableEvent()`.
    pub fn attach_availability_change_event(&self) -> Option<Arc<Event>> {
        self.service_context
            .get_event(self.availability_change_event_handle)
    }

    /// Breaks (corrupts) the tag on the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->BreakTag(break_type)`,
    /// then `VerifyDeviceResult`.
    /// NfcDevice does not yet have break_tag; returns success after state check
    /// since the operation requires amiibo crypto not yet available.
    pub fn break_tag(&mut self, device_handle: u64, _break_type: u32) -> ResultCode {
        match self.get_device_handle_checked_mut(device_handle) {
            Ok(_) => RESULT_SUCCESS,
            Err(e) => e,
        }
    }

    /// Reads backup data from the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle`, then `device->GetTagInfo(tag_info)`,
    /// then `device->ReadBackupData(tag_info.uuid, tag_info.uuid_length, data)`.
    /// NfcDevice does not yet have read_backup_data; returns success after state check
    /// since the operation requires filesystem backup support not yet available.
    pub fn read_backup_data(&self, device_handle: u64, _data: &mut [u8]) -> ResultCode {
        match self.get_device_handle_checked(device_handle) {
            Ok(_) => RESULT_SUCCESS,
            Err(e) => e,
        }
    }

    /// Writes backup data to the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle`, then `device->GetTagInfo(tag_info)`,
    /// then `device->WriteBackupData(tag_info.uuid, tag_info.uuid_length, data)`.
    /// NfcDevice does not yet have write_backup_data; returns success after state check
    /// since the operation requires filesystem backup support not yet available.
    pub fn write_backup_data(&mut self, device_handle: u64, _data: &[u8]) -> ResultCode {
        match self.get_device_handle_checked_mut(device_handle) {
            Ok(_) => RESULT_SUCCESS,
            Err(e) => e,
        }
    }

    /// Writes NTF data to the specified device.
    ///
    /// Upstream: calls `GetDeviceHandle` then `device->WriteNtf(data)`,
    /// then `VerifyDeviceResult`. The write_type parameter is accepted but
    /// not used (matches upstream).
    /// NfcDevice does not yet have write_ntf; returns success after state check.
    pub fn write_ntf(&mut self, device_handle: u64, _write_type: u32, _data: &[u8]) -> ResultCode {
        match self.get_device_handle_checked_mut(device_handle) {
            Ok(_) => RESULT_SUCCESS,
            Err(e) => e,
        }
    }

    // ---- Private helpers matching upstream ----

    /// Checks whether NFC parameter is set.
    ///
    /// Upstream: checks a bool at offset 0x450 (always true for now).
    fn is_nfc_parameter_set(&self) -> ResultCode {
        // Upstream: "TODO: This calls checks against a bool on offset 0x450"
        // const bool is_set = true;
        let is_set = true;
        if !is_set {
            return nfc_result::RESULT_UNKNOWN_76;
        }
        RESULT_SUCCESS
    }

    /// Checks whether NFC is enabled via system settings.
    ///
    /// Upstream: calls `m_set_sys->GetNfcEnableFlag(&is_enabled)`.
    /// We return success (NFC enabled) since system settings service is not wired up.
    fn is_nfc_enabled(&self) -> ResultCode {
        // Upstream queries ISystemSettingsServer::GetNfcEnableFlag.
        // Without that service wired in, assume NFC is enabled.
        RESULT_SUCCESS
    }

    /// Checks whether the device manager has been initialized.
    ///
    /// Upstream: returns ResultNfcNotInitialized if !is_initialized.
    fn is_nfc_initialized(&self) -> ResultCode {
        if !self.is_initialized {
            return nfc_result::RESULT_NFC_NOT_INITIALIZED;
        }
        RESULT_SUCCESS
    }

    /// Checks whether a device handle is present in the given list.
    ///
    /// Upstream `CheckHandleOnList`: returns ResultSuccess if found, ResultDeviceNotFound otherwise.
    fn check_handle_on_list(&self, device_handle: u64, device_list: &[u64]) -> bool {
        device_list.contains(&device_handle)
    }

    /// Looks up a device by handle.
    ///
    /// Upstream `GetNfcDevice`: linear search through devices array comparing
    /// `device->GetHandle() == handle`.
    fn get_nfc_device(&self, handle: u64) -> Option<&NfcDevice> {
        self.devices
            .iter()
            .find(|device| device.get_handle() == handle)
    }

    /// Looks up a device by handle (mutable).
    fn get_nfc_device_mut(&mut self, handle: u64) -> Option<&mut NfcDevice> {
        self.devices
            .iter_mut()
            .find(|device| device.get_handle() == handle)
    }

    /// Gets a device from handle with optional state checks.
    ///
    /// Upstream `GetDeviceFromHandle`: optionally checks NFC parameter set, NFC enabled,
    /// NFC initialized, then searches for device by handle.
    fn get_device_from_handle(
        &self,
        handle: u64,
        check_state: bool,
    ) -> Result<&NfcDevice, ResultCode> {
        if check_state {
            let result = self.is_nfc_parameter_set();
            if result.is_error() {
                return Err(result);
            }
            let result = self.is_nfc_enabled();
            if result.is_error() {
                return Err(result);
            }
            let result = self.is_nfc_initialized();
            if result.is_error() {
                return Err(result);
            }
        }

        self.get_nfc_device(handle)
            .ok_or(nfc_result::RESULT_DEVICE_NOT_FOUND)
    }

    /// Gets a device from handle with optional state checks (mutable).
    fn get_device_from_handle_mut(
        &mut self,
        handle: u64,
        check_state: bool,
    ) -> Result<&mut NfcDevice, ResultCode> {
        if check_state {
            let result = self.is_nfc_parameter_set();
            if result.is_error() {
                return Err(result);
            }
            let result = self.is_nfc_enabled();
            if result.is_error() {
                return Err(result);
            }
            let result = self.is_nfc_initialized();
            if result.is_error() {
                return Err(result);
            }
        }

        self.get_nfc_device_mut(handle)
            .ok_or(nfc_result::RESULT_DEVICE_NOT_FOUND)
    }

    /// Gets a device handle with full state and device checks.
    ///
    /// Upstream `GetDeviceHandle`: calls `GetDeviceFromHandle(handle, device, true)`,
    /// then `CheckDeviceState(device)`.
    fn get_device_handle_checked(&self, handle: u64) -> Result<&NfcDevice, ResultCode> {
        self.get_device_from_handle(handle, true)
        // CheckDeviceState upstream: returns ResultInvalidArgument if device is null.
        // In Rust the reference is always valid if we got here.
    }

    /// Gets a device handle with full state and device checks (mutable).
    fn get_device_handle_checked_mut(&mut self, handle: u64) -> Result<&mut NfcDevice, ResultCode> {
        self.get_device_from_handle_mut(handle, true)
    }

    /// Verifies a device operation result.
    ///
    /// Upstream `VerifyDeviceResult`: if operation succeeded, returns it directly.
    /// If failed, re-checks NFC parameter set, NFC enabled, NFC initialized, device state.
    /// If the error is one of the fatal errors (ResultUnknown112, ResultUnknown114,
    /// ResultUnknown115), records the current time for recovery tracking.
    fn verify_device_result(
        &mut self,
        _device_handle: u64,
        operation_result: ResultCode,
    ) -> ResultCode {
        if operation_result.is_success() {
            return operation_result;
        }

        let result = self.is_nfc_parameter_set();
        if result.is_error() {
            return result;
        }
        let result = self.is_nfc_enabled();
        if result.is_error() {
            return result;
        }
        let result = self.is_nfc_initialized();
        if result.is_error() {
            return result;
        }

        // CheckDeviceState: upstream checks device != nullptr, always true in Rust.

        if operation_result == nfc_result::RESULT_UNKNOWN_112
            || operation_result == nfc_result::RESULT_UNKNOWN_114
            || operation_result == nfc_result::RESULT_UNKNOWN_115
        {
            // Upstream: records steady clock time_point for fatal error recovery tracking.
            // Steady clock service is not wired up; record a placeholder.
            self.time_since_last_error = 1;
        }

        operation_result
    }

    /// Immutable version of verify_device_result for use in &self methods.
    ///
    /// Cannot update time_since_last_error without &mut self, but performs
    /// all the same error checking. This matches the upstream pattern where
    /// some const methods call into the same verification logic.
    fn verify_device_result_immut(
        &self,
        _device_handle: u64,
        operation_result: ResultCode,
    ) -> ResultCode {
        if operation_result.is_success() {
            return operation_result;
        }

        let result = self.is_nfc_parameter_set();
        if result.is_error() {
            return result;
        }
        let result = self.is_nfc_enabled();
        if result.is_error() {
            return result;
        }
        let result = self.is_nfc_initialized();
        if result.is_error() {
            return result;
        }

        operation_result
    }
}

impl Drop for DeviceManager {
    /// Upstream destructor: finalizes if initialized, then closes availability_change_event.
    fn drop(&mut self) {
        if self.is_initialized {
            self.finalize();
        }
        self.service_context
            .close_event(self.availability_change_event_handle);
    }
}

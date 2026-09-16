// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/nfp/nfp_interface.h
//! Port of zuyu/src/core/hle/service/nfp/nfp_interface.cpp
//!
//! Interface -- NFP interface for amiibo operations.
//! This is the concrete NFP service that extends NfcInterface (NFC base).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use super::nfp_result;
use super::nfp_types::{
    BreakType, CommonInfo, DeviceState, ModelType, MountTarget, TagInfo, WriteType,
};
use crate::hle::result::{ErrorModule, ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::nfc::common::device_manager::DeviceManager;
use crate::hle::service::nfc::nfc_result;
use crate::hle::service::nfc::nfc_types::NfcProtocol;
use crate::hle::service::service::{FunctionInfo, ServiceFramework};

pub use super::nfp::interface_commands as commands;

/// NFP State, mirroring NFC::State for the base class behavior.
///
/// Upstream NFP::Interface inherits NfcInterface which tracks this state.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    NonInitialized = 0,
    Initialized = 1,
}

/// Interface -- NFP service interface for amiibo.
///
/// Corresponds to `Interface` in upstream nfp_interface.h / nfp_interface.cpp.
/// Extends NfcInterface with NFP-specific methods (amiibo data management).
pub struct Interface {
    system: crate::core::SystemRef,
    name: String,
    state: AtomicU32,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    /// Lazily-initialized DeviceManager, matching upstream `device_manager` field
    /// in NfcInterface. Behind Mutex for interior mutability since IPC handlers
    /// receive `&self`.
    device_manager: Mutex<Option<DeviceManager>>,
}

impl Interface {
    pub fn new(system: crate::core::SystemRef, name: &str) -> Self {
        let handlers = Self::make_handlers();

        Self {
            system,
            name: name.to_string(),
            state: AtomicU32::new(State::NonInitialized as u32),
            handlers,
            handlers_tipc: BTreeMap::new(),
            device_manager: Mutex::new(None),
        }
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    // --- DeviceManager access ---
    // Upstream: NfcInterface::GetManager() lazily creates a DeviceManager.
    // Since Rust has no inheritance, we replicate that pattern here.

    /// Ensures the DeviceManager is created and returns a reference to the Mutex guard.
    /// Matches upstream `NfcInterface::GetManager()`.
    fn with_manager<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut DeviceManager) -> R,
    {
        let mut guard = self.device_manager.lock().unwrap();
        if guard.is_none() {
            *guard = Some(DeviceManager::new_with_system(self.system));
        }
        f(guard.as_mut().unwrap())
    }

    /// Translates NFC result codes to NFP-specific service error codes.
    ///
    /// Matches upstream `NfcInterface::TranslateResultToServiceError` with
    /// BackendType::Nfp.
    fn translate_result_to_service_error(result: ResultCode) -> ResultCode {
        if result.is_success() {
            return result;
        }
        if result.get_module() != ErrorModule::NFC {
            return result;
        }
        // Translate NFC results to NFP results (upstream TranslateResultToNfp)
        if result == nfc_result::RESULT_DEVICE_NOT_FOUND {
            return nfp_result::RESULT_DEVICE_NOT_FOUND;
        }
        if result == nfc_result::RESULT_INVALID_ARGUMENT {
            return nfp_result::RESULT_INVALID_ARGUMENT;
        }
        if result == nfc_result::RESULT_WRONG_APPLICATION_AREA_SIZE {
            return nfp_result::RESULT_WRONG_APPLICATION_AREA_SIZE;
        }
        if result == nfc_result::RESULT_WRONG_DEVICE_STATE {
            return nfp_result::RESULT_WRONG_DEVICE_STATE;
        }
        if result == nfc_result::RESULT_UNKNOWN_74 {
            return nfp_result::RESULT_UNKNOWN_74;
        }
        if result == nfc_result::RESULT_NFC_DISABLED {
            return nfp_result::RESULT_NFC_DISABLED;
        }
        if result == nfc_result::RESULT_NFC_NOT_INITIALIZED {
            return nfp_result::RESULT_NFC_DISABLED;
        }
        if result == nfc_result::RESULT_WRITE_AMIIBO_FAILED {
            return nfp_result::RESULT_WRITE_AMIIBO_FAILED;
        }
        if result == nfc_result::RESULT_TAG_REMOVED {
            return nfp_result::RESULT_TAG_REMOVED;
        }
        if result == nfc_result::RESULT_REGISTRATION_IS_NOT_INITIALIZED {
            return nfp_result::RESULT_REGISTRATION_IS_NOT_INITIALIZED;
        }
        if result == nfc_result::RESULT_APPLICATION_AREA_IS_NOT_INITIALIZED {
            return nfp_result::RESULT_APPLICATION_AREA_IS_NOT_INITIALIZED;
        }
        if result == nfc_result::RESULT_CORRUPTED_DATA_WITH_BACKUP {
            return nfp_result::RESULT_CORRUPTED_DATA_WITH_BACKUP;
        }
        if result == nfc_result::RESULT_CORRUPTED_DATA {
            return nfp_result::RESULT_CORRUPTED_DATA;
        }
        if result == nfc_result::RESULT_WRONG_APPLICATION_AREA_ID {
            return nfp_result::RESULT_WRONG_APPLICATION_AREA_ID;
        }
        if result == nfc_result::RESULT_APPLICATION_AREA_EXIST {
            return nfp_result::RESULT_APPLICATION_AREA_EXIST;
        }
        if result == nfc_result::RESULT_INVALID_TAG_TYPE {
            return nfp_result::RESULT_NOT_AN_AMIIBO;
        }
        if result == nfc_result::RESULT_BACKUP_PATH_ALREADY_EXIST {
            return nfp_result::RESULT_UNABLE_TO_ACCESS_BACKUP_FILE;
        }
        result
    }

    // --- Base class (NfcInterface) methods ---
    // In upstream these are inherited from NfcInterface. Since Rust has no
    // inheritance, they are implemented directly on Interface.

    /// Initialize (cmd 0).
    ///
    /// Corresponds to `NfcInterface::Initialize` in upstream nfc_interface.cpp.
    pub fn initialize(&self) -> ResultCode {
        log::info!("NFP::Interface({})::Initialize called", self.name);
        let result = self.with_manager(|mgr| mgr.initialize());
        if result.is_success() {
            self.state
                .store(State::Initialized as u32, Ordering::Relaxed);
        } else {
            self.with_manager(|mgr| {
                mgr.finalize();
            });
        }
        result
    }

    /// Finalize (cmd 1).
    ///
    /// Corresponds to `NfcInterface::Finalize` in upstream nfc_interface.cpp.
    pub fn finalize(&self) -> ResultCode {
        log::info!("NFP::Interface({})::Finalize called", self.name);
        if self.state.load(Ordering::Relaxed) != State::NonInitialized as u32 {
            self.with_manager(|mgr| {
                mgr.finalize();
            });
            // Upstream: device_manager = nullptr; drop the manager
            *self.device_manager.lock().unwrap() = None;
            self.state
                .store(State::NonInitialized as u32, Ordering::Relaxed);
        }
        RESULT_SUCCESS
    }

    /// ListDevices (cmd 2).
    ///
    /// Corresponds to `NfcInterface::ListDevices` in upstream nfc_interface.cpp.
    pub fn list_devices(&self, max_allowed: usize) -> (ResultCode, Vec<u64>) {
        log::debug!("NFP::ListDevices called");
        let mut devices = Vec::new();
        let result = self.with_manager(|mgr| mgr.list_devices(&mut devices, max_allowed, true));
        let result = Self::translate_result_to_service_error(result);
        (result, devices)
    }

    /// StartDetection (cmd 3).
    ///
    /// Corresponds to `NfcInterface::StartDetection` in upstream nfc_interface.cpp.
    /// For NFP backend, tag_protocol is always NfcProtocol::All (no rp.PopEnum).
    pub fn start_detection(&self, device_handle: u64) -> ResultCode {
        log::info!(
            "NFP::StartDetection called, device_handle={}",
            device_handle
        );
        let result = self.with_manager(|mgr| mgr.start_detection(device_handle, NfcProtocol::ALL));
        Self::translate_result_to_service_error(result)
    }

    /// StopDetection (cmd 4).
    ///
    /// Corresponds to `NfcInterface::StopDetection` in upstream nfc_interface.cpp.
    pub fn stop_detection(&self, device_handle: u64) -> ResultCode {
        log::info!("NFP::StopDetection called, device_handle={}", device_handle);
        let result = self.with_manager(|mgr| mgr.stop_detection(device_handle));
        Self::translate_result_to_service_error(result)
    }

    /// Mount (cmd 5).
    ///
    /// Corresponds to `Interface::Mount` in upstream nfp_interface.cpp.
    pub fn mount(
        &self,
        device_handle: u64,
        model_type: ModelType,
        mount_target: MountTarget,
    ) -> ResultCode {
        log::info!(
            "NFP::Mount called, device_handle={}, model_type={:?}, mount_target={:?}",
            device_handle,
            model_type,
            mount_target
        );
        let result = self
            .with_manager(|mgr| mgr.mount(device_handle, model_type as u32, mount_target as u32));
        Self::translate_result_to_service_error(result)
    }

    /// Unmount (cmd 6).
    ///
    /// Corresponds to `Interface::Unmount` in upstream nfp_interface.cpp.
    pub fn unmount(&self, device_handle: u64) -> ResultCode {
        log::info!("NFP::Unmount called, device_handle={}", device_handle);
        let result = self.with_manager(|mgr| mgr.unmount(device_handle));
        Self::translate_result_to_service_error(result)
    }

    /// OpenApplicationArea (cmd 7).
    ///
    /// Corresponds to `Interface::OpenApplicationArea` in upstream nfp_interface.cpp.
    pub fn open_application_area(&self, device_handle: u64, access_id: u32) -> ResultCode {
        log::info!(
            "NFP::OpenApplicationArea called, device_handle={}, access_id={}",
            device_handle,
            access_id
        );
        let result = self.with_manager(|mgr| mgr.open_application_area(device_handle, access_id));
        Self::translate_result_to_service_error(result)
    }

    /// GetApplicationArea (cmd 8).
    ///
    /// Corresponds to `Interface::GetApplicationArea` in upstream nfp_interface.cpp.
    pub fn get_application_area(&self, device_handle: u64, data: &mut [u8]) -> (ResultCode, u32) {
        log::info!(
            "NFP::GetApplicationArea called, device_handle={}",
            device_handle
        );
        let result = self.with_manager(|mgr| mgr.get_application_area(device_handle, data));
        match result {
            Ok(size) => (RESULT_SUCCESS, size),
            Err(e) => (Self::translate_result_to_service_error(e), 0),
        }
    }

    /// SetApplicationArea (cmd 9).
    ///
    /// Corresponds to `Interface::SetApplicationArea` in upstream nfp_interface.cpp.
    pub fn set_application_area(&self, device_handle: u64, data: &[u8]) -> ResultCode {
        log::info!(
            "NFP::SetApplicationArea called, device_handle={}, data_size={}",
            device_handle,
            data.len()
        );
        let result = self.with_manager(|mgr| mgr.set_application_area(device_handle, data));
        Self::translate_result_to_service_error(result)
    }

    /// Flush (cmd 10).
    ///
    /// Corresponds to `Interface::Flush` in upstream nfp_interface.cpp.
    pub fn flush(&self, device_handle: u64) -> ResultCode {
        log::info!("NFP::Flush called, device_handle={}", device_handle);
        let result = self.with_manager(|mgr| mgr.flush(device_handle));
        Self::translate_result_to_service_error(result)
    }

    /// Restore (cmd 11).
    ///
    /// Corresponds to `Interface::Restore` in upstream nfp_interface.cpp.
    pub fn restore(&self, device_handle: u64) -> ResultCode {
        log::info!("NFP::Restore called, device_handle={}", device_handle);
        let result = self.with_manager(|mgr| mgr.restore(device_handle));
        Self::translate_result_to_service_error(result)
    }

    /// CreateApplicationArea (cmd 12).
    ///
    /// Corresponds to `Interface::CreateApplicationArea` in upstream nfp_interface.cpp.
    pub fn create_application_area(
        &self,
        device_handle: u64,
        access_id: u32,
        data: &[u8],
    ) -> ResultCode {
        log::info!(
            "NFP::CreateApplicationArea called, device_handle={}, access_id={}, data_size={}",
            device_handle,
            access_id,
            data.len()
        );
        let result =
            self.with_manager(|mgr| mgr.create_application_area(device_handle, access_id, data));
        Self::translate_result_to_service_error(result)
    }

    /// GetTagInfo (cmd 13).
    ///
    /// Corresponds to `NfcInterface::GetTagInfo` in upstream nfc_interface.cpp.
    pub fn get_tag_info(&self, device_handle: u64) -> (ResultCode, TagInfo) {
        log::info!("NFP::GetTagInfo called, device_handle={}", device_handle);
        let result = self.with_manager(|mgr| mgr.get_tag_info(device_handle));
        match result {
            Ok(tag_info) => (RESULT_SUCCESS, tag_info),
            Err(e) => (
                Self::translate_result_to_service_error(e),
                TagInfo::default(),
            ),
        }
    }

    /// GetState (cmd 19).
    ///
    /// Corresponds to `NfcInterface::GetState` in upstream nfc_interface.cpp.
    pub fn get_state(&self) -> State {
        log::debug!("NFP::GetState called");
        let raw = self.state.load(Ordering::Relaxed);
        match raw {
            0 => State::NonInitialized,
            _ => State::Initialized,
        }
    }

    /// GetDeviceState (cmd 20).
    ///
    /// Corresponds to `NfcInterface::GetDeviceState` in upstream nfc_interface.cpp.
    pub fn get_device_state_for_handle(&self, device_handle: u64) -> DeviceState {
        log::debug!(
            "NFP::GetDeviceState called, device_handle={}",
            device_handle
        );
        let nfc_state = self.with_manager(|mgr| mgr.get_device_state(device_handle));
        // NFC DeviceState and NFP DeviceState have identical repr(u32) values.
        // Safe to transmute via the raw u32 value.
        let raw = nfc_state as u32;
        match raw {
            0 => DeviceState::Initialized,
            1 => DeviceState::SearchingForTag,
            2 => DeviceState::TagFound,
            3 => DeviceState::TagRemoved,
            4 => DeviceState::TagMounted,
            5 => DeviceState::Unavailable,
            _ => DeviceState::Finalized,
        }
    }

    /// GetNpadId (cmd 21).
    ///
    /// Corresponds to `NfcInterface::GetNpadId` in upstream nfc_interface.cpp.
    pub fn get_npad_id(&self, device_handle: u64) -> (ResultCode, u32) {
        log::debug!("NFP::GetNpadId called, device_handle={}", device_handle);
        let result = self.with_manager(|mgr| mgr.get_npad_id(device_handle));
        match result {
            Ok(npad_id) => (RESULT_SUCCESS, npad_id as u32),
            Err(e) => (Self::translate_result_to_service_error(e), 0),
        }
    }

    /// GetApplicationAreaSize (cmd 22).
    ///
    /// Corresponds to `Interface::GetApplicationAreaSize` in upstream nfp_interface.cpp.
    pub fn get_application_area_size(&self) -> u32 {
        log::debug!("NFP::GetApplicationAreaSize called");
        self.with_manager(|mgr| mgr.get_application_area_size())
    }

    /// RecreateApplicationArea (cmd 24).
    ///
    /// Corresponds to `Interface::RecreateApplicationArea` in upstream nfp_interface.cpp.
    pub fn recreate_application_area(
        &self,
        device_handle: u64,
        access_id: u32,
        data: &[u8],
    ) -> ResultCode {
        log::info!(
            "NFP::RecreateApplicationArea called, device_handle={}, access_id={}, data_size={}",
            device_handle,
            access_id,
            data.len()
        );
        let result =
            self.with_manager(|mgr| mgr.recreate_application_area(device_handle, access_id, data));
        Self::translate_result_to_service_error(result)
    }

    /// Format (cmd 100).
    ///
    /// Corresponds to `Interface::Format` in upstream nfp_interface.cpp.
    pub fn format(&self, device_handle: u64) -> ResultCode {
        log::info!("NFP::Format called, device_handle={}", device_handle);
        let result = self.with_manager(|mgr| mgr.format(device_handle));
        Self::translate_result_to_service_error(result)
    }

    /// GetRegisterInfo (cmd 14).
    ///
    /// Corresponds to `Interface::GetRegisterInfo` in upstream nfp_interface.cpp.
    pub fn get_register_info(
        &self,
        device_handle: u64,
    ) -> (ResultCode, super::nfp_types::RegisterInfo) {
        log::info!(
            "NFP::GetRegisterInfo called, device_handle={}",
            device_handle
        );
        let result = self.with_manager(|mgr| mgr.get_register_info(device_handle));
        match result {
            Ok(info) => (RESULT_SUCCESS, info),
            Err(error) => (
                Self::translate_result_to_service_error(error),
                super::nfp_types::RegisterInfo::default(),
            ),
        }
    }

    /// GetCommonInfo (cmd 15).
    ///
    /// Corresponds to `Interface::GetCommonInfo` in upstream nfp_interface.cpp.
    pub fn get_common_info(&self, device_handle: u64) -> (ResultCode, CommonInfo) {
        log::info!("NFP::GetCommonInfo called, device_handle={}", device_handle);
        let result = self.with_manager(|mgr| mgr.get_common_info(device_handle));
        match result {
            Ok(info) => (RESULT_SUCCESS, info),
            Err(e) => (
                Self::translate_result_to_service_error(e),
                CommonInfo::default(),
            ),
        }
    }

    /// GetModelInfo (cmd 16).
    ///
    /// Corresponds to `Interface::GetModelInfo` in upstream nfp_interface.cpp.
    pub fn get_model_info(&self, device_handle: u64) -> (ResultCode, super::nfp_types::ModelInfo) {
        log::info!("NFP::GetModelInfo called, device_handle={}", device_handle);
        let result = self.with_manager(|mgr| mgr.get_model_info(device_handle));
        match result {
            Ok(info) => (RESULT_SUCCESS, info),
            Err(error) => (
                Self::translate_result_to_service_error(error),
                super::nfp_types::ModelInfo::default(),
            ),
        }
    }

    /// DeleteRegisterInfo (cmd 104).
    ///
    /// Corresponds to `Interface::DeleteRegisterInfo` in upstream nfp_interface.cpp.
    pub fn delete_register_info(&self, device_handle: u64) -> ResultCode {
        log::info!(
            "NFP::DeleteRegisterInfo called, device_handle={}",
            device_handle
        );
        let result = self.with_manager(|mgr| mgr.delete_register_info(device_handle));
        Self::translate_result_to_service_error(result)
    }

    /// DeleteApplicationArea (cmd 105).
    ///
    /// Corresponds to `Interface::DeleteApplicationArea` in upstream nfp_interface.cpp.
    pub fn delete_application_area(&self, device_handle: u64) -> ResultCode {
        log::info!(
            "NFP::DeleteApplicationArea called, device_handle={}",
            device_handle
        );
        let result = self.with_manager(|mgr| mgr.delete_application_area(device_handle));
        Self::translate_result_to_service_error(result)
    }

    /// ExistsApplicationArea (cmd 106).
    ///
    /// Corresponds to `Interface::ExistsApplicationArea` in upstream nfp_interface.cpp.
    pub fn exists_application_area(&self, device_handle: u64) -> (ResultCode, bool) {
        log::info!(
            "NFP::ExistsApplicationArea called, device_handle={}",
            device_handle
        );
        let result = self.with_manager(|mgr| mgr.exists_application_area(device_handle));
        match result {
            Ok(exists) => (RESULT_SUCCESS, exists),
            Err(e) => (Self::translate_result_to_service_error(e), false),
        }
    }

    /// FlushDebug (cmd 202).
    ///
    /// Corresponds to `Interface::FlushDebug` in upstream nfp_interface.cpp.
    pub fn flush_debug(&self, device_handle: u64) -> ResultCode {
        log::info!("NFP::FlushDebug called, device_handle={}", device_handle);
        let result = self.with_manager(|mgr| mgr.flush_debug(device_handle));
        Self::translate_result_to_service_error(result)
    }

    /// BreakTag (cmd 203).
    ///
    /// Corresponds to `Interface::BreakTag` in upstream nfp_interface.cpp.
    pub fn break_tag(&self, device_handle: u64, break_type: BreakType) -> ResultCode {
        log::warn!(
            "(STUBBED) NFP::BreakTag called, device_handle={}, break_type={:?}",
            device_handle,
            break_type
        );
        let result = self.with_manager(|mgr| mgr.break_tag(device_handle, break_type as u32));
        Self::translate_result_to_service_error(result)
    }

    /// ReadBackupData (cmd 204).
    ///
    /// Corresponds to `Interface::ReadBackupData` in upstream nfp_interface.cpp.
    pub fn read_backup_data(&self, device_handle: u64, data: &mut [u8]) -> ResultCode {
        log::info!(
            "NFP::ReadBackupData called, device_handle={}",
            device_handle
        );
        let result = self.with_manager(|mgr| mgr.read_backup_data(device_handle, data));
        Self::translate_result_to_service_error(result)
    }

    /// WriteBackupData (cmd 205).
    ///
    /// Corresponds to `Interface::WriteBackupData` in upstream nfp_interface.cpp.
    pub fn write_backup_data(&self, device_handle: u64, data: &[u8]) -> ResultCode {
        log::info!(
            "NFP::WriteBackupData called, device_handle={}",
            device_handle
        );
        let result = self.with_manager(|mgr| mgr.write_backup_data(device_handle, data));
        Self::translate_result_to_service_error(result)
    }

    /// WriteNtf (cmd 206).
    ///
    /// Corresponds to `Interface::WriteNtf` in upstream nfp_interface.cpp.
    pub fn write_ntf(&self, device_handle: u64, write_type: WriteType, data: &[u8]) -> ResultCode {
        log::warn!(
            "(STUBBED) NFP::WriteNtf called, device_handle={}, write_type={:?}",
            device_handle,
            write_type
        );
        let result = self.with_manager(|mgr| mgr.write_ntf(device_handle, write_type as u32, data));
        Self::translate_result_to_service_error(result)
    }

    // --- IPC handler functions ---

    /// Initialize (cmd 0).
    /// Upstream: NfcInterface::Initialize
    pub(super) fn initialize_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        log::info!("NFP::Initialize called");
        let result = service.initialize();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// Finalize (cmd 1).
    /// Upstream: NfcInterface::Finalize
    pub(super) fn finalize_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        log::info!("NFP::Finalize called");
        let _result = service.finalize();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// ListDevices (cmd 2).
    /// Upstream: NfcInterface::ListDevices
    pub(super) fn list_devices_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let max_allowed = ctx.get_write_buffer_size(0) / 8; // sizeof(u64) = 8
        log::debug!("NFP::ListDevices called");

        let (result, devices) = service.list_devices(max_allowed);

        if result.is_error() {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(result);
            return;
        }

        // Write device handles to output buffer
        let bytes: Vec<u8> = devices.iter().flat_map(|d| d.to_le_bytes()).collect();
        ctx.write_buffer(&bytes, 0);

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(devices.len() as i32);
    }

    /// StartDetection (cmd 3).
    /// Upstream: NfcInterface::StartDetection
    /// For NFP backend, tag_protocol is always NfcProtocol::All (not read from params).
    pub(super) fn start_detection_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        // NFP backend does not read tag_protocol from params (uses NfcProtocol::All)
        log::info!(
            "NFP::StartDetection called, device_handle={}",
            device_handle
        );

        let result = service.start_detection(device_handle);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// StopDetection (cmd 4).
    /// Upstream: NfcInterface::StopDetection
    pub(super) fn stop_detection_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        log::info!("NFP::StopDetection called, device_handle={}", device_handle);

        let result = service.stop_detection(device_handle);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// Mount (cmd 5).
    /// Upstream: Interface::Mount
    pub(super) fn mount_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        let model_type_raw = rp.pop_u32();
        let mount_target_raw = rp.pop_u32();

        let model_type = match model_type_raw {
            0 => ModelType::Amiibo,
            _ => ModelType::Amiibo,
        };
        let mount_target = match mount_target_raw {
            1 => MountTarget::Rom,
            2 => MountTarget::Ram,
            3 => MountTarget::All,
            _ => MountTarget::All,
        };

        log::info!(
            "NFP::Mount called, device_handle={}, model_type={:?}, mount_target={:?}",
            device_handle,
            model_type,
            mount_target
        );

        let result = service.mount(device_handle, model_type, mount_target);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// Unmount (cmd 6).
    /// Upstream: Interface::Unmount
    pub(super) fn unmount_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        log::info!("NFP::Unmount called, device_handle={}", device_handle);

        let result = service.unmount(device_handle);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// OpenApplicationArea (cmd 7).
    /// Upstream: Interface::OpenApplicationArea
    pub(super) fn open_application_area_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        let access_id = rp.pop_u32();
        log::info!(
            "NFP::OpenApplicationArea called, device_handle={}, access_id={}",
            device_handle,
            access_id
        );

        let result = service.open_application_area(device_handle, access_id);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// GetApplicationArea (cmd 8).
    /// Upstream: Interface::GetApplicationArea
    pub(super) fn get_application_area_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        let data_size = ctx.get_write_buffer_size(0);
        log::info!(
            "NFP::GetApplicationArea called, device_handle={}",
            device_handle
        );

        let mut data = vec![0u8; data_size];
        let (result, _size) = service.get_application_area(device_handle, &mut data);

        if result.is_error() {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(result);
            return;
        }

        ctx.write_buffer(&data, 0);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(data_size as u32);
    }

    /// SetApplicationArea (cmd 9).
    /// Upstream: Interface::SetApplicationArea
    pub(super) fn set_application_area_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        let data = ctx.read_buffer(0);
        log::info!(
            "NFP::SetApplicationArea called, device_handle={}, data_size={}",
            device_handle,
            data.len()
        );

        let result = service.set_application_area(device_handle, &data);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// Flush (cmd 10).
    /// Upstream: Interface::Flush
    pub(super) fn flush_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        log::info!("NFP::Flush called, device_handle={}", device_handle);

        let result = service.flush(device_handle);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// Restore (cmd 11).
    /// Upstream: Interface::Restore
    pub(super) fn restore_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        log::info!("NFP::Restore called, device_handle={}", device_handle);

        let result = service.restore(device_handle);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// CreateApplicationArea (cmd 12).
    /// Upstream: Interface::CreateApplicationArea
    pub(super) fn create_application_area_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        let access_id = rp.pop_u32();
        let data = ctx.read_buffer(0);
        log::info!(
            "NFP::CreateApplicationArea called, device_handle={}, access_id={}, data_size={}",
            device_handle,
            access_id,
            data.len()
        );

        let result = service.create_application_area(device_handle, access_id, &data);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// GetTagInfo (cmd 13).
    /// Upstream: NfcInterface::GetTagInfo
    pub(super) fn get_tag_info_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        log::info!("NFP::GetTagInfo called, device_handle={}", device_handle);

        let (result, tag_info) = service.get_tag_info(device_handle);

        if result.is_success() {
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    &tag_info as *const TagInfo as *const u8,
                    core::mem::size_of::<TagInfo>(),
                )
            };
            ctx.write_buffer(bytes, 0);
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// GetRegisterInfo (cmd 14).
    /// Upstream: Interface::GetRegisterInfo
    pub(super) fn get_register_info_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        log::info!(
            "NFP::GetRegisterInfo called, device_handle={}",
            device_handle
        );

        let (result, register_info) = service.get_register_info(device_handle);

        if result.is_success() {
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    &register_info as *const super::nfp_types::RegisterInfo as *const u8,
                    core::mem::size_of::<super::nfp_types::RegisterInfo>(),
                )
            };
            ctx.write_buffer(bytes, 0);
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// GetCommonInfo (cmd 15).
    /// Upstream: Interface::GetCommonInfo
    pub(super) fn get_common_info_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        log::info!("NFP::GetCommonInfo called, device_handle={}", device_handle);

        let (result, common_info) = service.get_common_info(device_handle);

        if result.is_success() {
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    &common_info as *const CommonInfo as *const u8,
                    core::mem::size_of::<CommonInfo>(),
                )
            };
            ctx.write_buffer(bytes, 0);
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// GetModelInfo (cmd 16).
    /// Upstream: Interface::GetModelInfo
    pub(super) fn get_model_info_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        log::info!("NFP::GetModelInfo called, device_handle={}", device_handle);

        let (result, model_info) = service.get_model_info(device_handle);

        if result.is_success() {
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    &model_info as *const super::nfp_types::ModelInfo as *const u8,
                    core::mem::size_of::<super::nfp_types::ModelInfo>(),
                )
            };
            ctx.write_buffer(bytes, 0);
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// AttachActivateEvent (cmd 17).
    /// Upstream: NfcInterface::AttachActivateEvent
    pub(super) fn attach_activate_event_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        log::debug!(
            "NFP::AttachActivateEvent called, device_handle={}",
            device_handle
        );

        // Upstream: GetManager()->AttachActivateEvent(&out_event, device_handle)
        // The DeviceManager returns an Arc<Event>; we create a readable event handle from it.
        let _event = service.with_manager(|mgr| mgr.attach_activate_event(device_handle));
        let event_handle = ctx.create_readable_event_handle(false).unwrap_or(0);

        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_objects(event_handle);
    }

    /// AttachDeactivateEvent (cmd 18).
    /// Upstream: NfcInterface::AttachDeactivateEvent
    pub(super) fn attach_deactivate_event_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        log::debug!(
            "NFP::AttachDeactivateEvent called, device_handle={}",
            device_handle
        );

        // Upstream: GetManager()->AttachDeactivateEvent(&out_event, device_handle)
        let _event = service.with_manager(|mgr| mgr.attach_deactivate_event(device_handle));
        let event_handle = ctx.create_readable_event_handle(false).unwrap_or(0);

        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_objects(event_handle);
    }

    /// GetState (cmd 19).
    /// Upstream: NfcInterface::GetState
    pub(super) fn get_state_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        log::debug!("NFP::GetState called");

        let state = service.get_state();

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(state as u32);
    }

    /// GetDeviceState (cmd 20).
    /// Upstream: NfcInterface::GetDeviceState
    pub(super) fn get_device_state_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        log::debug!(
            "NFP::GetDeviceState called, device_handle={}",
            device_handle
        );

        let device_state = service.get_device_state_for_handle(device_handle);

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(device_state as u32);
    }

    /// GetNpadId (cmd 21).
    /// Upstream: NfcInterface::GetNpadId
    pub(super) fn get_npad_id_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        log::debug!("NFP::GetNpadId called, device_handle={}", device_handle);

        let (result, npad_id) = service.get_npad_id(device_handle);

        if result.is_error() {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(result);
            return;
        }

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(npad_id);
    }

    /// GetApplicationAreaSize (cmd 22).
    /// Upstream: Interface::GetApplicationAreaSize
    pub(super) fn get_application_area_size_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let _device_handle = rp.pop_u64();
        log::debug!(
            "NFP::GetApplicationAreaSize called, device_handle={}",
            _device_handle
        );

        let size = service.get_application_area_size();

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(size);
    }

    /// AttachAvailabilityChangeEvent (cmd 23).
    /// Upstream: NfcInterface::AttachAvailabilityChangeEvent
    pub(super) fn attach_availability_change_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        log::info!("NFP::AttachAvailabilityChangeEvent called");

        // Upstream: GetManager()->AttachAvailabilityChangeEvent()
        let _event = service.with_manager(|mgr| mgr.attach_availability_change_event());
        let event_handle = ctx.create_readable_event_handle(false).unwrap_or(0);

        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_objects(event_handle);
    }

    /// RecreateApplicationArea (cmd 24).
    /// Upstream: Interface::RecreateApplicationArea
    pub(super) fn recreate_application_area_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device_handle = rp.pop_u64();
        let access_id = rp.pop_u32();
        let data = ctx.read_buffer(0);
        log::info!(
            "NFP::RecreateApplicationArea called, device_handle={}, access_id={}, data_size={}",
            device_handle,
            access_id,
            data.len()
        );

        let result = service.recreate_application_area(device_handle, access_id, &data);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }
}

impl SessionRequestHandler for Interface {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        &self.name
    }
}

impl ServiceFramework for Interface {
    fn get_service_name(&self) -> &str {
        &self.name
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

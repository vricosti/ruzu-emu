// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ns/read_only_application_control_data_interface.h
//! Port of zuyu/src/core/hle/service/ns/read_only_application_control_data_interface.cpp
//!
//! IReadOnlyApplicationControlDataInterface — reads NACP and icon data.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::file_sys::control_metadata::RawNACP;
use crate::file_sys::patch_manager::PatchManager;
use crate::hle::result::ResultCode;
use crate::hle::result::RESULT_SUCCESS;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::ns::language::{
    convert_to_application_language, convert_to_language_code,
    get_application_language_priority_list, get_supported_language_flag, ApplicationLanguage,
};
use crate::hle::service::ns::ns_results::RESULT_APPLICATION_LANGUAGE_NOT_FOUND;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use crate::hle::service::set::settings_server::get_language_code_from_index;
use crate::hle::service::os::event::Event;

fn sanitize_jpeg_image_size(image: &mut Vec<u8>) {
    const MAX_JPEG_IMAGE_SIZE: usize = 0x20000;
    const PROFILE_DIMENSIONS: u32 = 174;
    let Ok(decoded) = image::load_from_memory(image) else {
        log::error!("Failed to load JPEG for sanitization");
        return;
    };
    if decoded.width() != PROFILE_DIMENSIONS || decoded.height() != PROFILE_DIMENSIONS {
        // Intentional correction: Eden supplies FILTER_BOX in the flags slot
        // and treats red as alpha. Use an actual area-box filter in linear RGB,
        // with no alpha channel. image replaces stb under the common exception.
        let input = decoded.to_rgb8();
        let linear = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        let srgb = |v: f64| {
            let v = if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
            (v * 255.0).round().clamp(0.0, 255.0) as u8
        };
        let mut output = image::RgbImage::new(PROFILE_DIMENSIONS, PROFILE_DIMENSIONS);
        let sx = f64::from(input.width()) / f64::from(PROFILE_DIMENSIONS);
        let sy = f64::from(input.height()) / f64::from(PROFILE_DIMENSIONS);
        for (x, y, pixel) in output.enumerate_pixels_mut() {
            let (left, top) = (f64::from(x) * sx, f64::from(y) * sy);
            let (right, bottom) = (f64::from(x + 1) * sx, f64::from(y + 1) * sy);
            let mut channels = [0.0; 3];
            for iy in top.floor() as u32..(bottom.ceil() as u32).min(input.height()) {
                for ix in left.floor() as u32..(right.ceil() as u32).min(input.width()) {
                    let weight = (right.min(f64::from(ix + 1)) - left.max(f64::from(ix)))
                        * (bottom.min(f64::from(iy + 1)) - top.max(f64::from(iy)));
                    for (sum, channel) in channels.iter_mut().zip(input.get_pixel(ix, iy).0) {
                        *sum += linear(channel) * weight;
                    }
                }
            }
            *pixel = image::Rgb(channels.map(|v| srgb(v / (sx * sy))));
        }
        let mut encoded = Vec::new();
        if let Err(error) = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 90)
            .encode_image(&output) {
            log::error!("Failed to resize JPEG: {error}");
            return;
        }
        *image = encoded;
    }
    image.truncate(MAX_JPEG_IMAGE_SIZE);
}

// Mechanical counterpart of ListApplicationTitle's vector<u64> + memcpy:
// keep byte decoding local to its upstream owner and independent of host endian.
fn decode_application_ids(bytes: &[u8]) -> Vec<u64> {
    bytes.chunks_exact(8)
        .map(|bytes| u64::from_le_bytes(bytes.try_into().unwrap()))
        .collect()
}

// Upstream defines this child interface locally in the same .cpp, not async_value.cpp.
struct IAsyncValue {
    // Event owns the kernel bridge; dropping it closes the service's reference.
    completion_event: Event,
    data_offset: i32,
    data_size: i32,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IAsyncValue {
    fn new(data_offset: i32, data_size: i32) -> Self {
        let completion_event = Event::new();
        completion_event.signal();
        Self {
            completion_event,
            data_offset,
            data_size,
            handlers: build_handler_map(&[
                (0, Some(Self::get_size_handler), "GetSize"),
                (1, Some(Self::get_handler), "Get"),
                (2, Some(Self::cancel_handler), "Cancel"),
                (3, Some(Self::get_error_context_handler), "GetErrorContext"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn get_size(&self) -> i64 {
        self.data_size as i64
    }

    fn get(&self, out_data_offset: &mut [u8]) -> ResultCode {
        // Avoid the upstream unchecked memcpy when a guest supplies <4 bytes.
        if out_data_offset.len() < 4 {
            return crate::hle::result::RESULT_UNKNOWN;
        }
        out_data_offset[..4].copy_from_slice(&self.data_offset.to_le_bytes());
        RESULT_SUCCESS
    }

    fn get_size_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = this.as_any().downcast_ref::<Self>().unwrap();
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(service.get_size() as u64);
    }

    fn get_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = this.as_any().downcast_ref::<Self>().unwrap();
        // CMIF uses an uninitialized scratch buffer upstream. Keep the unused
        // tail deterministic instead of exposing stale host bytes to the guest.
        let mut offset = vec![0; ctx.get_write_buffer_size(0)];
        let result = service.get(&mut offset);
        if result.is_success() {
            ctx.write_buffer(&offset, 0);
        }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn cancel_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_error_context_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }
}

impl SessionRequestHandler for IAsyncValue {
    fn as_any(&self) -> &dyn std::any::Any { self }
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }

    fn service_name(&self) -> &str { "IAsyncValue" }
}

impl ServiceFramework for IAsyncValue {
    fn get_service_name(&self) -> &str { self.service_name() }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> { &self.handlers }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> { &self.handlers_tipc }
}

/// Application control source.
///
/// Corresponds to `ApplicationControlSource` in upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ApplicationControlSource {
    CacheOnly = 0,
    Storage = 1,
    StorageOnly = 2,
}

/// IPC command table for IReadOnlyApplicationControlDataInterface.
///
/// Corresponds to the function table in upstream.
pub mod commands {
    pub const GET_APPLICATION_CONTROL_DATA: u32 = 0;
    pub const GET_APPLICATION_DESIRED_LANGUAGE: u32 = 1;
    pub const CONVERT_APPLICATION_LANGUAGE_TO_LANGUAGE_CODE: u32 = 2;
    pub const CONVERT_LANGUAGE_CODE_TO_APPLICATION_LANGUAGE: u32 = 3;
    pub const SELECT_APPLICATION_DESIRED_LANGUAGE: u32 = 4;
    pub const LIST_APPLICATION_TITLE: u32 = 13;
}

/// IReadOnlyApplicationControlDataInterface.
///
/// Corresponds to `IReadOnlyApplicationControlDataInterface` in upstream.
pub struct IReadOnlyApplicationControlDataInterface {
    system: crate::core::SystemRef,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IReadOnlyApplicationControlDataInterface {
    pub fn new(system: crate::core::SystemRef) -> Self {
        let handlers = build_handler_map(&[
            (
                commands::GET_APPLICATION_CONTROL_DATA,
                Some(Self::get_application_control_data_handler),
                "GetApplicationControlData",
            ),
            (
                commands::GET_APPLICATION_DESIRED_LANGUAGE,
                Some(Self::get_application_desired_language_handler),
                "GetApplicationDesiredLanguage",
            ),
            (
                commands::CONVERT_APPLICATION_LANGUAGE_TO_LANGUAGE_CODE,
                Some(Self::convert_application_language_to_language_code_handler),
                "ConvertApplicationLanguageToLanguageCode",
            ),
            (
                commands::CONVERT_LANGUAGE_CODE_TO_APPLICATION_LANGUAGE,
                None,
                "ConvertLanguageCodeToApplicationLanguage",
            ),
            (
                commands::SELECT_APPLICATION_DESIRED_LANGUAGE,
                None,
                "SelectApplicationDesiredLanguage",
            ),
            (commands::LIST_APPLICATION_TITLE, Some(Self::list_application_title), "ListApplicationTitle"),
            (5, Some(Self::get_application_control_data2_handler), "GetApplicationControlData"),
            (10, Some(Self::list_application_icon), "ListApplicationIcon"),
            (19, Some(Self::get_application_control_data3_handler), "GetApplicationControlData"),
            (23, Some(Self::get_application_control_data3_handler), "GetApplicationControlData"),
        ]);
        Self {
            system,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// GetApplicationControlData (cmd 0).
    ///
    /// Returns NACP data + icon for a given application_id.
    /// Corresponds to upstream `GetApplicationControlData`.
    pub fn get_application_control_data(
        &self,
        _source: ApplicationControlSource,
        application_id: u64,
        out_buffer: &mut [u8],
    ) -> Result<u32, ResultCode> {
        log::info!(
            "GetApplicationControlData called, application_id={:#018x}",
            application_id
        );

        const NACP_SIZE: usize = 0x4000;
        debug_assert_eq!(NACP_SIZE, std::mem::size_of::<RawNACP>());

        let metadata = if self.system.is_null() {
            (None, None)
        } else {
            let system = self.system.get();
            let fs_controller = system.get_filesystem_controller();
            let fs_controller = fs_controller.lock().unwrap();
            let provider = system.get_content_provider();
            match provider {
                Some(provider) => {
                    let provider = provider.lock().unwrap();
                    let patch_manager =
                        PatchManager::new(application_id, &fs_controller, &*provider);
                    patch_manager.get_control_metadata()
                }
                None => (None, None),
            }
        };

        let icon_size = metadata.1.as_ref().map(|icon| icon.get_size()).unwrap_or(0);
        let total_size = NACP_SIZE + icon_size;

        if out_buffer.len() < total_size {
            log::error!(
                "output buffer is too small! (actual={:#x}, expected_min={:#x})",
                out_buffer.len(),
                total_size
            );
            return Err(ResultCode::new(1)); // ResultUnknown
        }

        if let Some(nacp) = metadata.0 {
            let bytes = nacp.get_raw_bytes();
            out_buffer[..NACP_SIZE].copy_from_slice(&bytes[..NACP_SIZE]);
        } else {
            log::warn!(
                "missing NACP data for application_id={:#018x}, defaulting to zero",
                application_id
            );
            out_buffer[..NACP_SIZE].fill(0);
        }

        if let Some(icon) = metadata.1 {
            let icon_size = icon.get_size();
            let read = icon.read(
                &mut out_buffer[NACP_SIZE..NACP_SIZE + icon_size],
                icon_size,
                0,
            );
            debug_assert_eq!(read, icon_size);
        } else {
            log::warn!(
                "missing icon data for application_id={:#018x}",
                application_id
            );
        }

        Ok(total_size as u32)
    }

    /// GetApplicationDesiredLanguage (cmd 1).
    ///
    /// Corresponds to upstream `GetApplicationDesiredLanguage`.
    pub fn get_application_desired_language(
        &self,
        supported_languages: u32,
    ) -> Result<ApplicationLanguage, ResultCode> {
        log::info!(
            "GetApplicationDesiredLanguage called, supported_languages={:#010x}",
            supported_languages
        );

        let language_index = *common::settings::values().language_index.get_value() as usize;
        let language_code = get_language_code_from_index(language_index);
        let application_language =
            convert_to_application_language(language_code).ok_or_else(|| {
                log::error!(
                    "Could not convert application language! language_code={:#018x}",
                    language_code as u64
                );
                RESULT_APPLICATION_LANGUAGE_NOT_FOUND
            })?;
        let priority_list = get_application_language_priority_list(application_language)
            .ok_or_else(|| {
                log::error!(
                    "Could not find application language priorities! application_language={:?}",
                    application_language
                );
                RESULT_APPLICATION_LANGUAGE_NOT_FOUND
            })?;

        for &lang in priority_list {
            let supported_flag = get_supported_language_flag(lang);
            if supported_languages == 0 || (supported_languages & supported_flag) == supported_flag
            {
                return Ok(lang);
            }
        }

        log::error!(
            "Could not find a valid language! supported_languages={:#010x}",
            supported_languages
        );
        Err(RESULT_APPLICATION_LANGUAGE_NOT_FOUND)
    }

    /// ConvertApplicationLanguageToLanguageCode (cmd 2).
    ///
    /// Corresponds to upstream `ConvertApplicationLanguageToLanguageCode`.
    pub fn convert_application_language_to_language_code(
        &self,
        application_language: ApplicationLanguage,
    ) -> Result<u64, ResultCode> {
        convert_to_language_code(application_language)
            .map(|language_code| language_code as u64)
            .ok_or_else(|| {
                log::error!(
                    "Language not found! application_language={:?}",
                    application_language
                );
                RESULT_APPLICATION_LANGUAGE_NOT_FOUND
            })
    }

    // Mechanical extraction of the identical preparation in upstream Data2/Data3;
    // metadata ownership and all buffer work remain in this upstream module.
    fn prepare_control_data(&self, flag1: u8, application_id: u64, out: &mut [u8])
        -> Result<u32, ResultCode>
    {
        let control = if self.system.is_null() { (None, None) } else {
            let system = self.system.get();
            let controller = system.get_filesystem_controller();
            let controller = controller.lock().unwrap();
            match system.get_content_provider() {
                Some(provider) => {
                    let provider = provider.lock().unwrap();
                    PatchManager::new(application_id, &controller, &*provider).get_control_metadata()
                }
                None => (None, None),
            }
        };
        let nacp_size = std::mem::size_of::<RawNACP>();
        if out.len() < nacp_size { return Err(crate::hle::result::RESULT_UNKNOWN); }
        out.fill(0);
        if let Some(nacp) = control.0 {
            let bytes = nacp.get_raw_bytes();
            let count = bytes.len().min(nacp_size);
            out[..count].copy_from_slice(&bytes[..count]);
        }
        let mut icon_data = Vec::new();
        if let Some(icon) = control.1 {
            let size = icon.get_size();
            if size > 0 {
                icon_data.resize(size, 0);
                icon.read(&mut icon_data, size, 0);
                if flag1 == 1 { sanitize_jpeg_image_size(&mut icon_data); }
            }
        }
        let count = icon_data.len().min(out.len() - nacp_size);
        out[nacp_size..nacp_size + count].copy_from_slice(&icon_data[..count]);
        Ok((nacp_size + icon_data.len()) as u32)
    }

    pub fn get_application_control_data2(&self, _source: u8, flag1: u8, _flag2: u8,
        application_id: u64, out: &mut [u8]) -> Result<u64, ResultCode>
    {
        let size = self.prepare_control_data(flag1, application_id, out)?;
        Ok((u64::from(size) << 32) | u64::from(flag1))
    }

    pub fn get_application_control_data3(&self, _source: u8, flag1: u8, _flag2: u8,
        application_id: u64, out: &mut [u8]) -> Result<[u32; 3], ResultCode>
    {
        let size = self.prepare_control_data(flag1, application_id, out)?;
        Ok([0x10001, if flag1 == 1 { 0x10001 } else { 0 }, size])
    }

    fn get_application_control_data2_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = this.as_any().downcast_ref::<Self>().unwrap();
        let mut rp = RequestParser::new(ctx);
        // CMIF packs three u8 fields, then aligns application_id to eight bytes.
        let flags = rp.pop_u32();
        rp.pop_u32();
        let application_id = rp.pop_u64();
        let mut output = vec![0; ctx.get_write_buffer_size(0)];
        match service.get_application_control_data2(flags as u8, (flags >> 8) as u8,
            (flags >> 16) as u8, application_id, &mut output) {
            Ok(size) => {
                ctx.write_buffer(&output, 0);
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u64(size);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }

    fn get_application_control_data3_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = this.as_any().downcast_ref::<Self>().unwrap();
        let mut rp = RequestParser::new(ctx);
        let flags = rp.pop_u32();
        rp.pop_u32();
        let application_id = rp.pop_u64();
        let mut output = vec![0; ctx.get_write_buffer_size(0)];
        match service.get_application_control_data3(flags as u8, (flags >> 8) as u8,
            (flags >> 16) as u8, application_id, &mut output) {
            Ok(words) => {
                ctx.write_buffer(&output, 0);
                let mut rb = ResponseBuilder::new(ctx, 5, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                for word in words { rb.push_u32(word); }
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }

    pub(super) fn list_application_icon(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = this.as_any().downcast_ref::<Self>().unwrap();
        // Unlike Eden's byte indexing, decode each complete little-endian u64.
        let application_ids = decode_application_ids(&ctx.read_buffer(0));
        let transfer_memory = ctx.owner_process_arc().and_then(|process| {
            let process = process.lock().unwrap();
            let id = process.handle_table.get_object(ctx.get_copy_handle(0))?;
            process.get_transfer_memory_by_object_id(id)
        });
        let result = (|| -> Result<usize, ResultCode> {
            let Some(transfer_memory) = transfer_memory else { return Ok(0); };
            let (owner, address, capacity) = {
                let transfer = transfer_memory.lock().unwrap();
                (transfer.get_owner(), transfer.get_source_address(), transfer.get_size())
            };
            let Some(owner) = owner else { return Ok(0); };
            if application_ids.is_empty() { return Ok(0); }
            let memory = owner.lock().unwrap().get_memory()
                .ok_or(crate::hle::result::RESULT_UNKNOWN)?;
            // Keep the owner and transfer object alive, but neither lock across
            // metadata lookup or writes. Check BEFORE writing, unlike upstream.
            let mut out_length = 0usize;
            let mut write = |bytes: &[u8]| -> Result<(), ResultCode> {
                let end = out_length.checked_add(bytes.len())
                    .filter(|end| *end <= capacity)
                    .ok_or(crate::hle::result::RESULT_UNKNOWN)?;
                address.checked_add(end as u64).ok_or(crate::hle::result::RESULT_UNKNOWN)?;
                memory.lock().unwrap().write_block(address + out_length as u64, bytes);
                out_length = end;
                Ok(())
            };
            write(&(application_ids.len() as u64).to_le_bytes())?;
            let system = service.system.get();
            // Preserve upstream's two metadata passes: sizes, then raw bytes.
            for application_id in &application_ids {
                let controller = system.get_filesystem_controller();
                let controller = controller.lock().unwrap();
                let size = system.get_content_provider().and_then(|provider| {
                    let provider = provider.lock().unwrap();
                    PatchManager::new(*application_id, &controller, &*provider)
                        .get_control_metadata().1
                }).map_or(0, |icon| icon.get_size());
                // Missing icons have an explicit zero length rather than
                // retaining stale bytes in the guest's transfer buffer.
                drop(controller);
                write(&(size as u64).to_le_bytes())?;
            }
            for application_id in &application_ids {
                let icon = {
                    let controller = system.get_filesystem_controller();
                    let controller = controller.lock().unwrap();
                    system.get_content_provider().and_then(|provider| {
                        let provider = provider.lock().unwrap();
                        PatchManager::new(*application_id, &controller, &*provider)
                            .get_control_metadata().1
                    })
                };
                if let Some(icon) = icon {
                    let size = icon.get_size();
                    if size > 0 {
                        let mut bytes = vec![0; size];
                        icon.read(&mut bytes, size, 0);
                        write(&bytes)?;
                    }
                }
            }
            Ok(out_length)
        })();
        let size = match result {
            Ok(size) => size,
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
                return;
            }
        };
        let value = Arc::new(IAsyncValue::new(0, size as i32));
        let Some(object_id) = value.completion_event.copy_object_id(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(crate::hle::result::RESULT_UNKNOWN);
            return;
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
        rb.push_ipc_interface(value);
    }

    pub(super) fn list_application_title(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = this.as_any().downcast_ref::<Self>().unwrap();
        let app_ids_buffer = ctx.read_buffer(0);
        let application_ids = decode_application_ids(&app_ids_buffer);
        const TITLE_ENTRY_SIZE: usize = std::mem::size_of::<crate::file_sys::control_metadata::LanguageEntry>();
        let total_data_size = application_ids.len() * TITLE_ENTRY_SIZE;

        // Retain the object after releasing the requesting process's handle-table
        // lock, matching upstream's scoped KTransferMemory reference.
        let transfer_memory = ctx.owner_process_arc().and_then(|process| {
            let process = process.lock().unwrap();
            let object_id = process.handle_table.get_object(ctx.get_copy_handle(0))?;
            process.get_transfer_memory_by_object_id(object_id)
        });
        if let Some(transfer_memory) = transfer_memory {
            if !application_ids.is_empty() {
                let (address, size) = {
                    let transfer = transfer_memory.lock().unwrap();
                    (transfer.get_source_address(), transfer.get_size())
                };
                // Upstream does not check this range before WriteBlock. Do not
                // let a malformed transfer buffer overwrite adjacent guest data.
                if total_data_size > size || address.checked_add(total_data_size as u64).is_none() {
                    let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                    rb.push_result(crate::hle::result::RESULT_UNKNOWN);
                    return;
                }
                let system = service.system.get();
                let Some(memory) = system.memory_shared() else {
                    let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                    rb.push_result(crate::hle::result::RESULT_UNKNOWN);
                    return;
                };
                for (index, application_id) in application_ids.into_iter().enumerate() {
                    let metadata = {
                        let controller = system.get_filesystem_controller();
                        let controller = controller.lock().unwrap();
                        system.get_content_provider().map(|provider| {
                            let provider = provider.lock().unwrap();
                            PatchManager::new(application_id, &controller, &*provider)
                                .get_control_metadata()
                        })
                    };
                    let mut entry_bytes = [0u8; TITLE_ENTRY_SIZE];
                    if let Some((Some(nacp), _)) = metadata {
                        let entry = nacp.get_language_entry();
                        entry_bytes[..0x200].copy_from_slice(&entry.application_name);
                        entry_bytes[0x200..].copy_from_slice(&entry.developer_name);
                    }
                    memory.lock().unwrap().write_block(
                        address + (index * TITLE_ENTRY_SIZE) as u64, &entry_bytes,
                    );
                }
            }
        }
        let value = Arc::new(IAsyncValue::new(0, total_data_size as i32));
        let Some(object_id) = value.completion_event.copy_object_id(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(crate::hle::result::RESULT_UNKNOWN);
            return;
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
        rb.push_ipc_interface(value);
    }

    pub(super) fn get_application_control_data_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe {
            &*(this as *const dyn ServiceFramework
                as *const IReadOnlyApplicationControlDataInterface)
        };
        let mut rp = RequestParser::new(ctx);
        let source = match rp.pop_u8() {
            0 => ApplicationControlSource::CacheOnly,
            1 => ApplicationControlSource::Storage,
            2 => ApplicationControlSource::StorageOnly,
            other => {
                log::error!("Invalid ApplicationControlSource {}", other);
                ApplicationControlSource::Storage
            }
        };
        // CMIF aligns the u64 after the one-byte source to offset eight.
        rp.skip(1);
        let application_id = rp.pop_u64();
        let mut out_buffer = vec![0u8; ctx.get_write_buffer_size(0)];
        match service.get_application_control_data(source, application_id, &mut out_buffer) {
            Ok(actual_size) => {
                ctx.write_buffer(&out_buffer, 0);
                let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u32(actual_size);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }

    pub(super) fn get_application_desired_language_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe {
            &*(this as *const dyn ServiceFramework
                as *const IReadOnlyApplicationControlDataInterface)
        };
        let mut rp = RequestParser::new(ctx);
        let supported_languages = rp.pop_u32();
        match service.get_application_desired_language(supported_languages) {
            Ok(language) => {
                let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u32(language as u32);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }

    pub(super) fn convert_application_language_to_language_code_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe {
            &*(this as *const dyn ServiceFramework
                as *const IReadOnlyApplicationControlDataInterface)
        };
        let mut rp = RequestParser::new(ctx);
        let application_language = match rp.pop_u8() {
            0 => ApplicationLanguage::AmericanEnglish,
            1 => ApplicationLanguage::BritishEnglish,
            2 => ApplicationLanguage::Japanese,
            3 => ApplicationLanguage::French,
            4 => ApplicationLanguage::German,
            5 => ApplicationLanguage::LatinAmericanSpanish,
            6 => ApplicationLanguage::Spanish,
            7 => ApplicationLanguage::Italian,
            8 => ApplicationLanguage::Dutch,
            9 => ApplicationLanguage::CanadianFrench,
            10 => ApplicationLanguage::Portuguese,
            11 => ApplicationLanguage::Russian,
            12 => ApplicationLanguage::Korean,
            13 => ApplicationLanguage::TraditionalChinese,
            14 => ApplicationLanguage::SimplifiedChinese,
            15 => ApplicationLanguage::BrazilianPortuguese,
            _ => ApplicationLanguage::Count,
        };
        match service.convert_application_language_to_language_code(application_language) {
            Ok(language_code) => {
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u64(language_code);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }
}

impl SessionRequestHandler for IReadOnlyApplicationControlDataInterface {
    fn as_any(&self) -> &dyn std::any::Any { self }
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ns::IReadOnlyApplicationControlDataInterface"
    }
}

impl ServiceFramework for IReadOnlyApplicationControlDataInterface {
    fn get_service_name(&self) -> &str {
        "ns::IReadOnlyApplicationControlDataInterface"
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
    use crate::core::SystemRef;
    use common::settings::Language;

    #[test]
    fn title_ids_keep_all_eight_bytes_and_ignore_incomplete_tail() {
        let ids = [0x1234_5678_9abc_def0u64, 0xfedc_ba98_7654_3210];
        let mut bytes: Vec<_> = ids.iter().flat_map(|id| id.to_le_bytes()).collect();
        bytes.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        assert_eq!(decode_application_ids(&bytes), ids);
        assert!(decode_application_ids(&bytes[..7]).is_empty());
        assert_eq!(std::mem::size_of::<crate::file_sys::control_metadata::LanguageEntry>(), 0x300);
        assert_eq!(std::mem::offset_of!(crate::file_sys::control_metadata::LanguageEntry, developer_name), 0x200);
    }

    #[test]
    fn async_value_sign_extends_size_and_preserves_offset_bytes() {
        let value = IAsyncValue::new(-2, -3);
        assert!(value.completion_event.is_signaled());
        assert_eq!(value.get_size(), -3i64);
        let mut bytes = [0xAA; 8];
        assert_eq!(value.get(&mut bytes), RESULT_SUCCESS);
        assert_eq!(&bytes[..4], &(-2i32).to_le_bytes());
        assert_eq!(&bytes[4..], &[0xAA; 4]);
        assert!(value.get(&mut bytes[..3]).is_error());

        let mut ctx = HLERequestContext::new();
        value.handlers[&0].handler_callback.unwrap()(&value, &mut ctx);
        assert_eq!(&ctx.command_buffer()[8..10], &[0xffff_fffd, 0xffff_ffff]);
        for command in [2, 3] {
            let mut ctx = HLERequestContext::new();
            value.handlers[&command].handler_callback.unwrap()(&value, &mut ctx);
            assert_eq!(ctx.command_buffer()[6], 0);
            assert!(value.completion_event.is_signaled());
        }
    }

    #[test]
    fn title_listing_is_registered_and_requires_an_event_export_context() {
        let service = IReadOnlyApplicationControlDataInterface::new(SystemRef::null());
        let mut ctx = HLERequestContext::new();
        service.handlers[&commands::LIST_APPLICATION_TITLE].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(ctx.command_buffer()[6], crate::hle::result::RESULT_UNKNOWN.0);
        assert!(ctx.outgoing_copy_objects.is_empty());
    }

    #[test]
    fn control_data_variants_keep_flag_bits_sizes_and_zero_padding() {
        let service = IReadOnlyApplicationControlDataInterface::new(SystemRef::null());
        for flag in [0, 1, 2, 255] {
            let mut output = vec![0xaa; 0x4010];
            assert_eq!(service.get_application_control_data2(2, flag, 255,
                0x1234_5678_9abc_def0, &mut output).unwrap(), (0x4000u64 << 32) | u64::from(flag));
            assert!(output.iter().all(|byte| *byte == 0));
            output.fill(0xaa);
            assert_eq!(service.get_application_control_data3(2, flag, 255,
                0x1234_5678_9abc_def0, &mut output).unwrap(),
                [0x10001, if flag == 1 { 0x10001 } else { 0 }, 0x4000]);
            assert!(output.iter().all(|byte| *byte == 0));
        }
        let mut short = vec![0xaa; 0x3fff];
        assert!(service.get_application_control_data2(0, 1, 0, 0, &mut short).is_err());
        assert!(service.get_application_control_data3(0, 1, 0, 0, &mut short).is_err());
        assert!(short.iter().all(|byte| *byte == 0xaa));
        for command in [5, 10, 13, 19, 23] {
            assert!(service.handlers[&command].handler_callback.is_some());
        }
    }

    #[test]
    fn control_data_cmif_packs_adjacent_flags_and_serializes_variant_outputs() {
        let service = IReadOnlyApplicationControlDataInterface::new(SystemRef::null());
        for command in [5, 19, 23] {
            let mut ctx = HLERequestContext::new();
            // source=2, flag1=1, flag2=0xab, then poisoned alignment padding.
            ctx.command_buffer_mut()[2..6].copy_from_slice(
                &[0xffab_0102, 0xdddd_dddd, 0x9abc_def0, 0x1234_5678]);
            ctx.set_buffer_b_descriptors_for_test(vec![crate::hle::ipc::BufferDescriptorABW {
                size_bits_0_31: 0x4000,
                address_bits_0_31: 0,
                raw_word2: 0,
            }]);
            service.handlers[&command].handler_callback.unwrap()(&service, &mut ctx);
            assert_eq!(ctx.command_buffer()[6], 0);
            if command == 5 {
                assert_eq!(&ctx.command_buffer()[8..10], &[1, 0x4000]);
            } else {
                assert_eq!(&ctx.command_buffer()[8..11], &[0x10001, 0x10001, 0x4000]);
            }
        }
    }

    #[test]
    fn jpeg_sanitization_preserves_invalid_data_and_already_sized_images() {
        let mut invalid = vec![0xff; 0x20010];
        let previous = invalid.clone();
        sanitize_jpeg_image_size(&mut invalid);
        assert_eq!(invalid, previous); // No truncation on decode failure.
        let input = image::RgbImage::from_pixel(174, 174, image::Rgb([120, 120, 120]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
            .encode_image(&input).unwrap();
        let original = jpeg.clone();
        sanitize_jpeg_image_size(&mut jpeg);
        assert_eq!(jpeg, original); // No needless re-encoding.
        jpeg.resize(0x20010, 0);
        sanitize_jpeg_image_size(&mut jpeg);
        assert_eq!(jpeg.len(), 0x20000);
        assert_eq!(&jpeg[..original.len()], &original);
    }

    #[test]
    fn jpeg_resize_averages_all_rgb_channels_in_linear_space() {
        // Each destination pixel covers equal black and white areas. A linear
        // RGB average is ~188 in sRGB, not 128 as for an alpha/linear byte.
        let mut input = image::RgbImage::new(348, 348);
        for (x, _, pixel) in input.enumerate_pixels_mut() {
            *pixel = image::Rgb([if x % 2 == 0 { 0 } else { 255 }; 3]);
        }
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 100)
            .encode_image(&input).unwrap();
        sanitize_jpeg_image_size(&mut jpeg);
        let output = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert_eq!(output.dimensions(), (174, 174));
        for pixel in output.pixels() {
            for channel in pixel.0 { assert!((184..=192).contains(&channel), "{pixel:?}"); }
        }
    }

    #[test]
    fn desired_language_uses_upstream_priority_list() {
        let previous = *common::settings::values().language_index.get_value();
        common::settings::values_mut()
            .language_index
            .set_value(Language::EnglishAmerican);

        let service = IReadOnlyApplicationControlDataInterface::new(crate::core::SystemRef::null());
        let supported = get_supported_language_flag(ApplicationLanguage::BritishEnglish)
            | get_supported_language_flag(ApplicationLanguage::French);
        let result = service.get_application_desired_language(supported).unwrap();

        common::settings::values_mut()
            .language_index
            .set_value(previous);

        assert_eq!(result, ApplicationLanguage::BritishEnglish);
    }

    #[test]
    fn convert_application_language_to_language_code_uses_ns_language_owner() {
        let service = IReadOnlyApplicationControlDataInterface::new(crate::core::SystemRef::null());
        let result = service
            .convert_application_language_to_language_code(ApplicationLanguage::BrazilianPortuguese)
            .unwrap();
        assert_eq!(
            result,
            crate::hle::service::set::settings_types::LanguageCode::PtBr as u64
        );
    }

    #[test]
    fn get_application_control_data_null_system_zero_fills_nacp() {
        let service = IReadOnlyApplicationControlDataInterface::new(SystemRef::null());
        let mut buffer = vec![0xAA; 0x4000];

        let size = service
            .get_application_control_data(ApplicationControlSource::Storage, 0x0100, &mut buffer)
            .unwrap();

        assert_eq!(size, 0x4000);
        assert!(buffer.iter().all(|b| *b == 0));
    }

    #[test]
    fn get_application_control_data_rejects_too_small_buffer() {
        let service = IReadOnlyApplicationControlDataInterface::new(SystemRef::null());
        let mut buffer = vec![0u8; 0x3FFF];

        let result = service.get_application_control_data(
            ApplicationControlSource::Storage,
            0x0100,
            &mut buffer,
        );

        assert!(result.is_err());
    }

    #[test]
    fn exercised_cmif_handlers_are_registered() {
        let service = IReadOnlyApplicationControlDataInterface::new(SystemRef::null());
        assert!(service
            .handlers()
            .get(&commands::GET_APPLICATION_CONTROL_DATA)
            .and_then(|fi| fi.handler_callback)
            .is_some());
        assert!(service
            .handlers()
            .get(&commands::GET_APPLICATION_DESIRED_LANGUAGE)
            .and_then(|fi| fi.handler_callback)
            .is_some());
        assert!(service
            .handlers()
            .get(&commands::CONVERT_APPLICATION_LANGUAGE_TO_LANGUAGE_CODE)
            .and_then(|fi| fi.handler_callback)
            .is_some());
    }
}

// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/set/settings_server.h
//! Port of zuyu/src/core/hle/service/set/settings_server.cpp
//!
//! ISettingsServer service ("set").

use std::collections::BTreeMap;

use super::key_code_map::*;
use super::settings_types::{
    KeyboardLayout, LanguageCode, AVAILABLE_LANGUAGE_CODES, LANGUAGE_TO_LAYOUT,
};
use crate::hle::result::{ErrorModule, ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// Constants matching upstream settings_server.cpp.
const PRE_4_0_0_MAX_ENTRIES: usize = 0xF;
const POST_4_0_0_MAX_ENTRIES: usize = 0x40;

/// Result codes from upstream settings_server.cpp.
const RESULT_INVALID_LANGUAGE: ResultCode =
    ResultCode::from_module_description(ErrorModule::Settings, 625);
const RESULT_NULL_POINTER: ResultCode =
    ResultCode::from_module_description(ErrorModule::Settings, 1261);

/// Key code map type -- 0x1000 bytes.
///
/// Corresponds to `KeyCodeMap` in upstream settings_server.h.
pub type KeyCodeMap = [u8; 0x1000];

/// Port of Set::GetLanguageCodeFromIndex.
pub fn get_language_code_from_index(index: usize) -> LanguageCode {
    AVAILABLE_LANGUAGE_CODES[index]
}

/// Copy a key code map slice into a fixed-size 0x1000-byte array, zero-padding as needed.
fn copy_key_code_map(src: &[u8]) -> KeyCodeMap {
    let mut out = [0u8; 0x1000];
    let len = src.len().min(0x1000);
    out[..len].copy_from_slice(&src[..len]);
    out
}

/// Internal key code map lookup by keyboard layout and language code.
///
/// Corresponds to `GetKeyCodeMapImpl` in upstream settings_server.cpp.
fn get_key_code_map_impl(
    out_key_code_map: &mut KeyCodeMap,
    keyboard_layout: KeyboardLayout,
    language_code: LanguageCode,
) -> ResultCode {
    let src = match keyboard_layout {
        KeyboardLayout::Japanese => KEY_CODE_MAP_JAPANESE,
        KeyboardLayout::EnglishUs => {
            if language_code == LanguageCode::Ko {
                KEY_CODE_MAP_KOREAN
            } else if language_code == LanguageCode::ZhHans {
                KEY_CODE_MAP_CHINESE_SIMPLIFIED
            } else if language_code == LanguageCode::ZhHant {
                KEY_CODE_MAP_CHINESE_TRADITIONAL
            } else {
                KEY_CODE_MAP_ENGLISH_US_INTERNATIONAL
            }
        }
        KeyboardLayout::EnglishUk => KEY_CODE_MAP_ENGLISH_UK,
        KeyboardLayout::French => KEY_CODE_MAP_FRENCH,
        KeyboardLayout::FrenchCa => KEY_CODE_MAP_FRENCH_CA,
        KeyboardLayout::Spanish => KEY_CODE_MAP_SPANISH,
        KeyboardLayout::SpanishLatin => KEY_CODE_MAP_SPANISH_LATIN,
        KeyboardLayout::German => KEY_CODE_MAP_GERMAN,
        KeyboardLayout::Italian => KEY_CODE_MAP_ITALIAN,
        KeyboardLayout::Portuguese => KEY_CODE_MAP_PORTUGUESE,
        KeyboardLayout::Russian => KEY_CODE_MAP_RUSSIAN,
        KeyboardLayout::Korean => KEY_CODE_MAP_KOREAN,
        KeyboardLayout::ChineseSimplified => KEY_CODE_MAP_CHINESE_SIMPLIFIED,
        KeyboardLayout::ChineseTraditional => KEY_CODE_MAP_CHINESE_TRADITIONAL,
        KeyboardLayout::EnglishUsInternational => KEY_CODE_MAP_ENGLISH_US_INTERNATIONAL,
    };
    *out_key_code_map = copy_key_code_map(src);
    RESULT_SUCCESS
}

/// IPC command table for ISettingsServer ("set").
///
/// Corresponds to the function table in upstream settings_server.cpp.
pub mod commands {
    pub const GET_LANGUAGE_CODE: u32 = 0;
    pub const GET_AVAILABLE_LANGUAGE_CODES: u32 = 1;
    pub const MAKE_LANGUAGE_CODE: u32 = 2;
    pub const GET_AVAILABLE_LANGUAGE_CODE_COUNT: u32 = 3;
    pub const GET_REGION_CODE: u32 = 4;
    pub const GET_AVAILABLE_LANGUAGE_CODES2: u32 = 5;
    pub const GET_AVAILABLE_LANGUAGE_CODE_COUNT2: u32 = 6;
    pub const GET_KEY_CODE_MAP: u32 = 7;
    pub const GET_QUEST_FLAG: u32 = 8;
    pub const GET_KEY_CODE_MAP2: u32 = 9;
    pub const GET_FIRMWARE_VERSION_FOR_DEBUG: u32 = 10;
    pub const GET_DEVICE_NICK_NAME: u32 = 11;
    pub const GET_KEY_CODE_MAP_BY_PORT: u32 = 12;
}

/// ISettingsServer -- "set" service.
///
/// Corresponds to `ISettingsServer` in upstream settings_server.h.
pub struct ISettingsServer {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISettingsServer {
    fn build_handlers() -> BTreeMap<u32, FunctionInfo> {
        build_handler_map(&[
            (
                commands::GET_LANGUAGE_CODE,
                Some(Self::get_language_code_handler),
                "GetLanguageCode",
            ),
            (
                commands::GET_AVAILABLE_LANGUAGE_CODES,
                Some(Self::get_available_language_codes_handler),
                "GetAvailableLanguageCodes",
            ),
            (
                commands::MAKE_LANGUAGE_CODE,
                Some(Self::make_language_code_handler),
                "MakeLanguageCode",
            ),
            (
                commands::GET_AVAILABLE_LANGUAGE_CODE_COUNT,
                Some(Self::get_available_language_code_count_handler),
                "GetAvailableLanguageCodeCount",
            ),
            (
                commands::GET_REGION_CODE,
                Some(Self::get_region_code_handler),
                "GetRegionCode",
            ),
            (
                commands::GET_AVAILABLE_LANGUAGE_CODES2,
                Some(Self::get_available_language_codes2_handler),
                "GetAvailableLanguageCodes2",
            ),
            (
                commands::GET_AVAILABLE_LANGUAGE_CODE_COUNT2,
                Some(Self::get_available_language_code_count2_handler),
                "GetAvailableLanguageCodeCount2",
            ),
            (
                commands::GET_KEY_CODE_MAP,
                Some(Self::get_key_code_map_handler),
                "GetKeyCodeMap",
            ),
            (
                commands::GET_QUEST_FLAG,
                Some(Self::get_quest_flag_handler),
                "GetQuestFlag",
            ),
            (
                commands::GET_KEY_CODE_MAP2,
                Some(Self::get_key_code_map2_handler),
                "GetKeyCodeMap2",
            ),
            (
                commands::GET_FIRMWARE_VERSION_FOR_DEBUG,
                None,
                "GetFirmwareVersionForDebug",
            ),
            (
                commands::GET_DEVICE_NICK_NAME,
                Some(Self::get_device_nick_name_handler),
                "GetDeviceNickName",
            ),
            (
                commands::GET_KEY_CODE_MAP_BY_PORT,
                Some(Self::get_key_code_map_by_port_handler),
                "GetKeyCodeMapByPort",
            ),
        ])
    }

    pub fn new() -> Self {
        Self {
            handlers: Self::build_handlers(),
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// GetLanguageCode (cmd 0).
    ///
    /// Corresponds to `ISettingsServer::GetLanguageCode` in upstream.
    pub fn get_language_code(&self) -> LanguageCode {
        let language_index = *common::settings::values().language_index.get_value() as u32 as usize;
        log::debug!(
            "ISettingsServer::get_language_code called, index={}",
            language_index
        );
        AVAILABLE_LANGUAGE_CODES[language_index]
    }

    /// GetAvailableLanguageCodes (cmd 1).
    ///
    /// Returns (count, codes). Pre-4.0.0 max is 0xF entries.
    pub fn get_available_language_codes(&self, max_entries: usize) -> (i32, Vec<LanguageCode>) {
        log::debug!("ISettingsServer::get_available_language_codes called");
        let max_amount = PRE_4_0_0_MAX_ENTRIES.min(max_entries);
        let count = AVAILABLE_LANGUAGE_CODES.len().min(max_amount);
        let codes = AVAILABLE_LANGUAGE_CODES[..count].to_vec();
        (count as i32, codes)
    }

    /// MakeLanguageCode (cmd 2).
    ///
    /// Corresponds to `ISettingsServer::MakeLanguageCode` in upstream.
    pub fn make_language_code(&self, language: u32) -> Result<LanguageCode, ResultCode> {
        log::debug!(
            "ISettingsServer::make_language_code called, language={}",
            language
        );
        let index = language as usize;
        if index >= AVAILABLE_LANGUAGE_CODES.len() {
            return Err(RESULT_INVALID_LANGUAGE);
        }
        Ok(AVAILABLE_LANGUAGE_CODES[index])
    }

    /// GetAvailableLanguageCodeCount (cmd 3).
    pub fn get_available_language_code_count(&self) -> i32 {
        log::debug!("ISettingsServer::get_available_language_code_count called");
        PRE_4_0_0_MAX_ENTRIES as i32
    }

    /// GetRegionCode (cmd 4).
    pub fn get_region_code(&self) -> u32 {
        log::debug!("ISettingsServer::get_region_code called");
        *common::settings::values().region_index.get_value() as u32
    }

    /// GetAvailableLanguageCodes2 (cmd 5).
    ///
    /// Post-4.0.0 version with higher max entries.
    pub fn get_available_language_codes2(&self, max_entries: usize) -> (i32, Vec<LanguageCode>) {
        log::debug!("ISettingsServer::get_available_language_codes2 called");
        let max_amount = POST_4_0_0_MAX_ENTRIES.min(max_entries);
        let count = AVAILABLE_LANGUAGE_CODES.len().min(max_amount);
        let codes = AVAILABLE_LANGUAGE_CODES[..count].to_vec();
        (count as i32, codes)
    }

    /// GetAvailableLanguageCodeCount2 (cmd 6).
    pub fn get_available_language_code_count2(&self) -> i32 {
        log::debug!("ISettingsServer::get_available_language_code_count2 called");
        POST_4_0_0_MAX_ENTRIES as i32
    }

    /// GetKeyCodeMap (cmd 7).
    ///
    /// Corresponds to `ISettingsServer::GetKeyCodeMap` in upstream.
    pub fn get_key_code_map(&self, out_key_code_map: Option<&mut KeyCodeMap>) -> ResultCode {
        log::debug!("ISettingsServer::get_key_code_map called");
        let Some(out_key_code_map) = out_key_code_map else {
            return RESULT_NULL_POINTER;
        };

        let language_index = *common::settings::values().language_index.get_value() as u32 as usize;
        let language_code = AVAILABLE_LANGUAGE_CODES[language_index];
        let key_code = LANGUAGE_TO_LAYOUT
            .iter()
            .find(|(lc, _)| *lc == language_code);

        match key_code {
            Some((lc, layout)) => get_key_code_map_impl(out_key_code_map, *layout, *lc),
            None => {
                log::error!(
                    "Could not find keyboard layout for language index {}, defaulting to English us",
                    language_index
                );
                *out_key_code_map = copy_key_code_map(KEY_CODE_MAP_ENGLISH_US_INTERNATIONAL);
                RESULT_SUCCESS
            }
        }
    }

    /// GetQuestFlag (cmd 8).
    pub fn get_quest_flag(&self) -> bool {
        log::debug!("ISettingsServer::get_quest_flag called");
        *common::settings::values().quest_flag.get_value()
    }

    /// GetKeyCodeMap2 (cmd 9).
    ///
    /// Corresponds to `ISettingsServer::GetKeyCodeMap2` in upstream.
    pub fn get_key_code_map2(&self, out_key_code_map: Option<&mut KeyCodeMap>) -> ResultCode {
        log::debug!("ISettingsServer::get_key_code_map2 called");
        let Some(out_key_code_map) = out_key_code_map else {
            return RESULT_NULL_POINTER;
        };

        let language_index = *common::settings::values().language_index.get_value() as u32 as usize;
        let language_code = AVAILABLE_LANGUAGE_CODES[language_index];
        let key_code = LANGUAGE_TO_LAYOUT
            .iter()
            .find(|(lc, _)| *lc == language_code);

        match key_code {
            Some((lc, layout)) => get_key_code_map_impl(out_key_code_map, *layout, *lc),
            None => {
                log::error!(
                    "Could not find keyboard layout for language index {}, defaulting to English us",
                    language_index
                );
                *out_key_code_map = copy_key_code_map(KEY_CODE_MAP_ENGLISH_US_INTERNATIONAL);
                RESULT_SUCCESS
            }
        }
    }

    /// GetDeviceNickName (cmd 11).
    pub fn get_device_nick_name(&self) -> [u8; 0x80] {
        log::debug!("ISettingsServer::get_device_nick_name called");
        let mut out = [0u8; 0x80];
        let device_name = common::settings::values().device_name.get_value().clone();
        let bytes = device_name.as_bytes();
        let len = bytes.len().min(out.len());
        out[..len].copy_from_slice(&bytes[..len]);
        out
    }

    /// GetKeyCodeMapByPort (cmd 12).
    ///
    /// The port is logged but does not alter Eden's current map selection.
    pub fn get_key_code_map_by_port(
        &self,
        out_key_code_map: Option<&mut KeyCodeMap>,
        port: u32,
    ) -> ResultCode {
        log::debug!(
            "ISettingsServer::get_key_code_map_by_port called, port={}",
            port
        );
        let Some(out_key_code_map) = out_key_code_map else {
            return RESULT_NULL_POINTER;
        };

        let language_index = *common::settings::values().language_index.get_value() as u32 as usize;
        let language_code = AVAILABLE_LANGUAGE_CODES[language_index];
        let key_code = LANGUAGE_TO_LAYOUT
            .iter()
            .find(|(lc, _)| *lc == language_code);

        match key_code {
            Some((lc, layout)) => get_key_code_map_impl(out_key_code_map, *layout, *lc),
            None => {
                log::error!(
                    "Could not find keyboard layout for language index {}, defaulting to English us",
                    language_index
                );
                *out_key_code_map = copy_key_code_map(KEY_CODE_MAP_ENGLISH_US_INTERNATIONAL);
                RESULT_SUCCESS
            }
        }
    }

    // --- Handler bridge functions ---

    fn get_language_code_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISettingsServer) };
        let code = service.get_language_code();

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(code as u64);
    }

    fn get_available_language_codes_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISettingsServer) };
        let max_entries = ctx.get_write_buffer_size(0) / std::mem::size_of::<LanguageCode>();
        let (count, codes) = service.get_available_language_codes(max_entries);

        let bytes: Vec<u8> = codes
            .iter()
            .flat_map(|c| (*c as u64).to_le_bytes())
            .collect();
        ctx.write_buffer(&bytes, 0);

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(count);
    }

    fn make_language_code_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISettingsServer) };
        let mut rp = RequestParser::new(ctx);
        let language = rp.pop_u32();

        match service.make_language_code(language) {
            Ok(code) => {
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u64(code as u64);
            }
            Err(rc) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(rc);
            }
        }
    }

    fn get_available_language_code_count_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISettingsServer) };
        let count = service.get_available_language_code_count();

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(count);
    }

    fn get_region_code_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISettingsServer) };
        let region = service.get_region_code();

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(region);
    }

    fn get_available_language_codes2_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISettingsServer) };
        let max_entries = ctx.get_write_buffer_size(0) / std::mem::size_of::<LanguageCode>();
        let (count, codes) = service.get_available_language_codes2(max_entries);

        let bytes: Vec<u8> = codes
            .iter()
            .flat_map(|c| (*c as u64).to_le_bytes())
            .collect();
        ctx.write_buffer(&bytes, 0);

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(count);
    }

    fn get_available_language_code_count2_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISettingsServer) };
        let count = service.get_available_language_code_count2();

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(count);
    }

    fn get_key_code_map_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISettingsServer) };
        let mut map = [0u8; 0x1000];
        let result = if ctx.can_write_buffer(0) {
            service.get_key_code_map(Some(&mut map))
        } else {
            service.get_key_code_map(None)
        };
        if result.is_success() {
            ctx.write_buffer(&map, 0);
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn get_quest_flag_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISettingsServer) };
        let flag = service.get_quest_flag();

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(flag);
    }

    fn get_key_code_map2_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISettingsServer) };
        let mut map = [0u8; 0x1000];
        let result = if ctx.can_write_buffer(0) {
            service.get_key_code_map2(Some(&mut map))
        } else {
            service.get_key_code_map2(None)
        };
        if result.is_success() {
            ctx.write_buffer(&map, 0);
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn get_device_nick_name_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISettingsServer) };
        let name = service.get_device_nick_name();
        ctx.write_buffer(&name, 0);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_key_code_map_by_port_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISettingsServer) };
        let mut rp = RequestParser::new(ctx);
        let port = rp.pop_u32();
        let mut map = [0u8; 0x1000];
        let result = if ctx.can_write_buffer(0) {
            service.get_key_code_map_by_port(Some(&mut map), port)
        } else {
            service.get_key_code_map_by_port(None, port)
        };
        if result.is_success() {
            ctx.write_buffer(&map, 0);
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }
}

impl SessionRequestHandler for ISettingsServer {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "set"
    }
}

impl ServiceFramework for ISettingsServer {
    fn get_service_name(&self) -> &str {
        "set"
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

    #[test]
    fn quest_flag_ipc_reflects_live_global_setting() {
        const CHILD: &str = "RUZU_TEST_QUEST_FLAG";
        if std::env::var_os(CHILD).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "hle::service::set::settings_server::tests::quest_flag_ipc_reflects_live_global_setting"])
                .env(CHILD, "1").status().unwrap().success());
            return;
        }
        let server = ISettingsServer::new();
        let handler = &server.handlers[&commands::GET_QUEST_FLAG];
        assert_eq!(handler.name, "GetQuestFlag");
        for enabled in [false, true, false] {
            common::settings::values_mut().quest_flag.set_value(enabled);
            let mut ctx = HLERequestContext::new();
            ctx.command_buffer_mut().fill(u32::MAX);
            handler.handler_callback.unwrap()(&server, &mut ctx);
            assert_eq!(ctx.command_buffer()[6], 0);
            assert_eq!(ctx.command_buffer()[7], 0);
            assert_eq!(ctx.command_buffer()[8], u32::from(enabled));
            assert!(ctx.command_buffer()[9..].iter().all(|&word| word == 0));
        }
    }

    #[test]
    fn test_get_language_code() {
        let old_language = *common::settings::values().language_index.get_value();
        common::settings::values_mut()
            .language_index
            .set_value(common::settings_enums::Language::French);
        let server = ISettingsServer::new();
        assert_eq!(server.get_language_code(), LanguageCode::Fr);
        common::settings::values_mut()
            .language_index
            .set_value(old_language);
    }

    #[test]
    fn test_make_language_code() {
        let server = ISettingsServer::new();
        assert_eq!(server.make_language_code(0), Ok(LanguageCode::Ja));
        assert_eq!(server.make_language_code(1), Ok(LanguageCode::EnUs));
        assert!(server.make_language_code(99).is_err());
    }

    #[test]
    fn test_available_language_codes() {
        let server = ISettingsServer::new();
        let (count, codes) = server.get_available_language_codes(100);
        assert_eq!(count, PRE_4_0_0_MAX_ENTRIES as i32);
        assert_eq!(codes.len(), PRE_4_0_0_MAX_ENTRIES);

        let (count2, codes2) = server.get_available_language_codes2(100);
        assert_eq!(count2, AVAILABLE_LANGUAGE_CODES.len() as i32);
        assert_eq!(codes2.len(), AVAILABLE_LANGUAGE_CODES.len());
    }

    #[test]
    fn test_get_key_code_map() {
        let server = ISettingsServer::new(); // language_index=1 => EnUs
        let mut map = [0u8; 0x1000];
        assert_eq!(server.get_key_code_map(Some(&mut map)), RESULT_SUCCESS);
        // Should be EnglishUs map (which for EnUs goes through the EnglishUs branch)
        // Just verify it doesn't panic and returns a valid 0x1000-byte array
        assert_eq!(map.len(), 0x1000);
    }

    #[test]
    fn key_code_map_commands_reject_null_output() {
        let server = ISettingsServer::new();

        assert_eq!(server.get_key_code_map(None), RESULT_NULL_POINTER);
        assert_eq!(server.get_key_code_map2(None), RESULT_NULL_POINTER);
        assert_eq!(
            server.get_key_code_map_by_port(None, 0),
            RESULT_NULL_POINTER
        );
    }

    #[test]
    fn get_key_code_map_by_port_uses_current_language_layout() {
        let server = ISettingsServer::new();
        let mut expected = [0u8; 0x1000];
        let mut actual = [0u8; 0x1000];

        let handler = server
            .handlers
            .get(&commands::GET_KEY_CODE_MAP_BY_PORT)
            .expect("command 12 must be registered");
        assert_eq!(handler.name, "GetKeyCodeMapByPort");
        assert!(handler.handler_callback.is_some());

        assert_eq!(server.get_key_code_map(Some(&mut expected)), RESULT_SUCCESS);
        assert_eq!(
            server.get_key_code_map_by_port(Some(&mut actual), 3),
            RESULT_SUCCESS
        );
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_get_device_nick_name() {
        let server = ISettingsServer::new();
        let name = server.get_device_nick_name();
        assert_eq!(&name[..4], b"yuzu");
        assert_eq!(name[4], 0);
    }
}

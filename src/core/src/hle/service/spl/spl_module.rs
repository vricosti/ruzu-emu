// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/spl/spl_module.h
//! Port of zuyu/src/core/hle/service/spl/spl_module.cpp
//!
//! Module::Interface — SPL general service interface with GetConfig implementation.

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::mt19937::Mt19937;
use super::spl_results;
use super::spl_types::{AccessKey, AesKey, ConfigItem, KeySource};
use crate::hle::api_version;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for Module::Interface (IGeneralInterface).
pub mod commands {
    pub const GET_CONFIG: u32 = 0;
    pub const MODULAR_EXPONENTIATE: u32 = 1;
    pub const SET_CONFIG: u32 = 5;
    pub const GENERATE_RANDOM_BYTES: u32 = 7;
    pub const IS_DEVELOPMENT: u32 = 11;
    pub const SET_BOOT_REASON: u32 = 24;
    pub const GET_BOOT_REASON: u32 = 25;
}

/// Module::Interface — SPL general interface.
///
/// Corresponds to `Module::Interface` in upstream spl_module.h / spl_module.cpp.
///
/// Upstream owns a `std::mt19937 rng` member that advances across
/// `GenerateRandomBytes` calls. We mirror that with a persistent
/// `Mutex<Mt19937>`; resetting the state from the seed on each call — as
/// the original port did — caused consecutive RNG calls to return the same bytes.
pub struct ModuleInterface {
    name: String,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    rng: Mutex<Mt19937>,
}

impl ModuleInterface {
    pub fn new(name: &str, rng_seed: Option<u32>) -> Self {
        let seed = rng_seed.unwrap_or_else(|| {
            let settings = common::settings::values();
            if *settings.rng_seed_enabled.get_value() {
                return *settings.rng_seed.get_value();
            }
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as u32)
                .unwrap_or(0)
        });

        let handlers = build_handler_map(&[
            (0, Some(Self::get_config_callback), "GetConfig"),
            (1, Some(Self::modular_exponentiate_callback), "ModularExponentiate"),
            (5, Some(Self::set_config_callback), "SetConfig"),
            (7, Some(Self::generate_random_bytes_callback), "GenerateRandomBytes"),
            (11, Some(Self::is_development_callback), "IsDevelopment"),
            (24, Some(Self::set_boot_reason_callback), "SetBootReason"),
            (25, Some(Self::get_boot_reason_callback), "GetBootReason"),
        ]);

        Self {
            name: name.to_string(),
            handlers,
            handlers_tipc: BTreeMap::new(),
            rng: Mutex::new(Mt19937::new(seed)),
        }
    }

    fn get_config_callback(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        service.get_config_handler(ctx);
    }

    fn modular_exponentiate_callback(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let result = service.modular_exponentiate();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn set_config_callback(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let result = service.set_config();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn generate_random_bytes_callback(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        service.generate_random_bytes_handler(ctx);
    }

    fn is_development_callback(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let result = service.is_development();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn set_boot_reason_callback(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let result = service.set_boot_reason();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn get_boot_reason_callback(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let result = service.get_boot_reason();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    /// GetConfig (cmd 0).
    ///
    /// Corresponds to `Module::Interface::GetConfig` in upstream spl_module.cpp.
    pub fn get_config(&self, config_item_raw: u32) -> Result<u64, ResultCode> {
        let config_item = ConfigItem::from_u32(config_item_raw);

        match config_item {
            Some(ConfigItem::DisableProgramVerification)
            | Some(ConfigItem::DramId)
            | Some(ConfigItem::SecurityEngineInterruptNumber)
            | Some(ConfigItem::FuseVersion)
            | Some(ConfigItem::HardwareType)
            | Some(ConfigItem::HardwareState)
            | Some(ConfigItem::IsRecoveryBoot)
            | Some(ConfigItem::DeviceId)
            | Some(ConfigItem::BootReason)
            | Some(ConfigItem::MemoryMode)
            | Some(ConfigItem::IsDevelopmentFunctionEnabled)
            | Some(ConfigItem::KernelConfiguration)
            | Some(ConfigItem::IsChargerHiZModeEnabled)
            | Some(ConfigItem::QuestState)
            | Some(ConfigItem::RegulatorType)
            | Some(ConfigItem::DeviceUniqueKeyGeneration)
            | Some(ConfigItem::Package2Hash) => {
                log::error!("GetConfig: config_item={:?} not implemented", config_item);
                Err(spl_results::RESULT_SECURE_MONITOR_NOT_IMPLEMENTED)
            }
            Some(ConfigItem::ExosphereApiVersion) => {
                // Get information about the current exosphere version.
                let value = (u64::from(api_version::ATMOSPHERE_RELEASE_VERSION_MAJOR) << 56)
                    | (u64::from(api_version::ATMOSPHERE_RELEASE_VERSION_MINOR) << 48)
                    | (u64::from(api_version::ATMOSPHERE_RELEASE_VERSION_MICRO) << 40)
                    | u64::from(api_version::get_target_firmware());
                Ok(value)
            }
            Some(ConfigItem::ExosphereNeedsReboot) => {
                // We are executing, so we aren't in the process of rebooting.
                Ok(0)
            }
            Some(ConfigItem::ExosphereNeedsShutdown) => {
                // We are executing, so we aren't in the process of shutting down.
                Ok(0)
            }
            Some(ConfigItem::ExosphereGitCommitHash) => Ok(0),
            Some(ConfigItem::ExosphereHasRcmBugPatch) => Ok(0),
            Some(ConfigItem::ExosphereBlankProdInfo) => Ok(0),
            Some(ConfigItem::ExosphereAllowCalWrites) => Ok(0),
            Some(ConfigItem::ExosphereEmummcType) => Ok(0),
            Some(ConfigItem::ExospherePayloadAddress) => {
                // Gets the physical address of the reboot payload buffer, if one exists.
                Err(spl_results::RESULT_SECURE_MONITOR_NOT_INITIALIZED)
            }
            Some(ConfigItem::ExosphereLogConfiguration) => Ok(0),
            Some(ConfigItem::ExosphereForceEnableUsb30) => Ok(0),
            None => {
                log::error!("GetConfig: unknown config_item={}", config_item_raw);
                Err(spl_results::RESULT_SECURE_MONITOR_INVALID_ARGUMENT)
            }
        }
    }

    /// IPC adapter for upstream Module::Interface::GetConfig.
    pub fn get_config_handler(&self, ctx: &mut HLERequestContext) {
        let item = RequestParser::new(ctx).pop_u32();
        let (result, value) = match self.get_config(item) {
            Ok(value) => (RESULT_SUCCESS, value),
            Err(result) => {
                log::error!("GetConfig: item={item}, result={result:?}");
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
                (result, 0)
            }
        };
        // Upstream does not return after its error builder: the final response
        // always includes the zero-initialized secure-monitor output as well.
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(result);
        rb.push_u64(value);
    }

    pub fn generate_aes_kek(&self, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _source = rp.pop_raw::<KeySource>();
        let generation = rp.pop_u32();
        let option = rp.pop_u32();
        log::warn!("(STUBBED) GenerateAesKek: generation={generation:#x}, option={option:#x}");
        let key = AccessKey::default();
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&key);
    }

    pub fn generate_aes_key(&self, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _access_key = rp.pop_raw::<AccessKey>();
        let _source = rp.pop_raw::<KeySource>();
        log::warn!("(STUBBED) GenerateAesKey called");
        let key = AesKey::default();
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&key);
    }

    /// ModularExponentiate (cmd 1).
    pub fn modular_exponentiate(&self) -> ResultCode {
        log::warn!("ModularExponentiate is not implemented!");
        common::assert::assert_fail_soft_impl();
        spl_results::RESULT_SECURE_MONITOR_NOT_IMPLEMENTED
    }

    /// SetConfig (cmd 5).
    pub fn set_config(&self) -> ResultCode {
        log::warn!("SetConfig is not implemented!");
        common::assert::assert_fail_soft_impl();
        spl_results::RESULT_SECURE_MONITOR_NOT_IMPLEMENTED
    }

    /// GenerateRandomBytes (cmd 7).
    ///
    /// Corresponds to `Module::Interface::GenerateRandomBytes` in upstream.
    /// Upstream draws one 32-bit value per byte from a persistent
    /// `std::mt19937` through `std::uniform_int_distribution<u16>(0, 0xFF)`.
    /// A full-range 32-bit MT output reduced to 256 values uses the high
    /// eight bits with libstdc++, matching the existing CSRNG distribution.
    pub fn generate_random_bytes(&self, buf: &mut [u8]) {
        log::debug!("GenerateRandomBytes called, size={}", buf.len());
        let mut rng = self.rng.lock().unwrap();
        for byte in buf.iter_mut() {
            *byte = (rng.next_u32() >> 24) as u8;
        }
    }

    /// IPC adapter for upstream Module::Interface::GenerateRandomBytes.
    pub fn generate_random_bytes_handler(&self, ctx: &mut HLERequestContext) {
        let mut data = vec![0; ctx.get_write_buffer_size(0)];
        self.generate_random_bytes(&mut data);
        ctx.write_buffer(&data, 0);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// IsDevelopment (cmd 11).
    pub fn is_development(&self) -> ResultCode {
        log::warn!("IsDevelopment is not implemented!");
        common::assert::assert_fail_soft_impl();
        spl_results::RESULT_SECURE_MONITOR_NOT_IMPLEMENTED
    }

    /// SetBootReason (cmd 24).
    pub fn set_boot_reason(&self) -> ResultCode {
        log::warn!("SetBootReason is not implemented!");
        common::assert::assert_fail_soft_impl();
        spl_results::RESULT_SECURE_MONITOR_NOT_IMPLEMENTED
    }

    /// GetBootReason (cmd 25).
    pub fn get_boot_reason(&self) -> ResultCode {
        log::warn!("GetBootReason is not implemented!");
        common::assert::assert_fail_soft_impl();
        spl_results::RESULT_SECURE_MONITOR_NOT_IMPLEMENTED
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_reply_keeps_zero_output_on_secure_monitor_errors() {
        let service = ModuleInterface::new("spl:", Some(0));
        for (item, result) in [
            (ConfigItem::ExosphereNeedsReboot as u32, RESULT_SUCCESS),
            (ConfigItem::ExospherePayloadAddress as u32, spl_results::RESULT_SECURE_MONITOR_NOT_INITIALIZED),
            (ConfigItem::DramId as u32, spl_results::RESULT_SECURE_MONITOR_NOT_IMPLEMENTED),
            (u32::MAX, spl_results::RESULT_SECURE_MONITOR_INVALID_ARGUMENT),
        ] {
            let mut ctx = HLERequestContext::new();
            ctx.cmd_buf.fill(u32::MAX);
            ctx.cmd_buf[2] = item;
            service.handlers()[&0].handler_callback.unwrap()(&service, &mut ctx);
            assert_eq!(ctx.cmd_buf[6], result.get_inner_value());
            assert_eq!(&ctx.cmd_buf[7..10], &[0, 0, 0]);
        }
    }

    #[test]
    fn migration_aes_stubs_return_full_zeroed_keys() {
        let service = ModuleInterface::new("spl:mig", Some(0));
        for handler in [ModuleInterface::generate_aes_kek, ModuleInterface::generate_aes_key] {
            let mut ctx = HLERequestContext::new();
            ctx.cmd_buf.fill(u32::MAX);
            handler(&service, &mut ctx);
            assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
            assert_eq!(&ctx.cmd_buf[7..12], &[0; 5]);
            assert!(ctx.outgoing_copy_objects.is_empty());
        }
    }

    #[test]
    fn base_command_table_registers_only_general_commands() {
        let service = ModuleInterface::new("spl:", Some(0));
        assert_eq!(service.handlers().keys().copied().collect::<Vec<_>>(), [0, 1, 5, 7, 11, 24, 25]);
        assert!(service.handlers().values().all(|entry| entry.handler_callback.is_some()));
    }

    #[test]
    fn exosphere_version_uses_the_shared_api_version_owner() {
        let service = ModuleInterface::new("spl:", Some(0));
        let value = service.get_config(ConfigItem::ExosphereApiVersion as u32).unwrap();
        assert_eq!(value as u32, api_version::get_target_firmware());
        assert_eq!((value >> 56) as u8, api_version::ATMOSPHERE_RELEASE_VERSION_MAJOR);
        assert_eq!((value >> 48) as u8, api_version::ATMOSPHERE_RELEASE_VERSION_MINOR);
        assert_eq!((value >> 40) as u8, api_version::ATMOSPHERE_RELEASE_VERSION_MICRO);
        assert_eq!((value >> 32) as u8, 0);
    }
}

impl SessionRequestHandler for ModuleInterface {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        &self.name
    }
}

impl ServiceFramework for ModuleInterface {
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

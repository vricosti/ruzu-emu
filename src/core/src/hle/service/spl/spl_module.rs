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
use super::spl_types::ConfigItem;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// Atmosphere release version constants.
///
/// Corresponds to `HLE::ApiVersion::ATMOSPHERE_RELEASE_VERSION_*` in upstream.
const ATMOSPHERE_RELEASE_VERSION_MAJOR: u64 = 1;
const ATMOSPHERE_RELEASE_VERSION_MINOR: u64 = 0;
const ATMOSPHERE_RELEASE_VERSION_MICRO: u64 = 0;

/// Target firmware version (placeholder).
const TARGET_FIRMWARE: u64 = 0x0E0000000; // ~14.0.0

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
            (0, None, "GetConfig"),
            (1, None, "ModularExponentiate"),
            (5, None, "SetConfig"),
            (7, None, "GenerateRandomBytes"),
            (11, None, "IsDevelopment"),
            (24, None, "SetBootReason"),
            (25, None, "GetBootReason"),
        ]);

        Self {
            name: name.to_string(),
            handlers,
            handlers_tipc: BTreeMap::new(),
            rng: Mutex::new(Mt19937::new(seed)),
        }
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
                let value = (ATMOSPHERE_RELEASE_VERSION_MAJOR << 56)
                    | (ATMOSPHERE_RELEASE_VERSION_MINOR << 48)
                    | (ATMOSPHERE_RELEASE_VERSION_MICRO << 40)
                    | TARGET_FIRMWARE;
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

    /// ModularExponentiate (cmd 1).
    pub fn modular_exponentiate(&self) -> ResultCode {
        log::warn!("ModularExponentiate is not implemented!");
        spl_results::RESULT_SECURE_MONITOR_NOT_IMPLEMENTED
    }

    /// SetConfig (cmd 5).
    pub fn set_config(&self) -> ResultCode {
        log::warn!("SetConfig is not implemented!");
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
        spl_results::RESULT_SECURE_MONITOR_NOT_IMPLEMENTED
    }

    /// SetBootReason (cmd 24).
    pub fn set_boot_reason(&self) -> ResultCode {
        log::warn!("SetBootReason is not implemented!");
        spl_results::RESULT_SECURE_MONITOR_NOT_IMPLEMENTED
    }

    /// GetBootReason (cmd 25).
    pub fn get_boot_reason(&self) -> ResultCode {
        log::warn!("GetBootReason is not implemented!");
        spl_results::RESULT_SECURE_MONITOR_NOT_IMPLEMENTED
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

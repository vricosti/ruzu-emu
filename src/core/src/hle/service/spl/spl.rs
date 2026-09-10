// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/spl/spl.h and spl.cpp
//!
//! SPL service variants; process registration belongs to spl_module.rs.
//!
//! Upstream defines: SPL, SPL_MIG, SPL_FS, SPL_SSL, SPL_ES, SPL_MANU
//! Each is a Module::Interface with a different IPC function table.

use super::spl_module::ModuleInterface;
use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::FunctionInfo;

/// IPC function tables for each SPL service variant.
///
/// Corresponds to the function tables in upstream spl.cpp.
pub mod spl_functions {
    use super::{FunctionInfo, ModuleInterface};
    /// "spl:" -- basic SPL interface.
    pub const SPL_COMMANDS: &[FunctionInfo] = &[
        FunctionInfo::new(0, Some(ModuleInterface::get_config_callback), "GetConfig"),
        FunctionInfo::new(1, Some(ModuleInterface::modular_exponentiate_callback), "ModularExponentiate"),
        FunctionInfo::new(5, Some(ModuleInterface::set_config_callback), "SetConfig"),
        FunctionInfo::new(7, Some(ModuleInterface::generate_random_bytes_callback), "GenerateRandomBytes"),
        FunctionInfo::new(11, Some(ModuleInterface::is_development_callback), "IsDevelopment"),
        FunctionInfo::new(24, Some(ModuleInterface::set_boot_reason_callback), "SetBootReason"),
        FunctionInfo::new(25, Some(ModuleInterface::get_boot_reason_callback), "GetBootReason"),
    ];

    /// "spl:mig" -- migration SPL interface.
    pub const SPL_MIG_COMMANDS: &[FunctionInfo] = &[
        FunctionInfo::new(0, Some(ModuleInterface::get_config_callback), "GetConfig"),
        FunctionInfo::new(1, Some(ModuleInterface::modular_exponentiate_callback), "ModularExponentiate"),
        FunctionInfo::new(2, Some(ModuleInterface::generate_aes_kek_callback), "GenerateAesKek"),
        FunctionInfo::new(3, None, "LoadAesKey"),
        FunctionInfo::new(4, Some(ModuleInterface::generate_aes_key_callback), "GenerateAesKey"),
        FunctionInfo::new(5, Some(ModuleInterface::set_config_callback), "SetConfig"),
        FunctionInfo::new(7, Some(ModuleInterface::generate_random_bytes_callback), "GenerateRandomBytes"),
        FunctionInfo::new(11, Some(ModuleInterface::is_development_callback), "IsDevelopment"),
        FunctionInfo::new(14, None, "DecryptAesKey"),
        FunctionInfo::new(15, None, "CryptAesCtr"),
        FunctionInfo::new(16, None, "ComputeCmac"),
        FunctionInfo::new(21, None, "AllocateAesKeyslot"),
        FunctionInfo::new(22, None, "DeallocateAesKeySlot"),
        FunctionInfo::new(23, None, "GetAesKeyslotAvailableEvent"),
        FunctionInfo::new(24, Some(ModuleInterface::set_boot_reason_callback), "SetBootReason"),
        FunctionInfo::new(25, Some(ModuleInterface::get_boot_reason_callback), "GetBootReason"),
    ];

    /// "spl:fs" -- filesystem SPL interface.
    pub const SPL_FS_COMMANDS: &[FunctionInfo] = &[
        FunctionInfo::new(0, Some(ModuleInterface::get_config_callback), "GetConfig"),
        FunctionInfo::new(1, Some(ModuleInterface::modular_exponentiate_callback), "ModularExponentiate"),
        FunctionInfo::new(2, None, "GenerateAesKek"),
        FunctionInfo::new(3, None, "LoadAesKey"),
        FunctionInfo::new(4, None, "GenerateAesKey"),
        FunctionInfo::new(5, Some(ModuleInterface::set_config_callback), "SetConfig"),
        FunctionInfo::new(7, Some(ModuleInterface::generate_random_bytes_callback), "GenerateRandomBytes"),
        FunctionInfo::new(9, None, "ImportLotusKey"),
        FunctionInfo::new(10, None, "DecryptLotusMessage"),
        FunctionInfo::new(11, Some(ModuleInterface::is_development_callback), "IsDevelopment"),
        FunctionInfo::new(12, None, "GenerateSpecificAesKey"),
        FunctionInfo::new(14, None, "DecryptAesKey"),
        FunctionInfo::new(15, None, "CryptAesCtr"),
        FunctionInfo::new(16, None, "ComputeCmac"),
        FunctionInfo::new(19, None, "LoadTitleKey"),
        FunctionInfo::new(21, None, "AllocateAesKeyslot"),
        FunctionInfo::new(22, None, "DeallocateAesKeySlot"),
        FunctionInfo::new(23, None, "GetAesKeyslotAvailableEvent"),
        FunctionInfo::new(24, Some(ModuleInterface::set_boot_reason_callback), "SetBootReason"),
        FunctionInfo::new(25, Some(ModuleInterface::get_boot_reason_callback), "GetBootReason"),
        FunctionInfo::new(31, None, "GetPackage2Hash"),
    ];

    /// "spl:ssl" -- SSL SPL interface.
    pub const SPL_SSL_COMMANDS: &[FunctionInfo] = &[
        FunctionInfo::new(0, Some(ModuleInterface::get_config_callback), "GetConfig"),
        FunctionInfo::new(1, Some(ModuleInterface::modular_exponentiate_callback), "ModularExponentiate"),
        FunctionInfo::new(2, None, "GenerateAesKek"),
        FunctionInfo::new(3, None, "LoadAesKey"),
        FunctionInfo::new(4, None, "GenerateAesKey"),
        FunctionInfo::new(5, Some(ModuleInterface::set_config_callback), "SetConfig"),
        FunctionInfo::new(7, Some(ModuleInterface::generate_random_bytes_callback), "GenerateRandomBytes"),
        FunctionInfo::new(11, Some(ModuleInterface::is_development_callback), "IsDevelopment"),
        FunctionInfo::new(13, None, "DecryptDeviceUniqueData"),
        FunctionInfo::new(14, None, "DecryptAesKey"),
        FunctionInfo::new(15, None, "CryptAesCtr"),
        FunctionInfo::new(16, None, "ComputeCmac"),
        FunctionInfo::new(21, None, "AllocateAesKeyslot"),
        FunctionInfo::new(22, None, "DeallocateAesKeySlot"),
        FunctionInfo::new(23, None, "GetAesKeyslotAvailableEvent"),
        FunctionInfo::new(24, Some(ModuleInterface::set_boot_reason_callback), "SetBootReason"),
        FunctionInfo::new(25, Some(ModuleInterface::get_boot_reason_callback), "GetBootReason"),
        FunctionInfo::new(26, None, "DecryptAndStoreSslClientCertKey"),
        FunctionInfo::new(27, None, "ModularExponentiateWithSslClientCertKey"),
    ];

    /// "spl:es" -- ES SPL interface.
    pub const SPL_ES_COMMANDS: &[FunctionInfo] = &[
        FunctionInfo::new(0, Some(ModuleInterface::get_config_callback), "GetConfig"),
        FunctionInfo::new(1, Some(ModuleInterface::modular_exponentiate_callback), "ModularExponentiate"),
        FunctionInfo::new(2, None, "GenerateAesKek"),
        FunctionInfo::new(3, None, "LoadAesKey"),
        FunctionInfo::new(4, None, "GenerateAesKey"),
        FunctionInfo::new(5, Some(ModuleInterface::set_config_callback), "SetConfig"),
        FunctionInfo::new(7, Some(ModuleInterface::generate_random_bytes_callback), "GenerateRandomBytes"),
        FunctionInfo::new(11, Some(ModuleInterface::is_development_callback), "IsDevelopment"),
        FunctionInfo::new(13, None, "DecryptDeviceUniqueData"),
        FunctionInfo::new(14, None, "DecryptAesKey"),
        FunctionInfo::new(15, None, "CryptAesCtr"),
        FunctionInfo::new(16, None, "ComputeCmac"),
        FunctionInfo::new(17, None, "ImportEsKey"),
        FunctionInfo::new(18, None, "UnwrapTitleKey"),
        FunctionInfo::new(20, None, "PrepareEsCommonKey"),
        FunctionInfo::new(21, None, "AllocateAesKeyslot"),
        FunctionInfo::new(22, None, "DeallocateAesKeySlot"),
        FunctionInfo::new(23, None, "GetAesKeyslotAvailableEvent"),
        FunctionInfo::new(24, Some(ModuleInterface::set_boot_reason_callback), "SetBootReason"),
        FunctionInfo::new(25, Some(ModuleInterface::get_boot_reason_callback), "GetBootReason"),
        FunctionInfo::new(28, None, "DecryptAndStoreDrmDeviceCertKey"),
        FunctionInfo::new(29, None, "ModularExponentiateWithDrmDeviceCertKey"),
        FunctionInfo::new(31, None, "PrepareEsArchiveKey"),
        FunctionInfo::new(32, None, "LoadPreparedAesKey"),
    ];

    /// "spl:manu" -- manufacturing SPL interface.
    pub const SPL_MANU_COMMANDS: &[FunctionInfo] = &[
        FunctionInfo::new(0, Some(ModuleInterface::get_config_callback), "GetConfig"),
        FunctionInfo::new(1, Some(ModuleInterface::modular_exponentiate_callback), "ModularExponentiate"),
        FunctionInfo::new(2, None, "GenerateAesKek"),
        FunctionInfo::new(3, None, "LoadAesKey"),
        FunctionInfo::new(4, None, "GenerateAesKey"),
        FunctionInfo::new(5, Some(ModuleInterface::set_config_callback), "SetConfig"),
        FunctionInfo::new(7, Some(ModuleInterface::generate_random_bytes_callback), "GenerateRandomBytes"),
        FunctionInfo::new(11, Some(ModuleInterface::is_development_callback), "IsDevelopment"),
        FunctionInfo::new(13, None, "DecryptDeviceUniqueData"),
        FunctionInfo::new(14, None, "DecryptAesKey"),
        FunctionInfo::new(15, None, "CryptAesCtr"),
        FunctionInfo::new(16, None, "ComputeCmac"),
        FunctionInfo::new(21, None, "AllocateAesKeyslot"),
        FunctionInfo::new(22, None, "DeallocateAesKeySlot"),
        FunctionInfo::new(23, None, "GetAesKeyslotAvailableEvent"),
        FunctionInfo::new(24, Some(ModuleInterface::set_boot_reason_callback), "SetBootReason"),
        FunctionInfo::new(25, Some(ModuleInterface::get_boot_reason_callback), "GetBootReason"),
        FunctionInfo::new(30, None, "ReencryptDeviceUniqueData"),
    ];
}

/// SPL service ("spl:").
///
/// Corresponds to `SPL` in upstream spl.h / spl.cpp.
pub struct Spl {
    // Composition replaces C++ inheritance; each interface owns its RNG.
    pub module: ModuleInterface,
}

impl Spl {
    pub fn new(rng_seed: Option<u32>) -> Self {
        let mut module = ModuleInterface::new("spl:", rng_seed);
        module.register_handlers(spl_functions::SPL_COMMANDS);
        Self { module }
    }
}

impl SessionRequestHandler for Spl {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.module.handle_sync_request(ctx)
    }

    fn service_name(&self) -> &str {
        self.module.service_name()
    }
}

/// SPL_MIG service ("spl:mig").
///
/// Corresponds to `SPL_MIG` in upstream spl.h.
pub struct SplMig {
    // Composition replaces C++ inheritance; each interface owns its RNG.
    pub module: ModuleInterface,
}

impl SplMig {
    pub fn new(rng_seed: Option<u32>) -> Self {
        let mut module = ModuleInterface::new("spl:mig", rng_seed);
        module.register_handlers(spl_functions::SPL_MIG_COMMANDS);
        Self { module }
    }
}

impl SessionRequestHandler for SplMig {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.module.handle_sync_request(ctx)
    }

    fn service_name(&self) -> &str {
        self.module.service_name()
    }
}

/// SPL_FS service ("spl:fs").
///
/// Corresponds to `SPL_FS` in upstream spl.h.
pub struct SplFs {
    // Composition replaces C++ inheritance; each interface owns its RNG.
    pub module: ModuleInterface,
}

impl SplFs {
    pub fn new(rng_seed: Option<u32>) -> Self {
        let mut module = ModuleInterface::new("spl:fs", rng_seed);
        module.register_handlers(spl_functions::SPL_FS_COMMANDS);
        Self { module }
    }
}

impl SessionRequestHandler for SplFs {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.module.handle_sync_request(ctx)
    }

    fn service_name(&self) -> &str {
        self.module.service_name()
    }
}

/// SPL_SSL service ("spl:ssl").
///
/// Corresponds to `SPL_SSL` in upstream spl.h.
pub struct SplSsl {
    // Composition replaces C++ inheritance; each interface owns its RNG.
    pub module: ModuleInterface,
}

impl SplSsl {
    pub fn new(rng_seed: Option<u32>) -> Self {
        let mut module = ModuleInterface::new("spl:ssl", rng_seed);
        module.register_handlers(spl_functions::SPL_SSL_COMMANDS);
        Self { module }
    }
}

impl SessionRequestHandler for SplSsl {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.module.handle_sync_request(ctx)
    }

    fn service_name(&self) -> &str {
        self.module.service_name()
    }
}

/// SPL_ES service ("spl:es").
///
/// Corresponds to `SPL_ES` in upstream spl.h.
pub struct SplEs {
    // Composition replaces C++ inheritance; each interface owns its RNG.
    pub module: ModuleInterface,
}

impl SplEs {
    pub fn new(rng_seed: Option<u32>) -> Self {
        let mut module = ModuleInterface::new("spl:es", rng_seed);
        module.register_handlers(spl_functions::SPL_ES_COMMANDS);
        Self { module }
    }
}

impl SessionRequestHandler for SplEs {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.module.handle_sync_request(ctx)
    }

    fn service_name(&self) -> &str {
        self.module.service_name()
    }
}

/// SPL_MANU service ("spl:manu").
///
/// Corresponds to `SPL_MANU` in upstream spl.h.
pub struct SplManu {
    // Composition replaces C++ inheritance; each interface owns its RNG.
    pub module: ModuleInterface,
}

impl SplManu {
    pub fn new(rng_seed: Option<u32>) -> Self {
        let mut module = ModuleInterface::new("spl:manu", rng_seed);
        module.register_handlers(spl_functions::SPL_MANU_COMMANDS);
        Self { module }
    }
}

impl SessionRequestHandler for SplManu {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.module.handle_sync_request(ctx)
    }

    fn service_name(&self) -> &str {
        self.module.service_name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::result::RESULT_SUCCESS;
    use crate::hle::service::service::ServiceFramework;

    #[test]
    fn all_variant_constructors_use_the_effective_seed() {
        const CHILD: &str = "RUZU_SPL_VARIANT_SEED_TEST";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "hle::service::spl::spl::tests::all_variant_constructors_use_the_effective_seed"])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        for seed in [0, u32::MAX] {
            {
                let mut values = common::settings::values_mut();
                values.rng_seed_enabled.set_global(false);
                values.rng_seed_enabled.set_value(true);
                values.rng_seed.set_global(false);
                values.rng_seed.set_value(seed);
            }
            let reference = ModuleInterface::new("reference", Some(seed));
            let mut expected = [0; 32];
            reference.generate_random_bytes(&mut expected);
            for module in [
                Spl::new(None).module, SplMig::new(None).module,
                SplFs::new(None).module, SplSsl::new(None).module,
                SplEs::new(None).module, SplManu::new(None).module,
            ] {
                let mut actual = [0; 32];
                module.generate_random_bytes(&mut actual);
                assert_eq!(actual, expected, "{}", module.service_name());
            }
        }
    }

    #[test]
    fn variant_tables_and_rng_ownership_match_upstream() {
        let variants = [
            (Spl::new(Some(42)).module, "spl:", 7, false),
            (SplMig::new(Some(42)).module, "spl:mig", 16, true),
            (SplFs::new(Some(42)).module, "spl:fs", 21, false),
            (SplSsl::new(Some(42)).module, "spl:ssl", 19, false),
            (SplEs::new(Some(42)).module, "spl:es", 24, false),
            (SplManu::new(Some(42)).module, "spl:manu", 18, false),
        ];
        let reference = ModuleInterface::new("reference", Some(42));
        let mut expected = [0; 64];
        reference.generate_random_bytes(&mut expected);
        for (service, name, count, migration) in variants {
            assert_eq!(service.service_name(), name);
            assert_eq!(service.handlers().len(), count);
            for (&id, entry) in service.handlers() {
                let implemented = [0, 1, 5, 7, 11, 24, 25].contains(&id)
                    || (migration && [2, 4].contains(&id));
                assert_eq!(entry.handler_callback.is_some(), implemented, "{name} command {id}");
            }
            let mut actual = [0; 64];
            service.generate_random_bytes(&mut actual[..32]);
            service.generate_random_bytes(&mut actual[32..]);
            assert_eq!(actual, expected, "{name} must have its own advancing RNG");
            let mut ctx = HLERequestContext::new();
            service.handlers()[&7].handler_callback.unwrap()(&service, &mut ctx);
            assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
            if migration {
                for id in [2, 4] {
                    let mut ctx = HLERequestContext::new();
                    ctx.cmd_buf.fill(u32::MAX);
                    service.handlers()[&id].handler_callback.unwrap()(&service, &mut ctx);
                    assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
                    assert_eq!(&ctx.cmd_buf[8..12], &[0; 4]);
                }
            }
        }
    }

    #[test]
    fn variant_dispatch_delegates_to_the_owned_interface() {
        for service in [
            Box::new(Spl::new(Some(1))) as Box<dyn SessionRequestHandler>,
            Box::new(SplMig::new(Some(1))),
            Box::new(SplFs::new(Some(1))),
            Box::new(SplSsl::new(Some(1))),
            Box::new(SplEs::new(Some(1))),
            Box::new(SplManu::new(Some(1))),
        ] {
            let mut ctx = HLERequestContext::new();
            ctx.cmd_buf[2] = super::super::spl_types::ConfigItem::ExosphereNeedsReboot as u32;
            assert_eq!(service.handle_sync_request(&mut ctx), RESULT_SUCCESS);
            assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
            assert_eq!(&ctx.cmd_buf[8..10], &[0, 0]);
        }
    }
}

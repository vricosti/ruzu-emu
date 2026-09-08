// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/spl/csrng.h
//! Port of zuyu/src/core/hle/service/spl/csrng.cpp
//!
//! CSRNG service — cryptographic secure random number generator ("csrng").
//!
//! This is a Module::Interface variant with only GenerateRandomBytes (cmd 0).

use super::spl_module::ModuleInterface;
use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use std::collections::BTreeMap;

/// IPC command table for CSRNG (IRandomInterface).
///
/// Corresponds to the function table in upstream csrng.cpp.
pub mod commands {
    pub const GENERATE_RANDOM_BYTES: u32 = 0;
}

/// CSRNG — IRandomInterface service.
///
/// Corresponds to `CSRNG` in upstream csrng.h / csrng.cpp. This is a
/// `Module::Interface` with only the `GenerateRandomBytes` handler.
/// Upstream inherits the `std::mt19937 rng` member from `Module::Interface`;
/// composition keeps that state and behavior in the same upstream owner.
pub struct Csrng {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    module: ModuleInterface,
}

impl Csrng {
    pub fn new(rng_seed: Option<u32>) -> Self {
        let handlers = build_handler_map(&[(
            commands::GENERATE_RANDOM_BYTES,
            Some(Self::generate_random_bytes_handler),
            "GenerateRandomBytes",
        )]);

        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
            module: ModuleInterface::new("csrng", rng_seed),
        }
    }

    /// GenerateRandomBytes (cmd 0).
    ///
    /// Corresponds to `Module::Interface::GenerateRandomBytes` in upstream.
    pub fn generate_random_bytes(&self, buf: &mut [u8]) {
        self.module.generate_random_bytes(buf);
    }

    fn generate_random_bytes_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        service.module.generate_random_bytes_handler(ctx);
    }
}

impl SessionRequestHandler for Csrng {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "csrng"
    }
}

impl ServiceFramework for Csrng {
    fn get_service_name(&self) -> &str {
        "csrng"
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
    fn runtime_constructor_uses_configured_rng_seed() {
        const CHILD: &str = "RUZU_CSRNG_SETTINGS_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "hle::service::spl::csrng::tests::runtime_constructor_uses_configured_rng_seed",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        for seed in [0, 1, u32::MAX] {
            {
                let mut settings = common::settings::values_mut();
                settings.rng_seed_enabled.set_value(true);
                settings.rng_seed.set_value(seed);
            }
            let actual = Csrng::new(None);
            let expected = Csrng::new(Some(seed));
            for _ in 0..2 {
                let mut a = [0; 32];
                let mut b = [0; 32];
                actual.generate_random_bytes(&mut a);
                expected.generate_random_bytes(&mut b);
                assert_eq!(a, b);
            }
        }
        common::settings::values_mut()
            .rng_seed_enabled
            .set_value(false);
        let seconds = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        };
        let before = seconds();
        let actual = Csrng::new(None);
        let after = seconds();
        let mut bytes = [0; 32];
        actual.generate_random_bytes(&mut bytes);
        assert!((before..=after).any(|seed| {
            let mut expected = [0; 32];
            Csrng::new(Some(seed as u32)).generate_random_bytes(&mut expected);
            bytes == expected
        }));
    }

    #[test]
    fn generate_random_bytes_handler_is_registered() {
        let service = Csrng::new(Some(1));
        let handler = service
            .handlers()
            .get(&commands::GENERATE_RANDOM_BYTES)
            .expect("GenerateRandomBytes command must exist");
        assert!(handler.handler_callback.is_some());
    }

    #[test]
    fn random_state_advances_between_calls() {
        let service = Csrng::new(Some(1));
        let mut first = [0; 32];
        let mut second = [0; 32];
        service.generate_random_bytes(&mut first);
        service.generate_random_bytes(&mut second);
        assert_ne!(first, second);
    }

    #[test]
    fn explicit_seed_matches_std_mt19937_uniform_u8_sequence() {
        let service = Csrng::new(Some(5489));
        let mut bytes = [0; 8];
        service.generate_random_bytes(&mut bytes);
        assert_eq!(bytes, [0xd0, 0x22, 0xe7, 0xd5, 0x20, 0xf8, 0xe9, 0x38]);
    }
}

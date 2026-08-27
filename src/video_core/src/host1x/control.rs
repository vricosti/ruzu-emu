// SPDX-FileCopyrightText: Copyright 2024 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/host1x/control.h` and `control.cpp`.
//!
//! Host1x control channel — processes syncpoint wait methods.

use crate::host1x::syncpoint_manager::SyncpointManager;
use log::trace;
use std::sync::Arc;

/// Control methods for the Host1x control channel.
///
/// Port of `Tegra::Host1x::Control::Method`.
/// A transparent newtype preserves the raw value reaching Eden's `default`
/// switch arm without constructing an invalid Rust enum discriminant.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Method(u32);

#[allow(non_upper_case_globals)]
impl Method {
    pub const WaitSyncpt: Self = Self(0x8);
    pub const LoadSyncptPayload32: Self = Self(0x4e);
    pub const WaitSyncpt32: Self = Self(0x50);

    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Host1x control channel processor.
///
/// Port of `Tegra::Host1x::Control`.
pub struct Control {
    syncpoint_manager: Arc<SyncpointManager>,
    syncpoint_value: u32,
}

impl Control {
    pub fn new(syncpoint_manager: Arc<SyncpointManager>) -> Self {
        Self {
            syncpoint_manager,
            syncpoint_value: 0,
        }
    }

    /// Writes the method into the state; invokes Execute() if encountered.
    ///
    /// Port of `Control::ProcessMethod`.
    pub fn process_method(&mut self, method: Method, argument: u32) {
        match method {
            Method::LoadSyncptPayload32 => {
                self.syncpoint_value = argument;
            }
            Method::WaitSyncpt | Method::WaitSyncpt32 => {
                self.execute(argument);
            }
            _ => {
                log::error!("Unimplemented Control method 0x{:X}", method.raw());
            }
        }
    }

    /// For Host1x, execute is waiting on a syncpoint previously written into the state.
    ///
    /// Port of `Control::Execute`.
    fn execute(&self, data: u32) {
        trace!(
            "Control wait syncpt {} value {}",
            data,
            self.syncpoint_value
        );
        self.syncpoint_manager.wait_host(data, self.syncpoint_value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_preserves_known_and_unknown_raw_values() {
        assert_eq!(std::mem::size_of::<Method>(), 4);
        assert_eq!(Method::WaitSyncpt.raw(), 0x8);
        assert_eq!(Method::LoadSyncptPayload32.raw(), 0x4e);
        assert_eq!(Method::WaitSyncpt32.raw(), 0x50);
        assert_eq!(Method::from_raw(0x123).raw(), 0x123);

        let mut control = Control::new(Arc::new(SyncpointManager::new()));
        control.process_method(Method::from_raw(0x123), 99);
        assert_eq!(control.syncpoint_value, 0);
        control.process_method(Method::LoadSyncptPayload32, 99);
        assert_eq!(control.syncpoint_value, 99);
    }
}

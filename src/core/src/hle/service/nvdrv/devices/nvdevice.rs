// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/nvdrv/devices/nvdevice.h

use std::sync::{Arc, Mutex};

use crate::hle::kernel::k_readable_event::KReadableEvent;
use crate::hle::service::nvdrv::core::container::SessionId;
use crate::hle::service::nvdrv::nvdata::{DeviceFD, Ioctl, NvResult};

/// Represents an abstract nvidia device node. It is to be subclassed by concrete device nodes to
/// implement the ioctl interface.
pub trait NvDevice {
    fn as_any(&self) -> &dyn std::any::Any {
        panic!("as_any() not implemented for nvdevice")
    }

    /// Handles an ioctl1 request.
    fn ioctl1(&self, fd: DeviceFD, command: Ioctl, input: &[u8], output: &mut [u8]) -> NvResult;

    /// Handles an ioctl2 request.
    fn ioctl2(
        &self,
        fd: DeviceFD,
        command: Ioctl,
        input: &[u8],
        inline_input: &[u8],
        output: &mut [u8],
    ) -> NvResult;

    /// Handles an ioctl3 request.
    fn ioctl3(
        &self,
        fd: DeviceFD,
        command: Ioctl,
        input: &[u8],
        output: &mut [u8],
        inline_output: &mut [u8],
    ) -> NvResult;

    /// Called once a device is opened.
    fn on_open(&self, session_id: SessionId, fd: DeviceFD);

    /// Called once a device is closed.
    fn on_close(&self, fd: DeviceFD);

    /// Queries the readable end of a persistent nvdrv event by id.
    ///
    /// Upstream returns `Kernel::KEvent*` here and the interface layer copies its readable end
    /// into the caller handle table. The Rust port returns the persistent readable event directly.
    fn query_event(&self, _event_id: u32) -> Option<Arc<Mutex<KReadableEvent>>> {
        None
    }
}

// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of eden/src/core/hle/service/nvdrv/devices/nvhost_vic.h
//! Port of eden/src/core/hle/service/nvdrv/devices/nvhost_vic.cpp

use std::sync::{Arc, Mutex};

use crate::core::SystemRef;
use crate::hle::kernel::k_readable_event::KReadableEvent;
use crate::hle::service::nvdrv::core::container::Container;
use crate::hle::service::nvdrv::core::container::SessionId;
use crate::hle::service::nvdrv::core::syncpoint_manager::ChannelType;
use crate::hle::service::nvdrv::devices::ioctl_serialization::{wrap_fixed, wrap_fixed_variable};
use crate::hle::service::nvdrv::devices::nvdevice::NvDevice;
use crate::hle::service::nvdrv::devices::nvhost_nvdec_common::{
    IoctlGetSyncpoint, IoctlGetWaitbase, IoctlMapBuffer, IoctlSetNvmapFD, IoctlSubmit,
    MapBufferEntry, NvHostNvDecCommon,
};
use crate::hle::service::nvdrv::nvdata::{DeviceFD, Ioctl, NvResult};

/// nvhost_vic device.
pub struct NvHostVic {
    common: NvHostNvDecCommon,
}

impl NvHostVic {
    pub fn new(system: SystemRef, container: &Container) -> Self {
        Self {
            common: NvHostNvDecCommon::new(system, container, ChannelType::VIC),
        }
    }
}

impl NvDevice for NvHostVic {
    fn ioctl1(&self, fd: DeviceFD, command: Ioctl, input: &[u8], output: &mut [u8]) -> NvResult {
        match command.group() {
            0x0 => match command.cmd() {
                0x01 => wrap_fixed_variable::<IoctlSubmit, u8, _>(input, output, |params, data| {
                    self.common.submit(params, data, fd)
                }),
                0x02 => wrap_fixed::<IoctlGetSyncpoint, _>(input, output, |params| {
                    self.common.get_syncpoint(params)
                }),
                0x03 => wrap_fixed::<IoctlGetWaitbase, _>(input, output, |params| {
                    self.common.get_waitbase(params)
                }),
                0x09 => wrap_fixed_variable::<IoctlMapBuffer, MapBufferEntry, _>(
                    input,
                    output,
                    |params, entries| self.common.map_buffer(params, entries, fd),
                ),
                0x0A => wrap_fixed_variable::<IoctlMapBuffer, MapBufferEntry, _>(
                    input,
                    output,
                    |params, entries| self.common.unmap_buffer(params, entries),
                ),
                _ => {
                    log::error!("Unimplemented ioctl={:08X}", command.raw);
                    NvResult::NotImplemented
                }
            },
            b'H' => match command.cmd() {
                0x01 => wrap_fixed::<IoctlSetNvmapFD, _>(input, output, |params| {
                    self.common.set_nvmap_fd(params)
                }),
                _ => {
                    log::error!("Unimplemented ioctl={:08X}", command.raw);
                    NvResult::NotImplemented
                }
            },
            _ => {
                log::error!("Unimplemented ioctl={:08X}", command.raw);
                NvResult::NotImplemented
            }
        }
    }

    fn ioctl2(
        &self,
        _fd: DeviceFD,
        command: Ioctl,
        _input: &[u8],
        _inline_input: &[u8],
        _output: &mut [u8],
    ) -> NvResult {
        log::error!("Unimplemented ioctl={:08X}", command.raw);
        NvResult::NotImplemented
    }

    fn ioctl3(
        &self,
        _fd: DeviceFD,
        command: Ioctl,
        _input: &[u8],
        _output: &mut [u8],
        _inline_output: &mut [u8],
    ) -> NvResult {
        log::error!("Unimplemented ioctl={:08X}", command.raw);
        NvResult::NotImplemented
    }

    fn on_open(&self, session_id: SessionId, fd: DeviceFD) {
        let mut sessions = self.common.sessions.lock().unwrap();
        sessions.insert(fd, session_id);
        drop(sessions);
        self.common.start_host1x_device(fd);
    }

    fn on_close(&self, fd: DeviceFD) {
        self.common.stop_host1x_device(fd);
        let mut sessions = self.common.sessions.lock().unwrap();
        sessions.remove(&fd);
    }

    fn query_event(&self, event_id: u32) -> Option<Arc<Mutex<KReadableEvent>>> {
        self.common.query_event(event_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl1_does_not_expose_nvdec_only_commands() {
        let container = Container::new();
        let device = NvHostVic::new(SystemRef::null(), &container);
        let mut output = [0u8; 8];

        assert_eq!(
            device.ioctl1(1, Ioctl { raw: 0x07 }, &[0; 4], &mut output),
            NvResult::NotImplemented
        );
        assert_eq!(
            device.ioctl1(1, Ioctl { raw: 0x23 }, &[0; 8], &mut output),
            NvResult::NotImplemented
        );
    }
}

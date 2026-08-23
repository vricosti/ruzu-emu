// SPDX-FileCopyrightText: Copyright 2020 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of eden/src/core/hle/service/nvdrv/devices/nvhost_nvdec_common.h
//! Port of eden/src/core/hle/service/nvdrv/devices/nvhost_nvdec_common.cpp

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use crate::hle::kernel::k_readable_event::KReadableEvent;
use crate::hle::service::nvdrv::core::container::SessionId;
use crate::hle::service::nvdrv::core::syncpoint_manager::ChannelType;
use crate::hle::service::nvdrv::core::{
    container::Container, nvmap::NvMap, syncpoint_manager::SyncpointManager,
};
use crate::hle::service::nvdrv::nvdata::*;
use crate::{core::SystemRef, host1x_core::Host1xChannelType};

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlSetNvmapFD {
    pub nvmap_fd: i32,
}
const _: () = assert!(std::mem::size_of::<IoctlSetNvmapFD>() == 4);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlSubmit {
    pub cmd_buffer_count: u32,
    pub relocation_count: u32,
    pub syncpoint_count: u32,
    pub fence_count: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlSubmit>() == 0x10);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CommandBuffer {
    pub memory_id: i32,
    pub offset: u32,
    pub word_count: i32,
}
const _: () = assert!(std::mem::size_of::<CommandBuffer>() == 0xC);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Reloc {
    pub cmdbuffer_memory: i32,
    pub cmdbuffer_offset: i32,
    pub target: i32,
    pub target_offset: i32,
}
const _: () = assert!(std::mem::size_of::<Reloc>() == 0x10);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SyncptIncr {
    pub id: u32,
    pub increments: u32,
    pub unk0: u32,
    pub unk1: u32,
    pub unk2: u32,
}
const _: () = assert!(std::mem::size_of::<SyncptIncr>() == 0x14);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlGetSyncpoint {
    pub param: u32,
    pub value: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlGetSyncpoint>() == 0x8);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlGetWaitbase {
    pub unknown: u32,
    pub value: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlGetWaitbase>() == 0x8);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlMapBuffer {
    pub num_entries: u32,
    pub data_address: u32,
    pub attach_host_ch_das: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlMapBuffer>() == 0x0C);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MapBufferEntry {
    pub map_handle: u32,
    pub map_address: u32,
}
const _: () = assert!(std::mem::size_of::<MapBufferEntry>() == 0x8);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlGetClkRate {
    pub clk_rate: u32,
    pub module_id: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlGetClkRate>() == 0x8);

/// Common base for NVDEC/VIC devices.
pub struct NvHostNvDecCommon {
    system: SystemRef,
    core: Container,
    pub channel_syncpoint: u32,
    nvmap: Arc<NvMap>,
    syncpoint_manager: Arc<SyncpointManager>,
    channel_type: ChannelType,
    pub sessions: Mutex<HashMap<DeviceFD, SessionId>>,
}

impl NvHostNvDecCommon {
    pub fn new(system: SystemRef, container: &Container, channel_type: ChannelType) -> Self {
        let channel_syncpoint = container
            .take_accumulated_syncpoint()
            .unwrap_or_else(|| container.get_syncpoint_manager().allocate_syncpoint(false));
        Self {
            system,
            core: container.clone(),
            channel_syncpoint,
            nvmap: container.get_nv_map_file_handle(),
            syncpoint_manager: container.get_syncpoint_manager_handle(),
            channel_type,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn set_nvmap_fd(&self, params: &mut IoctlSetNvmapFD) -> NvResult {
        log::debug!(
            "nvhost_nvdec_common::SetNVMAPfd called, fd={}",
            params.nvmap_fd
        );
        NvResult::Success
    }

    pub fn submit(&self, params: &mut IoctlSubmit, data: &mut [u8], fd: DeviceFD) -> NvResult {
        log::debug!(
            "nvhost_nvdec_common::Submit fd={} cmd_buffers={} relocs={} syncpts={} fences={}",
            fd,
            params.cmd_buffer_count,
            params.relocation_count,
            params.syncpoint_count,
            params.fence_count
        );

        let mut offset = 0usize;
        let command_buffers =
            read_vec::<CommandBuffer>(data, params.cmd_buffer_count as usize, &mut offset);
        let relocs = read_vec::<Reloc>(data, params.relocation_count as usize, &mut offset);
        let reloc_shifts = read_vec::<u32>(data, params.relocation_count as usize, &mut offset);
        let syncpt_increments =
            read_vec::<SyncptIncr>(data, params.syncpoint_count as usize, &mut offset);
        let mut fence_thresholds = read_vec::<u32>(data, params.fence_count as usize, &mut offset);
        let trace_stage = match self.channel_type {
            ChannelType::NvDec => 1,
            ChannelType::VIC => 2,
            _ => 0,
        };

        let session_id = self
            .sessions
            .lock()
            .unwrap()
            .get(&fd)
            .copied()
            .unwrap_or_default();
        let Some(process) = self.core.get_session_process(session_id) else {
            log::error!(
                "nvhost_nvdec_common::Submit called without an active session for fd={}",
                fd
            );
            return NvResult::InvalidState;
        };
        let Some(memory) = process.lock().unwrap().get_memory() else {
            log::error!("nvhost_nvdec_common::Submit session has no process memory");
            return NvResult::InvalidState;
        };

        for (index, syncpt_incr) in syncpt_increments.iter().enumerate() {
            if let Some(threshold) = fence_thresholds.get_mut(index) {
                *threshold = self
                    .syncpoint_manager
                    .increment_syncpoint_max_ext(syncpt_incr.id, syncpt_incr.increments);
            }
        }

        let Some(host1x) = self.system.get().host1x_core() else {
            log::error!("nvhost_nvdec_common::Submit called without Host1x core");
            return NvResult::InvalidState;
        };

        for cmd_buffer in &command_buffers {
            if cmd_buffer.word_count <= 0 {
                continue;
            }
            let Some(object) = self.nvmap.get_handle(cmd_buffer.memory_id as u32) else {
                log::error!(
                    "nvhost_nvdec_common::Submit unknown command buffer nvmap handle=0x{:X}",
                    cmd_buffer.memory_id
                );
                return NvResult::InvalidState;
            };
            let address = {
                let object = object.lock_inner();
                object.address.wrapping_add(cmd_buffer.offset as u64)
            };
            let word_count = cmd_buffer.word_count as usize;
            if trace_stage != 0 {
                let _ = common::trace::emit(
                    common::trace::cat::HOST1X_VIDEO,
                    &[
                        trace_stage,
                        fd as u64,
                        params.cmd_buffer_count as u64,
                        params.relocation_count as u64,
                        params.syncpoint_count as u64,
                        params.fence_count as u64,
                        address,
                        word_count as u64,
                    ],
                );
            }
            let mut bytes = vec![0u8; word_count.saturating_mul(std::mem::size_of::<u32>())];
            memory.lock().unwrap().read_block(address, &mut bytes);
            let cmdlist = bytes
                .chunks_exact(std::mem::size_of::<u32>())
                .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
                .collect::<Vec<_>>();
            host1x.push_entries(fd, cmdlist);
        }

        offset = 0;
        write_vec(data, &command_buffers, &mut offset);
        write_vec(data, &relocs, &mut offset);
        write_vec(data, &reloc_shifts, &mut offset);
        write_vec(data, &syncpt_increments, &mut offset);
        write_vec(data, &fence_thresholds, &mut offset);

        NvResult::Success
    }

    pub fn get_syncpoint(&self, params: &mut IoctlGetSyncpoint) -> NvResult {
        log::debug!(
            "nvhost_nvdec_common::GetSyncpoint called, id={}",
            params.param
        );
        params.value = self.channel_syncpoint;
        NvResult::Success
    }

    pub fn get_waitbase(&self, params: &mut IoctlGetWaitbase) -> NvResult {
        log::debug!("nvhost_nvdec_common::GetWaitbase called");
        params.value = 0;
        NvResult::Success
    }

    pub fn map_buffer(
        &self,
        params: &mut IoctlMapBuffer,
        entries: &mut [MapBufferEntry],
        _fd: DeviceFD,
    ) -> NvResult {
        let num_entries = (params.num_entries as usize).min(entries.len());
        for entry in &mut entries[..num_entries] {
            entry.map_address = self.nvmap.pin_handle(entry.map_handle, true) as u32;
        }
        NvResult::Success
    }

    pub fn unmap_buffer(
        &self,
        params: &mut IoctlMapBuffer,
        entries: &mut [MapBufferEntry],
    ) -> NvResult {
        let num_entries = (params.num_entries as usize).min(entries.len());
        for entry in &mut entries[..num_entries] {
            self.nvmap.unpin_handle(entry.map_handle);
            *entry = MapBufferEntry::default();
        }
        *params = IoctlMapBuffer::default();
        NvResult::Success
    }

    pub fn set_submit_timeout(&self, _timeout: u32) -> NvResult {
        log::warn!("nvhost_nvdec_common::SetSubmitTimeout (STUBBED) called");
        NvResult::Success
    }

    pub fn get_clk_rate(&self, params: &mut IoctlGetClkRate) -> NvResult {
        log::warn!("nvhost_nvdec_common::GetClkRate (STUBBED) called");
        params.clk_rate = 614_400_000;
        params.module_id = 0;
        NvResult::Success
    }

    pub fn query_event(&self, event_id: u32) -> Option<Arc<Mutex<KReadableEvent>>> {
        log::error!("Unknown HOSTX1 Event {}", event_id);
        None
    }

    pub fn system(&self) -> SystemRef {
        self.system
    }

    pub fn host1x_channel_type(&self) -> Host1xChannelType {
        match self.channel_type {
            ChannelType::MsEnc => Host1xChannelType::MsEnc,
            ChannelType::VIC => Host1xChannelType::Vic,
            ChannelType::GPU => Host1xChannelType::Gpu,
            ChannelType::NvDec => Host1xChannelType::NvDec,
            ChannelType::Display => Host1xChannelType::Display,
            ChannelType::NvJpg => Host1xChannelType::NvJpg,
            ChannelType::TSec => Host1xChannelType::TSec,
            ChannelType::Max => Host1xChannelType::Max,
        }
    }

    pub fn start_host1x_device(&self, fd: DeviceFD) {
        if let Some(host1x) = self.system.get().host1x_core() {
            host1x.start_device(fd, self.host1x_channel_type(), self.channel_syncpoint);
        } else {
            log::error!("nvhost_nvdec_common::OnOpen missing Host1x core");
        }
    }

    pub fn stop_host1x_device(&self, fd: DeviceFD) {
        if let Some(host1x) = self.system.get().host1x_core() {
            host1x.stop_device(fd, self.host1x_channel_type());
        }
    }
}

impl Drop for NvHostNvDecCommon {
    fn drop(&mut self) {
        self.core.recycle_syncpoint(self.channel_syncpoint);
    }
}

fn read_vec<T: Copy + Default>(input: &[u8], count: usize, offset: &mut usize) -> Vec<T> {
    let mut out = vec![T::default(); count];
    let bytes = count.saturating_mul(std::mem::size_of::<T>());
    if count != 0 && input.len() >= offset.saturating_add(bytes) {
        unsafe {
            std::ptr::copy_nonoverlapping(
                input.as_ptr().add(*offset),
                out.as_mut_ptr() as *mut u8,
                bytes,
            );
        }
        *offset += bytes;
    }
    out
}

fn write_vec<T: Copy>(output: &mut [u8], input: &[T], offset: &mut usize) {
    let bytes = input.len().saturating_mul(std::mem::size_of::<T>());
    if !input.is_empty() && output.len() >= offset.saturating_add(bytes) {
        unsafe {
            std::ptr::copy_nonoverlapping(
                input.as_ptr() as *const u8,
                output.as_mut_ptr().add(*offset),
                bytes,
            );
        }
        *offset += bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvhost_nvdec_common_reuses_recycled_syncpoint_like_upstream() {
        let container = Container::new();

        let first_syncpoint = {
            let common = NvHostNvDecCommon::new(SystemRef::null(), &container, ChannelType::NvDec);
            common.channel_syncpoint
        };

        let second = NvHostNvDecCommon::new(SystemRef::null(), &container, ChannelType::NvDec);

        assert_eq!(second.channel_syncpoint, first_syncpoint);
    }

    #[test]
    fn map_and_unmap_respect_num_entries_like_upstream() {
        let container = Container::new();
        let common = NvHostNvDecCommon::new(SystemRef::null(), &container, ChannelType::NvDec);
        let original = MapBufferEntry {
            map_handle: 0x1234,
            map_address: 0x5678,
        };
        let mut entries = [original];
        let mut params = IoctlMapBuffer {
            num_entries: 0,
            ..Default::default()
        };

        assert_eq!(
            common.map_buffer(&mut params, &mut entries, 1),
            NvResult::Success
        );
        assert_eq!(entries[0].map_handle, original.map_handle);
        assert_eq!(entries[0].map_address, original.map_address);

        assert_eq!(
            common.unmap_buffer(&mut params, &mut entries),
            NvResult::Success
        );
        assert_eq!(entries[0].map_handle, original.map_handle);
        assert_eq!(entries[0].map_address, original.map_address);
        assert_eq!(params.num_entries, 0);
        assert_eq!(params.data_address, 0);
        assert_eq!(params.attach_host_ch_das, 0);
    }

    #[test]
    fn get_clk_rate_matches_upstream_stub_output() {
        let container = Container::new();
        let common = NvHostNvDecCommon::new(SystemRef::null(), &container, ChannelType::NvDec);
        let mut params = IoctlGetClkRate {
            clk_rate: 1,
            module_id: 2,
        };

        assert_eq!(common.get_clk_rate(&mut params), NvResult::Success);
        assert_eq!(params.clk_rate, 614_400_000);
        assert_eq!(params.module_id, 0);
    }
}

// SPDX-FileCopyrightText: Copyright 2024 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/host1x/host1x.h` and `host1x.cpp`.
//!
//! Main Host1x class: owns the syncpoint manager, device memory manager,
//! GMMU, allocator, frame queue, and active CDMA devices.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use log::error;
use ruzu_core::core::SystemRef;
use ruzu_core::host1x_core::{Host1xChannelType, Host1xCoreInterface};

use crate::host1x::ffmpeg::ffmpeg::Frame;
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::host1x::syncpoint_manager::SyncpointManager;
use crate::memory_manager::MemoryManager;
use crate::rasterizer_interface::RasterizerInterface;
use common::address_space::FlatAllocator;

// --------------------------------------------------------------------------
// FrameQueue
// --------------------------------------------------------------------------

/// Thread-safe queue for decoded video frames, indexed by NVDEC file descriptor
/// and memory offset. Supports both presentation-order and decode-order queuing.
///
/// Port of `Tegra::Host1x::FrameQueue`.
pub struct FrameQueue {
    inner: Mutex<FrameQueueInner>,
}

struct FrameQueueInner {
    frame_devices: HashMap<i32, FrameDevice>,
}

struct FrameDevice {
    presentation_order: VecDeque<(u64, Arc<Frame>)>,
    decode_order: HashMap<u64, Arc<Frame>>,
}

impl FrameQueue {
    const MAX_PRESENT_QUEUE: usize = 100;
    const MAX_DECODE_MAP: usize = 200;

    pub fn new() -> Self {
        Self {
            inner: Mutex::new(FrameQueueInner {
                frame_devices: HashMap::new(),
            }),
        }
    }

    /// Register a new NVDEC file descriptor.
    pub fn open(&self, fd: i32) {
        let mut inner = self.inner.lock().unwrap();
        inner.frame_devices.insert(
            fd,
            FrameDevice {
                presentation_order: VecDeque::new(),
                decode_order: HashMap::new(),
            },
        );
    }

    /// Unregister an NVDEC file descriptor.
    pub fn close(&self, fd: i32) {
        let mut inner = self.inner.lock().unwrap();
        inner.frame_devices.remove(&fd);
    }

    /// Search all FDs for a frame matching the given offset.
    /// Returns the FD that owns it, or -1 if not found.
    ///
    /// Port of `FrameQueue::VicFindNvdecFdFromOffset`.
    pub fn vic_find_nvdec_fd_from_offset(&self, search_offset: u64) -> i32 {
        let inner = self.inner.lock().unwrap();

        for (fd, device) in &inner.frame_devices {
            for (offset, _) in &device.presentation_order {
                if *offset == search_offset {
                    return *fd;
                }
            }
            for (offset, _) in &device.decode_order {
                if *offset == search_offset {
                    return *fd;
                }
            }
        }

        -1
    }

    /// Push a frame in presentation order.
    pub fn push_present_order(&self, fd: i32, offset: u64, frame: Arc<Frame>) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(device) = inner.frame_devices.get_mut(&fd) {
            if device.presentation_order.len() >= Self::MAX_PRESENT_QUEUE {
                device.presentation_order.pop_front();
            }
            device.presentation_order.push_back((offset, frame));
        }
    }

    /// Push a frame in decode order (keyed by offset, replaces existing).
    pub fn push_decode_order(&self, fd: i32, offset: u64, frame: Arc<Frame>) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(device) = inner.frame_devices.get_mut(&fd) {
            device.decode_order.insert(offset, frame);
            let excess = device
                .decode_order
                .len()
                .saturating_sub(Self::MAX_DECODE_MAP);
            let stale_offsets: Vec<_> = device.decode_order.keys().copied().take(excess).collect();
            for stale_offset in stale_offsets {
                device.decode_order.remove(&stale_offset);
            }
        }
    }

    /// Retrieve a frame for the given FD and offset.
    /// Prefers presentation order; falls back to decode order.
    pub fn get_frame(&self, fd: i32, offset: u64) -> Option<Arc<Frame>> {
        if fd == -1 {
            return None;
        }

        let mut inner = self.inner.lock().unwrap();

        if let Some(device) = inner.frame_devices.get_mut(&fd) {
            if let Some((_offset, frame)) = device.presentation_order.pop_front() {
                return Some(frame);
            }
            if let Some(frame) = device.decode_order.remove(&offset) {
                return Some(frame);
            }
        }

        None
    }
}

impl Default for FrameQueue {
    fn default() -> Self {
        Self::new()
    }
}

// --------------------------------------------------------------------------
// ChannelType
// --------------------------------------------------------------------------

/// Host1x channel types.
///
/// Port of `Tegra::Host1x::ChannelType`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelType {
    MsEnc = 0,
    Vic = 1,
    Gpu = 2,
    NvDec = 3,
    Display = 4,
    NvJpg = 5,
    TSec = 6,
    Max = 7,
}

impl ChannelType {
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::MsEnc),
            1 => Some(Self::Vic),
            2 => Some(Self::Gpu),
            3 => Some(Self::NvDec),
            4 => Some(Self::Display),
            5 => Some(Self::NvJpg),
            6 => Some(Self::TSec),
            7 => Some(Self::Max),
            _ => None,
        }
    }
}

// --------------------------------------------------------------------------
// Host1x
// --------------------------------------------------------------------------

/// Main Host1x subsystem.
///
/// Port of `Tegra::Host1x::Host1x`. The `devices` map mirrors upstream's
/// fixed variant array. Rust stores only active entries in a map and lets each
/// `CDmaPusher` own the corresponding `Nvdec` or `Vic` processor.
pub struct Host1x {
    /// Upstream stores `Core::System& system`.
    system: SystemRef,
    syncpoint_manager: Arc<SyncpointManager>,
    frame_queue: Arc<FrameQueue>,
    devices: Mutex<HashMap<i32, Arc<crate::cdma_pusher::CDmaPusher>>>,
    /// Single shared `MaxwellDeviceMemoryManager` instance. Mirrors
    /// upstream `Tegra::Host1x::Host1x::memory_manager` — every GPU cache
    /// (shader, buffer, texture, query) holds a reference to this same
    /// instance via the GPU/renderer/rasterizer construction chain.
    memory_manager: Arc<MaxwellDeviceMemoryManager>,
    /// Host1x-local GPU virtual address space.
    ///
    /// Upstream constructs this as
    /// `gmmu_manager{system, memory_manager, 32, 0, 12}` and binds the
    /// renderer rasterizer from `GPU::Impl::BindRenderer`.
    gmmu_manager: Arc<parking_lot::Mutex<MemoryManager>>,
    /// Upstream `Common::FlatAllocator<u32, 0, 32>` used by NvMap low-area
    /// pins before mapping through `gmmu_manager`.
    allocator: parking_lot::Mutex<FlatAllocator>,
}

impl Host1x {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::new_with_system(SystemRef::null())
    }

    pub fn new_with_system(system: SystemRef) -> Self {
        let memory_manager = Arc::new(if system.is_null() {
            MaxwellDeviceMemoryManager::default()
        } else {
            MaxwellDeviceMemoryManager::new_with_device_memory(system.get().device_memory())
        });
        Self {
            system,
            syncpoint_manager: Arc::new(SyncpointManager::new()),
            frame_queue: Arc::new(FrameQueue::new()),
            devices: Mutex::new(HashMap::new()),
            memory_manager: Arc::clone(&memory_manager),
            gmmu_manager: Arc::new(parking_lot::Mutex::new(
                MemoryManager::new_with_geometry_and_device_memory(
                    0,
                    memory_manager,
                    32,
                    0,
                    12,
                    12,
                ),
            )),
            allocator: parking_lot::Mutex::new(FlatAllocator::new(1 << 12, u32::MAX)),
        }
    }

    pub fn syncpoint_manager(&self) -> &Arc<SyncpointManager> {
        &self.syncpoint_manager
    }

    /// Port of upstream `Tegra::Host1x::Host1x::System()`.
    pub fn system_ref(&self) -> SystemRef {
        self.system
    }

    /// Port of upstream `Tegra::Host1x::Host1x::MemoryManager()`.
    /// Returns the single shared `MaxwellDeviceMemoryManager` instance
    /// that all GPU caches reference.
    pub fn memory_manager(&self) -> &Arc<MaxwellDeviceMemoryManager> {
        &self.memory_manager
    }

    /// Port of upstream `Tegra::Host1x::Host1x::GMMU()`.
    pub fn bind_gmmu_rasterizer(&self, rasterizer: &dyn RasterizerInterface) {
        self.gmmu_manager.lock().bind_rasterizer(rasterizer);
    }

    /// Port of the Host1x side of upstream `NvMap::PinHandle(low_area_pin)`.
    pub fn gmmu_map_low(&self, d_address: u64, size: usize) -> u32 {
        if size == 0 {
            return 0;
        }
        let Ok(size32) = u32::try_from(size) else {
            log::error!("Host1x::gmmu_map_low: size 0x{size:X} exceeds 32-bit allocator range");
            return 0;
        };
        let Some(address) = self.allocator.lock().allocate(size32) else {
            log::error!("Host1x::gmmu_map_low: allocator exhausted for size 0x{size:X}");
            return 0;
        };
        self.gmmu_manager
            .lock()
            .map(address as u64, d_address, size as u64, 0xFF, true);
        address
    }

    /// Port of the Host1x GMMU portion of upstream `NvMap::UnmapHandle`.
    pub fn gmmu_unmap_low(&self, gpu_address: u32, size: usize) {
        if gpu_address == 0 || size == 0 {
            return;
        }
        let Ok(size32) = u32::try_from(size) else {
            log::error!("Host1x::gmmu_unmap_low: size 0x{size:X} exceeds 32-bit allocator range");
            return;
        };
        self.gmmu_manager
            .lock()
            .unmap(gpu_address as u64, size as u64);
        self.allocator.lock().free(gpu_address, size32);
    }

    pub fn frame_queue(&self) -> &Arc<FrameQueue> {
        &self.frame_queue
    }

    /// Start a device (NvDec, VIC, etc.) on the given file descriptor.
    ///
    /// Port of `Host1x::StartDevice`. Constructs the concrete Host1x device
    /// processor and stores its `CDmaPusher` in `devices` keyed by fd.
    pub fn start_device(&self, fd: i32, channel_type: ChannelType, syncpt: u32) {
        use crate::cdma_pusher::{CDmaPusher, ChClassId, ProcessMethodHook};
        let (class_id, processor): (ChClassId, Box<dyn ProcessMethodHook>) = match channel_type {
            ChannelType::NvDec => (
                ChClassId::NvDec,
                Box::new(crate::host1x::nvdec::Nvdec::new(
                    fd,
                    syncpt,
                    Arc::clone(&self.frame_queue),
                    Arc::clone(&self.gmmu_manager),
                )),
            ),
            ChannelType::Vic => (
                ChClassId::GraphicsVic,
                Box::new(crate::host1x::vic::Vic::new(
                    fd,
                    syncpt,
                    Arc::clone(&self.frame_queue),
                    Arc::clone(&self.gmmu_manager),
                )),
            ),
            _ => {
                error!(
                    "Unimplemented host1x device {:?} ({})",
                    channel_type, channel_type as u32
                );
                return;
            }
        };
        let pusher = Arc::new(CDmaPusher::new_with_processor(
            self.syncpoint_manager.clone(),
            class_id.raw() as i32,
            processor,
        ));
        self.devices.lock().unwrap().insert(fd, pusher);
    }

    /// Stop a device on the given file descriptor.
    ///
    /// Port of `Host1x::StopDevice`.
    pub fn stop_device(&self, fd: i32, _channel_type: ChannelType) {
        self.devices.lock().unwrap().remove(&fd);
    }

    /// Push command entries to a device.
    ///
    /// Port of `Host1x::PushEntries`. Looks up the per-fd `CDmaPusher` and
    /// forwards the command headers to its `Nvdec` or `Vic` processor.
    pub fn push_entries(&self, fd: i32, entries: Vec<u32>) {
        use crate::cdma_pusher::ChCommandHeader;
        let pusher = match self.devices.lock().unwrap().get(&fd) {
            Some(p) => p.clone(),
            None => return,
        };
        let headers: Vec<ChCommandHeader> = entries
            .into_iter()
            .map(|raw| ChCommandHeader { raw })
            .collect();
        pusher.push_entries(headers);
    }
}

#[cfg(test)]
impl Default for Host1x {
    fn default() -> Self {
        Self::new_with_system(SystemRef::null())
    }
}

impl Host1xCoreInterface for Host1x {
    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
        self
    }

    fn get_host_syncpoint_value(&self, id: u32) -> u32 {
        self.syncpoint_manager.get_host_syncpoint_value(id)
    }

    fn wait_host(&self, id: u32, expected_value: u32) {
        self.syncpoint_manager.wait_host(id, expected_value);
    }

    fn register_guest_action(
        &self,
        id: u32,
        expected_value: u32,
        action: Box<dyn FnOnce() + Send>,
    ) -> Option<u64> {
        self.syncpoint_manager
            .register_guest_action(id, expected_value, action)
            .map(|handle| handle.raw())
    }

    fn register_host_action(
        &self,
        id: u32,
        expected_value: u32,
        action: Box<dyn FnOnce() + Send>,
    ) -> Option<u64> {
        self.syncpoint_manager
            .register_host_action(id, expected_value, action)
            .map(|handle| handle.raw())
    }

    fn deregister_host_action(&self, id: u32, handle: u64) {
        self.syncpoint_manager.deregister_host_action(
            id,
            &crate::host1x::syncpoint_manager::ActionHandle::from_raw(handle),
        );
    }

    fn smmu_allocate(&self, size: usize) -> u64 {
        self.memory_manager.smmu_allocate(size)
    }

    fn smmu_register_process(
        &self,
        memory: Option<std::sync::Arc<std::sync::Mutex<ruzu_core::memory::memory::Memory>>>,
    ) -> u32 {
        self.memory_manager.smmu_register_process(memory)
    }

    fn smmu_unregister_process(&self, asid: u32) {
        self.memory_manager.smmu_unregister_process(asid);
    }

    fn smmu_free(&self, d_address: u64, size: usize) {
        self.memory_manager.smmu_free(d_address, size);
    }

    fn smmu_map(&self, d_address: u64, virtual_address: u64, size: usize, asid: u32, track: bool) {
        self.memory_manager
            .smmu_map(d_address, virtual_address, size, asid, track);
    }

    fn smmu_track_continuity(&self, d_address: u64, size: usize) {
        self.memory_manager.smmu_track_continuity(d_address, size);
    }

    fn smmu_track_continuity_registered(
        &self,
        d_address: u64,
        virtual_address: u64,
        size: usize,
        asid: u32,
    ) {
        self.memory_manager.smmu_track_continuity_registered(
            d_address,
            virtual_address,
            size,
            asid,
        );
    }

    fn smmu_unmap(&self, d_address: u64, size: usize) {
        self.memory_manager.smmu_unmap(d_address, size);
    }

    fn smmu_lookup(&self, d_address: u64) -> usize {
        self.memory_manager
            .smmu_get_host_ptr(d_address)
            .map(|p| p as usize)
            .unwrap_or(0)
    }

    fn smmu_apply_op_on_host_pointer(
        &self,
        host_ptr: usize,
        scratch: &mut common::scratch_buffer::ScratchBuffer<u32>,
        operation: &mut dyn FnMut(u64),
    ) -> usize {
        self.memory_manager
            .smmu_apply_op_on_host_pointer(host_ptr as *const u8, scratch, operation)
    }

    fn bind_device_memory_invalidator(&self, callback: Box<dyn Fn(u64, usize) + Send + Sync>) {
        self.memory_manager.set_invalidate_region(callback);
    }

    fn bind_device_memory_flusher(&self, callback: Box<dyn Fn(u64, usize) + Send + Sync>) {
        self.memory_manager.set_flush_region(callback);
    }

    fn host1x_gmmu_map_low(&self, d_address: u64, size: usize) -> u32 {
        self.gmmu_map_low(d_address, size)
    }

    fn host1x_gmmu_unmap_low(&self, gpu_address: u32, size: usize) {
        self.gmmu_unmap_low(gpu_address, size);
    }

    fn start_device(&self, fd: i32, channel_type: Host1xChannelType, syncpt: u32) {
        let channel_type = match channel_type {
            Host1xChannelType::MsEnc => ChannelType::MsEnc,
            Host1xChannelType::Vic => ChannelType::Vic,
            Host1xChannelType::Gpu => ChannelType::Gpu,
            Host1xChannelType::NvDec => ChannelType::NvDec,
            Host1xChannelType::Display => ChannelType::Display,
            Host1xChannelType::NvJpg => ChannelType::NvJpg,
            Host1xChannelType::TSec => ChannelType::TSec,
            Host1xChannelType::Max => ChannelType::Max,
        };
        Host1x::start_device(self, fd, channel_type, syncpt);
    }

    fn stop_device(&self, fd: i32, channel_type: Host1xChannelType) {
        let channel_type = match channel_type {
            Host1xChannelType::MsEnc => ChannelType::MsEnc,
            Host1xChannelType::Vic => ChannelType::Vic,
            Host1xChannelType::Gpu => ChannelType::Gpu,
            Host1xChannelType::NvDec => ChannelType::NvDec,
            Host1xChannelType::Display => ChannelType::Display,
            Host1xChannelType::NvJpg => ChannelType::NvJpg,
            Host1xChannelType::TSec => ChannelType::TSec,
            Host1xChannelType::Max => ChannelType::Max,
        };
        Host1x::stop_device(self, fd, channel_type);
    }

    fn push_entries(&self, fd: i32, entries: Vec<u32>) {
        Host1x::push_entries(self, fd, entries);
    }
}

#[cfg(test)]
mod tests {
    use super::{ChannelType, Host1x};
    use ruzu_core::host1x_core::Host1xCoreInterface;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn frame_queue_contains(host1x: &Host1x, fd: i32) -> bool {
        let inner = host1x.frame_queue.inner.lock().unwrap();
        inner.frame_devices.contains_key(&fd)
    }

    #[test]
    fn device_owned_frame_queue_lifecycle_matches_upstream() {
        let host1x = Host1x::new();

        host1x.start_device(7, ChannelType::NvDec, 0);
        assert!(frame_queue_contains(&host1x, 7));
        host1x.stop_device(7, ChannelType::NvDec);
        assert!(!frame_queue_contains(&host1x, 7));

        host1x.frame_queue.open(9);
        host1x.start_device(9, ChannelType::Vic, 0);
        assert!(frame_queue_contains(&host1x, 9));
        host1x.stop_device(9, ChannelType::Vic);
        assert!(!frame_queue_contains(&host1x, 9));
    }

    #[test]
    fn frame_queue_limits_match_upstream() {
        let queue = super::FrameQueue::new();
        queue.open(3);
        let frame = Arc::new(crate::host1x::ffmpeg::ffmpeg::Frame::new());

        for offset in 0..=super::FrameQueue::MAX_PRESENT_QUEUE as u64 {
            queue.push_present_order(3, offset, Arc::clone(&frame));
        }
        for offset in 0..=super::FrameQueue::MAX_DECODE_MAP as u64 {
            queue.push_decode_order(3, offset, Arc::clone(&frame));
        }

        let inner = queue.inner.lock().unwrap();
        let device = inner.frame_devices.get(&3).unwrap();
        assert_eq!(
            device.presentation_order.len(),
            super::FrameQueue::MAX_PRESENT_QUEUE
        );
        assert_eq!(device.presentation_order.front().unwrap().0, 1);
        assert_eq!(device.decode_order.len(), super::FrameQueue::MAX_DECODE_MAP);
    }

    #[test]
    fn guest_action_bridge_fires_from_guest_counter_only() {
        let host1x = Host1x::new();
        let fired = Arc::new(AtomicUsize::new(0));
        let fired_clone = Arc::clone(&fired);

        let handle = Host1xCoreInterface::register_guest_action(
            &host1x,
            1,
            1,
            Box::new(move || {
                fired_clone.fetch_add(1, Ordering::SeqCst);
            }),
        );

        assert!(handle.is_some());
        host1x.syncpoint_manager().increment_host(1);
        assert_eq!(fired.load(Ordering::SeqCst), 0);
        host1x.syncpoint_manager().increment_guest(1);
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }
}

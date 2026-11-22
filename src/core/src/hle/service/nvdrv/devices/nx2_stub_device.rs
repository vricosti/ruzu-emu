// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of eden-nx2/src/core/hle/service/nvdrv/devices/nx2_stub_device.h.

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, OnceLock};

use super::nvdevice::NvDevice;
use crate::hle::service::nvdrv::core::container::SessionId;
use crate::hle::service::nvdrv::nvdata::{DeviceFD, Ioctl, NvResult};

pub static G_NX2_DRIVER: AtomicBool = AtomicBool::new(false);

pub fn nx2_unknown_ioctl(
    device: &str,
    command: Ioctl,
    input: &[u8],
    output: &mut [u8],
) -> NvResult {
    log::warn!(
        "(STUBBED) Switch 2 {} ioctl={:08X} group={:X} command={:02X} in={:#x}",
        device,
        command.raw,
        command.group(),
        command.cmd(),
        input.len()
    );
    output.fill(0);
    let size = input.len().min(output.len());
    output[..size].copy_from_slice(&input[..size]);
    NvResult::Success
}

pub type Listener = Box<dyn Fn(u64, u64, u64) + Send>;
pub type UnmapListener = Box<dyn Fn(u64, u64) + Send>;
pub type ReleaseListener = Box<dyn Fn(u64, u64) + Send>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mapping {
    gpu_address: u64,
    cpu_address: u64,
    size: u64,
}

struct Listeners {
    map: Listener,
    unmap: UnmapListener,
    release: Option<ReleaseListener>,
}

#[derive(Default)]
struct RegistryState {
    mappings: Vec<Mapping>,
    // Upstream const void* owners are opaque identities, ordered as pointer values.
    listeners: BTreeMap<usize, Listeners>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SharedRangeKey {
    asid: usize,
    cpu_address: u64,
    size: u64,
}

struct SharedRange {
    device_address: u64,
    references: u32,
}

#[derive(Default)]
struct DeviceRanges {
    /// cpu start -> (size, device address)
    device_ranges: BTreeMap<u64, (u64, u64)>,
    shared_ranges: BTreeMap<SharedRangeKey, SharedRange>,
}

#[derive(Default)]
pub struct Nx2GpuMemoryRegistry {
    // Separate lock: listeners may record device ranges while holding state.
    device_ranges: Mutex<DeviceRanges>,
    state: Mutex<RegistryState>,
}

impl Nx2GpuMemoryRegistry {
    pub fn get() -> &'static Self {
        static REGISTRY: OnceLock<Nx2GpuMemoryRegistry> = OnceLock::new();
        REGISTRY.get_or_init(Self::default)
    }

    pub fn register(&self, address: u64, size: u64) {
        self.map_at(address, address, size);
    }

    /// End the emulation session after its CPU/service workers have stopped.
    /// The upstream singleton retains mappings until explicit guest unmaps;
    /// forced shutdown need not issue those. None of its addresses or callbacks
    /// may be replayed into a later System. Do not invoke guest unmap/release
    /// callbacks here: the GPU and its address spaces are being destroyed.
    pub(crate) fn reset_session(&self) {
        let mut state = self.state.lock().unwrap();
        *state = RegistryState::default();
        let mut ranges = self.device_ranges.lock().unwrap();
        ranges.device_ranges.clear();
        ranges.shared_ranges.clear();
    }

    pub fn map_at(&self, gpu_address: u64, cpu_address: u64, size: u64) {
        let mut state = self.state.lock().unwrap();
        Self::trim_mappings(&mut state, gpu_address, size);
        state.mappings.push(Mapping {
            gpu_address,
            cpu_address,
            size,
        });
        for listener in state.listeners.values() {
            (listener.map)(gpu_address, cpu_address, size);
        }
    }

    pub fn unmap(&self, gpu_address: u64, size: u64) {
        let mut state = self.state.lock().unwrap();
        Self::trim_mappings(&mut state, gpu_address, size);
        for listener in state.listeners.values() {
            (listener.unmap)(gpu_address, size);
        }
    }

    pub fn release_backing(&self, cpu_address: u64, size: u64) {
        self.device_ranges.lock().unwrap().device_ranges.remove(&cpu_address);
        let state = self.state.lock().unwrap();
        for listener in state.listeners.values() {
            if let Some(release) = &listener.release {
                release(cpu_address, size);
            }
        }
    }

    pub fn add_listener(
        &self,
        owner: usize,
        listener: Listener,
        unmap_listener: UnmapListener,
        release_listener: Option<ReleaseListener>,
    ) {
        let mut state = self.state.lock().unwrap();
        for m in &state.mappings {
            listener(m.gpu_address, m.cpu_address, m.size);
        }
        state.listeners.insert(
            owner,
            Listeners {
                map: listener,
                unmap: unmap_listener,
                release: release_listener,
            },
        );
    }

    pub fn remove_listener(&self, owner: usize) {
        self.state.lock().unwrap().listeners.remove(&owner);
    }

    /// Device (SMMU) range of a mapping outside the heap, shared by every address space of one
    /// process: the caches key memory by device address, so a range per address space made
    /// separate copies of the same memory, and presentation could read one the game never drew
    /// into. The first address space to map the range allocates it with `allocate` (0: out of
    /// space); every call takes a reference.
    /// Lichii nx2_stub_device.h:251-269 (68f8d96368).
    pub fn acquire_device_range(&self, asid: usize, cpu_address: u64, size: u64,
        allocate: &mut dyn FnMut() -> u64) -> u64 {
        let mut ranges = self.device_ranges.lock().unwrap();
        let key = SharedRangeKey { asid, cpu_address, size };
        if let Some(range) = ranges.shared_ranges.get_mut(&key) {
            range.references += 1;
            return range.device_address;
        }
        let device_address = allocate();
        if device_address != 0 {
            ranges.shared_ranges.insert(key, SharedRange { device_address, references: 1 });
        }
        device_address
    }

    /// Drops a reference. True for the last one: the caller frees the range.
    /// Lichii nx2_stub_device.h:271-280 (68f8d96368).
    pub fn release_device_range(&self, asid: usize, cpu_address: u64, size: u64) -> bool {
        let mut ranges = self.device_ranges.lock().unwrap();
        let key = SharedRangeKey { asid, cpu_address, size };
        let Some(range) = ranges.shared_ranges.get_mut(&key) else {
            return false;
        };
        range.references -= 1;
        if range.references != 0 {
            return false;
        }
        ranges.shared_ranges.remove(&key);
        true
    }

    /// The process's session closed and its ASID can be reused: stop sharing its ranges.
    /// Lichii nx2_stub_device.h:282-286 (68f8d96368).
    pub fn forget_device_ranges(&self, asid: usize) {
        self.device_ranges.lock().unwrap().shared_ranges.retain(|key, _| key.asid != asid);
    }

    pub fn record_device_range(&self, cpu_address: u64, size: u64, device_address: u64) {
        self.device_ranges
            .lock()
            .unwrap()
            .device_ranges
            .insert(cpu_address, (size, device_address));
    }

    pub fn device_address_of(&self, cpu_address: u64) -> Option<u64> {
        let ranges = self.device_ranges.lock().unwrap();
        let (&start, &(size, device_address)) = ranges.device_ranges.range(..=cpu_address).next_back()?;
        let offset = cpu_address.wrapping_sub(start);
        if offset >= size {
            return None;
        }
        Some(device_address.wrapping_add(offset))
    }

    // Mechanical state argument lets the caller retain the upstream mutex scope.
    fn trim_mappings(state: &mut RegistryState, gpu_address: u64, size: u64) {
        let end = gpu_address.wrapping_add(size);
        let mut kept = Vec::with_capacity(state.mappings.len() + 1);
        for m in &state.mappings {
            let m_end = m.gpu_address.wrapping_add(m.size);
            if m_end <= gpu_address || m.gpu_address >= end {
                kept.push(*m);
                continue;
            }
            if m.gpu_address < gpu_address {
                kept.push(Mapping {
                    gpu_address: m.gpu_address,
                    cpu_address: m.cpu_address,
                    size: gpu_address.wrapping_sub(m.gpu_address),
                });
            }
            if m_end > end {
                kept.push(Mapping {
                    gpu_address: end,
                    cpu_address: m.cpu_address.wrapping_add(end.wrapping_sub(m.gpu_address)),
                    size: m_end.wrapping_sub(end),
                });
            }
        }
        state.mappings = kept;
    }
}

pub struct Nx2StubDevice {
    name: String,
}

impl Nx2StubDevice {
    // NvDevice has no System-owning base in Rust; this node never accesses System.
    pub fn new(name: String) -> Self {
        Self { name }
    }

    fn stub(
        &self,
        fd: DeviceFD,
        command: Ioctl,
        input: &[u8],
        output: &mut [u8],
        inline_output: &mut [u8],
    ) -> NvResult {
        log::warn!(
            "(STUBBED) {} fd={} ioctl={:08X} group={:X} command={:02X} in={:#x}",
            self.name,
            fd,
            command.raw,
            command.group(),
            command.cmd(),
            input.len()
        );
        output.fill(0);
        let size = input.len().min(output.len());
        output[..size].copy_from_slice(&input[..size]);
        inline_output.fill(0);
        NvResult::Success
    }
}

impl NvDevice for Nx2StubDevice {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn ioctl1(&self, fd: DeviceFD, command: Ioctl, input: &[u8], output: &mut [u8]) -> NvResult {
        self.stub(fd, command, input, output, &mut [])
    }
    fn ioctl2(
        &self,
        fd: DeviceFD,
        command: Ioctl,
        input: &[u8],
        _inline_input: &[u8],
        output: &mut [u8],
    ) -> NvResult {
        self.stub(fd, command, input, output, &mut [])
    }
    fn ioctl3(
        &self,
        fd: DeviceFD,
        command: Ioctl,
        input: &[u8],
        output: &mut [u8],
        inline_output: &mut [u8],
    ) -> NvResult {
        self.stub(fd, command, input, output, inline_output)
    }
    fn on_open(&self, _session_id: SessionId, fd: DeviceFD) {
        log::warn!("(STUBBED) {} opened as fd {}", self.name, fd);
    }
    fn on_close(&self, _fd: DeviceFD) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn session_reset_discards_callbacks_mappings_and_device_addresses() {
        let registry = Nx2GpuMemoryRegistry::default();
        let old_calls = Arc::new(Mutex::new(0));
        let calls = Arc::clone(&old_calls);
        registry.add_listener(1, Box::new(move |_, _, _| *calls.lock().unwrap() += 1),
            Box::new(|_, _| panic!("reset must not invoke old GPU callbacks")),
            Some(Box::new(|_, _| panic!("reset must not release through a destroyed GPU"))));
        registry.map_at(0x1000, 0x9000, 0x1000);
        registry.record_device_range(0x9000, 0x1000, 0x8000);
        registry.reset_session();
        registry.reset_session(); // Failed loads and repeated shutdown are harmless.
        assert_eq!(registry.device_address_of(0x9000), None);
        let replay = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&replay);
        registry.add_listener(2, Box::new(move |g, c, s| seen.lock().unwrap().push((g, c, s))),
            Box::new(|_, _| {}), None);
        assert!(replay.lock().unwrap().is_empty());
        registry.map_at(0x1000, 0xa000, 0x1000);
        registry.unmap(0x1000, 0x1000);
        registry.release_backing(0xa000, 0x1000);
        assert_eq!(*old_calls.lock().unwrap(), 1);
        assert_eq!(*replay.lock().unwrap(), vec![(0x1000, 0xa000, 0x1000)]);
    }

    #[test]
    fn mappings_split_and_replay_with_adjusted_cpu_offsets() {
        let registry = Nx2GpuMemoryRegistry::default();
        registry.map_at(0x1000, 0x9000, 0x4000);
        registry.map_at(0x2000, 0x20000, 0x1000);
        registry.unmap(0x3800, 0x800);
        let replay = Arc::new(Mutex::new(Vec::new()));
        let seen = replay.clone();
        registry.add_listener(
            1,
            Box::new(move |g, c, s| seen.lock().unwrap().push((g, c, s))),
            Box::new(|_, _| {}),
            None,
        );
        assert_eq!(
            *replay.lock().unwrap(),
            vec![
                (0x1000, 0x9000, 0x1000),
                (0x3000, 0xb000, 0x800),
                (0x4000, 0xc000, 0x1000),
                (0x2000, 0x20000, 0x1000),
            ]
        );
        registry.remove_listener(1);
        registry.register(0x6000, 0x1000);
        assert_eq!(replay.lock().unwrap().len(), 4);
    }

    #[test]
    fn callbacks_can_record_ranges_and_release_erases_before_notification() {
        let registry = Arc::new(Nx2GpuMemoryRegistry::default());
        let weak_map = Arc::downgrade(&registry);
        let weak_release = weak_map.clone();
        let events = Arc::new(Mutex::new(Vec::new()));
        let unmaps = events.clone();
        let releases = events.clone();
        registry.add_listener(
            1,
            Box::new(move |_, c, s| {
                weak_map
                    .upgrade()
                    .unwrap()
                    .record_device_range(c, s, 0x8000);
            }),
            Box::new(move |g, s| unmaps.lock().unwrap().push((0, g, s))),
            Some(Box::new(move |c, s| {
                assert_eq!(weak_release.upgrade().unwrap().device_address_of(c), None);
                releases.lock().unwrap().push((1, c, s));
            })),
        );
        registry.register(0x1000, 0x100);
        assert_eq!(registry.device_address_of(0xfff), None);
        assert_eq!(registry.device_address_of(0x1000), Some(0x8000));
        assert_eq!(registry.device_address_of(0x10ff), Some(0x80ff));
        assert_eq!(registry.device_address_of(0x1100), None);
        registry.unmap(0x1000, 0x100);
        assert_eq!(registry.device_address_of(0x1000), Some(0x8000));
        registry.release_backing(0x1000, 0x100);
        assert_eq!(
            *events.lock().unwrap(),
            vec![(0, 0x1000, 0x100), (1, 0x1000, 0x100)]
        );
    }

    // Lichii 68f8d96368: a process's address spaces share one device range per CPU range, so
    // what one address space draws into is what presentation reads through the other.
    #[test]
    fn device_ranges_are_shared_between_a_process_address_spaces_and_counted() {
        let registry = Nx2GpuMemoryRegistry::default();
        let mut allocations = 0;
        let mut next = 0x5000_0000u64;
        let mut allocate = || { allocations += 1; next += 0x4000_0000; next };
        let first = registry.acquire_device_range(3, 0x19_bfb1_6000, 0x1fe_0000, &mut allocate);
        let second = registry.acquire_device_range(3, 0x19_bfb1_6000, 0x1fe_0000, &mut allocate);
        assert_eq!(first, second, "the second address space reuses the first one's range");
        // Another size, CPU address or ASID is another range.
        let other_size = registry.acquire_device_range(3, 0x19_bfb1_6000, 0x1000, &mut allocate);
        let other_asid = registry.acquire_device_range(4, 0x19_bfb1_6000, 0x1fe_0000, &mut allocate);
        assert_ne!(other_size, first);
        assert_ne!(other_asid, first);
        assert_eq!(allocations, 3);
        // The last reference frees it, not the first.
        assert!(!registry.release_device_range(3, 0x19_bfb1_6000, 0x1fe_0000));
        assert!(registry.release_device_range(3, 0x19_bfb1_6000, 0x1fe_0000));
        assert!(!registry.release_device_range(3, 0x19_bfb1_6000, 0x1fe_0000), "unknown range");
        // A failed allocation (0) is not shared.
        assert_eq!(registry.acquire_device_range(5, 0x1000, 0x1000, &mut || 0), 0);
        assert!(!registry.release_device_range(5, 0x1000, 0x1000));
        // A closed session's ranges are forgotten, so a reused ASID allocates anew.
        registry.forget_device_ranges(4);
        let mut fresh = 0;
        let again = registry.acquire_device_range(4, 0x19_bfb1_6000, 0x1fe_0000, &mut || { fresh += 1; 0x7000 });
        assert_eq!((again, fresh), (0x7000, 1));
        // reset_session drops shared ranges too.
        registry.reset_session();
        assert!(!registry.release_device_range(3, 0x19_bfb1_6000, 0x1000));
    }

    #[test]
    fn ioctl_echo_is_bounded_and_zeroes_remaining_outputs() {
        let device = Nx2StubDevice::new("nx2:test".into());
        let command = Ioctl { raw: 0x1234 };
        let mut output = [0xff; 4];
        let mut inline = [0xff; 3];
        assert_eq!(
            device.ioctl3(1, command, &[1, 2], &mut output, &mut inline),
            NvResult::Success
        );
        assert_eq!(output, [1, 2, 0, 0]);
        assert_eq!(inline, [0; 3]);
        assert_eq!(
            device.ioctl2(1, command, &[9; 6], &[7], &mut output),
            NvResult::Success
        );
        assert_eq!(output, [9; 4]);
        assert_eq!(
            nx2_unknown_ioctl("test", command, &[], &mut output),
            NvResult::Success
        );
        assert_eq!(output, [0; 4]);
    }
}

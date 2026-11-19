// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of Fritchii core/hle/service/audio/nx2_sysmix.h and .cpp.
use super::errors::*;
use crate::core::{SysmixSinkStream, SystemRef};
use crate::hle::kernel::k_process::ProcessLock;
use crate::hle::result::{ErrorModule, ResultCode, RESULT_SUCCESS};
use crate::hle::service::{kernel_helpers::ServiceContext, os::event::Event};
use std::sync::{Arc, Mutex};

const READ_OFFSET: u64 = 0x44;
const WRITE_OFFSET: u64 = 0x1004;
const PAYLOAD_OFFSET: u64 = 0x1040;
const BLOCK_FRAMES: u32 = 240;
fn align_up(value: u64, alignment: u64) -> u64 {
    (value + alignment - 1) & !(alignment - 1)
}
pub(super) fn audio_result(code: (u32, u32)) -> ResultCode {
    ResultCode::from_module_description(ErrorModule::Audio, code.1)
}
#[derive(Clone, Copy, Default, Debug)]
pub struct SysmixOutputCounters {
    pub ticks: u64,
    pub copied_frames: u64,
    pub accepted_frames: u64,
    pub nonzero_frames: u64,
    pub admitted_blocks: u64,
    pub backpressure: u64,
    pub faults: u64,
}
pub struct SysmixOutput {
    inner: Mutex<Impl>,
    process: Arc<ProcessLock>,
}
struct Impl {
    system: SystemRef,
    process: Arc<ProcessLock>,
    context: ServiceContext,
    state_event: u32,
    data_event: u32,
    host: Option<Box<dyn SysmixSinkStream>>,
    id: u32,
    buffer: u64,
    size: u64,
    creation_flags: u32,
    format: u32,
    frame_bytes: u32,
    slots: u32,
    consumer: u32,
    data_mode: u32,
    threshold: i32,
    gain: f32,
    open: bool,
    engaged: bool,
    faulted: bool,
    input: [u8; BLOCK_FRAMES as usize * 8],
    output: [i16; BLOCK_FRAMES as usize * 2],
    counters: SysmixOutputCounters,
}
impl Impl {
    fn signal(&self, handle: u32) {
        if let Some(event) = self.context.get_event(handle) {
            event.signal();
        }
    }
    fn close(&mut self) {
        self.open = false;
        self.engaged = false;
        self.data_mode = 0;
        self.host.take();
        self.signal(self.state_event);
    }
    fn fault(&mut self, result: ResultCode) -> ResultCode {
        self.faulted = true;
        self.engaged = false;
        self.counters.faults += 1;
        if let Some(host) = &self.host {
            host.mark_faulted();
        }
        self.signal(self.state_event);
        log::error!(
            "sysmix stream {} fault {:?}: {:?}",
            self.id,
            result,
            self.counters
        );
        result
    }
    fn read_occupancy(&self) -> Option<(u32, u32, u32)> {
        let memory = self.process.lock().unwrap().get_memory()?;
        let memory = memory.access().ok()?;
        let read = memory.read_32_acquire(self.buffer + READ_OFFSET)?;
        if read != self.consumer {
            return None;
        }
        let write = memory.read_32_acquire(self.buffer + WRITE_OFFSET)?;
        if read >= self.slots || write >= self.slots {
            return None;
        }
        Some((
            read,
            write,
            ((write as u64 + self.slots as u64 - read as u64) % self.slots as u64) as u32,
        ))
    }
    fn notify(&self, filled: u32) {
        if self.data_mode == 2 && self.slots - 1 - filled >= self.threshold as u32 {
            self.signal(self.data_event);
        }
    }
}
impl Drop for Impl {
    fn drop(&mut self) {
        self.close();
        self.context.close_event(self.data_event);
        self.context.close_event(self.state_event);
    }
}
impl SysmixOutput {
    pub fn new(
        system: SystemRef,
        process: Arc<ProcessLock>,
        id: u32,
        buffer: u64,
        size: u64,
        creation_flags: u32,
    ) -> Self {
        let mut context = ServiceContext::new_with_system(system, "sysmix".into());
        let state_event = context.create_event("sysmix state".into());
        let data_event = context.create_event("sysmix data".into());
        Self {
            process: process.clone(),
            inner: Mutex::new(Impl {
                system,
                process,
                context,
                state_event,
                data_event,
                host: None,
                id,
                buffer,
                size,
                creation_flags,
                format: 0,
                frame_bytes: 0,
                slots: 0,
                consumer: 0,
                data_mode: 0,
                threshold: 0,
                gain: 1.0,
                open: false,
                engaged: false,
                faulted: false,
                input: [0; BLOCK_FRAMES as usize * 8],
                output: [0; BLOCK_FRAMES as usize * 2],
                counters: SysmixOutputCounters::default(),
            }),
        }
    }
    pub fn validate_region(
        process: &Arc<ProcessLock>,
        buffer: u64,
        size: u64,
        flags: u32,
    ) -> ResultCode {
        if flags != 0x24 {
            return audio_result(RESULT_NOT_SUPPORTED);
        }
        let valid = buffer != 0
            && buffer & 0xfff == 0
            && size & 0xfff == 0
            && size >= 0x2000
            && buffer.checked_add(size).is_some()
            && process.lock().unwrap().get_memory().is_some_and(|memory| {
                memory
                    .access()
                    .is_ok_and(|memory| memory.is_valid_virtual_address_range(buffer, size))
            });
        if valid {
            RESULT_SUCCESS
        } else {
            audio_result(RESULT_INVALID_ADDRESS_INFO)
        }
    }
    pub fn is_format_supported(sample_type: u32, channels: u32, rate: u32) -> bool {
        matches!(sample_type, 2 | 5) && channels == 2 && rate == 48000
    }
    pub fn open(
        &self,
        sample_type: u32,
        channels: u32,
        rate: u32,
        frames: u32,
        prefill: u32,
        flags: u32,
    ) -> ResultCode {
        let mut p = self.inner.lock().unwrap();
        if p.open {
            return audio_result(RESULT_OPERATION_FAILED);
        }
        if !Self::is_format_supported(sample_type, channels, rate) || prefill != 0 || flags != 0 {
            return audio_result(RESULT_NOT_SUPPORTED);
        }
        if frames == 0 || frames >= i32::MAX as u32 {
            return RESULT_INVALID_ARGUMENT;
        }
        let result = Self::validate_region(&p.process, p.buffer, p.size, p.creation_flags);
        if result != RESULT_SUCCESS {
            return result;
        }
        let slots = frames as u64 + 1;
        let frame_bytes = if sample_type == 2 { 4 } else { 8 };
        let minimum = align_up(
            0x1000
                + align_up(0x40 + slots.min(16) * 8, 64)
                + align_up(slots * frame_bytes as u64, 64),
            0x1000,
        );
        if p.size < minimum || PAYLOAD_OFFSET + slots * frame_bytes as u64 > p.size {
            return audio_result(RESULT_INSUFFICIENT_BUFFER);
        }
        if p.state_event == 0 || p.data_event == 0 {
            return audio_result(RESULT_OUT_OF_SESSIONS);
        }
        let indices = p.process.lock().unwrap().get_memory().and_then(|memory| {
            let memory = memory.access().ok()?;
            Some((
                memory.read_32_acquire(p.buffer + READ_OFFSET)?,
                memory.read_32_acquire(p.buffer + WRITE_OFFSET)?,
            ))
        });
        let Some((read, write)) =
            indices.filter(|(r, w)| (*r as u64) < slots && (*w as u64) < slots)
        else {
            return audio_result(RESULT_INVALID_ADDRESS_INFO);
        };
        p.host = p
            .system
            .get()
            .audio_core()
            .and_then(|audio| audio.acquire_sysmix_stream(&format!("sysmix-{}", p.id)));
        if !p.host.as_ref().is_some_and(|host| host.is_ready()) {
            p.host.take();
            return audio_result(RESULT_OPERATION_FAILED);
        }
        p.slots = slots as u32;
        p.format = sample_type;
        p.frame_bytes = frame_bytes;
        p.consumer = read;
        p.faulted = false;
        p.open = true;
        p.engaged = false;
        p.gain = 1.0;
        p.counters = SysmixOutputCounters::default();
        p.signal(p.state_event);
        p.notify(((write as u64 + slots - read as u64) % slots) as u32);
        log::info!(
            "sysmix stream {} host ready: format={} slots={}",
            p.id,
            sample_type,
            slots
        );
        RESULT_SUCCESS
    }
    pub fn close(&self) {
        self.inner.lock().unwrap().close();
    }
    pub fn set_engaged(&self, engaged: bool) -> ResultCode {
        let mut p = self.inner.lock().unwrap();
        if !p.open || p.faulted || !p.host.as_ref().is_some_and(|host| host.is_ready()) {
            return audio_result(RESULT_OPERATION_FAILED);
        }
        if p.engaged == engaged {
            return RESULT_SUCCESS;
        }
        let host = p.host.as_ref().unwrap();
        if engaged {
            host.start();
            if !host.is_ready() || host.is_paused() {
                return audio_result(RESULT_OPERATION_FAILED);
            }
        } else {
            host.stop();
            if !host.is_ready() {
                return p.fault(audio_result(RESULT_OPERATION_FAILED));
            }
        }
        p.engaged = engaged;
        p.signal(p.state_event);
        RESULT_SUCCESS
    }
    pub fn set_data_notification(&self, mode: u32, threshold: i32) -> ResultCode {
        let mut p = self.inner.lock().unwrap();
        if !matches!(mode, 0 | 2) {
            return audio_result(RESULT_NOT_SUPPORTED);
        }
        if !p.open || p.faulted || threshold < 0 || threshold as u32 >= p.slots {
            return RESULT_INVALID_ARGUMENT;
        }
        let Some((_, _, filled)) = p.read_occupancy() else {
            return p.fault(audio_result(RESULT_INVALID_ADDRESS_INFO));
        };
        p.data_mode = mode;
        p.threshold = threshold;
        p.notify(filled);
        RESULT_SUCCESS
    }
    pub fn set_gain(&self, gain: f32, ramp_ns: i64) -> ResultCode {
        if ramp_ns != 0 {
            return audio_result(RESULT_NOT_SUPPORTED);
        }
        if !gain.is_finite() || !(0.0..=128.0).contains(&gain) {
            return RESULT_INVALID_ARGUMENT;
        }
        self.inner.lock().unwrap().gain = gain;
        RESULT_SUCCESS
    }
    pub fn get_gain(&self) -> f32 {
        self.inner.lock().unwrap().gain
    }
    pub fn get_state(&self) -> u32 {
        let p = self.inner.lock().unwrap();
        if p.open && !p.faulted {
            if p.engaged {
                2
            } else {
                1
            }
        } else {
            0
        }
    }
    pub fn get_state_event(&self) -> Option<Arc<Event>> {
        let p = self.inner.lock().unwrap();
        p.context.get_event(p.state_event)
    }
    pub fn get_data_event(&self) -> Option<Arc<Event>> {
        let p = self.inner.lock().unwrap();
        p.context.get_event(p.data_event)
    }
    pub fn get_counters(&self) -> SysmixOutputCounters {
        self.inner.lock().unwrap().counters
    }
    pub fn is_owned_by(&self, process: &Arc<ProcessLock>) -> bool {
        Arc::ptr_eq(&self.process, process)
    }
    pub fn pump(&self) {
        let Ok(mut p) = self.inner.try_lock() else {
            return;
        };
        if !p.open || p.faulted || p.system.get().is_shutting_down() {
            return;
        }
        p.counters.ticks += 1;
        let Some((read, _, filled)) = p.read_occupancy() else {
            p.fault(audio_result(RESULT_INVALID_ADDRESS_INFO));
            return;
        };
        if !p.engaged {
            p.notify(filled);
            return;
        }
        if !p.host.as_ref().unwrap().is_ready() {
            p.fault(audio_result(RESULT_OPERATION_FAILED));
            return;
        }
        let taken = filled.min(BLOCK_FRAMES);
        let first = taken.min(p.slots - read) as usize * p.frame_bytes as usize;
        let total = taken as usize * p.frame_bytes as usize;
        let memory = p.process.lock().unwrap().get_memory().unwrap();
        let memory = memory.access().unwrap();
        let first_address = p.buffer + PAYLOAD_OFFSET + read as u64 * p.frame_bytes as u64;
        let second_address = p.buffer + PAYLOAD_OFFSET;
        if (first != 0 && !memory.read_block_checked(first_address, &mut p.input[..first]))
            || (total > first
                && !memory.read_block_checked(second_address, &mut p.input[first..total]))
        {
            p.fault(audio_result(RESULT_INVALID_ADDRESS_INFO));
            return;
        }
        p.counters.copied_frames += taken as u64;
        p.output.fill(0);
        let mut nonzero = 0u64;
        for frame in 0..taken as usize {
            let mut audible = false;
            for channel in 0..2 {
                let index = frame * 2 + channel;
                let value = if p.format == 2 {
                    i16::from_le_bytes(p.input[index * 2..index * 2 + 2].try_into().unwrap()) as f32
                        / 32768.0
                } else {
                    f32::from_le_bytes(p.input[index * 4..index * 4 + 4].try_into().unwrap())
                };
                if value.is_finite() {
                    p.output[index] = (((value * p.gain).clamp(-1.0, 1.0) * 32768.0) as i32)
                        .clamp(-32768, 32767) as i16;
                    audible |= p.output[index] != 0;
                }
            }
            nonzero += audible as u64;
        }
        if !p
            .host
            .as_ref()
            .unwrap()
            .try_append_output_block(p.counters.admitted_blocks, &p.output)
        {
            if !p.host.as_ref().unwrap().is_ready() {
                p.fault(audio_result(RESULT_OPERATION_FAILED));
            } else {
                p.counters.backpressure += 1;
            }
            return;
        }
        p.counters.admitted_blocks += 1;
        let next = (read + taken) % p.slots;
        if memory.read_32_acquire(p.buffer + READ_OFFSET) != Some(read)
            || !memory.write_exclusive_32(p.buffer + READ_OFFSET, next, read)
            || memory.read_32_acquire(p.buffer + READ_OFFSET) != Some(next)
        {
            p.fault(audio_result(RESULT_INVALID_ADDRESS_INFO));
            return;
        }
        drop(memory);
        p.consumer = next;
        p.counters.accepted_frames += taken as u64;
        p.counters.nonzero_frames += nonzero;
        let Some((_, _, filled)) = p.read_occupancy() else {
            p.fault(audio_result(RESULT_INVALID_ADDRESS_INFO));
            return;
        };
        p.notify(filled);
        if p.counters.admitted_blocks <= 3
            || p.counters.ticks % 400 == 0
            || (nonzero != 0 && p.counters.nonzero_frames == nonzero)
        {
            log::info!(
                "sysmix stream {} output: {:?} queued={}",
                p.id,
                p.counters,
                p.host.as_ref().unwrap().get_queue_size()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::{k_process::KProcess, kernel::ScopedKernelForTest};
    use crate::{
        core::System,
        device_memory::{dram_memory_map, DeviceMemory},
        memory::memory::{Memory, SharedMemory},
    };
    use common::{host_memory::MemoryPermission, page_table::PageTable};
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Host {
        accept: Arc<AtomicBool>,
        blocks: Arc<Mutex<Vec<Vec<i16>>>>,
    }
    impl SysmixSinkStream for Host {
        fn is_ready(&self) -> bool {
            true
        }
        fn is_paused(&self) -> bool {
            false
        }
        fn start(&self) {}
        fn stop(&self) {}
        fn mark_faulted(&self) {}
        fn get_queue_size(&self) -> u32 {
            0
        }
        fn try_append_output_block(&self, _: u64, samples: &[i16]) -> bool {
            if !self.accept.load(Ordering::Relaxed) {
                return false;
            }
            self.blocks.lock().unwrap().push(samples.to_vec());
            true
        }
    }
    #[test]
    fn sysmix_ring_backpressure_wraparound_and_consumer_corruption() {
        // Kernel/current-thread globals in other tests must not share this fixture.
        const CHILD: &str = "RUZU_TEST_SYSMIX_RING";
        if std::env::var_os(CHILD).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "hle::service::audio::nx2_sysmix::tests::sysmix_ring_backpressure_wraparound_and_consumer_corruption"])
                .env(CHILD,"1").status().unwrap().success());
            return;
        }
        let _scope = ScopedKernelForTest::without_kernel();
        let system = Box::new(System::new());
        let device = DeviceMemory::with_size(0x20_000);
        let mut table = PageTable::new();
        table.resize(32, 12);
        let mut memory = unsafe { Memory::new(SystemRef::null(), &device, &device.buffer) };
        memory.set_current_page_table(&mut table, false);
        memory.map_memory_region(
            &mut table,
            0x4000,
            0x2000,
            dram_memory_map::BASE + 0x2000,
            MemoryPermission::READ_WRITE,
            false,
        );
        let memory = Arc::new(SharedMemory::new(memory));
        let mut process = KProcess::new();
        process.memory = Some(memory.clone());
        let process = Arc::new(ProcessLock::from_value(process));
        let stream = SysmixOutput::new(
            SystemRef::from_ref(&system),
            process.clone(),
            1,
            0x4000,
            0x2000,
            0x24,
        );
        assert_eq!(
            SysmixOutput::validate_region(&process, 0x4001, 0x2000, 0x24),
            audio_result(RESULT_INVALID_ADDRESS_INFO)
        );
        assert_eq!(
            SysmixOutput::validate_region(&process, 0x4000, 0x2000, 0),
            audio_result(RESULT_NOT_SUPPORTED)
        );
        assert_eq!(stream.open(2, 2, 48000, 0, 0, 0), RESULT_INVALID_ARGUMENT);
        assert_eq!(
            stream.open(2, 6, 48000, 240, 0, 0),
            audio_result(RESULT_NOT_SUPPORTED)
        );
        assert_eq!(stream.set_gain(f32::NAN, 0), RESULT_INVALID_ARGUMENT);
        assert_eq!(stream.set_gain(1.0, 1), audio_result(RESULT_NOT_SUPPORTED));
        let accept = Arc::new(AtomicBool::new(false));
        let blocks = Arc::new(Mutex::new(Vec::new()));
        {
            let mut p = stream.inner.lock().unwrap();
            p.open = true;
            p.engaged = true;
            p.slots = 4;
            p.consumer = 3;
            p.frame_bytes = 4;
            p.format = 2;
            p.host = Some(Box::new(Host {
                accept: accept.clone(),
                blocks: blocks.clone(),
            }));
        }
        {
            let m = memory.access().unwrap();
            m.write_32_no_rasterizer(0x4000 + READ_OFFSET, 3);
            m.write_32_no_rasterizer(0x4000 + WRITE_OFFSET, 1);
            m.write_block(0x4000 + PAYLOAD_OFFSET + 12, &[1, 0, 2, 0]);
            m.write_block(0x4000 + PAYLOAD_OFFSET, &[3, 0, 4, 0]);
        }
        stream.pump();
        assert_eq!(stream.get_counters().backpressure, 1);
        assert_eq!(
            memory
                .access()
                .unwrap()
                .read_32_acquire(0x4000 + READ_OFFSET),
            Some(3)
        );
        accept.store(true, Ordering::Relaxed);
        stream.pump();
        assert_eq!(
            memory
                .access()
                .unwrap()
                .read_32_acquire(0x4000 + READ_OFFSET),
            Some(1)
        );
        assert_eq!(stream.get_counters().accepted_frames, 2);
        {
            let b = blocks.lock().unwrap();
            assert_eq!(&b[0][..4], &[1, 2, 3, 4]);
            assert!(b[0][4..].iter().all(|v| *v == 0));
        }
        memory
            .access()
            .unwrap()
            .write_32_no_rasterizer(0x4000 + READ_OFFSET, 2);
        stream.pump();
        assert_eq!(stream.get_state(), 0);
        assert_eq!(stream.get_counters().faults, 1);
        assert_eq!(blocks.lock().unwrap().len(), 1);
    }
}

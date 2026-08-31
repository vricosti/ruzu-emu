use crate::common::common::{CpuAddr, MAX_CHANNELS, TARGET_SAMPLE_COUNT};
use crate::renderer::command::util::write_copy;
use crate::sink::sink_stream::{start_sink_stream, SinkBuffer, SinkStreamHandle};
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};

static DEVICE_SINK_CLIP_LOGS: AtomicU64 = AtomicU64::new(0);
static DEVICE_SINK_SAMPLE_TRACES: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct DeviceSinkPayload {
    pub name: [u8; 0x100],
    pub session_id: i32,
    pub _padding0: u32,
    pub sample_buffer: CpuAddr,
    pub sample_count: u64,
    pub input_count: u32,
    pub inputs: [i16; MAX_CHANNELS],
}

#[derive(Debug, Clone, Copy)]
pub struct DeviceSinkCommand {
    pub name: [u8; 0x100],
    pub session_id: i32,
    pub sample_buffer: CpuAddr,
    pub sample_count: u64,
    pub input_count: u32,
    pub inputs: [i16; MAX_CHANNELS],
}

pub fn write_device_payload(cmd: &DeviceSinkCommand, output: &mut [u8]) -> usize {
    let mut payload: DeviceSinkPayload = unsafe { std::mem::zeroed() };
    payload.name = cmd.name;
    payload.session_id = cmd.session_id;
    payload._padding0 = 0;
    payload.sample_buffer = cmd.sample_buffer;
    payload.sample_count = cmd.sample_count;
    payload.input_count = cmd.input_count;
    payload.inputs = cmd.inputs;
    write_copy(&payload, output)
}

fn trace_device_sink_input_clip(
    payload: &DeviceSinkPayload,
    input_count: usize,
    buffer_count: i16,
    frames: usize,
) {
    if std::env::var_os("RUZU_TRACE_DEVICE_SINK_CLIP").is_none() {
        return;
    }

    let inputs: Vec<i16> = payload.inputs.iter().take(input_count).copied().collect();
    let has_negative_input = inputs.iter().any(|&input| input < 0);
    let max_input = inputs
        .iter()
        .filter_map(|&input| (input >= 0).then_some(input as usize))
        .max()
        .unwrap_or(0);
    let required_samples = (max_input + 1).saturating_mul(frames);
    let sample_count = payload.sample_count as usize;

    if payload.sample_buffer == 0 {
        let n = DEVICE_SINK_CLIP_LOGS.fetch_add(1, Ordering::Relaxed);
        if n < 64 || n.is_power_of_two() {
            eprintln!(
                "[DEVICE_SINK_CLIP] #{} null sample_buffer input_count={} buffer_count={} sample_count={} inputs={:?}",
                n, input_count, buffer_count, sample_count, inputs
            );
        }
        return;
    }

    let src =
        unsafe { std::slice::from_raw_parts(payload.sample_buffer as *const i32, sample_count) };
    let mut min_value = i32::MAX;
    let mut max_value = i32::MIN;
    let mut clipped = 0usize;
    let mut visited = 0usize;
    let mut out_of_range_reads = 0usize;
    let mut first_values = Vec::new();

    for &input in &inputs {
        if input < 0 {
            continue;
        }
        let base = (input as usize).saturating_mul(frames);
        let mut channel_first = Vec::new();
        for frame in 0..frames {
            let index = base + frame;
            let Some(&sample) = src.get(index) else {
                out_of_range_reads += 1;
                continue;
            };
            if frame < 8 {
                channel_first.push(sample);
            }
            min_value = min_value.min(sample);
            max_value = max_value.max(sample);
            clipped += usize::from(sample < i16::MIN as i32 || sample > i16::MAX as i32);
            visited += 1;
        }
        first_values.push(channel_first);
    }

    if visited == 0 {
        min_value = 0;
        max_value = 0;
    }

    if clipped > 0
        || out_of_range_reads > 0
        || has_negative_input
        || sample_count < required_samples
    {
        let n = DEVICE_SINK_CLIP_LOGS.fetch_add(1, Ordering::Relaxed);
        if n < 64 || n.is_power_of_two() {
            eprintln!(
                "[DEVICE_SINK_CLIP] #{} sample_buffer=0x{:X} input_count={} buffer_count={} sample_count={} required={} inputs={:?} min={} max={} clipped={} visited={} oor_reads={} first={:?}",
                n,
                payload.sample_buffer,
                input_count,
                buffer_count,
                sample_count,
                required_samples,
                inputs,
                min_value,
                max_value,
                clipped,
                visited,
                out_of_range_reads,
                first_values
            );
        }
    }
}

fn trace_device_sink_samples(
    payload: &DeviceSinkPayload,
    input_count: usize,
    buffer_count: i16,
    frames: usize,
) {
    if !::common::trace::is_enabled(::common::trace::cat::AUDIO_DEVICE_SINK) {
        return;
    }

    let seq = DEVICE_SINK_SAMPLE_TRACES.fetch_add(1, Ordering::Relaxed);
    let sample_count = payload.sample_count as usize;
    let mut min_value = i32::MAX;
    let mut max_value = i32::MIN;
    let mut nonzero = 0usize;
    let mut clipped = 0usize;
    let mut visited = 0usize;
    let mut first = [0i32; 3];

    if payload.sample_buffer != 0 {
        let src = unsafe {
            std::slice::from_raw_parts(payload.sample_buffer as *const i32, sample_count)
        };
        for &input in payload.inputs.iter().take(input_count) {
            if input < 0 {
                continue;
            }
            let base = (input as usize).saturating_mul(frames);
            for frame in 0..frames {
                let Some(&sample) = src.get(base + frame) else {
                    continue;
                };
                if visited < first.len() {
                    first[visited] = sample;
                }
                min_value = min_value.min(sample);
                max_value = max_value.max(sample);
                nonzero += usize::from(sample != 0);
                clipped += usize::from(sample < i16::MIN as i32 || sample > i16::MAX as i32);
                visited += 1;
            }
        }
    }

    if visited == 0 {
        min_value = 0;
        max_value = 0;
    }

    ::common::trace::emit_raw(
        ::common::trace::cat::AUDIO_DEVICE_SINK,
        &[
            seq,
            payload.sample_buffer as u64,
            input_count as u64,
            buffer_count as i64 as u64,
            sample_count as u64,
            frames as u64,
            min_value as u32 as u64,
            max_value as u32 as u64,
            nonzero as u64,
            clipped as u64,
            visited as u64,
            first[0] as u32 as u64,
            first[1] as u32 as u64,
            first[2] as u32 as u64,
        ],
    );
}

impl DeviceSinkPayload {
    pub fn process(self, stream: &SinkStreamHandle, buffer_count: i16) {
        process_device_command(&self, stream, buffer_count);
    }

    pub fn verify(self) -> bool {
        true
    }

    pub fn dump(self, dump: &mut String) {
        let end = self
            .name
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(self.name.len());
        let name = String::from_utf8_lossy(&self.name[..end]);
        let _ = write!(
            dump,
            "DeviceSinkCommand\n\t{} session {} input_count {}\n\tinputs: ",
            name, self.session_id, self.input_count
        );
        for input in self.inputs.iter().take(self.input_count as usize) {
            let _ = write!(dump, "{:02X}, ", input);
        }
        let _ = writeln!(dump);
    }
}

pub fn process_device_command(
    payload: &DeviceSinkPayload,
    stream_handle: &SinkStreamHandle,
    buffer_count: i16,
) {
    let input_count = payload.input_count as usize;
    let frames = TARGET_SAMPLE_COUNT as usize;
    trace_device_sink_input_clip(payload, input_count, buffer_count, frames);
    trace_device_sink_samples(payload, input_count, buffer_count, frames);
    let src = unsafe {
        std::slice::from_raw_parts(
            payload.sample_buffer as *const i32,
            payload.sample_count as usize,
        )
    };
    let mut samples = Vec::with_capacity(frames.saturating_mul(input_count));
    for frame in 0..frames {
        for channel in 0..input_count {
            let input = payload.inputs[channel];
            let buffer_index = input as usize;
            let sample = src[buffer_index * frames + frame];
            samples.push(sample.clamp(i16::MIN as i32, i16::MAX as i32) as i16);
        }
    }
    let profile = std::env::var_os("RUZU_PROFILE_DEVICE_SINK").is_some();
    let t_lock = std::time::Instant::now();
    let mut stream = stream_handle.lock();
    let lock_us = t_lock.elapsed().as_micros();
    let t_work = std::time::Instant::now();
    stream.set_system_channels(input_count as u32);
    let sample_tag = samples.as_ptr() as u64;
    stream.append_buffer(
        SinkBuffer {
            frames: frames as u64,
            frames_played: 0,
            tag: sample_tag,
            consumed: false,
        },
        &samples,
    );
    let should_start = stream.is_paused();
    if profile {
        let work_us = t_work.elapsed().as_micros();
        if lock_us > 1000 || work_us > 1000 {
            log::info!(
                "PROFILE_DEVICE_SINK lock_us={} work_us={} frames={} input_count={}",
                lock_us,
                work_us,
                frames,
                input_count
            );
        }
    }
    drop(stream);
    if should_start {
        start_sink_stream(stream_handle, false);
    }
}

pub fn verify_device_command(_payload: &DeviceSinkPayload) -> bool {
    true
}

pub fn dump_device_command(payload: &DeviceSinkPayload, dump: &mut String) {
    let end = payload
        .name
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(payload.name.len());
    let name = String::from_utf8_lossy(&payload.name[..end]);
    let _ = write!(
        dump,
        "DeviceSinkCommand\n\t{} session {} input_count {}\n\tinputs: ",
        name, payload.session_id, payload.input_count
    );
    for input in payload.inputs.iter().take(payload.input_count as usize) {
        let _ = write!(dump, "{:02X}, ", input);
    }
    let _ = writeln!(dump);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::sink_stream::{SinkStream, StreamType};
    use crate::SharedSystem;
    use parking_lot::Mutex;
    use std::sync::Arc;

    fn make_system() -> SharedSystem {
        crate::make_test_system()
    }

    #[test]
    fn verify_matches_upstream_and_accepts_payload_unconditionally() {
        let payload = DeviceSinkPayload {
            input_count: 2,
            inputs: [-1, 1, 0, 0, 0, 0],
            ..unsafe { std::mem::zeroed() }
        };

        assert!(payload.verify());
        assert!(verify_device_command(&payload));
    }

    #[test]
    fn process_uses_target_sample_count_frames() {
        let system = make_system();
        let stream = Arc::new(Mutex::new(SinkStream::new(system, StreamType::Render)));
        let samples = vec![123i32; (TARGET_SAMPLE_COUNT as usize) * 4];
        let payload = DeviceSinkPayload {
            session_id: 0,
            sample_buffer: samples.as_ptr() as CpuAddr,
            sample_count: samples.len() as u64,
            input_count: 2,
            inputs: [0, 1, 0, 0, 0, 0],
            ..unsafe { std::mem::zeroed() }
        };

        payload.process(&stream, 2);

        let stream = stream.lock();
        assert_eq!(stream.get_queue_size(), 1);
        assert_eq!(
            stream.queued_buffer_front().unwrap().frames,
            TARGET_SAMPLE_COUNT as u64
        );
        assert_ne!(stream.queued_buffer_front().unwrap().tag, 0);
    }
}

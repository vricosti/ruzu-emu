use crate::common::common::{MAX_CHANNELS, TARGET_SAMPLE_COUNT, TARGET_SAMPLE_RATE};
use crate::SharedSystem;
use std::cmp::min;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::sync::{Condvar, Mutex as StdMutex};
use std::time::Duration;

use parking_lot::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamType {
    Render,
    Out,
    In,
}

#[derive(Debug, Clone, Default)]
pub struct SinkBuffer {
    pub frames: u64,
    pub frames_played: u64,
    pub tag: u64,
    pub consumed: bool,
}

pub type SharedSinkStreamBase = Arc<Mutex<SinkStreamBase>>;
pub type SinkStreamHandle = Arc<dyn SinkStream>;

/// Lifecycle state serialized by each concrete backend stream.
///
/// Eden serializes these transitions through stream ownership. Rust exposes
/// streams through shared `Arc` handles, so the concrete backends make the
/// same ordering explicit without exposing this state to audio callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SinkStreamLifecycleState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Finalized,
}

/// Rust counterpart of upstream's virtual `SinkStream` interface.
///
/// Backend lifecycle methods remain owned by the concrete backend stream. The
/// shared base contains only the callback-visible queue and stream state, so a
/// synchronous native `stop` never holds a lock needed by the callback it is
/// waiting to finish.
pub trait SinkStream: Send + Sync {
    fn base(&self) -> &SharedSinkStreamBase;

    fn finalize(&self) {}
    fn start(&self, _resume: bool) {}
    fn stop(&self) {}

    fn is_paused(&self) -> bool {
        self.base().lock().is_paused()
    }

    fn get_system_channels(&self) -> u32 {
        self.base().lock().get_system_channels()
    }

    fn set_system_channels(&self, channels: u32) {
        self.base().lock().set_system_channels(channels);
    }

    fn get_device_channels(&self) -> u32 {
        self.base().lock().get_device_channels()
    }

    fn get_system_volume(&self) -> f32 {
        self.base().lock().get_system_volume()
    }

    fn get_device_volume(&self) -> f32 {
        self.base().lock().get_device_volume()
    }

    fn set_system_volume(&self, volume: f32) {
        self.base().lock().set_system_volume(volume);
    }

    fn set_device_volume(&self, volume: f32) {
        self.base().lock().set_device_volume(volume);
    }

    fn get_queue_size(&self) -> u32 {
        self.base().lock().get_queue_size()
    }

    fn set_ring_size(&self, ring_size: u32) {
        self.base().lock().set_ring_size(ring_size);
    }

    fn append_buffer(&self, buffer: SinkBuffer, samples: &[i16]) {
        self.base().lock().append_buffer(buffer, samples);
    }

    fn release_buffer(&self, num_samples: u64) -> Vec<i16> {
        self.base().lock().release_buffer(num_samples)
    }

    fn clear_queue(&self) {
        self.base().lock().clear_queue();
    }

    fn process_audio_in(&self, input_buffer: &[i16], num_frames: usize) {
        self.base()
            .lock()
            .process_audio_in(input_buffer, num_frames);
    }

    fn process_audio_out_and_render(&self, output_buffer: &mut [i16], num_frames: usize) {
        self.base()
            .lock()
            .process_audio_out_and_render(output_buffer, num_frames);
    }

    fn get_expected_played_sample_count(&self) -> u64 {
        self.base().lock().get_expected_played_sample_count()
    }

    fn wait_free_space(&self) {
        let release = Arc::clone(&self.base().lock().release);
        static NOT_STOPPED: AtomicBool = AtomicBool::new(false);
        release.wait_free_space_with_stop(&NOT_STOPPED);
    }

    fn wait_free_space_with_stop(&self, stop_requested: &AtomicBool) {
        let release = Arc::clone(&self.base().lock().release);
        release.wait_free_space_with_stop(stop_requested);
    }
}

pub fn start_sink_stream(stream: &SinkStreamHandle, resume: bool) {
    stream.start(resume);
}

pub fn stop_sink_stream(stream: &SinkStreamHandle) {
    stream.stop();
}

/// Shared synchronization state for wait_free_space / buffer release.
/// This is extracted so the ADSP thread can wait without holding the
/// SinkStream parking_lot::Mutex (which the cubeb callback also needs).
pub struct ReleaseSync {
    pub queued_buffers: AtomicU32,
    pub max_queue_size: AtomicU32,
    pub paused: AtomicBool,
    pub realtime_pacing: AtomicBool,
    pub cv: Condvar,
    pub mutex: StdMutex<()>,
}

impl ReleaseSync {
    fn new() -> Self {
        Self {
            queued_buffers: AtomicU32::new(0),
            max_queue_size: AtomicU32::new(0),
            paused: AtomicBool::new(true),
            realtime_pacing: AtomicBool::new(false),
            cv: Condvar::new(),
            mutex: StdMutex::new(()),
        }
    }

    /// Matches upstream SinkStream::WaitFreeSpace.
    pub fn wait_free_space_with_stop(&self, stop_requested: &AtomicBool) {
        let max = self.max_queue_size.load(Ordering::Acquire);
        if max == 0 {
            return;
        }

        let guard = self.mutex.lock().expect("release mutex poisoned");

        if self.realtime_pacing.load(Ordering::Acquire)
            && !self.paused.load(Ordering::Acquire)
            && self.queued_buffers.load(Ordering::Acquire) < max
            && !stop_requested.load(Ordering::SeqCst)
        {
            let (_guard, _) = self
                .cv
                .wait_timeout(guard, Duration::from_millis(5))
                .expect("release condvar poisoned");
            return;
        }

        let (mut guard, _) = self
            .cv
            .wait_timeout_while(guard, Duration::from_millis(5), |_| {
                !self.paused.load(Ordering::Acquire)
                    && self.queued_buffers.load(Ordering::Acquire) >= max
                    && !stop_requested.load(Ordering::SeqCst)
            })
            .expect("release condvar poisoned");

        while !self.paused.load(Ordering::Acquire)
            && self.queued_buffers.load(Ordering::Acquire) > max + 3
            && !stop_requested.load(Ordering::SeqCst)
        {
            let (new_guard, _) = self
                .cv
                .wait_timeout(guard, Duration::from_millis(5))
                .expect("release condvar poisoned");
            guard = new_guard;
        }
        drop(guard);
    }
}

pub struct SinkStreamBase {
    pub system: SharedSystem,
    pub stream_type: StreamType,
    pub system_channels: u32,
    pub device_channels: u32,
    pub name: String,
    queue: VecDeque<SinkBuffer>,
    playing_buffer: SinkBuffer,
    samples: VecDeque<i16>,
    last_frame: [i16; MAX_CHANNELS],
    min_played_sample_count: u64,
    max_played_sample_count: u64,
    last_sample_count_update_time: Duration,
    system_volume: f32,
    device_volume: f32,
    discard_buffers: bool,
    /// Shared release synchronization (atomics + condvar).
    pub release: Arc<ReleaseSync>,
}

impl SinkStreamBase {
    pub fn new(
        system: SharedSystem,
        stream_type: StreamType,
        system_channels: u32,
        device_channels: u32,
        name: String,
    ) -> Self {
        Self {
            system,
            stream_type,
            system_channels,
            device_channels,
            name,
            queue: VecDeque::new(),
            playing_buffer: SinkBuffer {
                consumed: true,
                ..SinkBuffer::default()
            },
            samples: VecDeque::new(),
            last_frame: [0; MAX_CHANNELS],
            min_played_sample_count: 0,
            max_played_sample_count: 0,
            last_sample_count_update_time: Duration::ZERO,
            system_volume: 1.0,
            device_volume: 1.0,
            discard_buffers: false,
            release: Arc::new(ReleaseSync::new()),
        }
    }

    pub fn set_discard_buffers(&mut self, discard: bool) {
        self.discard_buffers = discard;
    }

    pub fn set_realtime_pacing(&mut self, enabled: bool) {
        self.release
            .realtime_pacing
            .store(enabled, Ordering::Release);
    }

    pub fn is_paused(&self) -> bool {
        self.release.paused.load(Ordering::Acquire)
    }

    pub fn signal_start(&mut self) -> bool {
        if !self.is_paused() {
            return false;
        }
        self.release.paused.store(false, Ordering::Release);
        true
    }

    pub fn signal_stop(&mut self) -> bool {
        if self.is_paused() {
            return false;
        }
        self.signal_pause();
        true
    }

    pub fn get_system_channels(&self) -> u32 {
        self.system_channels
    }

    pub fn set_system_channels(&mut self, channels: u32) {
        self.system_channels = channels;
    }

    pub fn get_device_channels(&self) -> u32 {
        self.device_channels
    }

    pub fn get_system_volume(&self) -> f32 {
        self.system_volume
    }

    pub fn get_device_volume(&self) -> f32 {
        self.device_volume
    }

    pub fn set_system_volume(&mut self, volume: f32) {
        self.system_volume = volume;
    }

    pub fn set_device_volume(&mut self, volume: f32) {
        self.device_volume = volume;
    }

    pub fn get_queue_size(&self) -> u32 {
        self.release.queued_buffers.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn queued_buffer_front(&self) -> Option<&SinkBuffer> {
        self.queue.front()
    }

    pub fn set_ring_size(&mut self, ring_size: u32) {
        self.release
            .max_queue_size
            .store(ring_size, Ordering::Release);
    }

    pub fn append_buffer(&mut self, mut buffer: SinkBuffer, samples: &[i16]) {
        if self.discard_buffers {
            return;
        }

        if self.stream_type == StreamType::In {
            return;
        }

        if std::env::var_os("RUZU_TRACE_AUDIO_APPEND_CLIP").is_some() && samples.len() >= 100 {
            let mut min_sample = i16::MAX;
            let mut max_sample = i16::MIN;
            let mut clipped = 0usize;
            for &sample in samples {
                min_sample = min_sample.min(sample);
                max_sample = max_sample.max(sample);
                if sample == i16::MIN || sample == i16::MAX {
                    clipped += 1;
                }
            }
            if clipped * 100 / samples.len() >= 1 {
                use std::sync::atomic::{AtomicU64, Ordering};
                static TRACE_COUNT: AtomicU64 = AtomicU64::new(0);
                let n = TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
                if n < 16 || n.is_power_of_two() {
                    eprintln!(
                        "[AUDIO_APPEND_CLIP] #{} stream={:?} name={} tag=0x{:X} frames={} samples={} sys_ch={} dev_ch={} min={} max={} clipped={} clipped_pct={}",
                        n,
                        self.stream_type,
                        self.name,
                        buffer.tag,
                        buffer.frames,
                        samples.len(),
                        self.system_channels,
                        self.device_channels,
                        min_sample,
                        max_sample,
                        clipped,
                        clipped * 100 / samples.len()
                    );
                }
            }
        }

        let queued_buffer = {
            buffer.consumed = false;
            buffer.frames_played = 0;
            buffer
        };

        let settings = common::settings::values();
        let mut yuzu_volume = common::settings::volume(&settings);
        if yuzu_volume > 1.0 {
            yuzu_volume = 0.6 + 20.0 * yuzu_volume.log10();
        }
        let volume = self.system_volume * self.device_volume * yuzu_volume;
        if std::env::var_os("RUZU_TRACE_HWOPUS_AUDIO").is_some() {
            use std::sync::atomic::{AtomicU64, Ordering};
            static TRACE_COUNT: AtomicU64 = AtomicU64::new(0);
            let trace_index = TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
            if trace_index < 512 || trace_index.is_power_of_two() {
                let mut peaks = vec![0u16; self.system_channels as usize];
                for (sample_index, sample) in samples.iter().enumerate() {
                    let channel = sample_index % self.system_channels as usize;
                    peaks[channel] = peaks[channel].max(sample.unsigned_abs());
                }
                eprintln!(
                    "[AUDIO_APPEND_CHANNELS] #{} stream={:?} name={} tag=0x{:X} frames={} sys_ch={} dev_ch={} peaks={:?}",
                    trace_index,
                    self.stream_type,
                    self.name,
                    queued_buffer.tag,
                    queued_buffer.frames,
                    self.system_channels,
                    self.device_channels,
                    peaks
                );
            }
        }
        if self.system_channels == 6 && self.device_channels == 2 {
            // Match yuzu's 6ch->2ch sink downmix.
            const DOWN_MIX_COEFF: [f32; 4] = [1.0, 0.596, 0.354, 0.707];

            for frame in samples.chunks_exact(self.system_channels as usize) {
                let fl = frame[0] as f32;
                let fr = frame[1] as f32;
                let c = frame[2] as f32;
                let lfe = frame[3] as f32;
                let bl = frame[4] as f32;
                let br = frame[5] as f32;

                let left = (fl * DOWN_MIX_COEFF[0]
                    + c * DOWN_MIX_COEFF[1]
                    + lfe * DOWN_MIX_COEFF[2]
                    + bl * DOWN_MIX_COEFF[3])
                    * volume;
                let right = (fr * DOWN_MIX_COEFF[0]
                    + c * DOWN_MIX_COEFF[1]
                    + lfe * DOWN_MIX_COEFF[2]
                    + br * DOWN_MIX_COEFF[3])
                    * volume;

                self.samples.push_back(clamp_i16(left));
                self.samples.push_back(clamp_i16(right));
            }
        } else if self.system_channels == 2 && self.device_channels == 6 {
            // Match yuzu's current passthrough-style 2ch->6ch expansion.
            for frame in samples.chunks_exact(self.system_channels as usize) {
                self.samples.push_back(clamp_i16(frame[0] as f32 * volume));
                self.samples.push_back(clamp_i16(frame[1] as f32 * volume));
                self.samples.push_back(0);
                self.samples.push_back(0);
                self.samples.push_back(0);
                self.samples.push_back(0);
            }
        } else {
            for sample in samples {
                self.samples.push_back(clamp_i16(*sample as f32 * volume));
            }
        }
        self.queue.push_back(queued_buffer);
        self.release.queued_buffers.fetch_add(1, Ordering::Release);
    }

    pub fn release_buffer(&mut self, num_samples: u64) -> Vec<i16> {
        if self.discard_buffers {
            return Vec::new();
        }

        let count = min(num_samples as usize, self.samples.len());
        let mut out = Vec::with_capacity(num_samples as usize);
        for _ in 0..count {
            out.push(self.samples.pop_front().unwrap_or_default());
        }
        let volume = if self.stream_type == StreamType::In {
            self.system_volume * self.device_volume * 8.0
        } else {
            1.0
        };
        if volume != 1.0 {
            for sample in &mut out {
                *sample = clamp_i16(*sample as f32 * volume);
            }
        }
        if out.len() < num_samples as usize {
            out.resize(num_samples as usize, 0);
        }
        out
    }

    pub fn clear_queue(&mut self) {
        self.samples.clear();
        self.queue.clear();
        self.release.queued_buffers.store(0, Ordering::Release);
        self.playing_buffer = SinkBuffer {
            consumed: true,
            ..SinkBuffer::default()
        };
        self.release.cv.notify_one();
    }

    pub fn get_expected_played_sample_count(&self) -> u64 {
        let current_time = self.system.get().core_timing().get_global_time_ns();
        let elapsed = current_time.saturating_sub(self.last_sample_count_update_time);
        let expected_delta = (TARGET_SAMPLE_RATE as u128 * elapsed.as_nanos()) / 1_000_000_000u128;
        self.min_played_sample_count
            .saturating_add(expected_delta as u64)
            .min(self.max_played_sample_count)
            .saturating_add(TARGET_SAMPLE_COUNT as u64 * 5)
    }

    pub fn wait_free_space(&self) {
        static NOT_STOPPED: AtomicBool = AtomicBool::new(false);
        self.release.wait_free_space_with_stop(&NOT_STOPPED);
    }

    pub fn wait_free_space_with_stop(&self, stop_requested: &AtomicBool) {
        self.release.wait_free_space_with_stop(stop_requested);
    }

    pub fn process_audio_in(&mut self, input_buffer: &[i16], num_frames: usize) {
        let (paused, shutting_down) = {
            let sys = self.system.get();
            (sys.is_paused(), sys.is_shutting_down())
        };
        if paused || shutting_down {
            return;
        }

        let frame_size = self.device_channels as usize;
        let frame_size = frame_size.max(1);
        let frame_size_bytes = frame_size.min(MAX_CHANNELS) * std::mem::size_of::<i16>();
        let mut frames_written = 0usize;

        while frames_written < num_frames {
            if self.playing_buffer.consumed || self.playing_buffer.frames == 0 {
                let Some(buffer) = self.queue.pop_front() else {
                    for sample in input_buffer.iter().skip(frames_written * frame_size) {
                        self.samples.push_back(*sample);
                    }
                    frames_written = num_frames;
                    continue;
                };
                self.playing_buffer = buffer;
                self.release.queued_buffers.fetch_sub(1, Ordering::Release);
                self.release.cv.notify_one();
            }

            let frames_available = (self.playing_buffer.frames - self.playing_buffer.frames_played)
                .min((num_frames - frames_written) as u64)
                as usize;
            let sample_start = frames_written * frame_size;
            let sample_end = sample_start + frames_available * frame_size;
            for sample in &input_buffer[sample_start..sample_end] {
                self.samples.push_back(*sample);
            }

            frames_written += frames_available;
            self.playing_buffer.frames_played += frames_available as u64;
            if self.playing_buffer.frames_played >= self.playing_buffer.frames {
                self.playing_buffer.consumed = true;
            }
        }

        if num_frames > 0 && frame_size_bytes > 0 {
            let last_frame_start = (num_frames - 1) * frame_size;
            let copy_len = frame_size.min(MAX_CHANNELS);
            self.last_frame[..copy_len]
                .copy_from_slice(&input_buffer[last_frame_start..last_frame_start + copy_len]);
        }

        self.last_sample_count_update_time = self.system.get().core_timing().get_global_time_ns();
        self.min_played_sample_count = self.max_played_sample_count;
        self.max_played_sample_count = self
            .max_played_sample_count
            .saturating_add(frames_written as u64);
    }

    pub fn process_audio_out_and_render(&mut self, output_buffer: &mut [i16], num_frames: usize) {
        let (paused, shutting_down) = {
            let sys = self.system.get();
            (sys.is_paused(), sys.is_shutting_down())
        };
        if paused || shutting_down {
            if shutting_down {
                self.release.queued_buffers.store(0, Ordering::Release);
                self.release.cv.notify_one();
            }
            output_buffer.fill(0);
            return;
        }

        let frame_size = self.device_channels as usize;
        let frame_size = frame_size.max(1);
        let mut frames_written = 0usize;
        let mut actual_frames_written = 0usize;

        while frames_written < num_frames {
            if self.playing_buffer.consumed || self.playing_buffer.frames == 0 {
                let Some(buffer) = self.queue.pop_front() else {
                    for frame in frames_written..num_frames {
                        let base = frame * frame_size;
                        output_buffer[base..base + frame_size]
                            .copy_from_slice(&self.last_frame[..frame_size]);
                    }
                    frames_written = num_frames;
                    continue;
                };
                self.playing_buffer = buffer;
                self.release.queued_buffers.fetch_sub(1, Ordering::Release);
                self.release.cv.notify_one();
            }

            let frames_available = (self.playing_buffer.frames - self.playing_buffer.frames_played)
                .min((num_frames - frames_written) as u64)
                as usize;
            let samples_to_pop = frames_available * frame_size;
            let base = frames_written * frame_size;
            for sample in &mut output_buffer[base..base + samples_to_pop] {
                *sample = self.samples.pop_front().unwrap_or(0);
            }

            frames_written += frames_available;
            actual_frames_written += frames_available;
            self.playing_buffer.frames_played += frames_available as u64;
            if self.playing_buffer.frames_played >= self.playing_buffer.frames {
                self.playing_buffer.consumed = true;
            }
        }

        if num_frames > 0 {
            let last_frame_start = (num_frames - 1) * frame_size;
            let copy_len = frame_size.min(MAX_CHANNELS);
            self.last_frame[..copy_len]
                .copy_from_slice(&output_buffer[last_frame_start..last_frame_start + copy_len]);
        }

        self.last_sample_count_update_time = self.system.get().core_timing().get_global_time_ns();
        self.min_played_sample_count = self.max_played_sample_count;
        self.max_played_sample_count = self
            .max_played_sample_count
            .saturating_add(actual_frames_written as u64);
    }

    pub fn signal_pause(&mut self) {
        self.release.paused.store(true, Ordering::Release);
        self.release.cv.notify_one();
    }
}

/// Base-only stream used when a selected backend could not create a native
/// stream. Concrete working backends own their own stream type instead.
pub struct BaseSinkStream {
    base: SharedSinkStreamBase,
}

impl BaseSinkStream {
    pub fn new(
        system: SharedSystem,
        stream_type: StreamType,
        system_channels: u32,
        device_channels: u32,
        name: String,
    ) -> Self {
        Self {
            base: Arc::new(Mutex::new(SinkStreamBase::new(
                system,
                stream_type,
                system_channels,
                device_channels,
                name,
            ))),
        }
    }
}

impl SinkStream for BaseSinkStream {
    fn base(&self) -> &SharedSinkStreamBase {
        &self.base
    }
}

fn clamp_i16(sample: f32) -> i16 {
    sample.clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    fn make_system() -> SharedSystem {
        crate::make_test_system()
    }

    #[test]
    fn expected_played_sample_count_is_tracked_in_frames() {
        let system = make_system();
        let mut stream = SinkStreamBase::new(system, StreamType::Out, 2, 2, String::new());
        stream.append_buffer(
            SinkBuffer {
                frames: 2,
                frames_played: 0,
                tag: 1,
                consumed: false,
            },
            &[1, 2, 3, 4],
        );

        let mut output = [0i16; 4];
        stream.process_audio_out_and_render(&mut output, 2);

        assert_eq!(output, [1, 2, 3, 4]);
        assert_eq!(
            stream.get_expected_played_sample_count(),
            TARGET_SAMPLE_COUNT as u64 * 5
        );
    }

    #[test]
    fn process_audio_in_captures_frames_without_queued_buffers() {
        let system = make_system();
        let mut stream = SinkStreamBase::new(system, StreamType::In, 2, 2, String::new());
        stream.append_buffer(
            SinkBuffer {
                frames: 2,
                frames_played: 0,
                tag: 7,
                consumed: false,
            },
            &[],
        );

        assert_eq!(stream.get_queue_size(), 0);

        stream.process_audio_in(&[10, 11, 12, 13], 2);

        assert_eq!(stream.get_queue_size(), 0);
        assert_eq!(stream.release_buffer(4), vec![80, 88, 96, 104]);
    }

    #[test]
    fn append_buffer_downmixes_six_channels_to_two() {
        let system = make_system();
        let mut stream = SinkStreamBase::new(system, StreamType::Render, 6, 2, String::new());

        stream.append_buffer(
            SinkBuffer {
                frames: 1,
                frames_played: 0,
                tag: 1,
                consumed: false,
            },
            &[100, 200, 300, 400, 500, 600],
        );

        assert_eq!(stream.release_buffer(2), vec![773, 944]);
    }

    #[test]
    fn append_buffer_expands_two_channels_to_six() {
        let system = make_system();
        let mut stream = SinkStreamBase::new(system, StreamType::Render, 2, 6, String::new());

        stream.append_buffer(
            SinkBuffer {
                frames: 1,
                frames_played: 0,
                tag: 1,
                consumed: false,
            },
            &[10, 20],
        );

        assert_eq!(stream.release_buffer(6), vec![10, 20, 0, 0, 0, 0]);
    }

    #[test]
    fn release_buffer_applies_audio_in_gain() {
        let system = make_system();
        let mut stream = SinkStreamBase::new(system, StreamType::In, 2, 2, String::new());
        stream.samples.extend([10, -20]);

        assert_eq!(stream.release_buffer(2), vec![80, -160]);
    }

    #[test]
    fn backend_lifecycle_is_owned_by_the_concrete_stream() {
        struct TestStream {
            base: SharedSinkStreamBase,
            calls: Arc<AtomicU32>,
        }

        impl SinkStream for TestStream {
            fn base(&self) -> &SharedSinkStreamBase {
                &self.base
            }

            fn start(&self, _resume: bool) {
                assert!(self.base.lock().signal_start());
                assert!(self.base.try_lock().is_some());
                self.calls.fetch_add(1, Ordering::Relaxed);
            }

            fn stop(&self) {
                assert!(self.base.lock().signal_stop());
                assert!(self.base.try_lock().is_some());
                self.calls.fetch_add(1, Ordering::Relaxed);
            }
        }

        let calls = Arc::new(AtomicU32::new(0));
        let stream: SinkStreamHandle = Arc::new(TestStream {
            base: Arc::new(Mutex::new(SinkStreamBase::new(
                make_system(),
                StreamType::Out,
                2,
                2,
                String::new(),
            ))),
            calls: Arc::clone(&calls),
        });

        start_sink_stream(&stream, false);
        stop_sink_stream(&stream);

        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert!(stream.is_paused());
    }
}

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

pub type SinkStreamHandle = Arc<Mutex<SinkStream>>;

/// Callback to start/stop the audio backend (e.g. cubeb_stream_start/stop).
/// Matches upstream CubebSinkStream::Start/Stop which call cubeb_stream_start/stop.
pub type BackendStartStopFn = Arc<dyn Fn(bool) + Send + Sync>;

/// Start a sink stream without holding its state mutex while entering the
/// backend. Audio backends may wait for an in-flight callback, and that
/// callback also needs the state mutex.
pub fn start_sink_stream(stream: &SinkStreamHandle, _resume: bool) {
    let transition = Arc::clone(&stream.lock().backend_transition);
    let _transition_guard = transition.lock();
    let backend_ctl = stream.lock().prepare_start();
    if let Some(backend_ctl) = backend_ctl {
        backend_ctl(true);
    }
}

/// Stop a sink stream without holding its state mutex while entering the
/// backend. This preserves upstream's ordering: mark the stream paused, then
/// synchronously stop the native stream.
pub fn stop_sink_stream(stream: &SinkStreamHandle) {
    let transition = Arc::clone(&stream.lock().backend_transition);
    let _transition_guard = transition.lock();
    let backend_ctl = stream.lock().prepare_stop();
    if let Some(backend_ctl) = backend_ctl {
        backend_ctl(false);
    }
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

pub struct SinkStream {
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
    /// Backend start/stop callback. Called with `true` to start, `false` to stop.
    backend_ctl: Option<BackendStartStopFn>,
    /// Serializes native start/stop calls without blocking the audio callback
    /// from accessing the stream state.
    backend_transition: Arc<Mutex<()>>,
}

impl SinkStream {
    pub fn new(system: SharedSystem, stream_type: StreamType) -> Self {
        Self {
            system,
            stream_type,
            system_channels: 2,
            device_channels: 2,
            name: String::new(),
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
            backend_ctl: None,
            backend_transition: Arc::new(Mutex::new(())),
        }
    }

    pub fn finalize(&mut self) {}

    /// Set the backend start/stop callback. Called by the cubeb sink after creating
    /// the backend stream.
    pub fn set_backend_ctl(&mut self, ctl: Box<dyn Fn(bool) + Send + Sync>) {
        self.backend_ctl = Some(Arc::from(ctl));
    }

    pub fn set_discard_buffers(&mut self, discard: bool) {
        self.discard_buffers = discard;
    }

    pub fn set_realtime_pacing(&mut self, enabled: bool) {
        self.release
            .realtime_pacing
            .store(enabled, Ordering::Release);
    }

    fn prepare_start(&mut self) -> Option<BackendStartStopFn> {
        if !self.release.paused.load(Ordering::Acquire) {
            return None;
        }
        self.release.paused.store(false, Ordering::Release);
        self.backend_ctl.clone()
    }

    fn prepare_stop(&mut self) -> Option<BackendStartStopFn> {
        if self.release.paused.load(Ordering::Acquire) {
            return None;
        }
        self.signal_pause();
        self.backend_ctl.clone()
    }

    pub fn is_paused(&self) -> bool {
        self.release.paused.load(Ordering::Acquire)
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
        let mut stream = SinkStream::new(system, StreamType::Out);
        stream.device_channels = 2;
        stream.system_channels = 2;
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
        let mut stream = SinkStream::new(system, StreamType::In);
        stream.device_channels = 2;
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
        let mut stream = SinkStream::new(system, StreamType::Render);
        stream.system_channels = 6;
        stream.device_channels = 2;

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
        let mut stream = SinkStream::new(system, StreamType::Render);
        stream.system_channels = 2;
        stream.device_channels = 6;

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
        let mut stream = SinkStream::new(system, StreamType::In);
        stream.samples.extend([10, -20]);

        assert_eq!(stream.release_buffer(2), vec![80, -160]);
    }

    #[test]
    fn backend_start_stop_runs_without_holding_stream_mutex() {
        let stream = Arc::new(Mutex::new(SinkStream::new(make_system(), StreamType::Out)));
        let weak_stream = Arc::downgrade(&stream);
        let calls = Arc::new(AtomicU32::new(0));
        let callback_calls = Arc::clone(&calls);
        stream.lock().set_backend_ctl(Box::new(move |_| {
            let stream = weak_stream.upgrade().expect("stream must remain alive");
            assert!(
                stream.try_lock().is_some(),
                "backend control called while holding the stream mutex"
            );
            callback_calls.fetch_add(1, Ordering::Relaxed);
        }));

        start_sink_stream(&stream, false);
        stop_sink_stream(&stream);

        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert!(stream.lock().is_paused());
    }
}

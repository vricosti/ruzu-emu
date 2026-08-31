use crate::audio_event::Type as AudioEventType;
use crate::audio_manager::AudioManager;
use crate::common::common::SampleFormat;
use crate::device::guest_memory::SharedGuestMemory;
use crate::sink::{start_sink_stream, stop_sink_stream, SinkHandle, SinkStreamHandle, StreamType};
use crate::SharedSystem;
use log::warn;
use ruzu_core::core_timing::{create_event, EventType, UnscheduleEventType};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use super::AudioBuffer;

pub type SharedAudioEvent = Arc<AtomicBool>;
const INCREMENT_TIME: Duration = Duration::from_millis(5);

pub struct DeviceSession {
    system: SharedSystem,
    sink: Option<SinkHandle>,
    stream: Option<SinkStreamHandle>,
    name: String,
    stream_type: StreamType,
    sample_format: SampleFormat,
    channel_count: u16,
    session_id: usize,
    applet_resource_user_id: u64,
    guest_memory: Option<SharedGuestMemory>,
    played_sample_count: Arc<AtomicU64>,
    thread_event: Option<Arc<parking_lot::Mutex<EventType>>>,
    audio_manager: Option<Weak<AudioManager>>,
    audio_event_type: Option<AudioEventType>,
    initialized: AtomicBool,
    started: bool,
}

impl DeviceSession {
    pub fn new(system: SharedSystem) -> Self {
        Self {
            system,
            sink: None,
            stream: None,
            name: String::new(),
            stream_type: StreamType::Out,
            sample_format: SampleFormat::PcmInt16,
            channel_count: 2,
            session_id: 0,
            applet_resource_user_id: 0,
            guest_memory: None,
            played_sample_count: Arc::new(AtomicU64::new(0)),
            thread_event: None,
            audio_manager: None,
            audio_event_type: None,
            initialized: AtomicBool::new(false),
            started: false,
        }
    }

    pub fn initialize(
        &mut self,
        sink: SinkHandle,
        name: &str,
        sample_format: SampleFormat,
        channel_count: u16,
        session_id: usize,
        applet_resource_user_id: u64,
        stream_type: StreamType,
        guest_memory: Option<SharedGuestMemory>,
    ) {
        if self.initialized.load(Ordering::SeqCst) {
            self.finalize();
        }

        self.name = format!("{}-{}", name, session_id);
        self.stream_type = stream_type;
        self.sample_format = sample_format;
        self.channel_count = channel_count;
        self.session_id = session_id;
        self.applet_resource_user_id = applet_resource_user_id;
        self.guest_memory = guest_memory;
        let stream = sink.lock().acquire_sink_stream(
            self.system.clone(),
            channel_count as u32,
            &self.name,
            stream_type,
        );
        self.sink = Some(sink);
        self.stream = Some(stream);
        self.started = false;
        self.initialized.store(true, Ordering::SeqCst);
    }

    pub fn set_guest_memory(&mut self, guest_memory: Option<SharedGuestMemory>) {
        self.guest_memory = guest_memory;
    }

    pub fn set_audio_manager(
        &mut self,
        audio_manager: Option<Weak<AudioManager>>,
        event_type: Option<AudioEventType>,
    ) {
        self.audio_manager = audio_manager;
        self.audio_event_type = event_type;
    }

    pub fn finalize(&mut self) {
        if self.initialized.load(Ordering::SeqCst) {
            self.stop();
            if let (Some(sink), Some(stream)) = (&self.sink, &self.stream) {
                sink.lock().close_stream(stream);
            }
            self.stream = None;
            self.sink = None;
            self.guest_memory = None;
            self.thread_event = None;
            self.audio_manager = None;
            self.audio_event_type = None;
            self.started = false;
            self.initialized.store(false, Ordering::SeqCst);
        }
    }

    pub fn start(&mut self) {
        if self.started {
            return;
        }
        if let Some(stream) = &self.stream {
            start_sink_stream(stream, false);
            if self.thread_event.is_none() {
                self.thread_event = Some(self.make_thread_event(stream.clone()));
            }
            if let Some(thread_event) = &self.thread_event {
                self.system.get().core_timing().schedule_looping_event(
                    Duration::ZERO,
                    INCREMENT_TIME,
                    thread_event,
                    false,
                );
            }
            self.started = true;
        }
    }

    pub fn stop(&mut self) {
        if !self.started {
            return;
        }
        if let Some(stream) = &self.stream {
            stop_sink_stream(stream);
            if let Some(thread_event) = &self.thread_event {
                self.system
                    .get()
                    .core_timing()
                    .unschedule_event(thread_event, UnscheduleEventType::NoWait);
            }
        }
        self.started = false;
    }

    pub fn clear_buffers(&self) {
        if let Some(stream) = &self.stream {
            stream.lock().clear_queue();
        }
    }

    pub fn append_buffers(&self, buffers: &[AudioBuffer]) {
        let Some(stream) = &self.stream else {
            return;
        };
        let mut stream = stream.lock();
        for buffer in buffers {
            let frames =
                buffer.size / (self.channel_count as u64 * std::mem::size_of::<i16>() as u64);
            let sink_buffer = crate::sink::SinkBuffer {
                frames,
                frames_played: 0,
                tag: buffer.tag,
                consumed: false,
            };
            let sample_count = buffer.size as usize / std::mem::size_of::<i16>();
            let samples = if self.stream_type == StreamType::In {
                vec![0i16; sample_count]
            } else {
                self.read_guest_samples(buffer.samples, sample_count)
                    .unwrap_or_else(|| vec![0i16; sample_count])
            };
            if std::env::var("RUZU_TRACE_AUDIO_STREAM")
                .is_ok_and(|filter| self.name.contains(&filter))
            {
                use std::sync::atomic::AtomicU64;
                static TRACE_COUNT: AtomicU64 = AtomicU64::new(0);
                let trace_index = TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
                if trace_index < 512 || trace_index.is_power_of_two() {
                    let mut min_sample = i16::MAX;
                    let mut max_sample = i16::MIN;
                    let mut peak = 0u16;
                    let mut nonzero = 0usize;
                    for &sample in &samples {
                        min_sample = min_sample.min(sample);
                        max_sample = max_sample.max(sample);
                        peak = peak.max(sample.unsigned_abs());
                        nonzero += usize::from(sample != 0);
                    }
                    eprintln!(
                        "[AUDIO_STREAM_SAMPLES] #{} name={} tag=0x{:X} frames={} samples={} min={} max={} peak={} nonzero={}",
                        trace_index,
                        self.name,
                        buffer.tag,
                        frames,
                        samples.len(),
                        min_sample,
                        max_sample,
                        peak,
                        nonzero
                    );
                }
            }
            stream.append_buffer(sink_buffer, &samples);
        }
    }

    pub fn release_buffer(&self, buffer: AudioBuffer) {
        let Some(stream) = &self.stream else {
            return;
        };
        if self.stream_type != StreamType::In {
            return;
        }
        let num_samples = buffer.size / std::mem::size_of::<i16>() as u64;
        let samples = stream.lock().release_buffer(num_samples);
        if !self.write_guest_samples(buffer.samples, &samples) {
            warn!(
                "audio_core: failed to write {} input samples back to guest at {:#x}",
                samples.len(),
                buffer.samples
            );
        }
    }

    pub fn is_buffer_consumed(&self, buffer: AudioBuffer) -> bool {
        self.played_sample_count.load(Ordering::SeqCst) >= buffer.end_timestamp
    }

    pub fn set_volume(&self, volume: f32) {
        if let Some(stream) = &self.stream {
            stream.lock().set_system_volume(volume);
        }
    }

    pub fn get_played_sample_count(&self) -> u64 {
        self.played_sample_count.load(Ordering::SeqCst)
    }

    pub fn set_ring_size(&self, ring_size: u32) {
        if let Some(stream) = &self.stream {
            stream.lock().set_ring_size(ring_size);
        }
    }

    pub fn session_id(&self) -> usize {
        self.session_id
    }

    fn read_guest_samples(&self, address: u64, sample_count: usize) -> Option<Vec<i16>> {
        let guest_memory = self.guest_memory.as_ref()?;
        let samples = guest_memory.read_i16_samples(address, sample_count)?;
        if samples.len() != sample_count {
            warn!(
                "audio_core: guest read at {:#x} returned {} samples, expected {}",
                address,
                samples.len(),
                sample_count
            );
            return None;
        }
        Some(samples)
    }

    fn write_guest_samples(&self, address: u64, samples: &[i16]) -> bool {
        let Some(guest_memory) = &self.guest_memory else {
            return false;
        };
        guest_memory.write_i16_samples(address, samples)
    }

    fn make_thread_event(&self, stream: SinkStreamHandle) -> Arc<parking_lot::Mutex<EventType>> {
        let played_sample_count = self.played_sample_count.clone();
        let audio_manager = self.audio_manager.clone();
        let audio_event_type = self.audio_event_type;
        create_event(
            format!("AudioDeviceSampleTick-{}", self.session_id),
            Box::new(move |_, _| {
                let played = stream.lock().get_expected_played_sample_count();
                played_sample_count.store(played, Ordering::SeqCst);
                if let (Some(audio_manager), Some(event_type)) = (&audio_manager, audio_event_type)
                {
                    let Some(audio_manager) = audio_manager.upgrade() else {
                        return None;
                    };
                    audio_manager.set_event(event_type, true);
                }
                None
            }),
        )
    }
}

impl Drop for DeviceSession {
    fn drop(&mut self) {
        self.finalize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::sink::new_sink_handle;
    use crate::sink::NullSink;
    use crate::AudioManager;
    use parking_lot::Mutex;
    use ruzu_core::memory::memory_manager::{MemoryManager, MemoryPermission, MemoryState};

    fn make_system() -> SharedSystem {
        crate::make_test_system()
    }

    fn make_guest_memory() -> Arc<Mutex<MemoryManager>> {
        let mut memory = MemoryManager::with_capacity(0x10000, 0x100).unwrap();
        memory
            .map(
                0x1000,
                0x1000,
                MemoryPermission::READ_WRITE,
                MemoryState::Normal,
            )
            .unwrap();
        Arc::new(Mutex::new(memory))
    }

    #[test]
    fn append_buffers_reads_output_samples_from_guest_memory() {
        let system = make_system();
        let sink = new_sink_handle(Box::new(NullSink::new_recording_for_test("null")));
        let guest_memory = make_guest_memory();
        guest_memory
            .lock()
            .write_bytes(0x1000, &[1, 0, 2, 0, 3, 0, 4, 0])
            .unwrap();

        let mut session = DeviceSession::new(system.clone());
        session.initialize(
            sink.clone(),
            "DeviceOut",
            SampleFormat::PcmInt16,
            2,
            0,
            0,
            StreamType::Out,
            Some(Arc::new(crate::device::KernelMemoryProvider::new(
                guest_memory.clone(),
            ))),
        );

        session.append_buffers(&[AudioBuffer {
            start_timestamp: 0,
            end_timestamp: 2,
            played_timestamp: 0,
            samples: 0x1000,
            tag: 1,
            size: 8,
        }]);

        let stream = sink
            .lock()
            .acquire_sink_stream(system, 2, "DeviceOut-0", StreamType::Out);
        let samples = stream.lock().release_buffer(4);
        assert_eq!(samples, vec![1, 2, 3, 4]);
    }

    #[test]
    fn release_buffer_writes_input_samples_back_to_guest_memory() {
        let system = make_system();
        let sink = new_sink_handle(Box::new(NullSink::new_recording_for_test("null")));
        let guest_memory = make_guest_memory();

        let mut session = DeviceSession::new(system.clone());
        session.initialize(
            sink.clone(),
            "BuiltInHeadset",
            SampleFormat::PcmInt16,
            2,
            1,
            0,
            StreamType::In,
            Some(Arc::new(crate::device::KernelMemoryProvider::new(
                guest_memory.clone(),
            ))),
        );

        let stream = sink
            .lock()
            .acquire_sink_stream(system, 2, "BuiltInHeadset-1", StreamType::In);
        stream.lock().process_audio_in(&[5, 6, 7, 8], 2);
        session.release_buffer(AudioBuffer {
            start_timestamp: 0,
            end_timestamp: 2,
            played_timestamp: 0,
            samples: 0x1000,
            tag: 2,
            size: 8,
        });

        let bytes = guest_memory.lock().read_bytes(0x1000, 8).unwrap();
        assert_eq!(bytes, vec![40, 0, 48, 0, 56, 0, 64, 0]);
    }

    #[test]
    fn start_schedules_timing_event_for_played_sample_updates() {
        let system = make_system();
        let sink = new_sink_handle(Box::new(NullSink::new("null")));

        let mut session = DeviceSession::new(system.clone());
        session.initialize(
            sink.clone(),
            "DeviceOut",
            SampleFormat::PcmInt16,
            2,
            2,
            0,
            StreamType::Out,
            None,
        );

        session.append_buffers(&[AudioBuffer {
            start_timestamp: 0,
            end_timestamp: 2,
            played_timestamp: 0,
            samples: 0,
            tag: 3,
            size: 8,
        }]);

        let stream =
            sink.lock()
                .acquire_sink_stream(system.clone(), 2, "DeviceOut-2", StreamType::Out);
        let mut rendered = [0i16; 4];
        stream.lock().process_audio_out_and_render(&mut rendered, 2);

        session.start();
        let _ = system.get().core_timing().advance();

        assert!(session.get_played_sample_count() >= 2);
    }

    #[test]
    fn timing_event_signals_audio_manager() {
        let system = make_system();
        let sink = new_sink_handle(Box::new(NullSink::new("null")));
        let audio_manager = Arc::new(AudioManager::new());
        let callback_hit = Arc::new(AtomicBool::new(false));
        let callback_flag = callback_hit.clone();
        assert!(audio_manager
            .set_out_manager(Arc::new(move || {
                callback_flag.store(true, Ordering::SeqCst);
            }))
            .is_success());

        let mut session = DeviceSession::new(system.clone());
        session.set_audio_manager(
            Some(Arc::downgrade(&audio_manager)),
            Some(AudioEventType::AudioOutManager),
        );
        session.initialize(
            sink.clone(),
            "DeviceOut",
            SampleFormat::PcmInt16,
            2,
            3,
            0,
            StreamType::Out,
            None,
        );
        session.append_buffers(&[AudioBuffer {
            start_timestamp: 0,
            end_timestamp: 2,
            played_timestamp: 0,
            samples: 0,
            tag: 4,
            size: 8,
        }]);

        let stream =
            sink.lock()
                .acquire_sink_stream(system.clone(), 2, "DeviceOut-3", StreamType::Out);
        let mut rendered = [0i16; 4];
        stream.lock().process_audio_out_and_render(&mut rendered, 2);

        let event = session.make_thread_event(stream.clone());
        {
            let callback = event.lock();
            let _ = (callback.callback)(0, Duration::ZERO);
        }
        for _ in 0..10 {
            audio_manager.dispatch_events_for_test(false);
            if callback_hit.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(callback_hit.load(Ordering::SeqCst));
    }

    #[test]
    fn start_and_stop_are_idempotent() {
        let system = make_system();
        let sink = new_sink_handle(Box::new(NullSink::new("null")));

        let mut session = DeviceSession::new(system);
        session.initialize(
            sink,
            "DeviceOut",
            SampleFormat::PcmInt16,
            2,
            4,
            0,
            StreamType::Out,
            None,
        );

        session.start();
        assert!(session.started);
        let first_thread_event = session.thread_event.as_ref().map(Arc::as_ptr);

        session.start();
        assert!(session.started);
        assert_eq!(
            first_thread_event,
            session.thread_event.as_ref().map(Arc::as_ptr)
        );

        session.stop();
        assert!(!session.started);

        session.stop();
        assert!(!session.started);
    }
}

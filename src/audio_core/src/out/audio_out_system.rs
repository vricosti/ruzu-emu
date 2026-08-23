use crate::audio_event::Type as AudioEventType;
use crate::audio_manager::AudioManager;
use crate::common::common::{SampleFormat, SessionTypes, BUFFER_COUNT, TARGET_SAMPLE_RATE};
use crate::device::{
    AudioBuffer, AudioBuffers, DeviceSession, SharedAudioEvent, SharedGuestMemory,
};
use crate::errors::{
    RESULT_INVALID_CHANNEL_COUNT, RESULT_INVALID_SAMPLE_RATE, RESULT_NOT_FOUND,
    RESULT_OPERATION_FAILED,
};
use crate::sink::{SinkHandle, StreamType};
use crate::{Result, SharedSystem};
use common::ResultCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

pub const SESSION_TYPE: SessionTypes = SessionTypes::AudioOut;

#[derive(Debug, Clone, Copy, Default)]
pub struct AudioOutParameter {
    pub sample_rate: i32,
    pub channel_count: u16,
    pub reserved: u16,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AudioOutParameterInternal {
    pub sample_rate: u32,
    pub channel_count: u32,
    pub sample_format: u32,
    pub state: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AudioOutBuffer {
    pub next: u64,
    pub samples: u64,
    pub capacity: u64,
    pub size: u64,
    pub offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Started,
    Stopped,
}

pub struct System {
    system: SharedSystem,
    sink: SinkHandle,
    pub buffer_event: SharedAudioEvent,
    session_id: usize,
    session: DeviceSession,
    buffers: AudioBuffers<{ BUFFER_COUNT as usize }>,
    sample_rate: u32,
    sample_format: SampleFormat,
    channel_count: u16,
    state: AtomicBool,
    name: String,
    volume: f32,
    applet_resource_user_id: u64,
    guest_memory: Option<SharedGuestMemory>,
    audio_manager: Option<Weak<AudioManager>>,
}

impl System {
    pub fn new(
        system: SharedSystem,
        sink: SinkHandle,
        buffer_event: SharedAudioEvent,
        session_id: usize,
    ) -> Self {
        Self {
            session: DeviceSession::new(system.clone()),
            system,
            sink,
            buffer_event,
            session_id,
            buffers: AudioBuffers::new(BUFFER_COUNT as usize),
            sample_rate: 0,
            sample_format: SampleFormat::PcmInt16,
            channel_count: 2,
            state: AtomicBool::new(false),
            name: String::new(),
            volume: 1.0,
            applet_resource_user_id: 0,
            guest_memory: None,
            audio_manager: None,
        }
    }

    pub fn get_default_output_device_name(&self) -> &'static str {
        "DeviceOut"
    }

    pub fn is_config_valid(&self, device_name: &str, in_params: &AudioOutParameter) -> Result {
        if !device_name.is_empty() && device_name != self.get_default_output_device_name() {
            return RESULT_NOT_FOUND;
        }
        if in_params.sample_rate != TARGET_SAMPLE_RATE as i32 && in_params.sample_rate > 0 {
            return RESULT_INVALID_SAMPLE_RATE;
        }
        if in_params.channel_count == 0
            || in_params.channel_count == 2
            || in_params.channel_count == 6
        {
            return ResultCode::SUCCESS;
        }
        RESULT_INVALID_CHANNEL_COUNT
    }

    pub fn initialize(
        &mut self,
        device_name: String,
        in_params: &AudioOutParameter,
        applet_resource_user_id: u64,
    ) -> Result {
        let result = self.is_config_valid(&device_name, in_params);
        if result.is_error() {
            return result;
        }
        self.name = if device_name.is_empty() {
            self.get_default_output_device_name().to_string()
        } else {
            device_name
        };
        self.sample_rate = TARGET_SAMPLE_RATE;
        self.sample_format = SampleFormat::PcmInt16;
        self.channel_count = if in_params.channel_count <= 2 { 2 } else { 6 };
        self.volume = 1.0;
        self.applet_resource_user_id = applet_resource_user_id;
        ResultCode::SUCCESS
    }

    pub fn start_session(&mut self) {
        self.session.start();
    }
    pub fn get_session_id(&self) -> usize {
        self.session_id
    }

    pub fn start(&mut self) -> Result {
        if self.state.load(Ordering::SeqCst) {
            return RESULT_OPERATION_FAILED;
        }
        self.session.initialize(
            self.sink.clone(),
            &self.name,
            self.sample_format,
            self.channel_count,
            self.session_id,
            self.applet_resource_user_id,
            StreamType::Out,
            self.guest_memory.clone(),
        );
        self.session.set_audio_manager(
            self.audio_manager.clone(),
            self.audio_manager
                .as_ref()
                .map(|_| AudioEventType::AudioOutManager),
        );
        self.session.set_volume(self.volume);
        self.session.start();
        self.state.store(true, Ordering::SeqCst);
        let mut buffers_to_register = Vec::new();
        self.buffers.register_buffers(&mut buffers_to_register);
        self.session.append_buffers(&buffers_to_register);
        self.session.set_ring_size(buffers_to_register.len() as u32);
        ResultCode::SUCCESS
    }

    pub fn stop(&mut self) -> Result {
        if self.state.swap(false, Ordering::SeqCst) {
            self.session.stop();
            self.session.set_volume(0.0);
            self.session.clear_buffers();
            let current_time = self
                .system
                .get()
                .core_timing()
                .get_global_time_ns()
                .as_nanos() as u64;
            if self
                .buffers
                .release_buffers(current_time, &self.session, true)
            {
                self.buffer_event.store(true, Ordering::SeqCst);
            }
        }
        ResultCode::SUCCESS
    }

    pub fn finalize(&mut self) {
        let _ = self.stop();
        self.session.finalize();
    }

    pub fn append_buffer(&self, buffer: AudioOutBuffer, tag: u64) -> bool {
        trace_audio_out_buffer(
            1,
            self.session_id,
            self.get_state(),
            tag,
            buffer.size,
            buffer.samples,
            self.buffers.get_total_buffer_count(),
            0,
            self.session.get_played_sample_count(),
            0,
        );
        if self.buffers.get_total_buffer_count() == BUFFER_COUNT {
            trace_audio_out_buffer(
                2,
                self.session_id,
                self.get_state(),
                tag,
                buffer.size,
                buffer.samples,
                self.buffers.get_total_buffer_count(),
                0,
                self.session.get_played_sample_count(),
                0,
            );
            return false;
        }
        let timestamp = self.buffers.get_next_timestamp();
        self.buffers.append_buffer(AudioBuffer {
            start_timestamp: timestamp,
            end_timestamp: timestamp
                + buffer.size / (self.channel_count as u64 * std::mem::size_of::<i16>() as u64),
            played_timestamp: 0,
            samples: buffer.samples,
            tag,
            size: buffer.size,
        });
        self.register_buffers();
        trace_audio_out_buffer(
            2,
            self.session_id,
            self.get_state(),
            tag,
            buffer.size,
            buffer.samples,
            self.buffers.get_total_buffer_count(),
            0,
            self.session.get_played_sample_count(),
            1,
        );
        true
    }

    pub fn register_buffers(&self) {
        if self.get_state() == State::Started {
            let mut registered_buffers = Vec::new();
            self.buffers.register_buffers(&mut registered_buffers);
            trace_audio_out_buffer(
                3,
                self.session_id,
                self.get_state(),
                0,
                0,
                0,
                registered_buffers.len() as u32,
                0,
                self.session.get_played_sample_count(),
                0,
            );
            self.session.append_buffers(&registered_buffers);
        }
    }

    pub fn release_buffers(&self) {
        let current_time = self
            .system
            .get()
            .core_timing()
            .get_global_time_ns()
            .as_nanos() as u64;
        let signal = self
            .buffers
            .release_buffers(current_time, &self.session, false);
        trace_audio_out_buffer(
            4,
            self.session_id,
            self.get_state(),
            0,
            0,
            0,
            self.buffers.get_total_buffer_count(),
            u64::from(signal) as u32,
            self.session.get_played_sample_count(),
            0,
        );
        if signal {
            self.buffer_event.store(true, Ordering::SeqCst);
        }
    }

    pub fn get_released_buffers(&self, tags: &mut [u64]) -> u32 {
        let released = self.buffers.get_released_buffers(tags);
        trace_audio_out_buffer(
            5,
            self.session_id,
            self.get_state(),
            tags.first().copied().unwrap_or(0),
            0,
            0,
            self.buffers.get_total_buffer_count(),
            released,
            self.session.get_played_sample_count(),
            0,
        );
        released
    }

    pub fn flush_audio_out_buffers(&self) -> bool {
        if self.get_state() != State::Started {
            return false;
        }
        let mut buffers_released = 0;
        self.buffers.flush_buffers(&mut buffers_released);
        if buffers_released > 0 {
            self.buffer_event.store(true, Ordering::SeqCst);
        }
        true
    }

    pub fn get_channel_count(&self) -> u16 {
        self.channel_count
    }
    pub fn get_sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn get_sample_format(&self) -> SampleFormat {
        self.sample_format
    }
    pub fn get_state(&self) -> State {
        if self.state.load(Ordering::SeqCst) {
            State::Started
        } else {
            State::Stopped
        }
    }
    pub fn get_name(&self) -> String {
        self.name.clone()
    }
    pub fn get_volume(&self) -> f32 {
        self.volume
    }
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume;
        self.session.set_volume(volume);
    }

    pub fn set_guest_memory(&mut self, guest_memory: Option<SharedGuestMemory>) {
        self.guest_memory = guest_memory.clone();
        self.session.set_guest_memory(guest_memory);
    }

    pub fn set_audio_manager(&mut self, audio_manager: Option<Arc<AudioManager>>) {
        self.audio_manager = audio_manager.as_ref().map(Arc::downgrade);
        self.session.set_audio_manager(
            self.audio_manager.clone(),
            self.audio_manager
                .as_ref()
                .map(|_| AudioEventType::AudioOutManager),
        );
    }
    pub fn contains_audio_buffer(&self, tag: u64) -> bool {
        self.buffers.contains_buffer(tag)
    }
    pub fn get_buffer_count(&self) -> u32 {
        self.buffers.get_appended_registered_count()
    }
    pub fn get_played_sample_count(&self) -> u64 {
        self.session.get_played_sample_count()
    }
}

fn trace_audio_out_buffer(
    stage: u64,
    session_id: usize,
    state: State,
    tag: u64,
    size: u64,
    samples: u64,
    total_or_registered: u32,
    released_or_signal: u32,
    queue_or_played: u64,
    result: u64,
) {
    if common::trace::is_enabled(common::trace::cat::AUDIO_OUT_BUFFER) {
        common::trace::emit_raw(
            common::trace::cat::AUDIO_OUT_BUFFER,
            &[
                stage,
                session_id as u64,
                match state {
                    State::Started => 1,
                    State::Stopped => 0,
                },
                tag,
                size,
                samples,
                total_or_registered as u64,
                released_or_signal as u64,
                queue_or_played,
                result,
            ],
        );
    }
}

impl Drop for System {
    fn drop(&mut self) {
        self.finalize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::sink::new_sink_handle;
    use crate::sink::NullSink;
    fn make_system() -> SharedSystem {
        crate::make_test_system()
    }

    #[test]
    fn flush_audio_out_buffers_returns_true_when_started_without_releasing_buffers() {
        let system = System::new(
            make_system(),
            new_sink_handle(Box::new(NullSink::new("null"))),
            Arc::new(AtomicBool::new(false)),
            0,
        );
        system.state.store(true, Ordering::SeqCst);

        assert!(system.flush_audio_out_buffers());
        assert!(!system.buffer_event.load(Ordering::SeqCst));
    }
}

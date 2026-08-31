use crate::sink::sink::{new_stream_handle, Sink};
use crate::sink::sink_stream::{
    BaseSinkStream, SharedSinkStreamBase, SinkBuffer, SinkStream, SinkStreamBase, SinkStreamHandle,
    StreamType,
};
use crate::SharedSystem;
use parking_lot::Mutex;
use std::sync::Arc;

struct NullSinkStreamImpl {
    base: SharedSinkStreamBase,
}

impl NullSinkStreamImpl {
    fn new(system: SharedSystem, stream_type: StreamType) -> Self {
        Self {
            base: Arc::new(Mutex::new(SinkStreamBase::new(
                system,
                stream_type,
                2,
                2,
                String::new(),
            ))),
        }
    }
}

impl SinkStream for NullSinkStreamImpl {
    fn base(&self) -> &SharedSinkStreamBase {
        &self.base
    }

    fn append_buffer(&self, _buffer: SinkBuffer, _samples: &[i16]) {}

    fn release_buffer(&self, _num_samples: u64) -> Vec<i16> {
        Vec::new()
    }
}

#[derive(Default)]
pub struct NullSink {
    null_sink: Option<SinkStreamHandle>,
    recording_for_test: bool,
}

impl NullSink {
    pub fn new(_device_id: &str) -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn new_recording_for_test(_device_id: &str) -> Self {
        Self {
            null_sink: None,
            recording_for_test: true,
        }
    }
}

impl Sink for NullSink {
    fn close_stream(&mut self, _stream: &SinkStreamHandle) {}

    fn close_streams(&mut self) {}

    fn acquire_sink_stream(
        &mut self,
        system: SharedSystem,
        system_channels: u32,
        name: &str,
        stream_type: StreamType,
    ) -> SinkStreamHandle {
        if let Some(stream) = &self.null_sink {
            return stream.clone();
        }

        let stream = if self.recording_for_test {
            new_stream_handle(BaseSinkStream::new(
                system,
                stream_type,
                system_channels,
                2,
                name.to_string(),
            ))
        } else {
            new_stream_handle(NullSinkStreamImpl::new(system, stream_type))
        };
        self.null_sink = Some(stream.clone());
        stream
    }

    fn get_device_volume(&self) -> f32 {
        1.0
    }

    fn set_device_volume(&mut self, _volume: f32) {}

    fn set_system_volume(&mut self, _volume: f32) {}

    fn get_device_channels(&self) -> u32 {
        2
    }

    fn get_system_channels(&self) -> u32 {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_system() -> SharedSystem {
        crate::make_test_system()
    }

    #[test]
    fn null_sink_append_buffer_is_noop_like_upstream() {
        let mut sink = NullSink::new("null");
        let stream = sink.acquire_sink_stream(make_system(), 2, "null", StreamType::Render);

        stream.append_buffer(
            SinkBuffer {
                frames: 240,
                frames_played: 0,
                tag: 1,
                consumed: false,
            },
            &[0; 480],
        );

        assert_eq!(stream.get_queue_size(), 0);
    }

    #[test]
    fn null_sink_release_buffer_is_empty_like_upstream() {
        let mut sink = NullSink::new("null");
        let stream = sink.acquire_sink_stream(make_system(), 2, "null", StreamType::In);

        assert!(stream.release_buffer(240).is_empty());
    }
}

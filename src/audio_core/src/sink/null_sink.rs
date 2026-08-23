use crate::sink::sink::{new_stream_handle, Sink};
use crate::sink::sink_stream::{SinkStream, SinkStreamHandle, StreamType};
use crate::SharedSystem;
use std::sync::Arc;

struct StreamEntry {
    name: String,
    stream_type: StreamType,
    handle: SinkStreamHandle,
}

#[derive(Default)]
pub struct NullSink {
    streams: Vec<StreamEntry>,
    device_volume: f32,
    system_volume: f32,
    device_channels: u32,
    system_channels: u32,
    discard_stream_buffers: bool,
}

impl NullSink {
    pub fn new(_device_id: &str) -> Self {
        Self {
            streams: Vec::new(),
            device_volume: 1.0,
            system_volume: 1.0,
            device_channels: 2,
            system_channels: 2,
            discard_stream_buffers: true,
        }
    }

    #[cfg(test)]
    pub fn new_recording_for_test(device_id: &str) -> Self {
        let mut sink = Self::new(device_id);
        sink.discard_stream_buffers = false;
        sink
    }
}

impl Sink for NullSink {
    fn close_stream(&mut self, stream: &SinkStreamHandle) {
        self.streams
            .retain(|entry| !Arc::ptr_eq(&entry.handle, stream));
    }

    fn close_streams(&mut self) {
        self.streams.clear();
    }

    fn acquire_sink_stream(
        &mut self,
        system: SharedSystem,
        system_channels: u32,
        name: &str,
        stream_type: StreamType,
    ) -> SinkStreamHandle {
        if let Some(existing) = self
            .streams
            .iter()
            .find(|entry| entry.name == name && entry.stream_type == stream_type)
        {
            return existing.handle.clone();
        }

        let mut stream = SinkStream::new(system, stream_type);
        stream.system_channels = system_channels;
        stream.device_channels = self.device_channels;
        stream.name = name.to_string();
        stream.set_device_volume(self.device_volume);
        stream.set_system_volume(self.system_volume);
        stream.set_discard_buffers(self.discard_stream_buffers);
        if stream_type == StreamType::Render {
            stream.set_realtime_pacing(true);
        }
        let handle = new_stream_handle(stream);
        self.streams.push(StreamEntry {
            name: name.to_string(),
            stream_type,
            handle: handle.clone(),
        });
        handle
    }

    fn get_device_volume(&self) -> f32 {
        self.device_volume
    }

    fn set_device_volume(&mut self, volume: f32) {
        self.device_volume = volume;
    }

    fn set_system_volume(&mut self, volume: f32) {
        self.system_volume = volume;
    }

    fn get_device_channels(&self) -> u32 {
        self.device_channels
    }

    fn get_system_channels(&self) -> u32 {
        self.system_channels
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::sink_stream::SinkBuffer;
    fn make_system() -> SharedSystem {
        crate::make_test_system()
    }

    #[test]
    fn null_sink_append_buffer_is_noop_like_upstream() {
        let mut sink = NullSink::new("null");
        let stream = sink.acquire_sink_stream(make_system(), 2, "null", StreamType::Render);

        stream.lock().append_buffer(
            SinkBuffer {
                frames: 240,
                frames_played: 0,
                tag: 1,
                consumed: false,
            },
            &[0; 480],
        );

        assert_eq!(stream.lock().get_queue_size(), 0);
    }

    #[test]
    fn null_sink_release_buffer_is_empty_like_upstream() {
        let mut sink = NullSink::new("null");
        let stream = sink.acquire_sink_stream(make_system(), 2, "null", StreamType::In);

        assert!(stream.lock().release_buffer(240).is_empty());
    }
}

// Oboe sink stub — Android-only audio backend.
//
// The upstream C++ code uses the Oboe library (https://github.com/google/oboe),
// which is an Android-specific C++ audio API. There is no mature Rust crate
// equivalent at this time. This file preserves the structural parity with
// upstream oboe_sink.cpp/oboe_sink.h but provides only a stub implementation
// until an Android build target is needed.
//
// When targeting Android, this should be implemented using either:
// - oboe-rs bindings (if available)
// - raw AAudio/OpenSLES FFI calls
// - or a C++ interop layer

use crate::sink::sink::{new_stream_handle, Sink};
use crate::sink::sink_stream::{BaseSinkStream, SinkStreamHandle, StreamType};
use crate::SharedSystem;
use log::warn;

pub struct OboeSink {
    device_channels: u32,
    system_channels: u32,
    streams: Vec<SinkStreamHandle>,
}

impl OboeSink {
    pub fn new() -> Self {
        warn!("OboeSink is a stub — Oboe is only available on Android");
        Self {
            device_channels: 2,
            system_channels: 2,
            streams: Vec::new(),
        }
    }
}

impl Sink for OboeSink {
    fn acquire_sink_stream(
        &mut self,
        system: SharedSystem,
        system_channels: u32,
        name: &str,
        stream_type: StreamType,
    ) -> SinkStreamHandle {
        self.system_channels = system_channels;
        let handle = new_stream_handle(BaseSinkStream::new(
            system,
            stream_type,
            system_channels,
            self.device_channels,
            name.to_string(),
        ));
        self.streams.push(handle.clone());
        handle
    }

    fn close_stream(&mut self, stream: &SinkStreamHandle) {
        self.streams
            .retain(|entry| !std::sync::Arc::ptr_eq(entry, stream));
    }

    fn close_streams(&mut self) {
        self.streams.clear();
    }

    fn get_device_volume(&self) -> f32 {
        if let Some(entry) = self.streams.first() {
            entry.get_device_volume()
        } else {
            1.0
        }
    }

    fn set_device_volume(&mut self, volume: f32) {
        for entry in &self.streams {
            entry.set_device_volume(volume);
        }
    }

    fn set_system_volume(&mut self, volume: f32) {
        for entry in &self.streams {
            entry.set_system_volume(volume);
        }
    }

    fn get_device_channels(&self) -> u32 {
        self.device_channels
    }

    fn get_system_channels(&self) -> u32 {
        self.system_channels
    }
}

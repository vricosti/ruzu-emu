use crate::sink::sink_stream::{SinkStream, SinkStreamHandle, StreamType};
use crate::SharedSystem;
use parking_lot::Mutex;
use std::sync::Arc;

pub const AUTO_DEVICE_NAME: &str = "auto";

pub trait Sink: Send + Sync {
    fn close_stream(&mut self, stream: &SinkStreamHandle);
    fn close_streams(&mut self);
    fn acquire_sink_stream(
        &mut self,
        system: SharedSystem,
        system_channels: u32,
        name: &str,
        stream_type: StreamType,
    ) -> SinkStreamHandle;
    fn get_device_volume(&self) -> f32;
    fn set_device_volume(&mut self, volume: f32);
    fn set_system_volume(&mut self, volume: f32);
    fn get_device_channels(&self) -> u32;
    fn get_system_channels(&self) -> u32;
}

pub type SinkBox = Box<dyn Sink>;
pub type SinkHandle = Arc<Mutex<SinkBox>>;

pub fn new_sink_handle(sink: SinkBox) -> SinkHandle {
    Arc::new(Mutex::new(sink))
}

pub fn new_stream_handle<T>(stream: T) -> SinkStreamHandle
where
    T: SinkStream + 'static,
{
    Arc::new(stream)
}

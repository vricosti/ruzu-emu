use crate::common::common::{TARGET_SAMPLE_COUNT, TARGET_SAMPLE_RATE};
use crate::sink::sink::{new_stream_handle, Sink, AUTO_DEVICE_NAME};
use crate::sink::sink_stream::{
    SharedSinkStreamBase, SinkStream, SinkStreamBase, SinkStreamHandle, SinkStreamLifecycleState,
    StreamType,
};
use crate::SharedSystem;
use cubeb::{Context, DeviceState, DeviceType, SampleFormat, StreamParamsBuilder};
use log::{error, info, warn};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

static CUBEB_CALLBACK_TRACE_COUNT: AtomicU32 = AtomicU32::new(0);

#[cfg(windows)]
fn initialize_com_multithreaded() -> i32 {
    unsafe {
        winapi::um::combaseapi::CoInitializeEx(
            std::ptr::null_mut(),
            winapi::um::objbase::COINIT_MULTITHREADED,
        )
    }
}

#[cfg(windows)]
fn uninitialize_com() {
    unsafe {
        winapi::um::combaseapi::CoUninitialize();
    }
}

#[cfg(windows)]
struct StreamComApartment;

#[cfg(windows)]
impl Drop for StreamComApartment {
    fn drop(&mut self) {
        // Upstream CubebSinkStream balances its constructor's CoInitializeEx
        // unconditionally when the stream object is destroyed.
        uninitialize_com();
    }
}

fn should_trace_cubeb_callback() -> bool {
    std::env::var_os("RUZU_TRACE_CUBEB_CALLBACK").is_some()
}

struct CubebSinkStream {
    base: SharedSinkStreamBase,
    lifecycle: Mutex<CubebLifecycle>,
    #[cfg(windows)]
    _com_apartment: StreamComApartment,
}

struct CubebLifecycle {
    stream: Option<cubeb::Stream<i16>>,
    state: SinkStreamLifecycleState,
}

// cubeb streams are designed for cross-thread lifecycle control. The Rust
// wrapper carries raw pointers and therefore does not derive these traits.
unsafe impl Send for CubebSinkStream {}
unsafe impl Sync for CubebSinkStream {}

impl SinkStream for CubebSinkStream {
    fn base(&self) -> &SharedSinkStreamBase {
        &self.base
    }

    fn finalize(&self) {
        let mut lifecycle = self.lifecycle.lock();
        if lifecycle.state == SinkStreamLifecycleState::Finalized {
            return;
        }

        if lifecycle.state == SinkStreamLifecycleState::Running {
            lifecycle.state = SinkStreamLifecycleState::Stopping;
            if self.base.lock().signal_stop() {
                if let Some(stream) = lifecycle.stream.as_ref() {
                    if let Err(error) = stream.stop() {
                        log::error!("Error stopping cubeb stream: {:?}", error);
                    }
                }
            }
            lifecycle.state = SinkStreamLifecycleState::Stopped;
        }

        // Dropping the native stream unregisters its callbacks. The callback's
        // Arc<SinkStreamBase> remains alive until that teardown is complete.
        lifecycle.stream.take();
        lifecycle.state = SinkStreamLifecycleState::Finalized;
    }

    fn start(&self, _resume: bool) {
        let mut lifecycle = self.lifecycle.lock();
        if lifecycle.state != SinkStreamLifecycleState::Stopped || lifecycle.stream.is_none() {
            return;
        }
        if !self.base.lock().signal_start() {
            return;
        }

        lifecycle.state = SinkStreamLifecycleState::Starting;
        if let Some(stream) = lifecycle.stream.as_ref() {
            if let Err(error) = stream.start() {
                // Eden also leaves `paused` false when cubeb_stream_start fails.
                log::error!("Error starting cubeb stream: {:?}", error);
            }
        }
        lifecycle.state = SinkStreamLifecycleState::Running;
    }

    fn stop(&self) {
        let mut lifecycle = self.lifecycle.lock();
        if lifecycle.state != SinkStreamLifecycleState::Running || lifecycle.stream.is_none() {
            return;
        }

        lifecycle.state = SinkStreamLifecycleState::Stopping;
        if self.base.lock().signal_stop() {
            if let Some(stream) = lifecycle.stream.as_ref() {
                if let Err(error) = stream.stop() {
                    log::error!("Error stopping cubeb stream: {:?}", error);
                }
            }
        }
        lifecycle.state = SinkStreamLifecycleState::Stopped;
    }
}

impl CubebSinkStream {
    fn new(
        base: SharedSinkStreamBase,
        stream: Option<cubeb::Stream<i16>>,
        #[cfg(windows)] com_apartment: StreamComApartment,
    ) -> Self {
        Self {
            base,
            lifecycle: Mutex::new(CubebLifecycle {
                stream,
                state: SinkStreamLifecycleState::Stopped,
            }),
            #[cfg(windows)]
            _com_apartment: com_apartment,
        }
    }
}

impl Drop for CubebSinkStream {
    fn drop(&mut self) {
        self.finalize();
    }
}

pub struct CubebSink {
    ctx: Option<Context>,
    output_device: cubeb::DeviceId,
    input_device: cubeb::DeviceId,
    device_channels: u32,
    system_channels: u32,
    streams: Vec<SinkStreamHandle>,
    #[cfg(windows)]
    com_init_result: i32,
}

// Safety: cubeb::Context and cubeb::DeviceId contain raw pointers internally,
// but the cubeb library is designed to be used from multiple threads.
// The C++ upstream also shares these across threads freely.
unsafe impl Send for CubebSink {}
unsafe impl Sync for CubebSink {}

fn should_trace_cubeb_state() -> bool {
    std::env::var_os("RUZU_TRACE_CUBEB_STATE").is_some()
}

fn process_stream_callback(
    base: &SharedSinkStreamBase,
    stream_type: StreamType,
    device_channels: u32,
    input: &[i16],
    output: &mut [i16],
) -> isize {
    let num_channels = device_channels as usize;
    let frame_size = num_channels.max(1);
    let sample_count = if stream_type == StreamType::In {
        input.len()
    } else {
        output.len()
    };
    let num_frames = sample_count / frame_size;

    let mut stream = base.lock();
    let queue_before = stream.get_queue_size();
    if stream_type == StreamType::In {
        stream.process_audio_in(input, num_frames);
    } else {
        stream.process_audio_out_and_render(output, num_frames);
    }
    let queue_after = stream.get_queue_size();

    if std::env::var_os("RUZU_PROFILE_CUBEB_CB").is_some() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::OnceLock;
        use std::time::Instant;
        static CALLS: AtomicU64 = AtomicU64::new(0);
        static FRAMES: AtomicU64 = AtomicU64::new(0);
        static START: OnceLock<Instant> = OnceLock::new();
        let start = START.get_or_init(Instant::now);
        let n = CALLS.fetch_add(1, Ordering::Relaxed) + 1;
        let f = FRAMES.fetch_add(num_frames as u64, Ordering::Relaxed) + num_frames as u64;
        if n % 20 == 0 {
            let elapsed = start.elapsed().as_secs_f64();
            eprintln!(
                "[CUBEB_CB] type={:?} calls={} total_frames={} t={:.2}s callback_Hz={:.1} frames_Hz={:.0} queue_before={} queue_after={}",
                stream_type, n, f, elapsed,
                n as f64 / elapsed, f as f64 / elapsed,
                queue_before, queue_after
            );
        }
    }

    if should_trace_cubeb_callback() {
        let count = CUBEB_CALLBACK_TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
        if count < 32 {
            log::info!(
                "CubebSink callback type={:?} frames={} samples={} queue_before={} queue_after={} paused={}",
                stream_type,
                num_frames,
                output.len(),
                queue_before,
                queue_after,
                stream.is_paused()
            );
        }
    }

    num_frames as isize
}

impl CubebSink {
    pub fn new(target_device_name: &str) -> Self {
        #[cfg(windows)]
        let com_init_result = initialize_com_multithreaded();

        // RUZU_CUBEB_BACKEND env override is useful to bypass pulse-rust on
        // pipewire-pulse systems where it oscillates Drained↔Started.
        let backend_cstring = std::env::var("RUZU_CUBEB_BACKEND")
            .ok()
            .and_then(|s| std::ffi::CString::new(s).ok());
        let backend_name = backend_cstring.as_deref();
        let ctx = match Context::init(Some(c"ruzu"), backend_name) {
            Ok(ctx) => Some(ctx),
            Err(e) => {
                error!("cubeb_init failed (backend={:?}): {:?}", backend_name, e);
                None
            }
        };

        let mut output_device = cubeb::DeviceId::default();
        let input_device = cubeb::DeviceId::default();
        let mut device_channels = 2u32;

        if let Some(ref ctx) = ctx {
            info!("cubeb backend_id: {}", ctx.backend_id());

            // Query max channel count
            if let Ok(max_channels) = ctx.max_channel_count() {
                device_channels = if max_channels >= 6 { 6 } else { 2 };
            }

            // Find specific output device if requested
            if target_device_name != AUTO_DEVICE_NAME && !target_device_name.is_empty() {
                if let Ok(devices) = ctx.enumerate_devices(DeviceType::OUTPUT) {
                    for device in devices.iter() {
                        if let Some(friendly_name) = device.friendly_name() {
                            if friendly_name == target_device_name {
                                output_device = device.devid();
                                break;
                            }
                        }
                    }
                } else {
                    warn!("Audio output device enumeration not supported");
                }
            }
        }

        Self {
            ctx,
            output_device,
            input_device,
            device_channels,
            system_channels: 2,
            streams: Vec::new(),
            #[cfg(windows)]
            com_init_result,
        }
    }
}

impl Drop for CubebSink {
    fn drop(&mut self) {
        if self.ctx.is_none() {
            return;
        }

        self.streams.clear();
        self.ctx.take();

        #[cfg(windows)]
        if self.com_init_result >= 0 {
            uninitialize_com();
        }
    }
}

impl Sink for CubebSink {
    fn acquire_sink_stream(
        &mut self,
        system: SharedSystem,
        system_channels: u32,
        name: &str,
        stream_type: StreamType,
    ) -> SinkStreamHandle {
        self.system_channels = system_channels;
        #[cfg(windows)]
        let com_apartment = {
            let _ = initialize_com_multithreaded();
            StreamComApartment
        };

        let base = Arc::new(Mutex::new(SinkStreamBase::new(
            system,
            stream_type,
            system_channels,
            self.device_channels,
            name.to_string(),
        )));

        let Some(ref ctx) = self.ctx else {
            let handle = new_stream_handle(CubebSinkStream::new(
                base,
                None,
                #[cfg(windows)]
                com_apartment,
            ));
            self.streams.push(handle.clone());
            return handle;
        };

        // Build cubeb stream params
        let layout = match self.device_channels {
            1 => cubeb::ChannelLayout::MONO,
            6 => cubeb::ChannelLayout::_3F2_LFE,
            _ => cubeb::ChannelLayout::STEREO,
        };

        let params = StreamParamsBuilder::new()
            .rate(TARGET_SAMPLE_RATE)
            .channels(self.device_channels)
            .format(SampleFormat::S16LE)
            .layout(layout)
            .take();

        // RUZU_AUDIO_LATENCY_FRAMES=N — override the cubeb-reported minimum
        // latency at runtime. Useful for diagnosing the audio-event-rate
        // collapse on PipeWire-pulse hosts where cubeb's C pulse backend
        // reports 1200 frames (25 ms ≈ 40 Hz callback) which throttles the
        // audio renderer to ~65 Hz, ~3x below the ~200 Hz target. Default
        // behavior matches upstream (no clamp below what cubeb says).
        let env_latency = std::env::var("RUZU_AUDIO_LATENCY_FRAMES")
            .ok()
            .and_then(|s| s.parse::<u32>().ok());
        let minimum_latency = if let Some(v) = env_latency {
            v
        } else {
            match ctx.min_latency(&params) {
                Ok(latency) => latency.max(TARGET_SAMPLE_COUNT * 2),
                Err(e) => {
                    error!("Error getting minimum latency: {:?}", e);
                    TARGET_SAMPLE_COUNT * 2
                }
            }
        };

        info!(
            "Opening cubeb stream {} type {:?} with: rate {} channels {} (system channels {}) latency {}",
            name, stream_type, TARGET_SAMPLE_RATE, self.device_channels, system_channels, minimum_latency
        );

        let callback_base = Arc::clone(&base);
        let device_channels = self.device_channels;
        let st = stream_type;

        // cubeb-rs's `StreamBuilder<F>` treats `F` as a FRAME type, not a
        // sample type. Its internal `data_cb_c` slices the raw output buffer
        // as `nframes` elements of size `sizeof(F)`. With `F = i16` for a
        // stereo stream we only see/fill half the bytes cubeb actually
        // allocated, return `nframes/channels` to cubeb, and produce audio
        // at `1/channels` of real-time — observed as ~65 Hz audio-event rate
        // instead of zuyu's ~200 Hz on this host.
        //
        // Fix: extend the slice from `nframes` elements to the real
        // `nframes * device_channels` elements via raw pointer reslice. This
        // requires no F-type change because the underlying pointer is the
        // same; we're just correcting the slice length cubeb-rs got wrong
        // for any non-mono stream.
        let data_callback = move |input: &[i16], output: &mut [i16]| -> isize {
            let chans = device_channels as usize;
            // cubeb-rs gave us `nframes`-long slices; cubeb's real
            // allocation is `nframes * chans` samples. Re-slice to the
            // correct length.
            let in_full: &[i16] = if input.is_empty() {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(input.as_ptr(), input.len() * chans) }
            };
            let out_full: &mut [i16] = if output.is_empty() {
                &mut []
            } else {
                unsafe { std::slice::from_raw_parts_mut(output.as_mut_ptr(), output.len() * chans) }
            };
            process_stream_callback(&callback_base, st, device_channels, in_full, out_full)
        };

        let callback_name = name.to_string();
        let callback_type = stream_type;
        let state_callback = move |state: cubeb::State| {
            if should_trace_cubeb_state() {
                log::info!(
                    "CubebSink state name={} type={:?} state={:?}",
                    callback_name,
                    callback_type,
                    state
                );
            }
        };

        let mut builder = cubeb::StreamBuilder::<i16>::new();
        builder.name(name.to_string()).latency(minimum_latency);

        if stream_type == StreamType::In {
            builder.input(self.input_device, &params);
        } else {
            builder.output(self.output_device, &params);
        }

        builder
            .data_callback(data_callback)
            .state_callback(state_callback);

        match builder.init(ctx) {
            Ok(backend) => {
                let handle = new_stream_handle(CubebSinkStream::new(
                    base,
                    Some(backend),
                    #[cfg(windows)]
                    com_apartment,
                ));
                self.streams.push(handle.clone());
                handle
            }
            Err(e) => {
                error!("Error initializing cubeb stream: {:?}", e);
                let handle = new_stream_handle(CubebSinkStream::new(
                    base,
                    None,
                    #[cfg(windows)]
                    com_apartment,
                ));
                self.streams.push(handle.clone());
                handle
            }
        }
    }

    fn close_stream(&mut self, stream: &SinkStreamHandle) {
        if let Some(index) = self
            .streams
            .iter()
            .position(|entry| Arc::ptr_eq(entry, stream))
        {
            self.streams[index].finalize();
            self.streams.remove(index);
        }
    }

    fn close_streams(&mut self) {
        for stream in &self.streams {
            stream.finalize();
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::sink_stream::{SinkBuffer, StreamType};
    fn make_system() -> SharedSystem {
        crate::make_test_system()
    }

    #[test]
    fn cubeb_callback_returns_frame_count_not_sample_count() {
        let system = make_system();
        let handle = Arc::new(parking_lot::Mutex::new(SinkStreamBase::new(
            system,
            StreamType::Out,
            2,
            2,
            String::new(),
        )));
        handle.lock().append_buffer(
            SinkBuffer {
                frames: 2,
                frames_played: 0,
                tag: 1,
                consumed: false,
            },
            &[1, 2, 3, 4],
        );
        let mut output = [0i16; 4];

        let written = process_stream_callback(&handle, StreamType::Out, 2, &[], &mut output);

        assert_eq!(written, 2);
        assert_eq!(output, [1, 2, 3, 4]);
    }

    #[test]
    fn cubeb_input_callback_uses_the_input_frame_count() {
        let handle = Arc::new(parking_lot::Mutex::new(SinkStreamBase::new(
            make_system(),
            StreamType::In,
            2,
            2,
            String::new(),
        )));
        let mut output = [];

        let written =
            process_stream_callback(&handle, StreamType::In, 2, &[1, 2, 3, 4], &mut output);

        assert_eq!(written, 2);
        assert_eq!(handle.lock().release_buffer(4), vec![8, 16, 24, 32]);
    }
}

/// Get a list of connected devices from cubeb.
pub fn list_cubeb_sink_devices(capture: bool) -> Vec<String> {
    #[cfg(windows)]
    let com_init_result = initialize_com_multithreaded();

    let ctx = match Context::init(Some(c"ruzu Device Enumerator"), None) {
        Ok(ctx) => ctx,
        Err(e) => {
            error!("cubeb_init failed: {:?}", e);
            return Vec::new();
        }
    };

    #[cfg(windows)]
    if com_init_result >= 0 {
        uninitialize_com();
    }

    let device_type = if capture {
        DeviceType::INPUT
    } else {
        DeviceType::OUTPUT
    };

    let devices = match ctx.enumerate_devices(device_type) {
        Ok(devices) => devices,
        Err(_) => {
            warn!("Audio output device enumeration not supported");
            return Vec::new();
        }
    };

    let mut device_list = Vec::new();
    for device in devices.iter() {
        if let Some(friendly_name) = device.friendly_name() {
            if !friendly_name.is_empty() && device.state() == DeviceState::Enabled {
                device_list.push(friendly_name.to_string());
            }
        }
    }
    device_list
}

/// Return Cubeb's minimum output latency, matching upstream `GetCubebLatency`.
pub fn get_cubeb_latency() -> u32 {
    #[cfg(windows)]
    let com_init_result = initialize_com_multithreaded();

    let ctx = match Context::init(Some(c"ruzu Latency Getter"), None) {
        Ok(ctx) => ctx,
        Err(_) => {
            error!("cubeb_init failed");
            return 10_000;
        }
    };

    #[cfg(windows)]
    if com_init_result >= 0 {
        uninitialize_com();
    }

    let params = StreamParamsBuilder::new()
        .rate(TARGET_SAMPLE_RATE)
        .channels(2)
        .format(SampleFormat::S16LE)
        .layout(cubeb::ChannelLayout::STEREO)
        .take();

    match ctx.min_latency(&params) {
        Ok(l) => l.max(TARGET_SAMPLE_COUNT * 2),
        Err(_) => {
            error!("Error getting minimum Cubeb latency");
            TARGET_SAMPLE_COUNT * 2
        }
    }
}

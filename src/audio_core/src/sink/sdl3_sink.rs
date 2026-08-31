// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `audio_core/sink/sdl3_sink.h` and `sdl3_sink.cpp`.

use std::ffi::{c_void, CStr};
use std::sync::Arc;

use log::{error, info};
use parking_lot::Mutex;
use sdl3::sys::everything as sdl;

use crate::common::common::{TARGET_SAMPLE_COUNT, TARGET_SAMPLE_RATE};
use crate::sink::sink::{new_stream_handle, Sink, AUTO_DEVICE_NAME};
use crate::sink::sink_stream::{
    SharedSinkStreamBase, SinkStream, SinkStreamBase, SinkStreamHandle, SinkStreamLifecycleState,
    StreamType,
};
use crate::SharedSystem;

fn sdl_error() -> String {
    unsafe { CStr::from_ptr(sdl::SDL_GetError()) }
        .to_string_lossy()
        .into_owned()
}

fn ensure_audio_initialized() -> bool {
    unsafe {
        if sdl::SDL_WasInit(sdl::SDL_INIT_AUDIO).value() != 0 {
            return true;
        }
        if !sdl::SDL_InitSubSystem(sdl::SDL_INIT_AUDIO) {
            error!("SDL_InitSubSystem audio failed: {}", sdl_error());
            return false;
        }
    }
    true
}

fn find_audio_device_by_name(device_name: &str, capture: bool) -> sdl::SDL_AudioDeviceID {
    let default = if capture {
        sdl::SDL_AUDIO_DEVICE_DEFAULT_RECORDING
    } else {
        sdl::SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK
    };
    unsafe {
        let mut count = 0;
        let devices = if capture {
            sdl::SDL_GetAudioRecordingDevices(&mut count)
        } else {
            sdl::SDL_GetAudioPlaybackDevices(&mut count)
        };
        if devices.is_null() {
            return default;
        }
        let mut selected = default;
        for index in 0..count {
            let id = *devices.add(index as usize);
            let name = sdl::SDL_GetAudioDeviceName(id);
            if !name.is_null() && CStr::from_ptr(name).to_bytes() == device_name.as_bytes() {
                selected = id;
                break;
            }
        }
        sdl::SDL_free(devices.cast());
        selected
    }
}

struct SDLCallbackState {
    base: SharedSinkStreamBase,
    stream_type: StreamType,
    device_channels: usize,
}

unsafe extern "C" fn data_callback(
    userdata: *mut c_void,
    stream: *mut sdl::SDL_AudioStream,
    additional_amount: i32,
    total_amount: i32,
) {
    if userdata.is_null() || stream.is_null() {
        return;
    }
    let state = &*(userdata as *const SDLCallbackState);
    let frame_size = state.device_channels.max(1);

    if state.stream_type == StreamType::In {
        let bytes_available = sdl::SDL_GetAudioStreamAvailable(stream);
        if bytes_available <= 0 {
            return;
        }
        let mut input = vec![0i16; bytes_available as usize / size_of::<i16>()];
        let bytes_read =
            sdl::SDL_GetAudioStreamData(stream, input.as_mut_ptr().cast(), bytes_available);
        if bytes_read <= 0 {
            return;
        }
        let samples = bytes_read as usize / size_of::<i16>();
        state
            .base
            .lock()
            .process_audio_in(&input[..samples], samples / frame_size);
    } else {
        if additional_amount <= 0 && total_amount <= 0 {
            return;
        }
        let bytes_requested = if additional_amount > 0 {
            additional_amount
        } else {
            total_amount
        };
        let mut output = vec![0i16; bytes_requested as usize / size_of::<i16>()];
        let frames = output.len() / frame_size;
        state
            .base
            .lock()
            .process_audio_out_and_render(&mut output, frames);
        let _ = sdl::SDL_PutAudioStreamData(stream, output.as_ptr().cast(), bytes_requested);
    }
}

struct SDLLifecycle {
    stream: *mut sdl::SDL_AudioStream,
    state: SinkStreamLifecycleState,
    // Must outlive `stream`: SDL stores this address as callback userdata.
    callback_state: Option<Box<SDLCallbackState>>,
}

unsafe impl Send for SDLLifecycle {}
unsafe impl Sync for SDLLifecycle {}

struct SDLSinkStream {
    base: SharedSinkStreamBase,
    lifecycle: Mutex<SDLLifecycle>,
}

unsafe impl Send for SDLSinkStream {}
unsafe impl Sync for SDLSinkStream {}

impl SinkStream for SDLSinkStream {
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
            if self.base.lock().signal_stop() && !lifecycle.stream.is_null() {
                unsafe {
                    let _ = sdl::SDL_PauseAudioStreamDevice(lifecycle.stream);
                }
            }
            lifecycle.state = SinkStreamLifecycleState::Stopped;
        }

        if !lifecycle.stream.is_null() {
            unsafe {
                let _ = sdl::SDL_ClearAudioStream(lifecycle.stream);
                sdl::SDL_DestroyAudioStream(lifecycle.stream);
            }
        }
        lifecycle.stream = std::ptr::null_mut();
        lifecycle.callback_state.take();
        lifecycle.state = SinkStreamLifecycleState::Finalized;
    }

    fn start(&self, _resume: bool) {
        let mut lifecycle = self.lifecycle.lock();
        if lifecycle.state != SinkStreamLifecycleState::Stopped
            || lifecycle.stream.is_null()
            || !self.base.lock().signal_start()
        {
            return;
        }
        lifecycle.state = SinkStreamLifecycleState::Starting;
        unsafe {
            let _ = sdl::SDL_ResumeAudioStreamDevice(lifecycle.stream);
        }
        lifecycle.state = SinkStreamLifecycleState::Running;
    }

    fn stop(&self) {
        let mut lifecycle = self.lifecycle.lock();
        if lifecycle.state != SinkStreamLifecycleState::Running || lifecycle.stream.is_null() {
            return;
        }
        lifecycle.state = SinkStreamLifecycleState::Stopping;
        if self.base.lock().signal_stop() {
            unsafe {
                let _ = sdl::SDL_PauseAudioStreamDevice(lifecycle.stream);
            }
        }
        lifecycle.state = SinkStreamLifecycleState::Stopped;
    }
}

impl Drop for SDLSinkStream {
    fn drop(&mut self) {
        self.finalize();
    }
}

pub struct SDLSink {
    output_device: String,
    input_device: String,
    device_channels: u32,
    system_channels: u32,
    streams: Vec<SinkStreamHandle>,
}

unsafe impl Send for SDLSink {}
unsafe impl Sync for SDLSink {}

impl SDLSink {
    pub fn new(target_device_name: &str) -> Self {
        let _ = ensure_audio_initialized();
        let output_device =
            if target_device_name != AUTO_DEVICE_NAME && !target_device_name.is_empty() {
                target_device_name.to_string()
            } else {
                String::new()
            };
        Self {
            output_device,
            input_device: String::new(),
            device_channels: 2,
            system_channels: 2,
            streams: Vec::new(),
        }
    }
}

impl Sink for SDLSink {
    fn acquire_sink_stream(
        &mut self,
        system: SharedSystem,
        system_channels: u32,
        name: &str,
        stream_type: StreamType,
    ) -> SinkStreamHandle {
        self.system_channels = system_channels;
        let base = Arc::new(Mutex::new(SinkStreamBase::new(
            system,
            stream_type,
            system_channels,
            self.device_channels,
            name.to_string(),
        )));

        if !ensure_audio_initialized() {
            let handle = new_stream_handle(SDLSinkStream {
                base: Arc::clone(&base),
                lifecycle: Mutex::new(SDLLifecycle {
                    stream: std::ptr::null_mut(),
                    state: SinkStreamLifecycleState::Stopped,
                    callback_state: Some(Box::new(SDLCallbackState {
                        base,
                        stream_type,
                        device_channels: self.device_channels as usize,
                    })),
                }),
            });
            self.streams.push(handle.clone());
            return handle;
        }

        let capture = stream_type == StreamType::In;
        let device_name = if capture {
            &self.input_device
        } else {
            &self.output_device
        };
        let device = if device_name.is_empty() {
            if capture {
                sdl::SDL_AUDIO_DEVICE_DEFAULT_RECORDING
            } else {
                sdl::SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK
            }
        } else {
            find_audio_device_by_name(device_name, capture)
        };
        let spec = sdl::SDL_AudioSpec {
            format: sdl::SDL_AUDIO_S16,
            channels: self.device_channels as i32,
            freq: TARGET_SAMPLE_RATE as i32,
        };
        let mut callback_state = Box::new(SDLCallbackState {
            base: Arc::clone(&base),
            stream_type,
            device_channels: self.device_channels as usize,
        });
        let stream = unsafe {
            sdl::SDL_OpenAudioDeviceStream(
                device,
                &spec,
                Some(data_callback),
                (&mut *callback_state as *mut SDLCallbackState).cast(),
            )
        };
        if stream.is_null() {
            error!("Error opening SDL audio device: {}", sdl_error());
        } else {
            let mut stream_in = sdl::SDL_AudioSpec::default();
            let mut stream_out = sdl::SDL_AudioSpec::default();
            unsafe {
                let _ = sdl::SDL_GetAudioStreamFormat(stream, &mut stream_in, &mut stream_out);
            }
            info!(
                "Opening SDL stream {:?} with: rate {} channels {} (system channels {}) format {}",
                stream,
                stream_out.freq,
                stream_out.channels,
                system_channels,
                stream_out.format.value()
            );
        }

        let handle = new_stream_handle(SDLSinkStream {
            base,
            lifecycle: Mutex::new(SDLLifecycle {
                stream,
                state: SinkStreamLifecycleState::Stopped,
                callback_state: Some(callback_state),
            }),
        });
        self.streams.push(handle.clone());
        handle
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
        self.streams
            .first()
            .map_or(1.0, |stream| stream.get_device_volume())
    }

    fn set_device_volume(&mut self, volume: f32) {
        for stream in &self.streams {
            stream.set_device_volume(volume);
        }
    }

    fn set_system_volume(&mut self, volume: f32) {
        for stream in &self.streams {
            stream.set_system_volume(volume);
        }
    }

    fn get_device_channels(&self) -> u32 {
        self.device_channels
    }

    fn get_system_channels(&self) -> u32 {
        self.system_channels
    }
}

pub fn list_sdl_sink_devices(capture: bool) -> Vec<String> {
    if !ensure_audio_initialized() {
        return Vec::new();
    }
    unsafe {
        let mut count = 0;
        let devices = if capture {
            sdl::SDL_GetAudioRecordingDevices(&mut count)
        } else {
            sdl::SDL_GetAudioPlaybackDevices(&mut count)
        };
        if devices.is_null() {
            return Vec::new();
        }
        let mut result = Vec::with_capacity(count as usize);
        for index in 0..count {
            let name = sdl::SDL_GetAudioDeviceName(*devices.add(index as usize));
            if !name.is_null() {
                result.push(CStr::from_ptr(name).to_string_lossy().into_owned());
            }
        }
        sdl::SDL_free(devices.cast());
        result
    }
}

pub fn get_sdl_latency() -> u32 {
    TARGET_SAMPLE_COUNT * 2
}

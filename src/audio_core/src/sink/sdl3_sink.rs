// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `audio_core/sink/sdl3_sink.h` and `sdl3_sink.cpp`.

use std::ffi::{c_void, CStr};
use std::sync::Arc;

use log::{error, info};
use sdl3::sys::everything as sdl;

use crate::common::common::{TARGET_SAMPLE_COUNT, TARGET_SAMPLE_RATE};
use crate::sink::sink::{new_stream_handle, Sink, AUTO_DEVICE_NAME};
use crate::sink::sink_stream::{stop_sink_stream, SinkStream, SinkStreamHandle, StreamType};
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
    handle: SinkStreamHandle,
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
            .handle
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
            .handle
            .lock()
            .process_audio_out_and_render(&mut output, frames);
        let _ = sdl::SDL_PutAudioStreamData(stream, output.as_ptr().cast(), bytes_requested);
    }
}

struct SDLStream {
    handle: SinkStreamHandle,
    stream: *mut sdl::SDL_AudioStream,
    // Must outlive `stream`: SDL stores this address as callback userdata.
    _callback_state: Box<SDLCallbackState>,
}

unsafe impl Send for SDLStream {}
unsafe impl Sync for SDLStream {}

impl Drop for SDLStream {
    fn drop(&mut self) {
        if self.stream.is_null() {
            return;
        }
        stop_sink_stream(&self.handle);
        unsafe {
            let _ = sdl::SDL_ClearAudioStream(self.stream);
            sdl::SDL_DestroyAudioStream(self.stream);
        }
        self.stream = std::ptr::null_mut();
    }
}

pub struct SDLSink {
    output_device: String,
    input_device: String,
    device_channels: u32,
    system_channels: u32,
    streams: Vec<SDLStream>,
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
        let mut sink_stream = SinkStream::new(system, stream_type);
        sink_stream.system_channels = system_channels;
        sink_stream.device_channels = self.device_channels;
        sink_stream.name = name.to_string();
        let handle = new_stream_handle(sink_stream);

        if !ensure_audio_initialized() {
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
            handle: handle.clone(),
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
            return handle;
        }

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

        let stream_address = stream as usize;
        handle.lock().set_backend_ctl(Box::new(move |start| unsafe {
            let stream = stream_address as *mut sdl::SDL_AudioStream;
            if start {
                let _ = sdl::SDL_ResumeAudioStreamDevice(stream);
            } else {
                let _ = sdl::SDL_PauseAudioStreamDevice(stream);
            }
        }));
        self.streams.push(SDLStream {
            handle: handle.clone(),
            stream,
            _callback_state: callback_state,
        });
        handle
    }

    fn close_stream(&mut self, stream: &SinkStreamHandle) {
        self.streams
            .retain(|entry| !Arc::ptr_eq(&entry.handle, stream));
    }

    fn close_streams(&mut self) {
        self.streams.clear();
    }

    fn get_device_volume(&self) -> f32 {
        self.streams
            .first()
            .map_or(1.0, |stream| stream.handle.lock().get_device_volume())
    }

    fn set_device_volume(&mut self, volume: f32) {
        for stream in &self.streams {
            stream.handle.lock().set_device_volume(volume);
        }
    }

    fn set_system_volume(&mut self, volume: f32) {
        for stream in &self.streams {
            stream.handle.lock().set_system_volume(volume);
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

use crate::adsp::apps::audio_renderer::command_buffer::{CommandBuffer, ProcessHandle};
use crate::adsp::apps::audio_renderer::command_list_processor::CommandListProcessor;
use crate::adsp::mailbox::{AppMailboxId, Direction, Mailbox};
use crate::common::common::MAX_RENDERER_SESSIONS;
use crate::sink::{stop_sink_stream, SinkHandle, SinkStreamHandle, StreamType};
use crate::SharedSystem;
use common::thread::{set_current_thread_name, set_current_thread_priority, ThreadPriority};
use log::{error, warn};
use parking_lot::Mutex;
// Stub: ruzu_kernel removed. KProcess replaced with opaque *mut ().
use std::array;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

fn should_trace_adsp_audio() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_ADSP_AUDIO").is_some())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
#[repr(u32)]
pub enum Message {
    Invalid = 0,
    MapUnmap_Map = 1,
    MapUnmap_MapResponse = 2,
    MapUnmap_Unmap = 3,
    MapUnmap_UnmapResponse = 4,
    MapUnmap_InvalidateCache = 5,
    MapUnmap_InvalidateCacheResponse = 6,
    MapUnmap_Shutdown = 7,
    MapUnmap_ShutdownResponse = 8,
    InitializeOK = 22,
    RenderResponse = 32,
    Render = 42,
    Shutdown = 52,
}

pub struct AudioRenderer {
    system: SharedSystem,
    sink: SinkHandle,
    running: bool,
    mailbox: Mailbox,
    main_thread: Option<JoinHandle<()>>,
    shared: Arc<Mutex<RendererShared>>,
    stop_requested: Arc<AtomicBool>,
    signalled_tick: u64,
}

struct RendererShared {
    command_buffers: [CommandBuffer; MAX_RENDERER_SESSIONS],
    command_list_processors: [CommandListProcessor; MAX_RENDERER_SESSIONS],
    streams: [Option<SinkStreamHandle>; MAX_RENDERER_SESSIONS],
}

impl Default for RendererShared {
    fn default() -> Self {
        Self {
            command_buffers: array::from_fn(|_| CommandBuffer::default()),
            command_list_processors: array::from_fn(|_| CommandListProcessor::default()),
            streams: array::from_fn(|_| None),
        }
    }
}

impl AudioRenderer {
    pub fn new(system: SharedSystem, sink: SinkHandle) -> Self {
        Self {
            system,
            sink,
            running: false,
            mailbox: Mailbox::default(),
            main_thread: None,
            shared: Arc::new(Mutex::new(RendererShared::default())),
            stop_requested: Arc::new(AtomicBool::new(false)),
            signalled_tick: 0,
        }
    }

    pub fn start(&mut self) {
        if self.running {
            return;
        }
        self.create_sink_streams();
        self.mailbox.initialize(AppMailboxId::AudioRenderer);
        self.stop_requested.store(false, Ordering::SeqCst);
        let mailbox = self.mailbox.clone();
        let shared = self.shared.clone();
        let system = self.system.clone();
        let stop_requested = self.stop_requested.clone();
        self.main_thread = Some(
            thread::Builder::new()
                .name("DSP_AudioRenderer_Main".to_string())
                .spawn(move || Self::main(system, mailbox, shared, stop_requested))
                .expect("failed to spawn DSP audio renderer thread"),
        );
        self.mailbox
            .send(Direction::Dsp, Message::InitializeOK as u32);
        let message = self.mailbox.receive(Direction::Host);
        if message != Message::InitializeOK as u32 {
            error!(
                "Host Audio Renderer -- failed to receive initialize response from ADSP: expected {}, got {}",
                Message::InitializeOK as u32,
                message
            );
            self.abort_startup();
            return;
        }
        self.running = true;
    }

    pub fn stop(&mut self) {
        if !self.running {
            return;
        }

        self.mailbox.send(Direction::Dsp, Message::Shutdown as u32);
        self.stop_requested.store(true, Ordering::SeqCst);
        if let Some(thread) = self.main_thread.take() {
            let _ = thread.join();
        }

        let mut received_shutdown = false;
        while let Some(message) = self.mailbox.try_receive(Direction::Host) {
            received_shutdown |= message == Message::Shutdown as u32;
        }
        if !received_shutdown {
            error!(
                "Host Audio Renderer -- failed to receive shutdown response from ADSP: expected {}",
                Message::Shutdown as u32
            );
        }
        self.mailbox.reset();

        self.close_sink_streams();
        self.running = false;
    }

    pub fn signal(&mut self) {
        self.signalled_tick = self
            .system
            .get()
            .core_timing()
            .get_global_time_ns()
            .as_nanos() as u64;
        self.mailbox.send(Direction::Dsp, Message::Render as u32);
    }

    pub fn wait(&mut self) {
        if should_trace_adsp_audio() {
            log::info!("ADSP::AudioRenderer host wait begin");
        }
        let message = self.mailbox.receive(Direction::Host);
        if should_trace_adsp_audio() {
            log::info!("ADSP::AudioRenderer host wait end message={}", message);
        }
        if message != Message::RenderResponse as u32 {
            error!(
                "Did not receive the expected render response from the AudioRenderer: expected {}, got {}",
                Message::RenderResponse as u32,
                message
            );
        }
        self.post_dsp_clear_command_buffer();
    }

    /// Rust's system manager can be stopped from another host thread. Make
    /// that blocking receive interruptible so teardown does not depend on a
    /// DSP response after the DSP worker has already exited.
    pub fn wait_with_stop(&mut self, stop_requested: &AtomicBool) -> bool {
        let Some(message) = self
            .mailbox
            .receive_with_stop(Direction::Host, stop_requested)
        else {
            return false;
        };
        if message != Message::RenderResponse as u32 {
            error!(
                "Did not receive the expected render response from the AudioRenderer: expected {}, got {}",
                Message::RenderResponse as u32,
                message
            );
        }
        self.post_dsp_clear_command_buffer();
        true
    }

    /// Wait for ADSP response with a timeout. Returns true if response received.
    pub fn wait_with_timeout(&mut self, timeout: std::time::Duration) -> bool {
        let start = std::time::Instant::now();
        loop {
            if let Some(message) = self.mailbox.try_receive(Direction::Host) {
                if message == Message::RenderResponse as u32 {
                    self.post_dsp_clear_command_buffer();
                }
                return true;
            }
            if start.elapsed() >= timeout {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    pub fn send(&self, direction: Direction, message: u32) {
        self.mailbox.send(direction, message);
    }

    pub fn receive(&self, direction: Direction) -> u32 {
        self.mailbox.receive(direction)
    }

    pub fn set_command_buffer(
        &mut self,
        session_id: i32,
        buffer: usize,
        size: u64,
        time_limit: u64,
        applet_resource_user_id: u64,
        process: *mut (),
        reset: bool,
    ) {
        let mut shared = self.shared.lock();
        let buffer_state = &mut shared.command_buffers[session_id as usize];
        buffer_state.buffer = buffer;
        buffer_state.size = size;
        buffer_state.time_limit = time_limit;
        buffer_state.applet_resource_user_id = applet_resource_user_id;
        buffer_state.process = ProcessHandle::from_ptr(process);
        buffer_state.reset_buffer = reset;
    }

    pub fn get_remain_command_count(&self, session_id: i32) -> u32 {
        self.shared.lock().command_buffers[session_id as usize].remaining_command_count
    }

    pub fn clear_remain_command_count(&mut self, session_id: i32) {
        self.shared.lock().command_buffers[session_id as usize].remaining_command_count = 0;
    }

    pub fn get_rendering_start_tick(&self, session_id: i32) -> u64 {
        (1000 * self.shared.lock().command_buffers[session_id as usize].render_time_taken_us)
            + self.signalled_tick
    }

    pub fn get_device_channels(&self) -> u32 {
        self.sink.lock().get_device_channels()
    }

    #[cfg(test)]
    pub(crate) fn get_command_buffer_process(&self, session_id: usize) -> *mut () {
        self.shared.lock().command_buffers[session_id]
            .process
            .as_ptr()
    }

    fn create_sink_streams(&mut self) {
        let channels = self.sink.lock().get_device_channels();
        let mut shared = self.shared.lock();
        for index in 0..MAX_RENDERER_SESSIONS {
            let name = format!("ADSP_RenderStream-{index}");
            let handle = self.sink.lock().acquire_sink_stream(
                self.system.clone(),
                channels,
                &name,
                StreamType::Render,
            );
            handle.set_ring_size(4);
            shared.streams[index] = Some(handle);
        }
    }

    fn close_sink_streams(&mut self) {
        let mut sink = self.sink.lock();
        let mut shared = self.shared.lock();
        for stream in &mut shared.streams {
            if let Some(handle) = stream.take() {
                stop_sink_stream(&handle);
                sink.close_stream(&handle);
            }
        }
    }

    fn abort_startup(&mut self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        self.mailbox.reset();
        if let Some(thread) = self.main_thread.take() {
            let _ = thread.join();
        }
        self.close_sink_streams();
        self.running = false;
    }

    fn post_dsp_clear_command_buffer(&mut self) {
        for buffer in &mut self.shared.lock().command_buffers {
            buffer.buffer = 0;
            buffer.size = 0;
            buffer.reset_buffer = false;
        }
    }

    fn main(
        system: SharedSystem,
        mailbox: Mailbox,
        shared: Arc<Mutex<RendererShared>>,
        stop_requested: Arc<AtomicBool>,
    ) {
        set_current_thread_name("DSP_AudioRenderer_Main");
        set_current_thread_priority(ThreadPriority::High);

        let Some(message) = mailbox.receive_with_stop(Direction::Dsp, &stop_requested) else {
            return;
        };
        if message != Message::InitializeOK as u32 {
            error!(
                "ADSP Audio Renderer -- failed to receive initialize message from host: expected {}, got {}",
                Message::InitializeOK as u32,
                message
            );
            return;
        }
        mailbox.send(Direction::Host, Message::InitializeOK as u32);

        const MAX_PROCESS_TIME: u64 = 2_304_000;

        loop {
            let Some(message) = mailbox.receive_with_stop(Direction::Dsp, &stop_requested) else {
                return;
            };
            match message {
                message if message == Message::Shutdown as u32 => {
                    mailbox.send(Direction::Host, Message::Shutdown as u32);
                    return;
                }
                message if message == Message::MapUnmap_Map as u32 => {
                    mailbox.send(Direction::Host, Message::MapUnmap_MapResponse as u32);
                }
                message if message == Message::MapUnmap_Unmap as u32 => {
                    mailbox.send(Direction::Host, Message::MapUnmap_UnmapResponse as u32);
                }
                message if message == Message::MapUnmap_InvalidateCache as u32 => {
                    mailbox.send(
                        Direction::Host,
                        Message::MapUnmap_InvalidateCacheResponse as u32,
                    );
                }
                message if message == Message::MapUnmap_Shutdown as u32 => {
                    mailbox.send(Direction::Host, Message::MapUnmap_ShutdownResponse as u32);
                }
                message if message == Message::Render as u32 => {
                    if should_trace_adsp_audio() {
                        log::info!("ADSP::AudioRenderer dsp main received Render");
                    }
                    if system.get().is_shutting_down() {
                        thread::sleep(Duration::from_millis(200));
                        if should_trace_adsp_audio() {
                            log::info!(
                                "ADSP::AudioRenderer dsp main sending RenderResponse (shutdown)"
                            );
                        }
                        mailbox.send(Direction::Host, Message::RenderResponse as u32);
                        continue;
                    }

                    let mut buffers_reset = [false; MAX_RENDERER_SESSIONS];
                    let mut render_times_taken = [0u64; MAX_RENDERER_SESSIONS];
                    let start_time =
                        system.get().core_timing().get_global_time_us().as_micros() as u64;
                    let session0_applet_resource_user_id =
                        shared.lock().command_buffers[0].applet_resource_user_id;

                    for index in 0..MAX_RENDERER_SESSIONS {
                        let (stream, buffer_state) = {
                            let shared = shared.lock();
                            (shared.streams[index].clone(), shared.command_buffers[index])
                        };
                        if buffer_state.buffer == 0 {
                            continue;
                        }
                        let Some(stream) = stream else {
                            continue;
                        };

                        if should_trace_adsp_audio() && index == 0 {
                            log::info!(
                                "ADSP::AudioRenderer pre-process session={} buffer=0x{:X} size={} remaining={} reset={} queue={} paused={}",
                                index,
                                buffer_state.buffer,
                                buffer_state.size,
                                buffer_state.remaining_command_count,
                                buffer_state.reset_buffer,
                                stream.get_queue_size(),
                                stream.is_paused()
                            );
                        }

                        {
                            let mut shared = shared.lock();
                            let RendererShared {
                                command_buffers,
                                command_list_processors,
                                ..
                            } = &mut *shared;
                            let processor = &mut command_list_processors[index];
                            let buffer = &mut command_buffers[index];
                            if buffer.remaining_command_count == 0 {
                                let _ = processor.initialize(
                                    system.clone(),
                                    buffer.process.as_ptr(),
                                    buffer.buffer,
                                    buffer.size,
                                    stream.clone(),
                                );
                            }
                        }

                        if buffer_state.reset_buffer && !buffers_reset[index] {
                            stream.clear_queue();
                            buffers_reset[index] = true;
                        }

                        let mut max_time = MAX_PROCESS_TIME;
                        if index == 1
                            && buffer_state.applet_resource_user_id
                                == session0_applet_resource_user_id
                        {
                            max_time = max_time.saturating_sub(render_times_taken[0]);
                            if render_times_taken[0] > MAX_PROCESS_TIME {
                                max_time = 0;
                            }
                        }
                        max_time = max_time.min(buffer_state.time_limit);

                        shared.lock().command_list_processors[index].set_process_time_max(max_time);

                        if index == 0 {
                            // Wait without holding the stream lock — the sink
                            // callback needs the lock to consume buffers.
                            let release = stream.base().lock().release.clone();
                            if should_trace_adsp_audio() {
                                let queued = release.queued_buffers.load(Ordering::Acquire);
                                let max = release.max_queue_size.load(Ordering::Acquire);
                                let paused = release.paused.load(Ordering::Acquire);
                                log::info!(
                                    "ADSP::AudioRenderer wait_free_space begin session={} queued={} max={} paused={}",
                                    index,
                                    queued,
                                    max,
                                    paused
                                );
                            }
                            release.wait_free_space_with_stop(&stop_requested);
                            if should_trace_adsp_audio() {
                                let queued = release.queued_buffers.load(Ordering::Acquire);
                                let max = release.max_queue_size.load(Ordering::Acquire);
                                let paused = release.paused.load(Ordering::Acquire);
                                log::info!(
                                    "ADSP::AudioRenderer wait_free_space end session={} queued={} max={} paused={}",
                                    index,
                                    queued,
                                    max,
                                    paused
                                );
                            }
                        }

                        let mut shared = shared.lock();
                        let RendererShared {
                            command_buffers,
                            command_list_processors,
                            ..
                        } = &mut *shared;
                        let processor = &mut command_list_processors[index];
                        let buffer = &mut command_buffers[index];
                        render_times_taken[index] = processor.process(index as u32);
                        let end_time =
                            system.get().core_timing().get_global_time_us().as_micros() as u64;
                        buffer.remaining_command_count = processor.get_remaining_command_count();
                        buffer.render_time_taken_us = end_time.saturating_sub(start_time);
                        if should_trace_adsp_audio() && index == 0 {
                            log::info!(
                                "ADSP::AudioRenderer post-process session={} remaining={} render_time_us={} queue={} paused={}",
                                index,
                                buffer.remaining_command_count,
                                buffer.render_time_taken_us,
                                stream.get_queue_size(),
                                stream.is_paused()
                            );
                        }
                    }

                    if should_trace_adsp_audio() {
                        log::info!("ADSP::AudioRenderer dsp main sending RenderResponse");
                    }
                    mailbox.send(Direction::Host, Message::RenderResponse as u32);
                }
                _ => {
                    warn!("ADSP AudioRenderer received an invalid message, msg={message:02X}!");
                }
            }
        }
    }
}

impl Drop for AudioRenderer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::behavior::BehaviorInfo;
    use crate::renderer::command::command_buffer::CommandBuffer as RenderCommandBuffer;
    use crate::renderer::command::command_processing_time_estimator::CommandProcessingTimeEstimator;
    use crate::renderer::command::commands::{ClearMixBufferCommand, Command, DeviceSinkCommand};
    use crate::renderer::command::CommandListHeader;
    use crate::sink::null_sink::NullSink;
    use crate::sink::sink::new_sink_handle;
    use crate::sink::sink_stream::SinkBuffer;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    fn make_system() -> SharedSystem {
        crate::make_test_system()
    }

    fn serialize_commands(
        commands: &[Command],
        node_id: u32,
        sample_buffer: usize,
        sample_count: u32,
    ) -> Vec<u8> {
        let behavior = BehaviorInfo::new();
        let estimator = CommandProcessingTimeEstimator::new(&behavior, sample_count, 2);
        let mut command_buffer = RenderCommandBuffer::new(0x400, estimator);
        for command in commands {
            assert!(command_buffer.push(*command, node_id));
        }
        let header = CommandListHeader {
            buffer_size: 0x400,
            command_count: command_buffer.count(),
            samples_buffer: sample_buffer,
            buffer_count: 2,
            sample_count,
            sample_rate: 48_000,
        };
        let mut bytes = vec![0u8; 0x400];
        let written = command_buffer.serialize_into(&header, &mut bytes);
        bytes.truncate(written);
        bytes
    }

    #[test]
    fn wait_clears_stream_queue_when_command_buffer_requests_reset() {
        let system = make_system();
        let sink = new_sink_handle(Box::new(NullSink::new_recording_for_test("test")));
        let mut renderer = AudioRenderer::new(system, sink);
        renderer.start();
        let mut mix_buffer = vec![7i32; 4];
        let bytes = serialize_commands(
            &[Command::ClearMixBuffer(ClearMixBufferCommand {
                buffer_count: 2,
            })],
            1,
            mix_buffer.as_mut_ptr() as usize,
            2,
        );

        let stream = renderer.shared.lock().streams[0].as_ref().unwrap().clone();
        stream.append_buffer(
            SinkBuffer {
                frames: 2,
                frames_played: 0,
                tag: 1,
                consumed: false,
            },
            &[1, 2, 3, 4],
        );
        assert_eq!(stream.get_queue_size(), 1);

        renderer.set_command_buffer(
            0,
            bytes.as_ptr() as usize,
            bytes.len() as u64,
            1000,
            1,
            std::ptr::null_mut(),
            true,
        );
        renderer.signal();
        renderer.wait();

        assert_eq!(stream.get_queue_size(), 0);
        let shared = renderer.shared.lock();
        assert_eq!(shared.command_buffers[0].buffer, 0);
        assert!(!shared.command_buffers[0].reset_buffer);
    }

    #[test]
    fn send_and_receive_forward_to_mailbox() {
        let system = make_system();
        let sink = new_sink_handle(Box::new(NullSink::new("test")));
        let renderer = AudioRenderer::new(system, sink);

        renderer.send(Direction::Host, 0x1234);

        assert_eq!(renderer.receive(Direction::Host), 0x1234);
    }

    #[test]
    fn wait_records_render_time_for_active_command_buffers() {
        let system = make_system();
        let sink = new_sink_handle(Box::new(NullSink::new("test")));
        let mut renderer = AudioRenderer::new(system, sink);
        renderer.start();
        let mut mix_buffer = vec![11i32; 4];
        let bytes = serialize_commands(
            &[Command::ClearMixBuffer(ClearMixBufferCommand {
                buffer_count: 2,
            })],
            1,
            mix_buffer.as_mut_ptr() as usize,
            2,
        );

        renderer.set_command_buffer(
            0,
            bytes.as_ptr() as usize,
            bytes.len() as u64,
            1000,
            1,
            std::ptr::null_mut(),
            false,
        );
        renderer.signal();
        renderer.wait();

        assert_eq!(renderer.get_remain_command_count(0), 0);
        assert!(renderer.shared.lock().command_buffers[0].render_time_taken_us <= 1_000_000);
    }

    #[test]
    fn wait_clears_command_buffers_even_on_unexpected_response() {
        let system = make_system();
        let sink = new_sink_handle(Box::new(NullSink::new("test")));
        let mut renderer = AudioRenderer::new(system, sink);
        renderer.start();

        renderer.set_command_buffer(0, 0x1234, 0x40, 1000, 1, std::ptr::null_mut(), true);
        renderer
            .mailbox
            .send(Direction::Host, Message::Shutdown as u32);

        renderer.wait();

        let shared = renderer.shared.lock();
        assert_eq!(shared.command_buffers[0].buffer, 0);
        assert_eq!(shared.command_buffers[0].size, 0);
        assert!(!shared.command_buffers[0].reset_buffer);
    }

    #[test]
    fn null_sink_reuses_its_single_paused_stream_like_upstream() {
        let system = make_system();
        let sink = new_sink_handle(Box::new(NullSink::new("test")));
        let mut renderer = AudioRenderer::new(system, sink);

        renderer.start();

        let shared = renderer.shared.lock();
        let stream0 = shared.streams[0].as_ref().unwrap().clone();
        let stream1 = shared.streams[1].as_ref().unwrap().clone();
        assert!(Arc::ptr_eq(&stream0, &stream1));
        assert!(stream0.is_paused());
        assert!(stream1.is_paused());
    }

    #[test]
    fn wait_processes_full_command_list_even_with_small_time_limit() {
        use crate::common::common::TARGET_SAMPLE_COUNT;

        let system = make_system();
        let sink = new_sink_handle(Box::new(NullSink::new("test")));
        let mut renderer = AudioRenderer::new(system.clone(), sink);
        renderer.start();

        let mut sample_buffer = vec![7i32; TARGET_SAMPLE_COUNT as usize * 2];
        let bytes = serialize_commands(
            &[
                Command::ClearMixBuffer(ClearMixBufferCommand { buffer_count: 2 }),
                Command::DeviceSink(DeviceSinkCommand {
                    name: [0; 0x100],
                    session_id: 0,
                    sample_buffer: sample_buffer.as_ptr() as usize,
                    sample_count: sample_buffer.len() as u64,
                    input_count: 2,
                    inputs: [0, 1, 0, 0, 0, 0],
                }),
            ],
            1,
            sample_buffer.as_mut_ptr() as usize,
            TARGET_SAMPLE_COUNT,
        );
        let written = bytes.len();

        renderer.set_command_buffer(
            0,
            bytes.as_ptr() as usize,
            written as u64,
            1,
            1,
            std::ptr::null_mut(),
            false,
        );
        renderer.signal();
        renderer.wait();
        assert_eq!(renderer.get_remain_command_count(0), 0);
    }

    #[test]
    fn stop_wakes_wait_free_space_and_joins_renderer_thread() {
        use crate::common::common::TARGET_SAMPLE_COUNT;

        let system = make_system();
        let sink = new_sink_handle(Box::new(NullSink::new("test")));
        let mut renderer = AudioRenderer::new(system, sink);
        renderer.start();

        let mut sample_buffer = vec![7i32; TARGET_SAMPLE_COUNT as usize * 2];
        let bytes = serialize_commands(
            &[Command::DeviceSink(DeviceSinkCommand {
                name: [0; 0x100],
                session_id: 0,
                sample_buffer: sample_buffer.as_ptr() as usize,
                sample_count: sample_buffer.len() as u64,
                input_count: 2,
                inputs: [0, 1, 0, 0, 0, 0],
            })],
            1,
            sample_buffer.as_mut_ptr() as usize,
            TARGET_SAMPLE_COUNT,
        );

        let stream = renderer.shared.lock().streams[0].as_ref().unwrap().clone();
        crate::sink::start_sink_stream(&stream, false);
        stream.set_ring_size(1);
        stream.append_buffer(
            SinkBuffer {
                frames: 2,
                frames_played: 0,
                tag: 1,
                consumed: false,
            },
            &[1, 2, 3, 4],
        );

        renderer.set_command_buffer(
            0,
            bytes.as_ptr() as usize,
            bytes.len() as u64,
            u64::MAX,
            1,
            std::ptr::null_mut(),
            false,
        );
        renderer.signal();

        std::thread::sleep(Duration::from_millis(20));

        let stop_finished = Arc::new(AtomicBool::new(false));
        std::thread::scope(|scope| {
            let stop_finished_thread = stop_finished.clone();
            scope.spawn(move || {
                renderer.stop();
                stop_finished_thread.store(true, Ordering::SeqCst);
            });

            std::thread::sleep(Duration::from_millis(20));
            assert!(stop_finished.load(Ordering::SeqCst));
        });
    }

    #[test]
    fn map_unmap_mailbox_messages_return_responses() {
        let system = make_system();
        let sink = new_sink_handle(Box::new(NullSink::new("test")));
        let mut renderer = AudioRenderer::new(system, sink);
        renderer.start();

        renderer
            .mailbox
            .send(Direction::Dsp, Message::MapUnmap_Map as u32);
        assert_eq!(
            renderer.mailbox.receive(Direction::Host),
            Message::MapUnmap_MapResponse as u32
        );

        renderer
            .mailbox
            .send(Direction::Dsp, Message::MapUnmap_Unmap as u32);
        assert_eq!(
            renderer.mailbox.receive(Direction::Host),
            Message::MapUnmap_UnmapResponse as u32
        );

        renderer
            .mailbox
            .send(Direction::Dsp, Message::MapUnmap_InvalidateCache as u32);
        assert_eq!(
            renderer.mailbox.receive(Direction::Host),
            Message::MapUnmap_InvalidateCacheResponse as u32
        );

        renderer
            .mailbox
            .send(Direction::Dsp, Message::MapUnmap_Shutdown as u32);
        assert_eq!(
            renderer.mailbox.receive(Direction::Host),
            Message::MapUnmap_ShutdownResponse as u32
        );

        renderer.signal();
        renderer.wait();
    }
}

use crate::adsp::apps::opus::shared_memory::{SharedMemory, SharedMemoryHandle};
use crate::adsp::apps::opus::{Direction, Message, OpusDecoder as AdspOpusDecoder};
use crate::errors::{
    RESULT_BUFFER_TOO_SMALL, RESULT_INVALID_OPUS_DSP_RETURN_CODE, RESULT_LIB_OPUS_ALLOC_FAIL,
    RESULT_LIB_OPUS_BAD_ARG, RESULT_LIB_OPUS_INTERNAL_ERROR, RESULT_LIB_OPUS_INVALID_PACKET,
    RESULT_LIB_OPUS_INVALID_STATE, RESULT_LIB_OPUS_UNIMPLEMENTED,
};
use crate::Result;
use common::alignment::align_up;
use common::ResultCode;
use parking_lot::Mutex;
use std::mem::size_of;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

struct AdspBackend {
    decoder: Arc<Mutex<AdspOpusDecoder>>,
    shared_memory: SharedMemoryHandle,
    next_buffer_id: AtomicU64,
}

pub struct HardwareOpus {
    backend: Arc<AdspBackend>,
    buffer_id: u64,
}

impl Clone for HardwareOpus {
    fn clone(&self) -> Self {
        let buffer_id = self
            .backend
            .next_buffer_id
            .fetch_add(0x10_0000, Ordering::Relaxed);
        Self {
            backend: self.backend.clone(),
            buffer_id,
        }
    }
}

/// Port of `ResultCodeFromLibOpusErrorCode` from upstream `hardware_opus.cpp`.
pub(crate) fn result_code_from_libopus_error_code(error_code: i32) -> Result {
    assert!(error_code <= 0);
    match error_code {
        -7 => RESULT_LIB_OPUS_ALLOC_FAIL,
        -6 => RESULT_LIB_OPUS_INVALID_STATE,
        -5 => RESULT_LIB_OPUS_UNIMPLEMENTED,
        -4 => RESULT_LIB_OPUS_INVALID_PACKET,
        -3 => RESULT_LIB_OPUS_INTERNAL_ERROR,
        -2 => RESULT_BUFFER_TOO_SMALL,
        -1 => RESULT_LIB_OPUS_BAD_ARG,
        0 => ResultCode::SUCCESS,
        _ => unreachable!("unexpected libopus error code {error_code}"),
    }
}

impl HardwareOpus {
    pub fn new_from_adsp(decoder: Arc<Mutex<AdspOpusDecoder>>) -> Self {
        let buffer_id = 0x1000;
        let shared_memory = Arc::new(Mutex::new(SharedMemory::new(0)));
        decoder.lock().set_shared_memory(shared_memory.clone());
        Self {
            backend: Arc::new(AdspBackend {
                decoder,
                shared_memory,
                next_buffer_id: AtomicU64::new(buffer_id + 0x10_0000),
            }),
            buffer_id,
        }
    }

    pub fn get_work_buffer_size(&self, channel: u32) -> u32 {
        let backend = self.backend.as_ref();
        let decoder = backend.decoder.lock();
        if !decoder.is_running() {
            return 0;
        }
        {
            let mut shared = backend.shared_memory.lock();
            shared.host_send_data[0] = channel as u64;
        }
        decoder.send(Direction::Dsp, Message::GetWorkBufferSize);
        let message = decoder.receive(Direction::Host);
        if message != Message::GetWorkBufferSizeOK {
            log::error!(
                "OpusDecoder returned invalid message. Expected {:?} got {:?}",
                Message::GetWorkBufferSizeOK,
                message
            );
            return 0;
        }
        backend.shared_memory.lock().dsp_return_data[0] as u32
    }

    pub fn get_work_buffer_size_for_multi_stream(
        &self,
        total_stream_count: u32,
        stereo_stream_count: u32,
    ) -> u32 {
        let backend = self.backend.as_ref();
        let decoder = backend.decoder.lock();
        {
            let mut shared = backend.shared_memory.lock();
            shared.host_send_data[0] = total_stream_count as u64;
            shared.host_send_data[1] = stereo_stream_count as u64;
        }
        decoder.send(Direction::Dsp, Message::GetWorkBufferSizeForMultiStream);
        let message = decoder.receive(Direction::Host);
        if message != Message::GetWorkBufferSizeForMultiStreamOK {
            log::error!(
                "OpusDecoder returned invalid message. Expected {:?} got {:?}",
                Message::GetWorkBufferSizeForMultiStreamOK,
                message
            );
            return 0;
        }
        backend.shared_memory.lock().dsp_return_data[0] as u32
    }

    pub fn initialize_decode_object(
        &self,
        sample_rate: u32,
        channel_count: u32,
        buffer_size: u64,
    ) -> Result {
        let backend = self.backend.as_ref();
        let decoder = backend.decoder.lock();
        {
            let mut shared = backend.shared_memory.lock();
            shared.resize_transfer_memory(required_workbuffer_region_size(
                self.buffer_id,
                buffer_size,
            ));
            shared.host_send_data[0] = self.buffer_id;
            shared.host_send_data[1] = buffer_size;
            shared.host_send_data[2] = sample_rate as u64;
            shared.host_send_data[3] = channel_count as u64;
        }
        decoder.send(Direction::Dsp, Message::InitializeDecodeObject);
        let message = decoder.receive(Direction::Host);
        if message != Message::InitializeDecodeObjectOK {
            log::error!(
                "OpusDecoder returned invalid message. Expected {:?} got {:?}",
                Message::InitializeDecodeObjectOK,
                message
            );
            return RESULT_INVALID_OPUS_DSP_RETURN_CODE;
        }
        let error_code = backend.shared_memory.lock().dsp_return_data[0] as i32;
        if std::env::var_os("RUZU_TRACE_HWOPUS_AUDIO").is_some() {
            eprintln!(
                "[HWOPUS_INIT] buffer=0x{:X} multi=false rate={} channels={} size=0x{:X} result={}",
                self.buffer_id, sample_rate, channel_count, buffer_size, error_code
            );
        }
        result_code_from_libopus_error_code(error_code)
    }

    pub fn initialize_multi_stream_decode_object(
        &self,
        sample_rate: u32,
        channel_count: u32,
        total_stream_count: u32,
        stereo_stream_count: u32,
        mappings: &[u8],
        buffer_size: u64,
    ) -> Result {
        let backend = self.backend.as_ref();
        let decoder = backend.decoder.lock();
        {
            let mut shared = backend.shared_memory.lock();
            shared.resize_transfer_memory(required_workbuffer_region_size(
                self.buffer_id,
                buffer_size,
            ));
            shared.host_send_data[0] = self.buffer_id;
            shared.host_send_data[1] = buffer_size;
            shared.host_send_data[2] = sample_rate as u64;
            shared.host_send_data[3] = channel_count as u64;
            shared.host_send_data[4] = total_stream_count as u64;
            shared.host_send_data[5] = stereo_stream_count as u64;
            assert_eq!(mappings.len(), channel_count as usize);
            assert!(channel_count as usize <= shared.channel_mapping.len());
            shared.channel_mapping[..mappings.len()].copy_from_slice(mappings);
        }
        decoder.send(Direction::Dsp, Message::InitializeMultiStreamDecodeObject);
        let message = decoder.receive(Direction::Host);
        if message != Message::InitializeMultiStreamDecodeObjectOK {
            log::error!(
                "OpusDecoder returned invalid message. Expected {:?} got {:?}",
                Message::InitializeMultiStreamDecodeObjectOK,
                message
            );
            return RESULT_INVALID_OPUS_DSP_RETURN_CODE;
        }
        let error_code = backend.shared_memory.lock().dsp_return_data[0] as i32;
        if std::env::var_os("RUZU_TRACE_HWOPUS_AUDIO").is_some() {
            eprintln!(
                "[HWOPUS_INIT] buffer=0x{:X} multi=true rate={} channels={} streams={}/{} mapping={:?} size=0x{:X} result={}",
                self.buffer_id,
                sample_rate,
                channel_count,
                total_stream_count,
                stereo_stream_count,
                mappings,
                buffer_size,
                error_code
            );
        }
        result_code_from_libopus_error_code(error_code)
    }

    pub fn shutdown_decode_object(&self, buffer_size: u64) -> Result {
        let backend = self.backend.as_ref();
        let decoder = backend.decoder.lock();
        {
            let mut shared = backend.shared_memory.lock();
            shared.host_send_data[0] = self.buffer_id;
            shared.host_send_data[1] = buffer_size;
        }
        decoder.send(Direction::Dsp, Message::ShutdownDecodeObject);
        let message = decoder.receive(Direction::Host);
        assert_eq!(message, Message::ShutdownDecodeObjectOK);
        result_code_from_libopus_error_code(backend.shared_memory.lock().dsp_return_data[0] as i32)
    }

    pub fn shutdown_multi_stream_decode_object(&self, buffer_size: u64) -> Result {
        let backend = self.backend.as_ref();
        let decoder = backend.decoder.lock();
        {
            let mut shared = backend.shared_memory.lock();
            shared.host_send_data[0] = self.buffer_id;
            shared.host_send_data[1] = buffer_size;
        }
        decoder.send(Direction::Dsp, Message::ShutdownMultiStreamDecodeObject);
        let message = decoder.receive(Direction::Host);
        assert_eq!(message, Message::ShutdownMultiStreamDecodeObjectOK);
        result_code_from_libopus_error_code(backend.shared_memory.lock().dsp_return_data[0] as i32)
    }

    pub fn decode_interleaved(
        &self,
        out_sample_count: &mut u32,
        output_data: &mut [u8],
        channel_count: u32,
        input_data: &[u8],
        out_time_taken: &mut u64,
        reset: bool,
    ) -> Result {
        decode_interleaved_adsp(
            self.backend.as_ref(),
            self.buffer_id,
            out_sample_count,
            output_data,
            channel_count,
            input_data,
            out_time_taken,
            reset,
            false,
        )
    }

    pub fn decode_interleaved_for_multi_stream(
        &self,
        out_sample_count: &mut u32,
        output_data: &mut [u8],
        channel_count: u32,
        input_data: &[u8],
        out_time_taken: &mut u64,
        reset: bool,
    ) -> Result {
        decode_interleaved_adsp(
            self.backend.as_ref(),
            self.buffer_id,
            out_sample_count,
            output_data,
            channel_count,
            input_data,
            out_time_taken,
            reset,
            true,
        )
    }

    pub fn map_memory(&self, buffer_size: u64) -> Result {
        let backend = self.backend.as_ref();
        let decoder = backend.decoder.lock();
        {
            let mut shared = backend.shared_memory.lock();
            shared.host_send_data[0] = self.buffer_id;
            shared.host_send_data[1] = buffer_size;
        }
        decoder.send(Direction::Dsp, Message::MapMemory);
        let message = decoder.receive(Direction::Host);
        if message != Message::MapMemoryOK {
            log::error!(
                "OpusDecoder returned invalid message. Expected {:?} got {:?}",
                Message::MapMemoryOK,
                message
            );
            return RESULT_INVALID_OPUS_DSP_RETURN_CODE;
        }
        ResultCode::SUCCESS
    }

    pub fn unmap_memory(&self, buffer_size: u64) -> Result {
        let backend = self.backend.as_ref();
        let decoder = backend.decoder.lock();
        {
            let mut shared = backend.shared_memory.lock();
            shared.host_send_data[0] = self.buffer_id;
            shared.host_send_data[1] = buffer_size;
        }
        decoder.send(Direction::Dsp, Message::UnmapMemory);
        let message = decoder.receive(Direction::Host);
        if message != Message::UnmapMemoryOK {
            log::error!(
                "OpusDecoder returned invalid message. Expected {:?} got {:?}",
                Message::UnmapMemoryOK,
                message
            );
            return RESULT_INVALID_OPUS_DSP_RETURN_CODE;
        }
        ResultCode::SUCCESS
    }
}

fn decode_interleaved_adsp(
    backend: &AdspBackend,
    buffer_id: u64,
    out_sample_count: &mut u32,
    output_data: &mut [u8],
    channel_count: u32,
    input_data: &[u8],
    out_time_taken: &mut u64,
    reset: bool,
    multi_stream: bool,
) -> Result {
    let input_offset = 0x40usize;
    let output_offset = align_up(input_offset.wrapping_add(input_data.len()) as u64, 0x40) as usize;

    let decoder = backend.decoder.lock();
    {
        let mut shared = backend.shared_memory.lock();
        let required_transfer_size = shared
            .transfer_memory()
            .len()
            .max(output_offset.wrapping_add(output_data.len()));
        shared.resize_transfer_memory(required_transfer_size);
        let _ = shared.write_transfer(input_offset, input_data);
        shared.host_send_data[0] = buffer_id;
        shared.host_send_data[1] = input_offset as u64;
        shared.host_send_data[2] = input_data.len() as u64;
        shared.host_send_data[3] = output_offset as u64;
        shared.host_send_data[4] = output_data.len() as u64;
        shared.host_send_data[5] = 0;
        shared.host_send_data[6] = reset as u64;
    }
    decoder.send(
        Direction::Dsp,
        if multi_stream {
            Message::DecodeInterleavedForMultiStream
        } else {
            Message::DecodeInterleaved
        },
    );
    let expected = if multi_stream {
        Message::DecodeInterleavedForMultiStreamOK
    } else {
        Message::DecodeInterleavedOK
    };
    let message = decoder.receive(Direction::Host);
    if message != expected {
        log::error!(
            "OpusDecoder returned invalid message. Expected {:?} got {:?}",
            expected,
            message
        );
        return RESULT_INVALID_OPUS_DSP_RETURN_CODE;
    }

    let shared = backend.shared_memory.lock();
    let result = result_code_from_libopus_error_code(shared.dsp_return_data[0] as i32);
    if result.is_error() {
        return result;
    }
    *out_sample_count = shared.dsp_return_data[1] as u32;
    *out_time_taken = shared.dsp_return_data[2].wrapping_mul(1000);
    let output_size = (*out_sample_count as usize)
        .wrapping_mul(channel_count as usize)
        .wrapping_mul(size_of::<i16>());
    if output_size > output_data.len() {
        return RESULT_BUFFER_TOO_SMALL;
    }
    let Some(output) = shared.read_transfer(output_offset, output_size) else {
        return RESULT_BUFFER_TOO_SMALL;
    };
    output_data[..output_size].copy_from_slice(output);
    if std::env::var_os("RUZU_TRACE_HWOPUS_AUDIO").is_some() {
        static TRACE_COUNT: AtomicU64 = AtomicU64::new(0);
        let trace_index = TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
        if trace_index < 512 || trace_index.is_power_of_two() {
            let mut peaks = vec![0u16; channel_count as usize];
            for (sample_index, bytes) in output.chunks_exact(2).enumerate() {
                let channel = sample_index % channel_count as usize;
                peaks[channel] =
                    peaks[channel].max(i16::from_le_bytes([bytes[0], bytes[1]]).unsigned_abs());
            }
            eprintln!(
                "[HWOPUS_DECODE] #{} buffer=0x{:X} multi={} input={} head={:02X?} reset={} samples={} peaks={:?}",
                trace_index,
                buffer_id,
                multi_stream,
                input_data.len(),
                &input_data[..input_data.len().min(8)],
                reset,
                *out_sample_count,
                peaks
            );
        }
    }
    ResultCode::SUCCESS
}

fn required_workbuffer_region_size(buffer_id: u64, buffer_size: u64) -> usize {
    buffer_id.wrapping_add(buffer_size) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPUS_SILENCE_PACKET: [u8; 3] = [0xF8, 0xFF, 0xFE];

    #[test]
    fn adsp_backend_round_trips_basic_decode() {
        let decoder = Arc::new(Mutex::new(AdspOpusDecoder::new(crate::make_test_system())));
        {
            let decoder = decoder.lock();
            decoder.send(Direction::Dsp, Message::Start);
            assert_eq!(decoder.receive(Direction::Host), Message::StartOK);
        }

        let opus = HardwareOpus::new_from_adsp(decoder);
        let mut out_samples = 0;
        let mut out_time = 0;
        let mut output = vec![0; 0x2000];

        assert_eq!(
            opus.get_work_buffer_size(2),
            crate::adsp::apps::opus::opus_decode_object::OpusDecodeObject::get_work_buffer_size(2)
        );
        assert_eq!(
            opus.initialize_decode_object(48_000, 2, 0x10000),
            ResultCode::SUCCESS
        );
        assert_eq!(
            opus.decode_interleaved(
                &mut out_samples,
                &mut output,
                2,
                &OPUS_SILENCE_PACKET,
                &mut out_time,
                false,
            ),
            ResultCode::SUCCESS
        );
        assert!(out_samples > 0);
    }

    #[test]
    fn cloned_session_adapters_keep_decode_objects_independent() {
        let decoder = Arc::new(Mutex::new(AdspOpusDecoder::new(crate::make_test_system())));
        {
            let decoder = decoder.lock();
            decoder.send(Direction::Dsp, Message::Start);
            assert_eq!(decoder.receive(Direction::Host), Message::StartOK);
        }

        let manager_hardware = HardwareOpus::new_from_adsp(decoder);
        let first = manager_hardware.clone();
        let second = manager_hardware.clone();
        let workbuffer_size = first.get_work_buffer_size(2) as u64;
        assert_eq!(
            first.initialize_decode_object(48_000, 2, workbuffer_size),
            ResultCode::SUCCESS
        );
        assert_eq!(
            second.initialize_decode_object(48_000, 2, workbuffer_size),
            ResultCode::SUCCESS
        );

        for opus in [&first, &second] {
            let mut out_samples = 0;
            let mut out_time = 0;
            let mut output = vec![0; 0x2000];
            assert_eq!(
                opus.decode_interleaved(
                    &mut out_samples,
                    &mut output,
                    2,
                    &OPUS_SILENCE_PACKET,
                    &mut out_time,
                    false,
                ),
                ResultCode::SUCCESS
            );
            assert!(out_samples > 0);
        }
    }

    #[test]
    fn shutdown_response_is_consumed_before_the_next_command() {
        let decoder = Arc::new(Mutex::new(AdspOpusDecoder::new(crate::make_test_system())));
        {
            let decoder = decoder.lock();
            decoder.send(Direction::Dsp, Message::Start);
            assert_eq!(decoder.receive(Direction::Host), Message::StartOK);
        }

        let opus = HardwareOpus::new_from_adsp(decoder);
        let workbuffer_size = opus.get_work_buffer_size(2) as u64;
        assert_eq!(
            opus.initialize_decode_object(48_000, 2, workbuffer_size),
            ResultCode::SUCCESS
        );
        assert_eq!(
            opus.shutdown_decode_object(workbuffer_size),
            ResultCode::SUCCESS
        );

        assert_eq!(
            opus.get_work_buffer_size(2),
            crate::adsp::apps::opus::opus_decode_object::OpusDecodeObject::get_work_buffer_size(2)
        );
    }
}

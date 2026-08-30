use crate::adsp::apps::audio_renderer::command_list_processor::MemoryHandle;
use crate::common::common::{CpuAddr, SampleFormat, SrcQuality};
use crate::common::wave_buffer::WaveBufferVersion2;
use crate::renderer::command::resample::resample;
use crate::renderer::voice::voice_state::AdpcmContext;
use crate::renderer::voice::VoiceState;
use common::fixed_point::FixedPoint;

const TEMP_BUFFER_SIZE: usize = 0x3F00;
const PITCH_BY_SRC_QUALITY: [u8; 3] = [4, 8, 4];
const ADPCM_SAMPLES_PER_FRAME: u32 = 14;
const ADPCM_NIBBLES_PER_FRAME: u32 = 16;
const ADPCM_STEPS: [i32; 16] = [0, 1, 2, 3, 4, 5, 6, 7, -8, -7, -6, -5, -4, -3, -2, -1];

#[derive(Debug, Clone, Copy)]
pub struct DataSourceCommand {
    pub src_quality: SrcQuality,
    pub output_index: i16,
    pub flags: u16,
    pub sample_rate: u32,
    pub pitch: f32,
    pub channel_index: i8,
    pub channel_count: i8,
    pub wave_buffers: [WaveBufferVersion2; 4],
    pub voice_state: CpuAddr,
    pub data_address: CpuAddr,
    pub data_size: u64,
}

pub struct DecodeFromWaveBuffersArgs<'a> {
    pub sample_format: SampleFormat,
    pub output: &'a mut [i32],
    pub voice_state: &'static mut VoiceState,
    pub wave_buffers: &'a [WaveBufferVersion2; 4],
    pub channel: i8,
    pub channel_count: i8,
    pub src_quality: SrcQuality,
    pub pitch: f32,
    pub source_sample_rate: u32,
    pub target_sample_rate: u32,
    pub sample_count: u32,
    pub data_address: CpuAddr,
    pub data_size: u64,
    pub is_voice_played_sample_count_reset_at_loop_point_supported: bool,
    pub is_voice_pitch_and_src_skipped_supported: bool,
}

struct DecodeArg<'a> {
    buffer: CpuAddr,
    buffer_size: u64,
    start_offset: u32,
    end_offset: u32,
    channel_count: i8,
    target_channel: i8,
    offset: u32,
    samples_to_read: u32,
    coefficients: [i16; 16],
    adpcm_context: Option<&'a mut AdpcmContext>,
}

pub fn mix_buffer_range(
    mix_buffers: &[i32],
    index: i16,
    sample_count: usize,
) -> Option<std::ops::Range<usize>> {
    if index < 0 {
        return None;
    }
    let start = (index as usize).checked_mul(sample_count)?;
    let end = start.checked_add(sample_count)?;
    if end > mix_buffers.len() {
        return None;
    }
    Some(start..end)
}

pub fn read_voice_state_mut(addr: CpuAddr) -> Option<&'static mut VoiceState> {
    if addr == 0 {
        return None;
    }
    crate::raw_write_trace::maybe_trace_write_at(
        "decode:voice_state_mut",
        addr,
        std::mem::size_of::<VoiceState>(),
    );
    Some(unsafe { &mut *(addr as *mut VoiceState) })
}

pub fn decode_from_wave_buffers(memory: &MemoryHandle, args: DecodeFromWaveBuffersArgs<'_>) {
    // RUZU_TRACE_DECODE: count + log entry into decode pipeline.
    // Used to diagnose "audio worker runs but voices never play" wedge.
    if std::env::var_os("RUZU_TRACE_DECODE").is_some() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNT: AtomicU64 = AtomicU64::new(0);
        let c = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        if c <= 10 || c % 1000 == 0 {
            log::info!(
                "decode_from_wave_buffers #{} sample_count={} src_rate={} tgt_rate={} pitch={} wb_count={}",
                c,
                args.sample_count,
                args.source_sample_rate,
                args.target_sample_rate,
                args.pitch,
                args.wave_buffers.len(),
            );
        }
    }
    let voice_state = args.voice_state;
    let trace_details = std::env::var_os("RUZU_TRACE_DECODE_DETAILS").is_some();
    if trace_details {
        use std::sync::atomic::{AtomicU64, Ordering};
        static DETAILS_COUNT: AtomicU64 = AtomicU64::new(0);
        let n = DETAILS_COUNT.fetch_add(1, Ordering::Relaxed);
        if n < 64 || n.is_power_of_two() {
            log::info!(
                "decode details #{} fmt={:?} out={} flags valid={:?} wb_index={} offset={} played={} consumed={} ch={}/{} sample_count={} src_rate={} tgt_rate={} pitch={}",
                n,
                args.sample_format,
                args.output.len(),
                voice_state.wave_buffer_valid,
                voice_state.wave_buffer_index,
                voice_state.offset,
                voice_state.played_sample_count,
                voice_state.wave_buffers_consumed,
                args.channel,
                args.channel_count,
                args.sample_count,
                args.source_sample_rate,
                args.target_sample_rate,
                args.pitch,
            );
            for (i, wb) in args.wave_buffers.iter().enumerate() {
                log::info!(
                    "decode details #{} wb{} buffer=0x{:X} size={} start={} end={} loop={} loop_start={} loop_end={} loop_count={} stream_ended={}",
                    n,
                    i,
                    wb.buffer,
                    wb.buffer_size,
                    wb.start_offset,
                    wb.end_offset,
                    wb.looping,
                    wb.loop_start_offset,
                    wb.loop_end_offset,
                    wb.loop_count,
                    wb.stream_ended,
                );
            }
        }
    }
    let mut remaining_sample_count = args.sample_count.min(args.output.len() as u32);
    let mut fraction = voice_state.fraction;

    if remaining_sample_count == 0 || args.source_sample_rate == 0 || args.target_sample_rate == 0 {
        return;
    }

    let sample_rate_ratio = FixedPoint::<49, 15>::from_f32(
        args.source_sample_rate as f32 / args.target_sample_rate as f32 * args.pitch,
    );
    if sample_rate_ratio <= FixedPoint::from_int(0) {
        return;
    }

    let size_required =
        fraction + FixedPoint::from_int(remaining_sample_count as i64) * sample_rate_ratio;
    if size_required < FixedPoint::from_int(0) {
        return;
    }

    let pitch = pitch_by_src_quality(args.src_quality);
    if pitch.saturating_add(size_required.to_int_floor().max(0) as usize) > TEMP_BUFFER_SIZE {
        return;
    }

    let mut max_remaining_sample_count =
        ((FixedPoint::from_int(TEMP_BUFFER_SIZE as i64) - fraction) / sample_rate_ratio)
            .to_uint_floor();
    max_remaining_sample_count = max_remaining_sample_count.min(remaining_sample_count);

    let mut wavebuffers_consumed = voice_state.wave_buffers_consumed;
    let mut wavebuffer_index = voice_state.wave_buffer_index as usize;
    let mut played_sample_count = voice_state.played_sample_count;
    let mut is_buffer_starved = false;
    let mut offset = voice_state.offset;
    let mut output_offset = 0usize;
    let mut temp_buffer = [0i16; TEMP_BUFFER_SIZE];

    while remaining_sample_count > 0 {
        let samples_to_write = remaining_sample_count.min(max_remaining_sample_count);
        let samples_to_read = (fraction
            + FixedPoint::from_int(samples_to_write as i64) * sample_rate_ratio)
            .to_uint_floor();

        let mut temp_buffer_pos = 0usize;
        if !args.is_voice_pitch_and_src_skipped_supported {
            temp_buffer[..pitch].copy_from_slice(&voice_state.sample_history[..pitch]);
            temp_buffer_pos = pitch;
        }

        let mut samples_read = 0u32;
        while samples_read < samples_to_read {
            if wavebuffer_index >= args.wave_buffers.len() {
                wavebuffer_index = 0;
                voice_state.wave_buffer_valid.fill(false);
                wavebuffers_consumed = args.wave_buffers.len() as u32;
            }

            if !voice_state.wave_buffer_valid[wavebuffer_index] {
                is_buffer_starved = true;
                break;
            }

            let wavebuffer = args.wave_buffers[wavebuffer_index];
            if offset == 0 && args.sample_format == SampleFormat::Adpcm && wavebuffer.context != 0 {
                load_adpcm_context(memory, &mut voice_state.adpcm_context, &wavebuffer);
            }

            let mut start_offset = wavebuffer.start_offset;
            let mut end_offset = wavebuffer.end_offset;
            if wavebuffer.looping
                && voice_state.loop_count > 0
                && wavebuffer.loop_start_offset <= wavebuffer.loop_end_offset
            {
                start_offset = wavebuffer.loop_start_offset;
                end_offset = wavebuffer.loop_end_offset;
            }

            let mut decode_arg = DecodeArg {
                buffer: wavebuffer.buffer,
                buffer_size: wavebuffer.buffer_size,
                start_offset,
                end_offset,
                channel_count: args.channel_count,
                target_channel: args.channel,
                offset,
                samples_to_read: samples_to_read - samples_read,
                coefficients: [0; 16],
                adpcm_context: None,
            };

            let samples_decoded = match args.sample_format {
                SampleFormat::PcmInt16 => {
                    decode_pcm_i16(memory, &mut temp_buffer[temp_buffer_pos..], &decode_arg)
                }
                SampleFormat::PcmFloat => {
                    decode_pcm_f32(memory, &mut temp_buffer[temp_buffer_pos..], &decode_arg)
                }
                SampleFormat::Adpcm => {
                    match read_adpcm_coefficients(memory, args.data_address, args.data_size) {
                        Some(coefficients) => {
                            decode_arg.coefficients = coefficients;
                            decode_arg.adpcm_context = Some(&mut voice_state.adpcm_context);
                            decode_adpcm(
                                memory,
                                &mut temp_buffer[temp_buffer_pos..],
                                &mut decode_arg,
                            )
                        }
                        None => 0,
                    }
                }
                _ => 0,
            };
            if trace_details && samples_decoded != 0 {
                use std::sync::atomic::{AtomicU64, Ordering};
                static DECODED_COUNT: AtomicU64 = AtomicU64::new(0);
                let n = DECODED_COUNT.fetch_add(1, Ordering::Relaxed);
                if n < 64 || n.is_power_of_two() {
                    let decoded =
                        &temp_buffer[temp_buffer_pos..temp_buffer_pos + samples_decoded as usize];
                    let min = decoded.iter().copied().min().unwrap_or(0);
                    let max = decoded.iter().copied().max().unwrap_or(0);
                    let nonzero = decoded.iter().filter(|&&sample| sample != 0).count();
                    log::info!(
                        "decode decoded #{} wb_index={} start={} end={} offset={} samples={} min={} max={} nonzero={}",
                        n,
                        wavebuffer_index,
                        start_offset,
                        end_offset,
                        offset,
                        samples_decoded,
                        min,
                        max,
                        nonzero,
                    );
                }
            }

            played_sample_count = played_sample_count.saturating_add(samples_decoded as u64);
            samples_read = samples_read.saturating_add(samples_decoded);
            temp_buffer_pos = temp_buffer_pos.saturating_add(samples_decoded as usize);
            offset = offset.saturating_add(samples_decoded);

            if samples_decoded != 0 && offset < end_offset.saturating_sub(start_offset) {
                continue;
            }

            offset = 0;
            if wavebuffer.looping {
                voice_state.loop_count += 1;
                if wavebuffer.loop_count >= 0
                    && (voice_state.loop_count > wavebuffer.loop_count || samples_decoded == 0)
                {
                    end_wave_buffer(
                        voice_state,
                        wavebuffer,
                        &mut wavebuffer_index,
                        &mut played_sample_count,
                        &mut wavebuffers_consumed,
                        args.wave_buffers.len(),
                    );
                }

                if samples_decoded == 0 {
                    is_buffer_starved = true;
                    break;
                }

                if args.is_voice_played_sample_count_reset_at_loop_point_supported {
                    played_sample_count = 0;
                }
            } else {
                end_wave_buffer(
                    voice_state,
                    wavebuffer,
                    &mut wavebuffer_index,
                    &mut played_sample_count,
                    &mut wavebuffers_consumed,
                    args.wave_buffers.len(),
                );
            }
        }

        let output_end = output_offset
            .saturating_add(samples_to_write as usize)
            .min(args.output.len());
        let output_chunk = &mut args.output[output_offset..output_end];
        if args.is_voice_pitch_and_src_skipped_supported {
            for (dst, src) in output_chunk
                .iter_mut()
                .zip(temp_buffer.iter().copied().take(samples_read as usize))
            {
                *dst = src as i32;
            }
            if output_chunk.len() > samples_read as usize {
                output_chunk[samples_read as usize..].fill(0);
            }
        } else {
            let zero_count = (samples_to_read - samples_read) as usize;
            temp_buffer[temp_buffer_pos..temp_buffer_pos + zero_count].fill(0);
            resample(
                output_chunk,
                &temp_buffer[..samples_to_read as usize + pitch],
                sample_rate_ratio,
                &mut fraction,
                samples_to_write,
                args.src_quality,
            );
            voice_state.sample_history[..pitch].copy_from_slice(
                &temp_buffer[samples_to_read as usize..samples_to_read as usize + pitch],
            );
        }

        remaining_sample_count -= samples_to_write;
        output_offset = output_end;
        if remaining_sample_count != 0 && is_buffer_starved {
            break;
        }
    }

    voice_state.wave_buffers_consumed = wavebuffers_consumed;
    voice_state.played_sample_count = played_sample_count;
    voice_state.wave_buffer_index = wavebuffer_index as u32;
    voice_state.offset = offset;
    voice_state.fraction = fraction;
}

fn pitch_by_src_quality(src_quality: SrcQuality) -> usize {
    PITCH_BY_SRC_QUALITY[src_quality as usize] as usize
}

fn load_adpcm_context(
    memory: &MemoryHandle,
    target: &mut AdpcmContext,
    wavebuffer: &WaveBufferVersion2,
) {
    if wavebuffer.context == 0
        || wavebuffer.context_size < std::mem::size_of::<AdpcmContext>() as u64
    {
        return;
    }

    let mut buf = [0u8; std::mem::size_of::<AdpcmContext>()];
    if !read_audio_bytes(memory, wavebuffer.context, &mut buf) {
        return;
    }
    // SAFETY: AdpcmContext is repr(C) plain-old-data; bytewise copy.
    unsafe {
        std::ptr::copy_nonoverlapping(
            buf.as_ptr(),
            target as *mut AdpcmContext as *mut u8,
            std::mem::size_of::<AdpcmContext>(),
        );
    }
}

fn read_adpcm_coefficients(
    memory: &MemoryHandle,
    data_address: CpuAddr,
    data_size: u64,
) -> Option<[i16; 16]> {
    if data_address == 0 || data_size < std::mem::size_of::<[i16; 16]>() as u64 {
        return None;
    }
    let mut buf = [0u8; std::mem::size_of::<[i16; 16]>()];
    if !read_audio_bytes(memory, data_address, &mut buf) {
        return None;
    }

    let mut out = [0i16; 16];
    for (i, chunk) in buf.chunks_exact(2).enumerate() {
        out[i] = i16::from_le_bytes([chunk[0], chunk[1]]);
    }
    Some(out)
}

fn read_audio_bytes(memory: &MemoryHandle, addr: CpuAddr, dest: &mut [u8]) -> bool {
    if addr == 0 {
        return false;
    }
    memory.read_block(addr as u64, dest)
}

fn adpcm_coeff_pair(coefficients: &[i16; 16], coeff_index: usize) -> Option<(i32, i32)> {
    let base = coeff_index.checked_mul(2)?;
    let coeff0 = *coefficients.get(base)? as i32;
    let coeff1 = *coefficients.get(base + 1)? as i32;
    Some((coeff0, coeff1))
}

fn safe_adpcm_coeff_pair(
    coefficients: &[i16; 16],
    header: u16,
    buffer: CpuAddr,
    read_addr: CpuAddr,
    read_size: usize,
    read_index: usize,
    position_in_frame: u32,
) -> (i32, i32, bool) {
    // The ADPCM predictor occupies only bits 4..=6. Bit 7 is ignored by
    // the upstream decoder rather than extending the coefficient index.
    let coeff_index = ((header >> 4) & 0x7) as usize;
    if let Some(pair) = adpcm_coeff_pair(coefficients, coeff_index) {
        return (pair.0, pair.1, true);
    }

    trace_bad_adpcm_header(
        header,
        coeff_index,
        buffer,
        read_addr,
        read_size,
        read_index,
        position_in_frame,
    );

    // Upstream assumes valid ADPCM data and always advances by
    // samples_to_process. Returning 0 here makes the same corrupt header
    // replay forever; decode silence for this frame instead.
    (0, 0, false)
}

fn trace_bad_adpcm_header(
    header: u16,
    coeff_index: usize,
    buffer: CpuAddr,
    read_addr: CpuAddr,
    read_size: usize,
    read_index: usize,
    position_in_frame: u32,
) {
    if std::env::var_os("RUZU_TRACE_AUDIO_ADPCM_BAD_HEADER").is_none() {
        return;
    }
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let count = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if count <= 32 || count.is_power_of_two() {
        log::warn!(
            "audio ADPCM invalid header #{} header=0x{:02X} coeff_index={} buffer=0x{:X} read_addr=0x{:X} read_size={} read_index={} position_in_frame={}",
            count,
            header,
            coeff_index,
            buffer,
            read_addr,
            read_size,
            read_index,
            position_in_frame,
        );
    }
}

fn decode_pcm_i16(memory: &MemoryHandle, output: &mut [i16], req: &DecodeArg<'_>) -> u32 {
    decode_pcm(
        output,
        req,
        |addr, sample_index| {
            let mut buf = [0u8; 2];
            let byte_offset = (sample_index * std::mem::size_of::<i16>()) as CpuAddr;
            if !read_audio_bytes(memory, addr + byte_offset, &mut buf) {
                return 0;
            }
            i16::from_le_bytes(buf)
        },
        std::mem::size_of::<i16>(),
    )
}

fn decode_pcm_f32(memory: &MemoryHandle, output: &mut [i16], req: &DecodeArg<'_>) -> u32 {
    decode_pcm(
        output,
        req,
        |addr, sample_index| {
            let mut buf = [0u8; 4];
            let byte_offset = (sample_index * std::mem::size_of::<f32>()) as CpuAddr;
            if !read_audio_bytes(memory, addr + byte_offset, &mut buf) {
                return 0;
            }
            let sample = f32::from_le_bytes(buf);
            (sample * i16::MAX as f32).clamp(i16::MIN as f32, i16::MAX as f32) as i16
        },
        std::mem::size_of::<f32>(),
    )
}

fn decode_pcm(
    output: &mut [i16],
    req: &DecodeArg<'_>,
    mut read_sample: impl FnMut(CpuAddr, usize) -> i16,
    sample_size: usize,
) -> u32 {
    if req.buffer == 0 || req.buffer_size == 0 || req.start_offset >= req.end_offset {
        return 0;
    }

    let samples_to_decode = req
        .samples_to_read
        .min(
            req.end_offset
                .saturating_sub(req.start_offset)
                .saturating_sub(req.offset),
        )
        .min(output.len() as u32);
    if samples_to_decode == 0 {
        return 0;
    }

    let channel_count = req.channel_count.max(1) as usize;
    let Some(target_channel) = usize::try_from(req.target_channel).ok() else {
        return 0;
    };
    if target_channel >= channel_count {
        return 0;
    }

    let sample_capacity = (req.buffer_size as usize) / sample_size.max(1);
    let base_sample = (req.start_offset + req.offset) as usize;
    let mut decoded = 0usize;
    for i in 0..samples_to_decode as usize {
        let sample_index = if channel_count == 1 {
            base_sample + i
        } else {
            (base_sample + i) * channel_count + target_channel
        };
        if sample_index >= sample_capacity {
            break;
        }
        output[i] = read_sample(req.buffer, sample_index);
        decoded += 1;
    }

    decoded as u32
}

fn decode_adpcm(memory: &MemoryHandle, output: &mut [i16], req: &mut DecodeArg<'_>) -> u32 {
    let Some(context) = req.adpcm_context.as_deref_mut() else {
        return 0;
    };

    if req.buffer == 0 || req.buffer_size == 0 || req.end_offset < req.start_offset {
        return 0;
    }

    let mut end = (req.end_offset % ADPCM_SAMPLES_PER_FRAME)
        + ADPCM_NIBBLES_PER_FRAME * (req.end_offset / ADPCM_SAMPLES_PER_FRAME);
    if req.end_offset % ADPCM_SAMPLES_PER_FRAME != 0 {
        end += 3;
    } else {
        end += 1;
    }
    if req.buffer_size < (end / 2) as u64 {
        return 0;
    }

    let start_pos = req.start_offset + req.offset;
    let samples_to_process = req
        .end_offset
        .saturating_sub(start_pos)
        .min(req.samples_to_read)
        .min(output.len() as u32);
    if samples_to_process == 0 {
        return 0;
    }

    let mut samples_to_read = samples_to_process;
    let samples_remaining_in_frame = start_pos % ADPCM_SAMPLES_PER_FRAME;
    let mut position_in_frame = (start_pos / ADPCM_SAMPLES_PER_FRAME) * ADPCM_NIBBLES_PER_FRAME
        + samples_remaining_in_frame;
    if samples_remaining_in_frame != 0 {
        position_in_frame += 2;
    }

    let size = ((samples_to_process / 8) * ADPCM_SAMPLES_PER_FRAME).max(8) as usize;
    let read_size = size.min(req.buffer_size as usize);
    let read_addr = req.buffer + (position_in_frame / 2) as usize;
    let mut wavebuffer = vec![0u8; read_size];
    if !read_audio_bytes(memory, read_addr, &mut wavebuffer) {
        return 0;
    }

    let mut header = context.header;
    let mut scale = (header & 0xF) as i32;
    let (mut coeff0, mut coeff1, coeff_valid) = safe_adpcm_coeff_pair(
        &req.coefficients,
        header,
        req.buffer,
        read_addr,
        read_size,
        0,
        position_in_frame,
    );
    if !coeff_valid {
        scale = 0;
    }
    let mut yn0 = context.yn0 as i32;
    let mut yn1 = context.yn1 as i32;
    let mut read_index = 0usize;
    let mut write_index = 0usize;

    let decode_sample =
        |code: i32, yn0: &mut i32, yn1: &mut i32, coeff0: i32, coeff1: i32, scale: i32| {
            let xn = code * (1 << scale);
            let prediction = coeff0 * *yn0 + coeff1 * *yn1;
            let sample = ((xn << 11) + 0x400 + prediction) >> 11;
            let saturated = sample.clamp(-0x8000, 0x7FFF);
            *yn1 = *yn0;
            *yn0 = saturated;
            saturated as i16
        };

    while samples_to_read > 0 && write_index < output.len() {
        if (position_in_frame % ADPCM_NIBBLES_PER_FRAME) == 0 {
            if read_index >= wavebuffer.len() {
                break;
            }
            header = wavebuffer[read_index] as u16;
            read_index += 1;
            scale = (header & 0xF) as i32;
            let (next_coeff0, next_coeff1, coeff_valid) = safe_adpcm_coeff_pair(
                &req.coefficients,
                header,
                req.buffer,
                read_addr,
                read_size,
                read_index.saturating_sub(1),
                position_in_frame,
            );
            coeff0 = next_coeff0;
            coeff1 = next_coeff1;
            if !coeff_valid {
                scale = 0;
            }
            position_in_frame += 2;

            if samples_to_read >= ADPCM_SAMPLES_PER_FRAME {
                for _ in 0..(ADPCM_SAMPLES_PER_FRAME / 2) {
                    if read_index >= wavebuffer.len() || write_index + 1 >= output.len() {
                        break;
                    }
                    let byte = wavebuffer[read_index];
                    read_index += 1;
                    let code0 = ADPCM_STEPS[((byte >> 4) & 0xF) as usize];
                    let code1 = ADPCM_STEPS[(byte & 0xF) as usize];
                    output[write_index] =
                        decode_sample(code0, &mut yn0, &mut yn1, coeff0, coeff1, scale);
                    output[write_index + 1] =
                        decode_sample(code1, &mut yn0, &mut yn1, coeff0, coeff1, scale);
                    write_index += 2;
                }
                position_in_frame += ADPCM_SAMPLES_PER_FRAME;
                samples_to_read = samples_to_read.saturating_sub(ADPCM_SAMPLES_PER_FRAME);
                continue;
            }
        }

        if read_index >= wavebuffer.len() {
            break;
        }
        let mut code = wavebuffer[read_index];
        if (position_in_frame & 1) != 0 {
            code &= 0xF;
            read_index += 1;
        } else {
            code >>= 4;
        }

        output[write_index] = decode_sample(
            ADPCM_STEPS[code as usize],
            &mut yn0,
            &mut yn1,
            coeff0,
            coeff1,
            scale,
        );
        write_index += 1;
        position_in_frame += 1;
        samples_to_read -= 1;
    }

    context.header = header;
    context.yn0 = yn0 as i16;
    context.yn1 = yn1 as i16;
    samples_to_process
}

fn end_wave_buffer(
    voice_state: &mut VoiceState,
    wavebuffer: WaveBufferVersion2,
    index: &mut usize,
    played_samples: &mut u64,
    consumed: &mut u32,
    wave_buffer_count: usize,
) {
    voice_state.wave_buffer_valid[*index] = false;
    voice_state.loop_count = 0;
    if wavebuffer.stream_ended {
        *played_samples = 0;
    }
    *index = (*index + 1) % wave_buffer_count;
    *consumed = consumed.saturating_add(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_pcm_resamples_and_updates_voice_history() {
        let samples = [100i16, 200, 300, 400, 500, 600, 700, 800];
        let wave_buffers = [
            WaveBufferVersion2 {
                buffer: samples.as_ptr() as CpuAddr,
                buffer_size: std::mem::size_of_val(&samples) as u64,
                start_offset: 0,
                end_offset: samples.len() as u32,
                ..Default::default()
            },
            Default::default(),
            Default::default(),
            Default::default(),
        ];
        let mut output = [0i32; 4];
        let sample_count = output.len() as u32;
        let mut voice_state = VoiceState {
            wave_buffer_valid: [true, false, false, false],
            sample_history: [11, 22, 33, 44, 0, 0, 0, 0],
            ..Default::default()
        };

        decode_from_wave_buffers(
            &MemoryHandle::default(),
            DecodeFromWaveBuffersArgs {
                sample_format: SampleFormat::PcmInt16,
                output: &mut output,
                voice_state: unsafe { &mut *(&mut voice_state as *mut VoiceState) },
                wave_buffers: &wave_buffers,
                channel: 0,
                channel_count: 1,
                src_quality: SrcQuality::Medium,
                pitch: 1.0,
                source_sample_rate: 48_000,
                target_sample_rate: 24_000,
                sample_count,
                data_address: 0,
                data_size: 0,
                is_voice_played_sample_count_reset_at_loop_point_supported: false,
                is_voice_pitch_and_src_skipped_supported: false,
            },
        );

        assert_eq!(output, [22, 53, 200, 400]);
        assert_eq!(voice_state.sample_history[..4], [500, 600, 700, 800]);
        assert_eq!(voice_state.played_sample_count, 8);
        assert_eq!(voice_state.offset, 0);
        assert_eq!(voice_state.wave_buffers_consumed, 1);
        assert!(!voice_state.wave_buffer_valid[0]);
    }

    #[test]
    fn decode_pcm_rejects_negative_target_channel() {
        let samples = [100i16, 200, 300, 400];
        let req = DecodeArg {
            buffer: samples.as_ptr() as CpuAddr,
            buffer_size: std::mem::size_of_val(&samples) as u64,
            start_offset: 0,
            end_offset: 2,
            channel_count: 2,
            target_channel: -1,
            offset: 0,
            samples_to_read: 2,
            coefficients: [0; 16],
            adpcm_context: None,
        };
        let mut output = [0i16; 2];

        let decoded = decode_pcm_i16(&MemoryHandle::default(), &mut output, &req);

        assert_eq!(decoded, 0);
        assert_eq!(output, [0, 0]);
    }

    #[test]
    fn adpcm_predictor_ignores_header_bit_seven_like_upstream() {
        let coefficients = [
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 1234, -5678,
        ];

        let (coeff0, coeff1, valid) =
            safe_adpcm_coeff_pair(&coefficients, 0xF0, 0, 0, 0, 0, 0);

        assert!(valid);
        assert_eq!((coeff0, coeff1), (1234, -5678));
    }
}

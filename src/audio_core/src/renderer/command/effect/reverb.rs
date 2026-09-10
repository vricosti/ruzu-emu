use crate::common::common::{CpuAddr, MAX_CHANNELS};
use crate::renderer::command::util::write_copy;
use crate::renderer::effect::effect_info_base::ParameterState;
use crate::renderer::effect::reverb as effect_reverb;
use std::fmt::Write;

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ReverbPayload {
    pub inputs: [i16; MAX_CHANNELS],
    pub outputs: [i16; MAX_CHANNELS],
    pub parameter: effect_reverb::ParameterVersion2,
    pub state: CpuAddr,
    pub workbuffer: CpuAddr,
    pub effect_enabled: bool,
    pub long_size_pre_delay_supported: bool,
    pub _padding0: [u8; 6],
}

#[derive(Debug, Clone, Copy)]
pub struct ReverbCommand {
    pub inputs: [i16; MAX_CHANNELS],
    pub outputs: [i16; MAX_CHANNELS],
    pub parameter: effect_reverb::ParameterVersion2,
    pub state: CpuAddr,
    pub workbuffer: CpuAddr,
    pub effect_enabled: bool,
    pub long_size_pre_delay_supported: bool,
}

impl ReverbPayload {
    pub fn process(&self, mix_buffers: &mut [i32], sample_count: usize) {
        process_reverb_command(self, mix_buffers, sample_count);
    }

    pub fn verify(&self) -> bool {
        verify_reverb_command(self)
    }

    pub fn dump(&self, dump: &mut String) {
        dump_reverb_command(self, dump);
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ReverbState {
    pub early_delay_times: [u32; 10],
    pub early_gains: [f32; 10],
    pub pre_delay_time: u32,
    pub fdn_delay_samples: [u32; 4],
    pub decay_delay_samples: [u32; 4],
    pub fdn_positions: [u32; 4],
    pub decay_positions: [u32; 4],
    pub prev_feedback_output: [f32; 4],
    pub hf_decay_prev_gain: [f32; 4],
    pub hf_decay_gain: [f32; 4],
    pub decay_feedback: [f32; 4],
    pub pre_delay_pos: u32,
    pub pre_delay_len: u32,
    pub center_delay_pos: u32,
    pub center_delay_len: u32,
    pub _padding0: [u8; 0x448],
}

impl Default for ReverbState {
    fn default() -> Self {
        Self {
            early_delay_times: [0; 10],
            early_gains: [0.0; 10],
            pre_delay_time: 0,
            fdn_delay_samples: [0; 4],
            decay_delay_samples: [0; 4],
            fdn_positions: [0; 4],
            decay_positions: [0; 4],
            prev_feedback_output: [0.0; 4],
            hf_decay_prev_gain: [0.0; 4],
            hf_decay_gain: [0.0; 4],
            decay_feedback: [0.0; 4],
            pre_delay_pos: 0,
            pre_delay_len: 0,
            center_delay_pos: 0,
            center_delay_len: 0,
            _padding0: [0; 0x448],
        }
    }
}

pub const FDN_MAX_DELAY_LINE_TIMES: [f32; 4] = [53.953_247, 79.192_566, 116.238_77, 170.615_3];
pub const DECAY_MAX_DELAY_LINE_TIMES: [f32; 4] = [7.0, 9.0, 13.0, 17.0];
pub const EARLY_DELAY_TIMES: [[f32; 11]; 5] = [
    [
        0.0, 3.5, 2.799_988, 3.899_963, 2.699_951, 13.399_963, 7.899_963, 8.399_963, 9.899_963,
        12.0, 12.5,
    ],
    [
        0.0, 11.799_988, 5.5, 11.199_951, 10.399_963, 38.099_976, 22.199_951, 29.599_976,
        21.199_951, 24.799_988, 40.0,
    ],
    [
        0.0, 41.5, 20.5, 41.299_988, 0.0, 29.5, 33.799_988, 45.199_95, 46.799_988, 0.0, 50.0,
    ],
    [
        33.099_976, 43.299_988, 22.799_988, 37.899_963, 14.899_963, 35.299_988, 17.899_963,
        34.199_95, 0.0, 43.299_988, 50.0,
    ],
    [0.0; 11],
];
pub const EARLY_DELAY_GAINS: [[f32; 10]; 5] = [
    [
        0.699_951, 0.679_993, 0.699_951, 0.679_993, 0.699_951, 0.679_993, 0.699_951, 0.679_993,
        0.679_993, 0.679_993,
    ],
    [
        0.699_951, 0.679_993, 0.699_951, 0.679_993, 0.699_951, 0.679_993, 0.679_993, 0.679_993,
        0.679_993, 0.679_993,
    ],
    [
        0.5, 0.699_951, 0.699_951, 0.679_993, 0.5, 0.679_993, 0.679_993, 0.699_951, 0.679_993, 0.0,
    ],
    [
        0.929_993, 0.919_983, 0.869_995, 0.859_985, 0.939_941, 0.809_998, 0.799_988, 0.769_958,
        0.759_949, 0.649_963,
    ],
    [0.0; 10],
];
pub const FDN_DELAY_TIMES: [[f32; 4]; 5] = [
    [53.953_247, 79.192_566, 116.238_77, 130.615_3],
    [53.953_247, 79.192_566, 116.238_77, 170.615_3],
    [5.0, 10.0, 5.0, 10.0],
    [47.029_968, 71.0, 103.0, 170.0],
    [53.953_247, 79.192_566, 116.238_77, 170.615_3],
];
pub const DECAY_DELAY_TIMES: [[f32; 4]; 5] = [
    [7.0, 9.0, 13.0, 17.0],
    [7.0, 9.0, 13.0, 17.0],
    [1.0, 1.0, 1.0, 1.0],
    [7.0, 7.0, 13.0, 9.0],
    [7.0, 9.0, 13.0, 17.0],
];

pub fn write_reverb_payload(cmd: &ReverbCommand, output: &mut [u8]) -> usize {
    let mut payload: ReverbPayload = unsafe { std::mem::zeroed() };
    payload.inputs = cmd.inputs;
    payload.outputs = cmd.outputs;
    payload.parameter = cmd.parameter;
    payload.state = cmd.state;
    payload.workbuffer = cmd.workbuffer;
    payload.effect_enabled = cmd.effect_enabled;
    payload.long_size_pre_delay_supported = cmd.long_size_pre_delay_supported;
    payload._padding0 = [0; 6];
    write_copy(&payload, output)
}

pub fn process_reverb_command(
    payload: &ReverbPayload,
    mix_buffers: &mut [i32],
    sample_count: usize,
) {
    let channel_count = payload.parameter.channel_count.max(0) as usize;
    if channel_count == 0 {
        return;
    }

    let Some(state) = read_reverb_state_mut(payload.state) else {
        return;
    };
    if payload.effect_enabled {
        match payload.parameter.state {
            ParameterState::Updating => {
                update_reverb_effect_parameter(
                    &payload.parameter,
                    state,
                    payload.long_size_pre_delay_supported,
                );
            }
            ParameterState::Initialized => {
                let total_samples = total_workbuffer_samples(state);
                let Some(workbuffer) = reverb_workbuffer_mut(payload.state, total_samples) else {
                    return;
                };
                initialize_reverb_effect(
                    &payload.parameter,
                    state,
                    workbuffer,
                    payload.long_size_pre_delay_supported,
                );
            }
            ParameterState::Updated => {}
        }
    }

    let total_samples = total_workbuffer_samples(state);
    let mut empty_workbuffer = [];
    let workbuffer =
        reverb_workbuffer_mut(payload.state, total_samples).unwrap_or(&mut empty_workbuffer);
    apply_reverb_effect(
        &payload.parameter,
        state,
        payload.effect_enabled,
        &payload.inputs,
        &payload.outputs,
        mix_buffers,
        sample_count,
        workbuffer,
    );
}

pub fn verify_reverb_command(_payload: &ReverbPayload) -> bool {
    true
}

pub fn dump_reverb_command(payload: &ReverbPayload, dump: &mut String) {
    let _ = write!(
        dump,
        "ReverbCommand\n\tenabled {} long_size_pre_delay_supported {}\n\tinputs: ",
        payload.effect_enabled, payload.long_size_pre_delay_supported
    );
    for input in &payload.inputs {
        let _ = write!(dump, "{:02X}, ", input);
    }
    let _ = write!(dump, "\n\toutputs: ");
    for output in &payload.outputs {
        let _ = write!(dump, "{:02X}, ", output);
    }
    let _ = writeln!(dump);
}

pub fn update_reverb_effect_parameter(
    parameter: &effect_reverb::ParameterVersion2,
    state: &mut ReverbState,
    long_size_pre_delay_supported: bool,
) {
    let sr_ms = parameter.sample_rate.max(1) as f32 / 1000.0;
    let early_mode = (parameter.early_mode as usize).min(EARLY_DELAY_TIMES.len() - 1);
    let late_mode = parameter
        .late_mode
        .max(0)
        .min((FDN_DELAY_TIMES.len() - 1) as i32) as usize;
    let pre_delay_ms = fixed_14_to_f32(parameter.pre_delay);

    for i in 0..10 {
        let early_delay =
            ((pre_delay_ms + EARLY_DELAY_TIMES[early_mode][i]) * sr_ms).floor() as u32;
        state.early_delay_times[i] = early_delay.min(state.pre_delay_len.saturating_sub(1)) + 1;
        state.early_gains[i] =
            fixed_14_to_f32(parameter.early_gain) * EARLY_DELAY_GAINS[early_mode][i];
    }
    if parameter.channel_count == 2 {
        state.early_gains[4] *= 0.5;
        state.early_gains[5] *= 0.5;
    }

    let pre_time = ((pre_delay_ms + EARLY_DELAY_TIMES[early_mode][10]) * sr_ms).floor() as u32;
    state.pre_delay_time = pre_time.min(state.pre_delay_len.saturating_sub(1));

    let colouration = fixed_14_to_f32(parameter.colouration);
    let decay_time = fixed_14_to_f32(parameter.decay_time).max(0.001);
    let hf_decay_ratio = fixed_14_to_f32(parameter.high_freq_decay_ratio).clamp(0.0, 1.0);
    let cos_term = (1280.0 / parameter.sample_rate.max(1) as f32).cos();

    for i in 0..4 {
        let fdn_delay = (FDN_DELAY_TIMES[late_mode][i] * sr_ms).floor() as u32;
        state.fdn_delay_samples[i] = fdn_delay.max(1);

        let decay_delay = (DECAY_DELAY_TIMES[late_mode][i] * sr_ms).floor() as u32;
        state.decay_delay_samples[i] = decay_delay.max(1);

        state.decay_feedback[i] = 0.599_975_6 * (1.0 - colouration);

        let a = -3.0 * (FDN_MAX_DELAY_LINE_TIMES[i] + DECAY_MAX_DELAY_LINE_TIMES[i]);
        let b = a / decay_time.max(0.001);
        if hf_decay_ratio > 0.994_934_1 {
            state.hf_decay_prev_gain[i] = 0.0;
            state.hf_decay_gain[i] = 0.707_092_3 * 10.0f32.powf(b / 1000.0);
        } else {
            let e = 10.0f32
                .powf((((1.0 / hf_decay_ratio.max(0.000_001)) - 1.0) * 2.0 / 100.0) * (b / 10.0));
            let f = 1.0 - e;
            let g = 2.0 - (cos_term * e * 2.0);
            let h = (g * g - (f * f * 4.0)).max(0.0).sqrt();
            let c = (g - h) / (f * 2.0);
            let d = 1.0 - c;
            state.hf_decay_prev_gain[i] = c;
            state.hf_decay_gain[i] = 10.0f32.powf(b / 1000.0) * d * 0.707_092_3;
        }
        state.prev_feedback_output[i] = 0.0;
    }

    let pre_delay_ms_max = if long_size_pre_delay_supported {
        350.0
    } else {
        150.0
    };
    state.pre_delay_len = ((pre_delay_ms_max * sr_ms).floor() as u32).max(1);
    state.center_delay_len = ((5.0 * sr_ms).floor() as u32).max(1);
}

pub fn total_workbuffer_samples(state: &ReverbState) -> usize {
    state.pre_delay_len as usize
        + state.center_delay_len as usize
        + state
            .fdn_delay_samples
            .iter()
            .map(|&v| v.max(1) as usize)
            .sum::<usize>()
        + state
            .decay_delay_samples
            .iter()
            .map(|&v| v.max(1) as usize)
            .sum::<usize>()
}

pub fn workbuffer_offsets(state: &ReverbState) -> (usize, usize, [usize; 4], [usize; 4], usize) {
    let pre = 0usize;
    let center = pre + state.pre_delay_len as usize;
    let mut current = center + state.center_delay_len as usize;
    let mut fdn = [0usize; 4];
    let mut decay = [0usize; 4];
    for i in 0..4 {
        fdn[i] = current;
        current = current.saturating_add(state.fdn_delay_samples[i].max(1) as usize);
    }
    for i in 0..4 {
        decay[i] = current;
        current = current.saturating_add(state.decay_delay_samples[i].max(1) as usize);
    }
    (pre, center, fdn, decay, current)
}

pub fn tap_indexes(channel_count: usize) -> &'static [u8] {
    match channel_count {
        1 => &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        2 => &[0, 0, 1, 1, 0, 1, 0, 0, 1, 1],
        4 => &[0, 0, 2, 2, 1, 3, 0, 0, 1, 1],
        6 => &[0, 0, 2, 2, 1, 5, 4, 4, 5, 5],
        _ => &[],
    }
}

pub fn initialize_reverb_effect(
    parameter: &effect_reverb::ParameterVersion2,
    state: &mut ReverbState,
    workbuffer: &mut [f32],
    long_size_pre_delay_supported: bool,
) {
    *state = ReverbState::default();

    let sr_ms = parameter.sample_rate.max(1) as f32 / 1000.0;
    state.pre_delay_len = ((if long_size_pre_delay_supported {
        350.0
    } else {
        150.0
    } * sr_ms)
        .floor() as u32)
        .max(1);
    state.center_delay_len = ((5.0 * sr_ms).floor() as u32).max(1);
    for i in 0..4 {
        state.fdn_delay_samples[i] = (FDN_MAX_DELAY_LINE_TIMES[i] * sr_ms).floor() as u32;
        state.decay_delay_samples[i] = (DECAY_MAX_DELAY_LINE_TIMES[i] * sr_ms).floor() as u32;
    }

    update_reverb_effect_parameter(parameter, state, long_size_pre_delay_supported);

    let total_samples = total_workbuffer_samples(state);
    workbuffer
        .get_mut(..total_samples)
        .into_iter()
        .flatten()
        .for_each(|sample| *sample = 0.0);
}

pub fn apply_reverb_effect_bypass(
    inputs: &[i16; MAX_CHANNELS as usize],
    outputs: &[i16; MAX_CHANNELS as usize],
    channel_count: usize,
    sample_count: usize,
    mix_buffers: &mut [i32],
) {
    for channel in 0..channel_count.min(MAX_CHANNELS as usize) {
        copy_mix_buffer(mix_buffers, sample_count, outputs[channel], inputs[channel]);
    }
}

pub fn apply_reverb_effect(
    parameter: &effect_reverb::ParameterVersion2,
    state: &mut ReverbState,
    enabled: bool,
    inputs: &[i16; MAX_CHANNELS as usize],
    outputs: &[i16; MAX_CHANNELS as usize],
    mix_buffers: &mut [i32],
    sample_count: usize,
    workbuffer: &mut [f32],
) {
    let channel_count = parameter.channel_count.max(0) as usize;
    let active_channels = channel_count.min(inputs.len()).min(outputs.len());
    if active_channels == 0 {
        return;
    }

    if !enabled || !matches!(active_channels, 1 | 2 | 4 | 6) {
        apply_reverb_effect_bypass(inputs, outputs, active_channels, sample_count, mix_buffers);
        return;
    }

    let total_samples = total_workbuffer_samples(state);
    let Some(workbuffer) = workbuffer.get_mut(..total_samples) else {
        apply_reverb_effect_bypass(inputs, outputs, active_channels, sample_count, mix_buffers);
        return;
    };

    let (pre_offset, center_offset, fdn_offsets, decay_offsets, _) = workbuffer_offsets(state);
    let tap_indexes = tap_indexes(active_channels);
    let dry_gain = fixed_14_to_f32(parameter.dry_gain);
    let wet_gain = fixed_14_to_f32(parameter.wet_gain);
    let base_gain = fixed_14_to_f32(parameter.base_gain);
    let late_gain = fixed_14_to_f32(parameter.late_gain);

    for sample_index in 0..sample_count {
        let mut early_outputs = [0.0f32; MAX_CHANNELS as usize];

        for early_tap in 0..10 {
            let sample = ring_tap(
                workbuffer,
                pre_offset,
                state.pre_delay_len as usize,
                state.pre_delay_pos,
                state.early_delay_times[early_tap],
            ) * state.early_gains[early_tap];
            let channel = tap_indexes[early_tap] as usize;
            early_outputs[channel] += sample;
            if active_channels == 6 {
                early_outputs[3] += sample;
            }
        }
        if active_channels == 6 {
            early_outputs[3] *= 0.2;
        }

        let mut input_sample = 0.0f32;
        for channel in 0..active_channels {
            input_sample +=
                mix_buffer_sample(mix_buffers, inputs[channel], sample_count, sample_index) as f32;
        }
        input_sample *= 64.0 * base_gain;
        let _ = ring_tick(
            workbuffer,
            pre_offset,
            state.pre_delay_len as usize,
            &mut state.pre_delay_pos,
            input_sample,
        );

        for i in 0..4 {
            let read = ring_tap(
                workbuffer,
                fdn_offsets[i],
                state.fdn_delay_samples[i].max(1) as usize,
                state.fdn_positions[i],
                0,
            );
            state.prev_feedback_output[i] = state.prev_feedback_output[i]
                * state.hf_decay_prev_gain[i]
                + read * state.hf_decay_gain[i];
        }

        let pre_delay_sample = ring_tap(
            workbuffer,
            pre_offset,
            state.pre_delay_len as usize,
            state.pre_delay_pos,
            state.pre_delay_time,
        ) * late_gain;

        let mix_matrix = [
            state.prev_feedback_output[2] + state.prev_feedback_output[1] + pre_delay_sample,
            -state.prev_feedback_output[0] - state.prev_feedback_output[3] + pre_delay_sample,
            state.prev_feedback_output[0] - state.prev_feedback_output[3] + pre_delay_sample,
            state.prev_feedback_output[1] - state.prev_feedback_output[2] + pre_delay_sample,
        ];

        let mut allpass = [0.0f32; 4];
        for i in 0..4 {
            allpass[i] = axfx2_all_pass_tick(
                workbuffer,
                decay_offsets[i],
                state.decay_delay_samples[i].max(1) as usize,
                &mut state.decay_positions[i],
                state.decay_feedback[i],
                fdn_offsets[i],
                state.fdn_delay_samples[i].max(1) as usize,
                &mut state.fdn_positions[i],
                mix_matrix[i],
            );
        }

        for channel in 0..active_channels {
            let in_sample =
                mix_buffer_sample(mix_buffers, inputs[channel], sample_count, sample_index) as f32
                    * dry_gain;
            let wet_sample = if active_channels == 6 {
                let mapped = match channel {
                    0 => allpass[0],
                    1 => allpass[1],
                    2 => allpass[2] - allpass[3],
                    3 => ring_tick(
                        workbuffer,
                        center_offset,
                        state.center_delay_len as usize,
                        &mut state.center_delay_pos,
                        allpass[3] * 0.5,
                    ),
                    4 => allpass[2],
                    5 => allpass[3],
                    _ => 0.0,
                };
                (early_outputs[channel] + mapped) * wet_gain / 64.0
            } else {
                (early_outputs[channel] + allpass[channel.min(3)]) * wet_gain / 64.0
            };
            set_mix_buffer_sample(
                mix_buffers,
                outputs[channel],
                sample_count,
                sample_index,
                (in_sample + wet_sample)
                    .clamp(i32::MIN as f32, i32::MAX as f32)
                    .round() as i32,
            );
        }
    }
}

fn axfx2_all_pass_tick(
    workbuffer: &mut [f32],
    decay_start: usize,
    decay_len: usize,
    decay_pos: &mut u32,
    decay_feedback: f32,
    fdn_start: usize,
    fdn_len: usize,
    fdn_pos: &mut u32,
    mix: f32,
) -> f32 {
    let value = ring_tap(workbuffer, decay_start, decay_len, *decay_pos, 0);
    let mixed = mix - value * decay_feedback;
    let out =
        ring_tick(workbuffer, decay_start, decay_len, decay_pos, mixed) + mixed * decay_feedback;
    let _ = ring_tick(workbuffer, fdn_start, fdn_len, fdn_pos, out);
    out
}

fn ring_tap(buffer: &[f32], start: usize, len: usize, pos: u32, delay: u32) -> f32 {
    if len == 0 {
        return 0.0;
    }
    let delay = delay as usize % len;
    let pos = pos as usize % len;
    let idx = (pos + len - delay) % len;
    buffer.get(start + idx).copied().unwrap_or(0.0)
}

fn ring_tick(buffer: &mut [f32], start: usize, len: usize, pos: &mut u32, value: f32) -> f32 {
    if len == 0 {
        return 0.0;
    }
    let idx = (*pos as usize) % len;
    let out = buffer[start + idx];
    buffer[start + idx] = value;
    *pos = ((idx + 1) % len) as u32;
    out
}

fn mix_buffer_sample(
    mix_buffers: &[i32],
    buffer_index: i16,
    sample_count: usize,
    sample_index: usize,
) -> i32 {
    if buffer_index < 0 {
        return 0;
    }
    let buffer_index = buffer_index as usize;
    mix_buffers
        .get(buffer_index * sample_count + sample_index)
        .copied()
        .unwrap_or(0)
}

fn set_mix_buffer_sample(
    mix_buffers: &mut [i32],
    buffer_index: i16,
    sample_count: usize,
    sample_index: usize,
    value: i32,
) {
    if buffer_index < 0 {
        return;
    }
    let buffer_index = buffer_index as usize;
    if let Some(sample) = mix_buffers.get_mut(buffer_index * sample_count + sample_index) {
        *sample = value;
    }
}

fn mix_buffer_range(
    mix_buffers: &[i32],
    buffer_index: i16,
    sample_count: usize,
) -> Option<std::ops::Range<usize>> {
    if buffer_index < 0 {
        return None;
    }
    let start = buffer_index as usize * sample_count;
    let end = start.saturating_add(sample_count);
    (end <= mix_buffers.len()).then_some(start..end)
}

fn copy_mix_buffer(
    mix_buffers: &mut [i32],
    sample_count: usize,
    output_index: i16,
    input_index: i16,
) {
    let Some(input_range) = mix_buffer_range(mix_buffers, input_index, sample_count) else {
        return;
    };
    let Some(output_range) = mix_buffer_range(mix_buffers, output_index, sample_count) else {
        return;
    };
    if input_range == output_range {
        return;
    }
    let input = mix_buffers[input_range].to_vec();
    mix_buffers[output_range].copy_from_slice(&input);
}

fn fixed_14_to_f32(value: i32) -> f32 {
    value as f32 / 16384.0
}

fn read_reverb_state_mut(addr: CpuAddr) -> Option<&'static mut ReverbState> {
    if addr == 0 {
        return None;
    }
    crate::raw_write_trace::maybe_trace_write_at(
        "reverb:state_mut",
        addr,
        std::mem::size_of::<ReverbState>(),
    );
    Some(unsafe { &mut *(addr as *mut ReverbState) })
}

fn reverb_workbuffers() -> &'static parking_lot::Mutex<std::collections::HashMap<usize, Vec<f32>>> {
    static BUFFERS: std::sync::OnceLock<
        parking_lot::Mutex<std::collections::HashMap<usize, Vec<f32>>>,
    > = std::sync::OnceLock::new();
    BUFFERS.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()))
}

/// Host-side delay-line storage for the reverb effect, keyed by the reverb
/// state address.
///
/// Upstream `ReverbCommand::Process` keeps the delay lines inside the
/// host-allocated `ReverbInfo::State` and leaves the game-supplied `workbuffer`
/// unused (reverb.cpp:202 documents it as "Game-supplied memory for the state.
/// (Unused)"). This port lays the ring buffers out in a flat buffer instead, but
/// that buffer must be host memory: the guest `workbuffer` address is a CPU
/// (guest) address, not a valid host pointer, so casting it and dereferencing it
/// crashes. We allocate the storage on the host heap, keyed by the (host) reverb
/// state address, mirroring how `delay.rs` owns its delay lines.
fn reverb_workbuffer_mut(state_addr: CpuAddr, total_samples: usize) -> Option<&'static mut [f32]> {
    if state_addr == 0 {
        return None;
    }
    let mut buffers = reverb_workbuffers().lock();
    let buffer = buffers.entry(state_addr as usize).or_default();
    if buffer.len() < total_samples {
        buffer.resize(total_samples, 0.0);
    }
    let ptr = buffer.as_mut_ptr();
    // SAFETY: the Vec is owned by the 'static map and is not reallocated while
    // this borrow is live — the reverb command for a given state runs on a single
    // audio thread, and any later `resize` happens on a subsequent call once this
    // borrow is gone. The returned slice covers the first `total_samples`
    // elements, all of which are initialized.
    Some(unsafe { std::slice::from_raw_parts_mut(ptr, total_samples) })
}

/// Frees the host-side reverb delay-line storage for a state address.
///
/// Wired from `EffectInfoBase::drop_owned_raw_state` so the buffer is released
/// when the effect's state buffer is torn down.
pub(crate) fn drop_reverb_workbuffer(state_addr: CpuAddr) {
    if state_addr == 0 {
        return;
    }
    reverb_workbuffers().lock().remove(&(state_addr as usize));
}

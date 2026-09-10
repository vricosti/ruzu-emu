use std::mem::size_of;

use crate::common::common::{CpuAddr, INVALID_PROCESS_ORDER, MAX_CHANNELS, UNUSED_MIX_ID};
use crate::renderer::behavior::ErrorInfo;
use crate::renderer::memory::{AddressInfo, PoolMapper};
use common::ResultCode;

use super::effect_result_state::EffectResultState;
use super::{
    aux_, biquad_filter, buffer_mixer, capture, compressor, delay, effect_reset, i3dl2,
    light_limiter, reverb,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EffectType {
    Invalid = 0,
    Mix = 1,
    Aux = 2,
    Delay = 3,
    Reverb = 4,
    I3dl2Reverb = 5,
    BiquadFilter = 6,
    LightLimiter = 7,
    Capture = 8,
    Compressor = 9,
}

impl Default for EffectType {
    fn default() -> Self {
        Self::Invalid
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageState {
    Invalid,
    New,
    Enabled,
    Disabled,
}

impl Default for UsageState {
    fn default() -> Self {
        Self::Invalid
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OutStatus {
    Invalid = 0,
    New = 1,
    Initialized = 2,
    Used = 3,
    Removed = 4,
}

impl Default for OutStatus {
    fn default() -> Self {
        Self::Invalid
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ParameterState {
    Initialized = 0,
    Updating = 1,
    Updated = 2,
}

impl Default for ParameterState {
    fn default() -> Self {
        Self::Initialized
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct InParameterVersion1 {
    pub type_: EffectType,
    pub is_new: bool,
    pub enabled: bool,
    pub _padding: u8,
    pub mix_id: u32,
    pub workbuffer: CpuAddr,
    pub workbuffer_size: CpuAddr,
    pub process_order: u32,
    pub _unk1c: [u8; 0x4],
    pub specific: [u8; 0xA0],
}

impl Default for InParameterVersion1 {
    fn default() -> Self {
        Self {
            type_: EffectType::Invalid,
            is_new: false,
            enabled: false,
            _padding: 0,
            mix_id: 0,
            workbuffer: 0,
            workbuffer_size: 0,
            process_order: 0,
            _unk1c: [0; 0x4],
            specific: [0; 0xA0],
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct InParameterVersion2 {
    pub type_: EffectType,
    pub is_new: bool,
    pub enabled: bool,
    pub _padding: u8,
    pub mix_id: u32,
    pub workbuffer: CpuAddr,
    pub workbuffer_size: CpuAddr,
    pub process_order: u32,
    pub _unk1c: [u8; 0x4],
    pub specific: [u8; 0xA0],
}

impl Default for InParameterVersion2 {
    fn default() -> Self {
        Self {
            type_: EffectType::Invalid,
            is_new: false,
            enabled: false,
            _padding: 0,
            mix_id: 0,
            workbuffer: 0,
            workbuffer_size: 0,
            process_order: 0,
            _unk1c: [0; 0x4],
            specific: [0; 0xA0],
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct OutStatusVersion1 {
    pub state: OutStatus,
    pub _unk01: [u8; 0xF],
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct OutStatusVersion2 {
    pub state: OutStatus,
    pub _unk01: [u8; 0xF],
    pub result_state: EffectResultState,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct State {
    pub buffer: [u8; 0x500],
}

impl Default for State {
    fn default() -> Self {
        Self { buffer: [0; 0x500] }
    }
}

#[derive(Debug)]
pub struct EffectInfoBase {
    pub(crate) type_: EffectType,
    pub(crate) enabled: bool,
    pub(crate) mix_id: i32,
    pub(crate) process_order: i32,
    pub(crate) buffer_unmapped: bool,
    pub(crate) usage_state: UsageState,
    pub(crate) out_status: OutStatus,
    pub(crate) parameter_state: ParameterState,
    pub(crate) parameter: [u8; 0xA0],
    pub(crate) state: State,
    pub(crate) state_address: CpuAddr,
    pub(crate) workbuffers: [AddressInfo; 2],
    pub(crate) send_buffer_info: CpuAddr,
    pub(crate) send_buffer: CpuAddr,
    pub(crate) return_buffer_info: CpuAddr,
    pub(crate) return_buffer: CpuAddr,
    owns_raw_state: bool,
}

impl Default for EffectInfoBase {
    fn default() -> Self {
        let mut out = Self {
            type_: EffectType::Invalid,
            enabled: false,
            mix_id: UNUSED_MIX_ID,
            process_order: INVALID_PROCESS_ORDER,
            buffer_unmapped: false,
            usage_state: UsageState::Invalid,
            out_status: OutStatus::Invalid,
            parameter_state: ParameterState::Initialized,
            parameter: [0; 0xA0],
            state: State::default(),
            state_address: 0,
            workbuffers: [AddressInfo::default(); 2],
            send_buffer_info: 0,
            send_buffer: 0,
            return_buffer_info: 0,
            return_buffer: 0,
            owns_raw_state: true,
        };
        out.cleanup();
        out
    }
}

impl Clone for EffectInfoBase {
    fn clone(&self) -> Self {
        Self {
            type_: self.type_,
            enabled: self.enabled,
            mix_id: self.mix_id,
            process_order: self.process_order,
            buffer_unmapped: self.buffer_unmapped,
            usage_state: self.usage_state,
            out_status: self.out_status,
            parameter_state: self.parameter_state,
            parameter: self.parameter,
            state: self.state,
            state_address: self.state_address,
            workbuffers: self.workbuffers,
            send_buffer_info: self.send_buffer_info,
            send_buffer: self.send_buffer,
            return_buffer_info: self.return_buffer_info,
            return_buffer: self.return_buffer,
            owns_raw_state: false,
        }
    }
}

impl EffectInfoBase {
    fn drop_owned_raw_state(&mut self) {
        if !self.owns_raw_state {
            return;
        }
        let state_buffer_address = self.state.buffer.as_ptr() as CpuAddr;
        for (index, address) in [state_buffer_address, self.state_address]
            .into_iter()
            .enumerate()
        {
            if index == 1 && address == state_buffer_address {
                continue;
            }
            match self.type_ {
                EffectType::Delay => {
                    crate::renderer::command::effect::delay::drop_delay_state_if_initialized(address)
                }
                EffectType::I3dl2Reverb => {
                    crate::renderer::effect::i3dl2::drop_i3dl2_reverb_state_if_initialized(address)
                }
                EffectType::LightLimiter => {
                    crate::renderer::command::effect::light_limiter::drop_light_limiter_state_if_initialized(
                        address,
                    )
                }
                EffectType::Reverb => {
                    crate::renderer::command::effect::reverb::drop_reverb_workbuffer(address)
                }
                _ => {}
            }
        }
    }

    pub fn cleanup(&mut self) {
        self.drop_owned_raw_state();
        self.type_ = EffectType::Invalid;
        self.enabled = false;
        self.mix_id = UNUSED_MIX_ID;
        self.process_order = INVALID_PROCESS_ORDER;
        self.buffer_unmapped = false;
        self.usage_state = UsageState::Invalid;
        self.out_status = OutStatus::Invalid;
        self.parameter_state = ParameterState::Initialized;
        self.parameter.fill(0);
        self.state = State::default();
        self.state_address = self.state.buffer.as_ptr() as CpuAddr;
        self.send_buffer_info = 0;
        self.send_buffer = 0;
        self.return_buffer_info = 0;
        self.return_buffer = 0;
        for workbuffer in &mut self.workbuffers {
            workbuffer.setup(0, 0);
        }
    }

    pub fn force_unmap_buffers(&self, pool_mapper: &PoolMapper<'_>) {
        for workbuffer in &self.workbuffers {
            if workbuffer.is_mapped() {
                pool_mapper.force_unmap_pointer(workbuffer);
            }
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn should_skip(&self) -> bool {
        self.buffer_unmapped
    }

    pub fn get_type(&self) -> EffectType {
        self.type_
    }

    pub fn set_type(&mut self, type_: EffectType) {
        self.type_ = type_;
    }

    pub fn get_mix_id(&self) -> i32 {
        self.mix_id
    }

    pub fn get_processing_order(&self) -> i32 {
        self.process_order
    }

    pub fn get_parameter(&mut self) -> &mut [u8; 0xA0] {
        &mut self.parameter
    }

    pub fn get_state_buffer(&mut self) -> &mut [u8; 0x500] {
        &mut self.state.buffer
    }

    pub fn refresh_runtime_addresses(&mut self) {
        self.state_address = self.state.buffer.as_ptr() as CpuAddr;
    }

    pub fn get_state_address(&self) -> CpuAddr {
        self.state_address
    }

    pub fn get_state_size(&self) -> u64 {
        self.state.buffer.len() as u64
    }

    pub fn set_usage(&mut self, usage: UsageState) {
        self.usage_state = usage;
    }

    pub fn usage(&self) -> UsageState {
        self.usage_state
    }

    pub fn set_out_status(&mut self, status: OutStatus) {
        self.out_status = status;
    }

    pub fn out_status(&self) -> OutStatus {
        self.out_status
    }

    pub fn set_parameter_state(&mut self, state: ParameterState) {
        self.parameter_state = state;
    }

    pub fn parameter_state(&self) -> ParameterState {
        self.parameter_state
    }

    pub fn workbuffer(&self, index: usize) -> Option<&AddressInfo> {
        self.workbuffers.get(index)
    }

    pub fn workbuffer_mut(&mut self, index: usize) -> Option<&mut AddressInfo> {
        self.workbuffers.get_mut(index)
    }

    pub fn set_buffer_unmapped(&mut self, buffer_unmapped: bool) {
        self.buffer_unmapped = buffer_unmapped;
    }

    pub fn should_update_workbuffer_info_v1(&self, params: &InParameterVersion1) -> bool {
        self.buffer_unmapped || params.is_new
    }

    pub fn should_update_workbuffer_info_v2(&self, params: &InParameterVersion2) -> bool {
        self.buffer_unmapped || params.is_new
    }

    pub fn reset_type(&mut self, type_: EffectType) {
        effect_reset::reset_effect(self, type_);
    }

    pub fn store_status_v1(&self, out: &mut OutStatusVersion1, renderer_active: bool) {
        out.state = if renderer_active {
            if self.usage_state != UsageState::Disabled {
                OutStatus::Used
            } else {
                OutStatus::Removed
            }
        } else if self.usage_state == UsageState::New {
            OutStatus::Used
        } else {
            OutStatus::Removed
        };
    }

    pub fn store_status_v2(
        &self,
        out: &mut OutStatusVersion2,
        renderer_active: bool,
        result_state: EffectResultState,
    ) {
        out.state = if renderer_active {
            if self.usage_state != UsageState::Disabled {
                OutStatus::Used
            } else {
                OutStatus::Removed
            }
        } else if self.usage_state == UsageState::New {
            OutStatus::Used
        } else {
            OutStatus::Removed
        };
        out.result_state = result_state;
    }

    pub fn update_v1(
        &mut self,
        error_info: &mut ErrorInfo,
        params: &InParameterVersion1,
        pool_mapper: &PoolMapper<'_>,
    ) {
        if self.type_ != params.type_ {
            self.force_unmap_buffers(pool_mapper);
            self.reset_type(params.type_);
        }

        match params.type_ {
            EffectType::Invalid => {
                Self::set_success(error_info);
            }
            EffectType::Mix => buffer_mixer::update_v1(self, error_info, params, pool_mapper),
            EffectType::Aux => aux_::update_v1(self, error_info, params, pool_mapper),
            EffectType::Delay => delay::update_v1(self, error_info, params, pool_mapper),
            EffectType::Reverb => reverb::update_v1(self, error_info, params, pool_mapper),
            EffectType::I3dl2Reverb => i3dl2::update_v1(self, error_info, params, pool_mapper),
            EffectType::BiquadFilter => {
                biquad_filter::update_v1(self, error_info, params, pool_mapper)
            }
            EffectType::LightLimiter => {
                light_limiter::update_v1(self, error_info, params, pool_mapper)
            }
            EffectType::Capture => capture::update_v1(self, error_info, params, pool_mapper),
            EffectType::Compressor => compressor::update_v1(self, error_info, params, pool_mapper),
        }
        self.parameter_state = ParameterState::Updated;
    }

    pub fn update_v2(
        &mut self,
        error_info: &mut ErrorInfo,
        params: &InParameterVersion2,
        pool_mapper: &PoolMapper<'_>,
    ) {
        if self.type_ != params.type_ {
            self.force_unmap_buffers(pool_mapper);
            self.reset_type(params.type_);
        }

        match params.type_ {
            EffectType::Invalid => {
                Self::set_success(error_info);
            }
            EffectType::Mix => buffer_mixer::update_v2(self, error_info, params, pool_mapper),
            EffectType::Aux => aux_::update_v2(self, error_info, params, pool_mapper),
            EffectType::Delay => delay::update_v2(self, error_info, params, pool_mapper),
            EffectType::Reverb => reverb::update_v2(self, error_info, params, pool_mapper),
            EffectType::I3dl2Reverb => i3dl2::update_v2(self, error_info, params, pool_mapper),
            EffectType::BiquadFilter => {
                biquad_filter::update_v2(self, error_info, params, pool_mapper)
            }
            EffectType::LightLimiter => {
                light_limiter::update_v2(self, error_info, params, pool_mapper)
            }
            EffectType::Capture => capture::update_v2(self, error_info, params, pool_mapper),
            EffectType::Compressor => compressor::update_v2(self, error_info, params, pool_mapper),
        }
        self.parameter_state = ParameterState::Updated;
    }

    pub fn update_for_command_generation(&mut self) {
        match self.type_ {
            EffectType::Invalid => {}
            EffectType::Mix => buffer_mixer::update_for_command_generation(self),
            EffectType::Aux => aux_::update_for_command_generation(self),
            EffectType::Delay => delay::update_for_command_generation(self),
            EffectType::Reverb => reverb::update_for_command_generation(self),
            EffectType::I3dl2Reverb => i3dl2::update_for_command_generation(self),
            EffectType::BiquadFilter => biquad_filter::update_for_command_generation(self),
            EffectType::LightLimiter => light_limiter::update_for_command_generation(self),
            EffectType::Capture => capture::update_for_command_generation(self),
            EffectType::Compressor => compressor::update_for_command_generation(self),
        }
    }

    pub fn initialize_result_state(&self, result_state: &mut EffectResultState) {
        match self.type_ {
            EffectType::LightLimiter => light_limiter::initialize_result_state(result_state),
            _ => *result_state = EffectResultState::default(),
        }
    }

    pub fn update_result_state(
        &self,
        out_result_state: &mut EffectResultState,
        result_state: &EffectResultState,
    ) {
        match self.type_ {
            EffectType::LightLimiter => {
                light_limiter::update_result_state(out_result_state, result_state)
            }
            _ => *out_result_state = *result_state,
        }
    }

    pub fn get_workbuffer(&mut self, index: i32) -> CpuAddr {
        match self.type_ {
            EffectType::Aux => aux_::get_workbuffer(self, index),
            EffectType::Delay => delay::get_workbuffer(self, index),
            EffectType::Reverb => reverb::get_workbuffer(self, index),
            EffectType::I3dl2Reverb => i3dl2::get_workbuffer(self, index),
            EffectType::LightLimiter => light_limiter::get_workbuffer(self, index),
            EffectType::Capture => capture::get_workbuffer(self, index),
            EffectType::Compressor => compressor::get_workbuffer(self, index),
            _ => 0,
        }
    }

    pub fn get_send_buffer_info(&self) -> CpuAddr {
        self.send_buffer_info
    }

    pub fn get_send_buffer(&self) -> CpuAddr {
        self.send_buffer
    }

    pub fn get_return_buffer_info(&self) -> CpuAddr {
        self.return_buffer_info
    }

    pub fn get_return_buffer(&self) -> CpuAddr {
        self.return_buffer
    }

    pub(crate) fn apply_common_settings(
        &mut self,
        _is_new: bool,
        enabled: bool,
        mix_id: i32,
        process_order: i32,
    ) {
        // Match upstream zuyu's per-effect Update() — only mirror the
        // shared param fields. Upstream does NOT touch usage_state or
        // out_status here. usage_state is only set by:
        //   - per-effect-type update_v1/v2 with `if buffer_unmapped || is_new { usage_state = New }`
        //   - update_for_command_generation: `Enabled` when `enabled`, else `Disabled`
        // Setting usage_state=Disabled here (the prior ruzu bug) made
        // store_status emit OutStatus::Removed for all effects, which the
        // game's audio worker likely interprets as "no effects active" and
        // skips the post-update Flush+Signal cleanup branch.
        self.enabled = enabled;
        self.mix_id = mix_id;
        self.process_order = process_order;
    }

    pub(crate) fn is_channel_count_valid(channel_count: i32) -> bool {
        channel_count <= MAX_CHANNELS as i32 && matches!(channel_count, 1 | 2 | 4 | 6)
    }

    pub(crate) fn read_specific<T: Copy>(specific: &[u8]) -> T {
        debug_assert!(size_of::<T>() <= specific.len());
        unsafe { (specific.as_ptr() as *const T).read_unaligned() }
    }

    pub(crate) fn read_parameter<T: Copy>(&self) -> T {
        debug_assert!(size_of::<T>() <= self.parameter.len());
        unsafe { (self.parameter.as_ptr() as *const T).read_unaligned() }
    }

    pub(crate) fn write_parameter<T: Copy>(&mut self, value: &T) {
        let size = size_of::<T>();
        debug_assert!(size <= self.parameter.len());
        self.parameter.fill(0);
        unsafe {
            std::ptr::copy_nonoverlapping(
                value as *const T as *const u8,
                self.parameter.as_mut_ptr(),
                size,
            );
        }
    }

    pub(crate) fn write_parameter_at<T: Copy>(&mut self, offset: usize, value: &T) {
        let size = size_of::<T>();
        debug_assert!(offset + size <= self.parameter.len());
        unsafe {
            std::ptr::copy_nonoverlapping(
                value as *const T as *const u8,
                self.parameter.as_mut_ptr().add(offset),
                size,
            );
        }
    }

    pub(crate) fn set_success(error_info: &mut ErrorInfo) {
        error_info.error_code = ResultCode::SUCCESS;
        error_info.address = 0;
    }

    pub(crate) fn get_single_buffer(&mut self, _index: i32) -> CpuAddr {
        if self.enabled {
            return self.workbuffers[0].get_reference(true);
        }

        if self.usage_state != UsageState::Disabled {
            let reference = self.workbuffers[0].get_reference(false);
            let size = self.workbuffers[0].get_size();
            if reference != 0 && size > 0 {
                // Upstream: "// Invalidate DSP cache" — not implemented in C++ either.
            }
        }
        0
    }
}

impl Drop for EffectInfoBase {
    fn drop(&mut self) {
        self.drop_owned_raw_state();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_effect_update_preserves_cleanup_routing_fields() {
        let mapper = PoolMapper::new(None, false);
        let mut error_info = ErrorInfo::default();
        let mut effect = EffectInfoBase::default();
        let v1 = InParameterVersion1 {
            mix_id: 0,
            process_order: 0,
            ..Default::default()
        };

        effect.update_v1(&mut error_info, &v1, &mapper);

        assert_eq!(effect.get_mix_id(), UNUSED_MIX_ID);
        assert_eq!(effect.get_processing_order(), INVALID_PROCESS_ORDER);

        let v2 = InParameterVersion2 {
            mix_id: 0,
            process_order: 0,
            ..Default::default()
        };

        effect.update_v2(&mut error_info, &v2, &mapper);

        assert_eq!(effect.get_mix_id(), UNUSED_MIX_ID);
        assert_eq!(effect.get_processing_order(), INVALID_PROCESS_ORDER);
    }
}

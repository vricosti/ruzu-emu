use std::mem::size_of;

use common::alignment::align_up;

use crate::common::audio_renderer_parameter::AudioRendererParameterInternal;
use crate::common::common::{
    get_splitter_in_param_header_magic, get_splitter_info_magic, get_splitter_send_data_magic,
};
use crate::renderer::behavior::BehaviorInfo;

use super::{SplitterDestinationData, SplitterInfo};

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct InParameterHeader {
    pub magic: u32,
    pub info_count: i32,
    pub destination_count: i32,
    pub _unk0c: [u8; 0x14],
}

#[derive(Debug, Default, Clone)]
pub struct SplitterContext {
    splitter_infos: Vec<SplitterInfo>,
    info_count: i32,
    splitter_destinations: Vec<SplitterDestinationData>,
    destinations_count: i32,
    splitter_bug_fixed: bool,
    splitter_prev_volume_reset_supported: bool,
    splitter_biquad_param_supported: bool,
    splitter_float_coeff_supported: bool,
}

impl SplitterContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_destination_data(
        &self,
        splitter_id: i32,
        destination_id: i32,
    ) -> Option<&SplitterDestinationData> {
        let splitter_index = usize::try_from(splitter_id).ok()?;
        let destination_index = u32::try_from(destination_id).ok()?;
        let splitter = self.splitter_infos.get(splitter_index)?;
        splitter.get_data(destination_index, &self.splitter_destinations)
    }

    pub fn get_destination_data_mut(
        &mut self,
        splitter_id: i32,
        destination_id: i32,
    ) -> Option<&mut SplitterDestinationData> {
        let splitter_index = usize::try_from(splitter_id).ok()?;
        let destination_index = u32::try_from(destination_id).ok()?;
        let head = self
            .splitter_infos
            .get(splitter_index)?
            .destinations_head()?;
        let mut current = Some(head);
        let mut i = 0;
        while i < destination_index {
            current = current.and_then(|index| self.splitter_destinations.get(index)?.next());
            current?;
            i += 1;
        }
        self.splitter_destinations.get_mut(current?)
    }

    pub fn get_info(&self, splitter_id: i32) -> Option<&SplitterInfo> {
        let splitter_index = usize::try_from(splitter_id).ok()?;
        self.splitter_infos.get(splitter_index)
    }

    pub fn get_info_mut(&mut self, splitter_id: i32) -> Option<&mut SplitterInfo> {
        let splitter_index = usize::try_from(splitter_id).ok()?;
        self.splitter_infos.get_mut(splitter_index)
    }

    pub fn get_data_count(&self) -> u32 {
        self.destinations_count.max(0) as u32
    }

    pub fn get_info_count(&self) -> u32 {
        self.info_count.max(0) as u32
    }

    pub fn get_data(&self, index: u32) -> Option<&SplitterDestinationData> {
        self.splitter_destinations.get(index as usize)
    }

    pub fn using_splitter(&self) -> bool {
        !self.splitter_infos.is_empty()
            && self.info_count > 0
            && !self.splitter_destinations.is_empty()
            && self.destinations_count > 0
    }

    pub fn clear_all_new_connection_flag(&mut self) {
        for splitter_info in &mut self.splitter_infos {
            splitter_info.set_new_connection_flag();
        }
    }

    pub fn initialize(
        &mut self,
        behavior: &BehaviorInfo,
        params: &AudioRendererParameterInternal,
    ) -> bool {
        self.splitter_infos.clear();
        self.splitter_destinations.clear();
        self.info_count = 0;
        self.destinations_count = 0;
        self.splitter_bug_fixed = false;
        self.splitter_prev_volume_reset_supported = false;
        self.splitter_biquad_param_supported = false;
        self.splitter_float_coeff_supported = false;

        if behavior.is_splitter_supported()
            && params.splitter_infos > 0
            && params.splitter_destinations > 0
        {
            self.splitter_infos = (0..params.splitter_infos)
                .map(|i| SplitterInfo::new(i as i32))
                .collect();
            self.splitter_destinations = (0..params.splitter_destinations.max(0) as u32)
                .map(|i| SplitterDestinationData::new(i as i32))
                .collect();
            self.info_count = params.splitter_infos as i32;
            self.destinations_count = params.splitter_destinations;
            self.splitter_bug_fixed = behavior.is_splitter_bug_fixed();
            self.splitter_prev_volume_reset_supported =
                behavior.is_splitter_prev_volume_reset_supported();
            self.splitter_biquad_param_supported =
                behavior.is_biquad_filter_parameter_for_splitter_enabled();
            self.splitter_float_coeff_supported = behavior.is_splitter_destination_v2b_supported();
        }
        true
    }

    pub fn update(&mut self, input: &[u8]) -> bool {
        if self.destinations_count == 0 || self.info_count == 0 {
            return true;
        }

        let Some(in_params) = read_pod::<InParameterHeader>(input, 0) else {
            return false;
        };

        if in_params.magic != get_splitter_in_param_header_magic() {
            return false;
        }
        if in_params.info_count < 0 || in_params.destination_count < 0 {
            return false;
        }

        for splitter_info in &mut self.splitter_infos {
            splitter_info.clear_new_connection_flag();
        }

        let mut offset = size_of::<InParameterHeader>() as u32;
        offset = self.update_info(input, offset, in_params.info_count as u32);
        self.update_data(input, offset, in_params.destination_count as u32);
        true
    }

    pub fn update_info(&mut self, input: &[u8], mut offset: u32, splitter_count: u32) -> u32 {
        for _ in 0..splitter_count {
            let Some(info_header) =
                read_pod::<super::splitter_info::InParameter>(input, offset as usize)
            else {
                break;
            };
            if info_header.magic != get_splitter_info_magic() {
                continue;
            }
            if info_header.id < 0 || info_header.id >= self.info_count {
                break;
            }

            let extra_offset = offset as usize + size_of::<super::splitter_info::InParameter>();
            let mut destination_ids = Vec::with_capacity(info_header.destination_count as usize);
            for index in 0..info_header.destination_count as usize {
                let byte_offset = extra_offset + index * size_of::<u32>();
                let Some(raw) = read_pod::<u32>(input, byte_offset) else {
                    break;
                };
                destination_ids.push(raw as usize);
            }

            self.recompose_destination(info_header.id, &destination_ids);
            if let Some(info) = self.get_info_mut(info_header.id) {
                offset = offset.saturating_add(info.update(&info_header));
            }
        }

        offset
    }

    pub fn update_data(&mut self, input: &[u8], mut offset: u32, count: u32) -> u32 {
        for _ in 0..count {
            if !self.splitter_biquad_param_supported {
                let Some(mut data_header) = read_pod::<
                    super::splitter_destinations_data::InParameter,
                >(input, offset as usize) else {
                    break;
                };
                if data_header.magic != get_splitter_send_data_magic() {
                    continue;
                }
                // Upstream spells the upper bound as `id > count`, then indexes
                // the `count`-element array. Keep the effective array contract
                // explicit in Rust and reject the one-past-end id.
                if data_header.id < 0 || data_header.id >= self.destinations_count {
                    continue;
                }
                if !self.splitter_prev_volume_reset_supported {
                    data_header.reset_prev_volume = false;
                }
                if let Some(destination) =
                    self.splitter_destinations.get_mut(data_header.id as usize)
                {
                    destination.update(&data_header);
                }
                offset = offset.saturating_add(size_of::<
                    super::splitter_destinations_data::InParameter,
                >() as u32);
            } else if !self.splitter_float_coeff_supported {
                let Some(data_header) = read_pod::<
                    super::splitter_destinations_data::InParameterVersion2a,
                >(input, offset as usize) else {
                    break;
                };
                if data_header.magic != get_splitter_send_data_magic() {
                    continue;
                }
                if data_header.id < 0 || data_header.id >= self.destinations_count {
                    continue;
                }
                let mapped = super::splitter_destinations_data::InParameter {
                    magic: data_header.magic,
                    id: data_header.id,
                    mix_volumes: data_header.mix_volumes,
                    mix_id: data_header.mix_id,
                    in_use: data_header.in_use,
                    reset_prev_volume: self.splitter_prev_volume_reset_supported
                        && data_header.reset_prev_volume,
                    _padding: [0; 2],
                };
                if let Some(destination) =
                    self.splitter_destinations.get_mut(data_header.id as usize)
                {
                    destination.update(&mapped);
                    for (output, legacy) in destination
                        .get_biquad_filters_mut()
                        .iter_mut()
                        .zip(data_header.biquad_filters)
                    {
                        output.enabled = legacy.enabled;
                        output.numerator = legacy.b.map(|value| value as f32 / 16384.0);
                        output.denominator = legacy.a.map(|value| value as f32 / 16384.0);
                    }
                }
                offset = offset.saturating_add(size_of::<
                    super::splitter_destinations_data::InParameterVersion2a,
                >() as u32);
            } else {
                let Some(data_header) = read_pod::<
                    super::splitter_destinations_data::InParameterVersion2b,
                >(input, offset as usize) else {
                    break;
                };
                if data_header.magic != get_splitter_send_data_magic() {
                    continue;
                }
                if data_header.id < 0 || data_header.id >= self.destinations_count {
                    continue;
                }
                let mapped = super::splitter_destinations_data::InParameter {
                    magic: data_header.magic,
                    id: data_header.id,
                    mix_volumes: data_header.mix_volumes,
                    mix_id: data_header.mix_id,
                    in_use: data_header.in_use,
                    reset_prev_volume: self.splitter_prev_volume_reset_supported
                        && data_header.reset_prev_volume,
                    _padding: [0; 2],
                };
                if let Some(destination) =
                    self.splitter_destinations.get_mut(data_header.id as usize)
                {
                    destination.update(&mapped);
                    destination
                        .get_biquad_filters_mut()
                        .copy_from_slice(&data_header.biquad_filters);
                }
                offset = offset.saturating_add(size_of::<
                    super::splitter_destinations_data::InParameterVersion2b,
                >() as u32);
            }
        }

        offset
    }

    pub fn update_internal_state(&mut self) {
        for index in 0..self.info_count.max(0) as usize {
            let splitter = self.splitter_infos[index].clone();
            splitter.update_internal_state(&mut self.splitter_destinations);
        }
    }

    pub fn recompose_destination(&mut self, splitter_id: i32, destination_ids: &[usize]) {
        let Some(splitter_index) = usize::try_from(splitter_id).ok() else {
            return;
        };
        let Some(existing_head) = self
            .splitter_infos
            .get(splitter_index)
            .and_then(SplitterInfo::destinations_head)
        else {
            if let Some(info) = self.get_info_mut(splitter_id) {
                info.set_destinations(None);
                info.set_destination_count(0);
            }
            if destination_ids.is_empty() {
                return;
            }
            self.compose_destination_chain(splitter_id, destination_ids);
            return;
        };

        let mut destination = Some(existing_head);
        while let Some(index) = destination {
            let next = self.splitter_destinations[index].next();
            self.splitter_destinations[index].set_next(None);
            destination = next;
        }

        if let Some(info) = self.get_info_mut(splitter_id) {
            info.set_destinations(None);
            info.set_destination_count(0);
        }

        if destination_ids.is_empty() {
            return;
        }

        self.compose_destination_chain(splitter_id, destination_ids);
    }

    fn compose_destination_chain(&mut self, splitter_id: i32, destination_ids: &[usize]) {
        let mut dest_count = destination_ids.len() as u32;
        if !self.splitter_bug_fixed {
            dest_count = dest_count.min(self.get_dest_count_per_info_for_compat());
        }
        if dest_count == 0 {
            return;
        }

        let destination_ids = &destination_ids[..dest_count as usize];
        let head = destination_ids[0];
        let mut current = head;
        for next_index in destination_ids.iter().copied().skip(1) {
            if current >= self.splitter_destinations.len()
                || next_index >= self.splitter_destinations.len()
            {
                break;
            }
            self.splitter_destinations[current].set_next(Some(next_index));
            current = next_index;
        }

        if let Some(info) = self.get_info_mut(splitter_id) {
            info.set_destinations(Some(head));
            info.set_destination_count(dest_count);
        }
    }

    pub fn get_dest_count_per_info_for_compat(&self) -> u32 {
        if self.info_count <= 0 {
            return 0;
        }
        (self.destinations_count / self.info_count) as u32
    }

    pub fn calc_work_buffer_size(
        behavior: &BehaviorInfo,
        params: &AudioRendererParameterInternal,
    ) -> u64 {
        if !behavior.is_splitter_supported() {
            return 0;
        }

        let mut size = params.splitter_destinations.max(0) as u64
            * size_of::<SplitterDestinationData>() as u64
            + params.splitter_infos as u64 * size_of::<SplitterInfo>() as u64;
        if behavior.is_splitter_bug_fixed() {
            size += align_up(
                params.splitter_destinations.max(0) as u64 * size_of::<u32>() as u64,
                0x10,
            );
        }
        size
    }
}

fn read_pod<T: Copy>(input: &[u8], offset: usize) -> Option<T> {
    let end = offset.checked_add(size_of::<T>())?;
    let bytes = input.get(offset..end)?;
    let ptr = bytes.as_ptr() as *const T;
    Some(unsafe { ptr.read_unaligned() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::splitter::{splitter_destinations_data, splitter_info};

    fn make_context() -> SplitterContext {
        let mut context = SplitterContext {
            splitter_infos: vec![SplitterInfo::new(0)],
            info_count: 1,
            splitter_destinations: vec![SplitterDestinationData::new(0)],
            destinations_count: 1,
            splitter_bug_fixed: true,
            splitter_prev_volume_reset_supported: false,
            splitter_biquad_param_supported: false,
            splitter_float_coeff_supported: false,
        };
        context.splitter_infos[0].set_destinations(Some(0));
        context.splitter_infos[0].set_destination_count(1);
        context
    }

    #[test]
    fn negative_splitter_indices_do_not_alias_slot_zero() {
        let mut context = make_context();

        assert!(context.get_info(-1).is_none());
        assert!(context.get_info_mut(-1).is_none());
        assert!(context.get_destination_data(-1, 0).is_none());
        assert!(context.get_destination_data(0, -1).is_none());
        assert!(context.get_destination_data_mut(-1, 0).is_none());
        assert!(context.get_destination_data_mut(0, -1).is_none());

        assert!(context.get_info(0).is_some());
        assert!(context.get_destination_data(0, 0).is_some());
    }

    #[test]
    fn recompose_destination_rejects_negative_splitter_id() {
        let mut context = make_context();

        context.recompose_destination(-1, &[]);

        let info = context.get_info(0).unwrap();
        assert_eq!(info.destinations_head(), Some(0));
        assert_eq!(info.get_destination_count(), 1);
    }

    #[test]
    fn update_rejects_negative_header_counts() {
        let mut context = make_context();
        let mut input = Vec::new();
        let header = InParameterHeader {
            magic: get_splitter_in_param_header_magic(),
            info_count: -1,
            destination_count: 0,
            _unk0c: [0; 0x14],
        };
        input.extend_from_slice(unsafe {
            std::slice::from_raw_parts(
                &header as *const InParameterHeader as *const u8,
                size_of::<InParameterHeader>(),
            )
        });

        assert!(!context.update(&input));
    }

    #[test]
    fn update_info_rejects_id_equal_to_info_count() {
        let mut context = make_context();
        let mut input = vec![0u8; size_of::<splitter_info::InParameter>()];
        let info = splitter_info::InParameter {
            magic: get_splitter_info_magic(),
            id: 1,
            sample_rate: 48_000,
            destination_count: 0,
        };
        input[..size_of::<splitter_info::InParameter>()].copy_from_slice(unsafe {
            std::slice::from_raw_parts(
                &info as *const splitter_info::InParameter as *const u8,
                size_of::<splitter_info::InParameter>(),
            )
        });

        let offset = context.update_info(&input, 0, 1);
        assert_eq!(offset, 0);
        assert!(context.get_info(0).is_some());
    }

    #[test]
    fn update_data_rejects_id_equal_to_destination_count() {
        let mut context = make_context();
        let mut input = vec![0u8; size_of::<splitter_destinations_data::InParameter>()];
        let data = splitter_destinations_data::InParameter {
            magic: get_splitter_send_data_magic(),
            id: 1,
            ..Default::default()
        };
        input[..size_of::<splitter_destinations_data::InParameter>()].copy_from_slice(unsafe {
            std::slice::from_raw_parts(
                &data as *const splitter_destinations_data::InParameter as *const u8,
                size_of::<splitter_destinations_data::InParameter>(),
            )
        });

        let offset = context.update_data(&input, 0, 1);
        assert_eq!(offset, 0);
        assert!(!context.get_destination_data(0, 0).unwrap().is_configured());
    }

    #[test]
    fn update_data_revision_12_maps_fixed_point_biquads() {
        let mut context = make_context();
        context.splitter_biquad_param_supported = true;
        let mut data = splitter_destinations_data::InParameterVersion2a {
            magic: get_splitter_send_data_magic(),
            id: 0,
            mix_id: 7,
            in_use: true,
            ..Default::default()
        };
        data.biquad_filters[0].enabled = true;
        data.biquad_filters[0].b = [16384, -8192, 4096];
        data.biquad_filters[0].a = [-4096, 2048];
        let input = unsafe {
            std::slice::from_raw_parts(
                &data as *const splitter_destinations_data::InParameterVersion2a as *const u8,
                size_of::<splitter_destinations_data::InParameterVersion2a>(),
            )
        };

        assert_eq!(
            context.update_data(input, 0, 1),
            size_of::<splitter_destinations_data::InParameterVersion2a>() as u32
        );
        let destination = context.get_destination_data(0, 0).unwrap();
        assert_eq!(destination.get_mix_id(), 7);
        assert_eq!(
            destination.get_biquad_filters()[0].numerator,
            [1.0, -0.5, 0.25]
        );
        assert_eq!(
            destination.get_biquad_filters()[0].denominator,
            [-0.25, 0.125]
        );
    }

    #[test]
    fn update_data_revision_15_preserves_float_biquads() {
        let mut context = make_context();
        context.splitter_biquad_param_supported = true;
        context.splitter_prev_volume_reset_supported = true;
        context.splitter_float_coeff_supported = true;
        let mut data = splitter_destinations_data::InParameterVersion2b {
            magic: get_splitter_send_data_magic(),
            id: 0,
            mix_id: 9,
            in_use: true,
            reset_prev_volume: true,
            ..Default::default()
        };
        data.biquad_filters[0].enabled = true;
        data.biquad_filters[0].numerator = [0.125, -0.25, 0.5];
        data.biquad_filters[0].denominator = [-0.75, 0.875];
        let input = unsafe {
            std::slice::from_raw_parts(
                &data as *const splitter_destinations_data::InParameterVersion2b as *const u8,
                size_of::<splitter_destinations_data::InParameterVersion2b>(),
            )
        };

        assert_eq!(
            context.update_data(input, 0, 1),
            size_of::<splitter_destinations_data::InParameterVersion2b>() as u32
        );
        let destination = context.get_destination_data(0, 0).unwrap();
        assert_eq!(destination.get_mix_id(), 9);
        assert_eq!(
            destination.get_biquad_filters()[0].numerator,
            [0.125, -0.25, 0.5]
        );
        assert_eq!(
            destination.get_biquad_filters()[0].denominator,
            [-0.75, 0.875]
        );
    }

    #[test]
    fn clear_all_new_connection_flag_matches_upstream_set_behavior() {
        let mut context = make_context();
        context.get_info_mut(0).unwrap().clear_new_connection_flag();
        assert!(!context.get_info(0).unwrap().has_new_connection());

        context.clear_all_new_connection_flag();

        assert!(context.get_info(0).unwrap().has_new_connection());
    }

    #[test]
    fn initialize_with_disabled_splitter_clears_previous_state() {
        let mut behavior = BehaviorInfo::new();
        behavior.set_user_lib_revision(1);
        let mut context = make_context();
        let params = AudioRendererParameterInternal {
            sample_rate: 48_000,
            sample_count: 240,
            mixes: 1,
            sub_mixes: 0,
            voices: 0,
            sinks: 0,
            effects: 0,
            perf_frames: 0,
            voice_drop_enabled: 0,
            unk_21: 0,
            rendering_device: 0,
            execution_mode: crate::common::audio_renderer_parameter::ExecutionMode::Auto,
            splitter_infos: 0,
            splitter_destinations: 0,
            external_context_size: 0,
            revision: 1,
        };

        context.initialize(&behavior, &params);

        assert!(!context.using_splitter());
        assert_eq!(context.get_info_count(), 0);
        assert_eq!(context.get_data_count(), 0);
        assert!(context.get_info(0).is_none());
        assert!(context.get_data(0).is_none());
    }
}

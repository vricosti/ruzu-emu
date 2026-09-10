// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of hid_core/resources/six_axis/six_axis.h and six_axis.cpp

use crate::frontend::emulated_controller::ControllerMotion;
use crate::hid_core::{EmulatedControllerHandle, HIDCore};
use crate::hid_result::*;
use crate::hid_types::*;
use crate::hid_util::*;
use crate::resources::applet_resource::ARUID_INDEX_MAX;
use crate::resources::controller_base::ControllerActivation;
use common::ResultCode;

pub const NPAD_COUNT: usize = 10;

#[cfg(test)]
#[path = "six_axis_tests.rs"]
mod tests;

/// Per-sixaxis-style parameters, matching upstream SixaxisParameters.
#[derive(Debug, Clone)]
pub struct SixaxisParameters {
    pub is_fusion_enabled: bool,
    pub unaltered_passthrough: bool,
    pub fusion: SixAxisSensorFusionParameters,
    pub gyroscope_zero_drift_mode: GyroscopeZeroDriftMode,
    // Calibration and IC info are large opaque blobs; stored as byte arrays
    // matching upstream sizes.
    pub calibration: [u8; 0x744],
    pub ic_information: [u8; 0xC8],
}

impl Default for SixaxisParameters {
    fn default() -> Self {
        Self {
            is_fusion_enabled: true,
            unaltered_passthrough: false,
            fusion: SixAxisSensorFusionParameters::default(),
            gyroscope_zero_drift_mode: GyroscopeZeroDriftMode::Standard,
            calibration: [0u8; 0x744],
            ic_information: [0u8; 0xC8],
        }
    }
}

/// Per-npad controller data for six-axis processing.
#[derive(Clone)]
pub struct NpadControllerData {
    pub device: Option<EmulatedControllerHandle>,
    pub sixaxis_at_rest: bool,
    pub sixaxis_sensor_enabled: bool,
    pub sixaxis_fullkey: SixaxisParameters,
    pub sixaxis_handheld: SixaxisParameters,
    pub sixaxis_dual_left: SixaxisParameters,
    pub sixaxis_dual_right: SixaxisParameters,
    pub sixaxis_left: SixaxisParameters,
    pub sixaxis_right: SixaxisParameters,
    pub sixaxis_unknown: SixaxisParameters,
    // Current pad states
    pub sixaxis_fullkey_state: SixAxisSensorState,
    pub sixaxis_handheld_state: SixAxisSensorState,
    pub sixaxis_dual_left_state: SixAxisSensorState,
    pub sixaxis_dual_right_state: SixAxisSensorState,
    pub sixaxis_left_lifo_state: SixAxisSensorState,
    pub sixaxis_right_lifo_state: SixAxisSensorState,
}

impl Default for NpadControllerData {
    fn default() -> Self {
        Self {
            device: None,
            sixaxis_at_rest: true,
            sixaxis_sensor_enabled: true,
            sixaxis_fullkey: SixaxisParameters::default(),
            sixaxis_handheld: SixaxisParameters::default(),
            sixaxis_dual_left: SixaxisParameters::default(),
            sixaxis_dual_right: SixaxisParameters::default(),
            sixaxis_left: SixaxisParameters::default(),
            sixaxis_right: SixaxisParameters::default(),
            sixaxis_unknown: SixaxisParameters::default(),
            sixaxis_fullkey_state: SixAxisSensorState::default(),
            sixaxis_handheld_state: SixAxisSensorState::default(),
            sixaxis_dual_left_state: SixAxisSensorState::default(),
            sixaxis_dual_right_state: SixAxisSensorState::default(),
            sixaxis_left_lifo_state: SixAxisSensorState::default(),
            sixaxis_right_lifo_state: SixAxisSensorState::default(),
        }
    }
}

/// SixAxis controller — manages per-npad six-axis sensor parameters and writes
/// motion state into shared memory.
///
/// Upstream stores `std::array<NpadControllerData, NPAD_COUNT>` inline, but the
/// entire `SixAxis` is heap-allocated via `std::make_shared<SixAxis>(...)`.
/// In Rust we Box the array to avoid placing ~140 KB on the fiber stack during
/// construction (fiber stacks are 512 KB, matching upstream `default_stack_size`).
pub struct SixAxis {
    pub activation: ControllerActivation,
    pub controller_data: Box<[NpadControllerData; NPAD_COUNT]>,
}

impl SixAxis {
    pub fn new(hid_core: &HIDCore) -> Self {
        Self {
            activation: ControllerActivation::new(),
            controller_data: Box::new(std::array::from_fn(|index| NpadControllerData {
                device: Some(hid_core.get_emulated_controller_by_index(index)),
                ..NpadControllerData::default()
            })),
        }
    }

    pub fn on_init(&mut self) {}
    pub fn on_release(&mut self) {}

    /// Port of SixAxis::OnUpdate. ResourceManager holds its shared lock;
    /// AppletResource's mutex protects the mapped buffers during publication.
    pub fn on_update(&mut self) {
        let Some(resource) = self.activation.applet_resource.as_ref() else {
            return;
        };
        let mut resource = resource.lock();
        for aruid_index in 0..ARUID_INDEX_MAX {
            let data = resource.get_aruid_data_by_index(aruid_index);
            if !data.flag.is_assigned() {
                continue;
            }
            if !self.activation.is_controller_activated() {
                return;
            }
            let enabled = data.flag.enable_six_axis_sensor();
            let Some(shared) = resource.get_shared_memory_format_by_index_mut(aruid_index) else {
                continue;
            };
            for (index, controller) in self.controller_data.iter_mut().enumerate() {
                let device = controller
                    .device
                    .as_ref()
                    .expect("SixAxis requires a controller")
                    .lock();
                let style = device.get_npad_style_index(false);
                if !enabled || style == NpadStyleIndex::None || !device.is_connected(false) {
                    continue;
                }
                let motions = device.get_motions();
                drop(device);
                let memory = &mut shared.npad.npad_entry[index].internal_state;
                controller.sixaxis_fullkey_state = SixAxisSensorState::default();
                controller.sixaxis_handheld_state = SixAxisSensorState::default();
                controller.sixaxis_dual_left_state = SixAxisSensorState::default();
                controller.sixaxis_dual_right_state = SixAxisSensorState::default();
                controller.sixaxis_left_lifo_state = SixAxisSensorState::default();
                controller.sixaxis_right_lifo_state = SixAxisSensorState::default();
                let motion_enabled = *common::settings::values().motion_enabled.get_value();
                let sensor_enabled = controller.sixaxis_sensor_enabled;
                if sensor_enabled && motion_enabled {
                    controller.sixaxis_at_rest = motions.iter().all(|motion| motion.is_at_rest);
                }
                let set_motion_state =
                    |state: &mut SixAxisSensorState, motion: &ControllerMotion| {
                        state.delta_time = 5_000_000;
                        state.attribute.raw = 1;
                        if !sensor_enabled || !motion_enabled {
                            state.accel = Vec3f {
                                x: 0.0,
                                y: 0.0,
                                z: -1.0,
                            };
                            state.orientation = [
                                Vec3f {
                                    x: 1.0,
                                    y: 0.0,
                                    z: 0.0,
                                },
                                Vec3f {
                                    x: 0.0,
                                    y: 1.0,
                                    z: 0.0,
                                },
                                Vec3f {
                                    x: 0.0,
                                    y: 0.0,
                                    z: 1.0,
                                },
                            ];
                            return;
                        }
                        state.accel = motion.accel;
                        state.gyro = motion.gyro;
                        state.rotation = motion.rotation;
                        state.orientation = motion.orientation;
                    };
                match style {
                    NpadStyleIndex::Fullkey => {
                        set_motion_state(&mut controller.sixaxis_fullkey_state, &motions[0])
                    }
                    NpadStyleIndex::Handheld => {
                        set_motion_state(&mut controller.sixaxis_handheld_state, &motions[0])
                    }
                    NpadStyleIndex::JoyconDual => {
                        set_motion_state(&mut controller.sixaxis_dual_left_state, &motions[0]);
                        set_motion_state(&mut controller.sixaxis_dual_right_state, &motions[1]);
                    }
                    NpadStyleIndex::JoyconLeft => {
                        set_motion_state(&mut controller.sixaxis_left_lifo_state, &motions[0])
                    }
                    NpadStyleIndex::JoyconRight => {
                        set_motion_state(&mut controller.sixaxis_right_lifo_state, &motions[1])
                    }
                    NpadStyleIndex::Pokeball => {
                        set_motion_state(&mut controller.sixaxis_fullkey_state, &motions[0]);
                        controller.sixaxis_fullkey_state.delta_time = 15_000_000;
                    }
                    _ => {}
                }
                controller.sixaxis_fullkey_state.sampling_number = memory
                    .sixaxis_fullkey_lifo
                    .lifo
                    .read_current_entry()
                    .state
                    .sampling_number
                    .wrapping_add(1);
                controller.sixaxis_handheld_state.sampling_number = memory
                    .sixaxis_handheld_lifo
                    .lifo
                    .read_current_entry()
                    .state
                    .sampling_number
                    .wrapping_add(1);
                controller.sixaxis_dual_left_state.sampling_number = memory
                    .sixaxis_dual_left_lifo
                    .lifo
                    .read_current_entry()
                    .state
                    .sampling_number
                    .wrapping_add(1);
                controller.sixaxis_dual_right_state.sampling_number = memory
                    .sixaxis_dual_right_lifo
                    .lifo
                    .read_current_entry()
                    .state
                    .sampling_number
                    .wrapping_add(1);
                controller.sixaxis_left_lifo_state.sampling_number = memory
                    .sixaxis_left_lifo
                    .lifo
                    .read_current_entry()
                    .state
                    .sampling_number
                    .wrapping_add(1);
                controller.sixaxis_right_lifo_state.sampling_number = memory
                    .sixaxis_right_lifo
                    .lifo
                    .read_current_entry()
                    .state
                    .sampling_number
                    .wrapping_add(1);
                if index_to_npad_id_type(index) == NpadIdType::Handheld {
                    memory
                        .sixaxis_handheld_lifo
                        .lifo
                        .write_next_entry(controller.sixaxis_handheld_state);
                } else {
                    memory
                        .sixaxis_fullkey_lifo
                        .lifo
                        .write_next_entry(controller.sixaxis_fullkey_state);
                }
                memory
                    .sixaxis_dual_left_lifo
                    .lifo
                    .write_next_entry(controller.sixaxis_dual_left_state);
                memory
                    .sixaxis_dual_right_lifo
                    .lifo
                    .write_next_entry(controller.sixaxis_dual_right_state);
                memory
                    .sixaxis_left_lifo
                    .lifo
                    .write_next_entry(controller.sixaxis_left_lifo_state);
                memory
                    .sixaxis_right_lifo
                    .lifo
                    .write_next_entry(controller.sixaxis_right_lifo_state);
            }
        }
    }

    pub fn set_gyroscope_zero_drift_mode(
        &mut self,
        sixaxis_handle: &SixAxisSensorHandle,
        drift_mode: GyroscopeZeroDriftMode,
    ) -> ResultCode {
        let is_valid = is_sixaxis_handle_valid(sixaxis_handle);
        if is_valid != ResultCode::SUCCESS {
            return is_valid;
        }

        let sixaxis = self.get_sixaxis_state_mut(sixaxis_handle);
        sixaxis.gyroscope_zero_drift_mode = drift_mode;
        self.get_controller_from_handle(sixaxis_handle)
            .device
            .as_ref()
            .expect("SixAxis requires a controller")
            .lock()
            .set_gyroscope_zero_drift_mode(drift_mode);
        ResultCode::SUCCESS
    }

    pub fn get_gyroscope_zero_drift_mode(
        &self,
        sixaxis_handle: &SixAxisSensorHandle,
    ) -> Result<GyroscopeZeroDriftMode, ResultCode> {
        let is_valid = is_sixaxis_handle_valid(sixaxis_handle);
        if is_valid != ResultCode::SUCCESS {
            return Err(is_valid);
        }
        let sixaxis = self.get_sixaxis_state(sixaxis_handle);
        Ok(sixaxis.gyroscope_zero_drift_mode)
    }

    pub fn is_six_axis_sensor_at_rest(
        &self,
        sixaxis_handle: &SixAxisSensorHandle,
    ) -> Result<bool, ResultCode> {
        let is_valid = is_sixaxis_handle_valid(sixaxis_handle);
        if is_valid != ResultCode::SUCCESS {
            return Err(is_valid);
        }
        let controller = self.get_controller_from_handle(sixaxis_handle);
        Ok(controller.sixaxis_at_rest)
    }

    pub fn enable_six_axis_sensor_unaltered_passthrough(
        &mut self,
        sixaxis_handle: &SixAxisSensorHandle,
        is_enabled: bool,
    ) -> ResultCode {
        let is_valid = is_sixaxis_handle_valid(sixaxis_handle);
        if is_valid != ResultCode::SUCCESS {
            return is_valid;
        }
        let sixaxis = self.get_sixaxis_state_mut(sixaxis_handle);
        sixaxis.unaltered_passthrough = is_enabled;
        ResultCode::SUCCESS
    }

    pub fn is_six_axis_sensor_unaltered_passthrough_enabled(
        &self,
        sixaxis_handle: &SixAxisSensorHandle,
    ) -> Result<bool, ResultCode> {
        let is_valid = is_sixaxis_handle_valid(sixaxis_handle);
        if is_valid != ResultCode::SUCCESS {
            return Err(is_valid);
        }
        let sixaxis = self.get_sixaxis_state(sixaxis_handle);
        Ok(sixaxis.unaltered_passthrough)
    }

    pub fn set_six_axis_enabled(
        &mut self,
        sixaxis_handle: &SixAxisSensorHandle,
        sixaxis_status: bool,
    ) -> ResultCode {
        let is_valid = is_sixaxis_handle_valid(sixaxis_handle);
        if is_valid != ResultCode::SUCCESS {
            return is_valid;
        }
        let controller = self.get_controller_from_handle_mut(sixaxis_handle);
        controller.sixaxis_sensor_enabled = sixaxis_status;
        ResultCode::SUCCESS
    }

    pub fn is_six_axis_sensor_fusion_enabled(
        &self,
        sixaxis_handle: &SixAxisSensorHandle,
    ) -> Result<bool, ResultCode> {
        let is_valid = is_sixaxis_handle_valid(sixaxis_handle);
        if is_valid != ResultCode::SUCCESS {
            return Err(is_valid);
        }
        let sixaxis = self.get_sixaxis_state(sixaxis_handle);
        Ok(sixaxis.is_fusion_enabled)
    }

    pub fn set_six_axis_fusion_enabled(
        &mut self,
        sixaxis_handle: &SixAxisSensorHandle,
        is_fusion_enabled: bool,
    ) -> ResultCode {
        let is_valid = is_sixaxis_handle_valid(sixaxis_handle);
        if is_valid != ResultCode::SUCCESS {
            return is_valid;
        }
        let sixaxis = self.get_sixaxis_state_mut(sixaxis_handle);
        sixaxis.is_fusion_enabled = is_fusion_enabled;
        ResultCode::SUCCESS
    }

    pub fn set_six_axis_fusion_parameters(
        &mut self,
        sixaxis_handle: &SixAxisSensorHandle,
        sixaxis_fusion_parameters: SixAxisSensorFusionParameters,
    ) -> ResultCode {
        let is_valid = is_sixaxis_handle_valid(sixaxis_handle);
        if is_valid != ResultCode::SUCCESS {
            return is_valid;
        }

        let param1 = sixaxis_fusion_parameters.parameter1;
        if param1 < 0.0 || param1 > 1.0 {
            return INVALID_SIX_AXIS_FUSION_RANGE;
        }

        let sixaxis = self.get_sixaxis_state_mut(sixaxis_handle);
        sixaxis.fusion = sixaxis_fusion_parameters;
        ResultCode::SUCCESS
    }

    pub fn get_six_axis_fusion_parameters(
        &self,
        sixaxis_handle: &SixAxisSensorHandle,
    ) -> Result<SixAxisSensorFusionParameters, ResultCode> {
        let is_valid = is_sixaxis_handle_valid(sixaxis_handle);
        if is_valid != ResultCode::SUCCESS {
            return Err(is_valid);
        }
        let sixaxis = self.get_sixaxis_state(sixaxis_handle);
        Ok(sixaxis.fusion)
    }

    /// Port of SixAxis::LoadSixAxisSensorCalibrationParameter.
    /// Returns a reference to the calibration data for the given handle.
    pub fn get_sixaxis_calibration(&self, sixaxis_handle: &SixAxisSensorHandle) -> &[u8; 0x744] {
        let sixaxis = self.get_sixaxis_state(sixaxis_handle);
        &sixaxis.calibration
    }

    /// Port of SixAxis::GetSixAxisSensorIcInformation.
    /// Returns a reference to the IC information data for the given handle.
    pub fn get_sixaxis_ic_information(&self, sixaxis_handle: &SixAxisSensorHandle) -> &[u8; 0xC8] {
        let sixaxis = self.get_sixaxis_state(sixaxis_handle);
        &sixaxis.ic_information
    }

    fn get_sixaxis_state(&self, sixaxis_handle: &SixAxisSensorHandle) -> &SixaxisParameters {
        let controller = self.get_controller_from_handle(sixaxis_handle);
        match sixaxis_handle.npad_type {
            NpadStyleIndex::Fullkey | NpadStyleIndex::Pokeball => &controller.sixaxis_fullkey,
            NpadStyleIndex::Handheld => &controller.sixaxis_handheld,
            NpadStyleIndex::JoyconDual => {
                if sixaxis_handle.device_index == DeviceIndex::Left {
                    &controller.sixaxis_dual_left
                } else {
                    &controller.sixaxis_dual_right
                }
            }
            NpadStyleIndex::JoyconLeft => &controller.sixaxis_left,
            NpadStyleIndex::JoyconRight => &controller.sixaxis_right,
            _ => &controller.sixaxis_unknown,
        }
    }

    fn get_sixaxis_state_mut(
        &mut self,
        sixaxis_handle: &SixAxisSensorHandle,
    ) -> &mut SixaxisParameters {
        let controller = self.get_controller_from_handle_mut(sixaxis_handle);
        match sixaxis_handle.npad_type {
            NpadStyleIndex::Fullkey | NpadStyleIndex::Pokeball => &mut controller.sixaxis_fullkey,
            NpadStyleIndex::Handheld => &mut controller.sixaxis_handheld,
            NpadStyleIndex::JoyconDual => {
                if sixaxis_handle.device_index == DeviceIndex::Left {
                    &mut controller.sixaxis_dual_left
                } else {
                    &mut controller.sixaxis_dual_right
                }
            }
            NpadStyleIndex::JoyconLeft => &mut controller.sixaxis_left,
            NpadStyleIndex::JoyconRight => &mut controller.sixaxis_right,
            _ => &mut controller.sixaxis_unknown,
        }
    }

    fn get_controller_from_handle(
        &self,
        device_handle: &SixAxisSensorHandle,
    ) -> &NpadControllerData {
        let npad_id =
            unsafe { std::mem::transmute::<u32, NpadIdType>(device_handle.npad_id as u32) };
        self.get_controller_from_npad_id_type(npad_id)
    }

    fn get_controller_from_handle_mut(
        &mut self,
        device_handle: &SixAxisSensorHandle,
    ) -> &mut NpadControllerData {
        let npad_id =
            unsafe { std::mem::transmute::<u32, NpadIdType>(device_handle.npad_id as u32) };
        self.get_controller_from_npad_id_type_mut(npad_id)
    }

    fn get_controller_from_npad_id_type(&self, mut npad_id: NpadIdType) -> &NpadControllerData {
        if !is_npad_id_valid(npad_id) {
            npad_id = NpadIdType::Player1;
        }
        let npad_index = npad_id_type_to_index(npad_id);
        &self.controller_data[npad_index]
    }

    fn get_controller_from_npad_id_type_mut(
        &mut self,
        mut npad_id: NpadIdType,
    ) -> &mut NpadControllerData {
        if !is_npad_id_valid(npad_id) {
            npad_id = NpadIdType::Player1;
        }
        let npad_index = npad_id_type_to_index(npad_id);
        &mut self.controller_data[npad_index]
    }
}

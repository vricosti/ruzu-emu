//! Port of zuyu/src/core/hle/service/set/setting_formats/device_settings.h and .cpp
//!
//! Device settings format.

/// Port of Service::Set::DeviceSettings
#[derive(Clone, Copy)]
#[repr(C)]
pub struct DeviceSettings {
    /// Reserved
    pub _reserved_0x00: [u8; 0x10],

    /// nn::settings::BatteryLot
    pub ptm_battery_lot: [u8; 0x18],

    /// nn::settings::system::PtmFuelGaugeParameter
    pub ptm_fuel_gauge_parameter: [u8; 0x18],

    /// ptm_battery_version
    pub ptm_battery_version: u8,

    /// Padding (3 bytes implicit before u32)
    pub _padding_0x41: [u8; 3],

    /// nn::settings::system::PtmCycleCountReliability
    pub ptm_cycle_count_reliability: u32,

    /// Reserved
    pub _reserved_0x48: [u8; 0x48],

    /// nn::settings::system::AnalogStickUserCalibration L
    pub analog_user_stick_calibration_l: [u8; 0x10],

    /// nn::settings::system::AnalogStickUserCalibration R
    pub analog_user_stick_calibration_r: [u8; 0x10],

    /// Reserved
    pub _reserved_0x_b0: [u8; 0x20],

    /// nn::settings::system::ConsoleSixAxisSensorAccelerationBias
    pub console_six_axis_sensor_acceleration_bias: [f32; 3],

    /// nn::settings::system::ConsoleSixAxisSensorAngularVelocityBias
    pub console_six_axis_sensor_angular_velocity_bias: [f32; 3],

    /// nn::settings::system::ConsoleSixAxisSensorAccelerationGain
    pub console_six_axis_sensor_acceleration_gain: [u8; 0x24],

    /// nn::settings::system::ConsoleSixAxisSensorAngularVelocityGain
    pub console_six_axis_sensor_angular_velocity_gain: [u8; 0x24],

    /// nn::settings::system::ConsoleSixAxisSensorAngularVelocityTimeBias
    pub console_six_axis_sensor_angular_velocity_time_bias: [f32; 3],

    /// nn::settings::system::ConsoleSixAxisSensorAngularAcceleration
    pub console_six_axis_sensor_angular_acceleration: [u8; 0x24],
}

// offsetof checks (matching upstream static_asserts)
const _: () = {
    assert!(core::mem::offset_of!(DeviceSettings, ptm_battery_lot) == 0x10);
    assert!(core::mem::offset_of!(DeviceSettings, ptm_cycle_count_reliability) == 0x44);
    assert!(core::mem::offset_of!(DeviceSettings, analog_user_stick_calibration_l) == 0x90);
    assert!(
        core::mem::offset_of!(DeviceSettings, console_six_axis_sensor_acceleration_bias) == 0xD0
    );
    assert!(
        core::mem::offset_of!(DeviceSettings, console_six_axis_sensor_angular_acceleration)
            == 0x13C
    );
    assert!(core::mem::size_of::<DeviceSettings>() == 0x160);
};

impl Default for DeviceSettings {
    fn default() -> Self {
        // SAFETY: All-zero is valid for this repr(C) struct of plain data types.
        unsafe { core::mem::zeroed() }
    }
}

impl core::fmt::Debug for DeviceSettings {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DeviceSettings")
            .field("ptm_battery_version", &self.ptm_battery_version)
            .field(
                "ptm_cycle_count_reliability",
                &self.ptm_cycle_count_reliability,
            )
            .finish()
    }
}

impl DeviceSettings {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Corresponds to `DefaultDeviceSettings` in `device_settings.cpp`.
pub fn default_device_settings() -> DeviceSettings {
    DeviceSettings::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_payload_is_fully_zeroed() {
        let settings = default_device_settings();
        let bytes = unsafe {
            core::slice::from_raw_parts(
                (&settings as *const DeviceSettings).cast::<u8>(),
                core::mem::size_of::<DeviceSettings>(),
            )
        };

        assert_eq!(bytes.len(), 0x160);
        assert!(bytes.iter().all(|byte| *byte == 0));
    }
}

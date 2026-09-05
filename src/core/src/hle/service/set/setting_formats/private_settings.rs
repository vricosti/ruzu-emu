//! Port of zuyu/src/core/hle/service/set/setting_formats/private_settings.h and .cpp
//!
//! Private settings format.

use crate::hle::service::set::settings_types::InitialLaunchSettings;

/// Port of Service::Set::PrivateSettings
#[derive(Clone, Copy)]
#[repr(C)]
pub struct PrivateSettings {
    /// Reserved
    pub _reserved_0x00: [u8; 0x10],

    /// nn::settings::system::InitialLaunchSettings
    pub initial_launch_settings: InitialLaunchSettings,

    /// Reserved
    pub _reserved_0x30: [u8; 0x20],

    /// Common::UUID external_clock_source_id
    pub external_clock_source_id: [u8; 16],

    /// shutdown_rtc_value
    pub shutdown_rtc_value: i64,

    /// external_steady_clock_internal_offset
    pub external_steady_clock_internal_offset: i64,

    /// Reserved
    pub _reserved_0x70: [u8; 0x60],

    /// nn::settings::system::PlatformRegion
    pub platform_region: i32,

    /// Reserved
    pub _reserved_0x_d4: [u8; 0x4],
}

// offsetof checks (matching upstream static_asserts)
const _: () = {
    assert!(core::mem::offset_of!(PrivateSettings, initial_launch_settings) == 0x10);
    assert!(core::mem::offset_of!(PrivateSettings, external_clock_source_id) == 0x50);
    assert!(core::mem::offset_of!(PrivateSettings, shutdown_rtc_value) == 0x60);
    assert!(core::mem::offset_of!(PrivateSettings, external_steady_clock_internal_offset) == 0x68);
    assert!(core::mem::offset_of!(PrivateSettings, platform_region) == 0xD0);
    assert!(core::mem::size_of::<PrivateSettings>() == 0xD8);
};

impl Default for PrivateSettings {
    fn default() -> Self {
        // SAFETY: All-zero is valid for this repr(C) struct of plain data types.
        unsafe { core::mem::zeroed() }
    }
}

impl core::fmt::Debug for PrivateSettings {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PrivateSettings")
            .field("shutdown_rtc_value", &self.shutdown_rtc_value)
            .field(
                "external_steady_clock_internal_offset",
                &self.external_steady_clock_internal_offset,
            )
            .field("platform_region", &self.platform_region)
            .finish()
    }
}

impl PrivateSettings {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Corresponds to `DefaultPrivateSettings` in `private_settings.cpp`.
pub fn default_private_settings() -> PrivateSettings {
    PrivateSettings::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_payload_is_fully_zeroed() {
        let settings = default_private_settings();
        let bytes = unsafe {
            core::slice::from_raw_parts(
                (&settings as *const PrivateSettings).cast::<u8>(),
                core::mem::size_of::<PrivateSettings>(),
            )
        };

        assert_eq!(bytes.len(), 0xD8);
        assert!(bytes.iter().all(|byte| *byte == 0));
    }
}

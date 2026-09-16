//! Types from core/internal_network/wifi_scanner.h.

#[derive(Clone, Debug)]
#[repr(C)]
pub struct ScanData {
    pub ssid_len: u8,
    pub ssid: [u8; 0x21],
    pub quality: u8,
    pub padding: u8,
    pub flags: u32,
}

impl Default for ScanData {
    fn default() -> Self {
        Self { ssid_len: 0, ssid: [0; 0x21], quality: 0, padding: 0, flags: 0 }
    }
}

const _: () = assert!(std::mem::size_of::<ScanData>() == 0x28);

// Eden's default configuration has ENABLE_WIFI_SCAN=OFF. Preserve that
// production backend, rather than making a successful host scan up.
pub use super::wifi_scanner_dummy::scan_wifi_networks;

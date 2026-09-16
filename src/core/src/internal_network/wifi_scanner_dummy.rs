//! Default ENABLE_WIFI_SCAN=OFF backend from wifi_scanner_dummy.cpp.

pub fn scan_wifi_networks(_deadline: std::time::Duration) -> Vec<super::wifi_scanner::ScanData> {
    Vec::new()
}

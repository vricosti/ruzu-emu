// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/internal_network/network_interface.h and network_interface.cpp
//! Network interface enumeration.

use std::net::Ipv4Addr;

/// Host adapter transport reported to the emulated NIFM service.
///
/// Port of upstream `Network::HostAdapterKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostAdapterKind {
    Wifi,
    Ethernet,
}

/// Network interface information.
///
/// Corresponds to upstream `Network::NetworkInterface`.
#[derive(Debug, Clone)]
pub struct NetworkInterface {
    pub name: String,
    pub ip_address: Ipv4Addr,
    pub subnet_mask: Ipv4Addr,
    pub gateway: Ipv4Addr,
    pub kind: HostAdapterKind,
}

/// Get available network interfaces.
///
/// Corresponds to upstream `Network::GetAvailableNetworkInterfaces`.
///
/// On Linux, uses getifaddrs and reads /proc/net/route for gateway info.
/// On Windows, uses GetAdaptersAddresses.
pub fn get_available_network_interfaces() -> Vec<NetworkInterface> {
    #[cfg(target_os = "linux")]
    {
        get_available_network_interfaces_linux()
    }

    #[cfg(target_os = "windows")]
    {
        // TODO: Implement Windows version using GetAdaptersAddresses
        Vec::new()
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
fn get_available_network_interfaces_linux() -> Vec<NetworkInterface> {
    use std::ffi::CStr;

    let mut result = Vec::new();

    unsafe {
        let mut ifaddr: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifaddr) != 0 {
            log::error!("Failed to get network interfaces with getifaddrs");
            return result;
        }

        let mut ifa = ifaddr;
        while !ifa.is_null() {
            let iface = &*ifa;

            if iface.ifa_addr.is_null() || iface.ifa_netmask.is_null() {
                ifa = iface.ifa_next;
                continue;
            }

            if (*iface.ifa_addr).sa_family as i32 != libc::AF_INET {
                ifa = iface.ifa_next;
                continue;
            }

            if (iface.ifa_flags & libc::IFF_UP as u32) == 0
                || (iface.ifa_flags & libc::IFF_LOOPBACK as u32) != 0
            {
                ifa = iface.ifa_next;
                continue;
            }

            let name = CStr::from_ptr(iface.ifa_name).to_string_lossy().to_string();
            let ip_addr = sockaddr_to_ipv4(iface.ifa_addr);
            let subnet_mask = sockaddr_to_ipv4(iface.ifa_netmask);

            // Try to find gateway from /proc/net/route
            let gateway = find_gateway_linux(&name);

            result.push(NetworkInterface {
                name,
                ip_address: ip_addr,
                subnet_mask,
                gateway: Ipv4Addr::from(gateway),
                kind: HostAdapterKind::Ethernet,
            });

            ifa = iface.ifa_next;
        }

        libc::freeifaddrs(ifaddr);
    }

    result
}

#[cfg(target_os = "linux")]
unsafe fn sockaddr_to_ipv4(addr: *const libc::sockaddr) -> Ipv4Addr {
    let sin = addr as *const libc::sockaddr_in;
    let bytes = (*sin).sin_addr.s_addr.to_ne_bytes();
    Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3])
}

#[cfg(target_os = "linux")]
fn find_gateway_linux(iface_name: &str) -> u32 {
    use std::io::BufRead;

    let file = match std::fs::File::open("/proc/net/route") {
        Ok(f) => f,
        Err(_) => {
            log::error!("Failed to open /proc/net/route");
            return 0;
        }
    };

    let reader = std::io::BufReader::new(file);
    let mut lines = reader.lines();

    // Skip header
    let _ = lines.next();

    for line in lines {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }
        if parts[0] != iface_name {
            continue;
        }
        let dest = u32::from_str_radix(parts[1], 16).unwrap_or(u32::MAX);
        if dest != 0 {
            continue;
        }
        let gateway = u32::from_str_radix(parts[2], 16).unwrap_or(0);
        let flags = u16::from_str_radix(parts[3], 16).unwrap_or(0);
        // RTF_GATEWAY = 0x2
        if (flags & 0x2) == 0 {
            continue;
        }
        return gateway;
    }

    0
}

/// Get the currently selected network interface.
///
/// Corresponds to upstream `Network::GetSelectedNetworkInterface`.
pub fn get_selected_network_interface() -> Option<NetworkInterface> {
    let selected_name = common::settings::values()
        .network_interface
        .get_value()
        .clone();
    let interfaces = get_available_network_interfaces();
    if interfaces.is_empty() {
        log::error!("GetAvailableNetworkInterfaces returned no interfaces");
        return None;
    }
    let selected = select_network_interface(&interfaces, &selected_name);
    if selected.is_none() {
        log::error!("Selected network interface '{}' not found", selected_name);
    }
    selected
}

fn select_network_interface(
    interfaces: &[NetworkInterface],
    selected_name: &str,
) -> Option<NetworkInterface> {
    if selected_name.is_empty() {
        return interfaces.first().cloned();
    }
    interfaces
        .iter()
        .find(|interface| interface.name == selected_name)
        .cloned()
}

/// Select the first available network interface.
///
/// Corresponds to upstream `Network::SelectFirstNetworkInterface`.
pub fn select_first_network_interface() {
    let interfaces = get_available_network_interfaces();
    if interfaces.is_empty() {
        return;
    }
    common::settings::values_mut()
        .network_interface
        .set_value(interfaces[0].name.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interface(name: &str) -> NetworkInterface {
        NetworkInterface {
            name: name.to_string(),
            ip_address: Ipv4Addr::new(192, 0, 2, 1),
            subnet_mask: Ipv4Addr::new(255, 255, 255, 0),
            gateway: Ipv4Addr::new(192, 0, 2, 254),
            kind: HostAdapterKind::Ethernet,
        }
    }

    #[test]
    fn empty_selection_uses_first_interface_like_upstream() {
        let interfaces = [interface("eth0"), interface("wlan0")];
        assert_eq!(
            select_network_interface(&interfaces, "")
                .expect("first interface")
                .name,
            "eth0"
        );
    }

    #[test]
    fn named_selection_must_exist() {
        let interfaces = [interface("eth0"), interface("wlan0")];
        assert_eq!(
            select_network_interface(&interfaces, "wlan0")
                .expect("named interface")
                .name,
            "wlan0"
        );
        assert!(select_network_interface(&interfaces, "missing").is_none());
    }
}

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
        get_available_network_interfaces_windows()
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "windows")]
fn get_available_network_interfaces_windows() -> Vec<NetworkInterface> {
    use std::os::windows::ffi::OsStringExt;

    use winapi::shared::ifdef::IfOperStatusUp;
    use winapi::shared::ipifcons::IF_TYPE_IEEE80211;
    use winapi::shared::netioapi::ConvertLengthToIpv4Mask;
    use winapi::shared::winerror::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
    use winapi::shared::ws2def::AF_INET;
    use winapi::um::iphlpapi::GetAdaptersAddresses;
    use winapi::um::iptypes::{
        GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        IP_ADAPTER_ADDRESSES,
    };

    const FLAGS: u32 =
        GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER | GAA_FLAG_INCLUDE_GATEWAYS;

    let mut buffer_size = 0;
    let probe_result = unsafe {
        GetAdaptersAddresses(
            AF_INET as u32,
            FLAGS,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut buffer_size,
        )
    };
    if probe_result != ERROR_BUFFER_OVERFLOW {
        log::error!("GetAdaptersAddresses(overrun probe) failed");
        return Vec::new();
    }

    // `GetAdaptersAddresses` requires its caller-provided byte buffer to be
    // suitably aligned for `IP_ADAPTER_ADDRESSES`. A zeroed `usize` buffer
    // preserves Eden's zero-initialized byte vector while making that
    // alignment explicit in Rust.
    let word_count = (buffer_size as usize).div_ceil(std::mem::size_of::<usize>());
    let mut buffer = vec![0usize; word_count];
    let addresses = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES>();

    let data_result = unsafe {
        GetAdaptersAddresses(
            AF_INET as u32,
            FLAGS,
            std::ptr::null_mut(),
            addresses,
            &mut buffer_size,
        )
    };
    if data_result != NO_ERROR {
        log::error!("GetAdaptersAddresses(data) failed");
        return Vec::new();
    }

    let mut result = Vec::new();
    let mut adapter = addresses;
    while !adapter.is_null() {
        let current = unsafe { &*adapter };
        let unicast = current.FirstUnicastAddress;
        if current.OperStatus != IfOperStatusUp
            || unicast.is_null()
            || unsafe { (*unicast).Address.lpSockaddr.is_null() }
        {
            adapter = current.Next;
            continue;
        }

        let ip_address = unsafe { ipv4_from_windows_sockaddr((*unicast).Address.lpSockaddr) };

        let mut mask_raw = 0;
        if unsafe { ConvertLengthToIpv4Mask((*unicast).OnLinkPrefixLength as u32, &mut mask_raw) }
            != NO_ERROR
        {
            adapter = current.Next;
            continue;
        }
        let subnet_mask = Ipv4Addr::from(mask_raw.to_ne_bytes());

        let gateway = if current.FirstGatewayAddress.is_null()
            || unsafe { (*current.FirstGatewayAddress).Address.lpSockaddr.is_null() }
        {
            Ipv4Addr::UNSPECIFIED
        } else {
            unsafe { ipv4_from_windows_sockaddr((*current.FirstGatewayAddress).Address.lpSockaddr) }
        };

        let name = if current.FriendlyName.is_null() {
            String::new()
        } else {
            let mut length = 0;
            unsafe {
                while *current.FriendlyName.add(length) != 0 {
                    length += 1;
                }
                std::ffi::OsString::from_wide(std::slice::from_raw_parts(
                    current.FriendlyName,
                    length,
                ))
                .to_string_lossy()
                .into_owned()
            }
        };

        result.push(NetworkInterface {
            name,
            ip_address,
            subnet_mask,
            gateway,
            kind: if current.IfType == IF_TYPE_IEEE80211 {
                HostAdapterKind::Wifi
            } else {
                HostAdapterKind::Ethernet
            },
        });

        adapter = current.Next;
    }

    result
}

#[cfg(target_os = "windows")]
unsafe fn ipv4_from_windows_sockaddr(address: *mut winapi::shared::ws2def::SOCKADDR) -> Ipv4Addr {
    use winapi::shared::ws2def::SOCKADDR_IN;

    let address = &*(address.cast::<SOCKADDR_IN>());
    let octets = std::slice::from_raw_parts(
        (&address.sin_addr as *const winapi::shared::inaddr::IN_ADDR).cast::<u8>(),
        4,
    );
    Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3])
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
        return preferred_network_interface(interfaces).cloned();
    }
    interfaces
        .iter()
        .find(|interface| interface.name == selected_name)
        .cloned()
}

fn preferred_network_interface(interfaces: &[NetworkInterface]) -> Option<&NetworkInterface> {
    interfaces
        .iter()
        .find(|interface| {
            is_probable_physical_interface(interface) && interface.gateway != Ipv4Addr::UNSPECIFIED
        })
        .or_else(|| {
            interfaces
                .iter()
                .find(|interface| is_probable_physical_interface(interface))
        })
}

fn is_probable_physical_interface(interface: &NetworkInterface) -> bool {
    if interface.ip_address.is_unspecified()
        || interface.ip_address.is_loopback()
        || interface.ip_address.is_link_local()
        || interface.ip_address.is_multicast()
    {
        return false;
    }

    let name = interface.name.to_ascii_lowercase();
    const VIRTUAL_INTERFACE_MARKERS: &[&str] = &[
        "anyconnect",
        "docker",
        "fortinet",
        "hamachi",
        "hyper-v",
        "loopback",
        "nordlynx",
        "npcap",
        "openvpn",
        "podman",
        "protonvpn",
        "tap-windows",
        "tailscale",
        "tunnel",
        "vbox",
        "vethernet",
        "virtual",
        "vmnet",
        "vmware",
        "vpn",
        "warp",
        "wireguard",
        "wsl",
        "zerotier",
    ];
    !VIRTUAL_INTERFACE_MARKERS
        .iter()
        .any(|marker| name.contains(marker))
}

/// Select the first available network interface.
///
/// Corresponds to upstream `Network::SelectFirstNetworkInterface`.
pub fn select_first_network_interface() {
    let interfaces = get_available_network_interfaces();
    let Some(interface) = preferred_network_interface(&interfaces) else {
        return;
    };
    common::settings::values_mut()
        .network_interface
        .set_value(interface.name.clone());
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
    fn empty_selection_prefers_physical_interface_with_gateway() {
        let mut vmware = interface("VMware Network Adapter VMnet8");
        vmware.gateway = Ipv4Addr::UNSPECIFIED;
        let mut ethernet = interface("Ethernet");
        ethernet.gateway = Ipv4Addr::UNSPECIFIED;
        let wifi = interface("Wi-Fi");
        let interfaces = [vmware, ethernet, wifi];
        assert_eq!(
            select_network_interface(&interfaces, "")
                .expect("preferred interface")
                .name,
            "Wi-Fi"
        );
    }

    #[test]
    fn empty_selection_does_not_choose_virtual_vpn_or_loopback() {
        let mut loopback = interface("Loopback Pseudo-Interface 1");
        loopback.ip_address = Ipv4Addr::LOCALHOST;
        let interfaces = [
            loopback,
            interface("VMware Network Adapter VMnet8"),
            interface("Example VPN"),
        ];
        assert!(select_network_interface(&interfaces, "").is_none());
    }

    #[test]
    fn empty_selection_uses_physical_interface_without_gateway_as_fallback() {
        let mut ethernet = interface("Ethernet");
        ethernet.gateway = Ipv4Addr::UNSPECIFIED;
        let interfaces = [interface("VirtualBox Host-Only Network"), ethernet];
        assert_eq!(
            select_network_interface(&interfaces, "")
                .expect("physical fallback")
                .name,
            "Ethernet"
        );
    }

    #[test]
    fn named_selection_must_exist() {
        let interfaces = [interface("eth0"), interface("Example VPN")];
        assert_eq!(
            select_network_interface(&interfaces, "Example VPN")
                .expect("named interface")
                .name,
            "Example VPN"
        );
        assert!(select_network_interface(&interfaces, "missing").is_none());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_enumerates_active_ipv4_interfaces() {
        let interfaces = get_available_network_interfaces();
        assert!(
            !interfaces.is_empty(),
            "GetAdaptersAddresses returned no active IPv4 interfaces"
        );
        for interface in interfaces {
            assert!(!interface.name.is_empty());
            assert_ne!(interface.ip_address, Ipv4Addr::UNSPECIFIED);
        }
    }
}

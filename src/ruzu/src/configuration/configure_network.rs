// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/configure_network.cpp`
// (`ConfigureNetwork`), whose widget tree lives in `configure_network.ui`.
//
// A single "General" group with the network-interface picker. Upstream fills
// the combo directly from `Network::GetAvailableNetworkInterfaces()` and
// stores the *interface name* (not the index).

use gtk::prelude::*;

use super::configure_dialog::Page;
use super::shared_widget as w;

/// Build the Network tab — upstream `ConfigureNetwork`.
pub fn page() -> Page {
    let (scroller, column) = w::page();

    let (general_group, general) = w::group("General");

    let entries = available_network_interfaces();
    let entry_refs: Vec<&str> = entries.iter().map(String::as_str).collect();

    let configured = common::settings::values()
        .network_interface
        .get_value()
        .clone();
    let selected_name =
        ruzu_core::internal_network::network_interface::get_selected_network_interface()
            .map(|interface| interface.name)
            .or_else(|| (!configured.is_empty()).then_some(configured));
    let selected = selected_name
        .as_ref()
        .and_then(|selected_name| entries.iter().position(|name| name == selected_name));

    let (interface_row, interface) = w::combo_row(
        "Network Interface",
        &entry_refs,
        selected.unwrap_or(0) as u32,
    );
    if selected.is_none() {
        interface.set_selected(gtk::INVALID_LIST_POSITION);
    }
    general.append(&interface_row);
    let airplane_mode = w::check_row(
        "Enable Airplane Mode",
        *common::settings::values().airplane_mode.get_value(),
    );
    general.append(&airplane_mode);

    column.append(&general_group);

    Page::new("Network", scroller, move || {
        let index = interface.selected() as usize;
        let name = entries.get(index).cloned().unwrap_or_default();
        common::settings::values_mut()
            .network_interface
            .set_value(name);
        common::settings::values_mut()
            .airplane_mode
            .set_value(airplane_mode.is_active());
    })
}

/// Host network interface names — upstream
/// `Network::GetAvailableNetworkInterfaces()`.
///
/// Keep enumeration in the upstream-owned core counterpart; the frontend only
/// consumes interface names, exactly like `ConfigureNetwork` does.
fn available_network_interfaces() -> Vec<String> {
    interface_names_in_enumeration_order(
        ruzu_core::internal_network::network_interface::get_available_network_interfaces(),
    )
}

fn interface_names_in_enumeration_order(
    interfaces: Vec<ruzu_core::internal_network::network_interface::NetworkInterface>,
) -> Vec<String> {
    interfaces
        .into_iter()
        .map(|interface| interface.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_not_offered() {
        // Binding the emulated console to `lo` can never reach another host,
        // so upstream leaves it out of the picker.
        assert!(!available_network_interfaces().iter().any(|n| n == "lo"));
    }

    #[test]
    fn interface_names_preserve_core_enumeration_order() {
        use std::net::Ipv4Addr;

        let interface =
            |name: &str| ruzu_core::internal_network::network_interface::NetworkInterface {
                name: name.to_string(),
                ip_address: Ipv4Addr::UNSPECIFIED,
                subnet_mask: Ipv4Addr::UNSPECIFIED,
                gateway: Ipv4Addr::UNSPECIFIED,
                kind: ruzu_core::internal_network::network_interface::HostAdapterKind::Ethernet,
            };

        assert_eq!(
            interface_names_in_enumeration_order(vec![interface("wlan0"), interface("eth0")]),
            ["wlan0", "eth0"]
        );
    }
}

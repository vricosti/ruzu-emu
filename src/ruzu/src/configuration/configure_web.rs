// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/configure_web.cpp`
// (`ConfigureWeb`), whose widget tree lives in `configure_web.ui`.
//
// Two groups: the web-service credentials (username, token, Verify) and the
// telemetry opt-in with its regenerable telemetry ID.

use gtk::prelude::*;

use super::configure_dialog::Page;
use super::shared_widget as w;

/// Build the Web tab — upstream `ConfigureWeb`.
pub fn page() -> Page {
    let (scroller, column) = w::page();

    // --- "ruzu Web Service" ----------------------------------------------
    let (service_group, service) = w::group("ruzu Web Service");

    let consent = gtk::Label::new(Some(
        "By providing your username and token, you agree to allow ruzu to collect additional \
         usage data, which may include user identifying information.",
    ));
    consent.set_xalign(0.0);
    consent.set_wrap(true);
    service.append(&consent);

    let username_value = common::settings::values().eden_username.get_value().clone();
    let (username_row, username) = w::entry_row("Username:", &username_value);
    service.append(&username_row);

    let token_value = common::settings::values().eden_token.get_value().clone();
    let (token_row, token) = w::entry_row("Token:", &token_value);
    // Upstream sets `QLineEdit::Password` echo mode on the token field.
    token.set_visibility(false);
    service.append(&token_row);

    let links = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let sign_up = gtk::LinkButton::with_label("https://profile.yuzu-emu.org/", "Sign up");
    sign_up.set_has_frame(false);
    let what_is_token = gtk::LinkButton::with_label(
        "https://yuzu-emu.org/wiki/yuzu-web-service/",
        "What is my token?",
    );
    what_is_token.set_has_frame(false);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let verify = gtk::Button::with_label("Verify");
    links.append(&sign_up);
    links.append(&what_is_token);
    links.append(&spacer);
    links.append(&verify);
    service.append(&links);

    column.append(&service_group);

    // --- "Telemetry" ------------------------------------------------------
    let (telemetry_group, telemetry) = w::group("Telemetry");

    let share = w::check_row(
        "Share anonymous usage data with the ruzu team",
        *common::settings::values().enable_telemetry.get_value(),
    );
    telemetry.append(&share);

    let learn_more =
        gtk::LinkButton::with_label("https://yuzu-emu.org/help/feature/telemetry/", "Learn more");
    learn_more.set_has_frame(false);
    learn_more.set_halign(gtk::Align::Start);
    telemetry.append(&learn_more);

    let id_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let telemetry_id = gtk::Label::new(Some(&format!(
        "Telemetry ID: 0x{:016X}",
        current_telemetry_id()
    )));
    telemetry_id.set_xalign(0.0);
    telemetry_id.set_hexpand(true);
    let regenerate = gtk::Button::with_label("Regenerate");
    id_row.append(&telemetry_id);
    id_row.append(&regenerate);
    telemetry.append(&id_row);

    column.append(&telemetry_group);

    // Upstream's Verify posts the token to the web service and reports the
    // result; Regenerate calls `Core::RegenerateTelemetryId()`. Neither the web
    // service client nor the telemetry store is wired into ruzu yet.
    verify.connect_clicked(|_| {
        log::info!("Web: Verify requested (web service client not yet wired)");
    });
    regenerate.connect_clicked(|_| {
        log::info!("Web: Regenerate telemetry ID requested (telemetry store not yet wired)");
    });

    Page::new("Web", scroller, move || {
        let token_text = token.text().to_string();
        let telemetry_enabled = share.is_active();
        let mut values = common::settings::values_mut();
        values.eden_username.set_value(username.text().to_string());
        values.eden_token.set_value(token_text);
        values.enable_telemetry.set_value(telemetry_enabled);
    })
}

/// The telemetry ID upstream reads from `Core::GetTelemetryId()`. That store is
/// not ported, so report 0 rather than inventing an ID that would then differ
/// from whatever the real store eventually holds.
fn current_telemetry_id() -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display; run alone in its own test process"]
    fn web_page_edits_frontend_identity_without_overwriting_legacy_credentials() {
        gtk::init().expect("GTK display required");
        {
            let mut values = common::settings::values_mut();
            values.eden_username.set_value("InitialUser".into());
            values.eden_token.set_value("a".repeat(48));
            values.yuzu_username.set_value("LegacyUser".into());
            values.yuzu_token.set_value("legacy-token".into());
        }
        fn entries(widget: &gtk::Widget, output: &mut Vec<gtk::Entry>) {
            if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
                output.push(entry.clone());
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                entries(&widget, output);
            }
        }
        let page = page();
        let mut edits = Vec::new();
        entries(&page.widget, &mut edits);
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].text(), "InitialUser");
        assert_eq!(edits[1].text(), "a".repeat(48));
        edits[0].set_text("LocalUser");
        edits[1].set_text(&"b".repeat(48));
        (page.apply)();
        let values = common::settings::values();
        assert_eq!(values.eden_username.get_value(), "LocalUser");
        assert_eq!(values.eden_token.get_value(), &"b".repeat(48));
        assert_eq!(values.yuzu_username.get_value(), "LegacyUser");
        assert_eq!(values.yuzu_token.get_value(), "legacy-token");
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// Eden `src/yuzu/configuration/configure_web.cpp`
// (`ConfigureWeb`), whose widget tree lives in `configure_web.ui`.
//
// Local multiplayer identity validation and token generation. No telemetry
// controls: upstream removed them. Discord support is not compiled in Ruzu.

use gtk::prelude::*;

use super::configure_dialog::Page;
use super::shared_widget as w;

/// Build the Web tab — upstream `ConfigureWeb`.
pub fn page() -> Page {
    let (scroller, column) = w::page();

    // --- "ruzu Web Service" ----------------------------------------------
    let (service_group, service) = w::group("ruzu Web Service");

    let username_value = common::settings::values().eden_username.get_value().clone();
    let (username_row, username) = w::entry_row("Username:", &username_value);
    service.append(&username_row);

    let token_value = common::settings::values().eden_token.get_value().clone();
    let (token_row, token) = w::entry_row("Token:", &token_value);
    // Upstream sets `QLineEdit::Password` echo mode on the token field.
    token.set_visibility(false);
    service.append(&token_row);
    let username_status = gtk::Label::new(None);
    let token_status = gtk::Label::new(None);
    service.append(&username_status);
    service.append(&token_status);
    for (entry, status, is_token) in [
        (&username, &username_status, false), (&token, &token_status, true),
    ] {
        // Like QRegularExpressionValidator, permit incomplete input while
        // rejecting characters/lengths that cannot become an acceptable value.
        entry.connect_insert_text(move |entry, inserted, _| {
            let limit = if is_token { 48 } else { 20 };
            if entry.text().chars().count() + inserted.chars().count() > limit
                || (is_token && !inserted.bytes().all(|byte| byte.is_ascii_lowercase()))
                || (!is_token && inserted.contains('\n'))
            {
                entry.stop_signal_emission_by_name("insert-text");
            }
        });
        verify_login(entry, status, is_token);
        let status = status.clone();
        entry.connect_changed(move |entry| verify_login(entry, &status, is_token));
    }
    let generate = gtk::Button::with_label("Generate Token");
    let token_for_generate = token.clone();
    generate.connect_clicked(move |_| {
        match generate_token() {
            Ok(value) => token_for_generate.set_text(&value),
            Err(error) => token_status.set_text(&format!("Could not generate token: {error}")),
        }
    });
    service.append(&generate);
    column.append(&service_group);

    Page::new("Web", scroller, move || {
        let token_text = token.text().to_string();
        let mut values = common::settings::values_mut();
        values.eden_username.set_value(username.text().to_string());
        values.eden_token.set_value(token_text);
    })
}

fn verify_login(entry: &gtk::Entry, status: &gtk::Label, is_token: bool) {
    let text = entry.text();
    let valid = if is_token {
        text.len() == 48 && text.bytes().all(|byte| byte.is_ascii_lowercase())
    } else {
        (4..=20).contains(&text.chars().count()) && !text.contains('\n')
    };
    status.set_text(if valid { "All Good" } else if is_token {
        "Must be 48 characters, and lowercase a-z"
    } else { "Must be between 4-20 characters" });
}

/// ConfigureWeb::GenerateToken: system entropy replaces QRandomGenerator::system.
/// Reject the uneven tail before mapping bytes to the 26-letter alphabet.
fn generate_token() -> Result<String, getrandom::Error> {
    let mut result = String::with_capacity(48);
    while result.len() < 48 {
        let mut bytes = [0; 64];
        getrandom::getrandom(&mut bytes)?;
        for byte in bytes {
            if byte < 234 {
                result.push((b'a' + byte % 26) as char);
                if result.len() == 48 { break; }
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_match_upstream_alphabet_and_length() {
        for _ in 0..32 {
            let token = generate_token().unwrap();
            assert_eq!(token.len(), 48);
            assert!(token.bytes().all(|byte| byte.is_ascii_lowercase()));
        }
    }

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
        fn entries(widget: &gtk::Widget, output: &mut Vec<gtk::Entry>, buttons: &mut Vec<gtk::Button>) {
            if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
                output.push(entry.clone());
            }
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                if button.label().as_deref() == Some("Generate Token") {
                    buttons.push(button.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                entries(&widget, output, buttons);
            }
        }
        let page = page();
        let mut edits = Vec::new();
        let mut buttons = Vec::new();
        entries(&page.widget, &mut edits, &mut buttons);
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].text(), "InitialUser");
        assert_eq!(edits[1].text(), "a".repeat(48));
        assert_eq!(buttons.len(), 1);
        buttons[0].emit_clicked();
        assert_eq!(edits[1].text().len(), 48);
        assert!(edits[1].text().bytes().all(|byte| byte.is_ascii_lowercase()));
        // Generate changes only the draft, never the persistent setting.
        assert_eq!(common::settings::values().eden_token.get_value(), &"a".repeat(48));
        edits[1].set_text("");
        let mut position = 0;
        edits[1].insert_text("ABC123", &mut position);
        assert!(edits[1].text().is_empty());
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

// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/configure_input_advanced.cpp`
// (`ConfigureInputAdvanced`), whose widget tree lives in
// `configure_input_advanced.ui`.
//
// Two columns: "Joycon Colors" on the left (a 2x4 grid of per-player body /
// button colour swatches) and, on the right, "Emulated Devices" over "Other".

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;

use common::settings_input::PlayerInput;

use super::configure_dialog::Page;
use super::shared_widget as w;

/// Swatch size, matching `configure_input_advanced.ui`'s colour buttons.
const SWATCH_WIDTH: i32 = 70;
const SWATCH_HEIGHT: i32 = 26;

/// Build the Controls "Advanced" tab — upstream `ConfigureInputAdvanced`.
pub fn page(
    input_subsystem: Rc<RefCell<input_common::InputSubsystem>>,
    hid_core: Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
) -> Page {
    let (scroller, column) = w::page();

    let split = gtk::Box::new(gtk::Orientation::Horizontal, 10);

    // --- "Joycon Colors" --------------------------------------------------
    let (colors_group, colors) = w::group("Joycon Colors");
    colors_group.set_hexpand(true);

    let grid = gtk::Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(10);
    let mut controllers_color_buttons = Vec::new();
    for slot in 0..super::configure_input::NUM_PLAYERS {
        let player = player_input(slot);
        let (cell, buttons) = player_colors(slot, &player);
        controllers_color_buttons.push(buttons);
        grid.attach(&cell, (slot % 2) as i32, (slot / 2) as i32, 1, 1);
    }
    colors.append(&grid);
    split.append(&colors_group);

    // --- Right column -----------------------------------------------------
    let right = gtk::Box::new(gtk::Orientation::Vertical, 0);
    right.set_hexpand(true);

    let (devices_group, devices) = w::group("Emulated Devices");

    let keyboard = device_row(
        "Keyboard",
        *common::settings::values().keyboard_enabled.get_value(),
        None,
    );
    let mouse = device_row(
        "Mouse",
        *common::settings::values().mouse_enabled.get_value(),
        None,
    );
    let touchscreen = device_row(
        "Touchscreen",
        common::settings::values().touchscreen.enabled,
        Some("Advanced"),
    );
    let debug_controller = device_row(
        "Debug Controller",
        *common::settings::values().debug_pad_enabled.get_value(),
        Some("Configure"),
    );
    let ring_controller = device_row(
        "Ring Controller",
        *common::settings::values()
            .enable_ring_controller
            .get_value(),
        Some("Configure"),
    );
    let infrared = device_row(
        "Infrared Camera",
        *common::settings::values().enable_ir_sensor.get_value(),
        Some("Configure"),
    );
    // Upstream ships the IR camera row permanently disabled (no backend).
    infrared.row.set_sensitive(false);

    for device in [
        &keyboard,
        &mouse,
        &touchscreen,
        &debug_controller,
        &ring_controller,
        &infrared,
    ] {
        devices.append(&device.row);
    }
    right.append(&devices_group);

    // --- "Other" ----------------------------------------------------------
    let (other_group, other) = w::group("Other");

    let emulate_analog = w::check_row(
        "Emulate Analog with Keyboard Input",
        *common::settings::values()
            .emulate_analog_keyboard
            .get_value(),
    );
    let disable_wgi_xinput = w::check_row(
        "Disable SDL WGI/XInput (Requires restart)",
        *common::settings::values().disable_wgi_xinput.get_value(),
    );
    disable_wgi_xinput.set_tooltip_text(Some(
        "Aimed to disable SDL GUIDE button hack: synthetic GUIDE(HOME) event when SELECT(MINUS) + START(PLUS) pressed. May impact Win related trigger/rumble/etc stuff",
    ));
    disable_wgi_xinput.set_visible(cfg!(target_os = "windows"));
    let udp_controllers = w::check_row(
        "Enable UDP controllers (not needed for motion)",
        *common::settings::values().enable_udp_controller.get_value(),
    );
    let controller_navigation = w::check_row(
        "Controller navigation",
        *common::settings::values().controller_navigation.get_value(),
    );
    let joycon_driver = w::check_row(
        "Enable direct JoyCon driver",
        *common::settings::values().enable_joycon_driver.get_value(),
    );
    let procon_driver = w::check_row(
        "Enable direct Pro Controller driver [EXPERIMENTAL]",
        *common::settings::values().enable_procon_driver.get_value(),
    );
    let random_amiibo = w::check_row(
        "Use random Amiibo ID",
        *common::settings::values().random_amiibo_id.get_value(),
    );
    for check in [
        &emulate_analog,
        &disable_wgi_xinput,
        &udp_controllers,
        &controller_navigation,
        &joycon_driver,
        &procon_driver,
        &random_amiibo,
    ] {
        other.append(check);
    }

    let motion_touch = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let motion_touch_label = gtk::Label::new(Some("Motion / Touch"));
    motion_touch_label.set_xalign(0.0);
    motion_touch_label.set_hexpand(true);
    let configure_motion_touch = gtk::Button::with_label("Configure");
    motion_touch.append(&motion_touch_label);
    motion_touch.append(&configure_motion_touch);
    other.append(&motion_touch);

    right.append(&other_group);
    split.append(&right);

    column.append(&split);

    // The remaining per-device Configure dialogs are separate upstream
    // widgets; log until their matching owners are ported.
    for (button, name) in [
        (&touchscreen.configure, "Touchscreen advanced"),
        (&debug_controller.configure, "Debug controller"),
        (&ring_controller.configure, "Ring controller"),
        (&infrared.configure, "Infrared camera"),
    ] {
        let Some(button) = button else { continue };
        let name = name.to_string();
        button.connect_clicked(move |_| {
            log::info!("Controls: {name} configuration not yet ported");
        });
    }
    configure_motion_touch.connect_clicked(move |button| {
        super::configure_motion_touch::present(button, Rc::clone(&input_subsystem));
    });

    Page::new("Advanced", scroller, move || {
        apply_controller_colors(&controllers_color_buttons, &hid_core);
        let mut values = common::settings::values_mut();
        values
            .keyboard_enabled
            .set_value(keyboard.check.is_active());
        values.mouse_enabled.set_value(mouse.check.is_active());
        values.touchscreen.enabled = touchscreen.check.is_active();
        values
            .debug_pad_enabled
            .set_value(debug_controller.check.is_active());
        values
            .enable_ring_controller
            .set_value(ring_controller.check.is_active());
        values
            .enable_ir_sensor
            .set_value(infrared.check.is_active());

        values
            .emulate_analog_keyboard
            .set_value(emulate_analog.is_active());
        values
            .disable_wgi_xinput
            .set_value(disable_wgi_xinput.is_active());
        values
            .enable_udp_controller
            .set_value(udp_controllers.is_active());
        values
            .controller_navigation
            .set_value(controller_navigation.is_active());
        values
            .enable_joycon_driver
            .set_value(joycon_driver.is_active());
        values
            .enable_procon_driver
            .set_value(procon_driver.is_active());
        values.random_amiibo_id.set_value(random_amiibo.is_active());
    })
}

/// One "Emulated Devices" row: a check box plus an optional Configure button.
struct DeviceRow {
    row: gtk::Box,
    check: gtk::CheckButton,
    configure: Option<gtk::Button>,
}

fn device_row(label: &str, active: bool, configure_label: Option<&str>) -> DeviceRow {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let check = gtk::CheckButton::with_label(label);
    check.set_active(active);
    check.set_hexpand(true);
    row.append(&check);

    let configure = configure_label.map(|text| {
        let button = gtk::Button::with_label(text);
        // Upstream keeps each Configure disabled until its device is enabled.
        button.set_sensitive(active);
        row.append(&button);
        button
    });

    if let Some(button) = &configure {
        let button = button.clone();
        check.connect_toggled(move |check| button.set_sensitive(check.is_active()));
    }

    DeviceRow {
        row,
        check,
        configure,
    }
}

/// One player's four colour swatches, laid out as `configure_input_advanced.ui`
/// arranges them: L/R Body over L/R Button.
fn player_colors(index: usize, player: &PlayerInput) -> (gtk::Box, [gtk::ColorButton; 4]) {
    let cell = gtk::Box::new(gtk::Orientation::Vertical, 4);

    let title = gtk::Label::new(Some(&format!("Player {}", index + 1)));
    title.set_xalign(0.0);
    cell.append(&title);

    let frame = gtk::Frame::new(None);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.set_margin_top(6);
    content.set_margin_bottom(6);
    content.set_margin_start(8);
    content.set_margin_end(8);

    let (body_row, [left_body, right_body]) = swatch_pair(
        "L Body",
        player.body_color_left,
        "R Body",
        player.body_color_right,
    );
    content.append(&body_row);
    let (button_row, [left_button, right_button]) = swatch_pair(
        "L Button",
        player.button_color_left,
        "R Button",
        player.button_color_right,
    );
    content.append(&button_row);

    frame.set_child(Some(&content));
    cell.append(&frame);
    (cell, [left_body, left_button, right_body, right_button])
}

/// Two captioned swatches side by side.
fn swatch_pair(left_label: &str, left: u32, right_label: &str, right: u32) -> (gtk::Box, [gtk::ColorButton; 2]) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let buttons = [(left_label, left), (right_label, right)].map(|(label, color)| {
        let block = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let caption = gtk::Label::new(Some(label));
        let button = gtk::ColorButton::with_rgba(&rgba_from_u32(color));
        button.set_size_request(SWATCH_WIDTH, SWATCH_HEIGHT);
        block.append(&caption);
        block.append(&button);
        row.append(&block);
        button
    });
    (row, buttons)
}

/// Color portion of upstream ApplyConfiguration. Release settings before
/// ReloadColorsFromSettings, which reads them itself.
fn apply_controller_colors(
    buttons: &[[gtk::ColorButton; 4]],
    hid_core: &Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
) {
    for (index, buttons) in buttons.iter().enumerate() {
        let colors = buttons.each_ref().map(|button| packed_rgb(&button.rgba()));
        {
            let mut settings = common::settings::values_mut();
            let player = &mut settings.players.get_value_mut()[index];
            player.body_color_left = colors[0];
            player.button_color_left = colors[1];
            player.body_color_right = colors[2];
            player.button_color_right = colors[3];
        }
        let controller = hid_core.lock().get_emulated_controller_by_index(index);
        controller.lock().reload_colors_from_settings();
    }
}

/// QColor::rgb returns opaque ARGB even when the stored input had no alpha.
fn packed_rgb(color: &gtk::gdk::RGBA) -> u32 {
    let channel = |value: f32| (value * 255.0).round() as u32;
    0xFF00_0000 | (channel(color.red()) << 16) | (channel(color.green()) << 8) | channel(color.blue())
}

/// Convert a packed `0xRRGGBB` colour into a GDK colour.
///
/// `Settings` stores Joy-Con colours the way the console reports them: 24-bit
/// RGB in the low bytes, high byte unused.
fn rgba_from_u32(color: u32) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::new(
        ((color >> 16) & 0xFF) as f32 / 255.0,
        ((color >> 8) & 0xFF) as f32 / 255.0,
        (color & 0xFF) as f32 / 255.0,
        1.0,
    )
}

/// Read player `index`'s stored input configuration.
fn player_input(index: usize) -> PlayerInput {
    common::settings::values()
        .players
        .get_value()
        .get(index)
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joycon_neon_colors_decode_to_their_rgb_channels() {
        // `JOYCON_BODY_NEON_BLUE` is 0x0AB9E6 upstream; a byte-order slip would
        // show the default Joy-Con as orange instead of blue.
        let rgba = rgba_from_u32(0x0AB9E6);
        assert!((rgba.red() - 0x0A as f32 / 255.0).abs() < 1e-6);
        assert!((rgba.green() - 0xB9 as f32 / 255.0).abs() < 1e-6);
        assert!((rgba.blue() - 0xE6 as f32 / 255.0).abs() < 1e-6);
        assert_eq!(rgba.alpha(), 1.0);
    }

    #[test]
    fn black_and_white_round_trip() {
        assert_eq!(rgba_from_u32(0x000000).red(), 0.0);
        assert_eq!(rgba_from_u32(0xFFFFFF).blue(), 1.0);
    }

    #[test]
    fn color_conversion_preserves_every_channel_byte_like_qcolor_rgb() {
        for value in 0..=255 {
            for color in [value, value << 8, value << 16] {
                assert_eq!(packed_rgb(&rgba_from_u32(color)), 0xFF00_0000 | color);
            }
        }
    }

    #[test]
    #[ignore = "requires a GTK display; run alone with --ignored"]
    fn chosen_controller_colors_apply_to_settings_and_live_controller() {
        gtk::init().unwrap();
        let hid = Arc::new(parking_lot::Mutex::new(hid_core::hid_core::HIDCore::new()));
        let player = player_input(0);
        let (_widget, buttons) = player_colors(0, &player);
        let colors = [0x123456, 0x789ABC, 0xDEF012, 0x345678];
        for (button, color) in buttons.iter().zip(colors) {
            button.set_rgba(&rgba_from_u32(color));
        }
        // A draft selection must not leak through Cancel.
        assert_eq!(player_input(0).body_color_left, player.body_color_left);
        apply_controller_colors(&[buttons], &hid);
        let stored = player_input(0);
        assert_eq!([stored.body_color_left, stored.button_color_left,
            stored.body_color_right, stored.button_color_right],
            colors.map(|color| 0xFF00_0000 | color));
        let device = hid.lock().get_emulated_controller_by_index(0);
        let actual = device.lock().get_colors();
        for (actual, expected) in [actual.left.body, actual.left.button,
            actual.right.body, actual.right.button].into_iter().zip(colors) {
            assert_eq!([actual.r, actual.g, actual.b],
                [(expected >> 16) as u8, (expected >> 8) as u8, expected as u8]);
        }
    }
}

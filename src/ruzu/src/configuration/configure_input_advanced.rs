// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `eden/src/yuzu/configuration/configure_input_advanced.cpp`
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
    profiles: Rc<super::configure_input_player::InputProfileContext>,
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
    // Unlike Eden, this frontend does not embed Qt WebEngine, so its
    // "disables web applet" qualifier does not apply to this checkbox.
    let raw_input = w::check_row(
        "Enable XInput 8 player support (Requires restart)",
        *common::settings::values().enable_raw_input.get_value(),
    );
    raw_input.set_visible(cfg!(target_os = "windows"));
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
        &raw_input,
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
        (&ring_controller.configure, "Ring controller"),
        (&infrared.configure, "Infrared camera"),
    ] {
        let Some(button) = button else { continue };
        let name = name.to_string();
        button.connect_clicked(move |_| {
            log::info!("Controls: {name} configuration not yet ported");
        });
    }
    if let Some(button) = &debug_controller.configure {
        let input = Rc::clone(&input_subsystem);
        let hid_core = Arc::clone(&hid_core);
        button.connect_clicked(move |button| {
            super::configure_debug_controller::present(button, Rc::clone(&input), std::sync::Arc::clone(&hid_core), Rc::clone(&profiles));
        });
    }
    configure_motion_touch.connect_clicked(move |button| {
        super::configure_motion_touch::present(button, Rc::clone(&input_subsystem));
    });

    if let Some(button) = &touchscreen.configure {
        button.connect_clicked(super::configure_touchscreen_advanced::present);
    }

    Page::new("Advanced", scroller, move || {
        for (index, buttons) in controllers_color_buttons.iter().enumerate() {
            let colors = std::array::from_fn(|i| rgb_from_rgba(&buttons[i].rgba()));
            {
                let mut values = common::settings::values_mut();
                set_player_colors(&mut values.players.get_value_mut()[index], colors);
            }
            // Release the settings guard before the controller reads Settings.
            let controller = hid_core.lock().get_emulated_controller_by_index(index);
            controller.lock().reload_colors_from_settings();
        }
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
        values.enable_raw_input.set_value(raw_input.is_active());
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

    let (body_row, body) = swatch_pair(
        "L Body",
        player.body_color_left,
        "R Body",
        player.body_color_right,
    );
    content.append(&body_row);
    let (button_row, buttons) = swatch_pair(
        "L Button",
        player.button_color_left,
        "R Button",
        player.button_color_right,
    );
    content.append(&button_row);

    frame.set_child(Some(&content));
    cell.append(&frame);
    // Same order as upstream controllers_color_buttons, not visual row order.
    (cell, [body[0].clone(), buttons[0].clone(), body[1].clone(), buttons[1].clone()])
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

/// QColor::rgb() always returns opaque ARGB, irrespective of picker alpha.
fn rgb_from_rgba(color: &gtk::gdk::RGBA) -> u32 {
    let channel = |value: f32| (value * 255.0).round() as u32;
    0xFF00_0000 | channel(color.red()) << 16 | channel(color.green()) << 8 | channel(color.blue())
}

/// Field assignments from ConfigureInputAdvanced::ApplyConfiguration.
fn set_player_colors(player: &mut PlayerInput, colors: [u32; 4]) {
    player.body_color_left = colors[0];
    player.button_color_left = colors[1];
    player.body_color_right = colors[2];
    player.button_color_right = colors[3];
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

    fn page(input: Rc<RefCell<input_common::InputSubsystem>>, hid: Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>) -> Page {
        super::page(input, hid, Rc::new(super::super::configure_input_player::InputProfileContext::new(
            super::super::input_profiles::InputProfiles::new(),
        )))
    }

    #[test]
    #[ignore = "requires a GTK display; run alone with --ignored"]
    fn raw_input_checkbox_loads_applies_and_has_platform_visibility() {
        fn find(widget: &gtk::Widget) -> Option<gtk::CheckButton> {
            if let Some(check) = widget.downcast_ref::<gtk::CheckButton>() {
                if check.label().is_some_and(|label| label.starts_with("Enable XInput 8")) {
                    return Some(check.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(check) = find(&widget) { return Some(check); }
                child = widget.next_sibling();
            }
            None
        }
        gtk::init().unwrap();
        let input = Rc::new(RefCell::new(input_common::InputSubsystem::new()));
        let hid = Arc::new(parking_lot::Mutex::new(hid_core::hid_core::HIDCore::new()));
        for initial in [false, true] {
            common::settings::values_mut().enable_raw_input.set_value(initial);
            let page = page(Rc::clone(&input), Arc::clone(&hid));
            let check = find(&page.widget).expect("raw input checkbox");
            assert_eq!(check.is_visible(), cfg!(target_os = "windows"));
            assert_eq!(check.is_active(), initial);
            check.set_active(!initial);
            assert_eq!(*common::settings::values().enable_raw_input.get_value(), initial);
            (page.apply)();
            assert_eq!(*common::settings::values().enable_raw_input.get_value(), !initial);
        }
    }

    #[test]
    #[ignore = "requires a GTK display; run alone with --ignored"]
    fn advanced_color_pickers_apply_to_settings_and_live_controllers() {
        fn collect(widget: &gtk::Widget, buttons: &mut Vec<gtk::ColorButton>) {
            if let Some(button) = widget.downcast_ref::<gtk::ColorButton>() {
                buttons.push(button.clone());
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                collect(&widget, buttons);
                child = widget.next_sibling();
            }
        }

        gtk::init().unwrap();
        let input = Rc::new(RefCell::new(input_common::InputSubsystem::new()));
        let hid = Arc::new(parking_lot::Mutex::new(hid_core::hid_core::HIDCore::new()));
        let page = page(Rc::clone(&input), Arc::clone(&hid));
        let mut buttons = Vec::new();
        collect(&page.widget, &mut buttons);
        assert_eq!(buttons.len(), 32);
        for (index, button) in buttons.iter().enumerate() {
            button.set_rgba(&rgba_from_u32(0x123400 + index as u32));
        }
        (page.apply)();
        for index in 0..8 {
            let base = 0xFF123400 + index as u32 * 4;
            let player = player_input(index);
            // Tree order is left/right body, then left/right buttons.
            assert_eq!(player.body_color_left, base);
            assert_eq!(player.body_color_right, base + 1);
            assert_eq!(player.button_color_left, base + 2);
            assert_eq!(player.button_color_right, base + 3);
            let controller = hid.lock().get_emulated_controller_by_index(index);
            let colors = controller.lock().get_colors();
            assert_eq!(colors.left.body.b, (base & 255) as u8);
            assert_eq!(colors.right.body.b, ((base + 1) & 255) as u8);
            assert_eq!(colors.left.button.b, ((base + 2) & 255) as u8);
            assert_eq!(colors.right.button.b, ((base + 3) & 255) as u8);
        }
        // Editing without Apply must not alter settings (Cancel).
        buttons[0].set_rgba(&rgba_from_u32(0));
        drop(page);
        assert_eq!(player_input(0).body_color_left, 0xFF123400);
        let reopened = self::page(input, hid);
        let mut restored = Vec::new();
        collect(&reopened.widget, &mut restored);
        assert_eq!(rgb_from_rgba(&restored[0].rgba()), 0xFF123400);
    }

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
    fn picker_colors_preserve_rgb_and_upstream_field_order() {
        let colors = [0x123456, 0xABCDEF, 0x0AB9E6, 0xFF3C28];
        let mut player = PlayerInput::default();
        let original_buttons = player.buttons.clone();
        set_player_colors(&mut player, colors.map(|color| rgb_from_rgba(&rgba_from_u32(color))));
        assert_eq!(player.body_color_left, 0xFF123456);
        assert_eq!(player.button_color_left, 0xFFABCDEF);
        assert_eq!(player.body_color_right, 0xFF0AB9E6);
        assert_eq!(player.button_color_right, 0xFFFF3C28);
        assert_eq!(player.buttons, original_buttons);
        for channel in 0..=255 {
            let rgb = channel * 0x010101;
            assert_eq!(rgb_from_rgba(&rgba_from_u32(rgb)), 0xFF000000 | rgb);
        }
    }
}

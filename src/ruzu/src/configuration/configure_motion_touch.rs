// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/configure_motion_touch.cpp`.

use std::cell::{Cell, RefCell};
use std::net::Ipv4Addr;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use common::param_package::ParamPackage;
use common::settings::TouchFromButtonMap;
use gtk::prelude::*;
use input_common::drivers::udp_client::{self, CalibrationConfigurationJob, CalibrationStatus};

#[derive(Clone, Copy)]
struct Calibration {
    min_x: i32,
    min_y: i32,
    max_x: i32,
    max_y: i32,
}

impl Calibration {
    fn current() -> Self {
        let values = common::settings::values();
        let params = ParamPackage::from_serialized(values.touch_device.get_value());
        Self {
            min_x: params.get_int("min_x", 100),
            min_y: params.get_int("min_y", 50),
            max_x: params.get_int("max_x", 1800),
            max_y: params.get_int("max_y", 850),
        }
    }

    fn text(self) -> String {
        format!(
            "({}, {}) - ({}, {})",
            self.min_x, self.min_y, self.max_x, self.max_y
        )
    }
}

pub fn present(
    source: &impl IsA<gtk::Widget>,
    input_subsystem: Rc<RefCell<input_common::InputSubsystem>>,
) {
    let window = gtk::Window::builder()
        .title("Configure Motion / Touch")
        .modal(true)
        .resizable(false)
        .default_width(600)
        .build();
    if let Some(parent) = source.root().and_downcast::<gtk::Window>() {
        window.set_transient_for(Some(&parent));
    }

    let values = common::settings::values();
    let servers = Rc::new(RefCell::new(
        values
            .udp_input_servers
            .get_value()
            .split(',')
            .filter(|server| !server.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>(),
    ));
    let touch_maps = Rc::new(RefCell::new(values.touch_from_button_maps.clone()));
    if touch_maps.borrow().is_empty() {
        touch_maps.borrow_mut().push(TouchFromButtonMap {
            name: "default".to_string(),
            buttons: Vec::new(),
        });
    }
    let touch_map_index = Rc::new(Cell::new(
        (*values.touch_from_button_map_index.get_value())
            .clamp(0, touch_maps.borrow().len().saturating_sub(1) as i32) as usize,
    ));
    drop(values);
    let calibration = Rc::new(Cell::new(Calibration::current()));

    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let udp_frame = gtk::Frame::new(Some("Cemuhook UDP Config"));
    let udp_content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    udp_content.set_margin_top(8);
    udp_content.set_margin_bottom(8);
    udp_content.set_margin_start(8);
    udp_content.set_margin_end(8);
    let description = gtk::Label::new(Some(
        "You may use any Cemuhook compatible UDP input source to provide motion and touch input.",
    ));
    description.set_wrap(true);
    description.set_xalign(0.0);
    udp_content.append(&description);
    let learn_more = gtk::LinkButton::with_label(
        "https://yuzu-emu.org/wiki/using-a-controller-or-android-phone-for-motion-or-touch-input",
        "Learn More",
    );
    learn_more.set_halign(gtk::Align::Start);
    udp_content.append(&learn_more);

    let server_list = gtk::ListBox::new();
    server_list.set_selection_mode(gtk::SelectionMode::Single);
    server_list.set_height_request(110);
    refresh_servers(&server_list, &servers);
    udp_content.append(&server_list);

    let server_grid = gtk::Grid::new();
    server_grid.set_row_spacing(6);
    server_grid.set_column_spacing(6);
    let host = gtk::Entry::new();
    host.set_text("127.0.0.1");
    let port = gtk::Entry::new();
    port.set_text("26760");
    server_grid.attach(&gtk::Label::new(Some("Server:")), 0, 0, 1, 1);
    server_grid.attach(&host, 1, 0, 1, 1);
    server_grid.attach(&gtk::Label::new(Some("Port:")), 0, 1, 1, 1);
    server_grid.attach(&port, 1, 1, 1, 1);
    udp_content.append(&server_grid);

    let udp_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let test = gtk::Button::with_label("Test");
    let add = gtk::Button::with_label("Add Server");
    let remove = gtk::Button::with_label("Remove Server");
    udp_actions.append(&test);
    udp_actions.append(&add);
    udp_actions.append(&remove);
    udp_content.append(&udp_actions);
    udp_frame.set_child(Some(&udp_content));
    content.append(&udp_frame);

    let touch_frame = gtk::Frame::new(Some("Touch"));
    let touch_grid = gtk::Grid::new();
    touch_grid.set_row_spacing(8);
    touch_grid.set_column_spacing(8);
    touch_grid.set_margin_top(8);
    touch_grid.set_margin_bottom(8);
    touch_grid.set_margin_start(8);
    touch_grid.set_margin_end(8);
    let calibration_label = gtk::Label::new(Some(&calibration.get().text()));
    calibration_label.set_xalign(0.0);
    calibration_label.set_hexpand(true);
    let configure_calibration = gtk::Button::with_label("Configure");
    touch_grid.attach(&gtk::Label::new(Some("Touch Calibration:")), 0, 0, 1, 1);
    touch_grid.attach(&calibration_label, 1, 0, 1, 1);
    touch_grid.attach(&configure_calibration, 2, 0, 1, 1);

    let touch_map = gtk::ComboBoxText::new();
    for map in touch_maps.borrow().iter() {
        touch_map.append_text(&map.name);
    }
    touch_map.set_active(Some(touch_map_index.get() as u32));
    let configure_touch_map = gtk::Button::with_label("Configure");
    touch_grid.attach(&gtk::Label::new(Some("Touch From Button Map:")), 0, 1, 1, 1);
    touch_grid.attach(&super::shared_widget::popup_safe_combo(&touch_map), 1, 1, 1, 1);
    touch_grid.attach(&configure_touch_map, 2, 1, 1, 1);
    touch_frame.set_child(Some(&touch_grid));
    content.append(&touch_frame);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("OK");
    ok.add_css_class("suggested-action");
    actions.append(&cancel);
    actions.append(&ok);
    content.append(&actions);
    window.set_child(Some(&content));

    {
        let servers = Rc::clone(&servers);
        let server_list = server_list.clone();
        let window = window.downgrade();
        let host = host.clone();
        let port = port.clone();
        add.connect_clicked(move |_| {
            let host_text = host.text().to_string();
            let Ok(port_number) = port.text().parse::<i32>() else {
                show_warning(&window, "Port number has invalid characters");
                return;
            };
            if !(0..=65353).contains(&port_number) {
                show_warning(&window, "Port has to be in range 0 and 65353");
                return;
            }
            if host_text.parse::<Ipv4Addr>().is_err() {
                show_warning(&window, "IP address is not valid");
                return;
            }
            let server = format!("{host_text}:{port_number}");
            if servers.borrow().contains(&server) {
                show_warning(&window, "This UDP server already exists");
                return;
            }
            if servers.borrow().len() == 8 {
                show_warning(&window, "Unable to add more than 8 servers");
                return;
            }
            servers.borrow_mut().push(server);
            refresh_servers(&server_list, &servers);
        });
    }
    {
        let servers = Rc::clone(&servers);
        let server_list = server_list.clone();
        remove.connect_clicked(move |_| {
            let Some(row) = server_list.selected_row() else {
                return;
            };
            servers.borrow_mut().remove(row.index() as usize);
            refresh_servers(&server_list, &servers);
        });
    }
    let udp_test_in_progress = Rc::new(Cell::new(false));
    {
        let window = window.downgrade();
        let host = host.clone();
        let port = port.clone();
        let udp_test_in_progress = Rc::clone(&udp_test_in_progress);
        test.connect_clicked(move |button| {
            let Ok(port_number) = port.text().parse::<i32>() else {
                show_warning(&window, "Port number has invalid characters");
                return;
            };
            if !(0..=65353).contains(&port_number) {
                show_warning(&window, "Port has to be in range 0 and 65353");
                return;
            }
            button.set_sensitive(false);
            button.set_label("Testing");
            udp_test_in_progress.set(true);
            let (sender, receiver) = mpsc::channel();
            udp_client::test_communication(
                &host.text(),
                port_number as u16,
                Box::new({
                    let sender = sender.clone();
                    move || {
                        let _ = sender.send(true);
                    }
                }),
                Box::new(move || {
                    let _ = sender.send(false);
                }),
            );
            let button = button.clone();
            let window = window.clone();
            let udp_test_in_progress = Rc::clone(&udp_test_in_progress);
            gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
                let result = match receiver.try_recv() {
                    Ok(result) => result,
                    Err(mpsc::TryRecvError::Empty) => return gtk::glib::ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        button.set_sensitive(true);
                        button.set_label("Test");
                        udp_test_in_progress.set(false);
                        return gtk::glib::ControlFlow::Break;
                    }
                };
                button.set_sensitive(true);
                button.set_label("Test");
                udp_test_in_progress.set(false);
                if result {
                    show_message(
                        &window,
                        "Test Successful",
                        "Successfully received data from the server.",
                    );
                } else {
                    show_warning(&window, "Could not receive valid data from the server.");
                }
                gtk::glib::ControlFlow::Break
            });
        });
    }
    {
        let calibration = Rc::clone(&calibration);
        let calibration_label = calibration_label.clone();
        let host = host.clone();
        let port = port.clone();
        configure_calibration.connect_clicked(move |button| {
            let Ok(port_number) = port.text().parse::<i32>() else {
                return;
            };
            if !(0..=65353).contains(&port_number) {
                return;
            }
            present_calibration(
                button,
                host.text().to_string(),
                port_number as u16,
                Rc::clone(&calibration),
                calibration_label.clone(),
            );
        });
    }
    {
        let touch_maps = Rc::clone(&touch_maps);
        let touch_map_index = Rc::clone(&touch_map_index);
        let touch_map = touch_map.clone();
        let input_subsystem = Rc::clone(&input_subsystem);
        configure_touch_map.connect_clicked(move |button| {
            let touch_maps_result = Rc::clone(&touch_maps);
            let index_result = Rc::clone(&touch_map_index);
            let combo = touch_map.clone();
            super::configure_touch_from_button::present(
                button,
                touch_maps.borrow().clone(),
                Rc::clone(&input_subsystem),
                touch_map_index.get(),
                move |maps, index| {
                    *touch_maps_result.borrow_mut() = maps;
                    index_result.set(index);
                    combo.remove_all();
                    for map in touch_maps_result.borrow().iter() {
                        combo.append_text(&map.name);
                    }
                    combo.set_active(Some(index as u32));
                },
            );
        });
    }
    {
        let touch_map_index = Rc::clone(&touch_map_index);
        touch_map.connect_changed(move |combo| {
            if let Some(index) = combo.active() {
                touch_map_index.set(index as usize);
            }
        });
    }
    {
        let window = window.downgrade();
        let udp_test_in_progress = Rc::clone(&udp_test_in_progress);
        cancel.connect_clicked(move |_| {
            if udp_test_in_progress.get() {
                show_warning(
                    &window,
                    "UDP Test or calibration configuration is in progress. Please wait for it to finish.",
                );
                return;
            }
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }
    {
        let window = window.downgrade();
        let udp_test_in_progress = Rc::clone(&udp_test_in_progress);
        ok.connect_clicked(move |_| {
            if udp_test_in_progress.get() {
                show_warning(
                    &window,
                    "UDP Test or calibration configuration is in progress. Please wait for it to finish.",
                );
                return;
            }
            let current = calibration.get();
            let mut touch = ParamPackage::default();
            touch.set_int("min_x", current.min_x);
            touch.set_int("min_y", current.min_y);
            touch.set_int("max_x", current.max_x);
            touch.set_int("max_y", current.max_y);
            {
                let mut values = common::settings::values_mut();
                values.touch_device.set_value(touch.serialize());
                values
                    .touch_from_button_map_index
                    .set_value(touch_map_index.get() as i32);
                values.touch_from_button_maps = touch_maps.borrow().clone();
                values
                    .udp_input_servers
                    .set_value(servers.borrow().join(","));
            }
            input_subsystem.borrow_mut().reload_input_devices();
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }

    window.connect_close_request(move |window| {
        if udp_test_in_progress.get() {
            crate::gtk_compat::show_warning(
                Some(window),
                "ruzu",
                "UDP Test or calibration configuration is in progress. Please wait for it to finish.",
            );
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });

    window.present();
}

fn refresh_servers(list: &gtk::ListBox, servers: &Rc<RefCell<Vec<String>>>) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    for server in servers.borrow().iter() {
        let label = gtk::Label::new(Some(server));
        label.set_xalign(0.0);
        list.append(&label);
    }
}

fn present_calibration(
    source: &impl IsA<gtk::Widget>,
    host: String,
    port: u16,
    calibration: Rc<Cell<Calibration>>,
    target_label: gtk::Label,
) {
    let window = gtk::Window::builder()
        .title("Touch Calibration")
        .modal(true)
        .resizable(false)
        .default_width(360)
        .build();
    if let Some(parent) = source.root().and_downcast::<gtk::Window>() {
        window.set_transient_for(Some(&parent));
    }
    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let status = gtk::Label::new(Some("Communicating with the server..."));
    let close = gtk::Button::with_label("Cancel");
    content.append(&status);
    content.append(&close);
    window.set_child(Some(&content));

    enum Event {
        Status(CalibrationStatus),
        Data(Calibration),
    }
    let (sender, receiver) = mpsc::channel();
    let status_sender = sender.clone();
    let job = CalibrationConfigurationJob::new(
        &host,
        port,
        Box::new(move |value| {
            let _ = status_sender.send(Event::Status(value));
        }),
        Box::new(move |min_x, min_y, max_x, max_y| {
            let _ = sender.send(Event::Data(Calibration {
                min_x: min_x.into(),
                min_y: min_y.into(),
                max_x: max_x.into(),
                max_y: max_y.into(),
            }));
        }),
    );
    let job = Rc::new(RefCell::new(Some(job)));
    let closed = Rc::new(Cell::new(false));
    {
        let calibration = Rc::clone(&calibration);
        let close = close.clone();
        let closed = Rc::clone(&closed);
        gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
            if closed.get() {
                return gtk::glib::ControlFlow::Break;
            }
            loop {
                let event = match receiver.try_recv() {
                    Ok(event) => event,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return gtk::glib::ControlFlow::Break;
                    }
                };
                match event {
                    Event::Status(CalibrationStatus::Ready) => {
                        status.set_text("Touch the top left corner of your touchpad.")
                    }
                    Event::Status(CalibrationStatus::Stage1Completed) => {
                        status.set_text("Now touch the bottom right corner of your touchpad.")
                    }
                    Event::Status(CalibrationStatus::Completed) => {
                        status.set_text("Configuration completed!");
                        close.set_label("OK");
                    }
                    Event::Status(CalibrationStatus::Initialized) => {}
                    Event::Data(value) => {
                        calibration.set(value);
                        target_label.set_text(&value.text());
                    }
                }
            }
            gtk::glib::ControlFlow::Continue
        });
    }
    {
        let job = Rc::clone(&job);
        let window = window.downgrade();
        let closed = Rc::clone(&closed);
        close.connect_clicked(move |_| {
            closed.set(true);
            job.borrow_mut().take();
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }
    window.connect_close_request(move |_| {
        closed.set(true);
        job.borrow_mut().take();
        gtk::glib::Propagation::Proceed
    });
    window.present();
}

fn show_warning(window: &gtk::glib::WeakRef<gtk::Window>, detail: &str) {
    crate::gtk_compat::show_warning(window.upgrade().as_ref(), "ruzu", detail);
}

fn show_message(window: &gtk::glib::WeakRef<gtk::Window>, message: &str, detail: &str) {
    crate::gtk_compat::show_message(window.upgrade().as_ref(), message, detail);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_text_matches_upstream_order() {
        assert_eq!(
            Calibration {
                min_x: 100,
                min_y: 50,
                max_x: 1800,
                max_y: 850,
            }
            .text(),
            "(100, 50) - (1800, 850)"
        );
    }
}

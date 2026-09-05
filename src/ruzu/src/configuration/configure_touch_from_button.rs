// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/configure_touch_from_button.cpp`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use common::param_package::ParamPackage;
use common::settings::TouchFromButtonMap;
use gtk::prelude::*;

const SCREEN_WIDTH: i32 = 1280;
const SCREEN_HEIGHT: i32 = 720;

pub fn present(
    source: &impl IsA<gtk::Widget>,
    touch_maps: Vec<TouchFromButtonMap>,
    input_subsystem: Rc<RefCell<input_common::InputSubsystem>>,
    default_index: usize,
    on_accept: impl Fn(Vec<TouchFromButtonMap>, usize) + 'static,
) {
    let window = gtk::Window::builder()
        .title("Configure Touchscreen Mappings")
        .modal(true)
        .default_width(620)
        .default_height(520)
        .build();
    if let Some(parent) = source.root().and_downcast::<gtk::Window>() {
        window.set_transient_for(Some(&parent));
    }

    let maps = Rc::new(RefCell::new(if touch_maps.is_empty() {
        vec![TouchFromButtonMap {
            name: "default".to_string(),
            buttons: Vec::new(),
        }]
    } else {
        touch_maps
    }));
    let selected = Rc::new(Cell::new(default_index.min(maps.borrow().len() - 1)));

    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let profile_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    profile_row.append(&gtk::Label::new(Some("Mapping:")));
    let profiles = gtk::ComboBoxText::new();
    profiles.set_hexpand(true);
    for map in maps.borrow().iter() {
        profiles.append_text(&map.name);
    }
    profiles.set_active(Some(selected.get() as u32));
    let new_profile = gtk::Button::with_label("New");
    let delete_profile = gtk::Button::with_label("Delete");
    let rename_profile = gtk::Button::with_label("Rename");
    delete_profile.set_sensitive(maps.borrow().len() > 1);
    profile_row.append(&profiles);
    profile_row.append(&new_profile);
    profile_row.append(&delete_profile);
    profile_row.append(&rename_profile);
    content.append(&profile_row);

    let instructions = gtk::Label::new(Some(
        "Add a point, choose its coordinates, then press the controller button to bind.",
    ));
    instructions.set_xalign(0.0);
    instructions.set_wrap(true);
    content.append(&instructions);

    let bindings = gtk::ListBox::new();
    bindings.set_selection_mode(gtk::SelectionMode::None);
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .child(&bindings)
        .build();
    content.append(&scroller);

    let add_point = gtk::Button::with_label("Add Point");
    add_point.set_halign(gtk::Align::Start);
    content.append(&add_point);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("OK");
    ok.add_css_class("suggested-action");
    actions.append(&cancel);
    actions.append(&ok);
    content.append(&actions);
    window.set_child(Some(&content));

    refresh_bindings(&bindings, &maps, selected.get());

    {
        let bindings = bindings.clone();
        let maps = Rc::clone(&maps);
        let selected = Rc::clone(&selected);
        profiles.connect_changed(move |combo| {
            if let Some(index) = combo.active() {
                selected.set(index as usize);
                refresh_bindings(&bindings, &maps, selected.get());
            }
        });
    }
    {
        let profiles = profiles.clone();
        let bindings = bindings.clone();
        let maps = Rc::clone(&maps);
        let selected = Rc::clone(&selected);
        let delete_profile = delete_profile.clone();
        new_profile.connect_clicked(move |button| {
            let profiles = profiles.clone();
            let bindings = bindings.clone();
            let maps = Rc::clone(&maps);
            let selected = Rc::clone(&selected);
            let delete_profile = delete_profile.clone();
            request_name(
                button,
                "New Profile",
                "Enter the name for the new profile.",
                move |name| {
                    if name.is_empty() {
                        return;
                    }
                    maps.borrow_mut().push(TouchFromButtonMap {
                        name: name.clone(),
                        buttons: Vec::new(),
                    });
                    profiles.append_text(&name);
                    let index = maps.borrow().len() - 1;
                    selected.set(index);
                    profiles.set_active(Some(index as u32));
                    delete_profile.set_sensitive(true);
                    refresh_bindings(&bindings, &maps, index);
                },
            );
        });
    }
    {
        let profiles = profiles.clone();
        let bindings = bindings.clone();
        let maps = Rc::clone(&maps);
        let selected = Rc::clone(&selected);
        let delete_button = delete_profile.clone();
        let window = window.clone();
        delete_profile.connect_clicked(move |_| {
            if maps.borrow().len() <= 1 {
                return;
            }
            let index = selected.get();
            let profile_name = maps.borrow()[index].name.clone();
            let profiles = profiles.clone();
            let bindings = bindings.clone();
            let maps = Rc::clone(&maps);
            let selected = Rc::clone(&selected);
            let delete_button = delete_button.clone();
            crate::gtk_compat::ask_question(
                Some(&window),
                "Delete Profile",
                &format!("Delete profile {profile_name}?"),
                "No",
                "Yes",
                move |accepted| {
                    if !accepted {
                        return;
                    }
                    maps.borrow_mut().remove(index);
                    profiles.remove(index as i32);
                    let next = index.min(maps.borrow().len() - 1);
                    selected.set(next);
                    profiles.set_active(Some(next as u32));
                    delete_button.set_sensitive(maps.borrow().len() > 1);
                    refresh_bindings(&bindings, &maps, next);
                },
            );
        });
    }
    {
        let profiles = profiles.clone();
        let maps = Rc::clone(&maps);
        let selected = Rc::clone(&selected);
        rename_profile.connect_clicked(move |button| {
            let profiles = profiles.clone();
            let maps = Rc::clone(&maps);
            let selected = Rc::clone(&selected);
            request_name(button, "Rename Profile", "New name:", move |name| {
                if name.is_empty() {
                    return;
                }
                let index = selected.get();
                maps.borrow_mut()[index].name = name.clone();
                profiles.remove(index as i32);
                profiles.insert_text(index as i32, &name);
                profiles.set_active(Some(index as u32));
            });
        });
    }
    {
        let maps = Rc::clone(&maps);
        let selected = Rc::clone(&selected);
        let bindings = bindings.clone();
        let input_subsystem = Rc::clone(&input_subsystem);
        add_point.connect_clicked(move |button| {
            let maps = Rc::clone(&maps);
            let selected = Rc::clone(&selected);
            let bindings = bindings.clone();
            request_binding(
                button,
                Rc::clone(&input_subsystem),
                move |mut params, x, y| {
                    params.set_int("x", x);
                    params.set_int("y", y);
                    maps.borrow_mut()[selected.get()]
                        .buttons
                        .push(params.serialize());
                    refresh_bindings(&bindings, &maps, selected.get());
                },
            );
        });
    }
    {
        let window = window.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }
    {
        let window = window.downgrade();
        ok.connect_clicked(move |_| {
            on_accept(maps.borrow().clone(), selected.get());
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }
    window.present();
}

fn refresh_bindings(
    list: &gtk::ListBox,
    maps: &Rc<RefCell<Vec<TouchFromButtonMap>>>,
    selected: usize,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let buttons = maps.borrow()[selected].buttons.clone();
    for (index, serialized) in buttons.into_iter().enumerate() {
        let params = ParamPackage::from_serialized(&serialized);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let name = gtk::Label::new(Some(&super::configure_input_player::button_to_text(
            &serialized,
        )));
        name.set_xalign(0.0);
        name.set_hexpand(true);
        let x = gtk::SpinButton::with_range(0.0, (SCREEN_WIDTH - 1) as f64, 1.0);
        x.set_value(params.get_int("x", 0) as f64);
        let y = gtk::SpinButton::with_range(0.0, (SCREEN_HEIGHT - 1) as f64, 1.0);
        y.set_value(params.get_int("y", 0) as f64);
        let remove = gtk::Button::with_label("Delete");
        row.append(&name);
        row.append(&gtk::Label::new(Some("X")));
        row.append(&x);
        row.append(&gtk::Label::new(Some("Y")));
        row.append(&y);
        row.append(&remove);
        list.append(&row);

        {
            let maps = Rc::clone(maps);
            x.connect_value_changed(move |spin| {
                update_coordinate(&maps, selected, index, "x", spin.value_as_int());
            });
        }
        {
            let maps = Rc::clone(maps);
            y.connect_value_changed(move |spin| {
                update_coordinate(&maps, selected, index, "y", spin.value_as_int());
            });
        }
        {
            let maps = Rc::clone(maps);
            let list = list.clone();
            remove.connect_clicked(move |_| {
                maps.borrow_mut()[selected].buttons.remove(index);
                refresh_bindings(&list, &maps, selected);
            });
        }
    }
}

fn update_coordinate(
    maps: &Rc<RefCell<Vec<TouchFromButtonMap>>>,
    map_index: usize,
    binding_index: usize,
    key: &str,
    value: i32,
) {
    let serialized = maps.borrow()[map_index].buttons[binding_index].clone();
    let mut params = ParamPackage::from_serialized(&serialized);
    params.set_int(key, value);
    maps.borrow_mut()[map_index].buttons[binding_index] = params.serialize();
}

fn request_binding(
    source: &impl IsA<gtk::Widget>,
    input_subsystem: Rc<RefCell<input_common::InputSubsystem>>,
    on_accept: impl Fn(ParamPackage, i32, i32) + 'static,
) {
    let window = gtk::Window::builder()
        .title("New Touch Point")
        .modal(true)
        .resizable(false)
        .build();
    if let Some(parent) = source.root().and_downcast::<gtk::Window>() {
        window.set_transient_for(Some(&parent));
    }
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let coordinates = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let x = gtk::SpinButton::with_range(0.0, (SCREEN_WIDTH - 1) as f64, 1.0);
    let y = gtk::SpinButton::with_range(0.0, (SCREEN_HEIGHT - 1) as f64, 1.0);
    coordinates.append(&gtk::Label::new(Some("X")));
    coordinates.append(&x);
    coordinates.append(&gtk::Label::new(Some("Y")));
    coordinates.append(&y);
    content.append(&coordinates);
    let status = gtk::Label::new(Some("Press a controller button"));
    content.append(&status);
    let cancel = gtk::Button::with_label("Cancel");
    content.append(&cancel);
    window.set_child(Some(&content));

    input_subsystem
        .borrow_mut()
        .begin_mapping(input_common::polling::InputType::Button);
    let finished = Rc::new(Cell::new(false));
    let callback = Rc::new(RefCell::new(Some(on_accept)));
    let deadline = Instant::now() + Duration::from_secs(5);
    {
        let window = window.downgrade();
        let input_subsystem = Rc::clone(&input_subsystem);
        let finished = Rc::clone(&finished);
        let callback = Rc::clone(&callback);
        let x = x.clone();
        let y = y.clone();
        gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
            if finished.get() {
                return gtk::glib::ControlFlow::Break;
            }
            if Instant::now() >= deadline {
                finished.set(true);
                input_subsystem.borrow_mut().stop_mapping();
                if let Some(window) = window.upgrade() {
                    window.close();
                }
                return gtk::glib::ControlFlow::Break;
            }
            let params = input_subsystem.borrow_mut().get_next_input();
            if !params.has("engine") {
                return gtk::glib::ControlFlow::Continue;
            }
            finished.set(true);
            input_subsystem.borrow_mut().stop_mapping();
            if let Some(callback) = callback.borrow_mut().take() {
                callback(params, x.value_as_int(), y.value_as_int());
            }
            if let Some(window) = window.upgrade() {
                window.close();
            }
            gtk::glib::ControlFlow::Break
        });
    }
    {
        let window = window.downgrade();
        let finished = Rc::clone(&finished);
        let input_subsystem = Rc::clone(&input_subsystem);
        cancel.connect_clicked(move |_| {
            if !finished.replace(true) {
                input_subsystem.borrow_mut().stop_mapping();
            }
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }
    window.connect_close_request(move |_| {
        if !finished.replace(true) {
            input_subsystem.borrow_mut().stop_mapping();
        }
        gtk::glib::Propagation::Proceed
    });
    window.present();
}

fn request_name(
    source: &impl IsA<gtk::Widget>,
    title: &str,
    prompt: &str,
    on_accept: impl FnOnce(String) + 'static,
) {
    let window = gtk::Window::builder()
        .title(title)
        .modal(true)
        .resizable(false)
        .default_width(360)
        .build();
    if let Some(parent) = source.root().and_downcast::<gtk::Window>() {
        window.set_transient_for(Some(&parent));
    }
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&gtk::Label::new(Some(prompt)));
    let entry = gtk::Entry::new();
    content.append(&entry);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let accept = gtk::Button::with_label("OK");
    actions.append(&cancel);
    actions.append(&accept);
    content.append(&actions);
    window.set_child(Some(&content));
    {
        let window = window.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }
    let callback = Rc::new(RefCell::new(Some(on_accept)));
    {
        let window = window.downgrade();
        accept.connect_clicked(move |_| {
            if let Some(callback) = callback.borrow_mut().take() {
                callback(entry.text().to_string());
            }
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }
    window.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_coordinates_match_upstream_handheld_layout() {
        assert_eq!((SCREEN_WIDTH, SCREEN_HEIGHT), (1280, 720));
    }
}

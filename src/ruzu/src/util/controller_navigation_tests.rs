// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use gtk::glib;

#[test]
#[ignore = "requires GTK on the platform main thread and a display"]
fn controls_navigation_suspension_is_local_inherited_and_reversible() {
    gtk::init().unwrap();
    let main = gtk::Window::new();
    let config = gtk::Window::builder().transient_for(&main).build();
    let child = gtk::Window::builder().transient_for(&config).build();
    assert!(interface_navigation_allowed(&config));
    config.add_css_class(INPUT_CONFIGURATION_CSS_CLASS);
    assert!(!interface_navigation_allowed(&config));
    assert!(!interface_navigation_allowed(&child));
    assert!(interface_navigation_allowed(&main));
    config.remove_css_class(INPUT_CONFIGURATION_CSS_CLASS);
    assert!(interface_navigation_allowed(&config));
    assert!(interface_navigation_allowed(&child));
    child.destroy();
    config.destroy();
    main.destroy();
}

#[test]
fn vertical_repeat_delay_release_direction_change_and_no_backlog() {
    use std::time::{Duration, Instant};
    let start = Instant::now();
    let mut repeat = NavigationRepeat::default();
    let down = Some(NavigationKey::Down);
    assert_eq!(repeat.poll(down, start), None);
    assert_eq!(repeat.poll(down, start + Duration::from_millis(399)), None);
    assert_eq!(repeat.poll(down, start + Duration::from_millis(400)), down);
    assert_eq!(repeat.poll(down, start + Duration::from_millis(479)), None);
    assert_eq!(repeat.poll(down, start + Duration::from_millis(480)), down);
    let later = start + Duration::from_secs(10);
    assert_eq!(repeat.poll(down, later), down);
    assert_eq!(repeat.poll(down, later), None);
    let up = Some(NavigationKey::Up);
    assert_eq!(repeat.poll(up, later), None);
    assert_eq!(repeat.poll(up, later + Duration::from_millis(400)), up);
    assert_eq!(repeat.poll(None, later), None);
    assert_eq!(repeat.poll(up, later), None);
    repeat.suppress(up);
    assert_eq!(repeat.poll(up, later + Duration::from_secs(1)), None);
    assert_eq!(repeat.poll(down, later + Duration::from_secs(2)), None);
    assert_eq!(repeat.poll(None, later), None);
    assert_eq!(repeat.poll(down, later), None);
    assert_eq!(repeat.poll(down, later + Duration::from_millis(400)), down);
    for key in [NavigationKey::Enter, NavigationKey::Escape, NavigationKey::Menu, NavigationKey::Left, NavigationKey::Right] {
        assert_eq!(repeat.poll(Some(key), later), None);
        assert_eq!(repeat.poll(Some(key), later + Duration::from_secs(5)), None);
    }
}

#[test]
fn vertical_repeat_uses_dpad_and_rotated_stick_not_sideways_confirmation() {
    let mut state = NavigationState::default();
    state.button_values[native_button::Values::DDown as usize].value = true;
    assert_eq!(vertical_direction(&state, NpadStyleIndex::Fullkey), Some(NavigationKey::Down));
    assert_eq!(vertical_direction(&state, NpadStyleIndex::JoyconLeft), None);
    state.stick_values[native_analog::Values::LStick as usize].up = true;
    assert_eq!(vertical_direction(&state, NpadStyleIndex::Fullkey), None);
    state.button_values[native_button::Values::DDown as usize].value = false;
    assert_eq!(vertical_direction(&state, NpadStyleIndex::Fullkey), Some(NavigationKey::Up));
    state.stick_values[native_analog::Values::LStick as usize].up = false;
    state.stick_values[native_analog::Values::LStick as usize].left = true;
    assert_eq!(vertical_direction(&state, NpadStyleIndex::JoyconLeft), Some(NavigationKey::Down));
}

#[test]
fn interface_device_identity_ignores_parameter_order_and_display_name() {
    use common::param_package::ParamPackage;
    let first =
        ParamPackage::from_serialized("engine:sdl,guid:synthetic-pad,port:2,display:Controller");
    let renamed =
        ParamPackage::from_serialized("port:2,display:Renamed,engine:sdl,guid:synthetic-pad");
    assert_eq!(
        interface_device_identity(&first),
        interface_device_identity(&renamed)
    );
    let other = ParamPackage::from_serialized("engine:sdl,guid:synthetic-pad,port:3");
    assert_ne!(
        interface_device_identity(&first),
        interface_device_identity(&other)
    );
}

#[test]
#[ignore = "SDL input factories are process-global; run alone with --test-threads=1"]
fn interface_unmapped_sdl_pad_does_not_change_player_bindings() {
    use sdl3_sys::joystick::*;
    let input = std::rc::Rc::new(std::cell::RefCell::new(input_common::InputSubsystem::new()));
    input.borrow_mut().initialize();
    let description = SDL_VirtualJoystickDesc {
        r#type: SDL_JOYSTICK_TYPE_GAMEPAD.0 as u16,
        naxes: 6,
        nbuttons: 15,
        axis_mask: 0x3f,
        button_mask: 0x7fff,
        name: c"Ruzu navigation test".as_ptr(),
        ..Default::default()
    };
    // SDL owns the virtual joystick until detach, including on assertion failure.
    struct Pad(SDL_JoystickID, *mut SDL_Joystick);
    impl Drop for Pad {
        fn drop(&mut self) {
            unsafe {
                SDL_CloseJoystick(self.1);
                SDL_DetachVirtualJoystick(self.0);
            }
        }
    }
    let id = unsafe { SDL_AttachVirtualJoystick(&description) };
    assert_ne!(id.0, 0);
    let pad = Pad(id, unsafe { SDL_OpenJoystick(id) });
    assert!(!pad.1.is_null());
    input.borrow_mut().pump_events();
    let hid = Arc::new(Mutex::new(HIDCore::new()));
    let player = hid.lock().get_emulated_controller(NpadIdType::Player1);
    let before: Vec<_> = (0..native_button::NUM_BUTTONS)
        .map(|i| player.lock().get_button_param(i).serialize())
        .collect();
    let navigation = ControllerNavigation::for_interface(&hid, &input);
    assert!(navigation.take_pending_keys().is_empty());
    let device = input
        .borrow()
        .get_input_devices()
        .into_iter()
        .find(|device| {
            device
                .get_str("display", "")
                .contains("Ruzu navigation test")
        })
        .unwrap();
    assert!(navigation
        .interface_pad
        .borrow()
        .as_ref()
        .unwrap()
        .identity
        .split('|')
        .any(|key| key == interface_device_identity(&device)));
    let mappings = input.borrow().get_button_mapping_for_device(&device);
    let button = mappings[&(native_button::Values::A as i32)].get_int("button", -1) as i32;
    assert!(button >= 0);
    assert!(unsafe { SDL_SetJoystickVirtualButton(pad.1, button, true) });
    unsafe {
        SDL_UpdateJoysticks();
    }
    input.borrow_mut().pump_events();
    assert_eq!(navigation.take_pending_keys(), vec![NavigationKey::Enter]);
    assert!(
        navigation.take_pending_keys().is_empty(),
        "held input must not double-activate"
    );
    assert!(unsafe { SDL_SetJoystickVirtualButton(pad.1, button, false) });
    unsafe {
        SDL_UpdateJoysticks();
    }
    input.borrow_mut().pump_events();
    assert!(navigation.take_pending_keys().is_empty());
    assert!(unsafe { SDL_SetJoystickVirtualButton(pad.1, button, true) });
    unsafe {
        SDL_UpdateJoysticks();
    }
    input.borrow_mut().pump_events();
    assert_eq!(navigation.take_pending_keys(), vec![NavigationKey::Enter]);
    let after: Vec<_> = (0..native_button::NUM_BUTTONS)
        .map(|i| player.lock().get_button_param(i).serialize())
        .collect();
    assert_eq!(before, after);
    drop(navigation);
    drop(pad);
    input.borrow_mut().shutdown();
}

#[test]
#[ignore = "requires GTK and desktop focus; run alone with --test-threads=1"]
fn interface_hid_routes_once_and_yields_to_modal_and_capture() {
    use input_common::drivers::virtual_gamepad::VirtualButton;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    gtk::init().unwrap();
    let input = Rc::new(RefCell::new(input_common::InputSubsystem::new()));
    input.borrow_mut().initialize();
    let hid = Arc::new(Mutex::new(HIDCore::new()));
    hid.lock().reload_input_devices();
    hid.lock()
        .get_emulated_controller(NpadIdType::Player1)
        .lock()
        .set_npad_style_index(NpadStyleIndex::Fullkey);
    let window = gtk::Window::builder()
        .title("Ruzu controller routing test")
        .build();
    let button = gtk::Button::with_label("Open question");
    window.set_child(Some(&button));
    let replies = Rc::new(RefCell::new(Vec::new()));
    button.connect_clicked(gtk::glib::clone!(
        #[weak]
        window,
        #[strong]
        hid,
        #[strong]
        replies,
        move |_| {
            crate::gtk_compat::ask_question_with_navigation(
                Some(&window),
                "Routing test",
                "Choose",
                "No",
                "Yes",
                Some(ControllerNavigation::new(&hid)),
                {
                    let replies = replies.clone();
                    move |reply| replies.borrow_mut().push(reply)
                },
            );
        }
    ));
    let launcher = Rc::new(Cell::new(true));
    let routed = Rc::new(RefCell::new(Vec::new()));
    install_interface_navigation(
        &window,
        &hid,
        &input,
        {
            let launcher = launcher.clone();
            move || launcher.get()
        },
        {
            let routed = routed.clone();
            move |key| {
                routed.borrow_mut().push(key);
                false
            }
        },
    );
    let pump = |duration: std::time::Duration| {
        let deadline = std::time::Instant::now() + duration;
        while std::time::Instant::now() < deadline {
            gtk::glib::MainContext::default().iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    };
    let until = |condition: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !condition() && std::time::Instant::now() < deadline {
            pump(std::time::Duration::from_millis(10));
        }
        assert!(condition(), "routing test requires desktop focus");
        pump(std::time::Duration::from_millis(50));
    };
    let press = |input: &Rc<RefCell<input_common::InputSubsystem>>, button| {
        input
            .borrow_mut()
            .get_virtual_gamepad_mut()
            .unwrap()
            .set_button_state(0, button, true);
        pump(std::time::Duration::from_millis(100));
        input
            .borrow_mut()
            .get_virtual_gamepad_mut()
            .unwrap()
            .set_button_state(0, button, false);
        pump(std::time::Duration::from_millis(100));
    };
    window.present();
    until(&|| window.is_active());
    button.grab_focus();
    press(&input, VirtualButton::ButtonA);
    assert_eq!(&*routed.borrow(), &[NavigationKey::Enter]);
    let question = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|w| w.downcast::<gtk::MessageDialog>().ok())
        .unwrap();
    until(&|| question.is_active());
    routed.borrow_mut().clear();
    press(&input, VirtualButton::ButtonB);
    until(&|| !question.is_visible());
    assert_eq!(&*replies.borrow(), &[false]);
    assert!(
        routed.borrow().is_empty(),
        "modal B must not reach the launcher"
    );
    question.destroy();
    window.present();
    until(&|| window.is_active());
    window.add_css_class("ruzu-controller-capture");
    pump(std::time::Duration::from_millis(50));
    press(&input, VirtualButton::ButtonDown);
    assert!(routed.borrow().is_empty());
    window.remove_css_class("ruzu-controller-capture");
    pump(std::time::Duration::from_millis(50));
    press(&input, VirtualButton::ButtonDown);
    assert_eq!(&*routed.borrow(), &[NavigationKey::Down]);
    routed.borrow_mut().clear();
    launcher.set(false);
    press(&input, VirtualButton::ButtonA);
    assert!(
        routed.borrow().is_empty(),
        "gameplay input must not activate launcher controls"
    );
    window.destroy();
    drop(window);
    pump(std::time::Duration::from_millis(50));
    hid.lock().unload_input_devices();
    input.borrow_mut().shutdown();
}

#[test]
#[ignore = "requires GTK on the platform main thread and a display"]
fn interface_navigation_focus_buttons_menus_and_file_tree() {
    gtk::init().unwrap();
    fn pump() {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(250);
        while std::time::Instant::now() < until {
            gtk::glib::MainContext::default().iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    let window = gtk::Window::new();
    window.set_title(Some("Ruzu controller navigation test"));
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let first = gtk::Button::with_label("Add directory");
    let second = gtk::CheckButton::with_label("Option");
    root.append(&first);
    root.append(&second);
    window.set_child(Some(&root));
    let activated = std::rc::Rc::new(std::cell::Cell::new(0));
    first.connect_clicked({
        let activated = activated.clone();
        move |_| activated.set(activated.get() + 1)
    });
    window.present();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !window.is_active() && std::time::Instant::now() < deadline {
        pump();
    }
    assert!(
        window.is_active(),
        "test window must receive desktop focus: visible={} mapped={} realized={} display={:?}",
        window.is_visible(),
        window.is_mapped(),
        window.is_realized(),
        gtk::gdk::Display::default()
    );
    first.grab_focus();
    navigate_window(&window, NavigationKey::Enter);
    pump();
    assert_eq!(activated.get(), 1);
    navigate_window(&window, NavigationKey::Next);
    assert_eq!(
        gtk::prelude::GtkWindowExt::focus(&window),
        Some(second.clone().upcast())
    );
    navigate_window(&window, NavigationKey::Enter);
    pump();
    assert!(second.is_active());
    navigate_window(&window, NavigationKey::Previous);
    assert_eq!(
        gtk::prelude::GtkWindowExt::focus(&window),
        Some(first.clone().upcast())
    );
    assert!(window.gets_focus_visible());
    navigate_window(&window, NavigationKey::Escape);
    assert!(window.is_visible()); // B must not quit the launcher.

    let modal = gtk::Window::builder()
        .transient_for(&window)
        .modal(true)
        .build();
    assert!(belongs_to(&modal, &window));
    assert!(!belongs_to(&gtk::Window::new(), &window));
    modal.present();
    pump();
    navigate_window(&modal, NavigationKey::Escape);
    pump();
    assert!(!modal.is_visible());
    window.present();
    pump();

    let store = gtk::ListStore::new(&[String::static_type()]);
    for i in 0..80 {
        store.insert_with_values(None, &[(0, &format!("Folder {i}"))]);
    }
    let tree = gtk::TreeView::with_model(&store);
    let column = gtk::TreeViewColumn::new();
    let cell = gtk::CellRendererText::new();
    column.pack_start(&cell, true);
    column.add_attribute(&cell, "text", 0);
    tree.append_column(&column);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&tree)
        .height_request(120)
        .build();
    root.append(&scroll);
    pump();
    tree.grab_focus();
    gtk::prelude::TreeViewExt::set_cursor(&tree, &gtk::TreePath::from_indices(&[0]), None, false);
    for _ in 0..30 {
        navigate_window(&window, NavigationKey::Down);
    }
    pump();
    assert_eq!(
        gtk::prelude::TreeViewExt::cursor(&tree)
            .0
            .unwrap()
            .indices()
            .as_ref(),
        &[30]
    );
    assert!(scroll.vadjustment().value() > 0.0);
    let selected = std::rc::Rc::new(std::cell::Cell::new(false));
    tree.connect_row_activated({
        let selected = selected.clone();
        move |_, _, _| selected.set(true)
    });
    navigate_window(&window, NavigationKey::Enter);
    assert!(selected.get());

    let submenu = gtk::gio::Menu::new();
    submenu.append(Some("Action"), Some("test.action"));
    let model = gtk::gio::Menu::new();
    model.append_submenu(Some("Tools"), &submenu);
    let second_menu = gtk::gio::Menu::new();
    second_menu.append(Some("Other action"), Some("test.other"));
    model.append_submenu(Some("Other"), &second_menu);
    let menu = gtk::PopoverMenuBar::from_model(Some(&model));
    let group = gtk::gio::SimpleActionGroup::new();
    let action = gtk::gio::SimpleAction::new("action", None);
    let called = std::rc::Rc::new(std::cell::Cell::new(false));
    action.connect_activate({
        let called = called.clone();
        move |_, _| called.set(true)
    });
    group.add_action(&action);
    let other_called = std::rc::Rc::new(std::cell::Cell::new(false));
    let other_action = gtk::gio::SimpleAction::new("other", None);
    other_action.connect_activate({
        let called = other_called.clone();
        move |_, _| called.set(true)
    });
    group.add_action(&other_action);
    window.insert_action_group("test", Some(&group));
    root.prepend(&menu);
    pump();
    navigate_window(&window, NavigationKey::Menu);
    pump();
    let opened = gtk::prelude::GtkWindowExt::focus(&window);
    navigate_window(&window, NavigationKey::Down);
    let down = gtk::prelude::GtkWindowExt::focus(&window);
    navigate_window(&window, NavigationKey::Enter);
    pump();
    assert!(
        called.get(),
        "controller must activate the focused submenu item: opened={opened:?} down={down:?}"
    );
    navigate_window(&window, NavigationKey::Menu);
    pump();
    navigate_window(&window, NavigationKey::Right);
    pump();
    navigate_window(&window, NavigationKey::Enter);
    pump();
    assert!(
        other_called.get(),
        "right must reach the next top-level menu"
    );
    window.close();
    pump();
}

#[test]
fn fullkey_left_stick_matches_upstream_key_priority() {
    let mut sticks = vec![StickStatus::default(); native_analog::NUM_ANALOGS];
    sticks[native_analog::Values::LStick as usize].down = true;
    sticks[native_analog::Values::LStick as usize].right = true;

    assert_eq!(
        stick_navigation_key(NpadStyleIndex::Fullkey, &sticks),
        Some(NavigationKey::Down)
    );
}

#[test]
fn sideways_joycons_rotate_navigation_like_upstream() {
    let mut sticks = vec![StickStatus::default(); native_analog::NUM_ANALOGS];
    sticks[native_analog::Values::LStick as usize].left = true;
    assert_eq!(
        stick_navigation_key(NpadStyleIndex::JoyconLeft, &sticks),
        Some(NavigationKey::Down)
    );

    sticks.fill(StickStatus::default());
    sticks[native_analog::Values::RStick as usize].right = true;
    assert_eq!(
        stick_navigation_key(NpadStyleIndex::JoyconRight, &sticks),
        Some(NavigationKey::Down)
    );
}

#[test]
fn button_front_triggers_only_once() {
    let mut state = NavigationState::default();
    let index = native_button::Values::A as usize;

    state.button_values[index].value = true;
    state.button_values[index].locked = false;
    trigger_button(&mut state, native_button::Values::A, NavigationKey::Enter);
    state.button_values[index].locked = true;
    trigger_button(&mut state, native_button::Values::A, NavigationKey::Enter);

    assert_eq!(
        state.pending_keys.into_iter().collect::<Vec<_>>(),
        vec![NavigationKey::Enter]
    );
}

#[test]
fn hid_callback_only_queues_the_trigger() {
    let state = Arc::new(Mutex::new(NavigationState::default()));
    let callback_state = Arc::clone(&state);
    let callback = move |trigger_type| {
        callback_state
            .lock()
            .pending_triggers
            .push_back(trigger_type);
    };

    callback(ControllerTriggerType::Button);

    let state = state.lock();
    assert_eq!(
        state.pending_triggers.front(),
        Some(&ControllerTriggerType::Button)
    );
    assert!(state.pending_keys.is_empty());
}

// SPDX-License-Identifier: GPL-3.0-or-later
//
// Application-scoped File-menu handlers that do not depend on the window's
// render surface. Counterparts of upstream `main_window.cpp`:
//   * `OnOpenRootDataFolder`
//   * `QMainWindow::close` (action_Exit)
//
// The window-dependent File actions (Load File / Load Folder → in-process boot)
// live on `GMainWindow` (see `main_window.rs`), since they need the render
// surface, loading screen, and stack. Recent Files remains a dynamic menu
// placeholder.

use gtk::prelude::*;
use gtk::{gio, glib, Application};

/// Register the application-scoped File actions (open ruzu folder, exit).
/// Called from `init_app_menu`.
pub fn register(app: &Application) {
    // action_Root_Data_Folder → OnOpenRootDataFolder
    let open_folder = gio::SimpleAction::new("open_ruzu_folder", None);
    open_folder.connect_activate(|_, _| crate::util::game::open_root_data_folder());
    app.add_action(&open_folder);

    // action_Exit → QMainWindow::close
    let exit = gio::SimpleAction::new("exit", None);
    exit.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            if let Some(window) = app.active_window() {
                window.close();
            }
        }
    ));
    app.add_action(&exit);
}

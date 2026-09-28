// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/frontend/applets.h
//! Port of zuyu/src/core/hle/service/am/frontend/applets.cpp
//!
//! Base FrontendApplet class and FrontendAppletHolder.

use std::sync::Arc;

use hid_core::hid_core::HIDCore;
use parking_lot::Mutex;

use crate::core::SystemRef;
use crate::frontend::applets::cabinet::{CabinetApplet, DefaultCabinetApplet};
use crate::frontend::applets::controller::{ControllerApplet, DefaultControllerApplet};
use crate::frontend::applets::error::{DefaultErrorApplet, ErrorApplet};
use crate::frontend::applets::general::{
    DefaultParentalControlsApplet, DefaultPhotoViewerApplet, ParentalControlsApplet,
    PhotoViewerApplet,
};
use crate::frontend::applets::profile_select::{DefaultProfileSelectApplet, ProfileSelectApplet};
use crate::frontend::applets::software_keyboard::{
    DefaultSoftwareKeyboardApplet, SoftwareKeyboardApplet,
};
use crate::frontend::applets::web_browser::{DefaultWebBrowserApplet, WebBrowserApplet};
use crate::hle::result::ResultCode;
use crate::hle::service::am::am_types::{AppletId, LibraryAppletMode};
use crate::hle::service::am::applet::Applet;
use crate::hle::service::am::applet_data_broker::AppletDataBroker;
use crate::hle::service::nfp::nfp_types::CabinetMode;

use super::applet_cabinet::Cabinet;
use super::applet_controller::Controller;
use super::applet_error::Error;
use super::applet_general::{Auth, PhotoViewer, StubApplet};
use super::applet_mii_edit::MiiEdit;
use super::applet_profile_select::ProfileSelect;
use super::applet_software_keyboard::SoftwareKeyboard;
use super::applet_web_browser::WebBrowser;

/// Port of `FrontendApplet::Exit`. Call only after releasing the Applet mutex:
/// synchronous frontend callbacks defer this work until Execute returns.
pub fn exit(system: SystemRef, applet: &std::sync::Weak<std::sync::Mutex<Applet>>) {
    let Some(applet) = applet.upgrade() else { return };
    let caller = {
        let mut child = applet.lock().unwrap();
        child.is_completed = true;
        child.signal_state_changed_event_without_process();
        child.caller_applet.clone()
    };
    if let Some(caller) = caller.upgrade() {
        caller.lock().unwrap().child_applets.retain(|child| !Arc::ptr_eq(child, &applet));
    }
    if !system.is_null() {
        if let Some(window_system) = system.get().get_applet_manager().get_window_system() {
            window_system.lock().unwrap().request_update();
        }
    }
}

/// Base trait for all frontend applet implementations.
///
/// Port of FrontendApplet class.
pub trait FrontendApplet: Send + Sync {
    fn initialize(&mut self);
    fn get_status(&self) -> ResultCode;
    fn execute_interactive(&mut self);
    fn execute(&mut self);
    fn request_exit(&mut self);
    fn get_library_applet_mode(&self) -> LibraryAppletMode;
    fn is_initialized(&self) -> bool;
    /// Synchronous completion is applied through `exit` after the caller drops
    /// its Applet guard; asynchronous callbacks use their weak owner directly.
    fn is_complete(&self) -> bool;
}

/// Holds the set of frontend applet implementations.
///
/// Port of FrontendAppletHolder class.
#[derive(Default)]
pub struct FrontendAppletSet {
    pub cabinet: Option<Arc<dyn CabinetApplet>>,
    pub controller: Option<Arc<dyn ControllerApplet>>,
    pub error: Option<Arc<dyn ErrorApplet>>,
    pub parental_controls: Option<Arc<dyn ParentalControlsApplet>>,
    pub photo_viewer: Option<Arc<dyn PhotoViewerApplet>>,
    pub profile_select: Option<Arc<dyn ProfileSelectApplet>>,
    pub software_keyboard: Option<Arc<dyn SoftwareKeyboardApplet>>,
    pub web_browser: Option<Arc<dyn WebBrowserApplet>>,
}

pub struct FrontendAppletHolder {
    cabinet_mode: CabinetMode,
    current_applet_id: AppletId,
    frontend: FrontendAppletSet,
}

impl FrontendAppletHolder {
    pub fn new(hid_core: Arc<Mutex<HIDCore>>) -> Self {
        Self {
            cabinet_mode: CabinetMode::default(),
            current_applet_id: AppletId::None,
            frontend: FrontendAppletSet {
                cabinet: Some(Arc::new(DefaultCabinetApplet)),
                controller: Some(Arc::new(DefaultControllerApplet::new(hid_core))),
                error: Some(Arc::new(DefaultErrorApplet)),
                parental_controls: Some(Arc::new(DefaultParentalControlsApplet)),
                photo_viewer: Some(Arc::new(DefaultPhotoViewerApplet)),
                profile_select: Some(Arc::new(DefaultProfileSelectApplet)),
                software_keyboard: Some(Arc::new(DefaultSoftwareKeyboardApplet::new())),
                web_browser: Some(Arc::new(DefaultWebBrowserApplet)),
            },
        }
    }

    pub fn get_frontend_applet_set(&self) -> &FrontendAppletSet {
        &self.frontend
    }

    /// Replace every supplied frontend implementation while preserving the
    /// existing default for omitted entries.
    ///
    /// Port of `FrontendAppletHolder::SetFrontendAppletSet`.
    pub fn set_frontend_applet_set(&mut self, mut set: FrontendAppletSet) {
        if set.cabinet.is_some() {
            self.frontend.cabinet = set.cabinet.take();
        }
        if set.controller.is_some() {
            self.frontend.controller = set.controller.take();
        }
        if set.error.is_some() {
            self.frontend.error = set.error.take();
        }
        if set.parental_controls.is_some() {
            self.frontend.parental_controls = set.parental_controls.take();
        }
        if set.photo_viewer.is_some() {
            self.frontend.photo_viewer = set.photo_viewer.take();
        }
        if set.profile_select.is_some() {
            self.frontend.profile_select = set.profile_select.take();
        }
        if set.software_keyboard.is_some() {
            self.frontend.software_keyboard = set.software_keyboard.take();
        }
        if set.web_browser.is_some() {
            self.frontend.web_browser = set.web_browser.take();
        }
    }

    pub fn get_cabinet_mode(&self) -> CabinetMode {
        self.cabinet_mode
    }

    pub fn set_cabinet_mode(&mut self, mode: CabinetMode) {
        self.cabinet_mode = mode;
    }

    pub fn get_current_applet_id(&self) -> AppletId {
        self.current_applet_id
    }

    pub fn set_current_applet_id(&mut self, applet_id: AppletId) {
        self.current_applet_id = applet_id;
    }

    pub fn get_applet(
        &self,
        system: SystemRef,
        applet: std::sync::Weak<std::sync::Mutex<Applet>>,
        broker: Arc<AppletDataBroker>,
        id: AppletId,
        mode: LibraryAppletMode,
    ) -> Option<Box<dyn FrontendApplet>> {
        match id {
            AppletId::Cabinet => Some(Box::new(Cabinet::new(
                system,
                applet,
                broker,
                mode,
                Arc::clone(
                    self.frontend
                        .cabinet
                        .as_ref()
                        .expect("default cabinet applet is installed"),
                ),
            ))),
            AppletId::Auth => Some(Box::new(Auth::new(
                system,
                applet,
                broker,
                mode,
                Arc::clone(
                    self.frontend
                        .parental_controls
                        .as_ref()
                        .expect("default parental-controls applet is installed"),
                ),
            ))),
            AppletId::Controller => Some(Box::new(Controller::new(
                system,
                applet,
                broker,
                mode,
                Arc::clone(
                    self.frontend
                        .controller
                        .as_ref()
                        .expect("default controller applet is installed"),
                ),
            ))),
            AppletId::Error => Some(Box::new(Error::new(
                system,
                applet,
                broker,
                mode,
                Arc::clone(
                    self.frontend
                        .error
                        .as_ref()
                        .expect("default error applet is installed"),
                ),
            ))),
            AppletId::MiiEdit => Some(Box::new(MiiEdit::new(system, broker, mode))),
            AppletId::ProfileSelect => Some(Box::new(ProfileSelect::new(
                system,
                applet,
                broker,
                mode,
                Arc::clone(
                    self.frontend
                        .profile_select
                        .as_ref()
                        .expect("default profile-select applet is installed"),
                ),
            ))),
            AppletId::SoftwareKeyboard => Some(Box::new(SoftwareKeyboard::new(
                system,
                applet,
                broker,
                mode,
                Arc::clone(
                    self.frontend
                        .software_keyboard
                        .as_ref()
                        .expect("default software keyboard applet is installed"),
                ),
            ))),
            AppletId::PhotoViewer => Some(Box::new(PhotoViewer::new(
                system,
                applet,
                broker,
                mode,
                Arc::clone(
                    self.frontend
                        .photo_viewer
                        .as_ref()
                        .expect("default photo-viewer applet is installed"),
                ),
            ))),
            AppletId::Web
            | AppletId::Shop
            | AppletId::OfflineWeb
            | AppletId::LoginShare
            | AppletId::WebAuth
            | AppletId::Lhub => Some(Box::new(WebBrowser::new(
                system,
                applet,
                broker,
                mode,
                Arc::clone(
                    self.frontend
                        .web_browser
                        .as_ref()
                        .expect("default web-browser applet is installed"),
                ),
            ))),
            _ => {
                log::error!("No backend implementation exists for applet_id={id:?}; falling back to stub applet");
                Some(Box::new(StubApplet::new(system, applet, broker, id, mode)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::applets::applet::Applet;
    use crate::frontend::applets::controller::{ControllerParameters, ReconfigureCallback};

    struct TestControllerApplet;

    #[test]
    fn frontend_exit_removes_only_the_completed_child_without_retaining_it() {
        use crate::hle::service::am::applet::Applet as GuestApplet;
        use crate::hle::service::os::process::Process;
        use std::sync::Mutex as StdMutex;
        let parent = Arc::new(StdMutex::new(GuestApplet::new(SystemRef::null(), Process::new(), true)));
        let sibling = Arc::new(StdMutex::new(GuestApplet::new(SystemRef::null(), Process::new(), false)));
        parent.lock().unwrap().child_applets.push(Arc::clone(&sibling));
        for _ in 0..128 {
            let child = Arc::new(StdMutex::new(GuestApplet::new(SystemRef::null(), Process::new(), false)));
            child.lock().unwrap().caller_applet = Arc::downgrade(&parent);
            parent.lock().unwrap().child_applets.extend([Arc::clone(&child), Arc::clone(&child)]);
            let weak = Arc::downgrade(&child);
            exit(SystemRef::null(), &weak);
            assert!(child.lock().unwrap().is_completed);
            let children = &parent.lock().unwrap().child_applets;
            assert_eq!(children.len(), 1);
            assert!(Arc::ptr_eq(&children[0], &sibling));
            drop(child);
            assert!(weak.upgrade().is_none());
            exit(SystemRef::null(), &weak);
        }
        assert!(!sibling.lock().unwrap().is_completed);
    }

    impl Applet for TestControllerApplet {
        fn close(&self) {}
    }

    impl ControllerApplet for TestControllerApplet {
        fn reconfigure_controllers(
            &self,
            callback: ReconfigureCallback,
            _parameters: &ControllerParameters,
        ) {
            callback(true);
        }
    }

    #[test]
    fn stores_frontend_selected_applet_and_cabinet_mode() {
        let hid_core = Arc::new(Mutex::new(HIDCore::new()));
        let mut holder = FrontendAppletHolder::new(hid_core);

        holder.set_current_applet_id(AppletId::Cabinet);
        holder.set_cabinet_mode(CabinetMode::StartFormatter);

        assert_eq!(holder.get_current_applet_id(), AppletId::Cabinet);
        assert_eq!(holder.get_cabinet_mode(), CabinetMode::StartFormatter);
    }

    #[test]
    fn frontend_applet_set_replaces_only_supplied_implementations() {
        let hid_core = Arc::new(Mutex::new(HIDCore::new()));
        let mut holder = FrontendAppletHolder::new(hid_core);
        let original_keyboard = Arc::clone(
            holder
                .get_frontend_applet_set()
                .software_keyboard
                .as_ref()
                .unwrap(),
        );
        let controller: Arc<dyn ControllerApplet> = Arc::new(TestControllerApplet);

        holder.set_frontend_applet_set(FrontendAppletSet {
            cabinet: None,
            controller: Some(Arc::clone(&controller)),
            error: None,
            parental_controls: None,
            photo_viewer: None,
            profile_select: None,
            software_keyboard: None,
            web_browser: None,
        });

        assert!(Arc::ptr_eq(
            holder
                .get_frontend_applet_set()
                .controller
                .as_ref()
                .unwrap(),
            &controller,
        ));
        assert!(Arc::ptr_eq(
            holder
                .get_frontend_applet_set()
                .software_keyboard
                .as_ref()
                .unwrap(),
            &original_keyboard,
        ));
    }
}

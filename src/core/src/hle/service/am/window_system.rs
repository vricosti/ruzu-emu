// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/window_system.h
//! Port of zuyu/src/core/hle/service/am/window_system.cpp

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::am_results;
use super::am_types::*;
use super::applet::Applet;
use super::event_observer::EventObserver;
use super::lifecycle_manager::{ActivityState, LifecycleExitRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonPressDuration {
    ShortPressing,
    MiddlePressing,
    LongPressing,
}

/// Port of WindowSystem
///
/// Manages the foreground/background state of applets and dispatches
/// lifecycle events. Mirrors upstream window_system.h fields.
pub struct WindowSystem {
    /// Upstream: `Core::System& m_system`
    system: crate::core::SystemRef,

    /// Event observer — upstream: EventObserver* m_event_observer
    /// Owned here so its lifetime is tied to WindowSystem, not to the
    /// loop_process stack frame (which returns as soon as the service
    /// thread is spawned).
    event_observer: Option<Box<EventObserver>>,

    /// Lock protecting mutable state.
    lock: Mutex<WindowSystemInner>,
}

/// Interior state protected by the WindowSystem lock.
/// Upstream holds these as direct members with a std::mutex m_lock.
struct WindowSystemInner {
    /// Home menu foreground lock state.
    home_menu_foreground_locked: bool,

    /// The applet that has been requested to be in the foreground.
    /// Upstream: Applet* m_foreground_requested_applet
    foreground_requested_aruid: Option<u64>,

    /// Foreground roots — upstream: Applet* m_home_menu, Applet* m_application
    home_menu_aruid: Option<u64>,
    application_aruid: Option<u64>,
    overlay_display_aruid: Option<u64>,

    /// Applet map by aruid — upstream: std::map<u64, std::shared_ptr<Applet>>
    applets: BTreeMap<u64, Arc<Mutex<Applet>>>,

    /// Rust guest fibers can suspend while retaining `Applet`'s host mutex.
    /// Keep the lifecycle-owned exit endpoints separately reachable so the
    /// frontend can still reproduce `LifecycleManager::RequestExit`.
    exit_requests: BTreeMap<u64, Arc<LifecycleExitRequest>>,
}

impl WindowSystem {
    pub fn new(system: crate::core::SystemRef) -> Self {
        Self {
            system,
            event_observer: None,
            lock: Mutex::new(WindowSystemInner {
                home_menu_foreground_locked: false,
                foreground_requested_aruid: None,
                home_menu_aruid: None,
                application_aruid: None,
                overlay_display_aruid: None,
                applets: BTreeMap::new(),
                exit_requests: BTreeMap::new(),
            }),
        }
    }

    /// Upstream: void SetEventObserver(EventObserver* event_observer)
    ///
    /// Rust only stores the observer here. The blocking
    /// `AppletManager::set_window_system(...)` call is performed by `am.rs`
    /// after AM service registration so `loop_process()` does not deadlock
    /// before `appletOE` / `appletAE` are published.
    pub fn set_event_observer(&mut self, observer: Box<EventObserver>) {
        self.event_observer = Some(observer);
    }

    /// Upstream: void Update()
    pub fn update(&self) {
        let mut inner = self.lock.lock().unwrap();

        // Loop through all applets and remove terminated applets.
        self.prune_terminated_applets_locked(&mut inner);

        // If the home menu is being locked into the foreground, handle that.
        if self.lock_home_menu_into_foreground_locked(&mut inner) {
            return;
        }

        let overlay_blocks_input = inner
            .overlay_display_aruid
            .and_then(|aruid| inner.applets.get(&aruid))
            .is_some_and(|applet| applet.lock().unwrap().overlay_in_foreground);

        // Recursively update each applet root.
        let home_menu_aruid = inner.home_menu_aruid;
        let application_aruid = inner.application_aruid;
        let overlay_display_aruid = inner.overlay_display_aruid;
        let foreground = inner.foreground_requested_aruid;

        if let Some(aruid) = home_menu_aruid {
            let is_foreground = foreground == Some(aruid);
            if let Some(applet) = inner.applets.get(&aruid).cloned() {
                self.update_applet_state_locked(
                    &inner,
                    &applet,
                    is_foreground,
                    overlay_blocks_input,
                );
            }
        }
        if let Some(aruid) = application_aruid {
            let is_foreground = foreground == Some(aruid);
            if let Some(applet) = inner.applets.get(&aruid).cloned() {
                self.update_applet_state_locked(
                    &inner,
                    &applet,
                    is_foreground,
                    overlay_blocks_input,
                );
            }
        }
        if let Some(aruid) = overlay_display_aruid {
            if let Some(applet) = inner.applets.get(&aruid).cloned() {
                self.update_applet_state_locked(&inner, &applet, true, false);
            }
        }
    }

    /// Upstream: void TrackApplet(std::shared_ptr<Applet> applet, bool is_application)
    pub fn track_applet(&self, applet: Arc<Mutex<Applet>>, is_application: bool) {
        let (aruid, exit_request) = {
            let a = applet.lock().unwrap();
            (a.aruid.pid, a.lifecycle_manager.exit_request_handle())
        };

        let mut inner = self.lock.lock().unwrap();

        {
            let a = applet.lock().unwrap();
            if a.applet_id == AppletId::QLaunch {
                assert!(inner.home_menu_aruid.is_none(), "Home menu already tracked");
                inner.home_menu_aruid = Some(aruid);
            } else if a.applet_id == AppletId::OverlayDisplay {
                inner.overlay_display_aruid = Some(aruid);
            } else if is_application {
                assert!(
                    inner.application_aruid.is_none(),
                    "Application already tracked"
                );
                inner.application_aruid = Some(aruid);
            }
        }

        // Upstream: m_event_observer->TrackAppletProcess(*applet)
        if let Some(ref observer) = self.event_observer {
            observer.track_applet_process(&applet);
        }

        inner.applets.insert(aruid, applet);
        inner.exit_requests.insert(aruid, exit_request);
    }

    /// Upstream: std::shared_ptr<Applet> GetByAppletResourceUserId(u64 aruid)
    pub fn get_by_applet_resource_user_id(&self, aruid: u64) -> Option<Arc<Mutex<Applet>>> {
        let inner = self.lock.lock().unwrap();
        inner.applets.get(&aruid).cloned()
    }

    /// Upstream: std::shared_ptr<Applet> GetMainApplet()
    pub fn get_main_applet(&self) -> Option<Arc<Mutex<Applet>>> {
        let inner = self.lock.lock().unwrap();
        if let Some(aruid) = inner.application_aruid {
            inner.applets.get(&aruid).cloned()
        } else {
            None
        }
    }

    /// Upstream: `WindowSystem::GetOverlayDisplayApplet`.
    pub fn get_overlay_display_applet(&self) -> Option<Arc<Mutex<Applet>>> {
        let inner = self.lock.lock().unwrap();
        inner
            .overlay_display_aruid
            .and_then(|aruid| inner.applets.get(&aruid).cloned())
    }

    /// Upstream: void RequestHomeMenuToGetForeground()
    pub fn request_home_menu_to_get_foreground(&self) {
        {
            let mut inner = self.lock.lock().unwrap();
            inner.foreground_requested_aruid = inner.home_menu_aruid;
        }
        self.request_update();
    }

    /// Upstream: void RequestApplicationToGetForeground()
    pub fn request_application_to_get_foreground(&self) {
        {
            let mut inner = self.lock.lock().unwrap();
            inner.foreground_requested_aruid = inner.application_aruid;
        }
        self.request_update();
    }

    /// Upstream: void RequestLockHomeMenuIntoForeground()
    pub fn request_lock_home_menu_into_foreground(&self) {
        {
            let mut inner = self.lock.lock().unwrap();
            inner.home_menu_foreground_locked = true;
        }
        self.request_update();
    }

    /// Upstream: void RequestUnlockHomeMenuIntoForeground()
    pub fn request_unlock_home_menu_into_foreground(&self) {
        {
            let mut inner = self.lock.lock().unwrap();
            inner.home_menu_foreground_locked = false;
        }
        self.request_update();
    }

    /// Upstream: void RequestAppletVisibilityState(Applet& applet, bool visible)
    pub fn request_applet_visibility_state(&self, applet: &Arc<Mutex<Applet>>, visible: bool) {
        {
            let mut a = applet.lock().unwrap();
            a.window_visible = visible;
        }
        self.request_update();
    }

    /// Upstream: void OnOperationModeChanged()
    pub fn on_operation_mode_changed(&self) {
        let inner = self.lock.lock().unwrap();
        for (_aruid, applet) in &inner.applets {
            let mut a = applet.lock().unwrap();
            a.lifecycle_manager
                .on_operation_and_performance_mode_changed();
        }
    }

    /// Upstream: void OnExitRequested()
    pub fn on_exit_requested(&self) {
        let inner = self.lock.lock().unwrap();
        for exit_request in inner.exit_requests.values() {
            exit_request.request();
        }
    }

    /// Upstream: void OnHomeButtonPressed(ButtonPressDuration type)
    pub fn on_home_button_pressed(&self, duration: ButtonPressDuration) {
        let inner = self.lock.lock().unwrap();

        // If we don't have a home menu, nothing to do.
        let home_aruid = match inner.home_menu_aruid {
            Some(aruid) => aruid,
            None => return,
        };

        let home_applet = match inner.applets.get(&home_aruid) {
            Some(a) => a,
            None => return,
        };

        let mut a = home_applet.lock().unwrap();

        // Send home button press event to home menu.
        if duration == ButtonPressDuration::ShortPressing {
            a.lifecycle_manager
                .push_unordered_message(AppletMessage::DetectShortPressingHomeButton);
        }
    }

    /// Upstream: void OnCaptureButtonPressed(ButtonPressDuration type) — no-op
    pub fn on_capture_button_pressed(&self, _duration: ButtonPressDuration) {
        // No-op, matching upstream
    }

    /// Upstream: void OnPowerButtonPressed(ButtonPressDuration type) — no-op
    pub fn on_power_button_pressed(&self, _duration: ButtonPressDuration) {
        // No-op, matching upstream
    }

    // --- Private helpers ---

    fn request_update(&self) {
        if let Some(ref observer) = self.event_observer {
            observer.request_update();
        }
    }

    /// Upstream: void PruneTerminatedAppletsLocked()
    fn prune_terminated_applets_locked(&self, inner: &mut WindowSystemInner) {
        let aruids: Vec<u64> = inner.applets.keys().copied().collect();

        for aruid in aruids {
            let applet = match inner.applets.get(&aruid) {
                Some(a) => a.clone(),
                None => continue,
            };

            let mut a = applet.lock().unwrap();

            // Frontend applets mirror upstream's processless `Process` owner.
            // Their `FrontendApplet::Exit` completion is therefore the only
            // termination state available to the window-system owner.
            let processless_frontend_completed = !a.process.is_initialized() && a.is_completed;
            if !a.process.is_terminated() && !processless_frontend_completed {
                continue;
            }

            if std::env::var_os("RUZU_TRACE_APPLET_RETURN").is_some() {
                log::info!(
                    "[APPLET_RETURN] prune aruid={} applet_id={:?} children={} caller={}",
                    aruid,
                    a.applet_id,
                    a.child_applets.len(),
                    a.caller_applet.strong_count() != 0
                );
            }

            // Terminated, so ensure all child applets are terminated.
            if !a.child_applets.is_empty() {
                // Terminate child applets.
                let children = a.child_applets.clone();
                for child in &children {
                    let mut c = child.lock().unwrap();
                    c.process.terminate();
                    c.terminate_result = am_results::RESULT_LIBRARY_APPLET_TERMINATED.0;
                }
                // Not ready to unlink until all child applets are terminated.
                continue;
            }

            // Erase from caller applet's list of children.
            if let Some(caller) = a.caller_applet.upgrade() {
                let mut caller_a = caller.lock().unwrap();
                caller_a.child_applets.retain(|c| !Arc::ptr_eq(c, &applet));
                drop(a);
                a = applet.lock().unwrap();
                a.caller_applet = std::sync::Weak::new();
            }

            // If this applet was foreground, it no longer is.
            if inner.foreground_requested_aruid == Some(aruid) {
                inner.foreground_requested_aruid = None;
            }

            // If this was the home menu, clean up.
            if inner.home_menu_aruid == Some(aruid) {
                inner.home_menu_aruid = None;
                inner.foreground_requested_aruid = inner.application_aruid;
            }

            // If this was the application, try to switch to the home menu.
            if inner.application_aruid == Some(aruid) {
                inner.application_aruid = None;
                inner.foreground_requested_aruid = inner.home_menu_aruid;

                // If we have a home menu, send it the application exited message.
                if let Some(home_aruid) = inner.home_menu_aruid {
                    if let Some(home_applet) = inner.applets.get(&home_aruid) {
                        let mut home_a = home_applet.lock().unwrap();
                        home_a
                            .lifecycle_manager
                            .push_unordered_message(AppletMessage::ApplicationExited);
                    }
                }
            }

            if inner.overlay_display_aruid == Some(aruid) {
                inner.overlay_display_aruid = None;
            }

            // Finalize applet. This also signals the state-changed event so
            // the caller observing ILibraryAppletAccessor can resume.
            a.on_process_terminated_locked();

            if std::env::var_os("RUZU_TRACE_APPLET_RETURN").is_some() {
                log::info!(
                    "[APPLET_RETURN] completed aruid={} applet_id={:?} state_event_present={}",
                    aruid,
                    a.applet_id,
                    a.state_changed_event.is_some()
                );
            }

            // Request update to ensure quiescence.
            if let Some(ref observer) = self.event_observer {
                observer.request_update();
            }

            // Unlink.
            drop(a);
            inner.applets.remove(&aruid);
            inner.exit_requests.remove(&aruid);
        }

        // If the last applet has exited, exit the system.
        if inner.applets.is_empty() {
            // Production construction always supplies the upstream-owned
            // `Core::System&`. A null reference is supported only by isolated
            // Rust service tests that do not own a frontend.
            if !self.system.is_null() {
                self.system.get().exit();
            }
        }
    }

    /// Upstream: bool LockHomeMenuIntoForegroundLocked()
    fn lock_home_menu_into_foreground_locked(&self, inner: &mut WindowSystemInner) -> bool {
        let home_aruid = match inner.home_menu_aruid {
            Some(aruid) => aruid,
            None => {
                inner.home_menu_foreground_locked = false;
                return false;
            }
        };

        if !inner.home_menu_foreground_locked {
            inner.home_menu_foreground_locked = false;
            return false;
        }

        let home_applet = match inner.applets.get(&home_aruid) {
            Some(a) => a.clone(),
            None => {
                inner.home_menu_foreground_locked = false;
                return false;
            }
        };

        let mut a = home_applet.lock().unwrap();

        // Terminate any direct child applets of the home menu.
        let children = a.child_applets.clone();
        for child in &children {
            let mut c = child.lock().unwrap();
            c.process.terminate();
            c.terminate_result = am_results::RESULT_LIBRARY_APPLET_TERMINATED.0;
        }

        // When there are zero child applets left, we can proceed with the update.
        if a.child_applets.is_empty() {
            a.window_visible = true;
            inner.foreground_requested_aruid = Some(home_aruid);
            return false;
        }

        true
    }

    /// Upstream: void UpdateAppletStateLocked(Applet* applet, bool is_foreground)
    fn update_applet_state_locked(
        &self,
        inner: &WindowSystemInner,
        applet: &Arc<Mutex<Applet>>,
        is_foreground: bool,
        overlay_blocking: bool,
    ) {
        let mut a = applet.lock().unwrap();

        let inherited_foreground = a.is_process_running && is_foreground;
        let visible_state = if inherited_foreground {
            ActivityState::ForegroundVisible
        } else {
            ActivityState::BackgroundVisible
        };
        let obscured_state = if inherited_foreground {
            ActivityState::ForegroundObscured
        } else {
            ActivityState::BackgroundObscured
        };

        let has_obscuring_child_applets = {
            let mut found = false;
            for child in &a.child_applets {
                let c = child.lock().unwrap();
                let mode = c.library_applet_mode;
                if c.is_process_running
                    && c.window_visible
                    && (mode == LibraryAppletMode::AllForeground
                        || mode == LibraryAppletMode::AllForegroundInitiallyHidden)
                {
                    found = true;
                    break;
                }
            }
            found
        };

        // Update visibility state.
        let should_be_visible = if a.applet_id == AppletId::OverlayDisplay {
            a.window_visible
        } else {
            is_foreground && a.window_visible
        };
        a.display_layer_manager
            .set_window_visibility(should_be_visible);

        // Update interactibility state.
        let should_be_interactible = if a.applet_id == AppletId::OverlayDisplay {
            a.overlay_in_foreground
        } else {
            is_foreground && a.window_visible && !overlay_blocking
        };
        a.set_interactible_locked(should_be_interactible);

        // Update focus state and suspension.
        let is_obscured = has_obscuring_child_applets || !a.window_visible;
        let state = a.lifecycle_manager.get_activity_state();

        if is_obscured && state != obscured_state {
            a.lifecycle_manager.set_activity_state(obscured_state);
            a.update_suspension_state_locked(true);
        } else if !is_obscured && state != visible_state {
            a.lifecycle_manager.set_activity_state(visible_state);
            a.update_suspension_state_locked(true);
        }

        let z_index = if a.applet_id == AppletId::OverlayDisplay {
            if a.overlay_in_foreground {
                100_000
            } else {
                -1
            }
        } else if inherited_foreground && !is_obscured {
            2
        } else if inherited_foreground {
            1
        } else {
            0
        };
        a.display_layer_manager.set_overlay_z_index(z_index);

        // Recurse into child applets.
        let children = a.child_applets.clone();
        drop(a);
        for child in &children {
            self.update_applet_state_locked(inner, child, is_foreground, overlay_blocking);
        }
    }
}

impl Drop for WindowSystem {
    fn drop(&mut self) {
        if !self.system.is_null() {
            // The `None` branch in `AppletManager::set_window_system(...)` is
            // intentionally non-blocking; Drop must never wait for pending
            // frontend process parameters here.
            self.system
                .get()
                .get_applet_manager()
                .set_window_system(None::<Arc<Mutex<WindowSystem>>>);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::service::os::process::Process;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn empty_window_system_requests_frontend_exit() {
        let mut system = crate::core::System::new();
        let exit_requested = Arc::new(AtomicBool::new(false));
        let callback_flag = Arc::clone(&exit_requested);
        system.register_exit_callback(Box::new(move || {
            callback_flag.store(true, Ordering::Release);
        }));

        let window_system = WindowSystem::new(crate::core::SystemRef::from_ref(&system));
        window_system.update();

        assert!(exit_requested.load(Ordering::Acquire));
    }

    #[test]
    fn completed_processless_frontend_applet_is_pruned() {
        let window_system = WindowSystem::new(crate::core::SystemRef::null());
        let mut applet = Applet::new(crate::core::SystemRef::null(), Process::new(), false);
        applet.is_completed = true;
        let applet = Arc::new(Mutex::new(applet));

        window_system.track_applet(Arc::clone(&applet), false);
        assert!(window_system.get_by_applet_resource_user_id(0).is_some());

        window_system.update();

        assert!(window_system.get_by_applet_resource_user_id(0).is_none());
        assert!(window_system.lock.lock().unwrap().exit_requests.is_empty());
    }

    #[test]
    fn exit_request_reaches_a_busy_applet_fiber() {
        let window_system = WindowSystem::new(crate::core::SystemRef::null());

        let mut busy_applet = Applet::new(crate::core::SystemRef::null(), Process::new(), false);
        busy_applet.aruid = AppletResourceUserId { pid: 1 };
        let busy_applet = Arc::new(Mutex::new(busy_applet));
        window_system.track_applet(Arc::clone(&busy_applet), false);

        let mut available_applet =
            Applet::new(crate::core::SystemRef::null(), Process::new(), false);
        available_applet.aruid = AppletResourceUserId { pid: 2 };
        let available_applet = Arc::new(Mutex::new(available_applet));
        window_system.track_applet(Arc::clone(&available_applet), false);

        let busy_guard = busy_applet.lock().unwrap();
        window_system.on_exit_requested();

        assert!(busy_guard.lifecycle_manager.get_exit_requested());
        assert!(busy_guard
            .lifecycle_manager
            .get_system_event()
            .is_signaled());
        assert!(available_applet
            .lock()
            .unwrap()
            .lifecycle_manager
            .get_exit_requested());

        drop(busy_guard);
        let mut busy_applet = busy_applet.lock().unwrap();
        let mut message = AppletMessage::None;
        assert!(busy_applet.lifecycle_manager.pop_message(&mut message));
        assert_eq!(message, AppletMessage::Exit);
        assert!(!busy_applet
            .lifecycle_manager
            .get_system_event()
            .is_signaled());
    }

    #[test]
    fn foreground_overlay_owns_input_without_hiding_the_application() {
        let window_system = WindowSystem::new(crate::core::SystemRef::null());

        let mut application = Applet::new(crate::core::SystemRef::null(), Process::new(), true);
        application.aruid = AppletResourceUserId { pid: 1 };
        application.applet_id = AppletId::Application;
        application.is_process_running = true;
        let application = Arc::new(Mutex::new(application));

        let mut overlay = Applet::new(crate::core::SystemRef::null(), Process::new(), false);
        overlay.aruid = AppletResourceUserId { pid: 2 };
        overlay.applet_id = AppletId::OverlayDisplay;
        overlay.is_process_running = true;
        overlay.overlay_in_foreground = true;
        let overlay = Arc::new(Mutex::new(overlay));

        window_system.track_applet(Arc::clone(&application), true);
        window_system.track_applet(Arc::clone(&overlay), false);
        window_system.request_application_to_get_foreground();
        window_system.update();

        assert!(!application.lock().unwrap().is_interactible);
        assert!(application
            .lock()
            .unwrap()
            .display_layer_manager
            .get_window_visibility());
        assert!(overlay.lock().unwrap().is_interactible);

        overlay.lock().unwrap().overlay_in_foreground = false;
        window_system.update();

        assert!(application.lock().unwrap().is_interactible);
        assert!(!overlay.lock().unwrap().is_interactible);
    }
}

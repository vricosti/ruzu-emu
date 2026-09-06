// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/event_observer.h
//! Port of zuyu/src/core/hle/service/am/event_observer.cpp

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::applet::Applet;
use super::process_holder::ProcessHolder;
use super::window_system::WindowSystem;
use crate::hle::kernel::k_process::ProcessState;
use crate::hle::service::os::event::Event;
use crate::hle::service::os::multi_wait::MultiWait;
use crate::hle::service::os::multi_wait_holder::MultiWaitHolder;

/// Tag for multi-wait user data.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserDataTag {
    WakeupEvent = 0,
    AppletProcess = 1,
}

pub struct EventObserver {
    shared: Arc<SharedState>,
    thread: Option<std::thread::JoinHandle<()>>,
    context: crate::hle::service::kernel_helpers::ServiceContext,
    wakeup_event_handle: u32,
}

struct SharedState {
    window_system: usize,
    wakeup_event: Arc<Event>,
    stop_requested: AtomicBool,
    state: Mutex<ObserverState>,
}

struct ObserverState {
    process_holder_list: Vec<Box<ProcessHolder>>,
    wakeup_holder: Box<MultiWaitHolder>,
    multi_wait: MultiWait,
    deferred_wait_list: MultiWait,
}

impl EventObserver {
    /// Upstream: EventObserver(Core::System& system, WindowSystem& window_system)
    ///
    /// Creates the event observer and starts the background processing thread.
    /// The WindowSystem pointer must remain valid for the lifetime of this
    /// EventObserver (matching upstream lifetime contract).
    pub fn new(window_system: *const WindowSystem) -> Self {
        let mut context = crate::hle::service::kernel_helpers::ServiceContext::new("am:EventObserver".into());
        let wakeup_event_handle = context.create_event("Event".into());
        let wakeup_event = context.get_event(wakeup_event_handle).expect("observer wakeup event must exist");
        let mut observer_state = ObserverState {
            process_holder_list: Vec::new(),
            wakeup_holder: Box::new(MultiWaitHolder::from_event(wakeup_event.clone())),
            multi_wait: MultiWait::new(),
            deferred_wait_list: MultiWait::new(),
        };
        observer_state
            .wakeup_holder
            .set_user_data(UserDataTag::WakeupEvent as usize);
        let shared = Arc::new(SharedState {
            window_system: window_system as usize,
            wakeup_event,
            stop_requested: AtomicBool::new(false),
            state: Mutex::new(observer_state),
        });
        // Link after the Rust move into its stable owner, as upstream's
        // constructor links members at their final addresses.
        {
            let mut state = shared.state.lock().unwrap();
            let multi_wait = &mut state.multi_wait as *mut MultiWait;
            state.wakeup_holder.link_to_multi_wait(multi_wait);
        }

        let shared_clone = shared.clone();

        let thread = std::thread::Builder::new()
            .name("am:EventObserver".into())
            .spawn(move || {
                Self::thread_func(shared_clone);
            })
            .expect("Failed to spawn EventObserver thread");

        Self {
            shared,
            thread: Some(thread),
            context,
            wakeup_event_handle,
        }
    }

    /// Upstream: void TrackAppletProcess(Applet& applet)
    ///
    pub fn track_applet_process(&self, applet: &Arc<Mutex<Applet>>) {
        let (process, aruid, applet_id) = {
            let applet = applet.lock().unwrap();
            if !applet.process.is_initialized() {
                return;
            }
            let Some(process) = applet.process.get_handle() else {
                return;
            };
            (process, applet.aruid.pid, applet.applet_id)
        };

        if std::env::var_os("RUZU_TRACE_APPLET_RETURN").is_some() {
            log::info!(
                "[APPLET_RETURN] track aruid={} applet_id={:?}",
                aruid,
                applet_id
            );
        }

        let mut holder = Box::new(ProcessHolder::new(applet.clone(), process));
        holder
            .get_multi_wait_holder_mut()
            .set_user_data(UserDataTag::AppletProcess as usize);

        let mut state = self.shared.state.lock().unwrap();
        if state
            .process_holder_list
            .iter()
            .any(|existing| Arc::ptr_eq(existing.get_process(), holder.get_process()))
        {
            return;
        }

        holder
            .get_multi_wait_holder_mut()
            .link_to_multi_wait(&mut state.deferred_wait_list as *mut MultiWait);
        state.process_holder_list.push(holder);
        drop(state);
        self.shared.wakeup_event.signal();
    }

    /// Upstream: void RequestUpdate()
    pub fn request_update(&self) {
        self.shared.wakeup_event.signal();
    }

    fn link_deferred(shared: &SharedState) {
        let mut state = shared.state.lock().unwrap();
        // Upstream: m_multi_wait.MoveAll(std::addressof(m_deferred_wait_list));
        //
        // CRITICAL: We must NOT use std::mem::take() to swap the deferred list
        // out of `state` and back, because each holder stores a raw pointer to
        // the MultiWait it belongs to. Moving the MultiWait to a different
        // memory location invalidates those pointers and makes
        // `unlink_from_multi_wait` a no-op, causing `move_all`'s while-loop to
        // spin forever and burn 100% CPU on the EventObserver thread.
        //
        // Split-borrow the two fields so the holder pointers stay valid
        // relative to `state.deferred_wait_list`.
        let state_ref = &mut *state;
        state_ref
            .multi_wait
            .move_all(&mut state_ref.deferred_wait_list);
    }

    fn wait_signaled(shared: &SharedState) -> Option<*mut MultiWaitHolder> {
        loop {
            Self::link_deferred(shared);

            if shared.stop_requested.load(Ordering::Acquire) {
                return None;
            }

            let multi_wait = {
                let state = shared.state.lock().unwrap();
                &state.multi_wait as *const MultiWait
            };
            let kernel = crate::hle::kernel::kernel::get_kernel_ref()
                .expect("EventObserver requires a live kernel");
            // SharedState has a stable Arc owner. Only this observer thread
            // mutates multi_wait; producers modify the separate deferred list.
            // Release state before blocking so producers can link and signal.
            let Some(holder) = (unsafe { (&*multi_wait).wait_any(kernel) }) else {
                continue;
            };
            let tag = unsafe { (*holder).get_user_data() };
            if tag != UserDataTag::WakeupEvent as usize {
                shared.state.lock().unwrap().multi_wait.unlink_holder(holder);
            }
            return Some(holder);
        }
    }

    fn process(shared: &SharedState, holder: *mut MultiWaitHolder) {
        let user_data = unsafe { (*holder).get_user_data() };
        match user_data {
            x if x == UserDataTag::WakeupEvent as usize => {
                shared.wakeup_event.clear();
                let window_system = unsafe { &*(shared.window_system as *const WindowSystem) };
                window_system.update();
            }
            x if x == UserDataTag::AppletProcess as usize => {
                let (applet, process_running, terminated, process_state) = {
                    let mut state = shared.state.lock().unwrap();
                    let Some(index) = state.process_holder_list.iter().position(|candidate| {
                        std::ptr::eq(candidate.get_multi_wait_holder(), unsafe { &*holder })
                    }) else {
                        return;
                    };

                    let process = state.process_holder_list[index].get_process().clone();
                    let applet = state.process_holder_list[index].get_applet().clone();
                    let (terminated, process_running, process_state) = {
                        let process = process.lock().unwrap();
                        let state = process.get_state();
                        (
                            process.is_terminated(),
                            state == ProcessState::Running
                                || state == ProcessState::RunningAttached
                                || state == ProcessState::DebugBreak,
                            state,
                        )
                    };

                    if terminated {
                        state.process_holder_list.remove(index);
                    } else {
                        let deferred_wait_list = &mut state.deferred_wait_list as *mut MultiWait;
                        process.lock().unwrap().reset();
                        state.process_holder_list[index]
                            .get_multi_wait_holder_mut()
                            .link_to_multi_wait(deferred_wait_list);
                    }

                    (applet, process_running, terminated, process_state)
                };

                {
                    let mut applet = applet.lock().unwrap();
                    if std::env::var_os("RUZU_TRACE_APPLET_RETURN").is_some() {
                        log::info!(
                            "[APPLET_RETURN] process event aruid={} applet_id={:?} state={:?} terminated={} running={}",
                            applet.aruid.pid,
                            applet.applet_id,
                            process_state,
                            terminated,
                            process_running
                        );
                    }
                    applet.is_process_running = process_running;
                }

                let window_system = unsafe { &*(shared.window_system as *const WindowSystem) };
                window_system.update();
            }
            _ => unreachable!(),
        }
    }

    fn thread_func(shared: Arc<SharedState>) {
        while let Some(holder) = Self::wait_signaled(&shared) {
            Self::process(&shared, holder);
        }
    }
}

impl Drop for EventObserver {
    fn drop(&mut self) {
        self.shared.stop_requested.store(true, Ordering::Release);
        self.shared.wakeup_event.signal();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.context.close_event(self.wakeup_event_handle);
    }
}

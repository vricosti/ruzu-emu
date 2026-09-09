// SPDX-FileCopyrightText: Copyright 2017 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden src/network/announce_multiplayer_session.h and
//! announce_multiplayer_session.cpp
//!
//! Instruments `AnnounceMultiplayerRoom::Backend`. Creates a thread that
//! regularly updates the room information and submits them. An async get of
//! room information is also possible.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use common::announce_multiplayer_room::{Backend, RoomList, WebResult, WebResultCode};
use common::thread::Event;

use crate::network::RoomNetwork;
use crate::room::{Room, RoomState, NETWORK_VERSION};

// ---------------------------------------------------------------------------
// AnnounceMultiplayerSession
// ---------------------------------------------------------------------------

/// Callback handle for error callbacks.
pub type CallbackHandle = Arc<Box<dyn Fn(&WebResult) + Send + Sync>>;

/// Instruments `AnnounceMultiplayerRoom::Backend`.
/// Maps to C++ `Core::AnnounceMultiplayerSession`.
pub struct AnnounceMultiplayerSession {
    shutdown_event: Arc<Event>,
    error_callbacks: Arc<Mutex<Vec<CallbackHandle>>>,
    announce_multiplayer_thread: Mutex<Option<JoinHandle<()>>>,

    /// Backend interface that logs fields.
    backend: Arc<Mutex<Box<dyn Backend>>>,

    /// Whether the room has been registered.
    registered: Arc<AtomicBool>,

    room: Weak<Room>,
}

impl AnnounceMultiplayerSession {
    /// Creates a new session.
    ///
    /// The explicit `RoomNetwork` reference replaces Eden's module-global
    /// `Network::GetRoom()` owner without changing the room lifetime.
    pub fn new(room_network: &RoomNetwork) -> Self {
        let values = common::settings::values();
        let backend: Box<dyn Backend> = Box::new(web_service::announce_room_json::RoomJson::new(
            values.web_api_url.get_value(),
            values.eden_username.get_value(),
            values.eden_token.get_value(),
        ));

        Self {
            shutdown_event: Arc::new(Event::new()),
            error_callbacks: Arc::new(Mutex::new(Vec::new())),
            announce_multiplayer_thread: Mutex::new(None),
            backend: Arc::new(Mutex::new(backend)),
            registered: Arc::new(AtomicBool::new(false)),
            room: room_network.get_room(),
        }
    }

    #[cfg(test)]
    fn with_backend(room_network: &RoomNetwork, backend: Box<dyn Backend>) -> Self {
        Self {
            shutdown_event: Arc::new(Event::new()),
            error_callbacks: Arc::new(Mutex::new(Vec::new())),
            announce_multiplayer_thread: Mutex::new(None),
            backend: Arc::new(Mutex::new(backend)),
            registered: Arc::new(AtomicBool::new(false)),
            room: room_network.get_room(),
        }
    }

    /// Allows binding a function that will get called if the announce
    /// encounters an error.
    pub fn bind_error_callback(
        &self,
        callback: impl Fn(&WebResult) + Send + Sync + 'static,
    ) -> CallbackHandle {
        let handle: CallbackHandle = Arc::new(Box::new(callback));
        self.error_callbacks.lock().push(handle.clone());
        handle
    }

    /// Unbind a function from the error callbacks.
    pub fn unbind_error_callback(&self, handle: &CallbackHandle) {
        let mut callbacks = self.error_callbacks.lock();
        callbacks.retain(|h| !Arc::ptr_eq(h, handle));
    }

    /// Registers a room to web services.
    pub fn register(&self) -> WebResult {
        Self::register_impl(&self.room, &self.backend, &self.registered)
    }

    fn register_impl(
        room: &Weak<Room>,
        backend: &Mutex<Box<dyn Backend>>,
        registered: &AtomicBool,
    ) -> WebResult {
        let Some(room) = room.upgrade() else {
            return WebResult {
                result_code: WebResultCode::LibError,
                result_string: "Network is not initialized".to_string(),
                returned_data: String::new(),
            };
        };
        if room.get_state() != RoomState::Open {
            return WebResult {
                result_code: WebResultCode::LibError,
                result_string: "Room is not open".to_string(),
                returned_data: String::new(),
            };
        }
        let result = {
            let mut backend = backend.lock();
            Self::update_backend_data(&room, backend.as_mut());
            backend.register()
        };
        if result.result_code != WebResultCode::Success {
            return result;
        }
        log::info!("Room has been registered");
        room.set_verify_uid(&result.returned_data);
        registered.store(true, Ordering::SeqCst);
        WebResult {
            result_code: WebResultCode::Success,
            result_string: String::new(),
            returned_data: String::new(),
        }
    }

    /// Starts the announce of a room to web services.
    pub fn start(&self) {
        if self.is_running() {
            self.stop();
        }

        let shutdown_event = Arc::clone(&self.shutdown_event);
        let error_callbacks = Arc::clone(&self.error_callbacks);
        let backend = Arc::clone(&self.backend);
        let registered = Arc::clone(&self.registered);
        let room = self.room.clone();
        let thread = std::thread::spawn(move || {
            let error_callback = |result: WebResult| {
                let callbacks = error_callbacks.lock();
                for callback in callbacks.iter() {
                    callback(&result);
                }
            };

            if !registered.load(Ordering::SeqCst) {
                let result = Self::register_impl(&room, &backend, &registered);
                if result.result_code != WebResultCode::Success {
                    error_callback(result);
                    return;
                }
            }

            // Time between room announcements to web_service.
            const ANNOUNCE_TIME_INTERVAL: Duration = Duration::from_secs(15);
            let mut update_time = Instant::now();
            loop {
                let wait_time = update_time.saturating_duration_since(Instant::now());
                if shutdown_event.wait_for(wait_time) {
                    break;
                }
                update_time = Instant::now() + ANNOUNCE_TIME_INTERVAL;

                let Some(room_handle) = room.upgrade() else {
                    break;
                };
                if room_handle.get_state() != RoomState::Open {
                    break;
                }
                let result = {
                    let mut backend = backend.lock();
                    Self::update_backend_data(&room_handle, backend.as_mut());
                    backend.update()
                };
                if result.result_code != WebResultCode::Success {
                    error_callback(result.clone());
                }
                if result.result_string == "404" {
                    registered.store(false, Ordering::SeqCst);
                    let register_result = Self::register_impl(&room, &backend, &registered);
                    if register_result.result_code != WebResultCode::Success {
                        error_callback(register_result);
                    }
                }
            }
        });
        *self.announce_multiplayer_thread.lock() = Some(thread);
    }

    /// Stops the announce to web services.
    pub fn stop(&self) {
        let thread = self.announce_multiplayer_thread.lock().take();
        if let Some(thread) = thread {
            self.shutdown_event.set();
            let _ = thread.join();
            self.backend.lock().delete();
            self.registered.store(false, Ordering::SeqCst);
        }
    }

    /// Returns a list of all room information the backend got.
    pub fn get_room_list(&self) -> RoomList {
        self.backend.lock().get_room_list()
    }

    /// Whether the announce session is still running.
    pub fn is_running(&self) -> bool {
        self.announce_multiplayer_thread.lock().is_some()
    }

    /// Recreates the backend, updating the credentials.
    /// This can only be used when the announce session is not running.
    pub fn update_credentials(&self) {
        assert!(
            !self.is_running(),
            "Credentials can only be updated when session is not running"
        );
        let values = common::settings::values();
        *self.backend.lock() = Box::new(web_service::announce_room_json::RoomJson::new(
            values.web_api_url.get_value(),
            values.eden_username.get_value(),
            values.eden_token.get_value(),
        ));
    }

    fn update_backend_data(room: &Room, backend: &mut dyn Backend) {
        let room_information = room.get_room_information();
        let member_list = room.get_room_member_list();
        backend.set_room_information(
            &room_information.name,
            &room_information.description,
            room_information.port,
            room_information.member_slots,
            NETWORK_VERSION,
            room.has_password(),
            &room_information.preferred_game,
        );
        backend.clear_players();
        for member in &member_list {
            backend.add_player(member);
        }
    }
}

impl Drop for AnnounceMultiplayerSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::RoomNetwork;
    use common::announce_multiplayer_room::{GameInfo, Member};
    use std::sync::mpsc::{self, Sender};

    #[derive(Default)]
    struct BackendState {
        name: String,
        description: String,
        port: u16,
        member_slots: u32,
        network_version: u32,
        has_password: bool,
        preferred_game: String,
        clear_calls: usize,
        register_calls: usize,
        update_calls: usize,
        delete_calls: usize,
    }

    struct RecordingBackend {
        state: Arc<Mutex<BackendState>>,
        register_result: WebResult,
        update_result: WebResult,
        register_signal: Option<Sender<usize>>,
        update_signal: Option<Sender<()>>,
    }

    impl RecordingBackend {
        fn new(state: Arc<Mutex<BackendState>>) -> Self {
            Self {
                state,
                register_result: WebResult {
                    result_code: WebResultCode::Success,
                    result_string: String::new(),
                    returned_data: "verification-id".to_string(),
                },
                update_result: WebResult {
                    result_code: WebResultCode::Success,
                    result_string: String::new(),
                    returned_data: String::new(),
                },
                register_signal: None,
                update_signal: None,
            }
        }
    }

    impl Backend for RecordingBackend {
        fn set_room_information(
            &mut self,
            name: &str,
            description: &str,
            port: u16,
            max_player: u32,
            net_version: u32,
            has_password: bool,
            preferred_game: &GameInfo,
        ) {
            let mut state = self.state.lock();
            state.name = name.to_string();
            state.description = description.to_string();
            state.port = port;
            state.member_slots = max_player;
            state.network_version = net_version;
            state.has_password = has_password;
            state.preferred_game = preferred_game.name.clone();
        }

        fn add_player(&mut self, _member: &Member) {}

        fn update(&mut self) -> WebResult {
            self.state.lock().update_calls += 1;
            if let Some(signal) = &self.update_signal {
                let _ = signal.send(());
            }
            self.update_result.clone()
        }

        fn register(&mut self) -> WebResult {
            let calls = {
                let mut state = self.state.lock();
                state.register_calls += 1;
                state.register_calls
            };
            if let Some(signal) = &self.register_signal {
                let _ = signal.send(calls);
            }
            self.register_result.clone()
        }

        fn clear_players(&mut self) {
            self.state.lock().clear_calls += 1;
        }

        fn get_room_list(&mut self) -> RoomList {
            Vec::new()
        }

        fn delete(&mut self) {
            self.state.lock().delete_calls += 1;
        }
    }

    fn open_room(network: &RoomNetwork) -> Arc<Room> {
        let room = network.get_room().upgrade().unwrap();
        assert!(room.create(
            "Free Room",
            "Free homebrew multiplayer",
            "",
            24872,
            "secret",
            4,
            "FreeHost",
            GameInfo {
                name: "OpenArenaNX".to_string(),
                id: 0,
                version: "1.0".to_string(),
            },
            None,
            &(Vec::new(), Vec::new()),
            false,
        ));
        room
    }

    #[test]
    fn register_rejects_missing_and_closed_rooms_like_upstream() {
        let state = Arc::new(Mutex::new(BackendState::default()));
        let session = {
            let network = RoomNetwork::new();
            AnnounceMultiplayerSession::with_backend(
                &network,
                Box::new(RecordingBackend::new(Arc::clone(&state))),
            )
        };
        let result = session.register();
        assert_eq!(result.result_code, WebResultCode::LibError);
        assert_eq!(result.result_string, "Network is not initialized");

        let network = RoomNetwork::new();
        let session = AnnounceMultiplayerSession::with_backend(
            &network,
            Box::new(RecordingBackend::new(Arc::clone(&state))),
        );
        let result = session.register();
        assert_eq!(result.result_code, WebResultCode::LibError);
        assert_eq!(result.result_string, "Room is not open");
    }

    #[test]
    fn register_populates_backend_and_sets_verification_id() {
        let network = RoomNetwork::new();
        let room = open_room(&network);
        let state = Arc::new(Mutex::new(BackendState::default()));
        let session = AnnounceMultiplayerSession::with_backend(
            &network,
            Box::new(RecordingBackend::new(Arc::clone(&state))),
        );

        let result = session.register();
        assert_eq!(result.result_code, WebResultCode::Success);
        assert!(result.returned_data.is_empty());
        assert_eq!(room.get_verify_uid(), "verification-id");
        let state = state.lock();
        assert_eq!(state.name, "Free Room");
        assert_eq!(state.description, "Free homebrew multiplayer");
        assert_eq!(state.port, 24872);
        assert_eq!(state.member_slots, 4);
        assert_eq!(state.network_version, NETWORK_VERSION);
        assert!(state.has_password);
        assert_eq!(state.preferred_game, "OpenArenaNX");
        assert_eq!(state.clear_calls, 1);
        assert_eq!(state.register_calls, 1);
    }

    #[test]
    fn start_updates_immediately_and_stop_deletes_registration() {
        let network = RoomNetwork::new();
        let _room = open_room(&network);
        let state = Arc::new(Mutex::new(BackendState::default()));
        let (sender, receiver) = mpsc::channel();
        let mut backend = RecordingBackend::new(Arc::clone(&state));
        backend.update_signal = Some(sender);
        let session = AnnounceMultiplayerSession::with_backend(&network, Box::new(backend));

        assert!(!session.is_running());
        session.start();
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("the first Eden announce update is immediate");
        assert!(session.is_running());
        session.stop();
        assert!(!session.is_running());

        let state = state.lock();
        assert_eq!(state.register_calls, 1);
        assert_eq!(state.update_calls, 1);
        assert_eq!(state.delete_calls, 1);
    }

    #[test]
    fn update_404_registers_the_room_again() {
        let network = RoomNetwork::new();
        let _room = open_room(&network);
        let state = Arc::new(Mutex::new(BackendState::default()));
        let (sender, receiver) = mpsc::channel();
        let mut backend = RecordingBackend::new(Arc::clone(&state));
        backend.register_signal = Some(sender);
        backend.update_result = WebResult {
            result_code: WebResultCode::HttpError,
            result_string: "404".to_string(),
            returned_data: String::new(),
        };
        let session = AnnounceMultiplayerSession::with_backend(&network, Box::new(backend));

        session.start();
        assert_eq!(receiver.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        assert_eq!(receiver.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        session.stop();

        assert_eq!(state.lock().register_calls, 2);
    }
}

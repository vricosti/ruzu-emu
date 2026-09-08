// SPDX-License-Identifier: GPL-3.0-or-later
//! Opt-in local GUI test control. No host keyboard capture or OS event injection.
//! This diagnostic frontend facility has no Eden counterpart.

use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub(crate) enum Command {
    Buttons { ids: Vec<i32>, pressed: bool },
    Capture(PathBuf),
    Status,
}

struct Control {
    socket: UnixDatagram,
    directory: PathBuf,
    held: Option<(Vec<i32>, Instant)>,
    handler: Box<dyn FnMut(Command) -> Result<Value, String>>,
}

/// Bind only on explicit request, inside a newly created owner-only directory.
/// A datagram cannot block GTK waiting for the rest of a partial request.
pub(crate) fn start(handler: impl FnMut(Command) -> Result<Value, String> + 'static) {
    let Some(directory) = std::env::var_os("RUZU_INPUT_SESSION_DIR") else {
        return;
    };
    let directory = PathBuf::from(directory);
    let setup = || -> io::Result<UnixDatagram> {
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let socket = UnixDatagram::bind(directory.join("control.sock"))?;
        socket.set_nonblocking(true)?;
        Ok(socket)
    };
    let socket = match setup() {
        Ok(socket) => socket,
        Err(error) => {
            log::error!(
                "Cannot start input session at {}: {error}",
                directory.display()
            );
            return;
        }
    };
    log::info!(
        "Input session ready: {}",
        directory.join("control.sock").display()
    );
    let mut control = Control {
        socket,
        directory,
        held: None,
        handler: Box::new(handler),
    };
    gtk::glib::timeout_add_local(Duration::from_millis(10), move || {
        control.poll();
        gtk::glib::ControlFlow::Continue
    });
}

impl Control {
    fn poll(&mut self) {
        if self
            .held
            .as_ref()
            .is_some_and(|(_, deadline)| Instant::now() >= *deadline)
        {
            self.release();
        }
        // Bound work per main-loop iteration, including malformed/flooded input.
        for _ in 0..8 {
            let mut bytes = [0u8; 4097];
            let (len, peer) = match self.socket.recv_from(&mut bytes) {
                Ok(message) => message,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    log::error!("Input session receive: {error}");
                    break;
                }
            };
            let result = if len > 4096 {
                Err("request exceeds 4096 bytes".into())
            } else {
                serde_json::from_slice(&bytes[..len])
                    .map_err(|error| error.to_string())
                    .and_then(|request| self.execute(request))
            };
            let reply = match result {
                Ok(value) => json!({"ok": true, "result": value}),
                Err(error) => json!({"ok": false, "error": error}),
            };
            if let Some(path) = peer.as_pathname() {
                if let Err(error) = self.socket.send_to(reply.to_string().as_bytes(), path) {
                    log::warn!("Input session reply to {}: {error}", path.display());
                }
            } else {
                log::warn!("Input session request has no reply pathname: {peer:?}");
            }
        }
    }

    fn execute(&mut self, request: Value) -> Result<Value, String> {
        match request.get("command").and_then(Value::as_str) {
            Some("status") => (self.handler)(Command::Status),
            Some("release") => {
                self.release();
                Ok(json!({"released": true}))
            }
            Some("press") => {
                if self.held.is_some() {
                    return Err("previous press is still active".into());
                }
                let (ids, duration) = parse_press(&request)?;
                let result = (self.handler)(Command::Buttons {
                    ids: ids.clone(),
                    pressed: true,
                })?;
                self.held = Some((ids, Instant::now() + duration));
                Ok(result)
            }
            Some("capture") => {
                let name = request
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("missing capture name")?;
                if !valid_capture_name(name) {
                    return Err("capture name must be a simple .png filename".into());
                }
                let path = self.directory.join(name);
                if path.exists() {
                    return Err("capture already exists".into());
                }
                (self.handler)(Command::Capture(path))
            }
            _ => Err("expected status, press, release or capture".into()),
        }
    }

    fn release(&mut self) {
        if let Some((ids, _)) = self.held.take() {
            let _ = (self.handler)(Command::Buttons {
                ids,
                pressed: false,
            });
        }
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        self.release();
        let _ = std::fs::remove_file(self.directory.join("control.sock"));
    }
}

fn parse_press(request: &Value) -> Result<(Vec<i32>, Duration), String> {
    let buttons = request
        .get("buttons")
        .and_then(Value::as_array)
        .ok_or("missing buttons array")?;
    let duration = request
        .get("hold_ms")
        .and_then(Value::as_u64)
        .ok_or("missing hold_ms")?;
    if buttons.is_empty() || buttons.len() > 18 || !(1..=2000).contains(&duration) {
        return Err("use 1..18 buttons and hold_ms in 1..2000".into());
    }
    // VirtualGamepad::VirtualButton, not keyboard scancodes or TAS bit positions.
    use input_common::drivers::virtual_gamepad::VirtualButton as Button;
    const NAMES: [(&str, Button); 18] = [
        ("A", Button::ButtonA),
        ("B", Button::ButtonB),
        ("X", Button::ButtonX),
        ("Y", Button::ButtonY),
        ("LSTICK", Button::StickL),
        ("RSTICK", Button::StickR),
        ("L", Button::TriggerL),
        ("R", Button::TriggerR),
        ("ZL", Button::TriggerZL),
        ("ZR", Button::TriggerZR),
        ("PLUS", Button::ButtonPlus),
        ("MINUS", Button::ButtonMinus),
        ("LEFT", Button::ButtonLeft),
        ("UP", Button::ButtonUp),
        ("RIGHT", Button::ButtonRight),
        ("DOWN", Button::ButtonDown),
        ("SL", Button::ButtonSL),
        ("SR", Button::ButtonSR),
    ];
    let mut ids = Vec::new();
    for button in buttons {
        let name = button.as_str().ok_or("button must be a string")?;
        let id = NAMES
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, button)| *button as i32)
            .ok_or("unknown button")?;
        if ids.contains(&id) {
            return Err("duplicate button".into());
        }
        ids.push(id);
    }
    Ok((ids, Duration::from_millis(duration)))
}

fn valid_capture_name(name: &str) -> bool {
    name.ends_with(".png")
        && name.len() > 4
        && name.len() <= 100
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        && !name.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datagram_requests_receive_replies() {
        let directory = tempfile::tempdir().unwrap();
        let server_path = directory.path().join("control.sock");
        let socket = UnixDatagram::bind(&server_path).unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut control = Control {
            socket,
            directory: directory.path().into(),
            held: None,
            handler: Box::new(|_| Ok(json!({"received": true}))),
        };
        let client = UnixDatagram::bind(directory.path().join("reply.sock")).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        client.send_to(br#"{"command":"status"}"#, &server_path).unwrap();
        control.poll();
        let mut response = [0u8; 1024];
        let len = client.recv(&mut response).unwrap();
        let response: Value = serde_json::from_slice(&response[..len]).unwrap();
        assert_eq!(response, json!({"ok": true, "result": {"received": true}}));
    }

    #[test]
    fn logical_presses_are_bounded_and_do_not_use_keyboard_mapping() {
        assert_eq!(
            parse_press(&json!({"buttons":["A"],"hold_ms":300})).unwrap(),
            (vec![0], Duration::from_millis(300))
        );
        assert_eq!(
            parse_press(&json!({"buttons":["L","R"],"hold_ms":500}))
                .unwrap()
                .0,
            vec![6, 7]
        );
        for request in [
            json!({"buttons":[],"hold_ms":300}),
            json!({"buttons":["A"],"hold_ms":0}),
            json!({"buttons":["A"],"hold_ms":2001}),
            json!({"buttons":["A","A"],"hold_ms":3}),
            json!({"buttons":["HOME"],"hold_ms":3}),
            json!({"buttons":[2],"hold_ms":3}),
        ] {
            assert!(parse_press(&request).is_err());
        }
    }

    #[test]
    fn captures_stay_in_the_private_session_directory() {
        assert!(valid_capture_name("after-A-03.png"));
        for name in [
            "../escape.png",
            "/tmp/escape.png",
            "a/b.png",
            ".png",
            ".hidden.png",
            "a.jpg",
            "a\\b.png",
        ] {
            assert!(!valid_capture_name(name));
        }
    }

    #[test]
    fn release_on_cancel_and_expiration_and_drop() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let directory = tempfile::tempdir().unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let output = events.clone();
        let mut control = Control {
            socket: UnixDatagram::unbound().unwrap(),
            directory: directory.path().into(),
            held: None,
            handler: Box::new(move |event| {
                if let Command::Buttons { ids, pressed } = event {
                    output.borrow_mut().push((ids, pressed));
                }
                Ok(Value::Null)
            }),
        };
        control.socket.set_nonblocking(true).unwrap();
        let press = json!({"command":"press","buttons":["A"],"hold_ms":100});
        control.execute(press.clone()).unwrap();
        assert!(control.execute(press.clone()).is_err());
        control.execute(json!({"command":"release"})).unwrap();
        control.execute(press.clone()).unwrap();
        control.held.as_mut().unwrap().1 = Instant::now();
        control.poll();
        control.execute(press).unwrap();
        drop(control);
        assert_eq!(
            &*events.borrow(),
            &[
                (vec![0], true),
                (vec![0], false),
                (vec![0], true),
                (vec![0], false),
                (vec![0], true),
                (vec![0], false)
            ]
        );
    }
}

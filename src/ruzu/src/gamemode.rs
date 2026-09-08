// SPDX-License-Identifier: GPL-3.0-or-later
//! Frontend counterpart of Eden qt_common/gamemode.{h,cpp}.
//! The optional client is loaded dynamically, as in gamemode_client.h.

pub fn start() {
    if crate::uisettings::with(|values| *values.enable_gamemode.get_value()) {
        #[cfg(unix)]
        request(true);
    }
}

pub fn stop() {
    // Release a successful request even if Reset changed the setting meanwhile.
    #[cfg(unix)]
    request(false);
}

#[cfg(unix)]
type Request = unsafe extern "C" fn() -> libc::c_int;
#[cfg(unix)]
type ErrorString = unsafe extern "C" fn() -> *const libc::c_char;

#[cfg(unix)]
struct Client {
    // Owns the lifetime of all three function pointers.
    _library: common::dynamic_library::DynamicLibrary,
    start: Request,
    end: Request,
    error: ErrorString,
    active: bool,
}

#[cfg(unix)]
impl Client {
    fn load() -> Result<Self, String> {
        use common::dynamic_library::DynamicLibrary;
        let mut library = DynamicLibrary::from_filename("libgamemode.so.0");
        if !library.is_open() && !library.open("libgamemode.so") {
            return Err("GameMode client library is unavailable".to_owned());
        }
        // gamemode_client.h loads the real_* ABI, not its inline wrappers.
        // SAFETY: these signatures match the client's documented C ABI and
        // the library handle remains owned for their entire lifetime.
        unsafe {
            let start = library
                .get_symbol::<Request>("real_gamemode_request_start")
                .ok_or("GameMode start symbol is missing")?;
            let end = library
                .get_symbol::<Request>("real_gamemode_request_end")
                .ok_or("GameMode end symbol is missing")?;
            let error = library
                .get_symbol::<ErrorString>("real_gamemode_error_string")
                .ok_or("GameMode error symbol is missing")?;
            Ok(Self {
                _library: library,
                start,
                end,
                error,
                active: false,
            })
        }
    }

    fn request(&mut self, active: bool) -> Result<(), String> {
        if self.active == active {
            return Ok(());
        }
        // SAFETY: load validated both functions and retains their library.
        if unsafe {
            if active {
                (self.start)()
            } else {
                (self.end)()
            }
        } < 0
        {
            let pointer = unsafe { (self.error)() };
            return Err(if pointer.is_null() {
                "GameMode request failed".to_owned()
            } else {
                unsafe { std::ffi::CStr::from_ptr(pointer) }
                    .to_string_lossy()
                    .into_owned()
            });
        }
        self.active = active;
        log::info!(
            "GameMode request {}",
            if active { "started" } else { "ended" }
        );
        Ok(())
    }
}

#[cfg(unix)]
fn request(active: bool) {
    use std::sync::{Mutex, OnceLock};
    static CLIENT: OnceLock<Mutex<Result<Client, String>>> = OnceLock::new();
    // Stop before any successful start should neither load nor warn.
    if !active && CLIENT.get().is_none() {
        return;
    }
    let mut client = CLIENT
        .get_or_init(|| Mutex::new(Client::load()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let result = match client.as_mut() {
        Ok(client) => client.request(active),
        Err(error) => Err(error.clone()),
    };
    if let Err(error) = result {
        #[cfg(target_os = "linux")]
        log::warn!("{error}");
        #[cfg(not(target_os = "linux"))]
        log::info!("{error}");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static STARTS: AtomicUsize = AtomicUsize::new(0);
    static ENDS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn start() -> libc::c_int {
        STARTS.fetch_add(1, Ordering::SeqCst);
        0
    }
    unsafe extern "C" fn end() -> libc::c_int {
        ENDS.fetch_add(1, Ordering::SeqCst);
        0
    }
    unsafe extern "C" fn error() -> *const libc::c_char {
        std::ptr::null()
    }
    unsafe extern "C" fn fail() -> libc::c_int {
        -1
    }

    #[test]
    fn failed_requests_do_not_claim_a_state_change() {
        let mut client = Client {
            _library: common::dynamic_library::DynamicLibrary::new(),
            start: fail,
            end: fail,
            error,
            active: false,
        };
        assert!(client.request(true).is_err());
        assert!(!client.active);
        client.active = true;
        assert!(client.request(false).is_err());
        assert!(client.active);
    }

    #[test]
    fn requests_balance_across_pause_resume_and_duplicate_stop() {
        let mut client = Client {
            _library: common::dynamic_library::DynamicLibrary::new(),
            start,
            end,
            error,
            active: false,
        };
        client.request(false).unwrap();
        client.request(true).unwrap();
        client.request(true).unwrap();
        client.request(false).unwrap();
        client.request(true).unwrap();
        client.request(false).unwrap();
        client.request(false).unwrap();
        assert_eq!(STARTS.load(Ordering::SeqCst), 2);
        assert_eq!(ENDS.load(Ordering::SeqCst), 2);
        assert!(!client.active);
    }
}

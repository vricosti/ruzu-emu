//! Port of zuyu/src/common/logging/backend.h and zuyu/src/common/logging/backend.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-05
//!
//! The C++ version uses a singleton Impl with a background thread draining an MPSC queue.
//! This Rust port uses a similar architecture with a background thread, atomic flags, and
//! crossbeam-style channel (std::sync::mpsc).

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Instant;

use super::filter::Filter;
use super::log_entry::Entry;
use super::text_formatter::{format_log_message, print_colored_message};
use super::types::{Class, Level};

/// Trait for logging backends (matching the C++ Backend interface).
trait Backend: Send {
    fn write(&mut self, entry: &Entry);
    fn flush(&mut self);
}

/// Backend that writes to a file.
struct FileBackend {
    file: Option<fs::File>,
    enabled: bool,
    bytes_written: usize,
}

impl FileBackend {
    fn new(filename: &std::path::Path) -> Self {
        // Rotate old log file
        let old_filename = filename.with_extension("old.txt");
        let _ = fs::remove_file(&old_filename);
        let _ = fs::rename(filename, &old_filename);

        let file = fs::File::create(filename).ok();

        Self {
            file,
            enabled: true,
            bytes_written: 0,
        }
    }
}

impl Backend for FileBackend {
    fn write(&mut self, entry: &Entry) {
        if !self.enabled {
            return;
        }

        if let Some(ref mut file) = self.file {
            let msg = format!("{}\n", format_log_message(entry));
            let bytes = msg.as_bytes();
            if file.write_all(bytes).is_ok() {
                self.bytes_written += bytes.len();
            }

            // Prevent logs from exceeding 100 MiB
            const WRITE_LIMIT: usize = 100 * 1024 * 1024;
            let write_limit_exceeded = self.bytes_written > WRITE_LIMIT;

            if entry.log_level >= Level::Error || write_limit_exceeded {
                if write_limit_exceeded {
                    self.enabled = false;
                }
                let _ = file.flush();
            }
        }
    }

    fn flush(&mut self) {
        if let Some(ref mut file) = self.file {
            let _ = file.flush();
        }
    }
}

static LOGGER_INITIALIZED: AtomicBool = AtomicBool::new(false);
static SUPPRESS_LOGGING: AtomicBool = AtomicBool::new(true);

/// Lock-free snapshot of the active filter for `log::Log::enabled`.
///
/// `log::set_max_level(Trace)` routes every `trace!`/`debug!` macro in the
/// codebase through `enabled()`; taking the global `LOGGER` mutex there
/// convoys hot paths (the kernel scheduler lock showed up in profiles).
/// The per-class minimum levels change only in `initialize` and
/// `set_global_filter`, so they are published to atomics and read without
/// locking. `FILTER_GLOBAL_MIN` is the minimum across classes: a single load
/// rejects the common case before the class lookup.
const FILTER_LEVEL_INIT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);
static FILTER_LEVELS: [std::sync::atomic::AtomicU8; crate::logging::types::Class::COUNT] =
    [FILTER_LEVEL_INIT; crate::logging::types::Class::COUNT];
static FILTER_GLOBAL_MIN: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

fn publish_filter_snapshot(filter: &Filter) {
    let mut min = u8::MAX;
    for (slot, &level) in FILTER_LEVELS.iter().zip(filter.class_levels().iter()) {
        slot.store(level as u8, Ordering::Relaxed);
        min = min.min(level as u8);
    }
    FILTER_GLOBAL_MIN.store(min, Ordering::Release);
}

// We use a Mutex for the singleton since we need mutable access.
static LOGGER: Mutex<Option<LoggerState>> = Mutex::new(None);

struct LoggerState {
    filter: Filter,
    sender: mpsc::Sender<Entry>,
    time_origin: Instant,
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

/// Initializes the logging system. This should be called early in main.
pub fn initialize(log_dir: Option<PathBuf>, filter_string: Option<&str>) {
    let mut guard = LOGGER.lock().unwrap();
    if guard.is_some() {
        eprintln!("Reinitializing logging backend");
        return;
    }

    let log_dir = log_dir.unwrap_or_else(|| {
        let mut dir = dirs_or_default();
        dir.push("log");
        dir
    });

    let _ = fs::create_dir_all(&log_dir);

    let mut filter = Filter::default();
    if let Some(filter_str) = filter_string {
        filter.parse_filter_string(filter_str);
    }

    let log_file = log_dir.join("ruzu_log.txt");
    let (sender, receiver) = mpsc::channel::<Entry>();

    let time_origin = Instant::now();

    // Create backends for the background thread
    let mut file_backend = FileBackend::new(&log_file);

    // We'll read the color_enabled flag from shared state
    let color_flag = std::sync::Arc::new(AtomicBool::new(false));
    let color_flag_thread = color_flag.clone();

    let thread_handle = std::thread::Builder::new()
        .name("Logger".to_string())
        .spawn(move || {
            while let Ok(entry) = receiver.recv() {
                // Write to file backend
                file_backend.write(&entry);

                // Write to color console if enabled
                if color_flag_thread.load(Ordering::Relaxed) {
                    print_colored_message(&entry);
                }
            }

            // Drain remaining messages
            // (channel is closed, recv will return Err after sender is dropped)
            file_backend.flush();
        })
        .expect("Failed to spawn logger thread");

    publish_filter_snapshot(&filter);
    *guard = Some(LoggerState {
        filter,
        sender,
        time_origin,
        thread_handle: Some(thread_handle),
    });

    // Store the color_flag Arc somewhere accessible... For simplicity, we use the
    // AtomicBool in LoggerState and the thread checks it via the Arc.
    // Actually, let's just store the Arc in a separate static.
    {
        let mut cflag = COLOR_FLAG.lock().unwrap();
        *cflag = Some(color_flag);
    }

    SUPPRESS_LOGGING.store(false, Ordering::SeqCst);
    LOGGER_INITIALIZED.store(true, Ordering::SeqCst);
}

static COLOR_FLAG: Mutex<Option<std::sync::Arc<AtomicBool>>> = Mutex::new(None);

struct FacadeLogger;

static FACADE_LOGGER: FacadeLogger = FacadeLogger;

impl log::Log for FacadeLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        if SUPPRESS_LOGGING.load(Ordering::Relaxed) {
            return false;
        }

        // Lock-free fast path: reject on the global minimum level before the
        // per-class lookup, and never touch the LOGGER mutex here.
        let level = level_from_log(metadata.level()) as u8;
        if level < FILTER_GLOBAL_MIN.load(Ordering::Acquire) {
            return false;
        }
        let class = class_from_target(metadata.target());
        level >= FILTER_LEVELS[class as usize].load(Ordering::Relaxed)
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        push_entry(
            class_from_target(record.target()),
            level_from_log(record.level()),
            record.file().unwrap_or("<unknown>"),
            record.line().unwrap_or(0),
            record.module_path().unwrap_or(record.target()),
            record.args().to_string(),
        );
    }

    fn flush(&self) {}
}

/// Initialize the yuzu-style asynchronous backend and connect Rust's `log`
/// facade to it. `RUZU_LOG_FILTER` uses the native class syntax
/// (`*:Info Render.OpenGL:Debug`). A simple `RUST_LOG=info` style value is
/// also accepted for command-line compatibility.
pub fn initialize_from_env(log_dir: Option<PathBuf>) {
    let filter = std::env::var("RUZU_LOG_FILTER")
        .ok()
        .or_else(|| {
            std::env::var("RUST_LOG")
                .ok()
                .and_then(|s| rust_log_to_filter(&s))
        })
        .unwrap_or_else(|| "*:Warning".to_string());

    initialize(log_dir, Some(&filter));
    set_color_console_backend_enabled(env_flag_enabled("RUZU_LOG_COLOR", true));

    match log::set_logger(&FACADE_LOGGER) {
        Ok(()) => log::set_max_level(log::LevelFilter::Trace),
        Err(_) => eprintln!("logging facade was already initialized"),
    }
}

fn rust_log_to_filter(value: &str) -> Option<String> {
    let first = value.split(',').next()?.trim();
    let level = first.rsplit('=').next()?.trim();
    match level.to_ascii_lowercase().as_str() {
        "trace" => Some("*:Trace".to_string()),
        "debug" => Some("*:Debug".to_string()),
        "info" => Some("*:Info".to_string()),
        "warn" | "warning" => Some("*:Warning".to_string()),
        "error" => Some("*:Error".to_string()),
        "off" => Some("*:Critical".to_string()),
        _ => None,
    }
}

fn env_flag_enabled(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|value| {
            !matches!(
                value.as_str(),
                "0" | "false" | "False" | "FALSE" | "off" | "OFF"
            )
        })
        .unwrap_or(default)
}

fn level_from_log(level: log::Level) -> Level {
    match level {
        log::Level::Error => Level::Error,
        log::Level::Warn => Level::Warning,
        log::Level::Info => Level::Info,
        log::Level::Debug => Level::Debug,
        log::Level::Trace => Level::Trace,
    }
}

fn class_from_target(target: &str) -> Class {
    let normalized = target.replace("::", ".").replace('_', ".");
    Class::from_name(target)
        .or_else(|| Class::from_name(&normalized))
        .or_else(|| {
            Class::all()
                .filter(|class| {
                    normalized
                        .strip_prefix(class.name())
                        .is_some_and(|suffix| suffix.starts_with('.'))
                })
                .max_by_key(|class| class.name().len())
        })
        .unwrap_or(Class::Log)
}

/// Stops the logger thread and flushes buffers.
pub fn stop() {
    let mut guard = LOGGER.lock().unwrap();
    if let Some(mut state) = guard.take() {
        // Drop the sender to signal the thread to finish
        drop(state.sender);
        if let Some(handle) = state.thread_handle.take() {
            let _ = handle.join();
        }
    }
    LOGGER_INITIALIZED.store(false, Ordering::SeqCst);
    SUPPRESS_LOGGING.store(true, Ordering::SeqCst);
}

/// Disables logging (used in tests).
pub fn disable_logging_in_tests() {
    SUPPRESS_LOGGING.store(true, Ordering::SeqCst);
}

/// Sets the global filter.
pub fn set_global_filter(filter: &Filter) {
    let mut guard = LOGGER.lock().unwrap();
    // Serialize the facade snapshot with the queued logger's filter. Publishing
    // before this lock lets two concurrent updates leave different filters in
    // enabled() and push_entry(), even after both updates have returned.
    publish_filter_snapshot(filter);
    if let Some(ref mut state) = *guard {
        state.filter = filter.clone();
    }
}

/// Enables or disables the color console backend.
pub fn set_color_console_backend_enabled(enabled: bool) {
    if let Ok(cflag) = COLOR_FLAG.lock() {
        if let Some(ref flag) = *cflag {
            flag.store(enabled, Ordering::Relaxed);
        }
    }
}

/// Pushes a log entry into the logging system. Called by the `log_message!` macro.
pub fn push_entry(
    log_class: Class,
    log_level: Level,
    filename: &str,
    line_num: u32,
    function: &str,
    message: String,
) {
    if SUPPRESS_LOGGING.load(Ordering::Relaxed) {
        return;
    }

    let guard = LOGGER.lock().unwrap();
    if let Some(ref state) = *guard {
        if !state.filter.check_message(log_class, log_level) {
            return;
        }

        let entry = Entry {
            timestamp: state.time_origin.elapsed(),
            log_class,
            log_level,
            filename: filename.to_string(),
            line_num,
            function: function.to_string(),
            message,
        };

        let _ = state.sender.send(entry);
    }
}

fn dirs_or_default() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".local");
        p.push("share");
        p.push("ruzu");
        p
    } else {
        PathBuf::from(".")
    }
}

/// Convenience macro matching the C++ LOG_INFO, LOG_WARNING, etc. patterns.
///
/// Usage: `log_message!(Class::Kernel_SVC, Level::Info, "SVC called: {}", svc_name);`
#[macro_export]
macro_rules! log_message {
    ($class:expr, $level:expr, $($arg:tt)*) => {{
        $crate::logging::backend::push_entry(
            $class,
            $level,
            file!(),
            line!(),
            // Rust doesn't have __func__, we use module_path! as approximation
            module_path!(),
            format!($($arg)*),
        );
    }};
}

/// LOG_TRACE - only active in debug builds (matching the C++ behavior).
#[macro_export]
macro_rules! log_trace {
    ($class:expr, $($arg:tt)*) => {{
        #[cfg(debug_assertions)]
        $crate::log_message!($class, $crate::logging::types::Level::Trace, $($arg)*);
    }};
}

#[macro_export]
macro_rules! log_debug {
    ($class:expr, $($arg:tt)*) => {
        $crate::log_message!($class, $crate::logging::types::Level::Debug, $($arg)*)
    };
}

#[macro_export]
macro_rules! log_info {
    ($class:expr, $($arg:tt)*) => {
        $crate::log_message!($class, $crate::logging::types::Level::Info, $($arg)*)
    };
}

#[macro_export]
macro_rules! log_warning {
    ($class:expr, $($arg:tt)*) => {
        $crate::log_message!($class, $crate::logging::types::Level::Warning, $($arg)*)
    };
}

#[macro_export]
macro_rules! log_error {
    ($class:expr, $($arg:tt)*) => {
        $crate::log_message!($class, $crate::logging::types::Level::Error, $($arg)*)
    };
}

#[macro_export]
macro_rules! log_critical {
    ($class:expr, $($arg:tt)*) => {
        $crate::log_message!($class, $crate::logging::types::Level::Critical, $($arg)*)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_log_level_maps_to_yuzu_filter() {
        assert_eq!(rust_log_to_filter("info").as_deref(), Some("*:Info"));
        assert_eq!(
            rust_log_to_filter("video_core=debug").as_deref(),
            Some("*:Debug")
        );
        assert_eq!(rust_log_to_filter("off").as_deref(), Some("*:Critical"));
    }

    #[test]
    fn log_target_maps_to_known_class() {
        assert_eq!(class_from_target("Render.OpenGL"), Class::Render_OpenGL);
        assert_eq!(class_from_target("Render_OpenGL"), Class::Render_OpenGL);
        assert_eq!(
            class_from_target("Render.OpenGL.texture_cache"),
            Class::Render_OpenGL
        );
        assert_eq!(class_from_target("unknown_target"), Class::Log);
    }

    #[test]
    fn class_prefix_requires_a_component_boundary() {
        assert_eq!(class_from_target("Render.OpenGLExtra"), Class::Render);
        assert_eq!(class_from_target("Renderer"), Class::Log);
        assert_eq!(class_from_target("Service.FSExtra"), Class::Service);
        assert_eq!(class_from_target("Service.FS.operation"), Class::Service_FS);
        assert_eq!(class_from_target("Service_FS::operation"), Class::Service_FS);
        for class in Class::all() {
            assert_eq!(class_from_target(class.name()), class);
            assert_eq!(class_from_target(&format!("{}.message", class.name())), class);
        }
    }

    #[test]
    fn concurrent_filter_updates_publish_one_consistent_filter() {
        const CHILD: &str = "RUZU_TEST_CONCURRENT_LOG_FILTER";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "logging::backend::tests::concurrent_filter_updates_publish_one_consistent_filter"])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        // Isolate global logger state without starting file/console backends.
        let (sender, _receiver) = mpsc::channel();
        *LOGGER.lock().unwrap() = Some(LoggerState {
            filter: Filter::default(), sender, time_origin: Instant::now(), thread_handle: None,
        });
        for _ in 0..50 {
            std::thread::scope(|scope| {
                for level in [Level::Debug, Level::Critical] {
                    scope.spawn(move || {
                        for _ in 0..100 { set_global_filter(&Filter::new(level)); }
                    });
                }
            });
            let guard = LOGGER.lock().unwrap();
            let filter = &guard.as_ref().unwrap().filter;
            for (slot, level) in FILTER_LEVELS.iter().zip(filter.class_levels()) {
                assert_eq!(slot.load(Ordering::Relaxed), *level as u8);
            }
            assert_eq!(FILTER_GLOBAL_MIN.load(Ordering::Acquire),
                *filter.class_levels().iter().min().unwrap() as u8);
        }
    }

    #[test]
    fn env_flag_default_and_false_values() {
        std::env::remove_var("RUZU_TEST_LOG_FLAG");
        assert!(env_flag_enabled("RUZU_TEST_LOG_FLAG", true));
        assert!(!env_flag_enabled("RUZU_TEST_LOG_FLAG", false));

        std::env::set_var("RUZU_TEST_LOG_FLAG", "0");
        assert!(!env_flag_enabled("RUZU_TEST_LOG_FLAG", true));
        std::env::set_var("RUZU_TEST_LOG_FLAG", "false");
        assert!(!env_flag_enabled("RUZU_TEST_LOG_FLAG", true));
        std::env::set_var("RUZU_TEST_LOG_FLAG", "1");
        assert!(env_flag_enabled("RUZU_TEST_LOG_FLAG", false));
        std::env::remove_var("RUZU_TEST_LOG_FLAG");
    }
}

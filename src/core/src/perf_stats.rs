//! Counterpart of Eden's core/perf_stats.{h,cpp}.
//!
//! Performance statistics tracker (FPS, frame times, emulation speed).

use parking_lot::Mutex;
use std::io::{self, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Purposefully ignore the first five frames, as there's a significant amount of overhead in
/// booting that we shouldn't account for.
const IGNORE_FRAMES: usize = 5;

/// Number of frametime history entries (one hour at 60fps).
const PERF_HISTORY_SIZE: usize = 216_000;

/// Performance statistics results.
#[derive(Debug, Clone, Copy, Default)]
pub struct PerfStatsResults {
    /// System FPS (LCD VBlanks) in Hz
    pub system_fps: f64,
    /// Average game FPS (GPU frame renders) in Hz
    pub average_game_fps: f64,
    /// Walltime per system frame, in seconds, excluding any waits
    pub frametime: f64,
    /// Ratio of walltime / emulated time elapsed
    pub emulation_speed: f64,
}

/// Class to manage and query performance/timing statistics.
/// All public functions of this class are thread-safe.
pub struct PerfStats {
    inner: Mutex<PerfStatsInner>,
    /// Cumulative number of game frames (GPU frame submissions) since last reset.
    /// Atomic for cross-thread access without lock.
    game_frames: AtomicU32,
    /// Title ID for the game that is running. 0 if there is no game running yet.
    title_id: u64,
}

struct PerfStatsInner {
    /// Current index for writing to the perf_history array
    current_index: usize,
    /// Stores historical frametime data (in milliseconds) useful for processing
    /// and tracking performance regressions with code changes.
    perf_history: Box<[f64; PERF_HISTORY_SIZE]>,

    /// Point when the cumulative counters were reset
    reset_point: Instant,
    /// System time when the cumulative counters were reset
    reset_point_system_us: Duration,

    /// Cumulative duration (excluding v-sync/frame-limiting) of frames since last reset
    accumulated_frametime: Duration,
    /// Cumulative number of system frames (LCD VBlanks) presented since last reset
    system_frames: u32,

    /// Point when the previous system frame ended
    previous_frame_end: Instant,
    /// Point when the current system frame began
    frame_begin: Instant,
    /// Total visible duration (including frame-limiting, etc.) of the previous system frame
    previous_frame_length: Duration,
    /// Previously computed fps
    previous_fps: f64,
}

impl PerfStats {
    pub fn new(title_id: u64) -> Self {
        let now = Instant::now();
        Self {
            inner: Mutex::new(PerfStatsInner {
                current_index: 0,
                // Avoid materializing the 1.7 MiB history on the Rust stack.
                perf_history: vec![0.0; PERF_HISTORY_SIZE]
                    .into_boxed_slice()
                    .try_into()
                    .unwrap(),
                reset_point: now,
                reset_point_system_us: Duration::ZERO,
                accumulated_frametime: Duration::ZERO,
                system_frames: 0,
                previous_frame_end: now,
                frame_begin: now,
                previous_frame_length: Duration::ZERO,
                previous_fps: 0.0,
            }),
            game_frames: AtomicU32::new(0),
            title_id,
        }
    }

    /// Marks the beginning of a system frame.
    pub fn begin_system_frame(&self) {
        let mut inner = self.inner.lock();
        inner.frame_begin = Instant::now();
    }

    /// Marks the end of a system frame and records timing data.
    pub fn end_system_frame(&self) {
        let mut inner = self.inner.lock();
        let frame_end = Instant::now();
        let frame_time = frame_end - inner.frame_begin;

        let idx = inner.current_index;
        if idx < PERF_HISTORY_SIZE {
            inner.perf_history[idx] = frame_time.as_secs_f64() * 1000.0;
            inner.current_index += 1;
        }

        inner.accumulated_frametime += frame_time;
        inner.system_frames += 1;

        inner.previous_frame_length = frame_end - inner.previous_frame_end;
        inner.previous_frame_end = frame_end;
    }

    /// Marks the end of a game frame (GPU frame submission).
    pub fn end_game_frame(&self) {
        self.game_frames.fetch_add(1, Ordering::Relaxed);
    }

    /// Returns the arithmetic mean of all frametime values stored in the performance history.
    pub fn get_mean_frametime(&self) -> f64 {
        let inner = self.inner.lock();
        if inner.current_index <= IGNORE_FRAMES {
            return 0.0;
        }
        let sum: f64 = inner.perf_history[IGNORE_FRAMES..inner.current_index]
            .iter()
            .sum();
        sum / (inner.current_index - IGNORE_FRAMES) as f64
    }

    /// Gets and resets core performance statistics.
    pub fn get_and_reset_stats(&self, current_system_time_us: Duration) -> PerfStatsResults {
        let mut inner = self.inner.lock();
        let now = Instant::now();

        // Walltime elapsed since stats were reset
        let interval = (now - inner.reset_point).as_secs_f64();
        if interval == 0.0 {
            return PerfStatsResults::default();
        }

        let system_us_per_second =
            (current_system_time_us - inner.reset_point_system_us).as_micros() as f64 / interval;
        let current_frames = self.game_frames.load(Ordering::Relaxed) as f64;
        let current_fps = current_frames / interval;

        let system_fps = inner.system_frames as f64 / interval;
        let frametime = if inner.system_frames > 0 {
            inner.accumulated_frametime.as_secs_f64() / inner.system_frames as f64
        } else {
            0.0
        };

        let results = PerfStatsResults {
            system_fps,
            average_game_fps: (current_fps + inner.previous_fps) / 2.0,
            frametime,
            emulation_speed: system_us_per_second / 1_000_000.0,
        };

        // Reset counters
        inner.reset_point = now;
        inner.reset_point_system_us = current_system_time_us;
        inner.accumulated_frametime = Duration::ZERO;
        inner.system_frames = 0;
        self.game_frames.store(0, Ordering::Relaxed);
        inner.previous_fps = current_fps;

        results
    }

    /// Gets the ratio between walltime and the emulated time of the previous system frame.
    /// This is useful for scaling inputs or outputs moving between the two time domains.
    pub fn get_last_frame_time_scale(&self) -> f64 {
        let inner = self.inner.lock();
        const FRAME_LENGTH: f64 = 1.0 / 60.0;
        inner.previous_frame_length.as_secs_f64() / FRAME_LENGTH
    }
}

impl Drop for PerfStats {
    fn drop(&mut self) {
        if !common::settings::values().record_frame_times || self.title_id == 0 {
            return;
        }
        let result = (|| -> io::Result<()> {
            let timestamp = unsafe { libc::time(std::ptr::null_mut()) };
            let filename = frame_time_filename(self.title_id, timestamp)?;
            let directory =
                common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::LogDir);
            std::fs::create_dir_all(&directory)?;
            let mut file = io::BufWriter::new(std::fs::File::create(directory.join(filename))?);
            let inner = self.inner.get_mut();
            let newline = if cfg!(windows) { "\r\n" } else { "\n" };
            // Upstream intends to skip warm-up frames; a reversed iterator
            // range when fewer than five frames ran must not become Rust UB.
            for &duration in inner
                .perf_history
                .get(IGNORE_FRAMES..inner.current_index)
                .unwrap_or(&[])
            {
                write!(file, "{}{newline}", format_frame_time(duration))?;
            }
            file.flush()
        })();
        if let Err(error) = result {
            log::error!("Could not save frame time history: {error}");
        }
    }
}

// Destructor's local timestamp formatting, with thread-safe libc conversion
// instead of std::localtime's shared buffer. No guest RTC or timezone is used.
fn frame_time_filename(title_id: u64, timestamp: libc::time_t) -> io::Result<String> {
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    #[cfg(unix)]
    let valid = unsafe { !libc::localtime_r(&timestamp, &mut local).is_null() };
    #[cfg(windows)]
    let valid = unsafe { libc::localtime_s(&mut local, &timestamp) == 0 };
    #[cfg(not(any(unix, windows)))]
    let valid = false;
    if !valid {
        return Err(io::Error::other("local time conversion failed"));
    }
    Ok(format!(
        "{:04}-{:02}-{:02}-{:02}-{:02}_{title_id:016X}.csv",
        local.tm_year + 1900,
        local.tm_mon + 1,
        local.tm_mday,
        local.tm_hour,
        local.tm_min
    ))
}

// ostream_iterator<double> uses defaultfloat with six significant digits.
// Keep that CSV representation rather than Rust Display's full precision.
fn format_frame_time(value: f64) -> String {
    if !value.is_finite() {
        return value.to_string().to_ascii_lowercase();
    }
    let scientific = format!("{value:.5e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap();
    let exponent: i32 = exponent.parse().unwrap();
    if !(-4..6).contains(&exponent) {
        format!(
            "{}e{exponent:+03}",
            mantissa.trim_end_matches('0').trim_end_matches('.')
        )
    } else {
        let fixed = format!("{value:.precision$}", precision = (5 - exponent) as usize);
        if fixed.contains('.') {
            fixed.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            fixed
        }
    }
}

/// Speed limiter for single-core mode.
pub struct SpeedLimiter {
    /// Emulated system time (in microseconds) at the last limiter invocation
    previous_system_time_us: Duration,
    /// Walltime at the last limiter invocation
    previous_walltime: Instant,
    /// Accumulated difference between walltime and emulated time
    speed_limiting_delta_err: i64,
}

impl SpeedLimiter {
    pub fn new() -> Self {
        Self {
            previous_system_time_us: Duration::ZERO,
            previous_walltime: Instant::now(),
            speed_limiting_delta_err: 0,
        }
    }

    /// Performs speed limiting for single-core mode.
    /// Uses the emulated system time to determine how long to sleep.
    pub fn do_speed_limiting(
        &mut self,
        current_system_time_us: Duration,
        use_multi_core: bool,
        use_speed_limit: bool,
        speed_limit_percent: u16,
    ) {
        if use_multi_core || !use_speed_limit {
            return;
        }

        let mut now = Instant::now();
        let sleep_scale = speed_limit_percent as f64 / 100.0;

        // Max lag caused by slow frames
        let max_lag_time_us = (25_000.0 / sleep_scale) as i64; // 25ms in microseconds, scaled

        let elapsed_system_us = current_system_time_us
            .saturating_sub(self.previous_system_time_us)
            .as_micros() as f64;
        let scaled_elapsed = (elapsed_system_us / sleep_scale) as i64;
        let wall_elapsed = (now - self.previous_walltime).as_micros() as i64;

        self.speed_limiting_delta_err += scaled_elapsed;
        self.speed_limiting_delta_err -= wall_elapsed;
        self.speed_limiting_delta_err = self
            .speed_limiting_delta_err
            .clamp(-max_lag_time_us, max_lag_time_us);

        if self.speed_limiting_delta_err > 0 {
            let sleep_duration = Duration::from_micros(self.speed_limiting_delta_err as u64);
            std::thread::sleep(sleep_duration);
            let now_after_sleep = Instant::now();
            self.speed_limiting_delta_err -= (now_after_sleep - now).as_micros() as i64;
            now = now_after_sleep;
        }

        self.previous_system_time_us = current_system_time_us;
        self.previous_walltime = now;
    }
}

impl Default for SpeedLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_numbers_match_default_stream_precision() {
        for (value, expected) in [
            (0.0, "0"),
            (-0.0, "-0"),
            (16.6666666, "16.6667"),
            (0.001234567, "0.00123457"),
            (0.0001, "0.0001"),
            (0.00001234567, "1.23457e-05"),
            (999999.5, "1e+06"),
            (100000.0, "100000"),
            (12345678.0, "1.23457e+07"),
        ] {
            assert_eq!(format_frame_time(value), expected);
        }
    }

    #[test]
    fn frame_time_export_obeys_setting_and_skips_warmup() {
        const CHILD: &str = "RUZU_TEST_FRAME_TIME_EXPORT";
        if std::env::var_os(CHILD).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "perf_stats::tests::frame_time_export_obeys_setting_and_skips_warmup"
                ])
                .env(CHILD, "1")
                .env("TZ", "UTC")
                .status()
                .unwrap()
                .success());
            return;
        }
        use common::fs::path_util::{set_ruzu_path, RuzuPath};
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("ruzu-frame-times-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        set_ruzu_path(RuzuPath::LogDir, &directory);
        common::settings::values_mut().record_frame_times = false;
        drop(PerfStats::new(42)); // Synthetic identifier, not an installed title.
        common::settings::values_mut().record_frame_times = true;
        drop(PerfStats::new(0));
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
        assert_eq!(
            frame_time_filename(42, 1_709_210_096).unwrap(),
            "2024-02-29-12-34_000000000000002A.csv"
        );

        common::settings::values_mut().record_frame_times = false;
        let mut stats = PerfStats::new(42);
        let inner = stats.inner.get_mut();
        inner.current_index = 7;
        inner.perf_history[..7].copy_from_slice(&[
            1.0,
            2.0,
            3.0,
            4.0,
            5.0,
            16.6666666,
            0.001234567,
        ]);
        // Polling FPS must not clear the historical data exported on teardown.
        stats.get_and_reset_stats(Duration::from_micros(100));
        common::settings::values_mut().record_frame_times = true;
        drop(stats);
        let path = std::fs::read_dir(&directory)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with("_000000000000002A.csv"));
        let csv = std::fs::read_to_string(&path).unwrap();
        assert_eq!(csv.lines().collect::<Vec<_>>(), ["16.6667", "0.00123457"]);
        std::fs::remove_file(path).unwrap();
        drop(PerfStats::new(43)); // A short/failed boot produces no sample rows.
        let path = std::fs::read_dir(&directory)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(std::fs::read(path).unwrap().is_empty());
        common::settings::values_mut().record_frame_times = false;
        std::fs::remove_dir_all(&directory).unwrap();
    }
}

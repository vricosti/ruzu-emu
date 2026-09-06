// SPDX-FileCopyrightText: 2026 reden contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reproducible X11 screenshot and video capture harness.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
mod linux_input;
mod renderdoc;
mod session;

static CANCELLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn cancelled() -> bool {
    CANCELLED.load(std::sync::atomic::Ordering::Acquire)
}

#[derive(Parser)]
#[command(about = "Launch a process and capture reproducible X11 reference images/video")]
struct Cli {
    /// TOML capture configuration.
    config: Option<PathBuf>,

    /// List Linux input devices without recording or injecting anything.
    #[arg(long, conflicts_with = "config")]
    list_input_devices: bool,

    /// Parse and validate the configuration without launching anything.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    process: ProcessConfig,
    #[serde(default)]
    cleanup: Option<CleanupConfig>,
    #[serde(default)]
    input: Option<InputConfig>,
    capture: CaptureConfig,
    #[serde(default)]
    video: Option<VideoConfig>,
    #[serde(default)]
    session: Option<session::Config>,
    #[serde(default)]
    renderdoc: Option<renderdoc::Config>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessConfig {
    executable: PathBuf,
    rom_path: PathBuf,
    /// Optional argument placed immediately before `rom_path` (for example `-g`).
    rom_arg: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    working_directory: Option<PathBuf>,
    #[serde(default)]
    environment: BTreeMap<String, String>,
    #[serde(default)]
    clear_environment: bool,
    log_file: Option<PathBuf>,
    stop_at: Option<String>,
    #[serde(default = "default_true")]
    terminate_after_timeline: bool,
    #[serde(default = "default_termination_grace_ms")]
    termination_grace_ms: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CleanupConfig {
    #[serde(default)]
    shader_cache_paths: Vec<PathBuf>,
    #[serde(default)]
    save_data_paths: Vec<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputConfig {
    /// Override the frontend configuration path. When omitted, the harness
    /// selects sdl2-config.ini for ruzu-cmd and the frontend's qt-config.ini
    /// for Reden or Eden.
    config_file: Option<PathBuf>,
    #[serde(default = "default_input_hold_ms")]
    default_hold_ms: u64,
    #[serde(default = "default_true")]
    restore_config: bool,
    #[serde(default)]
    events: Vec<InputEventConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputEventConfig {
    at: String,
    buttons: Vec<SwitchButton>,
    hold_ms: Option<u64>,
    label: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum SwitchButton {
    A,
    B,
    X,
    Y,
    LStick,
    RStick,
    L,
    R,
    ZL,
    ZR,
    Plus,
    Minus,
    DLeft,
    DUp,
    DRight,
    DDown,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureConfig {
    output_directory: PathBuf,
    #[serde(default)]
    times: Vec<String>,
    #[serde(default)]
    target: CaptureTarget,
    window_id: Option<u64>,
    window_title_regex: Option<String>,
    #[serde(default = "default_true")]
    match_process_pid: bool,
    #[serde(default = "default_window_timeout")]
    window_wait_timeout: String,
    #[serde(default = "default_screenshot_prefix")]
    screenshot_prefix: String,
}

#[derive(Debug, Default, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CaptureTarget {
    #[default]
    Window,
    Screen,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VideoConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_zero_time")]
    start: String,
    end: String,
    output: PathBuf,
    #[serde(default = "default_fps")]
    fps: u32,
    #[serde(default = "default_video_codec")]
    codec: String,
    #[serde(default = "default_crf")]
    crf: u8,
    #[serde(default)]
    include_cursor: bool,
}

#[derive(Debug, Serialize)]
struct Manifest {
    process_id: u32,
    rom_path: String,
    timer_origin: String,
    target: String,
    window_id: Option<u64>,
    cleanup: Vec<CleanupRecord>,
    events: Vec<ManifestEvent>,
    process_exit: Option<i32>,
}

#[derive(Debug, Serialize)]
struct CleanupRecord {
    kind: &'static str,
    path: String,
    existed: bool,
    removed: bool,
}

#[derive(Debug, Serialize)]
struct ManifestEvent {
    kind: String,
    scheduled_ms: u128,
    actual_ms: u128,
    lateness_ms: i128,
    output: Option<String>,
    success: bool,
    detail: Option<String>,
}

#[derive(Debug, Clone)]
enum EventKind {
    VideoStart,
    InputRelease { index: usize },
    InputPress { index: usize },
    Screenshot { index: usize },
    VideoStop,
    TimelineStop,
}

#[derive(Debug, Clone)]
struct Event {
    at: Duration,
    priority: u8,
    kind: EventKind,
}

#[derive(Debug, Clone, Copy)]
struct WindowGeometry {
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputFrontend {
    Reden,
    Eden,
    RuzuCmd,
}

#[derive(Debug, Clone)]
struct PreparedInputEvent {
    at: Duration,
    release_at: Duration,
    buttons: Vec<SwitchButton>,
    keys: Vec<&'static str>,
    label: Option<String>,
}

#[derive(Debug, Clone)]
struct PreparedInputConfig {
    path: PathBuf,
    frontend: InputFrontend,
    restore: bool,
}

fn default_true() -> bool {
    true
}

fn default_termination_grace_ms() -> u64 {
    3_000
}

fn default_input_hold_ms() -> u64 {
    100
}

fn default_window_timeout() -> String {
    "00:00:15".to_owned()
}

fn default_screenshot_prefix() -> String {
    "reference".to_owned()
}

fn default_zero_time() -> String {
    "00:00:00".to_owned()
}

fn default_fps() -> u32 {
    60
}

fn default_video_codec() -> String {
    "libx264".to_owned()
}

fn default_crf() -> u8 {
    18
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.list_input_devices {
        #[cfg(target_os = "linux")]
        return linux_input::list_devices();
        #[cfg(not(target_os = "linux"))]
        bail!("raw input recording/replay currently requires Linux");
    }
    let config_arg = cli
        .config
        .context("provide a TOML configuration or --list-input-devices")?;
    let config_path = config_arg
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", config_arg.display()))?;
    let config_directory = config_path
        .parent()
        .context("configuration has no parent directory")?;
    let contents = fs::read_to_string(&config_path)
        .with_context(|| format!("cannot read {}", config_path.display()))?;
    let mut config: Config = toml::from_str(&contents)
        .with_context(|| format!("invalid TOML in {}", config_path.display()))?;
    resolve_paths(&mut config, config_directory);
    let prepared = PreparedRun::new(config, config_path)?;

    if cli.dry_run {
        prepared.print_summary();
        return Ok(());
    }
    prepared.run()
}

struct PreparedRun {
    config: Config,
    config_path: PathBuf,
    screenshot_times: Vec<Duration>,
    input_events: Vec<PreparedInputEvent>,
    input_config: Option<PreparedInputConfig>,
    window_timeout: Duration,
    video_times: Option<(Duration, Duration)>,
    stop_at: Option<Duration>,
    session_duration: Duration,
    renderdoc_end: Duration,
    needs_x11: bool,
    display: String,
}

impl PreparedRun {
    fn new(config: Config, config_path: PathBuf) -> Result<Self> {
        if !config.process.rom_path.try_exists().with_context(|| {
            format!(
                "cannot query configured ROM path {}",
                config.process.rom_path.display()
            )
        })? {
            bail!(
                "configured ROM path does not exist: {}",
                config.process.rom_path.display()
            );
        }
        if config
            .process
            .rom_arg
            .as_ref()
            .is_some_and(|argument| argument.is_empty())
        {
            bail!("process.rom_arg must not be empty when present");
        }
        let has_input_events = config
            .input
            .as_ref()
            .is_some_and(|input| !input.events.is_empty());
        if has_input_events && config.session.is_some() {
            bail!("session and input.events are mutually exclusive; raw sessions preserve frontend settings");
        }
        #[cfg(not(target_os = "linux"))]
        if config.session.is_some() {
            bail!("raw input sessions require Linux");
        }
        let session_duration = config
            .session
            .as_ref()
            .map(session::Config::validate)
            .transpose()?
            .unwrap_or_default();
        #[cfg(not(target_os = "linux"))]
        if config.renderdoc.is_some() {
            bail!("the RenderDoc preload bridge currently requires Linux");
        }
        let renderdoc_end = config
            .renderdoc
            .as_ref()
            .map(renderdoc::Config::end)
            .transpose()?
            .unwrap_or_default();
        let needs_x11 = has_input_events
            || !config.capture.times.is_empty()
            || config.video.as_ref().is_some_and(|video| video.enabled);
        if config.capture.times.is_empty()
            && !config.video.as_ref().is_some_and(|video| video.enabled)
            && !has_input_events
            && config.session.is_none()
            && config.renderdoc.is_none()
        {
            bail!("capture.times is empty, video is disabled, and no input events are configured");
        }
        if has_input_events && config.capture.target != CaptureTarget::Window {
            bail!("input events require capture.target = \"window\"");
        }
        if config.capture.target == CaptureTarget::Window
            && config.capture.window_id.is_none()
            && !config.capture.match_process_pid
            && config.capture.window_title_regex.is_none()
        {
            bail!("window capture needs window_id, match_process_pid, or window_title_regex");
        }

        let mut screenshot_times = config
            .capture
            .times
            .iter()
            .map(|value| parse_timecode(value))
            .collect::<Result<Vec<_>>>()?;
        screenshot_times.sort_unstable();
        if screenshot_times.windows(2).any(|pair| pair[0] == pair[1]) {
            bail!("capture.times contains a duplicate timecode");
        }

        let window_timeout = parse_timecode(&config.capture.window_wait_timeout)?;
        let video_times = match config.video.as_ref().filter(|video| video.enabled) {
            Some(video) => {
                if video.fps == 0 {
                    bail!("video.fps must be greater than zero");
                }
                let start = parse_timecode(&video.start)?;
                let end = parse_timecode(&video.end)?;
                if end <= start {
                    bail!("video.end must be later than video.start");
                }
                Some((start, end))
            }
            None => None,
        };
        let stop_at = config
            .process
            .stop_at
            .as_deref()
            .map(parse_timecode)
            .transpose()?;
        let (input_events, input_config) = prepare_input_timeline(&config)?;
        let last_capture = screenshot_times.last().copied().unwrap_or_default();
        let last_input = input_events
            .iter()
            .map(|event| event.release_at)
            .max()
            .unwrap_or_default();
        let required_end = video_times
            .map_or(last_capture, |(_, end)| end.max(last_capture))
            .max(last_input)
            .max(session_duration);
        let required_end = required_end.max(renderdoc_end);
        if stop_at.is_some_and(|stop| stop < required_end) {
            bail!("process.stop_at is earlier than the last capture/video event");
        }

        let display = config
            .process
            .environment
            .get("DISPLAY")
            .cloned()
            .or_else(|| std::env::var("DISPLAY").ok());
        if needs_x11 && display.is_none() {
            bail!("DISPLAY is required for X11 screenshots/video/logical input");
        }
        let display = display.unwrap_or_default();

        let prepared = Self {
            config,
            config_path,
            screenshot_times,
            input_events,
            input_config,
            window_timeout,
            video_times,
            stop_at,
            session_duration,
            renderdoc_end,
            needs_x11,
            display,
        };
        prepared.validate_session_output_paths()?;
        prepared.validate_cleanup_targets()?;
        Ok(prepared)
    }

    fn validate_session_output_paths(&self) -> Result<()> {
        let Some(session) = &self.config.session else {
            return Ok(());
        };
        let directory = &self.config.capture.output_directory;
        let mut outputs = vec![
            directory.join("capture-manifest.json"),
            directory.join("input-replay.json"),
            directory.join("renderdoc-manifest.json"),
        ];
        if let Some(log) = &self.config.process.log_file {
            outputs.push(log.clone());
        }
        if let Some(video) = self.config.video.as_ref().filter(|video| video.enabled) {
            outputs.push(video_output_path(video, directory));
        }
        for (index, &at) in self.screenshot_times.iter().enumerate() {
            outputs.push(screenshot_path(
                directory,
                &self.config.capture.screenshot_prefix,
                index,
                at,
            ));
        }
        let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_owned());
        if outputs
            .iter()
            .any(|path| canonical(path) == canonical(&session.file))
        {
            bail!(
                "session.file must differ from logs, screenshots, video and manifest output paths"
            );
        }
        Ok(())
    }

    fn print_summary(&self) {
        println!("executable: {}", self.config.process.executable.display());
        println!("ROM: {}", self.config.process.rom_path.display());
        println!("cleanup targets: {}", self.cleanup_targets().len());
        println!("target: {:?}", self.config.capture.target);
        println!("screenshots: {}", self.screenshot_times.len());
        println!("input events: {}", self.input_events.len());
        if let Some(session) = &self.config.session {
            println!(
                "raw input: {:?}; file: {}; duration: {:?}",
                session.mode,
                session.file.display(),
                self.session_duration
            );
        }
        if let Some(input) = &self.input_config {
            println!(
                "input frontend: {:?}; config: {}",
                input.frontend,
                input.path.display()
            );
        }
        println!("video: {}", self.video_times.is_some());
        println!("RenderDoc: {}", self.config.renderdoc.is_some());
        println!("output: {}", self.config.capture.output_directory.display());
    }

    fn run(self) -> Result<()> {
        ctrlc::set_handler(|| CANCELLED.store(true, std::sync::atomic::Ordering::Release))
            .context("cannot install cancellation handler")?;
        if self.needs_x11 {
            require_command("xdotool")?;
        }
        if !self.screenshot_times.is_empty() {
            require_command("import")?;
        }
        if self.video_times.is_some() {
            require_command("ffmpeg")?;
        }
        // XTEST key state outlives the process which injected it. In particular, an interrupted
        // ruzu-cmd run can leave its `A` key held; that same physical key is Reden's left-stick
        // left binding. Clear every key owned by the harness before changing frontend configs,
        // and release them again on every Rust-controlled exit path.
        let _input_release_guard = if self.input_events.is_empty() {
            None
        } else {
            Some(InputReleaseGuard::install(&self.display)?)
        };
        let cleanup = self.perform_cleanup()?;
        fs::create_dir_all(&self.config.capture.output_directory).with_context(|| {
            format!(
                "cannot create {}",
                self.config.capture.output_directory.display()
            )
        })?;

        let _input_config_guard = self
            .input_config
            .as_ref()
            .map(KeyboardConfigGuard::install)
            .transpose()?;
        #[cfg(target_os = "linux")]
        let prepared_session = self
            .config
            .session
            .as_ref()
            .map(|config| {
                linux_input::Prepared::new(
                    config,
                    &self
                        .config
                        .capture
                        .output_directory
                        .join("input-replay.json"),
                )
            })
            .transpose()?;
        #[cfg(target_os = "linux")]
        let prepared_renderdoc = self
            .config
            .renderdoc
            .as_ref()
            .map(|config| {
                renderdoc::Prepared::new(
                    config,
                    &self
                        .config
                        .capture
                        .output_directory
                        .join("renderdoc-manifest.json"),
                )
            })
            .transpose()?;
        let mut target = TargetGuard(
            spawn_target(
                &self.config.process,
                #[cfg(target_os = "linux")]
                prepared_renderdoc
                    .as_ref()
                    .zip(self.config.renderdoc.as_ref()),
            )?,
            Duration::from_millis(self.config.process.termination_grace_ms),
        );
        // This is the sole timer origin: immediately after the OS accepted the
        // child process. All waits below use absolute offsets from this Instant.
        let origin = Instant::now();
        #[cfg(target_os = "linux")]
        let mut session_worker = prepared_session
            .map(|session| {
                session.start(
                    origin,
                    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                )
            })
            .transpose()?;
        #[cfg(target_os = "linux")]
        let mut renderdoc_worker = prepared_renderdoc
            .map(|capture| capture.start(origin))
            .transpose()?;
        let pid = target.id();
        println!("launched pid={pid}; monotonic timer started");

        let (window_id, geometry) = if !self.needs_x11 {
            (
                None,
                WindowGeometry {
                    width: 0,
                    height: 0,
                },
            )
        } else {
            match self.config.capture.target {
                CaptureTarget::Window => match find_window(
                    pid,
                    &self.config.capture,
                    &self.display,
                    origin,
                    self.window_timeout,
                    &mut target,
                ) {
                    Ok(found) => (Some(found.0), found.1),
                    Err(error) => {
                        terminate_child(
                            &mut target,
                            Duration::from_millis(self.config.process.termination_grace_ms),
                        );
                        return Err(error);
                    }
                },
                CaptureTarget::Screen => (None, display_geometry(&self.display)?),
            }
        };

        // The pre-spawn cleanup resets XTEST globally, but the newly-created input frontend has
        // not observed those release events. Focus the emulation window and deliver a neutral
        // state before the first scheduled input.
        if let Some(window_id) = window_id.filter(|_| !self.input_events.is_empty()) {
            neutralize_window_input(window_id, &self.display)?;
        }

        let mut events = self.events();
        events.sort_by(|left, right| {
            left.at
                .cmp(&right.at)
                .then_with(|| left.priority.cmp(&right.priority))
        });
        let mut video_child: Option<Child> = None;
        let mut manifest_events = Vec::new();
        let mut target_exit: Option<ExitStatus> = None;

        for event in events {
            if target_exit.is_none() {
                target_exit = wait_until(&mut target, origin, event.at)?;
            }
            if cancelled() {
                break;
            }
            let actual = origin.elapsed();
            let mut record = ManifestEvent {
                kind: event_name(&event.kind).to_owned(),
                scheduled_ms: event.at.as_millis(),
                actual_ms: actual.as_millis(),
                lateness_ms: actual.as_millis() as i128 - event.at.as_millis() as i128,
                output: None,
                success: true,
                detail: None,
            };

            if target_exit.is_some() && !matches!(event.kind, EventKind::TimelineStop) {
                record.success = false;
                record.detail = Some("target process exited before this event".to_owned());
                manifest_events.push(record);
                continue;
            }

            match event.kind {
                EventKind::VideoStart => {
                    let video = self.config.video.as_ref().expect("validated video");
                    let (_, scheduled_end) = self.video_times.expect("validated video times");
                    let remaining = scheduled_end.saturating_sub(actual);
                    if remaining.is_zero() {
                        record.success = false;
                        record.detail =
                            Some("window discovery consumed the video interval".to_owned());
                    } else {
                        let output =
                            video_output_path(video, &self.config.capture.output_directory);
                        video_child = Some(start_video(
                            video,
                            &self.display,
                            window_id,
                            geometry,
                            remaining,
                            &output,
                        )?);
                        record.output = Some(output.display().to_string());
                    }
                }
                EventKind::InputRelease { index } | EventKind::InputPress { index } => {
                    let input = &self.input_events[index];
                    let pressed = matches!(event.kind, EventKind::InputPress { .. });
                    record.detail = Some(input_event_detail(input));
                    if let Err(error) = send_input(
                        window_id.expect("window target validated for input"),
                        &self.display,
                        &input.keys,
                        pressed,
                    ) {
                        record.success = false;
                        record.detail = Some(format!("{}: {error:#}", input_event_detail(input)));
                    }
                }
                EventKind::Screenshot { index } => {
                    let output = screenshot_path(
                        &self.config.capture.output_directory,
                        &self.config.capture.screenshot_prefix,
                        index,
                        event.at,
                    );
                    let result = take_screenshot(window_id, &self.display, &output);
                    record.output = Some(output.display().to_string());
                    if let Err(error) = result {
                        record.success = false;
                        record.detail = Some(format!("{error:#}"));
                    }
                }
                EventKind::VideoStop => {
                    if let Some(mut child) = video_child.take() {
                        stop_video(&mut child)?;
                        let status = child.wait().context("cannot wait for ffmpeg")?;
                        record.success = status.success();
                        if !status.success() {
                            record.detail = Some(format!("ffmpeg exited with {status}"));
                        }
                    }
                }
                EventKind::TimelineStop => {}
            }
            println!(
                "event={} scheduled={}ms actual={}ms",
                record.kind, record.scheduled_ms, record.actual_ms
            );
            manifest_events.push(record);
        }

        if let Some(mut child) = video_child {
            stop_video(&mut child)?;
            let _ = child.wait();
        }
        #[cfg(target_os = "linux")]
        let session_result = session_worker
            .as_mut()
            .map(|worker| {
                if target_exit.is_some() || cancelled() {
                    worker.finish()
                } else {
                    worker.complete()
                }
            })
            .transpose();
        #[cfg(target_os = "linux")]
        let renderdoc_result = renderdoc_worker
            .as_mut()
            .map(|worker| {
                if target_exit.is_some() || cancelled() {
                    worker.finish()
                } else {
                    worker.complete()
                }
            })
            .transpose();
        if target_exit.is_none() && (self.config.process.terminate_after_timeline || cancelled()) {
            terminate_child(
                &mut target,
                Duration::from_millis(self.config.process.termination_grace_ms),
            );
            target_exit = target.try_wait().context("cannot query target status")?;
        } else if target_exit.is_none() {
            while !cancelled() {
                target_exit = target
                    .try_wait()
                    .context("cannot wait for target process")?;
                if target_exit.is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
        }

        let manifest = Manifest {
            process_id: pid,
            rom_path: self.config.process.rom_path.display().to_string(),
            timer_origin: "Instant immediately after Command::spawn returned".to_owned(),
            target: format!("{:?}", self.config.capture.target).to_lowercase(),
            window_id,
            cleanup,
            events: manifest_events,
            process_exit: target_exit.and_then(|status| status.code()),
        };
        let manifest_path = self
            .config
            .capture
            .output_directory
            .join("capture-manifest.json");
        let manifest_json = serde_json::to_vec_pretty(&manifest)?;
        fs::write(&manifest_path, manifest_json)
            .with_context(|| format!("cannot write {}", manifest_path.display()))?;
        println!("manifest: {}", manifest_path.display());
        #[cfg(target_os = "linux")]
        session_result?;
        #[cfg(target_os = "linux")]
        renderdoc_result?;
        Ok(())
    }

    fn cleanup_targets(&self) -> Vec<CleanupTarget<'_>> {
        let Some(cleanup) = &self.config.cleanup else {
            return Vec::new();
        };
        cleanup
            .shader_cache_paths
            .iter()
            .map(|path| CleanupTarget {
                kind: "shader_cache",
                path,
            })
            .chain(cleanup.save_data_paths.iter().map(|path| CleanupTarget {
                kind: "save_data",
                path,
            }))
            .collect()
    }

    fn validate_cleanup_targets(&self) -> Result<()> {
        let targets = self.cleanup_targets();
        let critical_paths = self.critical_paths()?;
        for (index, target) in targets.iter().enumerate() {
            validate_cleanup_path(target.path, &critical_paths)?;
            for other in targets.iter().skip(index + 1) {
                if target.path == other.path {
                    bail!(
                        "cleanup path is listed more than once: {}",
                        target.path.display()
                    );
                }
                if target.path.starts_with(other.path) || other.path.starts_with(target.path) {
                    bail!(
                        "cleanup paths must not contain one another: {} and {}",
                        target.path.display(),
                        other.path.display()
                    );
                }
            }
        }
        Ok(())
    }

    fn critical_paths(&self) -> Result<Vec<PathBuf>> {
        let mut paths = vec![
            self.config_path.clone(),
            self.config.process.rom_path.clone(),
            self.config.capture.output_directory.clone(),
            std::env::current_dir().context("cannot resolve current directory")?,
        ];
        if self.config.process.executable.is_absolute() {
            paths.push(self.config.process.executable.clone());
        }
        if let Some(path) = &self.config.process.working_directory {
            paths.push(path.clone());
        }
        if let Some(path) = &self.config.process.log_file {
            paths.push(path.clone());
        }
        if let Some(input) = &self.input_config {
            paths.push(input.path.clone());
        }
        if let Some(session) = &self.config.session {
            paths.push(session.file.clone());
            if let Some(device) = &session.device {
                paths.push(device.clone());
            }
        }
        if let Some(renderdoc) = &self.config.renderdoc {
            paths.extend([
                renderdoc.library.clone(),
                renderdoc.helper.clone(),
                renderdoc.capture_prefix.clone(),
            ]);
            if let Some(path) = &renderdoc.vulkan_layer_manifest {
                paths.push(path.clone());
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home));
        }
        Ok(paths)
    }

    fn perform_cleanup(&self) -> Result<Vec<CleanupRecord>> {
        self.validate_cleanup_targets()?;
        let critical_paths = self.critical_paths()?;
        self.cleanup_targets()
            .into_iter()
            .map(|target| remove_cleanup_target(target, &critical_paths))
            .collect()
    }

    fn events(&self) -> Vec<Event> {
        let mut events = self
            .screenshot_times
            .iter()
            .copied()
            .enumerate()
            .map(|(index, at)| Event {
                at,
                priority: 3,
                kind: EventKind::Screenshot { index },
            })
            .collect::<Vec<_>>();
        if let Some((start, end)) = self.video_times {
            events.push(Event {
                at: start,
                priority: 0,
                kind: EventKind::VideoStart,
            });
            events.push(Event {
                at: end,
                priority: 4,
                kind: EventKind::VideoStop,
            });
        }
        for (index, input) in self.input_events.iter().enumerate() {
            events.push(Event {
                at: input.release_at,
                priority: 1,
                kind: EventKind::InputRelease { index },
            });
            events.push(Event {
                at: input.at,
                priority: 2,
                kind: EventKind::InputPress { index },
            });
        }
        let natural_end = events
            .iter()
            .map(|event| event.at)
            .max()
            .unwrap_or_default()
            .max(self.session_duration)
            .max(self.renderdoc_end);
        events.push(Event {
            at: self.stop_at.unwrap_or(natural_end),
            priority: 5,
            kind: EventKind::TimelineStop,
        });
        events
    }
}

fn prepare_input_timeline(
    config: &Config,
) -> Result<(Vec<PreparedInputEvent>, Option<PreparedInputConfig>)> {
    let Some(input) = config
        .input
        .as_ref()
        .filter(|input| !input.events.is_empty())
    else {
        return Ok((Vec::new(), None));
    };
    if input.default_hold_ms == 0 {
        bail!("input.default_hold_ms must be greater than zero");
    }

    let frontend = input_frontend(&config.process.executable)?;
    let path = input_config_path(&config.process, input, frontend)?;
    if !path
        .try_exists()
        .with_context(|| format!("cannot query input config {}", path.display()))?
    {
        bail!("input config does not exist: {}", path.display());
    }
    let mut events = Vec::with_capacity(input.events.len());
    for event in &input.events {
        if event.buttons.is_empty() {
            bail!("an input event at {} has no buttons", event.at);
        }
        let unique = event.buttons.iter().copied().collect::<BTreeSet<_>>();
        if unique.len() != event.buttons.len() {
            bail!(
                "an input event at {} lists a button more than once",
                event.at
            );
        }
        let hold_ms = event.hold_ms.unwrap_or(input.default_hold_ms);
        if hold_ms == 0 {
            bail!("input event hold_ms must be greater than zero");
        }
        let at = parse_timecode(&event.at)?;
        let release_at = at
            .checked_add(Duration::from_millis(hold_ms))
            .context("input event release time is too large")?;
        events.push(PreparedInputEvent {
            at,
            release_at,
            buttons: event.buttons.clone(),
            keys: event
                .buttons
                .iter()
                .map(|button| button.x_key(frontend))
                .collect(),
            label: event.label.clone(),
        });
    }
    events.sort_by_key(|event| event.at);
    for (index, event) in events.iter().enumerate() {
        for other in events.iter().skip(index + 1) {
            if other.at >= event.release_at {
                break;
            }
            if event
                .buttons
                .iter()
                .any(|button| other.buttons.contains(button))
            {
                bail!(
                    "input events at {}ms and {}ms overlap on the same button",
                    event.at.as_millis(),
                    other.at.as_millis()
                );
            }
        }
    }
    Ok((
        events,
        Some(PreparedInputConfig {
            path,
            frontend,
            restore: input.restore_config,
        }),
    ))
}

impl SwitchButton {
    fn x_key(self, _frontend: InputFrontend) -> &'static str {
        // The configuration formats use different key-code namespaces, but the injected physical
        // key must remain identical across frontends. Otherwise an interrupted ruzu-cmd `A` (`a`)
        // becomes Reden's left-stick-left input when the next run starts.
        match self {
            Self::A => "c",
            Self::B => "x",
            Self::X => "v",
            Self::Y => "z",
            Self::LStick => "f",
            Self::RStick => "g",
            Self::L => "q",
            Self::R => "e",
            Self::ZL => "r",
            Self::ZR => "t",
            Self::Plus => "m",
            Self::Minus => "n",
            Self::DLeft => "Left",
            Self::DUp => "Up",
            Self::DRight => "Right",
            Self::DDown => "Down",
        }
    }
}

fn input_frontend(executable: &Path) -> Result<InputFrontend> {
    let name = executable
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.contains("ruzu-cmd") || name.contains("ruzu_cmd") {
        Ok(InputFrontend::RuzuCmd)
    } else if name == "reden" || name.starts_with("reden-") {
        Ok(InputFrontend::Reden)
    } else if name == "eden" || name.starts_with("eden-") {
        Ok(InputFrontend::Eden)
    } else {
        bail!(
            "cannot infer input frontend from executable {}; use reden, eden, or ruzu-cmd",
            executable.display()
        )
    }
}

fn input_config_path(
    process: &ProcessConfig,
    input: &InputConfig,
    frontend: InputFrontend,
) -> Result<PathBuf> {
    if let Some(path) = &input.config_file {
        return Ok(path.clone());
    }
    if frontend == InputFrontend::RuzuCmd {
        if let Some(path) = command_line_config_path(process)? {
            return Ok(path);
        }
    }

    let home = process_environment(process, "HOME")
        .map(PathBuf::from)
        .context("HOME is not set; input.config_file is required")?;
    let data_home = process_environment(process, "XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let config_home = process_environment(process, "XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    if frontend == InputFrontend::Eden {
        return Ok(config_home.join("eden/qt-config.ini"));
    }
    let data_root = data_home.join("ruzu");
    let config_directory = if data_root.is_dir() {
        data_root.join("config")
    } else {
        config_home.join("ruzu")
    };
    Ok(config_directory.join(match frontend {
        InputFrontend::Reden => "qt-config.ini",
        InputFrontend::Eden => unreachable!("Eden config path returned above"),
        InputFrontend::RuzuCmd => "sdl2-config.ini",
    }))
}

fn process_environment(process: &ProcessConfig, name: &str) -> Option<OsString> {
    process
        .environment
        .get(name)
        .map(OsString::from)
        .or_else(|| {
            (!process.clear_environment)
                .then(|| std::env::var_os(name))
                .flatten()
        })
}

fn command_line_config_path(process: &ProcessConfig) -> Result<Option<PathBuf>> {
    let mut arguments = process.args.iter();
    while let Some(argument) = arguments.next() {
        let value = if argument == "-c" || argument == "--config" {
            Some(
                arguments
                    .next()
                    .context("ruzu-cmd config argument has no path")?
                    .as_str(),
            )
        } else {
            argument.strip_prefix("--config=")
        };
        if let Some(value) = value {
            let path = PathBuf::from(value);
            let base = process
                .working_directory
                .clone()
                .unwrap_or(std::env::current_dir()?);
            return Ok(Some(if path.is_absolute() {
                path
            } else {
                normalize_absolute(&base.join(path))
            }));
        }
    }
    Ok(None)
}

struct KeyboardConfigGuard {
    path: PathBuf,
    original: Vec<u8>,
    restore: bool,
}

impl KeyboardConfigGuard {
    fn install(config: &PreparedInputConfig) -> Result<Self> {
        let original = fs::read(&config.path)
            .with_context(|| format!("cannot read input config {}", config.path.display()))?;
        let contents = std::str::from_utf8(&original)
            .with_context(|| format!("input config is not UTF-8: {}", config.path.display()))?;
        let updated = keyboard_mouse_config(contents, config.frontend);
        fs::write(&config.path, updated)
            .with_context(|| format!("cannot update input config {}", config.path.display()))?;
        println!(
            "input config switched to Keyboard/Mouse: {}",
            config.path.display()
        );
        Ok(Self {
            path: config.path.clone(),
            original,
            restore: config.restore,
        })
    }
}

impl Drop for KeyboardConfigGuard {
    fn drop(&mut self) {
        if self.restore {
            match fs::write(&self.path, &self.original) {
                Ok(()) => println!("input config restored: {}", self.path.display()),
                Err(error) => eprintln!(
                    "cannot restore input config {}: {error}",
                    self.path.display()
                ),
            }
        }
    }
}

const BUTTON_NAMES: [&str; 22] = [
    "button_a",
    "button_b",
    "button_x",
    "button_y",
    "button_lstick",
    "button_rstick",
    "button_l",
    "button_r",
    "button_zl",
    "button_zr",
    "button_plus",
    "button_minus",
    "button_dleft",
    "button_dup",
    "button_dright",
    "button_ddown",
    "button_slleft",
    "button_srleft",
    "button_home",
    "button_screenshot",
    "button_slright",
    "button_srright",
];

const QT_BUTTON_CODES: [i32; 22] = [
    67, 88, 86, 90, 70, 71, 81, 69, 82, 84, 77, 78, 16_777_234, 16_777_235, 16_777_236, 16_777_237,
    81, 69, 0, 0, 81, 69,
];

const RUZU_CMD_BUTTON_CODES: [i32; 22] = [
    // SDL scancodes for the same physical keys as QT_BUTTON_CODES: C, X, V, Z, F, G, Q, E,
    // R, T, M, N, Left, Up, Right, Down.
    6, 27, 25, 29, 9, 10, 20, 8, 21, 23, 16, 17, 80, 82, 79, 81, 20, 8, 0, 0, 20, 8,
];

fn keyboard_mouse_config(contents: &str, frontend: InputFrontend) -> String {
    let mut entries = BTreeMap::new();
    let codes = match frontend {
        InputFrontend::Reden | InputFrontend::Eden => QT_BUTTON_CODES,
        InputFrontend::RuzuCmd => RUZU_CMD_BUTTON_CODES,
    };
    for (name, code) in BUTTON_NAMES.into_iter().zip(codes) {
        insert_control_binding(
            &mut entries,
            &format!("player_0_{name}"),
            &keyboard_binding(code),
        );
    }
    let (left_keys, modifier, motion_keys) = match frontend {
        InputFrontend::Reden | InputFrontend::Eden => ([87, 83, 65, 68], 16_777_248, [55, 56]),
        InputFrontend::RuzuCmd => ([26, 22, 4, 7], 225, [36, 37]),
    };
    insert_control_binding(
        &mut entries,
        "player_0_lstick",
        &keyboard_analog_binding(left_keys, modifier),
    );
    insert_control_binding(
        &mut entries,
        "player_0_rstick",
        "engine:mouse,axis_x:0,axis_y:1,threshold:0.5,range:1,deadzone:0",
    );
    insert_control_binding(
        &mut entries,
        "player_0_motionleft",
        &keyboard_binding(motion_keys[0]),
    );
    insert_control_binding(
        &mut entries,
        "player_0_motionright",
        &keyboard_binding(motion_keys[1]),
    );
    entries.insert("player_0_connected".to_owned(), "true".to_owned());
    entries.insert("player_0_connected\\default".to_owned(), "false".to_owned());
    entries.insert("player_0_type".to_owned(), "0".to_owned());
    entries.insert("player_0_type\\default".to_owned(), "false".to_owned());
    entries.insert("player_0_profile_name".to_owned(), String::new());
    entries.insert(
        "player_0_profile_name\\default".to_owned(),
        "false".to_owned(),
    );
    entries.insert("mouse_enabled".to_owned(), "true".to_owned());
    entries.insert("mouse_enabled\\default".to_owned(), "false".to_owned());
    replace_ini_section(contents, "Controls", &entries)
}

fn insert_control_binding(entries: &mut BTreeMap<String, String>, key: &str, value: &str) {
    entries.insert(key.to_owned(), format!("\"{value}\""));
    entries.insert(format!("{key}\\default"), "false".to_owned());
}

fn keyboard_binding(code: i32) -> String {
    format!("engine:keyboard,code:{code},toggle:0")
}

fn keyboard_analog_binding(keys: [i32; 4], modifier: i32) -> String {
    let escaped = |code| keyboard_binding(code).replace(':', "$0").replace(',', "$1");
    format!(
        "engine:analog_from_button,up:{},down:{},left:{},right:{},modifier:{},modifier_scale:0.5",
        escaped(keys[0]),
        escaped(keys[1]),
        escaped(keys[2]),
        escaped(keys[3]),
        escaped(modifier)
    )
}

fn replace_ini_section(
    contents: &str,
    section: &str,
    entries: &BTreeMap<String, String>,
) -> String {
    let section_header = format!("[{section}]");
    let had_trailing_newline = contents.ends_with('\n');
    let mut lines = contents.lines().map(str::to_owned).collect::<Vec<_>>();
    let start = lines.iter().position(|line| line.trim() == section_header);
    let (insert_at, range_start, range_end) = match start {
        Some(start) => {
            let end = lines
                .iter()
                .enumerate()
                .skip(start + 1)
                .find_map(|(index, line)| {
                    let trimmed = line.trim();
                    (trimmed.starts_with('[') && trimmed.ends_with(']')).then_some(index)
                })
                .unwrap_or(lines.len());
            (end, start + 1, end)
        }
        None => {
            if !lines.is_empty() && !lines.last().is_some_and(|line| line.is_empty()) {
                lines.push(String::new());
            }
            lines.push(section_header);
            let end = lines.len();
            (end, end, end)
        }
    };

    let mut seen = BTreeSet::new();
    for line in &mut lines[range_start..range_end] {
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_owned();
        if let Some(value) = entries.get(&key) {
            *line = format!("{key}={value}");
            seen.insert(key);
        }
    }
    let missing = entries
        .iter()
        .filter(|(key, _)| !seen.contains(*key))
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>();
    lines.splice(insert_at..insert_at, missing);
    let mut result = lines.join("\n");
    if had_trailing_newline || !result.is_empty() {
        result.push('\n');
    }
    result
}

fn resolve_paths(config: &mut Config, base: &Path) {
    if let Some(renderdoc) = &mut config.renderdoc {
        renderdoc.library = resolve_relative(&renderdoc.library, base);
        renderdoc.helper = resolve_relative(&renderdoc.helper, base);
        renderdoc.capture_prefix = resolve_relative(&renderdoc.capture_prefix, base);
        if let Some(path) = &mut renderdoc.vulkan_layer_manifest {
            *path = resolve_relative(path, base);
        }
    }
    if let Some(session) = &mut config.session {
        session.file = resolve_relative(&session.file, base);
        if let Some(device) = &mut session.device {
            *device = resolve_relative(device, base);
        }
    }
    config.process.executable = resolve_executable(&config.process.executable, base);
    config.process.rom_path = resolve_relative(&config.process.rom_path, base);
    if let Some(path) = config.process.working_directory.as_mut() {
        *path = resolve_relative(path, base);
    }
    if let Some(path) = config.process.log_file.as_mut() {
        *path = resolve_relative(path, base);
    }
    if let Some(cleanup) = config.cleanup.as_mut() {
        for path in cleanup
            .shader_cache_paths
            .iter_mut()
            .chain(cleanup.save_data_paths.iter_mut())
        {
            *path = resolve_relative(path, base);
        }
    }
    if let Some(path) = config
        .input
        .as_mut()
        .and_then(|input| input.config_file.as_mut())
    {
        *path = resolve_relative(path, base);
    }
    config.capture.output_directory = resolve_relative(&config.capture.output_directory, base);
}

fn resolve_executable(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() || path.components().count() == 1 {
        path.to_owned()
    } else {
        base.join(path)
    }
}

fn resolve_relative(path: &Path, base: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    };
    normalize_absolute(&absolute)
}

fn normalize_absolute(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

#[derive(Debug, Clone, Copy)]
struct CleanupTarget<'a> {
    kind: &'static str,
    path: &'a Path,
}

fn validate_cleanup_path(path: &Path, critical_paths: &[PathBuf]) -> Result<()> {
    if !path.is_absolute() || path.file_name().is_none() {
        bail!(
            "cleanup path must resolve to a specific absolute path: {}",
            path.display()
        );
    }
    let normal_components = path
        .components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count();
    if normal_components < 2 {
        bail!("cleanup path is too broad: {}", path.display());
    }
    for critical in critical_paths {
        if critical == path || critical.starts_with(path) {
            bail!(
                "cleanup path {} contains protected path {}",
                path.display(),
                critical.display()
            );
        }
    }
    Ok(())
}

fn remove_cleanup_target(
    target: CleanupTarget<'_>,
    critical_paths: &[PathBuf],
) -> Result<CleanupRecord> {
    validate_cleanup_path(target.path, critical_paths)?;
    let metadata = match fs::symlink_metadata(target.path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("cannot inspect cleanup path {}", target.path.display()));
        }
    };
    let existed = metadata.is_some();
    if let Some(metadata) = metadata {
        if metadata.file_type().is_symlink() || metadata.is_file() {
            fs::remove_file(target.path)
                .with_context(|| format!("cannot remove {}", target.path.display()))?;
        } else if metadata.is_dir() {
            let canonical = target.path.canonicalize().with_context(|| {
                format!("cannot resolve cleanup directory {}", target.path.display())
            })?;
            validate_cleanup_path(&canonical, critical_paths)?;
            fs::remove_dir_all(&canonical)
                .with_context(|| format!("cannot remove {}", canonical.display()))?;
        } else {
            bail!("unsupported cleanup target type: {}", target.path.display());
        }
        println!("removed {}: {}", target.kind, target.path.display());
    } else {
        println!("cleanup target already absent: {}", target.path.display());
    }
    Ok(CleanupRecord {
        kind: target.kind,
        path: target.path.display().to_string(),
        existed,
        removed: existed,
    })
}

fn target_arguments(config: &ProcessConfig) -> Vec<OsString> {
    let mut arguments = config.args.iter().map(OsString::from).collect::<Vec<_>>();
    if let Some(argument) = &config.rom_arg {
        arguments.push(OsString::from(argument));
    }
    arguments.push(config.rom_path.as_os_str().to_owned());
    arguments
}

/// A failed screenshot/window lookup must not leave a launched target behind.
struct TargetGuard(Child, Duration);

impl std::ops::Deref for TargetGuard {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl std::ops::DerefMut for TargetGuard {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for TargetGuard {
    fn drop(&mut self) {
        terminate_child(&mut self.0, self.1);
    }
}

fn spawn_target(
    config: &ProcessConfig,
    #[cfg(target_os = "linux")] renderdoc: Option<(&renderdoc::Prepared, &renderdoc::Config)>,
) -> Result<Child> {
    let mut command = Command::new(&config.executable);
    command.args(target_arguments(config));
    if let Some(directory) = &config.working_directory {
        command.current_dir(directory);
    }
    if config.clear_environment {
        command.env_clear();
    }
    command.envs(&config.environment);
    #[cfg(target_os = "linux")]
    if let Some((prepared, renderdoc_config)) = renderdoc {
        prepared.configure_command(&mut command, renderdoc_config, !config.clear_environment)?;
    }
    command.stdin(Stdio::null());
    if let Some(log_path) = &config.log_file {
        if let Some(parent) = log_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let log = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(log_path)
            .with_context(|| format!("cannot open {}", log_path.display()))?;
        command.stdout(Stdio::from(log.try_clone()?));
        command.stderr(Stdio::from(log));
    }
    command
        .spawn()
        .with_context(|| format!("cannot launch {}", config.executable.display()))
}

fn find_window(
    pid: u32,
    config: &CaptureConfig,
    display: &str,
    origin: Instant,
    timeout: Duration,
    child: &mut Child,
) -> Result<(u64, WindowGeometry)> {
    if let Some(window_id) = config.window_id {
        return Ok((window_id, window_geometry(window_id, display)?));
    }
    let deadline = origin + timeout;
    loop {
        if cancelled() {
            bail!("cancelled while waiting for target window");
        }
        if let Some(status) = child.try_wait().context("cannot query target status")? {
            bail!("target exited with {status} before its X11 window appeared");
        }
        let mut command = display_command("xdotool", display);
        command.args(["search", "--onlyvisible"]);
        if config.match_process_pid {
            command.args(["--pid", &pid.to_string()]);
        }
        if let Some(pattern) = &config.window_title_regex {
            command.args(["--name", pattern]);
        }
        let output = command.output().context("cannot run xdotool search")?;
        if output.status.success() {
            let candidates = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| line.trim().parse::<u64>().ok())
                .filter_map(|id| {
                    window_geometry(id, display)
                        .ok()
                        .map(|geometry| (id, geometry))
                })
                .collect::<Vec<_>>();
            if let Some(found) = candidates
                .into_iter()
                .max_by_key(|(_, geometry)| u64::from(geometry.width) * u64::from(geometry.height))
            {
                println!(
                    "window id={} size={}x{} discovered at {}ms",
                    found.0,
                    found.1.width,
                    found.1.height,
                    origin.elapsed().as_millis()
                );
                return Ok(found);
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "no matching X11 window appeared within {}ms",
                timeout.as_millis()
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn window_geometry(window_id: u64, display: &str) -> Result<WindowGeometry> {
    let output = display_command("xdotool", display)
        .args(["getwindowgeometry", "--shell", &window_id.to_string()])
        .output()
        .context("cannot query X11 window geometry")?;
    if !output.status.success() {
        bail!("xdotool cannot query window {window_id}");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let value = |name: &str| -> Result<u32> {
        text.lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .context("missing geometry field")?
            .parse()
            .with_context(|| format!("invalid {name} geometry"))
    };
    Ok(WindowGeometry {
        width: value("WIDTH")?,
        height: value("HEIGHT")?,
    })
}

fn display_geometry(display: &str) -> Result<WindowGeometry> {
    let output = display_command("xdotool", display)
        .arg("getdisplaygeometry")
        .output()
        .context("cannot query X11 display geometry")?;
    if !output.status.success() {
        bail!("xdotool getdisplaygeometry failed");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut values = text.split_whitespace();
    Ok(WindowGeometry {
        width: values.next().context("missing display width")?.parse()?,
        height: values.next().context("missing display height")?.parse()?,
    })
}

fn take_screenshot(window_id: Option<u64>, display: &str, output: &Path) -> Result<()> {
    let target = window_id.map_or_else(|| "root".to_owned(), |id| id.to_string());
    let status = display_command("import", display)
        .args(["-silent", "-window", &target])
        .arg(output)
        .status()
        .context("cannot launch ImageMagick import")?;
    if !status.success() {
        bail!("ImageMagick import exited with {status}");
    }
    Ok(())
}

fn send_input(window_id: u64, display: &str, keys: &[&str], pressed: bool) -> Result<()> {
    // `xdotool key* --window` uses XSendEvent for a specific target. GTK does not deliver those
    // synthetic events through EventControllerKey, so Reden never sees them. Activating the
    // emulation toplevel first lets the window manager assign real keyboard focus; the unqualified
    // keydown/keyup command then uses XTEST, which follows the same path as physical input.
    focus_input_window(window_id, display)?;

    let action = if pressed { "keydown" } else { "keyup" };
    let status = display_command("xdotool", display)
        .arg(action)
        .args(keys)
        .status()
        .with_context(|| format!("cannot run xdotool {action}"))?;
    if !status.success() {
        bail!("xdotool {action} exited with {status}");
    }
    Ok(())
}

fn neutralize_window_input(window_id: u64, display: &str) -> Result<()> {
    focus_input_window(window_id, display)?;
    release_automation_keys(display)
}

fn focus_input_window(window_id: u64, display: &str) -> Result<()> {
    let window_id = window_id.to_string();
    let status = display_command("xdotool", display)
        .args(["windowactivate", "--sync", &window_id])
        .status()
        .context("cannot run xdotool windowactivate")?;
    if !status.success() {
        bail!("xdotool windowactivate exited with {status}");
    }

    // Some minimal X11 window managers accept the EWMH activation request but
    // do not publish `_NET_ACTIVE_WINDOW`. XTEST then follows the old input
    // focus even though `windowactivate` returned success. Force the X input
    // focus onto the GTK toplevel so its capture-phase EventControllerKey sees
    // exactly the same events as physical keyboard input.
    let status = display_command("xdotool", display)
        .args(["windowfocus", "--sync", &window_id])
        .status()
        .context("cannot run xdotool windowfocus")?;
    if !status.success() {
        bail!("xdotool windowfocus exited with {status}");
    }
    Ok(())
}

// Union of the physical key names injected for Reden, Eden, and ruzu-cmd. This deliberately
// includes keys unused by the current profile: a stale key may come from a previous run using a
// different frontend and acquire a different meaning after the input configuration is replaced.
const AUTOMATION_KEYS: &[&str] = &[
    "a", "b", "c", "e", "f", "g", "h", "m", "n", "q", "r", "s", "t", "v", "w", "x", "z", "1", "2",
    "Left", "Up", "Right", "Down",
];

fn release_automation_keys(display: &str) -> Result<()> {
    let status = display_command("xdotool", display)
        .arg("keyup")
        .args(AUTOMATION_KEYS)
        .status()
        .context("cannot release capture-harness input keys")?;
    if !status.success() {
        bail!("xdotool keyup cleanup exited with {status}");
    }
    Ok(())
}

struct InputReleaseGuard {
    display: String,
}

impl InputReleaseGuard {
    fn install(display: &str) -> Result<Self> {
        release_automation_keys(display)?;
        Ok(Self {
            display: display.to_owned(),
        })
    }
}

impl Drop for InputReleaseGuard {
    fn drop(&mut self) {
        if let Err(error) = release_automation_keys(&self.display) {
            eprintln!("cannot release capture-harness input keys: {error:#}");
        }
    }
}

fn input_event_detail(event: &PreparedInputEvent) -> String {
    let buttons = event
        .buttons
        .iter()
        .map(|button| format!("{button:?}"))
        .collect::<Vec<_>>()
        .join("+");
    let description = event
        .label
        .as_ref()
        .map_or(buttons.clone(), |label| format!("{label} ({buttons})"));
    format!("{description}; X11 keys={}", event.keys.join("+"))
}

fn start_video(
    config: &VideoConfig,
    display: &str,
    window_id: Option<u64>,
    geometry: WindowGeometry,
    duration: Duration,
    output: &Path,
) -> Result<Child> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut command = display_command("ffmpeg", display);
    command.args(["-hide_banner", "-loglevel", "warning", "-y"]);
    command.args(["-f", "x11grab"]);
    command.args(["-draw_mouse", if config.include_cursor { "1" } else { "0" }]);
    command.args(["-framerate", &config.fps.to_string()]);
    command.args([
        "-video_size",
        &format!("{}x{}", geometry.width, geometry.height),
    ]);
    if let Some(window_id) = window_id {
        command.args(["-window_id", &window_id.to_string()]);
        command.args(["-i", display]);
    } else {
        command.args(["-i", &format!("{display}+0,0")]);
    }
    command.args(["-t", &format!("{:.6}", duration.as_secs_f64())]);
    command.args(["-c:v", &config.codec]);
    command.args(["-crf", &config.crf.to_string()]);
    // yuv420p encoders require even dimensions. Padding preserves the full
    // captured window instead of silently cropping its last row or column.
    command.args(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2"]);
    command.args(["-pix_fmt", "yuv420p"]);
    command.arg(output);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::null());
    command.stderr(Stdio::inherit());
    command.spawn().context("cannot launch ffmpeg")
}

fn stop_video(child: &mut Child) -> Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(b"q\n");
        let _ = stdin.flush();
    }
    Ok(())
}

fn wait_until(child: &mut Child, origin: Instant, target: Duration) -> Result<Option<ExitStatus>> {
    let deadline = origin + target;
    loop {
        if cancelled() {
            return Ok(None);
        }
        if let Some(status) = child.try_wait().context("cannot query target status")? {
            return Ok(Some(status));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        thread::sleep((deadline - now).min(Duration::from_millis(20)));
    }
}

fn terminate_child(child: &mut Child, grace: Duration) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn display_command(program: &str, display: &str) -> Command {
    let mut command = Command::new(program);
    command.env("DISPLAY", display);
    command
}

fn require_command(program: &str) -> Result<()> {
    let version_argument = match program {
        "ffmpeg" => "-version",
        _ => "--version",
    };
    let status = Command::new(program)
        .arg(version_argument)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("required command `{program}` is not installed"))?;
    if !status.success() {
        bail!("required command `{program}` is not usable");
    }
    Ok(())
}

fn screenshot_path(directory: &Path, prefix: &str, index: usize, at: Duration) -> PathBuf {
    directory.join(format!(
        "{prefix}_{index:03}_{}.png",
        format_timecode_filename(at)
    ))
}

fn video_output_path(config: &VideoConfig, output_directory: &Path) -> PathBuf {
    if config.output.is_absolute() {
        config.output.clone()
    } else {
        output_directory.join(&config.output)
    }
}

fn event_name(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::VideoStart => "video_start",
        EventKind::InputRelease { .. } => "input_release",
        EventKind::InputPress { .. } => "input_press",
        EventKind::Screenshot { .. } => "screenshot",
        EventKind::VideoStop => "video_stop",
        EventKind::TimelineStop => "timeline_stop",
    }
}

fn parse_timecode(value: &str) -> Result<Duration> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('-') {
        bail!("invalid timecode `{value}`");
    }
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() > 3 {
        bail!("invalid timecode `{value}`");
    }
    let seconds = parts
        .last()
        .context("missing seconds")?
        .parse::<f64>()
        .with_context(|| format!("invalid timecode `{value}`"))?;
    if !seconds.is_finite() || seconds < 0.0 || (parts.len() > 1 && seconds >= 60.0) {
        bail!("invalid seconds in timecode `{value}`");
    }
    let minutes = if parts.len() >= 2 {
        parts[parts.len() - 2]
            .parse::<u64>()
            .with_context(|| format!("invalid timecode `{value}`"))?
    } else {
        0
    };
    if parts.len() == 3 && minutes >= 60 {
        bail!("invalid minutes in timecode `{value}`");
    }
    let hours = if parts.len() == 3 {
        parts[0]
            .parse::<u64>()
            .with_context(|| format!("invalid timecode `{value}`"))?
    } else {
        0
    };
    let total = hours as f64 * 3600.0 + minutes as f64 * 60.0 + seconds;
    Duration::try_from_secs_f64(total).map_err(|_| anyhow!("timecode `{value}` is too large"))
}

fn format_timecode_filename(value: Duration) -> String {
    let total_ms = value.as_millis();
    let hours = total_ms / 3_600_000;
    let minutes = (total_ms / 60_000) % 60;
    let seconds = (total_ms / 1_000) % 60;
    let millis = total_ms % 1_000;
    format!("{hours:02}-{minutes:02}-{seconds:02}-{millis:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn raw_session_needs_no_x11_and_cannot_overwrite_its_own_recording() {
        let mut config: Config = toml::from_str(include_str!("../input-session.toml")).unwrap();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        config.process.rom_path = root.join("Cargo.toml");
        let session_file =
            std::env::temp_dir().join(format!("capture-session-test-{}.json", std::process::id()));
        config.session.as_mut().unwrap().file = session_file.clone();
        let mut prepared = PreparedRun::new(config, root.join("input-session.toml")).unwrap();
        assert!(!prepared.needs_x11);
        assert_eq!(
            prepared.events().last().unwrap().at,
            Duration::from_secs(300)
        );
        prepared.config.process.log_file = Some(session_file);
        assert!(prepared.validate_session_output_paths().is_err());
        prepared.config.process.log_file = None;
        prepared.config.session.as_mut().unwrap().file = prepared
            .config
            .capture
            .output_directory
            .join("capture-manifest.json");
        assert!(prepared.validate_session_output_paths().is_err());
    }

    fn test_process_config(rom_path: PathBuf) -> ProcessConfig {
        ProcessConfig {
            executable: PathBuf::from("emulator"),
            rom_path,
            rom_arg: Some("-g".to_owned()),
            args: vec!["--renderer".to_owned(), "vulkan".to_owned()],
            working_directory: None,
            environment: BTreeMap::new(),
            clear_environment: false,
            log_file: None,
            stop_at: None,
            terminate_after_timeline: true,
            termination_grace_ms: default_termination_grace_ms(),
        }
    }

    #[test]
    fn parses_supported_timecode_forms() {
        assert_eq!(
            parse_timecode("12.5").unwrap(),
            Duration::from_millis(12_500)
        );
        assert_eq!(
            parse_timecode("01:02.250").unwrap(),
            Duration::from_millis(62_250)
        );
        assert_eq!(
            parse_timecode("02:03:04.005").unwrap(),
            Duration::from_millis(7_384_005)
        );
    }

    #[test]
    fn rejects_ambiguous_or_invalid_timecodes() {
        assert!(parse_timecode("").is_err());
        assert!(parse_timecode("-1").is_err());
        assert!(parse_timecode("00:60").is_err());
        assert!(parse_timecode("00:60:00").is_err());
        assert!(parse_timecode("1:2:3:4").is_err());
    }

    #[test]
    fn formats_filenames_without_punctuation() {
        assert_eq!(
            format_timecode_filename(Duration::from_millis(3_723_045)),
            "01-02-03-045"
        );
    }

    #[test]
    fn event_order_is_stable_at_equal_timecodes() {
        let mut events = [
            Event {
                at: Duration::from_secs(1),
                priority: 4,
                kind: EventKind::VideoStop,
            },
            Event {
                at: Duration::from_secs(1),
                priority: 3,
                kind: EventKind::Screenshot { index: 0 },
            },
            Event {
                at: Duration::from_secs(1),
                priority: 0,
                kind: EventKind::VideoStart,
            },
        ];
        events.sort_by(|left, right| {
            left.at
                .cmp(&right.at)
                .then_with(|| left.priority.cmp(&right.priority))
        });
        assert!(matches!(events[0].kind, EventKind::VideoStart));
        assert!(matches!(events[1].kind, EventKind::Screenshot { .. }));
        assert!(matches!(events[2].kind, EventKind::VideoStop));
    }

    #[test]
    fn switch_buttons_use_identical_physical_keys_across_frontends() {
        assert_eq!(SwitchButton::A.x_key(InputFrontend::Reden), "c");
        assert_eq!(SwitchButton::L.x_key(InputFrontend::Reden), "q");
        assert_eq!(SwitchButton::R.x_key(InputFrontend::Reden), "e");
        assert_eq!(SwitchButton::A.x_key(InputFrontend::Eden), "c");
        assert_eq!(SwitchButton::L.x_key(InputFrontend::Eden), "q");
        assert_eq!(SwitchButton::R.x_key(InputFrontend::Eden), "e");
        assert_eq!(SwitchButton::A.x_key(InputFrontend::RuzuCmd), "c");
        assert_eq!(SwitchButton::L.x_key(InputFrontend::RuzuCmd), "q");
        assert_eq!(SwitchButton::R.x_key(InputFrontend::RuzuCmd), "e");
    }

    #[test]
    fn frontend_selects_its_own_input_config_file() {
        let mut process = test_process_config(PathBuf::from("/games/reference.nsp"));
        process.clear_environment = true;
        process.environment.insert(
            "HOME".to_owned(),
            "/capture-harness-test/no-home".to_owned(),
        );
        process.environment.insert(
            "XDG_CONFIG_HOME".to_owned(),
            "/capture-harness-test/config".to_owned(),
        );
        process.environment.insert(
            "XDG_DATA_HOME".to_owned(),
            "/capture-harness-test/no-data".to_owned(),
        );
        let input = InputConfig {
            config_file: None,
            default_hold_ms: default_input_hold_ms(),
            restore_config: true,
            events: Vec::new(),
        };

        assert_eq!(
            input_frontend(Path::new("/opt/reden")).unwrap(),
            InputFrontend::Reden
        );
        assert_eq!(
            input_frontend(Path::new("/opt/ruzu-cmd")).unwrap(),
            InputFrontend::RuzuCmd
        );
        assert_eq!(
            input_frontend(Path::new("/opt/eden")).unwrap(),
            InputFrontend::Eden
        );
        assert_eq!(
            input_config_path(&process, &input, InputFrontend::Reden).unwrap(),
            Path::new("/capture-harness-test/config/ruzu/qt-config.ini")
        );
        assert_eq!(
            input_config_path(&process, &input, InputFrontend::RuzuCmd).unwrap(),
            Path::new("/capture-harness-test/config/ruzu/sdl2-config.ini")
        );
        assert_eq!(
            input_config_path(&process, &input, InputFrontend::Eden).unwrap(),
            Path::new("/capture-harness-test/config/eden/qt-config.ini")
        );
    }

    #[test]
    fn keyboard_mouse_config_uses_qt_codes_for_reden_and_sdl_scancodes_for_cmd() {
        let original = "[UI]\nplayer_0_button_a=decoy\n[Controls]\nplayer_0_button_a=old\nplayer_0_button_l=old\nplayer_0_button_r=old\n[Core]\nuse_multi_core=true\n";
        let reden = keyboard_mouse_config(original, InputFrontend::Reden);
        assert!(reden.contains("[UI]\nplayer_0_button_a=decoy"));
        assert!(reden.contains("player_0_button_a=\"engine:keyboard,code:67,toggle:0\""));
        assert!(reden.contains("player_0_button_l=\"engine:keyboard,code:81,toggle:0\""));
        assert!(reden.contains("player_0_button_r=\"engine:keyboard,code:69,toggle:0\""));
        assert!(reden.contains("player_0_rstick=\"engine:mouse"));
        assert!(reden.contains("[Core]\nuse_multi_core=true"));

        let cmd = keyboard_mouse_config(original, InputFrontend::RuzuCmd);
        assert!(cmd.contains("player_0_button_a=\"engine:keyboard,code:6,toggle:0\""));
        assert!(cmd.contains("player_0_button_l=\"engine:keyboard,code:20,toggle:0\""));
        assert!(cmd.contains("player_0_button_r=\"engine:keyboard,code:8,toggle:0\""));

        let eden = keyboard_mouse_config(original, InputFrontend::Eden);
        assert!(eden.contains("player_0_button_a=\"engine:keyboard,code:67,toggle:0\""));
        assert!(eden.contains("player_0_button_l=\"engine:keyboard,code:81,toggle:0\""));
        assert!(eden.contains("player_0_button_r=\"engine:keyboard,code:69,toggle:0\""));
    }

    #[test]
    fn keyboard_config_guard_restores_the_original_file() {
        let root = std::env::temp_dir().join(format!(
            "capture-harness-input-config-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("qt-config.ini");
        let original = b"[Controls]\nplayer_0_button_a=original\n";
        fs::write(&path, original).unwrap();
        {
            let _guard = KeyboardConfigGuard::install(&PreparedInputConfig {
                path: path.clone(),
                frontend: InputFrontend::Reden,
                restore: true,
            })
            .unwrap();
            let active = fs::read_to_string(&path).unwrap();
            assert!(active.contains("player_0_button_a=\"engine:keyboard,code:67,toggle:0\""));
        }
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::remove_file(path).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn cleanup_covers_every_frontend_button_key() {
        let buttons = [
            SwitchButton::A,
            SwitchButton::B,
            SwitchButton::X,
            SwitchButton::Y,
            SwitchButton::LStick,
            SwitchButton::RStick,
            SwitchButton::L,
            SwitchButton::R,
            SwitchButton::ZL,
            SwitchButton::ZR,
            SwitchButton::Plus,
            SwitchButton::Minus,
            SwitchButton::DLeft,
            SwitchButton::DUp,
            SwitchButton::DRight,
            SwitchButton::DDown,
        ];
        for frontend in [
            InputFrontend::Reden,
            InputFrontend::Eden,
            InputFrontend::RuzuCmd,
        ] {
            for button in buttons {
                assert!(
                    AUTOMATION_KEYS.contains(&button.x_key(frontend)),
                    "cleanup misses {frontend:?} {button:?}"
                );
            }
        }
    }

    #[test]
    fn example_homebrew_profile_has_lr_then_a_at_the_requested_timecodes() {
        let config: Config = toml::from_str(include_str!("../example.toml")).unwrap();
        let events = config.input.unwrap().events;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].at, "00:00:12.000");
        assert_eq!(events[0].buttons, [SwitchButton::L, SwitchButton::R]);
        assert_eq!(events[1].buttons, [SwitchButton::A]);

        let times = events
            .iter()
            .map(|event| parse_timecode(&event.at).unwrap().as_millis())
            .collect::<Vec<_>>();
        assert_eq!(times, [12_000, 14_000]);
    }

    #[test]
    fn appends_rom_argument_and_path_after_regular_arguments() {
        let config = test_process_config(PathBuf::from("/games/reference.nsp"));
        assert_eq!(
            target_arguments(&config),
            ["--renderer", "vulkan", "-g", "/games/reference.nsp"].map(OsString::from)
        );
    }

    #[test]
    fn cleanup_removes_only_the_explicit_target() {
        let root = std::env::temp_dir().join(format!(
            "capture-harness-cleanup-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        ));
        let target = root.join("shader-cache");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("entry.bin"), b"cache").unwrap();
        let record = remove_cleanup_target(
            CleanupTarget {
                kind: "shader_cache",
                path: &target,
            },
            &[],
        )
        .unwrap();
        assert!(record.existed);
        assert!(record.removed);
        assert!(!target.exists());
        assert!(root.exists());
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn cleanup_rejects_broad_and_protected_paths() {
        assert!(validate_cleanup_path(Path::new("/"), &[]).is_err());
        let protected = PathBuf::from("/tmp/reference/game.nsp");
        assert!(validate_cleanup_path(Path::new("/tmp/reference"), &[protected]).is_err());
    }
}

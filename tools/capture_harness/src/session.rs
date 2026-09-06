// SPDX-License-Identifier: GPL-3.0-or-later
//! Versioned raw input sessions. This diagnostic tool has no Eden counterpart.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

pub const MAX_EVENTS: usize = 2_000_000;
pub const MAX_DURATION_US: u64 = 3_600_000_000;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Record,
    Replay,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub mode: Mode,
    pub file: PathBuf,
    /// Mandatory, explicitly selected evdev node for recording; never auto-select a keyboard.
    pub device: Option<PathBuf>,
    /// Recording limit. Replay uses the duration stored in the session.
    pub duration: Option<String>,
}

impl Config {
    pub fn validate(&self) -> Result<Duration> {
        match self.mode {
            Mode::Record => {
                ensure!(
                    self.device.is_some(),
                    "session.device is required for recording"
                );
                ensure!(
                    !self.file.exists(),
                    "session file already exists: {}",
                    self.file.display()
                );
                let duration = crate::parse_timecode(
                    self.duration
                        .as_deref()
                        .context("session.duration is required for recording")?,
                )?;
                ensure!(
                    !duration.is_zero() && duration.as_micros() <= MAX_DURATION_US as u128,
                    "session.duration must be between 0 and 3600 seconds"
                );
                Ok(duration)
            }
            Mode::Replay => {
                ensure!(
                    self.device.is_none() && self.duration.is_none(),
                    "replay uses its recorded device profile and duration; omit device/duration"
                );
                Ok(Duration::from_micros(
                    Session::load(&self.file)?.duration_us,
                ))
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Axis {
    pub code: u16,
    pub value: i32,
    pub minimum: i32,
    pub maximum: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceProfile {
    pub name: String,
    /// bus, vendor, product, version; preserved for SDL mapping, not a hardcoded controller.
    pub id: [u16; 4],
    pub keys: Vec<u16>,
    pub relative_axes: Vec<u16>,
    pub absolute_axes: Vec<Axis>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Value {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub at_us: u64,
    /// One original SYN_REPORT packet; SYN_REPORT itself is implicit.
    pub events: Vec<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub version: u32,
    pub clock: String,
    pub device: DeviceProfile,
    pub duration_us: u64,
    /// False after event loss/disconnection/read failure; such sessions cannot be replayed.
    pub valid: bool,
    pub end_reason: String,
    pub frames: Vec<Frame>,
}

impl Session {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let mut bytes = Vec::new();
        File::open(path)
            .with_context(|| format!("cannot read session {}", path.display()))?
            .take(128 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 128 * 1024 * 1024, "session exceeds 128 MiB");
        let session: Self = serde_json::from_slice(&bytes)?;
        session.validate()?;
        Ok(session)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1 && self.clock == "monotonic_microseconds",
            "unsupported session version/clock"
        );
        ensure!(self.valid, "incomplete input stream: {}", self.end_reason);
        ensure!(
            self.duration_us > 0 && self.duration_us <= MAX_DURATION_US,
            "invalid session duration"
        );
        ensure!(
            !self.device.name.is_empty()
                && self.device.name.len() < 80
                && !self.device.name.contains('\0'),
            "invalid device name"
        );
        let mut keys = std::collections::BTreeSet::new();
        for &key in &self.device.keys {
            ensure!(
                key <= 0x2ff && keys.insert(key),
                "invalid/duplicate key capability"
            );
        }
        let mut axes = std::collections::BTreeSet::new();
        for axis in &self.device.absolute_axes {
            ensure!(
                axis.code <= 0x3f && axes.insert(axis.code),
                "invalid/duplicate absolute axis"
            );
            ensure!(
                axis.minimum <= axis.value
                    && axis.value <= axis.maximum
                    && axis.minimum < axis.maximum,
                "invalid absolute range/state"
            );
            ensure!(
                axis.fuzz >= 0 && axis.flat >= 0 && axis.resolution >= 0,
                "invalid absolute metadata"
            );
        }
        let mut relative = std::collections::BTreeSet::new();
        for &axis in &self.device.relative_axes {
            ensure!(
                axis <= 0x0f && relative.insert(axis),
                "invalid/duplicate relative axis"
            );
        }
        let mut previous = 0;
        let mut count = 0;
        for frame in &self.frames {
            ensure!(
                frame.at_us >= previous && frame.at_us <= self.duration_us,
                "unordered/out-of-range input timestamp"
            );
            previous = frame.at_us;
            count += frame.events.len();
            ensure!(
                count <= MAX_EVENTS && !frame.events.is_empty(),
                "too many events or empty frame"
            );
            for event in &frame.events {
                match event.kind {
                    1 => ensure!(
                        keys.contains(&event.code) && (0..=2).contains(&event.value),
                        "invalid key event"
                    ),
                    2 => ensure!(relative.contains(&event.code), "undeclared relative axis"),
                    3 => {
                        let axis = self
                            .device
                            .absolute_axes
                            .iter()
                            .find(|axis| axis.code == event.code)
                            .context("undeclared absolute axis")?;
                        ensure!(
                            (axis.minimum..=axis.maximum).contains(&event.value),
                            "absolute value outside advertised range"
                        );
                    }
                    _ => bail!("unsupported input event type {}", event.kind),
                }
            }
        }
        Ok(())
    }
}

/// Shared cancellation only; screenshot work must never delay input releases.
pub fn wait_until(origin: Instant, at: Duration, stop: &AtomicBool) -> bool {
    while !stop.load(Ordering::Acquire) && !crate::cancelled() {
        let remaining = at.saturating_sub(origin.elapsed());
        if remaining.is_zero() {
            return true;
        }
        thread::sleep(remaining.min(Duration::from_millis(5)));
    }
    false
}

pub struct Worker {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<Result<()>>>,
}

impl Worker {
    pub fn spawn(
        stop: Arc<AtomicBool>,
        work: impl FnOnce() -> Result<()> + Send + 'static,
    ) -> Result<Self> {
        let failure_stop = stop.clone();
        let join = thread::Builder::new()
            .name("capture-input".into())
            .spawn(move || {
                let result = work();
                if let Err(error) = &result {
                    eprintln!("input session failed: {error:#}");
                    failure_stop.store(true, Ordering::Release);
                    crate::CANCELLED.store(true, Ordering::Release);
                }
                result
            })?;
        Ok(Self {
            stop,
            join: Some(join),
        })
    }

    pub fn finish(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        self.complete()
    }

    /// Natural timeline end: let an event exactly on the boundary finish before joining.
    pub fn complete(&mut self) -> Result<()> {
        if let Some(join) = self.join.take() {
            join.join()
                .map_err(|_| anyhow::anyhow!("input worker panicked"))??;
        }
        Ok(())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            eprintln!("cannot finish input session: {error:#}");
        }
    }
}

pub fn new_output(path: &std::path::Path) -> Result<File> {
    // create_new is deliberate: rerunning a config must not destroy a recorded sequence.
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).with_context(|| {
        format!(
            "cannot create {} (parent must exist; file must not)",
            path.display()
        )
    })
}

pub fn save(mut file: File, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Session {
        Session {
            version: 1,
            clock: "monotonic_microseconds".into(),
            device: DeviceProfile {
                name: "Synthetic controller".into(),
                id: [3, 1, 2, 1],
                keys: vec![304],
                relative_axes: vec![],
                absolute_axes: vec![Axis {
                    code: 0,
                    value: 0,
                    minimum: -32768,
                    maximum: 32767,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                }],
            },
            duration_us: 100_000,
            valid: true,
            end_reason: "duration".into(),
            frames: vec![
                Frame {
                    at_us: 14_000,
                    events: vec![
                        Value {
                            kind: 1,
                            code: 304,
                            value: 1,
                        },
                        Value {
                            kind: 3,
                            code: 0,
                            value: -123,
                        },
                    ],
                },
                Frame {
                    at_us: 17_000,
                    events: vec![Value {
                        kind: 1,
                        code: 304,
                        value: 0,
                    }],
                },
            ],
        }
    }

    #[test]
    fn roundtrip_keeps_packets_press_release_axes_and_microseconds() {
        let input = sample();
        let result: Session = serde_json::from_slice(&serde_json::to_vec(&input).unwrap()).unwrap();
        result.validate().unwrap();
        assert_eq!(result.frames[0].at_us, 14_000);
        assert_eq!(result.frames[1].at_us, 17_000);
        assert_eq!(result.frames[0].events, input.frames[0].events);
        assert_eq!(result.frames[1].events[0].value, 0);
    }

    #[test]
    fn rejects_loss_unordered_undeclared_and_invalid_range() {
        let mut input = sample();
        input.valid = false;
        assert!(input.validate().is_err());
        input = sample();
        input.frames[1].at_us = 1;
        assert!(input.validate().is_err());
        input = sample();
        input.frames[0].events[0].code = 99;
        assert!(input.validate().is_err());
        input = sample();
        input.frames[0].events[1].value = 32768;
        assert!(input.validate().is_err());
        input = sample();
        input.version = 2;
        assert!(input.validate().is_err());
    }

    #[test]
    fn cancellation_does_not_wait_for_next_event() {
        let stop = AtomicBool::new(true);
        assert!(!wait_until(Instant::now(), Duration::from_secs(600), &stop));
    }

    #[test]
    fn natural_completion_does_not_cancel_the_last_event() {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let mut worker = Worker::spawn(stop, move || {
            ensure!(
                wait_until(Instant::now(), Duration::from_millis(5), &worker_stop),
                "last event cancelled"
            );
            Ok(())
        })
        .unwrap();
        worker.complete().unwrap();
    }
}

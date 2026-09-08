// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux evdev recording and uinput replay; no emulator configuration is rewritten.

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{ensure, Context, Result};
use evdev::{
    raw_stream::RawDevice, uinput::VirtualDevice, AbsInfo, AbsoluteAxisCode, AttributeSet, BusType,
    InputEvent, InputId, KeyCode, RelativeAxisCode, UinputAbsSetup,
};
use serde::Serialize;

use crate::session::{self, Axis, DeviceProfile, Frame, Mode, Session, Value, Worker};

// Generate the architecture-specific ioctl number from the Linux UAPI, not an x86 literal.
nix::ioctl_write_ptr!(set_event_clock, b'E', 0xa0, libc::c_int);

pub fn monotonic_us() -> Result<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: time is a writable timespec and CLOCK_MONOTONIC is supported by Linux.
    ensure!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } == 0,
        "clock_gettime failed: {}",
        std::io::Error::last_os_error()
    );
    Ok(time.tv_sec as u64 * 1_000_000 + time.tv_nsec as u64 / 1_000)
}

pub fn list_devices() -> Result<()> {
    let mut paths = std::fs::read_dir("/dev/input")?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("event"))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        match open_record_device(&path) {
            Ok(device) => println!(
                "{}\t{}\t{:?}",
                path.display(),
                device.name().unwrap_or("unnamed"),
                device.input_id()
            ),
            Err(error) => println!("{}\t{error:#}", path.display()),
        }
    }
    Ok(())
}

fn open_record_device(path: &std::path::Path) -> Result<RawDevice> {
    // Read-only and no EVIOCGRAB: normal gameplay still receives the physical input.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .with_context(|| {
            format!(
                "cannot read {}; grant access to this device, not all keyboards",
                path.display()
            )
        })?;
    Ok(RawDevice::from_fd(file.into())?)
}

pub enum Prepared {
    Record {
        device: RawDevice,
        session: Session,
        output: File,
        duration: Duration,
    },
    Replay {
        device: ReplayDevice,
        session: Session,
        report: File,
    },
}

impl Prepared {
    pub fn new(config: &session::Config, report_path: &std::path::Path) -> Result<Self> {
        let duration = config.validate()?;
        match config.mode {
            Mode::Record => {
                let device = open_record_device(config.device.as_ref().expect("validated device"))?;
                let clock = libc::CLOCK_MONOTONIC;
                // SAFETY: evdev fd and pointer to the integer clock ID remain valid for this ioctl.
                unsafe { set_event_clock(device.as_raw_fd(), &clock) }
                    .context("cannot select monotonic evdev timestamps")?;
                ensure!(
                    device.get_key_state()?.iter().next().is_none(),
                    "release all device buttons before starting recording"
                );
                let id = device.input_id();
                let profile = DeviceProfile {
                    name: device
                        .name()
                        .context("input device has no name")?
                        .to_owned(),
                    id: [id.bus_type().0, id.vendor(), id.product(), id.version()],
                    keys: device
                        .supported_keys()
                        .map(|set| set.iter().map(|code| code.0).collect())
                        .unwrap_or_default(),
                    relative_axes: device
                        .supported_relative_axes()
                        .map(|set| set.iter().map(|code| code.0).collect())
                        .unwrap_or_default(),
                    absolute_axes: device
                        .get_absinfo()?
                        .map(|(code, info)| Axis {
                            code: code.0,
                            value: info.value(),
                            minimum: info.minimum(),
                            maximum: info.maximum(),
                            fuzz: info.fuzz(),
                            flat: info.flat(),
                            resolution: info.resolution(),
                        })
                        .collect(),
                };
                // Multitouch slots require state reconstruction, not gamepad axis replay.
                ensure!(
                    profile.absolute_axes.iter().all(|axis| axis.code < 0x2f),
                    "multitouch devices are not supported; select a keyboard/gamepad"
                );
                ensure!(
                    !profile.keys.is_empty()
                        || !profile.absolute_axes.is_empty()
                        || !profile.relative_axes.is_empty(),
                    "device has no supported inputs"
                );
                let session = Session {
                    version: 1,
                    clock: "monotonic_microseconds".into(),
                    device: profile,
                    duration_us: duration.as_micros() as u64,
                    valid: true,
                    end_reason: "duration".into(),
                    frames: vec![],
                };
                session.validate()?;
                let output = session::new_output(&config.file)?;
                eprintln!("RECORDING selected device {} ({}) into {}. Keyboard recordings may contain private text. Stop with Ctrl-C.",
                    config.device.as_ref().unwrap().display(), session.device.name, config.file.display());
                Ok(Self::Record {
                    device,
                    session,
                    output,
                    duration,
                })
            }
            Mode::Replay => {
                let session = Session::load(&config.file)?;
                let report = session::new_output(report_path)?;
                let device = ReplayDevice::new(&session.device)
                    .context("cannot create virtual input; check /dev/uinput access")?;
                eprintln!("Virtual input ready: {}. Select it in the emulator; the physical device is not grabbed or disabled.", session.device.name);
                // Let udev/SDL observe creation before launching the target. Not part of the input timeline.
                std::thread::sleep(Duration::from_millis(300));
                Ok(Self::Replay {
                    device,
                    session,
                    report,
                })
            }
        }
    }

    pub fn start(self, origin: Instant, stop: Arc<AtomicBool>) -> Result<Worker> {
        // Convert the shared Instant origin to the same CLOCK_MONOTONIC used by evdev.
        let kernel_origin = monotonic_us()?.saturating_sub(origin.elapsed().as_micros() as u64);
        let worker_stop = stop.clone();
        Worker::spawn(stop, move || match self {
            Self::Record {
                device,
                session,
                output,
                duration,
            } => record(
                device,
                session,
                output,
                origin,
                kernel_origin,
                duration,
                &worker_stop,
            ),
            Self::Replay {
                device,
                session,
                report,
            } => replay(device, session, report, origin, &worker_stop),
        })
    }
}

fn record(
    mut device: RawDevice,
    mut session: Session,
    output: File,
    origin: Instant,
    kernel_origin: u64,
    duration: Duration,
    stop: &AtomicBool,
) -> Result<()> {
    let mut packet = Vec::new();
    let mut count = 0;
    let result = (|| -> Result<()> {
        loop {
            match device.fetch_events() {
                Ok(events) => {
                    for event in events {
                        let stamp =
                            event.timestamp().duration_since(UNIX_EPOCH)?.as_micros() as u64;
                        // RawDevice deliberately does NOT synthesize recovery after SYN_DROPPED.
                        ensure!(!(event.event_type().0 == 0 && event.code() == 3), "SYN_DROPPED: kernel input queue overflowed; recording cannot be replayed safely");
                        if stamp < kernel_origin {
                            // Do not silently lose a press between the initial snapshot and spawn.
                            ensure!(!(1..=3).contains(&event.event_type().0), "input changed before launch; keep the device idle until the timer starts and retry");
                            continue;
                        }
                        let at_us = stamp - kernel_origin;
                        if at_us > duration.as_micros() as u64 {
                            continue;
                        }
                        match event.event_type().0 {
                            1..=3 => {
                                count += 1;
                                ensure!(count <= session::MAX_EVENTS, "input event limit exceeded");
                                packet.push(Value {
                                    kind: event.event_type().0,
                                    code: event.code(),
                                    value: event.value(),
                                });
                            }
                            0 if event.code() == 0 && !packet.is_empty() => {
                                session.frames.push(Frame {
                                    at_us,
                                    events: std::mem::take(&mut packet),
                                });
                            }
                            _ => {} // MSC_SCAN/LED/FF are metadata/output, not keyboard/gamepad input.
                        }
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
            // Drain once at the end as well, including a release queued just before the deadline.
            if origin.elapsed() >= duration || stop.load(Ordering::Acquire) || crate::cancelled() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        ensure!(packet.is_empty(), "recording ended inside an input packet");
        Ok(())
    })();
    session.duration_us = (origin.elapsed().min(duration).as_micros() as u64).max(1);
    session.end_reason = if origin.elapsed() >= duration {
        "duration"
    } else {
        "target exit or cancellation"
    }
    .into();
    if let Err(error) = &result {
        session.valid = false;
        session.end_reason = format!("{error:#}");
    }
    if let Err(error) = session.validate() {
        session.valid = false;
        session.end_reason = format!("{error:#}");
    }
    session::save(output, &session)?;
    result?;
    ensure!(
        session.valid,
        "recorded stream is invalid: {}",
        session.end_reason
    );
    println!(
        "recorded {} input packets, duration {} ms",
        session.frames.len(),
        session.duration_us / 1000
    );
    Ok(())
}

pub struct ReplayDevice {
    device: VirtualDevice,
    profile: DeviceProfile,
}

impl ReplayDevice {
    fn new(profile: &DeviceProfile) -> Result<Self> {
        let keys: AttributeSet<KeyCode> = profile.keys.iter().map(|&code| KeyCode(code)).collect();
        let relative: AttributeSet<RelativeAxisCode> = profile
            .relative_axes
            .iter()
            .map(|&code| RelativeAxisCode(code))
            .collect();
        let [bus, vendor, product, version] = profile.id;
        let mut builder = VirtualDevice::builder()?
            .name(&profile.name)
            .input_id(InputId::new(BusType(bus), vendor, product, version));
        if !profile.keys.is_empty() {
            builder = builder.with_keys(&keys)?;
        }
        if !profile.relative_axes.is_empty() {
            builder = builder.with_relative_axes(&relative)?;
        }
        for axis in &profile.absolute_axes {
            builder = builder.with_absolute_axis(&UinputAbsSetup::new(
                AbsoluteAxisCode(axis.code),
                AbsInfo::new(
                    axis.value,
                    axis.minimum,
                    axis.maximum,
                    axis.fuzz,
                    axis.flat,
                    axis.resolution,
                ),
            ))?;
        }
        Ok(Self {
            device: builder.build()?,
            profile: profile.clone(),
        })
    }

    fn emit(&mut self, events: &[Value]) -> Result<()> {
        self.device.emit(
            &events
                .iter()
                .map(|value| InputEvent::new(value.kind, value.code, value.value))
                .collect::<Vec<_>>(),
        )?;
        Ok(())
    }

    fn release(&mut self) -> Result<()> {
        self.emit(&release_values(&self.profile))
    }
}

fn release_values(profile: &DeviceProfile) -> Vec<Value> {
    // All keys belong to this virtual device, never the user's physical keyboard. Releasing
    // every declared key also handles a partially written packet without guessing host state.
    let mut values = profile
        .keys
        .iter()
        .map(|&code| Value {
            kind: 1,
            code,
            value: 0,
        })
        .collect::<Vec<_>>();
    values.extend(profile.absolute_axes.iter().map(|axis| Value {
        kind: 3,
        code: axis.code,
        value: axis.value,
    }));
    values
}

impl Drop for ReplayDevice {
    fn drop(&mut self) {
        if let Err(error) = self.release() {
            eprintln!("virtual input cleanup: {error:#}");
        }
        // Closing the final uinput fd destroys the virtual device even after a failed release.
    }
}

#[derive(Serialize)]
struct ReplayTiming {
    scheduled_us: u64,
    actual_us: u64,
    lateness_us: i128,
}

fn replay(
    mut device: ReplayDevice,
    session: Session,
    report: File,
    origin: Instant,
    stop: &AtomicBool,
) -> Result<()> {
    let mut timings = Vec::new();
    let result = replay_frames(&session, origin, stop, &mut timings, |events| {
        device.emit(events)
    });
    let released = device.release();
    session::save(
        report,
        &serde_json::json!({ "version": 1, "clock": "monotonic_microseconds",
        "completed": result.as_ref().is_ok_and(|completed| *completed), "released": released.is_ok(),
        "error": result.as_ref().err().map(|e| format!("{e:#}")), "frames": timings }),
    )?;
    result?;
    released
}

fn replay_frames(
    session: &Session,
    origin: Instant,
    stop: &AtomicBool,
    timings: &mut Vec<ReplayTiming>,
    mut emit: impl FnMut(&[Value]) -> Result<()>,
) -> Result<bool> {
    for frame in &session.frames {
        if !session::wait_until(origin, Duration::from_micros(frame.at_us), stop) {
            return Ok(false);
        }
        let actual_us = origin.elapsed().as_micros() as u64;
        emit(&frame.events)?;
        timings.push(ReplayTiming {
            scheduled_us: frame.at_us,
            actual_us,
            lateness_us: actual_us as i128 - frame.at_us as i128,
        });
    }
    Ok(session::wait_until(
        origin,
        Duration::from_micros(session.duration_us),
        stop,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_delivers_packets_then_releases_only_virtual_capabilities() {
        let profile = DeviceProfile {
            name: "Synthetic pad".into(),
            id: [3, 1, 1, 1],
            keys: vec![304],
            relative_axes: vec![],
            absolute_axes: vec![Axis {
                code: 0,
                value: 0,
                minimum: -10,
                maximum: 10,
                fuzz: 0,
                flat: 0,
                resolution: 0,
            }],
        };
        let input = Session {
            version: 1,
            clock: "monotonic_microseconds".into(),
            device: profile,
            duration_us: 2,
            valid: true,
            end_reason: "duration".into(),
            frames: vec![
                Frame {
                    at_us: 0,
                    events: vec![
                        Value {
                            kind: 1,
                            code: 304,
                            value: 1,
                        },
                        Value {
                            kind: 3,
                            code: 0,
                            value: -5,
                        },
                    ],
                },
                Frame {
                    at_us: 2,
                    events: vec![Value {
                        kind: 1,
                        code: 304,
                        value: 0,
                    }],
                },
            ],
        };
        let mut packets = Vec::new();
        let mut timings = Vec::new();
        let stop = AtomicBool::new(false);
        assert!(
            replay_frames(&input, Instant::now(), &stop, &mut timings, |packet| {
                packets.push(packet.to_vec());
                Ok(())
            })
            .unwrap()
        );
        assert_eq!(packets[0], input.frames[0].events);
        assert_eq!(packets[1], input.frames[1].events);
        assert_eq!(
            timings
                .iter()
                .map(|time| time.scheduled_us)
                .collect::<Vec<_>>(),
            [0, 2]
        );
        assert_eq!(
            release_values(&input.device),
            [
                Value {
                    kind: 1,
                    code: 304,
                    value: 0
                },
                Value {
                    kind: 3,
                    code: 0,
                    value: 0
                }
            ]
        );
        stop.store(true, Ordering::Release);
        assert!(
            !replay_frames(&input, Instant::now(), &stop, &mut Vec::new(), |_| panic!(
                "cancelled replay injected input"
            ))
            .unwrap()
        );
        stop.store(false, Ordering::Release);
        assert!(replay_frames(
            &input,
            Instant::now(),
            &stop,
            &mut Vec::new(),
            |_| anyhow::bail!("synthetic output error")
        )
        .is_err());
    }
}

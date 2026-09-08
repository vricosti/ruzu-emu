// SPDX-License-Identifier: GPL-3.0-or-later
//! Optional RenderDoc application-API bridge, separate from input injection.

use anyhow::{ensure, Result};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub library: PathBuf,
    pub helper: PathBuf,
    /// Optional SDK manifest; copied with a corrected library path for this run only.
    pub vulkan_layer_manifest: Option<PathBuf>,
    pub capture_prefix: PathBuf,
    pub times: Vec<String>,
    #[serde(default = "one")]
    pub frames: u32,
    #[serde(default = "timeout")]
    pub timeout: String,
}
fn one() -> u32 {
    1
}
fn timeout() -> String {
    "30".into()
}

impl Config {
    pub fn schedule(&self) -> Result<Vec<Duration>> {
        ensure!(!self.times.is_empty(), "renderdoc.times is empty");
        ensure!(
            (1..=16).contains(&self.frames),
            "renderdoc.frames must be 1..16"
        );
        let timeout = crate::parse_timecode(&self.timeout)?;
        ensure!(
            !timeout.is_zero() && timeout <= Duration::from_secs(300),
            "RenderDoc timeout must be 0..300 seconds"
        );
        for path in [&self.library, &self.helper] {
            ensure!(
                path.is_file(),
                "RenderDoc library/helper is missing: {}",
                path.display()
            );
            ensure!(
                !path
                    .as_os_str()
                    .to_string_lossy()
                    .contains([':', ' ', '\n', '\t']),
                "LD_PRELOAD paths must not contain whitespace or colon"
            );
        }
        if let Some(path) = &self.vulkan_layer_manifest {
            let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
            ensure!(
                manifest["layer"]["name"] == "VK_LAYER_RENDERDOC_Capture",
                "not a RenderDoc Vulkan layer manifest"
            );
        }
        ensure!(
            !self
                .capture_prefix
                .as_os_str()
                .to_string_lossy()
                .contains(['\n', '\r']),
            "invalid RenderDoc capture prefix"
        );
        let times = self
            .times
            .iter()
            .map(|time| crate::parse_timecode(time))
            .collect::<Result<Vec<_>>>()?;
        ensure!(times.windows(2).all(|pair| pair[1] >= pair[0] + timeout), "RenderDoc capture times must be separated by at least timeout (no overlapping captures)");
        Ok(times)
    }

    pub fn end(&self) -> Result<Duration> {
        Ok(self.schedule()?.last().copied().unwrap_or_default()
            + crate::parse_timecode(&self.timeout)?)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::session::{self, Worker};
    use anyhow::{bail, Context};
    use std::fs::File;
    use std::io::{BufRead, BufReader, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::Instant;

    pub struct Prepared {
        parent: UnixStream,
        child: UnixStream,
        report: File,
        times: Vec<Duration>,
        timeout: Duration,
        frames: u32,
        layer_directory: Option<PathBuf>,
    }

    impl Prepared {
        pub fn new(config: &Config, output: &std::path::Path) -> Result<Self> {
            let (parent, child) = UnixStream::pair()?;
            parent.set_read_timeout(Some(Duration::from_millis(100)))?;
            parent.set_write_timeout(Some(Duration::from_secs(1)))?;
            if let Some(directory) = config.capture_prefix.parent() {
                std::fs::create_dir_all(directory)?;
            }
            let layer_directory = if let Some(source) = &config.vulkan_layer_manifest {
                let mut manifest: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(source)?)?;
                manifest["layer"]["library_path"] = serde_json::json!(config.library);
                // Explicit activation avoids changing the machine's implicit-layer registration.
                manifest["layer"]
                    .as_object_mut()
                    .context("invalid layer manifest")?
                    .remove("enable_environment");
                manifest["layer"]
                    .as_object_mut()
                    .unwrap()
                    .remove("disable_environment");
                let directory = output
                    .parent()
                    .context("missing report directory")?
                    .join("renderdoc-layer");
                std::fs::create_dir_all(&directory)?;
                session::save(
                    session::new_output(&directory.join("renderdoc.json"))?,
                    &manifest,
                )?;
                Some(directory)
            } else {
                None
            };
            Ok(Self {
                parent,
                child,
                report: session::new_output(output)?,
                times: config.schedule()?,
                timeout: crate::parse_timecode(&config.timeout)?,
                frames: config.frames,
                layer_directory,
            })
        }

        pub fn configure_command(
            &self,
            command: &mut Command,
            config: &Config,
            inherit_environment: bool,
        ) -> Result<()> {
            let fd = self.child.as_raw_fd();
            // Only this child's fd loses CLOEXEC. Other commands cannot inherit the control socket.
            // SAFETY: fcntl is async-signal-safe; the captured fd is owned until spawn returns.
            unsafe {
                command.pre_exec(move || {
                    if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            command.env("RUZU_CAPTURE_CONTROL_FD", fd.to_string());
            command.env("RUZU_CAPTURE_PREFIX", &config.capture_prefix);
            let mut preload = format!("{}:{}", config.library.display(), config.helper.display());
            let configured = command
                .get_envs()
                .find(|(key, _)| *key == "LD_PRELOAD")
                .and_then(|(_, value)| value.map(std::ffi::OsStr::to_os_string));
            if let Some(existing) = configured.or_else(|| {
                inherit_environment
                    .then(|| std::env::var_os("LD_PRELOAD"))
                    .flatten()
            }) {
                if !existing.is_empty() {
                    preload.push(':');
                    preload.push_str(&existing.to_string_lossy());
                }
            }
            command.env("LD_PRELOAD", preload);
            if let Some(directory) = &self.layer_directory {
                for (key, prefix) in [
                    ("VK_ADD_LAYER_PATH", directory.as_os_str()),
                    (
                        "VK_INSTANCE_LAYERS",
                        std::ffi::OsStr::new("VK_LAYER_RENDERDOC_Capture"),
                    ),
                ] {
                    let mut value = prefix.to_os_string();
                    let existing = command
                        .get_envs()
                        .find(|(name, _)| *name == key)
                        .and_then(|(_, value)| value.map(std::ffi::OsStr::to_os_string))
                        .or_else(|| inherit_environment.then(|| std::env::var_os(key)).flatten());
                    if let Some(existing) = existing.filter(|value| !value.is_empty()) {
                        value.push(":");
                        value.push(existing);
                    }
                    command.env(key, value);
                }
            }
            Ok(())
        }

        pub fn start(self, origin: Instant) -> Result<Worker> {
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = stop.clone();
            Worker::spawn(stop, move || {
                let Self {
                    parent,
                    child,
                    report,
                    times,
                    timeout,
                    frames,
                    ..
                } = self;
                drop(child);
                let mut reader = BufReader::new(parent.try_clone()?);
                let mut writer = parent;
                let mut records = Vec::new();
                let result = (|| -> Result<()> {
                    let ready = read_line(
                        &mut reader,
                        Instant::now() + Duration::from_secs(10),
                        &worker_stop,
                    )?;
                    ensure!(ready == "READY", "RenderDoc helper unavailable: {ready}");
                    eprintln!("RenderDoc ready. Timed captures target its ACTIVE API/window (F11 selects it); verify Vulkan overlay, not GTK OpenGL ES.");
                    for at in times {
                        if !session::wait_until(origin, at, &worker_stop) {
                            break;
                        }
                        let actual = origin.elapsed();
                        writeln!(writer, "CAPTURE {} {}", frames, timeout.as_millis())?;
                        let reply = read_line(
                            &mut reader,
                            Instant::now() + timeout + Duration::from_secs(1),
                            &worker_stop,
                        );
                        let success = reply.as_ref().is_ok_and(|line| line.starts_with("DONE "));
                        records.push(serde_json::json!({ "scheduled_us": at.as_micros(), "actual_us": actual.as_micros(),
                            "lateness_us": actual.as_micros() as i128 - at.as_micros() as i128,
                            "success": success, "detail": reply.as_ref().map(String::as_str).map_err(|e| format!("{e:#}")) }));
                        let reply = reply?;
                        ensure!(success, "RenderDoc capture failed: {reply}");
                        println!("RenderDoc: {reply}");
                    }
                    Ok(())
                })();
                session::save(
                    report,
                    &serde_json::json!({ "version": 1, "captures": records,
                    "error": result.as_ref().err().map(|error| format!("{error:#}")) }),
                )?;
                result
            })
        }
    }

    fn read_line(
        reader: &mut BufReader<UnixStream>,
        deadline: Instant,
        stop: &AtomicBool,
    ) -> Result<String> {
        let mut line = String::new();
        loop {
            if stop.load(Ordering::Acquire) || crate::cancelled() {
                bail!("RenderDoc wait cancelled");
            }
            ensure!(Instant::now() < deadline, "RenderDoc response timed out");
            match reader.read_line(&mut line) {
                Ok(0) => bail!("RenderDoc helper disconnected"),
                Ok(_) if line.ends_with('\n') => return Ok(line.trim_end().to_owned()),
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error).context("RenderDoc control read failed"),
            }
            ensure!(line.len() <= 65536, "RenderDoc response too large");
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn bridge_reads_response_and_reports_disconnect_without_gpu() {
            let (left, mut right) = UnixStream::pair().unwrap();
            writeln!(right, "DONE 1 /tmp/synthetic.rdc").unwrap();
            let mut reader = BufReader::new(left);
            let stop = AtomicBool::new(false);
            assert_eq!(
                read_line(&mut reader, Instant::now() + Duration::from_secs(1), &stop).unwrap(),
                "DONE 1 /tmp/synthetic.rdc"
            );
            drop(right);
            assert!(
                read_line(&mut reader, Instant::now() + Duration::from_secs(1), &stop).is_err()
            );
        }
    }
}
#[cfg(target_os = "linux")]
pub use linux::Prepared;

// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! One-shot native GPU capture, not an emulation or synchronization workaround.
//! Apple MTLCaptureManager records buffers created and committed within the scope.
//! Eden has no native Metal counterpart.

use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSURL;
use objc2_metal::{MTLCaptureDescriptor, MTLCaptureDestination, MTLCaptureManager, MTLDevice};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Waiting,
    Capturing,
    Done,
}

pub(super) struct MetalFrameCapture {
    directory: PathBuf,
    manager: Retained<MTLCaptureManager>,
    state: State,
}

impl MetalFrameCapture {
    pub(super) fn from_environment() -> Option<Self> {
        let directory = PathBuf::from(std::env::var_os("RUZU_METAL_FRAME_CAPTURE_DIR")?);
        if !directory.is_absolute() {
            log::error!("Metal frame capture requires an absolute directory path");
            return None;
        }
        // SAFETY: Apple's process-wide manager; all access here is serialized
        // by the renderer owner. Do not take over an existing Xcode capture.
        let manager = unsafe { MTLCaptureManager::sharedCaptureManager() };
        if !manager.supportsDestination(MTLCaptureDestination::GPUTraceDocument) {
            log::error!("Metal GPU capture unavailable; launch with MTL_CAPTURE_ENABLED=1");
            return None;
        }
        if let Err(error) = std::fs::create_dir(&directory) {
            log::error!("Cannot create fresh Metal capture directory {directory:?}: {error}");
            return None;
        }
        log::info!("Metal GPU capture armed: create {:?} to capture one presentation interval",
            directory.join("capture.request"));
        Some(Self { directory, manager, state: State::Waiting })
    }

    pub(super) fn needs_boundary(&self) -> bool {
        boundary_requested(self.state, || self.directory.join("capture.request").is_file())
    }

    pub(super) fn abort(&mut self) {
        if self.state == State::Capturing {
            self.manager.stopCapture();
        }
        self.state = State::Done;
    }

    /// Called after flushing recording, not waiting for GPU completion. This
    /// includes all command buffers between two presentation boundaries.
    pub(super) fn frame_boundary(&mut self, device: &ProtocolObject<dyn MTLDevice>) {
        if self.state == State::Capturing {
            self.manager.stopCapture();
            self.state = State::Done;
            log::info!("Metal GPU capture finished: {:?}", self.directory.join("frame.gputrace"));
            return;
        }
        if self.state != State::Waiting {
            return;
        }
        // Every attempt is one-shot, including errors. Never repeatedly capture
        // frames or overwrite an existing archive on a retained request file.
        self.state = State::Done;
        if self.manager.isCapturing() {
            log::error!("Another Metal capture is active; leaving it untouched");
            return;
        }
        let path = self.directory.join("frame.gputrace");
        if path.exists() {
            log::error!("Refusing to overwrite Metal capture {path:?}");
            return;
        }
        let Some(url) = NSURL::from_file_path(&path) else {
            log::error!("Invalid Metal GPU capture path {path:?}");
            return;
        };
        let descriptor = MTLCaptureDescriptor::new();
        // SAFETY: captureObject accepts MTLDevice, held by the renderer.
        unsafe { descriptor.setCaptureObject(Some(device.as_ref())) };
        descriptor.setDestination(MTLCaptureDestination::GPUTraceDocument);
        descriptor.setOutputURL(Some(&url));
        match self.manager.startCaptureWithDescriptor_error(&descriptor) {
            Ok(()) => {
                self.state = State::Capturing;
                log::info!("Metal GPU capture started: {path:?}");
            }
            Err(error) => log::error!("Cannot start Metal GPU capture: {error}"),
        }
    }
}

impl Drop for MetalFrameCapture {
    fn drop(&mut self) {
        if self.state == State::Capturing {
            self.abort();
            log::warn!("Metal GPU capture interrupted before the next presentation boundary");
        }
    }
}

fn boundary_requested(state: State, request_exists: impl FnOnce() -> bool) -> bool {
    match state {
        State::Waiting => request_exists(),
        State::Capturing => true,
        State::Done => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_is_requested_once_and_stops_at_the_next_boundary() {
        assert!(!boundary_requested(State::Waiting, || false));
        assert!(boundary_requested(State::Waiting, || true));
        assert!(boundary_requested(State::Capturing, || panic!("must finish without polling")));
        assert!(!boundary_requested(State::Done, || panic!("must never restart")));
    }
}

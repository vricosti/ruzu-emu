// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/shader_notify.h and video_core/shader_notify.cpp
//!
//! Shader compilation notification system for reporting build progress to the UI.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Duration after which we stop reporting "shaders building".
const TIME_TO_STOP_REPORTING: Duration = Duration::from_secs(2);

/// Tracks shader compilation progress.
pub struct ShaderNotify {
    num_building: AtomicI32,
    num_complete: AtomicI32,
    report: Mutex<ReportState>,
}

struct ReportState {
    report_base: i32,
    completed: bool,
    num_when_completed: i32,
    complete_time: Option<Instant>,
}

impl ShaderNotify {
    pub fn new() -> Self {
        Self {
            num_building: AtomicI32::new(0),
            num_complete: AtomicI32::new(0),
            report: Mutex::new(ReportState {
                report_base: 0,
                completed: false,
                num_when_completed: 0,
                complete_time: None,
            }),
        }
    }

    /// Returns the number of shaders currently being built (relative to report base).
    ///
    /// After all shaders complete and a timeout passes, the report resets to zero.
    pub fn shaders_building(&self) -> i32 {
        let now_complete = self.num_complete.load(Ordering::Relaxed);
        let now_building = self.num_building.load(Ordering::Relaxed);
        let mut report = self
            .report
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if now_complete == now_building {
            let now = Instant::now();
            if report.completed
                && self.num_complete.load(Ordering::SeqCst) == report.num_when_completed
            {
                if let Some(complete_time) = report.complete_time {
                    if now.duration_since(complete_time) > TIME_TO_STOP_REPORTING {
                        report.report_base = now_complete;
                        report.completed = false;
                    }
                }
            } else {
                report.completed = true;
                report.num_when_completed = self.num_complete.load(Ordering::SeqCst);
                report.complete_time = Some(now);
            }
        }

        now_building - report.report_base
    }

    /// Mark a shader as completed.
    pub fn mark_shader_complete(&self) {
        self.num_complete.fetch_add(1, Ordering::SeqCst);
    }

    /// Mark a shader as building.
    pub fn mark_shader_building(&self) {
        self.num_building.fetch_add(1, Ordering::SeqCst);
    }
}

/// Non-owning counterpart of upstream's `VideoCore::ShaderNotify*`.
///
/// `Gpu` owns the pointee and drops its renderer before the notification
/// object. Pipeline workers are owned and joined by that renderer, so handles
/// cannot outlive the pointee.
#[derive(Clone, Copy)]
pub struct ShaderNotifyHandle(NonNull<ShaderNotify>);

impl ShaderNotifyHandle {
    /// The caller must ensure the pointee outlives every copied handle and all
    /// worker closures containing one.
    pub(crate) unsafe fn new(shader_notify: &ShaderNotify) -> Self {
        Self(NonNull::from(shader_notify))
    }

    pub fn mark_shader_complete(self) {
        unsafe { self.0.as_ref() }.mark_shader_complete();
    }

    pub fn mark_shader_building(self) {
        unsafe { self.0.as_ref() }.mark_shader_building();
    }
}

// SAFETY: `ShaderNotify` is thread-safe and `Gpu` keeps it alive until all
// renderer-owned pipeline workers have been joined.
unsafe impl Send for ShaderNotifyHandle {}
unsafe impl Sync for ShaderNotifyHandle {}

impl Default for ShaderNotify {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_started_builds_until_the_completion_timeout() {
        let notify = ShaderNotify::new();
        let handle = unsafe { ShaderNotifyHandle::new(&notify) };

        assert_eq!(notify.shaders_building(), 0);
        handle.mark_shader_building();
        handle.mark_shader_building();
        assert_eq!(notify.shaders_building(), 2);

        handle.mark_shader_complete();
        assert_eq!(notify.shaders_building(), 2);
        handle.mark_shader_complete();
        assert_eq!(notify.shaders_building(), 2);

        notify.report.lock().unwrap().complete_time =
            Some(Instant::now() - TIME_TO_STOP_REPORTING - Duration::from_millis(1));
        assert_eq!(notify.shaders_building(), 0);
    }
}

// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/shader_notify.h and video_core/shader_notify.cpp
//!
//! Shader compilation notification system for reporting build progress to the UI.
//!
//! The warm-up and precompiled-pipeline state is ported from Fritchii's
//! `shader_notify.h` (yuzu, 2020; Fritchii additions 2026): `WarmProgress`,
//! the warm-up gate (`WarmMayBuild`, upstream commit `4a4fa51576`), and the
//! precompiled-pipeline state machine used by the background preparation in
//! `renderer_vulkan::pipeline_cache`.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Duration after which we stop reporting "shaders building".
const TIME_TO_STOP_REPORTING: Duration = Duration::from_secs(2);

/// Nothing to prepare for this title (upstream `PrecompileAbsent`).
const PRECOMPILE_ABSENT: i32 = 0;
/// Prepared, or waiting for its turn (upstream `PrecompileIdle`).
const PRECOMPILE_IDLE: i32 = 1;
/// A preparation runs (counted in `warm_running`, upstream `PrecompileRunning`).
const PRECOMPILE_RUNNING: i32 = 2;
/// Requested (counted in `warm_running`, upstream `PrecompileAsked`).
const PRECOMPILE_ASKED: i32 = 3;

/// Warm-up progress shown in the status bar (upstream `ShaderNotify::WarmProgress`).
///
/// The three values are read separately, so a reading taken during an update can
/// mix two updates; fine for a display.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WarmProgress {
    /// 0 none, 1 compute, 2 graphics, 3 precompiled pipelines.
    pub kind: u32,
    pub done: usize,
    /// 0 when unknown; for graphics it can be an estimate.
    pub total: usize,
}

/// Tracks shader compilation progress.
pub struct ShaderNotify {
    num_building: AtomicI32,
    num_complete: AtomicI32,
    report: Mutex<ReportState>,
    warm_kind: AtomicUsize,
    warm_done: AtomicUsize,
    warm_total: AtomicUsize,
    warm_running: AtomicI32,
    emulation_paused: AtomicBool,
    warm_requested: AtomicBool,
    precompile_state: AtomicI32,
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
            warm_kind: AtomicUsize::new(0),
            warm_done: AtomicUsize::new(0),
            warm_total: AtomicUsize::new(0),
            warm_running: AtomicI32::new(0),
            emulation_paused: AtomicBool::new(false),
            warm_requested: AtomicBool::new(false),
            precompile_state: AtomicI32::new(PRECOMPILE_ABSENT),
        }
    }

    /// Called from the warm-up threads. `kind` 0 hides the indicator.
    pub fn set_warm_progress(&self, kind: u32, done: usize, total: usize) {
        self.warm_done.store(done, Ordering::Relaxed);
        self.warm_total.store(total, Ordering::Relaxed);
        self.warm_kind.store(kind as usize, Ordering::Release);
    }

    /// Upstream `ShaderNotify::GetWarmProgress`.
    pub fn warm_progress(&self) -> WarmProgress {
        WarmProgress {
            kind: self.warm_kind.load(Ordering::Acquire) as u32,
            done: self.warm_done.load(Ordering::Relaxed),
            total: self.warm_total.load(Ordering::Relaxed),
        }
    }

    /// A warm-up (compute or graphics) started or ended.
    pub fn begin_warm(&self) {
        self.warm_running.fetch_add(1, Ordering::Relaxed);
    }

    pub fn end_warm(&self) {
        self.warm_running.fetch_sub(1, Ordering::Relaxed);
    }

    /// Upstream `ShaderNotify::WarmActive`: true while any warm-up has work left.
    pub fn warm_active(&self) -> bool {
        self.warm_running.load(Ordering::Relaxed) > 0
    }

    /// The warm-up never competes with the game for the CPU and the driver: it
    /// builds only while emulation is paused, while the player runs it from the
    /// menu (Prepare Shaders Now), or always with `RUZU_WARM_WHILE_PLAYING=1`.
    pub fn set_emulation_paused(&self, paused: bool) {
        self.emulation_paused.store(paused, Ordering::Relaxed);
    }

    pub fn set_warm_requested(&self, requested: bool) {
        self.warm_requested.store(requested, Ordering::Relaxed);
    }

    /// Upstream `ShaderNotify::WarmMayBuild` (`shader_notify.cpp`).
    pub fn warm_may_build(&self) -> bool {
        let while_playing = common::env_flag!("RUZU_WARM_WHILE_PLAYING");
        while_playing
            || self.emulation_paused.load(Ordering::Relaxed)
            || self.warm_requested.load(Ordering::Relaxed)
    }

    /// False when this title has no precompiled pipelines to prepare.
    pub fn set_precompile_available(&self, available: bool) {
        let expected = if available {
            PRECOMPILE_ABSENT
        } else {
            PRECOMPILE_IDLE
        };
        let new = if available {
            PRECOMPILE_IDLE
        } else {
            PRECOMPILE_ABSENT
        };
        let _ = self
            .precompile_state
            .compare_exchange(expected, new, Ordering::AcqRel, Ordering::Acquire);
    }

    /// The preparation thread starts a run (on its own or for a request).
    pub fn precompile_started(&self) {
        if self.precompile_state.swap(PRECOMPILE_RUNNING, Ordering::AcqRel) != PRECOMPILE_ASKED {
            self.begin_warm();
        }
    }

    pub fn precompile_ended(&self) {
        self.precompile_state
            .store(PRECOMPILE_IDLE, Ordering::Release);
        self.end_warm();
    }

    /// Upstream `ShaderNotify::RequestPrecompile`: false when this title has no
    /// precompiled pipelines to prepare.
    pub fn request_precompile(&self) -> bool {
        let expected = self
            .precompile_state
            .compare_exchange(
                PRECOMPILE_IDLE,
                PRECOMPILE_ASKED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .unwrap_or_else(|value| value);
        if expected == PRECOMPILE_IDLE {
            self.begin_warm();
            true
        } else {
            expected == PRECOMPILE_RUNNING || expected == PRECOMPILE_ASKED
        }
    }

    pub fn precompile_requested(&self) -> bool {
        self.precompile_state.load(Ordering::Acquire) == PRECOMPILE_ASKED
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
    /// Balance the upstream start/complete pair across Rust early returns and unwinding.
    pub fn build_scope(self) -> ShaderBuildScope {
        self.mark_shader_building();
        ShaderBuildScope(self)
    }

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

    pub fn set_warm_progress(self, kind: u32, done: usize, total: usize) {
        unsafe { self.0.as_ref() }.set_warm_progress(kind, done, total);
    }

    pub fn begin_warm(self) {
        unsafe { self.0.as_ref() }.begin_warm();
    }

    pub fn end_warm(self) {
        unsafe { self.0.as_ref() }.end_warm();
    }

    pub fn precompile_started(self) {
        unsafe { self.0.as_ref() }.precompile_started();
    }

    pub fn precompile_ended(self) {
        unsafe { self.0.as_ref() }.precompile_ended();
    }

    pub fn precompile_requested(self) -> bool {
        unsafe { self.0.as_ref() }.precompile_requested()
    }

    pub fn set_precompile_available(self, available: bool) {
        unsafe { self.0.as_ref() }.set_precompile_available(available);
    }

    pub fn request_precompile(self) -> bool {
        unsafe { self.0.as_ref() }.request_precompile()
    }
}

// SAFETY: `ShaderNotify` is thread-safe and `Gpu` keeps it alive until all
// renderer-owned pipeline workers have been joined.
unsafe impl Send for ShaderNotifyHandle {}
unsafe impl Sync for ShaderNotifyHandle {}

#[must_use = "keep the notification scope alive until compilation completes"]
pub struct ShaderBuildScope(ShaderNotifyHandle);

impl Drop for ShaderBuildScope {
    fn drop(&mut self) {
        self.0.mark_shader_complete();
    }
}

impl Default for ShaderNotify {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_scope_balances_success_error_and_unwind() {
        let notify = ShaderNotify::new();
        let handle = unsafe { ShaderNotifyHandle::new(&notify) };
        {
            let _build = handle.build_scope();
            assert_eq!(notify.num_building.load(Ordering::SeqCst), 1);
            assert_eq!(notify.num_complete.load(Ordering::SeqCst), 0);
        }
        let failed = || -> Result<(), ()> {
            let _build = handle.build_scope();
            Err(())?;
            Ok(())
        };
        assert!(failed().is_err());
        assert!(std::panic::catch_unwind(|| {
            let _build = handle.build_scope();
            panic!("test compilation failure");
        }).is_err());
        assert_eq!(notify.num_building.load(Ordering::SeqCst), 3);
        assert_eq!(notify.num_complete.load(Ordering::SeqCst), 3);
    }

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

    #[test]
    fn warm_progress_round_trips_and_kind_zero_hides() {
        let notify = ShaderNotify::new();
        assert_eq!(notify.warm_progress(), WarmProgress::default());
        notify.set_warm_progress(3, 40, 260);
        assert_eq!(
            notify.warm_progress(),
            WarmProgress {
                kind: 3,
                done: 40,
                total: 260
            }
        );
        notify.set_warm_progress(0, 0, 0);
        assert_eq!(notify.warm_progress().kind, 0);
    }

    #[test]
    fn warm_active_counts_running_work() {
        let notify = ShaderNotify::new();
        assert!(!notify.warm_active());
        notify.begin_warm();
        notify.begin_warm();
        assert!(notify.warm_active());
        notify.end_warm();
        assert!(notify.warm_active());
        notify.end_warm();
        assert!(!notify.warm_active());
    }

    #[test]
    fn warm_may_build_waits_for_pause_or_request() {
        let notify = ShaderNotify::new();
        // No pause, no request and no override: warm-up must not compete.
        assert!(!notify.warm_may_build());
        notify.set_emulation_paused(true);
        assert!(notify.warm_may_build());
        notify.set_emulation_paused(false);
        notify.set_warm_requested(true);
        assert!(notify.warm_may_build());
        notify.set_warm_requested(false);
        assert!(!notify.warm_may_build());
    }

    #[test]
    fn precompile_state_machine_matches_upstream() {
        let notify = ShaderNotify::new();

        // Nothing to prepare for this title: a request is refused.
        assert!(!notify.request_precompile());
        assert!(!notify.precompile_requested());
        assert!(!notify.warm_active());

        // Available: a request asks for a run and counts as warm work at once.
        notify.set_precompile_available(true);
        assert!(notify.request_precompile());
        assert!(notify.precompile_requested());
        assert!(notify.warm_active());

        // A second request while asked only reports that work remains.
        assert!(notify.request_precompile());
        assert_eq!(notify.warm_running.load(Ordering::Acquire), 1);

        // The thread takes the request without counting the run twice.
        notify.precompile_started();
        assert!(!notify.precompile_requested());
        assert_eq!(notify.warm_running.load(Ordering::Acquire), 1);

        // A plain run (no request) counts as warm work.
        notify.precompile_ended();
        assert!(!notify.warm_active());
        notify.precompile_started();
        assert!(notify.warm_active());
        notify.precompile_ended();
        assert!(!notify.warm_active());

        // A request while a run is going reports work remains but is not queued
        // (upstream only moves Idle to Asked).
        assert!(notify.request_precompile());
        notify.precompile_started();
        assert!(notify.request_precompile());
        assert!(!notify.precompile_requested());
        notify.precompile_ended();
        assert!(!notify.warm_active());

        // Unavailable: no request.
        notify.set_precompile_available(false);
        assert!(!notify.request_precompile());
        assert!(!notify.warm_active());
    }
}

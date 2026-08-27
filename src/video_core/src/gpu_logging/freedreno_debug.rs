// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Android-only port of Eden `video_core/gpu_logging/freedreno_debug.{h,cpp}`.

#![cfg(target_os = "android")]

use std::sync::atomic::{AtomicBool, Ordering};

use log::info;

pub struct FreedrenoDebugger;

impl FreedrenoDebugger {
    pub fn initialize() {
        static IS_INITIALIZED: AtomicBool = AtomicBool::new(false);
        if IS_INITIALIZED.swap(true, Ordering::AcqRel) {
            return;
        }
        info!("[Freedreno Debug] Initialized");
    }

    pub fn set_tu_debug_flags(flags: &str) {
        if flags.is_empty() {
            return;
        }
        std::env::set_var("TU_DEBUG", flags);
        info!("[Freedreno Debug] TU_DEBUG set to: {flags}");
    }

    pub fn enable_command_stream_dump(frames_only: bool) {
        let dump_flags = if frames_only { "frames" } else { "all" };
        std::env::set_var("FD_RD_DUMP", dump_flags);
        info!("[Freedreno Debug] Command stream dump enabled: {dump_flags}");
    }

    pub fn get_breadcrumbs() -> String {
        "Breadcrumb capture not yet implemented".to_owned()
    }
}

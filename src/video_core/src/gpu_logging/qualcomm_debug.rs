// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden `video_core/gpu_logging/qualcomm_debug.{h,cpp}`.

use std::sync::atomic::{AtomicBool, Ordering};

use log::info;

pub struct QualcommDebugger;

impl QualcommDebugger {
    pub fn initialize() {
        static IS_INITIALIZED: AtomicBool = AtomicBool::new(false);
        if IS_INITIALIZED.swap(true, Ordering::AcqRel) {
            return;
        }
        info!("[Qualcomm Debug] Initialized (stub)");
    }

    pub fn get_debug_info() -> String {
        "Qualcomm debug info not yet implemented".to_owned()
    }
}

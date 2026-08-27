// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `video_core/control/` directory.
//!
//! GPU channel management, scheduling, and per-channel cache state.

pub mod channel_state;
pub mod channel_state_cache;
pub mod scheduler;

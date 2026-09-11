// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `video_core/buffer_cache/`
//!
//! GPU buffer cache subsystem. Tracks buffer allocations, dirty ranges,
//! and synchronization between CPU and GPU memory.

pub mod buffer_base;
pub mod buffer_cache;
pub mod buffer_cache_base;
pub mod memory_tracker_base;
pub mod usage_tracker;
pub mod virtual_range_cache;
pub mod word_manager;

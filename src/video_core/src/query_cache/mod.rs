// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `src/video_core/query_cache/`.
//!
//! GPU query cache subsystem. Manages hardware query counters (occlusion,
//! primitives generated, etc.), caching their results and synchronizing
//! between guest and host.

pub mod bank_base;
pub mod query_base;
pub mod query_cache;
pub mod query_cache_base;
pub mod query_stream;
pub mod types;

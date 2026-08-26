// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/textures/workers.h` and `workers.cpp`.
//!
//! Provides the shared `Common::ThreadWorker` instance used by texture
//! transcoding operations.

use common::thread_worker::ThreadWorker;
use std::sync::OnceLock;

/// Global singleton thread worker pool for texture transcoding.
///
/// Port of the static `Common::ThreadWorker` in `GetThreadWorkers()`.
static THREAD_WORKERS: OnceLock<ThreadWorker> = OnceLock::new();

/// Returns a reference to the shared texture transcoding thread worker pool.
///
/// Port of `Tegra::Texture::GetThreadWorkers()`.
///
/// The pool is lazily initialized with `max(hardware_concurrency, 2) / 2` threads.
pub fn get_thread_workers() -> &'static ThreadWorker {
    THREAD_WORKERS.get_or_init(|| {
        let num_threads = std::cmp::max(
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(2),
            2,
        ) / 2;
        ThreadWorker::new_stateless(num_threads, "ImageTranscode".to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_the_same_process_wide_worker() {
        assert!(std::ptr::eq(get_thread_workers(), get_thread_workers()));
    }
}

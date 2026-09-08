// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Opt-in, bounded command-lifecycle evidence for native driver stalls.
//!
//! Eden's GPU logger provides the diagnostic concept, but has no Metal queue
//! counterpart. No driver objects are retained here. Records bypass userspace
//! buffering; the last writes are not guaranteed to survive a system panic.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::metal_gpu_profiler::{ComputeWork, COMPUTE_WORK_COUNT};

const RECORD_SIZE: usize = 512;
const RECORD_COUNT: u64 = 8192;

pub(super) struct CommandJournal {
    state: Mutex<JournalState>,
    started: Instant,
}

struct JournalState {
    file: File,
    sequence: u64,
    failed: bool,
}

/// Recording-side summary, not execution evidence or a complete shader list.
/// Counts refer to scheduler helper calls and saturate instead of overflowing.
#[derive(Default)]
pub(super) struct CommandWorkload {
    blit_calls: u32,
    compute_calls: [u32; COMPUTE_WORK_COUNT],
    draws: u32,
    first_shaders: [u64; 6],
    last_shaders: [u64; 6],
}

impl CommandWorkload {
    pub(super) fn observe_blit(&mut self) {
        self.blit_calls = self.blit_calls.saturating_add(1);
    }

    pub(super) fn observe_compute(&mut self, work: ComputeWork) {
        let count = &mut self.compute_calls[work as usize];
        *count = count.saturating_add(1);
    }

    pub(super) fn observe_draw(&mut self, shaders: [u64; 6]) {
        if self.draws == 0 {
            self.first_shaders = shaders;
        }
        self.last_shaders = shaders;
        self.draws = self.draws.saturating_add(1);
    }

    pub(super) fn record(&self, journal: &CommandJournal, object: usize, tick: u64) {
        journal.record(format_args!(
            "recorded_work object=0x{object:x} tick={tick} blit_calls={} compute_calls={:?} draws={}",
            self.blit_calls, self.compute_calls, self.draws
        ));
        if self.draws != 0 {
            journal.record(format_args!(
                "recorded_shaders object=0x{object:x} tick={tick} first={:016x?} last={:016x?}",
                self.first_shaders, self.last_shaders
            ));
        }
    }
}

impl CommandJournal {
    pub(super) fn from_environment() -> Option<Arc<Self>> {
        let path = std::env::var_os("RUZU_METAL_COMMAND_JOURNAL")?;
        match Self::create(Path::new(&path)) {
            Ok(journal) => {
                journal.record(format_args!("opened pid={}", std::process::id()));
                Some(Arc::new(journal))
            }
            Err(error) => {
                log::error!("Cannot create Metal command journal {:?}: {error}", path);
                None
            }
        }
    }

    fn create(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.set_len(RECORD_SIZE as u64 * RECORD_COUNT)?;
        Ok(Self {
            state: Mutex::new(JournalState {
                file,
                sequence: 0,
                failed: false,
            }),
            started: Instant::now(),
        })
    }

    pub(super) fn record(&self, event: fmt::Arguments<'_>) {
        // The completion callback shares only this file mutex with recording.
        // No Metal API is called while holding it, including on write failure.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.failed {
            return;
        }
        let sequence = state.sequence;
        let text = format!(
            "seq={sequence:020} elapsed_us={} {event}",
            self.started.elapsed().as_micros()
        );
        let result = write_record(&state.file, sequence, &text);
        if result.is_err() || sequence == u64::MAX {
            state.failed = true;
            eprintln!("Metal command journal stopped: {result:?}, sequence={sequence}");
        } else {
            state.sequence += 1;
        }
    }
}

fn write_record(file: &File, sequence: u64, text: &str) -> io::Result<()> {
    if text.len() >= RECORD_SIZE || !text.is_ascii() || text.contains(['\n', '\r']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid journal record",
        ));
    }
    let mut record = [b' '; RECORD_SIZE];
    record[..text.len()].copy_from_slice(text.as_bytes());
    record[RECORD_SIZE - 1] = b'\n';
    file.write_all_at(&record, (sequence % RECORD_COUNT) * RECORD_SIZE as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

    struct TestFile(std::path::PathBuf);

    impl TestFile {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "metal-journal-{}-{}",
                std::process::id(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed)
            )))
        }
    }

    impl Drop for TestFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn journal_creation_never_overwrites_existing_evidence() {
        let path = TestFile::new();
        std::fs::write(&path.0, b"existing evidence").unwrap();
        assert!(CommandJournal::create(&path.0).is_err());
        assert_eq!(std::fs::read(&path.0).unwrap(), b"existing evidence");
    }

    #[test]
    fn workload_counts_calls_and_retains_first_and_last_shader_sets() {
        let mut workload = CommandWorkload::default();
        workload.observe_blit();
        workload.observe_compute(ComputeWork::IndexConversion);
        workload.observe_compute(ComputeWork::IndexConversion);
        workload.observe_compute(ComputeWork::Guest);
        workload.observe_draw([1; 6]);
        workload.observe_draw([2; 6]);
        workload.observe_draw([3; 6]);
        assert_eq!(workload.blit_calls, 1);
        let mut expected = [0; COMPUTE_WORK_COUNT];
        expected[ComputeWork::Guest as usize] = 1;
        expected[ComputeWork::IndexConversion as usize] = 2;
        assert_eq!(workload.compute_calls, expected);
        assert_eq!(workload.draws, 3);
        assert_eq!(workload.first_shaders, [1; 6]);
        assert_eq!(workload.last_shaders, [3; 6]);
        let finished = std::mem::take(&mut workload);
        assert_eq!(finished.draws, 3);
        assert_eq!(workload.draws, 0);
        assert_eq!(workload.compute_calls, [0; COMPUTE_WORK_COUNT]);
        workload.observe_draw([4; 6]);
        assert_eq!(workload.first_shaders, [4; 6]);
    }

    #[test]
    fn workload_maximum_values_fit_records_and_counters_saturate() {
        let path = TestFile::new();
        let journal = CommandJournal::create(&path.0).unwrap();
        let mut workload = CommandWorkload {
            blit_calls: u32::MAX,
            compute_calls: [u32::MAX; COMPUTE_WORK_COUNT],
            draws: u32::MAX,
            first_shaders: [u64::MAX; 6],
            last_shaders: [u64::MAX; 6],
        };
        workload.observe_blit();
        workload.observe_compute(ComputeWork::Other);
        workload.observe_draw([u64::MAX; 6]);
        assert_eq!(workload.blit_calls, u32::MAX);
        assert_eq!(workload.compute_calls[0], u32::MAX);
        assert_eq!(workload.draws, u32::MAX);
        workload.record(&journal, usize::MAX, u64::MAX);
        let state = journal.state.lock().unwrap();
        assert!(!state.failed);
        assert_eq!(state.sequence, 2);
        let bytes = std::fs::read(&path.0).unwrap();
        assert!(std::str::from_utf8(&bytes[..RECORD_SIZE]).unwrap().contains("recorded_work"));
        assert!(std::str::from_utf8(&bytes[RECORD_SIZE..2 * RECORD_SIZE]).unwrap().contains("recorded_shaders"));
    }

    #[test]
    fn journal_wrap_is_bounded_and_records_remain_individually_ordered() {
        let path = TestFile::new();
        let journal = CommandJournal::create(&path.0).unwrap();
        for index in 0..RECORD_COUNT + 3 {
            journal.record(format_args!("event={index}"));
        }
        let bytes = std::fs::read(&path.0).unwrap();
        assert_eq!(bytes.len(), RECORD_SIZE * RECORD_COUNT as usize);
        let mut sequences = Vec::new();
        for record in bytes.chunks_exact(RECORD_SIZE) {
            assert_eq!(record[RECORD_SIZE - 1], b'\n');
            sequences.push(
                std::str::from_utf8(&record[4..24])
                    .unwrap()
                    .parse::<u64>()
                    .unwrap(),
            );
        }
        sequences.sort_unstable();
        assert_eq!(sequences, (3..RECORD_COUNT + 3).collect::<Vec<_>>());
    }

    #[test]
    fn journal_accepts_completion_from_another_thread_without_scheduler_lifetime() {
        let path = TestFile::new();
        let journal = Arc::new(CommandJournal::create(&path.0).unwrap());
        journal.record(format_args!("submit_begin object=0x123 tick=4"));
        let callback = Arc::clone(&journal);
        drop(journal);
        std::thread::spawn(move || {
            callback.record(format_args!("completed object=0x123 tick=4 status=4"))
        })
        .join()
        .unwrap();
        let bytes = std::fs::read(&path.0).unwrap();
        let records = std::str::from_utf8(&bytes[..RECORD_SIZE * 2]).unwrap();
        assert!(records.contains("seq=00000000000000000000"));
        assert!(records.contains("seq=00000000000000000001"));
        assert!(records.contains("completed object=0x123 tick=4 status=4"));
    }

    #[test]
    fn journal_rejects_oversized_or_multiline_records_without_corrupting_next_slot() {
        let path = TestFile::new();
        let journal = CommandJournal::create(&path.0).unwrap();
        let state = journal.state.lock().unwrap();
        assert!(write_record(&state.file, 0, &"x".repeat(RECORD_SIZE)).is_err());
        assert!(write_record(&state.file, 0, "bad\nrecord").is_err());
        assert!(std::fs::read(&path.0)
            .unwrap()
            .iter()
            .all(|byte| *byte == 0));
    }
}

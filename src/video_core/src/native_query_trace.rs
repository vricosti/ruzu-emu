// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of header-only `video_core/native_query_trace.h`.

use std::io::Write;
use std::sync::OnceLock;

mod native_query_diagnostics {
    use parking_lot::Mutex;
    use std::fs::File;
    use std::io::{BufWriter, Write};
    use std::sync::OnceLock;

    pub(super) struct State {
        // None preserves ofstream's sticky failed state without throwing on diagnostic I/O.
        pub file: Option<BufWriter<File>>,
        pub records: usize,
    }

    impl State {
        fn new() -> Self {
            let mut file = File::create("native-query-trace.csv").ok().map(BufWriter::new);
            if let Some(writer) = &mut file {
                if writer.write_all(b"event,asid,gpu,cpu,type,value,flags\n").is_err() {
                    file = None;
                }
            }
            Self { file, records: 0 }
        }
    }

    pub(super) fn get_state() -> &'static Mutex<State> {
        static STATE: OnceLock<Mutex<State>> = OnceLock::new();
        STATE.get_or_init(|| {
            // C++ destroys the static ofstream at normal exit. Rust statics do not Drop.
            unsafe { libc::atexit(flush_at_exit); }
            Mutex::new(State::new())
        })
    }

    extern "C" fn flush_at_exit() {
        if let Some(file) = &mut get_state().lock().file {
            let _ = file.flush();
        }
    }
}

/// Events: 0=request, 1=written, 2=invalidated, 3=unsynchronized,
/// 4=channel suspended, 5=channel resumed, 6=buffer readback,
/// 7/8=QMD releases, 9=PCAS request, 10=DMA destination, 11=inline write.
pub fn trace_native_query(event: u32, asid: u64, gpu: u64, cpu: u64, kind: u32, value: u64, flags: u32) {
    static DIAGNOSTICS: OnceLock<bool> = OnceLock::new();
    if !*DIAGNOSTICS.get_or_init(|| std::env::var_os("EDEN_NATIVE_DRAW_DIAGNOSTICS").is_some()) {
        return;
    }
    let mut state = native_query_diagnostics::get_state().lock();
    let records = state.records;
    state.records = state.records.wrapping_add(1);
    if records < 1_000_000 || event == 4 || event == 5 {
        if let Some(file) = &mut state.file {
            if writeln!(file, "{event},{asid},{gpu:x},{cpu:x},{kind:x},{value:x},{flags:x}").is_err() {
                state.file = None;
            }
        }
    }
    if event == 4 || event == 5 {
        if let Some(file) = &mut state.file {
            if file.flush().is_err() {
                state.file = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_query_trace_preserves_opt_in_cap_numeric_bases_and_flushes() {
        const CHILD: &str = "RUZU_NATIVE_QUERY_TRACE_CHILD";
        let Ok(mode) = std::env::var(CHILD) else {
            for mode in ["disabled", "enabled", "failed"] {
                let dir = std::env::temp_dir().join(format!("ruzu-query-trace-{}-{mode}", std::process::id()));
                std::fs::create_dir_all(&dir).unwrap();
                let mut child = std::process::Command::new(std::env::current_exe().unwrap());
                child.args(["--exact", std::thread::current().name().unwrap(), "--test-threads=1"])
                    .current_dir(&dir).env(CHILD, mode);
                if mode == "disabled" { child.env_remove("EDEN_NATIVE_DRAW_DIAGNOSTICS"); }
                else { child.env("EDEN_NATIVE_DRAW_DIAGNOSTICS", ""); }
                let status = child.status().unwrap();
                if status.success() && mode == "enabled" {
                    assert!(std::fs::read_to_string(dir.join("native-query-trace.csv")).unwrap().ends_with("11,1,0,0,0,0,0\n"));
                }
                std::fs::remove_dir_all(&dir).unwrap();
                assert!(status.success(), "child {mode}");
            }
            return;
        };
        let file = "native-query-trace.csv";
        if mode == "failed" {
            std::fs::create_dir(file).unwrap(); // opening an output file fails
            trace_native_query(4, 0, 0, 0, 0, 0, 0);
            std::fs::remove_dir(file).unwrap();
            trace_native_query(5, 0, 0, 0, 0, 0, 0);
            assert!(!std::path::Path::new(file).exists());
            assert_eq!(native_query_diagnostics::get_state().lock().records, 2);
            return;
        }
        if mode == "disabled" {
            trace_native_query(4, 1, 2, 3, 4, 5, 6);
            unsafe { std::env::set_var("EDEN_NATIVE_DRAW_DIAGNOSTICS", "1"); }
            trace_native_query(5, 1, 2, 3, 4, 5, 6);
            assert!(!std::path::Path::new(file).exists());
            return;
        }
        std::fs::write(file, "old contents must be truncated").unwrap();
        trace_native_query(12, 26, 0xab, 0xcd, 0xef, u64::MAX, 0x10);
        unsafe { std::env::remove_var("EDEN_NATIVE_DRAW_DIAGNOSTICS"); }
        native_query_diagnostics::get_state().lock().records = 999_999;
        trace_native_query(9, 2, 3, 4, 5, 6, 7); // last ordinary record
        trace_native_query(10, 2, 3, 4, 5, 6, 7); // suppressed, still increments
        trace_native_query(4, 26, 0xab, 0xcd, 0xef, 0x10, 0x20);
        let text = std::fs::read_to_string(file).unwrap(); // suspension flushes all buffered records
        assert_eq!(text, "event,asid,gpu,cpu,type,value,flags\n12,26,ab,cd,ef,ffffffffffffffff,10\n9,2,3,4,5,6,7\n4,26,ab,cd,ef,10,20\n");
        trace_native_query(5, 1, 0, 0, 0, 0, 0);
        assert!(std::fs::read_to_string(file).unwrap().ends_with("5,1,0,0,0,0,0\n"));
        assert_eq!(native_query_diagnostics::get_state().lock().records, 1_000_003);
        native_query_diagnostics::get_state().lock().records = 0;
        trace_native_query(11, 1, 0, 0, 0, 0, 0); // normal exit must flush this record too
    }
}

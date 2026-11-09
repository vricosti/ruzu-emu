// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of header-only `video_core/nx2_profile.h`.

use parking_lot::{Mutex, MutexGuard};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::Write;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::LazyLock;

#[repr(u32)]
#[derive(Clone, Copy, Debug)]
pub enum Zone {
    Other,
    Idle,
    PendingSleep,
    ResumePending,
    Push,
    Tick,
    Run,
    FlushInvalidate,
    Macro,
    Draw,
    Clear,
    Compute,
    FlushWork,
    PipelineLookup,
    Configure,
    SyncDescriptors,
    ReadHandles,
    FillImageViews,
    TextureBuffers,
    UpdateBuffers,
    DescriptorAcquire,
    BindStageBuffers,
    PushDescriptors,
    RenderTargets,
    ConfigureDraw,
    DynamicState,
    DrawRecord,
    TextureUpload,
    Fence,
    FenceSkipped,
    Method,
    SplitDrain,
    SplitWait,
    // Rust diagnostic extension: distinguish residual SubmitList time without
    // changing upstream zone indices or command execution semantics.
    DmaFetch,
    DmaWait,
    DmaDispatch,
    DmaFlush,
    DmaListEnd,
    ReleaseFences,
    FenceHostWait,
    BufUpdateIndex,
    BufUpdateVertex,
    BufUpdateUniform,
    BufUpdateStorage,
    BindUniform,
    BindStorage,
    Count,
}

pub const ZONE_NAMES: [&str; Zone::Count as usize] = [
    "other",
    "idle",
    "pending sleep",
    "resume pending",
    "push",
    "tick",
    "run",
    "flush/inval",
    "macro",
    "draw",
    "clear",
    "compute",
    "flush work",
    "pipeline lookup",
    "configure",
    "sync descriptors",
    "read handles",
    "fill image views",
    "texture buffers",
    "update buffers",
    "descriptor acquire",
    "bind stage buffers",
    "push descriptors",
    "render targets",
    "configure draw",
    "dynamic state",
    "draw record",
    "texture upload",
    "fence",
    "fence skipped",
    "method",
    "split drain",
    "split wait",
    "dma fetch",
    "dma wait",
    "dma dispatch",
    "dma flush",
    "dma list end",
    "release fences",
    "fence host wait",
    "buf update index",
    "buf update vertex",
    "buf update uniform",
    "buf update storage",
    "bind uniform",
    "bind storage",
];

/// Cumulative shader frontend translation time across all threads, in
/// nanoseconds. Rust diagnostic: decomposes the disk pipeline cache load.
pub static TRANSLATE_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Cumulative Vulkan pipeline creation time across all threads, in nanoseconds.
pub static VKCREATE_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// Rust's process-wide initialization occurs on first use rather than pre-main.
// Presence (including an empty value or "0") enables profiling, as upstream.
static G_ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("EDEN_NX2_GPU_PROFILE").is_some());

pub fn enabled() -> bool {
    #[cfg(test)]
    if let Some(value) = TEST_ENABLED.with(|v| v.get()) {
        return value;
    }
    *G_ENABLED
}

pub fn now() -> u64 {
    #[cfg(test)]
    if let Some(value) = TEST_NOW.with(|v| v.get()) {
        return value;
    }
    common::cpu_features::G_WALL_CLOCK.get_time_ns().as_nanos() as u64
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadData {
    pub exclusive_ns: [u64; Zone::Count as usize],
    pub count: [u64; Zone::Count as usize],
    pub counters: [u64; 8],
    pub child_ns: u64,
    pub window_start: u64,
}
impl Default for ThreadData {
    fn default() -> Self {
        Self {
            exclusive_ns: [0; Zone::Count as usize],
            count: [0; Zone::Count as usize],
            counters: [0; 8],
            child_ns: 0,
            window_start: 0,
        }
    }
}
thread_local! {pub static T_DATA: RefCell<ThreadData> = RefCell::new(ThreadData::default());}

pub struct Scope {
    zone: Zone,
    enabled: bool,
    start: u64,
    parent_child_ns: u64,
    // A scope must exit on the same thread whose counters it entered.
    _thread: PhantomData<Rc<()>>,
}
impl Scope {
    pub fn new(zone: Zone) -> Self {
        let mut scope = Self {
            zone,
            enabled: enabled(),
            start: 0,
            parent_child_ns: 0,
            _thread: PhantomData,
        };
        if !scope.enabled {
            return scope;
        }
        T_DATA.with(|cell| {
            let mut data = cell.borrow_mut();
            scope.parent_child_ns = data.child_ns;
            data.child_ns = 0;
        });
        scope.start = now();
        scope
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }
        let elapsed = now().wrapping_sub(self.start);
        T_DATA.with(|cell| {
            let mut data = cell.borrow_mut();
            let index = self.zone as usize;
            data.exclusive_ns[index] =
                data.exclusive_ns[index].wrapping_add(elapsed.wrapping_sub(data.child_ns));
            data.count[index] = data.count[index].wrapping_add(1);
            data.child_ns = self.parent_child_ns.wrapping_add(elapsed);
        });
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug)]
pub enum Counter {
    DownloadBytes,
    DownloadCopies,
    DmaReadBytes,
    DownloadBytesOnDmaPages,
    MethodWrites,
    SameValueWrites,
    AcquireChecks,
    AcquireWaits,
}

/// Process-wide page set, retained across reports. The guard replaces upstream's
/// GPU-thread-only mutable reference and also makes split-thread access safe.
pub fn dma_read_pages() -> MutexGuard<'static, HashSet<u64>> {
    static PAGES: LazyLock<Mutex<HashSet<u64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
    PAGES.lock()
}

pub fn add(counter: Counter, amount: u64) {
    if enabled() {
        T_DATA.with(|cell| {
            let mut data = cell.borrow_mut();
            let index = counter as usize;
            data.counters[index] = data.counters[index].wrapping_add(amount);
        });
    }
}

/// Call with "GPU thread" for the upstream default; split workers pass their name.
pub fn report(thread_name: &str) {
    if !enabled() {
        return;
    }
    T_DATA.with(|cell| {
        let mut data=cell.borrow_mut();let now=now();
        if data.window_start==0 {data.window_start=now;data.child_ns=0;return;}
        let window=now.wrapping_sub(data.window_start);
        if window<10_000_000_000 {return;}
        let zoned=data.exclusive_ns[1..].iter().fold(0u64,|sum,&ns|sum.wrapping_add(ns));
        data.exclusive_ns[0]=if window>zoned {window-zoned}else{0};
        let mut line=String::new();
        for i in 0..data.exclusive_ns.len() {
            if data.exclusive_ns[i]==0 && data.count[i]==0 {continue;}
            let ms=data.exclusive_ns[i] as f64*1000.0/window as f64;
            let calls=data.count[i] as f64*1e9/window as f64;
            write!(line," | {} {:.1} ms ({:.0}/s)",ZONE_NAMES[i],ms,calls).unwrap();
        }
        write!(line," | buffer downloads {:.0} KiB in {:.0} copies ({:.0} KiB on pages MME DMA reads) | MME DMA reads {:.0} KiB",
            data.counters[0] as f64*1e9/1024.0/window as f64,
            data.counters[1] as f64*1e9/window as f64,
            data.counters[3] as f64*1e9/1024.0/window as f64,
            data.counters[2] as f64*1e9/1024.0/window as f64).unwrap();
        write!(line," | 3D register writes {:.0} ({:.0} same value)",
            data.counters[4] as f64*1e9/window as f64,data.counters[5] as f64*1e9/window as f64).unwrap();
        write!(line," | acquires {:.0} ({:.0} waited)",
            data.counters[6] as f64*1e9/window as f64,data.counters[7] as f64*1e9/window as f64).unwrap();
        log::info!("{} per second:{}",thread_name,line);
        *data=ThreadData::default();data.window_start=now;
    });
}

#[cfg(test)]
thread_local! {
    static TEST_NOW: std::cell::Cell<Option<u64>> = const {std::cell::Cell::new(None)};
    static TEST_ENABLED: std::cell::Cell<Option<bool>> = const {std::cell::Cell::new(None)};
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture;
    impl Fixture {
        fn new(enabled: bool, time: u64) -> Self {
            T_DATA.with(|d| *d.borrow_mut() = ThreadData::default());
            TEST_ENABLED.with(|v| v.set(Some(enabled)));
            TEST_NOW.with(|v| v.set(Some(time)));
            Self
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            TEST_ENABLED.with(|v| v.set(None));
            TEST_NOW.with(|v| v.set(None));
        }
    }
    fn time(value: u64) {
        TEST_NOW.with(|v| v.set(Some(value)));
    }
    fn data() -> ThreadData {
        T_DATA.with(|d| d.borrow().clone())
    }
    #[test]
    fn nested_and_sibling_zones_measure_exclusive_time() {
        let _fixture = Fixture::new(true, 100);
        let parent = Scope::new(Zone::Macro);
        time(110);
        let child = Scope::new(Zone::Method);
        time(115);
        let nested = Scope::new(Zone::Draw);
        time(120);
        drop(nested);
        time(130);
        drop(child);
        time(140);
        let sibling = Scope::new(Zone::Method);
        time(150);
        drop(sibling);
        time(180);
        drop(parent);
        let d = data();
        assert_eq!(d.exclusive_ns[Zone::Macro as usize], 50);
        assert_eq!(d.exclusive_ns[Zone::Method as usize], 25);
        assert_eq!(d.exclusive_ns[Zone::Draw as usize], 5);
        assert_eq!(d.count[Zone::Method as usize], 2);
        assert_eq!(d.child_ns, 80);
    }
    #[test]
    fn report_initializes_then_resets_only_at_ten_seconds_and_keeps_pages() {
        let _fixture = Fixture::new(true, 100);
        add(Counter::DmaReadBytes, 4096);
        T_DATA.with(|d| d.borrow_mut().child_ns = 99);
        report("test");
        assert_eq!(data().window_start, 100);
        assert_eq!(data().child_ns, 0);
        assert_eq!(data().counters[2], 4096);
        time(10_000_000_099);
        report("test");
        assert_eq!(data().counters[2], 4096);
        dma_read_pages().insert(u64::MAX);
        time(10_000_000_100);
        report("test");
        assert_eq!(data().window_start, 10_000_000_100);
        assert_eq!(data().counters, [0; 8]);
        assert_eq!(data().count, [0; Zone::Count as usize]);
        assert!(dma_read_pages().remove(&u64::MAX));
    }
    #[test]
    fn disabled_profiling_leaves_all_data_untouched() {
        let _fixture = Fixture::new(false, 1);
        T_DATA.with(|d| {
            let mut d = d.borrow_mut();
            d.child_ns = 7;
            d.counters[0] = 5;
        });
        let original = data();
        let scope = Scope::new(Zone::Draw);
        time(100);
        drop(scope);
        add(Counter::DownloadBytes, 10);
        report("test");
        assert_eq!(data(), original);
    }
    #[test]
    fn dma_breakdown_excludes_nested_macro_time_from_submission() {
        let _fixture = Fixture::new(true, 100);
        let push = Scope::new(Zone::Push);
        time(110);
        let fetch = Scope::new(Zone::DmaFetch);
        time(130);
        drop(fetch);
        let dispatch = Scope::new(Zone::DmaDispatch);
        time(140);
        let macro_scope = Scope::new(Zone::Macro);
        time(180);
        drop(macro_scope);
        time(190);
        drop(dispatch);
        let flush = Scope::new(Zone::DmaFlush);
        time(200);
        drop(flush);
        drop(push);
        let d = data();
        assert_eq!(d.exclusive_ns[Zone::Push as usize], 10);
        assert_eq!(d.exclusive_ns[Zone::DmaFetch as usize], 20);
        assert_eq!(d.exclusive_ns[Zone::DmaDispatch as usize], 20);
        assert_eq!(d.exclusive_ns[Zone::Macro as usize], 40);
        assert_eq!(d.exclusive_ns[Zone::DmaFlush as usize], 10);
        assert_eq!(d.child_ns, 100);
    }
    #[test]
    fn fence_wait_is_not_charged_to_release_setup_or_dma_dispatch() {
        let _fixture = Fixture::new(true, 100);
        let dispatch = Scope::new(Zone::DmaDispatch);
        time(110);
        let release = Scope::new(Zone::ReleaseFences);
        time(130);
        let wait = Scope::new(Zone::FenceHostWait);
        time(180);
        drop(wait);
        time(190);
        drop(release);
        time(200);
        drop(dispatch);
        let d = data();
        assert_eq!(d.exclusive_ns[Zone::DmaDispatch as usize], 20);
        assert_eq!(d.exclusive_ns[Zone::ReleaseFences as usize], 30);
        assert_eq!(d.exclusive_ns[Zone::FenceHostWait as usize], 50);
        assert_eq!(d.child_ns, 100);
    }

    #[test]
    fn thread_totals_are_isolated_and_counters_wrap() {
        let _fixture = Fixture::new(true, 1);
        add(Counter::MethodWrites, u64::MAX);
        add(Counter::MethodWrites, 2);
        std::thread::spawn(|| {
            let _fixture = Fixture::new(true, 1);
            assert_eq!(data().counters[4], 0);
            add(Counter::MethodWrites, 7);
            assert_eq!(data().counters[4], 7);
        })
        .join()
        .unwrap();
        assert_eq!(data().counters[4], 1);
        assert_eq!(ZONE_NAMES[Zone::SplitWait as usize], "split wait");
    }
}

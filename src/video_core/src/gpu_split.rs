// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Command transport and worker lifecycle from `video_core/gpu_split.h/.cpp`.
//! Session activation is owned by GPUThread::ThreadManager; this module
//! supplies the eligible transport and its scoped frontend access.

use std::cell::UnsafeCell;
use crate::engines::engine_interface::EngineHandle;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use parking_lot::{Condvar, Mutex};

static CHECK_OWNER: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static OWNER_STATE: std::sync::Mutex<std::sync::Weak<GpuSplitShared>> =
    std::sync::Mutex::new(std::sync::Weak::new());

/// Upstream CheckOwner diagnostic; disabled unless EDEN_NX2_SPLIT_CHECK exists.
#[inline]
pub(crate) fn check_owner(where_: &str) {
    #[cfg(windows)]
    if CHECK_OWNER.load(Ordering::Relaxed) { owner_diagnostic::check(where_); }
    #[cfg(not(windows))]
    let _ = where_;
}

#[cfg(windows)]
mod owner_diagnostic {
    use super::*;
    use std::ffi::{c_char, c_void, CStr};
    use std::io::Write;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub(super) fn GetCurrentThreadId() -> u32;
        fn RtlCaptureStackBackTrace(skip: u32, count: u32, frames: *mut *mut c_void,
            hash: *mut u32) -> u16;
        fn GetModuleHandleExA(flags: u32, address: *const c_char, module: *mut *mut c_void) -> i32;
        fn GetModuleFileNameA(module: *mut c_void, name: *mut c_char, size: u32) -> u32;
    }

    pub(super) fn check(where_: &str) {
        let owner = OWNER_STATE.lock().unwrap().upgrade();
        let Some(owner) = owner else { return; };
        if !owner.back_busy.load(Ordering::Acquire) { return; }
        let thread = unsafe { GetCurrentThreadId() };
        if thread == owner.back_thread_id.load(Ordering::Relaxed) { return; }
        static REPORTS: AtomicU32 = AtomicU32::new(0);
        if REPORTS.fetch_add(1, Ordering::Relaxed) >= 12 { return; }
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "GPU split owner check: {where_} on thread {thread} while the back runs");
        let mut frames = [std::ptr::null_mut(); 40];
        let count = unsafe { RtlCaptureStackBackTrace(1, 40, frames.as_mut_ptr(), std::ptr::null_mut()) };
        for &frame in &frames[..count as usize] {
            let mut module = std::ptr::null_mut();
            let mut name = [0 as c_char; 260];
            name[0] = b'?' as c_char;
            // FROM_ADDRESS | UNCHANGED_REFCOUNT, matching upstream.
            if unsafe { GetModuleHandleExA(0x4 | 0x2, frame.cast(), &mut module) } != 0 {
                unsafe { GetModuleFileNameA(module, name.as_mut_ptr(), name.len() as u32); }
                name[259] = 0;
            }
            let name = unsafe { CStr::from_ptr(name.as_ptr()) }.to_string_lossy();
            let base = name.rsplit('\\').next().unwrap_or(&name);
            let _ = writeln!(stderr, "  frame {base}+{:#x}", (frame as usize).wrapping_sub(module as usize));
        }
        let _ = stderr.flush();
    }
}

/// Front-thread access to GpuSplit::Get. Each transport operation borrows the
/// producer only for that operation, never across a macro, scheduler dispatch
/// or cache callback. Rc makes this handle statically confined to its owner.
#[derive(Clone)]
pub(crate) struct GpuSplitFrontend(std::rc::Rc<FrontendState>);

struct FrontendState {
    transport: std::cell::RefCell<GpuSplit>,
    front_macros: bool,
    /// GPU page translations note_write looked up, by page (Rust adaptation, see there).
    note_cache: std::cell::RefCell<[NoteCacheEntry; NOTE_CACHE_ENTRIES]>,
}

const NOTE_CACHE_ENTRIES: usize = 8;

/// One translation of note_write's cache: the 4 KiB GPU `page` of the manager `owner`
/// maps to `device` (None: nothing mapped there) while its map_serial is `serial`. The Weak
/// keeps the manager's allocation, so its address cannot name another manager later.
struct NoteCacheEntry {
    owner: std::sync::Weak<Mutex<crate::memory_manager::MemoryManager>>,
    serial: u64,
    page: u64,
    device: Option<u64>,
}

thread_local! {
    static CURRENT_FRONT: std::cell::RefCell<Option<GpuSplitFrontend>> = const {
        std::cell::RefCell::new(None)
    };
}

/// A scoped replacement for upstream's process-global singleton. No backend or
/// unrelated host thread sees another session's producer through this accessor.
pub(crate) fn current() -> Option<GpuSplitFrontend> {
    CURRENT_FRONT.with(|slot| slot.borrow().clone())
}

pub(crate) struct CurrentGuard(GpuSplitFrontend);
impl Drop for CurrentGuard {
    fn drop(&mut self) {
        CURRENT_FRONT.with(|slot| {
            let current = slot.borrow_mut().take().expect("GPU split scope was removed early");
            assert!(std::rc::Rc::ptr_eq(&current.0, &self.0.0), "GPU split scope changed owner");
        });
    }
}

// Mechanical extraction of MaybeStart's mode/settings predicates for testing
// without mutating process-global settings or starting a real kernel session.
fn eligible_mode(mode: u8, nx2: bool, values: &common::settings::Values, jit: bool) -> Option<bool> {
    use common::settings;
    if !matches!(mode, b'1' | b'2') || !nx2 { return None; }
    let high = settings::is_gpu_level_high(values);
    let safe_reads = if settings::is_dma_level_default(values) { high }
        else { settings::is_dma_level_safe(values) };
    if safe_reads || high || *values.renderer_backend.get_value()
        != common::settings_enums::RendererBackend::Vulkan
    { return None; }
    Some(cfg!(target_arch = "x86_64") && mode == b'2' && jit)
}

impl GpuSplitFrontend {
    pub(crate) fn new(front_macros: bool) -> Self {
        Self(std::rc::Rc::new(FrontendState {
            transport: std::cell::RefCell::new(GpuSplit::new()), front_macros,
            note_cache: std::cell::RefCell::new(std::array::from_fn(|_| NoteCacheEntry {
                owner: std::sync::Weak::new(), serial: 0, page: 0, device: None })),
        }))
    }

    /// Port of MaybeStart's eligibility and kernel registration. Caller retains
    /// the result and installs it only on the frontend thread. Runtime call sites
    /// are added together with DMA/puller/cache routing, not independently.
    pub(crate) fn maybe_start(system: ruzu_core::core::SystemRef,
        on_failure: &Arc<dyn Fn() + Send + Sync>) -> Option<Self> {
        if current().is_some() { return None; }
        static MODE: std::sync::LazyLock<u8> = std::sync::LazyLock::new(|| {
            // As Lichii MaybeStart (gpu_split.cpp:150-154): mode '2' when
            // EDEN_NX2_SPLIT is absent; EDEN_NX2_SPLIT=0 runs everything on
            // one thread. ruzu defaulted to '0' while its front waited for the
            // back at every macro DMA read (Mario Kart World start grid, Low
            // accuracy: 21.24 FPS at mode 0 versus 19.53/17.63 FPS at mode 2);
            // the FrontRunOnBack/FrontDmaRead port removed those waits. Mode 2
            // has not been measured again on that title since.
            std::env::var_os("EDEN_NX2_SPLIT").map_or(b'2', |value| {
                value.to_string_lossy().as_bytes().first().copied().unwrap_or(0)
            })
        });
        // Match upstream's cheap inactive gate before settings/environment
        // work. In particular Switch1 must not take the settings mutex or read
        // MME_JIT on every iteration of its GPU loop.
        if !matches!(*MODE, b'1' | b'2')
            || !crate::nx2::G_NX2_AMPERE.load(Ordering::Relaxed) { return None; }
        let front_macros = eligible_mode(*MODE, true, &common::settings::values(), true)?;
        let front_macros = front_macros && std::env::var_os("EDEN_NX2_MME_JIT")
            .is_none_or(|value| !value.to_string_lossy().starts_with('0'));
        CHECK_OWNER.store(std::env::var_os("EDEN_NX2_SPLIT_CHECK").is_some(), Ordering::Relaxed);
        let front = Self::new(front_macros);
        let failure = on_failure.clone();
        front.start_back_with_failure(
            move || { system.get().kernel().unwrap().register_host_thread(); },
            move || failure());
        log::info!("GPU thread split: engine methods run on a second thread{}",
            if front_macros { ", macros on the first" } else { "" });
        Some(front)
    }

    pub(crate) fn install(&self) -> CurrentGuard {
        CURRENT_FRONT.with(|slot| {
            let mut current = slot.borrow_mut();
            assert!(current.is_none(), "a GPU split session is already installed");
            *current = Some(self.clone());
        });
        CurrentGuard(self.clone())
    }

    pub(crate) fn front_macros(&self) -> bool { self.0.front_macros }

    /// After removing the frontend scope, move the transport out so the owning
    /// ThreadManager can first join the front and then stop/join the back.
    pub(crate) fn into_transport(self) -> GpuSplit {
        let state = std::rc::Rc::try_unwrap(self.0)
            .unwrap_or_else(|_| panic!("GPU split frontend handles survived scope teardown"));
        state.transport.into_inner()
    }

    pub(crate) fn start_back(&self, register: impl FnOnce() + Send + 'static) {
        self.0.transport.borrow_mut().start_back(register);
    }
    pub(crate) fn start_back_with_failure(&self, register: impl FnOnce() + Send + 'static,
        on_failure: impl FnOnce() + Send + 'static) {
        self.0.transport.borrow_mut().start_back_with_failure(register, on_failure);
    }
    pub(crate) fn stop(&self) -> std::thread::Result<()> { self.0.transport.borrow_mut().stop() }
    pub(crate) fn publish(&self) { self.0.transport.borrow_mut().publish(); }
    pub(crate) fn drain(&self) { self.0.transport.borrow_mut().drain(); }
    pub(crate) fn method(&self, engine: EngineHandle, method: u32, argument: u32,
        is_last: bool, segment: Option<u64>, writes: Writes)
    { self.0.transport.borrow_mut().method(engine, method, argument, is_last, segment, writes); }
    pub(crate) fn macro_write(&self, engine: EngineHandle, method: u32, argument: u32,
        writes: Writes) {
        self.0.transport.borrow_mut().macro_write(engine, method, argument, writes);
    }
    pub(crate) fn multi_method(&self, engine: EngineHandle, method: u32, words: &[u32],
        amount: u32, methods_pending: u32, segment: Option<u64>, writes: Writes)
    { self.0.transport.borrow_mut().multi_method(engine, method, words, amount, methods_pending, segment, writes); }
    pub(crate) fn defer<F: Fn() + Copy + Send + 'static>(&self, func: F) {
        self.0.transport.borrow_mut().defer(func);
    }
    pub(crate) fn defer_words<F: Fn(&[u32]) + Copy + Send + 'static>(&self, func: F, words: &[u32]) {
        self.0.transport.borrow_mut().defer_words(func, words);
    }
    pub(crate) fn defer_function(&self, func: Box<dyn FnOnce() + Send>) {
        self.0.transport.borrow_mut().defer_function(func);
    }
    /// Port of GpuSplit::Run: runs `func` on the back and waits for it. This
    /// handle exists only on the front of an active split, so upstream's
    /// on-back/inactive shortcut has no caller here. Rust takes an owned
    /// callback (DeferFunction) instead of a trivially copyable one so it can
    /// keep Arc owners such as the MemoryManager alive.
    pub(crate) fn run(&self, func: Box<dyn FnOnce() + Send>) {
        let mut transport = self.0.transport.borrow_mut();
        transport.defer_function(func);
        transport.drain();
    }
    pub(crate) fn position(&self) -> u64 { self.0.transport.borrow().position() }
    pub(crate) fn wait_until(&self, position: u64) -> bool {
        self.0.transport.borrow_mut().wait_until(position)
    }
    pub(crate) fn note_any_write(&self) { self.0.transport.borrow_mut().note_any_write(); }
    /// Port of GpuSplit::NoteWrite. The MemoryManager lock is held only to
    /// translate the range, never while waiting for the back.
    pub(crate) fn note_write(&self, memory: &Arc<Mutex<crate::memory_manager::MemoryManager>>,
        address: u64, size: u64)
    {
        let mut transport = self.0.transport.borrow_mut();
        if size > MAX_NOTED_BYTES || address.checked_add(size).is_none() {
            transport.note_any_write();
            return;
        }
        // Rust adaptation: Lichii's ForEachDeviceRange reads the page table unlocked. ruzu
        // keeps the translations of the last noted pages, valid while the manager's
        // map_serial is unchanged, and takes the manager's lock only to translate a page
        // it has not noted since the last page table change (most noted writes are
        // constant buffer words, page after page of the same buffer).
        let serial = crate::memory_manager::MemoryManager::map_serial_unlocked(memory);
        let owner = Arc::as_ptr(memory);
        let mut cache = self.0.note_cache.borrow_mut();
        let mut guard = None;
        const PAGE_SIZE: u64 = 0x1000;
        let end = address + size;
        let mut begin = address;
        while begin < end {
            let page = begin & !(PAGE_SIZE - 1);
            let page_end = (page + PAGE_SIZE).min(end);
            let slot = &mut cache[((page >> 12) & (NOTE_CACHE_ENTRIES as u64 - 1)) as usize];
            if !(std::ptr::eq(slot.owner.as_ptr(), owner) && slot.serial == serial && slot.page == page) {
                let locked = guard.get_or_insert_with(|| memory.lock());
                // Under the lock, the serial and the table agree.
                *slot = NoteCacheEntry { owner: Arc::downgrade(memory), serial: locked.map_serial(),
                    page, device: locked.gpu_to_cpu_address(page) };
            }
            if let Some(device) = slot.device {
                transport.note_device_write(device + (begin - page), page_end - begin);
            }
            begin = page_end;
        }
    }
    /// Port of GpuSplit::WaitForWrites. Rust computes the target under the
    /// MemoryManager lock and releases it before waiting: the back takes that
    /// lock under the buffer cache's, so holding it here could deadlock.
    pub(crate) fn wait_for_writes(&self, memory: &Mutex<crate::memory_manager::MemoryManager>,
        address: u64, size: u64) -> bool
    {
        let target = {
            let memory = memory.lock();
            self.0.transport.borrow().writes_target(&memory, address, size)
        };
        let mut transport = self.0.transport.borrow_mut();
        let target = target.min(transport.write_position);
        transport.wait_for(target)
    }
}

/// Port of GpuSplit::Writes: what the sender of an entry tells about the
/// guest memory the back writes running it, for the front's reads of guest
/// memory (WaitForWrites).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Writes {
    /// Unknown: a read anywhere waits for the entry.
    Anywhere,
    /// Nothing, or what the sender notes right after sending it (NoteWrite).
    Noted,
}

/// Rust adaptation of the engines' `std::unique_ptr<FrontState> front`
/// (Lichii maxwell_3d.h / kepler_compute.h): the front's copy of an engine's
/// state, which upstream reads and writes without a lock because only the
/// split's front thread touches it. ruzu shares the allocation between the
/// engine (so a rebinding finds it again) and the DmaPusher's per-subchannel
/// cache, hence the Arc; a Mutex there cost two locked instructions on every
/// method the front handled.
pub(crate) struct FrontOwned<T>(UnsafeCell<T>);
// SAFETY: the value is only reached through `get`, whose caller proves with a
// GpuSplitFrontend (an Rc, so !Send, installed on the front thread only) that
// it runs on the split's front thread; the value itself is Send.
unsafe impl<T: Send> Send for FrontOwned<T> {}
unsafe impl<T: Send> Sync for FrontOwned<T> {}
impl<T> FrontOwned<T> {
    pub(crate) fn new(value: T) -> Self { Self(UnsafeCell::new(value)) }

    /// The front's state, for the duration of one front call.
    ///
    /// # Safety
    /// No other reference returned by `get` for this value may be alive: the
    /// front calls (FrontCallMethod, FrontCallMultiMethod) take it once at
    /// their start and never re-enter one another for the same engine.
    #[allow(clippy::mut_from_ref)]
    #[inline(always)]
    pub(crate) unsafe fn get(&self, _front_thread: &GpuSplitFrontend) -> &mut T {
        unsafe { &mut *self.0.get() }
    }
}

/// Port of gpu_split.cpp ForEachDeviceRange: calls `func(device_address,
/// size)` for each mapped piece of [address, address + size) within one 4 KiB
/// GPU page; nothing is read or written where no device memory is mapped.
pub(crate) fn for_each_device_range(memory: &crate::memory_manager::MemoryManager,
    address: u64, size: u64, mut func: impl FnMut(u64, u64))
{
    const PAGE_SIZE: u64 = 0x1000;
    let end = address.wrapping_add(size);
    let mut begin = address;
    while begin < end {
        let page_end = ((begin & !(PAGE_SIZE - 1)) + PAGE_SIZE).min(end);
        if let Some(device) = memory.gpu_to_cpu_address(begin) {
            func(device, page_end - begin);
        }
        begin = page_end;
    }
}

const RING_WORDS: usize = 1 << 20;
/// NoteWrite places writes by 256-byte granule of device memory, up to
/// MAX_NOTED_BYTES long (longer ones count as anywhere), in write_positions
/// slots hashed by granule: granules that share a slot share its position, so
/// a read may wait for a write elsewhere, never miss one.
const WRITE_GRANULE_BITS: u32 = 8;
const MAX_NOTED_BYTES: u64 = 0x4000;
const WRITE_SLOT_BITS: u32 = 10;

fn write_slot(granule: u64) -> usize {
    (granule.wrapping_mul(0x9E3779B97F4A7C15) >> (64 - WRITE_SLOT_BITS)) as usize
}
const RING_MASK: usize = RING_WORDS - 1;
const PUBLISH_EVERY: usize = 256;
const MAX_MULTI_WORDS: usize = 8192;
const IS_LAST: u64 = 1 << 4;
const HAS_SEGMENT: u64 = 1 << 5;
const DIRECT: u64 = 1 << 6;
// Rust host-only Call flag: payload owns a heap FnOnce and needs cancellation.
// Bit4 is otherwise unused by Call (Method uses it for IsLast).
const OWNED_CALL: u64 = 1 << 4;
type OwnedCallback = Box<dyn FnOnce() + Send>;
type OwnedCallbackPtr = *mut OwnedCallback;
const _: () = assert!(std::mem::size_of::<OwnedCallbackPtr>() <= 8);
// Rust trait-object handles include the vtable. This is a host-only packet;
// copy the complete typed value rather than assuming the C++ pointer size.
const ENGINE_WORDS: usize = std::mem::size_of::<EngineHandle>().div_ceil(8);
const _: () = assert!(std::mem::align_of::<EngineHandle>() <= std::mem::align_of::<u64>());

fn pause() {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    std::hint::spin_loop();
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    std::thread::yield_now();
}

#[repr(u64)]
#[derive(Clone, Copy)]
enum Kind { Wrap, Engine, Method, Multi, Call }

#[repr(align(64))]
struct CacheLine<T>(T);

/// Rust adaptation of std::atomic<u32>::wait/notify_one in GpuSplit.
/// Notification and the final predicate check share a mutex, closing the gap
/// between observing an unchanged generation and entering Condvar::wait.
struct Wake {
    value: AtomicU32,
    guard: Mutex<()>,
    cv: Condvar,
}
impl Wake {
    fn new() -> Self {
        Self { value: AtomicU32::new(0), guard: Mutex::new(()), cv: Condvar::new() }
    }
    fn notify_one(&self) {
        self.value.fetch_add(1, Ordering::SeqCst);
        let _guard = self.guard.lock();
        self.cv.notify_one();
    }
    fn wait(&self, seen: u32) {
        let mut guard = self.guard.lock();
        while self.value.load(Ordering::SeqCst) == seen {
            self.cv.wait(&mut guard);
        }
    }
}

/// Shared fields of upstream GpuSplit, separated only to give the backend an
/// Arc without sharing mutable access to the frontend's positions.
struct GpuSplitShared {
    ring: Box<[UnsafeCell<u64>]>,
    published: CacheLine<AtomicU64>,
    consumed: CacheLine<AtomicU64>,
    /// A position the front waits for (WaitFor): the back stores `consumed`
    /// once past it.
    wait_target: CacheLine<AtomicU64>,
    back_waiting: CacheLine<AtomicBool>,
    wake: Wake,
    /// Lichii front_waiting / front_wake: a front sleeping in Backoff, and
    /// what the back bumps after storing `consumed` while it sleeps.
    front_waiting: CacheLine<AtomicBool>,
    front_wake: Wake,
    owned_pending: AtomicUsize,
    stop_requested: AtomicBool,
    back_failed: AtomicBool,
    back_busy: AtomicBool,
    #[cfg(windows)]
    back_thread_id: AtomicU32,
}
// SAFETY: single producer writes only reserved, unconsumed-free words. It
// publishes after writing (SeqCst); the sole consumer acquires publication
// before reading and releases consumed only after all reads finish. Neither
// side forms references spanning words owned by the other side.
unsafe impl Sync for GpuSplitShared {}
impl GpuSplitShared {
    fn new() -> Self {
        Self {
            ring: (0..RING_WORDS).map(|_| UnsafeCell::new(0)).collect(),
            published: CacheLine(AtomicU64::new(0)),
            consumed: CacheLine(AtomicU64::new(0)),
            wait_target: CacheLine(AtomicU64::new(u64::MAX)),
            back_waiting: CacheLine(AtomicBool::new(false)),
            wake: Wake::new(),
            front_waiting: CacheLine(AtomicBool::new(false)),
            front_wake: Wake::new(),
            owned_pending: AtomicUsize::new(0),
            stop_requested: AtomicBool::new(false),
            back_failed: AtomicBool::new(false),
            back_busy: AtomicBool::new(false),
            #[cfg(windows)]
            back_thread_id: AtomicU32::new(0),
        }
    }
    fn at(&self, index: usize) -> *mut u64 {
        self.ring[index].get()
    }

    /// Port of GpuSplit::NotifyFront (Lichii gpu_split.h:481-487): after the
    /// back stored `consumed` (SeqCst), wakes a front sleeping in Backoff.
    fn notify_front(&self) {
        if self.front_waiting.0.load(Ordering::SeqCst) {
            self.front_wake.notify_one();
        }
    }
}

type CallThunk = unsafe fn(*const u64);
const _: () = assert!(std::mem::size_of::<CallThunk>() <= 8);

/// Backend-owned fields of upstream GpuSplit. Separating them avoids sharing
/// a mutable borrow of the frontend's positions with the consumer.
struct GpuSplitBack {
    shared: Arc<GpuSplitShared>,
    back_engine: Option<EngineHandle>,
}
impl GpuSplitBack {
    fn back_loop(&mut self) {
        let mut read_position = 0u64;
        while !self.shared.stop_requested.load(Ordering::Acquire) {
            crate::nx2_profile::report("GPU back thread");
            let mut available = self.shared.published.0.load(Ordering::Acquire);
            if available == read_position {
                let _scope = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::SplitWait);
                for _ in 0..8192 {
                    if available != read_position { break; }
                    pause();
                    available = self.shared.published.0.load(Ordering::Acquire);
                }
                if available == read_position {
                    self.shared.back_waiting.0.store(true, Ordering::SeqCst);
                    let seen = self.shared.wake.value.load(Ordering::SeqCst);
                    available = self.shared.published.0.load(Ordering::SeqCst);
                    if available == read_position && !self.shared.stop_requested.load(Ordering::Acquire) {
                        self.shared.wake.wait(seen);
                    }
                    self.shared.back_waiting.0.store(false, Ordering::Relaxed);
                    continue;
                }
            }
            let mut last_store = read_position;
            self.shared.back_busy.store(true, Ordering::Relaxed);
            while read_position != available {
                let index = read_position as usize & RING_MASK;
                // The acquire above publishes complete producer-written packets.
                // Consumed is released only after Execute finishes each packet.
                let size = unsafe { self.execute(index) };
                read_position += if size == 0 { (RING_WORDS - index) as u64 } else { size as u64 };
                // Hand ring space back now and then while working through a
                // long batch, and at once when the front waits for this
                // position (WaitFor).
                if (read_position - last_store >= 4096
                    || read_position >= self.shared.wait_target.0.load(Ordering::Relaxed))
                    && read_position != available
                {
                    // SeqCst as upstream: NotifyFront's front_waiting load
                    // pairs with Backoff's store-then-load of consumed.
                    self.shared.consumed.0.store(read_position, Ordering::SeqCst);
                    last_store = read_position;
                    self.shared.notify_front();
                }
            }
            self.shared.back_busy.store(false, Ordering::Release);
            self.shared.consumed.0.store(read_position, Ordering::SeqCst);
            self.shared.notify_front();
        }
    }

    /// Port of GpuSplit::Execute.
    ///
    /// SAFETY: index starts a complete acquired/published packet. Its storage
    /// is not released to the producer until this call returns. All engine
    /// handles and captured callback resources remain alive; the backend has
    /// exclusive access to engine state while it executes commands.
    unsafe fn execute(&mut self, index: usize) -> usize {
        let entry = self.shared.at(index);
        let head = unsafe { entry.read() };
        match head & 0xf {
            0 => 0,
            1 => {
                self.back_engine = Some(unsafe { entry.add(1).cast::<EngineHandle>().read() });
                1 + ENGINE_WORDS
            }
            2 => {
                let method = ((head >> 8) & 0xffff) as u32;
                let argument = (head >> 32) as u32;
                let engine = unsafe { self.back_engine.expect("method requires selected engine").as_mut() };
                // One interface_state access replaces the per-packet mask/sink
                // trait calls; upstream reads these public members directly.
                if head & DIRECT != 0 {
                    let _scope = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::Method);
                    if !engine.interface_state().method_sink.is_empty() {
                        engine.consume_sink_impl();
                    }
                    engine.call_method(method, argument, true);
                } else {
                    let sink_pending = {
                        let state = engine.interface_state_mut();
                        if !state.execution_mask[method as usize] {
                            state.method_sink.push((method, argument));
                            return if head & HAS_SEGMENT != 0 { 2 } else { 1 };
                        }
                        !state.method_sink.is_empty()
                    };
                    let _scope = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::Method);
                    if sink_pending {
                        engine.consume_sink_impl();
                    }
                    if head & HAS_SEGMENT != 0 {
                        engine.interface_state_mut().current_dma_segment =
                            unsafe { entry.add(1).read() };
                    }
                    engine.call_method(method, argument, head & IS_LAST != 0);
                }
                if head & HAS_SEGMENT != 0 { 2 } else { 1 }
            }
            3 => {
                let method = ((head >> 8) & 0xffff) as u32;
                let amount = (head >> 32) as u32;
                let data = if head & HAS_SEGMENT != 0 { 3 } else { 2 };
                let engine = unsafe { self.back_engine.expect("multi method requires selected engine").as_mut() };
                let _scope = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::Method);
                if !engine.interface_state().method_sink.is_empty() {
                    engine.consume_sink_impl();
                }
                if head & HAS_SEGMENT != 0 {
                    engine.interface_state_mut().current_dma_segment =
                        unsafe { entry.add(2).read() };
                }
                let words = unsafe { std::slice::from_raw_parts(entry.add(data).cast::<u32>(), amount as usize) };
                engine.call_multi_method(method, words, amount, unsafe { entry.add(1).read() } as u32);
                data + (amount as usize).div_ceil(2)
            }
            4 => {
                // Diagnostic only: callbacks include synchronization not charged
                // to engine Method scopes. Keep their nested zones exclusive.
                let _scope = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::SplitCallback);
                let trace_start = crate::nx2_profile::enabled().then(std::time::Instant::now);
                let payload = ((head >> 8) & 0xffffff) as usize;
                let thunk = unsafe { entry.add(1).cast::<CallThunk>().read() };
                if head & OWNED_CALL == 0 {
                    unsafe { thunk(entry.add(2)); }
                } else if !unsafe { entry.add(2).cast::<OwnedCallbackPtr>().read() }.is_null() {
                    self.shared.owned_pending.fetch_sub(1, Ordering::Relaxed);
                    // The owned thunk clears its slot before calling user code.
                    // A panic cannot leave a stale pointer for cancellation.
                    unsafe { thunk(entry.add(2)); }
                }
                if let Some(start) = trace_start {
                    let elapsed = start.elapsed();
                    if elapsed >= std::time::Duration::from_millis(5) {
                        // Resolve this code address against the running binary's
                        // mappings; never log callback payloads or guest data.
                        log::info!("GPU split slow callback {:p}: {} us", thunk as *const (), elapsed.as_micros());
                    }
                }
                2 + payload
            }
            _ => 1,
        }
    }
}

/// Front-owned state of upstream GpuSplit. The transport methods keep their
/// upstream owner and update order. No worker is started by construction.
pub(crate) struct GpuSplit {
    shared: Arc<GpuSplitShared>,
    write_position: u64,
    published_local: u64,
    consumed_cache: u64,
    publish_every: usize,
    front_engine: Option<EngineHandle>,
    back_thread: Option<std::thread::JoinHandle<()>>,
    /// After the last entry that may write guest memory anywhere
    /// (NoteAnyWrite), the last one NoteWrite placed, and the last placed
    /// write in each slot's granules.
    last_any_write: u64,
    last_noted_write: u64,
    write_positions: Box<[u64; 1 << WRITE_SLOT_BITS]>,
}
impl GpuSplit {
    pub(crate) fn new() -> Self {
        Self { shared: Arc::new(GpuSplitShared::new()), write_position: 0,
            published_local: 0, consumed_cache: 0, publish_every: PUBLISH_EVERY, front_engine: None, back_thread: None,
            last_any_write: 0, last_noted_write: 0, write_positions: Box::new([0; 1 << WRITE_SLOT_BITS]) }
    }

    /// Thread-creation portion of upstream MaybeStart. The registration action
    /// is supplied by the owning session (and by headless tests); it runs at
    /// the same point as system->RegisterHostThread in upstream BackLoop.
    /// Eligibility/caller integration remains outside this implementation slice.
    pub(crate) fn start_back(&mut self, register_host_thread: impl FnOnce() + Send + 'static) {
        self.start_back_with_failure(register_host_thread, || {});
    }

    // Rust unwind recovery: the session must wake frontend/host waiters after
    // a consumer failure, including when the frontend is waiting on its queue.
    fn start_back_with_failure(&mut self, register_host_thread: impl FnOnce() + Send + 'static,
        on_failure: impl FnOnce() + Send + 'static) {
        assert!(self.back_thread.is_none() && self.write_position == 0
            && !self.shared.stop_requested.load(Ordering::Relaxed),
            "GPU split starts with fresh per-session state");
        let shared = self.shared.clone();
        #[cfg(windows)]
        if CHECK_OWNER.load(Ordering::Relaxed) {
            *OWNER_STATE.lock().unwrap() = Arc::downgrade(&shared);
        }
        self.back_thread = Some(std::thread::Builder::new().name("GPUBack".into()).spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                common::thread::set_current_thread_name("GPUBack");
                common::thread::set_current_thread_priority(common::thread::ThreadPriority::Critical);
                common::thread::set_current_thread_to_performance_cores();
                register_host_thread();
                #[cfg(windows)]
                shared.back_thread_id.store(unsafe { owner_diagnostic::GetCurrentThreadId() }, Ordering::Relaxed);
                GpuSplitBack { shared: shared.clone(), back_engine: None }.back_loop();
            }));
            if let Err(panic) = outcome {
                shared.back_busy.store(false, Ordering::Release);
                shared.back_failed.store(true, Ordering::Release);
                shared.stop_requested.store(true, Ordering::Release);
                shared.wake.notify_one();
                // As upstream's back_running = false at BackLoop's end: a front
                // sleeping in Backoff wakes and sees the failure.
                shared.front_wake.notify_one();
                on_failure();
                std::panic::resume_unwind(panic);
            }
        }).expect("failed to start GPU back thread"));
    }

    pub(crate) fn stop(&mut self) -> std::thread::Result<()> {
        let result = if let Some(thread) = self.back_thread.take() {
            self.shared.stop_requested.store(true, Ordering::Release);
            self.shared.wake.notify_one();
            thread.join()
        } else { Ok(()) };
        // Upstream abandons the remaining packets. Rust must additionally drop
        // captured owned resources, only after the consumer is no longer alive.
        self.discard_pending();
        result
    }

    fn check_back_failure(&self) {
        assert!(!self.shared.back_failed.load(Ordering::Acquire), "GPU split backend panicked");
        assert!(!self.shared.stop_requested.load(Ordering::Acquire), "GPU split has stopped");
    }

    fn select_engine(&mut self, engine: EngineHandle) {
        if !self.front_engine.is_some_and(|current| current.ptr_eq(engine)) {
            self.switch_engine(engine);
        }
    }

    fn switch_engine(&mut self, engine: EngineHandle) {
        self.front_engine = Some(engine);
        let entry = self.reserve(1 + ENGINE_WORDS);
        // SAFETY: producer owns this unpublished contiguous reservation;
        // payload alignment is checked above. A typed write preserves pointer
        // provenance and dynamic dispatch metadata without integer conversion.
        unsafe {
            entry.write(Kind::Engine as u64);
            entry.add(1).cast::<EngineHandle>().write(engine);
        }
        self.commit(1 + ENGINE_WORDS);
    }

    pub(crate) fn method(&mut self, engine: EngineHandle, method: u32, argument: u32,
              is_last: bool, segment: Option<u64>, writes: Writes) {
        self.select_engine(engine);
        let words = if segment.is_some() { 2 } else { 1 };
        let entry = self.reserve(words);
        unsafe {
            entry.write(Kind::Method as u64 | if is_last { IS_LAST } else { 0 }
                | if segment.is_some() { HAS_SEGMENT } else { 0 }
                | ((method as u64 & 0xffff) << 8) | ((argument as u64) << 32));
            if let Some(segment) = segment { entry.add(1).write(segment); }
        }
        self.commit(words);
        if writes == Writes::Anywhere { self.note_any_write(); }
    }

    pub(crate) fn macro_write(&mut self, engine: EngineHandle, method: u32, argument: u32,
        writes: Writes) {
        self.select_engine(engine);
        let entry = self.reserve(1);
        unsafe {
            entry.write(Kind::Method as u64 | IS_LAST | DIRECT
                | ((method as u64 & 0xffff) << 8) | ((argument as u64) << 32));
        }
        self.commit(1);
        if writes == Writes::Anywhere { self.note_any_write(); }
    }

    pub(crate) fn multi_method(&mut self, engine: EngineHandle, method: u32, words: &[u32],
                    amount: u32, mut methods_pending: u32, segment: Option<u64>, writes: Writes) {
        self.select_engine(engine);
        let mut words = &words[..amount as usize];
        while !words.is_empty() {
            let piece = words.len().min(MAX_MULTI_WORDS);
            let head = if segment.is_some() { 3 } else { 2 };
            let size = head + piece.div_ceil(2);
            let entry = self.reserve(size);
            // SAFETY: exactly piece source words are readable and the reserved
            // payload has ceil(piece/2) u64 words; source and ring are disjoint.
            unsafe {
                entry.write(Kind::Multi as u64 | if segment.is_some() { HAS_SEGMENT } else { 0 }
                    | ((method as u64 & 0xffff) << 8) | ((piece as u64) << 32));
                entry.add(1).write(methods_pending as u64);
                if let Some(segment) = segment { entry.add(2).write(segment); }
                entry.add(size - 1).write(0);
                std::ptr::copy_nonoverlapping(words.as_ptr(), entry.add(head).cast::<u32>(), piece);
            }
            self.commit(size);
            words = &words[piece..];
            methods_pending = methods_pending.wrapping_sub(piece as u32);
        }
        if writes == Writes::Anywhere { self.note_any_write(); }
    }

    /// Upstream's trivially-copyable deferred callback. Copy excludes owned
    /// destructors; Send and 'static express the cross-thread lifetime contract.
    pub(crate) fn defer<F: Fn() + Copy + Send + 'static>(&mut self, func: F) {
        assert!(std::mem::align_of::<F>() <= std::mem::align_of::<u64>());
        let payload = std::mem::size_of::<F>().div_ceil(8);
        let entry = self.reserve(2 + payload);
        unsafe {
            entry.write(Kind::Call as u64 | ((payload as u64) << 8));
            entry.add(1).cast::<CallThunk>().write(Self::call_thunk::<F>);
            entry.add(2).cast::<F>().write(func);
        }
        self.commit(2 + payload);
        self.note_any_write();
    }

    unsafe fn call_thunk<F: Fn() + Copy>(payload: *const u64) {
        let func = unsafe { payload.cast::<F>().read() };
        func();
    }

    /// Port of GpuSplit::DeferWords: Defer, calling `func` with a copy of
    /// `words`. Writes::Anywhere, upstream's default.
    pub(crate) fn defer_words<F: Fn(&[u32]) + Copy + Send + 'static>(&mut self, func: F,
        words: &[u32])
    {
        assert!(std::mem::align_of::<F>() <= std::mem::align_of::<u64>());
        let function_words = std::mem::size_of::<F>().div_ceil(8);
        let payload = function_words + 1 + words.len().div_ceil(2);
        let entry = self.reserve(2 + payload);
        // SAFETY: the reservation holds 2 + payload words: head, thunk, the
        // function, the count and the words, padded after an odd count.
        unsafe {
            entry.write(Kind::Call as u64 | ((payload as u64) << 8));
            entry.add(1).cast::<CallThunk>().write(Self::call_words_thunk::<F>);
            entry.add(2).cast::<F>().write(func);
            entry.add(2 + function_words).write(words.len() as u64);
            if !words.is_empty() {
                entry.add(1 + payload).write(0); // Padding after an odd count
                std::ptr::copy_nonoverlapping(words.as_ptr(),
                    entry.add(3 + function_words).cast::<u32>(), words.len());
            }
        }
        self.commit(2 + payload);
        self.note_any_write();
    }

    unsafe fn call_words_thunk<F: Fn(&[u32]) + Copy>(payload: *const u64) {
        let function_words = std::mem::size_of::<F>().div_ceil(8);
        let func = unsafe { payload.cast::<F>().read() };
        let count = unsafe { payload.add(function_words).read() } as usize;
        let words = unsafe {
            std::slice::from_raw_parts(payload.add(function_words + 1).cast::<u32>(), count)
        };
        func(words);
    }

    fn defer_function(&mut self, func: OwnedCallback) {
        let entry = self.reserve(3);
        let heap = Box::into_raw(Box::new(func));
        unsafe {
            entry.write(Kind::Call as u64 | OWNED_CALL | (1 << 8));
            entry.add(1).cast::<CallThunk>().write(Self::call_owned);
            entry.add(2).cast::<OwnedCallbackPtr>().write(heap);
        }
        self.shared.owned_pending.fetch_add(1, Ordering::Relaxed);
        self.commit(3);
        self.note_any_write();
    }

    unsafe fn call_owned(payload: *const u64) {
        let heap = unsafe { payload.cast_mut().cast::<OwnedCallbackPtr>().replace(std::ptr::null_mut()) };
        if !heap.is_null() {
            let func = *unsafe { Box::from_raw(heap) };
            func();
        }
    }

    /// Drops committed but unconsumed owned callbacks without executing work.
    /// Called only after the backend has stopped, including for unpublished
    /// packets. Execute may have completed entries past the last consumed
    /// publication; nulling slots before invocation makes this scan idempotent.
    fn discard_pending(&mut self) {
        assert!(self.back_thread.is_none(), "join backend before discarding queued work");
        if self.shared.owned_pending.load(Ordering::Relaxed) == 0 { return; }
        let mut read = self.shared.consumed.0.load(Ordering::Acquire);
        while read != self.write_position {
            let index = read as usize & RING_MASK;
            let entry = self.shared.at(index);
            let head = unsafe { entry.read() };
            let words = match head & 0xf {
                0 => { read += (RING_WORDS - index) as u64; continue; }
                1 => 1 + ENGINE_WORDS,
                2 => if head & HAS_SEGMENT != 0 { 2 } else { 1 },
                3 => (if head & HAS_SEGMENT != 0 { 3 } else { 2 }) + ((head >> 32) as usize).div_ceil(2),
                4 => {
                    if head & OWNED_CALL != 0 {
                        let heap = unsafe { entry.add(2).cast::<OwnedCallbackPtr>().replace(std::ptr::null_mut()) };
                        if !heap.is_null() {
                            self.shared.owned_pending.fetch_sub(1, Ordering::Relaxed);
                            drop(unsafe { Box::from_raw(heap) });
                        }
                    }
                    2 + ((head >> 8) & 0xffffff) as usize
                }
                _ => 1,
            };
            read += words as u64;
        }
    }

    fn reserve(&mut self, words: usize) -> *mut u64 {
        self.check_back_failure();
        // All upstream packets are bounded by the ring capacity. Reject an
        // internal caller error instead of waiting forever for impossible space.
        assert!(words > 0 && words <= RING_WORDS);
        let index = self.write_position as usize & RING_MASK;
        if index + words <= RING_WORDS &&
            self.write_position + words as u64 - self.consumed_cache <= RING_WORDS as u64 {
            return self.shared.at(index);
        }
        self.reserve_slow(words)
    }

    fn reserve_slow(&mut self, words: usize) -> *mut u64 {
        let mut index = self.write_position as usize & RING_MASK;
        if index + words > RING_WORDS {
            let rest = RING_WORDS - index;
            self.wait_for_space(rest);
            // SAFETY: the preceding wait reserves this tail for the producer.
            unsafe { self.shared.at(index).write(Kind::Wrap as u64); }
            self.write_position += rest as u64;
            index = 0;
        }
        self.wait_for_space(words);
        self.shared.at(index)
    }

    fn wait_for_space(&mut self, words: usize) {
        self.check_back_failure();
        if self.write_position + words as u64 - self.consumed_cache <= RING_WORDS as u64 {
            return;
        }
        self.publish();
        let _scope = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::SplitDrain);
        let mut spins = 0u32;
        loop {
            self.check_back_failure();
            self.consumed_cache = self.shared.consumed.0.load(Ordering::Acquire);
            if self.write_position + words as u64 - self.consumed_cache <= RING_WORDS as u64 {
                return;
            }
            if spins < 4096 { pause(); } else { std::thread::yield_now(); }
            spins = spins.wrapping_add(1);
        }
    }

    fn commit(&mut self, words: usize) {
        self.write_position += words as u64;
        if self.write_position - self.published_local >= self.publish_every as u64 {
            self.publish();
        }
    }

    fn publish(&mut self) {
        if self.published_local == self.write_position { return; }
        self.published_local = self.write_position;
        self.publish_every = (self.publish_every * 2).min(PUBLISH_EVERY);
        self.shared.published.0.store(self.write_position, Ordering::SeqCst);
        if self.shared.back_waiting.0.load(Ordering::SeqCst) {
            self.shared.wake.notify_one();
        }
    }

    pub(crate) fn drain(&mut self) {
        self.check_back_failure();
        self.consumed_cache = self.shared.consumed.0.load(Ordering::Acquire);
        if self.consumed_cache == self.write_position { return; }
        self.publish();
        let _scope = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::SplitDrain);
        let mut spins = 0u32;
        loop {
            self.check_back_failure();
            self.consumed_cache = self.shared.consumed.0.load(Ordering::Acquire);
            if self.consumed_cache == self.write_position {
                self.publish_every = 8;
                return;
            }
            self.backoff(spins, self.write_position);
            spins = spins.saturating_add(1);
        }
    }

    /// Port of GpuSplit::Backoff (Lichii gpu_split.cpp:441-458): one step of
    /// a front wait for the back to consume `target`. Spins first, then
    /// sleeps until the back stores a new position (NotifyFront): the back
    /// checks front_waiting after each store, the front checks the position
    /// after setting it.
    fn backoff(&self, spins: u32, target: u64) {
        if spins < 16384 {
            pause();
            return;
        }
        // Upstream's !back_running: no back to wake this front.
        if self.back_thread.is_none() || self.shared.stop_requested.load(Ordering::Acquire) {
            std::thread::yield_now();
            return;
        }
        let shared = &self.shared;
        shared.front_waiting.0.store(true, Ordering::SeqCst);
        let seen = shared.front_wake.value.load(Ordering::SeqCst);
        if shared.consumed.0.load(Ordering::SeqCst) < target
            && !shared.stop_requested.load(Ordering::SeqCst)
        {
            shared.front_wake.wait(seen);
        }
        shared.front_waiting.0.store(false, Ordering::Relaxed);
    }
}

impl GpuSplit {
    /// The ring position after the entries sent so far, for WaitUntil.
    pub(crate) fn position(&self) -> u64 { self.write_position }

    /// Waits until the back executed the entries sent before `position`.
    /// Returns whether any of them was still queued.
    pub(crate) fn wait_until(&mut self, position: u64) -> bool {
        self.wait_for(position.min(self.write_position))
    }

    /// The entry just sent may write guest memory anywhere.
    pub(crate) fn note_any_write(&mut self) { self.last_any_write = self.write_position; }

    fn note_device_write(&mut self, address: u64, size: u64) {
        let last = (address + size - 1) >> WRITE_GRANULE_BITS;
        for granule in (address >> WRITE_GRANULE_BITS)..=last {
            self.write_positions[write_slot(granule)] = self.write_position;
        }
        self.last_noted_write = self.write_position;
    }

    /// The position WaitForWrites waits for.
    fn writes_target(&self, memory: &crate::memory_manager::MemoryManager, address: u64,
        size: u64) -> u64
    {
        if size > MAX_NOTED_BYTES || address.checked_add(size).is_none() {
            return self.last_any_write.max(self.last_noted_write);
        }
        let mut target = self.last_any_write;
        for_each_device_range(memory, address, size, |device, bytes| {
            target = target.max(self.device_writes_target(device, bytes));
        });
        target
    }

    fn device_writes_target(&self, address: u64, size: u64) -> u64 {
        let last = (address + size - 1) >> WRITE_GRANULE_BITS;
        let mut target = 0;
        for granule in (address >> WRITE_GRANULE_BITS)..=last {
            target = target.max(self.write_positions[write_slot(granule)]);
        }
        target
    }

    /// Port of GpuSplit::WaitFor, with its Backoff.
    fn wait_for(&mut self, target: u64) -> bool {
        self.consumed_cache = self.shared.consumed.0.load(Ordering::Acquire);
        // Nothing to wait for: the back's state is not needed (a failed back
        // is reported by the next entry sent or by a real wait).
        if self.consumed_cache >= target { return false; }
        self.check_back_failure();
        let _scope = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::SplitDrain);
        self.shared.wait_target.0.store(target, Ordering::SeqCst);
        self.publish();
        let mut spins = 0u32;
        loop {
            self.check_back_failure();
            self.consumed_cache = self.shared.consumed.0.load(Ordering::Acquire);
            if self.consumed_cache >= target {
                self.shared.wait_target.0.store(u64::MAX, Ordering::Relaxed);
                return true;
            }
            self.backoff(spins, target);
            spins = spins.saturating_add(1);
        }
    }
}

impl Drop for GpuSplit {
    fn drop(&mut self) {
        // Drop must also join while unwinding from a frontend/backend failure.
        // The explicit stop result allows the owning session to report errors.
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_eligibility_matches_nx2_backend_accuracy_and_mode_matrix() {
        use common::settings_enums::{DmaAccuracy, GpuAccuracy, RendererBackend};
        let mut values = common::settings::Values::default();
        for nx2 in [false, true] {
            for accuracy in [GpuAccuracy::Low, GpuAccuracy::High] {
                values.current_gpu_accuracy = accuracy;
                for dma in [DmaAccuracy::Default, DmaAccuracy::Unsafe, DmaAccuracy::Safe] {
                    values.dma_accuracy.set_value(dma);
                    for backend in [RendererBackend::Vulkan, RendererBackend::OpenGlGlsl,
                        RendererBackend::Null, RendererBackend::Metal]
                    {
                        values.renderer_backend.set_value(backend);
                        for mode in [0, b'0', b'1', b'2', b'3'] {
                            for jit in [false, true] {
                                let wanted = nx2 && accuracy == GpuAccuracy::Low
                                    && dma != DmaAccuracy::Safe && backend == RendererBackend::Vulkan
                                    && matches!(mode, b'1' | b'2');
                                let expected = wanted.then_some(cfg!(target_arch = "x86_64")
                                    && mode == b'2' && jit);
                                assert_eq!(eligible_mode(mode, nx2, &values, jit), expected,
                                    "NX2={nx2} accuracy={accuracy:?} DMA={dma:?} renderer={backend:?} mode={mode} JIT={jit}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn frontend_scope_is_reentrant_thread_local_and_transfers_back_after_front_join() {
        let output = Arc::new(Mutex::new(Vec::new()));
        for session in 0..3 {
            let output = output.clone();
            let front_thread = std::thread::spawn(move || {
                assert!(current().is_none());
                let front = GpuSplitFrontend::new(session != 1);
                front.start_back(|| assert!(current().is_none(), "back inherited front TLS"));
                let scope = front.install();
                let outer = current().unwrap();
                assert_eq!(outer.front_macros(), session != 1);
                for index in 0..128 {
                    // Retain an outer handle while a nested subsystem obtains
                    // its own handle and queues work; no RefCell borrow spans it.
                    let inner = current().unwrap();
                    let output = output.clone();
                    inner.defer_function(Box::new(move || {
                        assert!(current().is_none());
                        output.lock().push((session, index));
                    }));
                }
                outer.publish();
                outer.drain();
                drop(outer);
                drop(scope);
                assert!(current().is_none());
                front.into_transport()
            });
            let mut transport = front_thread.join().unwrap();
            // The front is now joined; only now stop/join the back, matching
            // ThreadManager::NotifyShutdown's upstream owner ordering.
            transport.stop().unwrap();
            assert!(current().is_none());
        }
        let expected: Vec<_> = (0..3).flat_map(|session| (0..128).map(move |i| (session, i))).collect();
        assert_eq!(*output.lock(), expected);
    }

    #[test]
    fn frontend_scope_unwinds_without_leaving_a_stale_session() {
        assert!(current().is_none());
        let front = GpuSplitFrontend::new(true);
        front.start_back(|| {});
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _scope = front.install();
            assert!(current().is_some());
            std::panic::panic_any(17u32);
        }));
        assert_eq!(result.err().unwrap().downcast_ref::<u32>(), Some(&17));
        assert!(current().is_none());
        let mut transport = front.into_transport();
        transport.stop().unwrap();
        let fresh = GpuSplitFrontend::new(false);
        let scope = fresh.install();
        assert!(!current().unwrap().front_macros());
        drop(scope);
        assert!(current().is_none());
    }

    #[test]
    fn worker_registers_executes_fifo_drains_and_wakes_for_stop_in_fresh_sessions() {
        let registrations = Arc::new(AtomicUsize::new(0));
        for session in 0..3 {
            let mut split = GpuSplit::new();
            let registered = registrations.clone();
            split.start_back(move || { registered.fetch_add(1, Ordering::SeqCst); });
            let output = Arc::new(Mutex::new(Vec::new()));
            for index in 0..2000 {
                let output = output.clone();
                let registered = registrations.clone();
                split.defer_function(Box::new(move || {
                    assert_eq!(std::thread::current().name(), Some("GPUBack"));
                    assert_eq!(registered.load(Ordering::SeqCst), session + 1);
                    output.lock().push(index);
                }));
            }
            split.drain();
            assert_eq!(*output.lock(), (0..2000).collect::<Vec<_>>());
            assert_eq!(split.shared.owned_pending.load(Ordering::Relaxed), 0);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !split.shared.back_waiting.0.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                std::thread::yield_now();
            }
            assert!(split.shared.back_waiting.0.load(Ordering::SeqCst));
            assert!(split.stop().is_ok());
            assert!(split.stop().is_ok());
            assert!(split.back_thread.is_none());
        }
    }

    #[test]
    fn worker_panic_reaches_drain_and_stop_cleans_remaining_callbacks() {
        let drops = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut split = GpuSplit::new();
        split.start_back(|| {});
        let probe = DropProbe(drops.clone());
        split.defer_function(Box::new(move || {
            let _owned = probe;
            panic!("intentional worker failure");
        }));
        let pending_probe = DropProbe(drops.clone());
        let pending_calls = calls.clone();
        split.defer_function(Box::new(move || { drop(pending_probe); pending_calls.fetch_add(1, Ordering::SeqCst); }));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| split.drain()));
        assert!(result.is_err(), "drain must report failure instead of waiting forever");
        assert!(split.stop().is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(split.shared.owned_pending.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn backend_failure_releases_a_producer_waiting_for_ring_space() {
        let mut split = GpuSplit::new();
        let entry = split.reserve(RING_WORDS);
        unsafe { entry.write(Kind::Wrap as u64); }
        split.commit(RING_WORDS);
        let shared = split.shared.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let producer = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { split.reserve(1); })).is_err();
            done_tx.send(failed).unwrap();
        });
        ready_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        shared.back_failed.store(true, Ordering::Release);
        assert!(done_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap());
        producer.join().unwrap();
    }

    #[test]
    fn stop_joins_current_batch_then_cancels_later_published_work() {
        let drops = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut split = GpuSplit::new();
        split.start_back(|| {});
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        split.defer_function(Box::new(move || { entered_tx.send(()).unwrap(); release_rx.recv().unwrap(); }));
        split.publish();
        entered_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        let probe = DropProbe(drops.clone());
        let called = calls.clone();
        split.defer_function(Box::new(move || { drop(probe); called.fetch_add(1, Ordering::SeqCst); }));
        split.publish();
        let shared = split.shared.clone();
        let stopper = std::thread::spawn(move || split.stop().is_ok());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !shared.stop_requested.load(Ordering::Acquire) && std::time::Instant::now() < deadline { std::thread::yield_now(); }
        assert!(shared.stop_requested.load(Ordering::Acquire));
        release_tx.send(()).unwrap();
        assert!(stopper.join().unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    struct DropProbe(Arc<AtomicUsize>);
    impl Drop for DropProbe {
        fn drop(&mut self) { self.0.fetch_add(1, Ordering::SeqCst); }
    }

    #[test]
    fn owned_callback_execution_or_panic_cannot_double_drop_before_consumed_publication() {
        for panics in [false, true] {
            let drops = Arc::new(AtomicUsize::new(0));
            let calls = Arc::new(AtomicUsize::new(0));
            let mut split = GpuSplit::new();
            let probe = DropProbe(drops.clone());
            let called = calls.clone();
            split.defer_function(Box::new(move || {
                let _owned = probe;
                called.fetch_add(1, Ordering::SeqCst);
                if panics { panic!("intentional owned callback panic"); }
            }));
            let cancelled_probe = DropProbe(drops.clone());
            let cancelled_calls = calls.clone();
            split.defer_function(Box::new(move || {
                drop(cancelled_probe);
                cancelled_calls.fetch_add(100, Ordering::SeqCst);
            }));
            split.defer(|| panic!("cancellation must not invoke Copy callbacks either"));
            split.publish();
            let mut back = GpuSplitBack { shared: split.shared.clone(), back_engine: None };
            let _published = back.shared.published.0.load(Ordering::Acquire);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe { back.execute(0) }));
            assert_eq!(result.is_err(), panics);
            assert_eq!(drops.load(Ordering::SeqCst), 1);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            // Intentionally do not publish consumed, exactly as can happen
            // between execution and BackLoop's periodic consumed store.
            assert_eq!(back.shared.consumed.0.load(Ordering::Acquire), 0);
            split.discard_pending();
            split.discard_pending();
            assert_eq!(split.shared.owned_pending.load(Ordering::Relaxed), 0);
            drop(back);
            drop(split);
            assert_eq!(drops.load(Ordering::SeqCst), 2);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn drop_cancels_unpublished_owned_callbacks_across_wrap_without_running_work() {
        let drops = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let engine = TestEngine(1);
        let mut split = GpuSplit::new();
        split.write_position = RING_WORDS as u64 - 1;
        split.published_local = split.write_position;
        split.consumed_cache = split.write_position;
        split.shared.published.0.store(split.write_position, Ordering::Release);
        split.shared.consumed.0.store(split.write_position, Ordering::Release);
        let probe = DropProbe(drops.clone());
        let called = calls.clone();
        split.defer_function(Box::new(move || { drop(probe); called.fetch_add(1, Ordering::SeqCst); }));
        split.method(EngineHandle::from_ref(&engine), 0x40, 1, true, Some(0x1234), Writes::Anywhere);
        split.multi_method(EngineHandle::from_ref(&engine), 0x80, &[1, 2, 3], 3, 3, None, Writes::Anywhere);
        split.defer(|| panic!("drop must not execute queued GPU callbacks"));
        assert!(split.write_position > split.shared.published.0.load(Ordering::Acquire));
        drop(split);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[derive(Debug, PartialEq)]
    enum Event {
        Sink(u32, u32, u32),
        Method(u32, u32, u32, bool, u64),
        Multi(u32, u32, Vec<u32>, u32, u64),
        Callback(u32),
    }
    static EXECUTION_TRACE: Mutex<Vec<Event>> = Mutex::new(Vec::new());

    struct TraceEngine {
        id: u32,
        state: crate::engines::engine_interface::EngineInterfaceState,
    }
    impl TraceEngine {
        fn new(id: u32) -> Self {
            let mut state = crate::engines::engine_interface::EngineInterfaceState::new();
            state.execution_mask[0x80] = true;
            state.current_dma_segment = 7;
            Self { id, state }
        }
        fn segment(&self) -> u64 { self.state.current_dma_segment }
    }
    impl crate::engines::engine_interface::EngineInterface for TraceEngine {
        fn call_method(&mut self, method: u32, value: u32, last: bool) {
            EXECUTION_TRACE.lock().push(Event::Method(self.id, method, value, last, self.state.current_dma_segment));
        }
        fn call_multi_method(&mut self, method: u32, words: &[u32], amount: u32, pending: u32) {
            EXECUTION_TRACE.lock().push(Event::Multi(self.id, method, words[..amount as usize].to_vec(), pending, self.state.current_dma_segment));
        }
        fn consume_sink_impl(&mut self) {
            let drained: Vec<(u32, u32)> = self.state.method_sink.drain(..).collect();
            for (method, value) in drained {
                EXECUTION_TRACE.lock().push(Event::Sink(self.id, method, value));
            }
        }
        fn interface_state(&self) -> &crate::engines::engine_interface::EngineInterfaceState { &self.state }
        fn interface_state_mut(&mut self) -> &mut crate::engines::engine_interface::EngineInterfaceState { &mut self.state }
    }

    #[test]
    fn execute_preserves_sink_direct_macro_multi_segment_and_callback_order() {
        EXECUTION_TRACE.lock().clear();
        let mut first = TraceEngine::new(1);
        let mut second = TraceEngine::new(2);
        let a = EngineHandle::from_ref(&mut first);
        let b = EngineHandle::from_ref(&mut second);
        let mut split = GpuSplit::new();
        split.method(a, 0x40, 11, false, Some(100), Writes::Anywhere);
        split.method(a, 0x80, 22, false, Some(200), Writes::Anywhere);
        split.macro_write(a, 0x40, 33, Writes::Anywhere);
        split.method(a, 0x41, 44, true, None, Writes::Anywhere);
        split.multi_method(a, 0x90, &[55, 66, 77], 3, 9, Some(300), Writes::Anywhere);
        let marker = 101u32;
        split.defer(move || EXECUTION_TRACE.lock().push(Event::Callback(marker)));
        split.method(b, 0x80, 88, true, None, Writes::Anywhere);
        split.defer(|| EXECUTION_TRACE.lock().push(Event::Callback(102)));
        assert!(EXECUTION_TRACE.lock().is_empty());
        split.publish();
        let mut back = GpuSplitBack { shared: split.shared.clone(), back_engine: None };
        let published = back.shared.published.0.load(Ordering::Acquire);
        let mut read = 0u64;
        while read != published {
            let index = read as usize & RING_MASK;
            let words = unsafe { back.execute(index) };
            read += if words == 0 { (RING_WORDS - index) as u64 } else { words as u64 };
        }
        back.shared.consumed.0.store(read, Ordering::Release);
        assert_eq!(*EXECUTION_TRACE.lock(), [
            Event::Sink(1, 0x40, 11),
            Event::Method(1, 0x80, 22, false, 200),
            Event::Method(1, 0x40, 33, true, 200),
            Event::Sink(1, 0x41, 44),
            Event::Multi(1, 0x90, vec![55, 66, 77], 9, 300),
            Event::Callback(101),
            Event::Method(2, 0x80, 88, true, 7),
            Event::Callback(102),
        ]);
        assert_eq!(read, split.write_position);
        assert_eq!((first.segment(), second.segment()), (300, 7));
    }

    struct TestEngine(u32);
    impl crate::engines::engine_interface::EngineInterface for TestEngine {
        fn call_method(&mut self, _: u32, _: u32, _: bool) { panic!("encoding must not execute engines"); }
        fn call_multi_method(&mut self, _: u32, _: &[u32], _: u32, _: u32) { panic!("encoding must not execute engines"); }
        fn consume_sink_impl(&mut self) { panic!("encoding must not consume engine sinks"); }
        fn has_pending_methods(&self) -> bool { panic!("encoding must not read engine state"); }
        fn push_method_sink(&mut self, _: u32, _: u32) { panic!("encoding must not modify engine sinks"); }
        fn interface_state(&self) -> &crate::engines::engine_interface::EngineInterfaceState {
            panic!("encoding must not read engine state");
        }
        fn interface_state_mut(&mut self) -> &mut crate::engines::engine_interface::EngineInterfaceState {
            panic!("encoding must not modify engine state");
        }
    }

    #[test]
    fn engine_method_and_direct_macro_packets_preserve_handles_flags_and_segments() {
        let first = TestEngine(1);
        let second = TestEngine(2);
        let first_handle = EngineHandle::from_ref(&first);
        let second_handle = EngineHandle::from_ref(&second);
        let mut split = GpuSplit::new();
        split.method(first_handle, 0x12345, 0x89abcdef, true, Some(0x123456789abcdef0), Writes::Anywhere);
        split.macro_write(first_handle, 0x1abcd, 0x76543210, Writes::Anywhere);
        split.method(second_handle, 0x80, 42, false, None, Writes::Anywhere);
        let mut index = 0;
        unsafe {
            assert_eq!(split.shared.at(index).read(), 1);
            let copied = split.shared.at(index + 1).cast::<EngineHandle>().read();
            assert!(copied.ptr_eq(first_handle));
            index += 1 + ENGINE_WORDS;
            assert_eq!(split.shared.at(index).read(), (0x89abcdefu64 << 32) | (0x2345 << 8) | 0x32);
            assert_eq!(split.shared.at(index + 1).read(), 0x123456789abcdef0);
            index += 2;
            // Reusing the same engine emits no second Engine packet.
            assert_eq!(split.shared.at(index).read(), (0x76543210u64 << 32) | (0xabcd << 8) | 0x52);
            index += 1;
            assert_eq!(split.shared.at(index).read(), 1);
            assert!(split.shared.at(index + 1).cast::<EngineHandle>().read().ptr_eq(second_handle));
            index += 1 + ENGINE_WORDS;
            assert_eq!(split.shared.at(index).read(), (42u64 << 32) | (0x80 << 8) | 2);
            index += 1;
        }
        assert_eq!(split.write_position, index as u64);
        assert_eq!((first.0, second.0), (1, 2));
    }

    #[test]
    fn multi_packets_copy_source_split_at_8192_and_zero_odd_padding() {
        let engine = TestEngine(1);
        for segment in [None, Some(0x123456789abc)] {
            let mut split = GpuSplit::new();
            let mut source: Vec<u32> = (0..8195).map(|i| i ^ 0xa5a50000).collect();
            let expected = source.clone();
            split.multi_method(EngineHandle::from_ref(&engine), 0x180, &source, 8195, 8199, segment, Writes::Anywhere);
            source.fill(0); // The caller may reuse its DMA scratch immediately.
            let mut index = 1 + ENGINE_WORDS;
            let mut offset = 0;
            for (piece, pending) in [(8192, 8199), (3, 7)] {
                let head = if segment.is_some() { 3 } else { 2 };
                unsafe {
                    assert_eq!(split.shared.at(index).read(), ((piece as u64) << 32)
                        | (0x180 << 8) | if segment.is_some() { 0x23 } else { 3 });
                    assert_eq!(split.shared.at(index + 1).read(), pending);
                    if let Some(segment) = segment { assert_eq!(split.shared.at(index + 2).read(), segment); }
                    let payload = split.shared.at(index + head).cast::<u32>();
                    assert_eq!(std::slice::from_raw_parts(payload, piece), &expected[offset..offset + piece]);
                    if piece % 2 != 0 { assert_eq!(payload.add(piece).read(), 0); }
                }
                offset += piece;
                index += head + piece.div_ceil(2);
            }
            assert_eq!(split.write_position, index as u64);
        }
    }

    #[test]
    fn zero_multi_selects_engine_and_pending_count_preserves_unsigned_wrap() {
        let engine = TestEngine(1);
        let handle = EngineHandle::from_ref(&engine);
        let mut split = GpuSplit::new();
        split.multi_method(handle, 1, &[], 0, 0, None, Writes::Anywhere);
        assert_eq!(split.write_position, (1 + ENGINE_WORDS) as u64);
        split.multi_method(handle, 1, &vec![0; 8193], 8193, 0, None, Writes::Anywhere);
        let second = 1 + ENGINE_WORDS + 2 + 4096;
        assert_eq!(unsafe { split.shared.at(second + 1).read() }, 0u32.wrapping_sub(8192) as u64);
    }

    #[test]
    fn wrap_reserves_contiguous_packet_and_publishes_marker_before_reuse() {
        let mut split = GpuSplit::new();
        split.write_position = RING_WORDS as u64 - 2;
        split.published_local = split.write_position;
        split.shared.consumed.0.store(split.write_position, Ordering::Release);
        // The cached consumed position is intentionally stale: ReserveSlow
        // must refresh it before taking space at the beginning of the ring.
        let packet = split.reserve(4);
        assert_eq!(packet, split.shared.at(0));
        assert_eq!(split.write_position, RING_WORDS as u64);
        assert_eq!(unsafe { split.shared.at(RING_WORDS - 2).read() }, Kind::Wrap as u64);
        unsafe { std::ptr::copy_nonoverlapping([11, 22, 33, 44].as_ptr(), packet, 4); }
        split.commit(4);
        split.publish();
        assert_eq!(split.shared.published.0.load(Ordering::Acquire), RING_WORDS as u64 + 4);
        assert_eq!(unsafe { std::slice::from_raw_parts(split.shared.at(0), 4) }, [11, 22, 33, 44]);
    }

    #[test]
    fn publication_wait_and_drain_keep_fifo_data_across_repeated_wraps() {
        let mut split = GpuSplit::new();
        let shared = split.shared.clone();
        const COUNT: u64 = 10000;
        let consumer = std::thread::spawn(move || {
            let mut read = 0u64;
            let mut received = 0u64;
            let mut valid = true;
            while received < COUNT {
                let available = shared.published.0.load(Ordering::Acquire);
                if read == available {
                    shared.back_waiting.0.store(true, Ordering::SeqCst);
                    let seen = shared.wake.value.load(Ordering::SeqCst);
                    if shared.published.0.load(Ordering::SeqCst) == read {
                        shared.wake.wait(seen);
                    }
                    shared.back_waiting.0.store(false, Ordering::Relaxed);
                    continue;
                }
                while read != available {
                    let index = read as usize & RING_MASK;
                    let head = unsafe { shared.at(index).read() };
                    if head == Kind::Wrap as u64 { read += (RING_WORDS - index) as u64; continue; }
                    let words = (head >> 32) as usize;
                    for offset in 1..words {
                        valid &= unsafe { shared.at(index + offset).read() } == received ^ offset as u64;
                    }
                    received += 1;
                    read += words as u64;
                    // Same release contract as BackLoop; frequent publication
                    // makes this test exercise the producer's space reclamation.
                    shared.consumed.0.store(read, Ordering::Release);
                }
                shared.consumed.0.store(read, Ordering::Release);
            }
            valid
        });
        for sequence in 0..COUNT {
            let words = 257 + (sequence as usize % 31);
            let entry = split.reserve(words);
            unsafe {
                entry.write((Kind::Multi as u64) | ((words as u64) << 32));
                for offset in 1..words { entry.add(offset).write(sequence ^ offset as u64); }
            }
            split.commit(words);
        }
        split.drain();
        assert!(consumer.join().unwrap());
        assert_eq!(split.shared.consumed.0.load(Ordering::Acquire), split.write_position);
    }

    #[test]
    fn full_ring_cannot_be_reused_until_consumer_releases_space() {
        let mut split = GpuSplit::new();
        let entry = split.reserve(RING_WORDS);
        unsafe { entry.write(0x1234); }
        split.commit(RING_WORDS);
        let shared = split.shared.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let producer = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let entry = split.reserve(1);
            unsafe { entry.write(0x5678); }
            split.commit(1);
            done_tx.send(()).unwrap();
        });
        ready_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(done_rx.recv_timeout(std::time::Duration::from_millis(20)).is_err());
        assert_eq!(unsafe { shared.at(0).read() }, 0x1234);
        shared.consumed.0.store(RING_WORDS as u64, Ordering::Release);
        done_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        producer.join().unwrap();
        assert_eq!(unsafe { shared.at(0).read() }, 0x5678);
    }

    // Lichii DeferWords (gpu_split.h:266-284) and WaitUntil/WaitFor with the
    // back storing `consumed` once past wait_target (gpu_split.cpp:418-439,
    // 725-733): the front waits for one entry, not for the batch after it.
    #[test]
    fn defer_words_copies_words_and_wait_until_returns_before_later_entries() {
        static SEEN: std::sync::Mutex<Vec<Vec<u32>>> = std::sync::Mutex::new(Vec::new());
        static RELEASE: AtomicBool = AtomicBool::new(false);
        let mut split = GpuSplit::new();
        split.start_back(|| {});
        let mut words = vec![1, 2, 3];
        split.defer_words(|words: &[u32]| SEEN.lock().unwrap().push(words.to_vec()), &words);
        words.fill(0); // The ring holds its own copy.
        split.defer_words(|words: &[u32]| SEEN.lock().unwrap().push(words.to_vec()), &[]);
        split.defer_words(|words: &[u32]| SEEN.lock().unwrap().push(words.to_vec()), &[7, 8]);
        let position = split.position();
        split.defer(|| while !RELEASE.load(Ordering::Acquire) { std::thread::yield_now(); });
        assert_eq!(split.last_any_write, split.position(), "Writes::Anywhere");
        // The back blocks in the entry after `position`, in the same batch.
        assert!(split.wait_until(position));
        assert_eq!(*SEEN.lock().unwrap(), [vec![1, 2, 3], vec![], vec![7, 8]]);
        assert!(!split.wait_until(position), "already consumed");
        RELEASE.store(true, Ordering::Release);
        split.drain();
        split.stop().unwrap();
    }

    // Lichii NoteWrite/WritesTarget (gpu_split.cpp:371-412): writes are placed
    // by 256-byte device granule, hashed into slots; anywhere writes bound all.
    #[test]
    fn noted_writes_place_their_position_by_granule() {
        let mut split = GpuSplit::new();
        split.write_position = 10;
        split.note_device_write(0x10_0080, 0x100);
        assert_eq!(split.last_noted_write, 10);
        split.write_position = 20;
        split.note_any_write();
        split.write_position = 30;
        // Granules 0x1000 and 0x1001 hold position 10.
        assert_eq!(split.device_writes_target(0x10_0000, 4), 10);
        assert_eq!(split.device_writes_target(0x10_01fc, 4), 10);
        let elsewhere = 0x20_0000u64;
        let collides = [0x1000u64, 0x1001].iter()
            .any(|&granule| write_slot(granule) == write_slot(elsewhere >> WRITE_GRANULE_BITS));
        assert_eq!(split.device_writes_target(elsewhere, 4), if collides { 10 } else { 0 });
        assert_eq!(split.last_any_write, 20);
    }

    #[test]
    fn drain_publishes_unpublished_work_then_reduces_publication_interval() {
        let mut split = GpuSplit::new();
        let entry = split.reserve(1);
        unsafe { entry.write(Kind::Method as u64); }
        split.commit(1);
        assert_eq!(split.shared.published.0.load(Ordering::Acquire), 0);
        let shared = split.shared.clone();
        let consumer = std::thread::spawn(move || {
            while shared.published.0.load(Ordering::Acquire) == 0 { std::thread::yield_now(); }
            assert_eq!(unsafe { shared.at(0).read() }, Kind::Method as u64);
            shared.consumed.0.store(1, Ordering::Release);
        });
        split.drain();
        consumer.join().unwrap();
        assert_eq!(split.publish_every, 8);
        split.drain(); // already drained: preserve interval, no extra publication
        assert_eq!(split.publish_every, 8);
    }

    // Lichii GpuSplit::NoteWrite/ForEachDeviceRange (gpu_split.cpp:356-382): the front's
    // cached translations follow every page table change (map_serial) and split ranges by
    // 4 KiB GPU page, skipping unmapped pages.
    #[test]
    fn note_write_cache_follows_remaps_and_page_boundaries() {
        let device_memory = Arc::new(
            crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default());
        let memory = Arc::new(Mutex::new(
            crate::memory_manager::MemoryManager::new_with_geometry_and_device_memory(
                1, device_memory, 32, 0x1_0000_0000, 16, 12)));
        memory.lock().map(0x4000, 0x4000, 0x6000, 0, false);
        let front = GpuSplitFrontend::new(true);
        let position = |value: u64| front.0.transport.borrow_mut().write_position = value;
        let target = |device: u64, size: u64| front.0.transport.borrow().device_writes_target(device, size);
        position(5);
        front.note_write(&memory, 0x4010, 8);
        assert_eq!(target(0x4010, 8), 5);
        position(6);
        front.note_write(&memory, 0x4020, 8); // Same page, cached
        assert_eq!(target(0x4020, 8), 6);
        memory.lock().unmap(0x4000, 0x6000);
        memory.lock().map(0x4000, 0x8_0000, 0x6000, 0, false);
        position(7);
        front.note_write(&memory, 0x4ff8, 16);
        assert_eq!(target(0x8_0ff8, 8), 7, "remapped page");
        assert_eq!(target(0x8_1000, 8), 7, "next page");
        memory.lock().unmap(0x4000, 0x6000);
        position(8);
        let any_before = front.0.transport.borrow().last_noted_write;
        front.note_write(&memory, 0x4010, 8);
        assert_eq!(front.0.transport.borrow().last_noted_write, any_before, "nothing mapped");
    }

    // Lichii Backoff/NotifyFront (gpu_split.cpp:441-458, gpu_split.h:481-487):
    // a front waiting past its spins sleeps until the back stores a position,
    // and a failed back wakes it too.
    #[test]
    fn front_backoff_sleeps_until_back_stores_consumed_or_fails() {
        let mut split = GpuSplit::new();
        split.start_back(|| {});
        split.defer(|| std::thread::sleep(std::time::Duration::from_millis(50)));
        split.drain();
        assert_eq!(split.shared.consumed.0.load(Ordering::Acquire), split.write_position);
        assert!(!split.shared.front_waiting.0.load(Ordering::Relaxed));
        assert!(split.shared.front_wake.value.load(Ordering::SeqCst) > 0,
            "the back woke the sleeping front");
        split.defer(|| std::thread::sleep(std::time::Duration::from_millis(50)));
        let target = split.position();
        assert!(split.wait_until(target));
        assert!(!split.shared.front_waiting.0.load(Ordering::Relaxed));
        split.defer(|| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            panic!("injected backend failure");
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| split.drain()));
        assert!(result.is_err(), "drain reports the failed back instead of sleeping forever");
        assert!(split.stop().is_err());
    }

    #[test]
    fn notification_before_wait_is_not_lost() {
        let wake = Wake::new();
        let seen = wake.value.load(Ordering::SeqCst);
        wake.notify_one();
        wake.wait(seen);
        assert_eq!(wake.value.load(Ordering::SeqCst), seen + 1);
    }
}

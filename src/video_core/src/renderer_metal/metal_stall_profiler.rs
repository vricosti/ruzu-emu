// SPDX-License-Identifier: GPL-3.0-or-later
//! Opt-in CPU wall-time diagnostics, not GPU timestamp queries or emulation state.
//! Native Metal tooling; no Eden counterpart. Nested timings are inclusive.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::panic::Location;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const THRESHOLD: Duration = Duration::from_millis(100);
const COUNT: usize = 10;

#[derive(Clone, Copy, Debug)]
pub(super) enum Operation {
    ShaderBuild,
    MslCompile,
    RenderPipeline,
    ComputePipeline,
    GeometryPipeline,
    TessellationPipeline,
    GpuWait,
    Drawable,
    Upload,
    Download,
}

const OPERATIONS: [Operation; COUNT] = [
    Operation::ShaderBuild, Operation::MslCompile, Operation::RenderPipeline,
    Operation::ComputePipeline, Operation::GeometryPipeline, Operation::TessellationPipeline,
    Operation::GpuWait, Operation::Drawable, Operation::Upload, Operation::Download,
];

#[derive(Clone, Copy, Default)]
struct Sample {
    count: u64,
    total: Duration,
    max: Duration,
}

#[derive(Default)]
struct Window {
    previous: Option<Instant>,
    frame: u64,
    samples: [Sample; COUNT],
}

impl Window {
    fn observe(&mut self, op: Operation, duration: Duration) {
        let sample = &mut self.samples[op as usize];
        sample.count = sample.count.saturating_add(1);
        sample.total = sample.total.saturating_add(duration);
        sample.max = sample.max.max(duration);
    }

    fn present(&mut self, now: Instant) -> Option<(Duration, [Sample; COUNT])> {
        let elapsed = self.previous.replace(now).map(|previous| now.duration_since(previous));
        let samples = std::mem::take(&mut self.samples);
        self.frame += 1;
        elapsed.filter(|elapsed| *elapsed >= THRESHOLD).map(|elapsed| (elapsed, samples))
    }
}

thread_local! {
    static WINDOW: RefCell<Window> = RefCell::new(Window::default());
}

fn origin() -> Option<Instant> {
    static ORIGIN: OnceLock<Option<Instant>> = OnceLock::new();
    *ORIGIN.get_or_init(|| std::env::var_os("RUZU_PROFILE_METAL_STALLS").map(|_| Instant::now()))
}

pub(super) struct Span {
    timing: Option<(Instant, Instant, Operation, &'static Location<'static>)>,
    // Timings must be attributed to the thread that started the operation.
    _thread: PhantomData<Rc<()>>,
}

impl Span {
    #[track_caller]
    pub(super) fn start(op: Operation) -> Self {
        let caller = Location::caller();
        Self {
            timing: origin().map(|origin| (origin, Instant::now(), op, caller)),
            _thread: PhantomData,
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some((origin, start, op, site)) = self.timing else { return };
        let elapsed = start.elapsed();
        WINDOW.with(|window| window.borrow_mut().observe(op, elapsed));
        if elapsed >= THRESHOLD {
            log::info!("[METAL_STALL_OP] t_ms={:.3} op={op:?} wall_ms={:.3} thread={:?} site={}:{} inclusive=true",
                start.duration_since(origin).as_secs_f64() * 1000.0,
                elapsed.as_secs_f64() * 1000.0, std::thread::current().name(), site.file(), site.line());
        }
    }
}

/// Successful presentation submission, not drawable display/completion time.
pub(super) fn presented() {
    let Some(origin) = origin() else { return };
    let now = Instant::now();
    WINDOW.with(|window| {
        let mut window = window.borrow_mut();
        if let Some((elapsed, samples)) = window.present(now) {
            let details = OPERATIONS.iter().zip(samples).filter(|(_, sample)| sample.count != 0)
                .map(|(op, s)| format!("{op:?}:n={},sum_ms={:.3},max_ms={:.3}",
                    s.count, s.total.as_secs_f64()*1000.0, s.max.as_secs_f64()*1000.0))
                .collect::<Vec<_>>().join(" ");
            log::info!("[METAL_STALL_FRAME] t_ms={:.3} frame={} submit_gap_ms={:.3} thread={:?} inclusive=true {}",
                now.duration_since(origin).as_secs_f64()*1000.0, window.frame,
                elapsed.as_secs_f64()*1000.0, std::thread::current().name(), details);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_present_resets_startup_and_short_frames_do_not_leak() {
        let mut window = Window::default();
        let start = Instant::now();
        window.observe(Operation::MslCompile, Duration::from_secs(2));
        assert!(window.present(start).is_none());
        window.observe(Operation::Upload, Duration::from_millis(3));
        assert!(window.present(start + Duration::from_millis(16)).is_none());
        window.observe(Operation::GpuWait, Duration::from_millis(40));
        window.observe(Operation::GpuWait, Duration::from_millis(60));
        let (gap, samples) = window.present(start + Duration::from_millis(116)).unwrap();
        assert_eq!(gap, THRESHOLD);
        assert_eq!(samples[Operation::MslCompile as usize].count, 0);
        assert_eq!(samples[Operation::Upload as usize].count, 0);
        let waits = samples[Operation::GpuWait as usize];
        assert_eq!(waits.count, 2);
        assert_eq!(waits.total, THRESHOLD);
        assert_eq!(waits.max, Duration::from_millis(60));
        assert!(window.samples.iter().all(|sample| sample.count == 0));
    }
}

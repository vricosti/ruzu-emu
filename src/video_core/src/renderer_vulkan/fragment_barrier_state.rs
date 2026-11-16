// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Lichii `video_core/renderer_vulkan/fragment_barrier_state.h`.
//!
//! Lichii extension (absent from Eden, whose `RasterizerVulkan::FragmentBarrier`
//! ends the render pass at once): a fragment barrier recorded inside a render
//! pass is deferred, and the next draw ends the pass only if it reads what the
//! pass wrote (`Scheduler::defer_fragment_barrier`,
//! `GraphicsPipeline::configure`).

/// Lichii `Vulkan::FragmentBarrierState`.
#[derive(Clone, Copy, Debug, Default)]
pub struct FragmentBarrierState {
    pending: bool,
}

impl FragmentBarrierState {
    /// Lichii `FragmentBarrierState::Defer`: pending only while a render pass
    /// is active (outside one, the pass that wrote has already ended).
    pub fn defer(&mut self, render_pass_active: bool) {
        self.pending = render_pass_active;
    }

    /// Lichii `FragmentBarrierState::Pending`.
    pub fn pending(&self) -> bool {
        self.pending
    }

    /// Lichii `FragmentBarrierState::Reset`, when the render pass ends.
    pub fn reset(&mut self) {
        self.pending = false;
    }
}

#[cfg(test)]
mod tests {
    use super::FragmentBarrierState;

    /// Port of Lichii's "Nx2Perf draw-binds: deferred barrier survives
    /// unrelated draws" (tests/video_core, Lichii 5f6066b26b): a draw ends the
    /// pass (and so resets the barrier) only when a barrier is pending and the
    /// pass stores or the draw samples one of its attachments.
    #[test]
    fn deferred_barrier_survives_unrelated_draws() {
        let mut barrier = FragmentBarrierState::default();
        let prepare_draw = |barrier: &mut FragmentBarrierState, stores: bool, samples_attachment: bool| {
            let end_pass = barrier.pending() && (stores || samples_attachment);
            if end_pass {
                barrier.reset();
            }
            end_pass
        };
        assert!(!prepare_draw(&mut barrier, false, true));
        barrier.defer(true);
        assert!(!prepare_draw(&mut barrier, false, false));
        assert!(!prepare_draw(&mut barrier, false, false));
        assert!(prepare_draw(&mut barrier, false, true));
        assert!(!prepare_draw(&mut barrier, false, true));
        barrier.defer(true);
        assert!(prepare_draw(&mut barrier, true, false));
        barrier.defer(true);
        barrier.reset();
        assert!(!prepare_draw(&mut barrier, true, true));
        barrier.defer(false);
        assert!(!prepare_draw(&mut barrier, true, true));
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of Lichii nx2 core/hle/service/nx2_video_input_queue.h.

use std::collections::VecDeque;

#[derive(Clone)]
pub struct Input {
    pub data: Vec<u8>,
    pub pts: u64,
    pub end: bool,
}

#[derive(Default)]
pub struct Nx2VideoInputQueue {
    inputs: VecDeque<Input>,
    held: Option<Input>,
}

impl Nx2VideoInputQueue {
    pub const READY_WATERMARK: usize = 8;
    pub const REFUSE_AT: usize = Self::READY_WATERMARK * 3;

    pub fn push(&mut self, input: Input) {
        self.inputs.push_back(input);
    }

    /// Retain a refused packet until the next submission. Retried packets are
    /// deduplicated; a client advancing despite busy does not lose references.
    pub fn submit(&mut self, input: Input) -> bool {
        if let Some(held) = self.held.take() {
            if held.pts != input.pts || held.data != input.data {
                self.inputs.push_back(held);
            }
        }
        if !input.end && self.full() {
            self.held = Some(input);
            return false;
        }
        self.inputs.push_back(input);
        true
    }

    pub fn pop(&mut self) -> Input {
        self.inputs.pop_front().expect("nonempty video input queue")
    }
    pub fn clear(&mut self) {
        self.inputs.clear();
        self.held = None;
    }
    pub fn empty(&self) -> bool { self.inputs.is_empty() }
    pub fn size(&self) -> usize { self.inputs.len() }
    pub fn below_ready_watermark(&self) -> bool { self.size() < Self::READY_WATERMARK }
    pub fn full(&self) -> bool { self.size() >= Self::REFUSE_AT }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(pts: u64) -> Input { Input { data: vec![pts as u8], pts, end: false } }
    fn full_queue() -> Nx2VideoInputQueue {
        let mut q = Nx2VideoInputQueue::default();
        for n in 0..Nx2VideoInputQueue::REFUSE_AT {
            assert!(q.submit(packet(n as u64)));
            assert_eq!(q.below_ready_watermark(), n + 1 < Nx2VideoInputQueue::READY_WATERMARK);
        }
        q
    }
    #[test]
    fn retried_busy_packet_is_queued_once() {
        let mut q = full_queue();
        assert!(!q.submit(packet(24)));
        assert!(!q.submit(packet(24)));
        assert_eq!(q.size(), 24);
        assert_eq!(q.pop().pts, 0);
        assert!(q.submit(packet(24)));
        for n in 1..25 { assert_eq!(q.pop().pts, n); }
        assert!(q.empty());
    }
    #[test]
    fn advancing_client_preserves_held_packet_before_end() {
        let mut q = full_queue();
        assert!(!q.submit(packet(24)));
        assert!(!q.submit(packet(25)));
        assert!(q.submit(Input { end: true, ..packet(26) }));
        for n in 0..27 { assert_eq!(q.pop().pts, n); }
        assert!(q.empty());
    }
    #[test]
    fn clear_discards_held_packet_and_sessions_are_independent() {
        let mut q = full_queue();
        assert!(!q.submit(packet(24)));
        let mut other = Nx2VideoInputQueue::default();
        other.push(packet(99));
        q.clear();
        assert!(q.below_ready_watermark());
        assert!(q.submit(packet(42)));
        assert_eq!(q.pop().pts, 42);
        assert!(q.empty());
        assert_eq!(other.pop().pts, 99);
    }
}

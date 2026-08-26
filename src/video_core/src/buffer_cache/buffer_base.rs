// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `video_core/buffer_cache/buffer_base.h`
//!
//! Range-tracking buffer container.
//!
//! Keeps track of the modified CPU and GPU ranges on a CPU page granularity,
//! notifying the given rasterizer about state changes in the tracking behavior
//! of the buffer.
//!
//! The buffer size and address is forcefully aligned to CPU page boundaries.

use bitflags::bitflags;
use common::types::VAddr;

// ---------------------------------------------------------------------------
// BufferFlagBits
// ---------------------------------------------------------------------------

bitflags! {
    /// Flags associated with a buffer instance.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct BufferFlagBits: u32 {
        const PICKED             = 1 << 0;
        const CACHED_WRITES      = 1 << 1;
        const PREEMTIVE_DOWNLOAD = 1 << 2;
    }
}

// ---------------------------------------------------------------------------
// NullBufferParams
// ---------------------------------------------------------------------------

/// Tag for creating null buffers with no storage or size.
pub struct NullBufferParams;

// ---------------------------------------------------------------------------
// BufferBase
// ---------------------------------------------------------------------------

/// Constants from the upstream `BufferBase` class.
pub const BASE_PAGE_BITS: u64 = 16;
pub const BASE_PAGE_SIZE: u64 = 1u64 << BASE_PAGE_BITS;

/// Base buffer tracking structure.
///
/// This is the Rust counterpart of the C++ `BufferBase` class. It holds the
/// CPU address, size, flags, stream score, and LRU cache identifier but does
/// not own any GPU-side resource — that is the responsibility of the
/// backend-specific `Buffer` type.
pub struct BufferBase {
    /// Cached device-address view of `cpu_addr`, matching Eden's public
    /// `DAddr cpu_addr_cached` member (`DAddr` and `VAddr` are both `u64`).
    pub cpu_addr_cached: u64,
    cpu_addr: VAddr,
    flags: BufferFlagBits,
    stream_score: i32,
    lru_id: usize,
    size_bytes: usize,
    /// Tick of the most recent GPU write. Upstream owns this in BufferBase.
    write_tick: u64,
}

impl BufferBase {
    /// Create a new buffer base for the given CPU address and size.
    pub fn new(cpu_addr: VAddr, size_bytes: u64) -> Self {
        Self {
            cpu_addr_cached: cpu_addr,
            cpu_addr,
            flags: BufferFlagBits::empty(),
            stream_score: 0,
            lru_id: usize::MAX,
            size_bytes: size_bytes as usize,
            write_tick: 0,
        }
    }

    /// Create a null buffer (no storage, no size).
    pub fn null(_params: NullBufferParams) -> Self {
        Self {
            cpu_addr_cached: 0,
            cpu_addr: 0,
            flags: BufferFlagBits::empty(),
            stream_score: 0,
            lru_id: usize::MAX,
            size_bytes: 0,
            write_tick: 0,
        }
    }

    /// Mark buffer as picked.
    #[inline]
    pub fn pick(&mut self) {
        self.flags |= BufferFlagBits::PICKED;
    }

    /// Mark buffer for preemptive download.
    #[inline]
    pub fn mark_preemtive_download(&mut self) {
        self.flags |= BufferFlagBits::PREEMTIVE_DOWNLOAD;
    }

    /// Unmark buffer as picked.
    #[inline]
    pub fn unpick(&mut self) {
        self.flags -= BufferFlagBits::PICKED;
    }

    /// Increases the likeliness of this being a stream buffer.
    #[inline]
    pub fn increase_stream_score(&mut self, score: i32) {
        self.stream_score += score;
    }

    /// Returns the likeliness of this being a stream buffer.
    #[inline]
    pub fn stream_score(&self) -> i32 {
        self.stream_score
    }

    /// Returns true when `vaddr .. vaddr+size` is fully contained in the buffer.
    #[inline]
    pub fn is_in_bounds(&self, addr: VAddr, size: u64) -> bool {
        addr >= self.cpu_addr
            && addr.wrapping_add(size) <= self.cpu_addr.wrapping_add(self.size_bytes() as u64)
    }

    /// Returns true if the buffer has been marked as picked.
    #[inline]
    pub fn is_picked(&self) -> bool {
        self.flags.contains(BufferFlagBits::PICKED)
    }

    /// Returns true when the buffer has pending cached writes.
    #[inline]
    pub fn has_cached_writes(&self) -> bool {
        self.flags.contains(BufferFlagBits::CACHED_WRITES)
    }

    /// Returns true when the buffer has been marked for preemptive download.
    #[inline]
    pub fn is_preemtive_download(&self) -> bool {
        self.flags.contains(BufferFlagBits::PREEMTIVE_DOWNLOAD)
    }

    /// Returns the base CPU address of the buffer.
    #[inline]
    pub fn cpu_addr(&self) -> VAddr {
        self.cpu_addr
    }

    /// Returns the offset relative to the given CPU address.
    ///
    /// Precondition: `is_in_bounds` returns true for this address.
    #[inline]
    pub fn offset(&self, other_cpu_addr: VAddr) -> u32 {
        (other_cpu_addr - self.cpu_addr) as u32
    }

    /// Returns the LRU cache identifier.
    #[inline]
    pub fn get_lru_id(&self) -> usize {
        self.lru_id
    }

    /// Sets the LRU cache identifier.
    #[inline]
    pub fn set_lru_id(&mut self, lru_id: usize) {
        self.lru_id = lru_id;
    }

    /// Returns the size in bytes.
    #[inline]
    pub fn size_bytes(&self) -> usize {
        self.size_bytes
    }

    #[inline]
    pub fn set_write_tick(&mut self, tick: u64) {
        self.write_tick = tick;
    }

    #[inline]
    pub fn write_tick(&self) -> u64 {
        self.write_tick
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_flag_bits_match_upstream() {
        assert_eq!(BufferFlagBits::PICKED.bits(), 1);
        assert_eq!(BufferFlagBits::CACHED_WRITES.bits(), 2);
        assert_eq!(BufferFlagBits::PREEMTIVE_DOWNLOAD.bits(), 4);
    }

    #[test]
    fn test_null_buffer() {
        let buf = BufferBase::null(NullBufferParams);
        assert_eq!(buf.cpu_addr_cached, 0);
        assert_eq!(buf.cpu_addr(), 0);
        assert_eq!(buf.size_bytes(), 0);
        assert!(!buf.is_picked());
    }

    #[test]
    fn test_pick_unpick() {
        let mut buf = BufferBase::new(0x1000, 0x2000);
        assert!(!buf.is_picked());
        buf.pick();
        assert!(buf.is_picked());
        buf.unpick();
        assert!(!buf.is_picked());
    }

    #[test]
    fn test_is_in_bounds() {
        let buf = BufferBase::new(0x1000, 0x2000);
        assert_eq!(buf.cpu_addr_cached, 0x1000);
        assert!(buf.is_in_bounds(0x1000, 0x2000));
        assert!(buf.is_in_bounds(0x1500, 0x500));
        assert!(!buf.is_in_bounds(0x0FFF, 1));
        assert!(!buf.is_in_bounds(0x1000, 0x2001));
    }

    #[test]
    fn is_in_bounds_preserves_upstream_unsigned_wrapping() {
        let buf = BufferBase::new(0, 0);
        assert!(buf.is_in_bounds(u64::MAX, 1));

        let wrapped_end = BufferBase::new(u64::MAX - 1, 2);
        assert!(!wrapped_end.is_in_bounds(u64::MAX - 1, 0));
    }

    #[test]
    fn test_offset() {
        let buf = BufferBase::new(0x1000, 0x2000);
        assert_eq!(buf.offset(0x1000), 0);
        assert_eq!(buf.offset(0x1100), 0x100);
    }

    #[test]
    fn test_stream_score() {
        let mut buf = BufferBase::new(0x1000, 0x100);
        assert_eq!(buf.stream_score(), 0);
        buf.increase_stream_score(5);
        assert_eq!(buf.stream_score(), 5);
        buf.increase_stream_score(-2);
        assert_eq!(buf.stream_score(), 3);
    }

    #[test]
    fn preemptive_download_and_write_tick_match_upstream_defaults() {
        let mut buf = BufferBase::new(0x1000, 0x100);
        assert!(!buf.is_preemtive_download());
        assert_eq!(buf.write_tick(), 0);

        buf.mark_preemtive_download();
        buf.set_write_tick(27);
        assert!(buf.is_preemtive_download());
        assert_eq!(buf.write_tick(), 27);
    }

    #[test]
    fn test_lru_id() {
        let mut buf = BufferBase::new(0x1000, 0x100);
        assert_eq!(buf.get_lru_id(), usize::MAX);
        buf.set_lru_id(42);
        assert_eq!(buf.get_lru_id(), 42);
    }
}

// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Inline-to-Memory engine (NV class A140).
//!
//! Copies inline data from pushbuffer commands directly into GPU memory.
//! The command processor sends DATA words via NonIncMethod after an EXEC
//! trigger sets up the transfer parameters. Once enough bytes have been
//! accumulated, `execute_pending()` returns a `PendingWrite` with the
//! destination address and data.

use super::{PendingWrite, ENGINE_REG_COUNT};

// ── Register constants (method addresses) ────────────────────────────────────

const LINE_LENGTH_IN: u32 = 0x60;
const LINE_COUNT: u32 = 0x61;
const DST_ADDRESS_HIGH: u32 = 0x62;
const DST_ADDRESS_LOW: u32 = 0x63;
const DST_PITCH: u32 = 0x64;
const EXEC: u32 = 0x6C;
const DATA: u32 = 0x6D;

pub struct InlineToMemory {
    regs: Box<[u32; ENGINE_REG_COUNT]>,
    /// Accumulated inline data bytes.
    inner_buffer: Vec<u8>,
    /// Bytes written so far in the current transfer.
    write_offset: u32,
    /// Total bytes expected (line_length_in * line_count).
    copy_size: u32,
    /// Linear (true) vs block-linear (false) mode.
    is_linear: bool,
    /// Set when write_offset >= copy_size and copy_size > 0.
    pending_write: bool,
}

impl InlineToMemory {
    pub fn new() -> Self {
        Self {
            regs: Box::new([0u32; ENGINE_REG_COUNT]),
            inner_buffer: Vec::new(),
            write_offset: 0,
            copy_size: 0,
            is_linear: true,
            pending_write: false,
        }
    }

    // ── Typed accessors ──────────────────────────────────────────────────

    fn dst_address(&self) -> u64 {
        let high = self.regs[DST_ADDRESS_HIGH as usize] as u64;
        let low = self.regs[DST_ADDRESS_LOW as usize] as u64;
        (high << 32) | low
    }

    fn dst_pitch(&self) -> u32 {
        self.regs[DST_PITCH as usize]
    }

    fn line_length_in(&self) -> u32 {
        self.regs[LINE_LENGTH_IN as usize]
    }

    fn line_count(&self) -> u32 {
        self.regs[LINE_COUNT as usize]
    }

    // ── Trigger handlers ─────────────────────────────────────────────────

    fn handle_exec(&mut self, value: u32) {
        self.is_linear = (value & 1) != 0;
        if !self.is_linear {
            log::warn!("InlineToMemory: block-linear mode requested, falling back to linear");
            self.is_linear = true;
        }

        let ll = self.line_length_in();
        let lc = self.line_count();
        self.copy_size = ll.saturating_mul(lc);
        self.write_offset = 0;
        self.inner_buffer.clear();
        self.pending_write = false;

        if self.copy_size > 0 {
            self.inner_buffer.reserve(self.copy_size as usize);
        }

        log::debug!(
            "InlineToMemory: EXEC dst=0x{:X} line_len={} lines={} copy_size={} linear={}",
            self.dst_address(),
            ll,
            lc,
            self.copy_size,
            self.is_linear,
        );
    }

    fn handle_data(&mut self, value: u32) {
        if self.copy_size == 0 {
            return;
        }
        if self.write_offset >= self.copy_size {
            return;
        }

        let remaining = self.copy_size - self.write_offset;
        let bytes_to_write = remaining.min(4) as usize;
        let word_bytes = value.to_le_bytes();
        self.inner_buffer
            .extend_from_slice(&word_bytes[..bytes_to_write]);
        self.write_offset += bytes_to_write as u32;

        if self.write_offset >= self.copy_size {
            self.pending_write = true;
            log::debug!(
                "InlineToMemory: transfer complete, {} bytes accumulated",
                self.inner_buffer.len()
            );
        }
    }
}

impl Default for InlineToMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl InlineToMemory {
    fn write_reg(&mut self, method: u32, value: u32) {
        let idx = method as usize;
        if idx < ENGINE_REG_COUNT {
            self.regs[idx] = value;
        }

        match method {
            EXEC => self.handle_exec(value),
            DATA => self.handle_data(value),
            _ => {}
        }

        log::trace!("InlineToMemory: reg[0x{:X}] = 0x{:X}", method, value);
    }

    fn execute_pending(&mut self, _read_gpu: &dyn Fn(u64, &mut [u8])) -> Vec<PendingWrite> {
        if !self.pending_write {
            return vec![];
        }
        self.pending_write = false;

        let ll = self.line_length_in() as usize;
        let lc = self.line_count() as usize;
        let pitch = self.dst_pitch() as usize;

        if ll == 0 || lc == 0 {
            return vec![];
        }

        let data = if lc == 1 || pitch == 0 || pitch == ll {
            // Single line or pitch matches line length: use buffer as-is.
            std::mem::take(&mut self.inner_buffer)
        } else {
            // Multi-line with pitch > line_length: re-layout from packed to pitched.
            let packed = std::mem::take(&mut self.inner_buffer);
            let dst_size = pitch * lc;
            let mut pitched = vec![0u8; dst_size];
            for line in 0..lc {
                let src_off = line * ll;
                let dst_off = line * pitch;
                let copy_len = ll.min(packed.len().saturating_sub(src_off));
                if copy_len > 0 && dst_off + copy_len <= pitched.len() {
                    pitched[dst_off..dst_off + copy_len]
                        .copy_from_slice(&packed[src_off..src_off + copy_len]);
                }
            }
            pitched
        };

        log::debug!(
            "InlineToMemory: write {} bytes to GPU VA 0x{:X}",
            data.len(),
            self.dst_address()
        );

        vec![PendingWrite {
            gpu_va: self.dst_address(),
            data,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exec_sets_copy_size() {
        let mut eng = InlineToMemory::new();
        eng.write_reg(LINE_LENGTH_IN, 16);
        eng.write_reg(LINE_COUNT, 2);
        // EXEC with bit 0 = 1 (linear).
        eng.write_reg(EXEC, 1);
        assert_eq!(eng.copy_size, 32);
        assert!(eng.is_linear);
        assert_eq!(eng.write_offset, 0);
        assert!(!eng.pending_write);
    }

    #[test]
    fn test_single_word_upload() {
        let mut eng = InlineToMemory::new();
        eng.write_reg(DST_ADDRESS_HIGH, 0);
        eng.write_reg(DST_ADDRESS_LOW, 0x1000);
        eng.write_reg(LINE_LENGTH_IN, 4);
        eng.write_reg(LINE_COUNT, 1);
        eng.write_reg(EXEC, 1);

        eng.write_reg(DATA, 0xDEAD_BEEF);
        assert!(eng.pending_write);

        let writes = eng.execute_pending(&|_, _| {});
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, 0x1000);
        assert_eq!(writes[0].data, 0xDEAD_BEEFu32.to_le_bytes().to_vec());
    }

    #[test]
    fn test_multi_word_upload() {
        let mut eng = InlineToMemory::new();
        eng.write_reg(DST_ADDRESS_HIGH, 0);
        eng.write_reg(DST_ADDRESS_LOW, 0x2000);
        eng.write_reg(LINE_LENGTH_IN, 12);
        eng.write_reg(LINE_COUNT, 1);
        eng.write_reg(EXEC, 1);

        eng.write_reg(DATA, 0x1111_1111);
        assert!(!eng.pending_write);
        eng.write_reg(DATA, 0x2222_2222);
        assert!(!eng.pending_write);
        eng.write_reg(DATA, 0x3333_3333);
        assert!(eng.pending_write);

        let writes = eng.execute_pending(&|_, _| {});
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, 0x2000);
        assert_eq!(writes[0].data.len(), 12);
        assert_eq!(&writes[0].data[0..4], &0x1111_1111u32.to_le_bytes());
        assert_eq!(&writes[0].data[4..8], &0x2222_2222u32.to_le_bytes());
        assert_eq!(&writes[0].data[8..12], &0x3333_3333u32.to_le_bytes());
    }

    #[test]
    fn test_partial_last_word() {
        let mut eng = InlineToMemory::new();
        eng.write_reg(DST_ADDRESS_HIGH, 0);
        eng.write_reg(DST_ADDRESS_LOW, 0x3000);
        eng.write_reg(LINE_LENGTH_IN, 5);
        eng.write_reg(LINE_COUNT, 1);
        eng.write_reg(EXEC, 1);

        eng.write_reg(DATA, 0xAABBCCDD);
        assert!(!eng.pending_write);
        // Second word: only 1 byte should be taken (5 - 4 = 1).
        eng.write_reg(DATA, 0x00FF00EE);
        assert!(eng.pending_write);

        let writes = eng.execute_pending(&|_, _| {});
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].data.len(), 5);
        // First 4 bytes from first word.
        assert_eq!(&writes[0].data[0..4], &0xAABBCCDDu32.to_le_bytes());
        // Fifth byte: least significant byte of second word (LE).
        assert_eq!(writes[0].data[4], 0xEE);
    }

    #[test]
    fn test_multi_line_with_pitch() {
        let mut eng = InlineToMemory::new();
        eng.write_reg(DST_ADDRESS_HIGH, 0);
        eng.write_reg(DST_ADDRESS_LOW, 0x4000);
        eng.write_reg(DST_PITCH, 8); // pitch=8, but line_length=4
        eng.write_reg(LINE_LENGTH_IN, 4);
        eng.write_reg(LINE_COUNT, 2);
        eng.write_reg(EXEC, 1);

        // 2 lines * 4 bytes = 8 bytes total = 2 DATA words.
        eng.write_reg(DATA, 0x11223344);
        eng.write_reg(DATA, 0x55667788);
        assert!(eng.pending_write);

        let writes = eng.execute_pending(&|_, _| {});
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, 0x4000);
        // Output: 2 lines * pitch=8 = 16 bytes.
        assert_eq!(writes[0].data.len(), 16);
        // Line 0: 4 bytes data + 4 bytes zero padding.
        assert_eq!(&writes[0].data[0..4], &0x11223344u32.to_le_bytes());
        assert_eq!(&writes[0].data[4..8], &[0, 0, 0, 0]);
        // Line 1: 4 bytes data + 4 bytes zero padding.
        assert_eq!(&writes[0].data[8..12], &0x55667788u32.to_le_bytes());
        assert_eq!(&writes[0].data[12..16], &[0, 0, 0, 0]);
    }

    #[test]
    fn test_data_before_exec_ignored() {
        let mut eng = InlineToMemory::new();
        // DATA writes before EXEC should be ignored.
        eng.write_reg(DATA, 0xDEAD);
        eng.write_reg(DATA, 0xBEEF);
        assert!(!eng.pending_write);

        let writes = eng.execute_pending(&|_, _| {});
        assert!(writes.is_empty());
    }

    #[test]
    fn test_exec_resets_state() {
        let mut eng = InlineToMemory::new();
        eng.write_reg(DST_ADDRESS_HIGH, 0);
        eng.write_reg(DST_ADDRESS_LOW, 0x1000);
        eng.write_reg(LINE_LENGTH_IN, 4);
        eng.write_reg(LINE_COUNT, 1);
        eng.write_reg(EXEC, 1);
        eng.write_reg(DATA, 0xAAAAAAAA);
        assert!(eng.pending_write);
        let writes = eng.execute_pending(&|_, _| {});
        assert_eq!(writes.len(), 1);

        // Second transfer: different destination and size.
        eng.write_reg(DST_ADDRESS_LOW, 0x5000);
        eng.write_reg(LINE_LENGTH_IN, 8);
        eng.write_reg(LINE_COUNT, 1);
        eng.write_reg(EXEC, 1);
        // Old state should be cleared.
        assert_eq!(eng.copy_size, 8);
        assert_eq!(eng.write_offset, 0);
        assert!(!eng.pending_write);

        eng.write_reg(DATA, 0xBBBBBBBB);
        eng.write_reg(DATA, 0xCCCCCCCC);
        assert!(eng.pending_write);

        let writes = eng.execute_pending(&|_, _| {});
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, 0x5000);
        assert_eq!(writes[0].data.len(), 8);
    }

    #[test]
    fn test_write_via_write_reg() {
        let mut eng = InlineToMemory::new();
        // End-to-end through the test engine's register-write entry point.
        eng.write_reg(DST_ADDRESS_HIGH, 0);
        eng.write_reg(DST_ADDRESS_LOW, 0x8000);
        eng.write_reg(LINE_LENGTH_IN, 8);
        eng.write_reg(LINE_COUNT, 1);
        eng.write_reg(EXEC, 1);
        eng.write_reg(DATA, 0x12345678);
        eng.write_reg(DATA, 0x9ABCDEF0);

        let writes = eng.execute_pending(&|_, _| {});
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].gpu_va, 0x8000);
        assert_eq!(writes[0].data.len(), 8);
        assert_eq!(&writes[0].data[0..4], &0x12345678u32.to_le_bytes());
        assert_eq!(&writes[0].data[4..8], &0x9ABCDEF0u32.to_le_bytes());
    }

    #[test]
    fn test_zero_copy_size() {
        let mut eng = InlineToMemory::new();
        eng.write_reg(LINE_LENGTH_IN, 0);
        eng.write_reg(LINE_COUNT, 1);
        eng.write_reg(EXEC, 1);
        assert_eq!(eng.copy_size, 0);

        // DATA writes should be ignored.
        eng.write_reg(DATA, 0xDEAD);
        assert!(!eng.pending_write);

        let writes = eng.execute_pending(&|_, _| {});
        assert!(writes.is_empty());
    }
}

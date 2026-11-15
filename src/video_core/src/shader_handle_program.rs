// SPDX-License-Identifier: GPL-3.0-or-later
//! Draw-time descriptor evaluation; upstream video_core/shader_handle_program.h.

use shader_recompiler::shader_info::{evaluate_handle_program, Info};
use std::cell::RefCell;

/// Evaluate against the currently bound cbufs and guest GPU memory.
/// Rust's memory adapter is callback-based. RefCell only shares the two mutable
/// callbacks between the evaluator's sequential readers; it adds no mutex.
pub fn evaluate_texture_handle(
    info: &Info,
    program: u32,
    cbuf_address: impl FnMut(u32) -> u64,
    read_u32: impl FnMut(u64) -> u32,
) -> u32 {
    let cbuf_address = RefCell::new(cbuf_address);
    let read_u32 = RefCell::new(read_u32);
    evaluate_handle_program(
        &info.handle_programs[program as usize],
        |index, offset| {
            let base = cbuf_address.borrow_mut()(index);
            if base == 0 { 0 } else { read_u32.borrow_mut()(base.wrapping_add(offset as u64)) }
        },
        |binding, offset| {
            let Some(desc) = info.storage_buffers_descriptors.get(binding as usize) else { return 0 };
            let cbuf = cbuf_address.borrow_mut()(desc.cbuf_index);
            if cbuf == 0 { return 0; }
            let mut read = read_u32.borrow_mut();
            let address = cbuf.wrapping_add(desc.cbuf_offset as u64);
            let low = read(address) as u64;
            let high = read(address.wrapping_add(4)) as u64;
            let base = low | (high << 32);
            if base == 0 { 0 } else { read(base.wrapping_add(offset as u64)) }
        },
    ).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shader_recompiler::shader_info::{HandleOp as H, StorageBufferDescriptor};

    #[test]
    fn storage_handle_uses_current_binding_and_full_pointer() {
        let mut info = Info::default();
        info.storage_buffers_descriptors.push(StorageBufferDescriptor {
            cbuf_index: 2, cbuf_offset: 16, count: 1, is_written: false, ..Default::default() });
        info.handle_programs.push(vec![H::Immediate as u32, 12, H::Storage as u32, 0]);
        for handle in [17, 29] {
            let mut reads = Vec::new();
            assert_eq!(evaluate_texture_handle(&info, 0, |index| {
                assert_eq!(index, 2); 0x1000
            }, |address| {
                reads.push(address);
                match address { 0x1010 => 0x2000, 0x1014 => 3, 0x30000200c => handle,
                    _ => panic!("unexpected GPU read {address:x}") }
            }), handle);
            assert_eq!(reads, [0x1010, 0x1014, 0x30000200c]);
        }
        assert_eq!(evaluate_texture_handle(&info, 0, |_| 0, |_| panic!("unbound cbuf")), 0);
        info.handle_programs[0] = vec![H::Immediate as u32, 0, H::Storage as u32, 7];
        assert_eq!(evaluate_texture_handle(&info, 0, |_| panic!("invalid binding"), |_| 0), 0);
    }
}

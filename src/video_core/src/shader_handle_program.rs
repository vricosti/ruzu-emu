// SPDX-License-Identifier: GPL-3.0-or-later
//! Draw-time descriptor evaluation; upstream video_core/shader_handle_program.h.

use shader_recompiler::shader_info::{evaluate_handle_program, Info};
use std::cell::RefCell;

/// The address of storage buffer `binding` of a shader as its descriptor in a
/// constant buffer gives it; 0 when it has none. `storage_alignment` is the
/// host storage buffer offset alignment: storage offsets in a handle program
/// are relative to the buffer address aligned down to it (the translation's
/// `storage_offset`), which is also where the buffer cache binds the buffer.
///
/// Lichii: `video_core/shader_handle_program.h:24-41` `StorageBufferBase`.
pub fn storage_buffer_base(
    info: &Info,
    binding: u32,
    cbuf_address: &mut impl FnMut(u32) -> u64,
    read_u32: &mut impl FnMut(u64) -> u32,
    storage_alignment: u64,
) -> u64 {
    let Some(descriptor) = info.storage_buffers_descriptors.get(binding as usize) else {
        return 0;
    };
    let cbuf = cbuf_address(descriptor.cbuf_index);
    if cbuf == 0 {
        return 0;
    }
    let mut read_u64 = |address: u64| {
        let low = read_u32(address) as u64;
        let high = read_u32(address.wrapping_add(4)) as u64;
        low | (high << 32)
    };
    let mut base = read_u64(cbuf.wrapping_add(descriptor.cbuf_offset as u64));
    if base != 0 && descriptor.indirect_offset != u32::MAX {
        // The table entry is read at its exact address. Only the buffer
        // address it holds is aligned down, as the shader does.
        base = read_u64(base.wrapping_add(descriptor.indirect_offset as u64));
    }
    base & !(storage_alignment.max(1) - 1)
}

/// The texture handle a shader reads from a storage buffer, computed from the
/// current constant buffer and storage buffer words (`Info::handle_programs`).
/// `cbuf_address(index)` is the bound constant buffer's address, 0 when none
/// is bound. `storage_alignment` is as for `storage_buffer_base`. `element` is
/// the descriptor array element, for a program computing one handle per
/// element.
///
/// Lichii: `video_core/shader_handle_program.h:43-66` `EvaluateTextureHandle`.
/// Rust's memory adapter is callback-based. RefCell only shares the two
/// mutable callbacks between the evaluator's sequential readers; it adds no
/// mutex.
pub fn evaluate_texture_handle(
    info: &Info,
    program: u32,
    cbuf_address: impl FnMut(u32) -> u64,
    read_u32: impl FnMut(u64) -> u32,
    storage_alignment: u64,
    element: u32,
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
            let base = storage_buffer_base(
                info,
                binding,
                &mut *cbuf_address.borrow_mut(),
                &mut *read_u32.borrow_mut(),
                storage_alignment,
            );
            if base == 0 { 0 } else { read_u32.borrow_mut()(base.wrapping_add(offset as u64)) }
        },
        element,
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
            }, 1, 0), handle);
            assert_eq!(reads, [0x1010, 0x1014, 0x30000200c]);
        }
        assert_eq!(evaluate_texture_handle(&info, 0, |_| 0, |_| panic!("unbound cbuf"), 1, 0), 0);
        info.handle_programs[0] = vec![H::Immediate as u32, 0, H::Storage as u32, 7];
        assert_eq!(evaluate_texture_handle(&info, 0, |_| panic!("invalid binding"), |_| 0, 1, 0), 0);
    }

    #[test]
    fn storage_offsets_are_relative_to_the_aligned_down_base() {
        // The translation's storage_offset subtracts the buffer address aligned
        // down to min_ssbo_alignment; the evaluator must read from that base.
        let mut info = Info::default();
        info.storage_buffers_descriptors.push(StorageBufferDescriptor {
            cbuf_index: 0, cbuf_offset: 0x20, count: 1, ..Default::default() });
        info.handle_programs.push(vec![H::Immediate as u32, 0x18, H::Storage as u32, 0]);
        let read = |address: u64| match address {
            0x1020 => 0x2010, 0x1024 => 0,
            0x2018 => 7, // aligned base 0x2000 + 0x18
            0x2028 => 9, // unaligned base 0x2010 + 0x18
            _ => panic!("unexpected GPU read {address:x}"),
        };
        assert_eq!(evaluate_texture_handle(&info, 0, |_| 0x1000, read, 0x100, 0), 7);
        assert_eq!(evaluate_texture_handle(&info, 0, |_| 0x1000, read, 1, 0), 9);
    }

    #[test]
    fn indirect_storage_reads_the_table_entry_then_aligns_its_address() {
        let mut info = Info::default();
        info.storage_buffers_descriptors.push(StorageBufferDescriptor {
            cbuf_index: 1, cbuf_offset: 8, count: 1, indirect_offset: 0x10, ..Default::default() });
        info.handle_programs.push(vec![H::Immediate as u32, 4, H::Storage as u32, 0]);
        let read = |address: u64| match address {
            0x5008 => 0x6003, 0x500c => 0, // holder pointer, read exactly
            0x6013 => 0x7044, 0x6017 => 0, // table entry at holder + indirect_offset
            0x7044 => 0x55,                // aligned base 0x7040 + 4
            _ => panic!("unexpected GPU read {address:x}"),
        };
        assert_eq!(evaluate_texture_handle(&info, 0, |_| 0x5000, read, 0x10, 0), 0x55);
    }

    #[test]
    fn handle_table_elements_evaluate_one_handle_each() {
        let mut info = Info::default();
        info.storage_buffers_descriptors.push(StorageBufferDescriptor {
            cbuf_index: 0, cbuf_offset: 0, count: 1, ..Default::default() });
        info.handle_programs.push(vec![H::Index as u32, H::Immediate as u32, 0x60, H::Mul as u32,
                                       H::Storage as u32, 0]);
        for element in [0u32, 1, 63] {
            let handle = evaluate_texture_handle(&info, 0, |_| 0x1000, |address| match address {
                0x1000 => 0x8000, 0x1004 => 0,
                address => (address - 0x8000) as u32 / 0x60 + 100,
            }, 4, element);
            assert_eq!(handle, element + 100);
        }
    }
}

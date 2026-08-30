/// Number of 128-bit spill slots for register allocation.
pub const SPILL_COUNT: usize = 64;

/// Stack frame layout used during JIT dispatcher execution.
///
/// This structure lives on the x86-64 stack during guest code execution.
/// Must be 16-byte aligned for XMM operations.
#[repr(C, align(16))]
pub struct StackLayout {
    /// Spill area for register allocation (64 × 128-bit = 1024 bytes).
    pub spill: [[u64; 2]; SPILL_COUNT],
    /// Remaining cycle budget for the current run.
    pub cycles_remaining: i64,
    /// Original cycle budget allocated for this run.
    pub cycles_to_run: i64,
    /// Saved host MXCSR register value.
    pub save_host_mxcsr: u32,
    /// Check bit flag (used by CheckBit terminal).
    pub check_bit: u8,
    /// Explicit padding before the saved base pointer.
    _pad: [u8; 3],
    /// RBP value saved at entry to each emitted guest block.
    pub abi_base_pointer: u64,
}

impl StackLayout {
    /// Byte offset of a spill slot from the base of StackLayout.
    pub const fn spill_offset(index: usize) -> usize {
        // offset of spill field + index * 16
        core::mem::offset_of!(StackLayout, spill) + index * 16
    }

    /// Byte offset of cycles_remaining from the base of StackLayout.
    pub const fn cycles_remaining_offset() -> usize {
        core::mem::offset_of!(StackLayout, cycles_remaining)
    }

    /// Byte offset of cycles_to_run from the base of StackLayout.
    pub const fn cycles_to_run_offset() -> usize {
        core::mem::offset_of!(StackLayout, cycles_to_run)
    }

    /// Byte offset of save_host_mxcsr from the base of StackLayout.
    pub const fn save_host_mxcsr_offset() -> usize {
        core::mem::offset_of!(StackLayout, save_host_mxcsr)
    }

    /// Byte offset of check_bit from the base of StackLayout.
    pub const fn check_bit_offset() -> usize {
        core::mem::offset_of!(StackLayout, check_bit)
    }
}

const _: () = assert!(
    core::mem::size_of::<StackLayout>().is_multiple_of(16),
    "StackLayout must be 16-byte aligned in size"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stack_layout_alignment() {
        assert_eq!(core::mem::align_of::<StackLayout>(), 16);
        assert_eq!(core::mem::size_of::<StackLayout>(), 1056);
        assert_eq!(core::mem::offset_of!(StackLayout, spill), 0);
        assert_eq!(StackLayout::cycles_remaining_offset(), 1024);
        assert_eq!(StackLayout::cycles_to_run_offset(), 1032);
        assert_eq!(StackLayout::save_host_mxcsr_offset(), 1040);
        assert_eq!(StackLayout::check_bit_offset(), 1044);
        assert_eq!(core::mem::offset_of!(StackLayout, abi_base_pointer), 1048);
    }

    #[test]
    fn test_spill_offset() {
        let offset0 = StackLayout::spill_offset(0);
        let offset1 = StackLayout::spill_offset(1);
        assert_eq!(offset1 - offset0, 16); // each spill slot is 128 bits
    }
}

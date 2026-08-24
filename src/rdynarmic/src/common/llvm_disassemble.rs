//! Optional LLVM disassembly interface.
//!
//! Upstream owner: `dynarmic/common/llvm_disassemble.{h,cpp}`.

/// Return the same diagnostic emitted by Eden when `DYNARMIC_USE_LLVM` is disabled.
///
/// Upstream owner: `common/llvm_disassemble.{h,cpp}`.
pub fn disassemble_x64(begin: *const u8, end: *const u8) -> String {
    format!(
        "(recompile with DYNARMIC_USE_LLVM=ON to disassemble the generated x86_64 code)\n\
         start: {:016x}, end: {:016x}\n",
        begin as usize as u64, end as usize as u64
    )
}

/// Return the disabled-disassembly result used by Eden without LLVM support.
pub fn disassemble_aarch32(
    _is_thumb: bool,
    _pc: u32,
    _instructions: *const u8,
    _length: usize,
) -> String {
    "(disassembly disabled)\n".to_owned()
}

/// Return the disabled-disassembly result used by Eden without LLVM support.
pub fn disassemble_aarch64(_instruction: u32, _pc: u64) -> String {
    "(disassembly disabled)\n".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x64_disabled_message_matches_upstream_format() {
        assert_eq!(
            disassemble_x64(0x1234usize as *const u8, 0x5678usize as *const u8),
            "(recompile with DYNARMIC_USE_LLVM=ON to disassemble the generated x86_64 code)\n\
             start: 0000000000001234, end: 0000000000005678\n"
        );
    }

    #[test]
    fn arm_disabled_messages_match_upstream() {
        assert_eq!(
            disassemble_aarch32(false, 0, std::ptr::null(), 0),
            "(disassembly disabled)\n"
        );
        assert_eq!(disassemble_aarch64(0, 0), "(disassembly disabled)\n");
    }
}

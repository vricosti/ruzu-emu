use crate::backend::x64::hostloc::*;
use rxbyak::{xmmword_ptr, CodeAssembler, RegExp, RSP};

// ── System V x86-64 ABI (Linux / macOS) ──────────────────────────────────────
//   Params:        RDI, RSI, RDX, RCX, R8, R9
//   Return:        RAX (+ RDX for 128-bit)
//   Caller-saved:  RAX RCX RDX RDI RSI R8-R11 XMM0-XMM15
//   Callee-saved:  RBX RBP R12-R15
//
// ── Windows x64 ABI ──────────────────────────────────────────────────────────
//   Params:        RCX, RDX, R8, R9
//   Return:        RAX
//   Caller-saved:  RAX RCX RDX R8-R11 XMM0-XMM5
//   Callee-saved:  RBX RSI RDI RBP R12-R15 XMM6-XMM15
//   Shadow space:  32 bytes (allocated by caller above the return address)

/// First integer return register.
pub const ABI_RETURN: HostLoc = HOST_RAX;

/// Second integer return register (for 128-bit returns).
pub const ABI_RETURN2: HostLoc = HOST_RDX;

/// Shadow space size: 0 on System V, 32 on Windows.
#[cfg(target_os = "windows")]
pub const ABI_SHADOW_SPACE: usize = 32;
#[cfg(not(target_os = "windows"))]
pub const ABI_SHADOW_SPACE: usize = 0;

// ── Parameter registers ───────────────────────────────────────────────────────

/// Number of integer parameter registers.
#[cfg(target_os = "windows")]
pub const ABI_PARAM_COUNT: usize = 4;
#[cfg(not(target_os = "windows"))]
pub const ABI_PARAM_COUNT: usize = 6;

/// Integer parameter registers in order.
#[cfg(target_os = "windows")]
pub const ABI_PARAMS: [HostLoc; 4] = [HOST_RCX, HOST_RDX, HOST_R8, HOST_R9];
#[cfg(not(target_os = "windows"))]
pub const ABI_PARAMS: [HostLoc; 6] = [HOST_RDI, HOST_RSI, HOST_RDX, HOST_RCX, HOST_R8, HOST_R9];

// ── Caller-saved registers ────────────────────────────────────────────────────

/// Caller-saved GPRs (clobbered across calls; callee need not preserve them).
#[cfg(target_os = "windows")]
pub const CALLER_SAVE_GPRS: &[HostLoc] = &[
    HOST_RAX, HOST_RCX, HOST_RDX, HOST_R8, HOST_R9, HOST_R10, HOST_R11,
];
#[cfg(not(target_os = "windows"))]
pub const CALLER_SAVE_GPRS: &[HostLoc] = &[
    HOST_RAX, HOST_RCX, HOST_RDX, HOST_RDI, HOST_RSI, HOST_R8, HOST_R9, HOST_R10, HOST_R11,
];

/// Caller-saved XMM registers (clobbered across calls).
/// On Windows: XMM0-XMM5 only (XMM6-XMM15 are callee-saved).
/// On System V: all XMMs are caller-saved.
#[cfg(target_os = "windows")]
pub const CALLER_SAVE_XMMS: &[HostLoc] = &[
    HostLoc::Xmm(0),
    HostLoc::Xmm(1),
    HostLoc::Xmm(2),
    HostLoc::Xmm(3),
    HostLoc::Xmm(4),
    HostLoc::Xmm(5),
];
#[cfg(not(target_os = "windows"))]
pub const CALLER_SAVE_XMMS: &[HostLoc] = &[
    HostLoc::Xmm(0),
    HostLoc::Xmm(1),
    HostLoc::Xmm(2),
    HostLoc::Xmm(3),
    HostLoc::Xmm(4),
    HostLoc::Xmm(5),
    HostLoc::Xmm(6),
    HostLoc::Xmm(7),
    HostLoc::Xmm(8),
    HostLoc::Xmm(9),
    HostLoc::Xmm(10),
    HostLoc::Xmm(11),
    HostLoc::Xmm(12),
    HostLoc::Xmm(13),
    HostLoc::Xmm(14),
    HostLoc::Xmm(15),
];

// ── Callee-saved registers ────────────────────────────────────────────────────

/// Callee-saved GPRs (must be preserved across calls).
/// Windows adds RSI and RDI to the callee-saved set; push order matters for
/// the UNWIND_INFO prologue description.
#[cfg(target_os = "windows")]
pub const CALLEE_SAVE_GPRS: &[HostLoc] = &[
    HOST_RBX, HOST_RSI, HOST_RDI, HOST_RBP, HOST_R12, HOST_R13, HOST_R14, HOST_R15,
];
#[cfg(not(target_os = "windows"))]
pub const CALLEE_SAVE_GPRS: &[HostLoc] =
    &[HOST_RBX, HOST_RBP, HOST_R12, HOST_R13, HOST_R14, HOST_R15];

/// Callee-saved XMM registers.  Only Windows has these (XMM6-XMM15).
/// System V callee-saves no XMMs.
#[cfg(target_os = "windows")]
pub const CALLEE_SAVE_XMMS: &[u8] = &[6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
#[cfg(not(target_os = "windows"))]
pub const CALLEE_SAVE_XMMS: &[u8] = &[];

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Get the nth ABI parameter register.
pub fn abi_param(n: usize) -> HostLoc {
    assert!(n < ABI_PARAM_COUNT, "ABI param index {} out of range", n);
    ABI_PARAMS[n]
}

/// Stack/register state emitted by
/// `push_caller_save_registers_and_adjust_stack_except`.
pub struct CallerSaveFrame {
    registers: Vec<HostLoc>,
    stack_subtraction: usize,
    xmm_offset: usize,
}

/// Port of upstream `ABI_PushCallerSaveRegistersAndAdjustStack`.
pub fn push_caller_save_registers_and_adjust_stack(
    code: &mut CodeAssembler,
) -> rxbyak::Result<CallerSaveFrame> {
    push_caller_save_registers_and_adjust_stack_except(code, None)
}

/// Port of upstream `ABI_PushCallerSaveRegistersAndAdjustStackExcept`.
pub fn push_caller_save_registers_and_adjust_stack_except(
    code: &mut CodeAssembler,
    exception: Option<HostLoc>,
) -> rxbyak::Result<CallerSaveFrame> {
    push_caller_save_registers_and_adjust_stack_except_with_local(code, exception, 0)
        .map(|(frame, _)| frame)
}

/// Variant of `push_caller_save_registers_and_adjust_stack_except` that also
/// reserves an aligned local area while keeping Win64 shadow space at the
/// bottom of the call frame. The returned offset is relative to the adjusted
/// RSP and points to the first local byte.
pub fn push_caller_save_registers_and_adjust_stack_except_with_local(
    code: &mut CodeAssembler,
    exception: Option<HostLoc>,
    local_size: usize,
) -> rxbyak::Result<(CallerSaveFrame, usize)> {
    assert_eq!(
        local_size % 16,
        0,
        "local frame size must be 16-byte aligned"
    );
    let registers: Vec<_> = CALLER_SAVE_GPRS
        .iter()
        .chain(CALLER_SAVE_XMMS.iter())
        .copied()
        .filter(|register| Some(*register) != exception)
        .collect();
    let num_gprs = registers
        .iter()
        .filter(|register| register.is_gpr())
        .count();
    let num_xmms = registers
        .iter()
        .filter(|register| register.is_xmm())
        .count();

    // The fallback is entered as a fake call, with its return address already
    // pushed. This is upstream CalculateFrameInfo with frame_size == 0.
    let rsp_alignment = if num_gprs % 2 == 0 { 8 } else { 0 };
    let local_offset = ABI_SHADOW_SPACE;
    let xmm_offset = local_offset + local_size;
    let stack_subtraction = rsp_alignment + num_xmms * 16 + ABI_SHADOW_SPACE + local_size;

    for register in &registers {
        if register.is_gpr() {
            code.push(register.to_reg64())?;
        }
    }
    if stack_subtraction != 0 {
        code.sub(RSP, stack_subtraction as i32)?;
    }

    let mut offset = xmm_offset;
    for register in &registers {
        if register.is_xmm() {
            code.movaps(
                xmmword_ptr(RegExp::from(RSP) + offset as i32),
                register.to_xmm(),
            )?;
            offset += 16;
        }
    }

    Ok((
        CallerSaveFrame {
            registers,
            stack_subtraction,
            xmm_offset,
        },
        local_offset,
    ))
}

/// Port of upstream `ABI_PopCallerSaveRegistersAndAdjustStack`.
pub fn pop_caller_save_registers_and_adjust_stack(
    code: &mut CodeAssembler,
    frame: &CallerSaveFrame,
) -> rxbyak::Result<()> {
    let mut offset = frame.xmm_offset;
    for register in &frame.registers {
        if register.is_xmm() {
            code.movaps(
                register.to_xmm(),
                xmmword_ptr(RegExp::from(RSP) + offset as i32),
            )?;
            offset += 16;
        }
    }

    if frame.stack_subtraction != 0 {
        code.add(RSP, frame.stack_subtraction as i32)?;
    }
    for register in frame.registers.iter().rev() {
        if register.is_gpr() {
            code.pop(register.to_reg64())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(target_arch = "x86_64", not(target_os = "windows")))]
    use rxbyak::{qword_ptr, RAX, RDI, XMM1, XMM14};

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn test_abi_params_sysv() {
        assert_eq!(abi_param(0), HOST_RDI);
        assert_eq!(abi_param(1), HOST_RSI);
        assert_eq!(abi_param(2), HOST_RDX);
        assert_eq!(abi_param(3), HOST_RCX);
        assert_eq!(abi_param(4), HOST_R8);
        assert_eq!(abi_param(5), HOST_R9);
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn test_abi_params_win64() {
        assert_eq!(abi_param(0), HOST_RCX);
        assert_eq!(abi_param(1), HOST_RDX);
        assert_eq!(abi_param(2), HOST_R8);
        assert_eq!(abi_param(3), HOST_R9);
    }

    #[test]
    fn test_callee_save_no_overlap_with_caller_save() {
        for cs in CALLEE_SAVE_GPRS {
            assert!(
                !CALLER_SAVE_GPRS.contains(cs),
                "Register {:?} should not be both caller-save and callee-save",
                cs
            );
        }
    }

    #[test]
    fn test_register_count() {
        // Total GPRs (excluding RSP) = 15
        let total = CALLER_SAVE_GPRS.len() + CALLEE_SAVE_GPRS.len();
        assert_eq!(total, 15);
    }

    #[cfg(all(target_arch = "x86_64", not(target_os = "windows")))]
    extern "C" fn clobber_caller_save_xmms() {
        unsafe {
            std::arch::asm!(
                "pcmpeqd xmm1, xmm1",
                "pcmpeqd xmm14, xmm14",
                out("xmm1") _,
                out("xmm14") _,
                options(nostack)
            );
        }
    }

    #[test]
    #[cfg(all(target_arch = "x86_64", not(target_os = "windows")))]
    fn test_caller_save_frame_preserves_xmms_across_callback() {
        let expected_xmm1 = 0x0123_4567_89AB_CDEFu64;
        let expected_xmm14 = 0xFEDC_BA98_7654_3210u64;
        let mut result = [0u64; 2];
        let mut code = CodeAssembler::new(4096).unwrap();

        code.mov(RAX, expected_xmm1 as i64).unwrap();
        code.movq(XMM1, RAX).unwrap();
        code.mov(RAX, expected_xmm14 as i64).unwrap();
        code.movq(XMM14, RAX).unwrap();
        let frame = push_caller_save_registers_and_adjust_stack(&mut code).unwrap();
        code.mov(RAX, clobber_caller_save_xmms as *const () as usize as i64)
            .unwrap();
        code.call_reg(RAX).unwrap();
        pop_caller_save_registers_and_adjust_stack(&mut code, &frame).unwrap();
        code.movq(qword_ptr(RegExp::from(RDI)), XMM1).unwrap();
        code.movq(qword_ptr(RegExp::from(RDI) + 8), XMM14).unwrap();
        code.ret().unwrap();
        code.ready().unwrap();

        let function: extern "C" fn(*mut [u64; 2]) = unsafe { std::mem::transmute(code.top()) };
        function(&mut result);
        assert_eq!(result, [expected_xmm1, expected_xmm14]);
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn test_callee_save_xmms_windows() {
        assert_eq!(CALLEE_SAVE_XMMS.len(), 10);
        assert_eq!(CALLEE_SAVE_XMMS[0], 6);
        assert_eq!(CALLEE_SAVE_XMMS[9], 15);
    }
}

// SPDX-FileCopyrightText: 2017 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `core/arm/dynarmic/dynarmic_cp15.{h,cpp}`.

use std::cell::UnsafeCell;
use std::ffi::c_void;
#[cfg(not(all(target_env = "msvc", target_arch = "x86_64")))]
use std::sync::atomic::fence;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;

use rdynarmic::interface::a32::coprocessor::{
    Callback, CallbackOrAccessOneWord, CallbackOrAccessTwoWords, Coprocessor,
};
use rdynarmic::interface::a32::coprocessor_util::CoprocReg;

use super::arm_dynarmic_32::ArmDynarmic32;

/// CP15 coprocessor state.
///
/// Upstream embeds plain `u32` values and gives Dynarmic stable pointers to
/// them. `UnsafeCell` preserves that exact direct-memory-access contract while
/// allowing the compiler-facing methods to take `&self`.
pub struct DynarmicCP15 {
    parent: Arc<AtomicPtr<ArmDynarmic32>>,
    dummy_value: UnsafeCell<u32>,
    uprw: UnsafeCell<u32>,
    uro: UnsafeCell<u32>,
}

// The generated code and the owning A32 CPU use these cells on the same guest
// execution thread. The parent pointer remains valid for the coprocessor/JIT
// lifetime and is published before guest execution starts.
unsafe impl Send for DynarmicCP15 {}
unsafe impl Sync for DynarmicCP15 {}

impl DynarmicCP15 {
    pub fn new(parent: Arc<AtomicPtr<ArmDynarmic32>>) -> Self {
        Self {
            parent,
            dummy_value: UnsafeCell::new(0),
            uprw: UnsafeCell::new(0),
            uro: UnsafeCell::new(0),
        }
    }

    pub fn uprw(&self) -> u32 {
        // SAFETY: See the single-guest-thread invariant on the Sync impl.
        unsafe { *self.uprw.get() }
    }

    pub fn set_uprw(&self, value: u32) {
        // SAFETY: See the single-guest-thread invariant on the Sync impl.
        unsafe { *self.uprw.get() = value }
    }

    pub fn uro(&self) -> u32 {
        // SAFETY: See the single-guest-thread invariant on the Sync impl.
        unsafe { *self.uro.get() }
    }

    pub fn set_uro(&self, value: u32) {
        // SAFETY: See the single-guest-thread invariant on the Sync impl.
        unsafe { *self.uro.get() = value }
    }
}

unsafe extern "C" fn data_sync_barrier(_: *mut c_void, _: u32, _: u32) -> u64 {
    #[cfg(all(target_env = "msvc", target_arch = "x86_64"))]
    unsafe {
        std::arch::x86_64::_mm_mfence();
        std::arch::x86_64::_mm_lfence();
    }
    #[cfg(not(all(target_env = "msvc", target_arch = "x86_64")))]
    fence(Ordering::SeqCst);
    0
}

unsafe extern "C" fn data_memory_barrier(_: *mut c_void, _: u32, _: u32) -> u64 {
    #[cfg(all(target_env = "msvc", target_arch = "x86_64"))]
    unsafe {
        std::arch::x86_64::_mm_mfence();
    }
    #[cfg(not(all(target_env = "msvc", target_arch = "x86_64")))]
    fence(Ordering::SeqCst);
    0
}

unsafe extern "C" fn get_cntpct(parent: *mut c_void, _: u32, _: u32) -> u64 {
    let parent = parent.cast::<AtomicPtr<ArmDynarmic32>>();
    // SAFETY: `parent` points into the Arc retained by DynarmicCP15. The
    // ArmDynarmic32 pointer is published before the JIT can invoke callbacks.
    let parent = unsafe { (*parent).load(Ordering::Acquire) };
    assert!(!parent.is_null(), "A32 parent pointer is not initialized");
    // SAFETY: The pointed-to ArmDynarmic32 owns this coprocessor/JIT and remains
    // at a stable address for their complete lifetime.
    unsafe { (*parent).clock_ticks() }
}

impl Coprocessor for DynarmicCP15 {
    fn compile_internal_operation(
        &self,
        two: bool,
        opc1: u32,
        crd: CoprocReg,
        crn: CoprocReg,
        crm: CoprocReg,
        opc2: u32,
    ) -> Option<Callback> {
        log::error!(
            "CP15: cdp{} p15, {}, {:?}, {:?}, {:?}, {}",
            if two { "2" } else { "" },
            opc1,
            crd,
            crn,
            crm,
            opc2
        );
        None
    }

    fn compile_send_one_word(
        &self,
        two: bool,
        opc1: u32,
        crn: CoprocReg,
        crm: CoprocReg,
        opc2: u32,
    ) -> CallbackOrAccessOneWord {
        if !two && crn == CoprocReg::C7 && opc1 == 0 && crm == CoprocReg::C5 && opc2 == 4 {
            return CallbackOrAccessOneWord::Memory(self.dummy_value.get());
        }

        if !two && crn == CoprocReg::C7 && opc1 == 0 && crm == CoprocReg::C10 {
            match opc2 {
                4 => {
                    return CallbackOrAccessOneWord::Callback(Callback {
                        function: data_sync_barrier,
                        user_arg: None,
                    })
                }
                5 => {
                    return CallbackOrAccessOneWord::Callback(Callback {
                        function: data_memory_barrier,
                        user_arg: None,
                    })
                }
                _ => {}
            }
        }

        if !two && crn == CoprocReg::C13 && opc1 == 0 && crm == CoprocReg::C0 && opc2 == 2 {
            return CallbackOrAccessOneWord::Memory(self.uprw.get());
        }

        log::error!(
            "CP15: mcr{} p15, {}, <Rt>, {:?}, {:?}, {}",
            if two { "2" } else { "" },
            opc1,
            crn,
            crm,
            opc2
        );
        CallbackOrAccessOneWord::CoprocessorException
    }

    fn compile_send_two_words(
        &self,
        two: bool,
        opc: u32,
        crm: CoprocReg,
    ) -> CallbackOrAccessTwoWords {
        log::error!(
            "CP15: mcrr{} p15, {}, <Rt>, <Rt2>, {:?}",
            if two { "2" } else { "" },
            opc,
            crm
        );
        CallbackOrAccessTwoWords::CoprocessorException
    }

    fn compile_get_one_word(
        &self,
        two: bool,
        opc1: u32,
        crn: CoprocReg,
        crm: CoprocReg,
        opc2: u32,
    ) -> CallbackOrAccessOneWord {
        if !two && crn == CoprocReg::C13 && opc1 == 0 && crm == CoprocReg::C0 {
            match opc2 {
                2 => return CallbackOrAccessOneWord::Memory(self.uprw.get()),
                3 => return CallbackOrAccessOneWord::Memory(self.uro.get()),
                _ => {}
            }
        }

        log::error!(
            "CP15: mrc{} p15, {}, <Rt>, {:?}, {:?}, {}",
            if two { "2" } else { "" },
            opc1,
            crn,
            crm,
            opc2
        );
        CallbackOrAccessOneWord::CoprocessorException
    }

    fn compile_get_two_words(
        &self,
        two: bool,
        opc: u32,
        crm: CoprocReg,
    ) -> CallbackOrAccessTwoWords {
        if !two && opc == 0 && crm == CoprocReg::C14 {
            return CallbackOrAccessTwoWords::Callback(Callback {
                function: get_cntpct,
                user_arg: Some(Arc::as_ptr(&self.parent).cast_mut().cast()),
            });
        }

        log::error!(
            "CP15: mrrc{} p15, {}, <Rt>, <Rt2>, {:?}",
            if two { "2" } else { "" },
            opc,
            crm
        );
        CallbackOrAccessTwoWords::CoprocessorException
    }

    fn compile_load_words(
        &self,
        two: bool,
        long_transfer: bool,
        crd: CoprocReg,
        option: Option<u8>,
    ) -> Option<Callback> {
        if let Some(option) = option {
            log::error!(
                "CP15: mrrc{}{} p15, {:?}, [...], {}",
                if two { "2" } else { "" },
                if long_transfer { "l" } else { "" },
                crd,
                option
            );
        } else {
            log::error!(
                "CP15: mrrc{}{} p15, {:?}, [...]",
                if two { "2" } else { "" },
                if long_transfer { "l" } else { "" },
                crd
            );
        }
        None
    }

    fn compile_store_words(
        &self,
        two: bool,
        long_transfer: bool,
        crd: CoprocReg,
        option: Option<u8>,
    ) -> Option<Callback> {
        if let Some(option) = option {
            log::error!(
                "CP15: mrrc{}{} p15, {:?}, [...], {}",
                if two { "2" } else { "" },
                if long_transfer { "l" } else { "" },
                crd,
                option
            );
        } else {
            log::error!(
                "CP15: mrrc{}{} p15, {:?}, [...]",
                if two { "2" } else { "" },
                if long_transfer { "l" } else { "" },
                crd
            );
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_accesses_match_upstream_cp15_contract() {
        let cp15 = DynarmicCP15::new(Arc::new(AtomicPtr::new(std::ptr::null_mut())));

        let CallbackOrAccessOneWord::Memory(dummy) =
            cp15.compile_send_one_word(false, 0, CoprocReg::C7, CoprocReg::C5, 4)
        else {
            panic!("prefetch flush must compile to the dummy memory access");
        };
        assert_ne!(dummy, cp15.uprw.get());

        let CallbackOrAccessOneWord::Memory(uprw) =
            cp15.compile_get_one_word(false, 0, CoprocReg::C13, CoprocReg::C0, 2)
        else {
            panic!("TPIDRURW must compile to direct memory access");
        };
        assert_eq!(uprw, cp15.uprw.get());

        let CallbackOrAccessOneWord::Memory(uro) =
            cp15.compile_get_one_word(false, 0, CoprocReg::C13, CoprocReg::C0, 3)
        else {
            panic!("TPIDRURO must compile to direct memory access");
        };
        assert_eq!(uro, cp15.uro.get());
    }

    #[test]
    fn barriers_are_runtime_callbacks_not_compile_time_fences() {
        let cp15 = DynarmicCP15::new(Arc::new(AtomicPtr::new(std::ptr::null_mut())));
        for opc2 in [4, 5] {
            assert!(matches!(
                cp15.compile_send_one_word(false, 0, CoprocReg::C7, CoprocReg::C10, opc2),
                CallbackOrAccessOneWord::Callback(Callback { user_arg: None, .. })
            ));
        }
    }
}

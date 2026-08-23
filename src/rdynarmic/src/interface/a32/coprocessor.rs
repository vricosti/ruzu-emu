use std::ffi::c_void;

use super::coprocessor_util::CoprocReg;

/// A host callback selected while compiling an A32 coprocessor operation.
///
/// Upstream owner: `interface/A32/coprocessor.h::Coprocessor::Callback`.
#[derive(Clone, Copy, Debug)]
pub struct Callback {
    pub function: unsafe extern "C" fn(*mut c_void, u32, u32) -> u64,
    /// When absent, the generated call leaves the first ABI argument
    /// unspecified, matching upstream's callback contract.
    pub user_arg: Option<*mut c_void>,
}

/// Compile-time action for MCR/MRC operations.
#[derive(Clone, Copy, Debug)]
pub enum CallbackOrAccessOneWord {
    CoprocessorException,
    Callback(Callback),
    Memory(*mut u32),
}

/// Compile-time action for MCRR/MRRC operations.
#[derive(Clone, Copy, Debug)]
pub enum CallbackOrAccessTwoWords {
    CoprocessorException,
    Callback(Callback),
    Memory([*mut u32; 2]),
}

/// Configurable A32 coprocessor interface.
///
/// Each method is called while compiling the corresponding guest operation.
/// Returning a coprocessor-exception action preserves Eden's compile-time
/// rejection path; callbacks and memory pointers are embedded in generated
/// host code.
pub trait Coprocessor: Send + Sync {
    fn compile_internal_operation(
        &self,
        two: bool,
        opc1: u32,
        crd: CoprocReg,
        crn: CoprocReg,
        crm: CoprocReg,
        opc2: u32,
    ) -> Option<Callback>;

    fn compile_send_one_word(
        &self,
        two: bool,
        opc1: u32,
        crn: CoprocReg,
        crm: CoprocReg,
        opc2: u32,
    ) -> CallbackOrAccessOneWord;

    fn compile_send_two_words(
        &self,
        two: bool,
        opc: u32,
        crm: CoprocReg,
    ) -> CallbackOrAccessTwoWords;

    fn compile_get_one_word(
        &self,
        two: bool,
        opc1: u32,
        crn: CoprocReg,
        crm: CoprocReg,
        opc2: u32,
    ) -> CallbackOrAccessOneWord;

    fn compile_get_two_words(
        &self,
        two: bool,
        opc: u32,
        crm: CoprocReg,
    ) -> CallbackOrAccessTwoWords;

    fn compile_load_words(
        &self,
        two: bool,
        long_transfer: bool,
        crd: CoprocReg,
        option: Option<u8>,
    ) -> Option<Callback>;

    fn compile_store_words(
        &self,
        two: bool,
        long_transfer: bool,
        crd: CoprocReg,
        option: Option<u8>,
    ) -> Option<Callback>;
}

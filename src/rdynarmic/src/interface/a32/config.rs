use std::sync::Arc;

use super::coprocessor::Coprocessor;

/// Exception reported through `A32::UserCallbacks::ExceptionRaised`.
///
/// Upstream owner: `interface/A32/config.h::Exception`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum Exception {
    UndefinedInstruction = 0,
    UnpredictableInstruction = 1,
    DecodeError = 2,
    SendEvent = 3,
    SendEventLocal = 4,
    WaitForInterrupt = 5,
    WaitForEvent = 6,
    Yield = 7,
    Breakpoint = 8,
    PreloadData = 9,
    PreloadDataWithIntentToWrite = 10,
    PreloadInstruction = 11,
    NoExecuteFault = 12,
}

impl Exception {
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

/// The 16 configurable A32 coprocessor slots from `A32::UserConfig`.
///
/// Upstream owner: `interface/A32/config.h::UserConfig::coprocessors`.
pub type Coprocessors = [Option<Arc<dyn Coprocessor>>; 16];

pub fn empty_coprocessors() -> Coprocessors {
    [const { None }; 16]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exception_values_and_layout_match_upstream() {
        let values = [
            Exception::UndefinedInstruction,
            Exception::UnpredictableInstruction,
            Exception::DecodeError,
            Exception::SendEvent,
            Exception::SendEventLocal,
            Exception::WaitForInterrupt,
            Exception::WaitForEvent,
            Exception::Yield,
            Exception::Breakpoint,
            Exception::PreloadData,
            Exception::PreloadDataWithIntentToWrite,
            Exception::PreloadInstruction,
            Exception::NoExecuteFault,
        ];
        for (expected, exception) in values.into_iter().enumerate() {
            assert_eq!(exception.as_u32(), expected as u32);
        }
        assert_eq!(std::mem::size_of::<Exception>(), 4);
        assert_eq!(std::mem::align_of::<Exception>(), 4);
    }

    #[test]
    fn empty_registry_has_all_sixteen_upstream_slots() {
        let registry = empty_coprocessors();
        assert_eq!(registry.len(), 16);
        assert!(registry.iter().all(Option::is_none));
    }
}

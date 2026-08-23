use crate::ir::a32_emitter::A32IREmitter;
use crate::jit_config::UserCallbacks;

/// Translation-time callbacks used by the A32 frontend.
///
/// Upstream owner: `frontend/A32/translate/translate_callbacks.h`.
pub trait TranslateCallbacks {
    /// Read one aligned little-endian instruction word.
    fn memory_read_code(&self, vaddr: u32) -> Option<u32>;

    /// Called before the instruction at `pc` is read.
    ///
    /// Returning `false` stops translation immediately. The callback is then
    /// responsible for setting the block terminal, matching upstream.
    fn pre_code_read_hook(&self, _is_thumb: bool, _pc: u32, _ir: &mut A32IREmitter<'_>) -> bool {
        true
    }

    /// Called after the instruction was read and before it is translated.
    fn pre_code_translation_hook(&self, _is_thumb: bool, _pc: u32, _ir: &mut A32IREmitter<'_>) {}

    /// Return the number of guest ticks charged for this instruction.
    fn get_ticks_for_code(&self, _is_thumb: bool, _vaddr: u32, _instruction: u32) -> u64 {
        1
    }
}

/// Preserve the existing standalone-frontend API for tests and tools that
/// only provide instruction memory. These are the same defaults as
/// `A32::UserCallbacks` upstream.
impl<F> TranslateCallbacks for F
where
    F: Fn(u32) -> Option<u32> + ?Sized,
{
    fn memory_read_code(&self, vaddr: u32) -> Option<u32> {
        self(vaddr)
    }
}

/// Adapts the public JIT callback interface to the smaller translation-time
/// contract, preserving the upstream `UserCallbacks : TranslateCallbacks`
/// relationship without making the frontend depend on runtime callbacks.
pub struct UserCallbacksAdapter<'a> {
    callbacks: &'a dyn UserCallbacks,
}

impl<'a> UserCallbacksAdapter<'a> {
    pub fn new(callbacks: &'a dyn UserCallbacks) -> Self {
        Self { callbacks }
    }
}

impl TranslateCallbacks for UserCallbacksAdapter<'_> {
    fn memory_read_code(&self, vaddr: u32) -> Option<u32> {
        self.callbacks.memory_read_code(vaddr as u64)
    }

    fn pre_code_read_hook(&self, is_thumb: bool, pc: u32, ir: &mut A32IREmitter<'_>) -> bool {
        self.callbacks.pre_code_read_hook(is_thumb, pc, ir)
    }

    fn pre_code_translation_hook(&self, is_thumb: bool, pc: u32, ir: &mut A32IREmitter<'_>) {
        self.callbacks.pre_code_translation_hook(is_thumb, pc, ir);
    }

    fn get_ticks_for_code(&self, is_thumb: bool, vaddr: u32, instruction: u32) -> u64 {
        self.callbacks
            .get_ticks_for_code(is_thumb, vaddr, instruction)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::TranslateCallbacks;
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::frontend::a32::translate::{translate, TranslationOptions};
    use crate::frontend::a32::types::Reg;
    use crate::ir::a32_emitter::A32IREmitter;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;

    #[derive(Debug, PartialEq, Eq)]
    enum Event {
        PreRead(bool, u32),
        Read(u32),
        PreTranslation(bool, u32),
        Ticks(bool, u32, u32),
    }

    struct RecordingCallbacks {
        word: u32,
        ticks: u64,
        events: RefCell<Vec<Event>>,
    }

    impl RecordingCallbacks {
        fn new(word: u32, ticks: u64) -> Self {
            Self {
                word,
                ticks,
                events: RefCell::new(Vec::new()),
            }
        }
    }

    impl TranslateCallbacks for RecordingCallbacks {
        fn memory_read_code(&self, vaddr: u32) -> Option<u32> {
            self.events.borrow_mut().push(Event::Read(vaddr));
            Some(self.word)
        }

        fn pre_code_read_hook(&self, is_thumb: bool, pc: u32, ir: &mut A32IREmitter<'_>) -> bool {
            self.events.borrow_mut().push(Event::PreRead(is_thumb, pc));
            ir.get_register(Reg::R1);
            true
        }

        fn pre_code_translation_hook(&self, is_thumb: bool, pc: u32, ir: &mut A32IREmitter<'_>) {
            self.events
                .borrow_mut()
                .push(Event::PreTranslation(is_thumb, pc));
            ir.get_register(Reg::R2);
        }

        fn get_ticks_for_code(&self, is_thumb: bool, vaddr: u32, instruction: u32) -> u64 {
            self.events
                .borrow_mut()
                .push(Event::Ticks(is_thumb, vaddr, instruction));
            self.ticks
        }
    }

    #[test]
    fn arm_hooks_run_in_upstream_order_and_charge_custom_ticks() {
        let callbacks = RecordingCallbacks::new(0xE1A0_0000, 7);
        let location = A32LocationDescriptor::new(0x1000, PSR::default(), FPSCR::default(), true);

        let block = translate(location, &callbacks, TranslationOptions::default());

        assert_eq!(
            *callbacks.events.borrow(),
            [
                Event::PreRead(false, 0x1000),
                Event::Read(0x1000),
                Event::PreTranslation(false, 0x1000),
                Event::Ticks(false, 0x1000, 0xE1A0_0000),
            ]
        );
        assert_eq!(block.cycle_count, 7);
        assert_eq!(block.instructions[0].opcode, Opcode::A32GetRegister);
        assert_eq!(block.instructions[1].opcode, Opcode::A32GetRegister);
    }

    #[test]
    fn thumb_hooks_receive_instruction_pc_and_combined_instruction() {
        let callbacks = RecordingCallbacks::new(0xBF00_0000, 9);
        let mut psr = PSR::default();
        psr.set_t(true);
        let location = A32LocationDescriptor::new(0x1002, psr, FPSCR::default(), true);

        let block = translate(location, &callbacks, TranslationOptions::default());

        assert_eq!(
            *callbacks.events.borrow(),
            [
                Event::PreRead(true, 0x1002),
                Event::Read(0x1000),
                Event::PreTranslation(true, 0x1002),
                Event::Ticks(true, 0x1002, 0xBF00),
            ]
        );
        assert_eq!(block.cycle_count, 9);
        assert_eq!(
            block.end_location(),
            location.advance_pc(2).advance_it().to_location()
        );
    }

    struct StopBeforeRead {
        reads: Cell<usize>,
    }

    impl TranslateCallbacks for StopBeforeRead {
        fn memory_read_code(&self, _vaddr: u32) -> Option<u32> {
            self.reads.set(self.reads.get() + 1);
            None
        }

        fn pre_code_read_hook(&self, _is_thumb: bool, _pc: u32, ir: &mut A32IREmitter<'_>) -> bool {
            ir.set_term(Terminal::ReturnToDispatch);
            false
        }
    }

    #[test]
    fn false_pre_read_hook_stops_without_read_advance_or_ticks() {
        let callbacks = StopBeforeRead {
            reads: Cell::new(0),
        };
        let location = A32LocationDescriptor::at(0x4000);

        let block = translate(location, &callbacks, TranslationOptions::default());

        assert_eq!(callbacks.reads.get(), 0);
        assert_eq!(block.cycle_count, 0);
        assert_eq!(block.end_location(), location.to_location());
        assert!(matches!(block.terminal, Terminal::ReturnToDispatch));
    }
}

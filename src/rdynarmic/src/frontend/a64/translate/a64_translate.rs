//! Port of upstream `dynarmic/frontend/A64/translate/a64_translate.{h,cpp}`.

use crate::frontend::a64::decoder::decode;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Exception;
use crate::ir::block::Block;
use crate::ir::location::A64LocationDescriptor;
use crate::ir::terminal::Terminal;

/// Callback for reading instruction memory.
pub type MemoryReadCodeFn<'a> = dyn Fn(u64) -> Option<u32> + 'a;

/// Options controlling A64 translation behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranslationOptions {
    /// Define behavior for selected constrained-unpredictable instructions.
    pub define_unpredictable_behaviour: bool,
    /// Use wall clock for CNTPCT instead of a cycle timer.
    pub wall_clock_cntpct: bool,
    /// Raise exceptions for hint instructions instead of treating them as NOPs.
    pub hook_hint_instructions: bool,
}

impl Default for TranslationOptions {
    fn default() -> Self {
        Self {
            define_unpredictable_behaviour: false,
            wall_clock_cntpct: false,
            hook_hint_instructions: true,
        }
    }
}

/// Translate a block of ARM64 instructions into IR.
pub fn translate(
    descriptor: A64LocationDescriptor,
    memory_read_code: &MemoryReadCodeFn<'_>,
    options: TranslationOptions,
) -> Block {
    let single_step = descriptor.single_stepping();

    let mut block = Block::new(descriptor.to_location());
    let mut visitor = TranslatorVisitor::new(&mut block, descriptor, options);

    let mut should_continue;
    loop {
        let pc = visitor.ir.pc();

        if let Some(instruction) = memory_read_code(pc) {
            if let Some(decoded) = decode(instruction) {
                should_continue = visitor.dispatch(&decoded);
            } else {
                should_continue = visitor.raise_exception(Exception::UnallocatedEncoding);
            }
        } else {
            should_continue = visitor.raise_exception(Exception::NoExecuteFault);
        }

        let new_location = visitor
            .ir
            .current_location
            .expect("location not set")
            .advance_pc(4);
        visitor.ir.current_location = Some(new_location);
        visitor.ir.base.block.cycle_count += 1;

        if !should_continue || single_step {
            break;
        }
    }

    let final_location = visitor.ir.current_location;
    #[allow(clippy::drop_non_drop)]
    drop(visitor);

    if single_step && should_continue {
        if let Some(location) = final_location {
            block.set_terminal(Terminal::LinkBlock {
                next: location.to_location(),
            });
        }
    }

    assert!(!block.terminal.is_invalid(), "Terminal has not been set");
    if let Some(location) = final_location {
        block.end_location = location.to_location();
    }

    block
}

/// Translate one supplied ARM64 instruction into an existing IR block.
pub fn translate_single_instruction(
    block: &mut Block,
    descriptor: A64LocationDescriptor,
    instruction: u32,
) -> bool {
    let mut visitor = TranslatorVisitor::new(block, descriptor, TranslationOptions::default());
    let should_continue = decode(instruction)
        .map(|decoded| visitor.dispatch(&decoded))
        .unwrap_or(false);

    let final_location = visitor
        .ir
        .current_location
        .expect("location not set")
        .advance_pc(4);
    visitor.ir.current_location = Some(final_location);
    visitor.ir.base.block.cycle_count += 1;
    visitor.ir.base.block.end_location = final_location.to_location();
    should_continue
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translation_option_defaults_match_upstream() {
        let options = TranslationOptions::default();
        assert!(!options.define_unpredictable_behaviour);
        assert!(!options.wall_clock_cntpct);
        assert!(options.hook_hint_instructions);
    }

    #[test]
    fn translate_single_instruction_matches_upstream_bookkeeping() {
        let descriptor = A64LocationDescriptor::new(0x1000, 0, false);
        let mut block = Block::new(descriptor.to_location());

        assert!(translate_single_instruction(
            &mut block,
            descriptor,
            0xD503_201F
        ));
        assert_eq!(block.cycle_count, 1);
        assert_eq!(block.end_location, descriptor.advance_pc(4).to_location());
    }

    #[test]
    fn translate_single_instruction_returns_false_when_decode_fails() {
        let descriptor = A64LocationDescriptor::new(0x2000, 0, false);
        let mut block = Block::new(descriptor.to_location());

        assert!(!translate_single_instruction(&mut block, descriptor, 0));
        assert_eq!(block.cycle_count, 1);
        assert_eq!(block.end_location, descriptor.advance_pc(4).to_location());
    }
}

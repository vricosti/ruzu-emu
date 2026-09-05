use crate::frontend::a32::decoder::DecodedArm;
use crate::frontend::a32::types::Exception;
use crate::ir::a32_emitter::A32IREmitter;

use super::TranslationOptions;

/// ARM WFI / WFE / YIELD hint instructions.
/// Upstream treats them as NOPs when hook_hint_instructions is false.
pub fn arm_pld(ir: &mut A32IREmitter, inst: &DecodedArm, options: TranslationOptions) -> bool {
    if !options.hook_hint_instructions {
        return true;
    }

    let is_data = ((inst.raw >> 22) & 1) != 0;
    let exception = if is_data {
        Exception::PreloadData
    } else {
        Exception::PreloadDataWithIntentToWrite
    };
    super::raise_exception(ir, exception)
}

pub fn arm_sev(ir: &mut A32IREmitter, options: TranslationOptions) -> bool {
    hint_exception(ir, options, Exception::SendEvent)
}

pub fn arm_sevl(ir: &mut A32IREmitter, options: TranslationOptions) -> bool {
    hint_exception(ir, options, Exception::SendEventLocal)
}

pub fn arm_wfi(ir: &mut A32IREmitter, options: TranslationOptions) -> bool {
    hint_exception(ir, options, Exception::WaitForInterrupt)
}

pub fn arm_wfe(ir: &mut A32IREmitter, options: TranslationOptions) -> bool {
    hint_exception(ir, options, Exception::WaitForEvent)
}

pub fn arm_yield(ir: &mut A32IREmitter, options: TranslationOptions) -> bool {
    hint_exception(ir, options, Exception::Yield)
}

fn hint_exception(
    ir: &mut A32IREmitter,
    options: TranslationOptions,
    exception: Exception,
) -> bool {
    if !options.hook_hint_instructions {
        return true;
    }
    super::raise_exception(ir, exception)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder::ArmInstId;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::value::Value;

    fn emitted_exception(block: &Block) -> Option<u64> {
        let value = block
            .instructions
            .iter()
            .find(|instruction| instruction.opcode == Opcode::A32ExceptionRaised)?
            .args[1];
        let Value::ImmU64(value) = value else {
            return None;
        };
        Some(value)
    }

    #[test]
    fn hint_hook_option_selects_nop_or_exception() {
        let location = A32LocationDescriptor::at(0x1000);

        let mut disabled = Block::new(location.to_location());
        assert!(arm_wfi(
            &mut A32IREmitter::with_location(&mut disabled, location),
            TranslationOptions {
                hook_hint_instructions: false,
                ..TranslationOptions::default()
            },
        ));
        assert!(disabled.instructions.is_empty());

        let mut enabled = Block::new(location.to_location());
        assert!(!arm_wfi(
            &mut A32IREmitter::with_location(&mut enabled, location),
            TranslationOptions::default(),
        ));
        assert_eq!(
            emitted_exception(&enabled),
            Some(Exception::WaitForInterrupt.as_u32() as u64)
        );
    }

    #[test]
    fn sevl_raises_send_event_local_when_hooked() {
        let location = A32LocationDescriptor::at(0x1800);
        let mut block = Block::new(location.to_location());
        assert!(!arm_sevl(
            &mut A32IREmitter::with_location(&mut block, location),
            TranslationOptions::default(),
        ));
        assert_eq!(
            emitted_exception(&block),
            Some(Exception::SendEventLocal.as_u32() as u64)
        );
    }

    #[test]
    fn pld_r_bit_selects_upstream_exception() {
        let location = A32LocationDescriptor::at(0x2000);
        for (raw, expected) in [
            (0xF550_F000, Exception::PreloadData),
            (0xF510_F000, Exception::PreloadDataWithIntentToWrite),
        ] {
            let mut block = Block::new(location.to_location());
            let decoded = DecodedArm {
                raw,
                id: ArmInstId::PldImm,
            };
            assert!(!arm_pld(
                &mut A32IREmitter::with_location(&mut block, location),
                &decoded,
                TranslationOptions::default(),
            ));
            assert_eq!(emitted_exception(&block), Some(expected.as_u32() as u64));
        }
    }
}

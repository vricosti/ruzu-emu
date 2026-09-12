//! ARM64 A64 memory emission wrappers.
//!
//! Upstream owner: `backend/arm64/emit_arm64_a64_memory.cpp`.

use rhazel::CodeGenerator;

use crate::backend::arm64::abi::regs;
use crate::backend::arm64::block_of_code::BlockOfCode;
use crate::backend::arm64::emit_arm64_memory::{
    emit_exclusive_read_memory, emit_exclusive_write_memory, emit_read_memory, emit_write_memory,
};
use crate::backend::arm64::emit_context::EmitContext;
#[cfg(test)]
use crate::backend::arm64::inst;
use crate::backend::arm64::jit_state::A64JitState;
use crate::ir::value::InstRef;

#[cfg(test)]
const WZR: u8 = 31;
#[cfg(test)]
use crate::backend::arm64::abi::XSTATE;

pub fn emit_a64_clear_exclusive(code: &mut BlockOfCode) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    code.str(
        rhazel::WZR,
        regs::XSTATE,
        core::mem::offset_of!(A64JitState, exclusive_state) as u32,
    )
}

pub fn emit_a64_read_memory<const BITSIZE: usize>(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_read_memory::<BITSIZE>(code, ctx, inst_ref)
}

pub fn emit_a64_exclusive_read_memory<const BITSIZE: usize>(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_exclusive_read_memory::<BITSIZE>(code, ctx, inst_ref)
}

pub fn emit_a64_write_memory<const BITSIZE: usize>(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_write_memory::<BITSIZE>(code, ctx, inst_ref)
}

pub fn emit_a64_exclusive_write_memory<const BITSIZE: usize>(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_exclusive_write_memory::<BITSIZE>(code, ctx, inst_ref)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_instruction(code: &BlockOfCode, offset: usize) -> u32 {
        unsafe {
            code.code_base_ptr()
                .add(offset)
                .cast::<u32>()
                .read_unaligned()
        }
    }

    #[test]
    fn clear_exclusive_stores_wzr_to_a64_exclusive_state() {
        let mut code = BlockOfCode::with_size(4096).unwrap();

        emit_a64_clear_exclusive(&mut code).unwrap();

        assert_eq!(
            read_instruction(&code, 0),
            inst::str_w_unsigned(
                WZR,
                XSTATE,
                core::mem::offset_of!(A64JitState, exclusive_state) as u32
            )
        );
    }
}

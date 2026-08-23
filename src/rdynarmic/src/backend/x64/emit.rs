use crate::backend::x64::a32_emit_a32 as a32;
use crate::backend::x64::a64_emit_x64_memory;
use crate::backend::x64::emit_a64;
use crate::backend::x64::emit_aes;
use crate::backend::x64::emit_context::{BlockDescriptor, EmitContext};
use crate::backend::x64::emit_crc32;
use crate::backend::x64::emit_data_processing as dp;
use crate::backend::x64::emit_exclusive_memory as excl_mem;
use crate::backend::x64::emit_floating_point as fp;
use crate::backend::x64::emit_fp_vector as fpv;
use crate::backend::x64::emit_fp_vector_convert as fpvc;
use crate::backend::x64::emit_memory;
use crate::backend::x64::emit_packed as packed;
use crate::backend::x64::emit_saturation as sat;
use crate::backend::x64::emit_sha;
use crate::backend::x64::emit_sm4;
use crate::backend::x64::emit_terminal;
use crate::backend::x64::emit_vector_arrangement as varr;
use crate::backend::x64::emit_vector_basic as vbasic;
use crate::backend::x64::emit_vector_compare as vcmp;
use crate::backend::x64::emit_vector_misc as vmisc;
use crate::backend::x64::emit_vector_multiply as vmul;
use crate::backend::x64::emit_vector_saturated as vsat;
use crate::backend::x64::emit_vector_shift as vshift;
use crate::backend::x64::hostloc::HOST_RCX;
use crate::backend::x64::jit_state::{A32JitState, A64JitState};
use crate::backend::x64::patch_info::{PatchEntry, PatchType};
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::ir::block::Block;
use crate::ir::location::LocationDescriptor;
use crate::ir::opcode::Opcode;
use crate::ir::value::InstRef;
use rxbyak::RegExp;
use rxbyak::{
    dword_ptr, qword_ptr, xmmword_ptr, Reg, R10, R11, R15, R8, R9, RAX, RCX, RDI, RDX, RSI, RSP,
};

/// Cache the `RDYNARMIC_PROFILE_OPCODES` environment-variable lookup behind a
/// `OnceLock<bool>`. Only present when the `profile_opcodes` Cargo feature is
/// enabled — release builds without that feature carry no profiling code in
/// the inner emit loop at all.
#[cfg(feature = "profile_opcodes")]
fn profile_opcodes_enabled() -> bool {
    use std::sync::OnceLock;
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| std::env::var_os("RDYNARMIC_PROFILE_OPCODES").is_some())
}

fn rsb_offsets(ctx: &EmitContext) -> (usize, usize, usize) {
    if ctx.arch.is_a32() {
        (
            A32JitState::offset_of_rsb_ptr(),
            A32JitState::offset_of_rsb_location_descriptors(),
            A32JitState::offset_of_rsb_codeptrs(),
        )
    } else {
        (
            A64JitState::offset_of_rsb_ptr(),
            A64JitState::offset_of_rsb_location_descriptors(),
            A64JitState::offset_of_rsb_codeptrs(),
        )
    }
}

fn emit_patch_mov_rcx(ctx: &EmitContext, target_loc: LocationDescriptor) -> u64 {
    let fallback_code_ptr = ctx
        .dispatcher_offsets
        .map(|offsets| ctx.code_base_ptr as usize + offsets[0])
        .unwrap_or(0);

    ctx.block_lookup
        .as_ref()
        .and_then(|lookup| lookup(target_loc))
        .map_or(fallback_code_ptr as u64, |ptr| ptr as u64)
}

pub fn emit_push_rsb_location(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    target_loc: LocationDescriptor,
) {
    let (rsb_ptr_offset, rsb_loc_offset, rsb_code_offset) = rsb_offsets(ctx);

    let _rcx = ra.scratch_gpr_at(HOST_RCX);
    let loc_desc_reg = ra.scratch_gpr();
    let index_reg = ra.scratch_gpr();

    ra.asm
        .mov(
            index_reg.cvt32().unwrap(),
            dword_ptr(RegExp::from(R15) + rsb_ptr_offset as i32),
        )
        .unwrap();
    ra.asm.mov(loc_desc_reg, target_loc.value() as i64).unwrap();

    let patch_offset = ra.asm.size();
    let target_code_ptr = emit_patch_mov_rcx(ctx, target_loc);
    ra.asm.mov(rxbyak::RCX, target_code_ptr as i64).unwrap();
    ctx.patch_entries.borrow_mut().push(PatchEntry {
        target: target_loc,
        patch_type: PatchType::MovRcx,
        code_offset: patch_offset,
    });

    ra.asm
        .mov(
            qword_ptr(RegExp::from(R15) + index_reg * 8u8 + rsb_loc_offset as i32),
            loc_desc_reg,
        )
        .unwrap();
    ra.asm
        .mov(
            qword_ptr(RegExp::from(R15) + index_reg * 8u8 + rsb_code_offset as i32),
            rxbyak::RCX,
        )
        .unwrap();

    ra.asm.add(index_reg.cvt32().unwrap(), 1).unwrap();
    ra.asm
        .and_(
            index_reg.cvt32().unwrap(),
            crate::backend::x64::jit_state::RSB_PTR_MASK as i32,
        )
        .unwrap();
    ra.asm
        .mov(
            dword_ptr(RegExp::from(R15) + rsb_ptr_offset as i32),
            index_reg.cvt32().unwrap(),
        )
        .unwrap();
}

/// Shared x64 PushRSB implementation.
///
/// Traceability:
/// - upstream owner: `backend/x64/emit_x64.cpp`
/// - upstream methods: `EmitX64::EmitPushRSB`, `EmitX64::PushRSBHelper`
pub fn emit_push_rsb(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &crate::ir::inst::Inst,
) {
    let target_loc = LocationDescriptor::new(inst.args[0].get_imm_as_u64());
    emit_push_rsb_location(ctx, ra, target_loc);
}

/// Emit native x86-64 code for an IR block.
///
/// Walks all live instructions, dispatches each opcode to the appropriate
/// emitter function, then emits the block terminal (control flow).
///
/// Returns a `BlockDescriptor` with the entrypoint offset and size.
pub fn emit_block(ctx: &EmitContext, ra: &mut RegAlloc, block: &Block) -> BlockDescriptor {
    let start = ra.asm.size();

    // RUZU_BLOCK_PROLOGUE_COUNT_PC — inline per-core block-entry counter.
    // Emitted INSIDE emit_block (after `start` captures the entrypoint
    // offset) so the increment runs every time the block is entered,
    // including via FAST_DISPATCH chained jumps. The counter address is
    // stashed by the outer compile path through `ctx.prologue_counter_addr`.
    if let Some(counter_addr) = ctx.prologue_counter_addr.get() {
        let _ = ra.asm.push(rxbyak::RAX);
        let _ = ra.asm.mov(rxbyak::RAX, counter_addr as i64);
        let _ = ra.asm.lock();
        let _ = ra
            .asm
            .inc(rxbyak::qword_ptr(rxbyak::RegExp::from(rxbyak::RAX)));
        let _ = ra.asm.pop(rxbyak::RAX);
    }

    let trace_emit_at_pc = std::env::var("RUZU_TRACE_A64_EMIT_PC").ok().and_then(|s| {
        let s = s.trim_start_matches("0x");
        u64::from_str_radix(s, 16).ok()
    });

    if !ctx.arch.is_a32() && a64_block_entry_trace_hook_enabled(ctx) {
        emit_preserved_a64_block_entry_trace_hook(ra);
    }
    if !ctx.arch.is_a32() {
        emit_a64_bad_xreg_trap(ctx, ra);
    }

    // RUZU_BLOCK_ENTRY_MARKER=1 — at the very start of every A64 block,
    // emit `pcmpeqb xmm14, xmm14` (xmm14 = all-FFs). This marks "we
    // entered this block at offset 0". If the W128 callback observes
    // xmm14 != all-FFs, EITHER the block was entered MID-WAY (skipping
    // the marker), OR something between the marker and the callback
    // modified xmm14.
    if std::env::var("RUZU_BLOCK_ENTRY_MARKER").is_ok() && !ctx.arch.is_a32() {
        // pcmpeqb xmm14, xmm14: 66 45 0F 74 F6 (REX.RB to make BOTH
        // operands xmm14 instead of xmm6).
        ra.asm.db(0x66).unwrap();
        ra.asm.db(0x45).unwrap();
        ra.asm.db(0x0F).unwrap();
        ra.asm.db(0x74).unwrap();
        ra.asm.db(0xF6).unwrap();
    }

    // Emit condition prelude for A32 conditional blocks.
    // Matches upstream dynarmic `A32EmitX64::EmitCondPrelude()`.
    if ctx.arch.is_a32() {
        a32::emit_cond_prelude(ctx, ra, block);
    }

    // RUZU_A32_PC_TRACE=0xPC — low-overhead per-PC GPR-capture hook. Emitted
    // ONLY for the block whose entry PC matches the target, so there is zero
    // per-read / per-instruction cost elsewhere. The hook reads the A32 GPRs
    // and aggregates (buffered) — never per-hit I/O. Used to capture the
    if ctx.arch.is_a32() {
        if let Some(target_pc) = crate::jit::a32_pc_trace_target() {
            let blk_pc = ctx.arch.extract_pc(ctx.location);
            // One-time diagnostic: report block PCs within ±0x80 of the target
            // so we can see the actual block-entry encoding vs the requested PC.
            if blk_pc.wrapping_sub(target_pc) <= 0x80 || target_pc.wrapping_sub(blk_pc) <= 0x80 {
                static SEEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 20 {
                    log::warn!(
                        "[A32_PC_TRACE_EMIT] block_pc=0x{:X} target=0x{:X}",
                        blk_pc,
                        target_pc
                    );
                }
            }
            if blk_pc == target_pc {
                emit_preserved_a32_pc_trace_hook(ra, u64::MAX);
            }
        }
    }

    // Per-IR-opcode emit-time accounting. Compiled in only with the
    // `profile_opcodes` Cargo feature; release builds carry no profiling
    // code in the inner loop. Resolved once per emit_block via OnceLock
    // (see profile_opcodes_enabled).
    #[cfg(feature = "profile_opcodes")]
    let _profile_op = profile_opcodes_enabled();

    // Emit each instruction
    let mut bcast64_zero_seen = false;
    for (i, inst) in block.instructions.iter().enumerate() {
        if inst.is_tombstone() {
            continue;
        }
        let inst_ref = InstRef(i as u32);
        if matches!(inst.opcode, Opcode::VectorBroadcast64)
            && matches!(inst.args[0], crate::ir::Value::ImmU64(0))
        {
            bcast64_zero_seen = true;
        }
        if trace_emit_at_pc.is_some_and(|pc| ctx.arch.extract_pc(ctx.location) == pc) {
            eprintln!(
                "[TRACE_A64_EMIT_PC] pc=0x{:016X} inst#{} opcode={:?}",
                ctx.arch.extract_pc(ctx.location),
                i,
                inst.opcode
            );
        }
        ra.set_current_inst_for_diagnostics(
            ctx.arch.extract_pc(ctx.location),
            inst_ref,
            inst.opcode,
        );

        #[cfg(feature = "profile_opcodes")]
        let _t_op_start = if _profile_op {
            Some(std::time::Instant::now())
        } else {
            None
        };
        #[cfg(feature = "profile_opcodes")]
        let _op_for_log = inst.opcode;

        match inst.opcode {
            // --- Core ---
            Opcode::Void => emit_a64::emit_void(ctx, ra, inst_ref, inst),
            Opcode::Identity => emit_a64::emit_identity(ctx, ra, inst_ref, inst),
            Opcode::Breakpoint => emit_a64::emit_breakpoint(ctx, ra, inst_ref, inst),

            // --- A64 context getters/setters ---
            Opcode::A64SetCheckBit => emit_a64::emit_a64_set_check_bit(ctx, ra, inst_ref, inst),
            Opcode::A64GetCFlag => emit_a64::emit_a64_get_c_flag(ctx, ra, inst_ref, inst),
            Opcode::A64GetNZCVRaw => emit_a64::emit_a64_get_nzcv_raw(ctx, ra, inst_ref, inst),
            Opcode::A64SetNZCVRaw => emit_a64::emit_a64_set_nzcv_raw(ctx, ra, inst_ref, inst),
            Opcode::A64SetNZCV => emit_a64::emit_a64_set_nzcv(ctx, ra, inst_ref, inst),
            Opcode::A64GetW => emit_a64::emit_a64_get_w(ctx, ra, inst_ref, inst),
            Opcode::A64GetX => emit_a64::emit_a64_get_x(ctx, ra, inst_ref, inst),
            Opcode::A64GetS => emit_a64::emit_a64_get_s(ctx, ra, inst_ref, inst),
            Opcode::A64GetD => emit_a64::emit_a64_get_d(ctx, ra, inst_ref, inst),
            Opcode::A64GetQ => emit_a64::emit_a64_get_q(ctx, ra, inst_ref, inst),
            Opcode::A64GetSP => emit_a64::emit_a64_get_sp(ctx, ra, inst_ref, inst),
            Opcode::A64GetFPCR => emit_a64::emit_a64_get_fpcr(ctx, ra, inst_ref, inst),
            Opcode::A64GetFPSR => emit_a64::emit_a64_get_fpsr(ctx, ra, inst_ref, inst),
            Opcode::A64SetW => emit_a64::emit_a64_set_w(ctx, ra, inst_ref, inst),
            Opcode::A64SetX => emit_a64::emit_a64_set_x(ctx, ra, inst_ref, inst),
            Opcode::A64SetS => emit_a64::emit_a64_set_s(ctx, ra, inst_ref, inst),
            Opcode::A64SetD => emit_a64::emit_a64_set_d(ctx, ra, inst_ref, inst),
            Opcode::A64SetQ => emit_a64::emit_a64_set_q(ctx, ra, inst_ref, inst),
            Opcode::A64SetSP => emit_a64::emit_a64_set_sp(ctx, ra, inst_ref, inst),
            Opcode::A64SetPC => emit_a64::emit_a64_set_pc(ctx, ra, inst_ref, inst),
            Opcode::A64SetFPCR => emit_a64::emit_a64_set_fpcr(ctx, ra, inst_ref, inst),
            Opcode::A64SetFPSR => emit_a64::emit_a64_set_fpsr(ctx, ra, inst_ref, inst),
            Opcode::A64CallSupervisor => {
                emit_a64::emit_a64_call_supervisor(ctx, ra, inst_ref, inst)
            }
            Opcode::A64ExceptionRaised => {
                emit_a64::emit_a64_exception_raised(ctx, ra, inst_ref, inst)
            }
            Opcode::A64DataCacheOperationRaised => {
                emit_a64::emit_a64_data_cache_operation_raised(ctx, ra, inst_ref, inst)
            }
            Opcode::A64InstructionCacheOperationRaised => {
                emit_a64::emit_a64_instruction_cache_operation_raised(ctx, ra, inst_ref, inst)
            }
            Opcode::A64DataSynchronizationBarrier => {
                emit_a64::emit_a64_dsb(ctx, ra, inst_ref, inst)
            }
            Opcode::A64DataMemoryBarrier => emit_a64::emit_a64_dmb(ctx, ra, inst_ref, inst),
            Opcode::A64InstructionSynchronizationBarrier => {
                emit_a64::emit_a64_isb(ctx, ra, inst_ref, inst)
            }
            Opcode::A64GetCNTFRQ => emit_a64::emit_a64_get_cntfrq(ctx, ra, inst_ref, inst),
            Opcode::A64GetCNTPCT => emit_a64::emit_a64_get_cntpct(ctx, ra, inst_ref, inst),
            Opcode::A64GetCTR => emit_a64::emit_a64_get_ctr(ctx, ra, inst_ref, inst),
            Opcode::A64GetDCZID => emit_a64::emit_a64_get_dczid(ctx, ra, inst_ref, inst),
            Opcode::A64GetTPIDR => emit_a64::emit_a64_get_tpidr(ctx, ra, inst_ref, inst),
            Opcode::A64SetTPIDR => emit_a64::emit_a64_set_tpidr(ctx, ra, inst_ref, inst),
            Opcode::A64GetTPIDRRO => emit_a64::emit_a64_get_tpidrro(ctx, ra, inst_ref, inst),

            // --- RSB ---
            Opcode::PushRSB => emit_push_rsb(ctx, ra, inst_ref, inst),

            // --- Flags / pseudo-ops ---
            Opcode::GetCarryFromOp => emit_a64::emit_get_carry_from_op(ctx, ra, inst_ref, inst),
            Opcode::GetOverflowFromOp => {
                emit_a64::emit_get_overflow_from_op(ctx, ra, inst_ref, inst)
            }
            Opcode::GetNZCVFromOp => emit_a64::emit_get_nzcv_from_op(ctx, ra, inst_ref, inst),
            Opcode::GetNZFromOp => emit_a64::emit_get_nz_from_op(ctx, ra, inst_ref, inst),
            Opcode::GetUpperFromOp => emit_a64::emit_get_upper_from_op(ctx, ra, inst_ref, inst),
            Opcode::GetLowerFromOp => emit_a64::emit_get_lower_from_op(ctx, ra, inst_ref, inst),
            Opcode::GetCFlagFromNZCV => {
                emit_a64::emit_get_c_flag_from_nzcv(ctx, ra, inst_ref, inst)
            }
            Opcode::NZCVFromPackedFlags => {
                emit_a64::emit_nzcv_from_packed_flags(ctx, ra, inst_ref, inst)
            }

            // --- ALU: packing/extraction ---
            Opcode::Pack2x32To1x64 => dp::emit_pack_2x32_to_1x64(ctx, ra, inst_ref, inst),
            Opcode::Pack2x64To1x128 => fp::emit_pack_2x64_to_1x128(ctx, ra, inst_ref, inst),
            Opcode::LeastSignificantWord => {
                dp::emit_least_significant_word(ctx, ra, inst_ref, inst)
            }
            Opcode::MostSignificantWord => dp::emit_most_significant_word(ctx, ra, inst_ref, inst),
            Opcode::LeastSignificantHalf => {
                dp::emit_least_significant_half(ctx, ra, inst_ref, inst)
            }
            Opcode::LeastSignificantByte => {
                dp::emit_least_significant_byte(ctx, ra, inst_ref, inst)
            }
            Opcode::MostSignificantBit => dp::emit_most_significant_bit(ctx, ra, inst_ref, inst),

            // --- ALU: test/compare ---
            Opcode::IsZero32 => dp::emit_is_zero32(ctx, ra, inst_ref, inst),
            Opcode::IsZero64 => dp::emit_is_zero64(ctx, ra, inst_ref, inst),
            Opcode::TestBit => dp::emit_test_bit(ctx, ra, inst_ref, inst),

            // --- ALU: conditional select ---
            Opcode::ConditionalSelect32 => dp::emit_conditional_select32(ctx, ra, inst_ref, inst),
            Opcode::ConditionalSelect64 => dp::emit_conditional_select64(ctx, ra, inst_ref, inst),
            Opcode::ConditionalSelectNZCV => {
                dp::emit_conditional_select_nzcv(ctx, ra, inst_ref, inst)
            }

            // --- ALU: shifts (dynamic) ---
            Opcode::LogicalShiftLeft32 => dp::emit_logical_shift_left32(ctx, ra, inst_ref, inst),
            Opcode::LogicalShiftLeft64 => dp::emit_logical_shift_left64(ctx, ra, inst_ref, inst),
            Opcode::LogicalShiftRight32 => dp::emit_logical_shift_right32(ctx, ra, inst_ref, inst),
            Opcode::LogicalShiftRight64 => dp::emit_logical_shift_right64(ctx, ra, inst_ref, inst),
            Opcode::ArithmeticShiftRight32 => {
                dp::emit_arithmetic_shift_right32(ctx, ra, inst_ref, inst)
            }
            Opcode::ArithmeticShiftRight64 => {
                dp::emit_arithmetic_shift_right64(ctx, ra, inst_ref, inst)
            }
            Opcode::BitRotateRight32 => dp::emit_rotate_right32(ctx, ra, inst_ref, inst),
            Opcode::BitRotateRight64 => dp::emit_rotate_right64(ctx, ra, inst_ref, inst),
            Opcode::RotateRightExtended => dp::emit_rotate_right_extended(ctx, ra, inst_ref, inst),

            // --- ALU: shifts (masked, no clamping) ---
            Opcode::LogicalShiftLeftMasked32 => {
                dp::emit_logical_shift_left_masked32(ctx, ra, inst_ref, inst)
            }
            Opcode::LogicalShiftLeftMasked64 => {
                dp::emit_logical_shift_left_masked64(ctx, ra, inst_ref, inst)
            }
            Opcode::LogicalShiftRightMasked32 => {
                dp::emit_logical_shift_right_masked32(ctx, ra, inst_ref, inst)
            }
            Opcode::LogicalShiftRightMasked64 => {
                dp::emit_logical_shift_right_masked64(ctx, ra, inst_ref, inst)
            }
            Opcode::ArithmeticShiftRightMasked32 => {
                dp::emit_arithmetic_shift_right_masked32(ctx, ra, inst_ref, inst)
            }
            Opcode::ArithmeticShiftRightMasked64 => {
                dp::emit_arithmetic_shift_right_masked64(ctx, ra, inst_ref, inst)
            }
            Opcode::RotateRightMasked32 => dp::emit_rotate_right_masked32(ctx, ra, inst_ref, inst),
            Opcode::RotateRightMasked64 => dp::emit_rotate_right_masked64(ctx, ra, inst_ref, inst),

            // --- ALU: arithmetic ---
            Opcode::Add32 => dp::emit_add32(ctx, ra, inst_ref, inst),
            Opcode::Add64 => dp::emit_add64(ctx, ra, inst_ref, inst),
            Opcode::Sub32 => dp::emit_sub32(ctx, ra, inst_ref, inst),
            Opcode::Sub64 => dp::emit_sub64(ctx, ra, inst_ref, inst),
            Opcode::Mul32 => dp::emit_mul32(ctx, ra, inst_ref, inst),
            Opcode::Mul64 => dp::emit_mul64(ctx, ra, inst_ref, inst),
            Opcode::SignedMultiplyHigh64 => {
                dp::emit_signed_multiply_high_64(ctx, ra, inst_ref, inst)
            }
            Opcode::UnsignedMultiplyHigh64 => {
                dp::emit_unsigned_multiply_high_64(ctx, ra, inst_ref, inst)
            }
            Opcode::UnsignedDiv32 => dp::emit_unsigned_div32(ctx, ra, inst_ref, inst),
            Opcode::UnsignedDiv64 => dp::emit_unsigned_div64(ctx, ra, inst_ref, inst),
            Opcode::SignedDiv32 => dp::emit_signed_div32(ctx, ra, inst_ref, inst),
            Opcode::SignedDiv64 => dp::emit_signed_div64(ctx, ra, inst_ref, inst),

            // --- ALU: logical ---
            Opcode::And32 => dp::emit_and32(ctx, ra, inst_ref, inst),
            Opcode::And64 => dp::emit_and64(ctx, ra, inst_ref, inst),
            Opcode::AndNot32 => dp::emit_and_not32(ctx, ra, inst_ref, inst),
            Opcode::AndNot64 => dp::emit_and_not64(ctx, ra, inst_ref, inst),
            Opcode::Eor32 => dp::emit_eor32(ctx, ra, inst_ref, inst),
            Opcode::Eor64 => dp::emit_eor64(ctx, ra, inst_ref, inst),
            Opcode::Or32 => dp::emit_or32(ctx, ra, inst_ref, inst),
            Opcode::Or64 => dp::emit_or64(ctx, ra, inst_ref, inst),
            Opcode::Not32 => dp::emit_not32(ctx, ra, inst_ref, inst),
            Opcode::Not64 => dp::emit_not64(ctx, ra, inst_ref, inst),

            // --- ALU: extensions ---
            Opcode::SignExtendByteToWord => {
                dp::emit_sign_extend_byte_to_word(ctx, ra, inst_ref, inst)
            }
            Opcode::SignExtendHalfToWord => {
                dp::emit_sign_extend_half_to_word(ctx, ra, inst_ref, inst)
            }
            Opcode::SignExtendByteToLong => {
                dp::emit_sign_extend_byte_to_long(ctx, ra, inst_ref, inst)
            }
            Opcode::SignExtendHalfToLong => {
                dp::emit_sign_extend_half_to_long(ctx, ra, inst_ref, inst)
            }
            Opcode::SignExtendWordToLong => {
                dp::emit_sign_extend_word_to_long(ctx, ra, inst_ref, inst)
            }
            Opcode::ZeroExtendByteToWord => {
                dp::emit_zero_extend_byte_to_word(ctx, ra, inst_ref, inst)
            }
            Opcode::ZeroExtendHalfToWord => {
                dp::emit_zero_extend_half_to_word(ctx, ra, inst_ref, inst)
            }
            Opcode::ZeroExtendByteToLong => {
                dp::emit_zero_extend_byte_to_long(ctx, ra, inst_ref, inst)
            }
            Opcode::ZeroExtendHalfToLong => {
                dp::emit_zero_extend_half_to_long(ctx, ra, inst_ref, inst)
            }
            Opcode::ZeroExtendWordToLong => {
                dp::emit_zero_extend_word_to_long(ctx, ra, inst_ref, inst)
            }
            Opcode::ZeroExtendLongToQuad => {
                dp::emit_zero_extend_long_to_quad(ctx, ra, inst_ref, inst)
            }

            // --- ALU: byte reverse ---
            Opcode::ByteReverseWord => dp::emit_byte_reverse_word(ctx, ra, inst_ref, inst),
            Opcode::ByteReverseHalf => dp::emit_byte_reverse_half(ctx, ra, inst_ref, inst),
            Opcode::ByteReverseDual => dp::emit_byte_reverse_dual(ctx, ra, inst_ref, inst),

            // --- ALU: bit counting ---
            Opcode::CountLeadingZeros32 => dp::emit_count_leading_zeros32(ctx, ra, inst_ref, inst),
            Opcode::CountLeadingZeros64 => dp::emit_count_leading_zeros64(ctx, ra, inst_ref, inst),

            // --- ALU: extract/replicate ---
            Opcode::ExtractRegister32 => dp::emit_extract_register32(ctx, ra, inst_ref, inst),
            Opcode::ExtractRegister64 => dp::emit_extract_register64(ctx, ra, inst_ref, inst),
            Opcode::ReplicateBit32 => dp::emit_replicate_bit32(ctx, ra, inst_ref, inst),
            Opcode::ReplicateBit64 => dp::emit_replicate_bit64(ctx, ra, inst_ref, inst),

            // --- Saturated: max/min ---
            Opcode::MaxSigned32 => dp::emit_max_signed32(ctx, ra, inst_ref, inst),
            Opcode::MaxSigned64 => dp::emit_max_signed64(ctx, ra, inst_ref, inst),
            Opcode::MaxUnsigned32 => dp::emit_max_unsigned32(ctx, ra, inst_ref, inst),
            Opcode::MaxUnsigned64 => dp::emit_max_unsigned64(ctx, ra, inst_ref, inst),
            Opcode::MinSigned32 => dp::emit_min_signed32(ctx, ra, inst_ref, inst),
            Opcode::MinSigned64 => dp::emit_min_signed64(ctx, ra, inst_ref, inst),
            Opcode::MinUnsigned32 => dp::emit_min_unsigned32(ctx, ra, inst_ref, inst),
            Opcode::MinUnsigned64 => dp::emit_min_unsigned64(ctx, ra, inst_ref, inst),

            // --- Memory access ---
            // 8/16/32/64-bit reads + writes use the upstream-faithful
            // dispatcher in `a64_emit_x64_memory.rs` which selects the
            // fastmem / page-table / callback path based on
            // `ctx.fastmem_available` and `ctx.config.memory.page_table_present`.
            // 128-bit reads/writes stay on the existing callback path
            // in `emit_memory.rs` per decision 2 (deferred fastmem path
            // requires `cmpxchg16b` and 128-bit ABI shims).
            Opcode::A64ReadMemory8 => {
                a64_emit_x64_memory::emit_a64_read_memory_8(ctx, ra, inst_ref, inst)
            }
            Opcode::A64ReadMemory16 => {
                a64_emit_x64_memory::emit_a64_read_memory_16(ctx, ra, inst_ref, inst)
            }
            Opcode::A64ReadMemory32 => {
                a64_emit_x64_memory::emit_a64_read_memory_32(ctx, ra, inst_ref, inst)
            }
            Opcode::A64ReadMemory64 => {
                a64_emit_x64_memory::emit_a64_read_memory_64(ctx, ra, inst_ref, inst)
            }
            Opcode::A64ReadMemory128 => {
                emit_memory::emit_a64_read_memory_128(ctx, ra, inst_ref, inst)
            }
            Opcode::A64WriteMemory8 => {
                a64_emit_x64_memory::emit_a64_write_memory_8(ctx, ra, inst_ref, inst)
            }
            Opcode::A64WriteMemory16 => {
                a64_emit_x64_memory::emit_a64_write_memory_16(ctx, ra, inst_ref, inst)
            }
            Opcode::A64WriteMemory32 => {
                a64_emit_x64_memory::emit_a64_write_memory_32(ctx, ra, inst_ref, inst)
            }
            Opcode::A64WriteMemory64 => {
                a64_emit_x64_memory::emit_a64_write_memory_64(ctx, ra, inst_ref, inst)
            }
            Opcode::A64WriteMemory128 => {
                emit_memory::emit_a64_write_memory_128(ctx, ra, inst_ref, inst)
            }

            // --- Exclusive memory access ---
            Opcode::A64ClearExclusive => {
                excl_mem::emit_a64_clear_exclusive(ctx, ra, inst_ref, inst)
            }
            Opcode::A64ExclusiveReadMemory8 => {
                excl_mem::emit_a64_exclusive_read_memory_8(ctx, ra, inst_ref, inst)
            }
            Opcode::A64ExclusiveReadMemory16 => {
                excl_mem::emit_a64_exclusive_read_memory_16(ctx, ra, inst_ref, inst)
            }
            Opcode::A64ExclusiveReadMemory32 => {
                excl_mem::emit_a64_exclusive_read_memory_32(ctx, ra, inst_ref, inst)
            }
            Opcode::A64ExclusiveReadMemory64 => {
                excl_mem::emit_a64_exclusive_read_memory_64(ctx, ra, inst_ref, inst)
            }
            Opcode::A64ExclusiveReadMemory128 => {
                excl_mem::emit_a64_exclusive_read_memory_128(ctx, ra, inst_ref, inst)
            }
            Opcode::A64ExclusiveWriteMemory8 => {
                excl_mem::emit_a64_exclusive_write_memory_8(ctx, ra, inst_ref, inst)
            }
            Opcode::A64ExclusiveWriteMemory16 => {
                excl_mem::emit_a64_exclusive_write_memory_16(ctx, ra, inst_ref, inst)
            }
            Opcode::A64ExclusiveWriteMemory32 => {
                excl_mem::emit_a64_exclusive_write_memory_32(ctx, ra, inst_ref, inst)
            }
            Opcode::A64ExclusiveWriteMemory64 => {
                excl_mem::emit_a64_exclusive_write_memory_64(ctx, ra, inst_ref, inst)
            }
            Opcode::A64ExclusiveWriteMemory128 => {
                excl_mem::emit_a64_exclusive_write_memory_128(ctx, ra, inst_ref, inst)
            }

            // --- Saturated arithmetic ---
            Opcode::SignedSaturatedAdd8 => sat::emit_signed_saturated_add8(ctx, ra, inst_ref, inst),
            Opcode::SignedSaturatedAddWithFlag32 => {
                sat::emit_signed_saturated_add_with_flag32(ctx, ra, inst_ref, inst)
            }
            Opcode::SignedSaturatedAdd16 => {
                sat::emit_signed_saturated_add16(ctx, ra, inst_ref, inst)
            }
            Opcode::SignedSaturatedAdd32 => {
                sat::emit_signed_saturated_add32(ctx, ra, inst_ref, inst)
            }
            Opcode::SignedSaturatedAdd64 => {
                sat::emit_signed_saturated_add64(ctx, ra, inst_ref, inst)
            }
            Opcode::SignedSaturatedSub8 => sat::emit_signed_saturated_sub8(ctx, ra, inst_ref, inst),
            Opcode::SignedSaturatedSubWithFlag32 => {
                sat::emit_signed_saturated_sub_with_flag32(ctx, ra, inst_ref, inst)
            }
            Opcode::SignedSaturatedSub16 => {
                sat::emit_signed_saturated_sub16(ctx, ra, inst_ref, inst)
            }
            Opcode::SignedSaturatedSub32 => {
                sat::emit_signed_saturated_sub32(ctx, ra, inst_ref, inst)
            }
            Opcode::SignedSaturatedSub64 => {
                sat::emit_signed_saturated_sub64(ctx, ra, inst_ref, inst)
            }
            Opcode::UnsignedSaturatedAdd8 => {
                sat::emit_unsigned_saturated_add8(ctx, ra, inst_ref, inst)
            }
            Opcode::UnsignedSaturatedAdd16 => {
                sat::emit_unsigned_saturated_add16(ctx, ra, inst_ref, inst)
            }
            Opcode::UnsignedSaturatedAdd32 => {
                sat::emit_unsigned_saturated_add32(ctx, ra, inst_ref, inst)
            }
            Opcode::UnsignedSaturatedAdd64 => {
                sat::emit_unsigned_saturated_add64(ctx, ra, inst_ref, inst)
            }
            Opcode::UnsignedSaturatedSub8 => {
                sat::emit_unsigned_saturated_sub8(ctx, ra, inst_ref, inst)
            }
            Opcode::UnsignedSaturatedSub16 => {
                sat::emit_unsigned_saturated_sub16(ctx, ra, inst_ref, inst)
            }
            Opcode::UnsignedSaturatedSub32 => {
                sat::emit_unsigned_saturated_sub32(ctx, ra, inst_ref, inst)
            }
            Opcode::UnsignedSaturatedSub64 => {
                sat::emit_unsigned_saturated_sub64(ctx, ra, inst_ref, inst)
            }
            Opcode::SignedSaturation => sat::emit_signed_saturation(ctx, ra, inst_ref, inst),
            Opcode::UnsignedSaturation => sat::emit_unsigned_saturation(ctx, ra, inst_ref, inst),
            Opcode::SignedSaturatedDoublingMultiplyReturnHigh16 => {
                sat::emit_signed_saturated_doubling_multiply_return_high16(ctx, ra, inst_ref, inst)
            }
            Opcode::SignedSaturatedDoublingMultiplyReturnHigh32 => {
                sat::emit_signed_saturated_doubling_multiply_return_high32(ctx, ra, inst_ref, inst)
            }

            // --- FP scalar arithmetic ---
            Opcode::FPAdd32 => fp::emit_fp_add32(ctx, ra, inst_ref, inst),
            Opcode::FPAdd64 => fp::emit_fp_add64(ctx, ra, inst_ref, inst),
            Opcode::FPSub32 => fp::emit_fp_sub32(ctx, ra, inst_ref, inst),
            Opcode::FPSub64 => fp::emit_fp_sub64(ctx, ra, inst_ref, inst),
            Opcode::FPMul32 => fp::emit_fp_mul32(ctx, ra, inst_ref, inst),
            Opcode::FPMul64 => fp::emit_fp_mul64(ctx, ra, inst_ref, inst),
            Opcode::FPDiv32 => fp::emit_fp_div32(ctx, ra, inst_ref, inst),
            Opcode::FPDiv64 => fp::emit_fp_div64(ctx, ra, inst_ref, inst),
            Opcode::FPSqrt32 => fp::emit_fp_sqrt32(ctx, ra, inst_ref, inst),
            Opcode::FPSqrt64 => fp::emit_fp_sqrt64(ctx, ra, inst_ref, inst),
            Opcode::FPAbs32 => fp::emit_fp_abs32(ctx, ra, inst_ref, inst),
            Opcode::FPAbs64 => fp::emit_fp_abs64(ctx, ra, inst_ref, inst),
            Opcode::FPAbs16 => fp::emit_fp_abs16(ctx, ra, inst_ref, inst),
            Opcode::FPNeg32 => fp::emit_fp_neg32(ctx, ra, inst_ref, inst),
            Opcode::FPNeg64 => fp::emit_fp_neg64(ctx, ra, inst_ref, inst),
            Opcode::FPNeg16 => fp::emit_fp_neg16(ctx, ra, inst_ref, inst),
            Opcode::FPMax32 => fp::emit_fp_max32(ctx, ra, inst_ref, inst),
            Opcode::FPMax64 => fp::emit_fp_max64(ctx, ra, inst_ref, inst),
            Opcode::FPMin32 => fp::emit_fp_min32(ctx, ra, inst_ref, inst),
            Opcode::FPMin64 => fp::emit_fp_min64(ctx, ra, inst_ref, inst),
            Opcode::FPMaxNumeric32 => fp::emit_fp_max_numeric32(ctx, ra, inst_ref, inst),
            Opcode::FPMaxNumeric64 => fp::emit_fp_max_numeric64(ctx, ra, inst_ref, inst),
            Opcode::FPMinNumeric32 => fp::emit_fp_min_numeric32(ctx, ra, inst_ref, inst),
            Opcode::FPMinNumeric64 => fp::emit_fp_min_numeric64(ctx, ra, inst_ref, inst),
            Opcode::FPCompare32 => fp::emit_fp_compare32(ctx, ra, inst_ref, inst),
            Opcode::FPCompare64 => fp::emit_fp_compare64(ctx, ra, inst_ref, inst),
            Opcode::FPRoundInt32 => fp::emit_fp_round_int32(ctx, ra, inst_ref, inst),
            Opcode::FPRoundInt64 => fp::emit_fp_round_int64(ctx, ra, inst_ref, inst),
            Opcode::FPRoundInt16 => fp::emit_fp_round_int16(ctx, ra, inst_ref, inst),

            // --- FP fused multiply-add/sub ---
            Opcode::FPMulAdd32 => fp::emit_fp_mul_add32(ctx, ra, inst_ref, inst),
            Opcode::FPMulAdd64 => fp::emit_fp_mul_add64(ctx, ra, inst_ref, inst),
            Opcode::FPMulSub32 => fp::emit_fp_mul_sub32(ctx, ra, inst_ref, inst),
            Opcode::FPMulSub64 => fp::emit_fp_mul_sub64(ctx, ra, inst_ref, inst),
            Opcode::FPMulAdd16 => fp::emit_fp_mul_add16(ctx, ra, inst_ref, inst),
            Opcode::FPMulSub16 => fp::emit_fp_mul_sub16(ctx, ra, inst_ref, inst),

            // --- FP conversions ---
            Opcode::FPSingleToDouble => fp::emit_fp_single_to_double(ctx, ra, inst_ref, inst),
            Opcode::FPDoubleToSingle => fp::emit_fp_double_to_single(ctx, ra, inst_ref, inst),
            Opcode::FPHalfToSingle => fp::emit_fp_half_to_single(ctx, ra, inst_ref, inst),
            Opcode::FPHalfToDouble => fp::emit_fp_half_to_double(ctx, ra, inst_ref, inst),
            Opcode::FPSingleToHalf => fp::emit_fp_single_to_half(ctx, ra, inst_ref, inst),
            Opcode::FPDoubleToHalf => fp::emit_fp_double_to_half(ctx, ra, inst_ref, inst),

            // --- FP multiply extended ---
            Opcode::FPMulX32 => fp::emit_fp_mul_x32(ctx, ra, inst_ref, inst),
            Opcode::FPMulX64 => fp::emit_fp_mul_x64(ctx, ra, inst_ref, inst),

            // --- FP reciprocal/sqrt estimates ---
            Opcode::FPRecipEstimate16 => fp::emit_fp_recip_estimate16(ctx, ra, inst_ref, inst),
            Opcode::FPRecipEstimate32 => fp::emit_fp_recip_estimate32(ctx, ra, inst_ref, inst),
            Opcode::FPRecipEstimate64 => fp::emit_fp_recip_estimate64(ctx, ra, inst_ref, inst),
            Opcode::FPRecipExponent16 => fp::emit_fp_recip_exponent16(ctx, ra, inst_ref, inst),
            Opcode::FPRecipExponent32 => fp::emit_fp_recip_exponent32(ctx, ra, inst_ref, inst),
            Opcode::FPRecipExponent64 => fp::emit_fp_recip_exponent64(ctx, ra, inst_ref, inst),
            Opcode::FPRecipStepFused16 => fp::emit_fp_recip_step_fused16(ctx, ra, inst_ref, inst),
            Opcode::FPRecipStepFused32 => fp::emit_fp_recip_step_fused32(ctx, ra, inst_ref, inst),
            Opcode::FPRecipStepFused64 => fp::emit_fp_recip_step_fused64(ctx, ra, inst_ref, inst),
            Opcode::FPRSqrtEstimate16 => fp::emit_fp_rsqrt_estimate16(ctx, ra, inst_ref, inst),
            Opcode::FPRSqrtEstimate32 => fp::emit_fp_rsqrt_estimate32(ctx, ra, inst_ref, inst),
            Opcode::FPRSqrtEstimate64 => fp::emit_fp_rsqrt_estimate64(ctx, ra, inst_ref, inst),
            Opcode::FPRSqrtStepFused16 => fp::emit_fp_rsqrt_step_fused16(ctx, ra, inst_ref, inst),
            Opcode::FPRSqrtStepFused32 => fp::emit_fp_rsqrt_step_fused32(ctx, ra, inst_ref, inst),
            Opcode::FPRSqrtStepFused64 => fp::emit_fp_rsqrt_step_fused64(ctx, ra, inst_ref, inst),

            // --- FP fixed-point conversions ---
            Opcode::FPFixedS32ToSingle => fp::emit_fp_fixed_s32_to_single(ctx, ra, inst_ref, inst),
            Opcode::FPFixedS32ToDouble => fp::emit_fp_fixed_s32_to_double(ctx, ra, inst_ref, inst),
            Opcode::FPFixedU32ToSingle => fp::emit_fp_fixed_u32_to_single(ctx, ra, inst_ref, inst),
            Opcode::FPFixedU32ToDouble => fp::emit_fp_fixed_u32_to_double(ctx, ra, inst_ref, inst),
            Opcode::FPFixedS64ToSingle => fp::emit_fp_fixed_s64_to_single(ctx, ra, inst_ref, inst),
            Opcode::FPFixedS64ToDouble => fp::emit_fp_fixed_s64_to_double(ctx, ra, inst_ref, inst),
            Opcode::FPFixedU64ToSingle => fp::emit_fp_fixed_u64_to_single(ctx, ra, inst_ref, inst),
            Opcode::FPFixedU64ToDouble => fp::emit_fp_fixed_u64_to_double(ctx, ra, inst_ref, inst),
            Opcode::FPSingleToFixedS32 => fp::emit_fp_single_to_fixed_s32(ctx, ra, inst_ref, inst),
            Opcode::FPSingleToFixedS64 => fp::emit_fp_single_to_fixed_s64(ctx, ra, inst_ref, inst),
            Opcode::FPDoubleToFixedS32 => fp::emit_fp_double_to_fixed_s32(ctx, ra, inst_ref, inst),
            Opcode::FPDoubleToFixedS64 => fp::emit_fp_double_to_fixed_s64(ctx, ra, inst_ref, inst),
            Opcode::FPSingleToFixedU32 => fp::emit_fp_single_to_fixed_u32(ctx, ra, inst_ref, inst),
            Opcode::FPSingleToFixedU64 => fp::emit_fp_single_to_fixed_u64(ctx, ra, inst_ref, inst),
            Opcode::FPDoubleToFixedU32 => fp::emit_fp_double_to_fixed_u32(ctx, ra, inst_ref, inst),
            Opcode::FPDoubleToFixedU64 => fp::emit_fp_double_to_fixed_u64(ctx, ra, inst_ref, inst),
            Opcode::FPFixedU16ToSingle => fp::emit_fp_fixed_u16_to_single(ctx, ra, inst_ref, inst),
            Opcode::FPFixedS16ToSingle => fp::emit_fp_fixed_s16_to_single(ctx, ra, inst_ref, inst),
            Opcode::FPFixedU16ToDouble => fp::emit_fp_fixed_u16_to_double(ctx, ra, inst_ref, inst),
            Opcode::FPFixedS16ToDouble => fp::emit_fp_fixed_s16_to_double(ctx, ra, inst_ref, inst),

            // --- FP half/16-bit fixed-point ---
            Opcode::FPHalfToFixedS16 => fp::emit_fp_half_to_fixed_s16(ctx, ra, inst_ref, inst),
            Opcode::FPHalfToFixedS32 => fp::emit_fp_half_to_fixed_s32(ctx, ra, inst_ref, inst),
            Opcode::FPHalfToFixedS64 => fp::emit_fp_half_to_fixed_s64(ctx, ra, inst_ref, inst),
            Opcode::FPHalfToFixedU16 => fp::emit_fp_half_to_fixed_u16(ctx, ra, inst_ref, inst),
            Opcode::FPHalfToFixedU32 => fp::emit_fp_half_to_fixed_u32(ctx, ra, inst_ref, inst),
            Opcode::FPHalfToFixedU64 => fp::emit_fp_half_to_fixed_u64(ctx, ra, inst_ref, inst),
            Opcode::FPDoubleToFixedS16 => fp::emit_fp_double_to_fixed_s16(ctx, ra, inst_ref, inst),
            Opcode::FPDoubleToFixedU16 => fp::emit_fp_double_to_fixed_u16(ctx, ra, inst_ref, inst),
            Opcode::FPSingleToFixedS16 => fp::emit_fp_single_to_fixed_s16(ctx, ra, inst_ref, inst),
            Opcode::FPSingleToFixedU16 => fp::emit_fp_single_to_fixed_u16(ctx, ra, inst_ref, inst),

            // --- CRC32 ---
            Opcode::CRC32Castagnoli8 => emit_crc32::emit_crc32_castagnoli8(ctx, ra, inst_ref, inst),
            Opcode::CRC32Castagnoli16 => {
                emit_crc32::emit_crc32_castagnoli16(ctx, ra, inst_ref, inst)
            }
            Opcode::CRC32Castagnoli32 => {
                emit_crc32::emit_crc32_castagnoli32(ctx, ra, inst_ref, inst)
            }
            Opcode::CRC32Castagnoli64 => {
                emit_crc32::emit_crc32_castagnoli64(ctx, ra, inst_ref, inst)
            }
            Opcode::CRC32ISO8 => emit_crc32::emit_crc32_iso8(ctx, ra, inst_ref, inst),
            Opcode::CRC32ISO16 => emit_crc32::emit_crc32_iso16(ctx, ra, inst_ref, inst),
            Opcode::CRC32ISO32 => emit_crc32::emit_crc32_iso32(ctx, ra, inst_ref, inst),
            Opcode::CRC32ISO64 => emit_crc32::emit_crc32_iso64(ctx, ra, inst_ref, inst),

            // --- Crypto: AES ---
            Opcode::AESEncryptSingleRound => {
                emit_aes::emit_aes_encrypt_single_round(ctx, ra, inst_ref, inst)
            }
            Opcode::AESDecryptSingleRound => {
                emit_aes::emit_aes_decrypt_single_round(ctx, ra, inst_ref, inst)
            }
            Opcode::AESInverseMixColumns => {
                emit_aes::emit_aes_inverse_mix_columns(ctx, ra, inst_ref, inst)
            }
            Opcode::AESMixColumns => emit_aes::emit_aes_mix_columns(ctx, ra, inst_ref, inst),

            // --- Crypto: SHA/SM4 ---
            Opcode::SHA256Hash => emit_sha::emit_sha256_hash(ctx, ra, inst_ref, inst),
            Opcode::SHA256MessageSchedule0 => {
                emit_sha::emit_sha256_message_schedule_0(ctx, ra, inst_ref, inst)
            }
            Opcode::SHA256MessageSchedule1 => {
                emit_sha::emit_sha256_message_schedule_1(ctx, ra, inst_ref, inst)
            }
            Opcode::SM4AccessSubstitutionBox => {
                emit_sm4::emit_sm4_access_substitution_box(ctx, ra, inst_ref, inst)
            }

            // --- Packed operations ---
            Opcode::PackedAddU8 => packed::emit_packed_add_u8(ctx, ra, inst_ref, inst),
            Opcode::PackedAddS8 => packed::emit_packed_add_s8(ctx, ra, inst_ref, inst),
            Opcode::PackedAddU16 => packed::emit_packed_add_u16(ctx, ra, inst_ref, inst),
            Opcode::PackedAddS16 => packed::emit_packed_add_s16(ctx, ra, inst_ref, inst),
            Opcode::PackedSubU8 => packed::emit_packed_sub_u8(ctx, ra, inst_ref, inst),
            Opcode::PackedSubS8 => packed::emit_packed_sub_s8(ctx, ra, inst_ref, inst),
            Opcode::PackedSubU16 => packed::emit_packed_sub_u16(ctx, ra, inst_ref, inst),
            Opcode::PackedSubS16 => packed::emit_packed_sub_s16(ctx, ra, inst_ref, inst),
            Opcode::PackedSaturatedAddU8 => {
                packed::emit_packed_saturated_add_u8(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedSaturatedAddS8 => {
                packed::emit_packed_saturated_add_s8(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedSaturatedAddU16 => {
                packed::emit_packed_saturated_add_u16(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedSaturatedAddS16 => {
                packed::emit_packed_saturated_add_s16(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedSaturatedSubU8 => {
                packed::emit_packed_saturated_sub_u8(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedSaturatedSubS8 => {
                packed::emit_packed_saturated_sub_s8(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedSaturatedSubU16 => {
                packed::emit_packed_saturated_sub_u16(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedSaturatedSubS16 => {
                packed::emit_packed_saturated_sub_s16(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedAbsDiffSumU8 => {
                packed::emit_packed_abs_diff_sum_s8(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedSelect => packed::emit_packed_select(ctx, ra, inst_ref, inst),
            Opcode::PackedAddSubU16 => packed::emit_packed_add_sub_u16(ctx, ra, inst_ref, inst),
            Opcode::PackedAddSubS16 => packed::emit_packed_add_sub_s16(ctx, ra, inst_ref, inst),
            Opcode::PackedSubAddU16 => packed::emit_packed_sub_add_u16(ctx, ra, inst_ref, inst),
            Opcode::PackedSubAddS16 => packed::emit_packed_sub_add_s16(ctx, ra, inst_ref, inst),
            Opcode::PackedHalvingAddU8 => {
                packed::emit_packed_halving_add_u8(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedHalvingAddS8 => {
                packed::emit_packed_halving_add_s8(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedHalvingAddU16 => {
                packed::emit_packed_halving_add_u16(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedHalvingAddS16 => {
                packed::emit_packed_halving_add_s16(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedHalvingSubU8 => {
                packed::emit_packed_halving_sub_u8(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedHalvingSubS8 => {
                packed::emit_packed_halving_sub_s8(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedHalvingSubU16 => {
                packed::emit_packed_halving_sub_u16(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedHalvingSubS16 => {
                packed::emit_packed_halving_sub_s16(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedHalvingAddSubU16 => {
                packed::emit_packed_halving_add_sub_u16(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedHalvingAddSubS16 => {
                packed::emit_packed_halving_add_sub_s16(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedHalvingSubAddU16 => {
                packed::emit_packed_halving_sub_add_u16(ctx, ra, inst_ref, inst)
            }
            Opcode::PackedHalvingSubAddS16 => {
                packed::emit_packed_halving_sub_add_s16(ctx, ra, inst_ref, inst)
            }

            // --- Vector basic ---
            Opcode::VectorAdd8 => vbasic::emit_vector_add8(ctx, ra, inst_ref, inst),
            Opcode::VectorAdd16 => vbasic::emit_vector_add16(ctx, ra, inst_ref, inst),
            Opcode::VectorAdd32 => vbasic::emit_vector_add32(ctx, ra, inst_ref, inst),
            Opcode::VectorAdd64 => vbasic::emit_vector_add64(ctx, ra, inst_ref, inst),
            Opcode::VectorSub8 => vbasic::emit_vector_sub8(ctx, ra, inst_ref, inst),
            Opcode::VectorSub16 => vbasic::emit_vector_sub16(ctx, ra, inst_ref, inst),
            Opcode::VectorSub32 => vbasic::emit_vector_sub32(ctx, ra, inst_ref, inst),
            Opcode::VectorSub64 => vbasic::emit_vector_sub64(ctx, ra, inst_ref, inst),
            Opcode::VectorAnd => vbasic::emit_vector_and(ctx, ra, inst_ref, inst),
            Opcode::VectorAndNot => vbasic::emit_vector_and_not(ctx, ra, inst_ref, inst),
            Opcode::VectorOr => vbasic::emit_vector_or(ctx, ra, inst_ref, inst),
            Opcode::VectorEor => vbasic::emit_vector_eor(ctx, ra, inst_ref, inst),
            Opcode::VectorNot => vbasic::emit_vector_not(ctx, ra, inst_ref, inst),
            Opcode::VectorAbs8 => vbasic::emit_vector_abs8(ctx, ra, inst_ref, inst),
            Opcode::VectorAbs16 => vbasic::emit_vector_abs16(ctx, ra, inst_ref, inst),
            Opcode::VectorAbs32 => vbasic::emit_vector_abs32(ctx, ra, inst_ref, inst),
            Opcode::VectorAbs64 => vbasic::emit_vector_abs64(ctx, ra, inst_ref, inst),
            Opcode::ZeroVector => vbasic::emit_zero_vector(ctx, ra, inst_ref, inst),
            Opcode::VectorZeroUpper => vbasic::emit_vector_zero_upper(ctx, ra, inst_ref, inst),
            Opcode::VectorCountLeadingZeros8 => vbasic::emit_vector_clz8(ctx, ra, inst_ref, inst),
            Opcode::VectorCountLeadingZeros16 => vbasic::emit_vector_clz16(ctx, ra, inst_ref, inst),
            Opcode::VectorCountLeadingZeros32 => vbasic::emit_vector_clz32(ctx, ra, inst_ref, inst),
            Opcode::VectorPopulationCount => vbasic::emit_vector_popcount(ctx, ra, inst_ref, inst),
            Opcode::VectorReverseBits => vbasic::emit_vector_reverse_bits(ctx, ra, inst_ref, inst),
            Opcode::VectorReverseElementsInHalfGroups8 => {
                vbasic::emit_vector_reverse_half_groups_8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorReverseElementsInWordGroups8 => {
                vbasic::emit_vector_reverse_word_groups_8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorReverseElementsInWordGroups16 => {
                vbasic::emit_vector_reverse_word_groups_16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorReverseElementsInLongGroups8 => {
                vbasic::emit_vector_reverse_long_groups_8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorReverseElementsInLongGroups16 => {
                vbasic::emit_vector_reverse_long_groups_16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorReverseElementsInLongGroups32 => {
                vbasic::emit_vector_reverse_long_groups_32(ctx, ra, inst_ref, inst)
            }

            // --- Vector compare ---
            Opcode::VectorEqual8 => vcmp::emit_vector_equal8(ctx, ra, inst_ref, inst),
            Opcode::VectorEqual16 => vcmp::emit_vector_equal16(ctx, ra, inst_ref, inst),
            Opcode::VectorEqual32 => vcmp::emit_vector_equal32(ctx, ra, inst_ref, inst),
            Opcode::VectorEqual64 => vcmp::emit_vector_equal64(ctx, ra, inst_ref, inst),
            Opcode::VectorEqual128 => vcmp::emit_vector_equal128(ctx, ra, inst_ref, inst),
            Opcode::VectorGreaterS8 => {
                vcmp::emit_vector_greater_signed8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorGreaterS16 => {
                vcmp::emit_vector_greater_signed16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorGreaterS32 => {
                vcmp::emit_vector_greater_signed32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorGreaterS64 => {
                vcmp::emit_vector_greater_signed64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorGreaterEqualSigned8 => {
                vcmp::emit_vector_greater_equal_signed8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorGreaterEqualSigned16 => {
                vcmp::emit_vector_greater_equal_signed16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorGreaterEqualSigned32 => {
                vcmp::emit_vector_greater_equal_signed32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorGreaterEqualSigned64 => {
                vcmp::emit_vector_greater_equal_signed64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorGreaterEqualUnsigned8 => {
                vcmp::emit_vector_greater_equal_unsigned8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorGreaterEqualUnsigned16 => {
                vcmp::emit_vector_greater_equal_unsigned16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorGreaterEqualUnsigned32 => {
                vcmp::emit_vector_greater_equal_unsigned32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorGreaterEqualUnsigned64 => {
                vcmp::emit_vector_greater_equal_unsigned64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorLessEqualSigned8 => {
                vcmp::emit_vector_less_equal_signed8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorLessEqualSigned16 => {
                vcmp::emit_vector_less_equal_signed16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorLessEqualSigned32 => {
                vcmp::emit_vector_less_equal_signed32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorLessEqualSigned64 => {
                vcmp::emit_vector_less_equal_signed64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorLessSigned8 => vcmp::emit_vector_less_signed8(ctx, ra, inst_ref, inst),
            Opcode::VectorLessSigned16 => vcmp::emit_vector_less_signed16(ctx, ra, inst_ref, inst),
            Opcode::VectorLessSigned32 => vcmp::emit_vector_less_signed32(ctx, ra, inst_ref, inst),
            Opcode::VectorLessSigned64 => vcmp::emit_vector_less_signed64(ctx, ra, inst_ref, inst),
            Opcode::VectorMinS8 => vcmp::emit_vector_min_signed8(ctx, ra, inst_ref, inst),
            Opcode::VectorMinS16 => vcmp::emit_vector_min_signed16(ctx, ra, inst_ref, inst),
            Opcode::VectorMinS32 => vcmp::emit_vector_min_signed32(ctx, ra, inst_ref, inst),
            Opcode::VectorMinS64 => vcmp::emit_vector_min_signed64(ctx, ra, inst_ref, inst),
            Opcode::VectorMaxS8 => vcmp::emit_vector_max_signed8(ctx, ra, inst_ref, inst),
            Opcode::VectorMaxS16 => vcmp::emit_vector_max_signed16(ctx, ra, inst_ref, inst),
            Opcode::VectorMaxS32 => vcmp::emit_vector_max_signed32(ctx, ra, inst_ref, inst),
            Opcode::VectorMaxS64 => vcmp::emit_vector_max_signed64(ctx, ra, inst_ref, inst),
            Opcode::VectorMinU8 => vcmp::emit_vector_min_unsigned8(ctx, ra, inst_ref, inst),
            Opcode::VectorMinU16 => {
                vcmp::emit_vector_min_unsigned16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorMinU32 => {
                vcmp::emit_vector_min_unsigned32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorMinU64 => {
                vcmp::emit_vector_min_unsigned64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorMaxU8 => vcmp::emit_vector_max_unsigned8(ctx, ra, inst_ref, inst),
            Opcode::VectorMaxU16 => {
                vcmp::emit_vector_max_unsigned16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorMaxU32 => {
                vcmp::emit_vector_max_unsigned32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorMaxU64 => {
                vcmp::emit_vector_max_unsigned64(ctx, ra, inst_ref, inst)
            }

            // --- Vector shift ---
            Opcode::VectorLogicalShiftLeft8 => {
                vshift::emit_vector_logical_shift_left8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorLogicalShiftLeft16 => {
                vshift::emit_vector_logical_shift_left16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorLogicalShiftLeft32 => {
                vshift::emit_vector_logical_shift_left32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorLogicalShiftLeft64 => {
                vshift::emit_vector_logical_shift_left64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorLogicalShiftRight8 => {
                vshift::emit_vector_logical_shift_right8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorLogicalShiftRight16 => {
                vshift::emit_vector_logical_shift_right16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorLogicalShiftRight32 => {
                vshift::emit_vector_logical_shift_right32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorLogicalShiftRight64 => {
                vshift::emit_vector_logical_shift_right64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorArithmeticShiftRight8 => {
                vshift::emit_vector_arithmetic_shift_right8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorArithmeticShiftRight16 => {
                vshift::emit_vector_arithmetic_shift_right16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorArithmeticShiftRight32 => {
                vshift::emit_vector_arithmetic_shift_right32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorArithmeticShiftRight64 => {
                vshift::emit_vector_arithmetic_shift_right64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorLogicalVShift8 => {
                vshift::emit_vector_logical_vshift8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorLogicalVShift16 => {
                vshift::emit_vector_logical_vshift16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorLogicalVShift32 => {
                vshift::emit_vector_logical_vshift32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorLogicalVShift64 => {
                vshift::emit_vector_logical_vshift64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorArithmeticVShift8 => {
                vshift::emit_vector_arithmetic_vshift8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorArithmeticVShift16 => {
                vshift::emit_vector_arithmetic_vshift16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorArithmeticVShift32 => {
                vshift::emit_vector_arithmetic_vshift32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorArithmeticVShift64 => {
                vshift::emit_vector_arithmetic_vshift64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorRoundingShiftLeftS8 => {
                vshift::emit_vector_rounding_shift_left_signed8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorRoundingShiftLeftS16 => {
                vshift::emit_vector_rounding_shift_left_signed16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorRoundingShiftLeftS32 => {
                vshift::emit_vector_rounding_shift_left_signed32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorRoundingShiftLeftS64 => {
                vshift::emit_vector_rounding_shift_left_signed64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorRoundingShiftLeftU8 => {
                vshift::emit_vector_rounding_shift_left_unsigned8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorRoundingShiftLeftU16 => {
                vshift::emit_vector_rounding_shift_left_unsigned16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorRoundingShiftLeftU32 => {
                vshift::emit_vector_rounding_shift_left_unsigned32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorRoundingShiftLeftU64 => {
                vshift::emit_vector_rounding_shift_left_unsigned64(ctx, ra, inst_ref, inst)
            }

            // --- Vector multiply ---
            Opcode::VectorMultiply8 => vmul::emit_vector_multiply8(ctx, ra, inst_ref, inst),
            Opcode::VectorMultiply16 => vmul::emit_vector_multiply16(ctx, ra, inst_ref, inst),
            Opcode::VectorMultiply32 => vmul::emit_vector_multiply32(ctx, ra, inst_ref, inst),
            Opcode::VectorMultiply64 => vmul::emit_vector_multiply64(ctx, ra, inst_ref, inst),
            Opcode::VectorMultiplySignedWiden8 => {
                vmul::emit_vector_multiply_signed_widen8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorMultiplySignedWiden16 => {
                vmul::emit_vector_multiply_signed_widen16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorMultiplySignedWiden32 => {
                vmul::emit_vector_multiply_signed_widen32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorMultiplyUnsignedWiden8 => {
                vmul::emit_vector_multiply_unsigned_widen8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorMultiplyUnsignedWiden16 => {
                vmul::emit_vector_multiply_unsigned_widen16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorMultiplyUnsignedWiden32 => {
                vmul::emit_vector_multiply_unsigned_widen32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedMultiplyLong16 => {
                vmul::emit_vector_signed_multiply_long16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedMultiplyLong32 => {
                vmul::emit_vector_signed_multiply_long32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedMultiplyLong16 => {
                vmul::emit_vector_unsigned_multiply_long16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedMultiplyLong32 => {
                vmul::emit_vector_unsigned_multiply_long32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPolynomialMultiply8 => {
                vmul::emit_vector_polynomial_multiply8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPolynomialMultiplyLong8 => {
                vmul::emit_vector_polynomial_multiply_long8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPolynomialMultiplyLong64 => {
                vmul::emit_vector_polynomial_multiply_long64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedAdd8 => vmul::emit_vector_paired_add8(ctx, ra, inst_ref, inst),
            Opcode::VectorPairedAdd16 => vmul::emit_vector_paired_add16(ctx, ra, inst_ref, inst),
            Opcode::VectorPairedAdd32 => vmul::emit_vector_paired_add32(ctx, ra, inst_ref, inst),
            Opcode::VectorPairedAdd64 => vmul::emit_vector_paired_add64(ctx, ra, inst_ref, inst),
            Opcode::VectorPairedAddLower8 => {
                vmul::emit_vector_paired_add_lower8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedAddLower16 => {
                vmul::emit_vector_paired_add_lower16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedAddLower32 => {
                vmul::emit_vector_paired_add_lower32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedAddSignedWiden8 => {
                vmul::emit_vector_paired_add_signed_widen8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedAddSignedWiden16 => {
                vmul::emit_vector_paired_add_signed_widen16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedAddSignedWiden32 => {
                vmul::emit_vector_paired_add_signed_widen32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedAddUnsignedWiden8 => {
                vmul::emit_vector_paired_add_unsigned_widen8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedAddUnsignedWiden16 => {
                vmul::emit_vector_paired_add_unsigned_widen16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedAddUnsignedWiden32 => {
                vmul::emit_vector_paired_add_unsigned_widen32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMaxS8 => {
                vmul::emit_vector_paired_max_signed8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMaxS16 => {
                vmul::emit_vector_paired_max_signed16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMaxS32 => {
                vmul::emit_vector_paired_max_signed32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMaxU8 => {
                vmul::emit_vector_paired_max_unsigned8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMaxU16 => {
                vmul::emit_vector_paired_max_unsigned16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMaxU32 => {
                vmul::emit_vector_paired_max_unsigned32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMaxLowerS8 => {
                vmul::emit_vector_paired_max_signed_lower8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMaxLowerS16 => {
                vmul::emit_vector_paired_max_signed_lower16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMaxLowerS32 => {
                vmul::emit_vector_paired_max_signed_lower32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMaxLowerU8 => {
                vmul::emit_vector_paired_max_unsigned_lower8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMaxLowerU16 => {
                vmul::emit_vector_paired_max_unsigned_lower16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMaxLowerU32 => {
                vmul::emit_vector_paired_max_unsigned_lower32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMinS8 => {
                vmul::emit_vector_paired_min_signed8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMinS16 => {
                vmul::emit_vector_paired_min_signed16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMinS32 => {
                vmul::emit_vector_paired_min_signed32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMinU8 => {
                vmul::emit_vector_paired_min_unsigned8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMinU16 => {
                vmul::emit_vector_paired_min_unsigned16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMinU32 => {
                vmul::emit_vector_paired_min_unsigned32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMinLowerS8 => {
                vmul::emit_vector_paired_min_signed_lower8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMinLowerS16 => {
                vmul::emit_vector_paired_min_signed_lower16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMinLowerS32 => {
                vmul::emit_vector_paired_min_signed_lower32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMinLowerU8 => {
                vmul::emit_vector_paired_min_unsigned_lower8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMinLowerU16 => {
                vmul::emit_vector_paired_min_unsigned_lower16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorPairedMinLowerU32 => {
                vmul::emit_vector_paired_min_unsigned_lower32(ctx, ra, inst_ref, inst)
            }

            // --- Vector arrangement ---
            Opcode::VectorGetElement8 => varr::emit_vector_get_element8(ctx, ra, inst_ref, inst),
            Opcode::VectorGetElement16 => varr::emit_vector_get_element16(ctx, ra, inst_ref, inst),
            Opcode::VectorGetElement32 => varr::emit_vector_get_element32(ctx, ra, inst_ref, inst),
            Opcode::VectorGetElement64 => varr::emit_vector_get_element64(ctx, ra, inst_ref, inst),
            Opcode::VectorSetElement8 => varr::emit_vector_set_element8(ctx, ra, inst_ref, inst),
            Opcode::VectorSetElement16 => varr::emit_vector_set_element16(ctx, ra, inst_ref, inst),
            Opcode::VectorSetElement32 => varr::emit_vector_set_element32(ctx, ra, inst_ref, inst),
            Opcode::VectorSetElement64 => varr::emit_vector_set_element64(ctx, ra, inst_ref, inst),
            Opcode::VectorBroadcast8 => varr::emit_vector_broadcast8(ctx, ra, inst_ref, inst),
            Opcode::VectorBroadcast16 => varr::emit_vector_broadcast16(ctx, ra, inst_ref, inst),
            Opcode::VectorBroadcast32 => varr::emit_vector_broadcast32(ctx, ra, inst_ref, inst),
            Opcode::VectorBroadcast64 => varr::emit_vector_broadcast64(ctx, ra, inst_ref, inst),
            Opcode::VectorBroadcastLower8 => {
                varr::emit_vector_broadcast_lower8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorBroadcastLower16 => {
                varr::emit_vector_broadcast_lower16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorBroadcastLower32 => {
                varr::emit_vector_broadcast_lower32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorExtract => varr::emit_vector_extract(ctx, ra, inst_ref, inst),
            Opcode::VectorExtractLower => varr::emit_vector_extract_lower(ctx, ra, inst_ref, inst),
            Opcode::VectorRotateWholeVectorRight => {
                varr::emit_vector_rotate_whole_vector_right(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorInterleaveLower8 => {
                varr::emit_vector_interleave_lower8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorInterleaveLower16 => {
                varr::emit_vector_interleave_lower16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorInterleaveLower32 => {
                varr::emit_vector_interleave_lower32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorInterleaveLower64 => {
                varr::emit_vector_interleave_lower64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorInterleaveUpper8 => {
                varr::emit_vector_interleave_upper8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorInterleaveUpper16 => {
                varr::emit_vector_interleave_upper16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorInterleaveUpper32 => {
                varr::emit_vector_interleave_upper32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorInterleaveUpper64 => {
                varr::emit_vector_interleave_upper64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorDeinterleaveEven8 => {
                varr::emit_vector_deinterleave_even8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorDeinterleaveEven16 => {
                varr::emit_vector_deinterleave_even16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorDeinterleaveEven32 => {
                varr::emit_vector_deinterleave_even32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorDeinterleaveEven64 => {
                varr::emit_vector_deinterleave_even64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorDeinterleaveOdd8 => {
                varr::emit_vector_deinterleave_odd8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorDeinterleaveOdd16 => {
                varr::emit_vector_deinterleave_odd16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorDeinterleaveOdd32 => {
                varr::emit_vector_deinterleave_odd32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorDeinterleaveOdd64 => {
                varr::emit_vector_deinterleave_odd64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorDeinterleaveEvenLower8 => {
                varr::emit_vector_deinterleave_even_lower8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorDeinterleaveEvenLower16 => {
                varr::emit_vector_deinterleave_even_lower16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorDeinterleaveEvenLower32 => {
                varr::emit_vector_deinterleave_even_lower32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorDeinterleaveOddLower8 => {
                varr::emit_vector_deinterleave_odd_lower8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorDeinterleaveOddLower16 => {
                varr::emit_vector_deinterleave_odd_lower16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorDeinterleaveOddLower32 => {
                varr::emit_vector_deinterleave_odd_lower32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorTranspose8 => varr::emit_vector_transpose8(ctx, ra, inst_ref, inst),
            Opcode::VectorTranspose16 => varr::emit_vector_transpose16(ctx, ra, inst_ref, inst),
            Opcode::VectorTranspose32 => varr::emit_vector_transpose32(ctx, ra, inst_ref, inst),
            Opcode::VectorTranspose64 => varr::emit_vector_transpose64(ctx, ra, inst_ref, inst),
            Opcode::VectorShuffleWords => varr::emit_vector_shuffle_words(ctx, ra, inst_ref, inst),
            Opcode::VectorShuffleHighHalfwords => {
                varr::emit_vector_shuffle_high_halfwords(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorShuffleLowHalfwords => {
                varr::emit_vector_shuffle_low_halfwords(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorNarrow16 => varr::emit_vector_narrow16(ctx, ra, inst_ref, inst),
            Opcode::VectorNarrow32 => varr::emit_vector_narrow32(ctx, ra, inst_ref, inst),
            Opcode::VectorNarrow64 => varr::emit_vector_narrow64(ctx, ra, inst_ref, inst),
            Opcode::VectorSignExtend8 => varr::emit_vector_sign_extend8(ctx, ra, inst_ref, inst),
            Opcode::VectorSignExtend16 => varr::emit_vector_sign_extend16(ctx, ra, inst_ref, inst),
            Opcode::VectorSignExtend32 => varr::emit_vector_sign_extend32(ctx, ra, inst_ref, inst),
            Opcode::VectorSignExtend64 => varr::emit_vector_sign_extend64(ctx, ra, inst_ref, inst),
            Opcode::VectorZeroExtend8 => varr::emit_vector_zero_extend8(ctx, ra, inst_ref, inst),
            Opcode::VectorZeroExtend16 => varr::emit_vector_zero_extend16(ctx, ra, inst_ref, inst),
            Opcode::VectorZeroExtend32 => varr::emit_vector_zero_extend32(ctx, ra, inst_ref, inst),
            Opcode::VectorZeroExtend64 => varr::emit_vector_zero_extend64(ctx, ra, inst_ref, inst),

            // --- Vector saturated ---
            Opcode::VectorSignedSaturatedAbs8 => {
                vsat::emit_vector_signed_saturated_abs8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedAbs16 => {
                vsat::emit_vector_signed_saturated_abs16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedAbs32 => {
                vsat::emit_vector_signed_saturated_abs32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedAbs64 => {
                vsat::emit_vector_signed_saturated_abs64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedNeg8 => {
                vsat::emit_vector_signed_saturated_neg8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedNeg16 => {
                vsat::emit_vector_signed_saturated_neg16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedNeg32 => {
                vsat::emit_vector_signed_saturated_neg32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedNeg64 => {
                vsat::emit_vector_signed_saturated_neg64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedAccumulateUnsigned8 => {
                vsat::emit_vector_signed_saturated_accumulate_unsigned8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedAccumulateUnsigned16 => {
                vsat::emit_vector_signed_saturated_accumulate_unsigned16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedAccumulateUnsigned32 => {
                vsat::emit_vector_signed_saturated_accumulate_unsigned32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedAccumulateUnsigned64 => {
                vsat::emit_vector_signed_saturated_accumulate_unsigned64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedAccumulateSigned8 => {
                vsat::emit_vector_unsigned_saturated_accumulate_signed8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedAccumulateSigned16 => {
                vsat::emit_vector_unsigned_saturated_accumulate_signed16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedAccumulateSigned32 => {
                vsat::emit_vector_unsigned_saturated_accumulate_signed32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedAccumulateSigned64 => {
                vsat::emit_vector_unsigned_saturated_accumulate_signed64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedNarrowToSigned16 => {
                vsat::emit_vector_signed_saturated_narrow_to_signed16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedNarrowToSigned32 => {
                vsat::emit_vector_signed_saturated_narrow_to_signed32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedNarrowToSigned64 => {
                vsat::emit_vector_signed_saturated_narrow_to_signed64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedNarrowToUnsigned16 => {
                vsat::emit_vector_signed_saturated_narrow_to_unsigned16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedNarrowToUnsigned32 => {
                vsat::emit_vector_signed_saturated_narrow_to_unsigned32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedNarrowToUnsigned64 => {
                vsat::emit_vector_signed_saturated_narrow_to_unsigned64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedNarrow16 => {
                vsat::emit_vector_unsigned_saturated_narrow16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedNarrow32 => {
                vsat::emit_vector_unsigned_saturated_narrow32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedNarrow64 => {
                vsat::emit_vector_unsigned_saturated_narrow64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedShiftLeft8 => {
                vsat::emit_vector_signed_saturated_shift_left8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedShiftLeft16 => {
                vsat::emit_vector_signed_saturated_shift_left16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedShiftLeft32 => {
                vsat::emit_vector_signed_saturated_shift_left32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedShiftLeft64 => {
                vsat::emit_vector_signed_saturated_shift_left64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedShiftLeftUnsigned8 => {
                vsat::emit_vector_signed_saturated_shift_left_unsigned8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedShiftLeftUnsigned16 => {
                vsat::emit_vector_signed_saturated_shift_left_unsigned16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedShiftLeftUnsigned32 => {
                vsat::emit_vector_signed_saturated_shift_left_unsigned32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedShiftLeftUnsigned64 => {
                vsat::emit_vector_signed_saturated_shift_left_unsigned64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedShiftLeft8 => {
                vsat::emit_vector_unsigned_saturated_shift_left8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedShiftLeft16 => {
                vsat::emit_vector_unsigned_saturated_shift_left16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedShiftLeft32 => {
                vsat::emit_vector_unsigned_saturated_shift_left32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedShiftLeft64 => {
                vsat::emit_vector_unsigned_saturated_shift_left64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedAdd8 => {
                vsat::emit_vector_signed_saturated_add8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedAdd16 => {
                vsat::emit_vector_signed_saturated_add16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedAdd32 => {
                vsat::emit_vector_signed_saturated_add32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedAdd64 => {
                vsat::emit_vector_signed_saturated_add64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedSub8 => {
                vsat::emit_vector_signed_saturated_sub8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedSub16 => {
                vsat::emit_vector_signed_saturated_sub16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedSub32 => {
                vsat::emit_vector_signed_saturated_sub32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedSub64 => {
                vsat::emit_vector_signed_saturated_sub64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedAdd8 => {
                vsat::emit_vector_unsigned_saturated_add8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedAdd16 => {
                vsat::emit_vector_unsigned_saturated_add16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedAdd32 => {
                vsat::emit_vector_unsigned_saturated_add32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedAdd64 => {
                vsat::emit_vector_unsigned_saturated_add64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedSub8 => {
                vsat::emit_vector_unsigned_saturated_sub8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedSub16 => {
                vsat::emit_vector_unsigned_saturated_sub16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedSub32 => {
                vsat::emit_vector_unsigned_saturated_sub32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedSaturatedSub64 => {
                vsat::emit_vector_unsigned_saturated_sub64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedDoublingMultiplyHigh16 => {
                vsat::emit_vector_signed_saturated_doubling_multiply_high16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedDoublingMultiplyHigh32 => {
                vsat::emit_vector_signed_saturated_doubling_multiply_high32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedDoublingMultiplyHighRounding16 => {
                vsat::emit_vector_signed_saturated_doubling_multiply_high_rounding16(
                    ctx, ra, inst_ref, inst,
                )
            }
            Opcode::VectorSignedSaturatedDoublingMultiplyHighRounding32 => {
                vsat::emit_vector_signed_saturated_doubling_multiply_high_rounding32(
                    ctx, ra, inst_ref, inst,
                )
            }
            Opcode::VectorSignedSaturatedDoublingMultiplyLong16 => {
                vsat::emit_vector_signed_saturated_doubling_multiply_long16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedSaturatedDoublingMultiplyLong32 => {
                vsat::emit_vector_signed_saturated_doubling_multiply_long32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorHalvingAddS8 => {
                vsat::emit_vector_halving_add_signed8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorHalvingAddS16 => {
                vsat::emit_vector_halving_add_signed16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorHalvingAddS32 => {
                vsat::emit_vector_halving_add_signed32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorHalvingAddU8 => {
                vsat::emit_vector_halving_add_unsigned8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorHalvingAddU16 => {
                vsat::emit_vector_halving_add_unsigned16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorHalvingAddU32 => {
                vsat::emit_vector_halving_add_unsigned32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorHalvingSubS8 => {
                vsat::emit_vector_halving_sub_signed8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorHalvingSubS16 => {
                vsat::emit_vector_halving_sub_signed16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorHalvingSubS32 => {
                vsat::emit_vector_halving_sub_signed32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorHalvingSubU8 => {
                vsat::emit_vector_halving_sub_unsigned8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorHalvingSubU16 => {
                vsat::emit_vector_halving_sub_unsigned16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorHalvingSubU32 => {
                vsat::emit_vector_halving_sub_unsigned32(ctx, ra, inst_ref, inst)
            }

            // --- Vector misc ---
            Opcode::VectorSignedAbsoluteDifference8 => {
                vmisc::emit_vector_signed_absolute_difference8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedAbsoluteDifference16 => {
                vmisc::emit_vector_signed_absolute_difference16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorSignedAbsoluteDifference32 => {
                vmisc::emit_vector_signed_absolute_difference32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedAbsoluteDifference8 => {
                vmisc::emit_vector_unsigned_absolute_difference8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedAbsoluteDifference16 => {
                vmisc::emit_vector_unsigned_absolute_difference16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedAbsoluteDifference32 => {
                vmisc::emit_vector_unsigned_absolute_difference32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorRoundingHalvingAddS8 => {
                vmisc::emit_vector_rounding_halving_add_signed8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorRoundingHalvingAddS16 => {
                vmisc::emit_vector_rounding_halving_add_signed16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorRoundingHalvingAddS32 => {
                vmisc::emit_vector_rounding_halving_add_signed32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorRoundingHalvingAddU8 => {
                vmisc::emit_vector_rounding_halving_add_unsigned8(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorRoundingHalvingAddU16 => {
                vmisc::emit_vector_rounding_halving_add_unsigned16(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorRoundingHalvingAddU32 => {
                vmisc::emit_vector_rounding_halving_add_unsigned32(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorTable => vmisc::emit_vector_table(ctx, ra, inst_ref, inst),
            Opcode::VectorTableLookup64 => {
                vmisc::emit_vector_table_lookup64(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorTableLookup128 => {
                vmisc::emit_vector_table_lookup128(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedRecipEstimate => {
                vmisc::emit_vector_unsigned_recip_estimate(ctx, ra, inst_ref, inst)
            }
            Opcode::VectorUnsignedRecipSqrtEstimate => {
                vmisc::emit_vector_unsigned_recip_sqrt_estimate(ctx, ra, inst_ref, inst)
            }

            // --- FP vector ---
            Opcode::FPVectorAdd32 => fpv::emit_fp_vector_add32(ctx, ra, inst_ref, inst),
            Opcode::FPVectorAdd64 => fpv::emit_fp_vector_add64(ctx, ra, inst_ref, inst),
            Opcode::FPVectorSub32 => fpv::emit_fp_vector_sub32(ctx, ra, inst_ref, inst),
            Opcode::FPVectorSub64 => fpv::emit_fp_vector_sub64(ctx, ra, inst_ref, inst),
            Opcode::FPVectorMul32 => fpv::emit_fp_vector_mul32(ctx, ra, inst_ref, inst),
            Opcode::FPVectorMul64 => fpv::emit_fp_vector_mul64(ctx, ra, inst_ref, inst),
            Opcode::FPVectorDiv32 => fpv::emit_fp_vector_div32(ctx, ra, inst_ref, inst),
            Opcode::FPVectorDiv64 => fpv::emit_fp_vector_div64(ctx, ra, inst_ref, inst),
            Opcode::FPVectorSqrt32 => fpv::emit_fp_vector_sqrt32(ctx, ra, inst_ref, inst),
            Opcode::FPVectorSqrt64 => fpv::emit_fp_vector_sqrt64(ctx, ra, inst_ref, inst),
            Opcode::FPVectorAbs16 => fpv::emit_fp_vector_abs16(ctx, ra, inst_ref, inst),
            Opcode::FPVectorAbs32 => fpv::emit_fp_vector_abs32(ctx, ra, inst_ref, inst),
            Opcode::FPVectorAbs64 => fpv::emit_fp_vector_abs64(ctx, ra, inst_ref, inst),
            Opcode::FPVectorNeg16 => fpv::emit_fp_vector_neg16(ctx, ra, inst_ref, inst),
            Opcode::FPVectorNeg32 => fpv::emit_fp_vector_neg32(ctx, ra, inst_ref, inst),
            Opcode::FPVectorNeg64 => fpv::emit_fp_vector_neg64(ctx, ra, inst_ref, inst),
            Opcode::FPVectorMax32 => fpv::emit_fp_vector_max32(ctx, ra, inst_ref, inst),
            Opcode::FPVectorMax64 => fpv::emit_fp_vector_max64(ctx, ra, inst_ref, inst),
            Opcode::FPVectorMin32 => fpv::emit_fp_vector_min32(ctx, ra, inst_ref, inst),
            Opcode::FPVectorMin64 => fpv::emit_fp_vector_min64(ctx, ra, inst_ref, inst),
            Opcode::FPVectorMaxNumeric32 => {
                fpv::emit_fp_vector_max_numeric32(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorMaxNumeric64 => {
                fpv::emit_fp_vector_max_numeric64(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorMinNumeric32 => {
                fpv::emit_fp_vector_min_numeric32(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorMinNumeric64 => {
                fpv::emit_fp_vector_min_numeric64(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorEqual16 => fpv::emit_fp_vector_equal16(ctx, ra, inst_ref, inst),
            Opcode::FPVectorEqual32 => fpv::emit_fp_vector_equal32(ctx, ra, inst_ref, inst),
            Opcode::FPVectorEqual64 => fpv::emit_fp_vector_equal64(ctx, ra, inst_ref, inst),
            Opcode::FPVectorGreater32 => fpv::emit_fp_vector_greater32(ctx, ra, inst_ref, inst),
            Opcode::FPVectorGreater64 => fpv::emit_fp_vector_greater64(ctx, ra, inst_ref, inst),
            Opcode::FPVectorGreaterEqual32 => {
                fpv::emit_fp_vector_greater_equal32(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorGreaterEqual64 => {
                fpv::emit_fp_vector_greater_equal64(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorMulX32 => fpv::emit_fp_vector_mulx32(ctx, ra, inst_ref, inst),
            Opcode::FPVectorMulX64 => fpv::emit_fp_vector_mulx64(ctx, ra, inst_ref, inst),
            Opcode::FPVectorPairedAdd32 => {
                fpv::emit_fp_vector_paired_add32(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorPairedAdd64 => {
                fpv::emit_fp_vector_paired_add64(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorPairedAddLower32 => {
                fpv::emit_fp_vector_paired_add_lower32(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorPairedAddLower64 => {
                fpv::emit_fp_vector_paired_add_lower64(ctx, ra, inst_ref, inst)
            }

            // --- FP vector convert ---
            Opcode::FPVectorMulAdd16 => fpvc::emit_fp_vector_muladd16(ctx, ra, inst_ref, inst),
            Opcode::FPVectorMulAdd32 => fpvc::emit_fp_vector_muladd32(ctx, ra, inst_ref, inst),
            Opcode::FPVectorMulAdd64 => fpvc::emit_fp_vector_muladd64(ctx, ra, inst_ref, inst),
            Opcode::FPVectorRecipEstimate16 => {
                fpvc::emit_fp_vector_recip_estimate16(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorRecipEstimate32 => {
                fpvc::emit_fp_vector_recip_estimate32(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorRecipEstimate64 => {
                fpvc::emit_fp_vector_recip_estimate64(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorRecipStepFused16 => {
                fpvc::emit_fp_vector_recip_step_fused16(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorRecipStepFused32 => {
                fpvc::emit_fp_vector_recip_step_fused32(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorRecipStepFused64 => {
                fpvc::emit_fp_vector_recip_step_fused64(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorRSqrtEstimate16 => {
                fpvc::emit_fp_vector_rsqrt_estimate16(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorRSqrtEstimate32 => {
                fpvc::emit_fp_vector_rsqrt_estimate32(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorRSqrtEstimate64 => {
                fpvc::emit_fp_vector_rsqrt_estimate64(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorRSqrtStepFused16 => {
                fpvc::emit_fp_vector_rsqrt_step_fused16(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorRSqrtStepFused32 => {
                fpvc::emit_fp_vector_rsqrt_step_fused32(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorRSqrtStepFused64 => {
                fpvc::emit_fp_vector_rsqrt_step_fused64(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorRoundInt16 => fpvc::emit_fp_vector_round_int16(ctx, ra, inst_ref, inst),
            Opcode::FPVectorRoundInt32 => fpvc::emit_fp_vector_round_int32(ctx, ra, inst_ref, inst),
            Opcode::FPVectorRoundInt64 => fpvc::emit_fp_vector_round_int64(ctx, ra, inst_ref, inst),
            Opcode::FPVectorFromSignedFixed32 => {
                fpvc::emit_fp_vector_from_signed_fixed32(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorFromSignedFixed64 => {
                fpvc::emit_fp_vector_from_signed_fixed64(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorFromUnsignedFixed32 => {
                fpvc::emit_fp_vector_from_unsigned_fixed32(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorFromUnsignedFixed64 => {
                fpvc::emit_fp_vector_from_unsigned_fixed64(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorToSignedFixed16 => {
                fpvc::emit_fp_vector_to_signed_fixed16(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorToSignedFixed32 => {
                fpvc::emit_fp_vector_to_signed_fixed32(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorToSignedFixed64 => {
                fpvc::emit_fp_vector_to_signed_fixed64(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorToUnsignedFixed16 => {
                fpvc::emit_fp_vector_to_unsigned_fixed16(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorToUnsignedFixed32 => {
                fpvc::emit_fp_vector_to_unsigned_fixed32(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorToUnsignedFixed64 => {
                fpvc::emit_fp_vector_to_unsigned_fixed64(ctx, ra, inst_ref, inst)
            }
            Opcode::FPVectorFromHalf32 => fpvc::emit_fp_vector_from_half32(ctx, ra, inst_ref, inst),
            Opcode::FPVectorToHalf32 => fpvc::emit_fp_vector_to_half32(ctx, ra, inst_ref, inst),

            // --- A32 context getters/setters ---
            Opcode::A32SetCheckBit => a32::emit_a32_set_check_bit(ctx, ra, inst_ref, inst),
            Opcode::A32GetCFlag => a32::emit_a32_get_c_flag(ctx, ra, inst_ref, inst),
            Opcode::A32GetRegister => a32::emit_a32_get_register(ctx, ra, inst_ref, inst),
            Opcode::A32SetRegister => a32::emit_a32_set_register(ctx, ra, inst_ref, inst),
            Opcode::A32GetExtendedRegister32 => {
                a32::emit_a32_get_extended_register32(ctx, ra, inst_ref, inst)
            }
            Opcode::A32GetExtendedRegister64 => {
                a32::emit_a32_get_extended_register64(ctx, ra, inst_ref, inst)
            }
            Opcode::A32SetExtendedRegister32 => {
                a32::emit_a32_set_extended_register32(ctx, ra, inst_ref, inst)
            }
            Opcode::A32SetExtendedRegister64 => {
                a32::emit_a32_set_extended_register64(ctx, ra, inst_ref, inst)
            }
            Opcode::A32GetVector => a32::emit_a32_get_vector(ctx, ra, inst_ref, inst),
            Opcode::A32SetVector => a32::emit_a32_set_vector(ctx, ra, inst_ref, inst),
            Opcode::A32GetCpsr => a32::emit_a32_get_cpsr(ctx, ra, inst_ref, inst),
            Opcode::A32SetCpsr => a32::emit_a32_set_cpsr(ctx, ra, inst_ref, inst),
            Opcode::A32SetCpsrNZCVRaw => a32::emit_a32_set_cpsr_nzcv_raw(ctx, ra, inst_ref, inst),
            Opcode::A32SetCpsrNZCV => a32::emit_a32_set_cpsr_nzcv(ctx, ra, inst_ref, inst),
            Opcode::A32SetCpsrNZCVQ => a32::emit_a32_set_cpsr_nzcvq(ctx, ra, inst_ref, inst),
            Opcode::A32SetCpsrNZ => a32::emit_a32_set_cpsr_nz(ctx, ra, inst_ref, inst),
            Opcode::A32SetCpsrNZC => a32::emit_a32_set_cpsr_nzc(ctx, ra, inst_ref, inst),
            Opcode::A32OrQFlag => a32::emit_a32_or_q_flag(ctx, ra, inst_ref, inst),
            Opcode::A32GetGEFlags => a32::emit_a32_get_ge_flags(ctx, ra, inst_ref, inst),
            Opcode::A32SetGEFlags => a32::emit_a32_set_ge_flags(ctx, ra, inst_ref, inst),
            Opcode::A32SetGEFlagsCompressed => {
                a32::emit_a32_set_ge_flags_compressed(ctx, ra, inst_ref, inst)
            }
            Opcode::A32BXWritePC => a32::emit_a32_bx_write_pc(ctx, ra, inst_ref, inst),
            Opcode::A32UpdateUpperLocationDescriptor => {
                a32::emit_a32_update_upper_location_descriptor(ctx, ra, inst_ref, inst)
            }
            Opcode::A32CallSupervisor => a32::emit_a32_call_supervisor(ctx, ra, inst_ref, inst),
            Opcode::A32PcExecHook => a32::emit_a32_pc_exec_hook(ctx, ra, inst_ref, inst),
            Opcode::A32ExceptionRaised => a32::emit_a32_exception_raised(ctx, ra, inst_ref, inst),
            Opcode::A32DataSynchronizationBarrier => a32::emit_a32_dsb(ctx, ra, inst_ref, inst),
            Opcode::A32DataMemoryBarrier => a32::emit_a32_dmb(ctx, ra, inst_ref, inst),
            Opcode::A32InstructionSynchronizationBarrier => {
                a32::emit_a32_isb(ctx, ra, inst_ref, inst)
            }
            Opcode::A32GetFpscr => a32::emit_a32_get_fpscr(ctx, ra, inst_ref, inst),
            Opcode::A32SetFpscr => a32::emit_a32_set_fpscr(ctx, ra, inst_ref, inst),
            Opcode::A32GetFpscrNZCV => a32::emit_a32_get_fpscr_nzcv(ctx, ra, inst_ref, inst),
            Opcode::A32SetFpscrNZCV => a32::emit_a32_set_fpscr_nzcv(ctx, ra, inst_ref, inst),

            // --- A32 Memory ---
            Opcode::A32ClearExclusive => a32::emit_a32_clear_exclusive(ctx, ra, inst_ref, inst),
            Opcode::A32ReadMemory8 => a32::emit_a32_read_memory_8(ctx, ra, inst_ref, inst),
            Opcode::A32ReadMemory16 => a32::emit_a32_read_memory_16(ctx, ra, inst_ref, inst),
            Opcode::A32ReadMemory32 => a32::emit_a32_read_memory_32(ctx, ra, inst_ref, inst),
            Opcode::A32ReadMemory64 => a32::emit_a32_read_memory_64(ctx, ra, inst_ref, inst),
            Opcode::A32ExclusiveReadMemory8 => {
                a32::emit_a32_exclusive_read_memory_8(ctx, ra, inst_ref, inst)
            }
            Opcode::A32ExclusiveReadMemory16 => {
                a32::emit_a32_exclusive_read_memory_16(ctx, ra, inst_ref, inst)
            }
            Opcode::A32ExclusiveReadMemory32 => {
                a32::emit_a32_exclusive_read_memory_32(ctx, ra, inst_ref, inst)
            }
            Opcode::A32ExclusiveReadMemory64 => {
                a32::emit_a32_exclusive_read_memory_64(ctx, ra, inst_ref, inst)
            }
            Opcode::A32WriteMemory8 => a32::emit_a32_write_memory_8(ctx, ra, inst_ref, inst),
            Opcode::A32WriteMemory16 => a32::emit_a32_write_memory_16(ctx, ra, inst_ref, inst),
            Opcode::A32WriteMemory32 => a32::emit_a32_write_memory_32(ctx, ra, inst_ref, inst),
            Opcode::A32WriteMemory64 => a32::emit_a32_write_memory_64(ctx, ra, inst_ref, inst),
            Opcode::A32ExclusiveWriteMemory8 => {
                a32::emit_a32_exclusive_write_memory_8(ctx, ra, inst_ref, inst)
            }
            Opcode::A32ExclusiveWriteMemory16 => {
                a32::emit_a32_exclusive_write_memory_16(ctx, ra, inst_ref, inst)
            }
            Opcode::A32ExclusiveWriteMemory32 => {
                a32::emit_a32_exclusive_write_memory_32(ctx, ra, inst_ref, inst)
            }
            Opcode::A32ExclusiveWriteMemory64 => {
                a32::emit_a32_exclusive_write_memory_64(ctx, ra, inst_ref, inst)
            }

            // --- A32 Coprocessor (stubs) ---
            Opcode::A32CoprocInternalOperation => {
                a32::emit_a32_coproc_internal_operation(ctx, ra, inst_ref, inst)
            }
            Opcode::A32CoprocSendOneWord => {
                a32::emit_a32_coproc_send_one_word(ctx, ra, inst_ref, inst)
            }
            Opcode::A32CoprocSendTwoWords => {
                a32::emit_a32_coproc_send_two_words(ctx, ra, inst_ref, inst)
            }
            Opcode::A32CoprocGetOneWord => {
                a32::emit_a32_coproc_get_one_word(ctx, ra, inst_ref, inst)
            }
            Opcode::A32CoprocGetTwoWords => {
                a32::emit_a32_coproc_get_two_words(ctx, ra, inst_ref, inst)
            }
            Opcode::A32CoprocLoadWords => a32::emit_a32_coproc_load_words(ctx, ra, inst_ref, inst),
            Opcode::A32CoprocStoreWords => {
                a32::emit_a32_coproc_store_words(ctx, ra, inst_ref, inst)
            }

            // --- Not yet implemented ---
            _ => {
                panic!("Opcode {:?} not handled in emission pipeline", inst.opcode);
            }
        }

        // Accumulate per-opcode emit time when the `profile_opcodes` feature
        // is enabled. Compiled out entirely in default builds.
        #[cfg(feature = "profile_opcodes")]
        if let Some(t_start) = _t_op_start {
            let ns = t_start.elapsed().as_nanos() as u64;
            crate::backend::x64::opcode_profile::record(_op_for_log, ns);
        }

        if ctx.arch.is_a32() {
            if let Some(target_pc) = crate::jit::a32_pc_trace_target() {
                let blk_pc = ctx.arch.extract_pc(ctx.location);
                if blk_pc == target_pc && crate::jit::a32_pc_trace_after_insts().contains(&i) {
                    emit_preserved_a32_pc_trace_hook(ra, i as u64);
                }
            }
        }

        // RUZU_PER_INST_XMM1_CHECK=0xPC[,...] — emit `ptest xmm1, xmm1; jz
        // +SKIP; ud2` after EACH IR instruction's emit, restricted to the
        // listed block PCs. Only checks AFTER the first VectorBroadcast64
        // ImmU64(0) has been emitted (since xmm1 is only known to be 0
        // after that point in the block). Raises SIGILL when xmm1 becomes
        // non-zero so we can bisect the corruption point.
        if !ctx.arch.is_a32() && bcast64_zero_seen {
            if let Ok(spec) = std::env::var("RUZU_PER_INST_XMM1_CHECK") {
                let block_pc = ctx.arch.extract_pc(ctx.location);
                let pcs: Vec<u64> = spec
                    .split(',')
                    .filter_map(|p| u64::from_str_radix(p.trim().trim_start_matches("0x"), 16).ok())
                    .collect();
                if pcs.contains(&block_pc) {
                    use rxbyak::XMM1;
                    ra.asm.ptest(XMM1, XMM1).unwrap();
                    ra.asm.db(0x74).unwrap(); // je +2
                    ra.asm.db(0x02).unwrap();
                    ra.asm.ud2().unwrap();
                }
            }
        }

        ra.end_of_alloc_scope();
    }

    // Subtract block cycle count from cycles_remaining (matching dynarmic's EmitAddCycles).
    // This decrements the tick budget so the dispatcher loop eventually returns to the host.
    if ctx.config.enable_cycle_counting && block.cycle_count > 0 {
        use crate::backend::x64::block_of_code::STACK_LAYOUT_RSP_OFFSET;
        use crate::backend::x64::stack_layout::StackLayout;
        use rxbyak::{qword_ptr, RegExp, RSP};
        let cycles_offset = STACK_LAYOUT_RSP_OFFSET + StackLayout::cycles_remaining_offset();
        ra.asm
            .sub(
                qword_ptr(RegExp::from(RSP) + cycles_offset as i32),
                block.cycle_count as i32,
            )
            .unwrap();
    }

    // Emit the block terminal (control flow exit)
    emit_terminal::emit_terminal(ctx, ra, &block.terminal);

    BlockDescriptor {
        entrypoint_offset: start,
        size: ra.asm.size() - start,
    }
}

fn a64_block_trace_range() -> Option<(u64, u64)> {
    use std::sync::OnceLock;

    static RANGE: OnceLock<Option<(u64, u64)>> = OnceLock::new();
    *RANGE.get_or_init(|| {
        let raw = std::env::var("RUZU_BLOCK_TRACE_PC").ok()?;
        let (lo, hi) = raw.split_once('-')?;
        let parse =
            |value: &str| u64::from_str_radix(value.trim().trim_start_matches("0x"), 16).ok();
        Some((parse(lo)?, parse(hi)?))
    })
}

fn a64_dump_mem_trace_pcs() -> &'static [u64] {
    use std::sync::OnceLock;

    static PCS: OnceLock<Vec<u64>> = OnceLock::new();
    PCS.get_or_init(|| {
        std::env::var("RUZU_DUMP_MEM_AT")
            .ok()
            .into_iter()
            .flat_map(|value| {
                value
                    .split(',')
                    .filter_map(|spec| {
                        let pc = spec.split(':').next()?;
                        u64::from_str_radix(pc.trim().trim_start_matches("0x"), 16).ok()
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    })
}

fn a64_block_entry_trace_hook_enabled(ctx: &EmitContext) -> bool {
    use std::sync::OnceLock;

    let block_pc = ctx.arch.extract_pc(ctx.location);
    if a64_block_trace_range().is_some_and(|(lo, hi)| block_pc >= lo && block_pc < hi)
        || a64_dump_mem_trace_pcs().contains(&block_pc)
    {
        return true;
    }

    static GLOBAL_HOOK_ENABLED: OnceLock<bool> = OnceLock::new();
    *GLOBAL_HOOK_ENABLED.get_or_init(|| {
        std::env::var_os("RUZU_BLOCK_TRACE_CALLER_AT").is_some()
            || std::env::var_os("RUZU_BLOCK_TRACE_BAD_X19_CALLER_AT").is_some()
            || std::env::var_os("RUZU_BLOCK_TRACE_BAD_X0_LIVE_LR_AT").is_some()
            || std::env::var_os("RUZU_BLOCK_TRACE_BAD_X1_LIVE_LR_AT").is_some()
            || std::env::var_os("RUZU_BLOCK_TRACE_LIVE_LR_AT").is_some()
            || std::env::var_os("RUZU_DUMP_VEC_AT").is_some()
            || std::env::var_os("RUZU_DUMP_STRING_AT").is_some()
            || std::env::var_os("RUZU_FIRST_PCS_PER_CORE").is_some()
    })
}

fn a64_bad_xreg_trap_for_block(ctx: &EmitContext) -> Option<usize> {
    let block_pc = ctx.arch.extract_pc(ctx.location);
    let raw = std::env::var("RUZU_TRAP_BAD_XREG_AT").ok()?;
    for spec in raw.split(',') {
        let Some((pc_raw, reg_raw)) = spec.split_once(':') else {
            continue;
        };
        let pc_raw = pc_raw.trim().trim_start_matches("0x");
        let Ok(pc) = u64::from_str_radix(pc_raw, 16) else {
            continue;
        };
        if pc != block_pc {
            continue;
        }
        let reg_raw = reg_raw
            .trim()
            .trim_start_matches('x')
            .trim_start_matches('X');
        let Ok(reg) = reg_raw.parse::<usize>() else {
            continue;
        };
        if reg < 31 {
            return Some(reg);
        }
    }
    None
}

fn emit_a64_bad_xreg_trap(ctx: &EmitContext, ra: &mut RegAlloc) {
    let Some(reg) = a64_bad_xreg_trap_for_block(ctx) else {
        return;
    };

    let ok = ra.asm.create_label();
    let reg_offset = A64JitState::reg_offset(reg) as i32;

    // Preserve the caller-save registers we use on the non-trap path.
    // On trap, rewrite [rsp+16] to the bad value so ruzu-cmd's SIGILL
    // handler reports it as `recovered_vaddr`.
    ra.asm.push(RAX).unwrap();
    ra.asm.push(R11).unwrap();
    ra.asm.pushf().unwrap();
    ra.asm
        .mov(RAX, qword_ptr(RegExp::from(R15) + reg_offset))
        .unwrap();
    ra.asm.mov(R11, RAX).unwrap();
    ra.asm.shr(R11, 40u8).unwrap();
    ra.asm.cmp(rxbyak::Reg::gpr32(11), 0x21i32).unwrap();
    ra.asm.jne(&ok, rxbyak::JmpType::Near).unwrap();
    ra.asm.mov(R11, RAX).unwrap();
    ra.asm.shr(R11, 32u8).unwrap();
    ra.asm.and_(rxbyak::Reg::gpr32(11), 0xFFi32).unwrap();
    ra.asm.cmp(rxbyak::Reg::gpr32(11), 0x01i32).unwrap();
    ra.asm.jne(&ok, rxbyak::JmpType::Near).unwrap();
    ra.asm.mov(qword_ptr(RegExp::from(RSP) + 16), RAX).unwrap();
    ra.asm.mov(R11, 0xCAFE_F00Du32 as i32).unwrap();
    ra.asm.ud2().unwrap();
    ra.asm.bind(&ok).unwrap();
    ra.asm.popf().unwrap();
    ra.asm.pop(R11).unwrap();
    ra.asm.pop(RAX).unwrap();
}

fn emit_preserved_a64_block_entry_trace_hook(ra: &mut RegAlloc) {
    // This hook is emitted outside the IR register allocator's HostCall path.
    // Preserve the full System V caller-save set so diagnostics cannot change
    // guest execution state.
    let caller_save_gprs: &[Reg] = &[RAX, RCX, RDX, RDI, RSI, R8, R9, R10, R11];
    for &reg in caller_save_gprs {
        ra.asm.push(reg).unwrap();
    }

    // JIT block entries run with RSP 16-byte aligned. Nine GPR pushes leave
    // RSP misaligned by 8, so add explicit padding before calling Rust.
    ra.asm.sub(RSP, 8i32).unwrap();

    const XMM_SAVE_BYTES: i32 = 16 * 16;
    ra.asm.sub(RSP, XMM_SAVE_BYTES).unwrap();
    let rsp = RegExp::from(RSP);
    for i in 0..16 {
        ra.asm
            .movups(
                xmmword_ptr(rsp.clone() + (i * 16) as i32),
                Reg::xmm(i as u8),
            )
            .unwrap();
    }

    ra.asm.mov(RDI, R15).unwrap();
    ra.asm
        .mov(RAX, crate::jit::a64_block_entry_trace_hook as usize as i64)
        .unwrap();
    ra.asm.call_reg(RAX).unwrap();

    let rsp = RegExp::from(RSP);
    for i in 0..16 {
        ra.asm
            .movups(
                Reg::xmm(i as u8),
                xmmword_ptr(rsp.clone() + (i * 16) as i32),
            )
            .unwrap();
    }
    ra.asm.add(RSP, XMM_SAVE_BYTES).unwrap();
    ra.asm.add(RSP, 8i32).unwrap();

    for &reg in caller_save_gprs.iter().rev() {
        ra.asm.pop(reg).unwrap();
    }
}

/// A32 per-PC GPR-capture hook. Same preservation pattern as the A64 block-entry
/// hook: save all SysV caller-save GPRs + XMMs, pass R15 (A32JitState ptr) in RDI,
/// call the Rust aggregator, restore. Zero per-read cost (emitted only for the
/// configured target PC's block). Enabled via RUZU_A32_PC_TRACE=0xPC.
fn emit_preserved_a32_pc_trace_hook(ra: &mut RegAlloc, tag: u64) {
    let caller_save_gprs: &[Reg] = &[RAX, RCX, RDX, RDI, RSI, R8, R9, R10, R11];
    for &reg in caller_save_gprs {
        ra.asm.push(reg).unwrap();
    }
    ra.asm.sub(RSP, 8i32).unwrap();

    const XMM_SAVE_BYTES: i32 = 16 * 16;
    ra.asm.sub(RSP, XMM_SAVE_BYTES).unwrap();
    let rsp = RegExp::from(RSP);
    for i in 0..16 {
        ra.asm
            .movups(
                xmmword_ptr(rsp.clone() + (i * 16) as i32),
                Reg::xmm(i as u8),
            )
            .unwrap();
    }

    // A32 JIT runs with R15 = A32JitState pointer (GPRs at offset 0) and
    // R13 = fastmem arena base. Pass both so the hook can read guest memory.
    ra.asm.mov(RDI, R15).unwrap();
    ra.asm.mov(RSI, rxbyak::R13).unwrap();
    ra.asm.mov(RDX, tag as i64).unwrap();
    ra.asm
        .mov(RAX, crate::jit::a32_pc_trace_hook as usize as i64)
        .unwrap();
    ra.asm.call_reg(RAX).unwrap();

    let rsp = RegExp::from(RSP);
    for i in 0..16 {
        ra.asm
            .movups(
                Reg::xmm(i as u8),
                xmmword_ptr(rsp.clone() + (i * 16) as i32),
            )
            .unwrap();
    }
    ra.asm.add(RSP, XMM_SAVE_BYTES).unwrap();
    ra.asm.add(RSP, 8i32).unwrap();

    for &reg in caller_save_gprs.iter().rev() {
        ra.asm.pop(reg).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emit_block_exists() {
        // Type-check that emit_block has the right signature
        let _: fn(&EmitContext, &mut RegAlloc, &Block) -> BlockDescriptor = emit_block;
    }
}

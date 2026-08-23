use crate::backend::arm64::abi::{XFASTMEM, XSTATE};
use crate::backend::arm64::abi::{XSCRATCH0, XSCRATCH1, XSCRATCH2, XTICKS};
use crate::backend::arm64::emit_arm64_a32::{
    emit_a32_bx_write_pc, emit_a32_call_supervisor, emit_a32_check_memory_abort, emit_a32_cond,
    emit_a32_condition_failed_terminal, emit_a32_data_memory_barrier,
    emit_a32_data_synchronization_barrier, emit_a32_exception_raised, emit_a32_get_c_flag,
    emit_a32_get_cpsr, emit_a32_get_extended_register32, emit_a32_get_extended_register64,
    emit_a32_get_fpscr, emit_a32_get_fpscr_nzcv, emit_a32_get_ge_flags, emit_a32_get_register,
    emit_a32_get_vector, emit_a32_instruction_synchronization_barrier, emit_a32_or_q_flag,
    emit_a32_set_check_bit, emit_a32_set_cpsr, emit_a32_set_cpsr_nz, emit_a32_set_cpsr_nzc,
    emit_a32_set_cpsr_nzcv, emit_a32_set_cpsr_nzcv_raw, emit_a32_set_cpsr_nzcvq,
    emit_a32_set_extended_register32, emit_a32_set_extended_register64, emit_a32_set_fpscr,
    emit_a32_set_fpscr_nzcv, emit_a32_set_ge_flags, emit_a32_set_ge_flags_compressed,
    emit_a32_set_register, emit_a32_set_vector, emit_a32_terminal,
    emit_a32_update_upper_location_descriptor,
};
use crate::backend::arm64::emit_arm64_a32_coprocessor::{
    emit_a32_coproc_get_one_word, emit_a32_coproc_get_two_words,
    emit_a32_coproc_internal_operation, emit_a32_coproc_load_words, emit_a32_coproc_send_one_word,
    emit_a32_coproc_send_two_words, emit_a32_coproc_store_words,
};
use crate::backend::arm64::emit_arm64_a32_memory::{
    emit_a32_clear_exclusive, emit_a32_exclusive_read_memory, emit_a32_exclusive_write_memory,
    emit_a32_read_memory, emit_a32_write_memory,
};
use crate::backend::arm64::emit_arm64_a64::{
    emit_a64_call_supervisor, emit_a64_check_memory_abort, emit_a64_cond,
    emit_a64_condition_failed_terminal, emit_a64_data_cache_operation_raised,
    emit_a64_exception_raised, emit_a64_instruction_cache_operation_raised, emit_a64_terminal,
};
use crate::backend::arm64::emit_arm64_a64_memory::{
    emit_a64_clear_exclusive, emit_a64_exclusive_read_memory, emit_a64_exclusive_write_memory,
    emit_a64_read_memory, emit_a64_write_memory,
};
use crate::backend::arm64::emit_arm64_cryptography::{
    emit_aes_decrypt_single_round, emit_aes_encrypt_single_round, emit_aes_inverse_mix_columns,
    emit_aes_mix_columns, emit_crc32_castagnoli_16, emit_crc32_castagnoli_32,
    emit_crc32_castagnoli_64, emit_crc32_castagnoli_8, emit_crc32_iso_16, emit_crc32_iso_32,
    emit_crc32_iso_64, emit_crc32_iso_8, emit_sha256_hash, emit_sha256_message_schedule_0,
    emit_sha256_message_schedule_1,
};
use crate::backend::arm64::emit_arm64_data_processing::{
    emit_add32, emit_add64, emit_and32, emit_and64, emit_and_not32, emit_and_not64,
    emit_arithmetic_shift_right32, emit_arithmetic_shift_right64,
    emit_arithmetic_shift_right_masked32, emit_arithmetic_shift_right_masked64,
    emit_byte_reverse_dual, emit_byte_reverse_half, emit_byte_reverse_word,
    emit_conditional_select32, emit_conditional_select64, emit_count_leading_zeros32,
    emit_count_leading_zeros64, emit_eor32, emit_eor64, emit_extract_register32,
    emit_extract_register64, emit_get_nzcv_from_op, emit_least_significant_byte,
    emit_least_significant_half, emit_least_significant_word, emit_logical_shift_left32,
    emit_logical_shift_left64, emit_logical_shift_left_masked32, emit_logical_shift_left_masked64,
    emit_logical_shift_right32, emit_logical_shift_right64, emit_logical_shift_right_masked32,
    emit_logical_shift_right_masked64, emit_max_signed32, emit_max_signed64, emit_max_unsigned32,
    emit_max_unsigned64, emit_min_signed32, emit_min_signed64, emit_min_unsigned32,
    emit_min_unsigned64, emit_most_significant_word, emit_mul32, emit_mul64, emit_not32,
    emit_not64, emit_or32, emit_or64, emit_pack_2x32_to_1x64, emit_pack_2x64_to_1x128,
    emit_replicate_bit32, emit_replicate_bit64, emit_rotate_right32, emit_rotate_right64,
    emit_rotate_right_extended, emit_rotate_right_masked32, emit_rotate_right_masked64,
    emit_sign_extend_byte_to_long, emit_sign_extend_byte_to_word, emit_sign_extend_half_to_long,
    emit_sign_extend_half_to_word, emit_sign_extend_word_to_long, emit_signed_div32,
    emit_signed_div64, emit_signed_multiply_high64, emit_sub32, emit_sub64, emit_test_bit,
    emit_unsigned_div32, emit_unsigned_div64, emit_unsigned_multiply_high64, emit_zero_extend,
    emit_zero_extend_long_to_quad,
};
use crate::backend::arm64::emit_arm64_floating_point::{
    emit_fp_abs32, emit_fp_abs64, emit_fp_add32, emit_fp_add64, emit_fp_compare32,
    emit_fp_compare64, emit_fp_div32, emit_fp_div64, emit_fp_double_to_fixed_s16,
    emit_fp_double_to_fixed_s32, emit_fp_double_to_fixed_s64, emit_fp_double_to_fixed_u16,
    emit_fp_double_to_fixed_u32, emit_fp_double_to_fixed_u64, emit_fp_double_to_half,
    emit_fp_double_to_single, emit_fp_fixed_s16_to_double, emit_fp_fixed_s16_to_single,
    emit_fp_fixed_s32_to_double, emit_fp_fixed_s32_to_single, emit_fp_fixed_s64_to_double,
    emit_fp_fixed_s64_to_single, emit_fp_fixed_u16_to_double, emit_fp_fixed_u16_to_single,
    emit_fp_fixed_u32_to_double, emit_fp_fixed_u32_to_single, emit_fp_fixed_u64_to_double,
    emit_fp_fixed_u64_to_single, emit_fp_half_to_double, emit_fp_half_to_single, emit_fp_max32,
    emit_fp_max64, emit_fp_max_numeric32, emit_fp_max_numeric64, emit_fp_min32, emit_fp_min64,
    emit_fp_min_numeric32, emit_fp_min_numeric64, emit_fp_mul32, emit_fp_mul64, emit_fp_mul_add32,
    emit_fp_mul_add64, emit_fp_mul_sub32, emit_fp_mul_sub64, emit_fp_mul_x32, emit_fp_mul_x64,
    emit_fp_neg32, emit_fp_neg64, emit_fp_recip_estimate32, emit_fp_recip_estimate64,
    emit_fp_recip_exponent32, emit_fp_recip_exponent64, emit_fp_recip_step_fused32,
    emit_fp_recip_step_fused64, emit_fp_round_int32, emit_fp_round_int64, emit_fp_rsqrt_estimate32,
    emit_fp_rsqrt_estimate64, emit_fp_rsqrt_step_fused32, emit_fp_rsqrt_step_fused64,
    emit_fp_single_to_double, emit_fp_single_to_fixed_s16, emit_fp_single_to_fixed_s32,
    emit_fp_single_to_fixed_s64, emit_fp_single_to_fixed_u16, emit_fp_single_to_fixed_u32,
    emit_fp_single_to_fixed_u64, emit_fp_single_to_half, emit_fp_sqrt32, emit_fp_sqrt64,
    emit_fp_sub32, emit_fp_sub64,
};
use crate::backend::arm64::emit_arm64_packed::emit_packed_instruction;
use crate::backend::arm64::emit_arm64_saturation::{
    emit_signed_saturated_add_with_flag32, emit_signed_saturated_sub_with_flag32,
    emit_signed_saturation, emit_unsigned_saturation,
};
use crate::backend::arm64::emit_arm64_vector::emit_vector_instruction;
use crate::backend::arm64::emit_arm64_vector_floating_point::emit_fp_vector_instruction;
use crate::backend::arm64::emit_arm64_vector_saturation::emit_vector_saturation_instruction;
use crate::backend::arm64::fast_hash::FastHashMap;
use crate::backend::arm64::fastmem::FastmemManager;
use crate::backend::arm64::fastmem::FastmemPatchInfo;
use crate::backend::arm64::fpsr_manager::FpsrManager;
use crate::backend::arm64::jit_state::{A32JitState, A64JitState};
use crate::backend::arm64::label::Label;
use crate::backend::arm64::reg_alloc::{Argument, HostLoc, HostLocKind, RegAlloc};
use crate::backend::arm64::stack_layout::{RSBEntry, StackLayout, RSB_INDEX_MASK};
use crate::backend::arm64::{
    block_of_code::BlockOfCode,
    emit_context::{DescriptorToFpcr, EmitContext, Fpcr},
    inst,
};
use crate::backend::common::emit_context::MemoryEmitConfig;
use crate::ir::block::Block;
use crate::ir::cond::Cond;
use crate::ir::location::{A32LocationDescriptor, A64LocationDescriptor, LocationDescriptor};
use crate::ir::opcode::Opcode;
use crate::ir::value::InstRef;
use crate::jit_config::{JitConfig, OptimizationFlag};

pub type CodePtr = *const u8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    ReturnToDispatcher,
    ReturnFromRunCode,
    ReadMemory8,
    ReadMemory16,
    ReadMemory32,
    ReadMemory64,
    ReadMemory128,
    WrappedReadMemory8,
    WrappedReadMemory16,
    WrappedReadMemory32,
    WrappedReadMemory64,
    WrappedReadMemory128,
    ExclusiveReadMemory8,
    ExclusiveReadMemory16,
    ExclusiveReadMemory32,
    ExclusiveReadMemory64,
    ExclusiveReadMemory128,
    WriteMemory8,
    WriteMemory16,
    WriteMemory32,
    WriteMemory64,
    WriteMemory128,
    WrappedWriteMemory8,
    WrappedWriteMemory16,
    WrappedWriteMemory32,
    WrappedWriteMemory64,
    WrappedWriteMemory128,
    ExclusiveWriteMemory8,
    ExclusiveWriteMemory16,
    ExclusiveWriteMemory32,
    ExclusiveWriteMemory64,
    ExclusiveWriteMemory128,
    CallSVC,
    InterpreterFallback,
    ExceptionRaised,
    InstructionSynchronizationBarrierRaised,
    InstructionCacheOperationRaised,
    DataCacheOperationRaised,
    GetCNTPCT,
    AddTicks,
    GetTicksRemaining,
}

impl LinkTarget {
    pub(crate) fn is_bl_target(self) -> bool {
        !matches!(
            self,
            LinkTarget::ReturnToDispatcher | LinkTarget::ReturnFromRunCode
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relocation {
    pub code_offset: isize,
    pub target: LinkTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockRelocationType {
    Branch,
    MoveToScratch1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockRelocation {
    pub code_offset: isize,
    pub relocation_type: BlockRelocationType,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmittedBlockInfo {
    pub entry_point: CodePtr,
    pub size: usize,
    pub relocations: Vec<Relocation>,
    pub block_relocations: FastHashMap<LocationDescriptor, Vec<BlockRelocation>>,
    pub fastmem_patch_info: FastHashMap<isize, FastmemPatchInfo>,
}

pub type EmitTerminal = for<'a> fn(&mut BlockOfCode, &mut EmitContext<'a>) -> Result<(), String>;
pub type EmitCond =
    for<'a> fn(&mut BlockOfCode, &mut EmitContext<'a>, Cond) -> Result<usize, String>;
pub type EmitCheckMemoryAbort = for<'a> fn(
    &mut BlockOfCode,
    &mut EmitContext<'a>,
    LocationDescriptor,
    &mut Label,
) -> Result<(), String>;

/// ARM64 emission configuration.
///
/// Upstream owner: `backend/arm64/emit_arm64.h::EmitConfig`.
/// This is intentionally backend-local; it should grow with the ARM64 emitter
/// instead of reusing the x64 `EmitConfig` shape.
#[derive(Clone)]
pub struct EmitConfig {
    pub coprocessors: crate::interface::a32::config::Coprocessors,
    pub is_a32: bool,
    pub optimizations: OptimizationFlag,
    pub hook_isb: bool,
    pub cntfreq_el0: u64,
    pub ctr_el0: u32,
    pub dczid_el0: u32,
    pub tpidrro_el0: *const u64,
    pub tpidr_el0: *mut u64,
    pub check_halt_on_memory_access: bool,
    pub memory: MemoryEmitConfig,
    pub fastmem_pointer: u64,
    pub recompile_on_fastmem_failure: bool,
    pub fastmem_address_space_bits: usize,
    pub silently_mirror_fastmem: bool,
    pub page_table_pointer: u64,
    pub page_table_address_space_bits: usize,
    pub page_table_pointer_mask_bits: u32,
    pub silently_mirror_page_table: bool,
    pub absolute_offset_page_table: bool,
    pub detect_misaligned_access_via_page_table: u32,
    pub only_detect_misalignment_via_page_table_on_page_boundary: bool,
    pub wall_clock_cntpct: bool,
    pub enable_cycle_counting: bool,
    pub always_little_endian: bool,
    pub descriptor_to_fpcr: DescriptorToFpcr,
    pub emit_cond: EmitCond,
    pub emit_condition_failed_terminal: EmitTerminal,
    pub emit_terminal: EmitTerminal,
    pub emit_check_memory_abort: EmitCheckMemoryAbort,
    pub state_nzcv_offset: usize,
    pub state_fpsr_offset: usize,
    pub state_exclusive_state_offset: usize,
}

impl EmitConfig {
    pub fn has_optimization(&self, flag: OptimizationFlag) -> bool {
        (self.optimizations & flag) != OptimizationFlag::NO_OPTIMIZATIONS
    }

    pub fn from_a32_config(config: &JitConfig) -> Self {
        let mut memory = config.memory.clone();
        memory.processor_id = config.processor_id;
        memory.fastmem_address_space_bits = 32;
        memory.silently_mirror_fastmem = true;
        memory.page_table_address_space_bits = 32;
        memory.silently_mirror_page_table = true;
        memory.fastmem_exclusive_access =
            config.fastmem_pointer.is_some() && config.global_monitor.is_some();

        Self {
            coprocessors: config.coprocessors.clone(),
            is_a32: true,
            optimizations: effective_optimizations(config),
            hook_isb: config.hook_isb,
            cntfreq_el0: 0,
            ctr_el0: 0,
            dczid_el0: 0,
            tpidrro_el0: core::ptr::null(),
            tpidr_el0: core::ptr::null_mut(),
            check_halt_on_memory_access: memory.check_halt_on_memory_access,
            fastmem_pointer: config.fastmem_pointer.map_or(0, |p| p as u64),
            recompile_on_fastmem_failure: memory.recompile_on_fastmem_failure,
            fastmem_address_space_bits: 32,
            silently_mirror_fastmem: true,
            page_table_pointer: config.page_table_pointer.map_or(0, |p| p as u64),
            page_table_address_space_bits: 32,
            page_table_pointer_mask_bits: memory.page_table_pointer_mask_bits,
            silently_mirror_page_table: true,
            absolute_offset_page_table: memory.absolute_offset_page_table,
            detect_misaligned_access_via_page_table: memory.detect_misaligned_access_via_page_table,
            only_detect_misalignment_via_page_table_on_page_boundary: memory
                .only_detect_misalignment_via_page_table_on_page_boundary,
            memory,
            wall_clock_cntpct: config.wall_clock_cntpct,
            enable_cycle_counting: config.enable_cycle_counting,
            always_little_endian: true,
            descriptor_to_fpcr: descriptor_to_a32_fpcr,
            emit_cond: emit_a32_cond,
            emit_condition_failed_terminal: emit_a32_condition_failed_terminal,
            emit_terminal: emit_a32_terminal,
            emit_check_memory_abort: emit_a32_check_memory_abort,
            state_nzcv_offset: core::mem::offset_of!(A32JitState, cpsr_nzcv),
            state_fpsr_offset: core::mem::offset_of!(A32JitState, fpsr),
            state_exclusive_state_offset: core::mem::offset_of!(A32JitState, exclusive_state),
        }
    }

    pub fn from_a64_config(config: &JitConfig) -> Self {
        let mut memory = config.memory.clone();
        memory.processor_id = config.processor_id;

        Self {
            coprocessors: crate::interface::a32::config::empty_coprocessors(),
            is_a32: false,
            optimizations: effective_optimizations(config),
            hook_isb: config.hook_isb,
            // Upstream A64::UserConfig::cntfrq_el0 — forwarded from the
            // emulator (yuzu sets the Switch's 19'200'000 Hz; the dynarmic
            // default of 600'000'000 only applies when left unconfigured).
            cntfreq_el0: config.cntfrq_el0 as u64,
            ctr_el0: config.ctr_el0,
            dczid_el0: config.dczid_el0,
            tpidrro_el0: config.tpidrro_el0.unwrap_or(core::ptr::null()),
            tpidr_el0: config.tpidr_el0.unwrap_or(core::ptr::null_mut()),
            check_halt_on_memory_access: memory.check_halt_on_memory_access,
            fastmem_pointer: config.fastmem_pointer.map_or(0, |p| p as u64),
            recompile_on_fastmem_failure: memory.recompile_on_fastmem_failure,
            fastmem_address_space_bits: memory.fastmem_address_space_bits,
            silently_mirror_fastmem: memory.silently_mirror_fastmem,
            page_table_pointer: config.page_table_pointer.map_or(0, |p| p as u64),
            page_table_address_space_bits: memory.page_table_address_space_bits,
            page_table_pointer_mask_bits: memory.page_table_pointer_mask_bits,
            silently_mirror_page_table: memory.silently_mirror_page_table,
            absolute_offset_page_table: memory.absolute_offset_page_table,
            detect_misaligned_access_via_page_table: memory.detect_misaligned_access_via_page_table,
            only_detect_misalignment_via_page_table_on_page_boundary: memory
                .only_detect_misalignment_via_page_table_on_page_boundary,
            memory,
            wall_clock_cntpct: config.wall_clock_cntpct,
            enable_cycle_counting: config.enable_cycle_counting,
            always_little_endian: true,
            descriptor_to_fpcr: descriptor_to_a64_fpcr,
            emit_cond: emit_a64_cond,
            emit_condition_failed_terminal: emit_a64_condition_failed_terminal,
            emit_terminal: emit_a64_terminal,
            emit_check_memory_abort: emit_a64_check_memory_abort,
            state_nzcv_offset: core::mem::offset_of!(A64JitState, cpsr_nzcv),
            state_fpsr_offset: core::mem::offset_of!(A64JitState, fpsr),
            state_exclusive_state_offset: core::mem::offset_of!(A64JitState, exclusive_state),
        }
    }
}

fn effective_optimizations(config: &JitConfig) -> OptimizationFlag {
    if config.unsafe_optimizations {
        config.optimizations
    } else {
        config.optimizations & OptimizationFlag::ALL_SAFE_OPTIMIZATIONS
    }
}

fn descriptor_to_a32_fpcr(descriptor: LocationDescriptor) -> Fpcr {
    Fpcr::new(
        A32LocationDescriptor::from_location(descriptor)
            .fpscr()
            .value(),
    )
}

fn descriptor_to_a64_fpcr(descriptor: LocationDescriptor) -> Fpcr {
    Fpcr::new(A64LocationDescriptor::from_location(descriptor).fpcr())
}

/// Upstream owner: `backend/arm64/emit_arm64.cpp::EmitArm64`.
///
/// This is a strict backend boundary, not a fake interpreter fallback. The
/// caller has already performed the upstream `GetOrEmit` miss path up through
/// IR generation and emit-config construction; real AArch64 machine-code
/// emission is still missing.
pub fn emit_arm64(
    code: &mut BlockOfCode,
    mut block: Block,
    config: EmitConfig,
) -> Result<EmittedBlockInfo, String> {
    let mut emitted_block_info = EmittedBlockInfo {
        entry_point: unsafe { code.code_base_ptr().add(code.code_size()) },
        size: 0,
        relocations: Vec::new(),
        block_relocations: FastHashMap::default(),
        fastmem_patch_info: FastHashMap::default(),
    };
    let mut fpsr_manager = FpsrManager::new(config.state_fpsr_offset);
    let mut reg_alloc = RegAlloc::default();
    let mut fastmem_manager = FastmemManager::default();
    emit_a32_block_prologue_counter_if_enabled(code, &block, &config)?;
    {
        let mut ctx = EmitContext {
            block: &mut block,
            reg_alloc: &mut reg_alloc,
            conf: &config,
            emitted_block_info: &mut emitted_block_info,
            fpsr: &mut fpsr_manager,
            fastmem: &mut fastmem_manager,
            deferred_emits: Vec::new(),
        };

        if let Some(cond) = ctx.block.cond {
            if !ctx.block.has_condition_failed_location() {
                return Err("ARM64 conditional block without condition-failed location".to_string());
            }

            let pass_branch_offset = (ctx.conf.emit_cond)(code, &mut ctx, cond)?;
            emit_add_cycles(code, &ctx, ctx.block.condition_failed_cycle_count)?;
            (ctx.conf.emit_condition_failed_terminal)(code, &mut ctx)?;
            patch_conditional_branch_to_current(code, pass_branch_offset, cond)?;
        } else if ctx.block.has_condition_failed_location() {
            return Err("ARM64 condition-failed terminal without condition".to_string());
        }

        for index in 0..ctx.block.instructions.len() {
            emit_ir_instruction(code, &mut ctx, InstRef(index as u32))?;
            ctx.reg_alloc.update_all_uses();
            ctx.reg_alloc.assert_all_unlocked();
        }

        ctx.fpsr.spill(code)?;
        ctx.reg_alloc.assert_no_more_uses(ctx.block);
        emit_add_cycles(code, &ctx, ctx.block.cycle_count)?;
        (ctx.conf.emit_terminal)(code, &mut ctx)?;
        code.write_u32(inst::brk(0))?;

        let mut deferred_emits = std::mem::take(&mut ctx.deferred_emits);
        for deferred_emit in &mut deferred_emits {
            deferred_emit()?;
        }
        code.write_u32(inst::brk(0))?;

        ctx.emitted_block_info.size = code.code_size()
            - (ctx.emitted_block_info.entry_point as usize - code.code_base_ptr() as usize);
    }
    Ok(emitted_block_info)
}

fn emit_a32_block_prologue_counter_if_enabled(
    code: &mut BlockOfCode,
    block: &Block,
    config: &EmitConfig,
) -> Result<(), String> {
    if !config.always_little_endian {
        return Ok(());
    }
    if !config.is_a32 {
        return Ok(());
    }

    let pc = A32LocationDescriptor::from_location(block.location).pc();
    let idx = config.memory.processor_id.min(15);

    if let Some((lo, hi)) = crate::jit::block_prologue_count_range() {
        if pc >= lo && pc < hi {
            let counter = &crate::jit::block_prologue_counters()[idx] as *const _ as u64;
            emit_increment_u64_counter(code, counter)?;
        }
    }

    if let Some(counter) = crate::jit::block_prologue_top_counter(pc, idx) {
        emit_increment_u64_counter(code, counter)?;
    }

    Ok(())
}

fn emit_increment_u64_counter(code: &mut BlockOfCode, counter: u64) -> Result<(), String> {
    emit_mov_x_imm(code, XSCRATCH0, counter)?;
    code.write_u32(inst::ldr_x_unsigned(XSCRATCH1, XSCRATCH0, 0))?;
    code.write_u32(inst::add_x_imm(XSCRATCH1, XSCRATCH1, 1))?;
    code.write_u32(inst::str_x_unsigned(XSCRATCH1, XSCRATCH0, 0))?;
    Ok(())
}

fn patch_conditional_branch_to_current(
    code: &mut BlockOfCode,
    branch_offset: usize,
    cond: Cond,
) -> Result<(), String> {
    let target_offset = code.code_size();
    let pc_offset = i32::try_from(target_offset as isize - branch_offset as isize)
        .map_err(|_| "ARM64 conditional block branch offset overflow".to_string())?;
    code.patch_u32(branch_offset, inst::b_cond(cond, pc_offset))
}

fn emit_is_zero32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_w(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.reg_alloc.spill_flags(code)?;

    let result = result.index().expect("realized W result") as u8;
    let operand = operand.index().expect("realized W operand") as u8;
    code.write_u32(inst::cmp_w_imm(operand, 0))?;
    code.write_u32(inst::cinc_w(result, 31, Cond::EQ))?;
    Ok(())
}

fn emit_is_zero64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_x(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.reg_alloc.spill_flags(code)?;

    let result = result.index().expect("realized W result") as u8;
    let operand = operand.index().expect("realized X operand") as u8;
    code.write_u32(inst::cmp_x_imm(operand, 0))?;
    code.write_u32(inst::cinc_w(result, 31, Cond::EQ))?;
    Ok(())
}

fn emit_ir_instruction(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    match ctx.block.get(inst_ref).opcode {
        Opcode::Void => Ok(()),
        Opcode::Identity => {
            let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
            ctx.reg_alloc
                .define_as_existing(ctx.block, inst_ref, args[0]);
            Ok(())
        }
        Opcode::GetCarryFromOp | Opcode::GetOverflowFromOp | Opcode::GetGEFromOp => {
            let _args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
            if !ctx.reg_alloc.was_value_defined(inst_ref) {
                return Err(format!(
                    "ARM64 {:?} reached emitter before producer defined it",
                    ctx.block.get(inst_ref).opcode
                ));
            }
            Ok(())
        }
        Opcode::GetUpperFromOp | Opcode::GetLowerFromOp => {
            let _args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
            if !ctx.reg_alloc.was_value_defined(inst_ref) {
                return Err(format!(
                    "ARM64 {:?} reached emitter before producer defined it",
                    ctx.block.get(inst_ref).opcode
                ));
            }
            Ok(())
        }
        Opcode::GetNZCVFromOp | Opcode::GetNZFromOp => emit_get_nzcv_from_op(code, ctx, inst_ref),
        Opcode::GetCFlagFromNZCV => emit_get_c_flag_from_nzcv(code, ctx, inst_ref),
        Opcode::IsZero32 => emit_is_zero32(code, ctx, inst_ref),
        Opcode::IsZero64 => emit_is_zero64(code, ctx, inst_ref),
        Opcode::NZCVFromPackedFlags => {
            let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
            ctx.reg_alloc
                .define_as_existing(ctx.block, inst_ref, args[0]);
            Ok(())
        }
        Opcode::Breakpoint => {
            code.write_u32(inst::brk(0))?;
            Ok(())
        }
        Opcode::CallHostFunction => {
            let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
            ctx.reg_alloc.prepare_for_call(
                code,
                ctx.fpsr,
                [
                    optional_argument(args[1]),
                    optional_argument(args[2]),
                    optional_argument(args[3]),
                    None,
                ],
            )?;
            emit_mov_x_imm(code, XSCRATCH0, args[0].get_immediate_u64())?;
            code.write_u32(inst::blr(XSCRATCH0))?;
            Ok(())
        }
        Opcode::PushRSB => emit_push_rsb(code, ctx, inst_ref),
        Opcode::Pack2x32To1x64 => emit_pack_2x32_to_1x64(code, ctx, inst_ref),
        Opcode::Pack2x64To1x128 => emit_pack_2x64_to_1x128(code, ctx, inst_ref),
        Opcode::ExtractRegister32 => emit_extract_register32(code, ctx, inst_ref),
        Opcode::ExtractRegister64 => emit_extract_register64(code, ctx, inst_ref),
        Opcode::LeastSignificantWord => emit_least_significant_word(code, ctx, inst_ref),
        Opcode::MostSignificantWord => emit_most_significant_word(code, ctx, inst_ref),
        Opcode::LeastSignificantHalf => emit_least_significant_half(code, ctx, inst_ref),
        Opcode::LeastSignificantByte => emit_least_significant_byte(code, ctx, inst_ref),
        Opcode::TestBit => emit_test_bit(code, ctx, inst_ref),
        Opcode::And32 => emit_and32(code, ctx, inst_ref),
        Opcode::And64 => emit_and64(code, ctx, inst_ref),
        Opcode::AndNot32 => emit_and_not32(code, ctx, inst_ref),
        Opcode::AndNot64 => emit_and_not64(code, ctx, inst_ref),
        Opcode::Eor32 => emit_eor32(code, ctx, inst_ref),
        Opcode::Eor64 => emit_eor64(code, ctx, inst_ref),
        Opcode::Or32 => emit_or32(code, ctx, inst_ref),
        Opcode::Or64 => emit_or64(code, ctx, inst_ref),
        Opcode::Not32 => emit_not32(code, ctx, inst_ref),
        Opcode::Not64 => emit_not64(code, ctx, inst_ref),
        Opcode::Add32 => emit_add32(code, ctx, inst_ref),
        Opcode::Add64 => emit_add64(code, ctx, inst_ref),
        Opcode::Sub32 => emit_sub32(code, ctx, inst_ref),
        Opcode::Sub64 => emit_sub64(code, ctx, inst_ref),
        Opcode::Mul32 => emit_mul32(code, ctx, inst_ref),
        Opcode::Mul64 => emit_mul64(code, ctx, inst_ref),
        Opcode::SignedMultiplyHigh64 => emit_signed_multiply_high64(code, ctx, inst_ref),
        Opcode::UnsignedMultiplyHigh64 => emit_unsigned_multiply_high64(code, ctx, inst_ref),
        Opcode::UnsignedDiv32 => emit_unsigned_div32(code, ctx, inst_ref),
        Opcode::UnsignedDiv64 => emit_unsigned_div64(code, ctx, inst_ref),
        Opcode::SignedDiv32 => emit_signed_div32(code, ctx, inst_ref),
        Opcode::SignedDiv64 => emit_signed_div64(code, ctx, inst_ref),
        Opcode::MaxSigned32 => emit_max_signed32(code, ctx, inst_ref),
        Opcode::MaxSigned64 => emit_max_signed64(code, ctx, inst_ref),
        Opcode::MaxUnsigned32 => emit_max_unsigned32(code, ctx, inst_ref),
        Opcode::MaxUnsigned64 => emit_max_unsigned64(code, ctx, inst_ref),
        Opcode::MinSigned32 => emit_min_signed32(code, ctx, inst_ref),
        Opcode::MinSigned64 => emit_min_signed64(code, ctx, inst_ref),
        Opcode::MinUnsigned32 => emit_min_unsigned32(code, ctx, inst_ref),
        Opcode::MinUnsigned64 => emit_min_unsigned64(code, ctx, inst_ref),
        Opcode::FPAbs32 => emit_fp_abs32(code, ctx, inst_ref),
        Opcode::FPAbs64 => emit_fp_abs64(code, ctx, inst_ref),
        Opcode::FPAdd32 => emit_fp_add32(code, ctx, inst_ref),
        Opcode::FPAdd64 => emit_fp_add64(code, ctx, inst_ref),
        Opcode::FPCompare32 => emit_fp_compare32(code, ctx, inst_ref),
        Opcode::FPCompare64 => emit_fp_compare64(code, ctx, inst_ref),
        Opcode::FPDiv32 => emit_fp_div32(code, ctx, inst_ref),
        Opcode::FPDiv64 => emit_fp_div64(code, ctx, inst_ref),
        Opcode::FPMax32 => emit_fp_max32(code, ctx, inst_ref),
        Opcode::FPMax64 => emit_fp_max64(code, ctx, inst_ref),
        Opcode::FPMaxNumeric32 => emit_fp_max_numeric32(code, ctx, inst_ref),
        Opcode::FPMaxNumeric64 => emit_fp_max_numeric64(code, ctx, inst_ref),
        Opcode::FPMul32 => emit_fp_mul32(code, ctx, inst_ref),
        Opcode::FPMul64 => emit_fp_mul64(code, ctx, inst_ref),
        Opcode::FPMulX32 => emit_fp_mul_x32(code, ctx, inst_ref),
        Opcode::FPMulX64 => emit_fp_mul_x64(code, ctx, inst_ref),
        Opcode::FPMulAdd32 => emit_fp_mul_add32(code, ctx, inst_ref),
        Opcode::FPMulAdd64 => emit_fp_mul_add64(code, ctx, inst_ref),
        Opcode::FPMulSub32 => emit_fp_mul_sub32(code, ctx, inst_ref),
        Opcode::FPMulSub64 => emit_fp_mul_sub64(code, ctx, inst_ref),
        Opcode::FPMin32 => emit_fp_min32(code, ctx, inst_ref),
        Opcode::FPMin64 => emit_fp_min64(code, ctx, inst_ref),
        Opcode::FPMinNumeric32 => emit_fp_min_numeric32(code, ctx, inst_ref),
        Opcode::FPMinNumeric64 => emit_fp_min_numeric64(code, ctx, inst_ref),
        Opcode::FPNeg32 => emit_fp_neg32(code, ctx, inst_ref),
        Opcode::FPNeg64 => emit_fp_neg64(code, ctx, inst_ref),
        Opcode::FPRecipEstimate32 => emit_fp_recip_estimate32(code, ctx, inst_ref),
        Opcode::FPRecipEstimate64 => emit_fp_recip_estimate64(code, ctx, inst_ref),
        Opcode::FPRecipExponent32 => emit_fp_recip_exponent32(code, ctx, inst_ref),
        Opcode::FPRecipExponent64 => emit_fp_recip_exponent64(code, ctx, inst_ref),
        Opcode::FPRecipStepFused32 => emit_fp_recip_step_fused32(code, ctx, inst_ref),
        Opcode::FPRecipStepFused64 => emit_fp_recip_step_fused64(code, ctx, inst_ref),
        Opcode::FPRSqrtEstimate32 => emit_fp_rsqrt_estimate32(code, ctx, inst_ref),
        Opcode::FPRSqrtEstimate64 => emit_fp_rsqrt_estimate64(code, ctx, inst_ref),
        Opcode::FPRSqrtStepFused32 => emit_fp_rsqrt_step_fused32(code, ctx, inst_ref),
        Opcode::FPRSqrtStepFused64 => emit_fp_rsqrt_step_fused64(code, ctx, inst_ref),
        Opcode::FPRoundInt32 => emit_fp_round_int32(code, ctx, inst_ref),
        Opcode::FPRoundInt64 => emit_fp_round_int64(code, ctx, inst_ref),
        Opcode::FPSqrt32 => emit_fp_sqrt32(code, ctx, inst_ref),
        Opcode::FPSqrt64 => emit_fp_sqrt64(code, ctx, inst_ref),
        Opcode::FPSub32 => emit_fp_sub32(code, ctx, inst_ref),
        Opcode::FPSub64 => emit_fp_sub64(code, ctx, inst_ref),
        Opcode::FPSingleToDouble => emit_fp_single_to_double(code, ctx, inst_ref),
        Opcode::FPHalfToSingle => emit_fp_half_to_single(code, ctx, inst_ref),
        Opcode::FPHalfToDouble => emit_fp_half_to_double(code, ctx, inst_ref),
        Opcode::FPSingleToHalf => emit_fp_single_to_half(code, ctx, inst_ref),
        Opcode::FPDoubleToHalf => emit_fp_double_to_half(code, ctx, inst_ref),
        Opcode::FPDoubleToSingle => emit_fp_double_to_single(code, ctx, inst_ref),
        Opcode::FPSingleToFixedS16 => emit_fp_single_to_fixed_s16(code, ctx, inst_ref),
        Opcode::FPDoubleToFixedS16 => emit_fp_double_to_fixed_s16(code, ctx, inst_ref),
        Opcode::FPSingleToFixedS32 => emit_fp_single_to_fixed_s32(code, ctx, inst_ref),
        Opcode::FPDoubleToFixedS32 => emit_fp_double_to_fixed_s32(code, ctx, inst_ref),
        Opcode::FPSingleToFixedU16 => emit_fp_single_to_fixed_u16(code, ctx, inst_ref),
        Opcode::FPDoubleToFixedU16 => emit_fp_double_to_fixed_u16(code, ctx, inst_ref),
        Opcode::FPSingleToFixedU32 => emit_fp_single_to_fixed_u32(code, ctx, inst_ref),
        Opcode::FPDoubleToFixedU32 => emit_fp_double_to_fixed_u32(code, ctx, inst_ref),
        Opcode::FPSingleToFixedU64 => emit_fp_single_to_fixed_u64(code, ctx, inst_ref),
        Opcode::FPDoubleToFixedU64 => emit_fp_double_to_fixed_u64(code, ctx, inst_ref),
        Opcode::FPSingleToFixedS64 => emit_fp_single_to_fixed_s64(code, ctx, inst_ref),
        Opcode::FPDoubleToFixedS64 => emit_fp_double_to_fixed_s64(code, ctx, inst_ref),
        Opcode::FPFixedU16ToSingle => emit_fp_fixed_u16_to_single(code, ctx, inst_ref),
        Opcode::FPFixedU16ToDouble => emit_fp_fixed_u16_to_double(code, ctx, inst_ref),
        Opcode::FPFixedS16ToSingle => emit_fp_fixed_s16_to_single(code, ctx, inst_ref),
        Opcode::FPFixedS16ToDouble => emit_fp_fixed_s16_to_double(code, ctx, inst_ref),
        Opcode::FPFixedU32ToSingle => emit_fp_fixed_u32_to_single(code, ctx, inst_ref),
        Opcode::FPFixedU32ToDouble => emit_fp_fixed_u32_to_double(code, ctx, inst_ref),
        Opcode::FPFixedS32ToSingle => emit_fp_fixed_s32_to_single(code, ctx, inst_ref),
        Opcode::FPFixedS32ToDouble => emit_fp_fixed_s32_to_double(code, ctx, inst_ref),
        Opcode::FPFixedU64ToSingle => emit_fp_fixed_u64_to_single(code, ctx, inst_ref),
        Opcode::FPFixedU64ToDouble => emit_fp_fixed_u64_to_double(code, ctx, inst_ref),
        Opcode::FPFixedS64ToSingle => emit_fp_fixed_s64_to_single(code, ctx, inst_ref),
        Opcode::FPFixedS64ToDouble => emit_fp_fixed_s64_to_double(code, ctx, inst_ref),
        Opcode::CountLeadingZeros32 => emit_count_leading_zeros32(code, ctx, inst_ref),
        Opcode::CountLeadingZeros64 => emit_count_leading_zeros64(code, ctx, inst_ref),
        Opcode::ByteReverseWord => emit_byte_reverse_word(code, ctx, inst_ref),
        Opcode::ByteReverseHalf => emit_byte_reverse_half(code, ctx, inst_ref),
        Opcode::ByteReverseDual => emit_byte_reverse_dual(code, ctx, inst_ref),
        Opcode::ReplicateBit32 => emit_replicate_bit32(code, ctx, inst_ref),
        Opcode::ReplicateBit64 => emit_replicate_bit64(code, ctx, inst_ref),
        Opcode::ConditionalSelect32 | Opcode::ConditionalSelectNZCV => {
            emit_conditional_select32(code, ctx, inst_ref)
        }
        Opcode::ConditionalSelect64 => emit_conditional_select64(code, ctx, inst_ref),
        Opcode::LogicalShiftLeft32 => emit_logical_shift_left32(code, ctx, inst_ref),
        Opcode::LogicalShiftLeft64 => emit_logical_shift_left64(code, ctx, inst_ref),
        Opcode::LogicalShiftRight32 => emit_logical_shift_right32(code, ctx, inst_ref),
        Opcode::LogicalShiftRight64 => emit_logical_shift_right64(code, ctx, inst_ref),
        Opcode::ArithmeticShiftRight32 => emit_arithmetic_shift_right32(code, ctx, inst_ref),
        Opcode::ArithmeticShiftRight64 => emit_arithmetic_shift_right64(code, ctx, inst_ref),
        Opcode::BitRotateRight32 => emit_rotate_right32(code, ctx, inst_ref),
        Opcode::BitRotateRight64 => emit_rotate_right64(code, ctx, inst_ref),
        Opcode::LogicalShiftLeftMasked32 => emit_logical_shift_left_masked32(code, ctx, inst_ref),
        Opcode::LogicalShiftLeftMasked64 => emit_logical_shift_left_masked64(code, ctx, inst_ref),
        Opcode::LogicalShiftRightMasked32 => emit_logical_shift_right_masked32(code, ctx, inst_ref),
        Opcode::LogicalShiftRightMasked64 => emit_logical_shift_right_masked64(code, ctx, inst_ref),
        Opcode::ArithmeticShiftRightMasked32 => {
            emit_arithmetic_shift_right_masked32(code, ctx, inst_ref)
        }
        Opcode::ArithmeticShiftRightMasked64 => {
            emit_arithmetic_shift_right_masked64(code, ctx, inst_ref)
        }
        Opcode::RotateRightMasked32 => emit_rotate_right_masked32(code, ctx, inst_ref),
        Opcode::RotateRightMasked64 => emit_rotate_right_masked64(code, ctx, inst_ref),
        Opcode::RotateRightExtended => emit_rotate_right_extended(code, ctx, inst_ref),
        Opcode::SignExtendByteToWord => emit_sign_extend_byte_to_word(code, ctx, inst_ref),
        Opcode::SignExtendHalfToWord => emit_sign_extend_half_to_word(code, ctx, inst_ref),
        Opcode::SignExtendByteToLong => emit_sign_extend_byte_to_long(code, ctx, inst_ref),
        Opcode::SignExtendHalfToLong => emit_sign_extend_half_to_long(code, ctx, inst_ref),
        Opcode::SignExtendWordToLong => emit_sign_extend_word_to_long(code, ctx, inst_ref),
        Opcode::ZeroExtendByteToWord
        | Opcode::ZeroExtendHalfToWord
        | Opcode::ZeroExtendByteToLong
        | Opcode::ZeroExtendHalfToLong
        | Opcode::ZeroExtendWordToLong => emit_zero_extend(code, ctx, inst_ref),
        Opcode::ZeroExtendLongToQuad => emit_zero_extend_long_to_quad(code, ctx, inst_ref),
        Opcode::SignedSaturatedAddWithFlag32 => {
            emit_signed_saturated_add_with_flag32(code, ctx, inst_ref)
        }
        Opcode::SignedSaturatedSubWithFlag32 => {
            emit_signed_saturated_sub_with_flag32(code, ctx, inst_ref)
        }
        Opcode::SignedSaturation => emit_signed_saturation(code, ctx, inst_ref),
        Opcode::UnsignedSaturation => emit_unsigned_saturation(code, ctx, inst_ref),
        Opcode::AESDecryptSingleRound => emit_aes_decrypt_single_round(code, ctx, inst_ref),
        Opcode::AESEncryptSingleRound => emit_aes_encrypt_single_round(code, ctx, inst_ref),
        Opcode::AESInverseMixColumns => emit_aes_inverse_mix_columns(code, ctx, inst_ref),
        Opcode::AESMixColumns => emit_aes_mix_columns(code, ctx, inst_ref),
        Opcode::CRC32Castagnoli8 => emit_crc32_castagnoli_8(code, ctx, inst_ref),
        Opcode::CRC32Castagnoli16 => emit_crc32_castagnoli_16(code, ctx, inst_ref),
        Opcode::CRC32Castagnoli32 => emit_crc32_castagnoli_32(code, ctx, inst_ref),
        Opcode::CRC32Castagnoli64 => emit_crc32_castagnoli_64(code, ctx, inst_ref),
        Opcode::CRC32ISO8 => emit_crc32_iso_8(code, ctx, inst_ref),
        Opcode::CRC32ISO16 => emit_crc32_iso_16(code, ctx, inst_ref),
        Opcode::CRC32ISO32 => emit_crc32_iso_32(code, ctx, inst_ref),
        Opcode::CRC32ISO64 => emit_crc32_iso_64(code, ctx, inst_ref),
        Opcode::SHA256Hash => emit_sha256_hash(code, ctx, inst_ref),
        Opcode::SHA256MessageSchedule0 => emit_sha256_message_schedule_0(code, ctx, inst_ref),
        Opcode::SHA256MessageSchedule1 => emit_sha256_message_schedule_1(code, ctx, inst_ref),
        Opcode::PackedAddU8
        | Opcode::PackedAddS8
        | Opcode::PackedSubU8
        | Opcode::PackedSubS8
        | Opcode::PackedAddU16
        | Opcode::PackedAddS16
        | Opcode::PackedSubU16
        | Opcode::PackedSubS16
        | Opcode::PackedAddSubU16
        | Opcode::PackedAddSubS16
        | Opcode::PackedSubAddU16
        | Opcode::PackedSubAddS16
        | Opcode::PackedHalvingAddU8
        | Opcode::PackedHalvingAddS8
        | Opcode::PackedHalvingSubU8
        | Opcode::PackedHalvingSubS8
        | Opcode::PackedHalvingAddU16
        | Opcode::PackedHalvingAddS16
        | Opcode::PackedHalvingSubU16
        | Opcode::PackedHalvingSubS16
        | Opcode::PackedHalvingAddSubU16
        | Opcode::PackedHalvingAddSubS16
        | Opcode::PackedHalvingSubAddU16
        | Opcode::PackedHalvingSubAddS16
        | Opcode::PackedSaturatedAddU8
        | Opcode::PackedSaturatedAddS8
        | Opcode::PackedSaturatedSubU8
        | Opcode::PackedSaturatedSubS8
        | Opcode::PackedSaturatedAddU16
        | Opcode::PackedSaturatedAddS16
        | Opcode::PackedSaturatedSubU16
        | Opcode::PackedSaturatedSubS16
        | Opcode::PackedAbsDiffSumU8
        | Opcode::PackedSelect => emit_packed_instruction(code, ctx, inst_ref),
        Opcode::VectorSignedSaturatedAdd8
        | Opcode::VectorSignedSaturatedAdd16
        | Opcode::VectorSignedSaturatedAdd32
        | Opcode::VectorSignedSaturatedAdd64
        | Opcode::VectorSignedSaturatedSub8
        | Opcode::VectorSignedSaturatedSub16
        | Opcode::VectorSignedSaturatedSub32
        | Opcode::VectorSignedSaturatedSub64
        | Opcode::VectorUnsignedSaturatedAdd8
        | Opcode::VectorUnsignedSaturatedAdd16
        | Opcode::VectorUnsignedSaturatedAdd32
        | Opcode::VectorUnsignedSaturatedAdd64
        | Opcode::VectorUnsignedSaturatedSub8
        | Opcode::VectorUnsignedSaturatedSub16
        | Opcode::VectorUnsignedSaturatedSub32
        | Opcode::VectorUnsignedSaturatedSub64 => {
            emit_vector_saturation_instruction(code, ctx, inst_ref)
        }
        Opcode::FPVectorAbs16
        | Opcode::FPVectorAbs32
        | Opcode::FPVectorAbs64
        | Opcode::FPVectorAdd32
        | Opcode::FPVectorAdd64
        | Opcode::FPVectorSub32
        | Opcode::FPVectorSub64
        | Opcode::FPVectorMul32
        | Opcode::FPVectorMul64
        | Opcode::FPVectorMulX32
        | Opcode::FPVectorMulX64
        | Opcode::FPVectorNeg32
        | Opcode::FPVectorNeg64
        | Opcode::FPVectorSqrt32
        | Opcode::FPVectorSqrt64
        | Opcode::FPVectorRecipEstimate32
        | Opcode::FPVectorRecipEstimate64
        | Opcode::FPVectorRSqrtEstimate32
        | Opcode::FPVectorRSqrtEstimate64
        | Opcode::FPVectorDiv32
        | Opcode::FPVectorDiv64
        | Opcode::FPVectorMax32
        | Opcode::FPVectorMax64
        | Opcode::FPVectorMaxNumeric32
        | Opcode::FPVectorMaxNumeric64
        | Opcode::FPVectorMin32
        | Opcode::FPVectorMin64
        | Opcode::FPVectorMinNumeric32
        | Opcode::FPVectorMinNumeric64
        | Opcode::FPVectorEqual32
        | Opcode::FPVectorEqual64
        | Opcode::FPVectorGreater32
        | Opcode::FPVectorGreater64
        | Opcode::FPVectorGreaterEqual32
        | Opcode::FPVectorGreaterEqual64
        | Opcode::FPVectorMulAdd32
        | Opcode::FPVectorMulAdd64
        | Opcode::FPVectorPairedAdd32
        | Opcode::FPVectorPairedAdd64
        | Opcode::FPVectorPairedAddLower32
        | Opcode::FPVectorPairedAddLower64
        | Opcode::FPVectorFromHalf32
        | Opcode::FPVectorToHalf32
        | Opcode::FPVectorFromSignedFixed32
        | Opcode::FPVectorFromSignedFixed64
        | Opcode::FPVectorFromUnsignedFixed32
        | Opcode::FPVectorFromUnsignedFixed64
        | Opcode::FPVectorToSignedFixed32
        | Opcode::FPVectorToSignedFixed64
        | Opcode::FPVectorToUnsignedFixed32
        | Opcode::FPVectorToUnsignedFixed64
        | Opcode::FPVectorRoundInt16
        | Opcode::FPVectorRoundInt32
        | Opcode::FPVectorRoundInt64
        | Opcode::FPVectorRecipStepFused32
        | Opcode::FPVectorRecipStepFused64
        | Opcode::FPVectorRSqrtStepFused32
        | Opcode::FPVectorRSqrtStepFused64 => emit_fp_vector_instruction(code, ctx, inst_ref),
        Opcode::VectorGetElement8
        | Opcode::VectorGetElement16
        | Opcode::VectorGetElement32
        | Opcode::VectorGetElement64
        | Opcode::VectorSetElement8
        | Opcode::VectorSetElement16
        | Opcode::VectorSetElement32
        | Opcode::VectorSetElement64
        | Opcode::VectorBroadcastLower8
        | Opcode::VectorBroadcastLower16
        | Opcode::VectorBroadcastLower32
        | Opcode::VectorBroadcast8
        | Opcode::VectorBroadcast16
        | Opcode::VectorBroadcast32
        | Opcode::VectorBroadcast64
        | Opcode::VectorBroadcastElementLower8
        | Opcode::VectorBroadcastElementLower16
        | Opcode::VectorBroadcastElementLower32
        | Opcode::VectorBroadcastElement8
        | Opcode::VectorBroadcastElement16
        | Opcode::VectorBroadcastElement32
        | Opcode::VectorBroadcastElement64
        | Opcode::VectorAbs8
        | Opcode::VectorAbs16
        | Opcode::VectorAbs32
        | Opcode::VectorAbs64
        | Opcode::VectorNot
        | Opcode::VectorCountLeadingZeros8
        | Opcode::VectorCountLeadingZeros16
        | Opcode::VectorCountLeadingZeros32
        | Opcode::VectorPopulationCount
        | Opcode::VectorReverseBits
        | Opcode::VectorReverseElementsInHalfGroups8
        | Opcode::VectorReverseElementsInWordGroups8
        | Opcode::VectorReverseElementsInWordGroups16
        | Opcode::VectorReverseElementsInLongGroups8
        | Opcode::VectorReverseElementsInLongGroups16
        | Opcode::VectorReverseElementsInLongGroups32
        | Opcode::VectorReduceAdd8
        | Opcode::VectorReduceAdd16
        | Opcode::VectorReduceAdd32
        | Opcode::VectorReduceAdd64
        | Opcode::VectorZeroExtend8
        | Opcode::VectorZeroExtend16
        | Opcode::VectorZeroExtend32
        | Opcode::VectorZeroExtend64
        | Opcode::VectorSignExtend8
        | Opcode::VectorSignExtend16
        | Opcode::VectorSignExtend32
        | Opcode::VectorZeroUpper
        | Opcode::VectorNarrow16
        | Opcode::VectorNarrow32
        | Opcode::VectorNarrow64
        | Opcode::VectorAdd8
        | Opcode::VectorAdd16
        | Opcode::VectorAdd32
        | Opcode::VectorAdd64
        | Opcode::VectorSub8
        | Opcode::VectorSub16
        | Opcode::VectorSub32
        | Opcode::VectorSub64
        | Opcode::VectorMultiply8
        | Opcode::VectorMultiply16
        | Opcode::VectorMultiply32
        | Opcode::VectorMultiplySignedWiden8
        | Opcode::VectorMultiplySignedWiden16
        | Opcode::VectorMultiplySignedWiden32
        | Opcode::VectorMultiplyUnsignedWiden8
        | Opcode::VectorMultiplyUnsignedWiden16
        | Opcode::VectorMultiplyUnsignedWiden32
        | Opcode::VectorAnd
        | Opcode::VectorAndNot
        | Opcode::VectorEor
        | Opcode::VectorOr
        | Opcode::VectorEqual8
        | Opcode::VectorEqual16
        | Opcode::VectorEqual32
        | Opcode::VectorEqual64
        | Opcode::VectorGreaterS8
        | Opcode::VectorGreaterS16
        | Opcode::VectorGreaterS32
        | Opcode::VectorGreaterS64
        | Opcode::VectorHalvingAddS8
        | Opcode::VectorHalvingAddS16
        | Opcode::VectorHalvingAddS32
        | Opcode::VectorHalvingAddU8
        | Opcode::VectorHalvingAddU16
        | Opcode::VectorHalvingAddU32
        | Opcode::VectorHalvingSubS8
        | Opcode::VectorHalvingSubS16
        | Opcode::VectorHalvingSubS32
        | Opcode::VectorHalvingSubU8
        | Opcode::VectorHalvingSubU16
        | Opcode::VectorHalvingSubU32
        | Opcode::VectorMaxS8
        | Opcode::VectorMaxS16
        | Opcode::VectorMaxS32
        | Opcode::VectorMaxU8
        | Opcode::VectorMaxU16
        | Opcode::VectorMaxU32
        | Opcode::VectorMinS8
        | Opcode::VectorMinS16
        | Opcode::VectorMinS32
        | Opcode::VectorMinU8
        | Opcode::VectorMinU16
        | Opcode::VectorMinU32
        | Opcode::VectorPairedAddLower8
        | Opcode::VectorPairedAddLower16
        | Opcode::VectorPairedAddLower32
        | Opcode::VectorPairedAdd8
        | Opcode::VectorPairedAdd16
        | Opcode::VectorPairedAdd32
        | Opcode::VectorPairedAdd64
        | Opcode::VectorPairedAddSignedWiden8
        | Opcode::VectorPairedAddSignedWiden16
        | Opcode::VectorPairedAddSignedWiden32
        | Opcode::VectorPairedAddUnsignedWiden8
        | Opcode::VectorPairedAddUnsignedWiden16
        | Opcode::VectorPairedAddUnsignedWiden32
        | Opcode::VectorPairedMaxS8
        | Opcode::VectorPairedMaxS16
        | Opcode::VectorPairedMaxS32
        | Opcode::VectorPairedMaxU8
        | Opcode::VectorPairedMaxU16
        | Opcode::VectorPairedMaxU32
        | Opcode::VectorPairedMaxLowerS8
        | Opcode::VectorPairedMaxLowerS16
        | Opcode::VectorPairedMaxLowerS32
        | Opcode::VectorPairedMaxLowerU8
        | Opcode::VectorPairedMaxLowerU16
        | Opcode::VectorPairedMaxLowerU32
        | Opcode::VectorPairedMinS8
        | Opcode::VectorPairedMinS16
        | Opcode::VectorPairedMinS32
        | Opcode::VectorPairedMinU8
        | Opcode::VectorPairedMinU16
        | Opcode::VectorPairedMinU32
        | Opcode::VectorPairedMinLowerS8
        | Opcode::VectorPairedMinLowerS16
        | Opcode::VectorPairedMinLowerS32
        | Opcode::VectorPairedMinLowerU8
        | Opcode::VectorPairedMinLowerU16
        | Opcode::VectorPairedMinLowerU32
        | Opcode::VectorPolynomialMultiply8
        | Opcode::VectorPolynomialMultiplyLong8
        | Opcode::VectorPolynomialMultiplyLong64
        | Opcode::VectorArithmeticVShift8
        | Opcode::VectorArithmeticVShift16
        | Opcode::VectorArithmeticVShift32
        | Opcode::VectorArithmeticVShift64
        | Opcode::VectorLogicalVShift8
        | Opcode::VectorLogicalVShift16
        | Opcode::VectorLogicalVShift32
        | Opcode::VectorLogicalVShift64
        | Opcode::VectorRoundingShiftLeftS8
        | Opcode::VectorRoundingShiftLeftS16
        | Opcode::VectorRoundingShiftLeftS32
        | Opcode::VectorRoundingShiftLeftS64
        | Opcode::VectorRoundingShiftLeftU8
        | Opcode::VectorRoundingShiftLeftU16
        | Opcode::VectorRoundingShiftLeftU32
        | Opcode::VectorRoundingShiftLeftU64
        | Opcode::VectorSignedAbsoluteDifference8
        | Opcode::VectorSignedAbsoluteDifference16
        | Opcode::VectorSignedAbsoluteDifference32
        | Opcode::VectorSignedMultiply16
        | Opcode::VectorSignedMultiply32
        | Opcode::VectorUnsignedAbsoluteDifference8
        | Opcode::VectorUnsignedAbsoluteDifference16
        | Opcode::VectorUnsignedAbsoluteDifference32
        | Opcode::VectorRoundingHalvingAddS8
        | Opcode::VectorRoundingHalvingAddS16
        | Opcode::VectorRoundingHalvingAddS32
        | Opcode::VectorRoundingHalvingAddU8
        | Opcode::VectorRoundingHalvingAddU16
        | Opcode::VectorRoundingHalvingAddU32
        | Opcode::VectorSignedSaturatedAbs8
        | Opcode::VectorSignedSaturatedAbs16
        | Opcode::VectorSignedSaturatedAbs32
        | Opcode::VectorSignedSaturatedAbs64
        | Opcode::VectorSignedSaturatedAccumulateUnsigned8
        | Opcode::VectorSignedSaturatedAccumulateUnsigned16
        | Opcode::VectorSignedSaturatedAccumulateUnsigned32
        | Opcode::VectorSignedSaturatedAccumulateUnsigned64
        | Opcode::VectorSignedSaturatedDoublingMultiplyHigh16
        | Opcode::VectorSignedSaturatedDoublingMultiplyHigh32
        | Opcode::VectorSignedSaturatedDoublingMultiplyHighRounding16
        | Opcode::VectorSignedSaturatedDoublingMultiplyHighRounding32
        | Opcode::VectorSignedSaturatedDoublingMultiplyLong16
        | Opcode::VectorSignedSaturatedDoublingMultiplyLong32
        | Opcode::VectorSignedSaturatedNarrowToSigned16
        | Opcode::VectorSignedSaturatedNarrowToSigned32
        | Opcode::VectorSignedSaturatedNarrowToSigned64
        | Opcode::VectorSignedSaturatedNarrowToUnsigned16
        | Opcode::VectorSignedSaturatedNarrowToUnsigned32
        | Opcode::VectorSignedSaturatedNarrowToUnsigned64
        | Opcode::VectorSignedSaturatedNeg8
        | Opcode::VectorSignedSaturatedNeg16
        | Opcode::VectorSignedSaturatedNeg32
        | Opcode::VectorSignedSaturatedNeg64
        | Opcode::VectorSignedSaturatedShiftLeft8
        | Opcode::VectorSignedSaturatedShiftLeft16
        | Opcode::VectorSignedSaturatedShiftLeft32
        | Opcode::VectorSignedSaturatedShiftLeft64
        | Opcode::VectorSignedSaturatedShiftLeftUnsigned8
        | Opcode::VectorSignedSaturatedShiftLeftUnsigned16
        | Opcode::VectorSignedSaturatedShiftLeftUnsigned32
        | Opcode::VectorSignedSaturatedShiftLeftUnsigned64
        | Opcode::VectorTable
        | Opcode::VectorTableLookup64
        | Opcode::VectorTableLookup128
        | Opcode::VectorUnsignedRecipEstimate
        | Opcode::VectorUnsignedRecipSqrtEstimate
        | Opcode::VectorUnsignedMultiply16
        | Opcode::VectorUnsignedMultiply32
        | Opcode::VectorUnsignedSaturatedAccumulateSigned8
        | Opcode::VectorUnsignedSaturatedAccumulateSigned16
        | Opcode::VectorUnsignedSaturatedAccumulateSigned32
        | Opcode::VectorUnsignedSaturatedAccumulateSigned64
        | Opcode::VectorUnsignedSaturatedNarrow16
        | Opcode::VectorUnsignedSaturatedNarrow32
        | Opcode::VectorUnsignedSaturatedNarrow64
        | Opcode::VectorUnsignedSaturatedShiftLeft8
        | Opcode::VectorUnsignedSaturatedShiftLeft16
        | Opcode::VectorUnsignedSaturatedShiftLeft32
        | Opcode::VectorUnsignedSaturatedShiftLeft64
        | Opcode::VectorInterleaveLower8
        | Opcode::VectorInterleaveLower16
        | Opcode::VectorInterleaveLower32
        | Opcode::VectorInterleaveLower64
        | Opcode::VectorInterleaveUpper8
        | Opcode::VectorInterleaveUpper16
        | Opcode::VectorInterleaveUpper32
        | Opcode::VectorInterleaveUpper64
        | Opcode::VectorDeinterleaveEven8
        | Opcode::VectorDeinterleaveEven16
        | Opcode::VectorDeinterleaveEven32
        | Opcode::VectorDeinterleaveEven64
        | Opcode::VectorDeinterleaveEvenLower8
        | Opcode::VectorDeinterleaveEvenLower16
        | Opcode::VectorDeinterleaveEvenLower32
        | Opcode::VectorDeinterleaveOdd8
        | Opcode::VectorDeinterleaveOdd16
        | Opcode::VectorDeinterleaveOdd32
        | Opcode::VectorDeinterleaveOdd64
        | Opcode::VectorDeinterleaveOddLower8
        | Opcode::VectorDeinterleaveOddLower16
        | Opcode::VectorDeinterleaveOddLower32
        | Opcode::VectorTranspose8
        | Opcode::VectorTranspose16
        | Opcode::VectorTranspose32
        | Opcode::VectorTranspose64
        | Opcode::VectorLogicalShiftLeft8
        | Opcode::VectorLogicalShiftLeft16
        | Opcode::VectorLogicalShiftLeft32
        | Opcode::VectorLogicalShiftLeft64
        | Opcode::VectorLogicalShiftRight8
        | Opcode::VectorLogicalShiftRight16
        | Opcode::VectorLogicalShiftRight32
        | Opcode::VectorLogicalShiftRight64
        | Opcode::VectorArithmeticShiftRight8
        | Opcode::VectorArithmeticShiftRight16
        | Opcode::VectorArithmeticShiftRight32
        | Opcode::VectorArithmeticShiftRight64
        | Opcode::VectorExtract
        | Opcode::VectorExtractLower
        | Opcode::ZeroVector => emit_vector_instruction(code, ctx, inst_ref),
        Opcode::A64SetPC => {
            let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
            let mut pc = ctx.reg_alloc.read_x(args[0]);
            let pc_reg = pc.realize(code, ctx.block)? as u8;
            code.write_u32(inst::str_x_unsigned(
                pc_reg,
                crate::backend::arm64::abi::XSTATE,
                core::mem::offset_of!(A64JitState, pc) as u32,
            ))?;
            Ok(())
        }
        Opcode::A32SetCheckBit => emit_a32_set_check_bit(code, ctx, inst_ref),
        Opcode::A32GetRegister => emit_a32_get_register(code, ctx, inst_ref),
        Opcode::A32SetRegister => emit_a32_set_register(code, ctx, inst_ref),
        Opcode::A32GetExtendedRegister32 => emit_a32_get_extended_register32(code, ctx, inst_ref),
        Opcode::A32GetExtendedRegister64 => emit_a32_get_extended_register64(code, ctx, inst_ref),
        Opcode::A32SetExtendedRegister32 => emit_a32_set_extended_register32(code, ctx, inst_ref),
        Opcode::A32SetExtendedRegister64 => emit_a32_set_extended_register64(code, ctx, inst_ref),
        Opcode::A32GetVector => emit_a32_get_vector(code, ctx, inst_ref),
        Opcode::A32SetVector => emit_a32_set_vector(code, ctx, inst_ref),
        Opcode::A32GetCpsr => emit_a32_get_cpsr(code, ctx, inst_ref),
        Opcode::A32SetCpsr => emit_a32_set_cpsr(code, ctx, inst_ref),
        Opcode::A32GetCFlag => emit_a32_get_c_flag(code, ctx, inst_ref),
        Opcode::A32SetCpsrNZCV => emit_a32_set_cpsr_nzcv(code, ctx, inst_ref),
        Opcode::A32SetCpsrNZCVRaw => emit_a32_set_cpsr_nzcv_raw(code, ctx, inst_ref),
        Opcode::A32SetCpsrNZCVQ => emit_a32_set_cpsr_nzcvq(code, ctx, inst_ref),
        Opcode::A32SetCpsrNZ => emit_a32_set_cpsr_nz(code, ctx, inst_ref),
        Opcode::A32SetCpsrNZC => emit_a32_set_cpsr_nzc(code, ctx, inst_ref),
        Opcode::A32OrQFlag => emit_a32_or_q_flag(code, ctx, inst_ref),
        Opcode::A32GetGEFlags => emit_a32_get_ge_flags(code, ctx, inst_ref),
        Opcode::A32SetGEFlags => emit_a32_set_ge_flags(code, ctx, inst_ref),
        Opcode::A32SetGEFlagsCompressed => emit_a32_set_ge_flags_compressed(code, ctx, inst_ref),
        Opcode::A32BXWritePC => emit_a32_bx_write_pc(code, ctx, inst_ref),
        Opcode::A32UpdateUpperLocationDescriptor => {
            emit_a32_update_upper_location_descriptor(code, ctx)
        }
        Opcode::A32CallSupervisor => emit_a32_call_supervisor(code, ctx, inst_ref),
        Opcode::A32ExceptionRaised => emit_a32_exception_raised(code, ctx, inst_ref),
        Opcode::A32DataSynchronizationBarrier => emit_a32_data_synchronization_barrier(code),
        Opcode::A32DataMemoryBarrier => emit_a32_data_memory_barrier(code),
        Opcode::A32InstructionSynchronizationBarrier => {
            emit_a32_instruction_synchronization_barrier(code, ctx)
        }
        Opcode::A32GetFpscr => emit_a32_get_fpscr(code, ctx, inst_ref),
        Opcode::A32SetFpscr => emit_a32_set_fpscr(code, ctx, inst_ref),
        Opcode::A32GetFpscrNZCV => emit_a32_get_fpscr_nzcv(code, ctx, inst_ref),
        Opcode::A32SetFpscrNZCV => emit_a32_set_fpscr_nzcv(code, ctx, inst_ref),
        Opcode::A32ClearExclusive => emit_a32_clear_exclusive(code),
        Opcode::A32ReadMemory8 => emit_a32_read_memory::<8>(code, ctx, inst_ref),
        Opcode::A32ReadMemory16 => emit_a32_read_memory::<16>(code, ctx, inst_ref),
        Opcode::A32ReadMemory32 => emit_a32_read_memory::<32>(code, ctx, inst_ref),
        Opcode::A32ReadMemory64 => emit_a32_read_memory::<64>(code, ctx, inst_ref),
        Opcode::A32ExclusiveReadMemory8 => emit_a32_exclusive_read_memory::<8>(code, ctx, inst_ref),
        Opcode::A32ExclusiveReadMemory16 => {
            emit_a32_exclusive_read_memory::<16>(code, ctx, inst_ref)
        }
        Opcode::A32ExclusiveReadMemory32 => {
            emit_a32_exclusive_read_memory::<32>(code, ctx, inst_ref)
        }
        Opcode::A32ExclusiveReadMemory64 => {
            emit_a32_exclusive_read_memory::<64>(code, ctx, inst_ref)
        }
        Opcode::A32WriteMemory8 => emit_a32_write_memory::<8>(code, ctx, inst_ref),
        Opcode::A32WriteMemory16 => emit_a32_write_memory::<16>(code, ctx, inst_ref),
        Opcode::A32WriteMemory32 => emit_a32_write_memory::<32>(code, ctx, inst_ref),
        Opcode::A32WriteMemory64 => emit_a32_write_memory::<64>(code, ctx, inst_ref),
        Opcode::A32ExclusiveWriteMemory8 => {
            emit_a32_exclusive_write_memory::<8>(code, ctx, inst_ref)
        }
        Opcode::A32ExclusiveWriteMemory16 => {
            emit_a32_exclusive_write_memory::<16>(code, ctx, inst_ref)
        }
        Opcode::A32ExclusiveWriteMemory32 => {
            emit_a32_exclusive_write_memory::<32>(code, ctx, inst_ref)
        }
        Opcode::A32ExclusiveWriteMemory64 => {
            emit_a32_exclusive_write_memory::<64>(code, ctx, inst_ref)
        }
        Opcode::A32CoprocInternalOperation => {
            emit_a32_coproc_internal_operation(code, ctx, inst_ref)
        }
        Opcode::A32CoprocSendOneWord => emit_a32_coproc_send_one_word(code, ctx, inst_ref),
        Opcode::A32CoprocSendTwoWords => emit_a32_coproc_send_two_words(code, ctx, inst_ref),
        Opcode::A32CoprocGetOneWord => emit_a32_coproc_get_one_word(code, ctx, inst_ref),
        Opcode::A32CoprocGetTwoWords => emit_a32_coproc_get_two_words(code, ctx, inst_ref),
        Opcode::A32CoprocLoadWords => emit_a32_coproc_load_words(code, ctx, inst_ref),
        Opcode::A32CoprocStoreWords => emit_a32_coproc_store_words(code, ctx, inst_ref),
        Opcode::A64ClearExclusive => emit_a64_clear_exclusive(code),
        Opcode::A64ReadMemory8 => emit_a64_read_memory::<8>(code, ctx, inst_ref),
        Opcode::A64ReadMemory16 => emit_a64_read_memory::<16>(code, ctx, inst_ref),
        Opcode::A64ReadMemory32 => emit_a64_read_memory::<32>(code, ctx, inst_ref),
        Opcode::A64ReadMemory64 => emit_a64_read_memory::<64>(code, ctx, inst_ref),
        Opcode::A64ReadMemory128 => emit_a64_read_memory::<128>(code, ctx, inst_ref),
        Opcode::A64ExclusiveReadMemory8 => emit_a64_exclusive_read_memory::<8>(code, ctx, inst_ref),
        Opcode::A64ExclusiveReadMemory16 => {
            emit_a64_exclusive_read_memory::<16>(code, ctx, inst_ref)
        }
        Opcode::A64ExclusiveReadMemory32 => {
            emit_a64_exclusive_read_memory::<32>(code, ctx, inst_ref)
        }
        Opcode::A64ExclusiveReadMemory64 => {
            emit_a64_exclusive_read_memory::<64>(code, ctx, inst_ref)
        }
        Opcode::A64ExclusiveReadMemory128 => {
            emit_a64_exclusive_read_memory::<128>(code, ctx, inst_ref)
        }
        Opcode::A64WriteMemory8 => emit_a64_write_memory::<8>(code, ctx, inst_ref),
        Opcode::A64WriteMemory16 => emit_a64_write_memory::<16>(code, ctx, inst_ref),
        Opcode::A64WriteMemory32 => emit_a64_write_memory::<32>(code, ctx, inst_ref),
        Opcode::A64WriteMemory64 => emit_a64_write_memory::<64>(code, ctx, inst_ref),
        Opcode::A64WriteMemory128 => emit_a64_write_memory::<128>(code, ctx, inst_ref),
        Opcode::A64ExclusiveWriteMemory8 => {
            emit_a64_exclusive_write_memory::<8>(code, ctx, inst_ref)
        }
        Opcode::A64ExclusiveWriteMemory16 => {
            emit_a64_exclusive_write_memory::<16>(code, ctx, inst_ref)
        }
        Opcode::A64ExclusiveWriteMemory32 => {
            emit_a64_exclusive_write_memory::<32>(code, ctx, inst_ref)
        }
        Opcode::A64ExclusiveWriteMemory64 => {
            emit_a64_exclusive_write_memory::<64>(code, ctx, inst_ref)
        }
        Opcode::A64ExclusiveWriteMemory128 => {
            emit_a64_exclusive_write_memory::<128>(code, ctx, inst_ref)
        }
        Opcode::A64SetCheckBit => emit_a64_set_check_bit(code, ctx, inst_ref),
        Opcode::A64GetCFlag => emit_a64_get_c_flag(code, ctx, inst_ref),
        Opcode::A64GetNZCVRaw => emit_a64_get_nzcv_raw(code, ctx, inst_ref),
        Opcode::A64SetNZCVRaw | Opcode::A64SetNZCV => emit_a64_set_nzcv(code, ctx, inst_ref),
        Opcode::A64GetW => emit_a64_get_w(code, ctx, inst_ref),
        Opcode::A64GetX => emit_a64_get_x(code, ctx, inst_ref),
        Opcode::A64GetS => emit_a64_get_s(code, ctx, inst_ref),
        Opcode::A64GetD => emit_a64_get_d(code, ctx, inst_ref),
        Opcode::A64GetQ => emit_a64_get_q(code, ctx, inst_ref),
        Opcode::A64GetSP => emit_a64_get_sp(code, ctx, inst_ref),
        Opcode::A64GetFPCR => emit_a64_get_fpcr(code, ctx, inst_ref),
        Opcode::A64GetFPSR => emit_a64_get_fpsr(code, ctx, inst_ref),
        Opcode::A64SetW => emit_a64_set_w(code, ctx, inst_ref),
        Opcode::A64SetX => emit_a64_set_x(code, ctx, inst_ref),
        Opcode::A64SetS => emit_a64_set_s(code, ctx, inst_ref),
        Opcode::A64SetD => emit_a64_set_d(code, ctx, inst_ref),
        Opcode::A64SetQ => emit_a64_set_q(code, ctx, inst_ref),
        Opcode::A64SetSP => emit_a64_set_sp(code, ctx, inst_ref),
        Opcode::A64SetPC => emit_a64_set_pc(code, ctx, inst_ref),
        Opcode::A64SetFPCR => emit_a64_set_fpcr(code, ctx, inst_ref),
        Opcode::A64SetFPSR => emit_a64_set_fpsr(code, ctx, inst_ref),
        Opcode::A64CallSupervisor => emit_a64_call_supervisor(code, ctx, inst_ref),
        Opcode::A64ExceptionRaised => emit_a64_exception_raised(code, ctx, inst_ref),
        Opcode::A64DataCacheOperationRaised => {
            emit_a64_data_cache_operation_raised(code, ctx, inst_ref)
        }
        Opcode::A64InstructionCacheOperationRaised => {
            emit_a64_instruction_cache_operation_raised(code, ctx, inst_ref)
        }
        Opcode::A64DataSynchronizationBarrier => emit_a64_data_synchronization_barrier(code),
        Opcode::A64DataMemoryBarrier => emit_a64_data_memory_barrier(code),
        Opcode::A64InstructionSynchronizationBarrier => {
            emit_a64_instruction_synchronization_barrier(code, ctx)
        }
        Opcode::A64GetCNTFRQ => emit_a64_get_cntfrq(code, ctx, inst_ref),
        Opcode::A64GetCNTPCT => emit_a64_get_cntpct(code, ctx, inst_ref),
        Opcode::A64GetCTR => emit_a64_get_ctr(code, ctx, inst_ref),
        Opcode::A64GetDCZID => emit_a64_get_dczid(code, ctx, inst_ref),
        Opcode::A64GetTPIDR => emit_a64_get_tpidr(code, ctx, inst_ref),
        Opcode::A64SetTPIDR => emit_a64_set_tpidr(code, ctx, inst_ref),
        Opcode::A64GetTPIDRRO => emit_a64_get_tpidrro(code, ctx, inst_ref),
        opcode => Err(format!("ARM64 EmitIR is not ported for opcode {opcode:?}")),
    }
}

fn emit_a64_get_nzcv_raw(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let result = result.realize(code, ctx.block)? as u8;
    code.write_u32(inst::ldr_w_unsigned(result, XSTATE, a64_nzcv_offset()))?;
    Ok(())
}

fn emit_a64_get_c_flag(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let result = result.realize(code, ctx.block)? as u8;
    code.write_u32(inst::ldr_w_unsigned(result, XSTATE, a64_nzcv_offset()))?;
    code.write_u32(inst::and_w_imm(result, result, 1 << 29))?;
    Ok(())
}

fn emit_a64_set_check_bit(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    if args[0].is_immediate() {
        if args[0].get_immediate_u1() {
            emit_mov_w_imm(code, XSCRATCH0, 1)?;
            code.write_u32(inst::strb_w_unsigned(
                XSCRATCH0,
                31,
                StackLayout::check_bit_offset() as u32,
            ))?;
        } else {
            code.write_u32(inst::strb_w_unsigned(
                31,
                31,
                StackLayout::check_bit_offset() as u32,
            ))?;
        }
        return Ok(());
    }

    let mut bit = ctx.reg_alloc.read_w(args[0]);
    let bit = bit.realize(code, ctx.block)? as u8;
    code.write_u32(inst::strb_w_unsigned(
        bit,
        31,
        StackLayout::check_bit_offset() as u32,
    ))?;
    Ok(())
}

fn emit_a64_set_nzcv(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut value = ctx.reg_alloc.read_w(args[0]);
    let value = value.realize(code, ctx.block)? as u8;
    code.write_u32(inst::str_w_unsigned(value, XSTATE, a64_nzcv_offset()))?;
    Ok(())
}

fn emit_a64_get_w(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let offset = a64_reg_offset(args[0].value.get_a64_reg())?;
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let result = result.realize(code, ctx.block)? as u8;
    code.write_u32(inst::ldr_w_unsigned(result, XSTATE, offset))?;
    Ok(())
}

fn emit_a64_get_x(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let offset = a64_reg_offset(args[0].value.get_a64_reg())?;
    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let result = result.realize(code, ctx.block)? as u8;
    code.write_u32(inst::ldr_x_unsigned(result, XSTATE, offset))?;
    Ok(())
}

fn emit_a64_get_sp(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let result = result.realize(code, ctx.block)? as u8;
    code.write_u32(inst::ldr_x_unsigned(result, XSTATE, a64_sp_offset()))?;
    Ok(())
}

fn emit_a64_get_s(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let offset = a64_vec_offset(args[0].value.get_a64_vec());
    let mut result = ctx.reg_alloc.write_s(inst_ref);
    let result = result.realize(code, ctx.block)? as u8;
    code.write_u32(inst::ldr_s_unsigned(result, XSTATE, offset))?;
    Ok(())
}

fn emit_a64_get_d(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let offset = a64_vec_offset(args[0].value.get_a64_vec());
    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let result = result.realize(code, ctx.block)? as u8;
    code.write_u32(inst::ldr_d_unsigned(result, XSTATE, offset))?;
    Ok(())
}

fn emit_a64_get_q(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let offset = a64_vec_offset(args[0].value.get_a64_vec());
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let result = result.realize(code, ctx.block)? as u8;
    code.write_u32(inst::ldr_q_unsigned(result, XSTATE, offset))?;
    Ok(())
}

fn emit_a64_get_fpcr(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let result = result.realize(code, ctx.block)? as u8;
    code.write_u32(inst::ldr_w_unsigned(result, XSTATE, a64_fpcr_offset()))?;
    Ok(())
}

fn emit_a64_get_fpsr(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    ctx.fpsr.spill(code)?;
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let result = result.realize(code, ctx.block)? as u8;
    code.write_u32(inst::ldr_w_unsigned(result, XSTATE, a64_fpsr_offset()))?;
    Ok(())
}

fn emit_a64_set_w(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let offset = a64_reg_offset(args[0].value.get_a64_reg())?;
    let mut value = ctx.reg_alloc.read_w(args[1]);
    let value = value.realize(code, ctx.block)? as u8;
    code.write_u32(inst::mov_w(value, value))?;
    code.write_u32(inst::str_x_unsigned(value, XSTATE, offset))?;
    Ok(())
}

fn emit_a64_set_x(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let offset = a64_reg_offset(args[0].value.get_a64_reg())?;
    let mut value = ctx.reg_alloc.read_x(args[1]);
    let value = value.realize(code, ctx.block)? as u8;
    code.write_u32(inst::str_x_unsigned(value, XSTATE, offset))?;
    Ok(())
}

fn emit_a64_set_sp(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut value = ctx.reg_alloc.read_x(args[0]);
    let value = value.realize(code, ctx.block)? as u8;
    code.write_u32(inst::str_x_unsigned(value, XSTATE, a64_sp_offset()))?;
    Ok(())
}

fn emit_a64_set_s(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let offset = a64_vec_offset(args[0].value.get_a64_vec());
    let mut value = ctx.reg_alloc.read_s(args[1]);
    let value = value.realize(code, ctx.block)? as u8;
    code.write_u32(inst::fmov_s(value, value))?;
    code.write_u32(inst::str_q_unsigned(value, XSTATE, offset))?;
    Ok(())
}

fn emit_a64_set_d(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let offset = a64_vec_offset(args[0].value.get_a64_vec());
    let mut value = ctx.reg_alloc.read_d(args[1]);
    let value = value.realize(code, ctx.block)? as u8;
    code.write_u32(inst::fmov_d(value, value))?;
    code.write_u32(inst::str_q_unsigned(value, XSTATE, offset))?;
    Ok(())
}

fn emit_a64_set_q(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let offset = a64_vec_offset(args[0].value.get_a64_vec());
    let mut value = ctx.reg_alloc.read_q(args[1]);
    let value = value.realize(code, ctx.block)? as u8;
    code.write_u32(inst::str_q_unsigned(value, XSTATE, offset))?;
    Ok(())
}

fn emit_a64_set_pc(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut value = ctx.reg_alloc.read_x(args[0]);
    let value = value.realize(code, ctx.block)? as u8;
    code.write_u32(inst::str_x_unsigned(value, XSTATE, a64_pc_offset()))?;
    Ok(())
}

fn emit_a64_set_fpcr(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut value = ctx.reg_alloc.read_w(args[0]);
    let value = value.realize(code, ctx.block)? as u8;
    code.write_u32(inst::str_w_unsigned(value, XSTATE, a64_fpcr_offset()))?;
    code.write_u32(inst::msr_fpcr(value))?;
    Ok(())
}

fn emit_a64_set_fpsr(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut value = ctx.reg_alloc.read_w(args[0]);
    let value = value.realize(code, ctx.block)? as u8;
    code.write_u32(inst::str_w_unsigned(value, XSTATE, a64_fpsr_offset()))?;
    code.write_u32(inst::msr_fpsr(value))?;
    ctx.fpsr.overwrite();
    Ok(())
}

fn a64_reg_offset(reg: crate::frontend::a64::types::Reg) -> Result<u32, String> {
    let reg = reg.number();
    if reg >= 31 {
        return Err("A64 GPR state access cannot address SP/ZR through reg[]".to_string());
    }
    Ok((core::mem::offset_of!(A64JitState, reg) + core::mem::size_of::<u64>() * reg) as u32)
}

fn a64_vec_offset(vec: crate::frontend::a64::types::Vec) -> u32 {
    (core::mem::offset_of!(A64JitState, vec) + core::mem::size_of::<u64>() * 2 * vec.number())
        as u32
}

fn a64_nzcv_offset() -> u32 {
    core::mem::offset_of!(A64JitState, cpsr_nzcv) as u32
}

fn a64_sp_offset() -> u32 {
    core::mem::offset_of!(A64JitState, sp) as u32
}

fn a64_pc_offset() -> u32 {
    core::mem::offset_of!(A64JitState, pc) as u32
}

fn a64_fpsr_offset() -> u32 {
    core::mem::offset_of!(A64JitState, fpsr) as u32
}

fn a64_fpcr_offset() -> u32 {
    core::mem::offset_of!(A64JitState, fpcr) as u32
}

fn emit_a64_get_tpidr(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_load_system_u64_pointer(code, ctx, inst_ref, ctx.conf.tpidr_el0 as u64)
}

fn emit_a64_get_tpidrro(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_load_system_u64_pointer(code, ctx, inst_ref, ctx.conf.tpidrro_el0 as u64)
}

fn emit_a64_data_synchronization_barrier(code: &mut BlockOfCode) -> Result<(), String> {
    code.write_u32(inst::dsb_sy())?;
    Ok(())
}

fn emit_a64_data_memory_barrier(code: &mut BlockOfCode) -> Result<(), String> {
    code.write_u32(inst::dmb_sy())?;
    Ok(())
}

fn emit_a64_instruction_synchronization_barrier(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
) -> Result<(), String> {
    if !ctx.conf.hook_isb {
        return Ok(());
    }

    ctx.reg_alloc
        .prepare_for_call(code, ctx.fpsr, [None, None, None, None])?;
    emit_relocation(
        code,
        ctx.emitted_block_info,
        LinkTarget::InstructionSynchronizationBarrierRaised,
    )
}

fn emit_a64_get_cntfrq(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let mut value = ctx.reg_alloc.write_x(inst_ref);
    let value = value.realize(code, ctx.block)? as u8;
    emit_mov_x_imm(code, value, ctx.conf.cntfreq_el0)?;
    Ok(())
}

fn emit_a64_get_cntpct(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    ctx.reg_alloc
        .prepare_for_call(code, ctx.fpsr, [None, None, None, None])?;

    if !ctx.conf.wall_clock_cntpct && ctx.conf.enable_cycle_counting {
        code.write_u32(inst::ldr_x_unsigned(
            1,
            31,
            StackLayout::cycles_to_run_offset() as u32,
        ))?;
        code.write_u32(inst::sub_x_reg(1, 1, XTICKS))?;
        emit_relocation(code, ctx.emitted_block_info, LinkTarget::AddTicks)?;
        emit_relocation(code, ctx.emitted_block_info, LinkTarget::GetTicksRemaining)?;
        code.write_u32(inst::str_x_unsigned(
            0,
            31,
            StackLayout::cycles_to_run_offset() as u32,
        ))?;
        code.write_u32(inst::mov_x(XTICKS, 0))?;
    }

    emit_relocation(code, ctx.emitted_block_info, LinkTarget::GetCNTPCT)?;
    ctx.reg_alloc.define_as_register(
        ctx.block,
        inst_ref,
        HostLoc {
            kind: HostLocKind::Gpr,
            index: 0,
        },
    );
    Ok(())
}

fn emit_a64_get_ctr(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let mut value = ctx.reg_alloc.write_w(inst_ref);
    let value = value.realize(code, ctx.block)? as u8;
    emit_mov_w_imm(code, value, ctx.conf.ctr_el0)?;
    Ok(())
}

fn emit_a64_get_dczid(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let mut value = ctx.reg_alloc.write_w(inst_ref);
    let value = value.realize(code, ctx.block)? as u8;
    emit_mov_w_imm(code, value, ctx.conf.dczid_el0)?;
    Ok(())
}

fn emit_a64_set_tpidr(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    if ctx.conf.tpidr_el0.is_null() {
        return Err("A64SetTPIDR emitted without tpidr_el0 backing pointer".to_string());
    }

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut value = ctx.reg_alloc.read_x(args[0]);
    let value = value.realize(code, ctx.block)? as u8;
    emit_mov_x_imm(code, XSCRATCH0, ctx.conf.tpidr_el0 as u64)?;
    code.write_u32(inst::str_x_unsigned(value, XSCRATCH0, 0))?;
    Ok(())
}

fn emit_load_system_u64_pointer(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    ptr: u64,
) -> Result<(), String> {
    if ptr == 0 {
        return Err("A64 system register emitted without backing pointer".to_string());
    }

    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let result = result.realize(code, ctx.block)? as u8;
    emit_mov_x_imm(code, XSCRATCH0, ptr)?;
    code.write_u32(inst::ldr_x_unsigned(result, XSCRATCH0, 0))?;
    Ok(())
}

fn emit_push_rsb(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    if !ctx
        .conf
        .has_optimization(OptimizationFlag::RETURN_STACK_BUFFER)
    {
        return Ok(());
    }

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    if !args[0].is_immediate() {
        return Err("ARM64 PushRSB target must be immediate".to_string());
    }
    let target = LocationDescriptor::new(args[0].get_immediate_u64());

    code.write_u32(inst::ldr_w_unsigned(
        XSCRATCH2,
        31,
        StackLayout::rsb_ptr_offset() as u32,
    ))?;
    code.write_u32(inst::add_w_imm(
        XSCRATCH2,
        XSCRATCH2,
        core::mem::size_of::<RSBEntry>() as u32,
    ))?;
    code.write_u32(inst::and_w_imm(XSCRATCH2, XSCRATCH2, RSB_INDEX_MASK as u32))?;
    code.write_u32(inst::str_w_unsigned(
        XSCRATCH2,
        31,
        StackLayout::rsb_ptr_offset() as u32,
    ))?;
    code.write_u32(inst::add_x_reg_sp(XSCRATCH2, 31, XSCRATCH2))?;

    emit_mov_x_imm(code, XSCRATCH0, target.value())?;
    emit_block_link_relocation(
        code,
        ctx.emitted_block_info,
        target,
        BlockRelocationType::MoveToScratch1,
    )?;
    code.write_u32(inst::stp_x_offset(
        XSCRATCH0,
        XSCRATCH1,
        XSCRATCH2,
        StackLayout::rsb_offset() as i32,
    ))?;
    Ok(())
}

fn emit_get_c_flag_from_nzcv(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut carry = ctx.reg_alloc.write_w(inst_ref);
    let mut nzcv = ctx.reg_alloc.read_w(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut carry, &mut nzcv])?;
    code.write_u32(inst::and_w_imm(
        carry.index().expect("realized W carry") as u8,
        nzcv.index().expect("realized W NZCV") as u8,
        1 << 29,
    ))?;
    Ok(())
}

fn optional_argument(arg: Argument) -> Option<Argument> {
    (!arg.is_void()).then_some(arg)
}

fn emit_add_cycles(
    code: &mut BlockOfCode,
    ctx: &EmitContext<'_>,
    cycles_to_add: u64,
) -> Result<(), String> {
    if !ctx.conf.enable_cycle_counting || cycles_to_add == 0 {
        return Ok(());
    }
    if cycles_to_add < 4096 {
        code.write_u32(inst::sub_x_imm(XTICKS, XTICKS, cycles_to_add as u32))?;
    } else if cycles_to_add & 0xfff == 0 && cycles_to_add >> 12 < 4096 {
        code.write_u32(inst::sub_x_imm_shift(
            XTICKS,
            XTICKS,
            (cycles_to_add >> 12) as u32,
            true,
        ))?;
    } else {
        emit_mov_x_imm(code, XSCRATCH1, cycles_to_add)?;
        code.write_u32(inst::sub_x_reg(XTICKS, XTICKS, XSCRATCH1))?;
    }
    Ok(())
}

fn emit_mov_x_imm(code: &mut BlockOfCode, reg: u8, imm: u64) -> Result<(), String> {
    code.write_u32(inst::movz_x(reg, (imm & 0xffff) as u16, 0))?;
    for shift in [16, 32, 48] {
        let part = ((imm >> shift) & 0xffff) as u16;
        if part != 0 {
            code.write_u32(inst::movk_x(reg, part, shift as u8))?;
        }
    }
    Ok(())
}

fn emit_mov_w_imm(code: &mut BlockOfCode, reg: u8, imm: u32) -> Result<(), String> {
    code.write_u32(inst::movz_w(reg, (imm & 0xffff) as u16, 0))?;
    let part = ((imm >> 16) & 0xffff) as u16;
    if part != 0 {
        code.write_u32(inst::movk_w(reg, part, 16))?;
    }
    Ok(())
}

/// Upstream owner: `backend/arm64/emit_arm64.cpp::EmitRelocation`.
pub fn emit_relocation(
    code: &mut BlockOfCode,
    emitted_block_info: &mut EmittedBlockInfo,
    link_target: LinkTarget,
) -> Result<(), String> {
    emitted_block_info.relocations.push(Relocation {
        code_offset: emitted_block_offset(code, emitted_block_info)?,
        target: link_target,
    });
    code.write_u32(inst::nop())?;
    Ok(())
}

/// Upstream owner: `backend/arm64/emit_arm64.cpp::EmitBlockLinkRelocation`.
pub fn emit_block_link_relocation(
    code: &mut BlockOfCode,
    emitted_block_info: &mut EmittedBlockInfo,
    descriptor: LocationDescriptor,
    relocation_type: BlockRelocationType,
) -> Result<(), String> {
    let code_offset = emitted_block_offset(code, emitted_block_info)?;
    emitted_block_info
        .block_relocations
        .entry(descriptor)
        .or_default()
        .push(BlockRelocation {
            code_offset,
            relocation_type,
        });

    match relocation_type {
        BlockRelocationType::Branch => {
            code.write_u32(inst::nop())?;
        }
        BlockRelocationType::MoveToScratch1 => {
            code.write_u32(inst::brk(0))?;
            code.write_u32(inst::nop())?;
        }
    }
    Ok(())
}

fn emitted_block_offset(
    code: &BlockOfCode,
    emitted_block_info: &EmittedBlockInfo,
) -> Result<isize, String> {
    let current = unsafe { code.code_base_ptr().add(code.code_size()) } as isize;
    let offset = current
        .checked_sub(emitted_block_info.entry_point as isize)
        .ok_or_else(|| "ARM64 emitted-block relocation offset overflow".to_string())?;
    if offset < 0 {
        return Err(format!("ARM64 relocation before emitted block: {offset}"));
    }
    Ok(offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::types::{Reg as A64Reg, Vec as A64Vec};
    use crate::ir::acc_type::AccType;
    use crate::ir::inst::Inst;
    use crate::ir::terminal::Terminal;
    use crate::ir::value::Value;
    use crate::jit_config::UserCallbacks;
    use std::collections::HashMap;

    struct DummyCallbacks;

    impl UserCallbacks for DummyCallbacks {
        fn memory_read_code(&self, _vaddr: u64) -> Option<u32> {
            None
        }

        fn memory_read_8(&self, _vaddr: u64) -> u8 {
            0
        }

        fn memory_read_16(&self, _vaddr: u64) -> u16 {
            0
        }

        fn memory_read_32(&self, _vaddr: u64) -> u32 {
            0
        }

        fn memory_read_64(&self, _vaddr: u64) -> u64 {
            0
        }

        fn memory_read_128(&self, _vaddr: u64) -> (u64, u64) {
            (0, 0)
        }

        fn memory_write_8(&mut self, _vaddr: u64, _value: u8) {}
        fn memory_write_16(&mut self, _vaddr: u64, _value: u16) {}
        fn memory_write_32(&mut self, _vaddr: u64, _value: u32) {}
        fn memory_write_64(&mut self, _vaddr: u64, _value: u64) {}
        fn memory_write_128(&mut self, _vaddr: u64, _value_lo: u64, _value_hi: u64) {}

        fn exclusive_write_8(&mut self, _vaddr: u64, _value: u8, _expected: u8) -> bool {
            false
        }

        fn exclusive_write_16(&mut self, _vaddr: u64, _value: u16, _expected: u16) -> bool {
            false
        }

        fn exclusive_write_32(&mut self, _vaddr: u64, _value: u32, _expected: u32) -> bool {
            false
        }

        fn exclusive_write_64(&mut self, _vaddr: u64, _value: u64, _expected: u64) -> bool {
            false
        }

        fn exclusive_write_128(
            &mut self,
            _vaddr: u64,
            _value_lo: u64,
            _value_hi: u64,
            _expected_lo: u64,
            _expected_hi: u64,
        ) -> bool {
            false
        }

        fn call_supervisor(&mut self, _svc_num: u32) {}
        fn exception_raised(&mut self, _pc: u64, _exception: u64) {}
        fn add_ticks(&mut self, _ticks: u64) {}

        fn get_ticks_remaining(&self) -> u64 {
            0
        }
    }

    fn config(unsafe_optimizations: bool) -> JitConfig {
        JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(DummyCallbacks),
            enable_cycle_counting: true,
            code_cache_size: 0,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS
                | OptimizationFlag::UNSAFE_IGNORE_GLOBAL_MONITOR,
            unsafe_optimizations,
            global_monitor: None,
            fastmem_pointer: Some(0x1000 as *mut u8),
            page_table_pointer: Some(0x2000 as *const u8),
            define_unpredictable_behaviour: false,
            arch_version: crate::interface::a32::arch_version::ArchVersion::V8,
            hook_hint_instructions: false,
            processor_id: 3,
            wall_clock_cntpct: true,
            cntfrq_el0: 600_000_000,
            ctr_el0: 0x8444_c004,
            dczid_el0: 4,
            hook_data_cache_operations: false,
            hook_isb: false,
            tpidrro_el0: None,
            tpidr_el0: None,
            memory: MemoryEmitConfig::default(),
        }
    }

    fn empty_block_info(code: &BlockOfCode) -> EmittedBlockInfo {
        EmittedBlockInfo {
            entry_point: code.code_base_ptr(),
            size: 0,
            relocations: Vec::new(),
            block_relocations: FastHashMap::default(),
            fastmem_patch_info: FastHashMap::default(),
        }
    }

    fn read_instruction(code: &BlockOfCode, offset: usize) -> u32 {
        unsafe {
            code.code_base_ptr()
                .add(offset)
                .cast::<u32>()
                .read_unaligned()
        }
    }

    fn test_gpr(index: usize) -> u8 {
        crate::backend::arm64::abi::GPR_ORDER[index] as u8
    }

    fn return_to_dispatch_block() -> Block {
        let mut block = Block::new(LocationDescriptor::new(0x4000));
        block.terminal = Terminal::ReturnToDispatch;
        block
    }

    #[test]
    fn emit_arm64_routes_scalar_saturation_and_overflow_results() {
        for opcode in [
            Opcode::SignedSaturatedAddWithFlag32,
            Opcode::SignedSaturatedSubWithFlag32,
        ] {
            let mut block = return_to_dispatch_block();
            let a = block.append(Opcode::A64GetW, &[Value::ImmA64Reg(A64Reg::R0)]);
            let b = block.append(Opcode::A64GetW, &[Value::ImmA64Reg(A64Reg::R1)]);
            let result = block.append(opcode, &[Value::Inst(a), Value::Inst(b)]);
            let overflow = block.append(Opcode::GetOverflowFromOp, &[Value::Inst(result)]);
            block.append(
                Opcode::A64SetW,
                &[Value::ImmA64Reg(A64Reg::R2), Value::Inst(result)],
            );
            block.append(
                Opcode::A64SetW,
                &[Value::ImmA64Reg(A64Reg::R3), Value::Inst(overflow)],
            );
            block.rebuild_pseudo_op_links();

            let mut code = BlockOfCode::with_size(4096).unwrap();
            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap();
        }

        for (opcode, bit_size) in [
            (Opcode::SignedSaturation, 16),
            (Opcode::SignedSaturation, 32),
            (Opcode::UnsignedSaturation, 8),
        ] {
            let mut block = return_to_dispatch_block();
            let operand = block.append(Opcode::A64GetW, &[Value::ImmA64Reg(A64Reg::R0)]);
            let result = block.append(opcode, &[Value::Inst(operand), Value::ImmU8(bit_size)]);
            let overflow = block.append(Opcode::GetOverflowFromOp, &[Value::Inst(result)]);
            block.append(
                Opcode::A64SetW,
                &[Value::ImmA64Reg(A64Reg::R1), Value::Inst(result)],
            );
            block.append(
                Opcode::A64SetW,
                &[Value::ImmA64Reg(A64Reg::R2), Value::Inst(overflow)],
            );
            block.rebuild_pseudo_op_links();

            let mut code = BlockOfCode::with_size(4096).unwrap();
            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap();
        }
    }

    #[test]
    fn emit_arm64_routes_all_eden_packed_opcodes() {
        let binary_opcodes = [
            Opcode::PackedAddU8,
            Opcode::PackedAddS8,
            Opcode::PackedSubU8,
            Opcode::PackedSubS8,
            Opcode::PackedAddU16,
            Opcode::PackedAddS16,
            Opcode::PackedSubU16,
            Opcode::PackedSubS16,
            Opcode::PackedAddSubU16,
            Opcode::PackedAddSubS16,
            Opcode::PackedSubAddU16,
            Opcode::PackedSubAddS16,
            Opcode::PackedHalvingAddU8,
            Opcode::PackedHalvingAddS8,
            Opcode::PackedHalvingSubU8,
            Opcode::PackedHalvingSubS8,
            Opcode::PackedHalvingAddU16,
            Opcode::PackedHalvingAddS16,
            Opcode::PackedHalvingSubU16,
            Opcode::PackedHalvingSubS16,
            Opcode::PackedHalvingAddSubU16,
            Opcode::PackedHalvingAddSubS16,
            Opcode::PackedHalvingSubAddU16,
            Opcode::PackedHalvingSubAddS16,
            Opcode::PackedSaturatedAddU8,
            Opcode::PackedSaturatedAddS8,
            Opcode::PackedSaturatedSubU8,
            Opcode::PackedSaturatedSubS8,
            Opcode::PackedSaturatedAddU16,
            Opcode::PackedSaturatedAddS16,
            Opcode::PackedSaturatedSubU16,
            Opcode::PackedSaturatedSubS16,
            Opcode::PackedAbsDiffSumU8,
        ];

        for opcode in binary_opcodes {
            let mut block = return_to_dispatch_block();
            block.append(
                opcode,
                &[Value::ImmU32(0x1020_3040), Value::ImmU32(0x0102_0304)],
            );
            let mut code = BlockOfCode::with_size(4096).unwrap();

            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap_or_else(|error| panic!("{opcode:?} failed ARM64 emission: {error}"));
        }

        let mut block = return_to_dispatch_block();
        block.append(
            Opcode::PackedSelect,
            &[
                Value::ImmU32(0x00ff_00ff),
                Value::ImmU32(0x1122_3344),
                Value::ImmU32(0xaabb_ccdd),
            ],
        );
        let mut code = BlockOfCode::with_size(4096).unwrap();
        emit_arm64(
            &mut code,
            block,
            EmitConfig::from_a64_config(&config(false)),
        )
        .expect("PackedSelect must be routed to the packed emitter");
    }

    #[test]
    fn emit_arm64_packed_add_u8_emits_eden_ge_sequence() {
        let mut block = return_to_dispatch_block();
        let result = block.append(
            Opcode::PackedAddU8,
            &[Value::ImmU32(0xffff_00ff), Value::ImmU32(0x0102_0304)],
        );
        let ge = block.append(Opcode::GetGEFromOp, &[Value::Inst(result)]);
        block.append(
            Opcode::A64SetW,
            &[Value::ImmA64Reg(A64Reg::R0), Value::Inst(result)],
        );
        block.append(
            Opcode::A64SetW,
            &[Value::ImmA64Reg(A64Reg::R1), Value::Inst(ge)],
        );
        block.rebuild_pseudo_op_links();
        let mut code = BlockOfCode::with_size(4096).unwrap();

        let info = emit_arm64(
            &mut code,
            block,
            EmitConfig::from_a64_config(&config(false)),
        )
        .unwrap();
        let words = (0..info.size)
            .step_by(4)
            .map(|offset| read_instruction(&code, offset))
            .collect::<Vec<_>>();

        assert!(words.contains(&inst::add_v(8, 9, 10, 8, false)));
        assert!(words.contains(&inst::cmhi_v(11, 9, 8, 8, false)));
    }

    #[test]
    fn emit_arm64_packed_complex_sequences_match_eden_scratch_usage() {
        let mut block = return_to_dispatch_block();
        block.append(
            Opcode::PackedAddSubU16,
            &[Value::ImmU32(0x1020_3040), Value::ImmU32(0x0102_0304)],
        );
        block.append(
            Opcode::PackedSaturatedAddU16,
            &[Value::ImmU32(0xffff_ffff), Value::ImmU32(0x0001_0001)],
        );
        block.append(
            Opcode::PackedAbsDiffSumU8,
            &[Value::ImmU32(0x1020_3040), Value::ImmU32(0x0102_0304)],
        );
        block.append(
            Opcode::PackedSelect,
            &[
                Value::ImmU32(0x00ff_00ff),
                Value::ImmU32(0x1122_3344),
                Value::ImmU32(0xaabb_ccdd),
            ],
        );
        let mut code = BlockOfCode::with_size(4096).unwrap();

        let info = emit_arm64(
            &mut code,
            block,
            EmitConfig::from_a64_config(&config(false)),
        )
        .unwrap();
        let words = (0..info.size)
            .step_by(4)
            .map(|offset| read_instruction(&code, offset))
            .collect::<Vec<_>>();

        assert!(words.contains(&inst::uxtl_v(0, 9, 16)));
        assert!(words.contains(&inst::uxtl_v(1, 10, 16)));
        assert!(words.contains(&inst::ext_v16b(1, 1, 1, 4, false)));
        assert!(words.contains(&inst::movi_v8b_imm(2, 0xf0)));
        assert!(words.contains(&inst::eor_v8b(1, 1, 2)));
        assert!(words.contains(&inst::sub_v(8, 0, 1, 32, false)));
        assert!(words.contains(&inst::xtn_v(8, 8, 32)));
        assert!(words.iter().any(|word| {
            *word == inst::uqadd_v(8, 9, 10, 16, false)
                || *word == inst::uqadd_v(9, 10, 11, 16, false)
        }));
        assert!(words.contains(&inst::uabd_v(8, 9, 10, 8, false)));
        assert!(words.contains(&inst::and_v8b(8, 8, 2)));
        assert!(words.contains(&inst::uaddlv_from_v(8, 8, 8, false)));
        assert!(words.contains(&inst::fmov_d(8, 9)));
        assert!(words.contains(&inst::bsl_v8b(8, 11, 10)));
    }

    #[test]
    fn masks_unsafe_optimizations_unless_enabled() {
        let safe = EmitConfig::from_a64_config(&config(false));
        assert!(!safe.has_optimization(OptimizationFlag::UNSAFE_IGNORE_GLOBAL_MONITOR));

        let unsafe_enabled = EmitConfig::from_a64_config(&config(true));
        assert!(unsafe_enabled.has_optimization(OptimizationFlag::UNSAFE_IGNORE_GLOBAL_MONITOR));
    }

    #[test]
    fn emit_relocation_records_offset_and_writes_nop() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut info = empty_block_info(&code);
        code.write_u32(inst::nop()).unwrap();

        emit_relocation(&mut code, &mut info, LinkTarget::ReturnToDispatcher).unwrap();

        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 4,
                target: LinkTarget::ReturnToDispatcher,
            }]
        );
        assert_eq!(read_instruction(&code, 4), inst::nop());
    }

    #[test]
    fn emit_block_link_relocation_branch_records_offset_and_writes_nop() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut info = empty_block_info(&code);
        let target = LocationDescriptor::new(0x4000);

        emit_block_link_relocation(&mut code, &mut info, target, BlockRelocationType::Branch)
            .unwrap();

        assert_eq!(read_instruction(&code, 0), inst::nop());
        assert_eq!(
            info.block_relocations[&target],
            vec![BlockRelocation {
                code_offset: 0,
                relocation_type: BlockRelocationType::Branch,
            }]
        );
    }

    #[test]
    fn emit_block_link_relocation_move_to_scratch1_writes_brk_then_nop() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut info = empty_block_info(&code);
        let target = LocationDescriptor::new(0x8000);

        emit_block_link_relocation(
            &mut code,
            &mut info,
            target,
            BlockRelocationType::MoveToScratch1,
        )
        .unwrap();

        assert_eq!(read_instruction(&code, 0), inst::brk(0));
        assert_eq!(read_instruction(&code, 4), inst::nop());
        assert_eq!(
            info.block_relocations[&target],
            vec![BlockRelocation {
                code_offset: 0,
                relocation_type: BlockRelocationType::MoveToScratch1,
            }]
        );
    }

    #[test]
    fn emit_push_rsb_matches_upstream_stack_update_and_relocation_order() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = return_to_dispatch_block();
        let target = LocationDescriptor::new(0);
        block
            .instructions
            .push(Inst::new(Opcode::PushRSB, &[Value::ImmU64(target.value())]));

        let info = emit_arm64(
            &mut code,
            block,
            EmitConfig::from_a32_config(&config(false)),
        )
        .expect("PushRSB block should emit");

        assert_eq!(
            read_instruction(&code, 0),
            inst::ldr_w_unsigned(XSCRATCH2, 31, StackLayout::rsb_ptr_offset() as u32)
        );
        assert_eq!(
            read_instruction(&code, 4),
            inst::add_w_imm(
                XSCRATCH2,
                XSCRATCH2,
                core::mem::size_of::<RSBEntry>() as u32
            )
        );
        assert_eq!(
            read_instruction(&code, 8),
            inst::and_w_imm(XSCRATCH2, XSCRATCH2, RSB_INDEX_MASK as u32)
        );
        assert_eq!(
            read_instruction(&code, 12),
            inst::str_w_unsigned(XSCRATCH2, 31, StackLayout::rsb_ptr_offset() as u32)
        );
        assert_eq!(
            read_instruction(&code, 16),
            inst::add_x_reg_sp(XSCRATCH2, 31, XSCRATCH2)
        );
        assert_eq!(read_instruction(&code, 20), inst::movz_x(XSCRATCH0, 0, 0));
        assert_eq!(read_instruction(&code, 24), inst::brk(0));
        assert_eq!(read_instruction(&code, 28), inst::nop());
        assert_eq!(
            read_instruction(&code, 32),
            inst::stp_x_offset(
                XSCRATCH0,
                XSCRATCH1,
                XSCRATCH2,
                StackLayout::rsb_offset() as i32
            )
        );
        assert_eq!(
            info.block_relocations[&target],
            vec![BlockRelocation {
                code_offset: 24,
                relocation_type: BlockRelocationType::MoveToScratch1,
            }]
        );
    }

    #[test]
    fn a32_emit_config_forces_32_bit_mirrored_memory_spaces() {
        let cfg = EmitConfig::from_a32_config(&config(false));
        assert_eq!(cfg.memory.fastmem_address_space_bits, 32);
        assert_eq!(cfg.memory.page_table_address_space_bits, 32);
        assert!(cfg.memory.silently_mirror_fastmem);
        assert!(cfg.memory.silently_mirror_page_table);
        assert_eq!(cfg.fastmem_address_space_bits, 32);
        assert_eq!(cfg.page_table_address_space_bits, 32);
        assert!(cfg.silently_mirror_fastmem);
        assert!(cfg.silently_mirror_page_table);
        assert_eq!(cfg.fastmem_pointer, 0x1000);
        assert_eq!(cfg.page_table_pointer, 0x2000);
        assert_eq!(cfg.cntfreq_el0, 0);
        assert_eq!(cfg.ctr_el0, 0);
        assert_eq!(cfg.dczid_el0, 0);
        assert!(cfg.tpidrro_el0.is_null());
        assert!(cfg.tpidr_el0.is_null());
        assert_eq!(cfg.memory.processor_id, 3);
        assert!(cfg.wall_clock_cntpct);
        assert!(cfg.enable_cycle_counting);
        assert!(std::ptr::fn_addr_eq(
            cfg.emit_cond,
            emit_a32_cond as EmitCond
        ));
        assert!(std::ptr::fn_addr_eq(
            cfg.emit_check_memory_abort,
            emit_a32_check_memory_abort as EmitCheckMemoryAbort
        ));
        assert_eq!(
            cfg.state_nzcv_offset,
            core::mem::offset_of!(A32JitState, cpsr_nzcv)
        );
        assert_eq!(
            cfg.state_fpsr_offset,
            core::mem::offset_of!(A32JitState, fpsr)
        );
        assert_eq!(
            cfg.state_exclusive_state_offset,
            core::mem::offset_of!(A32JitState, exclusive_state)
        );
    }

    #[test]
    fn a64_emit_config_preserves_memory_and_system_defaults() {
        let mut config = config(false);
        config.cntfrq_el0 = 19_200_000;
        config.memory.fastmem_address_space_bits = 39;
        config.memory.silently_mirror_fastmem = false;
        config.memory.recompile_on_fastmem_failure = true;
        config.memory.page_table_address_space_bits = 40;
        config.memory.page_table_pointer_mask_bits = 3;
        config.memory.silently_mirror_page_table = false;
        config.memory.absolute_offset_page_table = true;
        config.memory.detect_misaligned_access_via_page_table = 16 | 32 | 64;
        config
            .memory
            .only_detect_misalignment_via_page_table_on_page_boundary = true;
        config.memory.check_halt_on_memory_access = true;

        let cfg = EmitConfig::from_a64_config(&config);

        assert_eq!(cfg.cntfreq_el0, 19_200_000);
        assert_eq!(cfg.ctr_el0, 0x8444_c004);
        assert_eq!(cfg.dczid_el0, 4);
        assert_eq!(cfg.fastmem_address_space_bits, 39);
        assert!(!cfg.silently_mirror_fastmem);
        assert!(cfg.recompile_on_fastmem_failure);
        assert_eq!(cfg.page_table_address_space_bits, 40);
        assert_eq!(cfg.page_table_pointer_mask_bits, 3);
        assert!(!cfg.silently_mirror_page_table);
        assert!(cfg.absolute_offset_page_table);
        assert_eq!(cfg.detect_misaligned_access_via_page_table, 16 | 32 | 64);
        assert!(cfg.only_detect_misalignment_via_page_table_on_page_boundary);
        assert!(cfg.check_halt_on_memory_access);
        assert!(std::ptr::fn_addr_eq(
            cfg.emit_cond,
            emit_a64_cond as EmitCond
        ));
        assert!(std::ptr::fn_addr_eq(
            cfg.emit_check_memory_abort,
            emit_a64_check_memory_abort as EmitCheckMemoryAbort
        ));
        assert_eq!(
            cfg.state_nzcv_offset,
            core::mem::offset_of!(A64JitState, cpsr_nzcv)
        );
        assert_eq!(
            cfg.state_fpsr_offset,
            core::mem::offset_of!(A64JitState, fpsr)
        );
        assert_eq!(
            cfg.state_exclusive_state_offset,
            core::mem::offset_of!(A64JitState, exclusive_state)
        );
    }

    #[test]
    fn emit_arm64_empty_a64_block_returns_to_dispatcher() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let info = emit_arm64(
            &mut code,
            return_to_dispatch_block(),
            EmitConfig::from_a64_config(&config(false)),
        )
        .unwrap();

        assert_eq!(info.entry_point, code.code_base_ptr());
        assert_eq!(info.size, 12);
        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 0,
                target: LinkTarget::ReturnToDispatcher,
            }]
        );
        assert_eq!(read_instruction(&code, 0), inst::nop());
        assert_eq!(read_instruction(&code, 4), inst::brk(0));
        assert_eq!(read_instruction(&code, 8), inst::brk(0));
    }

    #[test]
    fn emit_arm64_breakpoint_then_returns_to_dispatcher() {
        let mut block = return_to_dispatch_block();
        block.append(Opcode::Breakpoint, &[]);
        let mut code = BlockOfCode::with_size(4096).unwrap();

        let info = emit_arm64(
            &mut code,
            block,
            EmitConfig::from_a64_config(&config(false)),
        )
        .unwrap();

        assert_eq!(info.size, 16);
        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 4,
                target: LinkTarget::ReturnToDispatcher,
            }]
        );
        assert_eq!(read_instruction(&code, 0), inst::brk(0));
        assert_eq!(read_instruction(&code, 4), inst::nop());
        assert_eq!(read_instruction(&code, 8), inst::brk(0));
        assert_eq!(read_instruction(&code, 12), inst::brk(0));
    }

    #[test]
    fn emit_arm64_packs_two_gprs_into_one_q_register() {
        let mut block = return_to_dispatch_block();
        let lo = block.append(Opcode::A64GetX, &[Value::ImmA64Reg(A64Reg::R0)]);
        let hi = block.append(Opcode::A64GetX, &[Value::ImmA64Reg(A64Reg::R1)]);
        block.append(Opcode::Pack2x64To1x128, &[Value::Inst(lo), Value::Inst(hi)]);
        let mut code = BlockOfCode::with_size(4096).unwrap();

        emit_arm64(
            &mut code,
            block,
            EmitConfig::from_a64_config(&config(false)),
        )
        .unwrap();

        let lo_reg = test_gpr(0);
        let hi_reg = test_gpr(1);
        let result_reg = crate::backend::arm64::abi::FPR_ORDER[0] as u8;
        assert_eq!(
            read_instruction(&code, 0),
            inst::ldr_x_unsigned(lo_reg, XSTATE, 0)
        );
        assert_eq!(
            read_instruction(&code, 4),
            inst::ldr_x_unsigned(hi_reg, XSTATE, 8)
        );
        assert_eq!(
            read_instruction(&code, 8),
            inst::fmov_d_from_x(result_reg, lo_reg)
        );
        assert_eq!(
            read_instruction(&code, 12),
            inst::fmov_v_d1_from_x(result_reg, hi_reg)
        );
    }

    #[test]
    fn emit_arm64_routes_fp_vector_abs_to_vector_fp_owner() {
        for opcode in [
            Opcode::FPVectorAbs16,
            Opcode::FPVectorAbs32,
            Opcode::FPVectorAbs64,
        ] {
            let mut block = return_to_dispatch_block();
            let input = block.append(Opcode::A64GetQ, &[Value::ImmA64Vec(A64Vec::V0)]);
            block.append(opcode, &[Value::Inst(input)]);
            let mut code = BlockOfCode::with_size(4096).unwrap();

            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap();
        }
    }

    #[test]
    fn emit_arm64_routes_aes_to_cryptography_owner() {
        for opcode in [
            Opcode::AESDecryptSingleRound,
            Opcode::AESEncryptSingleRound,
            Opcode::AESInverseMixColumns,
            Opcode::AESMixColumns,
        ] {
            let mut block = return_to_dispatch_block();
            let input = block.append(Opcode::A64GetQ, &[Value::ImmA64Vec(A64Vec::V0)]);
            block.append(opcode, &[Value::Inst(input)]);
            let mut code = BlockOfCode::with_size(4096).unwrap();

            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap();
        }
    }

    #[test]
    fn emit_arm64_routes_crc32_to_cryptography_owner() {
        for (opcode, data) in [
            (Opcode::CRC32Castagnoli8, Value::ImmU32(0x12)),
            (Opcode::CRC32Castagnoli16, Value::ImmU32(0x1234)),
            (Opcode::CRC32Castagnoli32, Value::ImmU32(0x1234_5678)),
            (
                Opcode::CRC32Castagnoli64,
                Value::ImmU64(0x1234_5678_9abc_def0),
            ),
            (Opcode::CRC32ISO8, Value::ImmU32(0x12)),
            (Opcode::CRC32ISO16, Value::ImmU32(0x1234)),
            (Opcode::CRC32ISO32, Value::ImmU32(0x1234_5678)),
            (Opcode::CRC32ISO64, Value::ImmU64(0x1234_5678_9abc_def0)),
        ] {
            let mut block = return_to_dispatch_block();
            let input = block.append(Opcode::A64GetW, &[Value::ImmA64Reg(A64Reg::R0)]);
            block.append(opcode, &[Value::Inst(input), data]);
            let mut code = BlockOfCode::with_size(4096).unwrap();

            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap();
        }
    }

    #[test]
    fn emit_arm64_routes_sha256_to_cryptography_owner() {
        let mut block = return_to_dispatch_block();
        let x = block.append(Opcode::A64GetQ, &[Value::ImmA64Vec(A64Vec::V0)]);
        let y = block.append(Opcode::A64GetQ, &[Value::ImmA64Vec(A64Vec::V1)]);
        let w = block.append(Opcode::A64GetQ, &[Value::ImmA64Vec(A64Vec::V2)]);
        block.append(
            Opcode::SHA256Hash,
            &[
                Value::Inst(x),
                Value::Inst(y),
                Value::Inst(w),
                Value::ImmU1(true),
            ],
        );
        block.append(
            Opcode::SHA256MessageSchedule0,
            &[Value::Inst(x), Value::Inst(y)],
        );
        block.append(
            Opcode::SHA256MessageSchedule1,
            &[Value::Inst(x), Value::Inst(y), Value::Inst(w)],
        );
        let mut code = BlockOfCode::with_size(4096).unwrap();

        emit_arm64(
            &mut code,
            block,
            EmitConfig::from_a64_config(&config(false)),
        )
        .unwrap();
    }

    #[test]
    fn emit_arm64_masked_shifts_accept_full_width_immediates() {
        for (opcode, get_opcode, input, shift) in [
            (
                Opcode::LogicalShiftLeftMasked32,
                Opcode::A64GetW,
                Value::ImmA64Reg(A64Reg::R0),
                Value::ImmU32(0x1234_567f),
            ),
            (
                Opcode::LogicalShiftRightMasked32,
                Opcode::A64GetW,
                Value::ImmA64Reg(A64Reg::R0),
                Value::ImmU32(0x1234_567f),
            ),
            (
                Opcode::ArithmeticShiftRightMasked32,
                Opcode::A64GetW,
                Value::ImmA64Reg(A64Reg::R0),
                Value::ImmU32(0x1234_567f),
            ),
            (
                Opcode::RotateRightMasked32,
                Opcode::A64GetW,
                Value::ImmA64Reg(A64Reg::R0),
                Value::ImmU32(0x1234_567f),
            ),
            (
                Opcode::LogicalShiftLeftMasked64,
                Opcode::A64GetX,
                Value::ImmA64Reg(A64Reg::R0),
                Value::ImmU64(0x1234_5678_9abc_deff),
            ),
            (
                Opcode::LogicalShiftRightMasked64,
                Opcode::A64GetX,
                Value::ImmA64Reg(A64Reg::R0),
                Value::ImmU64(0x1234_5678_9abc_deff),
            ),
            (
                Opcode::ArithmeticShiftRightMasked64,
                Opcode::A64GetX,
                Value::ImmA64Reg(A64Reg::R0),
                Value::ImmU64(0x1234_5678_9abc_deff),
            ),
            (
                Opcode::RotateRightMasked64,
                Opcode::A64GetX,
                Value::ImmA64Reg(A64Reg::R0),
                Value::ImmU64(0x1234_5678_9abc_deff),
            ),
        ] {
            let mut block = return_to_dispatch_block();
            let input = block.append(get_opcode, &[input]);
            block.append(opcode, &[Value::Inst(input), shift]);
            let mut code = BlockOfCode::with_size(4096).unwrap();

            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap();
        }
    }

    #[test]
    fn emit_arm64_routes_scalar_integer_min_max_to_data_processing_owner() {
        for (opcode, get_opcode) in [
            (Opcode::MaxSigned32, Opcode::A64GetW),
            (Opcode::MaxSigned64, Opcode::A64GetX),
            (Opcode::MaxUnsigned32, Opcode::A64GetW),
            (Opcode::MaxUnsigned64, Opcode::A64GetX),
            (Opcode::MinSigned32, Opcode::A64GetW),
            (Opcode::MinSigned64, Opcode::A64GetX),
            (Opcode::MinUnsigned32, Opcode::A64GetW),
            (Opcode::MinUnsigned64, Opcode::A64GetX),
        ] {
            let mut block = return_to_dispatch_block();
            let op1 = block.append(get_opcode, &[Value::ImmA64Reg(A64Reg::R0)]);
            let op2 = block.append(get_opcode, &[Value::ImmA64Reg(A64Reg::R1)]);
            block.append(opcode, &[Value::Inst(op1), Value::Inst(op2)]);
            let mut code = BlockOfCode::with_size(4096).unwrap();

            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap();
        }
    }

    #[test]
    fn emit_arm64_routes_scalar_fp_min_max_to_fp_owner() {
        for (opcode, get_opcode) in [
            (Opcode::FPMax32, Opcode::A64GetS),
            (Opcode::FPMin32, Opcode::A64GetS),
            (Opcode::FPMax64, Opcode::A64GetD),
            (Opcode::FPMin64, Opcode::A64GetD),
        ] {
            let mut block = return_to_dispatch_block();
            let a = block.append(get_opcode, &[Value::ImmA64Vec(A64Vec::V0)]);
            let b = block.append(get_opcode, &[Value::ImmA64Vec(A64Vec::V1)]);
            block.append(opcode, &[Value::Inst(a), Value::Inst(b)]);
            let mut code = BlockOfCode::with_size(4096).unwrap();

            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap();
        }
    }

    #[test]
    fn emit_arm64_routes_remaining_upstream_scalar_fp_operations() {
        for (opcode, get_opcode) in [
            (Opcode::FPMulX32, Opcode::A64GetS),
            (Opcode::FPMulX64, Opcode::A64GetD),
            (Opcode::FPRecipStepFused32, Opcode::A64GetS),
            (Opcode::FPRecipStepFused64, Opcode::A64GetD),
            (Opcode::FPRSqrtStepFused32, Opcode::A64GetS),
            (Opcode::FPRSqrtStepFused64, Opcode::A64GetD),
        ] {
            let mut block = return_to_dispatch_block();
            let a = block.append(get_opcode, &[Value::ImmA64Vec(A64Vec::V0)]);
            let b = block.append(get_opcode, &[Value::ImmA64Vec(A64Vec::V1)]);
            block.append(opcode, &[Value::Inst(a), Value::Inst(b)]);
            let mut code = BlockOfCode::with_size(4096).unwrap();
            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap();
        }

        for (opcode, get_opcode) in [
            (Opcode::FPRecipEstimate32, Opcode::A64GetS),
            (Opcode::FPRecipEstimate64, Opcode::A64GetD),
            (Opcode::FPRecipExponent32, Opcode::A64GetS),
            (Opcode::FPRecipExponent64, Opcode::A64GetD),
            (Opcode::FPRSqrtEstimate32, Opcode::A64GetS),
            (Opcode::FPRSqrtEstimate64, Opcode::A64GetD),
        ] {
            let mut block = return_to_dispatch_block();
            let input = block.append(get_opcode, &[Value::ImmA64Vec(A64Vec::V0)]);
            block.append(opcode, &[Value::Inst(input)]);
            let mut code = BlockOfCode::with_size(4096).unwrap();
            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap();
        }
    }

    #[test]
    fn emit_arm64_routes_half_and_fixed16_conversions() {
        for opcode in [Opcode::FPHalfToSingle, Opcode::FPHalfToDouble] {
            let mut block = return_to_dispatch_block();
            let input = block.append(Opcode::A64GetS, &[Value::ImmA64Vec(A64Vec::V0)]);
            let half = block.append(Opcode::LeastSignificantHalf, &[Value::Inst(input)]);
            block.append(opcode, &[Value::Inst(half), Value::ImmU8(0)]);
            let mut code = BlockOfCode::with_size(4096).unwrap();
            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap();
        }

        for (opcode, get_opcode) in [
            (Opcode::FPSingleToHalf, Opcode::A64GetS),
            (Opcode::FPDoubleToHalf, Opcode::A64GetD),
        ] {
            let mut block = return_to_dispatch_block();
            let input = block.append(get_opcode, &[Value::ImmA64Vec(A64Vec::V0)]);
            block.append(opcode, &[Value::Inst(input), Value::ImmU8(0)]);
            let mut code = BlockOfCode::with_size(4096).unwrap();
            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap();
        }

        for (opcode, get_opcode) in [
            (Opcode::FPSingleToFixedS16, Opcode::A64GetS),
            (Opcode::FPSingleToFixedU16, Opcode::A64GetS),
            (Opcode::FPDoubleToFixedS16, Opcode::A64GetD),
            (Opcode::FPDoubleToFixedU16, Opcode::A64GetD),
        ] {
            let mut block = return_to_dispatch_block();
            let input = block.append(get_opcode, &[Value::ImmA64Vec(A64Vec::V0)]);
            block.append(
                opcode,
                &[Value::Inst(input), Value::ImmU8(7), Value::ImmU8(3)],
            );
            let mut code = BlockOfCode::with_size(4096).unwrap();
            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap();
        }

        for opcode in [
            Opcode::FPFixedS16ToSingle,
            Opcode::FPFixedU16ToSingle,
            Opcode::FPFixedS16ToDouble,
            Opcode::FPFixedU16ToDouble,
        ] {
            let mut block = return_to_dispatch_block();
            let input = block.append(Opcode::A64GetW, &[Value::ImmA64Reg(A64Reg::R0)]);
            let half = block.append(Opcode::LeastSignificantHalf, &[Value::Inst(input)]);
            block.append(
                opcode,
                &[Value::Inst(half), Value::ImmU8(7), Value::ImmU8(0)],
            );
            let mut code = BlockOfCode::with_size(4096).unwrap();
            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap();
        }
    }

    #[test]
    fn emit_arm64_routes_remaining_upstream_vector_fp_operations() {
        for opcode in [Opcode::FPVectorMulX32, Opcode::FPVectorMulX64] {
            let mut block = return_to_dispatch_block();
            let a = block.append(Opcode::A64GetQ, &[Value::ImmA64Vec(A64Vec::V0)]);
            let b = block.append(Opcode::A64GetQ, &[Value::ImmA64Vec(A64Vec::V1)]);
            block.append(
                opcode,
                &[Value::Inst(a), Value::Inst(b), Value::ImmU1(true)],
            );
            let mut code = BlockOfCode::with_size(4096).unwrap();
            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap();
        }

        for opcode in [Opcode::FPVectorFromHalf32, Opcode::FPVectorToHalf32] {
            let mut block = return_to_dispatch_block();
            let input = block.append(Opcode::A64GetQ, &[Value::ImmA64Vec(A64Vec::V0)]);
            block.append(
                opcode,
                &[Value::Inst(input), Value::ImmU8(0), Value::ImmU1(true)],
            );
            let mut code = BlockOfCode::with_size(4096).unwrap();
            emit_arm64(
                &mut code,
                block,
                EmitConfig::from_a64_config(&config(false)),
            )
            .unwrap();
        }
    }

    #[test]
    fn emit_arm64_subtracts_small_cycle_counts_before_terminal() {
        let mut block = return_to_dispatch_block();
        block.cycle_count = 7;
        let mut code = BlockOfCode::with_size(4096).unwrap();

        let info = emit_arm64(
            &mut code,
            block,
            EmitConfig::from_a64_config(&config(false)),
        )
        .unwrap();

        assert_eq!(info.size, 16);
        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 4,
                target: LinkTarget::ReturnToDispatcher,
            }]
        );
        assert_eq!(
            read_instruction(&code, 0),
            inst::sub_x_imm(XTICKS, XTICKS, 7)
        );
        assert_eq!(read_instruction(&code, 4), inst::nop());
        assert_eq!(read_instruction(&code, 8), inst::brk(0));
        assert_eq!(read_instruction(&code, 12), inst::brk(0));
    }

    #[test]
    fn emit_arm64_uses_shifted_immediate_for_aligned_cycle_count() {
        let mut block = return_to_dispatch_block();
        block.cycle_count = 4096;
        let mut code = BlockOfCode::with_size(4096).unwrap();

        let info = emit_arm64(
            &mut code,
            block,
            EmitConfig::from_a64_config(&config(false)),
        )
        .unwrap();

        assert_eq!(info.size, 16);
        assert_eq!(
            read_instruction(&code, 0),
            inst::sub_x_imm_shift(XTICKS, XTICKS, 1, true)
        );
    }

    #[test]
    fn emit_arm64_uses_register_for_large_unaligned_cycle_count() {
        let mut block = return_to_dispatch_block();
        block.cycle_count = 4097;
        let mut code = BlockOfCode::with_size(4096).unwrap();

        let info = emit_arm64(
            &mut code,
            block,
            EmitConfig::from_a64_config(&config(false)),
        )
        .unwrap();

        assert_eq!(info.size, 20);
        assert_eq!(read_instruction(&code, 0), inst::movz_x(XSCRATCH1, 4097, 0));
        assert_eq!(
            read_instruction(&code, 4),
            inst::sub_x_reg(XTICKS, XTICKS, XSCRATCH1)
        );
    }

    #[test]
    fn emit_arm64_a64_set_pc_stores_immediate_pc_before_terminal() {
        let mut block = return_to_dispatch_block();
        block.append(Opcode::A64SetPC, &[Value::ImmU64(0x1234_5678)]);
        let mut code = BlockOfCode::with_size(4096).unwrap();

        let info = emit_arm64(
            &mut code,
            block,
            EmitConfig::from_a64_config(&config(false)),
        )
        .unwrap();

        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 12,
                target: LinkTarget::ReturnToDispatcher,
            }]
        );
        assert_eq!(
            read_instruction(&code, 0),
            inst::movz_x(test_gpr(0), 0x5678, 0)
        );
        assert_eq!(
            read_instruction(&code, 4),
            inst::movk_x(test_gpr(0), 0x1234, 16)
        );
        assert_eq!(
            read_instruction(&code, 8),
            inst::str_x_unsigned(
                test_gpr(0),
                crate::backend::arm64::abi::XSTATE,
                core::mem::offset_of!(A64JitState, pc) as u32
            )
        );
        assert_eq!(read_instruction(&code, 12), inst::nop());
        assert_eq!(read_instruction(&code, 16), inst::brk(0));
        assert_eq!(read_instruction(&code, 20), inst::brk(0));
    }

    #[test]
    fn emit_arm64_routes_a32_memory_ops_through_a32_memory_owner() {
        let mut block = return_to_dispatch_block();
        block.append(
            Opcode::A32ReadMemory32,
            &[
                Value::ImmU64(LocationDescriptor::new(0x4000).value()),
                Value::ImmU32(0x1234),
                Value::ImmAccType(AccType::Normal),
            ],
        );
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut jit_config = config(false);
        jit_config.fastmem_pointer = None;
        jit_config.page_table_pointer = None;

        let info = emit_arm64(&mut code, block, EmitConfig::from_a32_config(&jit_config)).unwrap();

        assert_eq!(info.size, 20);
        assert_eq!(
            info.relocations,
            vec![
                Relocation {
                    code_offset: 4,
                    target: LinkTarget::ReadMemory32,
                },
                Relocation {
                    code_offset: 8,
                    target: LinkTarget::ReturnToDispatcher,
                },
            ]
        );
        assert_eq!(read_instruction(&code, 0), inst::movz_x(1, 0x1234, 0));
        assert_eq!(read_instruction(&code, 4), inst::nop());
        assert_eq!(read_instruction(&code, 8), inst::nop());
        assert_eq!(read_instruction(&code, 12), inst::brk(0));
        assert_eq!(read_instruction(&code, 16), inst::brk(0));
    }
}

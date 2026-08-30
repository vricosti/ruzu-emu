//! A32-specific x64 memory emission.
//!
//! Structural counterpart of Eden's `backend/x64/a32_emit_x64_memory.cpp`.
//! The shared template behavior from `emit_x64_memory.cpp.inc` is expressed
//! through Rust helpers in this module.

use std::collections::HashMap;

use rxbyak::{
    byte_ptr, dword_ptr, qword_ptr, word_ptr, xmmword_ptr, CodeAssembler, JmpType, Label,
};
use rxbyak::{Reg, RegExp, R10, R11, R15, R8, R9, RAX, RCX, RDI, RDX, RSI, RSP};

use crate::backend::x64::a32_jitstate::A32JitState;
use crate::backend::x64::abi;
use crate::backend::x64::block_of_code::FORCE_RETURN;
use crate::backend::x64::emit_context::{EmitCallbacks, EmitContext, RawExclusiveWriteCallbacks};
use crate::backend::x64::emit_terminal::emit_jmp_to_offset;
use crate::backend::x64::emit_x64_memory::{
    emit_call_to_offset, emit_read_memory_mov, emit_vaddr_lookup_a32, emit_write_memory_mov,
    is_ordered,
};
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::hostloc::HostLoc;
use crate::backend::x64::perf_map;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::interface::a32::config::UserCallbacks as A32UserCallbacks;
use crate::interface::exclusive_monitor::ExclusiveMonitor;
use crate::interface::halt_reason::HaltReason;
use crate::ir::inst::Inst;
use crate::ir::location::{A32LocationDescriptor, LocationDescriptor};
use crate::ir::value::InstRef;

const VALID_GPR_IDXES: [u8; 14] = [0, 1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14];

/// A32-owned fallback maps corresponding to the three maps on Eden's
/// `A32EmitX64` class.
#[derive(Default)]
pub struct FastmemFallbacksTable {
    pub read: HashMap<(bool, usize, u8, u8), usize>,
    pub write: HashMap<(bool, usize, u8, u8), usize>,
    pub exclusive_write: HashMap<(bool, usize, u8, u8), usize>,
}

impl FastmemFallbacksTable {
    pub fn new() -> Self {
        Self::default()
    }

    fn read_stub(&self, ordered: bool, bitsize: usize, vaddr_idx: u8, value_idx: u8) -> usize {
        *self
            .read
            .get(&(ordered, bitsize, vaddr_idx, value_idx))
            .unwrap_or_else(|| {
                panic!(
                    "no A32 read fallback stub for (ordered={}, bitsize={}, vaddr={}, value={})",
                    ordered, bitsize, vaddr_idx, value_idx
                )
            })
    }

    fn write_stub(&self, ordered: bool, bitsize: usize, vaddr_idx: u8, value_idx: u8) -> usize {
        *self
            .write
            .get(&(ordered, bitsize, vaddr_idx, value_idx))
            .unwrap_or_else(|| {
                panic!(
                    "no A32 write fallback stub for (ordered={}, bitsize={}, vaddr={}, value={})",
                    ordered, bitsize, vaddr_idx, value_idx
                )
            })
    }

    fn exclusive_write_stub(
        &self,
        ordered: bool,
        bitsize: usize,
        vaddr_idx: u8,
        value_idx: u8,
    ) -> usize {
        *self
            .exclusive_write
            .get(&(ordered, bitsize, vaddr_idx, value_idx))
            .unwrap_or_else(|| {
                panic!(
                    "no A32 exclusive-write fallback stub for (ordered={}, bitsize={}, vaddr={}, value={})",
                    ordered, bitsize, vaddr_idx, value_idx
                )
            })
    }
}

fn emit_zero_extend(asm: &mut CodeAssembler, bitsize: usize, reg: Reg) {
    match bitsize {
        8 => asm
            .movzx(reg.cvt32().unwrap(), reg.cvt8().unwrap())
            .unwrap(),
        16 => asm
            .movzx(reg.cvt32().unwrap(), reg.cvt16().unwrap())
            .unwrap(),
        32 => asm.mov(reg.cvt32().unwrap(), reg.cvt32().unwrap()).unwrap(),
        64 => {}
        _ => unreachable!(),
    }
}

fn register_fallback(asm: &CodeAssembler, start_offset: usize, name: &str) {
    let start = unsafe { asm.top().add(start_offset) };
    let end = unsafe { asm.top().add(asm.size()) };
    perf_map::register(start, end, name);
}

fn emit_read_fallback(
    asm: &mut CodeAssembler,
    callbacks: &EmitCallbacks,
    ordered: bool,
    bitsize: usize,
    vaddr_idx: u8,
    value_idx: u8,
) {
    let value = Reg::gpr64(value_idx);
    let saved =
        abi::push_caller_save_registers_and_adjust_stack_except(asm, Some(HostLoc::Gpr(value_idx)))
            .unwrap();
    let vaddr_param = abi::ABI_PARAMS[1].to_reg64();
    if vaddr_idx != vaddr_param.get_idx() {
        asm.mov(vaddr_param, Reg::gpr64(vaddr_idx)).unwrap();
    }
    if ordered {
        asm.mfence().unwrap();
    }
    match bitsize {
        8 => &callbacks.memory_read_8,
        16 => &callbacks.memory_read_16,
        32 => &callbacks.memory_read_32,
        64 => &callbacks.memory_read_64,
        _ => unreachable!(),
    }
    .emit_call_simple(asm)
    .unwrap();
    if value_idx != RAX.get_idx() {
        asm.mov(value, RAX).unwrap();
    }
    abi::pop_caller_save_registers_and_adjust_stack(asm, &saved).unwrap();
    emit_zero_extend(asm, bitsize, value);
    asm.ret().unwrap();
}

fn marshal_a32_write_arguments(asm: &mut CodeAssembler, vaddr_idx: u8, value_idx: u8) -> Reg {
    let vaddr_param = abi::ABI_PARAMS[1].to_reg64();
    let value_param = abi::ABI_PARAMS[2].to_reg64();
    if vaddr_idx == value_param.get_idx() && value_idx == vaddr_param.get_idx() {
        asm.xchg(vaddr_param, value_param).unwrap();
    } else if vaddr_idx == value_param.get_idx() {
        asm.mov(vaddr_param, Reg::gpr64(vaddr_idx)).unwrap();
        if value_idx != value_param.get_idx() {
            asm.mov(value_param, Reg::gpr64(value_idx)).unwrap();
        }
    } else {
        if value_idx != value_param.get_idx() {
            asm.mov(value_param, Reg::gpr64(value_idx)).unwrap();
        }
        if vaddr_idx != vaddr_param.get_idx() {
            asm.mov(vaddr_param, Reg::gpr64(vaddr_idx)).unwrap();
        }
    }
    value_param
}

fn emit_write_fallback(
    asm: &mut CodeAssembler,
    callbacks: &EmitCallbacks,
    ordered: bool,
    bitsize: usize,
    vaddr_idx: u8,
    value_idx: u8,
) {
    let saved = abi::push_caller_save_registers_and_adjust_stack(asm).unwrap();
    let value_param = marshal_a32_write_arguments(asm, vaddr_idx, value_idx);
    emit_zero_extend(asm, bitsize, value_param);
    match bitsize {
        8 => &callbacks.memory_write_8,
        16 => &callbacks.memory_write_16,
        32 => &callbacks.memory_write_32,
        64 => &callbacks.memory_write_64,
        _ => unreachable!(),
    }
    .emit_call_simple(asm)
    .unwrap();
    if ordered {
        asm.mfence().unwrap();
    }
    abi::pop_caller_save_registers_and_adjust_stack(asm, &saved).unwrap();
    asm.ret().unwrap();
}

fn emit_exclusive_write_fallback(
    asm: &mut CodeAssembler,
    callbacks: &RawExclusiveWriteCallbacks,
    bitsize: usize,
    vaddr_idx: u8,
    value_idx: u8,
) {
    let saved = abi::push_caller_save_registers_and_adjust_stack_except(
        asm,
        Some(HostLoc::Gpr(RAX.get_idx())),
    )
    .unwrap();
    let value_param = marshal_a32_write_arguments(asm, vaddr_idx, value_idx);
    emit_zero_extend(asm, bitsize, value_param);
    let expected_param = abi::ABI_PARAMS[3].to_reg64();
    asm.mov(expected_param, RAX).unwrap();
    emit_zero_extend(asm, bitsize, expected_param);
    match bitsize {
        8 => &callbacks.write_8,
        16 => &callbacks.write_16,
        32 => &callbacks.write_32,
        64 => &callbacks.write_64,
        _ => unreachable!(),
    }
    .emit_call_simple(asm)
    .unwrap();
    abi::pop_caller_save_registers_and_adjust_stack(asm, &saved).unwrap();
    asm.ret().unwrap();
}

/// Architecture-owned entry point for Eden's `A32EmitX64::GenFastmemFallbacks`.
///
pub fn gen_fastmem_fallbacks(
    asm: &mut CodeAssembler,
    callbacks: &EmitCallbacks,
    raw_exclusive_write_callbacks: Option<&RawExclusiveWriteCallbacks>,
) -> FastmemFallbacksTable {
    let mut table = FastmemFallbacksTable::new();

    for ordered in [false, true] {
        for &vaddr_idx in &VALID_GPR_IDXES {
            for &value_idx in &VALID_GPR_IDXES {
                for &bitsize in &[8usize, 16, 32, 64] {
                    asm.align(16).unwrap();
                    let read_offset = asm.size();
                    emit_read_fallback(asm, callbacks, ordered, bitsize, vaddr_idx, value_idx);
                    table
                        .read
                        .insert((ordered, bitsize, vaddr_idx, value_idx), read_offset);
                    register_fallback(asm, read_offset, &format!("a32_read_fallback_{bitsize}"));

                    asm.align(16).unwrap();
                    let write_offset = asm.size();
                    emit_write_fallback(asm, callbacks, ordered, bitsize, vaddr_idx, value_idx);
                    table
                        .write
                        .insert((ordered, bitsize, vaddr_idx, value_idx), write_offset);
                    register_fallback(asm, write_offset, &format!("a32_write_fallback_{bitsize}"));

                    if let Some(raw_callbacks) = raw_exclusive_write_callbacks {
                        asm.align(16).unwrap();
                        let exclusive_write_offset = asm.size();
                        emit_exclusive_write_fallback(
                            asm,
                            raw_callbacks,
                            bitsize,
                            vaddr_idx,
                            value_idx,
                        );
                        table.exclusive_write.insert(
                            (ordered, bitsize, vaddr_idx, value_idx),
                            exclusive_write_offset,
                        );
                        register_fallback(
                            asm,
                            exclusive_write_offset,
                            &format!("a32_exclusive_write_fallback_{bitsize}"),
                        );
                    }
                }
            }
        }
    }

    table
}

#[derive(Clone, Copy)]
struct MemoryAbortInfo {
    enabled: bool,
    current_pc: u32,
    upper_location_descriptor: Option<u32>,
    dispatcher_offsets: Option<[usize; 4]>,
    code_base_ptr: *const u8,
}

fn memory_abort_info(ctx: &EmitContext, inst: &Inst) -> MemoryAbortInfo {
    let current_location = A32LocationDescriptor::from_location(LocationDescriptor::new(
        inst.args[0].get_imm_as_u64(),
    ));
    let current = current_location.to_location();
    let new_upper = ctx.arch.extract_upper_location_descriptor(current) & !6;
    let old_upper = ctx.arch.extract_upper_location_descriptor(ctx.location) & !6;

    MemoryAbortInfo {
        enabled: ctx.config.memory.check_halt_on_memory_access,
        current_pc: current_location.pc(),
        upper_location_descriptor: (new_upper != old_upper).then_some(new_upper),
        dispatcher_offsets: ctx.dispatcher_offsets,
        code_base_ptr: ctx.code_base_ptr,
    }
}

/// Port of Eden's `A32EmitX64::EmitCheckMemoryAbort`.
fn emit_check_memory_abort(asm: &mut CodeAssembler, info: MemoryAbortInfo, end: Option<&Label>) {
    if !info.enabled {
        return;
    }

    let skip = asm.create_label();
    let skip_target = end.unwrap_or(&skip);
    asm.test(
        dword_ptr(RegExp::from(R15) + A32JitState::offset_of_halt_reason() as i32),
        HaltReason::MEMORY_ABORT.bits() as i32,
    )
    .unwrap();
    asm.jz(skip_target, JmpType::Near).unwrap();

    if let Some(upper) = info.upper_location_descriptor {
        asm.mov(
            dword_ptr(
                RegExp::from(R15) + A32JitState::offset_of_upper_location_descriptor() as i32,
            ),
            upper as i32,
        )
        .unwrap();
    }
    asm.mov(
        dword_ptr(RegExp::from(R15) + A32JitState::reg_offset(15) as i32),
        info.current_pc as i32,
    )
    .unwrap();
    if let Some(offsets) = info.dispatcher_offsets {
        emit_jmp_to_offset(asm, offsets[FORCE_RETURN], info.code_base_ptr);
    } else {
        asm.ret().unwrap();
    }

    if end.is_none() {
        asm.bind(&skip).unwrap();
    }
}

fn emit_bitsize_read_mov(
    ra: &mut RegAlloc,
    bitsize: usize,
    value_idx: u8,
    addr: RegExp,
    ordered: bool,
    host_features: HostFeature,
) -> usize {
    match bitsize {
        8 => emit_read_memory_mov::<8>(ra.asm, value_idx, addr, ordered, host_features),
        16 => emit_read_memory_mov::<16>(ra.asm, value_idx, addr, ordered, host_features),
        32 => emit_read_memory_mov::<32>(ra.asm, value_idx, addr, ordered, host_features),
        64 => emit_read_memory_mov::<64>(ra.asm, value_idx, addr, ordered, host_features),
        _ => unreachable!(),
    }
}

fn emit_bitsize_write_mov(
    ra: &mut RegAlloc,
    bitsize: usize,
    addr: RegExp,
    value_idx: u8,
    ordered: bool,
    host_features: HostFeature,
) -> usize {
    match bitsize {
        8 => emit_write_memory_mov::<8>(ra.asm, addr, value_idx, ordered, host_features),
        16 => emit_write_memory_mov::<16>(ra.asm, addr, value_idx, ordered, host_features),
        32 => emit_write_memory_mov::<32>(ra.asm, addr, value_idx, ordered, host_features),
        64 => emit_write_memory_mov::<64>(ra.asm, addr, value_idx, ordered, host_features),
        _ => unreachable!(),
    }
}

fn parse_fastmem_vaddr_range_env() -> Option<(u64, u64)> {
    std::env::var("RUZU_TRAP_FASTMEM_ANY_VADDR_RANGE")
        .ok()
        .and_then(|s| {
            let parts: Vec<&str> = s.split(':').collect();
            if parts.len() != 2 {
                return None;
            }
            let parse_hex = |raw: &str| {
                let raw = raw.trim();
                let digits = raw
                    .strip_prefix("0x")
                    .or_else(|| raw.strip_prefix("0X"))
                    .unwrap_or(raw);
                u64::from_str_radix(digits, 16).ok()
            };
            Some((parse_hex(parts[0])?, parse_hex(parts[1])?))
        })
}

fn parse_trace_fastmem_write_range_env() -> Option<(u64, u64)> {
    std::env::var("RUZU_TRACE_FASTMEM_W_RANGE")
        .ok()
        .and_then(|s| {
            let parts: Vec<&str> = s.split(':').collect();
            if parts.len() != 2 {
                return None;
            }
            let parse_hex = |raw: &str| {
                let raw = raw.trim();
                let digits = raw
                    .strip_prefix("0x")
                    .or_else(|| raw.strip_prefix("0X"))
                    .unwrap_or(raw);
                u64::from_str_radix(digits, 16).ok()
            };
            Some((parse_hex(parts[0])?, parse_hex(parts[1])?))
        })
}

fn parse_trap_fastmem_write_value_env() -> Option<u64> {
    std::env::var("RUZU_TRAP_FASTMEM_W_VALUE")
        .ok()
        .and_then(|raw| {
            let raw = raw.trim();
            let digits = raw
                .strip_prefix("0x")
                .or_else(|| raw.strip_prefix("0X"))
                .unwrap_or(raw);
            u64::from_str_radix(digits, 16).ok()
        })
}

fn emit_preserved_a32_fastmem_write_trace_hook(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    vaddr_idx: u8,
    value_idx: u8,
    bitsize: usize,
) {
    ra.asm.push(rxbyak::Reg::gpr64(vaddr_idx)).unwrap();
    ra.asm.push(rxbyak::Reg::gpr64(value_idx)).unwrap();

    let caller_save_gprs: &[Reg] = &[RAX, RCX, RDX, RDI, RSI, R8, R9, R10, R11];
    for &reg in caller_save_gprs {
        ra.asm.push(reg).unwrap();
    }

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

    // Keep the host stack aligned and, on Win64, provide the mandatory
    // 32-byte shadow space plus the stack slot for the fifth argument.
    #[cfg(target_os = "windows")]
    const CALL_FRAME_BYTES: i32 = 40;
    #[cfg(not(target_os = "windows"))]
    const CALL_FRAME_BYTES: i32 = 8;
    ra.asm.sub(RSP, CALL_FRAME_BYTES).unwrap();

    const SAVED_VALUE_OFFSET: i32 = CALL_FRAME_BYTES + XMM_SAVE_BYTES + 9 * 8;
    const SAVED_VADDR_OFFSET: i32 = SAVED_VALUE_OFFSET + 8;

    #[cfg(target_os = "windows")]
    {
        // Win64: RCX, RDX, R8, R9, then [rsp+32] for the fifth argument.
        ra.asm.mov(RCX, R15).unwrap();
        ra.asm
            .mov(RDX, ctx.arch.extract_pc(ctx.location) as i64)
            .unwrap();
        ra.asm
            .mov(R8, qword_ptr(RegExp::from(RSP) + SAVED_VADDR_OFFSET))
            .unwrap();
        ra.asm.mov(R9, bitsize as i64).unwrap();
        ra.asm
            .mov(RAX, qword_ptr(RegExp::from(RSP) + SAVED_VALUE_OFFSET))
            .unwrap();
        ra.asm.mov(qword_ptr(RegExp::from(RSP) + 32), RAX).unwrap();
    }

    #[cfg(not(target_os = "windows"))]
    {
        // System V: RDI, RSI, RDX, RCX, R8.
        ra.asm.mov(RDI, R15).unwrap();
        ra.asm
            .mov(RSI, ctx.arch.extract_pc(ctx.location) as i64)
            .unwrap();
        ra.asm
            .mov(RDX, qword_ptr(RegExp::from(RSP) + SAVED_VADDR_OFFSET))
            .unwrap();
        ra.asm.mov(RCX, bitsize as i64).unwrap();
        ra.asm
            .mov(R8, qword_ptr(RegExp::from(RSP) + SAVED_VALUE_OFFSET))
            .unwrap();
    }

    ra.asm
        .mov(
            RAX,
            crate::jit::a32_fastmem_write_trace_hook as usize as i64,
        )
        .unwrap();
    ra.asm.call_reg(RAX).unwrap();
    ra.asm.add(RSP, CALL_FRAME_BYTES).unwrap();

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

    for &reg in caller_save_gprs.iter().rev() {
        ra.asm.pop(reg).unwrap();
    }
    ra.asm.add(RSP, 16i32).unwrap();
}

// ---------------------------------------------------------------------------
// Memory operations (delegate to shared memory callbacks)
// ---------------------------------------------------------------------------

/// Matches upstream `A32EmitX64::ShouldFastmem`.
fn should_fastmem(ctx: &EmitContext, inst_ref: InstRef) -> bool {
    if !ctx.fastmem_available {
        return false;
    }

    let marker = (ctx.location, inst_ref.0);
    ctx.do_not_fastmem
        .map(|markers| !markers.contains(&marker))
        .unwrap_or(true)
}

fn emit_a32_memory_read(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    // args[2] is the AccType immediate (LDA / LDAH / LDAB / LDAEX use Ordered).
    let ordered = is_ordered(args[2].value.get_acc_type());

    // `RUZU_NO_FASTMEM_R{8,16,32,64}=1` — force this width's loads through
    // the slow callback path. Counterpart to the write gates above.
    let force_callback = match bitsize {
        8 => std::env::var_os("RUZU_NO_FASTMEM_R8").is_some(),
        16 => std::env::var_os("RUZU_NO_FASTMEM_R16").is_some(),
        32 => std::env::var_os("RUZU_NO_FASTMEM_R32").is_some(),
        64 => std::env::var_os("RUZU_NO_FASTMEM_R64").is_some(),
        _ => false,
    } || (bitsize == 64
        && std::env::var("RUZU_NO_FASTMEM_R64_AT_PC")
            .ok()
            .and_then(|raw| {
                u64::from_str_radix(
                    raw.trim()
                        .strip_prefix("0x")
                        .or_else(|| raw.trim().strip_prefix("0X"))
                        .unwrap_or(raw.trim()),
                    16,
                )
                .ok()
            })
            .is_some_and(|pc| ctx.arch.extract_pc(ctx.location) == pc));

    if should_fastmem(ctx, inst_ref) && bitsize <= 64 && !force_callback {
        // Fastmem path: result = [R13+vaddr]. Upstream's ShouldFastmem result
        // takes precedence over the configured page table; the page table is
        // only used after a fault disables fastmem for this instruction.
        // R13 = fastmem_pointer, loaded in the dispatcher prelude.
        //
        // Upstream-faithful: for ordered loads (LDA / LDAB / LDAH /
        // LDAEX) emit `xor reg, reg; lock xadd [r13+vaddr], reg`
        // (mirrors `EmitReadMemoryMov<bitsize>` ordered path in
        // `emit_x64_memory.h:204-247`). The xor pre-zeroes the
        // destination so the xadd performs `mem += 0` — i.e. a pure
        // atomic load with full memory-ordering semantics. The
        // returned register holds the pre-add value.
        //
        // The `inst_offset` captured below is the start of the `lock`
        // prefix (or of the `mov` in the unordered case). The SIGSEGV
        // handler delivers RIP at that exact byte when the access
        // faults on unmapped memory.
        let vaddr = ra.use_gpr(&mut args[1]);
        let result = ra.scratch_gpr();
        let vaddr_idx = vaddr.get_idx();
        let result_idx = result.get_idx();
        let addr = RegExp::from(rxbyak::R13) + vaddr;
        if ordered {
            // xor zero-extends the 64-bit register too (Intel SDM:
            // 32-bit operand zeroes upper 32 bits).
            ra.asm
                .xor_(result.cvt32().unwrap(), result.cvt32().unwrap())
                .unwrap();
        }
        let inst_offset = ra.asm.size();
        if ordered {
            ra.asm.lock().unwrap();
            match bitsize {
                8 => {
                    ra.asm.xadd(byte_ptr(addr), result.cvt8().unwrap()).unwrap();
                }
                16 => {
                    ra.asm
                        .xadd(word_ptr(addr), result.cvt16().unwrap())
                        .unwrap();
                }
                32 => {
                    ra.asm
                        .xadd(dword_ptr(addr), result.cvt32().unwrap())
                        .unwrap();
                }
                64 => {
                    ra.asm.xadd(qword_ptr(addr), result).unwrap();
                }
                _ => unreachable!(),
            }
        } else {
            match bitsize {
                8 => {
                    ra.asm
                        .movzx(result.cvt32().unwrap(), byte_ptr(addr))
                        .unwrap();
                }
                16 => {
                    ra.asm
                        .movzx(result.cvt32().unwrap(), word_ptr(addr))
                        .unwrap();
                }
                32 => {
                    ra.asm
                        .mov(result.cvt32().unwrap(), dword_ptr(addr))
                        .unwrap();
                }
                64 => {
                    ra.asm.mov(result, qword_ptr(addr)).unwrap();
                }
                _ => unreachable!(),
            }
        }
        let abort_info = memory_abort_info(ctx, inst);
        let resume_offset = if abort_info.enabled {
            let end = ra.asm.create_label();
            ra.asm.jmp(&end, JmpType::Near).unwrap();
            let resume_offset = ra.asm.size();
            emit_check_memory_abort(ra.asm, abort_info, Some(&end));
            ra.asm.bind(&end).unwrap();
            resume_offset
        } else {
            ra.asm.size()
        };
        let fallbacks = unsafe {
            &*(ctx
                .fastmem_fallbacks
                .expect("A32 fastmem path requires fallback table")
                as *const FastmemFallbacksTable)
        };
        let wrapped_fn_off = fallbacks.read_stub(ordered, bitsize, vaddr_idx, result_idx);
        let marker = (ctx.location, inst_ref.0);
        let recompile = ctx.config.memory.recompile_on_fastmem_failure;
        ctx.deferred_emits.borrow_mut().push(Box::new(move |dctx| {
            dctx.fastmem_patches.add(
                dctx.code_base + inst_offset as u64,
                crate::backend::x64::exception_handler::FastmemPatchInfo::new(
                    dctx.code_base + resume_offset as u64,
                    dctx.code_base + wrapped_fn_off as u64,
                    Some(marker),
                    recompile,
                ),
            );
        }));
        ra.define_value(inst_ref, result);
        return;
    }

    if ctx.config.memory.page_table_present && !force_callback {
        // When this instruction has been removed from `ShouldFastmem` after a
        // fault, upstream falls through to the page-table path rather than
        // calling the callback unconditionally.
        let vaddr = ra.use_gpr(&mut args[1]);
        let result = ra.scratch_gpr();
        let vaddr_idx = vaddr.get_idx();
        let result_idx = result.get_idx();
        let fallbacks = unsafe {
            &*(ctx
                .fastmem_fallbacks
                .expect("A32 page-table path requires fallback table")
                as *const FastmemFallbacksTable)
        };
        let wrapped_fn_off = fallbacks.read_stub(ordered, bitsize, vaddr_idx, result_idx);
        let abort = ra.asm.create_label();
        let end = ra.asm.create_label();
        let abort_info = memory_abort_info(ctx, inst);
        let addr = emit_vaddr_lookup_a32(ra, ctx, bitsize, abort, vaddr);
        emit_bitsize_read_mov(ra, bitsize, result_idx, addr, ordered, ctx.host_features);

        ctx.deferred_emits.borrow_mut().push(Box::new(move |dctx| {
            let asm = &mut *dctx.asm;
            asm.bind(&abort).unwrap();
            emit_call_to_offset(asm, wrapped_fn_off);
            emit_check_memory_abort(asm, abort_info, Some(&end));
            asm.jmp(&end, JmpType::Near).unwrap();
        }));
        ra.asm.bind(&end).unwrap();
        ra.define_value(inst_ref, result);
        return;
    }

    // Callback path (slow)
    ra.host_call(Some(inst_ref), &mut [None, Some(&mut args[1]), None, None]);
    if ordered {
        // Drain pending stores before the slow-path load. Mirrors upstream
        // `EmitMemoryRead<bitsize>` ordered path in
        // `emit_x64_memory.cpp.inc:64-66`.
        ra.asm.mfence().unwrap();
    }
    let callback = match bitsize {
        8 => &ctx.config.callbacks.memory_read_8,
        16 => &ctx.config.callbacks.memory_read_16,
        32 => &ctx.config.callbacks.memory_read_32,
        64 => &ctx.config.callbacks.memory_read_64,
        _ => unreachable!(),
    };
    callback.emit_call_simple(&mut *ra.asm).unwrap();
    emit_check_memory_abort(ra.asm, memory_abort_info(ctx, inst), None);
}

fn emit_a32_memory_write(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    // args[3] is the AccType immediate (STL / STLH / STLB / STLEX use Ordered).
    let ordered = is_ordered(args[3].value.get_acc_type());

    // `RUZU_NO_FASTMEM_W{8,16,32,64}=1` — force this width through the slow
    // callback path. Used to diagnose userspace-vs-kernel memory-visibility
    // slow-path traceable via the existing `RUZU_TRACE_W_AT_VADDR` hook in
    // `arm_dynarmic_32.rs::watch_write`. Mirrors A64's per-width gates in
    // `a64_emit_x64_memory.rs:711-716`.
    let force_callback = match bitsize {
        8 => std::env::var_os("RUZU_NO_FASTMEM_W8").is_some(),
        16 => std::env::var_os("RUZU_NO_FASTMEM_W16").is_some(),
        32 => std::env::var_os("RUZU_NO_FASTMEM_W32").is_some(),
        64 => std::env::var_os("RUZU_NO_FASTMEM_W64").is_some(),
        _ => false,
    };

    if should_fastmem(ctx, inst_ref) && bitsize <= 64 && !force_callback {
        // Fastmem path: [R13+vaddr] = value. As on the read side, upstream
        // gives a valid ShouldFastmem marker precedence over the page table.
        //
        // Upstream-faithful: for ordered stores (STL / STLB / STLH /
        // STLEX) emit `xchg [r13+vaddr], value` (mirrors
        // `EmitWriteMemoryMov<bitsize>` ordered path in
        // `emit_x64_memory.h:274-339`). `xchg` with a memory operand
        // has an implicit `lock` prefix on x86, providing full
        // memory-ordering semantics (release-store).
        //
        // Note: `xchg` *destroys* the value register (it ends up
        // holding the previous memory contents). Upstream uses
        // `UseScratchGpr` for the value when ordered to mark this;
        // we do the same below.
        let vaddr = ra.use_gpr(&mut args[1]);
        let value = if ordered {
            ra.use_scratch_gpr(&mut args[2])
        } else {
            ra.use_gpr(&mut args[2])
        };
        let vaddr_idx = vaddr.get_idx();
        let value_idx = value.get_idx();
        let addr = RegExp::from(rxbyak::R13) + vaddr;
        let inst_offset = ra.asm.size();
        if let Some(expected) = parse_trap_fastmem_write_value_env().filter(|_| bitsize == 32) {
            let ok = ra.asm.create_label();
            let r11 = rxbyak::Reg::gpr64(11);

            // Match ruzu-cmd's A32 SIGILL recovery convention: [rsp+16]
            // contains the guest destination vaddr while the trap fires.
            ra.asm.push(vaddr).unwrap();
            ra.asm.push(r11).unwrap();
            ra.asm.pushf().unwrap();

            match bitsize {
                8 => {
                    ra.asm
                        .mov(r11.cvt32().unwrap(), (expected & 0xFF) as i32)
                        .unwrap();
                    if value_idx == 11 {
                        ra.asm
                            .cmp(byte_ptr(RegExp::from(RSP) + 8), r11.cvt8().unwrap())
                            .unwrap();
                    } else {
                        ra.asm
                            .cmp(value.cvt8().unwrap(), r11.cvt8().unwrap())
                            .unwrap();
                    }
                }
                16 => {
                    ra.asm
                        .mov(r11.cvt32().unwrap(), (expected & 0xFFFF) as i32)
                        .unwrap();
                    if value_idx == 11 {
                        ra.asm
                            .cmp(word_ptr(RegExp::from(RSP) + 8), r11.cvt16().unwrap())
                            .unwrap();
                    } else {
                        ra.asm
                            .cmp(value.cvt16().unwrap(), r11.cvt16().unwrap())
                            .unwrap();
                    }
                }
                32 => {
                    ra.asm.mov(r11.cvt32().unwrap(), expected as i32).unwrap();
                    if value_idx == 11 {
                        ra.asm
                            .cmp(dword_ptr(RegExp::from(RSP) + 8), r11.cvt32().unwrap())
                            .unwrap();
                    } else {
                        ra.asm
                            .cmp(value.cvt32().unwrap(), r11.cvt32().unwrap())
                            .unwrap();
                    }
                }
                64 => {
                    ra.asm.mov(r11, expected as i64).unwrap();
                    if value_idx == 11 {
                        ra.asm.cmp(qword_ptr(RegExp::from(RSP) + 8), r11).unwrap();
                    } else {
                        ra.asm.cmp(value, r11).unwrap();
                    }
                }
                _ => unreachable!(),
            }
            ra.asm.jne(&ok, rxbyak::JmpType::Near).unwrap();
            ra.asm.ud2().unwrap();
            ra.asm.bind(&ok).unwrap();
            ra.asm.popf().unwrap();
            ra.asm.pop(r11).unwrap();
            ra.asm.pop(vaddr).unwrap();
        }
        if ordered {
            match bitsize {
                8 => {
                    ra.asm.xchg(byte_ptr(addr), value.cvt8().unwrap()).unwrap();
                }
                16 => {
                    ra.asm.xchg(word_ptr(addr), value.cvt16().unwrap()).unwrap();
                }
                32 => {
                    ra.asm
                        .xchg(dword_ptr(addr), value.cvt32().unwrap())
                        .unwrap();
                }
                64 => {
                    ra.asm.xchg(qword_ptr(addr), value).unwrap();
                }
                _ => unreachable!(),
            }
        } else {
            match bitsize {
                8 => {
                    ra.asm.mov(byte_ptr(addr), value.cvt8().unwrap()).unwrap();
                }
                16 => {
                    ra.asm.mov(word_ptr(addr), value.cvt16().unwrap()).unwrap();
                }
                32 => {
                    ra.asm.mov(dword_ptr(addr), value.cvt32().unwrap()).unwrap();
                }
                64 => {
                    ra.asm.mov(qword_ptr(addr), value).unwrap();
                }
                _ => unreachable!(),
            }
        }

        if let Some((lo, hi)) = parse_fastmem_vaddr_range_env() {
            let ok = ra.asm.create_label();
            let vaddr_reg = rxbyak::Reg::gpr64(vaddr_idx);
            let scratch_idx = if vaddr_idx == 11 { 10 } else { 11 };
            let scratch = rxbyak::Reg::gpr64(scratch_idx);
            let trap_nonzero =
                std::env::var_os("RUZU_TRAP_FASTMEM_ANY_VADDR_RANGE_NONZERO").is_some();
            let trap_value = std::env::var("RUZU_TRAP_FASTMEM_ANY_VADDR_RANGE_VALUE")
                .ok()
                .and_then(|raw| {
                    let raw = raw.trim();
                    let digits = raw
                        .strip_prefix("0x")
                        .or_else(|| raw.strip_prefix("0X"))
                        .unwrap_or(raw);
                    u64::from_str_radix(digits, 16).ok()
                });

            // Match A64's diagnostic stack convention: after
            // push(vaddr), push(scratch), pushf(), ruzu-cmd's SIGILL handler
            // recovers the trapped guest vaddr from [rsp+16].
            ra.asm.push(vaddr_reg).unwrap();
            ra.asm.push(scratch).unwrap();
            ra.asm.pushf().unwrap();

            // Trap when the emitted store range overlaps [lo, hi), not
            // only when the store starts inside the range. STM/STRD can
            // start before the interesting word and still overwrite it.
            ra.asm.mov(scratch, hi as i64).unwrap();
            ra.asm.cmp(vaddr_reg, scratch).unwrap();
            ra.asm.jae(&ok, rxbyak::JmpType::Near).unwrap();

            ra.asm.mov(scratch, vaddr_reg).unwrap();
            ra.asm.add(scratch, (bitsize / 8) as i32).unwrap();
            ra.asm.cmp(scratch, lo as i64).unwrap();
            ra.asm.jbe(&ok, rxbyak::JmpType::Near).unwrap();

            if trap_nonzero {
                let addr = RegExp::from(rxbyak::R13) + lo as i32;
                match bitsize {
                    8 => ra.asm.cmp(byte_ptr(addr), 0i32).unwrap(),
                    16 => ra.asm.cmp(word_ptr(addr), 0i32).unwrap(),
                    32 => ra.asm.cmp(dword_ptr(addr), 0i32).unwrap(),
                    64 => ra.asm.cmp(qword_ptr(addr), 0i32).unwrap(),
                    _ => unreachable!(),
                }
                ra.asm.je(&ok, rxbyak::JmpType::Near).unwrap();
            }

            if let Some(expected) = trap_value {
                let addr = RegExp::from(rxbyak::R13) + lo as i32;
                match bitsize {
                    8 => {
                        ra.asm.mov(scratch.cvt32().unwrap(), 0i32).unwrap();
                        ra.asm
                            .mov(scratch.cvt8().unwrap(), (expected & 0xFF) as i32)
                            .unwrap();
                        ra.asm.cmp(byte_ptr(addr), scratch.cvt8().unwrap()).unwrap();
                    }
                    16 => {
                        ra.asm
                            .mov(scratch.cvt32().unwrap(), (expected & 0xFFFF) as i32)
                            .unwrap();
                        ra.asm
                            .cmp(word_ptr(addr), scratch.cvt16().unwrap())
                            .unwrap();
                    }
                    32 => {
                        ra.asm
                            .mov(scratch.cvt32().unwrap(), expected as i32)
                            .unwrap();
                        ra.asm
                            .cmp(dword_ptr(addr), scratch.cvt32().unwrap())
                            .unwrap();
                    }
                    64 => {
                        ra.asm.mov(scratch, expected as i64).unwrap();
                        ra.asm.cmp(qword_ptr(addr), scratch).unwrap();
                    }
                    _ => unreachable!(),
                }
                ra.asm.jne(&ok, rxbyak::JmpType::Near).unwrap();
            }

            let sentinel: u32 = 0xCAFE_F000 | (bitsize as u32);
            ra.asm.mov(scratch, sentinel as i32).unwrap();
            ra.asm.ud2().unwrap();

            ra.asm.bind(&ok).unwrap();
            ra.asm.popf().unwrap();
            ra.asm.pop(scratch).unwrap();
            ra.asm.pop(vaddr_reg).unwrap();
        }

        if let Some((lo, hi)) = parse_trace_fastmem_write_range_env() {
            let skip = ra.asm.create_label();
            let vaddr_reg = rxbyak::Reg::gpr32(vaddr_idx);

            ra.asm.cmp(vaddr_reg, lo as u32 as i32).unwrap();
            ra.asm.jb(&skip, rxbyak::JmpType::Near).unwrap();
            ra.asm.cmp(vaddr_reg, hi as u32 as i32).unwrap();
            ra.asm.jae(&skip, rxbyak::JmpType::Near).unwrap();

            emit_preserved_a32_fastmem_write_trace_hook(ctx, ra, vaddr_idx, value_idx, bitsize);
            ra.asm.bind(&skip).unwrap();
        }

        let abort_info = memory_abort_info(ctx, inst);
        let resume_offset = if abort_info.enabled {
            let end = ra.asm.create_label();
            ra.asm.jmp(&end, JmpType::Near).unwrap();
            let resume_offset = ra.asm.size();
            emit_check_memory_abort(ra.asm, abort_info, Some(&end));
            ra.asm.bind(&end).unwrap();
            resume_offset
        } else {
            ra.asm.size()
        };
        let fallbacks = unsafe {
            &*(ctx
                .fastmem_fallbacks
                .expect("A32 fastmem path requires fallback table")
                as *const FastmemFallbacksTable)
        };
        let wrapped_fn_off = fallbacks.write_stub(ordered, bitsize, vaddr_idx, value_idx);
        let marker = (ctx.location, inst_ref.0);
        let recompile = ctx.config.memory.recompile_on_fastmem_failure;
        ctx.deferred_emits.borrow_mut().push(Box::new(move |dctx| {
            dctx.fastmem_patches.add(
                dctx.code_base + inst_offset as u64,
                crate::backend::x64::exception_handler::FastmemPatchInfo::new(
                    dctx.code_base + resume_offset as u64,
                    dctx.code_base + wrapped_fn_off as u64,
                    Some(marker),
                    recompile,
                ),
            );
        }));
        return;
    }

    if ctx.config.memory.page_table_present && !force_callback {
        let vaddr = ra.use_gpr(&mut args[1]);
        let value = if ordered {
            ra.use_scratch_gpr(&mut args[2])
        } else {
            ra.use_gpr(&mut args[2])
        };
        let vaddr_idx = vaddr.get_idx();
        let value_idx = value.get_idx();
        let fallbacks = unsafe {
            &*(ctx
                .fastmem_fallbacks
                .expect("A32 page-table path requires fallback table")
                as *const FastmemFallbacksTable)
        };
        let wrapped_fn_off = fallbacks.write_stub(ordered, bitsize, vaddr_idx, value_idx);
        let abort = ra.asm.create_label();
        let end = ra.asm.create_label();
        let abort_info = memory_abort_info(ctx, inst);
        let addr = emit_vaddr_lookup_a32(ra, ctx, bitsize, abort, vaddr);
        emit_bitsize_write_mov(ra, bitsize, addr, value_idx, ordered, ctx.host_features);

        ctx.deferred_emits.borrow_mut().push(Box::new(move |dctx| {
            let asm = &mut *dctx.asm;
            asm.bind(&abort).unwrap();
            emit_call_to_offset(asm, wrapped_fn_off);
            emit_check_memory_abort(asm, abort_info, Some(&end));
            asm.jmp(&end, JmpType::Near).unwrap();
        }));
        ra.asm.bind(&end).unwrap();
        return;
    }

    // Callback path (slow)
    let (first, rest) = args.split_at_mut(2);
    ra.host_call(
        None,
        &mut [None, Some(&mut first[1]), Some(&mut rest[0]), None],
    );
    let callback = match bitsize {
        8 => &ctx.config.callbacks.memory_write_8,
        16 => &ctx.config.callbacks.memory_write_16,
        32 => &ctx.config.callbacks.memory_write_32,
        64 => &ctx.config.callbacks.memory_write_64,
        _ => unreachable!(),
    };
    callback.emit_call_simple(&mut *ra.asm).unwrap();
    if ordered {
        // Drain the store buffer after the slow-path write so subsequent
        // memory ops observe it. Mirrors upstream `EmitMemoryWrite<bitsize>`
        // ordered path in `emit_x64_memory.cpp.inc:159-161`.
        ra.asm.mfence().unwrap();
    }
    emit_check_memory_abort(ra.asm, memory_abort_info(ctx, inst), None);
}

pub fn emit_a32_read_memory_8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_memory_read(ctx, ra, inst_ref, inst, 8);
}
pub fn emit_a32_read_memory_16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_memory_read(ctx, ra, inst_ref, inst, 16);
}
pub fn emit_a32_read_memory_32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_memory_read(ctx, ra, inst_ref, inst, 32);
}
pub fn emit_a32_read_memory_64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_memory_read(ctx, ra, inst_ref, inst, 64);
}

pub fn emit_a32_write_memory_8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_memory_write(ctx, ra, inst_ref, inst, 8);
}
pub fn emit_a32_write_memory_16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_memory_write(ctx, ra, inst_ref, inst, 16);
}
pub fn emit_a32_write_memory_32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_memory_write(ctx, ra, inst_ref, inst, 32);
}
pub fn emit_a32_write_memory_64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_memory_write(ctx, ra, inst_ref, inst, 64);
}

// ---------------------------------------------------------------------------
// Exclusive memory operations
// ---------------------------------------------------------------------------

pub(crate) fn exclusive_read_8(
    callbacks: &mut dyn A32UserCallbacks,
    monitor: *mut ExclusiveMonitor,
    processor_id: usize,
    vaddr: u64,
) -> u64 {
    unsafe {
        (&mut *monitor).read_and_mark(processor_id, vaddr, || {
            callbacks.memory_read_8(vaddr as u32)
        }) as u64
    }
}

pub(crate) fn exclusive_read_16(
    callbacks: &mut dyn A32UserCallbacks,
    monitor: *mut ExclusiveMonitor,
    processor_id: usize,
    vaddr: u64,
) -> u64 {
    unsafe {
        (&mut *monitor).read_and_mark(processor_id, vaddr, || {
            callbacks.memory_read_16(vaddr as u32)
        }) as u64
    }
}

pub(crate) fn exclusive_read_32(
    callbacks: &mut dyn A32UserCallbacks,
    monitor: *mut ExclusiveMonitor,
    processor_id: usize,
    vaddr: u64,
) -> u64 {
    unsafe {
        (&mut *monitor).read_and_mark(processor_id, vaddr, || {
            callbacks.memory_read_32(vaddr as u32)
        }) as u64
    }
}

pub(crate) fn exclusive_read_64(
    callbacks: &mut dyn A32UserCallbacks,
    monitor: *mut ExclusiveMonitor,
    processor_id: usize,
    vaddr: u64,
) -> u64 {
    unsafe {
        (&mut *monitor).read_and_mark(processor_id, vaddr, || {
            callbacks.memory_read_64(vaddr as u32)
        })
    }
}

pub(crate) fn exclusive_write_8(
    callbacks: &mut dyn A32UserCallbacks,
    monitor: *mut ExclusiveMonitor,
    processor_id: usize,
    vaddr: u64,
    value: u64,
) -> u64 {
    if unsafe {
        (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: u8| {
            callbacks.memory_write_exclusive_8(vaddr as u32, value as u8, expected)
        })
    } {
        0
    } else {
        1
    }
}

pub(crate) fn exclusive_write_16(
    callbacks: &mut dyn A32UserCallbacks,
    monitor: *mut ExclusiveMonitor,
    processor_id: usize,
    vaddr: u64,
    value: u64,
) -> u64 {
    if unsafe {
        (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: u16| {
            callbacks.memory_write_exclusive_16(vaddr as u32, value as u16, expected)
        })
    } {
        0
    } else {
        1
    }
}

pub(crate) fn exclusive_write_32(
    callbacks: &mut dyn A32UserCallbacks,
    monitor: *mut ExclusiveMonitor,
    processor_id: usize,
    vaddr: u64,
    value: u64,
) -> u64 {
    if unsafe {
        (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: u32| {
            callbacks.memory_write_exclusive_32(vaddr as u32, value as u32, expected)
        })
    } {
        0
    } else {
        1
    }
}

pub(crate) fn exclusive_write_64(
    callbacks: &mut dyn A32UserCallbacks,
    monitor: *mut ExclusiveMonitor,
    processor_id: usize,
    vaddr: u64,
    value: u64,
) -> u64 {
    if unsafe {
        (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: u64| {
            callbacks.memory_write_exclusive_64(vaddr as u32, value, expected)
        })
    } {
        0
    } else {
        1
    }
}

fn emit_a32_exclusive_read(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    assert!(
        ctx.config.global_monitor.is_some(),
        "A32 exclusive read requires a global monitor"
    );

    // Inline fast path: monitor + fastmem both configured. Mirrors upstream
    // `EmitExclusiveReadMemoryInline` (emit_x64_memory.cpp.inc:334-408).
    // Without this, every LDREX/LDAEX takes the trampoline path which costs
    // a virtual callback + SpinLock + page-table-walk per instruction —
    // dominant CPU hotspot for ARM32 binaries using userspace mutexes.
    if ctx.config.memory.fastmem_exclusive_access
        && ctx.fastmem_available
        && ctx.config.global_monitor.is_some()
        && std::env::var_os("RUZU_NO_EXCLUSIVE_INLINE").is_none()
    {
        emit_a32_exclusive_read_inline(ctx, ra, inst_ref, inst, bitsize);
        return;
    }

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    // args[2] is the AccType immediate. LDAEX uses Ordered.
    let ordered = is_ordered(args[2].value.get_acc_type());
    // args[0] = location descriptor (upper), args[1] = vaddr, args[2] = acc_type
    // ArgCallback: position 0 = None (context), position 1 = vaddr
    ra.host_call(Some(inst_ref), &mut [None, Some(&mut args[1]), None, None]);

    let excl_offset = A32JitState::offset_of_exclusive_state();
    ra.asm
        .mov(byte_ptr(RegExp::from(R15) + excl_offset as i32), 1i32)
        .unwrap();
    if ordered {
        // Drain pending stores before the exclusive load. Mirrors upstream
        // `EmitExclusiveReadMemory<bitsize>` ordered path in
        // `emit_x64_memory.cpp.inc:235-237`.
        ra.asm.mfence().unwrap();
    }

    let callback = match bitsize {
        8 => &ctx.config.callbacks.exclusive_read_8,
        16 => &ctx.config.callbacks.exclusive_read_16,
        32 => &ctx.config.callbacks.exclusive_read_32,
        64 => &ctx.config.callbacks.exclusive_read_64,
        _ => unreachable!(),
    };
    callback.emit_call_simple(&mut *ra.asm).unwrap();
    emit_zero_extend(ra.asm, bitsize, RAX);
    emit_check_memory_abort(ra.asm, memory_abort_info(ctx, inst), None);
}

/// Inline LDREX/LDAEX emit — upstream-faithful port of
/// `EmitExclusiveReadMemoryInline`.
///
/// Layout (with `pid` = `ctx.config.memory.processor_id`):
///
/// 1. Take monitor spin lock.
/// 2. `JitState.exclusive_state = 1`.
/// 3. `monitor.exclusive_addresses[pid] = vaddr`.
/// 4. Read through fastmem, or call the pre-generated fallback under the
///    monitor lock when this instruction was removed from fastmem.
/// 5. `monitor.exclusive_values[pid] = result`.
/// 6. Release monitor spin lock.
fn emit_a32_exclusive_read_inline(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    use crate::backend::x64::emit_exclusive_memory::{
        emit_exclusive_lock, emit_exclusive_unlock, exclusive_address_ptr, exclusive_value_ptr,
    };

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let pid = ctx.config.memory.processor_id;
    let addr_ptr = exclusive_address_ptr(ctx, pid).expect("global_monitor checked Some by caller");
    let value_ptr = exclusive_value_ptr(ctx, pid).expect("global_monitor checked Some by caller");

    // Reserve registers up-front so reg_alloc spills cleanly before any
    // emit; mirrors upstream's `ScratchGpr` calls in
    // `EmitExclusiveReadMemoryInline`.
    let vaddr = ra.use_gpr(&mut args[1]);
    let result = ra.scratch_gpr();
    let tmp = ra.scratch_gpr();
    let tmp2 = ra.scratch_gpr();

    // Take monitor lock (jmp/pause spin via `lock xchg [lock_storage], tmp2`).
    let locked = emit_exclusive_lock(ctx, &mut *ra.asm, tmp, tmp2.cvt32().unwrap());

    // Set exclusive_state = 1.
    let excl_state_off = A32JitState::offset_of_exclusive_state();
    ra.asm
        .mov(byte_ptr(RegExp::from(R15) + excl_state_off as i32), 1i32)
        .unwrap();

    // monitor.exclusive_addresses[pid] = vaddr (mov reg, imm64; then qword[reg], vaddr).
    ra.asm.mov(tmp, addr_ptr as u64 as i64).unwrap();
    ra.asm.mov(qword_ptr(RegExp::from(tmp)), vaddr).unwrap();

    let vaddr_idx = vaddr.get_idx();
    let result_idx = result.get_idx();
    let fallbacks = unsafe {
        &*(ctx
            .fastmem_fallbacks
            .expect("A32 exclusive read requires fallback table")
            as *const FastmemFallbacksTable)
    };
    let wrapped_fn_off = fallbacks.read_stub(true, bitsize, vaddr_idx, result_idx);

    if should_fastmem(ctx, inst_ref) {
        // Eden's inline exclusive helper always uses an ordered read,
        // independently of the IR access-type operand.
        let addr_expr = RegExp::from(rxbyak::R13) + vaddr;
        let inst_offset = ra.asm.size();
        emit_bitsize_read_mov(ra, bitsize, result_idx, addr_expr, true, ctx.host_features);
        let resume_offset = ra.asm.size();
        let marker = (ctx.location, inst_ref.0);
        let recompile = ctx.config.memory.recompile_on_exclusive_fastmem_failure;
        ctx.deferred_emits.borrow_mut().push(Box::new(move |dctx| {
            dctx.fastmem_patches.add(
                dctx.code_base + inst_offset as u64,
                crate::backend::x64::exception_handler::FastmemPatchInfo::new(
                    dctx.code_base + resume_offset as u64,
                    dctx.code_base + wrapped_fn_off as u64,
                    Some(marker),
                    recompile,
                ),
            );
        }));
    } else {
        emit_call_to_offset(ra.asm, wrapped_fn_off);
    }

    // monitor.exclusive_values[pid] = result (low qword; upper qword left
    // untouched — A32 LDREX{,H,B} write at most 4 bytes; LDREXD writes 8).
    ra.asm.mov(tmp, value_ptr as u64 as i64).unwrap();
    ra.asm.mov(qword_ptr(RegExp::from(tmp)), result).unwrap();

    // Release monitor lock.
    if locked {
        emit_exclusive_unlock(ctx, &mut *ra.asm, tmp, tmp2.cvt32().unwrap());
    }

    ra.define_value(inst_ref, result);
    emit_check_memory_abort(ra.asm, memory_abort_info(ctx, inst), None);
}

fn emit_a32_exclusive_write(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    assert!(
        ctx.config.global_monitor.is_some(),
        "A32 exclusive write requires a global monitor"
    );

    // Inline fast path — see `emit_a32_exclusive_write_inline` doc comment.
    if ctx.config.memory.fastmem_exclusive_access
        && ctx.fastmem_available
        && ctx.config.global_monitor.is_some()
        && std::env::var_os("RUZU_NO_EXCLUSIVE_INLINE").is_none()
    {
        emit_a32_exclusive_write_inline(ctx, ra, inst_ref, inst, bitsize);
        return;
    }

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    // args[3] is the AccType immediate. STLEX uses Ordered.
    let ordered = is_ordered(args[3].value.get_acc_type());
    // args[0] = location descriptor (upper), args[1] = vaddr, args[2] = value, args[3] = acc_type
    // ArgCallback: position 0 = None (context), position 1 = vaddr, position 2 = value
    let (first, rest) = args.split_at_mut(2);
    ra.host_call(
        Some(inst_ref),
        &mut [None, Some(&mut first[1]), Some(&mut rest[0]), None],
    );

    let tmp = ra.scratch_gpr();
    let end = ra.asm.create_label();
    let excl_offset = A32JitState::offset_of_exclusive_state();

    ra.asm.mov(RAX, 1i32).unwrap();
    ra.asm
        .movzx(
            tmp.cvt32().unwrap(),
            byte_ptr(RegExp::from(R15) + excl_offset as i32),
        )
        .unwrap();
    ra.asm
        .test(tmp.cvt8().unwrap(), tmp.cvt8().unwrap())
        .unwrap();
    ra.asm.je(&end, JmpType::Near).unwrap();
    ra.asm
        .xor_(tmp.cvt32().unwrap(), tmp.cvt32().unwrap())
        .unwrap();
    ra.asm
        .xchg(
            tmp.cvt8().unwrap(),
            byte_ptr(RegExp::from(R15) + excl_offset as i32),
        )
        .unwrap();

    let callback = match bitsize {
        8 => &ctx.config.callbacks.exclusive_write_8,
        16 => &ctx.config.callbacks.exclusive_write_16,
        32 => &ctx.config.callbacks.exclusive_write_32,
        64 => &ctx.config.callbacks.exclusive_write_64,
        _ => unreachable!(),
    };
    callback.emit_call_simple(&mut *ra.asm).unwrap();
    if ordered {
        // Drain the store buffer after the exclusive write. Mirrors upstream
        // `EmitExclusiveWriteMemory<bitsize>` ordered path in
        // `emit_x64_memory.cpp.inc:307-309`.
        ra.asm.mfence().unwrap();
    }
    ra.asm.bind(&end).unwrap();
    emit_check_memory_abort(ra.asm, memory_abort_info(ctx, inst), None);
}

/// Inline STREX/STLEX emit — upstream-faithful port of
/// `EmitExclusiveWriteMemoryInline`.
///
/// Layout (with `pid` = `ctx.config.memory.processor_id`):
///
/// 1. Force RAX as scratch (cmpxchg's implicit expected-value register).
/// 2. Take monitor spin lock.
/// 3. status = 1 (assume failure).
/// 4. If `JitState.exclusive_state == 0` → status stays 1, jump to end.
/// 5. If `monitor.exclusive_addresses[pid] != vaddr` → status stays 1,
///    jump to end.
/// 6. Clear other processors' reservations matching `vaddr` (so a
///    concurrent STREX on another core sees its reservation invalidated).
/// 7. `JitState.exclusive_state = 0` (we're consuming the reservation).
/// 8. Load the saved exclusive value into RAX from
///    `monitor.exclusive_values[pid]` (low qword for bitsize <= 64).
/// 9. Inline fastmem `lock cmpxchg [r13+vaddr], value`. If the memory
///    still holds RAX (= saved value), atomically replace with `value`
///    and ZF=1; otherwise ZF=0.
/// 10. status = ZF inverted (`setnz status`) — 0 on success, 1 on failure.
/// 11. Release monitor spin lock.
/// 12. end: result is status (defined to the IR inst).
fn emit_a32_exclusive_write_inline(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    use crate::backend::x64::emit_exclusive_memory::{
        emit_exclusive_lock, emit_exclusive_test_and_clear, emit_exclusive_unlock,
        exclusive_address_ptr, exclusive_value_ptr,
    };
    use crate::backend::x64::hostloc::HOST_RAX;

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let pid = ctx.config.memory.processor_id;
    let addr_ptr = exclusive_address_ptr(ctx, pid).expect("global_monitor checked Some by caller");
    let value_ptr = exclusive_value_ptr(ctx, pid).expect("global_monitor checked Some by caller");

    // Force-reserve RAX up front; cmpxchg implicitly uses RAX as its
    // "expected" value, and we'll also use RAX as the spin-lock scratch
    // (eax in upstream). Holding it here prevents reg_alloc from giving
    // RAX to vaddr / value / status / tmp.
    let _rax_reservation = ra.scratch_gpr_at(HOST_RAX);

    let value = ra.use_gpr(&mut args[2]);
    let vaddr = ra.use_gpr(&mut args[1]);
    let status = ra.scratch_gpr();
    let tmp = ra.scratch_gpr();

    let end = ra.asm.create_label();

    // Take monitor spin lock (clobbers RAX as scratch, which is fine since
    // we restore it below before cmpxchg).
    let locked = emit_exclusive_lock(ctx, &mut *ra.asm, tmp, _rax_reservation.cvt32().unwrap());

    // status = 1 (failure by default).
    ra.asm.mov(status.cvt32().unwrap(), 1u32).unwrap();

    // If exclusive_state == 0 → jump to end (no reservation to consume).
    let excl_state_off = A32JitState::offset_of_exclusive_state();
    ra.asm
        .movzx(
            tmp.cvt32().unwrap(),
            byte_ptr(RegExp::from(R15) + excl_state_off as i32),
        )
        .unwrap();
    ra.asm
        .test(tmp.cvt8().unwrap(), tmp.cvt8().unwrap())
        .unwrap();
    ra.asm.je(&end, rxbyak::JmpType::Near).unwrap();

    // If monitor.exclusive_addresses[pid] != vaddr → jump to end.
    ra.asm.mov(tmp, addr_ptr as u64 as i64).unwrap();
    ra.asm.cmp(qword_ptr(RegExp::from(tmp)), vaddr).unwrap();
    ra.asm.jne(&end, rxbyak::JmpType::Near).unwrap();

    // Clear OTHER processors' reservations matching vaddr.
    emit_exclusive_test_and_clear(ctx, &mut *ra.asm, vaddr, tmp, _rax_reservation);

    // Clear our exclusive_state (we're consuming the reservation).
    ra.asm
        .mov(byte_ptr(RegExp::from(R15) + excl_state_off as i32), 0i32)
        .unwrap();

    // Load saved exclusive value into RAX from monitor.exclusive_values[pid].
    ra.asm.mov(tmp, value_ptr as u64 as i64).unwrap();
    ra.asm
        .mov(_rax_reservation, qword_ptr(RegExp::from(tmp)))
        .unwrap();

    let vaddr_idx = vaddr.get_idx();
    let value_idx = value.get_idx();
    let fallbacks = unsafe {
        &*(ctx
            .fastmem_fallbacks
            .expect("A32 exclusive write requires fallback table")
            as *const FastmemFallbacksTable)
    };
    let wrapped_fn_off = fallbacks.exclusive_write_stub(true, bitsize, vaddr_idx, value_idx);

    if should_fastmem(ctx, inst_ref) {
        // Inline fastmem `lock cmpxchg [r13+vaddr], value` (`mem = value`
        // when it still equals RAX).
        let addr_expr = RegExp::from(rxbyak::R13) + vaddr;
        let inst_offset = ra.asm.size();
        ra.asm.lock().unwrap();
        match bitsize {
            8 => ra
                .asm
                .cmpxchg(byte_ptr(addr_expr), value.cvt8().unwrap())
                .unwrap(),
            16 => ra
                .asm
                .cmpxchg(word_ptr(addr_expr), value.cvt16().unwrap())
                .unwrap(),
            32 => ra
                .asm
                .cmpxchg(dword_ptr(addr_expr), value.cvt32().unwrap())
                .unwrap(),
            64 => ra.asm.cmpxchg(qword_ptr(addr_expr), value).unwrap(),
            _ => unreachable!("emit_a32_exclusive_write_inline: bitsize {}", bitsize),
        }

        // Normal fastmem result: status = !ZF.
        ra.asm.setnz(status.cvt8().unwrap()).unwrap();
        ra.asm
            .movzx(status.cvt32().unwrap(), status.cvt8().unwrap())
            .unwrap();

        // A fault fake-calls the pre-generated callback and resumes in this
        // deferred continuation, matching Eden's call/AL-to-status path.
        let marker = (ctx.location, inst_ref.0);
        let recompile = ctx.config.memory.recompile_on_exclusive_fastmem_failure;
        let fallback_end = end.clone();
        ctx.deferred_emits.borrow_mut().push(Box::new(move |dctx| {
            emit_call_to_offset(dctx.asm, wrapped_fn_off);
            let resume_offset = dctx.asm.size();
            dctx.fastmem_patches.add(
                dctx.code_base + inst_offset as u64,
                crate::backend::x64::exception_handler::FastmemPatchInfo::new(
                    dctx.code_base + resume_offset as u64,
                    dctx.code_base + wrapped_fn_off as u64,
                    Some(marker),
                    recompile,
                ),
            );
            dctx.asm
                .xor_(status.cvt32().unwrap(), status.cvt32().unwrap())
                .unwrap();
            dctx.asm
                .test(RAX.cvt8().unwrap(), RAX.cvt8().unwrap())
                .unwrap();
            dctx.asm.setz(status.cvt8().unwrap()).unwrap();
            dctx.asm.jmp(&fallback_end, JmpType::Near).unwrap();
        }));
    } else {
        emit_call_to_offset(ra.asm, wrapped_fn_off);
        ra.asm
            .xor_(status.cvt32().unwrap(), status.cvt32().unwrap())
            .unwrap();
        ra.asm
            .test(RAX.cvt8().unwrap(), RAX.cvt8().unwrap())
            .unwrap();
        ra.asm.setz(status.cvt8().unwrap()).unwrap();
    }

    // BUG FIX: `end:` must come BEFORE the unlock so that the early-exit
    // `je`/`jne` branches above (which target `end`) still release the spin
    // lock. Mirrors upstream `emit_x64_memory.cpp.inc:533-535` exactly:
    //
    //     code.L(*end);
    //     EmitExclusiveUnlock(code, conf, tmp, eax);
    //
    // The earlier version put `unlock` before `bind(end)` — every failed
    // reservation check jumped past the unlock, leaving the global monitor
    // ~30× fewer SVCs when write-inline was on.
    ra.asm.bind(&end).unwrap();

    // Release monitor spin lock.
    if locked {
        emit_exclusive_unlock(ctx, &mut *ra.asm, tmp, _rax_reservation.cvt32().unwrap());
    }

    ra.define_value(inst_ref, status);
    emit_check_memory_abort(ra.asm, memory_abort_info(ctx, inst), None);
}

pub fn emit_a32_exclusive_read_memory_8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_exclusive_read(ctx, ra, inst_ref, inst, 8);
}
pub fn emit_a32_exclusive_read_memory_16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_exclusive_read(ctx, ra, inst_ref, inst, 16);
}
pub fn emit_a32_exclusive_read_memory_32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_exclusive_read(ctx, ra, inst_ref, inst, 32);
}
pub fn emit_a32_exclusive_read_memory_64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_exclusive_read(ctx, ra, inst_ref, inst, 64);
}

pub fn emit_a32_exclusive_write_memory_8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_exclusive_write(ctx, ra, inst_ref, inst, 8);
}
pub fn emit_a32_exclusive_write_memory_16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_exclusive_write(ctx, ra, inst_ref, inst, 16);
}
pub fn emit_a32_exclusive_write_memory_32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_exclusive_write(ctx, ra, inst_ref, inst, 32);
}
pub fn emit_a32_exclusive_write_memory_64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_exclusive_write(ctx, ra, inst_ref, inst, 64);
}

/// A32ClearExclusive: clear exclusive monitor
pub fn emit_a32_clear_exclusive(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    let excl_offset = A32JitState::offset_of_exclusive_state();
    ra.asm
        .mov(byte_ptr(RegExp::from(R15) + excl_offset as i32), 0i32)
        .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::x64::callback::{Callback, SimpleCallback};

    fn callback() -> Box<dyn Callback> {
        Box::new(SimpleCallback::new(0))
    }

    fn callbacks() -> EmitCallbacks {
        EmitCallbacks {
            memory_read_8: callback(),
            memory_read_16: callback(),
            memory_read_32: callback(),
            memory_read_64: callback(),
            memory_read_128: callback(),
            memory_write_8: callback(),
            memory_write_16: callback(),
            memory_write_32: callback(),
            memory_write_64: callback(),
            memory_write_128: callback(),
            call_supervisor: callback(),
            exception_raised: callback(),
            data_cache_operation: callback(),
            instruction_cache_operation: callback(),
            instruction_synchronization_barrier: callback(),
            add_ticks: callback(),
            get_ticks_remaining: callback(),
            exclusive_clear: callback(),
            exclusive_read_8: callback(),
            exclusive_read_16: callback(),
            exclusive_read_32: callback(),
            exclusive_read_64: callback(),
            exclusive_read_128: callback(),
            get_cntpct: callback(),
            exclusive_write_8: callback(),
            exclusive_write_16: callback(),
            exclusive_write_32: callback(),
            exclusive_write_64: callback(),
            exclusive_write_128: callback(),
        }
    }

    fn raw_exclusive_callbacks() -> RawExclusiveWriteCallbacks {
        RawExclusiveWriteCallbacks {
            write_8: callback(),
            write_16: callback(),
            write_32: callback(),
            write_64: callback(),
            write_128: callback(),
        }
    }

    #[test]
    fn a32_fallback_table_matches_upstream_scalar_inventory() {
        let mut asm = CodeAssembler::new(2 * 1024 * 1024).unwrap();
        let callbacks = callbacks();
        let raw_callbacks = raw_exclusive_callbacks();
        let table = gen_fastmem_fallbacks(&mut asm, &callbacks, Some(&raw_callbacks));
        let expected = 2 * VALID_GPR_IDXES.len() * VALID_GPR_IDXES.len() * 4;

        assert_eq!(table.read.len(), expected);
        assert_eq!(table.write.len(), expected);
        assert_eq!(table.exclusive_write.len(), expected);
        assert!(table.read.keys().all(|(_, bitsize, _, _)| *bitsize <= 64));
        assert!(table.write.keys().all(|(_, bitsize, _, _)| *bitsize <= 64));
        assert!(table
            .exclusive_write
            .keys()
            .all(|(_, bitsize, _, _)| *bitsize <= 64));
    }

    #[test]
    fn disabled_memory_abort_check_emits_nothing() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        emit_check_memory_abort(
            &mut asm,
            MemoryAbortInfo {
                enabled: false,
                current_pc: 0x1234_5678,
                upper_location_descriptor: Some(0x0765_4321),
                dispatcher_offsets: None,
                code_base_ptr: core::ptr::null(),
            },
            None,
        );
        assert_eq!(asm.size(), 0);
    }

    #[test]
    fn enabled_memory_abort_check_embeds_exact_a32_resume_state() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let current_pc = 0x1234_5678u32;
        let upper = 0x0765_4321u32;
        emit_check_memory_abort(
            &mut asm,
            MemoryAbortInfo {
                enabled: true,
                current_pc,
                upper_location_descriptor: Some(upper),
                dispatcher_offsets: None,
                code_base_ptr: core::ptr::null(),
            },
            None,
        );

        let code = asm.code();
        assert!(code
            .windows(4)
            .any(|bytes| bytes == current_pc.to_le_bytes()));
        assert!(code.windows(4).any(|bytes| bytes == upper.to_le_bytes()));
        assert!(code
            .windows(4)
            .any(|bytes| { bytes == (A32JitState::offset_of_halt_reason() as u32).to_le_bytes() }));
        assert!(code.contains(&(A32JitState::reg_offset(15) as u8)));
    }
}

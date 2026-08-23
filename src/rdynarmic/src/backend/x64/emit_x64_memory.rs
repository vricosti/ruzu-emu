//! Shared memory-emit helpers for the A64 (and potentially A32) fastmem
//! / page-table paths.
//!
//! Port of upstream `dynarmic/src/dynarmic/backend/x64/emit_x64_memory.h`
//! (anonymous-namespace inline templates instantiated by both A32 and A64).
//!
//! The current rdynarmic A32 fastmem path is per-emission and lives in
//! `a32_emit_a32.rs`; it does NOT use these helpers. Per the porting
//! decision (option A, A64-only initially), only the A64-specialised
//! variants are implemented here. The A32 specialisations are left as
//! `unimplemented!()` stubs for future migration.
//!
//! ## Layout
//!
//! - `is_ordered` — predicate matching upstream `IsOrdered`.
//! - `emit_read_memory_mov<BITSIZE>` / `emit_write_memory_mov<BITSIZE>` —
//!   bitsize-dispatched, arch-agnostic load/store emitters that produce
//!   the fastmem `mov` (or atomic equivalent for ordered accesses) and
//!   return its byte offset (= upstream's `const void* fastmem_location`).
//! - `emit_detect_misaligned_vaddr` — pushes a deferred-emit closure to
//!   handle page-boundary-crossing detection.
//! - `emit_fastmem_vaddr_a64` — emits the fastmem-specific vaddr masking
//!   and returns the effective `[r13 + ...]` address.
//! - `emit_vaddr_lookup_a64` — emits the page-table lookup and returns
//!   the effective `[page + offset]` address (page-table path).
//! Global-monitor helpers and the exclusive-inline emitters live in
//! `emit_exclusive_memory.rs`, matching the separate behavioral owner in the
//! Rust backend while this file retains the shared address/move templates.

use rxbyak::{
    byte_ptr, dword_ptr, qword_ptr, word_ptr, xmmword_ptr, CodeAssembler, JmpType, Label, Reg,
    RegExp,
};

use crate::backend::x64::emit_context::{DeferredEmit, DeferredEmitCtx, EmitContext};
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::ir::acc_type::AccType;

/// Page-table page size constants. Mirror upstream
/// `constexpr size_t page_bits/page_size/page_mask` in `emit_x64_memory.h`.
pub const PAGE_BITS: usize = 12;
pub const PAGE_SIZE: usize = 1 << PAGE_BITS;
pub const PAGE_MASK: usize = PAGE_SIZE - 1;

/// Whether the access type makes a memory access "ordered" (requires
/// fence semantics).
///
/// Matches upstream `inline bool IsOrdered(IR::AccType acctype)` in
/// `emit_x64_memory.h:382-384`. ARM's "ORDEREDRW" maps to rdynarmic's
/// `OrderedRw`.
#[inline]
pub fn is_ordered(acc: AccType) -> bool {
    matches!(
        acc,
        AccType::Ordered | AccType::OrderedRw | AccType::LimitedOrdered
    )
}

// ---------------------------------------------------------------------------
// EmitReadMemoryMov / EmitWriteMemoryMov — bitsize-dispatched mov emitters.
//
// Both functions return the byte offset of the emitted "fastmem location"
// instruction in the assembler's code buffer (i.e. the offset of the byte
// after the prefix(es) that may come before the actual mov; matches
// upstream where the SIGSEGV handler keys patches by the RIP of the mov).
//
// Parameters:
//   asm        — the assembler (the helpers ONLY emit, they do NOT bind
//                labels here)
//   value_idx  — host register index (0..=15 for GPR, 0..=15 for XMM)
//   addr       — RegExp the access addresses, e.g. `r13 + vaddr`
//   ordered    — whether the access is ordered (uses xadd/xchg + LOCK
//                instead of plain mov, matching upstream)
//
// The 128-bit ordered path is currently `unimplemented!()` — decision 2
// defers 128-bit fastmem.
// ---------------------------------------------------------------------------

/// Equivalent of Xbyak's `Reg32{index}.cvt8()` / `Reg64{index}.cvt8()`.
/// rxbyak represents the REX-only low-byte registers explicitly, so indices
/// 4..=7 must not be constructed as the legacy AH/CH/DH/BH registers.
fn low_byte_reg(index: u8) -> Reg {
    if (4..8).contains(&index) {
        Reg::new_ext8(index)
    } else {
        Reg::gpr8(index)
    }
}

/// Emit a fastmem (or page-table) read move for a `BITSIZE`-bit access.
/// Returns the code-buffer offset of the emitted memory instruction.
///
/// Mirrors upstream `EmitReadMemoryMov<bitsize>` in `emit_x64_memory.h:202-271`.
#[allow(clippy::too_many_arguments)]
pub fn emit_read_memory_mov<const BITSIZE: usize>(
    asm: &mut CodeAssembler,
    value_idx: u8,
    addr: RegExp,
    ordered: bool,
) -> usize {
    if ordered {
        // Pre-zero the destination so the lock-xadd accumulates correctly.
        // For 128-bit, zero the four register accumulators rax/rbx/rcx/rdx
        // that cmpxchg16b uses (matches upstream exactly).
        if BITSIZE != 128 {
            let v32 = Reg::gpr32(value_idx);
            asm.xor_(v32, v32).unwrap();
        } else {
            asm.xor_(rxbyak::EAX, rxbyak::EAX).unwrap();
            asm.xor_(rxbyak::EBX, rxbyak::EBX).unwrap();
            asm.xor_(rxbyak::ECX, rxbyak::ECX).unwrap();
            asm.xor_(rxbyak::EDX, rxbyak::EDX).unwrap();
        }

        let fastmem_location = asm.size();
        match BITSIZE {
            8 => {
                asm.lock().unwrap();
                asm.xadd(byte_ptr(addr), low_byte_reg(value_idx)).unwrap();
            }
            16 => {
                asm.lock().unwrap();
                asm.xadd(word_ptr(addr), Reg::gpr16(value_idx)).unwrap();
            }
            32 => {
                asm.lock().unwrap();
                asm.xadd(dword_ptr(addr), Reg::gpr32(value_idx)).unwrap();
            }
            64 => {
                asm.lock().unwrap();
                asm.xadd(qword_ptr(addr), Reg::gpr64(value_idx)).unwrap();
            }
            128 => {
                // `lock cmpxchg16b [addr]` with rax:rdx=0 and rbx:rcx=0:
                // the compare fails (memory != 0:0 in general), the CPU
                // atomically loads the 16-byte value into rdx:rax, no
                // store occurs. Acts as a 16-byte acquire-load. Pack
                // rax/rdx into `xmm{value_idx}`. Mirrors upstream
                // `EmitReadMemoryMov<128>` in `emit_x64_memory.h:232-242`.
                asm.lock().unwrap();
                asm.cmpxchg16b(xmmword_ptr(addr)).unwrap();
                asm.movq(Reg::xmm(value_idx), rxbyak::RAX).unwrap();
                asm.pinsrq(Reg::xmm(value_idx), rxbyak::RDX, 1).unwrap();
            }
            _ => unreachable!("invalid bitsize: {}", BITSIZE),
        }
        return fastmem_location;
    }

    let fastmem_location = asm.size();
    match BITSIZE {
        8 => {
            asm.movzx(Reg::gpr32(value_idx), byte_ptr(addr)).unwrap();
        }
        16 => {
            asm.movzx(Reg::gpr32(value_idx), word_ptr(addr)).unwrap();
        }
        32 => {
            asm.mov(Reg::gpr32(value_idx), dword_ptr(addr)).unwrap();
        }
        64 => {
            asm.mov(Reg::gpr64(value_idx), qword_ptr(addr)).unwrap();
        }
        128 => {
            asm.movups(Reg::xmm(value_idx), xmmword_ptr(addr)).unwrap();
        }
        _ => unreachable!("invalid bitsize: {}", BITSIZE),
    }
    fastmem_location
}

/// Emit a fastmem (or page-table) write move for a `BITSIZE`-bit access.
/// Returns the code-buffer offset of the emitted memory instruction.
///
/// Mirrors upstream `EmitWriteMemoryMov<bitsize>` in `emit_x64_memory.h:273-339`.
#[allow(clippy::too_many_arguments)]
pub fn emit_write_memory_mov<const BITSIZE: usize>(
    asm: &mut CodeAssembler,
    addr: RegExp,
    value_idx: u8,
    ordered: bool,
) -> usize {
    if ordered {
        if BITSIZE == 128 {
            // Set up rdx:rax = 0 (expected = 0) and rcx:rbx = value (new).
            // The first cmpxchg16b iteration loads memory into rdx:rax (it
            // doesn't equal 0), the second iteration finds rdx:rax matches
            // memory and atomically swaps in rcx:rbx. Acts as a release-store.
            // Mirrors upstream `EmitWriteMemoryMov<128>` in
            // `emit_x64_memory.h:276-311`.
            asm.xor_(rxbyak::EAX, rxbyak::EAX).unwrap();
            asm.xor_(rxbyak::EDX, rxbyak::EDX).unwrap();
            asm.movq(rxbyak::RBX, Reg::xmm(value_idx)).unwrap();
            asm.pextrq(rxbyak::RCX, Reg::xmm(value_idx), 1).unwrap();
        }

        let fastmem_location = asm.size();
        match BITSIZE {
            8 => {
                asm.xchg(byte_ptr(addr), low_byte_reg(value_idx)).unwrap();
            }
            16 => {
                asm.xchg(word_ptr(addr), Reg::gpr16(value_idx)).unwrap();
            }
            32 => {
                asm.xchg(dword_ptr(addr), Reg::gpr32(value_idx)).unwrap();
            }
            64 => {
                asm.xchg(qword_ptr(addr), Reg::gpr64(value_idx)).unwrap();
            }
            128 => {
                // `loop: lock cmpxchg16b [addr]; jnz loop;` — see comment
                // above.
                let loop_lbl = asm.create_label();
                asm.bind(&loop_lbl).unwrap();
                asm.lock().unwrap();
                asm.cmpxchg16b(xmmword_ptr(addr)).unwrap();
                asm.jnz(&loop_lbl, JmpType::Near).unwrap();
            }
            _ => unreachable!("invalid bitsize: {}", BITSIZE),
        }
        return fastmem_location;
    }

    let fastmem_location = asm.size();
    match BITSIZE {
        8 => {
            asm.mov(byte_ptr(addr), low_byte_reg(value_idx)).unwrap();
        }
        16 => {
            asm.mov(word_ptr(addr), Reg::gpr16(value_idx)).unwrap();
        }
        32 => {
            asm.mov(dword_ptr(addr), Reg::gpr32(value_idx)).unwrap();
        }
        64 => {
            asm.mov(qword_ptr(addr), Reg::gpr64(value_idx)).unwrap();
        }
        128 => {
            asm.movups(xmmword_ptr(addr), Reg::xmm(value_idx)).unwrap();
        }
        _ => unreachable!("invalid bitsize: {}", BITSIZE),
    }
    fastmem_location
}

// ---------------------------------------------------------------------------
// EmitDetectMisalignedVAddr — page-boundary-cross misalignment check.
// ---------------------------------------------------------------------------

/// Emit code that aborts the access if the virtual address is misaligned
/// for `bitsize`. Pushes a deferred-emit closure to handle the slow-path
/// "is the access actually crossing a page boundary?" check when
/// `only_detect_misalignment_via_page_table_on_page_boundary` is set.
///
/// Mirrors upstream `EmitDetectMisalignedVAddr<EmitContext>` in
/// `emit_x64_memory.h:27-69`. A64-specialised: only callable when
/// `ctx.arch == ArchConfig::A64`.
pub fn emit_detect_misaligned_vaddr(
    asm: &mut CodeAssembler,
    ctx: &EmitContext,
    bitsize: usize,
    abort: Label,
    vaddr: Reg,
    tmp: Reg,
) {
    let mem_conf = &ctx.config.memory;

    if bitsize == 8 || (mem_conf.detect_misaligned_access_via_page_table & bitsize as u32) == 0 {
        return;
    }

    let align_mask: u32 = match bitsize {
        16 => 0b1,
        32 => 0b11,
        64 => 0b111,
        128 => 0b1111,
        _ => unreachable!(),
    };

    asm.test(vaddr, align_mask as i32).unwrap();

    if !mem_conf.only_detect_misalignment_via_page_table_on_page_boundary {
        asm.jne(&abort, rxbyak::JmpType::Near).unwrap();
        return;
    }

    let page_align_mask: u32 = ((PAGE_SIZE - 1) as u32) & !align_mask;

    // Forward-jump to a `detect_boundary` label which is bound by the
    // deferred emit; the fallthrough returns to the caller's `resume`
    // label.
    let detect_boundary = asm.create_label();
    let resume = asm.create_label();

    asm.jne(&detect_boundary, rxbyak::JmpType::Near).unwrap();
    asm.bind(&resume).unwrap();

    let vaddr_idx = vaddr.get_idx();
    let tmp_idx = tmp.get_idx();
    ctx.deferred_emits
        .borrow_mut()
        .push(Box::new(move |dctx: &mut DeferredEmitCtx<'_>| {
            let asm = &mut *dctx.asm;
            asm.bind(&detect_boundary).unwrap();
            let tmp = Reg::gpr64(tmp_idx);
            let vaddr = Reg::gpr64(vaddr_idx);
            asm.mov(tmp, vaddr).unwrap();
            asm.and_(tmp, page_align_mask as i32).unwrap();
            asm.cmp(tmp, page_align_mask as i32).unwrap();
            asm.jne(&resume, rxbyak::JmpType::Near).unwrap();
            // Fallthrough into the abort handler emitted by the parent
            // deferred-emit closure (matches upstream NOTE).
        }));
}

// ---------------------------------------------------------------------------
// EmitFastmemVAddr / EmitVAddrLookup — A64-specialised vaddr emitters.
// ---------------------------------------------------------------------------

/// Emit fastmem virtual-address translation for A64. Returns the
/// effective address (`[r13 + ...]`).
///
/// Mirrors upstream `EmitFastmemVAddr<A64EmitContext>` in
/// `emit_x64_memory.h:163-200`.
///
/// `require_abort_handling` is set to `true` if the emitter inserts an
/// out-of-range conditional jump to `abort`. The caller uses this to
/// decide whether the abort label must be bound at all.
pub fn emit_fastmem_vaddr_a64(
    ra: &mut RegAlloc,
    ctx: &EmitContext,
    abort: Label,
    vaddr: Reg,
    require_abort_handling: &mut bool,
    tmp: Option<Reg>,
) -> RegExp {
    let mem_conf = &ctx.config.memory;
    let unused_top_bits: usize = 64 - mem_conf.fastmem_address_space_bits;

    if unused_top_bits == 0 {
        return RegExp::from(rxbyak::R13) + vaddr;
    }

    if mem_conf.silently_mirror_fastmem {
        let tmp = tmp.unwrap_or_else(|| ra.scratch_gpr());
        if unused_top_bits < 32 {
            ra.asm.mov(tmp, vaddr).unwrap();
            ra.asm.shl(tmp, unused_top_bits as u8).unwrap();
            ra.asm.shr(tmp, unused_top_bits as u8).unwrap();
        } else if unused_top_bits == 32 {
            // `mov reg32, reg32` zero-extends into the upper 32 bits.
            ra.asm
                .mov(tmp.cvt32().unwrap(), vaddr.cvt32().unwrap())
                .unwrap();
        } else {
            ra.asm
                .mov(tmp.cvt32().unwrap(), vaddr.cvt32().unwrap())
                .unwrap();
            let mask = ((1u64 << mem_conf.fastmem_address_space_bits) - 1) as i32;
            ra.asm.and_(tmp, mask).unwrap();
        }
        return RegExp::from(rxbyak::R13) + tmp;
    }

    // Abort-on-out-of-range path.
    if mem_conf.fastmem_address_space_bits < 32 {
        // `test vaddr, ~((1<<bits) - 1)` — non-zero means out of range.
        let mask: i32 = -(1i64 << mem_conf.fastmem_address_space_bits) as i32;
        ra.asm.test(vaddr, mask).unwrap();
        ra.asm.jne(&abort, rxbyak::JmpType::Near).unwrap();
        *require_abort_handling = true;
    } else {
        let tmp = tmp.unwrap_or_else(|| ra.scratch_gpr());
        ra.asm.mov(tmp, vaddr).unwrap();
        ra.asm
            .shr(tmp, mem_conf.fastmem_address_space_bits as u8)
            .unwrap();
        ra.asm.jne(&abort, rxbyak::JmpType::Near).unwrap();
        *require_abort_handling = true;
    }
    RegExp::from(rxbyak::R13) + vaddr
}

/// Emit page-table lookup for A64. Returns the effective address pointing
/// at the host-mapped page+offset.
///
/// Mirrors upstream `EmitVAddrLookup<A64EmitContext>` in
/// `emit_x64_memory.h:102-152`. ruzu does not currently set
/// `page_table_present = true` so this path is dead code, but it is
/// ported for upstream parity.
///
/// Convention: the emitter assumes `r14` holds the page-table pointer
/// (matching upstream which passes `page_table` via `r14`).
pub fn emit_vaddr_lookup_a64(
    ra: &mut RegAlloc,
    ctx: &EmitContext,
    bitsize: usize,
    abort: Label,
    vaddr: Reg,
) -> RegExp {
    let mem_conf = &ctx.config.memory;
    let valid_page_index_bits = mem_conf.page_table_address_space_bits - PAGE_BITS;
    let unused_top_bits = 64 - mem_conf.page_table_address_space_bits;

    let page = ra.scratch_gpr();
    let tmp = if mem_conf.absolute_offset_page_table {
        page
    } else {
        ra.scratch_gpr()
    };

    emit_detect_misaligned_vaddr(ra.asm, ctx, bitsize, abort, vaddr, tmp);

    if unused_top_bits == 0 {
        ra.asm.mov(tmp, vaddr).unwrap();
        ra.asm.shr(tmp, PAGE_BITS as u8).unwrap();
    } else if mem_conf.silently_mirror_page_table {
        if valid_page_index_bits >= 32 {
            if ctx.has_host_feature(HostFeature::BMI2) {
                let bit_count = ra.scratch_gpr();
                ra.asm.mov(bit_count, unused_top_bits as i32).unwrap();
                ra.asm.bzhi(tmp, vaddr, bit_count).unwrap();
                ra.asm.shr(tmp, PAGE_BITS as u8).unwrap();
                ra.release(bit_count);
            } else {
                ra.asm.mov(tmp, vaddr).unwrap();
                ra.asm.shl(tmp, unused_top_bits as u8).unwrap();
                ra.asm
                    .shr(tmp, (unused_top_bits + PAGE_BITS) as u8)
                    .unwrap();
            }
        } else {
            ra.asm.mov(tmp, vaddr).unwrap();
            ra.asm.shr(tmp, PAGE_BITS as u8).unwrap();
            let mask = ((1u32 << valid_page_index_bits) - 1) as i32;
            ra.asm.and_(tmp, mask).unwrap();
        }
    } else {
        debug_assert!(valid_page_index_bits < 32);
        ra.asm.mov(tmp, vaddr).unwrap();
        ra.asm.shr(tmp, PAGE_BITS as u8).unwrap();
        let mask = -(1i64 << valid_page_index_bits) as i32;
        ra.asm.test(tmp, mask).unwrap();
        ra.asm.jne(&abort, rxbyak::JmpType::Near).unwrap();
    }

    // page = r14[tmp * 8]
    ra.asm
        .mov(page, qword_ptr(RegExp::from(rxbyak::R14) + tmp * 8u8))
        .unwrap();

    if mem_conf.page_table_pointer_mask_bits == 0 {
        ra.asm.test(page, page).unwrap();
    } else {
        let mask = (!0u32 << mem_conf.page_table_pointer_mask_bits) as i32;
        ra.asm.and_(page, mask).unwrap();
    }
    ra.asm.je(&abort, rxbyak::JmpType::Near).unwrap();

    if mem_conf.absolute_offset_page_table {
        return RegExp::from(page) + vaddr;
    }

    ra.asm.mov(tmp, vaddr).unwrap();
    ra.asm.and_(tmp, PAGE_MASK as i32).unwrap();
    RegExp::from(page) + tmp
}

/// Emit the A32 page-table lookup from upstream
/// `EmitVAddrLookup<A32EmitContext>`. Reden's host-pointer table stores
/// eight-byte entries directly, so `tmp * 8` is the Rust counterpart of
/// upstream's configurable `page_table_log2_stride`.
pub fn emit_vaddr_lookup_a32(
    ra: &mut RegAlloc,
    ctx: &EmitContext,
    bitsize: usize,
    abort: Label,
    vaddr: Reg,
) -> RegExp {
    let mem_conf = &ctx.config.memory;
    let page = ra.scratch_gpr();
    let tmp = if mem_conf.absolute_offset_page_table {
        page
    } else {
        ra.scratch_gpr()
    };

    emit_detect_misaligned_vaddr(ra.asm, ctx, bitsize, abort, vaddr, tmp);

    // Upstream A32 assumes the virtual address was zero-extended from 32 bits.
    ra.asm
        .mov(tmp.cvt32().unwrap(), vaddr.cvt32().unwrap())
        .unwrap();
    ra.asm.shr(tmp.cvt32().unwrap(), PAGE_BITS as u8).unwrap();
    ra.asm
        .mov(page, qword_ptr(RegExp::from(rxbyak::R14) + tmp * 8u8))
        .unwrap();

    if mem_conf.page_table_pointer_mask_bits == 0 {
        ra.asm.test(page, page).unwrap();
    } else {
        let mask = (!0u32 << mem_conf.page_table_pointer_mask_bits) as i32;
        ra.asm.and_(page, mask).unwrap();
    }
    ra.asm.je(&abort, rxbyak::JmpType::Near).unwrap();

    if mem_conf.absolute_offset_page_table {
        return RegExp::from(page) + vaddr;
    }

    ra.asm
        .mov(tmp.cvt32().unwrap(), vaddr.cvt32().unwrap())
        .unwrap();
    ra.asm.and_(tmp.cvt32().unwrap(), PAGE_MASK as i32).unwrap();
    RegExp::from(page) + tmp
}

// Suppress the "unused import" warning while M3-M5 land.
#[allow(dead_code)]
fn _suppress_unused_warnings(_: DeferredEmit) {}

#[cfg(test)]
mod tests {
    use super::*;
    use rxbyak::{R13, RAX};

    /// Verify is_ordered matches upstream IsOrdered semantics.
    #[test]
    fn test_is_ordered() {
        assert!(!is_ordered(AccType::Normal));
        assert!(!is_ordered(AccType::Vec));
        assert!(!is_ordered(AccType::Atomic));
        assert!(is_ordered(AccType::Ordered));
        assert!(is_ordered(AccType::OrderedRw));
        assert!(is_ordered(AccType::LimitedOrdered));
        assert!(!is_ordered(AccType::Unpriv));
        assert!(!is_ordered(AccType::Ifetch));
    }

    /// `mov eax, dword [r13+rax]` should be 5 bytes
    /// (REX.B 0x41 | opcode 0x8B | ModR/M 0x44 | SIB 0x05 | disp8 0x00).
    /// Verify the read helper emits exactly that and returns the start
    /// offset of the mov.
    #[test]
    fn test_emit_read_memory_mov_32_unordered() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let addr = RegExp::from(R13) + RAX;
        let off = emit_read_memory_mov::<32>(&mut asm, 0 /*=eax*/, addr, false);
        assert_eq!(off, 0, "should be at start of buffer");
        let bytes = asm.code();
        assert_eq!(
            bytes,
            &[0x41, 0x8B, 0x44, 0x05, 0x00],
            "expected `mov eax, [r13+rax]` encoding"
        );
    }

    /// `mov dword [r13+rax], eax` — write-mov mirror.
    #[test]
    fn test_emit_write_memory_mov_32_unordered() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let addr = RegExp::from(R13) + RAX;
        let off = emit_write_memory_mov::<32>(&mut asm, addr, 0 /*=eax*/, false);
        assert_eq!(off, 0);
        let bytes = asm.code();
        assert_eq!(
            bytes,
            &[0x41, 0x89, 0x44, 0x05, 0x00],
            "expected `mov [r13+rax], eax` encoding"
        );
    }

    #[test]
    fn test_emit_write_memory_mov_8_uses_low_byte_register() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let off = emit_write_memory_mov::<8>(&mut asm, RegExp::from(RAX), 6, false);
        assert_eq!(off, 0);
        assert_eq!(
            asm.code(),
            &[0x40, 0x88, 0x30],
            "expected `mov byte [rax], sil`, not `mov byte [rax], dh`"
        );
    }

    #[test]
    fn test_emit_write_memory_mov_8_ordered_uses_low_byte_register() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let off = emit_write_memory_mov::<8>(&mut asm, RegExp::from(RAX), 6, true);
        assert_eq!(off, 0);
        assert_eq!(
            asm.code(),
            &[0x40, 0x86, 0x30],
            "expected `xchg byte [rax], sil`, not `xchg byte [rax], dh`"
        );
    }

    #[test]
    fn test_emit_read_memory_mov_8_ordered_uses_low_byte_register() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let off = emit_read_memory_mov::<8>(&mut asm, 6, RegExp::from(RAX), true);
        assert_eq!(
            &asm.code()[off..],
            &[0xF0, 0x40, 0x0F, 0xC0, 0x30],
            "expected `lock xadd byte [rax], sil`, not `... dh`"
        );
    }

    /// Ordered 32-bit read: `xor eax,eax` (preamble) + `lock xadd
    /// dword [r13+rax], eax`. Verify that `fastmem_location` points
    /// past the xor (matching upstream where it points at the LOCK
    /// prefix), and that the LOCK + XADD sequence at that offset is
    /// `F0 41 0F C1 44 05 00` (= `lock xadd [r13+rax], eax`).
    /// The exact xor preamble encoding (33 C0 vs 31 C0 vs another
    /// alternate form) is rxbyak's choice and doesn't affect
    /// correctness — only that it zeros eax.
    #[test]
    fn test_emit_read_memory_mov_32_ordered() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let addr = RegExp::from(R13) + RAX;
        let off = emit_read_memory_mov::<32>(&mut asm, 0, addr, true);
        let bytes = asm.code();
        // Bytes from `off` onward must be the LOCK XADD encoding.
        assert_eq!(
            &bytes[off..],
            &[0xF0, 0x41, 0x0F, 0xC1, 0x44, 0x05, 0x00],
            "expected `lock xadd [r13+rax], eax` starting at fastmem_location"
        );
        // The preamble (bytes [0..off]) must zero eax — no LOCK prefix
        // in there. Verify it doesn't contain 0xF0.
        assert!(!bytes[..off].contains(&0xF0));
        assert!(off >= 2, "preamble must include at least the xor reg-reg");
    }

    /// Ordered 32-bit write: `lock` (1 byte) + `xchg dword [r13+rax],
    /// eax` (5 bytes) = 6 bytes total — but x86's `xchg mem,reg` has an
    /// implicit lock so xbyak/rxbyak does NOT emit a redundant LOCK
    /// prefix in upstream's `EmitWriteMemoryMov`. Note: upstream emits
    /// only `xchg` (no explicit `code.lock()`), see line 290-302 of
    /// `emit_x64_memory.h`.
    #[test]
    fn test_emit_write_memory_mov_32_ordered() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let addr = RegExp::from(R13) + RAX;
        let off = emit_write_memory_mov::<32>(&mut asm, addr, 0, true);
        assert_eq!(off, 0);
        let bytes = asm.code();
        assert_eq!(
            bytes,
            &[0x41, 0x87, 0x44, 0x05, 0x00],
            "expected `xchg [r13+rax], eax` encoding"
        );
    }

    /// Ordered 128-bit read: `xor eax,eax; xor ebx,ebx; xor ecx,ecx; xor
    /// edx,edx;` preamble, then `lock cmpxchg16b [r13+rax]; movq xmm0,
    /// rax; pinsrq xmm0, rdx, 1` after `fastmem_location`. Verify the
    /// LOCK prefix is at the returned offset.
    #[test]
    fn test_emit_read_memory_mov_128_ordered() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let addr = RegExp::from(R13) + RAX;
        let off = emit_read_memory_mov::<128>(&mut asm, 0, addr, true);
        let bytes = asm.code();
        // Preamble (4 xor reg32, reg32) is 4*2 = 8 bytes minimum (the
        // shorter encoding `33 c0`/`31 c0` style is fine — both 2 bytes).
        assert!(off >= 8, "preamble must zero rax/rbx/rcx/rdx");
        // No LOCK prefix can appear in the preamble.
        assert!(!bytes[..off].contains(&0xF0));
        // First byte at fastmem_location must be the LOCK prefix.
        assert_eq!(bytes[off], 0xF0);
        // REX.WB + 0F C7 — the cmpxchg16b opcode prefix.
        assert_eq!(
            &bytes[off + 1..off + 4],
            &[0x49, 0x0F, 0xC7],
            "expected `lock cmpxchg16b ...` opcode"
        );
    }

    /// Ordered 128-bit write: pre-loop setup (`xor eax,eax; xor edx,edx;
    /// movq rbx, xmm0; pextrq rcx, xmm0, 1`) followed by the
    /// `loop: lock cmpxchg16b [addr]; jnz loop;` loop. The
    /// `fastmem_location` must point at the LOCK prefix that begins the
    /// loop body.
    #[test]
    fn test_emit_write_memory_mov_128_ordered() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let addr = RegExp::from(R13) + RAX;
        let off = emit_write_memory_mov::<128>(&mut asm, addr, 0, true);
        let bytes = asm.code();
        assert!(off > 0, "preamble must come before fastmem_location");
        assert!(!bytes[..off].contains(&0xF0));
        assert_eq!(bytes[off], 0xF0);
        assert_eq!(&bytes[off + 1..off + 4], &[0x49, 0x0F, 0xC7]);
        // The trailing `jnz loop` must encode a backward jump (sign-bit
        // set in the displacement). For a near-form `jnz` this is
        // `0F 85 disp32`. Find the `0F 85` after the cmpxchg16b and
        // confirm the disp32 is negative (high bit of last byte set).
        let cmpx_end = off + 8; // F0 + 49 + 0F + C7 + 4C + 05 + 00 = 7 bytes lock+cmpxchg16b
                                // (cmpxchg16b [r13+rax] = F0 49 0F C7 4C 05 00 = 7 bytes)
        let after_cmpx = &bytes[cmpx_end - 1..];
        // Search for `0F 85` near-jnz after cmpxchg16b.
        let pos = after_cmpx
            .windows(2)
            .position(|w| w == [0x0F, 0x85])
            .expect("expected `jnz near` after cmpxchg16b");
        let disp_off = (cmpx_end - 1) + pos + 2;
        let disp = i32::from_le_bytes(bytes[disp_off..disp_off + 4].try_into().unwrap());
        assert!(disp < 0, "jnz must branch backward to the loop label");
    }
}

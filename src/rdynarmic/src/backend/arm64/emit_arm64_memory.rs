//! ARM64 memory emission.
//!
//! Upstream owner: `backend/arm64/emit_arm64_memory.cpp`.

use crate::backend::arm64::abi::{XFASTMEM, XPAGETABLE, XSCRATCH0, XSCRATCH1, XSTATE};
#[cfg(test)]
use crate::backend::arm64::block_of_code::BlockOfCode;
use crate::backend::arm64::emit_arm64::{emit_relocation, LinkTarget};
use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::label::Label;
use crate::backend::arm64::reg_alloc::RegAlloc;
use crate::backend::arm64::reg_alloc::{HostLoc, HostLocKind};
use crate::ir::acc_type::AccType;
use crate::ir::cond::Cond;
use crate::ir::value::InstRef;
use rhazel::{BarrierOp, CodeGenerator, QReg, VReg, WReg, XReg};

const X0: u8 = 0;
const Q0: u8 = 0;
const Q8: u8 = 8;
const WZR: u8 = 31;
const PAGE_BITS: usize = 12;
const PAGE_SIZE: usize = 1 << PAGE_BITS;
const PAGE_MASK: u64 = (1 << PAGE_BITS) - 1;

fn inline_watch_ranges() -> &'static [(u64, u64)] {
    use std::sync::OnceLock;
    static RANGES: OnceLock<Vec<(u64, u64)>> = OnceLock::new();
    RANGES.get_or_init(|| {
        let raw = std::env::var("RUZU_A32_INLINE_WATCH_ADDR").unwrap_or_default();
        raw.split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .filter_map(|token| {
                let (addr, size) = token
                    .split_once(':')
                    .map(|(addr, size)| (addr, size.parse::<u64>().unwrap_or(8)))
                    .unwrap_or((token, 8));
                let digits = addr
                    .strip_prefix("0x")
                    .or_else(|| addr.strip_prefix("0X"))
                    .unwrap_or(addr);
                let start = u64::from_str_radix(digits, 16)
                    .ok()
                    .or_else(|| addr.parse::<u64>().ok())?;
                (start != 0).then(|| (start, start.saturating_add(size)))
            })
            .collect()
    })
}

fn is_ordered(acc_type: AccType) -> bool {
    matches!(
        acc_type,
        AccType::Ordered | AccType::OrderedRw | AccType::LimitedOrdered
    )
}

fn read_memory_link_target(bitsize: usize) -> Result<LinkTarget, String> {
    match bitsize {
        8 => Ok(LinkTarget::ReadMemory8),
        16 => Ok(LinkTarget::ReadMemory16),
        32 => Ok(LinkTarget::ReadMemory32),
        64 => Ok(LinkTarget::ReadMemory64),
        128 => Ok(LinkTarget::ReadMemory128),
        _ => Err(format!("Invalid ARM64 read-memory bitsize: {bitsize}")),
    }
}

fn write_memory_link_target(bitsize: usize) -> Result<LinkTarget, String> {
    match bitsize {
        8 => Ok(LinkTarget::WriteMemory8),
        16 => Ok(LinkTarget::WriteMemory16),
        32 => Ok(LinkTarget::WriteMemory32),
        64 => Ok(LinkTarget::WriteMemory64),
        128 => Ok(LinkTarget::WriteMemory128),
        _ => Err(format!("Invalid ARM64 write-memory bitsize: {bitsize}")),
    }
}

fn wrapped_read_memory_link_target(bitsize: usize) -> Result<LinkTarget, String> {
    match bitsize {
        8 => Ok(LinkTarget::WrappedReadMemory8),
        16 => Ok(LinkTarget::WrappedReadMemory16),
        32 => Ok(LinkTarget::WrappedReadMemory32),
        64 => Ok(LinkTarget::WrappedReadMemory64),
        128 => Ok(LinkTarget::WrappedReadMemory128),
        _ => Err(format!(
            "Invalid ARM64 wrapped-read-memory bitsize: {bitsize}"
        )),
    }
}

fn wrapped_write_memory_link_target(bitsize: usize) -> Result<LinkTarget, String> {
    match bitsize {
        8 => Ok(LinkTarget::WrappedWriteMemory8),
        16 => Ok(LinkTarget::WrappedWriteMemory16),
        32 => Ok(LinkTarget::WrappedWriteMemory32),
        64 => Ok(LinkTarget::WrappedWriteMemory64),
        128 => Ok(LinkTarget::WrappedWriteMemory128),
        _ => Err(format!(
            "Invalid ARM64 wrapped-write-memory bitsize: {bitsize}"
        )),
    }
}

fn exclusive_read_memory_link_target(bitsize: usize) -> Result<LinkTarget, String> {
    match bitsize {
        8 => Ok(LinkTarget::ExclusiveReadMemory8),
        16 => Ok(LinkTarget::ExclusiveReadMemory16),
        32 => Ok(LinkTarget::ExclusiveReadMemory32),
        64 => Ok(LinkTarget::ExclusiveReadMemory64),
        128 => Ok(LinkTarget::ExclusiveReadMemory128),
        _ => Err(format!(
            "Invalid ARM64 exclusive-read-memory bitsize: {bitsize}"
        )),
    }
}

fn exclusive_write_memory_link_target(bitsize: usize) -> Result<LinkTarget, String> {
    match bitsize {
        8 => Ok(LinkTarget::ExclusiveWriteMemory8),
        16 => Ok(LinkTarget::ExclusiveWriteMemory16),
        32 => Ok(LinkTarget::ExclusiveWriteMemory32),
        64 => Ok(LinkTarget::ExclusiveWriteMemory64),
        128 => Ok(LinkTarget::ExclusiveWriteMemory128),
        _ => Err(format!(
            "Invalid ARM64 exclusive-write-memory bitsize: {bitsize}"
        )),
    }
}

pub fn emit_read_memory<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    ensure_memory_bitsize(BITSIZE)?;
    if should_fastmem(ctx) {
        return fastmem_emit_read_memory::<BITSIZE>(code, ctx, inst_ref);
    }
    if ctx.conf.page_table_pointer != 0 {
        inline_page_table_emit_read_memory::<BITSIZE>(code, ctx, inst_ref)
    } else {
        callback_only_emit_read_memory::<BITSIZE>(code, ctx, inst_ref)
    }
}

pub fn emit_exclusive_read_memory<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    ensure_memory_bitsize(BITSIZE)?;
    callback_only_emit_exclusive_read_memory::<BITSIZE>(code, ctx, inst_ref)
}

pub fn emit_write_memory<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    ensure_memory_bitsize(BITSIZE)?;
    if should_fastmem(ctx) {
        return fastmem_emit_write_memory::<BITSIZE>(code, ctx, inst_ref);
    }
    if ctx.conf.page_table_pointer != 0 {
        inline_page_table_emit_write_memory::<BITSIZE>(code, ctx, inst_ref)
    } else {
        callback_only_emit_write_memory::<BITSIZE>(code, ctx, inst_ref)
    }
}

pub fn emit_exclusive_write_memory<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    ensure_memory_bitsize(BITSIZE)?;
    callback_only_emit_exclusive_write_memory::<BITSIZE>(code, ctx, inst_ref)
}

fn should_fastmem(ctx: &EmitContext<'_>) -> bool {
    // Upstream `ShouldFastmem` requires a registered exception handler.
    // Without one there is no fault-to-callback path for protected pages, so
    // direct fastmem would bypass rasterizer-cache write notifications.
    ctx.conf.fastmem_pointer != 0
        && ctx.fastmem.supports_fastmem()
        && ctx.conf.fastmem_address_space_bits == 32
        && ctx.conf.silently_mirror_fastmem
}

fn ensure_memory_bitsize(bitsize: usize) -> Result<(), String> {
    match bitsize {
        8 | 16 | 32 | 64 | 128 => Ok(()),
        _ => Err(format!("Invalid ARM64 memory bitsize: {bitsize}")),
    }
}

fn callback_only_emit_read_memory<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    ctx.reg_alloc
        .prepare_for_call(code, ctx.fpsr, [None, Some(args[1]), None, None])?;
    let ordered = is_ordered(args[2].get_immediate_acc_type());

    emit_relocation(
        code,
        ctx.emitted_block_info,
        read_memory_link_target(BITSIZE)?,
    )?;
    if ordered {
        code.dmb(BarrierOp::ISH)?;
    }

    define_read_result::<BITSIZE>(code, ctx, inst_ref)?;
    Ok(())
}

fn callback_only_emit_exclusive_read_memory<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    ctx.reg_alloc
        .prepare_for_call(code, ctx.fpsr, [None, Some(args[1]), None, None])?;
    let ordered = is_ordered(args[2].get_immediate_acc_type());

    code.movz(WReg::new(XSCRATCH0), 1, 0)?;
    code.strb(
        WReg::new(XSCRATCH0),
        XReg::new(XSTATE),
        ctx.conf.state_exclusive_state_offset as u32,
    )?;
    emit_relocation(
        code,
        ctx.emitted_block_info,
        exclusive_read_memory_link_target(BITSIZE)?,
    )?;
    if ordered {
        code.dmb(BarrierOp::ISH)?;
    }

    define_read_result::<BITSIZE>(code, ctx, inst_ref)?;
    Ok(())
}

fn define_read_result<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    if BITSIZE == 128 {
        code.mov_v(VReg::new(Q8).b16(), VReg::new(Q0).b16())?;
        ctx.reg_alloc.define_as_register(
            ctx.block,
            inst_ref,
            HostLoc {
                kind: HostLocKind::Fpr,
                index: Q8 as usize,
            },
        );
    } else {
        ctx.reg_alloc.define_as_register(
            ctx.block,
            inst_ref,
            HostLoc {
                kind: HostLocKind::Gpr,
                index: X0 as usize,
            },
        );
    }
    Ok(())
}

fn callback_only_emit_write_memory<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    ctx.reg_alloc
        .prepare_for_call(code, ctx.fpsr, [None, Some(args[1]), Some(args[2]), None])?;
    let ordered = is_ordered(args[3].get_immediate_acc_type());

    if ordered {
        code.dmb(BarrierOp::ISH)?;
    }
    emit_relocation(
        code,
        ctx.emitted_block_info,
        write_memory_link_target(BITSIZE)?,
    )?;
    if ordered {
        code.dmb(BarrierOp::ISH)?;
    }
    Ok(())
}

fn callback_only_emit_exclusive_write_memory<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    ctx.reg_alloc
        .prepare_for_call(code, ctx.fpsr, [None, Some(args[1]), Some(args[2]), None])?;
    let ordered = is_ordered(args[3].get_immediate_acc_type());

    if ordered {
        code.dmb(BarrierOp::ISH)?;
    }
    code.movz(WReg::new(X0), 1, 0)?;
    code.ldrb(
        WReg::new(XSCRATCH0),
        XReg::new(XSTATE),
        ctx.conf.state_exclusive_state_offset as u32,
    )?;
    let mut end = Label::new();
    code.cbz(WReg::new(XSCRATCH0), &mut end)?;
    code.strb(
        WReg::new(WZR),
        XReg::new(XSTATE),
        ctx.conf.state_exclusive_state_offset as u32,
    )?;
    emit_relocation(
        code,
        ctx.emitted_block_info,
        exclusive_write_memory_link_target(BITSIZE)?,
    )?;
    if ordered {
        code.dmb(BarrierOp::ISH)?;
    }
    code.l(&mut end)?;

    ctx.reg_alloc.define_as_register(
        ctx.block,
        inst_ref,
        HostLoc {
            kind: HostLocKind::Gpr,
            index: X0 as usize,
        },
    );
    Ok(())
}

fn fastmem_emit_read_memory<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut xaddr = ctx.reg_alloc.read_x(args[1]);
    let mut rvalue = if BITSIZE == 128 {
        ctx.reg_alloc.write_q(inst_ref)
    } else if BITSIZE == 64 {
        ctx.reg_alloc.write_x(inst_ref)
    } else {
        ctx.reg_alloc.write_w(inst_ref)
    };
    let ordered = is_ordered(args[2].get_immediate_acc_type());

    ctx.fpsr.spill(code)?;
    ctx.reg_alloc.spill_flags(code)?;
    RegAlloc::realize_all(code, ctx.block, &mut [&mut xaddr, &mut rvalue])?;
    let xaddr_reg = xaddr.index().expect("Xaddr must be realized") as u8;
    let rvalue_reg = rvalue.index().expect("Rvalue must be realized") as u8;

    emit_memory_ldr::<BITSIZE>(code, rvalue_reg, XFASTMEM, xaddr_reg, ordered, true)
}

fn fastmem_emit_write_memory<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut xaddr = ctx.reg_alloc.read_x(args[1]);
    let mut rvalue = if BITSIZE == 128 {
        ctx.reg_alloc.read_q(args[2])
    } else if BITSIZE == 64 {
        ctx.reg_alloc.read_x(args[2])
    } else {
        ctx.reg_alloc.read_w(args[2])
    };
    let ordered = is_ordered(args[3].get_immediate_acc_type());

    ctx.fpsr.spill(code)?;
    ctx.reg_alloc.spill_flags(code)?;
    RegAlloc::realize_all(code, ctx.block, &mut [&mut xaddr, &mut rvalue])?;
    let xaddr_reg = xaddr.index().expect("Xaddr must be realized") as u8;
    let rvalue_reg = rvalue.index().expect("Rvalue must be realized") as u8;

    emit_memory_str::<BITSIZE>(code, rvalue_reg, XFASTMEM, xaddr_reg, ordered, true)
}

fn inline_page_table_emit_read_memory<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut xaddr = ctx.reg_alloc.read_x(args[1]);
    let mut rvalue = if BITSIZE == 128 {
        ctx.reg_alloc.write_q(inst_ref)
    } else if BITSIZE == 64 {
        ctx.reg_alloc.write_x(inst_ref)
    } else {
        ctx.reg_alloc.write_w(inst_ref)
    };
    let ordered = is_ordered(args[2].get_immediate_acc_type());

    ctx.fpsr.spill(code)?;
    ctx.reg_alloc.spill_flags(code)?;
    RegAlloc::realize_all(code, ctx.block, &mut [&mut xaddr, &mut rvalue])?;
    let xaddr_reg = xaddr.index().expect("Xaddr must be realized") as u8;
    let rvalue_reg = rvalue.index().expect("Rvalue must be realized") as u8;

    let mut fallback = Label::new();
    let mut end = Label::new();
    let (xbase, xoffset) =
        inline_page_table_emit_vaddr_lookup::<BITSIZE>(code, ctx, xaddr_reg, &mut fallback)?;
    emit_memory_ldr::<BITSIZE>(code, rvalue_reg, xbase, xoffset, ordered, false)?;

    code.l(&mut end)?;
    let current_location = ctx.block.location;
    ctx.deferred_emits.push(Box::new(move |code, ctx| {
        code.l(&mut fallback)?;
        code.mov(XReg::new(XSCRATCH0), XReg::new(xaddr_reg))?;
        emit_relocation(
            code,
            ctx.emitted_block_info,
            wrapped_read_memory_link_target(BITSIZE)?,
        )?;
        if ordered {
            code.dmb(BarrierOp::ISH)?;
        }
        if BITSIZE == 128 {
            code.mov_v(VReg::new(rvalue_reg).b16(), VReg::new(Q0).b16())?;
        } else {
            code.mov(XReg::new(rvalue_reg), XReg::new(XSCRATCH0))?;
        }
        (ctx.conf.emit_check_memory_abort)(code, ctx, current_location, &mut end)?;
        code.b(&mut end)?;
        Ok(())
    }));

    Ok(())
}

fn inline_page_table_emit_write_memory<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut xaddr = ctx.reg_alloc.read_x(args[1]);
    let mut rvalue = if BITSIZE == 128 {
        ctx.reg_alloc.read_q(args[2])
    } else if BITSIZE == 64 {
        ctx.reg_alloc.read_x(args[2])
    } else {
        ctx.reg_alloc.read_w(args[2])
    };
    let ordered = is_ordered(args[3].get_immediate_acc_type());

    ctx.fpsr.spill(code)?;
    ctx.reg_alloc.spill_flags(code)?;
    RegAlloc::realize_all(code, ctx.block, &mut [&mut xaddr, &mut rvalue])?;
    let xaddr_reg = xaddr.index().expect("Xaddr must be realized") as u8;
    let rvalue_reg = rvalue.index().expect("Rvalue must be realized") as u8;

    let mut fallback = Label::new();
    let mut end = Label::new();
    emit_inline_watch_fallback::<BITSIZE>(code, xaddr_reg, &mut fallback)?;
    let (xbase, xoffset) =
        inline_page_table_emit_vaddr_lookup::<BITSIZE>(code, ctx, xaddr_reg, &mut fallback)?;
    emit_memory_str::<BITSIZE>(code, rvalue_reg, xbase, xoffset, ordered, false)?;

    code.l(&mut end)?;
    let current_location = ctx.block.location;
    ctx.deferred_emits.push(Box::new(move |code, ctx| {
        code.l(&mut fallback)?;
        code.mov(XReg::new(XSCRATCH0), XReg::new(xaddr_reg))?;
        if BITSIZE == 128 {
            code.mov_v(VReg::new(Q0).b16(), VReg::new(rvalue_reg).b16())?;
        } else {
            code.mov(XReg::new(XSCRATCH1), XReg::new(rvalue_reg))?;
        }
        if ordered {
            code.dmb(BarrierOp::ISH)?;
        }
        emit_relocation(
            code,
            ctx.emitted_block_info,
            wrapped_write_memory_link_target(BITSIZE)?,
        )?;
        if ordered {
            code.dmb(BarrierOp::ISH)?;
        }
        (ctx.conf.emit_check_memory_abort)(code, ctx, current_location, &mut end)?;
        code.b(&mut end)?;
        Ok(())
    }));

    Ok(())
}

fn emit_inline_watch_fallback<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    xaddr: u8,
    fallback: &mut Label,
) -> Result<(), String> {
    let ranges = inline_watch_ranges();
    if ranges.is_empty() {
        return Ok(());
    }

    let access_size = (BITSIZE / 8) as u64;
    for &(start, end) in ranges {
        if start >= end {
            continue;
        }
        let overlap_start = start.saturating_sub(access_size.saturating_sub(1));
        let mut next_range = Label::new();

        emit_mov_x_imm_local(code, XSCRATCH0, overlap_start)?;
        code.cmp(XReg::new(xaddr), XReg::new(XSCRATCH0))?;
        code.b_cond(Cond::LO, &mut next_range)?;

        emit_mov_x_imm_local(code, XSCRATCH0, end)?;
        code.cmp(XReg::new(xaddr), XReg::new(XSCRATCH0))?;
        code.b_cond(Cond::LO, fallback)?;

        code.l(&mut next_range)?;
    }
    Ok(())
}

fn emit_mov_x_imm_local(code: &mut CodeGenerator<'_>, reg: u8, imm: u64) -> Result<(), String> {
    code.movz(XReg::new(reg), (imm & 0xffff) as u16, 0)?;
    for shift in [16, 32, 48] {
        let part = ((imm >> shift) & 0xffff) as u16;
        if part != 0 {
            code.movk(XReg::new(reg), part, shift as u8)?;
        }
    }
    Ok(())
}

fn inline_page_table_emit_vaddr_lookup<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    xaddr: u8,
    fallback: &mut Label,
) -> Result<(u8, u8), String> {
    let valid_page_index_bits = ctx
        .conf
        .page_table_address_space_bits
        .checked_sub(PAGE_BITS)
        .ok_or_else(|| "ARM64 page-table address space is smaller than a page".to_string())?;
    let unused_top_bits = 64usize.saturating_sub(ctx.conf.page_table_address_space_bits);

    emit_detect_misaligned_vaddr::<BITSIZE>(code, ctx, xaddr, fallback)?;

    if ctx.conf.silently_mirror_page_table || unused_top_bits == 0 {
        code.ubfx(
            XReg::new(XSCRATCH0),
            XReg::new(xaddr),
            PAGE_BITS as u8,
            valid_page_index_bits as u8,
        )?;
    } else {
        code.lsr(XReg::new(XSCRATCH0), XReg::new(xaddr), PAGE_BITS as u8)?;
        code.tst_imm(XReg::new(XSCRATCH0), u64::MAX << valid_page_index_bits)?;
        code.b_cond(Cond::NE, fallback)?;
    }

    code.lsl(
        XReg::new(XSCRATCH0),
        XReg::new(XSCRATCH0),
        ctx.conf.page_table_log2_stride as u8,
    )?;
    code.ldr_reg(
        XReg::new(XSCRATCH0),
        XReg::new(XPAGETABLE),
        XReg::new(XSCRATCH0),
    )?;

    if let Some(marked_bit) = ctx.conf.page_table_marked_bit {
        // check for marked bit
        code.tbnz(XReg::new(XSCRATCH0), marked_bit, fallback)?;
    }

    if ctx.conf.page_table_pointer_mask != 0 {
        code.and_imm(
            XReg::new(XSCRATCH0),
            XReg::new(XSCRATCH0),
            ctx.conf.page_table_pointer_mask,
        )?;
    }

    // TODO: combine this with page_table_pointer_mask
    if let Some(sign_extension) = ctx.conf.page_table_sign_extension {
        code.sbfm(
            XReg::new(XSCRATCH0),
            XReg::new(XSCRATCH0),
            0,
            sign_extension,
        )?;
    }

    code.cbz(XReg::new(XSCRATCH0), fallback)?;

    if ctx.conf.absolute_offset_page_table {
        Ok((XSCRATCH0, xaddr))
    } else {
        code.and_imm(XReg::new(XSCRATCH1), XReg::new(xaddr), PAGE_MASK)?;
        Ok((XSCRATCH0, XSCRATCH1))
    }
}

fn emit_detect_misaligned_vaddr<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &EmitContext<'_>,
    xaddr: u8,
    fallback: &mut Label,
) -> Result<(), String> {
    if BITSIZE == 8 || (ctx.conf.detect_misaligned_access_via_page_table & BITSIZE as u32) == 0 {
        return Ok(());
    }

    if !ctx
        .conf
        .only_detect_misalignment_via_page_table_on_page_boundary
    {
        let align_mask = match BITSIZE {
            16 => 0b1,
            32 => 0b11,
            64 => 0b111,
            128 => 0b1111,
            _ => return Err(format!("Invalid ARM64 memory bitsize: {BITSIZE}")),
        };
        code.tst_imm(XReg::new(xaddr), align_mask)?;
        code.b_cond(Cond::NE, fallback)?;
    } else {
        code.and_imm(XReg::new(XSCRATCH0), XReg::new(xaddr), PAGE_MASK)?;
        code.cmp_imm(XReg::new(XSCRATCH0), (PAGE_SIZE - BITSIZE / 8) as u32)?;
        code.b_cond(Cond::HI, fallback)?;
    }
    Ok(())
}

fn emit_memory_ldr<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    value_idx: u8,
    xbase: u8,
    xoffset: u8,
    ordered: bool,
    extend32: bool,
) -> Result<(), String> {
    if ordered {
        emit_add_address(code, XSCRATCH0, xbase, xoffset, extend32)?;
        match BITSIZE {
            8 => {
                code.ldarb(WReg::new(value_idx), XReg::new(XSCRATCH0))?;
            }
            16 => {
                code.ldarh(WReg::new(value_idx), XReg::new(XSCRATCH0))?;
            }
            32 => {
                code.ldar(WReg::new(value_idx), XReg::new(XSCRATCH0))?;
            }
            64 => {
                code.ldar(XReg::new(value_idx), XReg::new(XSCRATCH0))?;
            }
            128 => {
                code.ldr(QReg::new(value_idx), XReg::new(XSCRATCH0), 0)?;
                code.dmb(BarrierOp::ISH)?;
            }
            _ => return Err(format!("Invalid ARM64 memory bitsize: {BITSIZE}")),
        }
    } else {
        match (BITSIZE, extend32) {
            (8, false) => {
                code.ldrb_reg(WReg::new(value_idx), XReg::new(xbase), XReg::new(xoffset))?;
            }
            (16, false) => {
                code.ldrh_reg(WReg::new(value_idx), XReg::new(xbase), XReg::new(xoffset))?;
            }
            (32, false) => {
                code.ldr_reg(WReg::new(value_idx), XReg::new(xbase), XReg::new(xoffset))?;
            }
            (64, false) => {
                code.ldr_reg(XReg::new(value_idx), XReg::new(xbase), XReg::new(xoffset))?;
            }
            (128, false) => {
                code.ldr_q_reg(QReg::new(value_idx), XReg::new(xbase), XReg::new(xoffset))?;
            }
            (8, true) => {
                code.ldrb_reg(WReg::new(value_idx), XReg::new(xbase), WReg::new(xoffset))?;
            }
            (16, true) => {
                code.ldrh_reg(WReg::new(value_idx), XReg::new(xbase), WReg::new(xoffset))?;
            }
            (32, true) => {
                code.ldr_reg(WReg::new(value_idx), XReg::new(xbase), WReg::new(xoffset))?;
            }
            (64, true) => {
                code.ldr_reg(XReg::new(value_idx), XReg::new(xbase), WReg::new(xoffset))?;
            }
            (128, true) => {
                code.ldr_q_reg(QReg::new(value_idx), XReg::new(xbase), WReg::new(xoffset))?;
            }
            _ => return Err(format!("Invalid ARM64 memory bitsize: {BITSIZE}")),
        }
    }
    Ok(())
}

fn emit_memory_str<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    value_idx: u8,
    xbase: u8,
    xoffset: u8,
    ordered: bool,
    extend32: bool,
) -> Result<(), String> {
    if ordered {
        emit_add_address(code, XSCRATCH0, xbase, xoffset, extend32)?;
        match BITSIZE {
            8 => {
                code.stlrb(WReg::new(value_idx), XReg::new(XSCRATCH0))?;
            }
            16 => {
                code.stlrh(WReg::new(value_idx), XReg::new(XSCRATCH0))?;
            }
            32 => {
                code.stlr(WReg::new(value_idx), XReg::new(XSCRATCH0))?;
            }
            64 => {
                code.stlr(XReg::new(value_idx), XReg::new(XSCRATCH0))?;
            }
            128 => {
                code.dmb(BarrierOp::ISH)?;
                code.str(QReg::new(value_idx), XReg::new(XSCRATCH0), 0)?;
                code.dmb(BarrierOp::ISH)?;
            }
            _ => return Err(format!("Invalid ARM64 memory bitsize: {BITSIZE}")),
        }
    } else {
        match (BITSIZE, extend32) {
            (8, false) => {
                code.strb_reg(WReg::new(value_idx), XReg::new(xbase), XReg::new(xoffset))?;
            }
            (16, false) => {
                code.strh_reg(WReg::new(value_idx), XReg::new(xbase), XReg::new(xoffset))?;
            }
            (32, false) => {
                code.str_reg(WReg::new(value_idx), XReg::new(xbase), XReg::new(xoffset))?;
            }
            (64, false) => {
                code.str_reg(XReg::new(value_idx), XReg::new(xbase), XReg::new(xoffset))?;
            }
            (128, false) => {
                code.str_q_reg(QReg::new(value_idx), XReg::new(xbase), XReg::new(xoffset))?;
            }
            (8, true) => {
                code.strb_reg(WReg::new(value_idx), XReg::new(xbase), WReg::new(xoffset))?;
            }
            (16, true) => {
                code.strh_reg(WReg::new(value_idx), XReg::new(xbase), WReg::new(xoffset))?;
            }
            (32, true) => {
                code.str_reg(WReg::new(value_idx), XReg::new(xbase), WReg::new(xoffset))?;
            }
            (64, true) => {
                code.str_reg(XReg::new(value_idx), XReg::new(xbase), WReg::new(xoffset))?;
            }
            (128, true) => {
                code.str_q_reg(QReg::new(value_idx), XReg::new(xbase), WReg::new(xoffset))?;
            }
            _ => return Err(format!("Invalid ARM64 memory bitsize: {BITSIZE}")),
        }
    }
    Ok(())
}

fn emit_add_address(
    code: &mut CodeGenerator<'_>,
    rd: u8,
    xbase: u8,
    xoffset: u8,
    extend32: bool,
) -> Result<(), String> {
    if extend32 {
        code.add_uxtw(XReg::new(rd), XReg::new(xbase), WReg::new(xoffset))?;
    } else {
        code.add(XReg::new(rd), XReg::new(xbase), XReg::new(xoffset))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::arm64::inst;

    #[test]
    fn typed_memory_accesses_preserve_width_extension_and_barrier_order() {
        fn check<const BITS: usize>() {
            for extend32 in [false, true] {
                for ordered in [false, true] {
                    let mut code_storage = BlockOfCode::with_size(4096).unwrap();
                    let mut code = rhazel::CodeGenerator::new(&mut code_storage);
                    emit_memory_ldr::<BITS>(&mut code, 7, 8, 9, ordered, extend32).unwrap();
                    emit_memory_str::<BITS>(&mut code, 7, 8, 9, ordered, extend32).unwrap();

                    let mut expected = Vec::new();
                    if ordered {
                        let address = if extend32 {
                            inst::add_x_reg_uxtw(XSCRATCH0, 8, 9)
                        } else {
                            inst::add_x_reg(XSCRATCH0, 8, 9)
                        };
                        expected.push(address);
                        expected.push(match BITS {
                            8 => inst::ldarb_w(7, XSCRATCH0),
                            16 => inst::ldarh_w(7, XSCRATCH0),
                            32 => inst::ldar_w(7, XSCRATCH0),
                            64 => inst::ldar_x(7, XSCRATCH0),
                            128 => inst::ldr_q_unsigned(7, XSCRATCH0, 0),
                            _ => unreachable!(),
                        });
                        if BITS == 128 {
                            expected.push(inst::dmb_ish());
                        }
                        expected.push(address);
                        if BITS == 128 {
                            expected.push(inst::dmb_ish());
                        }
                        expected.push(match BITS {
                            8 => inst::stlrb_w(7, XSCRATCH0),
                            16 => inst::stlrh_w(7, XSCRATCH0),
                            32 => inst::stlr_w(7, XSCRATCH0),
                            64 => inst::stlr_x(7, XSCRATCH0),
                            128 => inst::str_q_unsigned(7, XSCRATCH0, 0),
                            _ => unreachable!(),
                        });
                        if BITS == 128 {
                            expected.push(inst::dmb_ish());
                        }
                    } else {
                        let (load, store): (fn(u8, u8, u8) -> u32, fn(u8, u8, u8) -> u32) =
                            match (BITS, extend32) {
                                (8, false) => (inst::ldrb_w_reg_lsl, inst::strb_w_reg_lsl),
                                (16, false) => (inst::ldrh_w_reg_lsl, inst::strh_w_reg_lsl),
                                (32, false) => (inst::ldr_w_reg_lsl, inst::str_w_reg_lsl),
                                (64, false) => (inst::ldr_x_reg_lsl, inst::str_x_reg_lsl),
                                (128, false) => (inst::ldr_q_reg_lsl, inst::str_q_reg_lsl),
                                (8, true) => (inst::ldrb_w_reg_uxtw, inst::strb_w_reg_uxtw),
                                (16, true) => (inst::ldrh_w_reg_uxtw, inst::strh_w_reg_uxtw),
                                (32, true) => (inst::ldr_w_reg_uxtw, inst::str_w_reg_uxtw),
                                (64, true) => (inst::ldr_x_reg_uxtw, inst::str_x_reg_uxtw),
                                (128, true) => (inst::ldr_q_reg_uxtw, inst::str_q_reg_uxtw),
                                _ => unreachable!(),
                            };
                        expected.extend([load(7, 8, 9), store(7, 8, 9)]);
                    }
                    let actual: Vec<_> = (0..code.code_size())
                        .step_by(4)
                        .map(|offset| read_instruction(&code, offset))
                        .collect();
                    assert_eq!(
                        actual, expected,
                        "bits={BITS}, ordered={ordered}, extend32={extend32}"
                    );
                }
            }
        }
        check::<8>();
        check::<16>();
        check::<32>();
        check::<64>();
        check::<128>();
    }

    use crate::backend::arm64::emit_arm64::{EmitConfig, EmittedBlockInfo, Relocation};
    use crate::backend::arm64::fastmem::FastmemManager;
    use crate::backend::arm64::fpsr_manager::FpsrManager;
    use crate::backend::arm64::jit_state::A64JitState;
    use crate::backend::arm64::reg_alloc::RegAlloc;
    use crate::backend::arm64::stack_layout::StackLayout;
    use crate::interface::a64::config::{
        Exception as A64Exception, UserCallbacks as A64UserCallbacks, UserConfig as A64UserConfig,
        Vector as A64Vector,
    };
    use crate::interface::optimization_flags::OptimizationFlag;
    use crate::ir::block::Block;
    use crate::ir::inst::Inst;
    use crate::ir::location::{A64LocationDescriptor, LocationDescriptor};
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;
    use crate::ir::value::Value;

    struct DummyCallbacks;

    impl A64UserCallbacks for DummyCallbacks {
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

        fn memory_read_128(&self, _vaddr: u64) -> A64Vector {
            [0, 0]
        }

        fn memory_write_8(&mut self, _vaddr: u64, _value: u8) {}
        fn memory_write_16(&mut self, _vaddr: u64, _value: u16) {}
        fn memory_write_32(&mut self, _vaddr: u64, _value: u32) {}
        fn memory_write_64(&mut self, _vaddr: u64, _value: u64) {}
        fn memory_write_128(&mut self, _vaddr: u64, _value: A64Vector) {}

        fn call_svc(&mut self, _svc_num: u32) {}
        fn exception_raised(&mut self, _pc: u64, _exception: A64Exception) {}
        fn add_ticks(&mut self, _ticks: u64) {}

        fn get_ticks_remaining(&self) -> u64 {
            0
        }

        fn get_cntpct(&self) -> u64 {
            0
        }
    }

    fn config() -> EmitConfig {
        let mut config = A64UserConfig::new(Box::new(DummyCallbacks));
        config.enable_cycle_counting = false;
        config.code_cache_size = 0;
        config.optimizations = OptimizationFlag::NO_OPTIMIZATIONS;
        config.check_halt_on_memory_access = true;
        EmitConfig::from_a64_config(&config)
    }

    fn block_with_inst(opcode: Opcode, args: &[Value]) -> Block {
        let location = A64LocationDescriptor::new(0x4000, 0, false).to_location();
        let mut block = Block::new(location);
        block.push_inst(Inst::new(opcode, args));
        block.terminal = Terminal::ReturnToDispatch;
        block
    }

    fn context_emit(
        block: &mut Block,
        code: &mut CodeGenerator<'_>,
        emitted_block_info: &mut EmittedBlockInfo,
        config: &EmitConfig,
        emit: impl FnOnce(&mut CodeGenerator<'_>, &mut EmitContext<'_>, InstRef) -> Result<(), String>,
    ) -> Result<RegAlloc, String> {
        context_emit_with_setup(block, code, emitted_block_info, config, |_, _| Ok(()), emit)
    }

    fn context_emit_with_setup(
        block: &mut Block,
        code: &mut CodeGenerator<'_>,
        emitted_block_info: &mut EmittedBlockInfo,
        config: &EmitConfig,
        setup: impl FnOnce(&Block, &mut RegAlloc) -> Result<(), String>,
        emit: impl FnOnce(&mut CodeGenerator<'_>, &mut EmitContext<'_>, InstRef) -> Result<(), String>,
    ) -> Result<RegAlloc, String> {
        let mut reg_alloc = RegAlloc::default();
        setup(block, &mut reg_alloc)?;
        let mut fpsr = FpsrManager::new(config.state_fpsr_offset);
        let mut fastmem = FastmemManager::default();
        {
            let mut ctx = EmitContext {
                block,
                reg_alloc: &mut reg_alloc,
                conf: config,
                emitted_block_info,
                fpsr: &mut fpsr,
                fastmem: &mut fastmem,
                deferred_emits: Vec::new(),
            };
            emit(code, &mut ctx, InstRef(0))?;
        }
        Ok(reg_alloc)
    }

    fn context_emit_with_deferred(
        block: &mut Block,
        code: &mut CodeGenerator<'_>,
        emitted_block_info: &mut EmittedBlockInfo,
        config: &EmitConfig,
        emit: impl FnOnce(&mut CodeGenerator<'_>, &mut EmitContext<'_>, InstRef) -> Result<(), String>,
    ) -> Result<RegAlloc, String> {
        let mut reg_alloc = RegAlloc::default();
        let mut fpsr = FpsrManager::new(config.state_fpsr_offset);
        let mut fastmem = FastmemManager::default();
        {
            let mut ctx = EmitContext {
                block,
                reg_alloc: &mut reg_alloc,
                conf: config,
                emitted_block_info,
                fpsr: &mut fpsr,
                fastmem: &mut fastmem,
                deferred_emits: Vec::new(),
            };
            emit(code, &mut ctx, InstRef(0))?;

            // Full block emission writes the terminal before deferred fallbacks.
            // Keep that separation here so fallback branches target the direct
            // path's bound `end` label, not the fallback body itself.
            code.write_u32(inst::brk(0))?;
            let mut deferred_emits = std::mem::take(&mut ctx.deferred_emits);
            for deferred_emit in &mut deferred_emits {
                deferred_emit(code, &mut ctx)?;
            }
        }
        Ok(reg_alloc)
    }

    fn context_emit_inst(
        block: &mut Block,
        code: &mut CodeGenerator<'_>,
        emitted_block_info: &mut EmittedBlockInfo,
        config: &EmitConfig,
        inst_ref: InstRef,
        setup: impl FnOnce(&Block, &mut RegAlloc) -> Result<(), String>,
        emit: impl FnOnce(&mut CodeGenerator<'_>, &mut EmitContext<'_>, InstRef) -> Result<(), String>,
    ) -> Result<RegAlloc, String> {
        let mut reg_alloc = RegAlloc::default();
        setup(block, &mut reg_alloc)?;
        let mut fpsr = FpsrManager::new(config.state_fpsr_offset);
        let mut fastmem = FastmemManager::default();
        {
            let mut ctx = EmitContext {
                block,
                reg_alloc: &mut reg_alloc,
                conf: config,
                emitted_block_info,
                fpsr: &mut fpsr,
                fastmem: &mut fastmem,
                deferred_emits: Vec::new(),
            };
            emit(code, &mut ctx, inst_ref)?;
        }
        Ok(reg_alloc)
    }

    fn empty_block_info(code: &BlockOfCode) -> EmittedBlockInfo {
        EmittedBlockInfo {
            entry_point: code.code_base_ptr(),
            size: 0,
            relocations: Vec::new(),
            block_relocations: crate::backend::arm64::fast_hash::FastHashMap::default(),
            fastmem_patch_info: crate::backend::arm64::fast_hash::FastHashMap::default(),
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

    #[test]
    fn page_table_read_memory32_emits_lookup_direct_load_and_wrapped_fallback() {
        let mut config = config();
        config.check_halt_on_memory_access = false;
        config.page_table_pointer = 0x1000_0000;
        config.page_table_address_space_bits = 32;
        config.page_table_log2_stride = 4;
        config.silently_mirror_page_table = true;
        config.absolute_offset_page_table = false;

        let mut code_storage = BlockOfCode::with_size(4096).unwrap();
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let mut info = empty_block_info(&code);
        let mut block = block_with_inst(
            Opcode::A64ReadMemory32,
            &[
                Value::ImmU64(LocationDescriptor::new(0x4000).value()),
                Value::ImmU64(0x1234),
                Value::ImmAccType(AccType::Normal),
            ],
        );

        context_emit_with_deferred(
            &mut block,
            &mut code,
            &mut info,
            &config,
            |code, ctx, inst| emit_read_memory::<32>(code, ctx, inst),
        )
        .unwrap();

        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 36,
                target: LinkTarget::WrappedReadMemory32,
            }]
        );
        assert_eq!(
            read_instruction(&code, 0),
            inst::movz_x(test_gpr(0), 0x1234, 0)
        );
        assert_eq!(
            read_instruction(&code, 4),
            inst::ubfx_x(XSCRATCH0, test_gpr(0), 12, 20)
        );
        assert_eq!(
            read_instruction(&code, 8),
            inst::lsl_x_imm(XSCRATCH0, XSCRATCH0, 4)
        );
        assert_eq!(
            read_instruction(&code, 12),
            inst::ldr_x_reg_lsl(XSCRATCH0, XPAGETABLE, XSCRATCH0)
        );
        assert_eq!(read_instruction(&code, 16), inst::cbz_x(XSCRATCH0, 16));
        assert_eq!(
            read_instruction(&code, 20),
            inst::and_x_imm(XSCRATCH1, test_gpr(0), PAGE_MASK)
        );
        assert_eq!(
            read_instruction(&code, 24),
            inst::ldr_w_reg_lsl(test_gpr(1), XSCRATCH0, XSCRATCH1)
        );
        assert_eq!(read_instruction(&code, 28), inst::brk(0));
        assert_eq!(
            read_instruction(&code, 32),
            inst::mov_x(XSCRATCH0, test_gpr(0))
        );
        assert_eq!(read_instruction(&code, 36), inst::nop());
        assert_eq!(
            read_instruction(&code, 40),
            inst::mov_x(test_gpr(1), XSCRATCH0)
        );
        assert_eq!(read_instruction(&code, 44), inst::b_imm(-16));
    }

    #[test]
    fn page_table_non_mirror_read_memory32_emits_upstream_range_check() {
        let mut config = config();
        config.check_halt_on_memory_access = false;
        config.page_table_pointer = 0x1000_0000;
        config.page_table_address_space_bits = 39;
        config.silently_mirror_page_table = false;
        config.absolute_offset_page_table = true;
        config.page_table_pointer_mask = u64::MAX << 5;

        let mut code_storage = BlockOfCode::with_size(4096).unwrap();
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let mut info = empty_block_info(&code);
        let mut block = block_with_inst(
            Opcode::A64ReadMemory32,
            &[
                Value::ImmU64(LocationDescriptor::new(0x4000).value()),
                Value::ImmU64(0x1234),
                Value::ImmAccType(AccType::Normal),
            ],
        );

        context_emit_with_deferred(
            &mut block,
            &mut code,
            &mut info,
            &config,
            |code, ctx, inst| emit_read_memory::<32>(code, ctx, inst),
        )
        .unwrap();

        assert_eq!(
            read_instruction(&code, 4),
            inst::lsr_x_imm(XSCRATCH0, test_gpr(0), 12)
        );
        assert_eq!(
            read_instruction(&code, 8),
            inst::tst_x_imm(XSCRATCH0, 0xffff_ffff_f800_0000)
        );
        assert_eq!(read_instruction(&code, 12), inst::b_cond(Cond::NE, 28));
        assert_eq!(
            read_instruction(&code, 16),
            inst::lsl_x_imm(XSCRATCH0, XSCRATCH0, 3)
        );
        assert_eq!(
            read_instruction(&code, 20),
            inst::ldr_x_reg_lsl(XSCRATCH0, XPAGETABLE, XSCRATCH0)
        );
        assert_eq!(
            read_instruction(&code, 24),
            inst::and_x_imm(XSCRATCH0, XSCRATCH0, 0xffff_ffff_ffff_ffe0)
        );
        assert_eq!(read_instruction(&code, 28), inst::cbz_x(XSCRATCH0, 12));
    }

    /// Eden's packed page entries (5f142c7926): the lookup checks the marked
    /// bit (`TBNZ`), masks the attributes with the 64-bit mask and sign
    /// extends (`SBFM`) before the null check.
    #[test]
    fn page_table_marked_bit_mask_and_sign_extension_emit_tbnz_and_sbfm() {
        let mut config = config();
        config.check_halt_on_memory_access = false;
        config.page_table_pointer = 0x1000_0000;
        config.page_table_address_space_bits = 39;
        config.silently_mirror_page_table = false;
        config.absolute_offset_page_table = true;
        config.page_table_pointer_mask = 0x00ff_ffff_ffff_f000;
        config.page_table_marked_bit = Some(0);
        config.page_table_sign_extension = Some(57);

        let mut code_storage = BlockOfCode::with_size(4096).unwrap();
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let mut info = empty_block_info(&code);
        let mut block = block_with_inst(
            Opcode::A64ReadMemory32,
            &[
                Value::ImmU64(LocationDescriptor::new(0x4000).value()),
                Value::ImmU64(0x1234),
                Value::ImmAccType(AccType::Normal),
            ],
        );

        context_emit_with_deferred(
            &mut block,
            &mut code,
            &mut info,
            &config,
            |code, ctx, inst| emit_read_memory::<32>(code, ctx, inst),
        )
        .unwrap();

        assert_eq!(
            read_instruction(&code, 4),
            inst::lsr_x_imm(XSCRATCH0, test_gpr(0), 12)
        );
        assert_eq!(
            read_instruction(&code, 8),
            inst::tst_x_imm(XSCRATCH0, 0xffff_ffff_f800_0000)
        );
        assert_eq!(read_instruction(&code, 12), inst::b_cond(Cond::NE, 40));
        assert_eq!(
            read_instruction(&code, 16),
            inst::lsl_x_imm(XSCRATCH0, XSCRATCH0, 3)
        );
        assert_eq!(
            read_instruction(&code, 20),
            inst::ldr_x_reg_lsl(XSCRATCH0, XPAGETABLE, XSCRATCH0)
        );
        assert_eq!(read_instruction(&code, 24), inst::tbz_x(XSCRATCH0, 0, 8));
        assert_eq!(read_instruction(&code, 28), inst::b_imm(24));
        assert_eq!(
            read_instruction(&code, 32),
            inst::and_x_imm(XSCRATCH0, XSCRATCH0, 0x00ff_ffff_ffff_f000)
        );
        assert_eq!(
            read_instruction(&code, 36),
            inst::sbfm_x(XSCRATCH0, XSCRATCH0, 0, 57)
        );
        assert_eq!(read_instruction(&code, 40), inst::cbz_x(XSCRATCH0, 12));
    }

    #[test]
    fn page_table_write_memory64_emits_lookup_direct_store_and_wrapped_fallback() {
        let mut config = config();
        config.check_halt_on_memory_access = false;
        config.page_table_pointer = 0x1000_0000;
        config.page_table_address_space_bits = 32;
        config.silently_mirror_page_table = true;
        config.absolute_offset_page_table = false;

        let mut code_storage = BlockOfCode::with_size(4096).unwrap();
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let mut info = empty_block_info(&code);
        let mut block = block_with_inst(
            Opcode::A64WriteMemory64,
            &[
                Value::ImmU64(LocationDescriptor::new(0x4000).value()),
                Value::ImmU64(0x2000),
                Value::ImmU64(0xfeed_face),
                Value::ImmAccType(AccType::Normal),
            ],
        );

        context_emit_with_deferred(
            &mut block,
            &mut code,
            &mut info,
            &config,
            |code, ctx, inst| emit_write_memory::<64>(code, ctx, inst),
        )
        .unwrap();

        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 48,
                target: LinkTarget::WrappedWriteMemory64,
            }]
        );
        assert_eq!(
            read_instruction(&code, 0),
            inst::movz_x(test_gpr(0), 0x2000, 0)
        );
        assert_eq!(
            read_instruction(&code, 4),
            inst::movz_x(test_gpr(1), 0xface, 0)
        );
        assert_eq!(
            read_instruction(&code, 8),
            inst::movk_x(test_gpr(1), 0xfeed, 16)
        );
        assert_eq!(
            read_instruction(&code, 12),
            inst::ubfx_x(XSCRATCH0, test_gpr(0), 12, 20)
        );
        assert_eq!(
            read_instruction(&code, 16),
            inst::lsl_x_imm(XSCRATCH0, XSCRATCH0, 3)
        );
        assert_eq!(
            read_instruction(&code, 20),
            inst::ldr_x_reg_lsl(XSCRATCH0, XPAGETABLE, XSCRATCH0)
        );
        assert_eq!(read_instruction(&code, 24), inst::cbz_x(XSCRATCH0, 16));
        assert_eq!(
            read_instruction(&code, 28),
            inst::and_x_imm(XSCRATCH1, test_gpr(0), PAGE_MASK)
        );
        assert_eq!(
            read_instruction(&code, 32),
            inst::str_x_reg_lsl(test_gpr(1), XSCRATCH0, XSCRATCH1)
        );
        assert_eq!(read_instruction(&code, 36), inst::brk(0));
        assert_eq!(
            read_instruction(&code, 40),
            inst::mov_x(XSCRATCH0, test_gpr(0))
        );
        assert_eq!(
            read_instruction(&code, 44),
            inst::mov_x(XSCRATCH1, test_gpr(1))
        );
        assert_eq!(read_instruction(&code, 48), inst::nop());
        assert_eq!(read_instruction(&code, 52), inst::b_imm(-16));
    }

    #[test]
    fn callback_only_read_memory_records_relocation_and_return_register() {
        let config = config();
        let mut code_storage = BlockOfCode::with_size(4096).unwrap();
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let mut info = empty_block_info(&code);
        let mut block = block_with_inst(
            Opcode::A64ReadMemory32,
            &[
                Value::ImmU64(LocationDescriptor::new(0x4000).value()),
                Value::ImmU64(0x1234),
                Value::ImmAccType(AccType::Normal),
            ],
        );

        context_emit(
            &mut block,
            &mut code,
            &mut info,
            &config,
            |code, ctx, inst| emit_read_memory::<32>(code, ctx, inst),
        )
        .unwrap();

        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 4,
                target: LinkTarget::ReadMemory32,
            }]
        );
        assert_eq!(read_instruction(&code, 0), inst::movz_x(1, 0x1234, 0));
        assert_eq!(read_instruction(&code, 4), inst::nop());
        assert_eq!(
            info.relocations[0].code_offset as usize,
            code.code_size() - 4
        );
    }

    #[test]
    fn callback_only_read_memory_128_moves_q0_to_q8_and_defines_fpr() {
        let config = config();
        let mut code_storage = BlockOfCode::with_size(4096).unwrap();
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let mut info = empty_block_info(&code);
        let mut block = block_with_inst(
            Opcode::A64ReadMemory128,
            &[
                Value::ImmU64(LocationDescriptor::new(0x4000).value()),
                Value::ImmU64(0x1234),
                Value::ImmAccType(AccType::Normal),
            ],
        );

        let reg_alloc = context_emit(
            &mut block,
            &mut code,
            &mut info,
            &config,
            |code, ctx, inst| emit_read_memory::<128>(code, ctx, inst),
        )
        .unwrap();

        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 4,
                target: LinkTarget::ReadMemory128,
            }]
        );
        assert_eq!(read_instruction(&code, 0), inst::movz_x(1, 0x1234, 0));
        assert_eq!(read_instruction(&code, 4), inst::nop());
        assert_eq!(read_instruction(&code, 8), inst::mov_v16b(Q8, Q0));
        assert_eq!(
            reg_alloc.value_location(InstRef(0)),
            Some(HostLoc {
                kind: HostLocKind::Fpr,
                index: Q8 as usize,
            })
        );
    }

    #[test]
    fn callback_only_ordered_write_memory_wraps_callback_with_dmb() {
        let config = config();
        let mut code_storage = BlockOfCode::with_size(4096).unwrap();
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let mut info = empty_block_info(&code);
        let mut block = block_with_inst(
            Opcode::A64WriteMemory64,
            &[
                Value::ImmU64(LocationDescriptor::new(0x4000).value()),
                Value::ImmU64(0x2000),
                Value::ImmU64(0xfeed_face),
                Value::ImmAccType(AccType::Ordered),
            ],
        );

        context_emit(
            &mut block,
            &mut code,
            &mut info,
            &config,
            |code, ctx, inst| emit_write_memory::<64>(code, ctx, inst),
        )
        .unwrap();

        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 16,
                target: LinkTarget::WriteMemory64,
            }]
        );
        assert_eq!(read_instruction(&code, 0), inst::movz_x(1, 0x2000, 0));
        assert_eq!(read_instruction(&code, 4), inst::movz_x(2, 0xface, 0));
        assert_eq!(read_instruction(&code, 8), inst::movk_x(2, 0xfeed, 16));
        assert_eq!(read_instruction(&code, 12), inst::dmb_ish());
        assert_eq!(read_instruction(&code, 16), inst::nop());
        assert_eq!(read_instruction(&code, 20), inst::dmb_ish());
    }

    #[test]
    fn callback_only_write_memory_128_passes_value_in_q0() {
        let config = config();
        let mut code_storage = BlockOfCode::with_size(4096).unwrap();
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let mut info = empty_block_info(&code);
        let location = LocationDescriptor::new(0x4000).value();
        let mut block = Block::new(A64LocationDescriptor::new(0x4000, 0, false).to_location());
        let value = block.append(
            Opcode::A64ReadMemory128,
            &[
                Value::ImmU64(location),
                Value::ImmU64(0x1000),
                Value::ImmAccType(AccType::Normal),
            ],
        );
        let write = block.append(
            Opcode::A64WriteMemory128,
            &[
                Value::ImmU64(location),
                Value::ImmU64(0x2000),
                Value::Inst(value),
                Value::ImmAccType(AccType::Normal),
            ],
        );
        block.terminal = Terminal::ReturnToDispatch;

        context_emit_inst(
            &mut block,
            &mut code,
            &mut info,
            &config,
            write,
            |block, reg_alloc| {
                reg_alloc.define_as_register(
                    block,
                    value,
                    HostLoc {
                        kind: HostLocKind::Fpr,
                        index: 9,
                    },
                );
                Ok(())
            },
            |code, ctx, inst| emit_write_memory::<128>(code, ctx, inst),
        )
        .unwrap();

        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 12,
                target: LinkTarget::WriteMemory128,
            }]
        );
        assert_eq!(
            read_instruction(&code, 0),
            inst::str_q_unsigned(9, 31, StackLayout::spill_offset(0) as u32)
        );
        assert_eq!(read_instruction(&code, 4), inst::movz_x(1, 0x2000, 0));
        assert_eq!(
            read_instruction(&code, 8),
            inst::ldr_q_unsigned(Q0, 31, StackLayout::spill_offset(0) as u32)
        );
        assert_eq!(read_instruction(&code, 12), inst::nop());
    }

    #[test]
    fn callback_only_exclusive_read_sets_exclusive_state_before_callback() {
        let config = config();
        let mut code_storage = BlockOfCode::with_size(4096).unwrap();
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let mut info = empty_block_info(&code);
        let mut block = block_with_inst(
            Opcode::A64ExclusiveReadMemory32,
            &[
                Value::ImmU64(LocationDescriptor::new(0x4000).value()),
                Value::ImmU64(0x3000),
                Value::ImmAccType(AccType::Normal),
            ],
        );

        context_emit(
            &mut block,
            &mut code,
            &mut info,
            &config,
            |code, ctx, inst| emit_exclusive_read_memory::<32>(code, ctx, inst),
        )
        .unwrap();

        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 12,
                target: LinkTarget::ExclusiveReadMemory32,
            }]
        );
        assert_eq!(read_instruction(&code, 0), inst::movz_x(1, 0x3000, 0));
        assert_eq!(read_instruction(&code, 4), inst::movz_w(XSCRATCH0, 1, 0));
        assert_eq!(
            read_instruction(&code, 8),
            inst::strb_w_unsigned(
                XSCRATCH0,
                XSTATE,
                core::mem::offset_of!(A64JitState, exclusive_state) as u32
            )
        );
        assert_eq!(read_instruction(&code, 12), inst::nop());
    }

    #[test]
    fn callback_only_exclusive_read_memory_128_sets_state_and_defines_fpr() {
        let config = config();
        let mut code_storage = BlockOfCode::with_size(4096).unwrap();
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let mut info = empty_block_info(&code);
        let mut block = block_with_inst(
            Opcode::A64ExclusiveReadMemory128,
            &[
                Value::ImmU64(LocationDescriptor::new(0x4000).value()),
                Value::ImmU64(0x3000),
                Value::ImmAccType(AccType::Normal),
            ],
        );

        let reg_alloc = context_emit(
            &mut block,
            &mut code,
            &mut info,
            &config,
            |code, ctx, inst| emit_exclusive_read_memory::<128>(code, ctx, inst),
        )
        .unwrap();

        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 12,
                target: LinkTarget::ExclusiveReadMemory128,
            }]
        );
        assert_eq!(read_instruction(&code, 0), inst::movz_x(1, 0x3000, 0));
        assert_eq!(read_instruction(&code, 4), inst::movz_w(XSCRATCH0, 1, 0));
        assert_eq!(
            read_instruction(&code, 8),
            inst::strb_w_unsigned(
                XSCRATCH0,
                XSTATE,
                core::mem::offset_of!(A64JitState, exclusive_state) as u32
            )
        );
        assert_eq!(read_instruction(&code, 12), inst::nop());
        assert_eq!(read_instruction(&code, 16), inst::mov_v16b(Q8, Q0));
        assert_eq!(
            reg_alloc.value_location(InstRef(0)),
            Some(HostLoc {
                kind: HostLocKind::Fpr,
                index: Q8 as usize,
            })
        );
    }

    #[test]
    fn callback_only_exclusive_write_fails_without_reservation() {
        let config = config();
        let mut code_storage = BlockOfCode::with_size(4096).unwrap();
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let mut info = empty_block_info(&code);
        let mut block = block_with_inst(
            Opcode::A64ExclusiveWriteMemory32,
            &[
                Value::ImmU64(LocationDescriptor::new(0x4000).value()),
                Value::ImmU64(0x4000),
                Value::ImmU32(0xabcd),
                Value::ImmAccType(AccType::Normal),
            ],
        );

        context_emit(
            &mut block,
            &mut code,
            &mut info,
            &config,
            |code, ctx, inst| emit_exclusive_write_memory::<32>(code, ctx, inst),
        )
        .unwrap();

        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 24,
                target: LinkTarget::ExclusiveWriteMemory32,
            }]
        );
        assert_eq!(read_instruction(&code, 0), inst::movz_x(1, 0x4000, 0));
        assert_eq!(read_instruction(&code, 4), inst::movz_x(2, 0xabcd, 0));
        assert_eq!(read_instruction(&code, 8), inst::movz_w(X0, 1, 0));
        assert_eq!(
            read_instruction(&code, 12),
            inst::ldrb_w_unsigned(
                XSCRATCH0,
                XSTATE,
                core::mem::offset_of!(A64JitState, exclusive_state) as u32
            )
        );
        assert_eq!(read_instruction(&code, 16), inst::cbz_w(XSCRATCH0, 12));
        assert_eq!(
            read_instruction(&code, 20),
            inst::strb_w_unsigned(
                WZR,
                XSTATE,
                core::mem::offset_of!(A64JitState, exclusive_state) as u32
            )
        );
        assert_eq!(read_instruction(&code, 24), inst::nop());
    }

    #[test]
    fn callback_only_exclusive_write_memory_128_passes_value_in_q0() {
        let config = config();
        let mut code_storage = BlockOfCode::with_size(4096).unwrap();
        let mut code = rhazel::CodeGenerator::new(&mut code_storage);
        let mut info = empty_block_info(&code);
        let location = LocationDescriptor::new(0x4000).value();
        let mut block = Block::new(A64LocationDescriptor::new(0x4000, 0, false).to_location());
        let value = block.append(
            Opcode::A64ReadMemory128,
            &[
                Value::ImmU64(location),
                Value::ImmU64(0x1000),
                Value::ImmAccType(AccType::Normal),
            ],
        );
        let write = block.append(
            Opcode::A64ExclusiveWriteMemory128,
            &[
                Value::ImmU64(location),
                Value::ImmU64(0x4000),
                Value::Inst(value),
                Value::ImmAccType(AccType::Normal),
            ],
        );
        block.terminal = Terminal::ReturnToDispatch;

        context_emit_inst(
            &mut block,
            &mut code,
            &mut info,
            &config,
            write,
            |block, reg_alloc| {
                reg_alloc.define_as_register(
                    block,
                    value,
                    HostLoc {
                        kind: HostLocKind::Fpr,
                        index: 9,
                    },
                );
                Ok(())
            },
            |code, ctx, inst| emit_exclusive_write_memory::<128>(code, ctx, inst),
        )
        .unwrap();

        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 28,
                target: LinkTarget::ExclusiveWriteMemory128,
            }]
        );
        assert_eq!(
            read_instruction(&code, 0),
            inst::str_q_unsigned(9, 31, StackLayout::spill_offset(0) as u32)
        );
        assert_eq!(read_instruction(&code, 4), inst::movz_x(1, 0x4000, 0));
        assert_eq!(
            read_instruction(&code, 8),
            inst::ldr_q_unsigned(Q0, 31, StackLayout::spill_offset(0) as u32)
        );
        assert_eq!(read_instruction(&code, 12), inst::movz_w(X0, 1, 0));
        assert_eq!(
            read_instruction(&code, 16),
            inst::ldrb_w_unsigned(
                XSCRATCH0,
                XSTATE,
                core::mem::offset_of!(A64JitState, exclusive_state) as u32
            )
        );
        assert_eq!(read_instruction(&code, 20), inst::cbz_w(XSCRATCH0, 12));
        assert_eq!(
            read_instruction(&code, 24),
            inst::strb_w_unsigned(
                WZR,
                XSTATE,
                core::mem::offset_of!(A64JitState, exclusive_state) as u32
            )
        );
        assert_eq!(read_instruction(&code, 28), inst::nop());
    }
}

//! ARM64 register allocator ownership shell.
//!
//! Upstream owner: `backend/arm64/reg_alloc.h/.cpp`.

use rhazel::{BReg, DReg, HReg, QReg, SReg, VReg, WReg, XReg};
use std::collections::HashSet;
use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::ir::acc_type::AccType;
use crate::ir::block::Block;
use crate::ir::cond::Cond;
use crate::ir::inst::MAX_ARGS;
use crate::ir::types::Type;
use crate::ir::value::{InstRef, Value};

use super::abi::{ABI_CALLER_SAVE, FPR_ORDER, GPR_ORDER, XSCRATCH0};
use super::block_of_code::BlockOfCode;
use super::fpsr_manager::FpsrManager;
use super::inst;
use super::stack_layout::{StackLayout, SPILL_COUNT};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HostLocKind {
    Gpr,
    Fpr,
    Flags,
    Spill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HostLoc {
    pub kind: HostLocKind,
    pub index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RWType {
    Void,
    Read,
    Write,
    ReadWrite,
}

/// Rust counterpart to upstream `RAReg<T>`.
///
/// The raw pointer deliberately mirrors the C++ reference lifetime: several
/// RAReg guards must coexist while locking different source values, then
/// `RegAlloc::realize_all` realizes them as a group.
pub struct RAReg {
    reg_alloc: NonNull<RegAlloc>,
    kind: HostLocKind,
    rw: RWType,
    read_value: Value,
    write_value: Option<InstRef>,
    reg: Option<usize>,
    _not_send_sync: PhantomData<*mut RegAlloc>,
}

impl RAReg {
    fn new(
        reg_alloc: &mut RegAlloc,
        kind: HostLocKind,
        rw: RWType,
        read_value: Value,
        write_value: Option<InstRef>,
    ) -> Self {
        if rw != RWType::Write && !read_value.is_immediate() {
            reg_alloc
                .value_info_mut_by_inst(read_value.inst_ref())
                .locked += 1;
        }

        Self {
            reg_alloc: NonNull::from(reg_alloc),
            kind,
            rw,
            read_value,
            write_value,
            reg: None,
            _not_send_sync: PhantomData,
        }
    }

    pub fn realize(&mut self, code: &mut BlockOfCode, block: &Block) -> Result<usize, String> {
        // RAReg guards are created only by RegAlloc methods, and the pointed
        // allocator must outlive them exactly like upstream's RegAlloc&.
        let reg_alloc = unsafe { self.reg_alloc.as_mut() };
        let reg = match self.rw {
            RWType::Read => reg_alloc.realize_read(code, self.kind, self.read_value)?,
            RWType::Write => reg_alloc.realize_write(
                code,
                block,
                self.kind,
                self.write_value
                    .expect("RegAlloc::RAReg::realize: missing write value"),
            )?,
            RWType::ReadWrite => reg_alloc.realize_read_write(
                code,
                block,
                self.kind,
                self.read_value,
                self.write_value
                    .expect("RegAlloc::RAReg::realize: missing write value"),
            )?,
            RWType::Void => panic!("Invalid RWType"),
        };
        self.reg = Some(reg);
        Ok(reg)
    }

    pub fn index(&self) -> Option<usize> {
        self.reg
    }

    fn realized(&self, what: &str) -> u8 {
        self.reg
            .unwrap_or_else(|| panic!("{what} used before RegAlloc::realize"))
            as u8
    }

    // Upstream `RAReg<T>` converts to its operand type `T` once realized; the
    // Rust handle has no static width, so the emitter names the view it wants.

    /// `RAReg<oaknut::WReg>`
    pub fn w(&self) -> WReg {
        WReg::new(self.realized("W register"))
    }
    /// `RAReg<oaknut::XReg>`
    pub fn x(&self) -> XReg {
        XReg::new(self.realized("X register"))
    }
    /// `RAReg<oaknut::BReg>`
    pub fn b(&self) -> BReg {
        BReg::new(self.realized("B register"))
    }
    /// `RAReg<oaknut::HReg>`
    pub fn h(&self) -> HReg {
        HReg::new(self.realized("H register"))
    }
    /// `RAReg<oaknut::SReg>`
    pub fn s(&self) -> SReg {
        SReg::new(self.realized("S register"))
    }
    /// `RAReg<oaknut::DReg>`
    pub fn d(&self) -> DReg {
        DReg::new(self.realized("D register"))
    }
    /// `RAReg<oaknut::QReg>`
    pub fn q(&self) -> QReg {
        QReg::new(self.realized("Q register"))
    }
    /// The vector register, to be arranged (`RAReg<oaknut::QReg>` used as
    /// `Qreg->B16()` and friends upstream).
    pub fn v(&self) -> VReg {
        VReg::new(self.realized("V register"))
    }
}

impl Drop for RAReg {
    fn drop(&mut self) {
        // See RAReg::realize: this is the same upstream-owned allocator.
        let reg_alloc = unsafe { self.reg_alloc.as_mut() };
        if self.rw != RWType::Write && !self.read_value.is_immediate() {
            let info = reg_alloc.value_info_mut_by_inst(self.read_value.inst_ref());
            assert!(info.locked > 0);
            info.locked -= 1;
        }

        if let Some(index) = self.reg {
            reg_alloc
                .value_info_mut(HostLoc {
                    kind: self.kind,
                    index,
                })
                .realized = false;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Argument {
    pub allocated: bool,
    pub value: Value,
    pub ty: Type,
}

impl Default for Argument {
    fn default() -> Self {
        Self {
            allocated: false,
            value: Value::Void,
            ty: Type::Void,
        }
    }
}

impl Argument {
    pub fn get_type(self) -> Type {
        self.ty
    }

    pub fn is_void(self) -> bool {
        self.value == Value::Void || self.get_type() == Type::Void
    }

    pub fn is_immediate(self) -> bool {
        self.value.is_immediate()
    }

    pub fn get_immediate_u1(self) -> bool {
        self.value.get_u1()
    }

    pub fn get_immediate_u8(self) -> u8 {
        let imm = self.value.get_imm_as_u64();
        assert!(imm < 0x100);
        imm as u8
    }

    pub fn get_immediate_u16(self) -> u16 {
        let imm = self.value.get_imm_as_u64();
        assert!(imm < 0x1_0000);
        imm as u16
    }

    pub fn get_immediate_u32(self) -> u32 {
        let imm = self.value.get_imm_as_u64();
        assert!(imm < 0x1_0000_0000);
        imm as u32
    }

    pub fn get_immediate_u64(self) -> u64 {
        self.value.get_imm_as_u64()
    }

    pub fn get_immediate_cond(self) -> Cond {
        assert!(self.is_immediate() && self.get_type() == Type::Cond);
        self.value.get_cond()
    }

    pub fn get_immediate_acc_type(self) -> AccType {
        assert!(self.is_immediate() && self.get_type() == Type::AccType);
        self.value.get_acc_type()
    }

    pub fn current_location_kind(self, reg_alloc: &RegAlloc) -> Option<HostLocKind> {
        let Value::Inst(inst) = self.value else {
            return None;
        };
        reg_alloc.value_location(inst).map(|loc| loc.kind)
    }

    pub fn is_in_gpr(self, reg_alloc: &RegAlloc) -> bool {
        self.current_location_kind(reg_alloc) == Some(HostLocKind::Gpr)
    }

    pub fn is_in_fpr(self, reg_alloc: &RegAlloc) -> bool {
        self.current_location_kind(reg_alloc) == Some(HostLocKind::Fpr)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostLocInfo {
    pub values: Vec<InstRef>,
    pub locked: usize,
    pub realized: bool,
    pub uses_this_inst: usize,
    pub accumulated_uses: usize,
    pub expected_uses: usize,
}

impl HostLocInfo {
    pub fn contains(&self, value: InstRef) -> bool {
        self.values.contains(&value)
    }

    pub fn setup_scratch_location(&mut self) {
        assert!(self.is_completely_empty());
        self.realized = true;
    }

    pub fn setup_location(&mut self, value: InstRef, expected_uses: usize) {
        assert!(self.is_completely_empty());
        self.values.clear();
        self.values.push(value);
        self.realized = true;
        self.uses_this_inst = 0;
        self.accumulated_uses = 0;
        self.expected_uses = expected_uses;
    }

    pub fn is_completely_empty(&self) -> bool {
        self.values.is_empty()
            && self.locked == 0
            && !self.realized
            && self.uses_this_inst == 0
            && self.accumulated_uses == 0
            && self.expected_uses == 0
    }

    pub fn maybe_allocatable(&self) -> bool {
        self.locked == 0 && !self.realized
    }

    pub fn is_one_remaining_use(&self) -> bool {
        self.accumulated_uses + 1 == self.expected_uses && self.uses_this_inst == 1
    }

    pub fn update_uses(&mut self) {
        self.accumulated_uses += self.uses_this_inst;
        self.uses_this_inst = 0;

        if self.accumulated_uses == self.expected_uses {
            self.values.clear();
            self.accumulated_uses = 0;
            self.expected_uses = 0;
        }
    }
}

pub struct RegAlloc {
    pub gpr_order: Vec<usize>,
    pub fpr_order: Vec<usize>,
    pub gprs: [HostLocInfo; 32],
    pub fprs: [HostLocInfo; 32],
    pub flags: HostLocInfo,
    pub spills: [HostLocInfo; SPILL_COUNT],
    defined_insts: HashSet<InstRef>,
}

impl RegAlloc {
    pub fn new(gpr_order: Vec<usize>, fpr_order: Vec<usize>) -> Self {
        Self {
            gpr_order,
            fpr_order,
            gprs: std::array::from_fn(|_| HostLocInfo::default()),
            fprs: std::array::from_fn(|_| HostLocInfo::default()),
            flags: HostLocInfo::default(),
            spills: std::array::from_fn(|_| HostLocInfo::default()),
            defined_insts: HashSet::new(),
        }
    }

    pub fn get_argument_info(&mut self, block: &Block, inst_ref: InstRef) -> [Argument; MAX_ARGS] {
        let inst = block.get(inst_ref);
        let num_args = inst.num_args();
        let arg_types = inst.opcode.arg_types();
        let mut ret = [Argument::default(); MAX_ARGS];
        let mut non_immediate_uses = Vec::new();

        for (i, arg) in inst.args[..num_args].iter().copied().enumerate() {
            ret[i].value = arg;
            ret[i].ty = arg_types[i];
            if arg == Value::Void {
                continue;
            }
            if !arg.is_immediate() && !is_valueless_type(arg_types[i]) {
                let value_inst = arg.inst_ref();
                assert!(
                    self.value_location(value_inst).is_some(),
                    "argument must already be defined: block={} user=%{} opcode={:?} arg{}=%{} source_opcode={:?} source_arg0={:?} source_use_count={}",
                    block.location,
                    inst_ref.0,
                    inst.opcode,
                    i,
                    value_inst.0,
                    block.get(value_inst).opcode,
                    block.get(value_inst).args[0],
                    block.get(value_inst).use_count
                );
                non_immediate_uses.push(value_inst);
            }
        }

        for value_inst in non_immediate_uses {
            self.value_info_mut_by_inst(value_inst).uses_this_inst += 1;
        }

        ret
    }

    pub fn was_value_defined(&self, inst: InstRef) -> bool {
        self.defined_insts.contains(&inst)
    }

    pub fn read_x(&mut self, arg: Argument) -> RAReg {
        RAReg::new(self, HostLocKind::Gpr, RWType::Read, arg.value, None)
    }

    pub fn read_w(&mut self, arg: Argument) -> RAReg {
        self.read_x(arg)
    }

    pub fn read_q(&mut self, arg: Argument) -> RAReg {
        RAReg::new(self, HostLocKind::Fpr, RWType::Read, arg.value, None)
    }

    pub fn read_d(&mut self, arg: Argument) -> RAReg {
        self.read_q(arg)
    }

    pub fn read_s(&mut self, arg: Argument) -> RAReg {
        self.read_q(arg)
    }

    pub fn read_h(&mut self, arg: Argument) -> RAReg {
        self.read_q(arg)
    }

    pub fn read_b(&mut self, arg: Argument) -> RAReg {
        self.read_q(arg)
    }

    pub fn write_x(&mut self, inst: InstRef) -> RAReg {
        RAReg::new(
            self,
            HostLocKind::Gpr,
            RWType::Write,
            Value::Void,
            Some(inst),
        )
    }

    pub fn write_w(&mut self, inst: InstRef) -> RAReg {
        self.write_x(inst)
    }

    pub fn write_q(&mut self, inst: InstRef) -> RAReg {
        RAReg::new(
            self,
            HostLocKind::Fpr,
            RWType::Write,
            Value::Void,
            Some(inst),
        )
    }

    pub fn write_d(&mut self, inst: InstRef) -> RAReg {
        self.write_q(inst)
    }

    pub fn write_s(&mut self, inst: InstRef) -> RAReg {
        self.write_q(inst)
    }

    pub fn write_h(&mut self, inst: InstRef) -> RAReg {
        self.write_q(inst)
    }

    pub fn write_b(&mut self, inst: InstRef) -> RAReg {
        self.write_q(inst)
    }

    pub fn write_flags(&mut self, inst: InstRef) -> RAReg {
        RAReg::new(
            self,
            HostLocKind::Flags,
            RWType::Write,
            Value::Void,
            Some(inst),
        )
    }

    pub fn read_write_x(&mut self, arg: Argument, inst: InstRef) -> RAReg {
        RAReg::new(
            self,
            HostLocKind::Gpr,
            RWType::ReadWrite,
            arg.value,
            Some(inst),
        )
    }

    pub fn read_write_w(&mut self, arg: Argument, inst: InstRef) -> RAReg {
        self.read_write_x(arg, inst)
    }

    pub fn read_write_q(&mut self, arg: Argument, inst: InstRef) -> RAReg {
        RAReg::new(
            self,
            HostLocKind::Fpr,
            RWType::ReadWrite,
            arg.value,
            Some(inst),
        )
    }

    pub fn read_write_d(&mut self, arg: Argument, inst: InstRef) -> RAReg {
        self.read_write_q(arg, inst)
    }

    pub fn read_write_s(&mut self, arg: Argument, inst: InstRef) -> RAReg {
        self.read_write_q(arg, inst)
    }

    pub fn read_write_h(&mut self, arg: Argument, inst: InstRef) -> RAReg {
        self.read_write_q(arg, inst)
    }

    pub fn read_write_b(&mut self, arg: Argument, inst: InstRef) -> RAReg {
        self.read_write_q(arg, inst)
    }

    pub fn realize_all(
        code: &mut BlockOfCode,
        block: &Block,
        regs: &mut [&mut RAReg],
    ) -> Result<(), String> {
        for reg in regs {
            reg.realize(code, block)?;
        }
        Ok(())
    }

    pub fn define_as_existing(&mut self, block: &mut Block, inst: InstRef, arg: Argument) {
        self.defined_insts.insert(inst);
        assert!(self.value_location(inst).is_none());

        if arg.value.is_immediate() {
            block.replace_uses(inst, arg.value);
            block.recompute_use_counts();
            return;
        }

        let value_inst = arg.value.inst_ref();
        let expected_uses = block.get(inst).use_count as usize;
        let info = self.value_info_mut_by_inst(value_inst);
        info.values.push(inst);
        info.expected_uses += expected_uses;
    }

    pub fn define_as_register(&mut self, block: &Block, inst: InstRef, loc: HostLoc) {
        self.defined_insts.insert(inst);
        assert!(self.value_location(inst).is_none());
        let expected_uses = block.get(inst).use_count as usize;
        let info = self.value_info_mut(loc);
        assert!(info.is_completely_empty());
        info.values.push(inst);
        info.expected_uses += expected_uses;
    }

    pub fn update_all_uses(&mut self) {
        for gpr in &mut self.gprs {
            gpr.update_uses();
        }
        for fpr in &mut self.fprs {
            fpr.update_uses();
        }
        self.flags.update_uses();
        for spill in &mut self.spills {
            spill.update_uses();
        }
    }

    pub fn assert_all_unlocked(&self) {
        let is_unlocked = |info: &HostLocInfo| info.locked == 0 && !info.realized;
        assert!(self.gprs.iter().all(is_unlocked));
        assert!(self.fprs.iter().all(is_unlocked));
        assert!(is_unlocked(&self.flags));
        assert!(self.spills.iter().all(is_unlocked));
    }

    pub fn assert_no_more_uses(&self, block: &Block) {
        let is_empty = HostLocInfo::is_completely_empty;
        let describe_values = |info: &HostLocInfo| {
            info.values
                .iter()
                .map(|inst_ref| {
                    let inst = block.get(*inst_ref);
                    let users = block
                        .instructions
                        .iter()
                        .enumerate()
                        .filter_map(|(user_index, user)| {
                            if user.is_tombstone() {
                                return None;
                            }
                            let args = &user.args[..user.num_args()];
                            args.iter()
                                .any(|arg| *arg == crate::ir::value::Value::Inst(*inst_ref))
                                .then_some((InstRef(user_index as u32), user.opcode, user.args))
                        })
                        .collect::<Vec<_>>();
                    (
                        *inst_ref,
                        inst.opcode,
                        inst.args,
                        inst.use_count,
                        info.accumulated_uses,
                        info.expected_uses,
                        users,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert!(
            self.gprs.iter().all(is_empty),
            "GPRs still contain values at end of block {}: {:?}",
            block.location,
            self.gprs
                .iter()
                .enumerate()
                .filter(|(_, info)| !info.is_completely_empty())
                .map(|(index, info)| (index, info, describe_values(info)))
                .collect::<Vec<_>>()
        );
        assert!(
            self.fprs.iter().all(is_empty),
            "FPRs still contain values at end of block: {:?}",
            self.fprs
                .iter()
                .enumerate()
                .filter(|(_, info)| !info.is_completely_empty())
                .map(|(index, info)| (index, info, describe_values(info)))
                .collect::<Vec<_>>()
        );
        assert!(
            is_empty(&self.flags),
            "flags still contain values at end of block"
        );
        assert!(
            self.spills.iter().all(is_empty),
            "spills still contain values at end of block: {:?}",
            self.spills
                .iter()
                .enumerate()
                .filter(|(_, info)| !info.is_completely_empty())
                .map(|(index, info)| (index, info, describe_values(info)))
                .collect::<Vec<_>>()
        );
    }

    pub fn value_location(&self, value: InstRef) -> Option<HostLoc> {
        find_host_loc(&self.gprs, value, HostLocKind::Gpr)
            .or_else(|| find_host_loc(&self.fprs, value, HostLocKind::Fpr))
            .or_else(|| {
                self.flags.contains(value).then_some(HostLoc {
                    kind: HostLocKind::Flags,
                    index: 0,
                })
            })
            .or_else(|| find_host_loc(&self.spills, value, HostLocKind::Spill))
    }

    pub fn value_info(&self, loc: HostLoc) -> &HostLocInfo {
        match loc.kind {
            HostLocKind::Gpr => &self.gprs[loc.index],
            HostLocKind::Fpr => &self.fprs[loc.index],
            HostLocKind::Flags => &self.flags,
            HostLocKind::Spill => &self.spills[loc.index],
        }
    }

    pub fn value_info_mut(&mut self, loc: HostLoc) -> &mut HostLocInfo {
        match loc.kind {
            HostLocKind::Gpr => &mut self.gprs[loc.index],
            HostLocKind::Fpr => &mut self.fprs[loc.index],
            HostLocKind::Flags => &mut self.flags,
            HostLocKind::Spill => &mut self.spills[loc.index],
        }
    }

    pub fn value_info_by_inst(&self, value: InstRef) -> &HostLocInfo {
        let loc = self
            .value_location(value)
            .expect("RegAlloc::value_info_by_inst: value not found");
        self.value_info(loc)
    }

    pub fn value_info_mut_by_inst(&mut self, value: InstRef) -> &mut HostLocInfo {
        let loc = self
            .value_location(value)
            .expect("RegAlloc::value_info_mut_by_inst: value not found");
        self.value_info_mut(loc)
    }

    pub fn allocate_register(&self, regs: &[HostLocInfo; 32], order: &[usize]) -> Option<usize> {
        order
            .iter()
            .copied()
            .find(|&i| regs[i].is_completely_empty())
            .or_else(|| order.iter().copied().find(|&i| regs[i].maybe_allocatable()))
    }

    pub fn find_free_spill(&self) -> Option<usize> {
        self.spills.iter().position(|spill| spill.values.is_empty())
    }

    pub fn spill_gpr(&mut self, code: &mut BlockOfCode, index: usize) -> Result<(), String> {
        assert!(index < self.gprs.len());
        assert!(self.gprs[index].locked == 0 && !self.gprs[index].realized);
        if self.gprs[index].values.is_empty() {
            return Ok(());
        }

        let new_location_index = self
            .find_free_spill()
            .ok_or_else(|| "ARM64 RegAlloc: all spill locations are full".to_string())?;
        code.write_u32(inst::str_x_unsigned(
            index as u8,
            31,
            StackLayout::spill_offset(new_location_index) as u32,
        ))?;
        self.spills[new_location_index] = std::mem::take(&mut self.gprs[index]);
        Ok(())
    }

    pub fn spill_fpr(&mut self, code: &mut BlockOfCode, index: usize) -> Result<(), String> {
        assert!(index < self.fprs.len());
        assert!(self.fprs[index].locked == 0 && !self.fprs[index].realized);
        if self.fprs[index].values.is_empty() {
            return Ok(());
        }

        let new_location_index = self
            .find_free_spill()
            .ok_or_else(|| "ARM64 RegAlloc: all spill locations are full".to_string())?;
        code.write_u32(inst::str_q_unsigned(
            index as u8,
            31,
            StackLayout::spill_offset(new_location_index) as u32,
        ))?;
        self.spills[new_location_index] = std::mem::take(&mut self.fprs[index]);
        Ok(())
    }

    pub fn spill_flags(&mut self, code: &mut BlockOfCode) -> Result<(), String> {
        assert!(self.flags.locked == 0 && !self.flags.realized);
        if self.flags.values.is_empty() {
            return Ok(());
        }

        let new_location_index = self
            .allocate_register(&self.gprs, &self.gpr_order)
            .ok_or_else(|| "ARM64 RegAlloc: no GPR available to spill flags".to_string())?;
        self.spill_gpr(code, new_location_index)?;
        code.write_u32(inst::mrs_nzcv(new_location_index as u8))?;
        self.gprs[new_location_index] = std::mem::take(&mut self.flags);
        Ok(())
    }

    pub(crate) fn prepare_for_call(
        &mut self,
        code: &mut BlockOfCode,
        fpsr_manager: &mut FpsrManager,
        args: [Option<Argument>; 4],
    ) -> Result<(), String> {
        fpsr_manager.spill(code)?;
        self.spill_flags(code)?;

        for i in 0..32 {
            if ((ABI_CALLER_SAVE >> i) & 1) != 0 {
                self.spill_gpr(code, i)?;
            }
        }

        for i in 0..32 {
            if ((ABI_CALLER_SAVE >> (i + 32)) & 1) != 0 {
                self.spill_fpr(code, i)?;
            }
        }

        let mut ngrn = 0usize;
        let mut nsrn = 0usize;

        for arg in args {
            if let Some(arg) = arg {
                if arg.get_type() == Type::U128 {
                    assert!(self.fprs[nsrn].is_completely_empty());
                    self.load_copy_into_fpr(code, arg.value, nsrn as u8)?;
                    nsrn += 1;
                } else {
                    assert!(self.gprs[ngrn].is_completely_empty());
                    self.load_copy_into_gpr(code, arg.value, ngrn as u8)?;
                    ngrn += 1;
                }
            } else {
                ngrn += 1;
            }
        }

        Ok(())
    }

    pub(crate) fn read_write_flags(
        &mut self,
        code: &mut BlockOfCode,
        block: &Block,
        read: Argument,
        write: Option<InstRef>,
    ) -> Result<(), String> {
        if let Some(write) = write {
            self.defined_insts.insert(write);
        }

        let current_location = self
            .value_location(read.value.inst_ref())
            .expect("RegAlloc::read_write_flags: value not found");

        match current_location.kind {
            HostLocKind::Flags => {
                if !self.flags.is_one_remaining_use() {
                    self.spill_flags(code)?;
                }
            }
            HostLocKind::Gpr => {
                if !self.flags.values.is_empty() {
                    self.spill_flags(code)?;
                }
                code.write_u32(inst::msr_nzcv(current_location.index as u8))?;
            }
            HostLocKind::Spill => {
                if !self.flags.values.is_empty() {
                    self.spill_flags(code)?;
                }
                code.write_u32(inst::ldr_w_unsigned(
                    XSCRATCH0,
                    31,
                    StackLayout::spill_offset(current_location.index) as u32,
                ))?;
                code.write_u32(inst::msr_nzcv(XSCRATCH0))?;
            }
            HostLocKind::Fpr => {
                panic!("Invalid current location for flags");
            }
        }

        if let Some(write) = write {
            self.flags
                .setup_location(write, block.get(write).use_count as usize);
            self.flags.realized = false;
        }

        Ok(())
    }

    pub(crate) fn generate_immediate(
        &mut self,
        code: &mut BlockOfCode,
        kind: HostLocKind,
        value: Value,
    ) -> Result<usize, String> {
        assert!(value.get_type() != Type::U1);

        match kind {
            HostLocKind::Gpr => {
                let new_location_index = self
                    .allocate_register(&self.gprs, &self.gpr_order)
                    .ok_or_else(|| "ARM64 RegAlloc: no GPR available for immediate".to_string())?;
                self.spill_gpr(code, new_location_index)?;
                self.gprs[new_location_index].setup_scratch_location();
                emit_mov_x_imm(code, new_location_index as u8, value.get_imm_as_u64())?;
                Ok(new_location_index)
            }
            HostLocKind::Fpr => {
                let new_location_index = self
                    .allocate_register(&self.fprs, &self.fpr_order)
                    .ok_or_else(|| "ARM64 RegAlloc: no FPR available for immediate".to_string())?;
                self.spill_fpr(code, new_location_index)?;
                self.fprs[new_location_index].setup_scratch_location();
                emit_mov_x_imm(code, XSCRATCH0, value.get_imm_as_u64())?;
                code.write_u32(inst::fmov_d_from_x(new_location_index as u8, XSCRATCH0))?;
                Ok(new_location_index)
            }
            HostLocKind::Flags => {
                self.spill_flags(code)?;
                self.flags.setup_scratch_location();
                emit_mov_x_imm(code, XSCRATCH0, value.get_imm_as_u64())?;
                code.write_u32(inst::msr_nzcv(XSCRATCH0))?;
                Ok(0)
            }
            HostLocKind::Spill => {
                panic!("GenerateImmediate into spill locations is not supported");
            }
        }
    }

    pub(crate) fn realize_read(
        &mut self,
        code: &mut BlockOfCode,
        required_kind: HostLocKind,
        value: Value,
    ) -> Result<usize, String> {
        if value.is_immediate() {
            return self.generate_immediate(code, required_kind, value);
        }

        let current_location = self
            .value_location(value.inst_ref())
            .expect("RegAlloc::realize_read: value not found");

        if current_location.kind == required_kind {
            self.value_info_mut(current_location).realized = true;
            return Ok(current_location.index);
        }

        {
            let current_info = self.value_info(current_location);
            assert!(!current_info.realized);
            assert!(current_info.locked > 0);
        }

        match required_kind {
            HostLocKind::Gpr => {
                let new_location_index = self
                    .allocate_register(&self.gprs, &self.gpr_order)
                    .ok_or_else(|| "ARM64 RegAlloc: no GPR available for read".to_string())?;
                self.spill_gpr(code, new_location_index)?;
                self.load_copy_into_gpr(code, value, new_location_index as u8)?;
                self.gprs[new_location_index] = self.take_value_info(current_location);
                self.gprs[new_location_index].realized = true;
                Ok(new_location_index)
            }
            HostLocKind::Fpr => {
                let new_location_index = self
                    .allocate_register(&self.fprs, &self.fpr_order)
                    .ok_or_else(|| "ARM64 RegAlloc: no FPR available for read".to_string())?;
                self.spill_fpr(code, new_location_index)?;
                self.load_copy_into_fpr(code, value, new_location_index as u8)?;
                self.fprs[new_location_index] = self.take_value_info(current_location);
                self.fprs[new_location_index].realized = true;
                Ok(new_location_index)
            }
            HostLocKind::Flags => {
                panic!("A simple read from flags is likely a logic error");
            }
            HostLocKind::Spill => {
                panic!("RealizeRead into spill locations is not supported");
            }
        }
    }

    pub(crate) fn realize_write(
        &mut self,
        code: &mut BlockOfCode,
        block: &Block,
        kind: HostLocKind,
        value: InstRef,
    ) -> Result<usize, String> {
        self.defined_insts.insert(value);
        assert!(self.value_location(value).is_none());

        let expected_uses = block.get(value).use_count as usize;
        match kind {
            HostLocKind::Gpr => {
                let new_location_index = self
                    .allocate_register(&self.gprs, &self.gpr_order)
                    .ok_or_else(|| "ARM64 RegAlloc: no GPR available for write".to_string())?;
                self.spill_gpr(code, new_location_index)?;
                self.gprs[new_location_index].setup_location(value, expected_uses);
                Ok(new_location_index)
            }
            HostLocKind::Fpr => {
                let new_location_index = self
                    .allocate_register(&self.fprs, &self.fpr_order)
                    .ok_or_else(|| "ARM64 RegAlloc: no FPR available for write".to_string())?;
                self.spill_fpr(code, new_location_index)?;
                self.fprs[new_location_index].setup_location(value, expected_uses);
                Ok(new_location_index)
            }
            HostLocKind::Flags => {
                self.spill_flags(code)?;
                self.flags.setup_location(value, expected_uses);
                Ok(0)
            }
            HostLocKind::Spill => {
                panic!("RealizeWrite into spill locations is not supported");
            }
        }
    }

    pub(crate) fn realize_read_write(
        &mut self,
        code: &mut BlockOfCode,
        block: &Block,
        kind: HostLocKind,
        read_value: Value,
        write_value: InstRef,
    ) -> Result<usize, String> {
        self.defined_insts.insert(write_value);
        let write_loc = self.realize_write(code, block, kind, write_value)?;

        match kind {
            HostLocKind::Gpr => {
                self.load_copy_into_gpr(code, read_value, write_loc as u8)?;
                Ok(write_loc)
            }
            HostLocKind::Fpr => {
                self.load_copy_into_fpr(code, read_value, write_loc as u8)?;
                Ok(write_loc)
            }
            HostLocKind::Flags => {
                panic!("Incorrect function for ReadWrite of flags");
            }
            HostLocKind::Spill => {
                panic!("RealizeReadWrite into spill locations is not supported");
            }
        }
    }

    pub fn load_copy_into_gpr(
        &self,
        code: &mut BlockOfCode,
        value: Value,
        reg: u8,
    ) -> Result<(), String> {
        if value.is_immediate() {
            emit_mov_x_imm(code, reg, value.get_imm_as_u64())?;
            return Ok(());
        }

        let current_location = self
            .value_location(value.inst_ref())
            .expect("RegAlloc::load_copy_into_gpr: value not found");
        match current_location.kind {
            HostLocKind::Gpr => {
                code.write_u32(inst::mov_x(reg, current_location.index as u8))?;
            }
            HostLocKind::Fpr => {
                code.write_u32(inst::fmov_x_from_d(reg, current_location.index as u8))?;
            }
            HostLocKind::Spill => {
                code.write_u32(inst::ldr_x_unsigned(
                    reg,
                    31,
                    StackLayout::spill_offset(current_location.index) as u32,
                ))?;
            }
            HostLocKind::Flags => {
                code.write_u32(inst::mrs_nzcv(reg))?;
            }
        }
        Ok(())
    }

    pub fn load_copy_into_fpr(
        &self,
        code: &mut BlockOfCode,
        value: Value,
        reg: u8,
    ) -> Result<(), String> {
        if value.is_immediate() {
            emit_mov_x_imm(code, XSCRATCH0, value.get_imm_as_u64())?;
            code.write_u32(inst::fmov_d_from_x(reg, XSCRATCH0))?;
            return Ok(());
        }

        let current_location = self
            .value_location(value.inst_ref())
            .expect("RegAlloc::load_copy_into_fpr: value not found");
        match current_location.kind {
            HostLocKind::Gpr => {
                code.write_u32(inst::fmov_d_from_x(reg, current_location.index as u8))?;
            }
            HostLocKind::Fpr => {
                code.write_u32(inst::mov_v16b(reg, current_location.index as u8))?;
            }
            HostLocKind::Spill => {
                code.write_u32(inst::ldr_q_unsigned(
                    reg,
                    31,
                    StackLayout::spill_offset(current_location.index) as u32,
                ))?;
            }
            HostLocKind::Flags => {
                panic!("Moving from flags into fprs is not currently supported");
            }
        }
        Ok(())
    }

    fn take_value_info(&mut self, loc: HostLoc) -> HostLocInfo {
        match loc.kind {
            HostLocKind::Gpr => std::mem::take(&mut self.gprs[loc.index]),
            HostLocKind::Fpr => std::mem::take(&mut self.fprs[loc.index]),
            HostLocKind::Flags => std::mem::take(&mut self.flags),
            HostLocKind::Spill => std::mem::take(&mut self.spills[loc.index]),
        }
    }
}

impl Default for RegAlloc {
    fn default() -> Self {
        Self::new(GPR_ORDER.to_vec(), FPR_ORDER.to_vec())
    }
}

fn is_valueless_type(kind: Type) -> bool {
    matches!(kind, Type::Table)
}

fn find_host_loc<const N: usize>(
    infos: &[HostLocInfo; N],
    value: InstRef,
    kind: HostLocKind,
) -> Option<HostLoc> {
    infos
        .iter()
        .position(|info| info.contains(value))
        .map(|index| HostLoc { kind, index })
}

fn emit_mov_x_imm(code: &mut BlockOfCode, reg: u8, imm: u64) -> Result<(), String> {
    code.write_u32(inst::movz_x(reg, (imm & 0xffff) as u16, 0))?;
    for shift in [16, 32, 48] {
        let chunk = ((imm >> shift) & 0xffff) as u16;
        if chunk != 0 {
            code.write_u32(inst::movk_x(reg, chunk, shift as u8))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::location::LocationDescriptor;
    use crate::ir::opcode::Opcode;

    #[test]
    fn host_loc_info_matches_upstream_use_lifecycle() {
        let inst = InstRef(3);
        let mut info = HostLocInfo::default();

        assert!(info.is_completely_empty());
        info.setup_location(inst, 2);
        assert_eq!(info.values, vec![inst]);
        assert!(info.realized);
        assert!(!info.maybe_allocatable());

        info.realized = false;
        info.uses_this_inst = 1;
        assert!(!info.is_one_remaining_use());
        info.update_uses();
        assert_eq!(info.values, vec![inst]);
        assert_eq!(info.accumulated_uses, 1);

        info.uses_this_inst = 1;
        assert!(info.is_one_remaining_use());
        info.update_uses();
        assert!(info.values.is_empty());
        assert_eq!(info.accumulated_uses, 0);
        assert_eq!(info.expected_uses, 0);
    }

    #[test]
    fn argument_immediate_accessors_preserve_upstream_bounds() {
        let arg = Argument {
            allocated: false,
            value: Value::ImmU32(0xff),
            ty: Type::U32,
        };
        assert!(arg.is_immediate());
        assert_eq!(arg.get_immediate_u8(), 0xff);
        assert_eq!(arg.get_immediate_u16(), 0xff);
        assert_eq!(arg.get_immediate_u32(), 0xff);
        assert_eq!(arg.get_immediate_u64(), 0xff);

        let cond = Argument {
            allocated: false,
            value: Value::ImmCond(Cond::EQ),
            ty: Type::Cond,
        };
        assert_eq!(cond.get_immediate_cond(), Cond::EQ);

        let acc = Argument {
            allocated: false,
            value: Value::ImmAccType(AccType::Atomic),
            ty: Type::AccType,
        };
        assert_eq!(acc.get_immediate_acc_type(), AccType::Atomic);
    }

    #[test]
    fn get_argument_info_tracks_uses_this_inst_for_defined_values() {
        let mut block = Block::new(LocationDescriptor::new(0x1000));
        let lhs = block.append(
            Opcode::Add32,
            &[Value::ImmU32(1), Value::ImmU32(2), false.into()],
        );
        let rhs = block.append(
            Opcode::Add32,
            &[Value::ImmU32(3), Value::ImmU32(4), false.into()],
        );
        let add = block.append(Opcode::Add32, &[lhs.into(), rhs.into(), false.into()]);

        let mut reg_alloc = RegAlloc::default();
        reg_alloc.define_as_register(
            &block,
            lhs,
            HostLoc {
                kind: HostLocKind::Gpr,
                index: 1,
            },
        );
        reg_alloc.define_as_register(
            &block,
            rhs,
            HostLoc {
                kind: HostLocKind::Fpr,
                index: 2,
            },
        );

        let args = reg_alloc.get_argument_info(&block, add);
        assert_eq!(args[0].value, lhs.into());
        assert_eq!(args[1].value, rhs.into());
        assert_eq!(args[2].value, false.into());
        assert_eq!(reg_alloc.value_info_by_inst(lhs).uses_this_inst, 1);
        assert_eq!(reg_alloc.value_info_by_inst(rhs).uses_this_inst, 1);
        assert_eq!(
            args[0].current_location_kind(&reg_alloc),
            Some(HostLocKind::Gpr)
        );
        assert!(args[1].is_in_fpr(&reg_alloc));
    }

    #[test]
    fn get_argument_info_preserves_opcode_argument_type_for_inst_values() {
        let mut block = Block::new(LocationDescriptor::new(0x4000));
        let read = block.append(
            Opcode::A64ReadMemory128,
            &[
                Value::ImmU64(LocationDescriptor::new(0x4000).value()),
                Value::ImmU64(0x1000),
                Value::ImmAccType(AccType::Normal),
            ],
        );
        let write = block.append(
            Opcode::A64WriteMemory128,
            &[
                Value::ImmU64(LocationDescriptor::new(0x4000).value()),
                Value::ImmU64(0x2000),
                Value::Inst(read),
                Value::ImmAccType(AccType::Normal),
            ],
        );

        let mut reg_alloc = RegAlloc::default();
        reg_alloc.define_as_register(
            &block,
            read,
            HostLoc {
                kind: HostLocKind::Fpr,
                index: 9,
            },
        );

        let args = reg_alloc.get_argument_info(&block, write);

        assert_eq!(args[2].value, Value::Inst(read));
        assert_eq!(args[2].get_type(), Type::U128);
        assert_eq!(reg_alloc.value_info_by_inst(read).uses_this_inst, 1);
    }

    #[test]
    fn get_argument_info_skips_valueless_table_argument() {
        let mut block = Block::new(LocationDescriptor::new(0x5000));
        let table_value = block.append(
            Opcode::VectorTable,
            &[Value::ImmU64(0), Value::Void, Value::Void, Value::Void],
        );
        let indices = block.append(
            Opcode::Add64,
            &[Value::ImmU64(1), Value::ImmU64(2), Value::ImmU1(false)],
        );
        let lookup = block.append(
            Opcode::VectorTableLookup64,
            &[
                Value::ImmU64(0),
                Value::Inst(table_value),
                Value::Inst(indices),
            ],
        );

        let mut reg_alloc = RegAlloc::default();
        reg_alloc.define_as_register(
            &block,
            indices,
            HostLoc {
                kind: HostLocKind::Fpr,
                index: 3,
            },
        );

        let args = reg_alloc.get_argument_info(&block, lookup);

        assert_eq!(args[1].value, Value::Inst(table_value));
        assert_eq!(args[1].get_type(), Type::Table);
        assert!(!reg_alloc.was_value_defined(table_value));
        assert_eq!(reg_alloc.value_info_by_inst(indices).uses_this_inst, 1);
    }

    #[test]
    fn define_as_existing_aliases_source_location_or_replaces_immediate_uses() {
        let mut block = Block::new(LocationDescriptor::new(0x1000));
        let source = block.append(
            Opcode::Add32,
            &[Value::ImmU32(1), Value::ImmU32(2), false.into()],
        );
        let alias = block.append(Opcode::Identity, &[source.into()]);
        let consumer = block.append(
            Opcode::Add32,
            &[alias.into(), Value::ImmU32(3), false.into()],
        );

        let mut reg_alloc = RegAlloc::default();
        reg_alloc.define_as_register(
            &block,
            source,
            HostLoc {
                kind: HostLocKind::Gpr,
                index: 5,
            },
        );
        reg_alloc.define_as_existing(
            &mut block,
            alias,
            Argument {
                allocated: false,
                value: source.into(),
                ty: Type::Opaque,
            },
        );

        let loc = reg_alloc.value_location(alias).unwrap();
        assert_eq!(loc.kind, HostLocKind::Gpr);
        assert_eq!(loc.index, 5);
        assert!(reg_alloc.value_info(loc).values.contains(&source));
        assert!(reg_alloc.value_info(loc).values.contains(&alias));
        assert!(reg_alloc.was_value_defined(alias));

        let immediate = block.append(
            Opcode::Add32,
            &[Value::ImmU32(4), Value::ImmU32(5), false.into()],
        );
        block.set_arg(consumer, 1, immediate.into());
        reg_alloc.define_as_existing(
            &mut block,
            immediate,
            Argument {
                allocated: false,
                value: Value::ImmU32(9),
                ty: Type::U32,
            },
        );
        assert_eq!(block.get(consumer).arg(1), Value::ImmU32(9));
        assert_eq!(block.get(immediate).use_count, 0);
    }

    #[test]
    fn allocation_prefers_empty_then_allocatable_registers_and_free_spills() {
        let mut reg_alloc = RegAlloc::default();
        reg_alloc.gprs[0].setup_scratch_location();
        reg_alloc.gprs[1].locked = 1;
        reg_alloc.gprs[2].values.push(InstRef(9));

        assert_eq!(
            reg_alloc.allocate_register(&reg_alloc.gprs, &[0, 1, 2, 3]),
            Some(3)
        );

        reg_alloc.gprs[3].values.push(InstRef(10));
        assert_eq!(
            reg_alloc.allocate_register(&reg_alloc.gprs, &[0, 1, 2, 3]),
            Some(2)
        );
        assert_eq!(reg_alloc.find_free_spill(), Some(0));
        reg_alloc.spills[0].values.push(InstRef(11));
        assert_eq!(reg_alloc.find_free_spill(), Some(1));
    }

    #[test]
    fn spill_gpr_and_fpr_emit_upstream_stack_stores_and_move_locations() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut reg_alloc = RegAlloc::default();
        let gpr_value = InstRef(1);
        let fpr_value = InstRef(2);

        reg_alloc.gprs[3].setup_location(gpr_value, 1);
        reg_alloc.gprs[3].realized = false;
        reg_alloc.fprs[6].setup_location(fpr_value, 1);
        reg_alloc.fprs[6].realized = false;

        reg_alloc.spill_gpr(&mut code, 3).unwrap();
        reg_alloc.spill_fpr(&mut code, 6).unwrap();

        let words = unsafe { std::slice::from_raw_parts(code.code_base_ptr().cast::<u32>(), 2) };
        assert_eq!(
            words[0],
            inst::str_x_unsigned(3, 31, StackLayout::spill_offset(0) as u32)
        );
        assert_eq!(
            words[1],
            inst::str_q_unsigned(6, 31, StackLayout::spill_offset(1) as u32)
        );
        assert!(reg_alloc.gprs[3].is_completely_empty());
        assert!(reg_alloc.fprs[6].is_completely_empty());
        assert_eq!(reg_alloc.spills[0].values, vec![gpr_value]);
        assert_eq!(reg_alloc.spills[1].values, vec![fpr_value]);
    }

    #[test]
    fn spill_flags_spills_selected_gpr_then_reads_nzcv_into_it() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut reg_alloc = RegAlloc::new(vec![4], Vec::new());
        let old_gpr_value = InstRef(7);
        let flags_value = InstRef(8);

        reg_alloc.gprs[4].setup_location(old_gpr_value, 1);
        reg_alloc.gprs[4].realized = false;
        reg_alloc.flags.setup_location(flags_value, 1);
        reg_alloc.flags.realized = false;

        reg_alloc.spill_flags(&mut code).unwrap();

        let words = unsafe { std::slice::from_raw_parts(code.code_base_ptr().cast::<u32>(), 2) };
        assert_eq!(
            words[0],
            inst::str_x_unsigned(4, 31, StackLayout::spill_offset(0) as u32)
        );
        assert_eq!(words[1], inst::mrs_nzcv(4));
        assert!(reg_alloc.flags.is_completely_empty());
        assert_eq!(reg_alloc.spills[0].values, vec![old_gpr_value]);
        assert_eq!(reg_alloc.gprs[4].values, vec![flags_value]);
    }

    #[test]
    fn load_copy_into_gpr_matches_upstream_sources() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut reg_alloc = RegAlloc::default();

        reg_alloc.gprs[2].setup_location(InstRef(1), 1);
        reg_alloc.fprs[3].setup_location(InstRef(2), 1);
        reg_alloc.spills[4].setup_location(InstRef(3), 1);
        reg_alloc.flags.setup_location(InstRef(4), 1);

        reg_alloc
            .load_copy_into_gpr(&mut code, InstRef(1).into(), 9)
            .unwrap();
        reg_alloc
            .load_copy_into_gpr(&mut code, InstRef(2).into(), 10)
            .unwrap();
        reg_alloc
            .load_copy_into_gpr(&mut code, InstRef(3).into(), 11)
            .unwrap();
        reg_alloc
            .load_copy_into_gpr(&mut code, InstRef(4).into(), 12)
            .unwrap();

        let words = unsafe { std::slice::from_raw_parts(code.code_base_ptr().cast::<u32>(), 4) };
        assert_eq!(words[0], inst::mov_x(9, 2));
        assert_eq!(words[1], inst::fmov_x_from_d(10, 3));
        assert_eq!(
            words[2],
            inst::ldr_x_unsigned(11, 31, StackLayout::spill_offset(4) as u32)
        );
        assert_eq!(words[3], inst::mrs_nzcv(12));
    }

    #[test]
    fn load_copy_into_fpr_matches_upstream_sources() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut reg_alloc = RegAlloc::default();

        reg_alloc.gprs[5].setup_location(InstRef(5), 1);
        reg_alloc.fprs[6].setup_location(InstRef(6), 1);
        reg_alloc.spills[7].setup_location(InstRef(7), 1);

        reg_alloc
            .load_copy_into_fpr(&mut code, InstRef(5).into(), 13)
            .unwrap();
        reg_alloc
            .load_copy_into_fpr(&mut code, InstRef(6).into(), 14)
            .unwrap();
        reg_alloc
            .load_copy_into_fpr(&mut code, InstRef(7).into(), 15)
            .unwrap();

        let words = unsafe { std::slice::from_raw_parts(code.code_base_ptr().cast::<u32>(), 3) };
        assert_eq!(words[0], inst::fmov_d_from_x(13, 5));
        assert_eq!(words[1], inst::mov_v16b(14, 6));
        assert_eq!(
            words[2],
            inst::ldr_q_unsigned(15, 31, StackLayout::spill_offset(7) as u32)
        );
    }

    #[test]
    fn load_copy_into_registers_materializes_immediates() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let reg_alloc = RegAlloc::default();

        reg_alloc
            .load_copy_into_gpr(&mut code, Value::ImmU64(0x1234_5678_9abc_def0), 1)
            .unwrap();
        reg_alloc
            .load_copy_into_fpr(&mut code, Value::ImmU64(0x0000_0000_0001_0000), 2)
            .unwrap();

        let words = unsafe { std::slice::from_raw_parts(code.code_base_ptr().cast::<u32>(), 7) };
        assert_eq!(words[0], inst::movz_x(1, 0xdef0, 0));
        assert_eq!(words[1], inst::movk_x(1, 0x9abc, 16));
        assert_eq!(words[2], inst::movk_x(1, 0x5678, 32));
        assert_eq!(words[3], inst::movk_x(1, 0x1234, 48));
        assert_eq!(words[4], inst::movz_x(XSCRATCH0, 0, 0));
        assert_eq!(words[5], inst::movk_x(XSCRATCH0, 1, 16));
        assert_eq!(words[6], inst::fmov_d_from_x(2, XSCRATCH0));
    }

    #[test]
    fn generate_immediate_realizes_upstream_target_kinds() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut reg_alloc = RegAlloc::new(vec![1], vec![2]);

        assert_eq!(
            reg_alloc
                .generate_immediate(&mut code, HostLocKind::Gpr, Value::ImmU64(0x1234))
                .unwrap(),
            1
        );
        assert_eq!(
            reg_alloc
                .generate_immediate(&mut code, HostLocKind::Fpr, Value::ImmU64(0x5678))
                .unwrap(),
            2
        );
        assert_eq!(
            reg_alloc
                .generate_immediate(&mut code, HostLocKind::Flags, Value::ImmU64(0))
                .unwrap(),
            0
        );

        let words = unsafe { std::slice::from_raw_parts(code.code_base_ptr().cast::<u32>(), 5) };
        assert_eq!(words[0], inst::movz_x(1, 0x1234, 0));
        assert_eq!(words[1], inst::movz_x(XSCRATCH0, 0x5678, 0));
        assert_eq!(words[2], inst::fmov_d_from_x(2, XSCRATCH0));
        assert_eq!(words[3], inst::movz_x(XSCRATCH0, 0, 0));
        assert_eq!(words[4], inst::msr_nzcv(XSCRATCH0));
        assert!(reg_alloc.gprs[1].realized);
        assert!(reg_alloc.fprs[2].realized);
        assert!(reg_alloc.flags.realized);
    }

    #[test]
    fn realize_read_reuses_or_moves_existing_locations() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut reg_alloc = RegAlloc::new(vec![4], vec![7]);

        reg_alloc.gprs[3].setup_location(InstRef(1), 1);
        reg_alloc.gprs[3].realized = false;
        assert_eq!(
            reg_alloc
                .realize_read(&mut code, HostLocKind::Gpr, InstRef(1).into())
                .unwrap(),
            3
        );
        assert!(reg_alloc.gprs[3].realized);
        assert_eq!(code.code_size(), 0);

        reg_alloc.spills[1].setup_location(InstRef(2), 1);
        reg_alloc.spills[1].realized = false;
        reg_alloc.spills[1].locked = 1;
        assert_eq!(
            reg_alloc
                .realize_read(&mut code, HostLocKind::Gpr, InstRef(2).into())
                .unwrap(),
            4
        );

        reg_alloc.gprs[6].setup_location(InstRef(3), 1);
        reg_alloc.gprs[6].realized = false;
        reg_alloc.gprs[6].locked = 1;
        assert_eq!(
            reg_alloc
                .realize_read(&mut code, HostLocKind::Fpr, InstRef(3).into())
                .unwrap(),
            7
        );

        let words = unsafe { std::slice::from_raw_parts(code.code_base_ptr().cast::<u32>(), 2) };
        assert_eq!(
            words[0],
            inst::ldr_x_unsigned(4, 31, StackLayout::spill_offset(1) as u32)
        );
        assert_eq!(words[1], inst::fmov_d_from_x(7, 6));
        assert!(reg_alloc.spills[1].is_completely_empty());
        assert!(reg_alloc.gprs[6].is_completely_empty());
        assert_eq!(reg_alloc.gprs[4].values, vec![InstRef(2)]);
        assert_eq!(reg_alloc.fprs[7].values, vec![InstRef(3)]);
    }

    #[test]
    fn realize_write_and_read_write_define_destinations() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(LocationDescriptor::new(0x2000));
        let write = block.append(
            Opcode::Add64,
            &[Value::ImmU64(1), Value::ImmU64(2), false.into()],
        );
        let consumer = block.append(Opcode::Identity, &[write.into()]);
        assert_eq!(block.get(write).use_count, 1);

        let mut reg_alloc = RegAlloc::new(vec![8], vec![9]);
        assert_eq!(
            reg_alloc
                .realize_write(&mut code, &block, HostLocKind::Gpr, write)
                .unwrap(),
            8
        );
        assert!(reg_alloc.was_value_defined(write));
        assert_eq!(reg_alloc.gprs[8].values, vec![write]);
        assert_eq!(reg_alloc.gprs[8].expected_uses, 1);

        let read_write = block.append(Opcode::Identity, &[consumer.into()]);
        let _consumer2 = block.append(Opcode::Identity, &[read_write.into()]);
        assert_eq!(
            reg_alloc
                .realize_read_write(
                    &mut code,
                    &block,
                    HostLocKind::Fpr,
                    Value::ImmU64(0x55aa),
                    read_write,
                )
                .unwrap(),
            9
        );

        let words = unsafe { std::slice::from_raw_parts(code.code_base_ptr().cast::<u32>(), 2) };
        assert_eq!(words[0], inst::movz_x(XSCRATCH0, 0x55aa, 0));
        assert_eq!(words[1], inst::fmov_d_from_x(9, XSCRATCH0));
        assert!(reg_alloc.was_value_defined(read_write));
        assert_eq!(reg_alloc.fprs[9].values, vec![read_write]);
        assert_eq!(reg_alloc.fprs[9].expected_uses, 1);
    }

    #[test]
    fn read_write_flags_matches_upstream_gpr_and_spill_sources() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(LocationDescriptor::new(0x3000));
        let read_gpr = block.append(
            Opcode::Add32,
            &[Value::ImmU32(1), Value::ImmU32(2), false.into()],
        );
        let write_flags = block.append(Opcode::Identity, &[read_gpr.into()]);
        let _consumer = block.append(Opcode::Identity, &[write_flags.into()]);

        let mut reg_alloc = RegAlloc::new(vec![18], Vec::new());
        reg_alloc.gprs[5].setup_location(read_gpr, 1);
        reg_alloc
            .read_write_flags(
                &mut code,
                &block,
                Argument {
                    allocated: false,
                    value: read_gpr.into(),
                    ty: Type::U32,
                },
                Some(write_flags),
            )
            .unwrap();

        assert_eq!(reg_alloc.flags.values, vec![write_flags]);
        assert!(!reg_alloc.flags.realized);

        let read_spill = block.append(
            Opcode::Add32,
            &[Value::ImmU32(3), Value::ImmU32(4), false.into()],
        );
        reg_alloc.spills[2].setup_location(read_spill, 1);
        reg_alloc
            .read_write_flags(
                &mut code,
                &block,
                Argument {
                    allocated: false,
                    value: read_spill.into(),
                    ty: Type::U32,
                },
                None,
            )
            .unwrap();

        let words = unsafe { std::slice::from_raw_parts(code.code_base_ptr().cast::<u32>(), 4) };
        assert_eq!(words[0], inst::msr_nzcv(5));
        assert_eq!(words[1], inst::mrs_nzcv(18));
        assert_eq!(
            words[2],
            inst::ldr_w_unsigned(XSCRATCH0, 31, StackLayout::spill_offset(2) as u32)
        );
        assert_eq!(words[3], inst::msr_nzcv(XSCRATCH0));
        assert_eq!(reg_alloc.gprs[18].values, vec![write_flags]);
    }

    #[test]
    fn prepare_for_call_spills_state_then_loads_abi_arguments() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut fpsr = FpsrManager::new(12);
        let mut reg_alloc = RegAlloc::new(vec![18], Vec::new());

        reg_alloc.gprs[1].setup_location(InstRef(1), 1);
        reg_alloc.gprs[1].realized = false;
        reg_alloc.fprs[0].setup_location(InstRef(2), 1);
        reg_alloc.fprs[0].realized = false;
        reg_alloc.flags.setup_location(InstRef(3), 1);
        reg_alloc.flags.realized = false;

        reg_alloc
            .prepare_for_call(
                &mut code,
                &mut fpsr,
                [
                    Some(Argument {
                        allocated: false,
                        value: Value::ImmU64(0x77),
                        ty: Type::U64,
                    }),
                    None,
                    None,
                    None,
                ],
            )
            .unwrap();

        let words = unsafe { std::slice::from_raw_parts(code.code_base_ptr().cast::<u32>(), 4) };
        assert_eq!(words[0], inst::mrs_nzcv(18));
        assert_eq!(
            words[1],
            inst::str_x_unsigned(1, 31, StackLayout::spill_offset(0) as u32)
        );
        assert_eq!(
            words[2],
            inst::str_q_unsigned(0, 31, StackLayout::spill_offset(1) as u32)
        );
        assert_eq!(words[3], inst::movz_x(0, 0x77, 0));
        assert_eq!(reg_alloc.gprs[18].values, vec![InstRef(3)]);
        assert_eq!(reg_alloc.spills[0].values, vec![InstRef(1)]);
        assert_eq!(reg_alloc.spills[1].values, vec![InstRef(2)]);
    }

    #[test]
    fn rareg_locks_realizes_and_releases_read_registers() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(LocationDescriptor::new(0x4000));
        let source = block.append(
            Opcode::Add64,
            &[Value::ImmU64(1), Value::ImmU64(2), false.into()],
        );

        let mut reg_alloc = RegAlloc::default();
        reg_alloc.define_as_register(
            &block,
            source,
            HostLoc {
                kind: HostLocKind::Gpr,
                index: 3,
            },
        );
        reg_alloc.gprs[3].realized = false;

        let mut read = reg_alloc.read_x(Argument {
            allocated: false,
            value: source.into(),
            ty: Type::U64,
        });
        assert_eq!(reg_alloc.gprs[3].locked, 1);
        assert_eq!(read.realize(&mut code, &block).unwrap(), 3);
        assert_eq!(read.index(), Some(3));
        assert!(reg_alloc.gprs[3].realized);

        drop(read);
        assert_eq!(reg_alloc.gprs[3].locked, 0);
        assert!(!reg_alloc.gprs[3].realized);
    }

    #[test]
    fn rareg_realize_all_handles_multiple_registers_then_drop_resets_realized() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(LocationDescriptor::new(0x5000));
        let source = block.append(
            Opcode::Add64,
            &[Value::ImmU64(1), Value::ImmU64(2), false.into()],
        );
        let write = block.append(Opcode::Identity, &[source.into()]);
        let _consumer = block.append(Opcode::Identity, &[write.into()]);

        let mut reg_alloc = RegAlloc::new(vec![4, 5], vec![6]);
        reg_alloc.define_as_register(
            &block,
            source,
            HostLoc {
                kind: HostLocKind::Gpr,
                index: 4,
            },
        );
        reg_alloc.gprs[4].realized = false;

        let mut read = reg_alloc.read_x(Argument {
            allocated: false,
            value: source.into(),
            ty: Type::U64,
        });
        let mut write_reg = reg_alloc.write_q(write);
        RegAlloc::realize_all(&mut code, &block, &mut [&mut read, &mut write_reg]).unwrap();

        assert_eq!(read.index(), Some(4));
        assert_eq!(write_reg.index(), Some(6));
        assert!(reg_alloc.gprs[4].realized);
        assert!(reg_alloc.fprs[6].realized);
        assert_eq!(reg_alloc.fprs[6].values, vec![write]);
        assert_eq!(reg_alloc.fprs[6].expected_uses, 1);

        drop(read);
        drop(write_reg);
        assert_eq!(reg_alloc.gprs[4].locked, 0);
        assert!(!reg_alloc.gprs[4].realized);
        assert!(!reg_alloc.fprs[6].realized);
    }

    #[test]
    fn rareg_read_write_locks_source_copies_into_destination_and_releases() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(LocationDescriptor::new(0x6000));
        let source = block.append(
            Opcode::Add64,
            &[Value::ImmU64(1), Value::ImmU64(2), false.into()],
        );
        let write = block.append(Opcode::Identity, &[source.into()]);
        let _consumer = block.append(Opcode::Identity, &[write.into()]);

        let mut reg_alloc = RegAlloc::new(vec![8], vec![9]);
        reg_alloc.define_as_register(
            &block,
            source,
            HostLoc {
                kind: HostLocKind::Gpr,
                index: 8,
            },
        );
        reg_alloc.gprs[8].realized = false;

        let mut read_write = reg_alloc.read_write_q(
            Argument {
                allocated: false,
                value: source.into(),
                ty: Type::U128,
            },
            write,
        );
        assert_eq!(reg_alloc.gprs[8].locked, 1);
        assert_eq!(read_write.realize(&mut code, &block).unwrap(), 9);

        let words = unsafe { std::slice::from_raw_parts(code.code_base_ptr().cast::<u32>(), 1) };
        assert_eq!(words[0], inst::fmov_d_from_x(9, 8));
        assert_eq!(reg_alloc.fprs[9].values, vec![write]);
        assert!(reg_alloc.fprs[9].realized);

        drop(read_write);
        assert_eq!(reg_alloc.gprs[8].locked, 0);
        assert!(!reg_alloc.fprs[9].realized);
    }
}
